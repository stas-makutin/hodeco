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
    pub services: Vec<ServiceConfig>,
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
    pub auto_persist: HumanDuration,
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
