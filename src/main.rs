use std::process;

use hodeco::command_line::{CommandLine, ServiceAction, ServiceOptions};
use hodeco::config::default_path;
use hodeco::console_runner;
use hodeco::service::ServiceManager;
use hodeco::service::service_factory::{get_service_manager, run_as_service};

fn main() {
    let command_line = CommandLine::from_env();

    if command_line.help {
        print_usage();
        return;
    }

    if run_as_service() {
        return;
    }

    if command_line.actions.is_empty() {
        print_usage();
        return;
    }

    let logger = |message: &str| println!("{}", message);
    let service_manager = get_service_manager(logger);
    let actions = prepare_actions(&command_line.actions);
    let success = actions
        .iter()
        .all(|action| execute_action(action, &service_manager, &command_line.options));

    if !success {
        process::exit(1);
    }
}

fn prepare_actions(actions: &[ServiceAction]) -> Vec<ServiceAction> {
    if actions.contains(&ServiceAction::Install) && actions.contains(&ServiceAction::Start) {
        return vec![ServiceAction::Install, ServiceAction::Start];
    }
    if actions.contains(&ServiceAction::Stop) && actions.contains(&ServiceAction::Uninstall) {
        return vec![ServiceAction::Stop, ServiceAction::Uninstall];
    }

    vec![actions[0]]
}

fn execute_action(
    action: &ServiceAction,
    service_manager: &impl ServiceManager,
    options: &ServiceOptions,
) -> bool {
    match action {
        ServiceAction::Install => service_manager.install(options),
        ServiceAction::Uninstall => service_manager.uninstall(),
        ServiceAction::Start => service_manager.start(),
        ServiceAction::Stop => service_manager.stop(),
        ServiceAction::Restart => service_manager.stop() && service_manager.start(),
        ServiceAction::Status => service_manager.status(),
        ServiceAction::Run => console_runner::run(),
    }
}

fn print_usage() {
    println!(
        r#"Hodeco - Home devices HTTP gateway.

Usage: [Action] [Options]

Actions:

install [option]
    Install the service
uninstall
    Uninstall the service
start
    Start the service
stop
    Stop the service
restart
    Restart the service
status
    Show the service status
run [option]
    Execute as console/terminal application

Options:
-h, --help
    Print this message
-c, --config <file name>
    Path to configuration yaml file. Works only for install and run actions.
    Default: {}
-u, --user <name>
    Linux service account used by install. Defaults to SUDO_USER, then USER.
"#,
        default_path().display()
    );
}
