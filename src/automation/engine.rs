//! Task trigger admission, action execution, next rules, and run recording.

use crate::app::AppState;
use crate::automation::command::{self, CaptureBudget};
use crate::automation::runtime::AdmitError;
use crate::messages::StoreEvent;
use crate::services::ServiceError;
use chrono::{DateTime, Utc};
use hodeco_contract::api::notifications::TaskRunFinishedNotification;
use hodeco_contract::api::payloads::TaskRunNotification;
use hodeco_contract::domain::{
    ActionKind, ActionOutcome, MessageDirection, MessagePayload, MessageState, MessageTrigger,
    NextCondition, NextTaskRecord, RunAction, RunChain, RunCommandAction, RunError,
    RunMessageAction, RunOutcome, RunStage, RunSummary, RunTrigger, SelectorOutcome,
    SelectorResult, TaskAction, TaskDefinition, TaskInfo, TaskRun, TaskRunTrigger, TriggerRef,
};
use hodeco_contract::utils::{Base64Bytes, HumanDuration};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireError {
    TaskNotFound,
    TaskDisabled,
    TaskBusy,
    TaskLimitExceeded,
    TriggerNotFound,
    PayloadTooLarge,
}

impl FireError {
    pub const fn reason(self) -> &'static str {
        match self {
            Self::TaskNotFound => "taskNotFound",
            Self::TaskDisabled => "taskDisabled",
            Self::TaskBusy => "taskBusy",
            Self::TaskLimitExceeded => "taskLimitExceeded",
            Self::TriggerNotFound => "triggerNotFound",
            Self::PayloadTooLarge => "payloadTooLarge",
        }
    }
}

/// Fires a task from an external API call.
pub async fn fire_manual(
    state: &AppState,
    task_id: &str,
    trigger_id: Option<String>,
    payload: Option<Base64Bytes>,
    metadata: BTreeMap<String, Value>,
) -> Result<Uuid, FireError> {
    let task = state
        .automation
        .task(task_id)
        .ok_or(FireError::TaskNotFound)?;
    let trigger_id = trigger_id.unwrap_or_else(|| "manual".to_string());
    if trigger_id != "manual" && task.trigger(&trigger_id).is_none() {
        return Err(FireError::TriggerNotFound);
    }
    let trigger = RunTrigger::Manual(hodeco_contract::domain::ManualRunTrigger {
        id: trigger_id,
        payload,
        metadata,
    });
    fire(state, &task, trigger, root_chain(), true).await
}

fn root_chain() -> RunChain {
    RunChain {
        chain_id: Uuid::new_v4(),
        parent_run_id: None,
        depth: 0,
    }
}

/// Fires a task with a typed trigger. Internal callers use `external = false`
/// so disabled, busy, rate-limited, and payload errors become suppression
/// counters instead of API errors.
pub async fn fire(
    state: &AppState,
    task: &TaskDefinition,
    trigger: RunTrigger,
    chain: RunChain,
    external: bool,
) -> Result<Uuid, FireError> {
    start(state, task, trigger, chain, external)
}

fn start(
    state: &AppState,
    task: &TaskDefinition,
    trigger: RunTrigger,
    chain: RunChain,
    external: bool,
) -> Result<Uuid, FireError> {
    if !task.enabled {
        return Err(FireError::TaskDisabled);
    }
    let payload_length = trigger
        .payload()
        .map_or(0, |payload| payload.as_slice().len());
    if payload_length > state.config.task_engine.max_trigger_payload_size.as_bytes() as usize {
        return Err(FireError::PayloadTooLarge);
    }

    let now = Utc::now();
    let guard = match state
        .runtime
        .try_start(&task.id, task.max_runs_per_minute, now)
    {
        Ok(guard) => guard,
        Err(AdmitError::Busy) => {
            state.runtime.suppress(&task.id, "taskBusy");
            return Err(FireError::TaskBusy);
        }
        Err(AdmitError::RateLimited) => {
            state.runtime.suppress(&task.id, "rateLimited");
            return Err(FireError::TaskLimitExceeded);
        }
        Err(AdmitError::Limit) => {
            state.runtime.suppress(&task.id, "taskLimitExceeded");
            return Err(FireError::TaskLimitExceeded);
        }
    };

    let run_id = Uuid::new_v4();
    let started_at = Utc::now();
    state.runtime.note_trigger_run(&task.id, trigger.id());
    let trigger_ref = TriggerRef {
        id: trigger.id().to_string(),
        kind: trigger.kind(),
    };
    let _ = state
        .events
        .send(crate::events::Event::TaskRunStarted(TaskRunNotification {
            run_id,
            task_id: task.id.clone(),
            trigger: trigger_ref,
            started_at,
        }));

    let state = state.clone();
    let task = task.clone();
    tokio::spawn(run_task_boxed(
        state, task, trigger, run_id, started_at, chain, guard,
    ));

    let _ = external;
    Ok(run_id)
}

#[allow(clippy::too_many_arguments)]
fn run_task_boxed(
    state: AppState,
    task: TaskDefinition,
    trigger: RunTrigger,
    run_id: Uuid,
    started_at: DateTime<Utc>,
    chain: RunChain,
    guard: crate::automation::runtime::RunGuard,
) -> futures_util::future::BoxFuture<'static, ()> {
    Box::pin(run_task(
        state, task, trigger, run_id, started_at, chain, guard,
    ))
}

async fn run_task(
    state: AppState,
    task: TaskDefinition,
    trigger: RunTrigger,
    run_id: Uuid,
    started_at: DateTime<Utc>,
    chain: RunChain,
    guard: crate::automation::runtime::RunGuard,
) {
    let mut cancellation = CancellationToken::new();
    let timeout = task.run_timeout.as_std();
    let timer_cancel = cancellation.clone();
    let timer = if timeout.is_zero() {
        None
    } else {
        Some(tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            timer_cancel.cancel();
        }))
    };

    let execution = execute(
        &state,
        &task,
        &trigger,
        run_id,
        started_at,
        &chain,
        &mut cancellation,
    )
    .await;
    if let Some(timer) = timer {
        timer.abort();
    }

    let finished_at = Utc::now();
    let time = state.runtime.next_history_time(finished_at);
    let error = execution
        .error
        .as_ref()
        .map(|error| error.message.clone())
        .or_else(|| {
            execution
                .actions
                .iter()
                .find(|action| action.status == ActionOutcome::Failed)
                .and_then(|action| action.error.clone())
        });
    let summary = RunSummary {
        id: run_id,
        task_id: task.id.clone(),
        time,
        started_at,
        finished_at,
        trigger: TriggerRef {
            id: trigger.id().to_string(),
            kind: trigger.kind(),
        },
        outcome: execution.outcome,
        action_count: execution.actions.len() as u32,
        next_count: execution.next_tasks.len() as u32,
        error,
    };
    let run = TaskRun {
        summary: summary.clone(),
        trigger,
        actions: execution.actions,
        selector: execution.selector,
        next_tasks: execution.next_tasks,
        chain,
        error: execution.error,
    };

    if task.history.max_rows > 0
        && let Err(error) = state.history.append_async(run).await
    {
        warn!(error = %error, task = %task.id, "unable to append task history");
    }
    state.runtime.set_last_run(&task.id, summary.clone());
    let _ = state.events.send(crate::events::Event::TaskRunFinished(
        TaskRunFinishedNotification { run: summary },
    ));
    drop(guard);
}

struct Execution {
    actions: Vec<RunAction>,
    selector: Option<SelectorResult>,
    next_tasks: Vec<NextTaskRecord>,
    outcome: RunOutcome,
    error: Option<RunError>,
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    state: &AppState,
    task: &TaskDefinition,
    trigger: &RunTrigger,
    run_id: Uuid,
    started_at: DateTime<Utc>,
    chain: &RunChain,
    cancellation: &mut CancellationToken,
) -> Execution {
    let snapshot = state.automation.snapshot();
    let mut budget = CaptureBudget::new();
    let mut actions = Vec::new();
    let mut first_error: Option<RunError> = None;

    for action in &task.actions {
        if cancellation.is_cancelled() {
            break;
        }
        let result = execute_action(
            state,
            &snapshot.commands,
            task,
            action,
            trigger,
            run_id,
            started_at,
            chain,
            &mut budget,
            cancellation.clone(),
        )
        .await;
        let failed = result.status == ActionOutcome::Failed;
        if failed && first_error.is_none() {
            first_error = Some(RunError {
                stage: RunStage::Actions,
                message: result
                    .error
                    .clone()
                    .unwrap_or_else(|| format!("action '{}' failed", action.id)),
            });
        }
        actions.push(result);
        if failed && task.fail_fast {
            break;
        }
    }

    let cancelled = cancellation.is_cancelled();
    let mut selector = None;
    let mut selector_targets: Vec<(String, NextCondition, Option<u8>)> = Vec::new();
    let mut next_tasks = Vec::new();

    if cancelled {
        return Execution {
            actions,
            selector: None,
            next_tasks,
            outcome: RunOutcome::Cancelled,
            error: Some(RunError {
                stage: RunStage::Actions,
                message: format!("run timed out after {:?}", task.run_timeout.as_std()),
            }),
        };
    }

    if let Some(rule) = task
        .next
        .iter()
        .find(|rule| rule.when == NextCondition::CommandResult)
    {
        let result = run_selector(
            &snapshot.commands,
            task,
            rule,
            trigger,
            run_id,
            started_at,
            chain,
            &actions,
            &mut budget,
            cancellation.clone(),
        )
        .await;
        if result.outcome == SelectorOutcome::Error {
            first_error.get_or_insert_with(|| RunError {
                stage: RunStage::Selector,
                message: result
                    .error
                    .clone()
                    .unwrap_or_else(|| "selector command failed".to_string()),
            });
        }
        if let Some(code) = result.exit_code
            && (0..=254).contains(&code)
            && result.outcome == SelectorOutcome::Matched
        {
            for target in rule.case_targets(code as u8) {
                selector_targets.push((
                    target.to_string(),
                    NextCondition::CommandResult,
                    Some(code as u8),
                ));
            }
        }
        selector = Some(result);
    }

    // Evaluate each non-command-result rule independently and deduplicate
    // targets across all matching rules.
    let mut seen = BTreeSet::new();
    let mut targets: Vec<(String, NextCondition, Option<u8>)> = Vec::new();
    for (target, rule, code) in selector_targets {
        if seen.insert(target.clone()) {
            targets.push((target, rule, code));
        }
    }
    for rule in &task.next {
        if rule.when == NextCondition::CommandResult {
            continue;
        }
        if !condition_matches(rule.when, &actions, first_error.is_some()) {
            continue;
        }
        for target in rule.tasks.iter() {
            if seen.insert(target.clone()) {
                targets.push((target.clone(), rule.when, None));
            }
        }
    }

    let max_fan_out = state.config.task_engine.max_fan_out as usize;
    let mut started_children = 0;
    for (target, rule, selector_exit_code) in targets {
        if started_children >= max_fan_out {
            debug!(task = %task.id, "task fan-out limit reached");
            break;
        }
        if chain.depth + 1 > state.config.task_engine.max_chain_depth {
            warn!(
                task = %task.id,
                target = %target,
                depth = chain.depth + 1,
                "chain depth limit reached"
            );
            continue;
        }
        let Some(target_task) = snapshot.tasks.get(&target) else {
            warn!(task = %task.id, target = %target, "next task no longer exists");
            continue;
        };
        if !target_task.enabled {
            debug!(task = %task.id, target = %target, "next task is disabled");
            continue;
        }
        let child_trigger = RunTrigger::Task(TaskRunTrigger {
            id: "task".to_string(),
            parent_run_id: run_id,
            chain_id: chain.chain_id,
            depth: chain.depth + 1,
            rule,
            payload: trigger.payload().cloned(),
        });
        let child_chain = RunChain {
            chain_id: chain.chain_id,
            parent_run_id: Some(run_id),
            depth: chain.depth + 1,
        };
        match start(state, target_task, child_trigger, child_chain, false) {
            Ok(child_run) => {
                started_children += 1;
                next_tasks.push(NextTaskRecord {
                    task_id: target,
                    run_id: child_run,
                    rule,
                    selector_exit_code,
                });
            }
            Err(error) => {
                debug!(task = %task.id, target = %target, reason = error.reason(), "next task suppressed");
            }
        }
    }

    let failed = first_error.is_some()
        || actions
            .iter()
            .any(|action| action.status == ActionOutcome::Failed);
    Execution {
        actions,
        selector,
        next_tasks,
        outcome: if failed {
            RunOutcome::Failed
        } else {
            RunOutcome::Succeeded
        },
        error: first_error,
    }
}

fn condition_matches(
    condition: NextCondition,
    actions: &[RunAction],
    selector_error: bool,
) -> bool {
    match condition {
        NextCondition::Always => true,
        NextCondition::AllSucceeded => !selector_error && actions.iter().all(RunAction::succeeded),
        NextCondition::AnyFailed => {
            selector_error || actions.iter().any(|action| !action.succeeded())
        }
        NextCondition::AllFailed => {
            !actions.is_empty() && actions.iter().all(|action| !action.succeeded())
        }
        NextCondition::CommandResult => false,
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_action(
    state: &AppState,
    commands: &BTreeMap<String, hodeco_contract::domain::CommandDefinition>,
    task: &TaskDefinition,
    action: &TaskAction,
    trigger: &RunTrigger,
    run_id: Uuid,
    started_at: DateTime<Utc>,
    chain: &RunChain,
    budget: &mut CaptureBudget,
    cancellation: CancellationToken,
) -> RunAction {
    let action_started = Utc::now();
    let base = ActionBase {
        task,
        trigger,
        run_id,
        started_at,
        chain,
    };
    let (status, command_result, message_result, error) = match &action.kind {
        ActionKind::Command(command_action) => {
            let Some(definition) = commands.get(&command_action.command) else {
                return RunAction {
                    id: action.id.clone(),
                    started_at: action_started,
                    finished_at: Utc::now(),
                    status: ActionOutcome::Failed,
                    command: Some(RunCommandAction {
                        command_id: command_action.command.clone(),
                        executable: String::new(),
                        args: command_action.args.clone(),
                        exit_code: None,
                        stdout: None,
                        stderr: None,
                        output_truncated: false,
                    }),
                    message: None,
                    error: Some(format!(
                        "command '{}' does not exist",
                        command_action.command
                    )),
                };
            };
            let stdin = match action_stdin(&base, None) {
                Ok(stdin) => stdin,
                Err(error) => {
                    return failed_action(&action.id, action_started, Some(error), None);
                }
            };
            let capture = command_action.output == hodeco_contract::domain::OutputMode::Capture;
            let outcome = command::run(
                definition,
                &command_action.args,
                &stdin,
                capture,
                budget,
                cancellation,
            )
            .await;
            let status = if outcome.succeeded() {
                ActionOutcome::Succeeded
            } else {
                ActionOutcome::Failed
            };
            let error = if outcome.succeeded() {
                None
            } else if let Some(error) = outcome.error.clone() {
                Some(error)
            } else {
                Some(format!(
                    "command '{}' exited with {:?}",
                    definition.id, outcome.exit_code
                ))
            };
            (
                status,
                Some(RunCommandAction {
                    command_id: definition.id.clone(),
                    executable: definition.executable.display().to_string(),
                    args: definition
                        .args
                        .iter()
                        .chain(command_action.args.iter())
                        .cloned()
                        .collect(),
                    exit_code: outcome.exit_code,
                    stdout: outcome.stdout,
                    stderr: outcome.stderr,
                    output_truncated: outcome.output_truncated,
                }),
                None,
                error,
            )
        }
        ActionKind::Message(message_action) => {
            let payload = match resolve_payload(&message_action.message.payload, trigger) {
                Ok(payload) => payload,
                Err(error) => {
                    return RunAction {
                        id: action.id.clone(),
                        started_at: action_started,
                        finished_at: Utc::now(),
                        status: ActionOutcome::Failed,
                        command: None,
                        message: Some(RunMessageAction {
                            service: message_action.message.service.clone(),
                            payload: Base64Bytes::default(),
                            message_id: None,
                            ttl: message_action.message.ttl,
                        }),
                        error: Some(error),
                    };
                }
            };
            let ttl = message_action.message.ttl.map(HumanDuration::as_std);
            let sent = state
                .registry
                .send(&message_action.message.service, payload.clone(), ttl);
            match sent {
                Ok(message) => (
                    ActionOutcome::Succeeded,
                    None,
                    Some(RunMessageAction {
                        service: message_action.message.service.clone(),
                        payload: Base64Bytes::from(payload),
                        message_id: Some(message.id),
                        ttl: message_action.message.ttl,
                    }),
                    None,
                ),
                Err(error) => (
                    ActionOutcome::Failed,
                    None,
                    Some(RunMessageAction {
                        service: message_action.message.service.clone(),
                        payload: Base64Bytes::from(payload),
                        message_id: None,
                        ttl: message_action.message.ttl,
                    }),
                    Some(service_error_text(error)),
                ),
            }
        }
    };

    RunAction {
        id: action.id.clone(),
        started_at: action_started,
        finished_at: Utc::now(),
        status,
        command: command_result,
        message: message_result,
        error,
    }
}

fn failed_action(
    id: &str,
    started_at: DateTime<Utc>,
    error: Option<String>,
    command: Option<RunCommandAction>,
) -> RunAction {
    RunAction {
        id: id.to_string(),
        started_at,
        finished_at: Utc::now(),
        status: ActionOutcome::Failed,
        command,
        message: None,
        error,
    }
}

fn service_error_text(error: ServiceError) -> String {
    match error {
        ServiceError::NotFound => "service not found".to_string(),
        ServiceError::NotReady => "service is not ready".to_string(),
        ServiceError::SendFailed => "service send failed".to_string(),
        ServiceError::InvalidPayload => "the payload is not valid".to_string(),
        other => other.to_string(),
    }
}

struct ActionBase<'a> {
    task: &'a TaskDefinition,
    trigger: &'a RunTrigger,
    run_id: Uuid,
    started_at: DateTime<Utc>,
    chain: &'a RunChain,
}

fn action_stdin(base: &ActionBase<'_>, actions: Option<Value>) -> Result<Vec<u8>, String> {
    let payload = base
        .trigger
        .payload()
        .map(|payload| {
            serde_json::to_value(payload)
                .unwrap_or_else(|_| serde_json::Value::String(String::new()))
        })
        .unwrap_or_else(|| serde_json::Value::String(String::new()));
    let value = json!({
        "run": {
            "id": base.run_id,
            "taskId": base.task.id,
            "trigger": {
                "id": base.trigger.id(),
                "type": base.trigger.kind(),
            },
            "parentRunId": base.chain.parent_run_id,
            "chainId": base.chain.chain_id,
            "depth": base.chain.depth,
            "startedAt": base.started_at,
        },
        "payload": payload,
        "payloadLength": base.trigger.payload().map_or(0, |payload| payload.as_slice().len()),
        "metadata": trigger_metadata(base.trigger),
        "actions": actions,
    });
    serde_json::to_vec(&value).map_err(|error| error.to_string())
}

fn trigger_metadata(trigger: &RunTrigger) -> Value {
    match trigger {
        RunTrigger::Message(trigger) => json!({
            "messageId": trigger.message_id,
            "time": trigger.time,
            "service": trigger.service,
            "protocol": trigger.protocol,
            "transport": trigger.transport,
            "entry": trigger.entry,
            "state": trigger.state.name(),
            "direction": match trigger.direction {
                MessageDirection::Incoming => "incoming",
                MessageDirection::Outgoing => "outgoing",
            },
        }),
        RunTrigger::Schedule(trigger) => json!({
            "scheduleId": trigger.schedule_id,
            "occurrenceAt": trigger.occurrence_at,
        }),
        RunTrigger::Http(trigger) => json!({
            "method": trigger.method.as_str(),
            "path": trigger.path,
            "query": trigger.query,
            "remote": trigger.remote,
            "headers": trigger.headers,
        }),
        RunTrigger::Manual(trigger) => Value::Object(
            trigger
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        RunTrigger::Task(trigger) => json!({
            "parentRunId": trigger.parent_run_id,
            "chainId": trigger.chain_id,
            "depth": trigger.depth,
            "rule": trigger.rule,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_selector(
    commands: &BTreeMap<String, hodeco_contract::domain::CommandDefinition>,
    task: &TaskDefinition,
    rule: &hodeco_contract::domain::NextRule,
    trigger: &RunTrigger,
    run_id: Uuid,
    started_at: DateTime<Utc>,
    chain: &RunChain,
    actions: &[RunAction],
    budget: &mut CaptureBudget,
    cancellation: CancellationToken,
) -> SelectorResult {
    let Some(command_id) = &rule.command else {
        return SelectorResult {
            command_id: String::new(),
            executable: String::new(),
            args: Vec::new(),
            exit_code: None,
            outcome: SelectorOutcome::Error,
            error: Some("command-result rule has no command".to_string()),
        };
    };
    let Some(definition) = commands.get(command_id) else {
        return SelectorResult {
            command_id: command_id.clone(),
            executable: String::new(),
            args: rule.args.clone(),
            exit_code: None,
            outcome: SelectorOutcome::Error,
            error: Some(format!("command '{command_id}' does not exist")),
        };
    };
    let action_summary: Vec<Value> = actions
        .iter()
        .map(|action| {
            json!({
                "id": action.id,
                "status": action.status,
                "command": action.command.as_ref().map(|command| &command.command_id),
                "exitCode": action.command.as_ref().and_then(|command| command.exit_code),
                "error": action.error,
            })
        })
        .collect();
    let base = ActionBase {
        task,
        trigger,
        run_id,
        started_at,
        chain,
    };
    let stdin = match action_stdin(&base, Some(Value::Array(action_summary))) {
        Ok(stdin) => stdin,
        Err(error) => {
            return SelectorResult {
                command_id: command_id.clone(),
                executable: definition.executable.display().to_string(),
                args: rule.args.clone(),
                exit_code: None,
                outcome: SelectorOutcome::Error,
                error: Some(error),
            };
        }
    };
    let outcome = command::run(definition, &rule.args, &stdin, false, budget, cancellation).await;
    if let Some(error) = outcome.error {
        return SelectorResult {
            command_id: command_id.clone(),
            executable: definition.executable.display().to_string(),
            args: rule.args.clone(),
            exit_code: outcome.exit_code,
            outcome: SelectorOutcome::Error,
            error: Some(error),
        };
    }
    let code = outcome.exit_code;
    let selector_outcome = match code {
        Some(255) => SelectorOutcome::Error,
        Some(code) if (0..=254).contains(&code) => {
            if rule.cases.contains_key(&(code as u8)) {
                SelectorOutcome::Matched
            } else {
                SelectorOutcome::NoMatch
            }
        }
        Some(code) => {
            let _ = code;
            SelectorOutcome::Error
        }
        None => SelectorOutcome::Error,
    };
    SelectorResult {
        command_id: command_id.clone(),
        executable: definition.executable.display().to_string(),
        args: definition
            .args
            .iter()
            .chain(rule.args.iter())
            .cloned()
            .collect(),
        exit_code: code,
        outcome: selector_outcome,
        error: if selector_outcome == SelectorOutcome::Error {
            Some(match code {
                Some(255) => "selector exited with 255".to_string(),
                Some(code) => format!("selector exited with {code}"),
                None => "selector terminated without an exit code".to_string(),
            })
        } else {
            None
        },
    }
}

fn resolve_payload(payload: &MessagePayload, trigger: &RunTrigger) -> Result<Vec<u8>, String> {
    match payload {
        MessagePayload::Trigger => Ok(trigger
            .payload()
            .map(|payload| payload.as_slice().to_vec())
            .unwrap_or_default()),
        MessagePayload::Text { text } => Ok(text.as_bytes().to_vec()),
        MessagePayload::Hex { hex } => decode_hex(hex),
        MessagePayload::Base64 { base64 } => Base64Bytes::from_str(base64)
            .map(Base64Bytes::into_inner)
            .map_err(|error| format!("invalid base64 payload: {error}")),
    }
}

fn decode_hex(input: &str) -> Result<Vec<u8>, String> {
    let compact: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if !compact.len().is_multiple_of(2) {
        return Err("hex payload must contain an even number of digits".to_string());
    }
    let mut bytes = Vec::with_capacity(compact.len() / 2);
    for pair in compact.chunks(2) {
        let text = std::str::from_utf8(pair).map_err(|_| "invalid hex payload".to_string())?;
        bytes.push(u8::from_str_radix(text, 16).map_err(|_| "invalid hex payload".to_string())?);
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Message triggers
// ---------------------------------------------------------------------------

pub async fn handle_store_event(state: &AppState, event: &StoreEvent) {
    let (message, key, transition) = match event {
        StoreEvent::New { key, message } => (message, key, MessageTransition::New),
        StoreEvent::StateChanged { key, message } => {
            (message, key, MessageTransition::StateChanged)
        }
        StoreEvent::Dropped { .. } => return,
    };
    let snapshot = state.automation.snapshot();
    for task in snapshot.tasks.values() {
        if !task.enabled {
            continue;
        }
        for trigger in task.message_triggers() {
            if !message_trigger_matches(state, trigger, transition, message, key) {
                continue;
            }
            let trigger_record = RunTrigger::Message(hodeco_contract::domain::MessageRunTrigger {
                id: trigger.id.clone(),
                message_id: message.id,
                time: message.time,
                service: message.service.clone(),
                protocol: key.protocol.clone(),
                transport: key.transport.clone(),
                entry: key.entry.clone(),
                state: message.state,
                direction: message.state.direction(),
                payload: Some(message.payload.clone()),
            });
            let _ = start(state, task, trigger_record, root_chain(), false);
            // One message run per task: a second matching trigger would only
            // be suppressed as busy.
            break;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageTransition {
    New,
    StateChanged,
}

fn message_trigger_matches(
    state: &AppState,
    trigger: &MessageTrigger,
    transition: MessageTransition,
    message: &hodeco_contract::domain::Message,
    key: &hodeco_contract::domain::ServiceKey,
) -> bool {
    let settled_incoming =
        transition == MessageTransition::New && message.state == MessageState::Incoming;
    let settled_outgoing = matches!(
        message.state,
        MessageState::Outgoing
            | MessageState::OutgoingFailed
            | MessageState::OutgoingRejected
            | MessageState::OutgoingTimedOut
    );
    let transition_matches = match trigger.when {
        hodeco_contract::domain::MessageTriggerWhen::New => transition == MessageTransition::New,
        hodeco_contract::domain::MessageTriggerWhen::StateChanged => {
            transition == MessageTransition::StateChanged
        }
        hodeco_contract::domain::MessageTriggerWhen::Settled => {
            settled_incoming
                || (settled_outgoing && transition == MessageTransition::StateChanged)
                || (settled_outgoing && transition == MessageTransition::New)
        }
    };
    if !transition_matches {
        return false;
    }
    if !trigger.states.is_empty() && !trigger.states.contains(&message.state) {
        return false;
    }
    if !trigger.directions.is_empty() && !trigger.directions.contains(&message.state.direction()) {
        return false;
    }
    if !trigger.protocols.is_empty() && !trigger.protocols.contains(&key.protocol) {
        return false;
    }
    if !trigger.transports.is_empty() && !trigger.transports.contains(&key.transport) {
        return false;
    }
    if !trigger.services.is_empty() {
        let matches = trigger.services.iter().any(|service| {
            state
                .registry
                .resolve(service)
                .is_ok_and(|resolved| &resolved == key)
        });
        if !matches {
            return false;
        }
    }
    if let Some(payload) = &trigger.payload
        && !payload.matches(message.payload.as_slice())
    {
        return false;
    }
    true
}

pub fn task_info(state: &AppState, task: &TaskDefinition) -> TaskInfo {
    let (suppressed_runs, last_suppressed_reason) = state.runtime.suppressed(&task.id);
    TaskInfo {
        task: task.clone(),
        running: state.runtime.running(&task.id),
        last_run: state
            .history
            .last_run(&task.id)
            .or_else(|| state.runtime.last_run(&task.id)),
        next: state.runtime.next(&task.id),
        suppressed_runs,
        last_suppressed_reason,
    }
}

/// Constructs the selector payload shape for tests and documentation.
pub fn selector_actions_example(actions: &[RunAction]) -> Value {
    Value::Array(
        actions
            .iter()
            .map(|action| {
                json!({
                    "id": action.id,
                    "status": action.status,
                    "command": action.command.as_ref().map(|command| &command.command_id),
                    "exitCode": action.command.as_ref().and_then(|command| command.exit_code),
                    "error": action.error,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_hex_and_base64_payloads() {
        assert_eq!(decode_hex("01 02 0a").unwrap(), vec![1, 2, 10]);
        assert!(decode_hex("0").is_err());
        let trigger = RunTrigger::Manual(hodeco_contract::domain::ManualRunTrigger {
            id: "manual".to_string(),
            payload: Some(Base64Bytes::from(vec![9, 8])),
            metadata: BTreeMap::new(),
        });
        assert_eq!(
            resolve_payload(&MessagePayload::Trigger, &trigger).unwrap(),
            vec![9, 8]
        );
        assert_eq!(
            resolve_payload(
                &MessagePayload::Base64 {
                    base64: "AQID".to_string()
                },
                &trigger
            )
            .unwrap(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn evaluates_next_conditions() {
        let success = RunAction {
            id: "a".to_string(),
            started_at: Utc::now(),
            finished_at: Utc::now(),
            status: ActionOutcome::Succeeded,
            command: None,
            message: None,
            error: None,
        };
        let failure = RunAction {
            status: ActionOutcome::Failed,
            error: Some("failed".to_string()),
            ..success.clone()
        };
        assert!(condition_matches(NextCondition::Always, &[], false));
        assert!(condition_matches(
            NextCondition::AllSucceeded,
            std::slice::from_ref(&success),
            false
        ));
        assert!(condition_matches(
            NextCondition::AnyFailed,
            &[success.clone(), failure.clone()],
            false
        ));
        assert!(condition_matches(
            NextCondition::AllFailed,
            &[failure],
            false
        ));
        assert!(!condition_matches(NextCondition::AllFailed, &[], false));
    }
}
