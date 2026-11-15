#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Status,
    Run,
}

impl ServiceAction {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "install" => Some(Self::Install),
            "uninstall" => Some(Self::Uninstall),
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            "status" => Some(Self::Status),
            "run" => Some(Self::Run),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct ServiceOptions {
    pub config_file: Option<String>,
    pub user: Option<String>,
}

#[derive(Default)]
pub struct CommandLine {
    pub actions: Vec<ServiceAction>,
    pub options: ServiceOptions,
    pub help: bool,
}

impl CommandLine {
    pub fn from_env() -> Self {
        Self::from_args(std::env::args().skip(1))
    }

    pub fn from_args<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut command_line = Self::default();
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            let arg = arg.as_ref();
            match arg {
                "--help" | "-h" => command_line.help = true,
                "--config" | "-c" => {
                    command_line.options.config_file =
                        args.next().map(|value| value.as_ref().to_owned());
                }
                "--user" | "-u" => {
                    command_line.options.user = args.next().map(|value| value.as_ref().to_owned());
                }
                _ => {
                    if let Some(action) = ServiceAction::parse(arg) {
                        command_line.actions.push(action);
                    }
                }
            }
        }

        command_line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config_and_user_options() {
        let command_line =
            CommandLine::from_args(["install", "--config", "config.yaml", "--user", "hodeco"]);

        assert_eq!(command_line.actions, vec![ServiceAction::Install]);
        assert_eq!(
            command_line.options.config_file.as_deref(),
            Some("config.yaml")
        );
        assert_eq!(command_line.options.user.as_deref(), Some("hodeco"));
    }

    #[test]
    fn parses_actions_and_help() {
        let command_line = CommandLine::from_args(["--help", "run", "unknown"]);

        assert!(command_line.help);
        assert_eq!(command_line.actions, vec![ServiceAction::Run]);
    }
}
