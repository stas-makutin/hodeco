use crate::app::AppState;
use crate::config::{ConfigError, restart_required, validate};
use crate::events::Topic;
use crate::messages::MessageFilter;
use crate::services::{ServiceError, catalog};
use hodeco_contract::api::envelope::empty_object;
use hodeco_contract::api::payloads::{
    ConfigUpdateParams, ConfigUpdateResult, DiscoveryCancelParams, DiscoveryGetParams,
    DiscoveryGetResult, DiscoveryStartParams, DiscoveryStartResult, Empty, EventTopic,
    EventsSubscribeParams, EventsUnsubscribeParams, MessagesGetParams, MessagesGetResult,
    MessagesListParams, MessagesListResult, ProtocolInfoParams, ProtocolInfoResult,
    ProtocolParamsParams, ProtocolParamsResult, ProtocolsListResult, ServerInfoResult,
    ServicesAddParams, ServicesAddResult, ServicesAliasParams, ServicesAliasResult,
    ServicesListResult, ServicesParamsParams, ServicesParamsResult, ServicesRemoveParams,
    ServicesSendParams, ServicesSendResult, ServicesStatusParams, ServicesStatusResult,
    TransportsListResult,
};
use hodeco_contract::api::{ApiError, ErrorCode, Method, Response};
use hodeco_contract::config::ServiceConfig;
use hodeco_contract::domain::{
    MessageDirection, MessageState, ParamValues, PayloadExpr, ServiceKey, ServiceRef,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::HashSet;
use tracing::warn;

pub const PROTOCOL_VERSION: &str = "1.0";

const DEFAULT_MESSAGE_LIMIT: u32 = 100;
const MAX_MESSAGE_LIMIT: u32 = 10_000;
const MAX_MESSAGE_IDS: usize = 10_000;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRequest {
    pub id: String,
    pub method: String,
    #[serde(default = "empty_object")]
    pub params: Value,
}

/// Limits the response encoding to a single allocation while keeping the
/// transport handlers independent from serde.
pub fn encode(response: &Response) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(response)
}

/// Subscription state for one WebSocket connection. HTTP requests use a
/// throwaway session because they cannot receive notifications.
#[derive(Debug, Default)]
pub struct Session {
    topics: HashSet<Topic>,
}

impl Session {
    pub fn is_subscribed(&self, topic: Topic) -> bool {
        self.topics.contains(&topic)
    }

    fn subscribe(&mut self, topics: &[EventTopic]) {
        for topic in topics {
            self.topics.insert(topic_from_api(*topic));
        }
    }

    fn unsubscribe(&mut self, topics: &[EventTopic]) {
        for topic in topics {
            self.topics.remove(&topic_from_api(*topic));
        }
    }
}

pub struct DispatchOutcome {
    pub result: Value,
    pub restart: bool,
}

pub async fn execute(
    state: &AppState,
    session: &mut Session,
    request: RawRequest,
) -> (Response, bool) {
    let id = request.id;
    if state.is_shutting_down() {
        return (
            Response::error(
                id,
                ApiError::new(ErrorCode::ServerShuttingDown, "server is shutting down"),
            ),
            false,
        );
    }
    match request.method.parse::<Method>() {
        Ok(method) => match dispatch(state, session, method, request.params).await {
            Ok(outcome) => (Response::success(id, outcome.result), outcome.restart),
            Err(error) => (Response::error(id, error), false),
        },
        Err(error) => (Response::error(id, error), false),
    }
}

async fn dispatch(
    state: &AppState,
    session: &mut Session,
    method: Method,
    params: Value,
) -> Result<DispatchOutcome, ApiError> {
    match method {
        Method::SystemRestart => {
            parse_params::<Empty>(method, params)?;
            Ok(DispatchOutcome {
                result: json!({}),
                restart: true,
            })
        }
        Method::ServerInfo => ok(ServerInfoResult {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            started_at: state.started_at,
        }),
        Method::ConfigGet => ok(state.config_store.snapshot()),
        Method::ConfigUpdate => {
            let params: ConfigUpdateParams = parse_params(method, params)?;
            let _guard = state.config_store.lock().await;
            let previous = state.config_store.snapshot();
            let mut config = params.config;
            // Services have their own runtime API. A general configuration
            // update must never replace them with a stale draft.
            config.services = previous.services.clone();
            validate(&config).map_err(config_validation_error)?;
            let restart_required = restart_required(state.config.as_ref(), &config)
                .into_iter()
                .map(str::to_string)
                .collect();
            state.config_store.replace(config.clone());
            if let Err(error) = state.config_store.save().await {
                state.config_store.replace(previous);
                return Err(config_write_error(error));
            }
            ok(ConfigUpdateResult {
                config,
                restart_required,
            })
        }
        Method::ProtocolsList => ok(ProtocolsListResult {
            protocols: catalog::protocol_infos(),
        }),
        Method::TransportsList => ok(TransportsListResult {
            transports: catalog::transport_infos(),
        }),
        Method::ProtocolsInfo => {
            let params: ProtocolInfoParams = parse_params(method, params)?;
            let protocol = catalog::protocol_info(&params.protocol).ok_or_else(|| {
                ApiError::new(
                    ErrorCode::UnknownProtocol,
                    format!("unknown protocol '{}'", params.protocol),
                )
                .with_details(json!({"protocol": params.protocol}))
            })?;
            ok(ProtocolInfoResult { protocol })
        }
        Method::ProtocolsParams => {
            let params: ProtocolParamsParams = parse_params(method, params)?;
            let params = catalog::protocol_transport_params(&params.protocol, &params.transport)
                .map_err(service_error)?;
            ok(ProtocolParamsResult { params })
        }
        Method::DiscoveryStart => {
            let params: DiscoveryStartParams = parse_params(method, params)?;
            let values = ParamValues::from(params.params);
            let info = state
                .discovery
                .start(&params.protocol, &params.transport, &values)
                .map_err(service_error)?;
            ok(DiscoveryStartResult {
                discovery_id: info.id,
            })
        }
        Method::DiscoveryGet => {
            let params: DiscoveryGetParams = parse_params(method, params)?;
            let discovery = state
                .discovery
                .get(params.discovery_id)
                .map_err(service_error)?;
            ok(DiscoveryGetResult { discovery })
        }
        Method::DiscoveryCancel => {
            let params: DiscoveryCancelParams = parse_params(method, params)?;
            state
                .discovery
                .cancel(params.discovery_id)
                .map_err(service_error)?;
            ok(Empty::default())
        }
        Method::ServicesList => ok(ServicesListResult {
            services: state.registry.list(),
        }),
        Method::ServicesAdd => {
            let params: ServicesAddParams = parse_params(method, params)?;
            let key = ServiceKey {
                protocol: params.protocol,
                transport: params.transport,
                entry: params.entry,
            };
            let values = ParamValues::from(params.params);
            let _guard = state.config_store.lock().await;
            let previous = state.config_store.snapshot();
            let service = state
                .registry
                .add(key.clone(), &values, params.alias)
                .map_err(service_error)?;
            state.config_store.update(|config| {
                config.services.push(ServiceConfig {
                    alias: service.alias.clone(),
                    protocol: key.protocol.clone(),
                    transport: key.transport.clone(),
                    entry: key.entry.clone(),
                    params: values.clone(),
                });
            });
            if let Err(error) = state.config_store.save().await {
                state.config_store.replace(previous);
                if let Err(error) = state.registry.remove(&ServiceRef::from(&key)).await {
                    warn!(error = %error, "failed to roll back service after config write failure");
                }
                return Err(config_write_error(error));
            }
            ok(ServicesAddResult { service })
        }
        Method::ServicesRemove => {
            let params: ServicesRemoveParams = parse_params(method, params)?;
            let _guard = state.config_store.lock().await;
            let key = state
                .registry
                .resolve(&params.service)
                .map_err(|error| service_ref_error(error, &params.service))?;
            let service_info = state.registry.info(&params.service).ok();
            let previous = state.config_store.snapshot();
            state
                .registry
                .remove(&params.service)
                .await
                .map_err(|error| service_ref_error(error, &params.service))?;
            state.config_store.update(|config| {
                config
                    .services
                    .retain(|service| !service_config_matches(service, &key));
            });
            if let Err(error) = state.config_store.save().await {
                state.config_store.replace(previous);
                if let Some(info) = service_info
                    && let Err(error) = state.registry.add(key, &info.params, info.alias)
                {
                    warn!(error = %error, "failed to roll back service removal after config write failure");
                }
                return Err(config_write_error(error));
            }
            ok(Empty::default())
        }
        Method::ServicesAlias => {
            let params: ServicesAliasParams = parse_params(method, params)?;
            let _guard = state.config_store.lock().await;
            let key = state
                .registry
                .resolve(&params.service)
                .map_err(|error| service_ref_error(error, &params.service))?;
            let old_alias = state
                .registry
                .info(&params.service)
                .ok()
                .and_then(|info| info.alias);
            let previous = state.config_store.snapshot();
            let service = state
                .registry
                .alias(&params.service, params.alias)
                .map_err(|error| service_ref_error(error, &params.service))?;
            let new_alias = service.alias.clone();
            state.config_store.update(|config| {
                if let Some(entry) = config
                    .services
                    .iter_mut()
                    .find(|entry| service_config_matches(entry, &key))
                {
                    entry.alias = new_alias;
                }
            });
            if let Err(error) = state.config_store.save().await {
                state.config_store.replace(previous);
                if let Err(error) = state.registry.alias(&ServiceRef::from(&key), old_alias) {
                    warn!(error = %error, "failed to roll back service alias after config write failure");
                }
                return Err(config_write_error(error));
            }
            ok(ServicesAliasResult { service })
        }
        Method::ServicesStatus => {
            let params: ServicesStatusParams = parse_params(method, params)?;
            let status = state
                .registry
                .status(&params.service)
                .map_err(|error| service_ref_error(error, &params.service))?;
            ok(ServicesStatusResult { status })
        }
        Method::ServicesParams => {
            let params: ServicesParamsParams = parse_params(method, params)?;
            let values = ParamValues::from(params.params);
            let _guard = state.config_store.lock().await;
            let key = state
                .registry
                .resolve(&params.service)
                .map_err(|error| service_ref_error(error, &params.service))?;
            let previous_info = state.registry.info(&params.service).ok();
            let previous = state.config_store.snapshot();
            let service = state
                .registry
                .update_params(&params.service, &values)
                .await
                .map_err(|error| service_ref_error(error, &params.service))?;
            state.config_store.update(|config| {
                if let Some(entry) = config
                    .services
                    .iter_mut()
                    .find(|entry| service_config_matches(entry, &key))
                {
                    entry.params = values.clone();
                }
            });
            if let Err(error) = state.config_store.save().await {
                state.config_store.replace(previous);
                if let Some(info) = previous_info
                    && let Err(error) = state
                        .registry
                        .update_params(&params.service, &info.params)
                        .await
                {
                    warn!(error = %error, "failed to roll back service parameters after config write failure");
                }
                return Err(config_write_error(error));
            }
            ok(ServicesParamsResult { service })
        }
        Method::ServicesSend => {
            let params: ServicesSendParams = parse_params(method, params)?;
            let message = state
                .registry
                .send(
                    &params.service,
                    params.payload.into_inner(),
                    params.ttl.map(|ttl| ttl.as_std()),
                )
                .map_err(|error| service_ref_error(error, &params.service))?;
            ok(ServicesSendResult { message })
        }
        Method::MessagesGet => {
            let params: MessagesGetParams = parse_params(method, params)?;
            let message = state.store.get(params.id).ok_or_else(|| {
                ApiError::new(
                    ErrorCode::MessageNotFound,
                    format!("message '{}' not found", params.id),
                )
                .with_details(json!({"id": params.id}))
            })?;
            ok(MessagesGetResult { message })
        }
        Method::MessagesList => {
            let params: MessagesListParams = parse_params(method, params)?;
            if params.ids.len() > MAX_MESSAGE_IDS {
                return Err(ApiError::new(
                    ErrorCode::InvalidParams,
                    format!("ids may contain at most {MAX_MESSAGE_IDS} entries"),
                ));
            }
            let filter = build_message_filter(
                state,
                params.states,
                params.directions,
                params.protocols,
                params.transports,
                params.services,
                params.ids,
                params.payload,
            )?;
            let limit = params
                .limit
                .unwrap_or(DEFAULT_MESSAGE_LIMIT)
                .clamp(1, MAX_MESSAGE_LIMIT) as usize;
            let page = state
                .store
                .list(params.from, params.until, params.order, limit, &filter);
            ok(MessagesListResult {
                messages: page.messages,
                next: page.next,
                store_total: page.store_total,
            })
        }
        Method::EventsSubscribe => {
            let params: EventsSubscribeParams = parse_params(method, params)?;
            session.subscribe(&params.topics);
            ok(Empty::default())
        }
        Method::EventsUnsubscribe => {
            let params: EventsUnsubscribeParams = parse_params(method, params)?;
            session.unsubscribe(&params.topics);
            ok(Empty::default())
        }
    }
}

fn parse_params<T>(method: Method, params: Value) -> Result<T, ApiError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(params).map_err(|error| {
        ApiError::new(
            ErrorCode::InvalidParams,
            format!("invalid params for '{method}': {error}"),
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn build_message_filter(
    state: &AppState,
    states: Vec<MessageState>,
    directions: Vec<MessageDirection>,
    protocols: Vec<String>,
    transports: Vec<String>,
    services: Vec<ServiceRef>,
    ids: Vec<uuid::Uuid>,
    payload: Option<PayloadExpr>,
) -> Result<MessageFilter, ApiError> {
    let mut resolved_services = Vec::with_capacity(services.len());
    for service in &services {
        resolved_services.push(
            state
                .registry
                .resolve(service)
                .map_err(|error| service_ref_error(error, service))?,
        );
    }
    Ok(MessageFilter {
        states,
        directions,
        protocols,
        transports,
        services: resolved_services,
        ids,
        payload,
    })
}

fn ok<T>(result: T) -> Result<DispatchOutcome, ApiError>
where
    T: Serialize,
{
    serde_json::to_value(result)
        .map(|result| DispatchOutcome {
            result,
            restart: false,
        })
        .map_err(|error| {
            ApiError::new(
                ErrorCode::InternalError,
                format!("failed to encode response: {error}"),
            )
        })
}

fn topic_from_api(topic: EventTopic) -> Topic {
    match topic {
        EventTopic::Messages => Topic::Messages,
        EventTopic::Discovery => Topic::Discovery,
        EventTopic::Services => Topic::Services,
    }
}

fn service_error(error: ServiceError) -> ApiError {
    match error {
        ServiceError::NotFound => ApiError::new(ErrorCode::ServiceNotFound, "service not found"),
        ServiceError::AlreadyExists => {
            ApiError::new(ErrorCode::ServiceAlreadyExists, "service already exists")
        }
        ServiceError::AliasConflict => ApiError::new(
            ErrorCode::ServiceAliasConflict,
            "service alias already exists",
        ),
        ServiceError::InvalidKey => {
            ApiError::new(ErrorCode::InvalidParams, "the service key is not valid")
        }
        ServiceError::InvalidAlias => {
            ApiError::new(ErrorCode::InvalidParams, "the service alias is not valid")
        }
        ServiceError::NotReady => ApiError::new(ErrorCode::ServiceNotReady, "service is not ready"),
        ServiceError::SendFailed => {
            ApiError::new(ErrorCode::ServiceSendFailed, "service send failed")
        }
        ServiceError::InvalidPayload => {
            ApiError::new(ErrorCode::InvalidParams, "the payload is not valid")
        }
        ServiceError::UnknownProtocol(protocol) => ApiError::new(
            ErrorCode::UnknownProtocol,
            format!("unknown protocol '{protocol}'"),
        )
        .with_details(json!({"protocol": protocol})),
        ServiceError::UnknownTransport(transport) => ApiError::new(
            ErrorCode::UnknownTransport,
            format!("unknown transport '{transport}'"),
        )
        .with_details(json!({"transport": transport})),
        ServiceError::UnsupportedTransport {
            protocol,
            transport,
        } => ApiError::new(
            ErrorCode::UnknownTransport,
            format!("transport '{transport}' is not supported for protocol '{protocol}'"),
        )
        .with_details(json!({"protocol": protocol, "transport": transport})),
        ServiceError::Protocol(message) => ApiError::new(ErrorCode::ProtocolError, message),
        ServiceError::Transport(message) => ApiError::new(ErrorCode::TransportError, message),
        ServiceError::InvalidParams(error) => {
            ApiError::new(ErrorCode::InvalidParams, error.to_string())
        }
        ServiceError::DiscoveryNotFound => {
            ApiError::new(ErrorCode::DiscoveryNotFound, "discovery not found")
        }
        ServiceError::DiscoveryBusy => {
            ApiError::new(ErrorCode::DiscoveryBusy, "discovery service is busy")
        }
        ServiceError::NoDiscovery {
            protocol,
            transport,
        } => ApiError::new(
            ErrorCode::InvalidParams,
            format!(
                "discovery is not supported for protocol '{protocol}' over transport '{transport}'"
            ),
        )
        .with_details(json!({"protocol": protocol, "transport": transport})),
    }
}

fn service_config_matches(service: &ServiceConfig, key: &ServiceKey) -> bool {
    service.protocol == key.protocol
        && service.transport == key.transport
        && service.entry == key.entry
}

fn config_validation_error(error: ConfigError) -> ApiError {
    let errors: Vec<String> = match &error {
        ConfigError::Validation(errors) => errors.split("; ").map(str::to_string).collect(),
        _ => Vec::new(),
    };
    let mut api = ApiError::new(ErrorCode::ConfigInvalid, error.to_string());
    if !errors.is_empty() {
        api = api.with_details(json!({ "errors": errors }));
    }
    api
}

fn config_write_error(error: ConfigError) -> ApiError {
    warn!(error = %error, "failed to write configuration");
    ApiError::new(
        ErrorCode::ConfigWriteFailed,
        format!("failed to write configuration: {error}"),
    )
}

fn service_ref_error(error: ServiceError, service: &ServiceRef) -> ApiError {
    let mut error = service_error(error);
    if error.code == ErrorCode::ServiceNotFound {
        error.details = Some(match service {
            ServiceRef::Alias { alias } => json!({"alias": alias}),
            ServiceRef::Key {
                protocol,
                transport,
                entry,
            } => json!({
                "protocol": protocol,
                "transport": transport,
                "entry": entry
            }),
        });
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_service_errors_to_api_codes() {
        assert_eq!(
            service_error(ServiceError::NotFound).code,
            ErrorCode::ServiceNotFound
        );
        assert_eq!(
            service_error(ServiceError::AliasConflict).code,
            ErrorCode::ServiceAliasConflict
        );
        assert_eq!(
            service_error(ServiceError::UnknownProtocol("zwave2".to_string())).code,
            ErrorCode::UnknownProtocol
        );
        assert_eq!(
            service_error(ServiceError::DiscoveryBusy).code,
            ErrorCode::DiscoveryBusy
        );
    }

    #[test]
    fn adds_service_details_to_not_found_errors() {
        let error = service_ref_error(
            ServiceError::NotFound,
            &ServiceRef::Alias {
                alias: "ZW".to_string(),
            },
        );
        assert_eq!(error.details, Some(serde_json::json!({"alias": "ZW"})));
    }
}
