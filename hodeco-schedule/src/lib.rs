//! Calendar, solar, and repeat schedule computation.
//!
//! All calculations are done in the schedule's effective IANA timezone. The
//! scheduler stores only in-memory state; a restart recomputes the next future
//! occurrence from the catalog.

pub mod jitter;

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset,
    TimeDelta, TimeZone, Utc, Weekday, offset::Local,
};
use chrono_tz::Tz;
use hodeco_contract::domain::{
    AtSpec, DayMatch, DaySpec, Location, OccurrenceKind, ParsedAt, ScheduleDate,
    ScheduleDefinition, ScheduleEvery, ScheduleOccurrence, SolarEvent,
};

const SEARCH_YEARS: i32 = 5;
const MAX_SEARCH_YEARS: i32 = 100;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScheduleContext {
    pub default_location: Option<Location>,
    pub default_timezone: Option<Tz>,
}

impl ScheduleContext {
    pub fn timezone(&self, schedule: &ScheduleDefinition) -> Result<Tz, String> {
        if let Some(timezone) = &schedule.timezone {
            return timezone
                .parse::<Tz>()
                .map_err(|_| format!("unknown timezone '{timezone}'"));
        }
        if let Some(timezone) = self.default_timezone {
            return Ok(timezone);
        }
        system_timezone()
    }

    pub fn location(&self, schedule: &ScheduleDefinition) -> Option<Location> {
        schedule.location.or(self.default_location)
    }
}

pub fn system_timezone() -> Result<Tz, String> {
    if let Ok(name) = iana_time_zone::get_timezone()
        && let Ok(tz) = name.parse::<Tz>()
    {
        return Ok(tz);
    }
    let offset = Local::now().offset().local_minus_utc();
    let now = Utc::now().naive_utc();
    for candidate in chrono_tz::TZ_VARIANTS {
        if candidate
            .offset_from_utc_datetime(&now)
            .fix()
            .local_minus_utc()
            == offset
        {
            return Ok(candidate);
        }
    }
    Err("unable to determine the system timezone; set schedules.timezone".to_string())
}

/// Returns the next occurrence strictly after `after` unless `inclusive`.
///
/// A repeat chain can still be active when the caller asks for the next
/// occurrence.
/// The stateless form looks back over the repeat span and the previous base
/// occurrence so it can return a repeat that follows that base.
pub fn next_occurrence(
    schedule: &ScheduleDefinition,
    context: ScheduleContext,
    after: DateTime<Utc>,
    inclusive: bool,
) -> Result<Option<ScheduleOccurrence>, String> {
    let start = cursor_start(schedule, context, after);
    let lookback = after - start;
    let mut cursor = ScheduleCursor::new(
        schedule.clone(),
        context,
        start,
        if lookback.is_zero() { inclusive } else { false },
        false,
    )?;
    loop {
        let Some(occurrence) = cursor.advance()? else {
            return Ok(None);
        };
        if occurrence.at > after || (inclusive && occurrence.at == after) {
            return Ok(Some(occurrence));
        }
    }
}

fn cursor_start(
    schedule: &ScheduleDefinition,
    context: ScheduleContext,
    after: DateTime<Utc>,
) -> DateTime<Utc> {
    if schedule.at.is_empty()
        && schedule.every.is_none()
        && !schedule.repeat_offsets().is_empty()
        && let Some(from) = schedule.from
        && let Ok(tz) = context.timezone(schedule)
        && let Some(from) = instant_for_date(from, tz)
        && from <= after
    {
        return from - TimeDelta::nanoseconds(1);
    }
    after - repeat_lookback(schedule)
}

fn repeat_lookback(schedule: &ScheduleDefinition) -> TimeDelta {
    let offsets = schedule.repeat_offsets();
    let Some(max_offset) = offsets.last() else {
        return TimeDelta::zero();
    };
    let mut total = base_lookback(schedule);
    total += TimeDelta::from_std(*max_offset).unwrap_or(TimeDelta::zero());
    if let Some(jitter) = schedule.jitter {
        total += TimeDelta::from_std(jitter.as_std()).unwrap_or(TimeDelta::zero());
    }
    total.min(TimeDelta::days(SEARCH_YEARS as i64 * 366 + 2))
}

fn base_lookback(schedule: &ScheduleDefinition) -> TimeDelta {
    let cadence = if schedule.at.is_empty() {
        match schedule.every {
            Some(every) if every.uses_elapsed_time() => every
                .duration()
                .and_then(|duration| TimeDelta::from_std(duration).ok())
                .unwrap_or(TimeDelta::zero()),
            Some(every) => calendar_period(every),
            None => TimeDelta::zero(),
        }
    } else {
        match schedule.every {
            Some(every) if !every.uses_elapsed_time() => calendar_period(every),
            _ => TimeDelta::days(2),
        }
    };
    cadence.max(filter_lookback(schedule))
}

fn filter_lookback(schedule: &ScheduleDefinition) -> TimeDelta {
    if !schedule.years.is_empty() {
        return TimeDelta::days(SEARCH_YEARS as i64 * 366 + 2);
    }
    if !schedule.months.is_empty() {
        return TimeDelta::days(397);
    }
    if schedule.days.iter().any(|day| {
        matches!(
            day,
            DaySpec::NthWeekday { nth, .. } if nth.unsigned_abs() == 5
        )
    }) {
        return TimeDelta::days(366);
    }
    if schedule.days.iter().any(|day| {
        matches!(
            day,
            DaySpec::MonthDay(_)
                | DaySpec::LastDay
                | DaySpec::LastWeekday
                | DaySpec::NearestWeekday(_)
                | DaySpec::NthWeekday { .. }
        )
    }) {
        return TimeDelta::days(62);
    }
    if !schedule.days.is_empty() {
        return TimeDelta::days(7);
    }
    TimeDelta::zero()
}

fn calendar_period(every: ScheduleEvery) -> TimeDelta {
    match every {
        ScheduleEvery::Days(days) => TimeDelta::days(i64::from(days)),
        ScheduleEvery::Weeks(weeks) => TimeDelta::days(i64::from(weeks) * 7),
        ScheduleEvery::Months(_) | ScheduleEvery::Years(_) => {
            TimeDelta::days(SEARCH_YEARS as i64 * 366 + 2)
        }
        ScheduleEvery::Duration(duration) => {
            TimeDelta::from_std(duration.as_std()).unwrap_or_else(|_| TimeDelta::zero())
        }
    }
}

/// Returns up to `limit` occurrences, including repeat attempts and skipped
/// allow/deny candidates.
pub fn preview(
    schedule: &ScheduleDefinition,
    context: ScheduleContext,
    after: DateTime<Utc>,
    limit: usize,
    include_skipped: bool,
) -> Result<Vec<ScheduleOccurrence>, String> {
    let start = cursor_start(schedule, context, after);
    let mut cursor = ScheduleCursor::new(schedule.clone(), context, start, false, include_skipped)?;
    let mut occurrences = Vec::new();
    while occurrences.len() < limit {
        match cursor.advance()? {
            Some(occurrence) => {
                if occurrence.at < after {
                    continue;
                }
                occurrences.push(occurrence)
            }
            None => break,
        }
    }
    Ok(occurrences)
}

pub struct ScheduleCursor {
    schedule: ScheduleDefinition,
    context: ScheduleContext,
    tz: Tz,
    after: DateTime<Utc>,
    inclusive: bool,
    include_skipped: bool,
    /// Repeat state after a base occurrence.
    repeat_state: Option<RepeatState>,
    /// Next base occurrence to consider.
    next_base: Option<DateTime<Utc>>,
    /// True when the remaining calendar has no future base occurrence.
    next_base_exhausted: bool,
    /// Exclusive lower bound for the next base occurrence.
    base_cursor: DateTime<Utc>,
    /// Cursor for date-based iteration.
    date_cursor: NaiveDate,
    /// Iteration guard.
    iterations: u32,
}

#[derive(Debug, Clone)]
struct RepeatState {
    base: DateTime<Utc>,
    next_attempt: usize,
    offsets: Vec<Duration>,
}

impl ScheduleCursor {
    pub fn new(
        schedule: ScheduleDefinition,
        context: ScheduleContext,
        after: DateTime<Utc>,
        inclusive: bool,
        include_skipped: bool,
    ) -> Result<Self, String> {
        let tz = context.timezone(&schedule)?;
        Ok(Self::with_timezone(
            schedule,
            context,
            tz,
            after,
            inclusive,
            include_skipped,
        ))
    }

    fn with_timezone(
        schedule: ScheduleDefinition,
        context: ScheduleContext,
        tz: Tz,
        after: DateTime<Utc>,
        inclusive: bool,
        include_skipped: bool,
    ) -> Self {
        let local_after = after.with_timezone(&tz);
        let date_cursor = local_after.date_naive() - Duration::days(1);
        Self {
            schedule,
            context,
            tz,
            after,
            inclusive,
            include_skipped,
            repeat_state: None,
            next_base: None,
            next_base_exhausted: false,
            base_cursor: after,
            date_cursor,
            iterations: 0,
        }
    }

    fn base_after(&self) -> DateTime<Utc> {
        self.base_cursor
    }

    pub fn advance(&mut self) -> Result<Option<ScheduleOccurrence>, String> {
        // Each call is a fresh search. Keeping the counter across calls would
        // eventually make a long-lived cursor fail permanently.
        self.iterations = 0;
        loop {
            self.iterations += 1;
            if self.iterations > 2_000_000 {
                return Err("schedule search exceeded the iteration limit".to_string());
            }

            // Repeat attempts run between base occurrences. A repeat offset
            // that lands on or after the next base is skipped, and the base
            // then replaces the remaining repeat list.
            if let Some(mut repeat_state) = self.repeat_state.take() {
                let attempt = repeat_state.next_attempt;
                if let Some(offset) = repeat_state.offsets.get(attempt).copied() {
                    let next = repeat_state.base + offset;
                    let can_repeat = self.schedule.until.is_none_or(|until| {
                        self.until_instant(until).is_none_or(|until| next < until)
                    });
                    if can_repeat {
                        if self.next_base.is_none() && !self.next_base_exhausted {
                            self.next_base = self.compute_next_base()?;
                            self.next_base_exhausted = self.next_base.is_none();
                        }
                        if self.next_base.is_none_or(|base| base > next) {
                            repeat_state.next_attempt += 1;
                            self.repeat_state = Some(repeat_state);
                            if (next > self.after || (self.inclusive && next == self.after))
                                && let Some(occurrence) = self.filter_occurrence(
                                    next,
                                    OccurrenceKind::Repeat,
                                    None,
                                    Some(attempt as u32),
                                )?
                            {
                                return Ok(Some(occurrence));
                            }
                            continue;
                        }
                    }
                }
            }

            let base = match self.next_base.take() {
                Some(base) => base,
                None if self.next_base_exhausted => return Ok(None),
                None => {
                    let Some(base) = self.compute_next_base()? else {
                        return Ok(None);
                    };
                    base
                }
            };
            self.next_base = None;

            // The base occurrence replaces any older repeat chain.
            let repeat_offsets = self.schedule.repeat_offsets();
            if !repeat_offsets.is_empty() {
                self.start_repeat(base, &repeat_offsets);
            }

            self.base_cursor = base + TimeDelta::nanoseconds(1);
            let Some(occurrence) =
                self.filter_occurrence(base, OccurrenceKind::Scheduled, None, None)?
            else {
                continue;
            };
            return Ok(Some(occurrence));
        }
    }

    fn start_repeat(&mut self, base: DateTime<Utc>, offsets: &[std::time::Duration]) {
        self.next_base_exhausted = false;
        let offsets: Vec<Duration> = offsets
            .iter()
            .map(|duration| Duration::from_std(*duration).unwrap_or(Duration::seconds(1)))
            .collect();
        self.repeat_state = Some(RepeatState {
            base,
            next_attempt: 0,
            offsets,
        });
    }

    fn compute_next_base(&mut self) -> Result<Option<DateTime<Utc>>, String> {
        let from = match self.schedule.from {
            Some(from) => self.date_instant(from)?,
            None => None,
        };
        let until = self
            .schedule
            .until
            .and_then(|until| self.until_instant(until));

        if self.schedule.at.is_empty() && self.schedule.every.is_none() {
            let Some(at) = from else {
                return Ok(None);
            };
            if at < self.base_after() || (!self.inclusive && at == self.base_after()) {
                return Ok(None);
            }
            if until.is_some_and(|until| at >= until) {
                return Ok(None);
            }
            return Ok(Some(at));
        }

        if let Some(every) = self.schedule.every
            && let Some(duration) = every.duration()
            && every.uses_elapsed_time()
        {
            return self.compute_interval(from, until, duration);
        }

        let anchor_date = match self.schedule.from {
            Some(ScheduleDate::Date(date)) => date,
            Some(ScheduleDate::DateTime(value)) => value.with_timezone(&self.tz).date_naive(),
            None => self.base_cursor.with_timezone(&self.tz).date_naive() - Duration::days(1),
        };
        let start_date = self.date_cursor.max(anchor_date);
        let mut date = start_date;

        // A schedule with an old `from` date must still find the next
        // occurrence. Search from the cursor, not from the original anchor, and
        // extend the horizon to requested `years` values.
        let mut horizon = start_date + Duration::days(i64::from(SEARCH_YEARS) * 366 + 1);
        if let Some(year) = self.schedule.years.iter().copied().max()
            && let Some(year_end) = NaiveDate::from_ymd_opt(year, 12, 31)
        {
            horizon = horizon.max(year_end);
        }
        horizon = horizon.min(start_date + Duration::days(i64::from(MAX_SEARCH_YEARS) * 366 + 1));

        for _ in 0..=(horizon - start_date).num_days() {
            if let Some(until) = until
                && self
                    .tz
                    .from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
                    .with_timezone(&Utc)
                    >= until
            {
                return Ok(None);
            }
            if self.date_matches(date)? {
                let candidates = self.candidates_for_date(date, from, until)?;
                for candidate in candidates {
                    if candidate < self.base_cursor
                        || (!self.inclusive && candidate == self.base_cursor)
                    {
                        continue;
                    }
                    if until.is_some_and(|until| candidate >= until) {
                        continue;
                    }
                    self.date_cursor = date;
                    return Ok(Some(candidate));
                }
            }
            date += Duration::days(1);
            self.date_cursor = date;
            if date > horizon {
                break;
            }
        }
        Ok(None)
    }

    fn compute_interval(
        &mut self,
        from: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        every: std::time::Duration,
    ) -> Result<Option<DateTime<Utc>>, String> {
        let Some(from) = from else {
            return Err("every for seconds, minutes, or hours requires from".to_string());
        };
        let interval =
            TimeDelta::from_std(every).map_err(|_| "interval is too large".to_string())?;
        if interval <= TimeDelta::zero() {
            return Err("interval must be greater than 0".to_string());
        }
        let after = self.base_after();
        let elapsed = (after - from).num_nanoseconds().unwrap_or(0);
        let mut step = if elapsed <= 0 {
            0
        } else {
            let step = elapsed / interval.num_nanoseconds().unwrap_or(1);
            step + 1
        };
        // The search may need to skip allow/deny candidates; cap the search.
        for _ in 0..100_000 {
            let at = from
                + TimeDelta::nanoseconds(
                    interval.num_nanoseconds().unwrap_or(0).saturating_mul(step),
                );
            if until.is_some_and(|until| at >= until) {
                return Ok(None);
            }
            if at < after || (!self.inclusive && at == after) {
                step += 1;
                continue;
            }
            if self.allow_matches(at) {
                self.next_base = Some(at);
                return Ok(Some(at));
            }
            step += 1;
        }
        Ok(None)
    }

    fn candidates_for_date(
        &self,
        date: NaiveDate,
        from: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<Vec<DateTime<Utc>>, String> {
        let mut candidates = Vec::new();
        let at_entries: Vec<&AtSpec> = self.schedule.at.as_slice().iter().collect();
        if at_entries.is_empty() {
            let time = self.default_time();
            if let Some(candidate) = self.local_instant(date, time) {
                candidates.push(candidate);
            }
        } else {
            for at in at_entries {
                match at.parse()? {
                    ParsedAt::Time(time) => {
                        if let Some(candidate) = self.local_instant(date, time) {
                            candidates.push(candidate);
                        }
                    }
                    ParsedAt::HourlyMinute(minute) => {
                        for hour in 0..24 {
                            if let Some(time) = NaiveTime::from_hms_opt(hour, minute as u32, 0)
                                && let Some(candidate) = self.local_instant(date, time)
                            {
                                candidates.push(candidate);
                            }
                        }
                    }
                    ParsedAt::Solar {
                        event,
                        offset_seconds,
                    } => {
                        let Some(location) = self.context.location(&self.schedule) else {
                            return Err("solar schedules require a location".to_string());
                        };
                        if let Some(candidate) =
                            solar_event(date, event, location, self.tz, offset_seconds)?
                        {
                            candidates.push(candidate);
                        }
                    }
                }
            }
        }
        candidates.retain(|candidate| {
            from.is_none_or(|from| *candidate >= from)
                && until.is_none_or(|until| *candidate < until)
        });
        candidates.sort();
        candidates.dedup();
        Ok(candidates)
    }

    fn date_matches(&self, date: NaiveDate) -> Result<bool, String> {
        if !self.schedule.months.is_empty()
            && !self
                .schedule
                .months
                .iter()
                .any(|month| month.number() == date.month())
        {
            return Ok(false);
        }
        if !self.schedule.years.is_empty() && !self.schedule.years.contains(&date.year()) {
            return Ok(false);
        }
        if self
            .schedule
            .except
            .iter()
            .any(|range| range.contains(date))
        {
            return Ok(false);
        }
        if !self.schedule.days.is_empty() {
            let matches = |day: &DaySpec| day_matches(day, date);
            let matched = if self.schedule.day_match == DayMatch::All {
                self.schedule.days.iter().all(matches)
            } else {
                self.schedule.days.iter().any(matches)
            };
            if !matched {
                return Ok(false);
            }
        }

        if let Some(every) = self.schedule.every
            && !every.uses_elapsed_time()
        {
            let anchor = match self.schedule.from {
                Some(ScheduleDate::Date(date)) => date,
                Some(ScheduleDate::DateTime(value)) => value.with_timezone(&self.tz).date_naive(),
                None => return Ok(true),
            };
            return Ok(on_boundary(anchor, date, every));
        }
        Ok(true)
    }

    fn filter_occurrence(
        &self,
        base: DateTime<Utc>,
        kind: OccurrenceKind,
        reason: Option<String>,
        attempt: Option<u32>,
    ) -> Result<Option<ScheduleOccurrence>, String> {
        if let Some(from) = self.schedule.from
            && let Some(from) = self.date_instant(from)?
            && base < from
        {
            return self.skipped_or_none(base, kind, "before from", attempt);
        }
        if let Some(until) = self.schedule.until
            && let Some(until) = self.until_instant(until)
            && base >= until
        {
            return Ok(None);
        }

        let local = base.with_timezone(&self.tz);
        let local_time = local.time();
        if self
            .schedule
            .except
            .iter()
            .any(|range| range.contains(local.date_naive()))
        {
            return Ok(None);
        }
        let denied = self
            .schedule
            .deny
            .iter()
            .any(|window| window.contains(local_time));
        let allowed = self.schedule.allow.is_empty()
            || self
                .schedule
                .allow
                .iter()
                .any(|window| window.contains(local_time));
        if denied || !allowed {
            let reason = reason.unwrap_or_else(|| {
                if denied {
                    "denied by time window".to_string()
                } else {
                    "outside allow window".to_string()
                }
            });
            return self.skipped_or_none(base, kind, &reason, attempt);
        }

        let at = if let Some(jitter) = self.schedule.jitter {
            let offset = crate::jitter::offset(
                self.schedule.id.as_bytes(),
                base.timestamp_nanos_opt().unwrap_or(0),
                attempt.unwrap_or(0),
                jitter.as_std(),
            );
            clamp_jitter(base + offset, &self.schedule, self.tz)?.max(self.after)
        } else {
            base
        };
        if at < self.after || (!self.inclusive && at == self.after) {
            return Ok(None);
        }
        Ok(Some(self.occurrence(at, kind, false, reason, attempt)))
    }

    fn skipped_or_none(
        &self,
        at: DateTime<Utc>,
        kind: OccurrenceKind,
        reason: &str,
        attempt: Option<u32>,
    ) -> Result<Option<ScheduleOccurrence>, String> {
        if self.include_skipped {
            Ok(Some(self.occurrence(
                at,
                kind,
                true,
                Some(reason.to_string()),
                attempt,
            )))
        } else {
            Ok(None)
        }
    }

    fn occurrence(
        &self,
        at: DateTime<Utc>,
        kind: OccurrenceKind,
        skipped: bool,
        reason: Option<String>,
        attempt: Option<u32>,
    ) -> ScheduleOccurrence {
        ScheduleOccurrence {
            at,
            local: at
                .with_timezone(&self.tz)
                .format("%Y-%m-%d %H:%M:%S %Z")
                .to_string(),
            repeat_index: if kind == OccurrenceKind::Repeat {
                attempt.map(|attempt| attempt + 1)
            } else {
                None
            },
            kind,
            skipped,
            reason,
        }
    }

    fn allow_matches(&self, at: DateTime<Utc>) -> bool {
        let local = at.with_timezone(&self.tz);
        let time = local.time();
        let denied = self
            .schedule
            .deny
            .iter()
            .any(|window| window.contains(time));
        let allowed = self.schedule.allow.is_empty()
            || self
                .schedule
                .allow
                .iter()
                .any(|window| window.contains(time));
        !denied && allowed
    }

    fn default_time(&self) -> NaiveTime {
        match self.schedule.from {
            Some(ScheduleDate::DateTime(value)) => value.with_timezone(&self.tz).time(),
            _ => NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        }
    }

    fn date_instant(&self, value: ScheduleDate) -> Result<Option<DateTime<Utc>>, String> {
        match value {
            ScheduleDate::DateTime(value) => Ok(Some(value.with_timezone(&Utc))),
            ScheduleDate::Date(date) => Ok(self
                .tz
                .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
                .earliest()
                .map(|value| value.with_timezone(&Utc))),
        }
    }

    fn until_instant(&self, value: ScheduleDate) -> Option<DateTime<Utc>> {
        self.date_instant(value).ok().flatten()
    }

    fn local_instant(&self, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
        let naive = NaiveDateTime::new(date, time);
        match self.tz.from_local_datetime(&naive) {
            LocalResult::Single(value) => Some(value.with_timezone(&Utc)),
            LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
            LocalResult::None => None,
        }
    }
}

fn on_boundary(anchor: NaiveDate, date: NaiveDate, every: ScheduleEvery) -> bool {
    if date < anchor {
        return false;
    }
    match every {
        ScheduleEvery::Days(days) => (date - anchor).num_days() % i64::from(days) == 0,
        ScheduleEvery::Weeks(weeks) => (date - anchor).num_days() % (i64::from(weeks) * 7) == 0,
        ScheduleEvery::Months(months) => {
            let months_between =
                (date.year() - anchor.year()) * 12 + date.month() as i32 - anchor.month() as i32;
            months_between >= 0 && months_between % months as i32 == 0 && date.day() == anchor.day()
        }
        ScheduleEvery::Years(years) => {
            (date.year() - anchor.year()) % years as i32 == 0
                && date.month() == anchor.month()
                && date.day() == anchor.day()
        }
        ScheduleEvery::Duration(_) => false,
    }
}

fn day_matches(day: &DaySpec, date: NaiveDate) -> bool {
    match day {
        DaySpec::Weekday(weekday) => date.weekday() == *weekday,
        DaySpec::MonthDay(day) => date.day() == *day as u32,
        DaySpec::LastDay => date
            .succ_opt()
            .is_none_or(|next| next.month() != date.month()),
        DaySpec::LastWeekday => last_weekday(date.year(), date.month()) == Some(date),
        DaySpec::NearestWeekday(day) => {
            nearest_weekday(date.year(), date.month(), *day).is_some_and(|nearest| nearest == date)
        }
        DaySpec::NthWeekday { nth, weekday } => {
            nth_weekday(date.year(), date.month(), *nth, *weekday)
                .is_some_and(|target| target == date)
        }
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first_next = NaiveDate::from_ymd_opt(next_year, next_month, 1).unwrap();
    first_next.pred_opt().unwrap().day()
}

fn last_weekday(year: i32, month: u32) -> Option<NaiveDate> {
    let mut date = NaiveDate::from_ymd_opt(year, month, days_in_month(year, month))?;
    while matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
        date = date.pred_opt()?;
    }
    Some(date)
}

fn nearest_weekday(year: i32, month: u32, day: u8) -> Option<NaiveDate> {
    let last_day = days_in_month(year, month);
    let day = day.min(last_day as u8);
    let date = NaiveDate::from_ymd_opt(year, month, day as u32)?;
    let candidate = match date.weekday() {
        // Keep the nearest weekday inside the requested month. A weekend first
        // or last day moves inward rather than into the adjacent month.
        Weekday::Sat if day == 1 => date.succ_opt()?.succ_opt()?,
        Weekday::Sat => date.pred_opt()?,
        Weekday::Sun if u32::from(day) == last_day => date.pred_opt()?.pred_opt()?,
        Weekday::Sun => date.succ_opt()?,
        _ => date,
    };
    (candidate.year() == year && candidate.month() == month).then_some(candidate)
}

fn nth_weekday(year: i32, month: u32, nth: i8, weekday: Weekday) -> Option<NaiveDate> {
    if nth > 0 {
        let first = NaiveDate::from_ymd_opt(year, month, 1)?;
        let offset = (weekday.num_days_from_monday() as i64
            - first.weekday().num_days_from_monday() as i64)
            .rem_euclid(7);
        let date = first + Duration::days(offset + (nth as i64 - 1) * 7);
        (date.month() == month).then_some(date)
    } else {
        let last = NaiveDate::from_ymd_opt(year, month, days_in_month(year, month))?;
        let offset = (last.weekday().num_days_from_monday() as i64
            - weekday.num_days_from_monday() as i64)
            .rem_euclid(7);
        let date = last - Duration::days(offset + ((-nth) as i64 - 1) * 7);
        (date.month() == month).then_some(date)
    }
}

fn clamp_jitter(
    at: DateTime<Utc>,
    schedule: &ScheduleDefinition,
    tz: Tz,
) -> Result<DateTime<Utc>, String> {
    let mut at = at;
    if let Some(from) = schedule.from
        && let Some(from) = instant_for_date(from, tz)
    {
        at = at.max(from);
    }
    if let Some(until) = schedule.until
        && let Some(until) = instant_for_date(until, tz)
    {
        at = at.min(until - TimeDelta::nanoseconds(1));
    }
    Ok(at)
}

fn instant_for_date(value: ScheduleDate, tz: Tz) -> Option<DateTime<Utc>> {
    match value {
        ScheduleDate::DateTime(value) => Some(value.with_timezone(&Utc)),
        ScheduleDate::Date(date) => tz
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .map(|value| value.with_timezone(&Utc)),
    }
}

/// NOAA-style solar event calculation.
pub fn solar_event(
    date: NaiveDate,
    event: SolarEvent,
    location: Location,
    tz: Tz,
    offset_seconds: i64,
) -> Result<Option<DateTime<Utc>>, String> {
    if !location.is_valid() {
        return Err("location is not valid".to_string());
    }
    let local_noon = tz
        .from_local_datetime(&date.and_hms_opt(12, 0, 0).unwrap())
        .earliest()
        .ok_or_else(|| format!("local noon does not exist on {date}"))?;
    let utc_date = local_noon.with_timezone(&Utc).date_naive();

    for delta in [0_i64, -1, 1] {
        let candidate_date = utc_date + Duration::days(delta);
        if let Some(at) = solar_event_for_date(candidate_date, event, location)? {
            let local = at.with_timezone(&tz);
            if local.date_naive() == date {
                return Ok(Some(at + TimeDelta::seconds(offset_seconds)));
            }
        }
    }
    Ok(None)
}

fn solar_event_for_date(
    date: NaiveDate,
    event: SolarEvent,
    location: Location,
) -> Result<Option<DateTime<Utc>>, String> {
    let latitude = location.latitude.to_radians();
    let longitude = location.longitude;
    let julian = julian_day(date);
    let n = julian - 2451545.0 + 0.0008;
    let j_star = n - longitude / 360.0;
    let m = (357.5291 + 0.98560028 * j_star).rem_euclid(360.0);
    let m_rad = m.to_radians();
    let c = 1.9148 * m_rad.sin() + 0.02 * (2.0 * m_rad).sin() + 0.0003 * (3.0 * m_rad).sin();
    let lambda = (m + c + 180.0 + 102.9372).rem_euclid(360.0);
    let lambda_rad = lambda.to_radians();
    let j_transit = 2451545.0 + j_star + 0.0053 * m_rad.sin() - 0.0069 * (2.0 * lambda_rad).sin();
    let sin_declination = lambda_rad.sin() * 23.44_f64.to_radians().sin();
    let declination = sin_declination.asin();

    let altitude = match event {
        SolarEvent::Sunrise | SolarEvent::Sunset => {
            let dip = location
                .elevation
                .map(|elevation| 0.0293 * elevation.max(0.0).sqrt())
                .unwrap_or(0.0);
            (-0.833 - dip).to_radians()
        }
        SolarEvent::Dawn | SolarEvent::Dusk => (-6.0_f64).to_radians(),
        SolarEvent::Noon => {
            return Ok(Some(julian_to_utc(j_transit)?));
        }
        SolarEvent::Nadir => {
            return Ok(Some(julian_to_utc(j_transit + 0.5)?));
        }
    };

    let cos_omega = (altitude.sin() - latitude.sin() * declination.sin())
        / (latitude.cos() * declination.cos());
    if !(-1.0..=1.0).contains(&cos_omega) {
        return Ok(None);
    }
    let omega = cos_omega.acos().to_degrees();
    let j = match event {
        SolarEvent::Sunrise | SolarEvent::Dawn => j_transit - omega / 360.0,
        SolarEvent::Sunset | SolarEvent::Dusk => j_transit + omega / 360.0,
        _ => unreachable!(),
    };
    Ok(Some(julian_to_utc(j)?))
}

fn julian_day(date: NaiveDate) -> f64 {
    let year = date.year() as f64;
    let month = date.month() as f64;
    let day = date.day() as f64;
    367.0 * year - (7.0 * (year + ((month + 9.0) / 12.0).floor()) / 4.0).floor()
        + (275.0 * month / 9.0).floor()
        + day
        + 1721013.5
}

fn julian_to_utc(julian: f64) -> Result<DateTime<Utc>, String> {
    let seconds = (julian - 2440587.5) * 86_400.0;
    if !seconds.is_finite() {
        return Err("solar event is out of range".to_string());
    }
    let nanos = (seconds * 1_000_000_000.0).round() as i64;
    let secs = nanos.div_euclid(1_000_000_000);
    let sub = nanos.rem_euclid(1_000_000_000) as u32;
    Utc.timestamp_opt(secs, sub)
        .single()
        .ok_or_else(|| "solar event is out of range".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodeco_contract::domain::{AtSpec, DateRange, OneOrMany, ScheduleEvery, TimeWindow};
    use hodeco_contract::utils::HumanDuration;

    fn schedule(at: &str) -> ScheduleDefinition {
        ScheduleDefinition {
            id: "test".to_string(),
            name: "Test".to_string(),
            timezone: Some("UTC".to_string()),
            at: OneOrMany::One(AtSpec::Text(at.to_string())),
            ..ScheduleDefinition::default()
        }
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn computes_fixed_times() {
        let schedule = schedule("09:00");
        let after = at("2026-11-17T08:00:00Z");
        let next = next_occurrence(&schedule, context(), after, false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-17T09:00:00Z"));
    }

    #[test]
    fn skips_denied_windows_and_exceptions() {
        let mut schedule = schedule("08:00");
        schedule.at = OneOrMany::Many(vec![
            AtSpec::Text("08:00".to_string()),
            AtSpec::Text("11:00".to_string()),
        ]);
        schedule.deny = OneOrMany::One(TimeWindow::parse("07:00~09:00").unwrap());
        let next = next_occurrence(&schedule, context(), at("2026-11-17T07:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-17T11:00:00Z"));

        schedule.deny = OneOrMany::Many(Vec::new());
        schedule.at = OneOrMany::One(AtSpec::Text("09:00".to_string()));
        schedule.except = OneOrMany::One(DateRange::parse("2026-11-17").unwrap());
        let next = next_occurrence(&schedule, context(), at("2026-11-17T08:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-18T09:00:00Z"));
    }

    #[test]
    fn exceptions_skip_elapsed_interval_occurrences() {
        let mut schedule = schedule("09:00");
        schedule.from = Some(ScheduleDate::parse("2026-11-19").unwrap());
        schedule.at = OneOrMany::Many(Vec::new());
        schedule.every = Some(ScheduleEvery::parse("20m").unwrap());
        schedule.except = OneOrMany::One(DateRange::parse("2026-11-19..2026-11-20").unwrap());
        let next = next_occurrence(&schedule, context(), at("2026-11-19T00:00:00Z"), true)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-21T00:00:00Z"));
    }

    #[test]
    fn computes_calendar_intervals() {
        let mut schedule = schedule("09:00");
        schedule.from = Some(ScheduleDate::parse("2026-11-17").unwrap());
        schedule.every = Some(ScheduleEvery::Days(3));
        let next = next_occurrence(&schedule, context(), at("2026-11-18T00:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-20T09:00:00Z"));
    }

    #[test]
    fn computes_elapsed_intervals_longer_than_a_day() {
        let mut schedule = schedule("09:00");
        schedule.from = Some(ScheduleDate::parse("2026-11-19").unwrap());
        schedule.at = OneOrMany::Many(Vec::new());
        schedule.every = Some(ScheduleEvery::parse("1800m").unwrap());
        let mut cursor = ScheduleCursor::new(
            schedule,
            context(),
            at("2026-11-18T23:00:00Z"),
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            cursor.advance().unwrap().unwrap().at,
            at("2026-11-19T00:00:00Z")
        );
        assert_eq!(
            cursor.advance().unwrap().unwrap().at,
            at("2026-11-20T06:00:00Z")
        );
    }

    #[test]
    fn computes_repeats_after_a_base_occurrence() {
        let mut schedule = schedule("09:00");
        schedule.repeat_at = OneOrMany::One(HumanDuration::from_std(
            std::time::Duration::from_secs(30 * 60),
        ));
        let next = next_occurrence(&schedule, context(), at("2026-11-17T08:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-17T09:00:00Z"));
        let next = next_occurrence(&schedule, context(), next.at, false)
            .unwrap()
            .unwrap();
        assert_eq!(next.kind, OccurrenceKind::Repeat);
        assert_eq!(next.repeat_index, Some(1));
        assert_eq!(next.at, at("2026-11-17T09:30:00Z"));
        let next = next_occurrence(&schedule, context(), next.at, false)
            .unwrap()
            .unwrap();
        assert_eq!(next.kind, OccurrenceKind::Scheduled);
        assert_eq!(next.at, at("2026-11-18T09:00:00Z"));
    }

    #[test]
    fn repeats_follow_a_calendar_cadence() {
        let mut schedule = schedule("09:00");
        schedule.from = Some(ScheduleDate::parse("2026-11-17").unwrap());
        schedule.every = Some(ScheduleEvery::Days(3));
        schedule.repeat_at = OneOrMany::Many(vec![
            HumanDuration::from_std(std::time::Duration::from_secs(30 * 60)),
            HumanDuration::from_std(std::time::Duration::from_secs(60 * 60)),
        ]);
        let next = next_occurrence(&schedule, context(), at("2026-11-17T09:20:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.kind, OccurrenceKind::Repeat);
        assert_eq!(next.at, at("2026-11-17T09:30:00Z"));
        let next = next_occurrence(&schedule, context(), next.at, false)
            .unwrap()
            .unwrap();
        assert_eq!(next.kind, OccurrenceKind::Repeat);
        assert_eq!(next.repeat_index, Some(2));
        assert_eq!(next.at, at("2026-11-17T10:00:00Z"));
        let next = next_occurrence(&schedule, context(), next.at, false)
            .unwrap()
            .unwrap();
        assert_eq!(next.kind, OccurrenceKind::Scheduled);
        assert_eq!(next.at, at("2026-11-20T09:00:00Z"));
    }

    #[test]
    fn repeats_skip_when_the_next_base_comes_first() {
        let mut schedule = schedule("09:00");
        schedule.at = OneOrMany::Many(Vec::new());
        schedule.from = Some(ScheduleDate::parse("2026-11-17T09:00:00Z").unwrap());
        schedule.every = Some(ScheduleEvery::parse("1h").unwrap());
        schedule.repeat_at = OneOrMany::Many(vec![
            HumanDuration::from_std(std::time::Duration::from_secs(10 * 60)),
            HumanDuration::from_std(std::time::Duration::from_secs(90 * 60)),
        ]);
        let next = next_occurrence(&schedule, context(), at("2026-11-17T09:05:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.kind, OccurrenceKind::Repeat);
        assert_eq!(next.at, at("2026-11-17T09:10:00Z"));
        let next = next_occurrence(&schedule, context(), next.at, false)
            .unwrap()
            .unwrap();
        assert_eq!(next.kind, OccurrenceKind::Scheduled);
        assert_eq!(next.at, at("2026-11-17T10:00:00Z"));
    }

    #[test]
    fn repeats_between_interval_base_occurrences() {
        let mut schedule = schedule("09:00");
        schedule.at = OneOrMany::Many(Vec::new());
        schedule.from = Some(ScheduleDate::parse("2026-11-17T09:00:00Z").unwrap());
        schedule.every = Some(ScheduleEvery::parse("1h").unwrap());
        schedule.repeat_at = OneOrMany::One(HumanDuration::from_std(
            std::time::Duration::from_secs(10 * 60),
        ));
        let next = next_occurrence(&schedule, context(), at("2026-11-17T09:05:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.kind, OccurrenceKind::Repeat);
        assert_eq!(next.at, at("2026-11-17T09:10:00Z"));
        let next = next_occurrence(&schedule, context(), next.at, false)
            .unwrap()
            .unwrap();
        assert_eq!(next.kind, OccurrenceKind::Scheduled);
        assert_eq!(next.at, at("2026-11-17T10:00:00Z"));
    }

    #[test]
    fn repeats_after_a_from_only_one_shot() {
        let mut schedule = ScheduleDefinition {
            id: "test".to_string(),
            name: "Test".to_string(),
            timezone: Some("UTC".to_string()),
            from: Some(ScheduleDate::parse("2026-11-17T09:00:00Z").unwrap()),
            repeat_at: OneOrMany::Many(vec![
                HumanDuration::from_std(std::time::Duration::from_secs(30 * 60)),
                HumanDuration::from_std(std::time::Duration::from_secs(60 * 60)),
            ]),
            ..ScheduleDefinition::default()
        };
        let next = next_occurrence(&schedule, context(), at("2026-11-17T09:30:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.kind, OccurrenceKind::Repeat);
        assert_eq!(next.at, at("2026-11-17T10:00:00Z"));
        schedule.repeat_at = OneOrMany::One(HumanDuration::from_std(
            std::time::Duration::from_secs(30 * 60),
        ));
        assert!(
            next_occurrence(&schedule, context(), at("2026-11-17T09:30:00Z"), false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn negative_nth_weekday_counts_from_the_end() {
        use chrono::Weekday;
        let mut schedule = schedule("09:00");
        schedule.from = Some(ScheduleDate::parse("2026-11-01").unwrap());
        schedule.days = vec![DaySpec::NthWeekday {
            nth: -1,
            weekday: Weekday::Mon,
        }];
        let next = next_occurrence(&schedule, context(), at("2026-11-01T00:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-30T09:00:00Z"));

        schedule.days = vec![DaySpec::NthWeekday {
            nth: -2,
            weekday: Weekday::Mon,
        }];
        let next = next_occurrence(&schedule, context(), at("2026-11-01T00:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-23T09:00:00Z"));
    }

    #[test]
    fn last_weekday_matches_only_the_final_weekday() {
        let mut schedule = schedule("09:00");
        schedule.from = Some(ScheduleDate::parse("2026-11-01").unwrap());
        schedule.days = vec![DaySpec::LastWeekday];
        let next = next_occurrence(&schedule, context(), at("2026-11-01T00:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-30T09:00:00Z"));
    }

    #[test]
    fn nearest_weekday_stays_inside_the_month() {
        assert_eq!(
            nearest_weekday(2026, 8, 1),
            Some(NaiveDate::from_ymd_opt(2026, 8, 3).unwrap())
        );
        assert_eq!(
            nearest_weekday(2026, 5, 31),
            Some(NaiveDate::from_ymd_opt(2026, 5, 29).unwrap())
        );
    }

    #[test]
    fn far_future_year_filter_is_reachable() {
        let mut schedule = schedule("09:00");
        schedule.from = Some(ScheduleDate::parse("2026-01-01").unwrap());
        schedule.years = vec![2040];
        let next = next_occurrence(&schedule, context(), at("2026-11-17T00:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2040-01-01T09:00:00Z"));
    }

    #[test]
    fn iteration_budget_resets_for_each_advance() {
        let mut schedule = schedule("09:00");
        schedule.from = Some(ScheduleDate::parse("2026-11-17").unwrap());
        let mut cursor = ScheduleCursor::new(
            schedule,
            context(),
            at("2026-11-17T00:00:00Z"),
            false,
            false,
        )
        .unwrap();
        cursor.iterations = 2_000_000;
        assert!(cursor.advance().unwrap().is_some());
    }

    #[test]
    fn negative_nth_weekday_skips_months_without_a_match() {
        use chrono::Weekday;
        let mut schedule = schedule("09:00");
        schedule.from = Some(ScheduleDate::parse("2026-02-01").unwrap());
        schedule.days = vec![DaySpec::NthWeekday {
            nth: -5,
            weekday: Weekday::Mon,
        }];
        let next = next_occurrence(&schedule, context(), at("2026-02-01T00:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-03-02T09:00:00Z"));
    }

    #[test]
    fn handles_solar_events_and_polar_nights() {
        let location = Location::new(52.52, 13.405, Some(34.0));
        let sunrise = solar_event(
            NaiveDate::from_ymd_opt(2026, 6, 21).unwrap(),
            SolarEvent::Sunrise,
            location,
            "Europe/Berlin".parse().unwrap(),
            0,
        )
        .unwrap()
        .unwrap();
        assert!(sunrise > at("2026-06-20T00:00:00Z"));
        assert!(sunrise < at("2026-06-22T00:00:00Z"));

        let polar = Location::new(78.0, 15.0, Some(0.0));
        assert!(
            solar_event(
                NaiveDate::from_ymd_opt(2026, 12, 21).unwrap(),
                SolarEvent::Sunrise,
                polar,
                "UTC".parse().unwrap(),
                0,
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn preview_includes_skipped_candidates() {
        let mut schedule = schedule("09:00");
        schedule.deny = OneOrMany::One(TimeWindow::parse("09:00~10:00").unwrap());
        let occurrences =
            preview(&schedule, context(), at("2026-11-17T00:00:00Z"), 3, true).unwrap();
        assert!(occurrences.iter().any(|occurrence| occurrence.skipped));
    }

    fn context() -> ScheduleContext {
        ScheduleContext {
            default_location: None,
            default_timezone: Some("UTC".parse().unwrap()),
        }
    }
}
