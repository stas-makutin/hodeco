//! In-memory task admission, suppression counters, and scheduler hints.

use chrono::{DateTime, TimeDelta, Utc};
use hodeco_contract::domain::{NextFire, RunSummary};
use parking_lot::Mutex;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const RATE_WINDOW: Duration = Duration::from_secs(60);
const NO_HISTORY_TIME: i64 = i64::MIN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitError {
    Busy,
    RateLimited,
    Limit,
}

#[derive(Debug, Clone, Default)]
struct Suppressed {
    count: u64,
    last_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RunCancelReason {
    User = 1,
    Timeout = 2,
    Shutdown = 3,
}

impl RunCancelReason {
    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::User),
            2 => Some(Self::Timeout),
            3 => Some(Self::Shutdown),
            _ => None,
        }
    }
}

/// Cancellation token plus the first reason it was fired.
#[derive(Debug, Clone)]
pub struct RunControl {
    token: CancellationToken,
    reason: Arc<AtomicU8>,
}

impl RunControl {
    fn new(parent: Option<&CancellationToken>) -> Self {
        Self {
            token: parent.map_or_else(CancellationToken::new, CancellationToken::child_token),
            reason: Arc::new(AtomicU8::new(0)),
        }
    }

    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    pub fn cancel(&self, reason: RunCancelReason) {
        // Keep the first reason; a timeout must not overwrite an explicit cancel.
        let _ = self
            .reason
            .compare_exchange(0, reason as u8, Ordering::AcqRel, Ordering::Acquire);
        self.token.cancel();
    }

    pub fn reason(&self) -> Option<RunCancelReason> {
        RunCancelReason::from_code(self.reason.load(Ordering::Acquire)).or_else(|| {
            self.token
                .is_cancelled()
                .then_some(RunCancelReason::Shutdown)
        })
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

#[derive(Debug)]
struct ActiveRun {
    run_id: Uuid,
    control: RunControl,
}

#[derive(Debug, Default)]
struct RuntimeInner {
    running: BTreeMap<String, ActiveRun>,
    suppressed: BTreeMap<String, Suppressed>,
    rates: BTreeMap<String, VecDeque<DateTime<Utc>>>,
    next: BTreeMap<String, NextFire>,
    last_runs: BTreeMap<String, RunSummary>,
    trigger_runs: BTreeMap<String, BTreeMap<String, u64>>,
}

pub struct TaskRuntime {
    inner: Mutex<RuntimeInner>,
    max_concurrent: u32,
    last_history_time: AtomicI64,
    wake: tokio::sync::Notify,
    drained: tokio::sync::Notify,
    shutdown: Option<CancellationToken>,
}

impl TaskRuntime {
    pub fn new(max_concurrent: u32) -> Self {
        Self {
            inner: Mutex::new(RuntimeInner::default()),
            max_concurrent: max_concurrent.max(1),
            last_history_time: AtomicI64::new(NO_HISTORY_TIME),
            wake: tokio::sync::Notify::new(),
            drained: tokio::sync::Notify::new(),
            shutdown: None,
        }
    }

    /// Creates a runtime whose active runs are cancelled with the application.
    pub fn with_shutdown(max_concurrent: u32, shutdown: &CancellationToken) -> Self {
        Self {
            inner: Mutex::new(RuntimeInner::default()),
            max_concurrent: max_concurrent.max(1),
            last_history_time: AtomicI64::new(NO_HISTORY_TIME),
            wake: tokio::sync::Notify::new(),
            drained: tokio::sync::Notify::new(),
            shutdown: Some(shutdown.child_token()),
        }
    }

    pub fn wake_scheduler(&self) {
        // Store a permit when the scheduler is busy so a catalog change cannot
        // be lost between iterations.
        self.wake.notify_one();
    }

    pub async fn scheduler_wake(&self) {
        self.wake.notified().await;
    }

    /// Cancels every active run. Runs also observe the application token when
    /// the runtime was created with `with_shutdown`.
    pub fn cancel_runs(&self) {
        if let Some(token) = &self.shutdown {
            token.cancel();
        }
    }

    /// Waits for active runs to finish after shutdown cancellation.
    pub async fn wait_for_runs(&self, timeout: Duration) {
        let drain = async {
            loop {
                if self.inner.lock().running.is_empty() {
                    return;
                }
                self.drained.notified().await;
            }
        };
        if tokio::time::timeout(timeout, drain).await.is_err() {
            tracing::warn!("timed out waiting for active task runs to finish");
        }
    }

    pub fn try_start(
        self: &Arc<Self>,
        task_id: &str,
        run_id: Uuid,
        max_runs_per_minute: u32,
        now: DateTime<Utc>,
    ) -> Result<RunGuard, AdmitError> {
        let mut inner = self.inner.lock();
        inner.prune_rates(task_id, now);
        if inner.running.contains_key(task_id) {
            return Err(AdmitError::Busy);
        }
        if inner.running.len() >= self.max_concurrent as usize {
            return Err(AdmitError::Limit);
        }
        let rate = inner.rates.entry(task_id.to_string()).or_default();
        if max_runs_per_minute > 0 && rate.len() >= max_runs_per_minute as usize {
            return Err(AdmitError::RateLimited);
        }
        rate.push_back(now);
        let control = RunControl::new(self.shutdown.as_ref());
        inner.running.insert(
            task_id.to_string(),
            ActiveRun {
                run_id,
                control: control.clone(),
            },
        );
        Ok(RunGuard {
            runtime: self.clone(),
            task_id: task_id.to_string(),
            run_id,
            control,
        })
    }

    /// Requests cancellation of an active run. Returns `false` when the task is
    /// not running or the run ID does not match the active run.
    pub fn cancel_run(&self, task_id: &str, run_id: Uuid) -> bool {
        let inner = self.inner.lock();
        let Some(active) = inner.running.get(task_id) else {
            return false;
        };
        if active.run_id != run_id {
            return false;
        }
        active.control.cancel(RunCancelReason::User);
        true
    }

    pub fn suppress(&self, task_id: &str, reason: &str) {
        let mut inner = self.inner.lock();
        let entry = inner.suppressed.entry(task_id.to_string()).or_default();
        entry.count += 1;
        entry.last_reason = Some(reason.to_string());
    }

    pub fn suppressed(&self, task_id: &str) -> (u64, Option<String>) {
        self.inner
            .lock()
            .suppressed
            .get(task_id)
            .map(|entry| (entry.count, entry.last_reason.clone()))
            .unwrap_or((0, None))
    }

    pub fn reset_suppressed(&self, task_id: &str) {
        self.inner.lock().suppressed.remove(task_id);
    }

    pub fn running(&self, task_id: &str) -> bool {
        self.inner.lock().running.contains_key(task_id)
    }

    pub fn running_run_id(&self, task_id: &str) -> Option<Uuid> {
        self.inner
            .lock()
            .running
            .get(task_id)
            .map(|active| active.run_id)
    }

    pub fn set_next(&self, task_id: &str, next: Option<NextFire>) {
        let mut inner = self.inner.lock();
        match next {
            Some(next) => {
                inner.next.insert(task_id.to_string(), next);
            }
            None => {
                inner.next.remove(task_id);
            }
        }
    }

    pub fn next(&self, task_id: &str) -> Option<NextFire> {
        self.inner.lock().next.get(task_id).cloned()
    }

    pub fn set_last_run(&self, task_id: &str, run: RunSummary) {
        self.inner.lock().last_runs.insert(task_id.to_string(), run);
    }

    pub fn last_run(&self, task_id: &str) -> Option<RunSummary> {
        self.inner.lock().last_runs.get(task_id).cloned()
    }

    pub fn clear_last_run(&self, task_id: &str) {
        self.inner.lock().last_runs.remove(task_id);
    }

    pub fn forget_task(&self, task_id: &str) {
        let mut inner = self.inner.lock();
        inner.suppressed.remove(task_id);
        inner.rates.remove(task_id);
        inner.next.remove(task_id);
        inner.last_runs.remove(task_id);
        inner.trigger_runs.remove(task_id);
    }

    pub fn reset_rate(&self, task_id: &str) {
        self.inner.lock().rates.remove(task_id);
    }

    pub fn note_trigger_run(&self, task_id: &str, trigger_id: &str) {
        *self
            .inner
            .lock()
            .trigger_runs
            .entry(task_id.to_string())
            .or_default()
            .entry(trigger_id.to_string())
            .or_default() += 1;
    }

    pub fn trigger_runs(&self, task_id: &str, trigger_id: &str) -> u64 {
        self.inner
            .lock()
            .trigger_runs
            .get(task_id)
            .and_then(|triggers| triggers.get(trigger_id))
            .copied()
            .unwrap_or(0)
    }

    pub fn reset_trigger_runs(&self, task_id: &str) {
        self.inner.lock().trigger_runs.remove(task_id);
    }

    pub fn seed_history_time(&self, time: Option<DateTime<Utc>>) {
        if let Some(time) = time
            && let Some(nanos) = time.timestamp_nanos_opt()
        {
            self.last_history_time.fetch_max(nanos, Ordering::AcqRel);
        }
    }

    pub fn next_history_time(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let now = now.timestamp_nanos_opt().unwrap_or_default();
        let mut last = self.last_history_time.load(Ordering::Acquire);
        loop {
            let next = if last == NO_HISTORY_TIME || now > last {
                now
            } else {
                last.saturating_add(1)
            };
            match self.last_history_time.compare_exchange_weak(
                last,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return DateTime::from_timestamp_nanos(next);
                }
                Err(current) => last = current,
            }
        }
    }
}

impl RuntimeInner {
    fn prune_rates(&mut self, task_id: &str, now: DateTime<Utc>) {
        let cutoff = now - TimeDelta::seconds(RATE_WINDOW.as_secs() as i64);
        let Some(rate) = self.rates.get_mut(task_id) else {
            return;
        };
        while rate.front().is_some_and(|time| *time <= cutoff) {
            rate.pop_front();
        }
        if rate.is_empty() {
            self.rates.remove(task_id);
        }
    }
}

pub struct RunGuard {
    runtime: Arc<TaskRuntime>,
    task_id: String,
    run_id: Uuid,
    control: RunControl,
}

impl RunGuard {
    pub fn control(&self) -> RunControl {
        self.control.clone()
    }
}

impl std::fmt::Debug for RunGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunGuard")
            .field("task_id", &self.task_id)
            .field("run_id", &self.run_id)
            .finish()
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        {
            let mut inner = self.runtime.inner.lock();
            if inner
                .running
                .get(&self.task_id)
                .is_some_and(|active| active.run_id == self.run_id)
            {
                inner.running.remove(&self.task_id);
            }
        }
        self.runtime.drained.notify_one();
        self.runtime.wake_scheduler();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[tokio::test]
    async fn admits_one_run_per_task_and_enforces_rate() {
        let runtime = Arc::new(TaskRuntime::new(2));
        let first = Uuid::new_v4();
        let guard = runtime.try_start("task", first, 1, now()).unwrap();
        assert_eq!(runtime.running_run_id("task"), Some(first));
        assert_eq!(
            runtime
                .try_start("task", Uuid::new_v4(), 1, now())
                .unwrap_err(),
            AdmitError::Busy
        );
        drop(guard);
        assert_eq!(
            runtime
                .try_start("task", Uuid::new_v4(), 1, now())
                .unwrap_err(),
            AdmitError::RateLimited
        );
        runtime.reset_rate("task");
        let guard = runtime.try_start("task", Uuid::new_v4(), 1, now()).unwrap();
        drop(guard);
    }

    #[tokio::test]
    async fn enforces_the_global_capacity_across_tasks() {
        let runtime = Arc::new(TaskRuntime::new(1));
        let _guard = runtime.try_start("one", Uuid::new_v4(), 10, now()).unwrap();
        assert_eq!(
            runtime
                .try_start("two", Uuid::new_v4(), 10, now())
                .unwrap_err(),
            AdmitError::Limit
        );
    }

    #[tokio::test]
    async fn cancels_only_the_matching_active_run() {
        let runtime = Arc::new(TaskRuntime::new(1));
        let run_id = Uuid::new_v4();
        let guard = runtime.try_start("task", run_id, 10, now()).unwrap();
        let control = guard.control();
        assert!(!control.is_cancelled());
        assert!(!runtime.cancel_run("task", Uuid::new_v4()));
        assert!(!control.is_cancelled());
        assert!(runtime.cancel_run("task", run_id));
        assert!(control.is_cancelled());
        assert_eq!(control.reason(), Some(RunCancelReason::User));
        assert!(!runtime.cancel_run("other", run_id));
    }

    #[test]
    fn counts_trigger_runs_in_memory() {
        let runtime = TaskRuntime::new(1);
        assert_eq!(runtime.trigger_runs("task", "sunset"), 0);
        runtime.note_trigger_run("task", "sunset");
        runtime.note_trigger_run("task", "sunset");
        runtime.note_trigger_run("task", "other");
        assert_eq!(runtime.trigger_runs("task", "sunset"), 2);
        assert_eq!(runtime.trigger_runs("task", "other"), 1);
        runtime.reset_trigger_runs("task");
        assert_eq!(runtime.trigger_runs("task", "sunset"), 0);
    }

    #[tokio::test]
    async fn shutdown_cancels_active_runs() {
        let shutdown = CancellationToken::new();
        let runtime = Arc::new(TaskRuntime::with_shutdown(1, &shutdown));
        let guard = runtime
            .try_start("task", Uuid::new_v4(), 10, now())
            .unwrap();
        let control = guard.control();
        assert!(!control.is_cancelled());
        shutdown.cancel();
        assert!(control.is_cancelled());
        assert_eq!(control.reason(), Some(RunCancelReason::Shutdown));
    }

    #[test]
    fn history_timestamps_are_monotonic() {
        let runtime = TaskRuntime::new(1);
        let first = runtime.next_history_time(now());
        let second = runtime.next_history_time(first);
        assert!(second > first);
    }
}
