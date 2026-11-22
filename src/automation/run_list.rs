//! Shared run-summary paging with the same timestamp semantics as
//! `messages.list`.

use chrono::{DateTime, TimeDelta, Utc};
use hodeco_contract::api::payloads::MessageTimeRange;
use hodeco_contract::domain::{MessageOrder, RunSummary};

pub const DEFAULT_RUN_PAGE_LIMIT: u32 = 100;
pub const MAX_RUN_PAGE_LIMIT: u32 = 10_000;

pub fn page_limit(limit: Option<u32>) -> usize {
    limit
        .unwrap_or(DEFAULT_RUN_PAGE_LIMIT)
        .clamp(1, MAX_RUN_PAGE_LIMIT) as usize
}

/// Sorts and pages already-filtered summaries.
///
/// `from` is inclusive, `until` is exclusive. The returned `next` is a complete
/// bound pair that the client sends back as `from` / `until`.
pub fn page_runs(
    mut runs: Vec<RunSummary>,
    from: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    order: MessageOrder,
    limit: usize,
) -> (Vec<RunSummary>, Option<MessageTimeRange>) {
    if from.is_some_and(|from| until.is_some_and(|until| from >= until)) {
        return (Vec::new(), None);
    }

    runs.retain(|run| {
        from.is_none_or(|from| run.time >= from) && until.is_none_or(|until| run.time < until)
    });
    runs.sort_by_key(|run| run.time);

    let limit = limit.max(1);
    let has_more = runs.len() > limit;

    let page = match order {
        MessageOrder::NewestFirst => {
            let older = runs.len().saturating_sub(limit);
            runs.drain(..older);
            runs.reverse();
            runs
        }
        MessageOrder::OldestFirst => {
            runs.truncate(limit);
            runs
        }
    };

    let next = if has_more {
        page.last().map(|run| match order {
            MessageOrder::NewestFirst => MessageTimeRange {
                from,
                until: Some(run.time),
            },
            MessageOrder::OldestFirst => MessageTimeRange {
                from: run
                    .time
                    .checked_add_signed(TimeDelta::nanoseconds(1))
                    .or(from),
                until,
            },
        })
    } else {
        None
    };

    (page, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodeco_contract::domain::{RunOutcome, RunSummary, TriggerRef, TriggerType};
    use uuid::Uuid;

    fn run(time: i64) -> RunSummary {
        RunSummary {
            id: Uuid::new_v4(),
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
        }
    }

    #[test]
    fn newest_first_keeps_the_lower_bound_in_next() {
        let runs = (0..5).map(run).collect();
        let from = DateTime::from_timestamp(0, 0).unwrap();
        let (page, next) = page_runs(runs, Some(from), None, MessageOrder::NewestFirst, 2);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].time.timestamp(), 4);
        assert_eq!(page[1].time.timestamp(), 3);
        let next = next.unwrap();
        assert_eq!(next.from, Some(from));
        assert_eq!(next.until.unwrap().timestamp(), 3);
    }

    #[test]
    fn oldest_first_keeps_the_upper_bound_in_next() {
        let runs = (0..5).map(run).collect();
        let until = DateTime::from_timestamp(5, 0).unwrap();
        let (page, next) = page_runs(runs, None, Some(until), MessageOrder::OldestFirst, 2);
        assert_eq!(page.len(), 2);
        let next = next.unwrap();
        assert_eq!(next.from.unwrap().timestamp(), 1);
        assert_eq!(next.until, Some(until));
    }
}
