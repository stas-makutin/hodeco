pub mod loader;
pub mod store;
pub mod validate;

use hodeco_contract::config::{CommandsFile, Config, SchedulesFile, ServicesFile, TasksFile};

/// The effective startup configuration: the core file plus the four catalogs.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigBundle {
    pub config: Config,
    pub services: ServicesFile,
    pub tasks: TasksFile,
    pub schedules: SchedulesFile,
    pub commands: CommandsFile,
}

pub use loader::{
    CatalogPaths, ConfigError, absolute_path, catalog_path, default_path, ensure_working_directory,
    executable_directory, load, load_bundle, parse_str, prepare_runtime_directories,
    prepare_working_directory, read_optional_yaml, read_yaml, save, save_yaml, to_yaml_string,
};
pub use store::{ConfigStore, ServicesStore};
pub use validate::{
    restart_required, validate, validate_bundle, validate_command, validate_schedule,
    validate_task, validate_task_id, validate_tasks,
};
