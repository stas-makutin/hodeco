use super::ConfigBundle;
use super::validate::{validate, validate_bundle};
use hodeco_contract::config::{CommandsFile, Config, SchedulesFile, ServicesFile, TasksFile};
use hodeco_contract::utils::FileMode;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to parse configuration: {0}")]
    Parse(#[from] serde_yaml_ng::Error),
    #[error("failed to parse configuration file '{path}': {source}")]
    ParseFile {
        path: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("failed to serialize configuration: {0}")]
    Serialize(serde_yaml_ng::Error),
    #[error("invalid configuration: {0}")]
    Validation(String),
    #[error("failed to read configuration file '{path}': {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write configuration file '{path}': {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("working directory '{path}' is not a directory")]
    NotDirectory { path: PathBuf },
    #[error("failed to read working directory metadata '{path}': {source}")]
    WorkingDirectoryMetadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create working directory '{path}': {source}")]
    CreateWorkingDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to change working directory to '{path}': {source}")]
    ChangeWorkingDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("'{path}' is not a directory")]
    PathNotDirectory { path: PathBuf },
    #[error("failed to read directory metadata for '{path}': {source}")]
    DirectoryMetadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create directory '{path}': {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to set permissions on '{path}': {source}")]
    SetDirectoryMode {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("configuration write task failed: {0}")]
    Task(String),
}

pub fn parse_str(input: &str) -> Result<Config, ConfigError> {
    Ok(serde_yaml_ng::from_str(input)?)
}

pub fn to_yaml_string(config: &Config) -> Result<String, ConfigError> {
    serde_yaml_ng::to_string(config).map_err(ConfigError::Serialize)
}

pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let input = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let config = parse_str(&input)?;
    validate(&config)?;
    Ok(config)
}

/// Loads the core configuration and the four sibling catalogs.
///
/// Missing catalog files are treated as empty. A malformed core file, catalog,
/// or cross-file reference fails startup with a path-specific error.
pub fn load_bundle(path: &Path) -> Result<ConfigBundle, ConfigError> {
    let config = load(path)?;
    let base = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let services: ServicesFile = read_optional_yaml(&base.join(&config.files.services))?;
    let tasks: TasksFile = read_optional_yaml(&base.join(&config.files.tasks))?;
    let schedules: SchedulesFile = read_optional_yaml(&base.join(&config.files.schedules))?;
    let commands: CommandsFile = read_optional_yaml(&base.join(&config.files.commands))?;

    let bundle = ConfigBundle {
        config,
        services,
        tasks,
        schedules,
        commands,
    };
    validate_bundle(&bundle)?;
    Ok(bundle)
}

pub fn read_optional_yaml<T>(path: &Path) -> Result<T, ConfigError>
where
    T: DeserializeOwned + Default,
{
    match std::fs::read_to_string(path) {
        Ok(input) => serde_yaml_ng::from_str(&input).map_err(|source| ConfigError::ParseFile {
            path: path.to_path_buf(),
            source,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(source) => Err(ConfigError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn read_yaml<T>(path: &Path) -> Result<T, ConfigError>
where
    T: DeserializeOwned,
{
    let input = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_yaml_ng::from_str(&input).map_err(|source| ConfigError::ParseFile {
        path: path.to_path_buf(),
        source,
    })
}

/// Writes the configuration atomically through a temporary file in the same
/// directory. Comments and formatting from the original file are not
/// preserved.
pub fn save(path: &Path, config: &Config) -> Result<(), ConfigError> {
    validate(config)?;
    let output = to_yaml_string(config)?;
    write_yaml_atomic(path, &output)
}

/// Writes any catalog or core file atomically.
pub fn save_yaml<T>(path: &Path, value: &T) -> Result<(), ConfigError>
where
    T: Serialize,
{
    let output = serde_yaml_ng::to_string(value).map_err(ConfigError::Serialize)?;
    write_yaml_atomic(path, &output)
}

fn write_yaml_atomic(path: &Path, output: &str) -> Result<(), ConfigError> {
    let file_name = path.file_name().ok_or_else(|| ConfigError::Write {
        path: path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "configuration path must include a file name",
        ),
    })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));

    let result = (|| -> Result<(), std::io::Error> {
        let mut file = std::fs::File::create(&temporary)?;
        std::io::Write::write_all(&mut file, output.as_bytes())?;
        file.sync_all()?;
        drop(file);

        apply_config_permissions(&temporary, path)?;
        std::fs::rename(&temporary, path)
    })();

    if let Err(source) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(ConfigError::Write {
            path: path.to_path_buf(),
            source,
        });
    }

    Ok(())
}

/// Resolves the four catalog paths against the core configuration directory.
pub fn catalog_path(config_path: &Path, config: &Config) -> CatalogPaths {
    let base = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    CatalogPaths {
        services: base.join(&config.files.services),
        tasks: base.join(&config.files.tasks),
        schedules: base.join(&config.files.schedules),
        commands: base.join(&config.files.commands),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPaths {
    pub services: PathBuf,
    pub tasks: PathBuf,
    pub schedules: PathBuf,
    pub commands: PathBuf,
}

#[cfg(unix)]
fn apply_config_permissions(temporary: &Path, target: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = match std::fs::metadata(target) {
        Ok(metadata) => metadata.permissions(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::Permissions::from_mode(0o644)
        }
        Err(error) => return Err(error),
    };
    std::fs::set_permissions(temporary, permissions)
}

#[cfg(not(unix))]
fn apply_config_permissions(_temporary: &Path, _target: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

/// Returns the directory containing the current executable. Windows defaults
/// are anchored here instead of the process working directory.
pub fn executable_directory() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn default_path() -> PathBuf {
    #[cfg(windows)]
    let path = executable_directory().join("config.yaml");

    #[cfg(not(windows))]
    let path = PathBuf::from("/etc/hodeco/config.yaml");

    path
}

/// Resolves a path to an absolute path without requiring the file to exist.
/// Existing paths are canonicalized; missing paths fall back to
/// `std::path::absolute`, which still resolves them against the current
/// directory before the service changes it.
pub fn absolute_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::path::absolute(path),
        Err(error) => Err(error),
    }
}

pub fn ensure_working_directory(path: &Path) -> Result<(), ConfigError> {
    if path.as_os_str().is_empty() {
        return Err(ConfigError::Validation(
            "workingDir must not be empty".to_string(),
        ));
    }

    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(ConfigError::NotDirectory {
            path: path.to_path_buf(),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path).map_err(|source| ConfigError::CreateWorkingDirectory {
                path: path.to_path_buf(),
                source,
            })
        }
        Err(source) => Err(ConfigError::WorkingDirectoryMetadata {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn prepare_working_directory(config: &Config) -> Result<(), ConfigError> {
    ensure_working_directory(&config.working_dir)?;
    std::env::set_current_dir(&config.working_dir).map_err(|source| {
        ConfigError::ChangeWorkingDirectory {
            path: config.working_dir.clone(),
            source,
        }
    })
}

/// Creates the directories required by the runtime and checks that static
/// assets are present. Missing static assets are returned as warnings; they do
/// not prevent the service from starting.
pub fn prepare_runtime_directories(config: &Config) -> Result<Vec<String>, ConfigError> {
    ensure_directory(&config.log.dir, Some(config.log.dir_mode))?;

    if let Some(parent) = config.message_log.file.parent()
        && !parent.as_os_str().is_empty()
    {
        ensure_directory(parent, None)?;
    }

    ensure_directory(&config.task_log.dir, None)?;

    let static_assets = &config.http_server.static_assets;
    let mut warnings = Vec::new();
    if static_assets.dir.is_dir() {
        let index = static_assets.dir.join(&static_assets.index);
        if !index.is_file() {
            warnings.push(format!(
                "static asset index '{}' does not exist",
                index.display()
            ));
        }
    } else if static_assets.dir.exists() {
        warnings.push(format!(
            "static asset path '{}' is not a directory",
            static_assets.dir.display()
        ));
    } else {
        warnings.push(format!(
            "static asset directory '{}' does not exist",
            static_assets.dir.display()
        ));
    }

    Ok(warnings)
}

fn ensure_directory(path: &Path, mode: Option<FileMode>) -> Result<(), ConfigError> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(ConfigError::PathNotDirectory {
            path: path.to_path_buf(),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path).map_err(|source| ConfigError::CreateDirectory {
                path: path.to_path_buf(),
                source,
            })?;
            if let Some(mode) = mode {
                set_directory_mode(path, mode)?;
            }
            Ok(())
        }
        Err(source) => Err(ConfigError::DirectoryMetadata {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(unix)]
fn set_directory_mode(path: &Path, mode: FileMode) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode.as_u32())).map_err(
        |source| ConfigError::SetDirectoryMode {
            path: path.to_path_buf(),
            source,
        },
    )
}

#[cfg(not(unix))]
fn set_directory_mode(_path: &Path, _mode: FileMode) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodeco_contract::config::{Archive, CorruptPolicy};
    use hodeco_contract::utils::{HumanDuration, Size};
    use std::time::Duration;

    #[test]
    fn uses_defaults_for_empty_configuration() {
        let config = parse_str("{}").unwrap();
        assert_eq!(config.http_server.bind, "127.0.0.1");
        assert_eq!(config.http_server.port, 8180);
        assert_eq!(config.http_server.max_connections, 1000);
        assert_eq!(config.http_server.static_assets.index, "index.html");
        assert_eq!(
            config.message_log.max_size,
            Size::from_bytes(10 * 1024 * 1024)
        );
        assert_eq!(
            config.message_log.auto_persist,
            HumanDuration::from_std(Duration::from_secs(60))
        );
        assert_eq!(config.files.services, PathBuf::from("services.yaml"));
        assert_eq!(config.task_log.compaction_factor, 2.0);
        assert_eq!(config.task_engine.max_concurrent_runs, 32);
    }

    #[test]
    fn parses_configuration() {
        let yaml = r#"
workingDir: /tmp/hodeco
log:
  dir: /tmp/log
  file: hodeco.log
  maxSize: 100 MiB
  maxAge: 1 d
  archive: zip
httpServer:
  bind: 127.0.0.1
  port: 9000
  static:
    dir: ./site
    index: index.html
    spaFallback: true
messageLog:
  maxSize: 10 MiB
  autoPersist: 60 s
  onCorrupt: fail
"#;

        let config = parse_str(yaml).unwrap();
        assert_eq!(config.working_dir, PathBuf::from("/tmp/hodeco"));
        assert_eq!(config.log.max_size, Size::from_bytes(100 * 1024 * 1024));
        assert_eq!(
            config.log.max_age,
            HumanDuration::from_std(Duration::from_secs(86_400))
        );
        assert_eq!(config.log.archive, Archive::Zip);
        assert_eq!(config.http_server.port, 9000);
        assert!(config.http_server.static_assets.spa_fallback);
        assert_eq!(config.message_log.on_corrupt, CorruptPolicy::Fail);
    }

    #[test]
    fn rejects_the_old_services_key() {
        let error = parse_str("services:\n  - protocol: zwave\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn loads_sibling_catalogs() {
        let dir = std::env::temp_dir().join(format!("hodeco-bundle-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.yaml");
        std::fs::write(&config_path, "{}").unwrap();
        std::fs::write(
            dir.join("services.yaml"),
            "services:\n  - alias: ZW\n    protocol: zwave\n    transport: serial\n    entry: COM4\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("commands.yaml"),
            "commands:\n  - id: meter\n    name: Meter\n    executable: meter\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("schedules.yaml"),
            "timezone: UTC\nschedules:\n  - id: morning\n    name: Morning\n    at: \"09:00\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("tasks.yaml"),
            "tasks:\n  - id: test\n    name: Test\n    actions:\n      - id: poll\n        command: meter\n",
        )
        .unwrap();

        let bundle = load_bundle(&config_path).unwrap();
        assert_eq!(bundle.services.services.len(), 1);
        assert_eq!(bundle.commands.commands.len(), 1);
        assert_eq!(bundle.schedules.schedules.len(), 1);
        assert_eq!(bundle.tasks.tasks.len(), 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(parse_str("httpServer:\n  unknown: 1\n").is_err());
        assert!(parse_str("unknown: 1\n").is_err());
    }

    #[test]
    fn loads_and_validates_configuration() {
        let dir = std::env::temp_dir().join(format!("hodeco-config-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, "httpServer:\n  port: 0\n").unwrap();

        let error = load(&path).unwrap_err().to_string();
        assert!(error.contains("httpServer.port"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn creates_working_directory() {
        let path =
            std::env::temp_dir().join(format!("hodeco-workdir-test-{}", uuid::Uuid::new_v4()));
        assert!(!path.exists());

        ensure_working_directory(&path).unwrap();
        assert!(path.is_dir());

        std::fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn rejects_non_directory_working_directory() {
        let path =
            std::env::temp_dir().join(format!("hodeco-workdir-file-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"not a directory").unwrap();

        let error = ensure_working_directory(&path).unwrap_err().to_string();
        assert!(error.contains("not a directory"));

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn default_path_ends_with_config_yaml() {
        let path = default_path();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("config.yaml")
        );

        #[cfg(windows)]
        assert_eq!(path.parent(), Some(executable_directory().as_path()));

        #[cfg(not(windows))]
        assert_eq!(path, PathBuf::from("/etc/hodeco/config.yaml"));
    }

    #[test]
    fn resolves_existing_and_missing_paths_to_absolute_paths() {
        let dir = std::env::temp_dir().join(format!(
            "hodeco-absolute-path-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("config.yaml");
        std::fs::write(&existing, b"{}").unwrap();

        assert_eq!(
            absolute_path(&existing).unwrap(),
            std::fs::canonicalize(&existing).unwrap()
        );
        assert!(
            absolute_path(&dir.join("missing.yaml"))
                .unwrap()
                .is_absolute()
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prepares_runtime_directories_and_reports_static_asset_warnings() {
        let root =
            std::env::temp_dir().join(format!("hodeco-runtime-dirs-test-{}", uuid::Uuid::new_v4()));
        let log_dir = root.join("log");
        let message_log = root.join("state").join("messages.log");
        let mut config = Config::default();
        config.log.dir = log_dir.clone();
        config.message_log.file = message_log.clone();
        config.http_server.static_assets.dir = root.join("missing-site");

        let warnings = prepare_runtime_directories(&config).unwrap();

        assert!(log_dir.is_dir());
        assert!(message_log.parent().unwrap().is_dir());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing-site"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rejects_non_directory_log_path() {
        let root = std::env::temp_dir().join(format!(
            "hodeco-runtime-log-file-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&root, b"not a directory").unwrap();
        let mut config = Config::default();
        config.log.dir = root.clone();

        let error = prepare_runtime_directories(&config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not a directory"));

        std::fs::remove_file(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn applies_log_directory_mode_only_when_creating_it() {
        use std::os::unix::fs::PermissionsExt;

        let root =
            std::env::temp_dir().join(format!("hodeco-runtime-mode-test-{}", uuid::Uuid::new_v4()));
        let log_dir = root.join("log");
        let mut config = Config::default();
        config.log.dir = log_dir.clone();
        config.log.dir_mode = FileMode::new(0o750);
        config.message_log.file = root.join("messages.log");
        config.http_server.static_assets.dir = root.join("site");

        prepare_runtime_directories(&config).unwrap();
        let mode = std::fs::metadata(&log_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);

        std::fs::set_permissions(&log_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        config.log.dir_mode = FileMode::new(0o755);
        prepare_runtime_directories(&config).unwrap();
        let mode = std::fs::metadata(&log_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn writes_configuration_atomically() {
        let dir =
            std::env::temp_dir().join(format!("hodeco-config-save-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");

        let mut config = Config::default();
        config.http_server.port = 9000;
        save(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap().http_server.port, 9000);

        config.http_server.port = 9001;
        save(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap().http_server.port, 9001);

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn round_trips_configuration() {
        let config = parse_str(
            r#"
workingDir: /tmp/hodeco
log:
  maxSize: 100 MiB
  maxAge: 1 d
  archive: zip
httpServer:
  port: 9000
messageLog:
  autoPersist: 60 s
  onCorrupt: fail
"#,
        )
        .unwrap();

        let yaml = to_yaml_string(&config).unwrap();
        let reparsed = parse_str(&yaml).unwrap();
        assert_eq!(config, reparsed);
    }
}
