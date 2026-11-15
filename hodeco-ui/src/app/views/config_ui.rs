//! Editable configuration draft and save state.

use crate::ui::params::ParamEditor;
use hodeco_contract::config::{
    Archive, Config, CorruptPolicy, HttpServerConfig, LogConfig, MessageLogConfig, ServiceConfig,
    StaticAssetsConfig,
};
use hodeco_contract::domain::ParamValues;
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
    pub services: Vec<ServiceDraft>,
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
    pub on_corrupt: CorruptPolicy,
}

#[derive(Debug, Clone)]
pub struct ServiceDraft {
    pub alias: String,
    pub protocol: String,
    pub transport: String,
    pub entry: String,
    pub params: ParamValues,
    pub editor: Option<ParamEditor>,
    pub editor_key: Option<(String, String)>,
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
                on_corrupt: config.message_log.on_corrupt,
            },
            services: config
                .services
                .iter()
                .map(|service| ServiceDraft {
                    alias: service.alias.clone().unwrap_or_default(),
                    protocol: service.protocol.clone(),
                    transport: service.transport.clone(),
                    entry: service.entry.clone(),
                    params: service.params.clone(),
                    editor: None,
                    editor_key: None,
                })
                .collect(),
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
        let mut services = Vec::with_capacity(self.services.len());
        for (index, service) in self.services.iter_mut().enumerate() {
            let params = if let Some(editor) = &mut service.editor {
                if service.editor_key.as_ref()
                    == Some(&(service.protocol.clone(), service.transport.clone()))
                {
                    editor
                        .collect()
                        .map_err(|errors| format!("services[{index}]: {}", errors.join("; ")))?
                } else {
                    service.params.clone()
                }
            } else {
                service.params.clone()
            };
            services.push(ServiceConfig {
                alias: (!service.alias.trim().is_empty()).then(|| service.alias.trim().to_string()),
                protocol: service.protocol.clone(),
                transport: service.transport.clone(),
                entry: service.entry.clone(),
                params,
            });
        }

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
                on_corrupt: self.message_log.on_corrupt,
            },
            services,
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
        let mut draft = ConfigDraft::from_config(&Config::default());
        draft.http_server.port = "not-a-port".to_string();
        let error = draft.build_config().unwrap_err();
        assert!(error.contains("httpServer.port"), "{error}");
    }

    #[test]
    fn service_overrides_roundtrip() {
        let mut config = Config::default();
        config.services.push(ServiceConfig {
            alias: Some("ZW".to_string()),
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: "COM4".to_string(),
            params: ParamValues::from([("baudRate".to_string(), serde_json::json!(57_600))]),
        });
        let mut draft = ConfigDraft::from_config(&config);
        assert_eq!(draft.build_config().unwrap(), config);
    }
}
