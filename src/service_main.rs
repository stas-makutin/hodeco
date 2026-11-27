use crate::app::{self, ShutdownReason};
use crate::command_line::CommandLine;
use crate::config;
use crate::config::ConfigBundle;
use crate::logging::{self, ServiceLogLevel, ServiceLogger};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Result of one gateway process lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceOutcome {
    Stopped,
    Restarting,
    Failed,
}

impl ServiceOutcome {
    pub fn succeeded(self) -> bool {
        !matches!(self, Self::Failed)
    }
}

/// Runs the gateway from process startup through shutdown. A failed load,
/// runtime setup, or gateway run returns [`ServiceOutcome::Failed`]; a requested
/// restart returns [`ServiceOutcome::Restarting`] so the caller can start a new
/// process.
pub fn service_main(logger: ServiceLogger, cancel_token: CancellationToken) -> ServiceOutcome {
    let command_line = CommandLine::from_env();
    let loaded = match load_config(&command_line, logger) {
        Some(config) => config,
        None => return ServiceOutcome::Failed,
    };
    let config = loaded.bundle.config.clone();
    let services_count = loaded.bundle.services.services.len();
    let config_path = loaded.config_path.clone();

    if let Err(error) = config::prepare_working_directory(&config) {
        logger(ServiceLogLevel::Error, &format!("{error}"));
        return ServiceOutcome::Failed;
    }

    let warnings = match config::prepare_runtime_directories(&config) {
        Ok(warnings) => warnings,
        Err(error) => {
            logger(ServiceLogLevel::Error, &format!("{error}"));
            return ServiceOutcome::Failed;
        }
    };

    if let Err(error) = logging::init(&config.log, logger) {
        logger(
            ServiceLogLevel::Error,
            &format!("Failed to initialize logging: {error}"),
        );
        return ServiceOutcome::Failed;
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
        services = services_count,
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
            return ServiceOutcome::Failed;
        }
    };

    let shutdown = cancel_token.clone();
    let outcome = match runtime.block_on(async move {
        install_signal_handlers(cancel_token);
        app::run(loaded.bundle, config_path, shutdown).await
    }) {
        Ok(ShutdownReason::Stopped) => {
            info!("service stopped");
            ServiceOutcome::Stopped
        }
        Ok(ShutdownReason::Restart) => {
            info!("service restarting");
            ServiceOutcome::Restarting
        }
        Err(error) => {
            error!(error = %error, "service failed");
            logger(ServiceLogLevel::Error, &format!("Service failed: {error}"));
            ServiceOutcome::Failed
        }
    };

    logging::shutdown();
    outcome
}

/// Installs console and SIGTERM handling on the service runtime. This keeps
/// signal delivery on Tokio instead of dedicating another OS thread to it.
fn install_signal_handlers(cancel: CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut terminate =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(signal) => signal,
                    Err(error) => {
                        warn!(error = %error, "failed to install SIGTERM handler");
                        return;
                    }
                };

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            if let Err(error) = tokio::signal::ctrl_c().await {
                warn!(error = %error, "failed to install Ctrl+C handler");
                return;
            }
        }

        info!("shutdown signal received");
        cancel.cancel();
    });
}

struct LoadedConfig {
    bundle: ConfigBundle,
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
            match config::load_bundle(&path) {
                Ok(bundle) => Some(LoadedConfig {
                    bundle,
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
                match config::load_bundle(&path) {
                    Ok(bundle) => Some(LoadedConfig {
                        bundle,
                        config_path: path,
                        default_config_missing: None,
                    }),
                    Err(error) => {
                        logger(ServiceLogLevel::Error, &format!("{error}"));
                        None
                    }
                }
            } else {
                let config = hodeco_contract::config::Config::default();
                if let Err(error) = config::validate(&config) {
                    logger(ServiceLogLevel::Error, &format!("{error}"));
                    return None;
                }
                Some(LoadedConfig {
                    bundle: ConfigBundle {
                        config,
                        services: hodeco_contract::config::ServicesFile::default(),
                        tasks: hodeco_contract::config::TasksFile::default(),
                        schedules: hodeco_contract::config::SchedulesFile::default(),
                        commands: hodeco_contract::config::CommandsFile::default(),
                    },
                    config_path: path.clone(),
                    default_config_missing: Some(path),
                })
            }
        }
    }
}
