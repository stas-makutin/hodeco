use super::ServiceError;
use super::catalog;
use super::serial::{
    PortDescriptor, PortEnumerator, PortOpener, SerialSettings, SystemPortEnumerator,
    SystemPortOpener,
};
use super::zwave::probe_version;
use futures_util::StreamExt;
use hodeco_contract::domain::{DiscoveryEntry, DiscoveryInfo, DiscoveryState, ParamValues};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const DISCOVERY_MAX_COUNT: usize = 10;
pub const DISCOVERY_MAX_ACTIVE: usize = 3;
pub const DISCOVERY_PROBE_TIMEOUT: Duration = Duration::from_millis(1_500);
const EVENT_CHANNEL_CAPACITY: usize = 128;

#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    Started(DiscoveryInfo),
    Finished(DiscoveryInfo),
}

pub trait DiscoveryBackend: Send + Sync {
    fn discover(
        &self,
        protocol: String,
        transport: String,
        params: ParamValues,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DiscoveryEntry>, ServiceError>> + Send + 'static>>;
}

pub struct SerialDiscoveryBackend {
    opener: Arc<dyn PortOpener>,
    enumerator: Arc<dyn PortEnumerator>,
}

impl SerialDiscoveryBackend {
    pub fn new(opener: Arc<dyn PortOpener>, enumerator: Arc<dyn PortEnumerator>) -> Self {
        Self { opener, enumerator }
    }

    pub fn system() -> Self {
        Self::new(Arc::new(SystemPortOpener), Arc::new(SystemPortEnumerator))
    }
}

impl DiscoveryBackend for SerialDiscoveryBackend {
    fn discover(
        &self,
        _protocol: String,
        _transport: String,
        params: ParamValues,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DiscoveryEntry>, ServiceError>> + Send + 'static>>
    {
        let opener = self.opener.clone();
        let enumerator = self.enumerator.clone();
        Box::pin(async move {
            let ports = tokio::task::spawn_blocking(move || enumerator.ports())
                .await
                .map_err(|error| ServiceError::Transport(error.to_string()))?
                .map_err(|error| ServiceError::Transport(error.to_string()))?;

            // Reuse the configured serial line settings for probing. Read and
            // write timeouts stay disabled; the probe has its own deadline.
            let mut settings = SerialSettings::from_params(&params)?;
            settings.read_timeout = None;
            settings.write_timeout = None;

            let mut entries = Vec::with_capacity(ports.len());
            let mut probes = futures_util::stream::iter(
                ports
                    .into_iter()
                    .map(|port| probe_port(opener.as_ref(), port, &cancel, settings.clone())),
            )
            .buffered(4);

            while let Some(entry) = probes.next().await {
                entries.push(entry);
                if cancel.is_cancelled() {
                    break;
                }
            }
            Ok(entries)
        })
    }
}

async fn probe_port(
    opener: &dyn PortOpener,
    port: PortDescriptor,
    cancel: &CancellationToken,
    settings: SerialSettings,
) -> DiscoveryEntry {
    let result = tokio::select! {
        _ = cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "discovery cancelled")),
        result = tokio::time::timeout(
            DISCOVERY_PROBE_TIMEOUT,
            probe_port_version(opener, &port.name, settings),
        ) => {
            match result {
                Ok(result) => result,
                Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "probe timed out")),
            }
        }
    };

    match result {
        Ok(Some(version)) => DiscoveryEntry {
            entry: port.name,
            available: true,
            description: describe(port.product, &version),
            error: None,
        },
        Ok(None) => DiscoveryEntry {
            entry: port.name,
            available: false,
            description: describe(port.product, ""),
            error: Some("no Z-Wave version response".to_string()),
        },
        Err(error) => DiscoveryEntry {
            entry: port.name,
            available: false,
            description: describe(port.product, ""),
            error: Some(error.to_string()),
        },
    }
}

async fn probe_port_version(
    opener: &dyn PortOpener,
    entry: &str,
    settings: SerialSettings,
) -> io::Result<Option<String>> {
    let mut port = opener.open(entry.to_string(), settings).await?;
    probe_version(&mut port, DISCOVERY_PROBE_TIMEOUT).await
}

fn describe(product: Option<String>, version: &str) -> Option<String> {
    let version = version.trim();
    match (
        product.filter(|product| !product.is_empty()),
        version.is_empty(),
    ) {
        (Some(product), false) => Some(format!("{product} [{version}]")),
        (Some(product), true) => Some(product),
        (None, false) => Some(format!("[{version}]")),
        (None, true) => None,
    }
}

struct DiscoveryRecord {
    info: DiscoveryInfo,
    cancel: CancellationToken,
    join: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct DiscoveryInner {
    entries: HashMap<Uuid, DiscoveryRecord>,
    order: VecDeque<Uuid>,
    completed: VecDeque<Uuid>,
    active: usize,
}

pub struct DiscoveryRegistry {
    backend: Arc<dyn DiscoveryBackend>,
    events: broadcast::Sender<DiscoveryEvent>,
    cancel: CancellationToken,
    inner: Arc<Mutex<DiscoveryInner>>,
}

impl DiscoveryRegistry {
    pub fn new(backend: Arc<dyn DiscoveryBackend>, parent_cancel: &CancellationToken) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            backend,
            events,
            cancel: parent_cancel.child_token(),
            inner: Arc::new(Mutex::new(DiscoveryInner::default())),
        }
    }

    pub fn with_defaults(parent_cancel: &CancellationToken) -> Self {
        Self::new(Arc::new(SerialDiscoveryBackend::system()), parent_cancel)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DiscoveryEvent> {
        self.events.subscribe()
    }

    pub fn start(
        &self,
        protocol: &str,
        transport: &str,
        params: &ParamValues,
    ) -> Result<DiscoveryInfo, ServiceError> {
        let parsed = catalog::validate_service(protocol, transport, params)?;
        let id = Uuid::new_v4();
        let cancel = self.cancel.child_token();
        let info = DiscoveryInfo {
            id,
            protocol: protocol.to_string(),
            transport: transport.to_string(),
            state: DiscoveryState::Running,
            entries: Vec::new(),
        };

        {
            let mut inner = self.inner.lock();
            if inner.active >= DISCOVERY_MAX_ACTIVE {
                return Err(ServiceError::DiscoveryBusy);
            }
            while inner.entries.len() >= DISCOVERY_MAX_COUNT {
                let Some(evicted) = inner.completed.pop_front() else {
                    return Err(ServiceError::DiscoveryBusy);
                };
                if inner.entries.remove(&evicted).is_some() {
                    inner.order.retain(|candidate| *candidate != evicted);
                }
            }
            inner.entries.insert(
                id,
                DiscoveryRecord {
                    info: info.clone(),
                    cancel: cancel.clone(),
                    join: None,
                },
            );
            inner.order.push_back(id);
            inner.active += 1;
        }

        let _ = self.events.send(DiscoveryEvent::Started(info.clone()));

        let backend = self.backend.clone();
        let inner = self.inner.clone();
        let events = self.events.clone();
        let protocol = protocol.to_string();
        let transport = transport.to_string();
        let task_cancel = cancel.clone();

        let join = tokio::spawn(async move {
            let result = backend
                .discover(protocol, transport, parsed, task_cancel.clone())
                .await;
            let cancelled = task_cancel.is_cancelled();
            let (state, entries) = if cancelled {
                (DiscoveryState::Cancelled, Vec::new())
            } else {
                match result {
                    Ok(entries) => (DiscoveryState::Finished, entries),
                    Err(error) => {
                        tracing::warn!(error = %error, "service discovery failed");
                        (DiscoveryState::Failed, Vec::new())
                    }
                }
            };

            let mut inner = inner.lock();
            let info = {
                let Some(record) = inner.entries.get_mut(&id) else {
                    return;
                };
                record.info.state = state;
                record.info.entries = entries;
                record.info.clone()
            };
            inner.active = inner.active.saturating_sub(1);
            inner.completed.push_back(id);
            drop(inner);

            let _ = events.send(DiscoveryEvent::Finished(info));
        });

        if let Some(record) = self.inner.lock().entries.get_mut(&id) {
            record.join = Some(join);
        }

        Ok(info)
    }

    pub fn get(&self, id: Option<Uuid>) -> Result<DiscoveryInfo, ServiceError> {
        let inner = self.inner.lock();
        let id = match id {
            Some(id) => id,
            None => *inner.order.back().ok_or(ServiceError::DiscoveryNotFound)?,
        };
        inner
            .entries
            .get(&id)
            .map(|record| record.info.clone())
            .ok_or(ServiceError::DiscoveryNotFound)
    }

    pub fn cancel(&self, id: Uuid) -> Result<(), ServiceError> {
        let mut inner = self.inner.lock();
        let record = inner
            .entries
            .get_mut(&id)
            .ok_or(ServiceError::DiscoveryNotFound)?;
        if record.info.state == DiscoveryState::Running {
            record.info.state = DiscoveryState::Cancelled;
            record.cancel.cancel();
        }
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.cancel.cancel();
        let joins: Vec<_> = {
            let mut inner = self.inner.lock();
            inner
                .entries
                .values_mut()
                .filter_map(|record| record.join.take())
                .collect()
        };
        for join in joins {
            let _ = join.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::serial::Port;
    use crate::services::test_support::MockPortOpener;
    use crate::services::zwave::{FRAME_ACK, ZW_VERSION, data_response, version_request};
    use serde_json::json;
    use std::io;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    struct MockBackend {
        delay: Duration,
        entries: Vec<DiscoveryEntry>,
    }

    impl MockBackend {
        fn new(delay: Duration, entries: Vec<DiscoveryEntry>) -> Arc<Self> {
            Arc::new(Self { delay, entries })
        }
    }

    impl DiscoveryBackend for MockBackend {
        fn discover(
            &self,
            _protocol: String,
            _transport: String,
            _params: ParamValues,
            cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<DiscoveryEntry>, ServiceError>> + Send>>
        {
            let delay = self.delay;
            let entries = self.entries.clone();
            Box::pin(async move {
                if !delay.is_zero() {
                    tokio::select! {
                        _ = cancel.cancelled() => {}
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
                if cancel.is_cancelled() {
                    Ok(Vec::new())
                } else {
                    Ok(entries)
                }
            })
        }
    }

    fn entry(name: &str, available: bool) -> DiscoveryEntry {
        DiscoveryEntry {
            entry: name.to_string(),
            available,
            description: None,
            error: None,
        }
    }

    async fn wait_state(
        registry: &DiscoveryRegistry,
        id: Uuid,
        state: DiscoveryState,
    ) -> DiscoveryInfo {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let info = registry.get(Some(id)).unwrap();
                if info.state == state {
                    break info;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("discovery did not reach the expected state")
    }

    fn registry(backend: Arc<dyn DiscoveryBackend>) -> (DiscoveryRegistry, CancellationToken) {
        let cancel = CancellationToken::new();
        (DiscoveryRegistry::new(backend, &cancel), cancel)
    }

    #[tokio::test]
    async fn starts_and_completes_discoveries() {
        let backend = MockBackend::new(
            Duration::from_millis(5),
            vec![entry("COM3", true), entry("COM4", false)],
        );
        let (registry, cancel) = registry(backend);
        let mut events = registry.subscribe();

        let info = registry
            .start("zwave", "serial", &ParamValues::new())
            .unwrap();
        assert_eq!(info.state, DiscoveryState::Running);
        assert!(matches!(
            events.try_recv().unwrap(),
            DiscoveryEvent::Started(_)
        ));

        let done = wait_state(&registry, info.id, DiscoveryState::Finished).await;
        assert_eq!(
            done.entries,
            vec![entry("COM3", true), entry("COM4", false)]
        );
        assert!(matches!(
            events.try_recv().unwrap(),
            DiscoveryEvent::Finished(_)
        ));

        assert_eq!(registry.get(None).unwrap().id, info.id);
        registry.shutdown().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn limits_active_discoveries() {
        let backend = MockBackend::new(Duration::from_secs(60), Vec::new());
        let (registry, cancel) = registry(backend);

        let mut ids = Vec::new();
        for _ in 0..DISCOVERY_MAX_ACTIVE {
            ids.push(
                registry
                    .start("zwave", "serial", &ParamValues::new())
                    .unwrap()
                    .id,
            );
        }
        assert!(matches!(
            registry.start("zwave", "serial", &ParamValues::new()),
            Err(ServiceError::DiscoveryBusy)
        ));

        for id in ids {
            registry.cancel(id).unwrap();
            wait_state(&registry, id, DiscoveryState::Cancelled).await;
        }

        registry.shutdown().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn keeps_at_most_the_configured_number_of_results() {
        let backend = MockBackend::new(Duration::ZERO, Vec::new());
        let (registry, cancel) = registry(backend);

        let mut ids = Vec::new();
        for _ in 0..=DISCOVERY_MAX_COUNT {
            let id = registry
                .start("zwave", "serial", &ParamValues::new())
                .unwrap()
                .id;
            wait_state(&registry, id, DiscoveryState::Finished).await;
            ids.push(id);
        }

        assert!(matches!(
            registry.get(Some(ids[0])),
            Err(ServiceError::DiscoveryNotFound)
        ));
        for id in &ids[1..] {
            assert_eq!(
                registry.get(Some(*id)).unwrap().state,
                DiscoveryState::Finished
            );
        }

        registry.shutdown().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn cancels_running_discoveries() {
        let backend = MockBackend::new(Duration::from_secs(60), Vec::new());
        let (registry, cancel) = registry(backend);
        let info = registry
            .start("zwave", "serial", &ParamValues::new())
            .unwrap();

        registry.cancel(info.id).unwrap();
        let cancelled = wait_state(&registry, info.id, DiscoveryState::Cancelled).await;
        assert!(cancelled.entries.is_empty());

        registry.cancel(info.id).unwrap();
        assert!(matches!(
            registry.cancel(Uuid::new_v4()),
            Err(ServiceError::DiscoveryNotFound)
        ));
        assert!(matches!(
            registry.get(Some(Uuid::new_v4())),
            Err(ServiceError::DiscoveryNotFound)
        ));

        registry.shutdown().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn rejects_unknown_protocols_and_transports() {
        let (registry, cancel) = registry(MockBackend::new(Duration::ZERO, Vec::new()));
        assert!(matches!(
            registry.start("unknown", "serial", &ParamValues::new()),
            Err(ServiceError::UnknownProtocol(_))
        ));
        assert!(matches!(
            registry.start("zwave", "unknown", &ParamValues::new()),
            Err(ServiceError::UnknownTransport(_))
        ));
        registry.shutdown().await;
        cancel.cancel();
    }

    struct MockEnumerator(Vec<PortDescriptor>);

    impl PortEnumerator for MockEnumerator {
        fn ports(&self) -> io::Result<Vec<PortDescriptor>> {
            Ok(self.0.clone())
        }
    }

    fn port(name: &str, product: Option<&str>) -> PortDescriptor {
        PortDescriptor {
            name: name.to_string(),
            product: product.map(str::to_string),
            manufacturer: None,
            serial_number: None,
        }
    }

    #[tokio::test]
    async fn probes_serial_ports() {
        let (device, mut host) = tokio::io::duplex(512);
        let responder = tokio::spawn(async move {
            let mut request = [0_u8; 32];
            let count = host.read(&mut request).await.unwrap();
            assert_eq!(&request[..count], version_request().as_slice());
            let payload = [&[ZW_VERSION][..], b"Z-Wave v4.39\0"].concat();
            let mut reply = vec![FRAME_ACK];
            reply.extend_from_slice(&data_response(&payload));
            host.write_all(&reply).await.unwrap();
        });

        let opener = MockPortOpener::new(vec![
            Ok(Box::new(device) as Port),
            Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
        ]);
        let backend = SerialDiscoveryBackend::new(
            opener,
            Arc::new(MockEnumerator(vec![
                port("COM3", Some("USB Stick")),
                port("COM4", None),
            ])),
        );

        let entries = backend
            .discover(
                "zwave".to_string(),
                "serial".to_string(),
                ParamValues::new(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries[0].available);
        assert_eq!(entries[0].entry, "COM3");
        assert_eq!(
            entries[0].description.as_deref(),
            Some("USB Stick [Z-Wave v4.39]")
        );
        assert!(entries[0].error.is_none());
        assert!(!entries[1].available);
        assert!(entries[1].error.is_some());
        responder.await.unwrap();
    }

    struct RecordingOpener(Arc<std::sync::Mutex<Option<SerialSettings>>>);

    impl PortOpener for RecordingOpener {
        fn open(
            &self,
            _entry: String,
            settings: SerialSettings,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<Port>> + Send + 'static>>
        {
            *self.0.lock().unwrap() = Some(settings);
            Box::pin(async { Err(io::Error::new(io::ErrorKind::NotFound, "no mock port")) })
        }
    }

    #[tokio::test]
    async fn probes_with_the_requested_serial_settings() {
        let settings = Arc::new(std::sync::Mutex::new(None));
        let backend = SerialDiscoveryBackend::new(
            Arc::new(RecordingOpener(settings.clone())),
            Arc::new(MockEnumerator(vec![port("COM3", None)])),
        );
        let params = ParamValues::from([
            ("baudRate".to_string(), json!(57_600)),
            ("dataBits".to_string(), json!("7")),
            ("parity".to_string(), json!("even")),
            ("stopBits".to_string(), json!("2")),
        ]);

        let entries = backend
            .discover(
                "zwave".to_string(),
                "serial".to_string(),
                params,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert!(!entries[0].available);
        let settings = settings.lock().unwrap().clone().expect("port was opened");
        assert_eq!(settings.baud_rate, 57_600);
        assert_eq!(settings.data_bits, tokio_serial::DataBits::Seven);
        assert_eq!(settings.parity, tokio_serial::Parity::Even);
        assert_eq!(settings.stop_bits, tokio_serial::StopBits::Two);
    }

    #[tokio::test]
    async fn reports_enumeration_failures() {
        struct FailingEnumerator;
        impl PortEnumerator for FailingEnumerator {
            fn ports(&self) -> io::Result<Vec<PortDescriptor>> {
                Err(io::Error::other("enumeration failed"))
            }
        }

        let backend = SerialDiscoveryBackend::new(
            MockPortOpener::new(Vec::new()),
            Arc::new(FailingEnumerator),
        );
        let error = backend
            .discover(
                "zwave".to_string(),
                "serial".to_string(),
                ParamValues::new(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ServiceError::Transport(_)));
    }

    #[test]
    fn builds_descriptions() {
        assert_eq!(
            describe(Some("Stick".to_string()), "Z-Wave 4.39"),
            Some("Stick [Z-Wave 4.39]".to_string())
        );
        assert_eq!(
            describe(Some("Stick".to_string()), ""),
            Some("Stick".to_string())
        );
        assert_eq!(
            describe(None, "Z-Wave 4.39"),
            Some("[Z-Wave 4.39]".to_string())
        );
        assert_eq!(describe(None, ""), None);
        assert_eq!(describe(Some(String::new()), ""), None);
    }
}
