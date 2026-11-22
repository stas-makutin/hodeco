//! Editable configuration draft and save state.

use hodeco_contract::config::{
    Archive, Config, CorruptPolicy, FileSetConfig, HttpServerConfig, LogConfig, MessageLogConfig,
    RunFeedConfig, StaticAssetsConfig, TaskEngineConfig, TaskLogConfig,
};
use hodeco_contract::utils::{FileMode, HumanDuration, Size};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct ConfigUi {
    pub draft: Option<ConfigDraft>,
    pub dirty: bool,
    pub saving: bool,
    pub error: Option<String>,
    pub restart_required: Vec<String>,
    pub saved: bool,
}

impl ConfigUi {
    pub fn load(&mut self, config: &Config) {
        self.draft = Some(ConfigDraft::from_config(config));
        self.dirty = false;
        self.saving = false;
        self.error = None;
    }

    pub fn apply_saved(&mut self, config: &Config, restart_required: Vec<String>) {
        self.load(config);
        self.saved = true;
        self.restart_required = restart_required;
    }

    pub fn build(&mut self) -> Result<Config, String> {
        self.draft
            .as_mut()
            .ok_or_else(|| "configuration is not loaded".to_string())?
            .build_config()
    }
}

#[derive(Debug, Clone)]
pub struct ConfigDraft {
    pub working_dir: String,
    pub log: LogDraft,
    pub http_server: HttpDraft,
    pub message_log: MessageLogDraft,
    pub task_log: TaskLogDraft,
    pub files: FilesDraft,
    pub task_engine: TaskEngineDraft,
}

#[derive(Debug, Clone)]
pub struct LogDraft {
    pub dir: String,
    pub file: String,
    pub dir_mode: String,
    pub file_mode: String,
    pub max_size: String,
    pub max_age: String,
    pub backups: String,
    pub backup_days: String,
    pub archive: Archive,
}

#[derive(Debug, Clone)]
pub struct HttpDraft {
    pub bind: String,
    pub port: String,
    pub max_connections: String,
    pub read_timeout: String,
    pub read_header_timeout: String,
    pub write_timeout: String,
    pub idle_timeout: String,
    pub max_header_bytes: String,
    pub static_assets: StaticDraft,
}

#[derive(Debug, Clone)]
pub struct StaticDraft {
    pub dir: String,
    pub index: String,
    pub spa_fallback: bool,
    pub precompressed: bool,
}

#[derive(Debug, Clone)]
pub struct MessageLogDraft {
    pub max_size: String,
    pub file: String,
    pub file_mode: String,
    pub auto_persist: String,
    pub compaction_factor: String,
    pub compact_interval: String,
    pub on_corrupt: CorruptPolicy,
}

#[derive(Debug, Clone)]
pub struct TaskLogDraft {
    pub dir: String,
    pub file_mode: String,
    pub compaction_factor: String,
    pub compact_interval: String,
    pub on_corrupt: CorruptPolicy,
}

#[derive(Debug, Clone)]
pub struct FilesDraft {
    pub services: String,
    pub tasks: String,
    pub schedules: String,
    pub commands: String,
}

#[derive(Debug, Clone)]
pub struct TaskEngineDraft {
    pub max_trigger_payload_size: String,
    pub max_concurrent_runs: String,
    pub max_chain_depth: String,
    pub max_fan_out: String,
    pub run_feed: RunFeedDraft,
}

#[derive(Debug, Clone)]
pub struct RunFeedDraft {
    pub max_rows: String,
    pub max_size: String,
}

impl ConfigDraft {
    pub fn from_config(config: &Config) -> Self {
        Self {
            working_dir: config.working_dir.display().to_string(),
            log: LogDraft {
                dir: config.log.dir.display().to_string(),
                file: config.log.file.clone(),
                dir_mode: config.log.dir_mode.to_string(),
                file_mode: config.log.file_mode.to_string(),
                max_size: config.log.max_size.to_string(),
                max_age: config.log.max_age.to_string(),
                backups: config.log.backups.to_string(),
                backup_days: config.log.backup_days.to_string(),
                archive: config.log.archive,
            },
            http_server: HttpDraft {
                bind: config.http_server.bind.clone(),
                port: config.http_server.port.to_string(),
                max_connections: config.http_server.max_connections.to_string(),
                read_timeout: config.http_server.read_timeout.to_string(),
                read_header_timeout: config.http_server.read_header_timeout.to_string(),
                write_timeout: config.http_server.write_timeout.to_string(),
                idle_timeout: config.http_server.idle_timeout.to_string(),
                max_header_bytes: config.http_server.max_header_bytes.to_string(),
                static_assets: StaticDraft {
                    dir: config.http_server.static_assets.dir.display().to_string(),
                    index: config.http_server.static_assets.index.clone(),
                    spa_fallback: config.http_server.static_assets.spa_fallback,
                    precompressed: config.http_server.static_assets.precompressed,
                },
            },
            message_log: MessageLogDraft {
                max_size: config.message_log.max_size.to_string(),
                file: config.message_log.file.display().to_string(),
                file_mode: config.message_log.file_mode.to_string(),
                auto_persist: config.message_log.auto_persist.to_string(),
                compaction_factor: config.message_log.compaction_factor.to_string(),
                compact_interval: config.message_log.compact_interval.to_string(),
                on_corrupt: config.message_log.on_corrupt,
            },
            task_log: TaskLogDraft {
                dir: config.task_log.dir.display().to_string(),
                file_mode: config.task_log.file_mode.to_string(),
                compaction_factor: config.task_log.compaction_factor.to_string(),
                compact_interval: config.task_log.compact_interval.to_string(),
                on_corrupt: config.task_log.on_corrupt,
            },
            files: FilesDraft {
                services: config.files.services.display().to_string(),
                tasks: config.files.tasks.display().to_string(),
                schedules: config.files.schedules.display().to_string(),
                commands: config.files.commands.display().to_string(),
            },
            task_engine: TaskEngineDraft {
                max_trigger_payload_size: config.task_engine.max_trigger_payload_size.to_string(),
                max_concurrent_runs: config.task_engine.max_concurrent_runs.to_string(),
                max_chain_depth: config.task_engine.max_chain_depth.to_string(),
                max_fan_out: config.task_engine.max_fan_out.to_string(),
                run_feed: RunFeedDraft {
                    max_rows: config.task_engine.run_feed.max_rows.to_string(),
                    max_size: config.task_engine.run_feed.max_size.to_string(),
                },
            },
        }
    }

    /// True when the editable draft no longer represents the loaded config.
    /// Parsing is used so whitespace or equivalent duration/size spellings do
    /// not keep the form dirty. Invalid input counts as dirty.
    pub fn differs_from(&mut self, config: &Config) -> bool {
        match self.build_config() {
            Ok(built) => built != *config,
            Err(_) => true,
        }
    }

    pub fn build_config(&mut self) -> Result<Config, String> {
        Ok(Config {
            working_dir: PathBuf::from(&self.working_dir),
            log: LogConfig {
                dir: PathBuf::from(&self.log.dir),
                file: self.log.file.clone(),
                dir_mode: parse_file_mode(&self.log.dir_mode, "log.dirMode")?,
                file_mode: parse_file_mode(&self.log.file_mode, "log.fileMode")?,
                max_size: parse_size(&self.log.max_size, "log.maxSize")?,
                max_age: parse_duration(&self.log.max_age, "log.maxAge")?,
                backups: parse_u32(&self.log.backups, "log.backups")?,
                backup_days: parse_u32(&self.log.backup_days, "log.backupDays")?,
                archive: self.log.archive,
            },
            http_server: HttpServerConfig {
                bind: self.http_server.bind.clone(),
                port: parse_u16(&self.http_server.port, "httpServer.port")?,
                max_connections: parse_u32(
                    &self.http_server.max_connections,
                    "httpServer.maxConnections",
                )?,
                read_timeout: parse_duration(
                    &self.http_server.read_timeout,
                    "httpServer.readTimeout",
                )?,
                read_header_timeout: parse_duration(
                    &self.http_server.read_header_timeout,
                    "httpServer.readHeaderTimeout",
                )?,
                write_timeout: parse_duration(
                    &self.http_server.write_timeout,
                    "httpServer.writeTimeout",
                )?,
                idle_timeout: parse_duration(
                    &self.http_server.idle_timeout,
                    "httpServer.idleTimeout",
                )?,
                max_header_bytes: parse_u32(
                    &self.http_server.max_header_bytes,
                    "httpServer.maxHeaderBytes",
                )?,
                static_assets: StaticAssetsConfig {
                    dir: PathBuf::from(&self.http_server.static_assets.dir),
                    index: self.http_server.static_assets.index.clone(),
                    spa_fallback: self.http_server.static_assets.spa_fallback,
                    precompressed: self.http_server.static_assets.precompressed,
                },
            },
            message_log: MessageLogConfig {
                max_size: parse_size(&self.message_log.max_size, "messageLog.maxSize")?,
                file: PathBuf::from(&self.message_log.file),
                file_mode: parse_file_mode(&self.message_log.file_mode, "messageLog.fileMode")?,
                auto_persist: parse_duration(
                    &self.message_log.auto_persist,
                    "messageLog.autoPersist",
                )?,
                compaction_factor: parse_factor(
                    &self.message_log.compaction_factor,
                    "messageLog.compactionFactor",
                )?,
                compact_interval: parse_duration(
                    &self.message_log.compact_interval,
                    "messageLog.compactInterval",
                )?,
                on_corrupt: self.message_log.on_corrupt,
            },
            files: FileSetConfig {
                services: PathBuf::from(&self.files.services),
                tasks: PathBuf::from(&self.files.tasks),
                schedules: PathBuf::from(&self.files.schedules),
                commands: PathBuf::from(&self.files.commands),
            },
            task_log: TaskLogConfig {
                dir: PathBuf::from(&self.task_log.dir),
                file_mode: parse_file_mode(&self.task_log.file_mode, "taskLog.fileMode")?,
                compaction_factor: parse_factor(
                    &self.task_log.compaction_factor,
                    "taskLog.compactionFactor",
                )?,
                compact_interval: parse_duration(
                    &self.task_log.compact_interval,
                    "taskLog.compactInterval",
                )?,
                on_corrupt: self.task_log.on_corrupt,
            },
            task_engine: TaskEngineConfig {
                max_trigger_payload_size: parse_size(
                    &self.task_engine.max_trigger_payload_size,
                    "taskEngine.maxTriggerPayloadSize",
                )?,
                max_concurrent_runs: parse_u32(
                    &self.task_engine.max_concurrent_runs,
                    "taskEngine.maxConcurrentRuns",
                )?,
                max_chain_depth: parse_u32(
                    &self.task_engine.max_chain_depth,
                    "taskEngine.maxChainDepth",
                )?,
                max_fan_out: parse_u32(&self.task_engine.max_fan_out, "taskEngine.maxFanOut")?,
                run_feed: RunFeedConfig {
                    max_rows: parse_u32(
                        &self.task_engine.run_feed.max_rows,
                        "taskEngine.runFeed.maxRows",
                    )?,
                    max_size: parse_size(
                        &self.task_engine.run_feed.max_size,
                        "taskEngine.runFeed.maxSize",
                    )?,
                },
            },
        })
    }
}

fn parse_size(value: &str, field: &str) -> Result<Size, String> {
    value
        .trim()
        .parse()
        .map_err(|error| format!("{field}: {error}"))
}

fn parse_duration(value: &str, field: &str) -> Result<HumanDuration, String> {
    value
        .trim()
        .parse()
        .map_err(|error| format!("{field}: {error}"))
}

fn parse_file_mode(value: &str, field: &str) -> Result<FileMode, String> {
    value
        .trim()
        .parse()
        .map_err(|error| format!("{field}: {error}"))
}

fn parse_u16(value: &str, field: &str) -> Result<u16, String> {
    value
        .trim()
        .parse()
        .map_err(|error| format!("{field}: {error}"))
}

fn parse_u32(value: &str, field: &str) -> Result<u32, String> {
    value
        .trim()
        .parse()
        .map_err(|error| format!("{field}: {error}"))
}

fn parse_factor(value: &str, field: &str) -> Result<f64, String> {
    value
        .trim()
        .parse()
        .map_err(|error| format!("{field}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_roundtrip_the_wire_configuration() {
        let config = Config::default();
        let mut draft = ConfigDraft::from_config(&config);
        assert_eq!(draft.build_config().unwrap(), config);
    }

    #[test]
    fn reverting_a_field_clears_dirty_state() {
        let config = Config::default();
        let mut draft = ConfigDraft::from_config(&config);
        draft.http_server.port = "8181".to_string();
        assert!(draft.differs_from(&config));
        draft.http_server.port = "8180".to_string();
        assert!(!draft.differs_from(&config));
    }

    #[test]
    fn invalid_numbers_report_their_field() {
        let config = Config::default();
        let mut draft = ConfigDraft::from_config(&config);
        draft.http_server.port = "not-a-port".to_string();
        let error = draft.build_config().unwrap_err();
        assert!(error.contains("httpServer.port"), "{error}");
    }

    #[test]
    fn task_log_fields_are_editable() {
        let config = Config::default();
        let mut draft = ConfigDraft::from_config(&config);
        draft.task_log.compaction_factor = "3.0".to_string();
        draft.task_log.compact_interval = "7 d".to_string();
        let built = draft.build_config().unwrap();
        assert_eq!(built.task_log.compaction_factor, 3.0);
        let expected: HumanDuration = "7 d".parse().unwrap();
        assert_eq!(built.task_log.compact_interval, expected);
        assert_eq!(built.task_log.dir, config.task_log.dir);
    }

    #[test]
    fn task_engine_and_files_are_editable() {
        let config = Config::default();
        let mut draft = ConfigDraft::from_config(&config);
        draft.task_engine.max_chain_depth = "12".to_string();
        draft.task_engine.run_feed.max_rows = "750".to_string();
        draft.task_engine.run_feed.max_size = "8 MiB".to_string();
        draft.files.tasks = "custom-tasks.yaml".to_string();
        let built = draft.build_config().unwrap();
        assert_eq!(built.task_engine.max_chain_depth, 12);
        assert_eq!(built.task_engine.run_feed.max_rows, 750);
        assert_eq!(
            built.task_engine.run_feed.max_size,
            Size::from_bytes(8 * 1024 * 1024)
        );
        assert_eq!(built.files.tasks, PathBuf::from("custom-tasks.yaml"));
    }
}
