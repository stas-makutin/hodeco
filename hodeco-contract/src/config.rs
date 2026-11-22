use crate::domain::command::CommandDefinition;
use crate::domain::schedule::{Location, ScheduleDefinition};
use crate::domain::task::TaskDefinition;
use crate::utils::{FileMode, HumanDuration, Size};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    pub working_dir: PathBuf,
    pub log: LogConfig,
    pub http_server: HttpServerConfig,
    pub message_log: MessageLogConfig,
    pub files: FileSetConfig,
    pub task_log: TaskLogConfig,
    pub task_engine: TaskEngineConfig,
}

/// Paths to the four sibling catalogs. Relative paths are resolved against the
/// directory containing `config.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct FileSetConfig {
    pub services: PathBuf,
    pub tasks: PathBuf,
    pub schedules: PathBuf,
    pub commands: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskLogConfig {
    pub dir: PathBuf,
    pub file_mode: FileMode,
    pub compaction_factor: f64,
    pub compact_interval: HumanDuration,
    pub on_corrupt: CorruptPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskEngineConfig {
    pub max_trigger_payload_size: Size,
    pub max_concurrent_runs: u32,
    pub max_chain_depth: u32,
    pub max_fan_out: u32,
    pub run_feed: RunFeedConfig,
}

/// Bounded in-memory feed used by the Runs view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct RunFeedConfig {
    pub max_rows: u32,
    pub max_size: Size,
}

/// Services file shape (`services.yaml`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ServicesFile {
    pub services: Vec<ServiceConfig>,
}

/// Tasks file shape (`tasks.yaml`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct TasksFile {
    pub tasks: Vec<TaskDefinition>,
}

/// Schedules file shape (`schedules.yaml`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct SchedulesFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub schedules: Vec<ScheduleDefinition>,
}

/// Commands file shape (`commands.yaml`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct CommandsFile {
    pub commands: Vec<CommandDefinition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct LogConfig {
    pub dir: PathBuf,
    pub file: String,
    pub dir_mode: FileMode,
    pub file_mode: FileMode,
    pub max_size: Size,
    pub max_age: HumanDuration,
    pub backups: u32,
    pub backup_days: u32,
    pub archive: Archive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Archive {
    #[default]
    None,
    Zip,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct HttpServerConfig {
    pub bind: String,
    pub port: u16,
    pub max_connections: u32,
    /// 0 removes the practical connection limit.
    pub read_timeout: HumanDuration,
    pub read_header_timeout: HumanDuration,
    pub write_timeout: HumanDuration,
    pub idle_timeout: HumanDuration,
    pub max_header_bytes: u32,
    #[serde(rename = "static")]
    pub static_assets: StaticAssetsConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct StaticAssetsConfig {
    pub dir: PathBuf,
    pub index: String,
    pub spa_fallback: bool,
    /// Prefer sibling `.br`/`.gz` files when the client supports them.
    pub precompressed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct MessageLogConfig {
    pub max_size: Size,
    pub file: PathBuf,
    pub file_mode: FileMode,
    /// Flush interval for message-history writes. Defaults to 60 seconds.
    pub auto_persist: HumanDuration,
    pub compaction_factor: f64,
    pub compact_interval: HumanDuration,
    pub on_corrupt: CorruptPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum CorruptPolicy {
    #[default]
    Ignore,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServiceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub protocol: String,
    pub transport: String,
    pub entry: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, serde_json::Value>,
}
