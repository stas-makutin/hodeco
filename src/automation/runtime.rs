//! In-memory task admission, suppression counters, and scheduler hints.

use chrono::{DateTime, TimeDelta, Utc};
use hodeco_contract::domain::{NextFire, RunSummary};
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

const RATE_WINDOW: Duration = Duration::from_secs(60);

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

#[derive(Debug, Default)]
struct RuntimeInner {
    running: HashSet<String>,
    suppressed: BTreeMap<String, Suppressed>,
    rates: BTreeMap<String, VecDeque<DateTime<Utc>>>,
    next: BTreeMap<String, NextFire>,
    last_runs: BTreeMap<String, RunSummary>,
    trigger_runs: BTreeMap<(String, String), u64>,
}

pub struct TaskRuntime {
    inner: Mutex<RuntimeInner>,
    max_concurrent: u32,
    last_history_time: Mutex<Option<DateTime<Utc>>>,
    wake: tokio::sync::Notify,
}

impl TaskRuntime {
    pub fn new(max_concurrent: u32) -> Self {
        Self {
            inner: Mutex::new(RuntimeInner::default()),
            max_concurrent: max_concurrent.max(1),
            last_history_time: Mutex::new(None),
            wake: tokio::sync::Notify::new(),
        }
    }

    pub fn wake_scheduler(&self) {
        self.wake.notify_waiters();
    }

    pub async fn scheduler_wake(&self) {
        self.wake.notified().await;
    }

    pub fn try_start(
        self: &Arc<Self>,
        task_id: &str,
        max_runs_per_minute: u32,
        now: DateTime<Utc>,
    ) -> Result<RunGuard, AdmitError> {
        let mut inner = self.inner.lock();
        inner.prune_rates(now);
        if inner.running.contains(task_id) {
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
        inner.running.insert(task_id.to_string());
        Ok(RunGuard {
            runtime: self.clone(),
            task_id: task_id.to_string(),
        })
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
        self.inner.lock().running.contains(task_id)
    }

    pub fn running_count(&self) -> usize {
        self.inner.lock().running.len()
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

    pub fn forget_task(&self, task_id: &str) {
        let mut inner = self.inner.lock();
        inner.suppressed.remove(task_id);
        inner.rates.remove(task_id);
        inner.next.remove(task_id);
        inner.last_runs.remove(task_id);
        inner.trigger_runs.retain(|(id, _), _| id != task_id);
    }

    pub fn reset_rate(&self, task_id: &str) {
        self.inner.lock().rates.remove(task_id);
    }

    pub fn note_trigger_run(&self, task_id: &str, trigger_id: &str) {
        *self
            .inner
            .lock()
            .trigger_runs
            .entry((task_id.to_string(), trigger_id.to_string()))
            .or_default() += 1;
    }

    pub fn trigger_runs(&self, task_id: &str, trigger_id: &str) -> u64 {
        self.inner
            .lock()
            .trigger_runs
            .get(&(task_id.to_string(), trigger_id.to_string()))
            .copied()
            .unwrap_or(0)
    }

    pub fn reset_trigger_runs(&self, task_id: &str) {
        self.inner
            .lock()
            .trigger_runs
            .retain(|(id, _), _| id != task_id);
    }

    pub fn seed_history_time(&self, time: Option<DateTime<Utc>>) {
        if let Some(time) = time {
            let mut last = self.last_history_time.lock();
            if last.is_none_or(|current| time > current) {
                *last = Some(time);
            }
        }
    }

    pub fn next_history_time(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let mut last = self.last_history_time.lock();
        let time = match *last {
            Some(last) if now <= last => last
                .checked_add_signed(TimeDelta::nanoseconds(1))
                .unwrap_or(now),
            _ => now,
        };
        *last = Some(time);
        time
    }
}

impl RuntimeInner {
    fn prune_rates(&mut self, now: DateTime<Utc>) {
        let cutoff = now - TimeDelta::seconds(RATE_WINDOW.as_secs() as i64);
        for rate in self.rates.values_mut() {
            while rate.front().is_some_and(|time| *time <= cutoff) {
                rate.pop_front();
            }
        }
    }
}

pub struct RunGuard {
    runtime: Arc<TaskRuntime>,
    task_id: String,
}

impl std::fmt::Debug for RunGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunGuard")
            .field("task_id", &self.task_id)
            .finish()
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.runtime.inner.lock().running.remove(&self.task_id);
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
        let guard = runtime.try_start("task", 1, now()).unwrap();
        assert_eq!(
            runtime.try_start("task", 1, now()).unwrap_err(),
            AdmitError::Busy
        );
        drop(guard);
        assert_eq!(
            runtime.try_start("task", 1, now()).unwrap_err(),
            AdmitError::RateLimited
        );
        runtime.reset_rate("task");
        let guard = runtime.try_start("task", 1, now()).unwrap();
        drop(guard);
    }

    #[tokio::test]
    async fn enforces_the_global_capacity_across_tasks() {
        let runtime = Arc::new(TaskRuntime::new(1));
        let _guard = runtime.try_start("one", 10, now()).unwrap();
        assert_eq!(
            runtime.try_start("two", 10, now()).unwrap_err(),
            AdmitError::Limit
        );
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

    #[test]
    fn history_timestamps_are_monotonic() {
        let runtime = TaskRuntime::new(1);
        let first = runtime.next_history_time(now());
        let second = runtime.next_history_time(first);
        assert!(second > first);
    }
}
