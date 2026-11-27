use crate::logging::ServiceLogLevel;
use crate::service_main::{self, ServiceOutcome};
use crate::utils::timestamp;
use std::process::{Command, Stdio};
use tokio_util::sync::CancellationToken;

pub fn run() -> bool {
    match service_main::service_main(console_log, CancellationToken::new()) {
        ServiceOutcome::Stopped => true,
        ServiceOutcome::Restarting => restart_process(),
        ServiceOutcome::Failed => false,
    }
}

/// Console runs have no external supervisor, so start a replacement process.
fn restart_process() -> bool {
    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => {
            eprintln!("{} E unable to locate the executable: {error}", timestamp());
            return false;
        }
    };
    match Command::new(executable)
        .args(std::env::args_os().skip(1))
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(_) => true,
        Err(error) => {
            eprintln!(
                "{} E unable to start a replacement process: {error}",
                timestamp()
            );
            false
        }
    }
}

fn console_log(level: ServiceLogLevel, message: &str) {
    let (level, use_stderr) = match level {
        ServiceLogLevel::Error => ('E', true),
        ServiceLogLevel::Warn => ('W', true),
        ServiceLogLevel::Info => ('I', false),
        ServiceLogLevel::Debug => ('D', false),
    };
    let line = format!("{} {} {}", timestamp(), level, message);

    if use_stderr {
        eprintln!("{}", line);
    } else {
        println!("{}", line);
    }
}
