//! Tasks, triggers, actions, next rules, and run history DTOs.
//!
//! The same types are used by the YAML catalogs, the JSON-RPC API, and the
//! UI. Optional fields are omitted on the wire so the shapes stay compact.

use crate::domain::{MessageDirection, MessageState, PayloadExpr, ServiceRef};
use crate::utils::{Base64Bytes, HumanDuration, Size};
use chrono::{DateTime, Utc};
use serde::de;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::time::Duration;
use uuid::Uuid;

pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub const DEFAULT_MAX_RUNS_PER_MINUTE: u32 = 60;
pub const DEFAULT_HISTORY_SIZE: u64 = 100 * 1024;
pub const DEFAULT_HISTORY_ROWS: u32 = 1_000;

pub fn default_run_timeout() -> HumanDuration {
    HumanDuration::from_std(DEFAULT_RUN_TIMEOUT)
}

pub fn default_max_runs_per_minute() -> u32 {
    DEFAULT_MAX_RUNS_PER_MINUTE
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskHistoryConfig {
    pub max_size: Size,
    pub max_rows: u32,
}

impl Default for TaskHistoryConfig {
    fn default() -> Self {
        Self {
            max_size: Size::from_bytes(DEFAULT_HISTORY_SIZE),
            max_rows: DEFAULT_HISTORY_ROWS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub fail_fast: bool,
    pub run_timeout: HumanDuration,
    pub max_runs_per_minute: u32,
    pub history: TaskHistoryConfig,
    pub triggers: Vec<TaskTrigger>,
    pub actions: Vec<TaskAction>,
    pub next: Vec<NextRule>,
}

impl Default for TaskDefinition {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            enabled: true,
            fail_fast: true,
            run_timeout: default_run_timeout(),
            max_runs_per_minute: default_max_runs_per_minute(),
            history: TaskHistoryConfig::default(),
            triggers: Vec::new(),
            actions: Vec::new(),
            next: Vec::new(),
        }
    }
}

impl TaskDefinition {
    pub fn trigger(&self, id: &str) -> Option<&TaskTrigger> {
        self.triggers.iter().find(|trigger| trigger.id() == id)
    }

    pub fn http_trigger(&self) -> Option<&HttpTrigger> {
        self.triggers.iter().find_map(|trigger| match trigger {
            TaskTrigger::Http(trigger) => Some(trigger),
            _ => None,
        })
    }

    pub fn schedule_triggers(&self) -> impl Iterator<Item = &ScheduleTrigger> {
        self.triggers.iter().filter_map(|trigger| match trigger {
            TaskTrigger::Schedule(trigger) => Some(trigger),
            _ => None,
        })
    }

    pub fn message_triggers(&self) -> impl Iterator<Item = &MessageTrigger> {
        self.triggers.iter().filter_map(|trigger| match trigger {
            TaskTrigger::Message(trigger) => Some(trigger),
            _ => None,
        })
    }

    pub fn has_command_result(&self) -> bool {
        self.next
            .iter()
            .any(|rule| rule.when == NextCondition::CommandResult)
    }
}

// ---------------------------------------------------------------------------
// Triggers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TriggerType {
    Message,
    Schedule,
    Http,
    Manual,
    Task,
}

impl TriggerType {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Message => "Message",
            Self::Schedule => "Schedule",
            Self::Http => "HTTP",
            Self::Manual => "Manual",
            Self::Task => "Task",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TaskTrigger {
    Message(MessageTrigger),
    Schedule(ScheduleTrigger),
    Http(HttpTrigger),
    Manual(ManualTrigger),
    Task(TaskSourceTrigger),
}

impl TaskTrigger {
    pub fn id(&self) -> &str {
        match self {
            Self::Message(trigger) => &trigger.id,
            Self::Schedule(trigger) => &trigger.id,
            Self::Http(trigger) => &trigger.id,
            Self::Manual(trigger) => &trigger.id,
            Self::Task(trigger) => &trigger.id,
        }
    }

    pub fn set_id(&mut self, value: String) {
        match self {
            Self::Message(trigger) => trigger.id = value,
            Self::Schedule(trigger) => trigger.id = value,
            Self::Http(trigger) => trigger.id = value,
            Self::Manual(trigger) => trigger.id = value,
            Self::Task(trigger) => trigger.id = value,
        }
    }

    pub const fn kind(&self) -> TriggerType {
        match self {
            Self::Message(_) => TriggerType::Message,
            Self::Schedule(_) => TriggerType::Schedule,
            Self::Http(_) => TriggerType::Http,
            Self::Manual(_) => TriggerType::Manual,
            Self::Task(_) => TriggerType::Task,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageTriggerWhen {
    #[default]
    Settled,
    New,
    StateChanged,
}

impl MessageTriggerWhen {
    pub const ALL: [Self; 3] = [Self::Settled, Self::New, Self::StateChanged];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Settled => "When settled",
            Self::New => "On registration",
            Self::StateChanged => "On every state change",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct MessageTrigger {
    pub id: String,
    pub when: MessageTriggerWhen,
    pub states: Vec<MessageState>,
    pub directions: Vec<MessageDirection>,
    pub protocols: Vec<String>,
    pub transports: Vec<String>,
    pub services: Vec<ServiceRef>,
    pub payload: Option<PayloadExpr>,
}

impl Default for MessageTrigger {
    fn default() -> Self {
        Self {
            id: String::new(),
            when: MessageTriggerWhen::Settled,
            states: Vec::new(),
            directions: Vec::new(),
            protocols: Vec::new(),
            transports: Vec::new(),
            services: Vec::new(),
            payload: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScheduleTrigger {
    pub id: String,
    pub schedule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runs: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HttpTrigger {
    pub id: String,
    #[serde(default)]
    pub methods: Vec<HttpMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

impl HttpTrigger {
    pub fn allows(&self, method: HttpMethod) -> bool {
        self.methods.is_empty() || self.methods.contains(&method)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
}

impl HttpMethod {
    pub const ALL: [Self; 2] = [Self::Get, Self::Post];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManualTrigger {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskSourceTrigger {
    pub id: String,
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputMode {
    #[default]
    Ignore,
    Capture,
}

impl OutputMode {
    pub const ALL: [Self; 2] = [Self::Ignore, Self::Capture];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Ignore => "Ignore",
            Self::Capture => "Capture (up to 8 KiB per run)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CommandAction {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub output: OutputMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MessageAction {
    pub message: MessageActionPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MessageActionPayload {
    pub service: ServiceRef,
    pub payload: MessagePayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<HumanDuration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "camelCase")]
pub enum MessagePayload {
    Trigger,
    Text { text: String },
    Hex { hex: String },
    Base64 { base64: String },
}

impl MessagePayload {
    pub const fn source_label(&self) -> &'static str {
        match self {
            Self::Trigger => "Trigger payload",
            Self::Text { .. } => "Text",
            Self::Hex { .. } => "Hex",
            Self::Base64 { .. } => "Base64",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskAction {
    pub id: String,
    pub kind: ActionKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActionKind {
    Command(CommandAction),
    Message(MessageAction),
}

impl TaskAction {
    pub fn command(&self) -> Option<&CommandAction> {
        match &self.kind {
            ActionKind::Command(action) => Some(action),
            ActionKind::Message(_) => None,
        }
    }

    pub fn message(&self) -> Option<&MessageAction> {
        match &self.kind {
            ActionKind::Message(action) => Some(action),
            ActionKind::Command(_) => None,
        }
    }

    pub const fn kind_label(&self) -> &'static str {
        match &self.kind {
            ActionKind::Command(_) => "Command",
            ActionKind::Message(_) => "Message",
        }
    }
}

impl Serialize for TaskAction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", &self.id)?;
        match &self.kind {
            ActionKind::Command(action) => {
                map.serialize_entry("command", &action.command)?;
                if !action.args.is_empty() {
                    map.serialize_entry("args", &action.args)?;
                }
                map.serialize_entry("output", &action.output)?;
            }
            ActionKind::Message(action) => {
                map.serialize_entry("message", &action.message)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for TaskAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct Repr {
            id: String,
            command: Option<String>,
            #[serde(default)]
            args: Vec<String>,
            #[serde(default)]
            output: OutputMode,
            message: Option<MessageActionPayload>,
        }

        let repr = Repr::deserialize(deserializer)?;
        match (repr.command, repr.message) {
            (Some(command), None) => Ok(Self {
                id: repr.id,
                kind: ActionKind::Command(CommandAction {
                    command,
                    args: repr.args,
                    output: repr.output,
                }),
            }),
            (None, Some(message)) => Ok(Self {
                id: repr.id,
                kind: ActionKind::Message(MessageAction { message }),
            }),
            _ => Err(de::Error::custom(
                "an action must contain exactly one of 'command' or 'message'",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Next rules
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NextCondition {
    #[serde(rename = "always")]
    Always,
    #[serde(rename = "allSucceeded")]
    AllSucceeded,
    #[serde(rename = "anyFailed")]
    AnyFailed,
    #[serde(rename = "allFailed")]
    AllFailed,
    #[serde(rename = "command-result")]
    CommandResult,
}

impl NextCondition {
    pub const ALL: [Self; 5] = [
        Self::Always,
        Self::AllSucceeded,
        Self::AnyFailed,
        Self::AllFailed,
        Self::CommandResult,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Always => "Always",
            Self::AllSucceeded => "All actions succeeded",
            Self::AnyFailed => "Any action failed",
            Self::AllFailed => "All actions failed",
            Self::CommandResult => "Command result",
        }
    }
}

/// A single value or an array of values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> Default for OneOrMany<T> {
    fn default() -> Self {
        Self::Many(Vec::new())
    }
}

impl<T> OneOrMany<T> {
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        match self {
            Self::One(value) => std::slice::from_ref(value).iter(),
            Self::Many(values) => values.iter(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::One(_) => false,
            Self::Many(values) => values.is_empty(),
        }
    }

    pub fn as_slice(&self) -> &[T] {
        match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
}

impl<T> From<Vec<T>> for OneOrMany<T> {
    fn from(values: Vec<T>) -> Self {
        Self::Many(values)
    }
}

impl<T> From<T> for OneOrMany<T> {
    fn from(value: T) -> Self {
        Self::One(value)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct NextRule {
    pub when: NextCondition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub tasks: OneOrMany<String>,
    #[serde(default)]
    pub cases: BTreeMap<u8, OneOrMany<String>>,
}

impl Default for NextRule {
    fn default() -> Self {
        Self {
            when: NextCondition::Always,
            command: None,
            args: Vec::new(),
            tasks: OneOrMany::Many(Vec::new()),
            cases: BTreeMap::new(),
        }
    }
}

impl NextRule {
    pub fn targets(&self) -> Vec<&str> {
        self.tasks.iter().map(String::as_str).collect()
    }

    pub fn case_targets(&self, code: u8) -> Vec<&str> {
        self.cases
            .get(&code)
            .map(|targets| targets.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Runtime and history
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NextFire {
    pub at: DateTime<Utc>,
    pub trigger_id: String,
    pub trigger_type: TriggerType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskInfo {
    pub task: TaskDefinition,
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<RunSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<NextFire>,
    pub suppressed_runs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_suppressed_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TriggerRef {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: TriggerType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl RunOutcome {
    pub const ALL: [Self; 3] = [Self::Succeeded, Self::Failed, Self::Cancelled];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunSummary {
    pub id: Uuid,
    pub task_id: String,
    /// Monotonic history timestamp used for paging.
    pub time: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub trigger: TriggerRef,
    pub outcome: RunOutcome,
    pub action_count: u32,
    pub next_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunChain {
    pub chain_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<Uuid>,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RunTrigger {
    Message(MessageRunTrigger),
    Schedule(ScheduleRunTrigger),
    Http(HttpRunTrigger),
    Manual(ManualRunTrigger),
    Task(TaskRunTrigger),
}

impl RunTrigger {
    pub fn id(&self) -> &str {
        match self {
            Self::Message(trigger) => &trigger.id,
            Self::Schedule(trigger) => &trigger.id,
            Self::Http(trigger) => &trigger.id,
            Self::Manual(trigger) => &trigger.id,
            Self::Task(trigger) => &trigger.id,
        }
    }

    pub const fn kind(&self) -> TriggerType {
        match self {
            Self::Message(_) => TriggerType::Message,
            Self::Schedule(_) => TriggerType::Schedule,
            Self::Http(_) => TriggerType::Http,
            Self::Manual(_) => TriggerType::Manual,
            Self::Task(_) => TriggerType::Task,
        }
    }

    pub fn payload(&self) -> Option<&Base64Bytes> {
        match self {
            Self::Message(trigger) => trigger.payload.as_ref(),
            Self::Http(trigger) => trigger.payload.as_ref(),
            Self::Manual(trigger) => trigger.payload.as_ref(),
            Self::Task(trigger) => trigger.payload.as_ref(),
            Self::Schedule(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MessageRunTrigger {
    pub id: String,
    pub message_id: Uuid,
    pub time: DateTime<Utc>,
    pub service: ServiceRef,
    pub protocol: String,
    pub transport: String,
    pub entry: String,
    pub state: MessageState,
    pub direction: MessageDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Base64Bytes>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScheduleRunTrigger {
    pub id: String,
    pub schedule_id: String,
    pub occurrence_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HttpRunTrigger {
    pub id: String,
    pub method: HttpMethod,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Base64Bytes>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManualRunTrigger {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Base64Bytes>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskRunTrigger {
    pub id: String,
    pub parent_run_id: Uuid,
    pub chain_id: Uuid,
    pub depth: u32,
    pub rule: NextCondition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Base64Bytes>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionOutcome {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunAction {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub status: ActionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<RunCommandAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<RunMessageAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RunAction {
    pub fn succeeded(&self) -> bool {
        self.status == ActionOutcome::Succeeded
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunCommandAction {
    pub command_id: String,
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(default)]
    pub output_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunMessageAction {
    pub service: ServiceRef,
    pub payload: Base64Bytes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<HumanDuration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectorOutcome {
    Matched,
    NoMatch,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SelectorResult {
    pub command_id: String,
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub outcome: SelectorOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NextTaskRecord {
    pub task_id: String,
    pub run_id: Uuid,
    pub rule: NextCondition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector_exit_code: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunStage {
    Trigger,
    Actions,
    Selector,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunError {
    pub stage: RunStage,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskRun {
    pub summary: RunSummary,
    pub trigger: RunTrigger,
    #[serde(default)]
    pub actions: Vec<RunAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<SelectorResult>,
    #[serde(default)]
    pub next_tasks: Vec<NextTaskRecord>,
    pub chain: RunChain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RunError>,
}

impl TaskRun {
    pub fn from_summary(summary: RunSummary, trigger: RunTrigger, chain: RunChain) -> Self {
        Self {
            summary,
            trigger,
            actions: Vec::new(),
            selector: None,
            next_tasks: Vec::new(),
            chain,
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_task_yaml() {
        let yaml = r#"
id: evening-lights
name: Evening lights
description: Turn on the porch lights
enabled: true
failFast: true
runTimeout: 5m
maxRunsPerMinute: 60
history:
  maxSize: 100 KiB
  maxRows: 1000
triggers:
  - id: porch-motion
    type: message
    when: settled
    states: [incoming]
    protocols: [zwave]
  - id: sunset
    type: schedule
    schedule: before-sunset
    maxRuns: 365
  - id: hook
    type: http
    methods: [POST]
actions:
  - id: lights
    command: lights-on
    args: ["--porch"]
    output: ignore
  - id: notify
    message:
      service: { alias: ZW }
      payload: { source: trigger }
      ttl: 30s
next:
  - when: allSucceeded
    tasks: log-success
  - when: command-result
    command: classify
    args: ["--json"]
    cases:
      0: success-task
      1: [notify, retry]
"#;
        let task: TaskDefinition = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(task.id, "evening-lights");
        assert_eq!(task.triggers.len(), 3);
        assert_eq!(task.triggers[0].kind(), TriggerType::Message);
        assert_eq!(task.triggers[1].kind(), TriggerType::Schedule);
        assert_eq!(task.triggers[2].kind(), TriggerType::Http);
        assert_eq!(task.actions.len(), 2);
        assert_eq!(task.actions[0].command().unwrap().command, "lights-on");
        assert_eq!(
            task.actions[1].message().unwrap().message.payload,
            MessagePayload::Trigger
        );
        assert_eq!(task.next[0].when, NextCondition::AllSucceeded);
        assert_eq!(task.next[0].targets(), vec!["log-success"]);
        assert_eq!(task.next[1].case_targets(1), vec!["notify", "retry"]);
    }

    #[test]
    fn round_trips_actions() {
        let actions = vec![
            TaskAction {
                id: "meter".to_string(),
                kind: ActionKind::Command(CommandAction {
                    command: "meter".to_string(),
                    args: vec!["--json".to_string()],
                    output: OutputMode::Capture,
                }),
            },
            TaskAction {
                id: "notify".to_string(),
                kind: ActionKind::Message(MessageAction {
                    message: MessageActionPayload {
                        service: ServiceRef::Alias {
                            alias: "ZW".to_string(),
                        },
                        payload: MessagePayload::Text {
                            text: "hello".to_string(),
                        },
                        ttl: Some(HumanDuration::from_std(Duration::from_secs(30))),
                    },
                }),
            },
        ];
        let yaml = serde_yaml_ng::to_string(&actions).unwrap();
        let parsed: Vec<TaskAction> = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(parsed, actions);
    }

    #[test]
    fn rejects_actions_with_zero_or_two_kinds() {
        let none: Result<TaskAction, _> = serde_yaml_ng::from_str("id: x\nargs: []\n");
        assert!(none.is_err());

        let both: Result<TaskAction, _> = serde_yaml_ng::from_str(
            "id: x\ncommand: meter\nmessage:\n  service: {alias: ZW}\n  payload: {source: trigger}\n",
        );
        assert!(both.is_err());
    }

    #[test]
    fn serializes_run_summaries() {
        let now = DateTime::parse_from_rfc3339("2026-11-17T16:42:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let summary = RunSummary {
            id: Uuid::nil(),
            task_id: "task".to_string(),
            time: now,
            started_at: now,
            finished_at: now,
            trigger: TriggerRef {
                id: "sunset".to_string(),
                kind: TriggerType::Schedule,
            },
            outcome: RunOutcome::Succeeded,
            action_count: 2,
            next_count: 1,
            error: None,
        };
        let value = serde_json::to_value(&summary).unwrap();
        assert_eq!(value["trigger"]["type"], "schedule");
        assert_eq!(value["outcome"], "succeeded");
        assert!(value.get("error").is_none());
    }
}

#[cfg(test)]
mod serialization_tests {
    use super::*;

    #[test]
    fn deserializes_json_case_keys_as_exit_codes() {
        let rule: NextRule = serde_json::from_value(serde_json::json!({
            "when": "command-result",
            "command": "classify",
            "cases": {"0": "ok", "1": ["notify", "retry"]}
        }))
        .unwrap();
        assert_eq!(rule.case_targets(0), vec!["ok"]);
        assert_eq!(rule.case_targets(1), vec!["notify", "retry"]);
    }
}
