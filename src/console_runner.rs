use crate::logging::ServiceLogLevel;
use crate::service_main;
use crate::utils::timestamp;
use tokio_util::sync::CancellationToken;

pub fn run() -> bool {
    let cancel_token = CancellationToken::new();
    install_signal_handlers(cancel_token.clone());
    service_main::service_main(console_log, cancel_token);
    true
}

fn install_signal_handlers(cancel_token: CancellationToken) {
    let result = std::thread::Builder::new()
        .name("hodeco-console-signal-handler".to_string())
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
                #[cfg(unix)]
                {
                    let mut terminate = match tokio::signal::unix::signal(
                        tokio::signal::unix::SignalKind::terminate(),
                    ) {
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
                }
                #[cfg(not(unix))]
                {
                    if let Err(e) = tokio::signal::ctrl_c().await {
                        eprintln!("Failed to listen for Ctrl+C: {}", e);
                        return;
                    }
                }

                cancel_token.cancel();
            });
        });

    if let Err(e) = result {
        eprintln!("Failed to start signal handler: {}", e);
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
