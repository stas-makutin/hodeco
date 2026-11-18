//! Per-task append-only run history (TLOG).
//!
//! Each task owns `tasks/<id>.tlog`. The container is shared with the message
//! log: magic, version, reserved bytes, compacted-at timestamp, then framed
//! records. TLOG records contain a typed block list instead of JSON so run
//! details stay compact.

use chrono::{DateTime, TimeZone, Utc};
use hodeco_contract::api::payloads::{
    MessageTimeRange, TaskStoreTotal, TasksHistoryParams, TasksHistoryResult,
};
use hodeco_contract::config::TaskLogConfig;
use hodeco_contract::domain::{
    ActionOutcome, HttpMethod, MessageDirection, MessageState, NextCondition, NextTaskRecord,
    RunAction, RunChain, RunCommandAction, RunError, RunMessageAction, RunOutcome, RunStage,
    RunSummary, RunTrigger, SelectorOutcome, SelectorResult, ServiceRef, TaskRun, TriggerRef,
    TriggerType,
};
use hodeco_contract::utils::{Base64Bytes, FileMode};
use parking_lot::RwLock;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const MAGIC: [u8; 4] = *b"TLOG";
pub const VERSION: u8 = 1;
const RESERVED: [u8; 3] = [0; 3];
const HEADER_LEN: usize = 4 + 1 + 3 + 8;
const RECORD_OVERHEAD: usize = 1 + 4;

const RUN_CORE: u16 = 0x0001;
const CHAIN: u16 = 0x0002;
const TRIGGER_MESSAGE: u16 = 0x0010;
const TRIGGER_SCHEDULE: u16 = 0x0011;
const TRIGGER_HTTP: u16 = 0x0012;
const TRIGGER_WEBSOCKET: u16 = 0x0013;
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

pub struct HistoryStore {
    config: TaskLogConfig,
    root: PathBuf,
    tasks: RwLock<BTreeMap<String, TaskHistory>>,
    mutation: tokio::sync::Mutex<()>,
}

#[derive(Debug, Default)]
struct TaskHistory {
    runs: Vec<TaskRun>,
    bytes: u64,
}

impl TaskHistory {
    fn encoded_size(run: &TaskRun) -> Result<u64, HistoryError> {
        Ok(encode_run(run)?.len() as u64)
    }

    fn trim(&mut self, max_rows: u32) {
        let max_rows = max_rows as usize;
        if self.runs.len() > max_rows {
            let excess = self.runs.len() - max_rows;
            self.runs.drain(0..excess);
        }
    }
}

impl HistoryStore {
    pub fn new(config: TaskLogConfig, root: PathBuf) -> Self {
        Self {
            config,
            root,
            tasks: RwLock::new(BTreeMap::new()),
            mutation: tokio::sync::Mutex::new(()),
        }
    }

    pub fn config(&self) -> &TaskLogConfig {
        &self.config
    }

    /// Loads configured task files and removes orphan `.tlog` files.
    pub fn load(&self, task_ids: &BTreeSet<String>) -> Result<HistoryLoadReport, HistoryError> {
        fs::create_dir_all(&self.root)?;
        let mut report = HistoryLoadReport::default();
        let mut tasks = BTreeMap::new();

        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("tlog") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if !task_ids.contains(stem) {
                fs::remove_file(&path)?;
                report.orphans_removed.push(path);
                continue;
            }
            match load_file(&path) {
                Ok(runs) => {
                    let mut history = TaskHistory::default();
                    for run in runs {
                        history.bytes += TaskHistory::encoded_size(&run)?;
                        history.runs.push(run);
                    }
                    history.trim(self.config.max_rows);
                    report.loaded += history.runs.len();
                    tasks.insert(stem.to_string(), history);
                }
                Err(error) => {
                    report.corrupt.push((path.clone(), error.to_string()));
                    match self.config.on_corrupt {
                        hodeco_contract::config::CorruptPolicy::Ignore => {
                            let _ = fs::remove_file(&path);
                            tasks.insert(stem.to_string(), TaskHistory::default());
                        }
                        hodeco_contract::config::CorruptPolicy::Fail => return Err(error),
                    }
                }
            }
        }

        for task_id in task_ids {
            tasks.entry(task_id.clone()).or_default();
        }
        *self.tasks.write() = tasks;
        Ok(report)
    }

    /// Appends a completed run, updates in-memory history, and compacts when a
    /// size, row, or age threshold is crossed. Called from a blocking task by
    /// the engine.
    pub fn append(&self, run: TaskRun) -> Result<(), HistoryError> {
        let record = encode_run(&run)?;
        let task_id = run.summary.task_id.clone();
        let path = self.root.join(format!("{task_id}.tlog"));
        let overflow = {
            let mut tasks = self.tasks.write();
            let history = tasks.entry(task_id.clone()).or_default();
            let overflow = history.runs.len() + 1 > self.config.max_rows as usize;
            history.bytes += (record.len() + RECORD_OVERHEAD) as u64;
            history.runs.push(run);
            history.trim(self.config.max_rows);
            overflow
        };
        append_record(&path, TASK_RUN, &record)?;
        if overflow {
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
        let threshold = (self.config.max_size.as_bytes() as f64
            * self.config.compaction_factor.max(1.0)) as u64;
        let too_large = size > threshold;
        let too_many = self
            .tasks
            .read()
            .get(task_id)
            .is_some_and(|history| history.runs.len() > self.config.max_rows as usize);
        let stale = file_age_exceeds(&path, self.config.compact_interval.as_std());
        if too_large || too_many || stale {
            self.compact(task_id)?;
        }
        Ok(())
    }

    fn compact(&self, task_id: &str) -> Result<(), HistoryError> {
        let path = self.root.join(format!("{task_id}.tlog"));
        let runs = self
            .tasks
            .read()
            .get(task_id)
            .map(|history| history.runs.clone())
            .unwrap_or_default();
        let encoded = encode_file(&runs)?;
        write_atomic(&path, &encoded, Some(self.config.file_mode))?;
        if let Some(history) = self.tasks.write().get_mut(task_id) {
            history.bytes = runs
                .iter()
                .map(TaskHistory::encoded_size)
                .sum::<Result<u64, _>>()
                .unwrap_or(0);
        }
        Ok(())
    }

    /// Serializes appends from async callers. The actual file write runs on a
    /// blocking worker.
    pub async fn append_async(
        self: std::sync::Arc<Self>,
        run: TaskRun,
    ) -> Result<(), HistoryError> {
        let this = self.clone();
        let _guard = self.mutation.lock().await;
        tokio::task::spawn_blocking(move || this.append(run))
            .await
            .map_err(|error| HistoryError::Io(io::Error::other(error.to_string())))?
    }

    pub fn get(&self, task_id: &str, run_id: Uuid) -> Option<TaskRun> {
        self.tasks.read().get(task_id).and_then(|history| {
            history
                .runs
                .iter()
                .find(|run| run.summary.id == run_id)
                .cloned()
        })
    }

    pub fn last_run(&self, task_id: &str) -> Option<RunSummary> {
        self.tasks
            .read()
            .get(task_id)
            .and_then(|history| history.runs.last())
            .map(|run| run.summary.clone())
    }

    pub fn count_trigger_runs(&self, task_id: &str, trigger_id: &str) -> u64 {
        self.tasks
            .read()
            .get(task_id)
            .map(|history| {
                history
                    .runs
                    .iter()
                    .filter(|run| run.summary.trigger.id == trigger_id)
                    .count() as u64
            })
            .unwrap_or(0)
    }

    pub fn list(&self, params: &TasksHistoryParams) -> TasksHistoryResult {
        let task_filter: BTreeSet<&str> = params.task_ids.iter().map(String::as_str).collect();
        let trigger_filter: BTreeSet<&str> =
            params.trigger_ids.iter().map(String::as_str).collect();
        let outcomes: BTreeSet<RunOutcome> = params.outcomes.iter().copied().collect();
        let types: BTreeSet<TriggerType> = params.types.iter().copied().collect();

        let mut selected: Vec<RunSummary> = Vec::new();
        let mut store_totals = Vec::new();
        let tasks = self.tasks.read();
        for (task_id, history) in tasks.iter() {
            if !task_filter.is_empty() && !task_filter.contains(task_id.as_str()) {
                continue;
            }
            store_totals.push(TaskStoreTotal {
                task_id: task_id.clone(),
                rows: history.runs.len() as u64,
                bytes: history.bytes,
            });
            for run in &history.runs {
                let summary = &run.summary;
                if !trigger_filter.is_empty()
                    && !trigger_filter.contains(summary.trigger.id.as_str())
                {
                    continue;
                }
                if !types.is_empty() && !types.contains(&summary.trigger.kind) {
                    continue;
                }
                if !outcomes.is_empty() && !outcomes.contains(&summary.outcome) {
                    continue;
                }
                if params.from.is_some_and(|from| summary.time < from) {
                    continue;
                }
                if params.until.is_some_and(|until| summary.time >= until) {
                    continue;
                }
                if let Some(next) = &params.next {
                    if next.from.is_some_and(|from| summary.time < from) {
                        continue;
                    }
                    if next.until.is_some_and(|until| summary.time >= until) {
                        continue;
                    }
                }
                selected.push(summary.clone());
            }
        }
        drop(tasks);

        selected.sort_by_key(|summary| summary.time);
        let newest_first = params.order == hodeco_contract::domain::MessageOrder::NewestFirst;
        if newest_first {
            selected.reverse();
        }
        let limit = params.limit.unwrap_or(50).clamp(1, 500) as usize;
        let page: Vec<_> = selected.iter().take(limit).cloned().collect();
        let next = if selected.len() > limit {
            let last = page.last().map(|summary| summary.time);
            if let Some(last) = last {
                if newest_first {
                    Some(MessageTimeRange {
                        from: None,
                        until: Some(last),
                    })
                } else {
                    Some(MessageTimeRange {
                        from: Some(
                            last.checked_add_signed(chrono::TimeDelta::nanoseconds(1))
                                .unwrap_or(last),
                        ),
                        until: None,
                    })
                }
            } else {
                None
            }
        } else {
            None
        };

        TasksHistoryResult {
            runs: page,
            next,
            store_totals,
        }
    }

    pub fn purge(&self, task_id: &str) -> Result<(), HistoryError> {
        let path = self.root.join(format!("{task_id}.tlog"));
        if let Some(history) = self.tasks.write().get_mut(task_id) {
            history.runs.clear();
            history.bytes = 0;
        }
        let encoded = encode_file(&[])?;
        write_atomic(&path, &encoded, Some(self.config.file_mode))
    }

    pub fn remove_task(&self, task_id: &str) -> Result<(), HistoryError> {
        let path = self.root.join(format!("{task_id}.tlog"));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(HistoryError::Io(error)),
        }
        self.tasks.write().remove(task_id);
        Ok(())
    }

    pub fn task_ids(&self) -> BTreeSet<String> {
        self.tasks.read().keys().cloned().collect()
    }

    pub fn latest_time(&self) -> Option<DateTime<Utc>> {
        self.tasks
            .read()
            .values()
            .filter_map(|history| history.runs.last())
            .map(|run| run.summary.time)
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
        return Err(HistoryError::Corrupt("header is truncated".to_string()));
    }
    if data[..4] != MAGIC {
        return Err(HistoryError::Corrupt("invalid magic".to_string()));
    }
    if data[4] != VERSION {
        return Err(HistoryError::Corrupt(format!(
            "unsupported version {}",
            data[4]
        )));
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
    let data = fs::read(path)?;
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

#[cfg(unix)]
fn apply_mode(file: &fs::File, mode: FileMode) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode.as_u32()))
}

#[cfg(not(unix))]
fn apply_mode(_file: &fs::File, _mode: FileMode) -> io::Result<()> {
    Ok(())
}

fn write_atomic(path: &Path, data: &[u8], mode: Option<FileMode>) -> Result<(), HistoryError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| HistoryError::InvalidPath(path.to_path_buf()))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        Uuid::new_v4()
    ));
    let result = (|| -> Result<(), io::Error> {
        let file = fs::File::create(&temporary)?;
        if let Some(mode) = mode {
            apply_mode(&file, mode)?;
        }
        let mut writer = io::BufWriter::new(file);
        writer.write_all(data)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(HistoryError::Io)
}

// ---------------------------------------------------------------------------
// Typed blocks
// ---------------------------------------------------------------------------

fn encode_run(run: &TaskRun) -> Result<Vec<u8>, HistoryError> {
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
            block.u8(direction_code(trigger.direction));
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
                block.opt_string(command.stdout.as_deref());
                block.opt_string(command.stderr.as_deref());
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
            TRIGGER_MESSAGE | TRIGGER_SCHEDULE | TRIGGER_HTTP | TRIGGER_WEBSOCKET
            | TRIGGER_TASK | TRIGGER_MANUAL => {
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
        TRIGGER_MESSAGE => Ok(RunTrigger::Message(
            hodeco_contract::domain::MessageRunTrigger {
                id: reader.string()?,
                message_id: reader.uuid()?,
                time: reader.time()?,
                service: reader.service_ref()?,
                protocol: reader.string()?,
                transport: reader.string()?,
                entry: reader.string()?,
                state: message_state_from_code(reader.u8()?)?,
                direction: direction_from_code(reader.u8()?)?,
                payload: reader.opt_bytes()?.map(Base64Bytes::from),
            },
        )),
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
        (ACTION_COMMAND, 0) => Ok(RunAction {
            id,
            started_at,
            finished_at,
            status,
            command: Some(RunCommandAction {
                command_id: reader.string()?,
                executable: reader.string()?,
                args: reader.strings()?,
                exit_code: reader.opt_i32()?,
                stdout: reader.opt_string()?,
                stderr: reader.opt_string()?,
                output_truncated: reader.u8()? != 0,
            }),
            message: None,
            error: reader.opt_string()?,
        }),
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

fn direction_code(value: MessageDirection) -> u8 {
    value as u8
}

fn direction_from_code(value: u8) -> Result<MessageDirection, HistoryError> {
    MessageDirection::try_from(value).map_err(HistoryError::Corrupt)
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
                    stdout: Some("{}".to_string()),
                    stderr: None,
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
    fn round_trips_files_and_pages() {
        let dir = std::env::temp_dir().join(format!("hodeco-tlog-test-{}", Uuid::new_v4()));
        let store = HistoryStore::new(TaskLogConfig::default(), dir.clone());
        store.load(&BTreeSet::from(["task".to_string()])).unwrap();
        for index in 0..3 {
            store
                .append(run("task", Uuid::new_v4(), 1_000 + index))
                .unwrap();
        }
        let page = store.list(&hodeco_contract::api::payloads::TasksHistoryParams {
            task_ids: vec!["task".to_string()],
            order: hodeco_contract::domain::MessageOrder::OldestFirst,
            limit: Some(2),
            ..Default::default()
        });
        assert_eq!(page.runs.len(), 2);
        assert!(page.next.is_some());
        assert_eq!(store.count_trigger_runs("task", "manual"), 3);

        let reloaded = HistoryStore::new(TaskLogConfig::default(), dir.clone());
        reloaded
            .load(&BTreeSet::from(["task".to_string()]))
            .unwrap();
        assert_eq!(reloaded.list(&Default::default()).runs.len(), 3);

        reloaded.purge("task").unwrap();
        assert_eq!(reloaded.list(&Default::default()).runs.len(), 0);
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
    fn rejects_v1_and_corrupt_files() {
        let dir = std::env::temp_dir().join(format!("hodeco-tlog-corrupt-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("task.tlog"), b"NOPE").unwrap();
        let store = HistoryStore::new(TaskLogConfig::default(), dir.clone());
        let report = store.load(&BTreeSet::from(["task".to_string()])).unwrap();
        assert_eq!(report.corrupt.len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
