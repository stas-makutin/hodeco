//! Command catalog execution.
//!
//! Commands run as the service account. Task arguments are appended after the
//! catalog arguments. A single JSON document is written to stdin. Output is
//! captured only when requested and is capped per run.

use hodeco_contract::domain::CommandDefinition;
use hodeco_contract::utils::Base64Bytes;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub const OUTPUT_CAPTURE_LIMIT: usize = 8 * 1024;
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct CommandOutcome {
    pub exit_code: Option<i32>,
    pub stdout: Option<Base64Bytes>,
    pub output_truncated: bool,
    pub error: Option<String>,
}

impl CommandOutcome {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && self.error.is_none()
    }
}

#[derive(Debug, Default)]
pub struct CaptureBudget {
    remaining: usize,
}

impl CaptureBudget {
    pub fn new() -> Self {
        Self {
            remaining: OUTPUT_CAPTURE_LIMIT,
        }
    }

    fn remaining(&self) -> usize {
        self.remaining
    }
}

pub async fn run(
    command: &CommandDefinition,
    extra_args: &[String],
    input: &[u8],
    capture: bool,
    budget: &mut CaptureBudget,
    cancel: CancellationToken,
) -> CommandOutcome {
    let mut process = Command::new(&command.executable);
    process.args(&command.args).args(extra_args);
    if let Some(working_dir) = &command.working_dir {
        process.current_dir(working_dir);
    }
    process.stdin(Stdio::piped());
    if capture {
        process.stdout(Stdio::piped());
    } else {
        process.stdout(Stdio::null());
    }
    process.stderr(Stdio::null());
    process.kill_on_drop(true);

    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CommandOutcome {
                exit_code: None,
                stdout: None,
                output_truncated: false,
                error: Some(format!(
                    "failed to start '{}': {error}",
                    command.executable.display()
                )),
            };
        }
    };

    let stdin_writer = child.stdin.take().map(|mut stdin| {
        let input = input.to_vec();
        tokio::spawn(async move {
            // A command is free to exit without reading stdin.
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
        })
    });

    let remaining = Arc::new(AtomicUsize::new(budget.remaining()));
    let stdout_reader = child.stdout.take().map(|pipe| {
        let remaining = remaining.clone();
        tokio::spawn(read_limited(pipe, remaining))
    });

    let timeout = command.timeout.as_std().max(Duration::from_millis(1));
    let waited = tokio::select! {
        _ = cancel.cancelled() => Err("cancelled".to_string()),
        result = tokio::time::timeout(timeout, child.wait()) => match result {
            Ok(Ok(status)) => Ok(status.code()),
            Ok(Err(error)) => Err(format!("failed to wait for '{}': {error}", command.executable.display())),
            Err(_) => Err(format!("command timed out after {:?}", timeout)),
        },
    };

    if waited.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
        // Drain readers so their tasks do not linger. A descendant may inherit
        // the pipes and keep them open, so bound the drain.
        if let Some(mut handle) = stdout_reader {
            let _ = tokio::time::timeout(PIPE_DRAIN_TIMEOUT, &mut handle).await;
            handle.abort();
        }
        if let Some(handle) = stdin_writer {
            handle.abort();
        }
        budget.remaining = remaining.load(Ordering::Relaxed);
        return CommandOutcome {
            exit_code: None,
            stdout: None,
            output_truncated: budget.remaining == 0,
            error: waited.err(),
        };
    }

    if let Some(mut handle) = stdin_writer {
        let _ = tokio::time::timeout(PIPE_DRAIN_TIMEOUT, &mut handle).await;
        handle.abort();
    }
    let (stdout, stdout_truncated) = drain_output(stdout_reader).await;

    let stored = remaining.load(Ordering::Relaxed);
    budget.remaining = stored;
    CommandOutcome {
        exit_code: waited.ok().flatten(),
        stdout: capture.then_some(Base64Bytes::from(stdout)),
        output_truncated: budget.remaining() == 0 && stdout_truncated,
        error: None,
    }
}

type LimitedOutput = Result<(Vec<u8>, bool), std::io::Error>;

async fn drain_output(handle: Option<tokio::task::JoinHandle<LimitedOutput>>) -> (Vec<u8>, bool) {
    match handle {
        Some(mut handle) => match tokio::time::timeout(PIPE_DRAIN_TIMEOUT, &mut handle).await {
            Ok(Ok(Ok(output))) => output,
            _ => {
                handle.abort();
                (Vec::new(), true)
            }
        },
        None => (Vec::new(), false),
    }
}

async fn read_limited<R>(mut reader: R, remaining: Arc<AtomicUsize>) -> LimitedOutput
where
    R: AsyncRead + Unpin,
{
    let mut stored = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let left = remaining.load(Ordering::Relaxed);
        if left == 0 {
            truncated = true;
            continue;
        }
        let take = read.min(left);
        remaining.fetch_sub(take, Ordering::Relaxed);
        stored.extend_from_slice(&buffer[..take]);
        if take < read {
            truncated = true;
        }
    }
    Ok((stored, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodeco_contract::utils::HumanDuration;
    use std::path::PathBuf;

    fn command(executable: &str, args: &[&str]) -> CommandDefinition {
        CommandDefinition {
            id: "test".to_string(),
            name: "Test".to_string(),
            executable: PathBuf::from(executable),
            args: args.iter().map(|value| value.to_string()).collect(),
            timeout: HumanDuration::from_std(Duration::from_secs(5)),
            ..CommandDefinition::default()
        }
    }

    #[tokio::test]
    async fn runs_and_captures_output() {
        #[cfg(windows)]
        let definition = command("cmd", &["/C", "echo hello && echo err 1>&2 && exit /b 0"]);
        #[cfg(not(windows))]
        let definition = command("sh", &["-c", "echo hello; echo err 1>&2; exit 0"]);

        let mut budget = CaptureBudget::new();
        let outcome = run(
            &definition,
            &[],
            b"{}",
            true,
            &mut budget,
            CancellationToken::new(),
        )
        .await;
        assert!(outcome.succeeded(), "{outcome:?}");
        let stdout =
            String::from_utf8_lossy(outcome.stdout.as_ref().expect("captured stdout").as_slice());
        assert!(stdout.contains("hello"));
    }

    #[tokio::test]
    async fn reports_non_zero_exit_codes() {
        #[cfg(windows)]
        let definition = command("cmd", &["/C", "exit /b 7"]);
        #[cfg(not(windows))]
        let definition = command("sh", &["-c", "exit 7"]);

        let mut budget = CaptureBudget::new();
        let outcome = run(
            &definition,
            &[],
            b"",
            false,
            &mut budget,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(outcome.exit_code, Some(7));
        assert!(!outcome.succeeded());
        assert!(outcome.stdout.is_none());
    }

    #[tokio::test]
    async fn times_out() {
        #[cfg(windows)]
        let mut definition = command("cmd", &["/C", "ping -n 6 127.0.0.1 >NUL"]);
        #[cfg(not(windows))]
        let mut definition = command("sh", &["-c", "sleep 5"]);
        definition.timeout = HumanDuration::from_std(Duration::from_millis(100));

        let mut budget = CaptureBudget::new();
        let outcome = run(
            &definition,
            &[],
            b"",
            false,
            &mut budget,
            CancellationToken::new(),
        )
        .await;
        assert!(outcome.error.is_some());
    }
}
