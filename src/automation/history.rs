//! Per-task run history stored in per-task files.
//!
//! Run details are encoded compactly rather than as JSON. The storage format
//! is private to this module.

use chrono::{DateTime, TimeZone, Utc};
use hodeco_contract::api::payloads::{TaskStoreTotal, TasksHistoryParams, TasksHistoryResult};
use hodeco_contract::config::TaskLogConfig;
use hodeco_contract::domain::{
    ActionOutcome, HttpMethod, MessageState, NextCondition, NextTaskRecord, RunAction, RunChain,
    RunCommandAction, RunError, RunMessageAction, RunOutcome, RunStage, RunSummary, RunTrigger,
    SelectorOutcome, SelectorResult, ServiceRef, TaskHistoryConfig, TaskRun, TriggerRef,
    TriggerType,
};
use hodeco_contract::utils::{Base64Bytes, FileMode};
use parking_lot::RwLock;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

const MAGIC: [u8; 4] = *b"TLOG";
const VERSION: u8 = 1;
const RESERVED: [u8; 3] = [0; 3];
const HEADER_LEN: usize = 4 + 1 + 3 + 8;
const RECORD_OVERHEAD: usize = 1 + 4;
const MAX_RECORD_LEN: usize = 64 * 1024 * 1024;

const RUN_CORE: u16 = 0x0001;
const CHAIN: u16 = 0x0002;
const TRIGGER_MESSAGE: u16 = 0x0010;
const TRIGGER_SCHEDULE: u16 = 0x0011;
const TRIGGER_HTTP: u16 = 0x0012;
const TRIGGER_TASK: u16 = 0x0014;
const TRIGGER_MANUAL: u16 = 0x0015;
const ACTION_COMMAND: u16 = 0x0030;
const ACTION_MESSAGE: u16 = 0x0031;
const SELECTOR_RESULT: u16 = 0x0040;
const NEXT_TASKS: u16 = 0x0041;
const ERROR: u16 = 0x0050;
const TASK_RUN: u8 = 0x10;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("unable to access task history: {0}")]
    Io(#[from] io::Error),
    #[error("task history path '{0}' does not name a file")]
    InvalidPath(PathBuf),
    #[error("the task history is corrupt: {0}")]
    Corrupt(String),
}

#[derive(Debug, Clone, Default)]
pub struct HistoryLoadReport {
    pub loaded: usize,
    pub orphans_removed: Vec<PathBuf>,
    pub corrupt: Vec<(PathBuf, String)>,
}

fn blocking_error(error: tokio::task::JoinError) -> HistoryError {
    HistoryError::Io(io::Error::other(format!(
        "task history worker failed: {error}"
    )))
}

pub struct HistoryStore {
    config: TaskLogConfig,
    root: PathBuf,
    tasks: RwLock<BTreeMap<String, TaskHistoryMeta>>,
    limits: RwLock<BTreeMap<String, HistoryLimits>>,
    mutation: tokio::sync::RwLock<()>,
}

#[derive(Debug, Default, Clone)]
struct TaskHistoryMeta {
    rows: u64,
    bytes: u64,
    last: Option<RunSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HistoryLimits {
    max_rows: u32,
    max_size: u64,
}

impl HistoryStore {
    pub fn new(config: TaskLogConfig, root: PathBuf) -> Self {
        Self {
            config,
            root,
            tasks: RwLock::new(BTreeMap::new()),
            limits: RwLock::new(BTreeMap::new()),
            mutation: tokio::sync::RwLock::new(()),
        }
    }

    fn effective_limits(&self, config: &TaskHistoryConfig) -> HistoryLimits {
        HistoryLimits {
            max_rows: if config.max_rows == 0 {
                u32::MAX
            } else {
                config.max_rows
            },
            max_size: if config.max_size.as_bytes() == 0 {
                100 * 1024
            } else {
                config.max_size.as_bytes()
            },
        }
    }

    fn limits_for(&self, task_id: &str) -> HistoryLimits {
        self.limits
            .read()
            .get(task_id)
            .copied()
            .unwrap_or(HistoryLimits {
                max_rows: u32::MAX,
                max_size: 100 * 1024,
            })
    }

    fn set_task_history(&self, task_id: &str, config: &TaskHistoryConfig) {
        let limits = self.effective_limits(config);
        let mut current = self.limits.write();
        if current.get(task_id) != Some(&limits) {
            current.insert(task_id.to_string(), limits);
        }
    }

    /// Loads task file metadata with the default per-task history config.
    #[cfg(test)]
    pub fn load(&self, task_ids: &BTreeSet<String>) -> Result<HistoryLoadReport, HistoryError> {
        let history = task_ids
            .iter()
            .map(|task_id| (task_id.clone(), TaskHistoryConfig::default()))
            .collect();
        self.load_with_history(&history)
    }

    /// Loads task file metadata and removes orphan files. Run rows are decoded
    /// only long enough to trim a file that exceeds its row limit. This performs
    /// blocking file I/O; call it from a blocking worker at runtime.
    pub fn load_with_history(
        &self,
        history: &BTreeMap<String, TaskHistoryConfig>,
    ) -> Result<HistoryLoadReport, HistoryError> {
        fs::create_dir_all(&self.root)?;
        let mut report = HistoryLoadReport::default();
        let mut tasks = BTreeMap::new();
        let mut limits = BTreeMap::new();

        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("tlog") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some(config) = history.get(stem) else {
                fs::remove_file(&path)?;
                report.orphans_removed.push(path);
                continue;
            };
            let effective = self.effective_limits(config);
            limits.insert(stem.to_string(), effective);
            match load_file(&path) {
                Ok(mut runs) => {
                    let max_rows = effective.max_rows as usize;
                    if runs.len() > max_rows {
                        let excess = runs.len() - max_rows;
                        runs.drain(0..excess);
                        let encoded = encode_file(&runs)?;
                        write_atomic(&path, &encoded, Some(self.config.file_mode))?;
                    }
                    let meta = TaskHistoryMeta {
                        rows: runs.len() as u64,
                        bytes: fs::metadata(&path)?.len(),
                        last: runs.last().map(|run| run.summary.clone()),
                    };
                    report.loaded += meta.rows as usize;
                    tasks.insert(stem.to_string(), meta);
                }
                Err(error) => {
                    report.corrupt.push((path.clone(), error.to_string()));
                    match self.config.on_corrupt {
                        hodeco_contract::config::CorruptPolicy::Ignore => {
                            let _ = fs::remove_file(&path);
                            tasks.insert(stem.to_string(), TaskHistoryMeta::default());
                        }
                        hodeco_contract::config::CorruptPolicy::Fail => return Err(error),
                    }
                }
            }
        }

        for (task_id, config) in history {
            tasks.entry(task_id.clone()).or_default();
            limits.insert(task_id.clone(), self.effective_limits(config));
        }
        *self.tasks.write() = tasks;
        *self.limits.write() = limits;
        Ok(report)
    }

    /// Appends an already encoded completed run and updates metadata. The run
    /// itself is not retained in memory.
    fn append(
        &self,
        run: Arc<TaskRun>,
        record: Vec<u8>,
        history: &TaskHistoryConfig,
    ) -> Result<(), HistoryError> {
        let task_id = run.summary.task_id.clone();
        self.set_task_history(&task_id, history);
        let path = self.root.join(format!("{task_id}.tlog"));
        append_record(&path, TASK_RUN, &record)?;
        // Query file metadata before taking the write lock so async readers are
        // never blocked on a filesystem call.
        let bytes = fs::metadata(&path).map(|value| value.len()).ok();
        let rows = {
            let mut tasks = self.tasks.write();
            let meta = tasks.entry(task_id.clone()).or_default();
            meta.rows = meta.rows.saturating_add(1);
            if let Some(bytes) = bytes {
                meta.bytes = bytes;
            }
            meta.last = Some(run.summary.clone());
            meta.rows
        };
        if rows > self.limits_for(&task_id).max_rows as u64 {
            self.compact(&task_id)?;
        } else {
            self.maybe_compact(&task_id)?;
        }
        Ok(())
    }

    fn maybe_compact(&self, task_id: &str) -> Result<(), HistoryError> {
        let path = self.root.join(format!("{task_id}.tlog"));
        let size = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(HistoryError::Io(error)),
        };
        let limits = self.limits_for(task_id);
        let threshold = (limits.max_size as f64 * self.config.compaction_factor.max(1.0)) as u64;
        let too_large = size > threshold;
        let too_many = self
            .tasks
            .read()
            .get(task_id)
            .is_some_and(|meta| meta.rows > limits.max_rows as u64);
        let stale = file_age_exceeds(&path, self.config.compact_interval.as_std());
        if too_large || too_many || stale {
            self.compact(task_id)?;
        }
        Ok(())
    }

    fn compact(&self, task_id: &str) -> Result<(), HistoryError> {
        let path = self.root.join(format!("{task_id}.tlog"));
        let mut runs = load_file(&path)?;
        let max_rows = self.limits_for(task_id).max_rows as usize;
        if runs.len() > max_rows {
            let excess = runs.len() - max_rows;
            runs.drain(0..excess);
        }
        let encoded = encode_file(&runs)?;
        write_atomic(&path, &encoded, Some(self.config.file_mode))?;
        if let Some(meta) = self.tasks.write().get_mut(task_id) {
            meta.rows = runs.len() as u64;
            meta.bytes = encoded.len() as u64;
            meta.last = runs.last().map(|run| run.summary.clone());
        }
        Ok(())
    }

    /// Serializes appends from async callers. The actual file write runs on a
    /// blocking worker.
    pub async fn append_async(
        self: std::sync::Arc<Self>,
        run: Arc<TaskRun>,
        record: Vec<u8>,
        history: TaskHistoryConfig,
    ) -> Result<(), HistoryError> {
        let this = self.clone();
        let _guard = self.mutation.write().await;
        tokio::task::spawn_blocking(move || this.append(run, record, &history))
            .await
            .map_err(blocking_error)?
    }

    pub async fn get(
        self: &Arc<Self>,
        task_id: &str,
        run_id: Uuid,
    ) -> Result<Option<TaskRun>, HistoryError> {
        let _guard = self.mutation.read().await;
        let this = self.clone();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || this.get_blocking(&task_id, run_id))
            .await
            .map_err(blocking_error)?
    }

    fn get_blocking(&self, task_id: &str, run_id: Uuid) -> Result<Option<TaskRun>, HistoryError> {
        if !self.tasks.read().contains_key(task_id) {
            return Ok(None);
        }
        Ok(load_file(&self.root.join(format!("{task_id}.tlog")))?
            .into_iter()
            .find(|run| run.summary.id == run_id))
    }

    pub fn last_run(&self, task_id: &str) -> Option<RunSummary> {
        self.tasks
            .read()
            .get(task_id)
            .and_then(|meta| meta.last.clone())
    }

    pub async fn list(
        self: &Arc<Self>,
        params: &TasksHistoryParams,
    ) -> Result<TasksHistoryResult, HistoryError> {
        let _guard = self.mutation.read().await;
        let this = self.clone();
        let params = params.clone();
        tokio::task::spawn_blocking(move || this.list_blocking(&params))
            .await
            .map_err(blocking_error)?
    }

    fn list_blocking(
        &self,
        params: &TasksHistoryParams,
    ) -> Result<TasksHistoryResult, HistoryError> {
        let task_filter: BTreeSet<&str> = params.task_ids.iter().map(String::as_str).collect();
        let trigger_filter: BTreeSet<&str> =
            params.trigger_ids.iter().map(String::as_str).collect();
        let outcomes: BTreeSet<RunOutcome> = params.outcomes.iter().copied().collect();
        let types: BTreeSet<TriggerType> = params.types.iter().copied().collect();

        let metadata: Vec<(String, TaskHistoryMeta)> = self
            .tasks
            .read()
            .iter()
            .map(|(task_id, meta)| (task_id.clone(), meta.clone()))
            .collect();

        let mut selected: Vec<RunSummary> = Vec::new();
        let mut store_totals = Vec::new();
        for (task_id, meta) in metadata {
            if !task_filter.is_empty() && !task_filter.contains(task_id.as_str()) {
                continue;
            }
            store_totals.push(TaskStoreTotal {
                task_id: task_id.clone(),
                rows: meta.rows,
                bytes: meta.bytes,
            });
            scan_summaries(&self.root.join(format!("{task_id}.tlog")), |summary| {
                if !trigger_filter.is_empty()
                    && !trigger_filter.contains(summary.trigger.id.as_str())
                {
                    return;
                }
                if !types.is_empty() && !types.contains(&summary.trigger.kind) {
                    return;
                }
                if !outcomes.is_empty() && !outcomes.contains(&summary.outcome) {
                    return;
                }
                selected.push(summary);
            })?;
        }

        let (runs, next) = super::run_list::page_runs(
            selected,
            params.from,
            params.until,
            params.order,
            super::run_list::page_limit(params.limit),
        );
        Ok(TasksHistoryResult {
            runs,
            next,
            store_totals,
        })
    }

    pub async fn purge(self: &Arc<Self>, task_id: &str) -> Result<(), HistoryError> {
        let _guard = self.mutation.write().await;
        let this = self.clone();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || this.purge_blocking(&task_id))
            .await
            .map_err(blocking_error)?
    }

    fn purge_blocking(&self, task_id: &str) -> Result<(), HistoryError> {
        let path = self.root.join(format!("{task_id}.tlog"));
        let encoded = encode_file(&[])?;
        write_atomic(&path, &encoded, Some(self.config.file_mode))?;
        self.tasks
            .write()
            .insert(task_id.to_string(), TaskHistoryMeta::default());
        Ok(())
    }

    pub async fn remove_task(self: &Arc<Self>, task_id: &str) -> Result<(), HistoryError> {
        let _guard = self.mutation.write().await;
        let this = self.clone();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || this.remove_task_blocking(&task_id))
            .await
            .map_err(blocking_error)?
    }

    fn remove_task_blocking(&self, task_id: &str) -> Result<(), HistoryError> {
        let path = self.root.join(format!("{task_id}.tlog"));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(HistoryError::Io(error)),
        }
        self.tasks.write().remove(task_id);
        self.limits.write().remove(task_id);
        Ok(())
    }

    pub fn latest_time(&self) -> Option<DateTime<Utc>> {
        self.tasks
            .read()
            .values()
            .filter_map(|meta| meta.last.as_ref().map(|run| run.time))
            .max()
    }
}

fn file_age_exceeds(path: &Path, interval: std::time::Duration) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    modified.elapsed().is_ok_and(|elapsed| elapsed > interval)
}

fn read_header(data: &[u8]) -> Result<(i64, usize), HistoryError> {
    if data.len() < HEADER_LEN {
        return Err(HistoryError::Corrupt(
            "the task history file is incomplete".to_string(),
        ));
    }
    if data[..4] != MAGIC {
        return Err(HistoryError::Corrupt(
            "the task history file is not recognized".to_string(),
        ));
    }
    if data[4] != VERSION {
        return Err(HistoryError::Corrupt(
            "the task history file uses an unsupported format".to_string(),
        ));
    }
    let compacted_at = i64::from_le_bytes(data[8..16].try_into().unwrap());
    Ok((compacted_at, HEADER_LEN))
}

fn encode_header(compacted_at: i64) -> Vec<u8> {
    let mut data = Vec::with_capacity(HEADER_LEN);
    data.extend_from_slice(&MAGIC);
    data.push(VERSION);
    data.extend_from_slice(&RESERVED);
    data.extend_from_slice(&compacted_at.to_le_bytes());
    data
}

fn encode_file(runs: &[TaskRun]) -> Result<Vec<u8>, HistoryError> {
    let mut data = encode_header(Utc::now().timestamp_nanos_opt().unwrap_or(0));
    for run in runs {
        let record = encode_run(run)?;
        data.push(TASK_RUN);
        data.extend_from_slice(&(record.len() as u32).to_le_bytes());
        data.extend_from_slice(&record);
    }
    Ok(data)
}

fn load_file(path: &Path) -> Result<Vec<TaskRun>, HistoryError> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(HistoryError::Io(error)),
    };
    let (_, mut offset) = read_header(&data)?;
    let mut runs = Vec::new();
    while offset < data.len() {
        if offset + RECORD_OVERHEAD > data.len() {
            break;
        }
        let record_type = data[offset];
        let length = u32::from_le_bytes(data[offset + 1..offset + 5].try_into().unwrap()) as usize;
        offset += RECORD_OVERHEAD;
        if offset + length > data.len() {
            break;
        }
        let body = &data[offset..offset + length];
        offset += length;
        if record_type == TASK_RUN {
            runs.push(decode_run(body)?);
        }
    }
    Ok(runs)
}

fn scan_summaries(path: &Path, mut visit: impl FnMut(RunSummary)) -> Result<(), HistoryError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(HistoryError::Io(error)),
    };
    let mut reader = io::BufReader::new(file);
    let mut header = [0u8; HEADER_LEN];
    if let Err(error) = reader.read_exact(&mut header) {
        return match error.kind() {
            io::ErrorKind::UnexpectedEof => Err(HistoryError::Corrupt(
                "the task history file is incomplete".to_string(),
            )),
            _ => Err(HistoryError::Io(error)),
        };
    }
    read_header(&header)?;

    let mut overhead = [0u8; RECORD_OVERHEAD];
    loop {
        match reader.read_exact(&mut overhead) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(HistoryError::Io(error)),
        }
        let record_type = overhead[0];
        let length = u32::from_le_bytes(overhead[1..5].try_into().unwrap()) as usize;
        if length > MAX_RECORD_LEN {
            return Err(HistoryError::Corrupt(
                "a task history record is too large".to_string(),
            ));
        }
        let mut body = vec![0u8; length];
        match reader.read_exact(&mut body) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(HistoryError::Io(error)),
        }
        if record_type == TASK_RUN {
            visit(decode_summary(&body)?);
        }
    }
    Ok(())
}

fn decode_summary(body: &[u8]) -> Result<RunSummary, HistoryError> {
    let mut reader = Reader::new(body);
    let block_count = reader.u16()? as usize;
    let mut summary = None;
    for _ in 0..block_count {
        let kind = reader.u16()?;
        let length = reader.u32()? as usize;
        let block = reader.bytes(length)?;
        if kind == RUN_CORE {
            if summary.is_some() {
                return Err(HistoryError::Corrupt("duplicate RUN_CORE".to_string()));
            }
            summary = Some(decode_core(block)?);
        }
    }
    if !reader.finished() {
        return Err(HistoryError::Corrupt(
            "unexpected data after the block list".to_string(),
        ));
    }
    summary.ok_or_else(|| HistoryError::Corrupt("missing RUN_CORE".to_string()))
}

fn append_record(path: &Path, record_type: u8, body: &[u8]) -> Result<(), HistoryError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        // Ignore a partial trailing record, but truncate it before appending so
        // the next frame does not start inside it.
        let data = fs::read(path)?;
        let (_, mut offset) = read_header(&data)?;
        while offset + RECORD_OVERHEAD <= data.len() {
            let length =
                u32::from_le_bytes(data[offset + 1..offset + 5].try_into().unwrap()) as usize;
            let end = offset + RECORD_OVERHEAD + length;
            if end > data.len() {
                break;
            }
            offset = end;
        }
        if offset < data.len() {
            let file = fs::OpenOptions::new().write(true).open(path)?;
            file.set_len(offset as u64)?;
        }
    }
    let file_exists = path.exists();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if !file_exists {
        let header = encode_header(Utc::now().timestamp_nanos_opt().unwrap_or(0));
        file.write_all(&header)?;
    }
    file.write_all(&[record_type])?;
    file.write_all(&(body.len() as u32).to_le_bytes())?;
    file.write_all(body)?;
    file.flush()?;
    file.sync_data()?;
    Ok(())
}

fn write_atomic(path: &Path, data: &[u8], mode: Option<FileMode>) -> Result<(), HistoryError> {
    if path.file_name().is_none() {
        return Err(HistoryError::InvalidPath(path.to_path_buf()));
    }
    crate::utils::write_atomic(path, data, mode).map_err(HistoryError::Io)
}

// ---------------------------------------------------------------------------
// Typed blocks
// ---------------------------------------------------------------------------

pub(crate) fn encode_run(run: &TaskRun) -> Result<Vec<u8>, HistoryError> {
    let mut blocks: Vec<(u16, Vec<u8>)> = Vec::new();

    let mut core = Writer::default();
    core.uuid(run.summary.id);
    core.string(&run.summary.task_id);
    core.time(run.summary.time);
    core.time(run.summary.started_at);
    core.time(run.summary.finished_at);
    core.string(&run.summary.trigger.id);
    core.u8(trigger_type_code(run.summary.trigger.kind));
    core.u8(outcome_code(run.summary.outcome));
    core.u16(
        u16::try_from(run.summary.action_count)
            .map_err(|_| HistoryError::Corrupt("too many actions".to_string()))?,
    );
    core.u16(
        u16::try_from(run.summary.next_count)
            .map_err(|_| HistoryError::Corrupt("too many next tasks".to_string()))?,
    );
    core.opt_string(run.summary.error.as_deref());
    core.opt_time(run.summary.delayed_until);
    blocks.push((RUN_CORE, core.finish()));

    let mut chain = Writer::default();
    chain.uuid(run.chain.chain_id);
    chain.opt_uuid(run.chain.parent_run_id);
    chain.u32(run.chain.depth);
    blocks.push((CHAIN, chain.finish()));

    match &run.trigger {
        RunTrigger::Message(trigger) => {
            let mut block = Writer::default();
            block.string(&trigger.id);
            block.uuid(trigger.message_id);
            block.time(trigger.time);
            block.service_ref(&trigger.service);
            block.string(&trigger.protocol);
            block.string(&trigger.transport);
            block.string(&trigger.entry);
            block.u8(message_state_code(trigger.state));
            block.opt_bytes(trigger.payload.as_ref().map(Base64Bytes::as_slice));
            blocks.push((TRIGGER_MESSAGE, block.finish()));
        }
        RunTrigger::Schedule(trigger) => {
            let mut block = Writer::default();
            block.string(&trigger.id);
            block.string(&trigger.schedule_id);
            block.time(trigger.occurrence_at);
            blocks.push((TRIGGER_SCHEDULE, block.finish()));
        }
        RunTrigger::Http(trigger) => {
            let mut block = Writer::default();
            block.string(&trigger.id);
            block.u8(http_method_code(trigger.method));
            block.string(&trigger.path);
            block.opt_string(trigger.query.as_deref());
            block.opt_string(trigger.remote.as_deref());
            block.string(&serde_json::to_string(&trigger.headers).unwrap_or_else(|_| "{}".into()));
            block.opt_bytes(trigger.payload.as_ref().map(Base64Bytes::as_slice));
            blocks.push((TRIGGER_HTTP, block.finish()));
        }
        RunTrigger::Manual(trigger) => {
            let mut block = Writer::default();
            block.string(&trigger.id);
            block.opt_bytes(trigger.payload.as_ref().map(Base64Bytes::as_slice));
            block.string(&serde_json::to_string(&trigger.metadata).unwrap_or_else(|_| "{}".into()));
            blocks.push((TRIGGER_MANUAL, block.finish()));
        }
        RunTrigger::Task(trigger) => {
            let mut block = Writer::default();
            block.string(&trigger.id);
            block.uuid(trigger.parent_run_id);
            block.uuid(trigger.chain_id);
            block.u32(trigger.depth);
            block.u8(next_condition_code(trigger.rule));
            block.opt_bytes(trigger.payload.as_ref().map(Base64Bytes::as_slice));
            blocks.push((TRIGGER_TASK, block.finish()));
        }
    }

    for action in &run.actions {
        let mut block = Writer::default();
        block.string(&action.id);
        block.time(action.started_at);
        block.time(action.finished_at);
        block.u8(action_status_code(action.status));
        match (&action.command, &action.message) {
            (Some(command), None) => {
                block.u8(0);
                block.string(&command.command_id);
                block.string(&command.executable);
                block.strings(&command.args);
                block.opt_i32(command.exit_code);
                block.opt_bytes(command.stdout.as_ref().map(Base64Bytes::as_slice));
                // Retained as an empty legacy field so existing records
                // keep the same block layout. New runs never capture stderr.
                block.opt_bytes(None);
                block.u8(u8::from(command.output_truncated));
                block.opt_string(action.error.as_deref());
                blocks.push((ACTION_COMMAND, block.finish()));
            }
            (None, Some(message)) => {
                block.u8(1);
                block.service_ref(&message.service);
                block.bytes(message.payload.as_slice());
                block.opt_uuid(message.message_id);
                block.opt_u64(message.ttl.map(|ttl| ttl.as_std().as_secs()));
                block.opt_string(action.error.as_deref());
                blocks.push((ACTION_MESSAGE, block.finish()));
            }
            _ => {
                return Err(HistoryError::Corrupt(
                    "an action has no result kind".to_string(),
                ));
            }
        }
    }

    if let Some(selector) = &run.selector {
        let mut block = Writer::default();
        block.string(&selector.command_id);
        block.string(&selector.executable);
        block.strings(&selector.args);
        block.opt_i32(selector.exit_code);
        block.u8(selector_outcome_code(selector.outcome));
        block.opt_string(selector.error.as_deref());
        blocks.push((SELECTOR_RESULT, block.finish()));
    }

    if !run.next_tasks.is_empty() {
        let mut block = Writer::default();
        block.u16(
            u16::try_from(run.next_tasks.len())
                .map_err(|_| HistoryError::Corrupt("too many next tasks".to_string()))?,
        );
        for next in &run.next_tasks {
            block.string(&next.task_id);
            block.uuid(next.run_id);
            block.u8(next_condition_code(next.rule));
            block.opt_u8(next.selector_exit_code);
        }
        blocks.push((NEXT_TASKS, block.finish()));
    }

    if let Some(error) = &run.error {
        let mut block = Writer::default();
        block.u8(run_stage_code(error.stage));
        block.string(&error.message);
        blocks.push((ERROR, block.finish()));
    }

    let mut body = Writer::default();
    body.u16(
        u16::try_from(blocks.len())
            .map_err(|_| HistoryError::Corrupt("too many blocks".to_string()))?,
    );
    for (kind, block) in blocks {
        body.u16(kind);
        body.u32(
            u32::try_from(block.len())
                .map_err(|_| HistoryError::Corrupt("block is too large".to_string()))?,
        );
        body.raw(&block);
    }
    Ok(body.finish())
}

fn decode_run(body: &[u8]) -> Result<TaskRun, HistoryError> {
    let mut reader = Reader::new(body);
    let block_count = reader.u16()? as usize;
    let mut blocks = Vec::with_capacity(block_count);
    for _ in 0..block_count {
        let kind = reader.u16()?;
        let length = reader.u32()? as usize;
        blocks.push((kind, reader.bytes(length)?.to_vec()));
    }
    if !reader.finished() {
        return Err(HistoryError::Corrupt(
            "unexpected data after the block list".to_string(),
        ));
    }

    let mut summary = None;
    let mut chain = None;
    let mut trigger = None;
    let mut actions = Vec::new();
    let mut selector = None;
    let mut next_tasks = Vec::new();
    let mut error = None;
    let mut saw_selector = false;
    let mut saw_next = false;
    let mut saw_error = false;

    for (kind, block) in blocks {
        match kind {
            RUN_CORE => {
                if summary.is_some() {
                    return Err(HistoryError::Corrupt("duplicate RUN_CORE".to_string()));
                }
                summary = Some(decode_core(&block)?);
            }
            CHAIN => {
                if chain.is_some() {
                    return Err(HistoryError::Corrupt("duplicate CHAIN".to_string()));
                }
                chain = Some(decode_chain(&block)?);
            }
            TRIGGER_MESSAGE | TRIGGER_SCHEDULE | TRIGGER_HTTP | TRIGGER_TASK | TRIGGER_MANUAL => {
                if trigger.is_some() {
                    return Err(HistoryError::Corrupt("duplicate trigger block".to_string()));
                }
                trigger = Some(decode_trigger(kind, &block)?);
            }
            ACTION_COMMAND | ACTION_MESSAGE => {
                actions.push(decode_action(kind, &block)?);
            }
            SELECTOR_RESULT => {
                if saw_selector {
                    return Err(HistoryError::Corrupt(
                        "duplicate SELECTOR_RESULT".to_string(),
                    ));
                }
                saw_selector = true;
                selector = Some(decode_selector(&block)?);
            }
            NEXT_TASKS => {
                if saw_next {
                    return Err(HistoryError::Corrupt("duplicate NEXT_TASKS".to_string()));
                }
                saw_next = true;
                next_tasks = decode_next_tasks(&block)?;
            }
            ERROR => {
                if saw_error {
                    return Err(HistoryError::Corrupt("duplicate ERROR".to_string()));
                }
                saw_error = true;
                error = Some(decode_error(&block)?);
            }
            _ => {}
        }
    }

    Ok(TaskRun {
        summary: summary.ok_or_else(|| HistoryError::Corrupt("missing RUN_CORE".to_string()))?,
        trigger: trigger
            .ok_or_else(|| HistoryError::Corrupt("missing trigger block".to_string()))?,
        actions,
        selector,
        next_tasks,
        chain: chain.ok_or_else(|| HistoryError::Corrupt("missing CHAIN".to_string()))?,
        error,
    })
}

fn decode_core(body: &[u8]) -> Result<RunSummary, HistoryError> {
    let mut reader = Reader::new(body);
    Ok(RunSummary {
        id: reader.uuid()?,
        task_id: reader.string()?,
        time: reader.time()?,
        started_at: reader.time()?,
        finished_at: reader.time()?,
        trigger: TriggerRef {
            id: reader.string()?,
            kind: trigger_type_from_code(reader.u8()?)?,
        },
        outcome: outcome_from_code(reader.u8()?)?,
        action_count: reader.u16()? as u32,
        next_count: reader.u16()? as u32,
        error: reader.opt_string()?,
        delayed_until: reader.opt_time_tail()?,
    })
}

fn decode_chain(body: &[u8]) -> Result<RunChain, HistoryError> {
    let mut reader = Reader::new(body);
    Ok(RunChain {
        chain_id: reader.uuid()?,
        parent_run_id: reader.opt_uuid()?,
        depth: reader.u32()?,
    })
}

fn decode_trigger(kind: u16, body: &[u8]) -> Result<RunTrigger, HistoryError> {
    let mut reader = Reader::new(body);
    match kind {
        TRIGGER_MESSAGE => {
            let id = reader.string()?;
            let message_id = reader.uuid()?;
            let time = reader.time()?;
            let service = reader.service_ref()?;
            let protocol = reader.string()?;
            let transport = reader.string()?;
            let entry = reader.string()?;
            let state = message_state_from_code(reader.u8()?)?;
            let payload = reader.opt_bytes()?.map(Base64Bytes::from);
            Ok(RunTrigger::Message(
                hodeco_contract::domain::MessageRunTrigger {
                    id,
                    message_id,
                    time,
                    service,
                    protocol,
                    transport,
                    entry,
                    state,
                    payload,
                },
            ))
        }
        TRIGGER_SCHEDULE => Ok(RunTrigger::Schedule(
            hodeco_contract::domain::ScheduleRunTrigger {
                id: reader.string()?,
                schedule_id: reader.string()?,
                occurrence_at: reader.time()?,
            },
        )),
        TRIGGER_HTTP => Ok(RunTrigger::Http(hodeco_contract::domain::HttpRunTrigger {
            id: reader.string()?,
            method: http_method_from_code(reader.u8()?)?,
            path: reader.string()?,
            query: reader.opt_string()?,
            remote: reader.opt_string()?,
            headers: serde_json::from_str(&reader.string()?).unwrap_or_default(),
            payload: reader.opt_bytes()?.map(Base64Bytes::from),
        })),
        TRIGGER_MANUAL => Ok(RunTrigger::Manual(
            hodeco_contract::domain::ManualRunTrigger {
                id: reader.string()?,
                payload: reader.opt_bytes()?.map(Base64Bytes::from),
                metadata: serde_json::from_str(&reader.string()?).unwrap_or_default(),
            },
        )),
        TRIGGER_TASK => Ok(RunTrigger::Task(hodeco_contract::domain::TaskRunTrigger {
            id: reader.string()?,
            parent_run_id: reader.uuid()?,
            chain_id: reader.uuid()?,
            depth: reader.u32()?,
            rule: next_condition_from_code(reader.u8()?)?,
            payload: reader.opt_bytes()?.map(Base64Bytes::from),
        })),
        _ => Err(HistoryError::Corrupt(format!(
            "unknown trigger block {kind:#06x}"
        ))),
    }
}

fn decode_action(kind: u16, body: &[u8]) -> Result<RunAction, HistoryError> {
    let mut reader = Reader::new(body);
    let id = reader.string()?;
    let started_at = reader.time()?;
    let finished_at = reader.time()?;
    let status = action_status_from_code(reader.u8()?)?;
    let action_kind = reader.u8()?;
    match (kind, action_kind) {
        (ACTION_COMMAND, 0) => {
            let command_id = reader.string()?;
            let executable = reader.string()?;
            let args = reader.strings()?;
            let exit_code = reader.opt_i32()?;
            let stdout = reader.opt_bytes()?.map(Base64Bytes::from);
            let _legacy_stderr = reader.opt_bytes()?;
            let output_truncated = reader.u8()? != 0;
            Ok(RunAction {
                id,
                started_at,
                finished_at,
                status,
                command: Some(RunCommandAction {
                    command_id,
                    executable,
                    args,
                    exit_code,
                    stdout,
                    output_truncated,
                }),
                message: None,
                error: reader.opt_string()?,
            })
        }
        (ACTION_MESSAGE, 1) => Ok(RunAction {
            id,
            started_at,
            finished_at,
            status,
            command: None,
            message: Some(RunMessageAction {
                service: reader.service_ref()?,
                payload: {
                    let length = reader.u32()? as usize;
                    Base64Bytes::from(reader.bytes(length)?.to_vec())
                },
                message_id: reader.opt_uuid()?,
                ttl: reader.opt_u64()?.map(|seconds| {
                    hodeco_contract::utils::HumanDuration::from_std(std::time::Duration::from_secs(
                        seconds,
                    ))
                }),
            }),
            error: reader.opt_string()?,
        }),
        _ => Err(HistoryError::Corrupt(
            "action block kind mismatch".to_string(),
        )),
    }
}

fn decode_selector(body: &[u8]) -> Result<SelectorResult, HistoryError> {
    let mut reader = Reader::new(body);
    Ok(SelectorResult {
        command_id: reader.string()?,
        executable: reader.string()?,
        args: reader.strings()?,
        exit_code: reader.opt_i32()?,
        outcome: selector_outcome_from_code(reader.u8()?)?,
        error: reader.opt_string()?,
    })
}

fn decode_next_tasks(body: &[u8]) -> Result<Vec<NextTaskRecord>, HistoryError> {
    let mut reader = Reader::new(body);
    let count = reader.u16()? as usize;
    let mut next = Vec::with_capacity(count);
    for _ in 0..count {
        next.push(NextTaskRecord {
            task_id: reader.string()?,
            run_id: reader.uuid()?,
            rule: next_condition_from_code(reader.u8()?)?,
            selector_exit_code: reader.opt_u8()?,
        });
    }
    Ok(next)
}

fn decode_error(body: &[u8]) -> Result<RunError, HistoryError> {
    let mut reader = Reader::new(body);
    Ok(RunError {
        stage: run_stage_from_code(reader.u8()?)?,
        message: reader.string()?,
    })
}

#[derive(Default)]
struct Writer {
    data: Vec<u8>,
}

impl Writer {
    fn finish(self) -> Vec<u8> {
        self.data
    }

    fn raw(&mut self, value: &[u8]) {
        self.data.extend_from_slice(value);
    }

    fn u8(&mut self, value: u8) {
        self.data.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, value: &str) {
        let bytes = value.as_bytes();
        let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX) as usize;
        self.u32(length as u32);
        self.raw(&bytes[..length]);
    }

    fn strings(&mut self, values: &[String]) {
        self.u16(u16::try_from(values.len()).unwrap_or(u16::MAX));
        for value in values {
            self.string(value);
        }
    }

    fn bytes(&mut self, value: &[u8]) {
        self.u32(u32::try_from(value.len()).unwrap_or(u32::MAX));
        self.raw(value);
    }

    fn opt_bytes(&mut self, value: Option<&[u8]>) {
        match value {
            Some(value) => {
                self.u8(1);
                self.bytes(value);
            }
            None => self.u8(0),
        }
    }

    fn opt_string(&mut self, value: Option<&str>) {
        match value {
            Some(value) => {
                self.u8(1);
                self.string(value);
            }
            None => self.u8(0),
        }
    }

    fn uuid(&mut self, value: Uuid) {
        self.raw(value.as_bytes());
    }

    fn opt_uuid(&mut self, value: Option<Uuid>) {
        match value {
            Some(value) => {
                self.u8(1);
                self.uuid(value);
            }
            None => self.u8(0),
        }
    }

    fn opt_u8(&mut self, value: Option<u8>) {
        match value {
            Some(value) => {
                self.u8(1);
                self.u8(value);
            }
            None => self.u8(0),
        }
    }

    fn opt_u64(&mut self, value: Option<u64>) {
        match value {
            Some(value) => {
                self.u8(1);
                self.u64(value);
            }
            None => self.u8(0),
        }
    }

    fn opt_i32(&mut self, value: Option<i32>) {
        match value {
            Some(value) => {
                self.u8(1);
                self.i32(value);
            }
            None => self.u8(0),
        }
    }

    fn time(&mut self, value: DateTime<Utc>) {
        let nanos = value.timestamp_nanos_opt().unwrap_or(0);
        self.data.extend_from_slice(&nanos.to_le_bytes());
    }

    fn opt_time(&mut self, value: Option<DateTime<Utc>>) {
        match value {
            Some(value) => {
                self.u8(1);
                self.time(value);
            }
            None => self.u8(0),
        }
    }

    fn service_ref(&mut self, value: &ServiceRef) {
        match value {
            ServiceRef::Alias { alias } => {
                self.u8(0);
                self.string(alias);
            }
            ServiceRef::Key {
                protocol,
                transport,
                entry,
            } => {
                self.u8(1);
                self.string(protocol);
                self.string(transport);
                self.string(entry);
            }
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    fn finished(&self) -> bool {
        self.offset >= self.data.len()
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], HistoryError> {
        if self.offset + length > self.data.len() {
            return Err(HistoryError::Corrupt("block is truncated".to_string()));
        }
        let value = &self.data[self.offset..self.offset + length];
        self.offset += length;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, HistoryError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, HistoryError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, HistoryError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, HistoryError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, HistoryError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], HistoryError> {
        self.take(length)
    }

    fn string(&mut self) -> Result<String, HistoryError> {
        let length = self.u32()? as usize;
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| HistoryError::Corrupt("a string is not valid UTF-8".to_string()))
    }

    fn strings(&mut self) -> Result<Vec<String>, HistoryError> {
        let count = self.u16()? as usize;
        let mut values = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            values.push(self.string()?);
        }
        Ok(values)
    }

    fn opt_bytes(&mut self) -> Result<Option<Vec<u8>>, HistoryError> {
        if self.u8()? == 0 {
            return Ok(None);
        }
        let length = self.u32()? as usize;
        Ok(Some(self.take(length)?.to_vec()))
    }

    fn opt_string(&mut self) -> Result<Option<String>, HistoryError> {
        if self.u8()? == 0 {
            return Ok(None);
        }
        Ok(Some(self.string()?))
    }

    fn uuid(&mut self) -> Result<Uuid, HistoryError> {
        Ok(Uuid::from_bytes(self.take(16)?.try_into().unwrap()))
    }

    fn opt_uuid(&mut self) -> Result<Option<Uuid>, HistoryError> {
        if self.u8()? == 0 {
            return Ok(None);
        }
        Ok(Some(self.uuid()?))
    }

    fn opt_u8(&mut self) -> Result<Option<u8>, HistoryError> {
        if self.u8()? == 0 {
            return Ok(None);
        }
        Ok(Some(self.u8()?))
    }

    fn opt_u64(&mut self) -> Result<Option<u64>, HistoryError> {
        if self.u8()? == 0 {
            return Ok(None);
        }
        Ok(Some(self.u64()?))
    }

    fn opt_i32(&mut self) -> Result<Option<i32>, HistoryError> {
        if self.u8()? == 0 {
            return Ok(None);
        }
        Ok(Some(self.i32()?))
    }

    fn time(&mut self) -> Result<DateTime<Utc>, HistoryError> {
        let nanos = i64::from_le_bytes(self.take(8)?.try_into().unwrap());
        let seconds = nanos.div_euclid(1_000_000_000);
        let sub = nanos.rem_euclid(1_000_000_000) as u32;
        Utc.timestamp_opt(seconds, sub)
            .single()
            .ok_or_else(|| HistoryError::Corrupt("timestamp is out of range".to_string()))
    }

    /// Reads the optional delay timestamp appended to new RUN_CORE blocks.
    /// Earlier records end before it.
    fn opt_time_tail(&mut self) -> Result<Option<DateTime<Utc>>, HistoryError> {
        if self.finished() || self.u8()? == 0 {
            return Ok(None);
        }
        Ok(Some(self.time()?))
    }

    fn service_ref(&mut self) -> Result<ServiceRef, HistoryError> {
        match self.u8()? {
            0 => Ok(ServiceRef::Alias {
                alias: self.string()?,
            }),
            1 => Ok(ServiceRef::Key {
                protocol: self.string()?,
                transport: self.string()?,
                entry: self.string()?,
            }),
            other => Err(HistoryError::Corrupt(format!(
                "unknown service reference tag {other}"
            ))),
        }
    }
}

fn trigger_type_code(value: TriggerType) -> u8 {
    match value {
        TriggerType::Message => 0,
        TriggerType::Schedule => 1,
        TriggerType::Http => 2,
        TriggerType::Manual => 3,
        TriggerType::Task => 4,
    }
}

fn trigger_type_from_code(value: u8) -> Result<TriggerType, HistoryError> {
    match value {
        0 => Ok(TriggerType::Message),
        1 => Ok(TriggerType::Schedule),
        2 => Ok(TriggerType::Http),
        3 => Ok(TriggerType::Manual),
        4 => Ok(TriggerType::Task),
        other => Err(HistoryError::Corrupt(format!(
            "unknown trigger type {other}"
        ))),
    }
}

fn outcome_code(value: RunOutcome) -> u8 {
    match value {
        RunOutcome::Succeeded => 0,
        RunOutcome::Failed => 1,
        RunOutcome::Cancelled => 2,
    }
}

fn outcome_from_code(value: u8) -> Result<RunOutcome, HistoryError> {
    match value {
        0 => Ok(RunOutcome::Succeeded),
        1 => Ok(RunOutcome::Failed),
        2 => Ok(RunOutcome::Cancelled),
        other => Err(HistoryError::Corrupt(format!("unknown outcome {other}"))),
    }
}

fn action_status_code(value: ActionOutcome) -> u8 {
    match value {
        ActionOutcome::Succeeded => 0,
        ActionOutcome::Failed => 1,
    }
}

fn action_status_from_code(value: u8) -> Result<ActionOutcome, HistoryError> {
    match value {
        0 => Ok(ActionOutcome::Succeeded),
        1 => Ok(ActionOutcome::Failed),
        other => Err(HistoryError::Corrupt(format!(
            "unknown action status {other}"
        ))),
    }
}

fn next_condition_code(value: NextCondition) -> u8 {
    match value {
        NextCondition::Always => 0,
        NextCondition::AllSucceeded => 1,
        NextCondition::AnyFailed => 2,
        NextCondition::AllFailed => 3,
        NextCondition::CommandResult => 4,
    }
}

fn next_condition_from_code(value: u8) -> Result<NextCondition, HistoryError> {
    match value {
        0 => Ok(NextCondition::Always),
        1 => Ok(NextCondition::AllSucceeded),
        2 => Ok(NextCondition::AnyFailed),
        3 => Ok(NextCondition::AllFailed),
        4 => Ok(NextCondition::CommandResult),
        other => Err(HistoryError::Corrupt(format!(
            "unknown next condition {other}"
        ))),
    }
}

fn selector_outcome_code(value: SelectorOutcome) -> u8 {
    match value {
        SelectorOutcome::Matched => 0,
        SelectorOutcome::NoMatch => 1,
        SelectorOutcome::Error => 2,
    }
}

fn selector_outcome_from_code(value: u8) -> Result<SelectorOutcome, HistoryError> {
    match value {
        0 => Ok(SelectorOutcome::Matched),
        1 => Ok(SelectorOutcome::NoMatch),
        2 => Ok(SelectorOutcome::Error),
        other => Err(HistoryError::Corrupt(format!(
            "unknown selector outcome {other}"
        ))),
    }
}

fn run_stage_code(value: RunStage) -> u8 {
    match value {
        RunStage::Trigger => 0,
        RunStage::Actions => 1,
        RunStage::Selector => 2,
        RunStage::Internal => 3,
    }
}

fn run_stage_from_code(value: u8) -> Result<RunStage, HistoryError> {
    match value {
        0 => Ok(RunStage::Trigger),
        1 => Ok(RunStage::Actions),
        2 => Ok(RunStage::Selector),
        3 => Ok(RunStage::Internal),
        other => Err(HistoryError::Corrupt(format!("unknown run stage {other}"))),
    }
}

fn http_method_code(value: HttpMethod) -> u8 {
    match value {
        HttpMethod::Get => 0,
        HttpMethod::Post => 1,
    }
}

fn http_method_from_code(value: u8) -> Result<HttpMethod, HistoryError> {
    match value {
        0 => Ok(HttpMethod::Get),
        1 => Ok(HttpMethod::Post),
        other => Err(HistoryError::Corrupt(format!(
            "unknown HTTP method {other}"
        ))),
    }
}

fn message_state_code(value: MessageState) -> u8 {
    value as u8
}

fn message_state_from_code(value: u8) -> Result<MessageState, HistoryError> {
    MessageState::try_from(value).map_err(HistoryError::Corrupt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn summary(task_id: &str, id: Uuid, time: i64) -> RunSummary {
        RunSummary {
            id,
            task_id: task_id.to_string(),
            time: Utc.timestamp_opt(time, 0).unwrap(),
            started_at: Utc.timestamp_opt(time, 0).unwrap(),
            delayed_until: None,
            finished_at: Utc.timestamp_opt(time + 1, 0).unwrap(),
            trigger: TriggerRef {
                id: "manual".to_string(),
                kind: TriggerType::Manual,
            },
            outcome: RunOutcome::Succeeded,
            action_count: 1,
            next_count: 0,
            error: None,
        }
    }

    fn run(task_id: &str, id: Uuid, time: i64) -> TaskRun {
        let summary = summary(task_id, id, time);
        TaskRun {
            summary,
            trigger: RunTrigger::Manual(hodeco_contract::domain::ManualRunTrigger {
                id: "manual".to_string(),
                payload: Some(Base64Bytes::from(vec![1, 2, 3])),
                metadata: BTreeMap::from([("key".to_string(), serde_json::json!("value"))]),
            }),
            actions: vec![RunAction {
                id: "action".to_string(),
                started_at: Utc.timestamp_opt(time, 0).unwrap(),
                finished_at: Utc.timestamp_opt(time + 1, 0).unwrap(),
                status: ActionOutcome::Succeeded,
                command: Some(RunCommandAction {
                    command_id: "meter".to_string(),
                    executable: "meter".to_string(),
                    args: vec!["--json".to_string()],
                    exit_code: Some(0),
                    stdout: Some(Base64Bytes::from(b"{}".to_vec())),
                    output_truncated: false,
                }),
                message: None,
                error: None,
            }],
            selector: None,
            next_tasks: Vec::new(),
            chain: RunChain {
                chain_id: id,
                parent_run_id: None,
                depth: 0,
            },
            error: None,
        }
    }

    #[test]
    fn round_trips_runs() {
        let run = run("task", Uuid::new_v4(), 1_000);
        let encoded = encode_run(&run).unwrap();
        let decoded = decode_run(&encoded).unwrap();
        assert_eq!(decoded, run);
    }

    #[test]
    fn round_trips_delayed_runs() {
        let mut run = run("task", Uuid::new_v4(), 1_000);
        run.summary.delayed_until = Some(Utc.timestamp_opt(1_000, 500_000_000).unwrap());
        let encoded = encode_run(&run).unwrap();
        let decoded = decode_run(&encoded).unwrap();
        assert_eq!(decoded.summary.delayed_until, run.summary.delayed_until);
    }

    #[test]
    fn decodes_legacy_run_core_without_delay() {
        let mut core = Writer::default();
        core.uuid(Uuid::nil());
        core.string("task");
        core.time(Utc.timestamp_opt(1_000, 0).unwrap());
        core.time(Utc.timestamp_opt(1_000, 0).unwrap());
        core.time(Utc.timestamp_opt(1_001, 0).unwrap());
        core.string("manual");
        core.u8(trigger_type_code(TriggerType::Manual));
        core.u8(outcome_code(RunOutcome::Succeeded));
        core.u16(0);
        core.u16(0);
        core.opt_string(None);
        let summary = decode_core(&core.finish()).unwrap();
        assert_eq!(summary.delayed_until, None);
    }

    #[test]
    fn round_trips_message_trigger_without_direction() {
        let mut run = run("task", Uuid::new_v4(), 1_000);
        run.summary.trigger = TriggerRef {
            id: "message".to_string(),
            kind: TriggerType::Message,
        };
        run.trigger = RunTrigger::Message(hodeco_contract::domain::MessageRunTrigger {
            id: "message".to_string(),
            message_id: Uuid::new_v4(),
            time: Utc.timestamp_opt(1_000, 0).unwrap(),
            service: ServiceRef::Alias {
                alias: "ZW".to_string(),
            },
            protocol: "zwave".to_string(),
            transport: "serial".to_string(),
            entry: "COM4".to_string(),
            state: MessageState::Incoming,
            payload: Some(Base64Bytes::from(vec![1, 2, 3])),
        });
        let encoded = encode_run(&run).unwrap();
        let decoded = decode_run(&encoded).unwrap();
        assert_eq!(decoded, run);
    }

    #[tokio::test]
    async fn round_trips_files_and_pages() {
        let dir = std::env::temp_dir().join(format!("hodeco-tlog-test-{}", Uuid::new_v4()));
        let store = Arc::new(HistoryStore::new(TaskLogConfig::default(), dir.clone()));
        store.load(&BTreeSet::from(["task".to_string()])).unwrap();
        let mut ids = Vec::new();
        for index in 0..3 {
            let id = Uuid::new_v4();
            ids.push(id);
            let run = run("task", id, 1_000 + index);
            let record = encode_run(&run).unwrap();
            store
                .append(Arc::new(run), record, &TaskHistoryConfig::default())
                .unwrap();
        }
        let page = store
            .list(&hodeco_contract::api::payloads::TasksHistoryParams {
                task_ids: vec!["task".to_string()],
                order: hodeco_contract::domain::MessageOrder::OldestFirst,
                limit: Some(2),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.runs.len(), 2);
        assert!(page.next.is_some());

        let reloaded = Arc::new(HistoryStore::new(TaskLogConfig::default(), dir.clone()));
        reloaded
            .load(&BTreeSet::from(["task".to_string()]))
            .unwrap();
        assert_eq!(
            reloaded.list(&Default::default()).await.unwrap().runs.len(),
            3
        );
        assert_eq!(
            reloaded
                .get("task", ids[1])
                .await
                .unwrap()
                .unwrap()
                .summary
                .id,
            ids[1]
        );
        assert_eq!(reloaded.last_run("task").unwrap().id, ids[2]);

        reloaded.purge("task").await.unwrap();
        assert_eq!(
            reloaded.list(&Default::default()).await.unwrap().runs.len(),
            0
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn applies_per_task_row_limits() {
        let dir = std::env::temp_dir().join(format!("hodeco-tlog-limit-{}", Uuid::new_v4()));
        let store = Arc::new(HistoryStore::new(TaskLogConfig::default(), dir.clone()));
        store.load(&BTreeSet::from(["task".to_string()])).unwrap();
        let history = TaskHistoryConfig {
            max_rows: 2,
            ..TaskHistoryConfig::default()
        };
        for index in 0..3 {
            let run = run("task", Uuid::new_v4(), 1_000 + index);
            let record = encode_run(&run).unwrap();
            store.append(Arc::new(run), record, &history).unwrap();
        }
        assert_eq!(store.list(&Default::default()).await.unwrap().runs.len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removes_orphans() {
        let dir = std::env::temp_dir().join(format!("hodeco-tlog-orphan-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("orphan.tlog"), b"x").unwrap();
        let store = HistoryStore::new(TaskLogConfig::default(), dir.clone());
        let report = store.load(&BTreeSet::new()).unwrap();
        assert_eq!(report.orphans_removed.len(), 1);
        assert!(!dir.join("orphan.tlog").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_corrupt_files() {
        let dir = std::env::temp_dir().join(format!("hodeco-tlog-corrupt-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("task.tlog"), b"NOPE").unwrap();
        let store = HistoryStore::new(TaskLogConfig::default(), dir.clone());
        let report = store.load(&BTreeSet::from(["task".to_string()])).unwrap();
        assert_eq!(report.corrupt.len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
