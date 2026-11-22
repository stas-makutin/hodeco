use super::{ConfigError, save, save_yaml};
use hodeco_contract::config::{Config, ServiceConfig, ServicesFile};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use tokio::sync::Mutex as AsyncMutex;

/// Owns the core configuration and serializes write-back operations.
pub struct ConfigStore {
    path: Option<PathBuf>,
    config: Mutex<Config>,
    mutation: AsyncMutex<()>,
}

impl ConfigStore {
    /// Creates a store for `config`. `path` is `None` when changes are kept in
    /// memory only, which is useful for tests.
    pub fn new(path: Option<PathBuf>, config: Config) -> Self {
        Self {
            path,
            config: Mutex::new(config),
            mutation: AsyncMutex::new(()),
        }
    }

    pub fn snapshot(&self) -> Config {
        self.config().clone()
    }

    pub fn replace(&self, config: Config) {
        *self.config() = config;
    }

    #[cfg(test)]
    pub fn update(&self, update: impl FnOnce(&mut Config)) {
        let mut config = self.config();
        update(&mut config);
    }

    /// Holds the write-back lock across a mutation, snapshot update, and file
    /// save so concurrent API calls cannot overwrite one another.
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.mutation.lock().await
    }

    pub async fn save(&self) -> Result<(), ConfigError> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        let config = self.snapshot();

        tokio::task::spawn_blocking(move || save(&path, &config))
            .await
            .map_err(|error| ConfigError::Task(error.to_string()))?
    }

    fn config(&self) -> MutexGuard<'_, Config> {
        self.config
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Owns the service catalog (`services.yaml`) independently from the core
/// configuration. Service mutations also touch the runtime registry; the API
/// holds this store's lock across both operations.
pub struct ServicesStore {
    path: Option<PathBuf>,
    services: Mutex<Vec<ServiceConfig>>,
    mutation: AsyncMutex<()>,
}

impl ServicesStore {
    pub fn new(path: Option<PathBuf>, services: Vec<ServiceConfig>) -> Self {
        Self {
            path,
            services: Mutex::new(services),
            mutation: AsyncMutex::new(()),
        }
    }

    pub fn snapshot(&self) -> Vec<ServiceConfig> {
        self.services
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn file(&self) -> ServicesFile {
        ServicesFile {
            services: self.snapshot(),
        }
    }

    pub fn replace(&self, services: Vec<ServiceConfig>) {
        *self
            .services
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = services;
    }

    pub fn update(&self, update: impl FnOnce(&mut Vec<ServiceConfig>)) {
        let mut services = self
            .services
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut services);
    }

    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.mutation.lock().await
    }

    pub async fn save(&self) -> Result<(), ConfigError> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        let file = self.file();
        tokio::task::spawn_blocking(move || save_yaml(&path, &file))
            .await
            .map_err(|error| ConfigError::Task(error.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_str;

    #[test]
    fn snapshots_updates_and_replaces_configuration() {
        let store = ConfigStore::new(None, parse_str("httpServer:\n  port: 9000\n").unwrap());

        assert_eq!(store.snapshot().http_server.port, 9000);

        store.update(|config| config.http_server.port = 9001);
        assert_eq!(store.snapshot().http_server.port, 9001);

        store.replace(parse_str("httpServer:\n  port: 9002\n").unwrap());
        assert_eq!(store.snapshot().http_server.port, 9002);
    }

    #[test]
    fn services_store_updates_snapshots() {
        let store = ServicesStore::new(None, Vec::new());
        assert!(store.snapshot().is_empty());
        store.update(|services| {
            services.push(ServiceConfig {
                alias: Some("ZW".to_string()),
                protocol: "zwave".to_string(),
                transport: "serial".to_string(),
                entry: "COM4".to_string(),
                params: Default::default(),
            });
        });
        assert_eq!(store.file().services.len(), 1);
    }
}
