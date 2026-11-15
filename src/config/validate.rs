use super::loader::ConfigError;
use crate::services::catalog;
use hodeco_contract::config::Config;
use hodeco_contract::utils::Size;
use std::collections::BTreeSet;

const MIN_MESSAGE_LOG_SIZE: Size = Size::from_bytes(8 * 1024);

/// Top-level configuration sections that only apply after a restart.
///
/// Services are deliberately excluded: they are applied immediately through the
/// `services.*` API, and `config.update` always preserves the current service
/// list. Comparing them against the startup snapshot would produce a false
/// restart warning after any live service add, remove, or alias change.
pub fn restart_required(previous: &Config, next: &Config) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if previous.working_dir != next.working_dir {
        fields.push("workingDir");
    }
    if previous.log != next.log {
        fields.push("log");
    }
    if previous.http_server != next.http_server {
        fields.push("httpServer");
    }
    if previous.message_log != next.message_log {
        fields.push("messageLog");
    }
    fields
}

pub fn validate(config: &Config) -> Result<(), ConfigError> {
    let mut errors = Vec::new();

    if config.working_dir.as_os_str().is_empty() {
        errors.push("workingDir must not be empty".to_string());
    }

    if config.log.dir.as_os_str().is_empty() {
        errors.push("log.dir must not be empty".to_string());
    }
    let log_file = config.log.file.trim();
    if log_file.is_empty() {
        errors.push("log.file must not be empty".to_string());
    } else if log_file == "."
        || log_file == ".."
        || log_file.contains(['/', '\\'])
        || std::path::Path::new(log_file).components().count() != 1
    {
        errors.push("log.file must be a file name, not a path".to_string());
    }

    if config.http_server.bind.trim().is_empty() {
        errors.push("httpServer.bind must not be empty".to_string());
    }
    if config.http_server.port == 0 {
        errors.push("httpServer.port must be greater than 0".to_string());
    }
    if config.http_server.max_connections == 0 {
        errors.push("httpServer.maxConnections must be greater than 0".to_string());
    }
    if config.http_server.read_timeout.as_std().is_zero() {
        errors.push("httpServer.readTimeout must be greater than 0".to_string());
    }
    if config.http_server.read_header_timeout.as_std().is_zero() {
        errors.push("httpServer.readHeaderTimeout must be greater than 0".to_string());
    }
    if config.http_server.write_timeout.as_std().is_zero() {
        errors.push("httpServer.writeTimeout must be greater than 0".to_string());
    }
    if config.http_server.idle_timeout.as_std().is_zero() {
        errors.push("httpServer.idleTimeout must be greater than 0".to_string());
    }
    if config.http_server.max_header_bytes == 0 {
        errors.push("httpServer.maxHeaderBytes must be greater than 0".to_string());
    }
    if config.http_server.static_assets.dir.as_os_str().is_empty() {
        errors.push("httpServer.static.dir must not be empty".to_string());
    }
    if config.http_server.static_assets.index.trim().is_empty() {
        errors.push("httpServer.static.index must not be empty".to_string());
    }

    if config.message_log.max_size < MIN_MESSAGE_LOG_SIZE {
        errors.push(format!(
            "messageLog.maxSize must be at least {}",
            MIN_MESSAGE_LOG_SIZE
        ));
    }
    if config.message_log.file.as_os_str().is_empty() {
        errors.push("messageLog.file must not be empty".to_string());
    } else if config.message_log.file.file_name().is_none() {
        errors.push("messageLog.file must include a file name".to_string());
    }
    if config.message_log.auto_persist.as_std().is_zero() {
        errors.push("messageLog.autoPersist must be greater than 0".to_string());
    }

    let mut aliases = BTreeSet::new();
    let mut keys = BTreeSet::new();
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
        if let Err(error) =
            catalog::validate_service(&service.protocol, &service.transport, &service.params)
        {
            errors.push(format!("services[{index}]: {error}"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::Validation(errors.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodeco_contract::config::ServiceConfig;
    use std::collections::BTreeMap;

    fn service(alias: Option<&str>, protocol: &str, transport: &str, entry: &str) -> ServiceConfig {
        ServiceConfig {
            alias: alias.map(str::to_string),
            protocol: protocol.to_string(),
            transport: transport.to_string(),
            entry: entry.to_string(),
            params: BTreeMap::new(),
        }
    }

    #[test]
    fn accepts_defaults() {
        validate(&Config::default()).unwrap();
    }

    #[test]
    fn rejects_invalid_http_settings() {
        let mut config = Config::default();
        config.http_server.bind.clear();
        config.http_server.port = 0;
        config.http_server.max_connections = 0;
        config.http_server.read_timeout = Default::default();
        let error = validate(&config).unwrap_err().to_string();
        assert!(error.contains("httpServer.bind"));
        assert!(error.contains("httpServer.port"));
        assert!(error.contains("httpServer.maxConnections"));
        assert!(error.contains("httpServer.readTimeout"));
    }

    #[test]
    fn rejects_small_message_log() {
        let mut config = Config::default();
        config.message_log.max_size = Size::from_bytes(1024);
        let error = validate(&config).unwrap_err().to_string();
        assert!(error.contains("messageLog.maxSize"));
    }

    #[test]
    fn rejects_log_file_paths() {
        for file in ["sub/hodeco.log", "sub\\hodeco.log", "..", "."] {
            let mut config = Config::default();
            config.log.file = file.to_string();

            let error = validate(&config).unwrap_err().to_string();
            assert!(error.contains("log.file"), "{file}: {error}");
        }

        let mut config = Config::default();
        config.log.file = "hodeco.log".to_string();
        validate(&config).unwrap();
    }

    #[test]
    fn rejects_message_log_path_without_file_name() {
        let mut config = Config::default();
        config.message_log.file = std::path::PathBuf::from("..");

        let error = validate(&config).unwrap_err().to_string();
        assert!(error.contains("messageLog.file"));
    }

    #[test]
    fn rejects_duplicate_service_aliases_and_keys() {
        let config = Config {
            services: vec![
                service(Some("ZW"), "zwave", "serial", "COM3"),
                service(Some("ZW"), "zwave", "serial", "COM3"),
            ],
            ..Config::default()
        };
        let error = validate(&config).unwrap_err().to_string();
        assert!(error.contains("duplicate service alias 'ZW'"));
        assert!(error.contains("duplicate service key 'zwave:serial:COM3'"));
    }

    #[test]
    fn rejects_empty_service_fields() {
        let config = Config {
            services: vec![service(Some(" "), "", "", "")],
            ..Config::default()
        };
        let error = validate(&config).unwrap_err().to_string();
        assert!(error.contains("services[0].protocol"));
        assert!(error.contains("services[0].transport"));
        assert!(error.contains("services[0].entry"));
        assert!(error.contains("services[0].alias"));
    }

    #[test]
    fn reports_only_changed_sections_as_restart_required() {
        let previous = Config::default();
        assert!(restart_required(&previous, &previous).is_empty());

        let mut next = previous.clone();
        next.http_server.port = 9000;
        next.services
            .push(service(Some("ZW"), "zwave", "serial", "COM4"));
        assert_eq!(restart_required(&previous, &next), vec!["httpServer"]);
    }

    #[test]
    fn rejects_unknown_protocols_transports_and_parameters() {
        let mut unknown_param = service(None, "zwave", "serial", "COM2");
        unknown_param
            .params
            .insert("unknownParam".to_string(), serde_json::json!(1));
        let mut invalid_param = service(None, "zwave", "serial", "COM3");
        invalid_param
            .params
            .insert("baudRate".to_string(), serde_json::json!("fast"));

        let config = Config {
            services: vec![
                service(None, "unknown", "serial", "COM1"),
                unknown_param,
                invalid_param,
            ],
            ..Config::default()
        };
        let error = validate(&config).unwrap_err().to_string();
        assert!(error.contains("services[0]"));
        assert!(error.contains("unknown"));
        assert!(error.contains("services[1]"));
        assert!(error.contains("unknownParam"));
        assert!(error.contains("services[2]"));
        assert!(error.contains("baudRate"));
    }
}
