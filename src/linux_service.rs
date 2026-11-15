use super::ServiceManager as ServiceManagerTrait;
use crate::command_line::ServiceOptions;
use crate::config;
use crate::logging::ServiceLogLevel;
use crate::service_main;
use crate::utils::timestamp;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const SERVICE_NAME: &str = "hodeco";
const DISPLAY_NAME: &str = "Hodeco Service";
const DESCRIPTION: &str = "Hodeco - Home devices HTTP gateway service";
const CONFIG_DIR: &str = "/etc/hodeco";
const DEFAULT_CONFIG_PATH: &str = "/etc/hodeco/config.yaml";
const SYSTEMD_RUNTIME_DIR: &str = "/run/systemd/system";
const SYSTEMD_SERVICE_FILE: &str = "/etc/systemd/system/hodeco.service";
const SYSTEMD_RUNNING_STATE: &str = "active";
const SYSTEMD_STOPPED_STATE: &str = "inactive";
const SERVICE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const SERVICE_POLL_INTERVAL: Duration = Duration::from_millis(250);

static STOP_TOKEN: OnceLock<CancellationToken> = OnceLock::new();

#[derive(Clone, Copy)]
enum InitSystem {
    Systemd,
}

impl InitSystem {
    fn detect() -> Option<Self> {
        // systemd creates this directory when it is the running init system.
        if Path::new(SYSTEMD_RUNTIME_DIR).is_dir() {
            return Some(Self::Systemd);
        }

        // Detection for other init systems (OpenRC, runit, ...) goes here.
        None
    }
}

pub fn run_as_service() -> bool {
    if env::var_os("INVOCATION_ID").is_none() {
        return false;
    }

    let cancel_token = STOP_TOKEN.get_or_init(CancellationToken::new).clone();
    install_signal_handlers(cancel_token.clone());
    service_main::service_main(service_log, cancel_token);
    true
}

fn install_signal_handlers(cancel_token: CancellationToken) {
    let _ = std::thread::Builder::new()
        .name("hodeco-signal-handler".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    eprintln!("Failed to start signal handler: {}", e);
                    return;
                }
            };

            runtime.block_on(async move {
                let mut terminate =
                    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    {
                        Ok(signal) => signal,
                        Err(e) => {
                            eprintln!("Failed to install SIGTERM handler: {}", e);
                            return;
                        }
                    };

                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }

                cancel_token.cancel();
            });
        });
}

/// Writes initialization and fatal errors to stdout/stderr, which systemd
/// forwards to the journal. Operational events go to the application log file.
fn service_log(level: ServiceLogLevel, message: &str) {
    let (level, use_stderr) = match level {
        ServiceLogLevel::Debug => return,
        ServiceLogLevel::Error => ('E', true),
        ServiceLogLevel::Warn => ('W', true),
        ServiceLogLevel::Info => ('I', false),
    };
    let line = format!("{} {} {}", timestamp(), level, message);

    if use_stderr {
        eprintln!("{}", line);
    } else {
        println!("{}", line);
    }
}

pub fn get_service_manager(logger: fn(&str)) -> impl ServiceManagerTrait {
    match InitSystem::detect() {
        Some(InitSystem::Systemd) => LinuxServiceManager::Systemd(SystemdServiceManager { logger }),
        None => LinuxServiceManager::Unsupported(UnsupportedServiceManager { logger }),
    }
}

enum LinuxServiceManager {
    Systemd(SystemdServiceManager),
    Unsupported(UnsupportedServiceManager),
}

impl ServiceManagerTrait for LinuxServiceManager {
    fn install(&self, options: &ServiceOptions) -> bool {
        match self {
            Self::Systemd(manager) => manager.install(options),
            Self::Unsupported(manager) => manager.install(options),
        }
    }

    fn uninstall(&self) -> bool {
        match self {
            Self::Systemd(manager) => manager.uninstall(),
            Self::Unsupported(manager) => manager.uninstall(),
        }
    }

    fn start(&self) -> bool {
        match self {
            Self::Systemd(manager) => manager.start(),
            Self::Unsupported(manager) => manager.start(),
        }
    }

    fn stop(&self) -> bool {
        match self {
            Self::Systemd(manager) => manager.stop(),
            Self::Unsupported(manager) => manager.stop(),
        }
    }

    fn status(&self) -> bool {
        match self {
            Self::Systemd(manager) => manager.status(),
            Self::Unsupported(manager) => manager.status(),
        }
    }
}

struct UnsupportedServiceManager {
    logger: fn(&str),
}

impl UnsupportedServiceManager {
    fn report(&self) -> bool {
        (self.logger)(
            "Not supported init system detected. systemd is required to manage the service.",
        );
        false
    }
}

impl ServiceManagerTrait for UnsupportedServiceManager {
    fn install(&self, _options: &ServiceOptions) -> bool {
        self.report()
    }

    fn uninstall(&self) -> bool {
        self.report()
    }

    fn start(&self) -> bool {
        self.report()
    }

    fn stop(&self) -> bool {
        self.report()
    }

    fn status(&self) -> bool {
        self.report()
    }
}

struct SystemdServiceManager {
    logger: fn(&str),
}

impl ServiceManagerTrait for SystemdServiceManager {
    fn install(&self, options: &ServiceOptions) -> bool {
        if is_installed() {
            (self.logger)(&format!("Service '{}' is already installed.", DISPLAY_NAME));
            return false;
        }

        let executable = match env::current_exe() {
            Ok(path) => path,
            Err(e) => {
                (self.logger)(&format!("Failed to get executable path: {}", e));
                return false;
            }
        };

        let executable = match executable.to_str() {
            Some(path) => path,
            None => {
                (self.logger)("Executable path is not valid UTF-8; cannot write systemd unit.");
                return false;
            }
        };

        let config_path = match options.config_file.as_deref() {
            Some(path) => match config::absolute_path(Path::new(path)) {
                Ok(path) => Some(path),
                Err(e) => {
                    (self.logger)(&format!(
                        "Failed to resolve configuration path '{}': {}",
                        path, e
                    ));
                    return false;
                }
            },
            None => None,
        };

        let user = match resolve_service_user(options.user.as_deref(), self.logger) {
            Some(user) => user,
            None => return false,
        };

        if let Err(error) = fs::create_dir_all(CONFIG_DIR) {
            (self.logger)(&format!(
                "Warning: failed to create config directory '{}': {}",
                CONFIG_DIR, error
            ));
        }

        let uses_default_config = config_path
            .as_deref()
            .is_none_or(|path| path == Path::new(DEFAULT_CONFIG_PATH));
        if user != "root" && uses_default_config {
            (self.logger)(&format!(
                "Warning: config write-back requires '{}' to be writable by service user '{}'; root-owned 0644 configurations are read-only. Use --config with a writable path if service mutations must persist.",
                DEFAULT_CONFIG_PATH, user
            ));
        }

        let service_content = systemd_unit(executable, config_path.as_deref(), &user);

        if let Err(e) = fs::write(SYSTEMD_SERVICE_FILE, service_content) {
            (self.logger)(&format!(
                "Failed to write service file '{}': {}",
                SYSTEMD_SERVICE_FILE, e
            ));
            return false;
        }

        if let Err(e) = run_systemctl(&["daemon-reload"]) {
            (self.logger)(&format!("Failed to reload systemd daemon: {}", e));
            rollback_install();
            return false;
        }

        if let Err(e) = run_systemctl(&["enable", SERVICE_NAME]) {
            (self.logger)(&format!(
                "Failed to enable service '{}': {}",
                DISPLAY_NAME, e
            ));
            rollback_install();
            return false;
        }

        (self.logger)(&format!(
            "Service '{}' installed successfully.",
            DISPLAY_NAME
        ));
        true
    }

    fn uninstall(&self) -> bool {
        if !is_installed() {
            (self.logger)(&format!("Service '{}' is not installed.", DISPLAY_NAME));
            return false;
        }

        let stopped = match query_service_state() {
            Ok(state) if state == SYSTEMD_STOPPED_STATE => true,
            Ok(_) => {
                if !self.stop_and_wait() {
                    (self.logger)(&format!(
                        "Failed to stop service '{}' before uninstalling.",
                        DISPLAY_NAME
                    ));
                    return false;
                }
                true
            }
            Err(e) => {
                (self.logger)(&format!(
                    "Failed to query service '{}' status: {}",
                    DISPLAY_NAME, e
                ));
                false
            }
        };

        if !stopped {
            return false;
        }

        let mut success = true;

        if let Err(e) = run_systemctl(&["disable", SERVICE_NAME]) {
            (self.logger)(&format!(
                "Warning: failed to disable service '{}': {}",
                DISPLAY_NAME, e
            ));
            success = false;
        }

        match fs::remove_file(SYSTEMD_SERVICE_FILE) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                (self.logger)(&format!(
                    "Warning: failed to remove service file '{}': {}",
                    SYSTEMD_SERVICE_FILE, e
                ));
                success = false;
            }
        }

        if let Err(e) = run_systemctl(&["daemon-reload"]) {
            (self.logger)(&format!("Warning: failed to reload systemd daemon: {}", e));
            success = false;
        }

        if success {
            (self.logger)(&format!(
                "Service '{}' uninstalled successfully.",
                DISPLAY_NAME
            ));
            true
        } else {
            false
        }
    }

    fn start(&self) -> bool {
        if !is_installed() {
            (self.logger)(&format!("Service '{}' is not installed.", DISPLAY_NAME));
            return false;
        }

        match query_service_state() {
            Ok(state) if state == SYSTEMD_RUNNING_STATE => {
                (self.logger)(&format!("Service '{}' is already running.", DISPLAY_NAME));
                return false;
            }
            Ok(_) => {}
            Err(e) => {
                (self.logger)(&format!(
                    "Failed to query service '{}' status: {}",
                    DISPLAY_NAME, e
                ));
                return false;
            }
        }

        match run_systemctl(&["start", SERVICE_NAME]) {
            Ok(_) => {
                if self.wait_for_state(SYSTEMD_RUNNING_STATE) {
                    (self.logger)(&format!("Service '{}' started successfully.", DISPLAY_NAME));
                    true
                } else {
                    (self.logger)(&format!(
                        "Timed out waiting for service '{}' to start.",
                        DISPLAY_NAME
                    ));
                    false
                }
            }
            Err(e) => {
                (self.logger)(&format!(
                    "Failed to start service '{}': {}",
                    DISPLAY_NAME, e
                ));
                false
            }
        }
    }

    fn stop(&self) -> bool {
        if !is_installed() {
            (self.logger)(&format!("Service '{}' is not installed.", DISPLAY_NAME));
            return false;
        }

        match query_service_state() {
            Ok(state) if state == SYSTEMD_STOPPED_STATE => {
                (self.logger)(&format!("Service '{}' is already stopped.", DISPLAY_NAME));
                false
            }
            Ok(_) => self.stop_and_wait(),
            Err(e) => {
                (self.logger)(&format!(
                    "Failed to query service '{}' status: {}",
                    DISPLAY_NAME, e
                ));
                false
            }
        }
    }

    fn status(&self) -> bool {
        if !is_installed() {
            (self.logger)(&format!("Service '{}' is not installed.", DISPLAY_NAME));
            return false;
        }

        match query_service_state() {
            Ok(state) => {
                (self.logger)(&service_state_message(&state));
                true
            }
            Err(e) => {
                (self.logger)(&format!(
                    "Failed to query service '{}' status: {}",
                    DISPLAY_NAME, e
                ));
                false
            }
        }
    }
}

impl SystemdServiceManager {
    fn stop_and_wait(&self) -> bool {
        if let Err(e) = run_systemctl(&["stop", SERVICE_NAME]) {
            (self.logger)(&format!("Failed to stop service '{}': {}", DISPLAY_NAME, e));
            return false;
        }

        if self.wait_for_state(SYSTEMD_STOPPED_STATE) {
            (self.logger)(&format!("Service '{}' stopped successfully.", DISPLAY_NAME));
            true
        } else {
            (self.logger)(&format!(
                "Timed out waiting for service '{}' to stop.",
                DISPLAY_NAME
            ));
            false
        }
    }

    fn wait_for_state(&self, target: &str) -> bool {
        let deadline = Instant::now() + SERVICE_WAIT_TIMEOUT;
        loop {
            match query_service_state() {
                Ok(state) if state == target => return true,
                Ok(state) => {
                    let pending =
                        state == "activating" || state == "deactivating" || state == "reloading";

                    if !pending || Instant::now() >= deadline {
                        return false;
                    }
                }
                Err(e) => {
                    (self.logger)(&format!(
                        "Failed to query service '{}' status: {}",
                        DISPLAY_NAME, e
                    ));
                    return false;
                }
            }

            std::thread::sleep(SERVICE_POLL_INTERVAL);
        }
    }
}

fn run_systemctl(args: &[&str]) -> Result<String, String> {
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| format!("Failed to execute systemctl: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            Err(format!(
                "systemctl {} failed with {}",
                args.join(" "),
                output.status
            ))
        } else {
            Err(stderr)
        }
    }
}

fn query_service_state() -> Result<String, String> {
    // `systemctl is-active` exits non-zero for inactive units, so only the
    // reported state on stdout matters here.
    let output = Command::new("systemctl")
        .args(["is-active", SERVICE_NAME])
        .output()
        .map_err(|e| format!("Failed to execute systemctl: {}", e))?;

    let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if state.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("systemctl is-active failed with {}", output.status)
        } else {
            stderr
        })
    } else {
        Ok(state)
    }
}

fn service_state_message(state: &str) -> String {
    let text = match state {
        SYSTEMD_RUNNING_STATE => "is running",
        SYSTEMD_STOPPED_STATE => "is stopped",
        "activating" => "is starting...",
        "deactivating" => "is stopping...",
        "reloading" => "is reloading...",
        "failed" => "has failed",
        other => {
            return format!(
                "Service '{}' is in an unknown state ({}).",
                DISPLAY_NAME, other
            );
        }
    };

    format!("Service '{}' {}.", DISPLAY_NAME, text)
}

fn resolve_service_user(explicit: Option<&str>, logger: fn(&str)) -> Option<String> {
    let user = explicit
        .map(str::trim)
        .filter(|user| !user.is_empty())
        .map(str::to_string)
        .or_else(|| {
            env::var("SUDO_USER")
                .ok()
                .map(|user| user.trim().to_string())
                .filter(|user| !user.is_empty())
        })
        .or_else(|| {
            env::var("USER")
                .ok()
                .map(|user| user.trim().to_string())
                .filter(|user| !user.is_empty())
        })
        .unwrap_or_else(|| "root".to_string());

    if !account_exists(&user) {
        logger(&format!("Service user '{}' does not exist.", user));
        return None;
    }

    if user != "root" && !has_serial_group(&user) {
        logger(&format!(
            "Warning: user '{}' is not a member of dialout, uucp, or plugdev; serial ports may be inaccessible.",
            user
        ));
    }

    Some(user)
}

fn account_exists(user: &str) -> bool {
    Command::new("id")
        .args(["-u", user])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn has_serial_group(user: &str) -> bool {
    let Ok(output) = Command::new("id").args(["-Gn", user]).output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }

    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .any(|group| matches!(group, "dialout" | "uucp" | "plugdev"))
}

fn is_installed() -> bool {
    Path::new(SYSTEMD_SERVICE_FILE).exists()
}

fn rollback_install() {
    let _ = fs::remove_file(SYSTEMD_SERVICE_FILE);
    let _ = run_systemctl(&["daemon-reload"]);
}

fn systemd_unit(executable: &str, config_path: Option<&Path>, user: &str) -> String {
    // Quote the executable and config path so systemd preserves spaces correctly.
    let mut command = format!("\"{executable}\"");
    if let Some(config) = config_path {
        command.push_str(&format!(" --config \"{}\"", config.display()));
    }

    format!(
        r#"[Unit]
Description={DESCRIPTION}
After=network.target

[Service]
Type=simple
ExecStart={command}
Restart=on-failure
User={user}
LogsDirectory=hodeco
StateDirectory=hodeco

[Install]
WantedBy=multi-user.target
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_service_accounts() {
        assert!(account_exists("root"));
        assert!(!account_exists("hodeco-definitely-missing-user"));
        assert_eq!(
            resolve_service_user(Some("root"), |_| {}),
            Some("root".to_string())
        );
    }

    #[test]
    fn generates_a_systemd_unit_with_the_configured_user() {
        let unit = systemd_unit(
            "/opt/hodeco/hodeco",
            Some(Path::new("/etc/hodeco/config.yaml")),
            "hodeco",
        );

        assert!(
            unit.contains("ExecStart=\"/opt/hodeco/hodeco\" --config \"/etc/hodeco/config.yaml\"")
        );
        assert!(unit.contains("User=hodeco"));
        assert!(unit.contains("LogsDirectory=hodeco"));
        assert!(unit.contains("StateDirectory=hodeco"));
        assert!(!unit.contains("StandardOutput=append"));
        assert!(!unit.contains("logrotate"));
    }
}
