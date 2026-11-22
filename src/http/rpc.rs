use crate::app::AppState;
use crate::automation::engine::{self, FireError};
use crate::config::{
    ConfigError, restart_required, validate, validate_command, validate_schedule, validate_task,
    validate_task_id,
};
use crate::events::Topic;
use crate::messages::MessageFilter;
use crate::services::{ServiceError, catalog};
use hodeco_contract::api::envelope::empty_object;
use hodeco_contract::api::payloads::{
    CommandsAddParams, CommandsAddResult, CommandsGetParams, CommandsGetResult, CommandsListResult,
    CommandsRemoveParams, CommandsUpdateParams, CommandsUpdateResult, ConfigUpdateParams,
    ConfigUpdateResult, DiscoveryCancelParams, DiscoveryGetParams, DiscoveryGetResult,
    DiscoveryStartParams, DiscoveryStartResult, Empty, EventTopic, EventsSubscribeParams,
    EventsUnsubscribeParams, MessagesGetParams, MessagesGetResult, MessagesListParams,
    MessagesListResult, ProtocolInfoParams, ProtocolInfoResult, ProtocolParamsParams,
    ProtocolParamsResult, ProtocolsListResult, SchedulesAddParams, SchedulesAddResult,
    SchedulesGetParams, SchedulesGetResult, SchedulesListResult, SchedulesPreviewParams,
    SchedulesPreviewResult, SchedulesRemoveParams, SchedulesSetDefaultsParams,
    SchedulesSetDefaultsResult, SchedulesSolarPreviewParams, SchedulesSolarPreviewResult,
    SchedulesUpdateParams, SchedulesUpdateResult, ServerInfoResult, ServicesAddParams,
    ServicesAddResult, ServicesAliasParams, ServicesAliasResult, ServicesListResult,
    ServicesParamsParams, ServicesParamsResult, ServicesRemoveParams, ServicesSendParams,
    ServicesSendResult, ServicesStatusParams, ServicesStatusResult, SolarPreviewEvent,
    TasksAddParams, TasksAddResult, TasksCancelRunParams, TasksFireParams, TasksFireResult,
    TasksGetParams, TasksGetResult, TasksGetRunParams, TasksGetRunResult, TasksHistoryParams,
    TasksListResult, TasksPurgeHistoryParams, TasksRemoveParams, TasksRunFeedParams,
    TasksUpdateParams, TasksUpdateResult, TransportsListResult,
};
use hodeco_contract::api::{ApiError, ErrorCode, Method, Response};
use hodeco_contract::config::{SchedulesFile, ServiceConfig};
use hodeco_contract::domain::{
    MessageDirection, MessageState, ParamValues, PayloadExpr, ServiceKey, ServiceRef, SolarEvent,
    TaskDefinition,
};
use hodeco_contract::utils::{generated_id, id_from_name};
use hodeco_schedule::{self as schedule, ScheduleContext};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashSet};
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
            let config = params.config;
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
            let _guard = state.services_store.lock().await;
            let previous = state.services_store.snapshot();
            let service = state
                .registry
                .add(key.clone(), &values, params.alias)
                .map_err(service_error)?;
            state.services_store.update(|services| {
                services.push(ServiceConfig {
                    alias: service.alias.clone(),
                    protocol: key.protocol.clone(),
                    transport: key.transport.clone(),
                    entry: key.entry.clone(),
                    params: values.clone(),
                });
            });
            if let Err(error) = state.services_store.save().await {
                state.services_store.replace(previous);
                if let Err(error) = state.registry.remove(&ServiceRef::from(&key)).await {
                    warn!(error = %error, "failed to roll back service after services write failure");
                }
                return Err(config_write_error(error));
            }
            ok(ServicesAddResult { service })
        }
        Method::ServicesRemove => {
            let params: ServicesRemoveParams = parse_params(method, params)?;
            let _guard = state.services_store.lock().await;
            let key = state
                .registry
                .resolve(&params.service)
                .map_err(|error| service_ref_error(error, &params.service))?;
            let used_by = tasks_using_service(state, &params.service, &key);
            if !used_by.is_empty() {
                return Err(ApiError::new(
                    ErrorCode::InvalidParams,
                    "the service is referenced by tasks",
                )
                .with_details(json!({"usedBy": used_by})));
            }
            let service_info = state.registry.info(&params.service).ok();
            let previous = state.services_store.snapshot();
            state
                .registry
                .remove(&params.service)
                .await
                .map_err(|error| service_ref_error(error, &params.service))?;
            state.services_store.update(|services| {
                services.retain(|service| !service_config_matches(service, &key));
            });
            if let Err(error) = state.services_store.save().await {
                state.services_store.replace(previous);
                if let Some(info) = service_info
                    && let Err(error) = state.registry.add(key, &info.params, info.alias)
                {
                    warn!(error = %error, "failed to roll back service removal after services write failure");
                }
                return Err(config_write_error(error));
            }
            ok(Empty::default())
        }
        Method::ServicesAlias => {
            let params: ServicesAliasParams = parse_params(method, params)?;
            let _guard = state.services_store.lock().await;
            let key = state
                .registry
                .resolve(&params.service)
                .map_err(|error| service_ref_error(error, &params.service))?;
            let old_alias = state
                .registry
                .info(&params.service)
                .ok()
                .and_then(|info| info.alias);
            let used_by_old_alias = old_alias.as_ref().map(|old_alias| {
                tasks_using_service(
                    state,
                    &ServiceRef::Alias {
                        alias: old_alias.clone(),
                    },
                    &key,
                )
            });
            if let Some(old_alias) = &old_alias
                && params.alias.as_deref() != Some(old_alias.as_str())
                && used_by_old_alias
                    .as_ref()
                    .is_some_and(|used_by| !used_by.is_empty())
            {
                return Err(ApiError::new(
                    ErrorCode::InvalidParams,
                    "the service alias is referenced by tasks",
                )
                .with_details(json!({"usedBy": used_by_old_alias.unwrap_or_default()})));
            }
            let previous = state.services_store.snapshot();
            let service = state
                .registry
                .alias(&params.service, params.alias)
                .map_err(|error| service_ref_error(error, &params.service))?;
            let new_alias = service.alias.clone();
            state.services_store.update(|services| {
                if let Some(entry) = services
                    .iter_mut()
                    .find(|entry| service_config_matches(entry, &key))
                {
                    entry.alias = new_alias;
                }
            });
            if let Err(error) = state.services_store.save().await {
                state.services_store.replace(previous);
                if let Err(error) = state.registry.alias(&ServiceRef::from(&key), old_alias) {
                    warn!(error = %error, "failed to roll back service alias after services write failure");
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
            let _guard = state.services_store.lock().await;
            let key = state
                .registry
                .resolve(&params.service)
                .map_err(|error| service_ref_error(error, &params.service))?;
            let previous_info = state.registry.info(&params.service).ok();
            let previous = state.services_store.snapshot();
            let service = state
                .registry
                .update_params(&params.service, &values)
                .await
                .map_err(|error| service_ref_error(error, &params.service))?;
            state.services_store.update(|services| {
                if let Some(entry) = services
                    .iter_mut()
                    .find(|entry| service_config_matches(entry, &key))
                {
                    entry.params = values.clone();
                }
            });
            if let Err(error) = state.services_store.save().await {
                state.services_store.replace(previous);
                if let Some(info) = previous_info
                    && let Err(error) = state
                        .registry
                        .update_params(&params.service, &info.params)
                        .await
                {
                    warn!(error = %error, "failed to roll back service parameters after services write failure");
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
            let store = state.store.clone();
            let page = tokio::task::spawn_blocking(move || {
                store.list(params.from, params.until, params.order, limit, &filter)
            })
            .await
            .map_err(|error| {
                ApiError::new(
                    ErrorCode::InternalError,
                    format!("message query task failed: {error}"),
                )
            })?;
            ok(MessagesListResult {
                messages: page.messages,
                next: page.next,
                store_total: page.store_total,
            })
        }
        Method::TasksList => {
            let snapshot = state.automation.snapshot();
            let tasks = snapshot
                .tasks
                .values()
                .map(|task| engine::task_info(state, task))
                .collect();
            ok(TasksListResult { tasks })
        }
        Method::TasksGet => {
            let params: TasksGetParams = parse_params(method, params)?;
            let snapshot = state.automation.snapshot();
            let Some(task) = snapshot.tasks.get(&params.task_id) else {
                return Err(task_not_found(&params.task_id));
            };
            ok(TasksGetResult {
                task: engine::task_info(state, task),
            })
        }
        Method::TasksGetRun => {
            let params: TasksGetRunParams = parse_params(method, params)?;
            let run = match state.run_feed.get(&params.task_id, params.run_id) {
                Some(run) => Some(run),
                None => state
                    .history
                    .get(&params.task_id, params.run_id)
                    .await
                    .map_err(history_error)?,
            }
            .ok_or_else(|| {
                ApiError::new(ErrorCode::RunNotFound, "run not found").with_details(json!({
                    "taskId": params.task_id,
                    "runId": params.run_id,
                }))
            })?;
            ok(TasksGetRunResult { run })
        }
        Method::TasksAdd => {
            let params: TasksAddParams = parse_params(method, params)?;
            let _guard = state.automation.lock().await;
            let snapshot = state.automation.snapshot();
            let mut task = params.task;
            if task.id.is_empty() {
                task.id = unique_id("task", &task.name, snapshot.tasks.keys());
            }
            validate_task_id(&task.id).map_err(validation_error)?;
            if snapshot.tasks.contains_key(&task.id) {
                return Err(ApiError::new(
                    ErrorCode::InvalidParams,
                    format!("task '{}' already exists", task.id),
                ));
            }
            let services = service_refs(state);
            let others: Vec<&TaskDefinition> = snapshot.tasks.values().collect();
            validate_task(
                &task,
                &others,
                &snapshot.schedule_ids(),
                &snapshot.command_ids(),
                &services,
            )
            .map_err(validation_error)?;
            state.automation.update_tasks(|tasks| {
                tasks.insert(task.id.clone(), task.clone());
            });
            if let Err(error) = state.automation.save_tasks().await {
                state.automation.replace_snapshot(snapshot);
                return Err(catalog_write_error(error));
            }
            state.runtime.wake_scheduler();
            ok(TasksAddResult {
                task: engine::task_info(state, &task),
            })
        }
        Method::TasksUpdate => {
            let params: TasksUpdateParams = parse_params(method, params)?;
            let _guard = state.automation.lock().await;
            let snapshot = state.automation.snapshot();
            let Some(previous) = snapshot.tasks.get(&params.task_id).cloned() else {
                return Err(task_not_found(&params.task_id));
            };
            let mut task = params.task;
            if !task.id.is_empty() && task.id != params.task_id {
                return Err(ApiError::new(
                    ErrorCode::InvalidParams,
                    "a task ID cannot be changed",
                ));
            }
            task.id = params.task_id.clone();
            let services = service_refs(state);
            let all_tasks: Vec<&TaskDefinition> = snapshot.tasks.values().collect();
            validate_task(
                &task,
                &all_tasks,
                &snapshot.schedule_ids(),
                &snapshot.command_ids(),
                &services,
            )
            .map_err(validation_error)?;
            let was_enabled = previous.enabled;
            state.automation.update_tasks(|tasks| {
                tasks.insert(task.id.clone(), task.clone());
            });
            if let Err(error) = state.automation.save_tasks().await {
                state.automation.replace_snapshot(snapshot);
                return Err(catalog_write_error(error));
            }
            if !was_enabled && task.enabled {
                state.runtime.reset_suppressed(&task.id);
                state.runtime.reset_rate(&task.id);
                state.runtime.reset_trigger_runs(&task.id);
            }
            state.runtime.wake_scheduler();
            ok(TasksUpdateResult {
                task: engine::task_info(state, &task),
            })
        }
        Method::TasksRemove => {
            let params: TasksRemoveParams = parse_params(method, params)?;
            let _guard = state.automation.lock().await;
            let snapshot = state.automation.snapshot();
            if !snapshot.tasks.contains_key(&params.task_id) {
                return Err(task_not_found(&params.task_id));
            }
            if state.runtime.running(&params.task_id) {
                return Err(ApiError::new(
                    ErrorCode::TaskBusy,
                    "the task is running; wait for it to finish before deleting it",
                ));
            }
            state.automation.update_tasks(|tasks| {
                tasks.remove(&params.task_id);
            });
            if let Err(error) = state.automation.save_tasks().await {
                state.automation.replace_snapshot(snapshot);
                return Err(catalog_write_error(error));
            }
            state.runtime.forget_task(&params.task_id);
            if let Err(error) = state.history.remove_task(&params.task_id).await {
                warn!(error = %error, task = %params.task_id, "unable to remove task history");
            }
            let dropped = state.run_feed.remove_task(&params.task_id);
            if !dropped.is_empty() {
                let _ = state
                    .events
                    .send(crate::events::Event::TaskRunDropped { run_ids: dropped });
            }
            state.runtime.wake_scheduler();
            ok(Empty::default())
        }
        Method::TasksFire => {
            let params: TasksFireParams = parse_params(method, params)?;
            let run_id = engine::fire_manual(
                state,
                &params.task_id,
                params.trigger_id,
                params.payload,
                params.metadata,
            )
            .map_err(fire_error)?;
            ok(TasksFireResult { run_id })
        }
        Method::TasksCancelRun => {
            let params: TasksCancelRunParams = parse_params(method, params)?;
            if !state.runtime.cancel_run(&params.task_id, params.run_id) {
                return Err(ApiError::new(ErrorCode::RunNotFound, "run is not active")
                    .with_details(json!({
                        "taskId": params.task_id,
                        "runId": params.run_id,
                    })));
            }
            ok(Empty::default())
        }
        Method::TasksHistory => {
            let params: TasksHistoryParams = parse_params(method, params)?;
            ok(state.history.list(&params).await.map_err(history_error)?)
        }
        Method::TasksRunFeed => {
            let params: TasksRunFeedParams = parse_params(method, params)?;
            ok(state.run_feed.list(&params))
        }
        Method::TasksPurgeHistory => {
            let params: TasksPurgeHistoryParams = parse_params(method, params)?;
            let _guard = state.automation.lock().await;
            if !state
                .automation
                .snapshot()
                .tasks
                .contains_key(&params.task_id)
            {
                return Err(task_not_found(&params.task_id));
            }
            if state.runtime.running(&params.task_id) {
                return Err(ApiError::new(
                    ErrorCode::TaskBusy,
                    "the task is running; wait for it to finish before purging history",
                ));
            }
            state
                .history
                .purge(&params.task_id)
                .await
                .map_err(history_error)?;
            let dropped = state.run_feed.remove_task(&params.task_id);
            if !dropped.is_empty() {
                let _ = state
                    .events
                    .send(crate::events::Event::TaskRunDropped { run_ids: dropped });
            }
            state.runtime.reset_rate(&params.task_id);
            state.runtime.reset_trigger_runs(&params.task_id);
            state.runtime.clear_last_run(&params.task_id);
            state.runtime.wake_scheduler();
            ok(Empty::default())
        }
        Method::SchedulesList => {
            let snapshot = state.automation.snapshot();
            let context = schedule_context(&snapshot);
            let schedules = snapshot
                .schedules
                .values()
                .map(|schedule| {
                    let next =
                        schedule::next_occurrence(schedule, context, chrono::Utc::now(), true)
                            .ok()
                            .flatten();
                    snapshot.schedule_info(schedule, next)
                })
                .collect();
            ok(SchedulesListResult {
                schedules,
                location: snapshot.location,
                timezone: snapshot.timezone.clone(),
                system_timezone: state.system_timezone.clone(),
            })
        }
        Method::SchedulesGet => {
            let params: SchedulesGetParams = parse_params(method, params)?;
            let snapshot = state.automation.snapshot();
            let Some(schedule) = snapshot.schedules.get(&params.schedule_id) else {
                return Err(schedule_not_found(&params.schedule_id));
            };
            let context = schedule_context(&snapshot);
            let next = schedule::next_occurrence(schedule, context, chrono::Utc::now(), true)
                .ok()
                .flatten();
            ok(SchedulesGetResult {
                schedule: snapshot.schedule_info(schedule, next),
            })
        }
        Method::SchedulesAdd => {
            let params: SchedulesAddParams = parse_params(method, params)?;
            let _guard = state.automation.lock().await;
            let snapshot = state.automation.snapshot();
            let mut schedule = params.schedule;
            if schedule.id.is_empty() {
                schedule.id = unique_id("schedule", &schedule.name, snapshot.schedules.keys());
            }
            validate_task_id(&schedule.id).map_err(validation_error)?;
            if snapshot.schedules.contains_key(&schedule.id) {
                return Err(ApiError::new(
                    ErrorCode::InvalidParams,
                    format!("schedule '{}' already exists", schedule.id),
                ));
            }
            let defaults = schedule_defaults(&snapshot);
            validate_schedule(&schedule, &defaults).map_err(validation_error)?;
            state.automation.update_schedules(|schedules| {
                schedules.insert(schedule.id.clone(), schedule.clone());
            });
            if let Err(error) = state.automation.save_schedules().await {
                state.automation.replace_snapshot(snapshot);
                return Err(catalog_write_error(error));
            }
            state.runtime.wake_scheduler();
            let next = schedule::next_occurrence(
                &schedule,
                schedule_context(&state.automation.snapshot()),
                chrono::Utc::now(),
                true,
            )
            .ok()
            .flatten();
            let snapshot = state.automation.snapshot();
            ok(SchedulesAddResult {
                schedule: snapshot.schedule_info(&schedule, next),
            })
        }
        Method::SchedulesUpdate => {
            let params: SchedulesUpdateParams = parse_params(method, params)?;
            let _guard = state.automation.lock().await;
            let snapshot = state.automation.snapshot();
            if !snapshot.schedules.contains_key(&params.schedule_id) {
                return Err(schedule_not_found(&params.schedule_id));
            }
            let mut schedule = params.schedule;
            if !schedule.id.is_empty() && schedule.id != params.schedule_id {
                return Err(ApiError::new(
                    ErrorCode::InvalidParams,
                    "a schedule ID cannot be changed",
                ));
            }
            schedule.id = params.schedule_id.clone();
            let defaults = schedule_defaults(&snapshot);
            validate_schedule(&schedule, &defaults).map_err(validation_error)?;
            state.automation.update_schedules(|schedules| {
                schedules.insert(schedule.id.clone(), schedule.clone());
            });
            if let Err(error) = state.automation.save_schedules().await {
                state.automation.replace_snapshot(snapshot);
                return Err(catalog_write_error(error));
            }
            state.runtime.wake_scheduler();
            let snapshot = state.automation.snapshot();
            let next = schedule::next_occurrence(
                &schedule,
                schedule_context(&snapshot),
                chrono::Utc::now(),
                true,
            )
            .ok()
            .flatten();
            ok(SchedulesUpdateResult {
                schedule: snapshot.schedule_info(&schedule, next),
            })
        }
        Method::SchedulesRemove => {
            let params: SchedulesRemoveParams = parse_params(method, params)?;
            let _guard = state.automation.lock().await;
            let snapshot = state.automation.snapshot();
            if !snapshot.schedules.contains_key(&params.schedule_id) {
                return Err(schedule_not_found(&params.schedule_id));
            }
            let used_by = snapshot.used_by_schedule(&params.schedule_id);
            if !used_by.is_empty() {
                return Err(ApiError::new(
                    ErrorCode::ScheduleInUse,
                    format!("schedule '{}' is used by tasks", params.schedule_id),
                )
                .with_details(json!({
                    "scheduleId": params.schedule_id,
                    "usedBy": used_by,
                })));
            }
            state.automation.update_schedules(|schedules| {
                schedules.remove(&params.schedule_id);
            });
            if let Err(error) = state.automation.save_schedules().await {
                state.automation.replace_snapshot(snapshot);
                return Err(catalog_write_error(error));
            }
            state.runtime.wake_scheduler();
            ok(Empty::default())
        }
        Method::SchedulesPreview => {
            let params: SchedulesPreviewParams = parse_params(method, params)?;
            let snapshot = state.automation.snapshot();
            let defaults = schedule_defaults(&snapshot);
            validate_schedule(&params.schedule, &defaults).map_err(validation_error)?;
            let limit = params.limit.unwrap_or(20).clamp(1, 100) as usize;
            let occurrences = schedule::preview(
                &params.schedule,
                schedule_context(&snapshot),
                chrono::Utc::now(),
                limit,
                true,
            )
            .map_err(validation_error)?;
            ok(SchedulesPreviewResult { occurrences })
        }
        Method::SchedulesSetDefaults => {
            let params: SchedulesSetDefaultsParams = parse_params(method, params)?;
            let _guard = state.automation.lock().await;
            let snapshot = state.automation.snapshot();
            let timezone = params
                .timezone
                .map(|timezone| timezone.trim().to_string())
                .filter(|timezone| !timezone.is_empty());
            if let Some(timezone) = &timezone
                && timezone.parse::<chrono_tz::Tz>().is_err()
            {
                return Err(validation_error(format!(
                    "timezone '{timezone}' is not an IANA timezone"
                )));
            }
            if let Some(location) = params.location
                && !location.is_valid()
            {
                return Err(validation_error(
                    "location must be latitude, longitude, and optional elevation in range"
                        .to_string(),
                ));
            }
            let defaults = SchedulesFile {
                location: params.location,
                timezone: timezone.clone(),
                schedules: Vec::new(),
            };
            for schedule in snapshot.schedules.values() {
                validate_schedule(schedule, &defaults).map_err(validation_error)?;
            }
            state
                .automation
                .set_schedule_defaults(params.location, timezone.clone());
            if let Err(error) = state.automation.save_schedules().await {
                state.automation.replace_snapshot(snapshot);
                return Err(catalog_write_error(error));
            }
            state.runtime.wake_scheduler();
            ok(SchedulesSetDefaultsResult {
                location: params.location,
                timezone,
            })
        }
        Method::SchedulesSolarPreview => {
            let params: SchedulesSolarPreviewParams = parse_params(method, params)?;
            if !params.location.is_valid() {
                return Err(validation_error(
                    "location must be latitude, longitude, and optional elevation in range"
                        .to_string(),
                ));
            }
            let snapshot = state.automation.snapshot();
            let timezone = match params
                .timezone
                .as_deref()
                .map(str::trim)
                .filter(|timezone| !timezone.is_empty())
            {
                Some(timezone) => timezone.parse::<chrono_tz::Tz>().map_err(|_| {
                    validation_error(format!("timezone '{timezone}' is not an IANA timezone"))
                })?,
                None => schedule_context(&snapshot)
                    .default_timezone
                    .map(Ok)
                    .unwrap_or_else(schedule::system_timezone)
                    .map_err(validation_error)?,
            };
            let mut events = Vec::with_capacity(SolarEvent::ALL.len());
            for event in SolarEvent::ALL {
                let at = schedule::solar_event(params.date, event, params.location, timezone, 0)
                    .map_err(validation_error)?;
                events.push(match at {
                    Some(at) => SolarPreviewEvent {
                        event,
                        at: Some(at),
                        local: Some(at.with_timezone(&timezone).format("%H:%M:%S").to_string()),
                        reason: None,
                    },
                    None => SolarPreviewEvent {
                        event,
                        at: None,
                        local: None,
                        reason: Some(
                            "The sun does not cross this altitude on the selected date."
                                .to_string(),
                        ),
                    },
                });
            }
            ok(SchedulesSolarPreviewResult {
                date: params.date,
                timezone: timezone.name().to_string(),
                events,
            })
        }
        Method::CommandsList => {
            let snapshot = state.automation.snapshot();
            let commands = snapshot
                .commands
                .values()
                .map(|command| snapshot.command_info(command))
                .collect();
            ok(CommandsListResult { commands })
        }
        Method::CommandsGet => {
            let params: CommandsGetParams = parse_params(method, params)?;
            let snapshot = state.automation.snapshot();
            let Some(command) = snapshot.commands.get(&params.command_id) else {
                return Err(command_not_found(&params.command_id));
            };
            ok(CommandsGetResult {
                command: snapshot.command_info(command),
            })
        }
        Method::CommandsAdd => {
            let params: CommandsAddParams = parse_params(method, params)?;
            let _guard = state.automation.lock().await;
            let snapshot = state.automation.snapshot();
            let mut command = params.command;
            if command.id.is_empty() {
                command.id = unique_id("command", &command.name, snapshot.commands.keys());
            }
            validate_task_id(&command.id).map_err(validation_error)?;
            if snapshot.commands.contains_key(&command.id) {
                return Err(ApiError::new(
                    ErrorCode::InvalidParams,
                    format!("command '{}' already exists", command.id),
                ));
            }
            validate_command(&command).map_err(validation_error)?;
            state.automation.update_commands(|commands| {
                commands.insert(command.id.clone(), command.clone());
            });
            if let Err(error) = state.automation.save_commands().await {
                state.automation.replace_snapshot(snapshot);
                return Err(catalog_write_error(error));
            }
            let snapshot = state.automation.snapshot();
            ok(CommandsAddResult {
                command: snapshot.command_info(&command),
            })
        }
        Method::CommandsUpdate => {
            let params: CommandsUpdateParams = parse_params(method, params)?;
            let _guard = state.automation.lock().await;
            let snapshot = state.automation.snapshot();
            if !snapshot.commands.contains_key(&params.command_id) {
                return Err(command_not_found(&params.command_id));
            }
            let mut command = params.command;
            if !command.id.is_empty() && command.id != params.command_id {
                return Err(ApiError::new(
                    ErrorCode::InvalidParams,
                    "a command ID cannot be changed",
                ));
            }
            command.id = params.command_id.clone();
            validate_command(&command).map_err(validation_error)?;
            state.automation.update_commands(|commands| {
                commands.insert(command.id.clone(), command.clone());
            });
            if let Err(error) = state.automation.save_commands().await {
                state.automation.replace_snapshot(snapshot);
                return Err(catalog_write_error(error));
            }
            let snapshot = state.automation.snapshot();
            ok(CommandsUpdateResult {
                command: snapshot.command_info(&command),
            })
        }
        Method::CommandsRemove => {
            let params: CommandsRemoveParams = parse_params(method, params)?;
            let _guard = state.automation.lock().await;
            let snapshot = state.automation.snapshot();
            if !snapshot.commands.contains_key(&params.command_id) {
                return Err(command_not_found(&params.command_id));
            }
            let used_by = snapshot.used_by_command(&params.command_id);
            if !used_by.is_empty() {
                return Err(ApiError::new(
                    ErrorCode::CommandInUse,
                    format!("command '{}' is used by tasks", params.command_id),
                )
                .with_details(json!({
                    "commandId": params.command_id,
                    "usedBy": used_by,
                })));
            }
            state.automation.update_commands(|commands| {
                commands.remove(&params.command_id);
            });
            if let Err(error) = state.automation.save_commands().await {
                state.automation.replace_snapshot(snapshot);
                return Err(catalog_write_error(error));
            }
            ok(Empty::default())
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
        EventTopic::Tasks => Topic::Tasks,
    }
}

fn tasks_using_service(state: &AppState, reference: &ServiceRef, key: &ServiceKey) -> Vec<String> {
    let snapshot = state.automation.snapshot();
    snapshot
        .tasks
        .values()
        .filter(|task| {
            task.triggers.iter().any(|trigger| match trigger {
                hodeco_contract::domain::TaskTrigger::Message(trigger) => trigger
                    .services
                    .iter()
                    .any(|service| service_reference_matches(service, reference, key)),
                _ => false,
            }) || task.actions.iter().any(|action| match &action.kind {
                hodeco_contract::domain::ActionKind::Message(action) => {
                    service_reference_matches(&action.message.service, reference, key)
                }
                hodeco_contract::domain::ActionKind::Command(_) => false,
            })
        })
        .map(|task| task.id.clone())
        .collect()
}

fn service_reference_matches(
    service: &ServiceRef,
    reference: &ServiceRef,
    key: &ServiceKey,
) -> bool {
    match service {
        ServiceRef::Alias { alias } => match reference {
            ServiceRef::Alias { alias: reference } => alias == reference,
            ServiceRef::Key {
                protocol,
                transport,
                entry,
            } => protocol == &key.protocol && transport == &key.transport && entry == &key.entry,
        },
        ServiceRef::Key {
            protocol,
            transport,
            entry,
        } => protocol == &key.protocol && transport == &key.transport && entry == &key.entry,
    }
}

fn service_refs(state: &AppState) -> BTreeSet<ServiceRef> {
    let mut refs = BTreeSet::new();
    for service in state.services_store.snapshot() {
        refs.insert(ServiceRef::Key {
            protocol: service.protocol,
            transport: service.transport,
            entry: service.entry,
        });
        if let Some(alias) = service.alias {
            refs.insert(ServiceRef::Alias { alias });
        }
    }
    refs
}

fn schedule_defaults(snapshot: &crate::automation::AutomationSnapshot) -> SchedulesFile {
    SchedulesFile {
        location: snapshot.location,
        timezone: snapshot.timezone.clone(),
        schedules: Vec::new(),
    }
}

fn schedule_context(snapshot: &crate::automation::AutomationSnapshot) -> ScheduleContext {
    ScheduleContext {
        default_location: snapshot.location,
        default_timezone: snapshot
            .timezone
            .as_deref()
            .and_then(|timezone| timezone.parse().ok()),
    }
}

fn unique_id<'a>(prefix: &str, name: &str, existing: impl Iterator<Item = &'a String>) -> String {
    let existing: BTreeSet<&str> = existing.map(String::as_str).collect();
    let base = id_from_name(prefix, name);
    if !existing.contains(base.as_str()) {
        return base;
    }
    loop {
        let candidate = generated_id(prefix);
        if !existing.contains(candidate.as_str()) {
            return candidate;
        }
    }
}

fn validation_error(message: String) -> ApiError {
    let errors: Vec<String> = message.split("; ").map(str::to_string).collect();
    ApiError::new(ErrorCode::ConfigInvalid, message.clone()).with_details(json!({"errors": errors}))
}

fn task_not_found(task_id: &str) -> ApiError {
    ApiError::new(
        ErrorCode::TaskNotFound,
        format!("task '{task_id}' not found"),
    )
    .with_details(json!({"taskId": task_id}))
}

fn schedule_not_found(schedule_id: &str) -> ApiError {
    ApiError::new(
        ErrorCode::ScheduleNotFound,
        format!("schedule '{schedule_id}' not found"),
    )
    .with_details(json!({"scheduleId": schedule_id}))
}

fn command_not_found(command_id: &str) -> ApiError {
    ApiError::new(
        ErrorCode::CommandNotFound,
        format!("command '{command_id}' not found"),
    )
    .with_details(json!({"commandId": command_id}))
}

fn fire_error(error: FireError) -> ApiError {
    let code = match error {
        FireError::TaskNotFound => ErrorCode::TaskNotFound,
        FireError::TaskDisabled => ErrorCode::TaskDisabled,
        FireError::TaskBusy => ErrorCode::TaskBusy,
        FireError::TaskLimitExceeded => ErrorCode::TaskLimitExceeded,
        FireError::TriggerNotFound => ErrorCode::TriggerNotFound,
        FireError::PayloadTooLarge => ErrorCode::PayloadTooLarge,
    };
    ApiError::new(code, error.reason())
}

fn history_error(error: crate::automation::history::HistoryError) -> ApiError {
    warn!(error = %error, "task history operation failed");
    ApiError::new(ErrorCode::InternalError, "task history operation failed")
}

fn catalog_write_error(error: ConfigError) -> ApiError {
    warn!(error = %error, "failed to write an automation catalog");
    ApiError::new(
        ErrorCode::ConfigWriteFailed,
        format!("failed to write the catalog: {error}"),
    )
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
