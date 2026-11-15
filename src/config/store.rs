use super::{ConfigError, save};
use hodeco_contract::config::Config;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use tokio::sync::Mutex as AsyncMutex;

/// Owns the effective configuration, including the service list, and
/// serializes write-back operations.
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

    pub fn update(&self, update: impl FnOnce(&mut Config)) {
        let mut config = self.config();
        update(&mut config);
    }

    /// Holds the write-back lock across a registry mutation, snapshot update,
    /// and file save so concurrent API calls cannot overwrite one another.
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
}
