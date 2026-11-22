use crate::logging::ServiceLogLevel;
use crate::service_main;
use crate::utils::timestamp;
use tokio_util::sync::CancellationToken;

pub fn run() -> bool {
    service_main::service_main(console_log, CancellationToken::new());
    true
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
