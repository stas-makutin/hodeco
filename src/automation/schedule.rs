//! Calendar, solar, and loop schedule computation.
//!
//! All calculations are done in the schedule's effective IANA timezone. The
//! scheduler stores only in-memory state; a restart recomputes the next future
//! occurrence from the catalog.

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset,
    TimeDelta, TimeZone, Utc, Weekday, offset::Local,
};
use chrono_tz::Tz;
use hodeco_contract::domain::{
    AtSpec, DayMatch, DaySpec, Location, LoopRule, OccurrenceKind, ParsedAt, ScheduleDate,
    ScheduleDefinition, ScheduleEvery, ScheduleOccurrence, ScheduleRule, SolarEvent,
};

const SEARCH_YEARS: i32 = 5;

#[derive(Debug, Clone, Copy)]
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
    for candidate in chrono_tz::TZ_VARIANTS {
        if candidate
            .offset_from_utc_datetime(&Utc::now().naive_utc())
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
/// A loop can still be active when the caller asks for the next occurrence.
/// The stateless form looks back over the maximum loop span so it can return a
/// loop attempt that follows the last base occurrence.
pub fn next_occurrence(
    schedule: &ScheduleDefinition,
    context: ScheduleContext,
    after: DateTime<Utc>,
    inclusive: bool,
) -> Result<Option<ScheduleOccurrence>, String> {
    let lookback = loop_lookback(schedule);
    let start = after - lookback;
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

fn loop_lookback(schedule: &ScheduleDefinition) -> TimeDelta {
    let Some(loop_rule) = &schedule.loop_rule else {
        return TimeDelta::zero();
    };
    let intervals = loop_rule.intervals();
    if intervals.is_empty() {
        return TimeDelta::zero();
    }
    let mut total = TimeDelta::zero();
    match loop_rule.repeat {
        Some(repeat) => {
            for attempt in 0..repeat {
                let interval = intervals[(attempt as usize).min(intervals.len().saturating_sub(1))];
                total += TimeDelta::from_std(interval).unwrap_or(TimeDelta::zero());
            }
        }
        None => {
            total = TimeDelta::days(31);
        }
    }
    if let Some(jitter) = loop_rule.jitter {
        total += TimeDelta::from_std(jitter.as_std()).unwrap_or(TimeDelta::zero());
    }
    if let Some(jitter) = schedule.jitter {
        total += TimeDelta::from_std(jitter.as_std()).unwrap_or(TimeDelta::zero());
    }
    total.min(TimeDelta::days(31))
}

/// Returns up to `limit` occurrences, including loop attempts and skipped
/// allow/deny candidates.
pub fn preview(
    schedule: &ScheduleDefinition,
    context: ScheduleContext,
    after: DateTime<Utc>,
    limit: usize,
    include_skipped: bool,
) -> Result<Vec<ScheduleOccurrence>, String> {
    let lookback = loop_lookback(schedule);
    let mut cursor = ScheduleCursor::new(
        schedule.clone(),
        context,
        after - lookback,
        false,
        include_skipped,
    )?;
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
    /// Loop state after a base occurrence.
    loop_state: Option<LoopState>,
    /// Next base occurrence to consider.
    next_base: Option<DateTime<Utc>>,
    /// Exclusive lower bound for the next base occurrence.
    base_cursor: DateTime<Utc>,
    /// Cursor for date-based iteration.
    date_cursor: NaiveDate,
    /// Iteration guard.
    iterations: u32,
}

#[derive(Debug, Clone)]
struct LoopState {
    attempt: u32,
    next: DateTime<Utc>,
    intervals: Vec<Duration>,
    repeat: Option<u32>,
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
            loop_state: None,
            next_base: None,
            base_cursor: after,
            date_cursor,
            iterations: 0,
        }
    }

    fn base_after(&self) -> DateTime<Utc> {
        self.base_cursor
    }

    pub fn advance(&mut self) -> Result<Option<ScheduleOccurrence>, String> {
        if self.schedule.on.is_none() {
            return self.next_one_shot();
        }
        loop {
            self.iterations += 1;
            if self.iterations > 2_000_000 {
                return Err("schedule search exceeded the iteration limit".to_string());
            }

            // Loop attempts run between base occurrences. A base occurrence
            // replaces the active loop, so peek at the next base first.
            if self.loop_state.is_some() {
                let Some(mut loop_state) = self.loop_state.take() else {
                    unreachable!()
                };
                let attempt = loop_state.attempt;
                let next = loop_state.next;
                let can_loop =
                    self.schedule.until.is_none_or(|until| {
                        self.until_instant(until).is_none_or(|until| next < until)
                    }) && loop_state.repeat.is_none_or(|repeat| attempt < repeat);
                if can_loop {
                    if self.next_base.is_none() {
                        self.next_base = self.compute_next_base()?;
                    }
                    if self.next_base.is_some_and(|base| base <= next) {
                        // The next base occurrence replaces this loop.
                    } else {
                        let interval = loop_state.intervals
                            [(attempt as usize).min(loop_state.intervals.len().saturating_sub(1))]
                        .max(Duration::seconds(1));
                        loop_state.attempt += 1;
                        loop_state.next = next + interval;
                        self.loop_state = Some(loop_state);
                        if (next > self.after || (self.inclusive && next == self.after))
                            && let Some(occurrence) = self.filter_occurrence(
                                next,
                                OccurrenceKind::Loop,
                                None,
                                Some(attempt),
                            )?
                        {
                            return Ok(Some(occurrence));
                        }
                        continue;
                    }
                }
            }

            let base = match self.next_base.take() {
                Some(base) => base,
                None => {
                    let Some(base) = self.compute_next_base()? else {
                        return Ok(None);
                    };
                    base
                }
            };

            // The base occurrence replaces any older loop.
            let loop_rule = self.schedule.loop_rule.clone();
            if let Some(loop_rule) = loop_rule
                && !loop_rule.intervals().is_empty()
            {
                self.start_loop(base, &loop_rule);
            }

            self.base_cursor = base + TimeDelta::nanoseconds(1);
            let Some(occurrence) =
                self.filter_occurrence(base, OccurrenceKind::Scheduled, None, Some(0))?
            else {
                continue;
            };
            return Ok(Some(occurrence));
        }
    }

    fn next_one_shot(&mut self) -> Result<Option<ScheduleOccurrence>, String> {
        let Some(from) = self.schedule.from else {
            return Ok(None);
        };
        let Some(at) = self.date_instant(from)? else {
            return Ok(None);
        };
        if at < self.after || (!self.inclusive && at == self.after) {
            return Ok(None);
        }
        if self
            .schedule
            .until
            .is_some_and(|until| self.until_instant(until).is_some_and(|until| at >= until))
        {
            return Ok(None);
        }
        Ok(Some(self.occurrence(
            at,
            OccurrenceKind::Scheduled,
            false,
            None,
        )))
    }

    fn start_loop(&mut self, base: DateTime<Utc>, loop_rule: &LoopRule) {
        let intervals: Vec<Duration> = loop_rule
            .intervals()
            .into_iter()
            .map(|duration| Duration::from_std(duration).unwrap_or(Duration::seconds(1)))
            .collect();
        let first = intervals.first().copied().unwrap_or(Duration::seconds(1));
        self.loop_state = Some(LoopState {
            attempt: 0,
            next: base + first,
            intervals,
            repeat: loop_rule.repeat,
        });
    }

    fn compute_next_base(&mut self) -> Result<Option<DateTime<Utc>>, String> {
        let rule = self
            .schedule
            .on
            .clone()
            .ok_or_else(|| "schedule has no rule".to_string())?;
        let from = match self.schedule.from {
            Some(from) => self.date_instant(from)?,
            None => None,
        };
        let until = self
            .schedule
            .until
            .and_then(|until| self.until_instant(until));

        if let Some(every) = rule.every
            && let Some(duration) = every.duration()
            && every.is_sub_day()
        {
            return self.compute_interval(&rule, from, until, duration);
        }

        let anchor_date = match self.schedule.from {
            Some(ScheduleDate::Date(date)) => date,
            Some(ScheduleDate::DateTime(value)) => value.with_timezone(&self.tz).date_naive(),
            None => self.base_cursor.with_timezone(&self.tz).date_naive() - Duration::days(1),
        };
        let start_date = self.date_cursor.max(anchor_date);
        let mut date = start_date;
        let anchor = anchor_date;

        for _ in 0..(SEARCH_YEARS * 366 + 2) {
            if let Some(until) = until
                && self
                    .tz
                    .from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
                    .with_timezone(&Utc)
                    >= until
            {
                return Ok(None);
            }
            if self.date_matches(&rule, date)? {
                let candidates = self.candidates_for_date(&rule, date, from, until)?;
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
            if date > anchor + Duration::days(SEARCH_YEARS as i64 * 366) {
                break;
            }
        }
        Ok(None)
    }

    fn compute_interval(
        &mut self,
        rule: &ScheduleRule,
        from: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        every: std::time::Duration,
    ) -> Result<Option<DateTime<Utc>>, String> {
        let _ = rule;
        let Some(from) = from else {
            return Err("on.every for seconds, minutes, or hours requires from".to_string());
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
        let _ = rule;
        Ok(None)
    }

    fn candidates_for_date(
        &self,
        rule: &ScheduleRule,
        date: NaiveDate,
        from: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<Vec<DateTime<Utc>>, String> {
        let mut candidates = Vec::new();
        let at_entries: Vec<&AtSpec> = rule.at.as_slice().iter().collect();
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

    fn date_matches(&self, rule: &ScheduleRule, date: NaiveDate) -> Result<bool, String> {
        if !rule.months.is_empty()
            && !rule
                .months
                .iter()
                .any(|month| month.number() == date.month())
        {
            return Ok(false);
        }
        if !rule.years.is_empty() && !rule.years.contains(&date.year()) {
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
        if !rule.days.is_empty() {
            let matches = |day: &DaySpec| day_matches(day, date);
            let matched = if rule.day_match == DayMatch::All {
                rule.days.iter().all(matches)
            } else {
                rule.days.iter().any(matches)
            };
            if !matched {
                return Ok(false);
            }
        }

        if let Some(every) = rule.every
            && !every.is_sub_day()
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
            return self.skipped_or_none(base, kind, "before from");
        }
        if let Some(until) = self.schedule.until
            && let Some(until) = self.until_instant(until)
            && base >= until
        {
            return Ok(None);
        }

        let local = base.with_timezone(&self.tz);
        let local_time = local.time();
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
            return self.skipped_or_none(base, kind, &reason);
        }

        let jitter = match kind {
            OccurrenceKind::Scheduled => self.schedule.jitter,
            OccurrenceKind::Loop => self
                .schedule
                .loop_rule
                .as_ref()
                .and_then(|rule| rule.jitter),
        };
        let at = if let Some(jitter) = jitter {
            let offset = jitter_offset(
                &self.schedule.id,
                base.timestamp_nanos_opt().unwrap_or(0),
                attempt.unwrap_or(0),
                jitter.as_std(),
            );
            clamp_jitter(base + offset, &self.schedule, self.tz)?
        } else {
            base
        };
        if at < self.after || (!self.inclusive && at == self.after) {
            return Ok(None);
        }
        Ok(Some(self.occurrence(at, kind, false, reason)))
    }

    fn skipped_or_none(
        &self,
        at: DateTime<Utc>,
        kind: OccurrenceKind,
        reason: &str,
    ) -> Result<Option<ScheduleOccurrence>, String> {
        if self.include_skipped {
            Ok(Some(self.occurrence(
                at,
                kind,
                true,
                Some(reason.to_string()),
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
    ) -> ScheduleOccurrence {
        ScheduleOccurrence {
            at,
            local: at
                .with_timezone(&self.tz)
                .format("%Y-%m-%d %H:%M:%S %Z")
                .to_string(),
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
        DaySpec::LastWeekday => date.day() + 7 > days_in_month(date.year(), date.month()),
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

fn nearest_weekday(year: i32, month: u32, day: u8) -> Option<NaiveDate> {
    let day = day.min(days_in_month(year, month) as u8);
    let date = NaiveDate::from_ymd_opt(year, month, day as u32)?;
    match date.weekday() {
        Weekday::Sat => date.pred_opt(),
        Weekday::Sun => date.succ_opt(),
        _ => Some(date),
    }
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

fn jitter_offset(
    schedule_id: &str,
    base_nanos: i64,
    attempt: u32,
    jitter: std::time::Duration,
) -> TimeDelta {
    let millis = jitter.as_millis() as i64;
    if millis <= 0 {
        return TimeDelta::zero();
    }
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in schedule_id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    for byte in base_nanos.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    for byte in attempt.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let span = (millis * 2 + 1) as u64;
    let offset = (hash % span) as i64 - millis;
    TimeDelta::milliseconds(offset)
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
    use hodeco_contract::domain::{
        AtSpec, DateRange, OneOrMany, ScheduleEvery, ScheduleRule, TimeWindow,
    };
    use hodeco_contract::utils::HumanDuration;

    fn schedule(at: &str) -> ScheduleDefinition {
        ScheduleDefinition {
            id: "test".to_string(),
            name: "Test".to_string(),
            timezone: Some("UTC".to_string()),
            on: Some(ScheduleRule {
                at: OneOrMany::One(AtSpec::Text(at.to_string())),
                ..ScheduleRule::default()
            }),
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
        schedule.on.as_mut().unwrap().at = OneOrMany::Many(vec![
            AtSpec::Text("08:00".to_string()),
            AtSpec::Text("11:00".to_string()),
        ]);
        schedule.deny = OneOrMany::One(TimeWindow::parse("07:00~09:00").unwrap());
        let next = next_occurrence(&schedule, context(), at("2026-11-17T07:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-17T11:00:00Z"));

        schedule.deny = OneOrMany::Many(Vec::new());
        schedule.on.as_mut().unwrap().at = OneOrMany::One(AtSpec::Text("09:00".to_string()));
        schedule.except = OneOrMany::One(DateRange::parse("2026-11-17").unwrap());
        let next = next_occurrence(&schedule, context(), at("2026-11-17T08:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-18T09:00:00Z"));
    }

    #[test]
    fn computes_calendar_intervals() {
        let mut schedule = schedule("09:00");
        schedule.from = Some(ScheduleDate::parse("2026-11-17").unwrap());
        schedule.on.as_mut().unwrap().every = Some(ScheduleEvery::Days(3));
        let next = next_occurrence(&schedule, context(), at("2026-11-18T00:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-20T09:00:00Z"));
    }

    #[test]
    fn computes_loops_after_a_base_occurrence() {
        let mut schedule = schedule("09:00");
        schedule.loop_rule = Some(LoopRule {
            every: OneOrMany::One(HumanDuration::from_std(std::time::Duration::from_secs(
                30 * 60,
            ))),
            repeat: Some(2),
            jitter: None,
        });
        let next = next_occurrence(&schedule, context(), at("2026-11-17T08:00:00Z"), false)
            .unwrap()
            .unwrap();
        assert_eq!(next.at, at("2026-11-17T09:00:00Z"));
        let next = next_occurrence(&schedule, context(), next.at, false)
            .unwrap()
            .unwrap();
        assert_eq!(next.kind, OccurrenceKind::Loop);
        assert_eq!(next.at, at("2026-11-17T09:30:00Z"));
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
    fn jitter_is_deterministic_and_bounded() {
        let first = jitter_offset("test", 12345, 0, std::time::Duration::from_secs(10));
        let second = jitter_offset("test", 12345, 0, std::time::Duration::from_secs(10));
        assert_eq!(first, second);
        assert!(first.abs() <= TimeDelta::seconds(10));
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
