use super::ServiceError;
use super::catalog;
use super::serial::{PortOpener, SystemPortOpener};
use super::zwave::{StatusEvent, ZWaveHandle};
use crate::messages::MessageRecorder;
use hodeco_contract::domain::{
    Message, ParamValues, ServiceInfo, ServiceKey, ServiceRef, ServiceStatus,
};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::warn;

const STATUS_EVENT_CHANNEL_CAPACITY: usize = 1024;

struct ServiceEntry {
    alias: Option<String>,
    /// Explicit service overrides as stored in the configuration.
    overrides: ParamValues,
    /// Effective values after applying protocol/transport defaults.
    params: ParamValues,
    handle: Arc<ZWaveHandle>,
}

#[derive(Default)]
struct RegistryInner {
    services: HashMap<ServiceKey, ServiceEntry>,
    aliases: HashMap<String, ServiceKey>,
    order: Vec<ServiceKey>,
}

impl RegistryInner {
    fn resolve(&self, service: &ServiceRef) -> Result<ServiceKey, ServiceError> {
        let key = match service {
            ServiceRef::Alias { alias } => self
                .aliases
                .get(alias)
                .cloned()
                .ok_or(ServiceError::NotFound)?,
            ServiceRef::Key {
                protocol,
                transport,
                entry,
            } => ServiceKey {
                protocol: protocol.clone(),
                transport: transport.clone(),
                entry: entry.clone(),
            },
        };
        if self.services.contains_key(&key) {
            Ok(key)
        } else {
            Err(ServiceError::NotFound)
        }
    }

    fn info(&self, key: &ServiceKey) -> Option<ServiceInfo> {
        let entry = self.services.get(key)?;
        Some(ServiceInfo {
            alias: entry.alias.clone(),
            protocol: key.protocol.clone(),
            transport: key.transport.clone(),
            entry: key.entry.clone(),
            status: entry.handle.status(),
            params: entry.overrides.clone(),
        })
    }
}

pub struct ServiceRegistry {
    recorder: Arc<dyn MessageRecorder>,
    opener: Arc<dyn PortOpener>,
    cancel: CancellationToken,
    status_events: broadcast::Sender<StatusEvent>,
    inner: Mutex<RegistryInner>,
}

impl ServiceRegistry {
    pub fn new(
        recorder: Arc<dyn MessageRecorder>,
        opener: Arc<dyn PortOpener>,
        parent_cancel: &CancellationToken,
    ) -> Self {
        let (status_events, _) = broadcast::channel(STATUS_EVENT_CHANNEL_CAPACITY);
        Self {
            recorder,
            opener,
            cancel: parent_cancel.child_token(),
            status_events,
            inner: Mutex::new(RegistryInner::default()),
        }
    }

    pub fn subscribe_status(&self) -> broadcast::Receiver<StatusEvent> {
        self.status_events.subscribe()
    }

    pub fn with_defaults(
        recorder: Arc<dyn MessageRecorder>,
        parent_cancel: &CancellationToken,
    ) -> Self {
        Self::new(recorder, Arc::new(SystemPortOpener), parent_cancel)
    }

    pub fn add(
        &self,
        key: ServiceKey,
        params: &ParamValues,
        alias: Option<String>,
    ) -> Result<ServiceInfo, ServiceError> {
        validate_key(&key)?;
        let alias = validate_alias(alias)?;
        let parsed = catalog::validate_service(&key.protocol, &key.transport, params)?;

        let mut inner = self.inner.lock();
        if inner.services.contains_key(&key) {
            return Err(ServiceError::AlreadyExists);
        }
        if let Some(alias) = &alias
            && inner.aliases.contains_key(alias)
        {
            return Err(ServiceError::AliasConflict);
        }

        let handle = Arc::new(self.start_handle(&key, &parsed)?);
        let info = ServiceInfo {
            alias: alias.clone(),
            protocol: key.protocol.clone(),
            transport: key.transport.clone(),
            entry: key.entry.clone(),
            status: handle.status(),
            params: params.clone(),
        };

        if let Some(alias) = &alias {
            inner.aliases.insert(alias.clone(), key.clone());
        }
        inner.services.insert(
            key.clone(),
            ServiceEntry {
                alias,
                overrides: params.clone(),
                params: parsed,
                handle,
            },
        );
        inner.order.push(key);

        Ok(info)
    }

    pub async fn remove(&self, service: &ServiceRef) -> Result<(), ServiceError> {
        let entry = {
            let mut inner = self.inner.lock();
            let key = inner.resolve(service)?;
            let entry = inner.services.remove(&key).ok_or(ServiceError::NotFound)?;
            if let Some(alias) = &entry.alias {
                inner.aliases.remove(alias);
            }
            inner.order.retain(|candidate| candidate != &key);
            entry
        };
        entry.handle.stop().await;
        Ok(())
    }

    pub fn alias(
        &self,
        service: &ServiceRef,
        new_alias: Option<String>,
    ) -> Result<ServiceInfo, ServiceError> {
        let new_alias = validate_alias(new_alias)?;
        let mut inner = self.inner.lock();
        let key = inner.resolve(service)?;
        let old_alias = inner
            .services
            .get(&key)
            .and_then(|entry| entry.alias.clone());

        if old_alias == new_alias {
            return inner.info(&key).ok_or(ServiceError::NotFound);
        }

        if let Some(alias) = &new_alias
            && let Some(existing) = inner.aliases.get(alias)
            && existing != &key
        {
            return Err(ServiceError::AliasConflict);
        }

        if let Some(alias) = &old_alias {
            inner.aliases.remove(alias);
        }
        if let Some(alias) = &new_alias {
            inner.aliases.insert(alias.clone(), key.clone());
        }
        let entry = inner.services.get_mut(&key).ok_or(ServiceError::NotFound)?;
        entry.alias = new_alias;
        inner.info(&key).ok_or(ServiceError::NotFound)
    }

    /// Replaces the explicit overrides for a service and restarts just that
    /// service handle with the newly resolved effective parameters. The service
    /// entry is removed from the registry while the old handle stops so a new
    /// handle can reopen the same transport.
    pub async fn update_params(
        &self,
        service: &ServiceRef,
        overrides: &ParamValues,
    ) -> Result<ServiceInfo, ServiceError> {
        let (key, alias, old_overrides, old_effective, old_handle, effective) = {
            let mut inner = self.inner.lock();
            let key = inner.resolve(service)?;
            let effective = catalog::validate_service(&key.protocol, &key.transport, overrides)?;
            let entry = inner.services.remove(&key).ok_or(ServiceError::NotFound)?;
            if let Some(alias) = &entry.alias {
                inner.aliases.remove(alias);
            }
            (
                key,
                entry.alias,
                entry.overrides,
                entry.params,
                entry.handle,
                effective,
            )
        };

        old_handle.stop().await;

        let handle = match self.start_handle(&key, &effective) {
            Ok(handle) => Arc::new(handle),
            Err(error) => {
                match self.start_handle(&key, &old_effective) {
                    Ok(handle) => {
                        let mut inner = self.inner.lock();
                        if let Some(alias) = &alias {
                            inner.aliases.insert(alias.clone(), key.clone());
                        }
                        inner.services.insert(
                            key,
                            ServiceEntry {
                                alias,
                                overrides: old_overrides,
                                params: old_effective,
                                handle: Arc::new(handle),
                            },
                        );
                    }
                    Err(restore_error) => warn!(
                        error = %restore_error,
                        protocol = %key.protocol,
                        transport = %key.transport,
                        entry = %key.entry,
                        "failed to restore service after parameter update failure"
                    ),
                }
                return Err(error);
            }
        };

        let mut inner = self.inner.lock();
        if let Some(alias) = &alias {
            inner.aliases.insert(alias.clone(), key.clone());
        }
        inner.services.insert(
            key.clone(),
            ServiceEntry {
                alias,
                overrides: overrides.clone(),
                params: effective,
                handle,
            },
        );
        inner.info(&key).ok_or(ServiceError::NotFound)
    }

    pub fn info(&self, service: &ServiceRef) -> Result<ServiceInfo, ServiceError> {
        let inner = self.inner.lock();
        let key = inner.resolve(service)?;
        inner.info(&key).ok_or(ServiceError::NotFound)
    }

    pub fn status(&self, service: &ServiceRef) -> Result<ServiceStatus, ServiceError> {
        let inner = self.inner.lock();
        let key = inner.resolve(service)?;
        inner
            .services
            .get(&key)
            .map(|entry| entry.handle.status())
            .ok_or(ServiceError::NotFound)
    }

    pub fn send(
        &self,
        service: &ServiceRef,
        payload: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<Message, ServiceError> {
        let handle = {
            let inner = self.inner.lock();
            let key = inner.resolve(service)?;
            inner
                .services
                .get(&key)
                .ok_or(ServiceError::NotFound)?
                .handle
                .clone()
        };
        handle.send(payload, ttl)
    }

    pub fn list(&self) -> Vec<ServiceInfo> {
        let inner = self.inner.lock();
        inner
            .order
            .iter()
            .filter_map(|key| inner.info(key))
            .collect()
    }

    pub fn resolve(&self, service: &ServiceRef) -> Result<ServiceKey, ServiceError> {
        self.inner.lock().resolve(service)
    }

    fn start_handle(
        &self,
        key: &ServiceKey,
        params: &ParamValues,
    ) -> Result<ZWaveHandle, ServiceError> {
        ZWaveHandle::start_with_status_events(
            key.clone(),
            params.clone(),
            self.recorder.clone(),
            self.opener.clone(),
            &self.cancel,
            Some(self.status_events.clone()),
        )
    }

    pub async fn shutdown(&self) {
        self.cancel.cancel();
        let handles: Vec<Arc<ZWaveHandle>> = {
            let mut inner = self.inner.lock();
            inner.aliases.clear();
            inner.order.clear();
            inner
                .services
                .drain()
                .map(|(_, entry)| entry.handle)
                .collect()
        };
        for handle in handles {
            handle.stop().await;
        }
    }
}

fn validate_key(key: &ServiceKey) -> Result<(), ServiceError> {
    if key.protocol.trim().is_empty()
        || key.transport.trim().is_empty()
        || key.entry.trim().is_empty()
    {
        return Err(ServiceError::InvalidKey);
    }
    Ok(())
}

fn validate_alias(alias: Option<String>) -> Result<Option<String>, ServiceError> {
    match alias {
        Some(alias) if alias.trim().is_empty() => Err(ServiceError::InvalidAlias),
        alias => Ok(alias),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::MessageStore;
    use crate::services::test_support::MockPortOpener;
    use hodeco_contract::domain::ServiceState;
    use std::io;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::io::AsyncReadExt;

    fn key(entry: &str) -> ServiceKey {
        ServiceKey {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: entry.to_string(),
        }
    }

    fn alias_ref(alias: &str) -> ServiceRef {
        ServiceRef::Alias {
            alias: alias.to_string(),
        }
    }

    fn registry(opener: Arc<dyn PortOpener>) -> (ServiceRegistry, CancellationToken) {
        let cancel = CancellationToken::new();
        let store = Arc::new(MessageStore::new(1024 * 1024));
        (ServiceRegistry::new(store, opener, &cancel), cancel)
    }

    async fn wait_running(registry: &ServiceRegistry, service: &ServiceRef) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status = registry.status(service).unwrap();
            if status.state == ServiceState::Running {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "service did not start: {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn adds_lists_and_removes_services() {
        let (device, mut host) = tokio::io::duplex(1024);
        let opener =
            MockPortOpener::new(vec![Ok(Box::new(device) as crate::services::serial::Port)]);
        let (registry, cancel) = registry(opener);

        let info = registry
            .add(key("COM3"), &ParamValues::new(), Some("ZW".to_string()))
            .unwrap();
        assert_eq!(info.protocol, "zwave");
        assert_eq!(info.alias.as_deref(), Some("ZW"));
        assert!(info.params.is_empty());

        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.resolve(&alias_ref("ZW")).unwrap(), key("COM3"));
        assert_eq!(
            registry
                .resolve(&ServiceRef::Key {
                    protocol: "zwave".to_string(),
                    transport: "serial".to_string(),
                    entry: "COM3".to_string(),
                })
                .unwrap(),
            key("COM3")
        );

        wait_running(&registry, &alias_ref("ZW")).await;

        let message = registry
            .send(&alias_ref("ZW"), vec![1, 2, 3], None)
            .unwrap();
        assert_eq!(
            message.state,
            hodeco_contract::domain::MessageState::OutgoingPending
        );
        let mut buffer = [0_u8; 16];
        let count = host.read(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..count], &[1, 2, 3]);

        registry.remove(&alias_ref("ZW")).await.unwrap();
        assert!(registry.list().is_empty());
        assert!(matches!(
            registry.status(&alias_ref("ZW")),
            Err(ServiceError::NotFound)
        ));

        registry.shutdown().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn updates_service_parameter_overrides() {
        let (first, _first_host) = tokio::io::duplex(64);
        let (second, _second_host) = tokio::io::duplex(64);
        let opener = MockPortOpener::new(vec![
            Ok(Box::new(first) as crate::services::serial::Port),
            Ok(Box::new(second) as crate::services::serial::Port),
        ]);
        let (registry, cancel) = registry(opener);

        registry
            .add(key("COM3"), &ParamValues::new(), Some("ZW".to_string()))
            .unwrap();
        let params = ParamValues::from([("baudRate".to_string(), serde_json::json!(57_600))]);
        let info = registry
            .update_params(&alias_ref("ZW"), &params)
            .await
            .unwrap();
        assert_eq!(
            info.params.get("baudRate"),
            Some(&serde_json::json!(57_600))
        );
        assert_eq!(
            registry.list()[0].params.get("baudRate"),
            Some(&serde_json::json!(57_600))
        );

        let info = registry
            .update_params(&alias_ref("ZW"), &ParamValues::new())
            .await
            .unwrap();
        assert!(info.params.is_empty());

        let invalid = ParamValues::from([("baudRate".to_string(), serde_json::json!("fast"))]);
        assert!(
            registry
                .update_params(&alias_ref("ZW"), &invalid)
                .await
                .is_err()
        );
        assert!(registry.status(&alias_ref("ZW")).is_ok());

        registry.shutdown().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn rejects_duplicate_keys_and_aliases() {
        let (first, _first_host) = tokio::io::duplex(64);
        let (second, _second_host) = tokio::io::duplex(64);
        let opener = MockPortOpener::new(vec![
            Ok(Box::new(first) as crate::services::serial::Port),
            Ok(Box::new(second) as crate::services::serial::Port),
        ]);
        let (registry, cancel) = registry(opener);

        registry
            .add(key("COM3"), &ParamValues::new(), Some("ZW".to_string()))
            .unwrap();
        assert!(matches!(
            registry.add(key("COM3"), &ParamValues::new(), None),
            Err(ServiceError::AlreadyExists)
        ));
        assert!(matches!(
            registry.add(key("COM4"), &ParamValues::new(), Some("ZW".to_string())),
            Err(ServiceError::AliasConflict)
        ));
        assert_eq!(registry.list().len(), 1);

        registry.shutdown().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn changes_and_clears_aliases() {
        let (device, _host) = tokio::io::duplex(64);
        let opener =
            MockPortOpener::new(vec![Ok(Box::new(device) as crate::services::serial::Port)]);
        let (registry, cancel) = registry(opener);

        registry
            .add(key("COM3"), &ParamValues::new(), Some("ZW".to_string()))
            .unwrap();
        let info = registry
            .alias(&alias_ref("ZW"), Some("OTHER".to_string()))
            .unwrap();
        assert_eq!(info.alias.as_deref(), Some("OTHER"));
        assert!(matches!(
            registry.status(&alias_ref("ZW")),
            Err(ServiceError::NotFound)
        ));
        assert_eq!(registry.get_alias("OTHER"), Some(key("COM3")));

        let info = registry.alias(&alias_ref("OTHER"), None).unwrap();
        assert!(info.alias.is_none());
        assert!(registry.get_alias("OTHER").is_none());
        assert_eq!(registry.list().len(), 1);

        registry.shutdown().await;
        cancel.cancel();
    }

    impl ServiceRegistry {
        fn get_alias(&self, alias: &str) -> Option<ServiceKey> {
            self.inner.lock().aliases.get(alias).cloned()
        }
    }

    #[tokio::test]
    async fn validates_keys_aliases_protocols_and_params() {
        let (registry, cancel) = registry(MockPortOpener::new(Vec::new()));

        assert!(matches!(
            registry.add(
                ServiceKey {
                    protocol: String::new(),
                    transport: "serial".to_string(),
                    entry: "COM3".to_string(),
                },
                &ParamValues::new(),
                None,
            ),
            Err(ServiceError::InvalidKey)
        ));
        assert!(matches!(
            registry.add(key(" "), &ParamValues::new(), None),
            Err(ServiceError::InvalidKey)
        ));
        assert!(matches!(
            registry.add(key("COM3"), &ParamValues::new(), Some(" ".to_string())),
            Err(ServiceError::InvalidAlias)
        ));
        assert!(matches!(
            registry.add(
                ServiceKey {
                    protocol: "unknown".to_string(),
                    transport: "serial".to_string(),
                    entry: "COM3".to_string(),
                },
                &ParamValues::new(),
                None,
            ),
            Err(ServiceError::UnknownProtocol(_))
        ));
        assert!(matches!(
            registry.add(
                ServiceKey {
                    protocol: "zwave".to_string(),
                    transport: "unknown".to_string(),
                    entry: "COM3".to_string(),
                },
                &ParamValues::new(),
                None,
            ),
            Err(ServiceError::UnknownTransport(_))
        ));
        assert!(matches!(
            registry.add(
                key("COM3"),
                &ParamValues::from([("baudRate".to_string(), serde_json::json!("fast"))]),
                None,
            ),
            Err(ServiceError::InvalidParams(_))
        ));
        assert!(registry.list().is_empty());

        registry.shutdown().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn reports_unknown_services() {
        let (registry, cancel) = registry(MockPortOpener::new(Vec::new()));
        assert!(matches!(
            registry.status(&alias_ref("MISSING")),
            Err(ServiceError::NotFound)
        ));
        assert!(matches!(
            registry.send(&alias_ref("MISSING"), vec![1], None),
            Err(ServiceError::NotFound)
        ));
        assert!(matches!(
            registry.remove(&alias_ref("MISSING")).await,
            Err(ServiceError::NotFound)
        ));
        assert!(matches!(
            registry.alias(&alias_ref("MISSING"), None),
            Err(ServiceError::NotFound)
        ));
        registry.shutdown().await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn reports_open_failures_through_status() {
        let opener = MockPortOpener::new(vec![Err(io::Error::new(
            io::ErrorKind::NotFound,
            "missing",
        ))]);
        let (registry, cancel) = registry(opener);
        registry
            .add(key("COM3"), &ParamValues::new(), None)
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status = registry.status(&key_ref("COM3")).unwrap();
            if status.state == ServiceState::Error {
                assert!(status.error.is_some());
                break;
            }
            assert!(Instant::now() < deadline, "service did not report an error");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        registry.shutdown().await;
        cancel.cancel();
    }

    fn key_ref(entry: &str) -> ServiceRef {
        ServiceRef::Key {
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: entry.to_string(),
        }
    }
}
