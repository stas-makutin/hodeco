use crate::logging::{ServiceLogLevel, ServiceLogger};
use chrono::{Local, NaiveDate};
use hodeco_contract::config::{Archive, LogConfig};
use hodeco_contract::utils::FileMode;
use std::fs::{File, FileTimes, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const ERROR_REPORT_INTERVAL: Duration = Duration::from_secs(60);

/// A `tracing` writer that appends to the configured log file and rotates it
/// according to size and age limits. The dedicated log thread owns the writer,
/// so no synchronization is needed around the file state.
pub struct RotatingWriter {
    state: WriterState,
}

struct WriterState {
    settings: Settings,
    file: Option<File>,
    size: u64,
    logger: ServiceLogger,
    last_error: Option<Instant>,
    last_age_check: Option<Instant>,
}

struct Settings {
    dir: PathBuf,
    file: String,
    file_mode: FileMode,
    max_size: u64,
    max_age: Duration,
    backups: u32,
    backup_days: u32,
    archive: Archive,
}

struct HistoryFile {
    path: PathBuf,
    date: NaiveDate,
    ordinal: u64,
}

impl RotatingWriter {
    pub fn new(config: &LogConfig, logger: ServiceLogger) -> io::Result<Self> {
        let settings = Settings {
            dir: config.dir.clone(),
            file: config.file.clone(),
            file_mode: config.file_mode,
            max_size: config.max_size.as_bytes(),
            max_age: config.max_age.as_std(),
            backups: config.backups,
            backup_days: config.backup_days,
            archive: config.archive,
        };

        ensure_directory(&settings.dir, config.dir_mode)?;

        let active = settings.active_path();
        let file = OpenOptions::new().create(true).append(true).open(&active)?;
        set_file_mode(&active, settings.file_mode)?;
        let size = file.metadata()?.len();

        let state = WriterState {
            settings,
            file: Some(file),
            size,
            logger,
            last_error: None,
            last_age_check: None,
        };

        if !state.settings.max_age.is_zero() {
            state.ensure_status_file()?;
        }

        Ok(Self { state })
    }
}

impl Write for RotatingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.state.write_record(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.state.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

impl Settings {
    fn active_path(&self) -> PathBuf {
        self.dir.join(&self.file)
    }

    fn backup_path(&self) -> PathBuf {
        self.dir.join(format!("{}.backup", self.file))
    }

    fn status_path(&self) -> PathBuf {
        self.dir.join(format!("{}.status", self.file))
    }

    fn rotation_enabled(&self) -> bool {
        (self.backups > 0 || self.backup_days > 0) && (self.max_size > 0 || !self.max_age.is_zero())
    }
}

impl WriterState {
    fn write_record(&mut self, record: &[u8]) -> io::Result<()> {
        self.rotate_if_needed(record.len() as u64);

        if self.file.is_none()
            && let Err(error) = self.open_active()
        {
            self.report(&format!("unable to open log file: {error}"));
            return Err(error);
        }

        let file = self.file.as_mut().expect("log file opened above");
        match file.write_all(record) {
            Ok(()) => {
                self.size = self.size.saturating_add(record.len() as u64);
                Ok(())
            }
            Err(error) => {
                self.file = None;
                self.report(&format!("unable to write log record: {error}"));
                Err(error)
            }
        }
    }

    fn open_active(&mut self) -> io::Result<()> {
        let path = self.settings.active_path();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        set_file_mode(&path, self.settings.file_mode)?;
        let size = file.metadata()?.len();
        self.file = Some(file);
        self.size = size;
        Ok(())
    }

    fn rotate_if_needed(&mut self, incoming: u64) {
        if !self.settings.rotation_enabled() {
            return;
        }

        let age_due = self.age_exceeded();
        let size_due = self.settings.max_size > 0
            && self.size > 0
            && self.size.saturating_add(incoming) > self.settings.max_size;

        if age_due || size_due {
            self.rotate();
        }
    }

    fn age_exceeded(&mut self) -> bool {
        if self.settings.max_age.is_zero() {
            return false;
        }

        // Checking the status file on every record is expensive. One check per
        // second is enough for operational rotation while still honoring short
        // test/config durations.
        let check_interval = self.settings.max_age.min(Duration::from_secs(1));
        if let Some(last_check) = self.last_age_check
            && last_check.elapsed() < check_interval
        {
            return false;
        }
        self.last_age_check = Some(Instant::now());

        match std::fs::metadata(self.settings.status_path()) {
            Ok(metadata) => metadata
                .modified()
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age >= self.settings.max_age),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Err(error) = self.touch_status() {
                    self.report(&format!("unable to create log status file: {error}"));
                }
                false
            }
            Err(error) => {
                self.report(&format!("unable to read log status file: {error}"));
                false
            }
        }
    }

    fn ensure_status_file(&self) -> io::Result<()> {
        match std::fs::metadata(self.settings.status_path()) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => self.touch_status(),
            Err(error) => Err(error),
        }
    }

    fn touch_status(&self) -> io::Result<()> {
        let path = self.settings.status_path();
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        file.set_times(FileTimes::new().set_modified(SystemTime::now()))?;
        set_file_mode(&path, self.settings.file_mode)
    }

    fn rotate(&mut self) {
        let active = self.settings.active_path();
        let backup = self.settings.backup_path();

        // Close the active file before renaming it; Windows does not allow a
        // rename while the file is still open.
        self.file = None;

        if backup.exists() {
            // A previous rotation failed after renaming. Finalize that file
            // before starting a new rotation so no records are lost.
            if let Err(error) = self.finalize_backup(&backup) {
                self.report(&format!("unable to finalize pending backup: {error}"));
                self.reopen_active();
                return;
            }
        }

        match std::fs::rename(&active, &backup) {
            Ok(()) => {
                if !self.settings.max_age.is_zero() {
                    self.last_age_check = Some(Instant::now());
                    if let Err(error) = self.touch_status() {
                        self.report(&format!("unable to update log status file: {error}"));
                    }
                }

                if let Err(error) = self.finalize_backup(&backup) {
                    self.report(&format!("unable to finalize log backup: {error}"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                self.report(&format!("unable to rename log file: {error}"));
            }
        }

        self.reopen_active();
    }

    fn reopen_active(&mut self) {
        if let Err(error) = self.open_active() {
            self.report(&format!("unable to reopen log file: {error}"));
        }
    }

    fn finalize_backup(&mut self, backup: &Path) -> io::Result<()> {
        let today = Local::now().date_naive();
        let files = self.history_files()?;
        let ordinal = files
            .iter()
            .filter(|file| file.date == today)
            .map(|file| file.ordinal)
            .max()
            .map_or(0, |ordinal| ordinal.saturating_add(1));

        self.prune_history(&files, today);

        let base_name = history_name(&self.settings.file, today, ordinal);
        let history = self.settings.dir.join(match self.settings.archive {
            Archive::None => base_name.clone(),
            Archive::Zip => format!("{base_name}.zip"),
        });

        match self.settings.archive {
            Archive::None => {
                std::fs::rename(backup, &history)?;
                set_file_mode(&history, self.settings.file_mode)?;
            }
            Archive::Zip => {
                self.archive_backup(backup, &history, &base_name)?;
            }
        }

        Ok(())
    }

    fn history_files(&self) -> io::Result<Vec<HistoryFile>> {
        let prefix = format!("{}-", self.settings.file);
        let mut files = Vec::new();

        for entry in std::fs::read_dir(&self.settings.dir)? {
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }

            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            let Some(suffix) = file_name.strip_prefix(&prefix) else {
                continue;
            };
            let suffix = suffix.strip_suffix(".zip").unwrap_or(suffix);
            let Some((date, ordinal)) = parse_history_suffix(suffix) else {
                continue;
            };

            files.push(HistoryFile {
                path: entry.path(),
                date,
                ordinal,
            });
        }

        files.sort_by(|left, right| {
            right
                .date
                .cmp(&left.date)
                .then_with(|| right.ordinal.cmp(&left.ordinal))
        });

        Ok(files)
    }

    fn prune_history(&mut self, files: &[HistoryFile], today: NaiveDate) {
        if self.settings.backup_days == 0 {
            let keep = self.settings.backups.saturating_sub(1) as usize;
            let mut kept = 0usize;

            for file in files {
                if file.date > today || kept >= keep {
                    self.delete_history(&file.path);
                } else {
                    kept += 1;
                }
            }
            return;
        }

        let per_day = self.settings.backups.max(1) as usize;
        let older_dates_allowed = (self.settings.backup_days as usize).saturating_sub(1);
        let mut older_dates_kept = 0usize;
        let mut current_date = None;
        let mut current_allowed = false;
        let mut per_date_count = 0usize;

        for file in files {
            if file.date > today {
                self.delete_history(&file.path);
                continue;
            }

            if current_date != Some(file.date) {
                current_date = Some(file.date);
                per_date_count = 0;
                current_allowed = if file.date == today {
                    true
                } else if older_dates_kept < older_dates_allowed {
                    older_dates_kept += 1;
                    true
                } else {
                    false
                };
            }

            let limit = if file.date == today {
                per_day.saturating_sub(1)
            } else if current_allowed {
                per_day
            } else {
                0
            };

            if per_date_count < limit {
                per_date_count += 1;
            } else {
                self.delete_history(&file.path);
            }
        }
    }

    fn delete_history(&mut self, path: &Path) {
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != io::ErrorKind::NotFound
        {
            self.report(&format!("unable to remove '{}': {error}", path.display()));
        }
    }

    fn archive_backup(&mut self, backup: &Path, history: &Path, name: &str) -> io::Result<()> {
        let result = (|| -> io::Result<()> {
            let mut input = File::open(backup)?;
            let large_file = input.metadata()?.len() > u32::MAX as u64;
            let output = File::create(history)?;
            let mut zip = zip::ZipWriter::new(output);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(self.settings.file_mode.as_u32())
                .large_file(large_file);

            zip.start_file(name, options).map_err(io::Error::other)?;
            io::copy(&mut input, &mut zip)?;
            let mut output = zip.finish().map_err(io::Error::other)?;
            output.flush()?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                set_file_mode(history, self.settings.file_mode)?;
                std::fs::remove_file(backup)?;
                Ok(())
            }
            Err(error) => {
                let _ = std::fs::remove_file(history);
                Err(error)
            }
        }
    }

    fn report(&mut self, message: &str) {
        let now = Instant::now();
        if self
            .last_error
            .is_none_or(|last| now.duration_since(last) >= ERROR_REPORT_INTERVAL)
        {
            self.last_error = Some(now);
            (self.logger)(ServiceLogLevel::Error, &format!("log rotation: {message}"));
        }
    }
}

fn history_name(file: &str, date: NaiveDate, ordinal: u64) -> String {
    let date = date.format("%Y-%m-%d");
    if ordinal == 0 {
        format!("{file}-{date}")
    } else {
        format!("{file}-{date}_{ordinal}")
    }
}

fn parse_history_suffix(suffix: &str) -> Option<(NaiveDate, u64)> {
    let (date, ordinal) = match suffix.split_once('_') {
        Some((date, ordinal)) => (date, ordinal.parse().ok()?),
        None => (suffix, 0),
    };
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Some((date, ordinal))
}

fn ensure_directory(path: &Path, mode: FileMode) -> io::Result<()> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("'{}' is not a directory", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path)?;
            set_directory_mode(path, mode)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn set_directory_mode(path: &Path, mode: FileMode) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode.as_u32()))
}

#[cfg(not(unix))]
fn set_directory_mode(_path: &Path, _mode: FileMode) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(path: &Path, mode: FileMode) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode.as_u32()))
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _mode: FileMode) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodeco_contract::utils::{HumanDuration, Size};
    use std::io::Read;

    fn config(dir: &Path) -> LogConfig {
        LogConfig {
            dir: dir.to_path_buf(),
            file: "hodeco.log".to_string(),
            dir_mode: FileMode::new(0o750),
            file_mode: FileMode::new(0o640),
            max_size: Size::from_bytes(32),
            max_age: HumanDuration::default(),
            backups: 3,
            backup_days: 0,
            archive: Archive::None,
        }
    }

    fn ignored_logger(_: ServiceLogLevel, _: &str) {}

    fn today() -> String {
        Local::now().format("%Y-%m-%d").to_string()
    }

    #[test]
    fn rotates_by_size_before_exceeding_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.max_size = Size::from_bytes(20);
        let mut writer = RotatingWriter::new(&config, ignored_logger).unwrap();

        writer.write_all(b"first record\n").unwrap();
        writer.write_all(b"second record\n").unwrap();

        let history = dir.path().join(format!("hodeco.log-{}", today()));
        assert_eq!(std::fs::read(&history).unwrap(), b"first record\n");
        assert_eq!(
            std::fs::read(dir.path().join("hodeco.log")).unwrap(),
            b"second record\n"
        );
    }

    #[test]
    fn keeps_only_the_configured_number_of_backups() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.max_size = Size::from_bytes(8);
        config.backups = 2;
        let mut writer = RotatingWriter::new(&config, ignored_logger).unwrap();

        for _ in 0..6 {
            writer.write_all(b"1234567\n").unwrap();
        }

        let history: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("hodeco.log-")
            })
            .collect();

        assert_eq!(history.len(), 2);
        assert!(history.iter().any(|entry| {
            entry.file_name().to_string_lossy() == format!("hodeco.log-{}_3", today())
        }));
    }

    #[test]
    fn rotates_by_age_using_the_status_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.max_size = Size::from_bytes(0);
        config.max_age = HumanDuration::from_std(Duration::from_millis(1));
        config.backups = 1;
        let mut writer = RotatingWriter::new(&config, ignored_logger).unwrap();

        let status = dir.path().join("hodeco.log.status");
        std::thread::sleep(Duration::from_millis(5));
        writer.write_all(b"after the age limit\n").unwrap();

        assert!(status.is_file());
        assert!(dir.path().join(format!("hodeco.log-{}", today())).is_file());
    }

    #[test]
    fn archives_backups_and_removes_the_backup_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.max_size = Size::from_bytes(20);
        config.archive = Archive::Zip;
        let mut writer = RotatingWriter::new(&config, ignored_logger).unwrap();

        writer.write_all(b"first record\n").unwrap();
        writer.write_all(b"second record\n").unwrap();

        let archive = dir.path().join(format!("hodeco.log-{}.zip", today()));
        assert!(archive.is_file());
        assert!(!dir.path().join("hodeco.log.backup").exists());

        let mut zip = zip::ZipArchive::new(File::open(archive).unwrap()).unwrap();
        let mut contents = String::new();
        zip.by_name(&format!("hodeco.log-{}", today()))
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "first record\n");
    }

    #[test]
    fn deletes_future_dated_history_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.max_size = Size::from_bytes(20);
        let mut writer = RotatingWriter::new(&config, ignored_logger).unwrap();
        let future = Local::now().date_naive() + chrono::Days::new(1);
        let future_path = dir
            .path()
            .join(format!("hodeco.log-{}", future.format("%Y-%m-%d")));
        std::fs::write(&future_path, b"future").unwrap();

        writer.write_all(b"first record\n").unwrap();
        writer.write_all(b"second record\n").unwrap();

        assert!(!future_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn applies_directory_and_file_modes() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("log");
        let mut config = config(&dir);
        config.max_size = Size::from_bytes(8);
        config.backups = 2;
        let mut writer = RotatingWriter::new(&config, ignored_logger).unwrap();
        writer.write_all(b"first\n").unwrap();
        writer.write_all(b"second\n").unwrap();

        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o750);
        assert_eq!(mode(&dir.join("hodeco.log")), 0o640);
        assert_eq!(mode(&dir.join(format!("hodeco.log-{}", today()))), 0o640);
    }
}
