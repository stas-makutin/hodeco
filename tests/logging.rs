use std::sync::Mutex;

use hodeco::logging::ServiceLogLevel;
use hodeco_contract::config::LogConfig;
use hodeco_contract::utils::FileMode;

static LOGS: Mutex<Vec<(ServiceLogLevel, String)>> = Mutex::new(Vec::new());

fn capture(level: ServiceLogLevel, message: &str) {
    LOGS.lock().unwrap().push((level, message.to_string()));
}

#[test]
fn writes_tracing_events_to_the_configured_file() {
    let dir = tempfile::tempdir().unwrap();
    let config = LogConfig {
        dir: dir.path().to_path_buf(),
        file: "hodeco.log".to_string(),
        dir_mode: FileMode::new(0o750),
        file_mode: FileMode::new(0o640),
        ..LogConfig::default()
    };

    hodeco::logging::init(&config, capture).unwrap();

    tracing::info!(answer = 42, "hello");
    tracing::warn!("careful");
    hodeco::logging::flush();

    let contents = std::fs::read_to_string(dir.path().join("hodeco.log")).unwrap();
    assert!(contents.contains("hello"), "{contents}");
    assert!(contents.contains("answer=42"), "{contents}");
    assert!(contents.contains("careful"), "{contents}");
    assert!(contents.contains(" I logging: hello"), "{contents}");
    assert!(contents.contains(" W logging: careful"), "{contents}");
    assert!(!contents.contains("INFO"), "{contents}");
    assert!(!contents.contains("WARN"), "{contents}");
    assert!(
        !contents.lines().any(|line| line.contains("  ")),
        "the level and message must be separated by a single space: {contents}"
    );

    let logs = LOGS.lock().unwrap();
    assert!(
        logs.is_empty(),
        "operational events must not use the service logger: {logs:?}"
    );

    hodeco::logging::shutdown();
}
