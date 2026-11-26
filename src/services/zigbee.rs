//! ZigBee EZSP service over the ASH serial link protocol.
//!
//! The gateway owns ASH framing, acknowledgements, retries, reset, and EZSP
//! version negotiation. Clients see canonical EZSP frames:
//!
//! ```text
//! seq | frameControlLow | frameControlHigh = 0x01 | frameId (2, LE) | params
//! ```
//!
//! ASH link frames (ACK/NAK/RST/RSTACK/ERROR) are recorded as raw wire bytes so
//! the message log can show exactly what happened on the link.

use super::catalog::{
    SERVICE_PARAM_OPEN_ATTEMPTS_INTERVAL, SERVICE_PARAM_OUTGOING_MAX_TTL,
    ZIGBEE_PARAM_ASH_ACK_TIMEOUT, ZIGBEE_PARAM_ASH_MAX_RETRIES,
};
use super::serial::{Port, PortOpener, SerialSettings};
use super::{ServiceError, StatusEvent};
use crate::messages::MessageRecorder;
use hodeco_contract::domain::{
    Message, MessageState, ParamValues, ServiceKey, ServiceState, ServiceStatus, param_duration,
    param_u64,
};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const ASH_FLAG: u8 = 0x7e;
pub const ASH_ESCAPE: u8 = 0x7d;
pub const ASH_XON: u8 = 0x11;
pub const ASH_XOFF: u8 = 0x13;
pub const ASH_SUBSTITUTE: u8 = 0x18;
pub const ASH_CANCEL: u8 = 0x1a;

pub const ASH_CONTROL_RESPONSE: u8 = 0x80;
pub const ASH_CONTROL_ASYNC_CALLBACK: u8 = 0x10;
pub const ASH_CONTROL_SYNC_CALLBACK: u8 = 0x08;

pub const EZSP_FRAME_FORMAT_1: u8 = 0x01;
pub const EZSP_HEADER_LEN: usize = 5;

pub const SEND_QUEUE_DEPTH: usize = 10;
const READ_CHUNK: usize = 4096;
const DEFAULT_RESET_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ASH_BUFFER: usize = 2048;

/// CRC-16/CCITT (XMODEM): polynomial 0x1021, initial value 0xffff.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn xor_sequence(data: &mut [u8]) {
    let mut rand = 0x42u8;
    for byte in data {
        *byte ^= rand;
        rand = if rand & 1 == 0 {
            rand >> 1
        } else {
            (rand >> 1) ^ 0xb8
        };
    }
}

fn stuff(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 4);
    for byte in bytes {
        if matches!(
            *byte,
            ASH_FLAG | ASH_ESCAPE | ASH_XON | ASH_XOFF | ASH_SUBSTITUTE | ASH_CANCEL
        ) {
            out.push(ASH_ESCAPE);
            out.push(*byte ^ 0x20);
        } else {
            out.push(*byte);
        }
    }
    out
}

fn unstuff(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut escaped = false;
    for byte in bytes {
        if escaped {
            let value = *byte ^ 0x20;
            if !matches!(
                value,
                ASH_FLAG | ASH_ESCAPE | ASH_XON | ASH_XOFF | ASH_SUBSTITUTE | ASH_CANCEL
            ) {
                return None;
            }
            out.push(value);
            escaped = false;
        } else if *byte == ASH_ESCAPE {
            escaped = true;
        } else {
            out.push(*byte);
        }
    }
    if escaped { None } else { Some(out) }
}

pub fn encode_ash_data(control: u8, payload: &[u8]) -> Vec<u8> {
    let mut randomized = payload.to_vec();
    xor_sequence(&mut randomized);
    let mut body = Vec::with_capacity(randomized.len() + 3);
    body.push(control);
    body.extend_from_slice(&randomized);
    let crc = crc16(&body);
    body.extend_from_slice(&crc.to_be_bytes());
    let mut out = stuff(&body);
    out.push(ASH_FLAG);
    out
}

pub fn encode_ash_control(control: u8, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(data.len() + 3);
    body.push(control);
    body.extend_from_slice(data);
    let crc = crc16(&body);
    body.extend_from_slice(&crc.to_be_bytes());
    let mut out = stuff(&body);
    out.push(ASH_FLAG);
    out
}

/// RST frame used before EZSP version negotiation.
pub fn reset_frame() -> Vec<u8> {
    encode_ash_control(0xc0, &[])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EzspFrameFormat {
    /// Legacy single-byte frame control and frame ID (EZSP v4).
    Legacy,
    /// Five-byte header with the 0xFF/0x00 legacy marker (EZSP v5-v7).
    V5,
    /// Extended two-byte frame control and frame ID (EZSP v8+).
    Extended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EzspVersion {
    pub protocol: u8,
    pub stack_version: u16,
}

impl EzspVersion {
    pub fn format(self) -> EzspFrameFormat {
        if self.protocol >= 8 {
            EzspFrameFormat::Extended
        } else if self.protocol >= 5 {
            EzspFrameFormat::V5
        } else {
            EzspFrameFormat::Legacy
        }
    }

    pub fn detail(self) -> String {
        format!(
            "EZSP v{} · stack 0x{:04x}",
            self.protocol, self.stack_version
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZigBeeSettings {
    pub open_attempts_interval: Duration,
    pub outgoing_max_ttl: Duration,
    pub ack_timeout: Duration,
    pub max_retries: u32,
    pub reset_timeout: Duration,
    pub write_timeout: Option<Duration>,
}

impl ZigBeeSettings {
    pub fn from_params(params: &ParamValues) -> Self {
        let open_attempts_interval = param_duration(params, SERVICE_PARAM_OPEN_ATTEMPTS_INTERVAL)
            .unwrap_or(Duration::from_secs(3));
        let open_attempts_interval = if open_attempts_interval.is_zero() {
            Duration::from_millis(100)
        } else {
            open_attempts_interval
        };

        let outgoing_max_ttl = param_duration(params, SERVICE_PARAM_OUTGOING_MAX_TTL)
            .unwrap_or(Duration::from_secs(10));

        let ack_timeout = param_duration(params, ZIGBEE_PARAM_ASH_ACK_TIMEOUT)
            .unwrap_or(Duration::from_millis(1_600))
            .clamp(Duration::from_millis(100), Duration::from_secs(10));
        let max_retries = param_u64(params, ZIGBEE_PARAM_ASH_MAX_RETRIES)
            .unwrap_or(5)
            .clamp(1, 20) as u32;
        let write_timeout =
            param_duration(params, crate::services::catalog::SERIAL_PARAM_WRITE_TIMEOUT)
                .filter(|timeout| !timeout.is_zero());

        Self {
            open_attempts_interval,
            outgoing_max_ttl,
            ack_timeout,
            max_retries,
            reset_timeout: DEFAULT_RESET_TIMEOUT,
            write_timeout,
        }
    }
}

struct Outgoing {
    id: Uuid,
    payload: Vec<u8>,
    ttl: Option<Duration>,
    created: Instant,
}

#[derive(Debug, Clone)]
struct Pending {
    id: Uuid,
    frame_num: u8,
    wire: Vec<u8>,
    attempts: u32,
    timeout: Duration,
    deadline: Instant,
}

pub struct ZigBeeHandle {
    key: ServiceKey,
    recorder: Arc<dyn MessageRecorder>,
    sender: mpsc::Sender<Outgoing>,
    status: watch::Receiver<ServiceStatus>,
    version: Arc<Mutex<Option<EzspVersion>>>,
    cancel: CancellationToken,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl ZigBeeHandle {
    pub(crate) fn start_with_status_events(
        key: ServiceKey,
        params: ParamValues,
        recorder: Arc<dyn MessageRecorder>,
        opener: Arc<dyn PortOpener>,
        parent_cancel: &CancellationToken,
        status_events: Option<broadcast::Sender<StatusEvent>>,
    ) -> Result<Self, ServiceError> {
        let settings = ZigBeeSettings::from_params(&params);
        let mut serial_settings = SerialSettings::from_params(&params)?;
        serial_settings.read_timeout = None;
        serial_settings.write_timeout = None;
        let (sender, receiver) = mpsc::channel(SEND_QUEUE_DEPTH);
        let (status_tx, status) = watch::channel(ServiceStatus {
            state: ServiceState::Starting,
            error: None,
            detail: None,
        });
        let version = Arc::new(Mutex::new(None));
        let cancel = parent_cancel.child_token();
        let task_version = version.clone();

        let join = tokio::spawn(run(
            key.clone(),
            settings,
            serial_settings,
            recorder.clone(),
            opener,
            receiver,
            status_tx,
            task_version,
            cancel.clone(),
        ));

        if let Some(status_events) = status_events {
            let mut status = status.clone();
            let key = key.clone();
            tokio::spawn(async move {
                let _ = status_events.send((key.clone(), status.borrow().clone()));
                while status.changed().await.is_ok() {
                    let _ = status_events.send((key.clone(), status.borrow().clone()));
                }
            });
        }

        Ok(Self {
            key,
            recorder,
            sender,
            status,
            version,
            cancel,
            join: Mutex::new(Some(join)),
        })
    }

    pub fn status(&self) -> ServiceStatus {
        self.status.borrow().clone()
    }

    pub fn send(&self, payload: Vec<u8>, ttl: Option<Duration>) -> Result<Message, ServiceError> {
        validate_canonical(&payload)?;
        if let Some(version) = *self.version.lock()
            && version.protocol < 8
            && payload.get(4).copied().unwrap_or_default() != 0
        {
            return Err(ServiceError::InvalidPayload);
        }

        let message = self
            .recorder
            .register(&self.key, &payload, MessageState::OutgoingPending);

        if self.status().state != ServiceState::Running {
            self.recorder
                .update_state(message.id, MessageState::OutgoingRejected);
            return Err(ServiceError::NotReady);
        }

        match self.sender.try_send(Outgoing {
            id: message.id,
            payload,
            ttl,
            created: Instant::now(),
        }) {
            Ok(()) => Ok(message),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.recorder
                    .update_state(message.id, MessageState::OutgoingRejected);
                Err(ServiceError::SendFailed)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.recorder
                    .update_state(message.id, MessageState::OutgoingRejected);
                Err(ServiceError::NotReady)
            }
        }
    }

    pub async fn stop(&self) {
        self.cancel.cancel();
        let join = self.join.lock().take();
        if let Some(join) = join {
            let _ = join.await;
        }
    }
}

impl Drop for ZigBeeHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn validate_canonical(payload: &[u8]) -> Result<(), ServiceError> {
    if payload.len() < EZSP_HEADER_LEN {
        return Err(ServiceError::InvalidPayload);
    }
    if payload[1] & ASH_CONTROL_RESPONSE != 0 {
        return Err(ServiceError::InvalidPayload);
    }
    if payload[2] & 0x03 != EZSP_FRAME_FORMAT_1 {
        return Err(ServiceError::InvalidPayload);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run(
    key: ServiceKey,
    settings: ZigBeeSettings,
    serial_settings: SerialSettings,
    recorder: Arc<dyn MessageRecorder>,
    opener: Arc<dyn PortOpener>,
    mut receiver: mpsc::Receiver<Outgoing>,
    status: watch::Sender<ServiceStatus>,
    version: Arc<Mutex<Option<EzspVersion>>>,
    cancel: CancellationToken,
) {
    loop {
        update_status(&status, ServiceState::Starting, None, None);

        let mut port =
            open_port(&key, &settings, &serial_settings, &opener, &cancel, &status).await;
        let Some(ref mut opened) = port else {
            return;
        };

        let handshake =
            handshake(opened, &settings, &cancel, Some((recorder.as_ref(), &key))).await;
        let result = match handshake {
            Ok(Some(result)) => result,
            Ok(None) => {
                update_status(
                    &status,
                    ServiceState::Error,
                    Some("no EZSP version response".to_string()),
                    None,
                );
                drop(port);
                if !sleep_or_cancel(settings.open_attempts_interval, &cancel).await {
                    purge(&mut receiver, recorder.as_ref());
                    update_status(&status, ServiceState::Stopped, None, None);
                    return;
                }
                continue;
            }
            Err(error) => {
                update_status(
                    &status,
                    ServiceState::Error,
                    Some(format!("EZSP handshake failed: {error}")),
                    None,
                );
                drop(port);
                if !sleep_or_cancel(settings.open_attempts_interval, &cancel).await {
                    purge(&mut receiver, recorder.as_ref());
                    update_status(&status, ServiceState::Stopped, None, None);
                    return;
                }
                continue;
            }
        };

        *version.lock() = Some(result.version);
        let format = result.format;
        update_status(
            &status,
            ServiceState::Running,
            None,
            Some(result.version.detail()),
        );
        tracing::info!(
            service = %key.entry,
            version = result.version.protocol,
            stack = format_args!("0x{:04x}", result.version.stack_version),
            "zigbee service ready"
        );

        let port = port.take().expect("port must be present");
        let (mut reader, mut writer) = tokio::io::split(port);
        let mut parser = AshParser::new();
        let mut rx_seq = result.rx_seq;
        let mut tx_seq = result.tx_seq;
        let mut pending: Option<Pending> = None;
        let mut read_buffer = [0_u8; READ_CHUNK];
        let mut reopen = false;

        while !reopen {
            // One select covers both link states. The retransmit deadline is
            // copied per iteration, and the send queue is polled only while no
            // frame awaits its acknowledgement, preserving one-at-a-time
            // transmission.
            let retry_deadline = pending.as_ref().map(|active| active.deadline);

            tokio::select! {
                _ = cancel.cancelled() => {
                    if let Some(pending) = pending.take() {
                        recorder.update_state(pending.id, MessageState::OutgoingFailed);
                    }
                    purge(&mut receiver, recorder.as_ref());
                    update_status(&status, ServiceState::Stopped, None, None);
                    return;
                }
                _ = async {
                    if let Some(deadline) = retry_deadline {
                        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    if let Some(active) = pending.take() {
                        if active.attempts >= settings.max_retries {
                            recorder.update_state(active.id, MessageState::OutgoingFailed);
                            tracing::warn!(
                                service = %key.entry,
                                attempts = active.attempts,
                                "zigbee ASH acknowledgement retries exhausted"
                            );
                            update_status(
                                &status,
                                ServiceState::Error,
                                Some("ASH acknowledgement retries exhausted".to_string()),
                                None,
                            );
                            reopen = true;
                        } else {
                            let timeout = (active.timeout * 2).min(Duration::from_secs(10));
                            let control = control_byte(active.frame_num, true, rx_seq);
                            let raw = encode_ash_data(control, &active.wire);
                            match write_with_timeout(&mut writer, &raw, &cancel, settings.write_timeout).await {
                                Ok(()) => {
                                    pending = Some(Pending {
                                        attempts: active.attempts + 1,
                                        timeout,
                                        deadline: Instant::now() + timeout,
                                        ..active
                                    });
                                }
                                Err(_) if cancel.is_cancelled() => {
                                    recorder.update_state(active.id, MessageState::OutgoingFailed);
                                    purge(&mut receiver, recorder.as_ref());
                                    update_status(&status, ServiceState::Stopped, None, None);
                                    return;
                                }
                                Err(error) => {
                                    recorder.update_state(active.id, MessageState::OutgoingFailed);
                                    tracing::warn!(service = %key.entry, error = %error, "zigbee ASH retry write failed");
                                    update_status(
                                        &status,
                                        ServiceState::Error,
                                        Some(format!("unable to write using transport: {error}")),
                                        None,
                                    );
                                    reopen = true;
                                }
                            }
                        }
                    }
                }
                result = reader.read(&mut read_buffer) => {
                    match read_result(result, &key, &status, &cancel) {
                        ReadOutcome::Data(count) => {
                            match dispatch_frames(
                                &read_buffer[..count],
                                &mut parser,
                                &mut writer,
                                &settings,
                                &key,
                                recorder.as_ref(),
                                &mut receiver,
                                &status,
                                &cancel,
                                format,
                                &mut rx_seq,
                                &mut pending,
                            )
                            .await
                            {
                                DispatchOutcome::Continue => {}
                                DispatchOutcome::Reopen => reopen = true,
                                DispatchOutcome::Stopped => return,
                            }
                        }
                        ReadOutcome::Reopen => reopen = true,
                        ReadOutcome::Stopped => return,
                    }
                }
                outgoing = receiver.recv(), if pending.is_none() => {
                    let Some(outgoing) = outgoing else {
                        purge(&mut receiver, recorder.as_ref());
                        update_status(&status, ServiceState::Stopped, None, None);
                        return;
                    };
                    let ttl = outgoing.ttl.unwrap_or(settings.outgoing_max_ttl);
                    if !ttl.is_zero() && outgoing.created.elapsed() > ttl {
                        recorder.update_state(outgoing.id, MessageState::OutgoingTimedOut);
                        continue;
                    }
                    let wire = match canonical_to_wire(format, &outgoing.payload) {
                        Ok(wire) => wire,
                        Err(()) => {
                            recorder.update_state(outgoing.id, MessageState::OutgoingFailed);
                            continue;
                        }
                    };
                    let control = control_byte(tx_seq, false, rx_seq);
                    let raw = encode_ash_data(control, &wire);
                    match write_with_timeout(&mut writer, &raw, &cancel, settings.write_timeout).await {
                        Ok(()) => {
                            pending = Some(Pending {
                                id: outgoing.id,
                                frame_num: tx_seq,
                                wire,
                                attempts: 1,
                                timeout: settings.ack_timeout,
                                deadline: Instant::now() + settings.ack_timeout,
                            });
                            tx_seq = (tx_seq + 1) & 0x07;
                        }
                        Err(_) if cancel.is_cancelled() => {
                            recorder.update_state(outgoing.id, MessageState::OutgoingFailed);
                            purge(&mut receiver, recorder.as_ref());
                            update_status(&status, ServiceState::Stopped, None, None);
                            return;
                        }
                        Err(error) => {
                            recorder.update_state(outgoing.id, MessageState::OutgoingFailed);
                            tracing::warn!(service = %key.entry, error = %error, "zigbee ASH write failed");
                            update_status(
                                &status,
                                ServiceState::Error,
                                Some(format!("unable to write using transport: {error}")),
                                None,
                            );
                            reopen = true;
                        }
                    }
                }
            }
        }

        if let Some(pending) = pending.take() {
            recorder.update_state(pending.id, MessageState::OutgoingFailed);
        }
        *version.lock() = None;
        drop(reader);
        drop(writer);
        tracing::warn!(service = %key.entry, "reopening the zigbee serial port");
    }
}

enum ReadOutcome {
    Data(usize),
    Reopen,
    Stopped,
}

fn read_result(
    result: io::Result<usize>,
    key: &ServiceKey,
    status: &watch::Sender<ServiceStatus>,
    cancel: &CancellationToken,
) -> ReadOutcome {
    match result {
        Err(error) => {
            if cancel.is_cancelled() {
                return ReadOutcome::Stopped;
            }
            tracing::warn!(service = %key.entry, error = %error, "unable to read from the serial port");
            update_status(
                status,
                ServiceState::Error,
                Some(format!("unable to read using transport: {error}")),
                None,
            );
            ReadOutcome::Reopen
        }
        Ok(0) => {
            if cancel.is_cancelled() {
                ReadOutcome::Stopped
            } else {
                tracing::warn!(service = %key.entry, "serial port reached EOF; reopening");
                ReadOutcome::Reopen
            }
        }
        Ok(count) => ReadOutcome::Data(count),
    }
}

enum DispatchOutcome {
    Continue,
    Reopen,
    Stopped,
}

/// Feeds received bytes into the ASH parser and dispatches every complete
/// frame through `handle_frame`. Shared by the single `select!` of the running
/// loop so both the sending and the acknowledgement-waiting states handle
/// frames identically.
#[allow(clippy::too_many_arguments)]
async fn dispatch_frames(
    bytes: &[u8],
    parser: &mut AshParser,
    writer: &mut (impl AsyncWrite + Unpin),
    settings: &ZigBeeSettings,
    key: &ServiceKey,
    recorder: &dyn MessageRecorder,
    receiver: &mut mpsc::Receiver<Outgoing>,
    status: &watch::Sender<ServiceStatus>,
    cancel: &CancellationToken,
    format: EzspFrameFormat,
    rx_seq: &mut u8,
    pending: &mut Option<Pending>,
) -> DispatchOutcome {
    parser.feed(bytes);
    while let Some(raw) = parser.next_frame() {
        match handle_frame(
            raw, writer, settings, key, recorder, cancel, format, rx_seq, pending,
        )
        .await
        {
            Ok(FrameOutcome::Continue) => {}
            Ok(FrameOutcome::Reopen) => return DispatchOutcome::Reopen,
            Err(error) => {
                if cancel.is_cancelled() {
                    // A frame can still be in flight here; record it before
                    // the purge marks every queued send as rejected.
                    if let Some(pending) = pending.take() {
                        recorder.update_state(pending.id, MessageState::OutgoingFailed);
                    }
                    purge(receiver, recorder);
                    update_status(status, ServiceState::Stopped, None, None);
                    return DispatchOutcome::Stopped;
                }
                tracing::warn!(service = %key.entry, error = %error, "zigbee ASH link error");
                update_status(
                    status,
                    ServiceState::Error,
                    Some(format!("ASH link error: {error}")),
                    None,
                );
                return DispatchOutcome::Reopen;
            }
        }
    }
    DispatchOutcome::Continue
}

async fn open_port(
    key: &ServiceKey,
    settings: &ZigBeeSettings,
    serial_settings: &SerialSettings,
    opener: &Arc<dyn PortOpener>,
    cancel: &CancellationToken,
    status: &watch::Sender<ServiceStatus>,
) -> Option<Port> {
    loop {
        if cancel.is_cancelled() {
            update_status(status, ServiceState::Stopped, None, None);
            return None;
        }
        let opened = tokio::select! {
            _ = cancel.cancelled() => {
                update_status(status, ServiceState::Stopped, None, None);
                return None;
            }
            result = opener.open(key.entry.clone(), serial_settings.clone()) => result,
        };
        match opened {
            Ok(port) => return Some(port),
            Err(error) => {
                tracing::warn!(service = %key.entry, error = %error, "unable to open the serial port");
                update_status(
                    status,
                    ServiceState::Error,
                    Some(format!("unable to open transport: {error}")),
                    None,
                );
                if !sleep_or_cancel(settings.open_attempts_interval, cancel).await {
                    update_status(status, ServiceState::Stopped, None, None);
                    return None;
                }
            }
        }
    }
}

async fn sleep_or_cancel(duration: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(duration) => true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameOutcome {
    Continue,
    Reopen,
}

#[allow(clippy::too_many_arguments)]
async fn handle_frame(
    raw: RawAshFrame,
    writer: &mut (impl AsyncWrite + Unpin),
    settings: &ZigBeeSettings,
    key: &ServiceKey,
    recorder: &dyn MessageRecorder,
    cancel: &CancellationToken,
    format: EzspFrameFormat,
    rx_seq: &mut u8,
    pending: &mut Option<Pending>,
) -> io::Result<FrameOutcome> {
    match &raw.frame {
        AshFrame::Data {
            frm_num,
            retransmit,
            ack_num,
            data,
            ..
        } => {
            acknowledge_pending(*ack_num, recorder, pending);
            if frm_num == rx_seq {
                *rx_seq = (frm_num + 1) & 0x07;
                send_control(
                    writer,
                    0x80 | *rx_seq,
                    recorder,
                    key,
                    cancel,
                    settings.write_timeout,
                )
                .await?;
                if let Some(canonical) = wire_to_canonical(format, data) {
                    recorder.register(key, &canonical, MessageState::Incoming);
                }
            } else if *retransmit {
                send_control(
                    writer,
                    0x80 | *rx_seq,
                    recorder,
                    key,
                    cancel,
                    settings.write_timeout,
                )
                .await?;
            } else {
                send_control(
                    writer,
                    0xa0 | *rx_seq,
                    recorder,
                    key,
                    cancel,
                    settings.write_timeout,
                )
                .await?;
            }
        }
        AshFrame::Ack { ack_num, nready } => {
            recorder.register(key, &raw.raw, MessageState::Incoming);
            tracing::debug!(
                service = %key.entry,
                ack = ack_num,
                nready,
                "zigbee ASH ACK"
            );
            acknowledge_pending(*ack_num, recorder, pending);
        }
        AshFrame::Nak { ack_num, nready } => {
            recorder.register(key, &raw.raw, MessageState::Incoming);
            tracing::debug!(
                service = %key.entry,
                ack = ack_num,
                nready,
                "zigbee ASH NAK"
            );
            if let Some(active) = pending.take() {
                if active.attempts >= settings.max_retries
                    || ack_num.wrapping_sub(1) & 0x07 != active.frame_num
                {
                    recorder.update_state(active.id, MessageState::OutgoingFailed);
                    return Ok(FrameOutcome::Reopen);
                }
                let timeout = (active.timeout * 2).min(Duration::from_secs(10));
                let control = control_byte(active.frame_num, true, *rx_seq);
                let raw = encode_ash_data(control, &active.wire);
                match write_with_timeout(writer, &raw, cancel, settings.write_timeout).await {
                    Ok(()) => {
                        *pending = Some(Pending {
                            attempts: active.attempts + 1,
                            timeout,
                            deadline: Instant::now() + timeout,
                            ..active
                        });
                    }
                    Err(error) => {
                        recorder.update_state(active.id, MessageState::OutgoingFailed);
                        return Err(error);
                    }
                }
            }
        }
        AshFrame::Rst => {
            recorder.register(key, &raw.raw, MessageState::Incoming);
            tracing::warn!(service = %key.entry, "zigbee NCP sent ASH RST");
            return Ok(FrameOutcome::Reopen);
        }
        AshFrame::Rstack { reset_code, .. } => {
            recorder.register(key, &raw.raw, MessageState::Incoming);
            tracing::warn!(
                service = %key.entry,
                code = reset_code,
                "zigbee NCP sent ASH RSTACK"
            );
            return Ok(FrameOutcome::Reopen);
        }
        AshFrame::Error { reset_code } => {
            recorder.register(key, &raw.raw, MessageState::Incoming);
            tracing::error!(
                service = %key.entry,
                code = reset_code,
                "zigbee NCP entered the failed state"
            );
            return Ok(FrameOutcome::Reopen);
        }
        AshFrame::Unknown { control } => {
            tracing::warn!(
                service = %key.entry,
                control = format_args!("0x{control:02x}"),
                "discarding an unknown ASH frame"
            );
        }
    }
    Ok(FrameOutcome::Continue)
}

fn acknowledge_pending(ack_num: u8, recorder: &dyn MessageRecorder, pending: &mut Option<Pending>) {
    if let Some(active) = pending.as_ref()
        && ack_num.wrapping_sub(1) & 0x07 == active.frame_num
    {
        recorder.update_state(active.id, MessageState::Outgoing);
        *pending = None;
    }
}

async fn send_control(
    writer: &mut (impl AsyncWrite + Unpin),
    control: u8,
    recorder: &dyn MessageRecorder,
    key: &ServiceKey,
    cancel: &CancellationToken,
    write_timeout: Option<Duration>,
) -> io::Result<()> {
    let raw = encode_ash_control(control, &[]);
    write_with_timeout(writer, &raw, cancel, write_timeout).await?;
    recorder.register(key, &raw, MessageState::Outgoing);
    Ok(())
}

async fn write_with_timeout(
    writer: &mut (impl AsyncWrite + Unpin),
    payload: &[u8],
    cancel: &CancellationToken,
    timeout: Option<Duration>,
) -> io::Result<()> {
    let write = async {
        tokio::select! {
            _ = cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "operation cancelled")),
            result = writer.write_all(payload) => result,
        }
    };
    match timeout {
        Some(timeout) => tokio::time::timeout(timeout, write)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "serial write timed out"))?,
        None => write.await,
    }
}

fn control_byte(frame_num: u8, retransmit: bool, ack_num: u8) -> u8 {
    ((frame_num & 0x07) << 4) | (u8::from(retransmit) << 3) | (ack_num & 0x07)
}

fn canonical_to_wire(format: EzspFrameFormat, canonical: &[u8]) -> Result<Vec<u8>, ()> {
    if canonical.len() < EZSP_HEADER_LEN {
        return Err(());
    }
    let seq = canonical[0];
    let control = canonical[1];
    let high = canonical[2];
    if high & 0x03 != EZSP_FRAME_FORMAT_1 {
        return Err(());
    }
    let command = u16::from_le_bytes([canonical[3], canonical[4]]);
    let params = &canonical[5..];
    let mut wire = Vec::with_capacity(canonical.len());
    wire.push(seq);
    wire.push(control);
    match format {
        EzspFrameFormat::Legacy => {
            let command = u8::try_from(command).map_err(|_| ())?;
            wire.push(command);
            wire.extend_from_slice(params);
        }
        EzspFrameFormat::V5 => {
            let command = u8::try_from(command).map_err(|_| ())?;
            wire.push(0xff);
            wire.push(0x00);
            wire.push(command);
            wire.extend_from_slice(params);
        }
        EzspFrameFormat::Extended => {
            wire.push(EZSP_FRAME_FORMAT_1);
            wire.extend_from_slice(&command.to_le_bytes());
            wire.extend_from_slice(params);
        }
    }
    Ok(wire)
}

fn wire_to_canonical(format: EzspFrameFormat, wire: &[u8]) -> Option<Vec<u8>> {
    let (seq, control, command, params) = match format {
        EzspFrameFormat::Legacy => {
            if wire.len() < 3 {
                return None;
            }
            (wire[0], wire[1], u16::from(wire[2]), &wire[3..])
        }
        EzspFrameFormat::V5 => {
            if wire.len() < 5 {
                return None;
            }
            (wire[0], wire[1], u16::from(wire[4]), &wire[5..])
        }
        EzspFrameFormat::Extended => {
            if wire.len() < 5 {
                return None;
            }
            (
                wire[0],
                wire[1],
                u16::from_le_bytes([wire[3], wire[4]]),
                &wire[5..],
            )
        }
    };
    let mut canonical = Vec::with_capacity(EZSP_HEADER_LEN + params.len());
    canonical.push(seq);
    canonical.push(control);
    canonical.push(EZSP_FRAME_FORMAT_1);
    canonical.extend_from_slice(&command.to_le_bytes());
    canonical.extend_from_slice(params);
    Some(canonical)
}

/// True when the payload is a complete, valid ASH link frame (not DATA).
pub fn is_link_frame(payload: &[u8]) -> bool {
    let mut parser = AshParser::new();
    parser.feed(payload);
    let mut frames = Vec::new();
    while let Some(frame) = parser.next_frame() {
        frames.push(frame);
    }
    frames.len() == 1 && frames[0].raw == payload && frames[0].frame.is_link()
}

fn purge(receiver: &mut mpsc::Receiver<Outgoing>, recorder: &dyn MessageRecorder) {
    while let Ok(outgoing) = receiver.try_recv() {
        recorder.update_state(outgoing.id, MessageState::OutgoingRejected);
    }
}

fn update_status(
    status: &watch::Sender<ServiceStatus>,
    state: ServiceState,
    error: Option<String>,
    detail: Option<String>,
) {
    let _ = status.send(ServiceStatus {
        state,
        error,
        detail,
    });
}

// ---------------------------------------------------------------------------
// ASH parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum AshFrame {
    Data {
        control: u8,
        frm_num: u8,
        retransmit: bool,
        ack_num: u8,
        data: Vec<u8>,
    },
    Ack {
        ack_num: u8,
        nready: bool,
    },
    Nak {
        ack_num: u8,
        nready: bool,
    },
    Rst,
    Rstack {
        version: u8,
        reset_code: u8,
    },
    Error {
        reset_code: u8,
    },
    Unknown {
        control: u8,
    },
}

impl AshFrame {
    fn is_link(&self) -> bool {
        matches!(
            self,
            Self::Ack { .. }
                | Self::Nak { .. }
                | Self::Rst
                | Self::Rstack { .. }
                | Self::Error { .. }
        )
    }
}

#[derive(Debug, Clone)]
struct RawAshFrame {
    frame: AshFrame,
    raw: Vec<u8>,
}

struct AshParser {
    buffer: Vec<u8>,
    discard_until_flag: bool,
    pending: VecDeque<RawAshFrame>,
}

impl AshParser {
    fn new() -> Self {
        Self {
            buffer: Vec::new(),
            discard_until_flag: false,
            pending: VecDeque::new(),
        }
    }

    fn feed(&mut self, data: &[u8]) {
        for byte in data {
            if self.discard_until_flag {
                if *byte == ASH_FLAG {
                    self.discard_until_flag = false;
                }
                continue;
            }
            match *byte {
                ASH_FLAG => {
                    if !self.buffer.is_empty() {
                        let mut raw = std::mem::take(&mut self.buffer);
                        raw.push(ASH_FLAG);
                        if let Some(frame) = parse_ash_frame(&raw) {
                            self.pending.push_back(frame);
                        }
                    }
                }
                ASH_CANCEL => self.buffer.clear(),
                ASH_SUBSTITUTE => {
                    self.buffer.clear();
                    self.discard_until_flag = true;
                }
                ASH_XON | ASH_XOFF => {}
                byte => {
                    self.buffer.push(byte);
                    if self.buffer.len() > MAX_ASH_BUFFER {
                        self.buffer.clear();
                        self.discard_until_flag = true;
                    }
                }
            }
        }
    }

    fn next_frame(&mut self) -> Option<RawAshFrame> {
        self.pending.pop_front()
    }
}

fn parse_ash_frame(raw: &[u8]) -> Option<RawAshFrame> {
    if raw.last() != Some(&ASH_FLAG) {
        return None;
    }
    let unstuffed = unstuff(&raw[..raw.len() - 1])?;
    if unstuffed.len() < 3 {
        return None;
    }
    let crc_index = unstuffed.len() - 2;
    let expected = crc16(&unstuffed[..crc_index]);
    let actual = u16::from_be_bytes([unstuffed[crc_index], unstuffed[crc_index + 1]]);
    if expected != actual {
        return None;
    }
    let control = unstuffed[0];
    let data = &unstuffed[1..crc_index];
    let frame = match control & 0xe0 {
        0x80 => AshFrame::Ack {
            ack_num: control & 0x07,
            nready: control & 0x08 != 0,
        },
        0xa0 => AshFrame::Nak {
            ack_num: control & 0x07,
            nready: control & 0x08 != 0,
        },
        0xc0 => match control {
            0xc0 if data.is_empty() => AshFrame::Rst,
            0xc1 if data.len() == 2 => AshFrame::Rstack {
                version: data[0],
                reset_code: data[1],
            },
            0xc2 if data.len() == 2 => AshFrame::Error {
                reset_code: data[1],
            },
            other => AshFrame::Unknown { control: other },
        },
        _ => {
            if control & ASH_CONTROL_RESPONSE == 0 {
                let mut payload = data.to_vec();
                xor_sequence(&mut payload);
                AshFrame::Data {
                    control,
                    frm_num: (control & 0x70) >> 4,
                    retransmit: control & 0x08 != 0,
                    ack_num: control & 0x07,
                    data: payload,
                }
            } else {
                AshFrame::Unknown { control }
            }
        }
    };
    Some(RawAshFrame {
        frame,
        raw: raw.to_vec(),
    })
}

// ---------------------------------------------------------------------------
// Handshake and discovery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct HandshakeResult {
    version: EzspVersion,
    format: EzspFrameFormat,
    rx_seq: u8,
    tx_seq: u8,
}

async fn handshake(
    port: &mut (impl AsyncRead + AsyncWrite + Unpin),
    settings: &ZigBeeSettings,
    cancel: &CancellationToken,
    recorder: Option<(&dyn MessageRecorder, &ServiceKey)>,
) -> io::Result<Option<HandshakeResult>> {
    let reset = reset_frame();
    write_with_timeout(port, &reset, cancel, settings.write_timeout).await?;
    if let Some((recorder, key)) = recorder {
        recorder.register(key, &reset, MessageState::Outgoing);
    }

    let mut parser = AshParser::new();
    let mut rx_seq = 0;
    let reset_deadline = Instant::now() + settings.reset_timeout;
    let mut saw_reset = false;
    while !saw_reset {
        let Some(frame) = read_frame_until(port, &mut parser, reset_deadline, cancel).await? else {
            return Ok(None);
        };
        if let Some((recorder, key)) = recorder {
            match &frame.frame {
                AshFrame::Rstack { reset_code, .. } => {
                    recorder.register(key, &frame.raw, MessageState::Incoming);
                    tracing::debug!(code = reset_code, "zigbee ASH reset acknowledged");
                    saw_reset = true;
                }
                AshFrame::Rst | AshFrame::Error { .. } => {
                    recorder.register(key, &frame.raw, MessageState::Incoming);
                }
                _ => {}
            }
        } else if matches!(frame.frame, AshFrame::Rstack { .. }) {
            saw_reset = true;
        }
    }

    let version = send_version_request(
        port,
        settings,
        cancel,
        &mut parser,
        EzspFrameFormat::Legacy,
        0,
        0,
        4,
        false,
        &mut rx_seq,
    )
    .await?;
    let Some((version, wire_format)) = version else {
        return Ok(None);
    };

    if version.protocol >= 5 {
        let requested = version.protocol;
        let second = send_version_request(
            port,
            settings,
            cancel,
            &mut parser,
            wire_format,
            1,
            rx_seq,
            requested,
            true,
            &mut rx_seq,
        )
        .await?;
        let Some((second_version, _)) = second else {
            return Ok(None);
        };
        return Ok(Some(HandshakeResult {
            version: second_version,
            format: second_version.format(),
            rx_seq,
            tx_seq: 2,
        }));
    }

    Ok(Some(HandshakeResult {
        version,
        format: EzspFrameFormat::Legacy,
        rx_seq,
        tx_seq: 1,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn send_version_request(
    port: &mut (impl AsyncRead + AsyncWrite + Unpin),
    settings: &ZigBeeSettings,
    cancel: &CancellationToken,
    parser: &mut AshParser,
    format: EzspFrameFormat,
    ash_frame_num: u8,
    ack_num: u8,
    desired: u8,
    use_format: bool,
    rx_seq: &mut u8,
) -> io::Result<Option<(EzspVersion, EzspFrameFormat)>> {
    let wire = if use_format {
        match format {
            EzspFrameFormat::Legacy => vec![0, 0, 0, desired],
            EzspFrameFormat::V5 => vec![1, 0, 0xff, 0x00, 0x00, desired],
            EzspFrameFormat::Extended => vec![1, 0, 0x01, 0x00, 0x00, desired],
        }
    } else {
        vec![0, 0, 0, desired]
    };
    let control = control_byte(ash_frame_num, false, ack_num);
    let raw = encode_ash_data(control, &wire);
    write_with_timeout(port, &raw, cancel, settings.write_timeout).await?;

    let deadline = Instant::now() + settings.ack_timeout.max(Duration::from_secs(2));
    loop {
        let Some(frame) = read_frame_until(port, parser, deadline, cancel).await? else {
            return Ok(None);
        };
        match frame.frame {
            AshFrame::Data { frm_num, data, .. } => {
                let next = (frm_num + 1) & 0x07;
                let ack = encode_ash_control(0x80 | next, &[]);
                write_with_timeout(port, &ack, cancel, settings.write_timeout).await?;
                *rx_seq = next;
                let (command, params) = if use_format {
                    match format {
                        EzspFrameFormat::Legacy => {
                            if data.len() < 3 {
                                continue;
                            }
                            (u16::from(data[2]), &data[3..])
                        }
                        EzspFrameFormat::V5 => {
                            if data.len() < 5 {
                                continue;
                            }
                            (u16::from(data[4]), &data[5..])
                        }
                        EzspFrameFormat::Extended => {
                            if data.len() < 5 {
                                continue;
                            }
                            (u16::from_le_bytes([data[3], data[4]]), &data[5..])
                        }
                    }
                } else {
                    if data.len() < 3 {
                        continue;
                    }
                    (u16::from(data[2]), &data[3..])
                };
                if command != 0 || params.len() < 4 {
                    continue;
                }
                let protocol = params[0];
                let stack_version = u16::from_le_bytes([params[2], params[3]]);
                if !(4..=19).contains(&protocol) {
                    return Ok(None);
                }
                return Ok(Some((
                    EzspVersion {
                        protocol,
                        stack_version,
                    },
                    if protocol >= 8 {
                        EzspFrameFormat::Extended
                    } else if protocol >= 5 {
                        EzspFrameFormat::V5
                    } else {
                        EzspFrameFormat::Legacy
                    },
                )));
            }
            AshFrame::Ack { .. } | AshFrame::Nak { .. } => {}
            AshFrame::Rst | AshFrame::Rstack { .. } | AshFrame::Error { .. } => {
                return Ok(None);
            }
            AshFrame::Unknown { .. } => {}
        }
    }
}

async fn read_frame_until(
    port: &mut (impl AsyncRead + Unpin),
    parser: &mut AshParser,
    deadline: Instant,
    cancel: &CancellationToken,
) -> io::Result<Option<RawAshFrame>> {
    loop {
        if let Some(frame) = parser.next_frame() {
            return Ok(Some(frame));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let mut chunk = [0_u8; 256];
        let result = tokio::select! {
            _ = cancel.cancelled() => return Ok(None),
            result = tokio::time::timeout(remaining, port.read(&mut chunk)) => result,
        };
        let count = match result {
            Err(_) => return Ok(None),
            Ok(Err(error)) => return Err(error),
            Ok(Ok(0)) => return Ok(None),
            Ok(Ok(count)) => count,
        };
        parser.feed(&chunk[..count]);
    }
}

/// Probes a serial stream for an EZSP coordinator.
pub async fn probe_version(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    timeout: Duration,
) -> io::Result<Option<EzspVersion>> {
    let per_step = (timeout / 2).max(Duration::from_millis(100));
    let settings = ZigBeeSettings {
        open_attempts_interval: Duration::ZERO,
        outgoing_max_ttl: Duration::ZERO,
        ack_timeout: per_step,
        max_retries: 1,
        reset_timeout: per_step,
        write_timeout: Some(timeout),
    };
    let cancel = CancellationToken::new();
    let result = handshake(stream, &settings, &cancel, None).await?;
    Ok(result.map(|result| result.version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::MessageStore;
    use crate::services::serial::Port;
    use crate::services::test_support::MockPortOpener;
    use hodeco_contract::domain::{MessageOrder, ServiceState};

    #[test]
    fn crc_matches_known_ash_vectors() {
        assert_eq!(crc16(&[0xc0]), 0x38bc);
        assert_eq!(crc16(&[0x81]), 0x6059);
        assert_eq!(crc16(&[0xa1]), 0x443b);
        assert_eq!(crc16(&[0xc1, 0x02, 0x0b]), 0x0a52);
    }

    #[test]
    fn parser_round_trips_control_frames() {
        let ack = encode_ash_control(0x81, &[]);
        assert_eq!(ack, [0x81, 0x60, 0x59, 0x7e]);
        assert!(is_link_frame(&ack));
        assert!(!is_link_frame(&[0, 0, 0x01, 0x18, 0x00]));

        let mut parser = AshParser::new();
        parser.feed(&ack);
        let frame = parser.next_frame().expect("ack frame");
        assert!(matches!(
            frame.frame,
            AshFrame::Ack {
                ack_num: 1,
                nready: false
            }
        ));
    }

    #[test]
    fn canonical_and_wire_round_trip() {
        let canonical = vec![7, 0, 1, 0x34, 0x12, 1, 2, 3];
        let wire = canonical_to_wire(EzspFrameFormat::Extended, &canonical).unwrap();
        assert_eq!(wire, canonical);
        assert_eq!(
            wire_to_canonical(EzspFrameFormat::Extended, &wire).unwrap(),
            canonical
        );

        let legacy = vec![7, 0, 1, 0x34, 0x00, 1, 2, 3];
        let wire = canonical_to_wire(EzspFrameFormat::Legacy, &legacy).unwrap();
        assert_eq!(wire, vec![7, 0, 0x34, 1, 2, 3]);
        assert_eq!(
            wire_to_canonical(EzspFrameFormat::Legacy, &wire).unwrap(),
            legacy
        );
    }

    async fn read_raw_frame(stream: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
        let mut out = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            stream.read_exact(&mut byte).await.unwrap();
            out.push(byte[0]);
            if byte[0] == ASH_FLAG {
                return out;
            }
        }
    }

    #[tokio::test]
    async fn handle_sends_and_receives_frames() {
        use crate::messages::MessageFilter;

        let (device, mut ncp) = tokio::io::duplex(4096);
        let fake = tokio::spawn(async move {
            assert_eq!(read_raw_frame(&mut ncp).await, reset_frame());
            ncp.write_all(&encode_ash_control(0xc1, &[0x02, 0x0b]))
                .await
                .unwrap();
            let request = read_raw_frame(&mut ncp).await;
            let Some(RawAshFrame {
                frame: AshFrame::Data {
                    frm_num: _frm_num, ..
                },
                ..
            }) = parse_ash_frame(&request)
            else {
                panic!("expected version request DATA frame");
            };
            ncp.write_all(&encode_ash_data(
                0x01,
                &[0x00, 0x80, 0x00, 0x04, 0x02, 0x10, 0x54],
            ))
            .await
            .unwrap();
            let _ = read_raw_frame(&mut ncp).await;

            let outgoing = read_raw_frame(&mut ncp).await;
            let Some(RawAshFrame {
                frame:
                    AshFrame::Data {
                        frm_num: outgoing_frame,
                        ack_num,
                        ..
                    },
                ..
            }) = parse_ash_frame(&outgoing)
            else {
                panic!("expected outgoing EZSP DATA frame");
            };
            assert_eq!(outgoing_frame, 1);
            assert_eq!(ack_num, 1);
            ncp.write_all(&encode_ash_control(
                0x80 | ((outgoing_frame + 1) & 0x07),
                &[],
            ))
            .await
            .unwrap();

            // networkState response: seq 1, response bit, command 0x18, joined.
            ncp.write_all(&encode_ash_data(0x12, &[0x01, 0x80, 0x18, 0x02]))
                .await
                .unwrap();
            let _ = read_raw_frame(&mut ncp).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let store = Arc::new(MessageStore::new(1024 * 1024));
        let opener = MockPortOpener::new(vec![Ok(Box::new(device) as Port)]);
        let cancel = CancellationToken::new();
        let handle = start_handle(ParamValues::new(), store.clone(), opener, &cancel);
        wait_running(&handle).await;
        assert_eq!(handle.status().state, ServiceState::Running);
        assert_eq!(
            handle.status().detail.as_deref(),
            Some("EZSP v4 · stack 0x5410")
        );

        let message = handle
            .send(vec![0x01, 0x00, 0x01, 0x18, 0x00], None)
            .expect("send");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if store
                    .get(message.id)
                    .is_some_and(|message| message.state == MessageState::Outgoing)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("message was not acknowledged");

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let page = store.list(
                    None,
                    None,
                    MessageOrder::OldestFirst,
                    64,
                    &MessageFilter {
                        states: vec![MessageState::Incoming],
                        ..Default::default()
                    },
                );
                if page.messages.iter().any(|message| {
                    message.payload.as_slice() == [0x01, 0x80, 0x01, 0x18, 0x00, 0x02]
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("response was not recorded");

        let all = store.list(
            None,
            None,
            MessageOrder::OldestFirst,
            128,
            &MessageFilter::default(),
        );
        assert!(all.messages.iter().any(|message| {
            message.payload.as_slice() == encode_ash_control(0x82, &[]).as_slice()
        }));

        handle.stop().await;
        cancel.cancel();
        fake.abort();
    }

    fn start_handle(
        params: ParamValues,
        store: Arc<MessageStore>,
        opener: Arc<dyn PortOpener>,
        cancel: &CancellationToken,
    ) -> ZigBeeHandle {
        ZigBeeHandle::start_with_status_events(
            ServiceKey {
                protocol: "zigbee".to_string(),
                transport: "serial".to_string(),
                entry: "TEST".to_string(),
            },
            params,
            store,
            opener,
            cancel,
            None,
        )
        .expect("test service handle")
    }

    async fn wait_running(handle: &ZigBeeHandle) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if handle.status().state == ServiceState::Running {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("service did not start");
    }

    #[tokio::test]
    async fn handshake_negotiates_ezsp_v4() {
        let (mut host, mut ncp) = tokio::io::duplex(2048);
        let fake = tokio::spawn(async move {
            assert_eq!(read_raw_frame(&mut ncp).await, reset_frame());
            ncp.write_all(&encode_ash_control(0xc1, &[0x02, 0x0b]))
                .await
                .unwrap();
            assert_eq!(
                read_raw_frame(&mut ncp).await,
                encode_ash_data(0x00, &[0x00, 0x00, 0x00, 0x04])
            );
            ncp.write_all(&encode_ash_data(
                0x01,
                &[0x00, 0x80, 0x00, 0x04, 0x02, 0x10, 0x54],
            ))
            .await
            .unwrap();
            assert_eq!(
                read_raw_frame(&mut ncp).await,
                encode_ash_control(0x81, &[])
            );
        });

        let settings = ZigBeeSettings {
            open_attempts_interval: Duration::from_millis(10),
            outgoing_max_ttl: Duration::ZERO,
            ack_timeout: Duration::from_secs(1),
            max_retries: 1,
            reset_timeout: Duration::from_secs(1),
            write_timeout: Some(Duration::from_secs(1)),
        };
        let result = handshake(&mut host, &settings, &CancellationToken::new(), None)
            .await
            .unwrap()
            .expect("handshake");
        assert_eq!(result.version.protocol, 4);
        assert_eq!(result.version.stack_version, 0x5410);
        assert_eq!(result.format, EzspFrameFormat::Legacy);
        assert_eq!(result.tx_seq, 1);
        fake.await.unwrap();
    }
}
