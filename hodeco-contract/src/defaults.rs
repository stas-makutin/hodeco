use crate::config::{
    Archive, Config, CorruptPolicy, FileSetConfig, HttpServerConfig, LogConfig, MessageLogConfig,
    RunFeedConfig, StaticAssetsConfig, TaskEngineConfig, TaskLogConfig,
};
use crate::utils::{FileMode, HumanDuration, Size};
use std::path::PathBuf;
use std::time::Duration;

#[cfg(all(feature = "server-defaults", windows))]
fn executable_directory() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_working_dir() -> PathBuf {
    #[cfg(all(feature = "server-defaults", windows))]
    {
        executable_directory()
    }
    #[cfg(all(feature = "server-defaults", not(windows)))]
    {
        PathBuf::from("/var/lib/hodeco")
    }
    #[cfg(not(feature = "server-defaults"))]
    {
        PathBuf::from(".")
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            working_dir: default_working_dir(),
            log: LogConfig::default(),
            http_server: HttpServerConfig::default(),
            message_log: MessageLogConfig::default(),
            files: FileSetConfig::default(),
            task_log: TaskLogConfig::default(),
            task_engine: TaskEngineConfig::default(),
        }
    }
}

impl Default for FileSetConfig {
    fn default() -> Self {
        Self {
            services: PathBuf::from("services.yaml"),
            tasks: PathBuf::from("tasks.yaml"),
            schedules: PathBuf::from("schedules.yaml"),
            commands: PathBuf::from("commands.yaml"),
        }
    }
}

impl Default for TaskLogConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("tasks"),
            file_mode: FileMode::new(0o640),
            compaction_factor: 2.0,
            compact_interval: HumanDuration::from_std(Duration::from_secs(30 * 24 * 60 * 60)),
            on_corrupt: CorruptPolicy::Ignore,
        }
    }
}

impl Default for TaskEngineConfig {
    fn default() -> Self {
        Self {
            max_trigger_payload_size: Size::from_bytes(16 * 1024),
            max_concurrent_runs: 32,
            max_chain_depth: 16,
            max_fan_out: 16,
            run_feed: RunFeedConfig::default(),
        }
    }
}

impl Default for RunFeedConfig {
    fn default() -> Self {
        Self {
            max_rows: 1_000,
            max_size: Size::from_bytes(4 * 1024 * 1024),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from(default_log_dir()),
            file: "hodeco.log".to_string(),
            dir_mode: FileMode::new(0o755),
            file_mode: FileMode::new(0o644),
            max_size: Size::from_bytes(100 * 1024 * 1024),
            max_age: HumanDuration::from_std(Duration::from_secs(24 * 60 * 60)),
            backups: 5,
            backup_days: 14,
            archive: Archive::Zip,
        }
    }
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".to_string(),
            port: 8180,
            max_connections: 1000,
            read_timeout: HumanDuration::from_std(Duration::from_secs(5)),
            read_header_timeout: HumanDuration::from_std(Duration::from_secs(5)),
            write_timeout: HumanDuration::from_std(Duration::from_secs(5)),
            idle_timeout: HumanDuration::from_std(Duration::from_secs(2 * 60)),
            max_header_bytes: 8192,
            static_assets: StaticAssetsConfig::default(),
        }
    }
}

impl Default for StaticAssetsConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from(default_static_dir()),
            index: "index.html".to_string(),
            spa_fallback: true,
            precompressed: true,
        }
    }
}

impl Default for MessageLogConfig {
    fn default() -> Self {
        Self {
            max_size: Size::from_bytes(10 * 1024 * 1024),
            file: PathBuf::from(default_message_log_file()),
            file_mode: FileMode::new(0o644),
            auto_persist: HumanDuration::from_std(Duration::from_secs(60)),
            compaction_factor: 2.0,
            compact_interval: HumanDuration::from_std(Duration::from_secs(30 * 24 * 60 * 60)),
            on_corrupt: CorruptPolicy::Ignore,
        }
    }
}

#[cfg(all(feature = "server-defaults", windows))]
const fn default_log_dir() -> &'static str {
    "./log"
}

#[cfg(all(feature = "server-defaults", not(windows)))]
const fn default_log_dir() -> &'static str {
    "/var/log/hodeco"
}

#[cfg(not(feature = "server-defaults"))]
const fn default_log_dir() -> &'static str {
    "./log"
}

#[cfg(all(feature = "server-defaults", windows))]
const fn default_message_log_file() -> &'static str {
    "./messages.log"
}

#[cfg(all(feature = "server-defaults", not(windows)))]
const fn default_message_log_file() -> &'static str {
    "/var/lib/hodeco/messages.log"
}

#[cfg(not(feature = "server-defaults"))]
const fn default_message_log_file() -> &'static str {
    "./messages.log"
}

#[cfg(all(feature = "server-defaults", windows))]
const fn default_static_dir() -> &'static str {
    "./site"
}

#[cfg(all(feature = "server-defaults", not(windows)))]
const fn default_static_dir() -> &'static str {
    "/usr/share/hodeco/site"
}

#[cfg(not(feature = "server-defaults"))]
const fn default_static_dir() -> &'static str {
    "./site"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths_match_the_platform() {
        let config = Config::default();
        assert_eq!(config.log.archive, Archive::Zip);

        #[cfg(all(feature = "server-defaults", windows))]
        {
            assert_eq!(config.working_dir, executable_directory());
            assert_eq!(config.log.dir, PathBuf::from("./log"));
            assert_eq!(config.message_log.file, PathBuf::from("./messages.log"));
            assert_eq!(
                config.http_server.static_assets.dir,
                PathBuf::from("./site")
            );
        }

        #[cfg(all(feature = "server-defaults", not(windows)))]
        {
            assert_eq!(config.working_dir, PathBuf::from("/var/lib/hodeco"));
            assert_eq!(config.log.dir, PathBuf::from("/var/log/hodeco"));
            assert_eq!(
                config.message_log.file,
                PathBuf::from("/var/lib/hodeco/messages.log")
            );
            assert_eq!(
                config.http_server.static_assets.dir,
                PathBuf::from("/usr/share/hodeco/site")
            );
        }

        #[cfg(not(feature = "server-defaults"))]
        {
            assert_eq!(config.working_dir, PathBuf::from("."));
            assert_eq!(config.log.dir, PathBuf::from("./log"));
            assert_eq!(config.message_log.file, PathBuf::from("./messages.log"));
            assert_eq!(
                config.http_server.static_assets.dir,
                PathBuf::from("./site")
            );
        }
    }
}
