use crate::automation::engine;
use crate::automation::feed::RunFeed;
use crate::automation::history::HistoryStore;
use crate::automation::runtime::TaskRuntime;
use crate::automation::scheduler;
use crate::automation::store::{AutomationSnapshot, AutomationStore};
use crate::config::{ConfigBundle, ConfigStore, ServicesStore, catalog_path};
use crate::events::{self, Event};
use crate::http;
use crate::messages::{MessageStore, persist};
use crate::services::StatusEvent;
use crate::services::discovery::DiscoveryRegistry;
use crate::services::registry::ServiceRegistry;
use chrono::{DateTime, Utc};
use hodeco_contract::config::{Config, CorruptPolicy, MessageLogConfig};
use hodeco_contract::domain::{ServiceKey, ServiceRef};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, broadcast};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const RESTART_DELAY: Duration = Duration::from_millis(100);
const MESSAGE_LOG_QUEUE: usize = 4096;

pub type AppError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Why the application runtime stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    Stopped,
    Restart,
}

/// Shared application services used by the HTTP/WebSocket API.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub config_store: Arc<ConfigStore>,
    pub services_store: Arc<ServicesStore>,
    pub automation: Arc<AutomationStore>,
    pub runtime: Arc<TaskRuntime>,
    pub history: Arc<HistoryStore>,
    pub run_feed: Arc<RunFeed>,
    pub registry: Arc<ServiceRegistry>,
    pub discovery: Arc<DiscoveryRegistry>,
    pub store: Arc<MessageStore>,
    pub events: broadcast::Sender<Event>,
    pub resync: broadcast::Sender<()>,
    pub cancel: CancellationToken,
    pub shutting_down: Arc<AtomicBool>,
    pub restart_requested: Arc<AtomicBool>,
    pub ws_limits: Arc<Semaphore>,
    pub started_at: DateTime<Utc>,
    /// System IANA time zone, resolved once at startup for `schedules.list`.
    pub system_timezone: Option<String>,
}

impl AppState {
    /// Asks the application to restart after giving the current response time
    /// to flush. The caller decides how to start a new process.
    pub fn request_restart(&self) {
        self.restart_requested.store(true, Ordering::SeqCst);
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
    writer: persist::LogHandle,
    writer_task: JoinHandle<()>,
}

impl Gateway {
    async fn run(self, cancel: CancellationToken) -> Result<(), AppError> {
        let Gateway {
            state,
            mut status_events,
            writer,
            writer_task,
        } = self;

        let mut store_events = state.store.subscribe();
        let mut discovery_events = state.discovery.subscribe();
        let mut flush = tokio::time::interval(state.config.message_log.auto_persist.as_std());
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        flush.tick().await;
        let mut last_compact = Instant::now();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = flush.tick() => {
                    if let Err(error) = writer.flush().await {
                        warn!(error = %error, "unable to flush the message log");
                    }
                    maybe_compact_messages(&state, &writer, &mut last_compact).await;
                }
                event = store_events.recv() => {
                    match event {
                        Ok(event) => {
                            if let Err(error) = writer.append(event.clone()).await {
                                warn!(error = %error, "unable to append to the message log");
                            }
                            engine::handle_store_event(&state, &event);
                            let _ = state.events.send(Event::from(event));
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "message event stream lagged; requesting client resync");
                            let _ = state.resync.send(());
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            warn!("message event stream closed");
                            break;
                        }
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
                        Err(broadcast::error::RecvError::Closed) => {
                            warn!("discovery event stream closed");
                            break;
                        }
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
                        Err(broadcast::error::RecvError::Closed) => {
                            warn!("service status event stream closed");
                            break;
                        }
                    }
                }
            }
        }

        if let Err(error) = writer.shutdown().await {
            warn!(error = %error, "unable to flush the message log during shutdown");
        }
        if let Err(error) = writer_task.await {
            warn!(error = %error, "message-log writer task failed");
        }
        state.registry.shutdown().await;
        state.discovery.shutdown().await;
        persist_messages(&state.config.message_log, &state.store).await;
        Ok(())
    }
}

async fn maybe_compact_messages(
    state: &AppState,
    writer: &persist::LogHandle,
    last_compact: &mut Instant,
) {
    let config = &state.config.message_log;
    let threshold = (config.max_size.as_bytes() as f64 * config.compaction_factor.max(1.0)) as u64;
    let by_size = writer.size() > threshold;
    let by_age = last_compact.elapsed() >= config.compact_interval.as_std();
    if !by_size && !by_age {
        return;
    }
    match writer
        .compact(
            config.file.clone(),
            Some(config.file_mode),
            config.on_corrupt,
            state.store.clone(),
        )
        .await
    {
        Ok(()) => {
            *last_compact = Instant::now();
            info!("message log compacted");
        }
        Err(error) => warn!(error = %error, "unable to compact the message log"),
    }
}

pub async fn run(
    bundle: ConfigBundle,
    config_path: PathBuf,
    cancel_token: CancellationToken,
) -> Result<ShutdownReason, AppError> {
    let ConfigBundle {
        config,
        services,
        tasks,
        schedules,
        commands,
    } = bundle;
    let paths = catalog_path(&config_path, &config);
    let config_store = Arc::new(ConfigStore::new(Some(config_path.clone()), config.clone()));
    let services_store = Arc::new(ServicesStore::new(
        Some(paths.services.clone()),
        services.services,
    ));
    let automation_snapshot = AutomationSnapshot {
        tasks: tasks
            .tasks
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect::<BTreeMap<_, _>>(),
        schedules: schedules
            .schedules
            .into_iter()
            .map(|schedule| (schedule.id.clone(), schedule))
            .collect::<BTreeMap<_, _>>(),
        commands: commands
            .commands
            .into_iter()
            .map(|command| (command.id.clone(), command))
            .collect::<BTreeMap<_, _>>(),
        location: schedules.location,
        timezone: schedules.timezone,
    };
    let automation = Arc::new(AutomationStore::new(automation_snapshot, paths));
    let config = Arc::new(config);
    let store = Arc::new(MessageStore::new(config.message_log.max_size.as_bytes()));
    let message_log = config.message_log.clone();
    let load_store = store.clone();
    tokio::task::spawn_blocking(move || load_message_log(&message_log, &load_store))
        .await
        .map_err(|error| -> AppError { Box::new(error) })??;
    let writer_path = config.message_log.file.clone();
    let writer_mode = Some(config.message_log.file_mode);
    let writer_corrupt = config.message_log.on_corrupt;
    let writer = tokio::task::spawn_blocking(move || {
        persist::LogWriter::open(&writer_path, writer_mode, writer_corrupt)
    })
    .await
    .map_err(|error| -> AppError { Box::new(error) })?
    .map_err(|error| -> AppError { Box::new(error) })?;
    let (writer, writer_task) = persist::LogHandle::spawn(writer, MESSAGE_LOG_QUEUE);

    let history = Arc::new(HistoryStore::new(
        config.task_log.clone(),
        config.task_log.dir.clone(),
    ));
    let task_history: BTreeMap<String, hodeco_contract::domain::TaskHistoryConfig> = automation
        .snapshot()
        .tasks
        .values()
        .map(|task| (task.id.clone(), task.history.clone()))
        .collect();
    let load_history = history.clone();
    let report = tokio::task::spawn_blocking(move || load_history.load_with_history(&task_history))
        .await
        .map_err(|error| -> AppError { Box::new(error) })?
        .map_err(|error| -> AppError { Box::new(error) })?;
    if report.loaded > 0 {
        info!(rows = report.loaded, "task history loaded");
    }
    for path in &report.orphans_removed {
        warn!(path = %path.display(), "removed orphan task history file");
    }
    for (path, error) in &report.corrupt {
        warn!(path = %path.display(), error = %error, "ignored corrupt task history file");
    }

    let (events, _) = events::channel();
    let (resync, _) = broadcast::channel(1);
    let registry = Arc::new(ServiceRegistry::with_defaults(store.clone(), &cancel_token));
    let status_events = registry.subscribe_status();
    let discovery = Arc::new(DiscoveryRegistry::with_defaults(&cancel_token));
    let run_feed = Arc::new(RunFeed::new(&config.task_engine.run_feed));
    let runtime = Arc::new(TaskRuntime::with_shutdown(
        config.task_engine.max_concurrent_runs,
        &cancel_token,
    ));
    runtime.seed_history_time(history.latest_time());
    let ws_limit = if config.http_server.max_connections == 0 {
        Semaphore::MAX_PERMITS
    } else {
        config.http_server.max_connections as usize
    };
    let ws_limits = Arc::new(Semaphore::new(ws_limit));
    let system_timezone = tokio::task::spawn_blocking(hodeco_schedule::system_timezone)
        .await
        .ok()
        .and_then(Result::ok)
        .map(|timezone| timezone.name().to_string());
    if system_timezone.is_none() {
        warn!("unable to determine the system timezone; set schedules.timezone");
    }
    let state = AppState {
        config: config.clone(),
        config_store,
        services_store: services_store.clone(),
        automation,
        runtime,
        history,
        run_feed,
        registry: registry.clone(),
        discovery: discovery.clone(),
        store: store.clone(),
        events,
        resync,
        cancel: cancel_token.clone(),
        shutting_down: Arc::new(AtomicBool::new(false)),
        restart_requested: Arc::new(AtomicBool::new(false)),
        ws_limits,
        started_at: Utc::now(),
        system_timezone,
    };

    for service in &services_store.snapshot() {
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
            writer,
            writer_task,
        }
        .run(gateway_cancel.clone()),
    );

    let scheduler_cancel = cancel_token.child_token();
    let mut scheduler = tokio::spawn(scheduler::run(state.clone(), scheduler_cancel.clone()));

    let http_cancel = cancel_token.child_token();
    let mut http = tokio::spawn(http::serve(state.clone(), http_cancel.clone()));

    let mut gateway_done = false;
    let mut http_done = false;
    let mut scheduler_done = false;
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
        result = &mut scheduler => {
            scheduler_done = true;
            early_error = Some(match result {
                Ok(()) => "scheduler stopped unexpectedly".to_string().into(),
                Err(error) => Box::new(error),
            });
        }
    }

    state.shutting_down.store(true, Ordering::SeqCst);
    state.runtime.cancel_runs();
    http_cancel.cancel();
    gateway_cancel.cancel();
    scheduler_cancel.cancel();

    if !http_done {
        shutdown_task("http server", &mut http).await;
    }
    if !gateway_done {
        shutdown_task("gateway", &mut gateway).await;
    }
    if !scheduler_done {
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut scheduler).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(error = %error, "scheduler task failed during shutdown"),
            Err(_) => {
                warn!("scheduler shutdown timed out");
                scheduler.abort();
            }
        }
    }

    state.runtime.wait_for_runs(SHUTDOWN_TIMEOUT).await;

    if cancel_token.is_cancelled() {
        Ok(if state.restart_requested.load(Ordering::SeqCst) {
            ShutdownReason::Restart
        } else {
            ShutdownReason::Stopped
        })
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
