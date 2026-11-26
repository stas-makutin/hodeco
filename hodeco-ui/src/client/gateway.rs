//! Connection state machine, request tracking, and resync.
//!
//! Every disconnect is a resync boundary: pending requests are dropped, volatile
//! collections are cleared by the app, and a fresh subscription plus initial
//! state is requested on the next connection.

use crate::client::transport::{DemoTransport, LiveTransport, Transport, TransportEvent, Wake};
use crate::client::{Incoming, classify};
use hodeco_contract::api::notifications::Notification;
use hodeco_contract::api::{ApiError, Method, Request, Response};
use serde_json::{Value, json};
use std::time::Duration;
use web_time::Instant;

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BOOTSTRAP_REQUESTS: usize = 8;
const SUBSCRIBED_TOPICS: [hodeco_contract::api::payloads::EventTopic; 4] = [
    hodeco_contract::api::payloads::EventTopic::Messages,
    hodeco_contract::api::payloads::EventTopic::Discovery,
    hodeco_contract::api::payloads::EventTopic::Services,
    hodeco_contract::api::payloads::EventTopic::Tasks,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Disconnected,
    Connecting,
    Syncing,
    Ready,
    Backoff,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Disconnected => "Disconnected",
            Self::Connecting => "Connecting",
            Self::Syncing => "Syncing",
            Self::Ready => "Connected",
            Self::Backoff => "Reconnecting",
        }
    }

    pub fn is_live(self) -> bool {
        matches!(self, Self::Syncing | Self::Ready)
    }
}

/// Delays after consecutive connection failures: 0.5, 1, 2, 4, 8, then 10 s.
pub fn backoff(attempt: u32) -> Duration {
    match attempt {
        0 => Duration::from_millis(500),
        1 => Duration::from_secs(1),
        2 => Duration::from_secs(2),
        3 => Duration::from_secs(4),
        4 => Duration::from_secs(8),
        _ => Duration::from_secs(10),
    }
}

/// Logical request identity. The response router uses this to decide what part
/// of the app model to update, independent of the wire request ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestTag {
    Subscribe,
    ServerInfo,
    ProtocolList,
    TransportList,
    Config,
    ConfigUpdate,
    ServiceList,
    MessagesPage {
        generation: u64,
        kind: MessagePageKind,
    },
    MessageSearch {
        generation: u64,
    },
    MessageBatch {
        generation: u64,
    },
    MessageAnchor {
        list_generation: u64,
        search_generation: u64,
        hit: uuid::Uuid,
    },
    MessageGet {
        id: uuid::Uuid,
    },
    ServiceAdd,
    ServiceRemove,
    ServiceAlias,
    ServiceStatus {
        service: hodeco_contract::domain::ServiceRef,
    },
    ServiceParams {
        service: hodeco_contract::domain::ServiceRef,
    },
    ServiceSend,
    DiscoveryStart,
    DiscoveryGet,
    DiscoveryCancel,
    ProtocolParams {
        protocol: String,
        transport: String,
    },
    TasksList,
    TaskAdd,
    TaskUpdate,
    TaskRemove {
        task_id: String,
    },
    TaskFire,
    TaskCancel {
        task_id: String,
        run_id: uuid::Uuid,
    },
    TaskPurgeHistory {
        task_id: String,
    },
    TaskRunFeed {
        generation: u64,
        kind: RunPageKind,
    },
    TaskHistoryPage {
        generation: u64,
        kind: RunPageKind,
    },
    TaskRunGet {
        task_id: String,
        run_id: uuid::Uuid,
    },
    TaskHistoryRunGet {
        task_id: String,
        run_id: uuid::Uuid,
    },
    SchedulesList,
    ScheduleAdd,
    ScheduleUpdate,
    ScheduleRemove {
        schedule_id: String,
    },
    SchedulePreview {
        generation: u64,
    },
    ScheduleSetDefaults,
    SolarPreview {
        generation: u64,
    },
    CommandsList,
    CommandAdd,
    CommandUpdate,
    CommandRemove {
        command_id: String,
    },
    RawConsole,
}

/// The reason a `messages.list` page is being requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessagePageKind {
    Refresh,
    Older,
    Newer,
}

/// The reason a `tasks.runFeed` page is being requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPageKind {
    Refresh,
    Older,
    Newer,
}

impl RequestTag {
    /// The RPC method this tag is associated with, used for diagnostics labels.
    /// Like `RawConsole` (send path) and `Subscribe` (setup path), tags that do
    /// not map to a single method are reported with a bespoke label instead.
    pub fn method(&self) -> Method {
        match self {
            Self::Subscribe => Method::EventsSubscribe,
            Self::ServerInfo => Method::ServerInfo,
            Self::ProtocolList => Method::ProtocolsList,
            Self::TransportList => Method::TransportsList,
            Self::Config => Method::ConfigGet,
            Self::ConfigUpdate => Method::ConfigUpdate,
            Self::ServiceList => Method::ServicesList,
            Self::MessagesPage { .. }
            | Self::MessageSearch { .. }
            | Self::MessageBatch { .. }
            | Self::MessageAnchor { .. } => Method::MessagesList,
            Self::MessageGet { .. } => Method::MessagesGet,
            Self::ServiceAdd => Method::ServicesAdd,
            Self::ServiceRemove => Method::ServicesRemove,
            Self::ServiceAlias => Method::ServicesAlias,
            Self::ServiceStatus { .. } => Method::ServicesStatus,
            Self::ServiceParams { .. } => Method::ServicesParams,
            Self::ServiceSend => Method::ServicesSend,
            Self::DiscoveryStart => Method::DiscoveryStart,
            Self::DiscoveryGet => Method::DiscoveryGet,
            Self::DiscoveryCancel => Method::DiscoveryCancel,
            Self::ProtocolParams { .. } => Method::ProtocolsParams,
            Self::TasksList => Method::TasksList,
            Self::TaskAdd => Method::TasksAdd,
            Self::TaskUpdate => Method::TasksUpdate,
            Self::TaskRemove { .. } => Method::TasksRemove,
            Self::TaskFire => Method::TasksFire,
            Self::TaskCancel { .. } => Method::TasksCancelRun,
            Self::TaskPurgeHistory { .. } => Method::TasksPurgeHistory,
            Self::TaskRunFeed { .. } => Method::TasksRunFeed,
            Self::TaskHistoryPage { .. } => Method::TasksHistory,
            Self::TaskRunGet { .. } | Self::TaskHistoryRunGet { .. } => Method::TasksGetRun,
            Self::SchedulesList => Method::SchedulesList,
            Self::ScheduleAdd => Method::SchedulesAdd,
            Self::ScheduleUpdate => Method::SchedulesUpdate,
            Self::ScheduleRemove { .. } => Method::SchedulesRemove,
            Self::SchedulePreview { .. } => Method::SchedulesPreview,
            Self::ScheduleSetDefaults => Method::SchedulesSetDefaults,
            Self::SolarPreview { .. } => Method::SchedulesSolarPreview,
            Self::CommandsList => Method::CommandsList,
            Self::CommandAdd => Method::CommandsAdd,
            Self::CommandUpdate => Method::CommandsUpdate,
            Self::CommandRemove { .. } => Method::CommandsRemove,
            Self::RawConsole => Method::ServerInfo,
        }
    }

    fn is_bootstrap(&self) -> bool {
        matches!(
            self,
            Self::ServerInfo
                | Self::ProtocolList
                | Self::TransportList
                | Self::Config
                | Self::ServiceList
                | Self::TasksList
                | Self::SchedulesList
                | Self::CommandsList
        )
    }

    /// Mutations whose outcome cannot be known when their request is lost.
    pub fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::ConfigUpdate
                | Self::ServiceAdd
                | Self::ServiceRemove
                | Self::ServiceAlias
                | Self::ServiceParams { .. }
                | Self::ServiceSend
                | Self::DiscoveryStart
                | Self::DiscoveryCancel
                | Self::TaskAdd
                | Self::TaskUpdate
                | Self::TaskRemove { .. }
                | Self::TaskFire
                | Self::TaskCancel { .. }
                | Self::TaskPurgeHistory { .. }
                | Self::ScheduleAdd
                | Self::ScheduleUpdate
                | Self::ScheduleRemove { .. }
                | Self::ScheduleSetDefaults
                | Self::CommandAdd
                | Self::CommandUpdate
                | Self::CommandRemove { .. }
                | Self::RawConsole
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GatewayEvent {
    Connected,
    Synced,
    Disconnected {
        reason: String,
        unknown_mutations: Vec<RequestTag>,
    },
    Response {
        tag: RequestTag,
        result: Value,
    },
    ErrorResponse {
        tag: RequestTag,
        error: ApiError,
    },
    Timeout {
        tag: RequestTag,
    },
    Notification(Notification),
    BadMessage(String),
}

struct PendingRequest {
    tag: RequestTag,
    sent_at: Instant,
    deadline: Instant,
}

pub struct Gateway {
    endpoint: String,
    demo: bool,
    transport: Option<Transport>,
    phase: Phase,
    epoch: u64,
    counter: u64,
    pending: Vec<(String, PendingRequest)>,
    attempts: u32,
    next_attempt_at: Option<Instant>,
    sync_remaining: usize,
    started: bool,
}

impl Gateway {
    pub fn new(endpoint: impl Into<String>, demo: bool) -> Self {
        Self {
            endpoint: endpoint.into(),
            demo,
            transport: None,
            phase: Phase::Disconnected,
            epoch: 0,
            counter: 0,
            pending: Vec::new(),
            attempts: 0,
            next_attempt_at: None,
            sync_remaining: 0,
            started: false,
        }
    }

    pub fn start(&mut self, now: Instant) {
        if self.started {
            return;
        }
        self.started = true;
        self.next_attempt_at = Some(now);
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn is_demo(&self) -> bool {
        self.demo
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Tears down the current connection and starts over against a (possibly
    /// new) endpoint. All pending requests are abandoned.
    pub fn reconnect(&mut self, endpoint: String, demo: bool, now: Instant) {
        self.transport = None;
        self.pending.clear();
        self.endpoint = endpoint;
        self.demo = demo;
        self.epoch += 1;
        self.counter = 0;
        self.sync_remaining = 0;
        self.attempts = 0;
        self.phase = Phase::Disconnected;
        self.started = true;
        self.next_attempt_at = Some(now);
    }

    /// Skips the remaining backoff delay. Used by the Retry now button.
    pub fn retry_now(&mut self, now: Instant) {
        if self.transport.is_none() {
            self.attempts = 0;
            self.next_attempt_at = Some(now);
            self.phase = Phase::Disconnected;
        }
    }

    /// Polls transport events, connection retries, and request deadlines. The
    /// app drains this once per frame before drawing.
    pub fn poll(&mut self, now: Instant, wake: &Wake) -> Vec<GatewayEvent> {
        let mut events = Vec::new();

        if self.transport.is_none()
            && self.started
            && self.next_attempt_at.is_some_and(|at| now >= at)
        {
            self.next_attempt_at = None;
            self.attempt(now, wake, &mut events);
        }

        let transport_events: Vec<TransportEvent> = self
            .transport
            .as_mut()
            .map(|transport| transport.poll(now))
            .unwrap_or_default();

        let mut failure = None;
        for event in transport_events {
            match event {
                TransportEvent::Opened => self.on_opened(now, &mut events),
                TransportEvent::Text(text) => self.handle_incoming(&text, &mut events),
                TransportEvent::Error(error) => failure = Some(error),
                TransportEvent::Closed => failure = Some("connection closed".to_string()),
            }
        }
        if let Some(reason) = failure {
            self.begin_reconnect(now, reason, &mut events);
        }

        self.expire(now, &mut events);
        events
    }

    /// Sends a tagged RPC request. Returns an error when the socket is not live.
    pub fn request(
        &mut self,
        tag: RequestTag,
        method: Method,
        params: Value,
        now: Instant,
    ) -> Result<(), String> {
        if !self.phase.is_live() {
            return Err(format!(
                "not connected (currently {})",
                self.phase.label().to_lowercase()
            ));
        }
        self.send_tagged(tag, method, params, now)
    }

    /// Earliest time the app should repaint to service a deadline or demo tick.
    pub fn next_repaint(&self, now: Instant) -> Option<Duration> {
        let mut next = self.next_attempt_at;
        for (_, pending) in &self.pending {
            if next.is_none_or(|current| pending.deadline < current) {
                next = Some(pending.deadline);
            }
        }
        if self.demo && self.transport.is_some() {
            let tick = now + Duration::from_millis(250);
            if next.is_none_or(|current| tick < current) {
                next = Some(tick);
            }
        }
        next.map(|at| at.saturating_duration_since(now))
    }

    fn attempt(&mut self, now: Instant, wake: &Wake, events: &mut Vec<GatewayEvent>) {
        self.epoch += 1;
        self.counter = 0;
        self.phase = Phase::Connecting;

        let transport = if self.demo {
            Ok(Transport::Demo(DemoTransport::new(now)))
        } else {
            LiveTransport::connect(&self.endpoint, wake.clone()).map(Transport::Live)
        };

        match transport {
            Ok(transport) => self.transport = Some(transport),
            Err(error) => self.begin_reconnect(now, error, events),
        }
    }

    fn on_opened(&mut self, now: Instant, events: &mut Vec<GatewayEvent>) {
        self.phase = Phase::Syncing;
        self.sync_remaining = BOOTSTRAP_REQUESTS;
        events.push(GatewayEvent::Connected);

        if let Err(error) = self.send_bootstrap(now) {
            self.begin_reconnect(now, error, events);
        }
    }

    fn send_bootstrap(&mut self, now: Instant) -> Result<(), String> {
        // Subscribe before the initial lists so no message can fall into the gap.
        self.send_tagged(
            RequestTag::Subscribe,
            Method::EventsSubscribe,
            json!({"topics": SUBSCRIBED_TOPICS}),
            now,
        )?;
        self.send_tagged(RequestTag::ServerInfo, Method::ServerInfo, json!({}), now)?;
        self.send_tagged(
            RequestTag::ProtocolList,
            Method::ProtocolsList,
            json!({}),
            now,
        )?;
        self.send_tagged(
            RequestTag::TransportList,
            Method::TransportsList,
            json!({}),
            now,
        )?;
        self.send_tagged(RequestTag::Config, Method::ConfigGet, json!({}), now)?;
        self.send_tagged(
            RequestTag::ServiceList,
            Method::ServicesList,
            json!({}),
            now,
        )?;
        self.send_tagged(RequestTag::TasksList, Method::TasksList, json!({}), now)?;
        self.send_tagged(
            RequestTag::SchedulesList,
            Method::SchedulesList,
            json!({}),
            now,
        )?;
        self.send_tagged(
            RequestTag::CommandsList,
            Method::CommandsList,
            json!({}),
            now,
        )?;
        Ok(())
    }

    fn send_tagged(
        &mut self,
        tag: RequestTag,
        method: Method,
        params: Value,
        now: Instant,
    ) -> Result<(), String> {
        self.counter += 1;
        let id = format!("ui-{}-{}", self.epoch, self.counter);
        let request = Request::new(id.clone(), method, params);
        let text = serde_json::to_string(&request).map_err(|error| error.to_string())?;
        let Some(transport) = self.transport.as_mut() else {
            return Err("not connected".to_string());
        };
        transport.send(text)?;
        self.pending.push((
            id,
            PendingRequest {
                tag,
                sent_at: now,
                deadline: now + REQUEST_TIMEOUT,
            },
        ));
        Ok(())
    }

    fn handle_incoming(&mut self, text: &str, events: &mut Vec<GatewayEvent>) {
        match classify(text) {
            Ok(Incoming::Response(response)) => {
                let id = match &response {
                    Response::Success(success) => success.id.clone(),
                    Response::Error(error) => error.id.clone(),
                };
                let Some(position) = self
                    .pending
                    .iter()
                    .position(|(pending_id, _)| *pending_id == id)
                else {
                    log::debug!("ignoring response for unknown or expired request {id}");
                    return;
                };
                let (_, pending) = self.pending.remove(position);
                let tag = pending.tag;
                self.note_sync(&tag, events);
                match response {
                    Response::Success(success) => events.push(GatewayEvent::Response {
                        tag,
                        result: success.result,
                    }),
                    Response::Error(error) => events.push(GatewayEvent::ErrorResponse {
                        tag,
                        error: error.error,
                    }),
                }
            }
            Ok(Incoming::Notification(notification)) => {
                events.push(GatewayEvent::Notification(notification));
            }
            Ok(Incoming::Unknown(value)) => {
                events.push(GatewayEvent::BadMessage(format!(
                    "unrecognised message: {value}"
                )));
            }
            Err(error) => {
                events.push(GatewayEvent::BadMessage(format!(
                    "invalid JSON from gateway: {error}"
                )));
            }
        }
    }

    fn expire(&mut self, now: Instant, events: &mut Vec<GatewayEvent>) {
        let mut index = 0;
        while index < self.pending.len() {
            if self.pending[index].1.deadline <= now {
                let (id, pending) = self.pending.remove(index);
                log::debug!(
                    "request {id} ({:?}) timed out after {:?}",
                    pending.tag,
                    pending.sent_at.saturating_duration_since(now)
                );
                events.push(GatewayEvent::Timeout {
                    tag: pending.tag.clone(),
                });
                self.note_sync(&pending.tag, events);
            } else {
                index += 1;
            }
        }
    }

    fn note_sync(&mut self, tag: &RequestTag, events: &mut Vec<GatewayEvent>) {
        if self.phase != Phase::Syncing || !tag.is_bootstrap() {
            return;
        }
        self.sync_remaining = self.sync_remaining.saturating_sub(1);
        if self.sync_remaining == 0 {
            self.phase = Phase::Ready;
            self.attempts = 0;
            events.push(GatewayEvent::Synced);
        }
    }

    fn begin_reconnect(&mut self, now: Instant, reason: String, events: &mut Vec<GatewayEvent>) {
        self.transport = None;
        let unknown_mutations: Vec<RequestTag> = self
            .pending
            .drain(..)
            .filter_map(|(_, pending)| pending.tag.is_mutation().then_some(pending.tag))
            .collect();
        self.sync_remaining = 0;

        let delay = backoff(self.attempts);
        self.attempts = (self.attempts + 1).min(5);
        self.next_attempt_at = Some(now + delay);
        self.phase = Phase::Backoff;
        log::warn!("gateway disconnected: {reason}; retrying in {delay:?}");
        events.push(GatewayEvent::Disconnected {
            reason,
            unknown_mutations,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_gateway() -> Gateway {
        let mut gateway = Gateway::new("ws://example.test/api/ws", true);
        gateway.phase = Phase::Ready;
        gateway.epoch = 3;
        gateway
    }

    fn add_pending(gateway: &mut Gateway, tag: RequestTag, id: &str, now: Instant) {
        gateway.pending.push((
            id.to_string(),
            PendingRequest {
                tag,
                sent_at: now,
                deadline: now + REQUEST_TIMEOUT,
            },
        ));
    }

    #[test]
    fn demo_gateway_reaches_ready_and_adds_services() {
        use std::sync::Arc;

        let now = Instant::now();
        let wake: Wake = Arc::new(|| {});
        let mut gateway = Gateway::new("", true);
        gateway.start(now);
        let mut events = Vec::new();
        for step in 0..20 {
            events.extend(gateway.poll(now + Duration::from_millis(step * 10), &wake));
            if gateway.phase() == Phase::Ready {
                break;
            }
        }
        assert_eq!(gateway.phase(), Phase::Ready);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, GatewayEvent::Synced))
        );

        gateway
            .request(
                RequestTag::ProtocolParams {
                    protocol: "zigbee".to_string(),
                    transport: "serial".to_string(),
                },
                Method::ProtocolsParams,
                json!({"protocol": "zigbee", "transport": "serial"}),
                now,
            )
            .unwrap();
        let mut params = None;
        for _ in 0..5 {
            for event in gateway.poll(now + Duration::from_millis(100), &wake) {
                if let GatewayEvent::Response { tag, result } = event
                    && matches!(tag, RequestTag::ProtocolParams { .. })
                {
                    params = Some(result);
                }
            }
        }
        let params = params.expect("protocol params response");
        assert!(
            params["params"]
                .as_array()
                .is_some_and(|params| params.iter().any(|param| param["name"] == "baudRate"))
        );

        gateway
            .request(
                RequestTag::ServiceAdd,
                Method::ServicesAdd,
                json!({
                    "alias": "ZB2",
                    "protocol": "zigbee",
                    "transport": "serial",
                    "entry": "COM9",
                }),
                now,
            )
            .unwrap();
        let mut added = None;
        for _ in 0..5 {
            for event in gateway.poll(now + Duration::from_millis(200), &wake) {
                if let GatewayEvent::Response { tag, result } = event
                    && tag == RequestTag::ServiceAdd
                {
                    added = Some(result);
                }
            }
        }
        let added = added.expect("service add response");
        assert_eq!(added["service"]["alias"], "ZB2");
        assert_eq!(added["service"]["protocol"], "zigbee");
    }

    #[test]
    fn backoff_is_bounded() {
        assert_eq!(backoff(0), Duration::from_millis(500));
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(4), Duration::from_secs(8));
        assert_eq!(backoff(5), Duration::from_secs(10));
        assert_eq!(backoff(50), Duration::from_secs(10));
    }

    #[test]
    fn matches_responses_by_wire_id() {
        let now = Instant::now();
        let mut gateway = test_gateway();
        add_pending(&mut gateway, RequestTag::ServerInfo, "ui-3-1", now);
        let mut events = Vec::new();
        gateway.handle_incoming(
            r#"{"id":"ui-3-1","result":{"version":"1.2.3"}}"#,
            &mut events,
        );
        assert_eq!(
            events,
            vec![GatewayEvent::Response {
                tag: RequestTag::ServerInfo,
                result: json!({"version": "1.2.3"}),
            }]
        );
        assert!(gateway.pending.is_empty());
    }

    #[test]
    fn ignores_responses_from_an_old_epoch() {
        let now = Instant::now();
        let mut gateway = test_gateway();
        add_pending(&mut gateway, RequestTag::ServerInfo, "ui-3-1", now);
        let mut events = Vec::new();
        gateway.handle_incoming(r#"{"id":"ui-2-9","result":{}}"#, &mut events);
        assert!(events.is_empty());
        assert_eq!(gateway.pending.len(), 1);
    }

    #[test]
    fn times_out_pending_requests() {
        let now = Instant::now();
        let mut gateway = test_gateway();
        gateway.phase = Phase::Syncing;
        gateway.sync_remaining = 1;
        add_pending(&mut gateway, RequestTag::ServerInfo, "ui-3-1", now);
        let mut events = Vec::new();
        gateway.expire(now + REQUEST_TIMEOUT, &mut events);
        assert_eq!(
            events,
            vec![
                GatewayEvent::Timeout {
                    tag: RequestTag::ServerInfo
                },
                GatewayEvent::Synced
            ]
        );
        assert_eq!(gateway.phase, Phase::Ready);
    }

    #[test]
    fn reports_unknown_mutations_on_disconnect() {
        let now = Instant::now();
        let mut gateway = test_gateway();
        add_pending(&mut gateway, RequestTag::ServiceRemove, "ui-3-1", now);
        add_pending(&mut gateway, RequestTag::ServiceList, "ui-3-2", now);
        let mut events = Vec::new();
        gateway.begin_reconnect(now, "test".to_string(), &mut events);
        match &events[0] {
            GatewayEvent::Disconnected {
                unknown_mutations, ..
            } => assert_eq!(unknown_mutations, &[RequestTag::ServiceRemove]),
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(gateway.phase, Phase::Backoff);
        assert!(gateway.transport.is_none());
        assert!(gateway.pending.is_empty());
    }

    #[test]
    fn request_ids_include_the_epoch() {
        let now = Instant::now();
        let mut gateway = test_gateway();
        gateway.counter = 41;
        // No transport, so the send fails, but the id counter still advances.
        let result = gateway.request(RequestTag::RawConsole, Method::ServerInfo, json!({}), now);
        assert!(result.is_err());
        assert_eq!(gateway.counter, 42);
    }
}
