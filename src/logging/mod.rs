mod rotation;

use hodeco_contract::config::LogConfig;
use std::fmt;
use std::io;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    EnvFilter,
    fmt::{FmtContext, MakeWriter, format::FormatEvent, format::FormatFields, format::Writer},
    layer::SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
};

const LOG_CHANNEL_CAPACITY: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ServiceLogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

pub type ServiceLogger = fn(ServiceLogLevel, &str);

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("failed to open the log file: {0}")]
    File(#[from] std::io::Error),
    #[error("failed to start the log writer thread: {0}")]
    Thread(#[source] std::io::Error),
    #[error("failed to install the tracing subscriber: {0}")]
    Subscriber(#[from] tracing_subscriber::util::TryInitError),
}

enum LogCommand {
    Record(Vec<u8>),
    Flush(SyncSender<()>),
    Shutdown,
}

struct LoggingHandle {
    sender: SyncSender<LogCommand>,
    join: JoinHandle<()>,
}

static LOGGING: Mutex<Option<LoggingHandle>> = Mutex::new(None);

/// Installs the global tracing subscriber. Operational events are formatted on
/// the emitting thread and handed to a dedicated writer thread, so filesystem
/// I/O and rotation never block a Tokio worker.
pub fn init(config: &LogConfig, logger: ServiceLogger) -> Result<(), LoggingError> {
    let writer = rotation::RotatingWriter::new(config, logger)?;
    let (sender, receiver) = sync_channel(LOG_CHANNEL_CAPACITY);
    let join = thread::Builder::new()
        .name("hodeco-log".to_string())
        .spawn(move || run_writer(writer, receiver))
        .map_err(LoggingError::Thread)?;

    let channel_writer = ChannelWriter {
        sender: sender.clone(),
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if let Err(error) = tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(channel_writer)
                .with_ansi(false)
                .event_format(FileLogFormatter),
        )
        .try_init()
    {
        let _ = sender.send(LogCommand::Shutdown);
        let _ = join.join();
        return Err(error.into());
    }

    *LOGGING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(LoggingHandle { sender, join });

    Ok(())
}

/// Waits until every record queued before this call has been written.
pub fn flush() {
    let guard = LOGGING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(handle) = guard.as_ref() else {
        return;
    };

    let (reply, wait) = sync_channel(1);
    if handle.sender.send(LogCommand::Flush(reply)).is_ok() {
        // Do not hold the global lock while the writer thread flushes to disk;
        // a slow disk would otherwise block every other logging caller.
        drop(guard);
        let _ = wait.recv();
    }
}

/// Stops the dedicated writer thread and flushes pending records. Intended for
/// process shutdown; no further operational records are written afterwards.
pub fn shutdown() {
    let handle = LOGGING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(handle) = handle {
        let _ = handle.sender.send(LogCommand::Shutdown);
        let _ = handle.join.join();
    }
}

#[derive(Clone)]
struct ChannelWriter {
    sender: SyncSender<LogCommand>,
}

impl std::io::Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Never block a request, service, or Tokio worker on the log writer.
        // A full queue or a completed shutdown drops the record; the service
        // remains responsive while the writer catches up.
        match self.sender.try_send(LogCommand::Record(buf.to_vec())) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // Records are handed to the writer immediately, so there is nothing
        // buffered here to flush. Use `logging::flush` when a caller needs to
        // wait for the writer thread.
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for ChannelWriter {
    type Writer = ChannelWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn run_writer(mut writer: rotation::RotatingWriter, receiver: Receiver<LogCommand>) {
    while let Ok(command) = receiver.recv() {
        match command {
            LogCommand::Record(record) => {
                let _ = std::io::Write::write_all(&mut writer, &record);
            }
            LogCommand::Flush(reply) => {
                let _ = std::io::Write::flush(&mut writer);
                let _ = reply.send(());
            }
            LogCommand::Shutdown => break,
        }
    }

    let _ = std::io::Write::flush(&mut writer);
}

struct FileLogFormatter;

impl<S, N> FormatEvent<S, N> for FileLogFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let level = match *metadata.level() {
            Level::ERROR => 'E',
            Level::WARN => 'W',
            Level::INFO => 'I',
            Level::DEBUG => 'D',
            Level::TRACE => 'T',
        };

        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        // One level character and one separator: no fixed-width padding, so the
        // message starts immediately after a single space.
        write!(
            writer,
            "{} {} ",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            level
        )?;

        let message = visitor.message.unwrap_or_default();
        if !metadata.target().starts_with("hodeco") {
            if message.is_empty() {
                write!(writer, "{}", metadata.target())?;
            } else {
                write!(writer, "{}: {message}", metadata.target())?;
            }
        } else if !message.is_empty() {
            write!(writer, "{message}")?;
        } else {
            write!(writer, "{}", metadata.target())?;
        }

        for field in visitor.fields {
            write!(writer, " {field}")?;
        }
        writeln!(writer)
    }
}

#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    fields: Vec<String>,
}

impl FieldVisitor {
    fn push(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }
}
