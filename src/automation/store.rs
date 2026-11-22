//! Live automation catalog store.
//!
//! Tasks, schedules, and commands share one mutation lock. CRUD applies to
//! memory and the relevant YAML file; callers validate cross-file references
//! before replacing a catalog.

use crate::config::{CatalogPaths, ConfigError, save_yaml};
use hodeco_contract::config::{CommandsFile, SchedulesFile, TasksFile};
use hodeco_contract::domain::{
    CommandDefinition, CommandInfo, Location, ScheduleDefinition, ScheduleInfo, TaskDefinition,
};
use parking_lot::RwLock;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

#[derive(Debug, Clone, PartialEq)]
pub struct AutomationSnapshot {
    pub tasks: BTreeMap<String, TaskDefinition>,
    pub schedules: BTreeMap<String, ScheduleDefinition>,
    pub commands: BTreeMap<String, CommandDefinition>,
    pub location: Option<Location>,
    pub timezone: Option<String>,
}

impl AutomationSnapshot {
    pub fn schedule_ids(&self) -> BTreeSet<String> {
        self.schedules.keys().cloned().collect()
    }

    pub fn command_ids(&self) -> BTreeSet<String> {
        self.commands.keys().cloned().collect()
    }

    pub fn used_by_schedule(&self, schedule_id: &str) -> Vec<String> {
        self.tasks
            .values()
            .filter(|task| {
                task.schedule_triggers()
                    .any(|trigger| trigger.schedule == schedule_id)
            })
            .map(|task| task.id.clone())
            .collect()
    }

    pub fn used_by_command(&self, command_id: &str) -> Vec<String> {
        self.tasks
            .values()
            .filter(|task| {
                task.actions.iter().any(|action| {
                    action
                        .command()
                        .is_some_and(|command| command.command == command_id)
                }) || task
                    .next
                    .iter()
                    .any(|rule| rule.command.as_deref() == Some(command_id))
            })
            .map(|task| task.id.clone())
            .collect()
    }

    pub fn schedule_info(
        &self,
        schedule: &ScheduleDefinition,
        next: Option<hodeco_contract::domain::ScheduleOccurrence>,
    ) -> ScheduleInfo {
        ScheduleInfo {
            schedule: schedule.clone(),
            next,
            used_by: self.used_by_schedule(&schedule.id),
        }
    }

    pub fn command_info(&self, command: &CommandDefinition) -> CommandInfo {
        CommandInfo {
            command: command.clone(),
            used_by: self.used_by_command(&command.id),
        }
    }
}

pub struct AutomationStore {
    paths: CatalogPaths,
    data: RwLock<Arc<AutomationSnapshot>>,
    mutation: AsyncMutex<()>,
}

impl AutomationStore {
    pub fn new(snapshot: AutomationSnapshot, paths: CatalogPaths) -> Self {
        Self {
            paths,
            data: RwLock::new(Arc::new(snapshot)),
            mutation: AsyncMutex::new(()),
        }
    }

    /// Shares the current catalog without cloning it. Hot paths (message-trigger
    /// evaluation, scheduling, and list requests) read the catalog per event; a
    /// cheap `Arc` clone keeps them from paying a full catalog copy each time.
    pub fn snapshot(&self) -> Arc<AutomationSnapshot> {
        self.data.read().clone()
    }

    pub fn task(&self, id: &str) -> Option<TaskDefinition> {
        self.data.read().tasks.get(id).cloned()
    }

    pub fn replace_snapshot(&self, snapshot: Arc<AutomationSnapshot>) {
        *self.data.write() = snapshot;
    }

    /// Replaces the shared `location` / `timezone` defaults in `schedules.yaml`.
    pub fn set_schedule_defaults(&self, location: Option<Location>, timezone: Option<String>) {
        let mut data = self.data.write();
        let data = Arc::make_mut(&mut data);
        data.location = location;
        data.timezone = timezone;
    }

    pub fn update_tasks(&self, update: impl FnOnce(&mut BTreeMap<String, TaskDefinition>)) {
        let mut data = self.data.write();
        update(&mut Arc::make_mut(&mut data).tasks);
    }

    pub fn update_schedules(&self, update: impl FnOnce(&mut BTreeMap<String, ScheduleDefinition>)) {
        let mut data = self.data.write();
        update(&mut Arc::make_mut(&mut data).schedules);
    }

    pub fn update_commands(&self, update: impl FnOnce(&mut BTreeMap<String, CommandDefinition>)) {
        let mut data = self.data.write();
        update(&mut Arc::make_mut(&mut data).commands);
    }

    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.mutation.lock().await
    }

    pub fn tasks_file(&self) -> TasksFile {
        TasksFile {
            tasks: self.data.read().tasks.values().cloned().collect(),
        }
    }

    pub fn schedules_file(&self) -> SchedulesFile {
        let data = self.data.read();
        SchedulesFile {
            location: data.location,
            timezone: data.timezone.clone(),
            schedules: data.schedules.values().cloned().collect(),
        }
    }

    pub fn commands_file(&self) -> CommandsFile {
        CommandsFile {
            commands: self.data.read().commands.values().cloned().collect(),
        }
    }

    pub async fn save_tasks(&self) -> Result<(), ConfigError> {
        let path = self.paths.tasks.clone();
        let file = self.tasks_file();
        tokio::task::spawn_blocking(move || save_yaml(&path, &file))
            .await
            .map_err(|error| ConfigError::Task(error.to_string()))?
    }

    pub async fn save_schedules(&self) -> Result<(), ConfigError> {
        let path = self.paths.schedules.clone();
        let file = self.schedules_file();
        tokio::task::spawn_blocking(move || save_yaml(&path, &file))
            .await
            .map_err(|error| ConfigError::Task(error.to_string()))?
    }

    pub async fn save_commands(&self) -> Result<(), ConfigError> {
        let path = self.paths.commands.clone();
        let file = self.commands_file();
        tokio::task::spawn_blocking(move || save_yaml(&path, &file))
            .await
            .map_err(|error| ConfigError::Task(error.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodeco_contract::domain::{ScheduleTrigger, TaskTrigger};
    use std::path::PathBuf;

    fn paths() -> CatalogPaths {
        CatalogPaths {
            services: PathBuf::from("services.yaml"),
            tasks: PathBuf::from("tasks.yaml"),
            schedules: PathBuf::from("schedules.yaml"),
            commands: PathBuf::from("commands.yaml"),
        }
    }

    #[test]
    fn reports_catalog_usage() {
        let mut task = TaskDefinition {
            id: "evening".to_string(),
            name: "Evening".to_string(),
            ..TaskDefinition::default()
        };
        task.triggers.push(TaskTrigger::Schedule(ScheduleTrigger {
            id: "sunset".to_string(),
            schedule: "before-sunset".to_string(),
            max_runs: None,
        }));
        let snapshot = AutomationSnapshot {
            tasks: BTreeMap::from([(task.id.clone(), task)]),
            schedules: BTreeMap::new(),
            commands: BTreeMap::new(),
            location: None,
            timezone: None,
        };
        let store = AutomationStore::new(snapshot, paths());
        let snapshot = store.snapshot();
        assert_eq!(snapshot.used_by_schedule("before-sunset"), vec!["evening"]);
        assert!(snapshot.used_by_command("meter").is_empty());
    }

    #[test]
    fn serializes_catalog_files() {
        let snapshot = AutomationSnapshot {
            tasks: BTreeMap::new(),
            schedules: BTreeMap::new(),
            commands: BTreeMap::new(),
            location: None,
            timezone: Some("Europe/Berlin".to_string()),
        };
        let store = AutomationStore::new(snapshot, paths());
        assert_eq!(
            store.schedules_file().timezone.as_deref(),
            Some("Europe/Berlin")
        );
    }
}
