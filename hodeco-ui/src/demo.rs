//! In-process fixture server used by `--demo` and the component gallery.
//!
//! It speaks the same JSON envelopes as the real gateway and implements the
//! read-only portions of the RPC surface plus enough mutation to exercise the UI:
//! service add/remove/alias/send, discovery transitions, and live traffic.
//! Nothing here touches the network, so it also works in the browser.

use chrono::{DateTime, TimeDelta, Utc};
use hodeco_codec::zwave::{request as frame_request, response as frame_response};
use hodeco_contract::api::notifications::{
    DiscoveryNotification, MessageNotification, Notification,
};
use hodeco_contract::api::payloads::{
    ConfigUpdateParams, ConfigUpdateResult, DiscoveryCancelParams, DiscoveryGetParams,
    DiscoveryGetResult, DiscoveryStartParams, DiscoveryStartResult, Empty, EventTopic,
    EventsSubscribeParams, EventsUnsubscribeParams, MessageTimeRange, MessagesGetParams,
    MessagesGetResult, MessagesListParams, MessagesListResult, ProtocolInfoParams,
    ProtocolInfoResult, ProtocolParamsParams, ProtocolParamsResult, ProtocolsListResult,
    ServerInfoResult, ServicesAddParams, ServicesAddResult, ServicesAliasParams,
    ServicesAliasResult, ServicesListResult, ServicesParamsParams, ServicesParamsResult,
    ServicesRemoveParams, ServicesSendParams, ServicesSendResult, ServicesStatusParams,
    ServicesStatusResult, TransportsListResult,
};
use hodeco_contract::api::{ApiError, ErrorCode, Method, Request, Response};
use hodeco_contract::catalog;
use hodeco_contract::config::{Config, ServiceConfig};
use hodeco_contract::domain::{
    DiscoveryEntry, DiscoveryInfo, DiscoveryState, Message, MessageOrder, MessageState, ParamDef,
    ParamType, ProtocolInfo, ServiceInfo, ServiceKey, ServiceRef, ServiceState, ServiceStatus,
    TransportInfo,
};
use hodeco_contract::utils::Base64Bytes;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

const DEMO_PROTOCOL_VERSION: &str = "1.0";
const DEMO_GET_VERSION: [u8; 14] = [
    0x15, b'Z', b'-', b'W', b'a', b'v', b'e', b' ', b'4', b'.', b'0', b'5', 0x00, 0x01,
];

pub struct DemoServer {
    config: Config,
    runtime_config: Config,
    services: Vec<ServiceInfo>,
    messages: Vec<Message>,
    last_time: DateTime<Utc>,
    subscribed: Vec<EventTopic>,
    discovery: Option<DiscoveryInfo>,
    discovery_finished: bool,
    pending_outgoing: Vec<Uuid>,
    started_at: DateTime<Utc>,
    ticks: u64,
    next_traffic: usize,
}

impl Default for DemoServer {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoServer {
    pub fn new() -> Self {
        let now = Utc::now();
        let services = vec![
            ServiceInfo {
                alias: Some("ZW".to_string()),
                protocol: "zwave".to_string(),
                transport: "serial".to_string(),
                entry: "COM4".to_string(),
                status: ServiceStatus {
                    state: ServiceState::Running,
                    error: None,
                },
                params: BTreeMap::from([
                    ("baudRate".to_string(), json!(115_200)),
                    ("openAttemptsInterval".to_string(), json!("3 s")),
                    ("outgoingMaxTTL".to_string(), json!("10 s")),
                ]),
            },
            ServiceInfo {
                alias: None,
                protocol: "zwave".to_string(),
                transport: "serial".to_string(),
                entry: "COM3".to_string(),
                status: ServiceStatus {
                    state: ServiceState::Error,
                    error: Some("access denied while opening COM3".to_string()),
                },
                params: BTreeMap::new(),
            },
            ServiceInfo {
                alias: Some("ZW-Lab".to_string()),
                protocol: "zwave".to_string(),
                transport: "serial".to_string(),
                entry: "COM7".to_string(),
                status: ServiceStatus {
                    state: ServiceState::Starting,
                    error: None,
                },
                params: BTreeMap::from([("baudRate".to_string(), json!(57_600))]),
            },
            ServiceInfo {
                alias: Some("HTTP".to_string()),
                protocol: "http".to_string(),
                transport: "tcp".to_string(),
                entry: "api.example.test:443".to_string(),
                status: ServiceStatus {
                    state: ServiceState::Running,
                    error: None,
                },
                params: BTreeMap::from([
                    ("connectTimeout".to_string(), json!("10 s")),
                    ("readTimeout".to_string(), json!("30 s")),
                ]),
            },
        ];

        let alias = ServiceRef::Alias {
            alias: "ZW".to_string(),
        };
        let http = ServiceRef::Alias {
            alias: "HTTP".to_string(),
        };
        let mut messages = vec![
            new_message(
                now - TimeDelta::minutes(12),
                &alias,
                MessageState::Incoming,
                vec![0x06],
            ),
            new_message(
                now - TimeDelta::minutes(11),
                &alias,
                MessageState::Incoming,
                frame_response(&DEMO_GET_VERSION),
            ),
            new_message(
                now - TimeDelta::minutes(9),
                &alias,
                MessageState::Outgoing,
                frame_request(&[0x13, 0x02, 0x03, 0x20, 0x01, 0x63, 0x00, 0x01]),
            ),
            new_message(
                now - TimeDelta::minutes(8),
                &alias,
                MessageState::Incoming,
                frame_request(&[0x04, 0x00, 0x04, 0x03, 0x20, 0x01, 0x2d]),
            ),
            new_message(
                now - TimeDelta::minutes(6),
                &alias,
                MessageState::Incoming,
                frame_request(&[0x04, 0x00, 0x02, 0x03, 0x25, 0x03, 0xff]),
            ),
            new_message(
                now - TimeDelta::minutes(4),
                &alias,
                MessageState::Incoming,
                frame_request(&[
                    0x04, 0x00, 0x06, 0x08, 0x32, 0x02, 0x01, 0x00, 0x00, 0x00, 0x30, 0x39,
                ]),
            ),
            new_message(
                now - TimeDelta::minutes(3),
                &alias,
                MessageState::Incoming,
                frame_request(&[0x04, 0x00, 0x07, 0x04, 0x26, 0x03, 0x48, 0x05]),
            ),
            new_message(
                now - TimeDelta::minutes(2),
                &alias,
                MessageState::OutgoingTimedOut,
                frame_request(&[0x13, 0x03, 0x03, 0x20, 0x01, 0x00, 0x00, 0x02]),
            ),
            new_message(
                now - TimeDelta::minutes(1),
                &alias,
                MessageState::Incoming,
                frame_request(&[0x04, 0x00, 0x04, 0x04, 0x2b, 0x01, 0x05, 0x00]),
            ),
            {
                let mut payload = frame_response(&DEMO_GET_VERSION);
                let last = payload.len() - 1;
                payload[last] ^= 0xff;
                new_message(
                    now - TimeDelta::seconds(30),
                    &alias,
                    MessageState::Incoming,
                    payload,
                )
            },
            new_message(
                now - TimeDelta::seconds(55),
                &http,
                MessageState::Incoming,
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\xd0\x9f\xd1\x80\xd0\xb8\xd0\xb2\xd0\xb5\xd1\x82, hodeco!".to_vec(),
            ),
            new_message(
                now - TimeDelta::seconds(50),
                &http,
                MessageState::Incoming,
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nこんにちは、hodeco。日本語のテスト。".as_bytes().to_vec(),
            ),
            new_message(
                now - TimeDelta::seconds(45),
                &http,
                MessageState::Incoming,
                utf16le_payload("Привет 日本 — UTF-16 LE"),
            ),
            new_message(
                now - TimeDelta::seconds(40),
                &http,
                MessageState::Incoming,
                utf16be_payload("Привет 日本 — UTF-16 BE"),
            ),
            new_message(
                now - TimeDelta::seconds(55),
                &http,
                MessageState::Outgoing,
                large_http_request_payload(100_000),
            ),
            new_message(
                now - TimeDelta::seconds(20),
                &http,
                MessageState::Incoming,
                large_http_response_payload(102_400),
            ),
        ];
        messages.sort_by_key(|message| message.time);
        let mut last_time = messages.first().map(|message| message.time).unwrap_or(now);
        for message in messages.iter_mut().skip(1) {
            if message.time <= last_time {
                message.time = last_time
                    .checked_add_signed(TimeDelta::nanoseconds(1))
                    .unwrap_or(last_time);
            }
            last_time = message.time;
        }

        let config = Config {
            services: services
                .iter()
                .map(|service| ServiceConfig {
                    alias: service.alias.clone(),
                    protocol: service.protocol.clone(),
                    transport: service.transport.clone(),
                    entry: service.entry.clone(),
                    params: service.params.clone(),
                })
                .collect(),
            ..Config::default()
        };

        Self {
            runtime_config: config.clone(),
            config,
            services,
            messages,
            last_time,
            subscribed: Vec::new(),
            discovery: None,
            discovery_finished: false,
            pending_outgoing: Vec::new(),
            started_at: now - TimeDelta::hours(2),
            ticks: 0,
            next_traffic: 0,
        }
    }

    /// Handles one request envelope and returns the response followed by any
    /// notifications caused by the request.
    pub fn handle(&mut self, text: &str) -> Vec<String> {
        let request = match serde_json::from_str::<Request>(text) {
            Ok(request) => request,
            Err(error) => return vec![invalid_request(text, &error.to_string())],
        };

        let id = request.id.clone();
        let mut notifications = Vec::new();
        let result = self.dispatch(&request, &mut notifications);

        let response = match result {
            Ok(value) => serde_json::to_string(&Response::success(id, value)),
            Err(error) => serde_json::to_string(&Response::error(id, error)),
        };
        let mut output = Vec::with_capacity(notifications.len() + 1);
        output.push(response.expect("demo response serializes"));
        output.extend(notifications);
        output
    }

    fn dispatch(
        &mut self,
        request: &Request,
        notifications: &mut Vec<String>,
    ) -> Result<Value, ApiError> {
        match request.method {
            Method::SystemRestart => {
                let _: Empty = parse(&request.params)?;
                Ok(json!({}))
            }
            Method::ServerInfo => to_value(ServerInfoResult {
                version: format!("{}-demo", env!("CARGO_PKG_VERSION")),
                protocol_version: DEMO_PROTOCOL_VERSION.to_string(),
                started_at: self.started_at,
            }),
            Method::ConfigGet => to_value(self.config()),
            Method::ConfigUpdate => {
                let params: ConfigUpdateParams = parse(&request.params)?;
                // Match the server: services are managed through services.*,
                // so a config update must never replace them with a stale draft.
                let mut config = params.config;
                config.services = self.config.services.clone();
                validate_demo_config(&config)?;
                let restart_required = demo_restart_required(&self.runtime_config, &config);
                self.config = config.clone();
                to_value(ConfigUpdateResult {
                    config,
                    restart_required,
                })
            }
            Method::ProtocolsList => to_value(ProtocolsListResult {
                protocols: protocol_infos(),
            }),
            Method::TransportsList => to_value(TransportsListResult {
                transports: transport_infos(),
            }),
            Method::ProtocolsInfo => {
                let params: ProtocolInfoParams = parse(&request.params)?;
                let protocol = match params.protocol.as_str() {
                    "zwave" => catalog::zwave_protocol_info(),
                    "http" => http_protocol_info(),
                    _ => {
                        return Err(ApiError::new(
                            ErrorCode::UnknownProtocol,
                            format!("unknown protocol '{}'", params.protocol),
                        ));
                    }
                };
                to_value(ProtocolInfoResult { protocol })
            }
            Method::ProtocolsParams => {
                let params: ProtocolParamsParams = parse(&request.params)?;
                let params = protocol_transport_params(&params.protocol, &params.transport)?;
                to_value(ProtocolParamsResult { params })
            }
            Method::DiscoveryStart => {
                let params: DiscoveryStartParams = parse(&request.params)?;
                if !matches!(params.protocol.as_str(), "zwave" | "http") {
                    return Err(ApiError::new(
                        ErrorCode::UnknownProtocol,
                        format!("unknown protocol '{}'", params.protocol),
                    ));
                }
                if !matches!(params.transport.as_str(), "serial" | "tcp") {
                    return Err(ApiError::new(
                        ErrorCode::UnknownTransport,
                        format!("unknown transport '{}'", params.transport),
                    ));
                }
                let discovery = DiscoveryInfo {
                    id: Uuid::new_v4(),
                    protocol: params.protocol,
                    transport: params.transport,
                    state: DiscoveryState::Running,
                    entries: Vec::new(),
                };
                self.discovery = Some(discovery.clone());
                self.discovery_finished = false;
                notifications.push(notification(Notification::DiscoveryStarted(
                    DiscoveryNotification {
                        discovery: discovery.clone(),
                    },
                )));
                to_value(DiscoveryStartResult {
                    discovery_id: discovery.id,
                })
            }
            Method::DiscoveryGet => {
                let params: DiscoveryGetParams = parse(&request.params)?;
                match &self.discovery {
                    Some(discovery) if params.discovery_id.is_none_or(|id| id == discovery.id) => {
                        to_value(DiscoveryGetResult {
                            discovery: discovery.clone(),
                        })
                    }
                    _ => Err(ApiError::new(
                        ErrorCode::DiscoveryNotFound,
                        "no discovery has been started",
                    )),
                }
            }
            Method::DiscoveryCancel => {
                let params: DiscoveryCancelParams = parse(&request.params)?;
                match &mut self.discovery {
                    Some(discovery) if discovery.id == params.discovery_id => {
                        discovery.state = DiscoveryState::Cancelled;
                        self.discovery_finished = true;
                        let finished = discovery.clone();
                        notifications.push(notification(Notification::DiscoveryFinished(
                            DiscoveryNotification {
                                discovery: finished,
                            },
                        )));
                        Ok(json!({}))
                    }
                    _ => Err(ApiError::new(
                        ErrorCode::DiscoveryNotFound,
                        "discovery not found",
                    )),
                }
            }
            Method::ServicesList => to_value(ServicesListResult {
                services: self.services.clone(),
            }),
            Method::ServicesAdd => {
                let params: ServicesAddParams = parse(&request.params)?;
                let key = ServiceRef::Key {
                    protocol: params.protocol.clone(),
                    transport: params.transport.clone(),
                    entry: params.entry.clone(),
                };
                if self.services.iter().any(|service| same_key(service, &key)) {
                    return Err(ApiError::new(
                        ErrorCode::ServiceAlreadyExists,
                        format!("a service for {} already exists", params.entry),
                    ));
                }
                let alias = params
                    .alias
                    .as_deref()
                    .map(str::trim)
                    .filter(|alias| !alias.is_empty())
                    .map(str::to_string);
                if let Some(alias) = &alias
                    && self
                        .services
                        .iter()
                        .any(|service| service.alias.as_deref() == Some(alias.as_str()))
                {
                    return Err(ApiError::new(
                        ErrorCode::ServiceAliasConflict,
                        format!("the alias '{alias}' is already in use"),
                    ));
                }
                let service = ServiceInfo {
                    alias,
                    protocol: params.protocol,
                    transport: params.transport,
                    entry: params.entry,
                    status: ServiceStatus {
                        state: ServiceState::Running,
                        error: None,
                    },
                    params: params.params,
                };
                self.config.services.push(ServiceConfig {
                    alias: service.alias.clone(),
                    protocol: service.protocol.clone(),
                    transport: service.transport.clone(),
                    entry: service.entry.clone(),
                    params: service.params.clone(),
                });
                self.services.push(service.clone());
                to_value(ServicesAddResult { service })
            }
            Method::ServicesRemove => {
                let params: ServicesRemoveParams = parse(&request.params)?;
                let Some(index) = self
                    .services
                    .iter()
                    .position(|service| matches_ref(service, &params.service))
                else {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotFound,
                        "service not found",
                    ));
                };
                let key = ServiceKey {
                    protocol: self.services[index].protocol.clone(),
                    transport: self.services[index].transport.clone(),
                    entry: self.services[index].entry.clone(),
                };
                self.services.remove(index);
                self.config
                    .services
                    .retain(|entry| !service_config_matches(entry, &key));
                Ok(json!({}))
            }
            Method::ServicesAlias => {
                let params: ServicesAliasParams = parse(&request.params)?;
                let requested = params
                    .alias
                    .as_deref()
                    .map(str::trim)
                    .filter(|alias| !alias.is_empty())
                    .map(str::to_string);
                if let Some(alias) = &requested
                    && self.services.iter().any(|service| {
                        service.alias.as_deref() == Some(alias.as_str())
                            && !matches_ref(service, &params.service)
                    })
                {
                    return Err(ApiError::new(
                        ErrorCode::ServiceAliasConflict,
                        format!("the alias '{alias}' is already in use"),
                    ));
                }
                let Some(service) = self
                    .services
                    .iter_mut()
                    .find(|service| matches_ref(service, &params.service))
                else {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotFound,
                        "service not found",
                    ));
                };
                let key = ServiceKey {
                    protocol: service.protocol.clone(),
                    transport: service.transport.clone(),
                    entry: service.entry.clone(),
                };
                service.alias = requested;
                if let Some(entry) = self
                    .config
                    .services
                    .iter_mut()
                    .find(|entry| service_config_matches(entry, &key))
                {
                    entry.alias = service.alias.clone();
                }
                to_value(ServicesAliasResult {
                    service: service.clone(),
                })
            }
            Method::ServicesStatus => {
                let params: ServicesStatusParams = parse(&request.params)?;
                let Some(service) = self
                    .services
                    .iter()
                    .find(|service| matches_ref(service, &params.service))
                else {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotFound,
                        "service not found",
                    ));
                };
                to_value(ServicesStatusResult {
                    status: service.status.clone(),
                })
            }
            Method::ServicesParams => {
                let params: ServicesParamsParams = parse(&request.params)?;
                let Some(service) = self
                    .services
                    .iter_mut()
                    .find(|service| matches_ref(service, &params.service))
                else {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotFound,
                        "service not found",
                    ));
                };
                let key = ServiceKey {
                    protocol: service.protocol.clone(),
                    transport: service.transport.clone(),
                    entry: service.entry.clone(),
                };
                service.params = params.params.clone();
                if let Some(entry) = self
                    .config
                    .services
                    .iter_mut()
                    .find(|entry| service_config_matches(entry, &key))
                {
                    entry.params = params.params;
                }
                to_value(ServicesParamsResult {
                    service: service.clone(),
                })
            }
            Method::ServicesSend => {
                let params: ServicesSendParams = parse(&request.params)?;
                let Some(service) = self
                    .services
                    .iter()
                    .find(|service| matches_ref(service, &params.service))
                else {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotFound,
                        "service not found",
                    ));
                };
                if service.status.state != ServiceState::Running {
                    return Err(ApiError::new(
                        ErrorCode::ServiceNotReady,
                        "service is not running",
                    ));
                }
                let time = self.next_time(Utc::now());
                let message = new_message(
                    time,
                    &params.service,
                    MessageState::OutgoingPending,
                    params.payload.into_inner(),
                );
                self.pending_outgoing.push(message.id);
                self.messages.push(message.clone());
                notifications.push(notification(Notification::MessageNew(
                    MessageNotification {
                        message: message.clone(),
                    },
                )));
                to_value(ServicesSendResult { message })
            }
            Method::MessagesGet => {
                let params: MessagesGetParams = parse(&request.params)?;
                let Some(message) = self.messages.iter().find(|message| message.id == params.id)
                else {
                    return Err(ApiError::new(
                        ErrorCode::MessageNotFound,
                        "message not found",
                    ));
                };
                to_value(MessagesGetResult {
                    message: message.clone(),
                })
            }
            Method::MessagesList => {
                let params: MessagesListParams = parse(&request.params)?;
                let limit = params.limit.unwrap_or(100).clamp(1, 10_000) as usize;
                if params
                    .from
                    .is_some_and(|from| params.until.is_some_and(|until| from >= until))
                {
                    return to_value(MessagesListResult {
                        messages: Vec::new(),
                        next: None,
                        store_total: self.messages.len() as u64,
                    });
                }
                let ids: HashSet<Uuid> = params.ids.iter().copied().collect();
                let mut selected: Vec<Message> = self
                    .messages
                    .iter()
                    .filter(|message| {
                        params.from.is_none_or(|from| message.time >= from)
                            && params.until.is_none_or(|until| message.time < until)
                            && (ids.is_empty() || ids.contains(&message.id))
                            && self.message_matches(message, &params)
                    })
                    .cloned()
                    .collect();
                selected.sort_by_key(|message| message.time);
                if params.order == MessageOrder::NewestFirst {
                    selected.reverse();
                }
                let has_more = selected.len() > limit;
                selected.truncate(limit);
                let next = if has_more {
                    selected.last().map(|message| match params.order {
                        MessageOrder::NewestFirst => MessageTimeRange {
                            from: params.from,
                            until: Some(message.time),
                        },
                        MessageOrder::OldestFirst => MessageTimeRange {
                            from: message
                                .time
                                .checked_add_signed(TimeDelta::nanoseconds(1))
                                .or(params.from),
                            until: params.until,
                        },
                    })
                } else {
                    None
                };
                to_value(MessagesListResult {
                    messages: selected,
                    next,
                    store_total: self.messages.len() as u64,
                })
            }
            Method::EventsSubscribe => {
                let params: EventsSubscribeParams = parse(&request.params)?;
                for topic in params.topics {
                    if !self.subscribed.contains(&topic) {
                        self.subscribed.push(topic);
                    }
                }
                Ok(json!({}))
            }
            Method::EventsUnsubscribe => {
                let params: EventsUnsubscribeParams = parse(&request.params)?;
                for topic in params.topics {
                    if let Some(position) = self.subscribed.iter().position(|value| *value == topic)
                    {
                        self.subscribed.remove(position);
                    }
                }
                Ok(json!({}))
            }
        }
    }

    /// Produces live traffic and discovery transitions. Called on a timer by
    /// the demo transport.
    pub fn tick(&mut self) -> Vec<String> {
        self.ticks += 1;
        let mut output = Vec::new();

        if let Some(discovery) = &mut self.discovery
            && discovery.state == DiscoveryState::Running
            && !self.discovery_finished
        {
            self.discovery_finished = true;
            discovery.state = DiscoveryState::Finished;
            discovery.entries = discovery_entries(&discovery.protocol);
            let finished = discovery.clone();
            output.push(notification(Notification::DiscoveryFinished(
                DiscoveryNotification {
                    discovery: finished,
                },
            )));
        }

        // Promote outgoing messages one tick after they were queued.
        let pending = std::mem::take(&mut self.pending_outgoing);
        for id in pending {
            if let Some(message) = self.messages.iter_mut().find(|message| message.id == id)
                && message.state == MessageState::OutgoingPending
            {
                message.state = MessageState::Outgoing;
                output.push(notification(Notification::MessageStateChanged(
                    MessageNotification {
                        message: message.clone(),
                    },
                )));
            }
        }

        if self.subscribed.contains(&EventTopic::Messages) {
            let payload = traffic_payload(self.next_traffic);
            self.next_traffic += 1;
            let time = self.next_time(Utc::now());
            let message = new_message(
                time,
                &self.traffic_service(),
                MessageState::Incoming,
                payload,
            );
            self.messages.push(message.clone());
            output.push(notification(Notification::MessageNew(
                MessageNotification { message },
            )));
        }

        output
    }

    fn message_matches(&self, message: &Message, params: &MessagesListParams) -> bool {
        if !params.states.is_empty() && !params.states.contains(&message.state) {
            return false;
        }
        if !params.directions.is_empty() && !params.directions.contains(&message.state.direction())
        {
            return false;
        }
        let (protocol, transport) = self.message_service_key(message);
        if !params.protocols.is_empty() && !params.protocols.contains(&protocol) {
            return false;
        }
        if !params.transports.is_empty() && !params.transports.contains(&transport) {
            return false;
        }
        if !params.services.is_empty()
            && !params
                .services
                .iter()
                .any(|reference| matches_message_ref(message, reference))
        {
            return false;
        }
        if let Some(payload) = &params.payload
            && !payload.matches(message.payload.as_slice())
        {
            return false;
        }
        true
    }

    fn message_service_key(&self, message: &Message) -> (String, String) {
        match &message.service {
            ServiceRef::Key {
                protocol,
                transport,
                ..
            } => (protocol.clone(), transport.clone()),
            ServiceRef::Alias { alias } => self
                .services
                .iter()
                .find(|service| service.alias.as_deref() == Some(alias.as_str()))
                .map(|service| (service.protocol.clone(), service.transport.clone()))
                .unwrap_or_default(),
        }
    }

    fn next_time(&mut self, now: DateTime<Utc>) -> DateTime<Utc> {
        let time = if now <= self.last_time {
            self.last_time
                .checked_add_signed(TimeDelta::nanoseconds(1))
                .unwrap_or(now)
        } else {
            now
        };
        self.last_time = time;
        time
    }

    fn traffic_service(&self) -> ServiceRef {
        self.services
            .iter()
            .find(|service| service.status.state == ServiceState::Running)
            .map(|service| match &service.alias {
                Some(alias) => ServiceRef::Alias {
                    alias: alias.clone(),
                },
                None => ServiceRef::Key {
                    protocol: service.protocol.clone(),
                    transport: service.transport.clone(),
                    entry: service.entry.clone(),
                },
            })
            .unwrap_or_else(|| ServiceRef::Alias {
                alias: "ZW".to_string(),
            })
    }

    pub fn config(&self) -> Config {
        self.config.clone()
    }
}

fn invalid_request(text: &str, reason: &str) -> String {
    let value = serde_json::from_str::<Value>(text).ok();
    let id = value
        .as_ref()
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(method) = value
        .as_ref()
        .and_then(|value| value.get("method"))
        .and_then(Value::as_str)
        && method.parse::<Method>().is_err()
    {
        return response_error(
            id,
            ApiError::new(
                ErrorCode::UnknownMethod,
                format!("unknown method '{method}'"),
            ),
        );
    }
    response_error(
        id,
        ApiError::new(
            ErrorCode::InvalidRequest,
            format!("invalid request: {reason}"),
        ),
    )
}

fn response_error(id: &str, error: ApiError) -> String {
    serde_json::to_string(&Response::error(id, error)).expect("error response serializes")
}

fn parse<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, ApiError> {
    serde_json::from_value(params.clone()).map_err(|error| {
        ApiError::new(ErrorCode::InvalidParams, format!("invalid params: {error}"))
    })
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, ApiError> {
    serde_json::to_value(value)
        .map_err(|error| ApiError::new(ErrorCode::InternalError, error.to_string()))
}

fn notification(notification: Notification) -> String {
    serde_json::to_string(&notification).expect("notification serializes")
}

fn new_message(
    time: DateTime<Utc>,
    service: &ServiceRef,
    state: MessageState,
    payload: Vec<u8>,
) -> Message {
    Message {
        id: Uuid::new_v4(),
        time,
        service: service.clone(),
        state,
        payload: Base64Bytes::from(payload),
    }
}

fn traffic_payload(index: usize) -> Vec<u8> {
    match index % 5 {
        0 => frame_request(&[0x04, 0x00, 0x04, 0x03, 0x20, 0x01, 0x45]),
        1 => frame_request(&[0x04, 0x00, 0x02, 0x03, 0x25, 0x03, 0xff]),
        2 => frame_request(&[0x04, 0x00, 0x07, 0x04, 0x26, 0x03, 0x48, 0x05]),
        3 => frame_request(&[
            0x04, 0x00, 0x06, 0x08, 0x32, 0x02, 0x01, 0x00, 0x00, 0x00, 0x30, 0x39,
        ]),
        _ => frame_response(&DEMO_GET_VERSION),
    }
}

fn utf16le_payload(text: &str) -> Vec<u8> {
    let mut payload = vec![0xff, 0xfe];
    payload.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    payload
}

fn utf16be_payload(text: &str) -> Vec<u8> {
    let mut payload = vec![0xfe, 0xff];
    payload.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
    payload
}

fn large_http_request_payload(size: usize) -> Vec<u8> {
    large_http_payload(
        size,
        "POST /api/v1/telemetry HTTP/1.1\r\nHost: api.example.test\r\nContent-Type: application/json\r\nX-Demo-Message: large-http-request\r\n\r\n",
    )
}

fn large_http_response_payload(size: usize) -> Vec<u8> {
    large_http_payload(
        size,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Demo-Message: large-http-response\r\n\r\n",
    )
}

/// Builds a JSON-shaped HTTP body of exactly `size` bytes so the demo can
/// exercise large raw payloads without any protocol-specific parser.
fn large_http_payload(size: usize, header: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(size);
    payload.extend_from_slice(header.as_bytes());
    payload.extend_from_slice(b"{\"demo\":\"large-http\",\"padding\":\"");
    let suffix = b"\"}";
    let body_len = size.saturating_sub(payload.len() + suffix.len());
    let pattern = b"0123456789abcdef";
    payload.extend(pattern.iter().copied().cycle().take(body_len));
    payload.extend_from_slice(suffix);
    payload.truncate(size);
    payload
}

fn discovery_entries(protocol: &str) -> Vec<DiscoveryEntry> {
    if protocol == "http" {
        vec![
            DiscoveryEntry {
                entry: "api.example.test:443".to_string(),
                available: true,
                description: Some("Demo HTTP endpoint".to_string()),
                error: None,
            },
            DiscoveryEntry {
                entry: "localhost:8080".to_string(),
                available: true,
                description: Some("Local HTTP endpoint".to_string()),
                error: None,
            },
            DiscoveryEntry {
                entry: "blocked.example.test:80".to_string(),
                available: false,
                description: None,
                error: Some("connection refused".to_string()),
            },
        ]
    } else {
        vec![
            DiscoveryEntry {
                entry: "COM3".to_string(),
                available: false,
                description: None,
                error: Some("access denied".to_string()),
            },
            DiscoveryEntry {
                entry: "COM4".to_string(),
                available: true,
                description: Some("USB serial controller".to_string()),
                error: None,
            },
            DiscoveryEntry {
                entry: "COM7".to_string(),
                available: true,
                description: Some("USB serial controller".to_string()),
                error: None,
            },
        ]
    }
}

fn service_config_matches(service: &ServiceConfig, key: &ServiceKey) -> bool {
    service.protocol == key.protocol
        && service.transport == key.transport
        && service.entry == key.entry
}

fn demo_restart_required(previous: &Config, next: &Config) -> Vec<String> {
    let mut fields = Vec::new();
    if previous.working_dir != next.working_dir {
        fields.push("workingDir".to_string());
    }
    if previous.log != next.log {
        fields.push("log".to_string());
    }
    if previous.http_server != next.http_server {
        fields.push("httpServer".to_string());
    }
    if previous.message_log != next.message_log {
        fields.push("messageLog".to_string());
    }
    fields
}

fn validate_demo_config(config: &Config) -> Result<(), ApiError> {
    let mut errors = Vec::new();
    if config.working_dir.as_os_str().is_empty() {
        errors.push("workingDir must not be empty".to_string());
    }
    if config.log.dir.as_os_str().is_empty() {
        errors.push("log.dir must not be empty".to_string());
    }
    if config.log.file.trim().is_empty() {
        errors.push("log.file must not be empty".to_string());
    }
    if config.http_server.bind.trim().is_empty() {
        errors.push("httpServer.bind must not be empty".to_string());
    }
    if config.http_server.port == 0 {
        errors.push("httpServer.port must be greater than 0".to_string());
    }
    if config.message_log.file.as_os_str().is_empty() {
        errors.push("messageLog.file must not be empty".to_string());
    }
    let mut aliases = std::collections::BTreeSet::new();
    let mut keys = std::collections::BTreeSet::new();
    for (index, service) in config.services.iter().enumerate() {
        if service.protocol.trim().is_empty() {
            errors.push(format!("services[{index}].protocol must not be empty"));
        }
        if service.transport.trim().is_empty() {
            errors.push(format!("services[{index}].transport must not be empty"));
        }
        if service.entry.trim().is_empty() {
            errors.push(format!("services[{index}].entry must not be empty"));
        }
        if let Some(alias) = &service.alias {
            if alias.trim().is_empty() {
                errors.push(format!("services[{index}].alias must not be empty"));
            } else if !aliases.insert(alias.clone()) {
                errors.push(format!("duplicate service alias '{alias}'"));
            }
        }
        let key = (
            service.protocol.clone(),
            service.transport.clone(),
            service.entry.clone(),
        );
        if !keys.insert(key.clone()) {
            errors.push(format!(
                "duplicate service key '{}:{}:{}'",
                key.0, key.1, key.2
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::new(ErrorCode::ConfigInvalid, errors.join("; "))
            .with_details(json!({ "errors": errors })))
    }
}

fn protocol_transport_params(protocol: &str, transport: &str) -> Result<Vec<ParamDef>, ApiError> {
    match (protocol, transport) {
        ("zwave", "serial") => {
            let mut definitions = catalog::serial_params();
            definitions.extend(catalog::zwave_params());
            Ok(definitions)
        }
        ("http", "tcp") => Ok(tcp_transport_info().params),
        (protocol, _) if !matches!(protocol, "zwave" | "http") => Err(ApiError::new(
            ErrorCode::UnknownProtocol,
            format!("unknown protocol '{protocol}'"),
        )),
        (_, transport) => Err(ApiError::new(
            ErrorCode::UnknownTransport,
            format!("unknown transport '{transport}'"),
        )),
    }
}

fn same_key(service: &ServiceInfo, reference: &ServiceRef) -> bool {
    matches!(
        reference,
        ServiceRef::Key {
            protocol,
            transport,
            entry
        } if service.protocol == *protocol
            && service.transport == *transport
            && service.entry == *entry
    )
}

fn matches_ref(service: &ServiceInfo, reference: &ServiceRef) -> bool {
    match reference {
        ServiceRef::Alias { alias } => service.alias.as_deref() == Some(alias.as_str()),
        ServiceRef::Key { .. } => same_key(service, reference),
    }
}

fn matches_message_ref(message: &Message, reference: &ServiceRef) -> bool {
    match (&message.service, reference) {
        (ServiceRef::Alias { alias }, ServiceRef::Alias { alias: other }) => alias == other,
        (
            ServiceRef::Key {
                protocol,
                transport,
                entry,
            },
            ServiceRef::Key {
                protocol: other_protocol,
                transport: other_transport,
                entry: other_entry,
            },
        ) => protocol == other_protocol && transport == other_transport && entry == other_entry,
        _ => false,
    }
}

fn protocol_infos() -> Vec<ProtocolInfo> {
    vec![catalog::zwave_protocol_info(), http_protocol_info()]
}

fn http_protocol_info() -> ProtocolInfo {
    ProtocolInfo {
        id: "http".to_string(),
        name: "HTTP".to_string(),
        description: Some("Raw HTTP request/response byte relay".to_string()),
        transports: vec!["tcp".to_string()],
        params: Vec::new(),
    }
}

fn transport_infos() -> Vec<TransportInfo> {
    vec![catalog::serial_transport_info(), tcp_transport_info()]
}

fn tcp_transport_info() -> TransportInfo {
    TransportInfo {
        id: "tcp".to_string(),
        name: "TCP".to_string(),
        description: Some("TCP stream transport".to_string()),
        params: vec![
            ParamDef::with_default(
                "connectTimeout",
                ParamType::Duration,
                json!("10 s"),
                "Connection timeout",
            ),
            ParamDef::with_default(
                "readTimeout",
                ParamType::Duration,
                json!("30 s"),
                "Read timeout",
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, method: &str, params: Value) -> String {
        json!({"id": id, "method": method, "params": params}).to_string()
    }

    fn parse_response(text: &str) -> Value {
        serde_json::from_str(text).expect("response is JSON")
    }

    #[test]
    fn lists_fixture_services_and_messages() {
        let mut server = DemoServer::new();
        let responses = server.handle(&request("1", "services.list", json!({})));
        assert_eq!(responses.len(), 1);
        let response = parse_response(&responses[0]);
        assert_eq!(response["id"], "1");
        assert_eq!(response["result"]["services"].as_array().unwrap().len(), 4);

        let responses = server.handle(&request("2", "messages.list", json!({"limit": 5})));
        let response = parse_response(&responses[0]);
        assert_eq!(response["result"]["messages"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn demo_exposes_a_non_zwave_protocol_with_large_messages() {
        let server = DemoServer::new();
        assert!(
            server
                .services
                .iter()
                .any(|service| service.protocol == "http" && service.transport == "tcp")
        );
        assert!(
            protocol_infos()
                .iter()
                .any(|protocol| protocol.id == "http")
        );
        assert!(
            transport_infos()
                .iter()
                .any(|transport| transport.id == "tcp")
        );

        let http_messages: Vec<_> = server
            .messages
            .iter()
            .filter(|message| {
                matches!(
                    &message.service,
                    ServiceRef::Alias { alias } if alias == "HTTP"
                )
            })
            .collect();
        assert_eq!(http_messages.len(), 6);
        assert_eq!(
            http_messages
                .iter()
                .filter(|message| message.payload.as_slice().len() >= 100_000)
                .count(),
            2
        );
        assert!(http_messages.iter().any(|message| {
            String::from_utf8_lossy(message.payload.as_slice()).contains("こんにちは")
        }));
        assert!(http_messages.iter().any(|message| {
            String::from_utf8_lossy(message.payload.as_slice()).contains("Привет")
        }));
    }

    #[test]
    fn discovery_runs_and_finishes() {
        let mut server = DemoServer::new();
        let responses = server.handle(&request(
            "1",
            "discovery.start",
            json!({"protocol": "zwave", "transport": "serial"}),
        ));
        assert_eq!(responses.len(), 2);
        assert!(responses[1].contains("events.discoveryStarted"));

        let output = server.tick();
        assert!(
            output
                .iter()
                .any(|event| event.contains("events.discoveryFinished"))
        );

        let responses = server.handle(&request("2", "discovery.get", json!({})));
        let response = parse_response(&responses[0]);
        assert_eq!(response["result"]["discovery"]["state"], "finished");
        assert_eq!(
            response["result"]["discovery"]["entries"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn http_discovery_returns_http_entries() {
        let mut server = DemoServer::new();
        let responses = server.handle(&request(
            "1",
            "discovery.start",
            json!({"protocol": "http", "transport": "tcp"}),
        ));
        assert_eq!(responses.len(), 2);
        assert!(responses[1].contains("events.discoveryStarted"));

        server.tick();
        let discovery = server.discovery.as_ref().expect("discovery exists");
        assert_eq!(discovery.protocol, "http");
        assert!(
            discovery
                .entries
                .iter()
                .any(|entry| entry.entry == "localhost:8080")
        );
    }

    #[test]
    fn send_creates_and_promotes_an_outgoing_message() {
        let mut server = DemoServer::new();
        server.handle(&request(
            "1",
            "events.subscribe",
            json!({"topics": ["messages", "discovery", "services"]}),
        ));
        let responses = server.handle(&request(
            "2",
            "services.send",
            json!({"service": {"alias": "ZW"}, "payload": "AQID"}),
        ));
        assert!(responses[0].contains("\"result\""));
        assert!(responses[1].contains("events.messageNew"));

        let output = server.tick();
        assert!(
            output
                .iter()
                .any(|event| event.contains("events.messageStateChanged"))
        );
    }

    #[test]
    fn rejects_unknown_methods_and_services() {
        let mut server = DemoServer::new();
        let response = parse_response(&server.handle("{\"id\":\"1\",\"method\":\"nope\"}")[0]);
        assert_eq!(response["error"]["code"], "unknownMethod");

        let response = parse_response(
            &server.handle(&request(
                "2",
                "services.status",
                json!({"service": {"alias": "missing"}}),
            ))[0],
        );
        assert_eq!(response["error"]["code"], "serviceNotFound");
    }

    #[test]
    fn filters_messages_by_payload_expression_and_ids() {
        let mut server = DemoServer::new();
        let payload = frame_request(&[0x04, 0x00, 0x04, 0x03, 0x20, 0x01, 0x2d]);
        let encoded = Base64Bytes::from(payload).to_string();
        let response = parse_response(
            &server.handle(&request(
                "1",
                "messages.list",
                json!({
                    "payload": {"offset": 0, "bytes": encoded},
                    "states": [0],
                    "order": 1
                }),
            ))[0],
        );
        let messages = response["result"]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["state"], serde_json::json!(0));
        let id = messages[0]["id"].as_str().unwrap().to_string();

        let response = parse_response(
            &server.handle(&request(
                "2",
                "messages.list",
                json!({"ids": [id, "00000000-0000-0000-0000-000000000000"]}),
            ))[0],
        );
        assert_eq!(response["result"]["messages"].as_array().unwrap().len(), 1);
        assert!(response["result"]["storeTotal"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn updates_config_and_service_parameters() {
        let mut server = DemoServer::new();
        let mut config = server.config();
        config.http_server.port = 9000;
        let response = parse_response(
            &server.handle(&request("1", "config.update", json!({"config": config})))[0],
        );
        assert!(response["error"].is_null());
        assert_eq!(response["result"]["restartRequired"], json!(["httpServer"]));
        assert_eq!(server.config().http_server.port, 9000);

        let response = parse_response(
            &server.handle(&request(
                "2",
                "services.params",
                json!({"service": {"alias": "HTTP"}, "params": {"readTimeout": "45 s"}}),
            ))[0],
        );
        assert!(response["error"].is_null());
        assert_eq!(
            response["result"]["service"]["params"]["readTimeout"],
            "45 s"
        );
        let service = server
            .config()
            .services
            .into_iter()
            .find(|service| service.alias.as_deref() == Some("HTTP"))
            .expect("HTTP service");
        assert_eq!(service.params["readTimeout"], "45 s");

        let response = parse_response(
            &server.handle(&request(
                "3",
                "protocols.params",
                json!({"protocol": "http", "transport": "tcp"}),
            ))[0],
        );
        assert!(
            response["result"]["params"]
                .as_array()
                .unwrap()
                .iter()
                .any(|definition| definition["name"] == "readTimeout")
        );
    }

    #[test]
    fn config_update_ignores_service_changes() {
        let mut server = DemoServer::new();
        let mut config = server.config();
        config.services.clear();
        config.log.file = "updated.log".to_string();

        let response = parse_response(
            &server.handle(&request("1", "config.update", json!({"config": config})))[0],
        );
        assert!(response["error"].is_null());
        assert_eq!(
            response["result"]["config"]["services"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(response["result"]["restartRequired"], json!(["log"]));
        assert_eq!(server.config().services.len(), 4);
    }

    #[test]
    fn config_matches_the_service_list() {
        let server = DemoServer::new();
        let config = server.config();
        assert_eq!(config.services.len(), 4);
        assert_eq!(config.services[0].entry, "COM4");
    }
}
