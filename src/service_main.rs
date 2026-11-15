use crate::app;
use crate::command_line::CommandLine;
use crate::config;
use crate::logging::{self, ServiceLogLevel, ServiceLogger};
use hodeco_contract::config::Config;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

pub fn service_main(logger: ServiceLogger, cancel_token: CancellationToken) {
    let command_line = CommandLine::from_env();
    let loaded = match load_config(&command_line, logger) {
        Some(config) => config,
        None => return,
    };
    let config = loaded.config;
    let config_path = loaded.config_path;

    if let Err(error) = config::prepare_working_directory(&config) {
        logger(ServiceLogLevel::Error, &format!("{error}"));
        return;
    }

    let warnings = match config::prepare_runtime_directories(&config) {
        Ok(warnings) => warnings,
        Err(error) => {
            logger(ServiceLogLevel::Error, &format!("{error}"));
            return;
        }
    };

    if let Err(error) = logging::init(&config.log, logger) {
        logger(
            ServiceLogLevel::Error,
            &format!("Failed to initialize logging: {error}"),
        );
        return;
    }

    for warning in warnings {
        warn!("{warning}");
    }
    if let Some(path) = loaded.default_config_missing {
        info!(
            path = %path.display(),
            "configuration file not found; using defaults"
        );
    }

    info!(
        http = %format!("{}:{}", config.http_server.bind, config.http_server.port),
        services = config.services.len(),
        "configuration loaded"
    );
    info!("service started");

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("hodeco")
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            error!(error = %error, "failed to initialize service runtime");
            logger(
                ServiceLogLevel::Error,
                &format!("Failed to initialize service runtime: {error}"),
            );
            logging::shutdown();
            return;
        }
    };

    match runtime.block_on(app::run(config, config_path, cancel_token)) {
        Ok(()) => info!("service stopped"),
        Err(error) => {
            error!(error = %error, "service failed");
            logger(ServiceLogLevel::Error, &format!("Service failed: {error}"));
        }
    }

    logging::shutdown();
}

struct LoadedConfig {
    config: Config,
    config_path: PathBuf,
    default_config_missing: Option<PathBuf>,
}

fn load_config(command_line: &CommandLine, logger: ServiceLogger) -> Option<LoadedConfig> {
    match command_line.options.config_file.as_deref() {
        Some(path) => {
            let path = match config::absolute_path(Path::new(path)) {
                Ok(path) => path,
                Err(error) => {
                    logger(
                        ServiceLogLevel::Error,
                        &format!("Failed to resolve configuration path '{}': {error}", path),
                    );
                    return None;
                }
            };
            match config::load(&path) {
                Ok(config) => Some(LoadedConfig {
                    config,
                    config_path: path,
                    default_config_missing: None,
                }),
                Err(error) => {
                    logger(ServiceLogLevel::Error, &format!("{error}"));
                    None
                }
            }
        }
        None => {
            let path = config::default_path();
            let path = match config::absolute_path(&path) {
                Ok(path) => path,
                Err(error) => {
                    logger(
                        ServiceLogLevel::Error,
                        &format!(
                            "Failed to resolve configuration path '{}': {error}",
                            path.display()
                        ),
                    );
                    return None;
                }
            };
            if path.exists() {
                match config::load(&path) {
                    Ok(config) => Some(LoadedConfig {
                        config,
                        config_path: path,
                        default_config_missing: None,
                    }),
                    Err(error) => {
                        logger(ServiceLogLevel::Error, &format!("{error}"));
                        None
                    }
                }
            } else {
                let config = Config::default();
                if let Err(error) = config::validate(&config) {
                    logger(ServiceLogLevel::Error, &format!("{error}"));
                    return None;
                }
                Some(LoadedConfig {
                    config,
                    config_path: path.clone(),
                    default_config_missing: Some(path),
                })
            }
        }
    }
}
