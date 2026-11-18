//! Command catalog DTOs.

use crate::utils::HumanDuration;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

pub fn default_command_timeout() -> HumanDuration {
    HumanDuration::from_std(DEFAULT_COMMAND_TIMEOUT)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct CommandDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub executable: PathBuf,
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
    pub timeout: HumanDuration,
}

impl Default for CommandDefinition {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            executable: PathBuf::new(),
            args: Vec::new(),
            working_dir: None,
            timeout: default_command_timeout(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CommandInfo {
    pub command: CommandDefinition,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        let yaml = r#"
commands:
  - id: meter
    name: Meter poll
    executable: /usr/local/bin/meter-poll
    args: ["--json"]
    workingDir: /var/lib/hodeco
    timeout: 10s
    description: Read the meter value
"#;
        #[derive(Deserialize)]
        struct File {
            commands: Vec<CommandDefinition>,
        }
        let file: File = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(file.commands[0].id, "meter");
        assert_eq!(file.commands[0].args, vec!["--json"]);
        assert_eq!(
            file.commands[0].timeout,
            HumanDuration::from_std(Duration::from_secs(10))
        );
        assert_eq!(
            file.commands[0].working_dir,
            Some(PathBuf::from("/var/lib/hodeco"))
        );
    }
}
