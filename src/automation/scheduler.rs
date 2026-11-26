//! In-memory schedule scheduler.
//!
//! On startup and after catalog changes the scheduler computes the next future
//! occurrence. Missed occurrences are skipped. Schedule cursors are never
//! persisted; `maxRuns` is counted in memory per schedule trigger.

use crate::app::AppState;
use crate::automation::engine;
use chrono::Utc;
use hodeco_contract::domain::{
    NextFire, RunChain, RunTrigger, ScheduleDefinition, ScheduleOccurrence, ScheduleRunTrigger,
    ScheduleTrigger, TaskDefinition, TriggerType,
};
use hodeco_schedule::{ScheduleContext, ScheduleCursor};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use uuid::Uuid;

/// Upper bound on the idle sleep so wall-clock changes and any missed wakeup
/// are reconciled promptly without polling every schedule continuously.
const MAX_IDLE: Duration = Duration::from_secs(60);
const MIN_SLEEP: Duration = Duration::from_millis(10);

struct TriggerCursor {
    schedule: ScheduleDefinition,
    context: ScheduleContext,
    cursor: ScheduleCursor,
    next: Option<ScheduleOccurrence>,
    exhausted: bool,
    maxed: bool,
}

pub async fn run(state: AppState, cancel: CancellationToken) {
    let mut cursors: BTreeMap<(String, String), TriggerCursor> = BTreeMap::new();

    loop {
        if cancel.is_cancelled() {
            break;
        }

        reconcile(&state, &mut cursors).await;
        fire_due(&state, &mut cursors).await;

        let wait = next_wait(&cursors);
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = state.runtime.scheduler_wake() => {}
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

fn next_wait(cursors: &BTreeMap<(String, String), TriggerCursor>) -> Duration {
    let now = Utc::now();
    let Some(next) = cursors
        .values()
        .filter_map(|cursor| cursor.next.as_ref())
        .map(|occurrence| occurrence.at)
        .min()
    else {
        return MAX_IDLE;
    };
    (next - now)
        .to_std()
        .unwrap_or(Duration::ZERO)
        .clamp(MIN_SLEEP, MAX_IDLE)
}

async fn reconcile(state: &AppState, cursors: &mut BTreeMap<(String, String), TriggerCursor>) {
    let snapshot = state.automation.snapshot();
    let context = ScheduleContext {
        default_location: snapshot.location,
        default_timezone: snapshot
            .timezone
            .as_deref()
            .and_then(|timezone| timezone.parse().ok())
            .or_else(|| {
                state
                    .system_timezone
                    .as_deref()
                    .and_then(|timezone| timezone.parse().ok())
            }),
    };
    let now = Utc::now();
    let mut active = BTreeSet::new();

    for task in snapshot.tasks.values() {
        if !task.enabled {
            state.runtime.set_next(&task.id, None);
            continue;
        }
        let mut earliest: Option<NextFire> = None;
        for trigger in task.schedule_triggers() {
            let key = (task.id.clone(), trigger.id.clone());
            active.insert(key.clone());
            let Some(schedule) = snapshot.schedules.get(&trigger.schedule) else {
                warn!(
                    task = %task.id,
                    trigger = %trigger.id,
                    schedule = %trigger.schedule,
                    "schedule trigger references a missing schedule"
                );
                continue;
            };

            let maxed = trigger.max_runs.is_some_and(|maximum| {
                state.runtime.trigger_runs(&task.id, &trigger.id) >= u64::from(maximum)
            });
            let entry = cursors.get(&key);
            let changed =
                entry.is_none_or(|entry| entry.schedule != *schedule || entry.context != context);
            let reset_after_limit = !maxed && entry.is_some_and(|entry| entry.maxed);
            if changed || reset_after_limit || (maxed && !entry.is_some_and(|entry| entry.maxed)) {
                let cursor = match ScheduleCursor::new(schedule.clone(), context, now, true, false)
                {
                    Ok(cursor) => cursor,
                    Err(error) => {
                        warn!(
                            task = %task.id,
                            trigger = %trigger.id,
                            error = %error,
                            "unable to compute schedule occurrence"
                        );
                        continue;
                    }
                };
                cursors.insert(
                    key.clone(),
                    TriggerCursor {
                        schedule: schedule.clone(),
                        context,
                        cursor,
                        next: None,
                        exhausted: false,
                        maxed: false,
                    },
                );
            }

            let Some(entry) = cursors.get_mut(&key) else {
                continue;
            };
            entry.maxed = maxed;
            if maxed {
                entry.next = None;
                entry.exhausted = true;
                continue;
            }
            if entry.exhausted {
                entry.exhausted = false;
                entry.next = None;
            }
            if entry.next.is_none() {
                match entry.cursor.advance() {
                    Ok(Some(occurrence)) => entry.next = Some(occurrence),
                    Ok(None) => entry.exhausted = true,
                    Err(error) => {
                        warn!(
                            task = %task.id,
                            trigger = %trigger.id,
                            error = %error,
                            "unable to compute the next schedule occurrence"
                        );
                        entry.exhausted = true;
                    }
                }
            }
            if let Some(occurrence) = &entry.next
                && earliest
                    .as_ref()
                    .is_none_or(|current| occurrence.at < current.at)
            {
                earliest = Some(NextFire {
                    at: occurrence.at,
                    trigger_id: trigger.id.clone(),
                    trigger_type: TriggerType::Schedule,
                    schedule_id: Some(trigger.schedule.clone()),
                });
            }
        }
        state.runtime.set_next(&task.id, earliest);
    }

    cursors.retain(|key, _| active.contains(key));
}

async fn fire_due(state: &AppState, cursors: &mut BTreeMap<(String, String), TriggerCursor>) {
    let snapshot = state.automation.snapshot();
    let now = Utc::now();
    let due: Vec<(String, String, ScheduleOccurrence)> = cursors
        .iter()
        .filter_map(|((task_id, trigger_id), cursor)| {
            cursor
                .next
                .as_ref()
                .filter(|occurrence| occurrence.at <= now)
                .map(|occurrence| (task_id.clone(), trigger_id.clone(), occurrence.clone()))
        })
        .collect();

    for (task_id, trigger_id, occurrence) in due {
        let Some(task) = snapshot.tasks.get(&task_id) else {
            continue;
        };
        let Some(trigger) = task
            .schedule_triggers()
            .find(|trigger| trigger.id == trigger_id)
        else {
            continue;
        };
        if let Some(entry) = cursors.get_mut(&(task_id.clone(), trigger_id.clone())) {
            entry.next = None;
        }
        if !task.enabled {
            continue;
        }
        fire_schedule(state, task, trigger, &occurrence);
        if let Some(entry) = cursors.get_mut(&(task_id.clone(), trigger_id.clone())) {
            match entry.cursor.advance() {
                Ok(Some(next)) => entry.next = Some(next),
                Ok(None) => entry.exhausted = true,
                Err(error) => {
                    warn!(
                        task = %task_id,
                        trigger = %trigger_id,
                        error = %error,
                        "unable to advance the schedule cursor"
                    );
                    entry.exhausted = true;
                }
            }
        }
    }
}

fn fire_schedule(
    state: &AppState,
    task: &TaskDefinition,
    trigger: &ScheduleTrigger,
    occurrence: &ScheduleOccurrence,
) {
    if let Some(maximum) = trigger.max_runs
        && state.runtime.trigger_runs(&task.id, &trigger.id) >= u64::from(maximum)
    {
        return;
    }
    let trigger_record = RunTrigger::Schedule(ScheduleRunTrigger {
        id: trigger.id.clone(),
        schedule_id: trigger.schedule.clone(),
        occurrence_at: occurrence.at,
    });
    let chain = RunChain {
        chain_id: Uuid::new_v4(),
        parent_run_id: None,
        depth: 0,
    };
    if let Err(error) = engine::fire(state, task, trigger_record, chain) {
        debug!(
            task = %task.id,
            trigger = %trigger.id,
            reason = error.reason(),
            "schedule trigger suppressed"
        );
    }
}
