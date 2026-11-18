//! Command catalog execution.
//!
//! Commands run as the service account. Task arguments are appended after the
//! catalog arguments. A single JSON document is written to stdin. Output is
//! captured only when requested and is capped per run.

use hodeco_contract::domain::CommandDefinition;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub const OUTPUT_CAPTURE_LIMIT: usize = 8 * 1024;

#[derive(Debug, Clone)]
pub struct CommandOutcome {
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
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

    pub fn remaining(&self) -> usize {
        self.remaining
    }

    pub fn take(&mut self, value: &mut String, text: &str) -> bool {
        if self.remaining == 0 {
            return false;
        }
        let take = text.len().min(self.remaining);
        self.remaining -= take;
        if take < text.len() {
            value.push_str(&text[..take]);
            true
        } else {
            value.push_str(text);
            false
        }
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
        process.stdout(Stdio::piped()).stderr(Stdio::piped());
    } else {
        process.stdout(Stdio::null()).stderr(Stdio::null());
    }
    process.kill_on_drop(true);

    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CommandOutcome {
                exit_code: None,
                stdout: None,
                stderr: None,
                output_truncated: false,
                error: Some(format!(
                    "failed to start '{}': {error}",
                    command.executable.display()
                )),
            };
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let input = input.to_vec();
        tokio::spawn(async move {
            // A command is free to exit without reading stdin.
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
        });
    }

    let remaining = Arc::new(AtomicUsize::new(budget.remaining()));
    let stdout_reader = child.stdout.take().map(|pipe| {
        let remaining = remaining.clone();
        tokio::spawn(read_limited(pipe, remaining))
    });
    let stderr_reader = child.stderr.take().map(|pipe| {
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
        // Drain readers so their tasks do not linger.
        if let Some(handle) = stdout_reader {
            let _ = handle.await;
        }
        if let Some(handle) = stderr_reader {
            let _ = handle.await;
        }
        return CommandOutcome {
            exit_code: None,
            stdout: None,
            stderr: None,
            output_truncated: budget.remaining() == 0,
            error: waited.err(),
        };
    }

    let (stdout, stdout_truncated) = join_output(stdout_reader).await;
    let (stderr, stderr_truncated) = join_output(stderr_reader).await;

    let stored = remaining.load(Ordering::Relaxed);
    budget.remaining = stored;
    CommandOutcome {
        exit_code: waited.ok().flatten(),
        stdout: capture.then_some(stdout),
        stderr: capture.then_some(stderr),
        output_truncated: budget.remaining() == 0 && (stdout_truncated || stderr_truncated),
        error: None,
    }
}

type LimitedOutput = Result<(String, bool), std::io::Error>;

async fn join_output(handle: Option<tokio::task::JoinHandle<LimitedOutput>>) -> (String, bool) {
    match handle {
        Some(handle) => match handle.await {
            Ok(Ok(output)) => output,
            _ => (String::new(), false),
        },
        None => (String::new(), false),
    }
}

async fn read_limited<R>(mut reader: R, remaining: Arc<AtomicUsize>) -> LimitedOutput
where
    R: AsyncRead + Unpin,
{
    let mut stored = String::new();
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
        stored.push_str(&String::from_utf8_lossy(&buffer[..take]));
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
        assert!(outcome.stdout.as_deref().unwrap_or("").contains("hello"));
        assert!(outcome.stderr.as_deref().unwrap_or("").contains("err"));
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
