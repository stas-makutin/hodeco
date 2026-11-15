use crate::config::ConfigStore;
use crate::events::{self, Event};
use crate::http;
use crate::messages::{MessageStore, persist};
use crate::services::discovery::DiscoveryRegistry;
use crate::services::registry::ServiceRegistry;
use crate::services::zwave::StatusEvent;
use chrono::{DateTime, Utc};
use hodeco_contract::config::{Config, CorruptPolicy, MessageLogConfig};
use hodeco_contract::domain::{ServiceKey, ServiceRef};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Semaphore, broadcast};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const RESTART_DELAY: Duration = Duration::from_millis(100);

pub type AppError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Shared application services used by the HTTP/WebSocket API.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub config_store: Arc<ConfigStore>,
    pub registry: Arc<ServiceRegistry>,
    pub discovery: Arc<DiscoveryRegistry>,
    pub store: Arc<MessageStore>,
    pub events: broadcast::Sender<Event>,
    pub resync: broadcast::Sender<()>,
    pub cancel: CancellationToken,
    pub shutting_down: Arc<AtomicBool>,
    pub ws_limits: Arc<Semaphore>,
    pub started_at: DateTime<Utc>,
}

impl AppState {
    /// Asks the application to stop after giving the current response time to flush.
    pub fn request_restart(&self) {
        let cancel = self.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RESTART_DELAY).await;
            cancel.cancel();
        });
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }
}

struct Gateway {
    state: AppState,
    status_events: broadcast::Receiver<StatusEvent>,
}

impl Gateway {
    async fn run(self, cancel: CancellationToken) -> Result<(), AppError> {
        let Gateway {
            state,
            mut status_events,
        } = self;

        let mut store_events = state.store.subscribe();
        let mut discovery_events = state.discovery.subscribe();
        let persistence = tokio::spawn(auto_persist(
            state.config.message_log.clone(),
            state.store.clone(),
            cancel.clone(),
        ));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                event = store_events.recv() => {
                    match event {
                        Ok(event) => {
                            let _ = state.events.send(Event::from(event));
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "message event stream lagged; requesting client resync");
                            let _ = state.resync.send(());
                        }
                        Err(broadcast::error::RecvError::Closed) => {}
                    }
                }
                event = discovery_events.recv() => {
                    match event {
                        Ok(event) => {
                            let _ = state.events.send(Event::from(event));
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "discovery event stream lagged; requesting client resync");
                            let _ = state.resync.send(());
                        }
                        Err(broadcast::error::RecvError::Closed) => {}
                    }
                }
                event = status_events.recv() => {
                    match event {
                        Ok((service, status)) => {
                            let _ = state.events.send(Event::ServiceStateChanged {
                                service: ServiceRef::from(service),
                                status,
                            });
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "service status event stream lagged; requesting client resync");
                            let _ = state.resync.send(());
                        }
                        Err(broadcast::error::RecvError::Closed) => {}
                    }
                }
            }
        }

        // Wait for an in-flight automatic save before shutting services down and
        // writing the final snapshot, so two writers never target the log file.
        let _ = persistence.await;
        state.registry.shutdown().await;
        state.discovery.shutdown().await;
        persist_messages(&state.config.message_log, &state.store).await;
        Ok(())
    }
}

async fn auto_persist(
    message_log: MessageLogConfig,
    store: Arc<MessageStore>,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(message_log.auto_persist.as_std());
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => persist_messages(&message_log, &store).await,
        }
    }
}

pub async fn run(
    config: Config,
    config_path: PathBuf,
    cancel_token: CancellationToken,
) -> Result<(), AppError> {
    let config_store = Arc::new(ConfigStore::new(Some(config_path), config.clone()));
    let config = Arc::new(config);
    let store = Arc::new(MessageStore::new(config.message_log.max_size.as_bytes()));
    let message_log = config.message_log.clone();
    let load_store = store.clone();
    tokio::task::spawn_blocking(move || load_message_log(&message_log, &load_store))
        .await
        .map_err(|error| -> AppError { Box::new(error) })??;
    let (events, _) = events::channel();
    let (resync, _) = broadcast::channel(1);
    let registry = Arc::new(ServiceRegistry::with_defaults(store.clone(), &cancel_token));
    let status_events = registry.subscribe_status();
    let discovery = Arc::new(DiscoveryRegistry::with_defaults(&cancel_token));
    let ws_limits = Arc::new(Semaphore::new(
        config.http_server.max_connections.max(1) as usize
    ));
    let state = AppState {
        config: config.clone(),
        config_store,
        registry: registry.clone(),
        discovery: discovery.clone(),
        store: store.clone(),
        events,
        resync,
        cancel: cancel_token.clone(),
        shutting_down: Arc::new(AtomicBool::new(false)),
        ws_limits,
        started_at: Utc::now(),
    };

    for service in &config.services {
        let key = ServiceKey {
            protocol: service.protocol.clone(),
            transport: service.transport.clone(),
            entry: service.entry.clone(),
        };
        match registry.add(key, &service.params, service.alias.clone()) {
            Ok(info) => info!(
                protocol = %info.protocol,
                transport = %info.transport,
                entry = %info.entry,
                alias = ?info.alias,
                "service registered"
            ),
            Err(error) => warn!(
                protocol = %service.protocol,
                transport = %service.transport,
                entry = %service.entry,
                error = %error,
                "unable to register service"
            ),
        }
    }

    let gateway_cancel = cancel_token.child_token();
    let mut gateway = tokio::spawn(
        Gateway {
            state: state.clone(),
            status_events,
        }
        .run(gateway_cancel.clone()),
    );

    let http_cancel = cancel_token.child_token();
    let mut http = tokio::spawn(http::serve(state.clone(), http_cancel.clone()));

    let mut gateway_done = false;
    let mut http_done = false;
    let mut early_error = None;

    tokio::select! {
        _ = cancel_token.cancelled() => {}
        result = &mut gateway => {
            gateway_done = true;
            early_error = Some(task_error("gateway", result));
        }
        result = &mut http => {
            http_done = true;
            early_error = Some(task_error("http server", result));
        }
    }

    state.shutting_down.store(true, Ordering::SeqCst);
    http_cancel.cancel();
    gateway_cancel.cancel();

    if !http_done {
        shutdown_task("http server", &mut http).await;
    }
    if !gateway_done {
        shutdown_task("gateway", &mut gateway).await;
    }

    if cancel_token.is_cancelled() {
        Ok(())
    } else {
        Err(early_error.unwrap_or_else(|| "application stopped unexpectedly".to_string().into()))
    }
}

fn task_error(
    name: &str,
    result: Result<Result<(), AppError>, tokio::task::JoinError>,
) -> AppError {
    match result {
        Ok(Ok(())) => format!("{name} stopped unexpectedly").into(),
        Ok(Err(error)) => error,
        Err(error) => Box::new(error),
    }
}

async fn shutdown_task(name: &str, task: &mut JoinHandle<Result<(), AppError>>) {
    match tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut *task).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => error!(error = %error, "{name} failed during shutdown"),
        Ok(Err(error)) => error!(error = %error, "{name} task failed during shutdown"),
        Err(_) => {
            warn!("{name} shutdown timed out");
            task.abort();
        }
    }
}

async fn persist_messages(message_log: &MessageLogConfig, store: &Arc<MessageStore>) {
    let path = message_log.file.clone();
    let file_mode = message_log.file_mode;
    let store = store.clone();

    match tokio::task::spawn_blocking(move || persist::save(&path, &store, Some(file_mode))).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(error = %error, "unable to persist messages"),
        Err(error) => warn!(error = %error, "message persistence task failed"),
    }
}

fn load_message_log(message_log: &MessageLogConfig, store: &MessageStore) -> Result<(), AppError> {
    match persist::load(&message_log.file) {
        Ok(None) => Ok(()),
        Ok(Some(log)) => {
            let messages = log.entries.len();
            store.replace_all(log.entries, log.bytes);
            info!(
                path = %message_log.file.display(),
                messages,
                "message log loaded"
            );
            Ok(())
        }
        Err(error) => match message_log.on_corrupt {
            CorruptPolicy::Ignore => {
                warn!(
                    path = %message_log.file.display(),
                    error = %error,
                    "ignoring corrupt message log"
                );
                Ok(())
            }
            CorruptPolicy::Fail => Err(Box::new(error)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodeco_contract::domain::MessageState;
    use std::path::Path;
    use uuid::Uuid;

    struct TestLog(std::path::PathBuf);

    impl TestLog {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("hodeco-service-{}.hlog", Uuid::new_v4())))
        }
    }

    impl Drop for TestLog {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn config(path: &Path) -> Config {
        Config {
            message_log: MessageLogConfig {
                file: path.to_path_buf(),
                ..MessageLogConfig::default()
            },
            ..Config::default()
        }
    }

    #[test]
    fn loads_and_restores_persisted_messages() {
        let log = TestLog::new();
        let config = config(&log.0);
        let source = MessageStore::new(config.message_log.max_size.as_bytes());
        let key = ServiceKey {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: "COM3".to_string(),
        };
        source.register(&key, &[1, 2, 3], MessageState::Incoming);
        persist::save(&log.0, &source, None).unwrap();

        let store = MessageStore::new(config.message_log.max_size.as_bytes());
        load_message_log(&config.message_log, &store).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.bytes(), source.bytes());
    }

    #[test]
    fn applies_the_configured_corrupt_log_policy() {
        let log = TestLog::new();
        std::fs::write(&log.0, b"not a message log").unwrap();
        let mut config = config(&log.0);

        let store = MessageStore::new(config.message_log.max_size.as_bytes());
        load_message_log(&config.message_log, &store).unwrap();
        assert!(store.is_empty());

        config.message_log.on_corrupt = CorruptPolicy::Fail;
        assert!(load_message_log(&config.message_log, &store).is_err());
    }
}
