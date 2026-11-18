use super::ServiceError;
use super::catalog::{ZWAVE_PARAM_OPEN_ATTEMPTS_INTERVAL, ZWAVE_PARAM_OUTGOING_MAX_TTL};
use super::serial::{Port, PortOpener, SerialSettings};
use crate::messages::MessageRecorder;
use hodeco_contract::domain::{
    Message, MessageState, ParamValues, ServiceKey, ServiceState, ServiceStatus, param_duration,
};
use parking_lot::Mutex;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const FRAME_SOF: u8 = 0x01;
pub const FRAME_ACK: u8 = 0x06;
pub const FRAME_NAK: u8 = 0x15;
pub const FRAME_CAN: u8 = 0x18;

pub const FRAME_REQUEST: u8 = 0;
pub const FRAME_RESPONSE: u8 = 1;

pub const FRAME_MIN_LENGTH: usize = 3;
pub const FRAME_MAX_LENGTH: usize = 253;

pub const ZW_VERSION: u8 = 0x15;

pub const REPLY_TIMEOUT: Duration = Duration::from_millis(1_500);
pub const SEND_QUEUE_DEPTH: usize = 10;
const READ_CHUNK: usize = 4096;
const MAX_PENDING_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameValidation {
    Ok,
    Incomplete,
    NoSof,
    WrongLength,
    WrongChecksum,
}

pub fn checksum(source: &[u8]) -> u8 {
    source.iter().fold(0xff, |result, byte| result ^ byte)
}

pub fn data_frame(frame_type: u8, body: &[u8]) -> Vec<u8> {
    let length =
        u8::try_from(body.len() + 2).expect("z-wave frame body must fit into a single length byte");
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.push(length);
    frame.push(frame_type);
    frame.extend_from_slice(body);
    let checksum = checksum(&frame);
    frame.push(checksum);
    frame.insert(0, FRAME_SOF);
    frame
}

pub fn data_request(body: &[u8]) -> Vec<u8> {
    data_frame(FRAME_REQUEST, body)
}

pub fn data_response(body: &[u8]) -> Vec<u8> {
    data_frame(FRAME_RESPONSE, body)
}

pub fn unpack_data_frame(frame: &[u8]) -> Option<&[u8]> {
    if frame.len() <= 3 {
        return None;
    }
    Some(&frame[2..frame.len() - 1])
}

pub fn unpack_request(frame: &[u8]) -> Option<&[u8]> {
    if frame.len() <= 4 || frame[2] != FRAME_REQUEST {
        return None;
    }
    Some(&frame[3..frame.len() - 1])
}

pub fn unpack_response(frame: &[u8]) -> Option<&[u8]> {
    if frame.len() <= 4 || frame[2] != FRAME_RESPONSE {
        return None;
    }
    Some(&frame[3..frame.len() - 1])
}

pub fn validate_data_frame(frame: &[u8]) -> (FrameValidation, usize) {
    if frame.first().is_some_and(|byte| *byte != FRAME_SOF) {
        return (FrameValidation::NoSof, 0);
    }
    if frame.len() < 2 {
        return (FrameValidation::Incomplete, 0);
    }

    let frame_length = frame[1] as usize;
    if !(FRAME_MIN_LENGTH..=FRAME_MAX_LENGTH).contains(&frame_length) {
        return (FrameValidation::WrongLength, 0);
    }
    if frame.len() < frame_length + 2 {
        return (FrameValidation::Incomplete, 0);
    }
    if checksum(&frame[1..frame_length + 1]) != frame[frame_length + 1] {
        return (FrameValidation::WrongChecksum, frame_length + 2);
    }

    (FrameValidation::Ok, frame_length + 2)
}

pub fn version_request() -> Vec<u8> {
    data_request(&[ZW_VERSION])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZWaveSettings {
    pub open_attempts_interval: Duration,
    pub outgoing_max_ttl: Duration,
}

impl ZWaveSettings {
    pub fn from_params(params: &ParamValues) -> Self {
        let open_attempts_interval = param_duration(params, ZWAVE_PARAM_OPEN_ATTEMPTS_INTERVAL)
            .unwrap_or(Duration::from_secs(3));
        let open_attempts_interval = if open_attempts_interval.is_zero() {
            Duration::from_millis(100)
        } else {
            open_attempts_interval
        };

        let outgoing_max_ttl =
            param_duration(params, ZWAVE_PARAM_OUTGOING_MAX_TTL).unwrap_or(Duration::from_secs(10));
        let outgoing_max_ttl = if outgoing_max_ttl.is_zero() {
            Duration::ZERO
        } else if outgoing_max_ttl < Duration::from_secs(1) {
            Duration::from_secs(1)
        } else {
            outgoing_max_ttl
        };

        Self {
            open_attempts_interval,
            outgoing_max_ttl,
        }
    }
}

struct Outgoing {
    id: Uuid,
    payload: Vec<u8>,
    ttl: Option<Duration>,
    created: Instant,
}

pub type StatusEvent = (ServiceKey, ServiceStatus);

pub struct ZWaveHandle {
    key: ServiceKey,
    recorder: Arc<dyn MessageRecorder>,
    sender: mpsc::Sender<Outgoing>,
    status: watch::Receiver<ServiceStatus>,
    cancel: CancellationToken,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl ZWaveHandle {
    pub(crate) fn start_with_status_events(
        key: ServiceKey,
        params: ParamValues,
        recorder: Arc<dyn MessageRecorder>,
        opener: Arc<dyn PortOpener>,
        parent_cancel: &CancellationToken,
        status_events: Option<broadcast::Sender<StatusEvent>>,
    ) -> Result<Self, ServiceError> {
        let settings = ZWaveSettings::from_params(&params);
        let mut serial_settings = SerialSettings::from_params(&params)?;
        serial_settings.read_timeout = None;
        serial_settings.write_timeout = None;
        let (sender, receiver) = mpsc::channel(SEND_QUEUE_DEPTH);
        let (status_tx, status) = watch::channel(ServiceStatus {
            state: ServiceState::Starting,
            error: None,
        });
        let cancel = parent_cancel.child_token();

        let join = tokio::spawn(run(
            key.clone(),
            settings,
            serial_settings,
            recorder.clone(),
            opener,
            receiver,
            status_tx,
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
            cancel,
            join: Mutex::new(Some(join)),
        })
    }

    pub fn status(&self) -> ServiceStatus {
        self.status.borrow().clone()
    }

    pub fn send(&self, payload: Vec<u8>, ttl: Option<Duration>) -> Result<Message, ServiceError> {
        if payload.is_empty() || payload.len() > 255 {
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

impl Drop for ZWaveHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    key: ServiceKey,
    settings: ZWaveSettings,
    serial_settings: SerialSettings,
    recorder: Arc<dyn MessageRecorder>,
    opener: Arc<dyn PortOpener>,
    mut receiver: mpsc::Receiver<Outgoing>,
    status: watch::Sender<ServiceStatus>,
    cancel: CancellationToken,
) {
    let mut buffer = Vec::new();

    loop {
        update_status(&status, ServiceState::Starting, None);

        let mut port: Option<Port> = None;
        while port.is_none() {
            if cancel.is_cancelled() {
                purge(&mut receiver, recorder.as_ref());
                update_status(&status, ServiceState::Stopped, None);
                return;
            }
            match opener
                .open(key.entry.clone(), serial_settings.clone())
                .await
            {
                Ok(opened) => port = Some(opened),
                Err(error) => {
                    tracing::warn!(
                        service = %key.entry,
                        error = %error,
                        "unable to open the serial port"
                    );
                    update_status(
                        &status,
                        ServiceState::Error,
                        Some(format!("unable to open transport: {error}")),
                    );
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            purge(&mut receiver, recorder.as_ref());
                            update_status(&status, ServiceState::Stopped, None);
                            return;
                        }
                        _ = tokio::time::sleep(settings.open_attempts_interval) => {}
                    }
                }
            }
        }

        let port = port.expect("port must be opened");
        buffer.clear();
        update_status(&status, ServiceState::Running, None);
        tracing::info!(service = %key.entry, "serial port opened");

        let (mut reader, mut writer) = tokio::io::split(port);
        let mut read_buffer = [0_u8; READ_CHUNK];
        let mut expect_reply = false;
        let mut reopen = false;

        while !reopen {
            if expect_reply {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        purge(&mut receiver, recorder.as_ref());
                        update_status(&status, ServiceState::Stopped, None);
                        return;
                    }
                    _ = tokio::time::sleep(REPLY_TIMEOUT) => {
                        tracing::debug!(service = %key.entry, "z-wave reply timeout");
                        expect_reply = false;
                    }
                    result = reader.read(&mut read_buffer) => {
                        match read_result(
                            result,
                            &read_buffer,
                            &mut buffer,
                            &mut writer,
                            &key,
                            recorder.as_ref(),
                            &status,
                            &cancel,
                        ).await {
                            ReadOutcome::Continue => expect_reply = false,
                            ReadOutcome::Reopen => {
                                expect_reply = false;
                                reopen = true;
                            }
                            ReadOutcome::Stopped => return,
                        }
                    }
                }
            } else {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        purge(&mut receiver, recorder.as_ref());
                        update_status(&status, ServiceState::Stopped, None);
                        return;
                    }
                    outgoing = receiver.recv() => {
                        let Some(outgoing) = outgoing else {
                            purge(&mut receiver, recorder.as_ref());
                            update_status(&status, ServiceState::Stopped, None);
                            return;
                        };
                        let ttl = outgoing.ttl.unwrap_or(settings.outgoing_max_ttl);
                        if !ttl.is_zero() && outgoing.created.elapsed() > ttl {
                            recorder.update_state(outgoing.id, MessageState::OutgoingTimedOut);
                        } else {
                            match write_cancellable(&mut writer, &outgoing.payload, &cancel).await {
                                Ok(()) => {
                                    recorder.update_state(outgoing.id, MessageState::Outgoing);
                                    if matches!(
                                        validate_data_frame(&outgoing.payload).0,
                                        FrameValidation::Ok | FrameValidation::WrongChecksum
                                    ) {
                                        expect_reply = true;
                                    }
                                }
                                Err(_) if cancel.is_cancelled() => {
                                    recorder
                                        .update_state(outgoing.id, MessageState::OutgoingFailed);
                                    purge(&mut receiver, recorder.as_ref());
                                    update_status(&status, ServiceState::Stopped, None);
                                    return;
                                }
                                Err(error) => {
                                    recorder
                                        .update_state(outgoing.id, MessageState::OutgoingFailed);
                                    tracing::warn!(
                                        service = %key.entry,
                                        error = %error,
                                        "unable to write to the serial port"
                                    );
                                    update_status(
                                        &status,
                                        ServiceState::Error,
                                        Some(format!("unable to write using transport: {error}")),
                                    );
                                    reopen = true;
                                }
                            }
                        }
                    }
                    result = reader.read(&mut read_buffer) => {
                        match read_result(
                            result,
                            &read_buffer,
                            &mut buffer,
                            &mut writer,
                            &key,
                            recorder.as_ref(),
                            &status,
                            &cancel,
                        ).await {
                            ReadOutcome::Continue => {}
                            ReadOutcome::Reopen => reopen = true,
                            ReadOutcome::Stopped => return,
                        }
                    }
                }
            }
        }

        drop(reader);
        drop(writer);
        tracing::warn!(service = %key.entry, "reopening the serial port");
    }
}

enum ReadOutcome {
    Continue,
    Reopen,
    Stopped,
}

#[allow(clippy::too_many_arguments)]
async fn read_result(
    result: io::Result<usize>,
    chunk: &[u8],
    buffer: &mut Vec<u8>,
    writer: &mut (impl AsyncWrite + Unpin),
    key: &ServiceKey,
    recorder: &dyn MessageRecorder,
    status: &watch::Sender<ServiceStatus>,
    cancel: &CancellationToken,
) -> ReadOutcome {
    match result {
        Err(error) => {
            if cancel.is_cancelled() {
                return ReadOutcome::Stopped;
            }
            tracing::warn!(
                service = %key.entry,
                error = %error,
                "unable to read from the serial port"
            );
            update_status(
                status,
                ServiceState::Error,
                Some(format!("unable to read using transport: {error}")),
            );
            ReadOutcome::Reopen
        }
        Ok(0) => {
            if cancel.is_cancelled() {
                ReadOutcome::Stopped
            } else {
                tracing::warn!(
                    service = %key.entry,
                    "serial port reached EOF; reopening"
                );
                ReadOutcome::Reopen
            }
        }
        Ok(count) => {
            buffer.extend_from_slice(&chunk[..count]);
            if buffer.len() > MAX_PENDING_BYTES {
                tracing::warn!(
                    service = %key.entry,
                    "discarding an oversized z-wave receive buffer"
                );
                buffer.clear();
                return ReadOutcome::Continue;
            }

            match process_incoming(buffer, writer, key, recorder, cancel).await {
                Ok(()) => ReadOutcome::Continue,
                Err(error) => {
                    if cancel.is_cancelled() {
                        return ReadOutcome::Stopped;
                    }
                    tracing::warn!(
                        service = %key.entry,
                        error = %error,
                        "unable to write a z-wave reply"
                    );
                    update_status(
                        status,
                        ServiceState::Error,
                        Some(format!("unable to write using transport: {error}")),
                    );
                    ReadOutcome::Reopen
                }
            }
        }
    }
}

async fn process_incoming(
    buffer: &mut Vec<u8>,
    writer: &mut (impl AsyncWrite + Unpin),
    key: &ServiceKey,
    recorder: &dyn MessageRecorder,
    cancel: &CancellationToken,
) -> io::Result<()> {
    let mut index = 0;

    while index < buffer.len() {
        match buffer[index] {
            FRAME_ACK | FRAME_NAK | FRAME_CAN => {
                recorder.register(key, &buffer[index..index + 1], MessageState::Incoming);
                index += 1;
            }
            FRAME_SOF => match validate_data_frame(&buffer[index..]) {
                (FrameValidation::Ok, consumed) => {
                    let frame = buffer[index..index + consumed].to_vec();
                    recorder.register(key, &frame, MessageState::Incoming);
                    write_reply(writer, recorder, key, FRAME_ACK, cancel).await?;
                    index += consumed;
                }
                (FrameValidation::Incomplete, _) => break,
                (FrameValidation::WrongLength, _) => {
                    tracing::warn!("discarding a z-wave data frame with an invalid length");
                    write_reply(writer, recorder, key, FRAME_NAK, cancel).await?;
                    buffer.clear();
                    index = 0;
                    break;
                }
                (FrameValidation::WrongChecksum, consumed) => {
                    tracing::warn!(
                        frame = %to_hex(&buffer[index..index + consumed]),
                        "discarding a z-wave data frame with an invalid checksum"
                    );
                    write_reply(writer, recorder, key, FRAME_NAK, cancel).await?;
                    index += consumed;
                }
                (FrameValidation::NoSof, _) => unreachable!("the frame starts with a SOF byte"),
            },
            _ => {
                let end = (index + 20).min(buffer.len());
                tracing::warn!(
                    data = %to_hex(&buffer[index..end]),
                    "discarding unknown z-wave frame data"
                );
                buffer.clear();
                index = 0;
                break;
            }
        }
    }

    if index >= buffer.len() {
        buffer.clear();
    } else {
        buffer.drain(..index);
    }

    Ok(())
}

async fn write_cancellable(
    writer: &mut (impl AsyncWrite + Unpin),
    payload: &[u8],
    cancel: &CancellationToken,
) -> io::Result<()> {
    tokio::select! {
        _ = cancel.cancelled() => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "operation cancelled",
        )),
        result = writer.write_all(payload) => result,
    }
}

async fn write_reply(
    writer: &mut (impl AsyncWrite + Unpin),
    recorder: &dyn MessageRecorder,
    key: &ServiceKey,
    reply: u8,
    cancel: &CancellationToken,
) -> io::Result<()> {
    let message = recorder.register(key, &[reply], MessageState::OutgoingPending);
    match write_cancellable(writer, &[reply], cancel).await {
        Ok(()) => {
            recorder.update_state(message.id, MessageState::Outgoing);
            Ok(())
        }
        Err(error) => {
            recorder.update_state(message.id, MessageState::OutgoingFailed);
            Err(error)
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

pub async fn probe_version(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    timeout: Duration,
) -> io::Result<Option<String>> {
    stream.write_all(&version_request()).await?;

    let deadline = Instant::now() + timeout;
    let mut buffer = Vec::new();
    let mut saw_ack = false;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }

        let mut chunk = [0_u8; 128];
        let count = match tokio::time::timeout(remaining, stream.read(&mut chunk)).await {
            Err(_) => return Ok(None),
            Ok(Err(error)) => return Err(error),
            Ok(Ok(0)) => return Ok(None),
            Ok(Ok(count)) => count,
        };
        buffer.extend_from_slice(&chunk[..count]);

        let mut index = 0;
        while index < buffer.len() {
            if !saw_ack {
                if buffer[index] != FRAME_ACK {
                    return Ok(None);
                }
                saw_ack = true;
                index += 1;
                continue;
            }

            match validate_data_frame(&buffer[index..]) {
                (FrameValidation::Ok, consumed) => {
                    let version =
                        unpack_response(&buffer[index..index + consumed]).and_then(|payload| {
                            if payload.len() == 14 && payload[0] == ZW_VERSION {
                                Some(
                                    String::from_utf8_lossy(&payload[1..13])
                                        .trim_end_matches('\0')
                                        .to_string(),
                                )
                            } else {
                                None
                            }
                        });
                    return Ok(version);
                }
                (FrameValidation::Incomplete, _) => break,
                _ => return Ok(None),
            }
        }

        buffer.drain(..index);
    }
}

fn update_status(
    status: &watch::Sender<ServiceStatus>,
    state: ServiceState,
    error: Option<String>,
) {
    let _ = status.send(ServiceStatus { state, error });
}

fn purge(receiver: &mut mpsc::Receiver<Outgoing>, recorder: &dyn MessageRecorder) {
    while let Ok(outgoing) = receiver.try_recv() {
        recorder.update_state(outgoing.id, MessageState::OutgoingRejected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{MessageStore, StoreEvent};
    use crate::services::serial::Port;
    use crate::services::test_support::MockPortOpener;
    use proptest::prelude::*;

    fn service_key() -> ServiceKey {
        ServiceKey {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: "TEST".to_string(),
        }
    }

    fn start_handle(
        params: ParamValues,
        store: Arc<MessageStore>,
        opener: Arc<dyn PortOpener>,
        cancel: &CancellationToken,
    ) -> ZWaveHandle {
        ZWaveHandle::start_with_status_events(service_key(), params, store, opener, cancel, None)
            .expect("test service handle")
    }

    async fn wait_running(handle: &ZWaveHandle) {
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

    #[test]
    fn builds_and_validates_request_frames() {
        let frame = data_request(&[1, 2, 3]);
        assert_eq!(frame[0], FRAME_SOF);
        assert_eq!(frame[1], 5);
        assert_eq!(frame[2], FRAME_REQUEST);
        assert_eq!(
            validate_data_frame(&frame),
            (FrameValidation::Ok, frame.len())
        );
        assert_eq!(unpack_request(&frame), Some([1, 2, 3].as_slice()));
        assert_eq!(unpack_response(&frame), None);
    }

    #[test]
    fn builds_and_validates_response_frames() {
        let frame = data_response(&[4, 5, 6]);
        assert_eq!(unpack_response(&frame), Some([4, 5, 6].as_slice()));
        assert_eq!(unpack_request(&frame), None);
    }

    #[test]
    fn reports_incomplete_frames() {
        let frame = data_request(&[1, 2, 3]);
        assert_eq!(
            validate_data_frame(&frame[..frame.len() - 1]).0,
            FrameValidation::Incomplete
        );
        assert_eq!(validate_data_frame(&[]).0, FrameValidation::Incomplete);
        assert_eq!(
            validate_data_frame(&[FRAME_SOF]).0,
            FrameValidation::Incomplete
        );
    }

    #[test]
    fn reports_invalid_checksums() {
        let mut frame = data_request(&[1, 2, 3]);
        let last = frame.len() - 1;
        frame[last] ^= 0xff;
        assert_eq!(
            validate_data_frame(&frame).0,
            FrameValidation::WrongChecksum
        );
    }

    #[test]
    fn reports_invalid_lengths() {
        for length in [0, 1, 2, 254, 255] {
            let frame = [FRAME_SOF, length, FRAME_REQUEST, 0];
            assert_eq!(validate_data_frame(&frame).0, FrameValidation::WrongLength);
        }
    }

    #[test]
    fn rejects_missing_sof() {
        assert_eq!(
            validate_data_frame(&[0x02, 3, 0, 0, 0]).0,
            FrameValidation::NoSof
        );
    }

    #[test]
    fn checksum_is_xor_of_all_bytes() {
        assert_eq!(checksum(&[]), 0xff);
        assert_eq!(checksum(&[0xff]), 0x00);
        assert_eq!(checksum(&[0x01, 0x02, 0x03]), 0xff);
    }

    proptest! {
        #[test]
        fn validation_never_panics(data in proptest::collection::vec(any::<u8>(), 0..600)) {
            let _ = validate_data_frame(&data);
            let _ = unpack_data_frame(&data);
            let _ = unpack_request(&data);
            let _ = unpack_response(&data);
        }

        #[test]
        fn requests_round_trip(body in proptest::collection::vec(any::<u8>(), 1..251)) {
            let frame = data_request(&body);
            prop_assert_eq!(validate_data_frame(&frame), (FrameValidation::Ok, frame.len()));
            prop_assert_eq!(unpack_request(&frame), Some(body.as_slice()));
        }

        #[test]
        fn responses_round_trip(body in proptest::collection::vec(any::<u8>(), 1..251)) {
            let frame = data_response(&body);
            prop_assert_eq!(validate_data_frame(&frame), (FrameValidation::Ok, frame.len()));
            prop_assert_eq!(unpack_response(&frame), Some(body.as_slice()));
        }
    }

    #[tokio::test]
    async fn sends_frames_and_records_states() {
        let (device, mut host) = tokio::io::duplex(1024);
        let opener = MockPortOpener::new(vec![Ok(Box::new(device) as Port)]);
        let store = Arc::new(MessageStore::new(1024 * 1024));
        let cancel = CancellationToken::new();
        let handle = start_handle(ParamValues::new(), store.clone(), opener, &cancel);

        wait_running(&handle).await;

        let frame = data_request(&[1, 2, 3]);
        let message = handle.send(frame.clone(), None).unwrap();
        assert_eq!(message.state, MessageState::OutgoingPending);

        let mut buffer = [0_u8; 64];
        let count = tokio::time::timeout(Duration::from_secs(1), host.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buffer[..count], frame.as_slice());
        assert_eq!(store.get(message.id).unwrap().state, MessageState::Outgoing);

        handle.stop().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn cancels_a_blocked_serial_write() {
        let (device, _host) = tokio::io::duplex(1);
        let opener = MockPortOpener::new(vec![Ok(Box::new(device) as Port)]);
        let store = Arc::new(MessageStore::new(1024 * 1024));
        let cancel = CancellationToken::new();
        let handle = start_handle(ParamValues::new(), store, opener, &cancel);

        wait_running(&handle).await;
        handle.send(vec![0x55; 255], None).unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;

        tokio::time::timeout(Duration::from_secs(1), handle.stop())
            .await
            .expect("service stop was blocked by the serial write");
        cancel.cancel();
    }

    #[tokio::test]
    async fn acknowledges_incoming_frames() {
        let (device, mut host) = tokio::io::duplex(1024);
        let opener = MockPortOpener::new(vec![Ok(Box::new(device) as Port)]);
        let store = Arc::new(MessageStore::new(1024 * 1024));
        let cancel = CancellationToken::new();
        let handle = start_handle(ParamValues::new(), store.clone(), opener, &cancel);
        wait_running(&handle).await;

        let frame = data_response(&[0x15]);
        host.write_all(&frame).await.unwrap();

        let mut buffer = [0_u8; 4];
        let count = tokio::time::timeout(Duration::from_secs(1), host.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buffer[..count], &[FRAME_ACK]);

        let incoming = store
            .list(
                None,
                None,
                hodeco_contract::domain::MessageOrder::OldestFirst,
                10,
                &crate::messages::MessageFilter {
                    states: vec![MessageState::Incoming],
                    ..Default::default()
                },
            )
            .messages;
        assert!(
            incoming
                .iter()
                .any(|message| message.payload.as_slice() == frame)
        );

        handle.stop().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn rejects_invalid_incoming_frames_with_nak() {
        let (device, mut host) = tokio::io::duplex(1024);
        let opener = MockPortOpener::new(vec![Ok(Box::new(device) as Port)]);
        let store = Arc::new(MessageStore::new(1024 * 1024));
        let cancel = CancellationToken::new();
        let handle = start_handle(ParamValues::new(), store.clone(), opener, &cancel);
        wait_running(&handle).await;

        let mut frame = data_response(&[0x15]);
        let last = frame.len() - 1;
        frame[last] ^= 0xff;
        host.write_all(&frame).await.unwrap();

        let mut buffer = [0_u8; 4];
        let count = tokio::time::timeout(Duration::from_secs(1), host.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buffer[..count], &[FRAME_NAK]);

        handle.stop().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn retries_opening_the_port() {
        let (device, _host) = tokio::io::duplex(1024);
        let opener = MockPortOpener::new(vec![
            Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
            Ok(Box::new(device) as Port),
        ]);
        let store = Arc::new(MessageStore::new(1024 * 1024));
        let cancel = CancellationToken::new();
        let params = ParamValues::from([(
            "openAttemptsInterval".to_string(),
            serde_json::json!("10 ms"),
        )]);
        let handle = start_handle(params, store, opener, &cancel);

        wait_running(&handle).await;
        handle.stop().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn rejects_sends_when_not_running() {
        let opener = MockPortOpener::new(Vec::new());
        let store = Arc::new(MessageStore::new(1024 * 1024));
        let cancel = CancellationToken::new();
        let handle = start_handle(ParamValues::new(), store.clone(), opener, &cancel);

        let result = handle.send(vec![1], None);
        assert!(matches!(result, Err(ServiceError::NotReady)));

        handle.stop().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn records_timed_out_messages() {
        let (device, mut host) = tokio::io::duplex(1);
        let opener = MockPortOpener::new(vec![Ok(Box::new(device) as Port)]);
        let store = Arc::new(MessageStore::new(1024 * 1024));
        let cancel = CancellationToken::new();
        let handle = start_handle(ParamValues::new(), store.clone(), opener, &cancel);
        wait_running(&handle).await;

        handle.send(vec![0x99; 255], None).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let message = handle
            .send(vec![0x88], Some(Duration::from_millis(1)))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut drained = 0;
        while drained < 255 {
            let mut buffer = [0_u8; 255];
            let count = tokio::time::timeout(Duration::from_secs(1), host.read(&mut buffer))
                .await
                .unwrap()
                .unwrap();
            drained += count;
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if store
                    .get(message.id)
                    .is_some_and(|message| message.state == MessageState::OutgoingTimedOut)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("message was not timed out");

        handle.stop().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn emits_store_events() {
        let (device, _host) = tokio::io::duplex(1024);
        let opener = MockPortOpener::new(vec![Ok(Box::new(device) as Port)]);
        let store = Arc::new(MessageStore::new(1024 * 1024));
        let mut events = store.subscribe();
        let cancel = CancellationToken::new();
        let handle = start_handle(ParamValues::new(), store.clone(), opener, &cancel);
        wait_running(&handle).await;

        handle.send(data_request(&[1]), None).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match events.recv().await.unwrap() {
                    StoreEvent::New { .. } | StoreEvent::StateChanged { .. } => break,
                    StoreEvent::Dropped { .. } => {}
                }
            }
        })
        .await
        .unwrap();

        handle.stop().await;
        cancel.cancel();
    }
}
