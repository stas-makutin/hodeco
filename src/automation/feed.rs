//! Bounded in-memory run feed used by the live Runs view.

use super::run_list::page_runs;
use hodeco_contract::api::payloads::{RunFeedTotals, TasksRunFeedParams, TasksRunFeedResult};
use hodeco_contract::config::RunFeedConfig;
use hodeco_contract::domain::{RunOutcome, RunSummary, TaskRun, TriggerType};
use parking_lot::Mutex;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use uuid::Uuid;

const ENTRY_OVERHEAD: u64 = 64;

#[derive(Debug, Clone)]
struct FeedRun {
    run: Arc<TaskRun>,
    size: u64,
}

#[derive(Debug, Default)]
struct FeedInner {
    order: VecDeque<Uuid>,
    runs: HashMap<Uuid, FeedRun>,
    bytes: u64,
}

pub struct RunFeed {
    inner: Mutex<FeedInner>,
    max_rows: u32,
    max_size: u64,
}

impl RunFeed {
    pub fn new(config: &RunFeedConfig) -> Self {
        Self {
            inner: Mutex::new(FeedInner::default()),
            max_rows: config.max_rows.max(1),
            max_size: config.max_size.as_bytes(),
        }
    }

    /// Appends a completed run, using its pre-encoded record size (passed in
    /// by the caller that already encoded it for history) for byte accounting.
    pub fn push_run(&self, run: Arc<TaskRun>, encoded_size: u64) -> Vec<Uuid> {
        self.push(run, encoded_size + ENTRY_OVERHEAD)
    }

    /// Appends a completed run and returns evicted run IDs.
    fn push(&self, run: Arc<TaskRun>, size: u64) -> Vec<Uuid> {
        let mut inner = self.inner.lock();
        if inner.runs.contains_key(&run.summary.id) {
            return Vec::new();
        }

        let id = run.summary.id;
        inner.order.push_back(id);
        inner.bytes = inner.bytes.saturating_add(size);
        inner.runs.insert(id, FeedRun { run, size });

        let mut dropped = Vec::new();
        while inner.order.len() > self.max_rows as usize
            || (inner.bytes > self.max_size && inner.order.len() > 1)
        {
            let Some(id) = inner.order.pop_front() else {
                break;
            };
            if let Some(entry) = inner.runs.remove(&id) {
                inner.bytes = inner.bytes.saturating_sub(entry.size);
                dropped.push(id);
            }
        }
        dropped
    }

    pub fn get(&self, task_id: &str, run_id: Uuid) -> Option<TaskRun> {
        let inner = self.inner.lock();
        inner
            .runs
            .get(&run_id)
            .filter(|entry| entry.run.summary.task_id == task_id)
            .map(|entry| entry.run.as_ref().clone())
    }

    pub fn list(&self, params: &TasksRunFeedParams) -> TasksRunFeedResult {
        let inner = self.inner.lock();
        let selected = matching_summaries(
            &inner,
            &params.task_ids,
            &params.trigger_ids,
            &params.types,
            &params.outcomes,
        );
        let totals = RunFeedTotals {
            rows: inner.order.len() as u64,
            bytes: inner.bytes,
            max_rows: self.max_rows,
            max_size: self.max_size,
        };
        drop(inner);

        let (runs, next) = page_runs(
            selected,
            params.from,
            params.until,
            params.order,
            super::run_list::page_limit(params.limit),
        );
        TasksRunFeedResult {
            runs,
            next,
            feed: totals,
        }
    }

    /// Returns every matching summary so callers can merge the feed into
    /// another run source before paging.
    pub fn summaries(
        &self,
        task_ids: &[String],
        trigger_ids: &[String],
        types: &[TriggerType],
        outcomes: &[RunOutcome],
    ) -> Vec<RunSummary> {
        let inner = self.inner.lock();
        matching_summaries(&inner, task_ids, trigger_ids, types, outcomes)
    }

    pub fn remove_task(&self, task_id: &str) -> Vec<Uuid> {
        let mut inner = self.inner.lock();
        let mut removed = Vec::new();
        let order = std::mem::take(&mut inner.order);
        let mut kept = VecDeque::with_capacity(order.len());

        for id in order {
            let belongs_to_task = inner
                .runs
                .get(&id)
                .is_some_and(|entry| entry.run.summary.task_id == task_id);
            if belongs_to_task {
                if let Some(entry) = inner.runs.remove(&id) {
                    inner.bytes = inner.bytes.saturating_sub(entry.size);
                    removed.push(id);
                }
            } else {
                kept.push_back(id);
            }
        }
        inner.order = kept;
        removed
    }
}

fn matching_summaries(
    inner: &FeedInner,
    task_ids: &[String],
    trigger_ids: &[String],
    types: &[TriggerType],
    outcomes: &[RunOutcome],
) -> Vec<RunSummary> {
    let task_filter: BTreeSet<&str> = task_ids.iter().map(String::as_str).collect();
    let trigger_filter: BTreeSet<&str> = trigger_ids.iter().map(String::as_str).collect();
    let outcomes: BTreeSet<RunOutcome> = outcomes.iter().copied().collect();
    let types: BTreeSet<TriggerType> = types.iter().copied().collect();

    let mut selected = Vec::new();
    for id in &inner.order {
        let Some(entry) = inner.runs.get(id) else {
            continue;
        };
        let summary = &entry.run.summary;
        if !task_filter.is_empty() && !task_filter.contains(summary.task_id.as_str()) {
            continue;
        }
        if !trigger_filter.is_empty() && !trigger_filter.contains(summary.trigger.id.as_str()) {
            continue;
        }
        if !types.is_empty() && !types.contains(&summary.trigger.kind) {
            continue;
        }
        if !outcomes.is_empty() && !outcomes.contains(&summary.outcome) {
            continue;
        }
        selected.push(summary.clone());
    }
    selected
}

impl std::fmt::Debug for RunFeed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock();
        formatter
            .debug_struct("RunFeed")
            .field("rows", &inner.order.len())
            .field("bytes", &inner.bytes)
            .field("max_rows", &self.max_rows)
            .field("max_size", &self.max_size)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use hodeco_contract::domain::{
        ManualRunTrigger, RunChain, RunOutcome, RunSummary, RunTrigger, TriggerRef, TriggerType,
    };
    use hodeco_contract::utils::Base64Bytes;

    fn run(id: Uuid, time: i64) -> TaskRun {
        TaskRun {
            summary: RunSummary {
                id,
                task_id: "task".to_string(),
                time: DateTime::from_timestamp(time, 0).unwrap(),
                started_at: DateTime::from_timestamp(time, 0).unwrap(),
                delayed_until: None,
                finished_at: DateTime::from_timestamp(time + 1, 0).unwrap(),
                trigger: TriggerRef {
                    id: "manual".to_string(),
                    kind: TriggerType::Manual,
                },
                outcome: RunOutcome::Succeeded,
                action_count: 0,
                next_count: 0,
                error: None,
            },
            trigger: RunTrigger::Manual(ManualRunTrigger {
                id: "manual".to_string(),
                payload: Some(Base64Bytes::from(vec![1])),
                metadata: Default::default(),
            }),
            actions: Vec::new(),
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

    fn feed(max_rows: u32, max_size: u64) -> RunFeed {
        RunFeed::new(&RunFeedConfig {
            max_rows,
            max_size: hodeco_contract::utils::Size::from_bytes(max_size),
        })
    }

    #[test]
    fn evicts_oldest_rows() {
        let feed = feed(2, 1024 * 1024);
        let first = Uuid::new_v4();
        assert!(feed.push(Arc::new(run(first, 0)), 10).is_empty());
        assert!(feed.push(Arc::new(run(Uuid::new_v4(), 1)), 10).is_empty());
        assert_eq!(feed.push(Arc::new(run(Uuid::new_v4(), 2)), 10), vec![first]);
        let page = feed.list(&TasksRunFeedParams {
            order: hodeco_contract::domain::MessageOrder::OldestFirst,
            ..Default::default()
        });
        assert_eq!(page.runs.len(), 2);
        assert_eq!(page.runs[0].time.timestamp(), 1);
        assert_eq!(page.runs[1].time.timestamp(), 2);
    }

    #[test]
    fn evicts_oldest_by_bytes() {
        let feed = feed(10, 25);
        let first = Uuid::new_v4();
        feed.push(Arc::new(run(first, 0)), 10);
        feed.push(Arc::new(run(Uuid::new_v4(), 1)), 10);
        feed.push(Arc::new(run(Uuid::new_v4(), 2)), 10);
        let page = feed.list(&TasksRunFeedParams::default());
        assert_eq!(page.runs.len(), 2);
        assert!(feed.get("task", first).is_none());
    }

    #[test]
    fn pages_newest_first_with_complete_bounds() {
        let feed = feed(10, 1024 * 1024);
        for index in 0..5 {
            feed.push(Arc::new(run(Uuid::new_v4(), index)), 10);
        }
        let page = feed.list(&TasksRunFeedParams {
            limit: Some(2),
            ..Default::default()
        });
        assert_eq!(page.runs.len(), 2);
        assert_eq!(page.runs[0].time.timestamp(), 4);
        assert_eq!(page.runs[1].time.timestamp(), 3);
        let next = page.next.unwrap();
        assert!(next.from.is_none());
        assert_eq!(
            next.until.unwrap(),
            DateTime::<Utc>::from_timestamp(3, 0).unwrap()
        );
    }
}
