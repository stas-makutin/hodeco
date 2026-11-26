//! Schedules list and full-page schedule editor.

use super::super::App;
use crate::ui::widgets;
use chrono::{Datelike, Local, NaiveDate, NaiveTime, TimeZone, Timelike, Weekday};
use chrono_tz::{TZ_VARIANTS, Tz};
use egui::{LayerId, Order, RichText, Ui};
use egui_extras::{Column, DatePickerButton, TableBuilder};
use hodeco_contract::api::payloads::SchedulesSolarPreviewResult;
use hodeco_contract::domain::{
    AtSpec, AtStructured, DateRange, DayMatch, DaySpec, Location, Month, OccurrenceKind, OneOrMany,
    ParsedAt, ScheduleDate, ScheduleDefinition, ScheduleEvery, ScheduleInfo, ScheduleOccurrence,
    SolarEvent, TimeWindow,
};
use hodeco_contract::utils::{HumanDuration, id_from_name};
use jiff::civil::Date as CivilDate;
use std::str::FromStr;
use std::sync::OnceLock;

#[derive(Debug, Default)]
pub(crate) struct SchedulesUi {
    pub schedules: Vec<ScheduleInfo>,
    pub loading: bool,
    pub error: Option<String>,
    pub search: String,
    pub selected: Option<usize>,
    pub confirm_delete: Option<String>,
    pub editor: Option<ScheduleEditor>,
    pub defaults_location: Option<Location>,
    pub defaults_timezone: Option<String>,
    pub system_timezone: Option<String>,
    pub defaults_editor: Option<ScheduleDefaultsEditor>,
    pub location_dialog: Option<LocationDialog>,
}

#[derive(Debug)]
pub(crate) struct ScheduleDefaultsEditor {
    pub location: String,
    pub timezone: String,
    pub timezone_custom: bool,
    pub location_custom: bool,
    pub initial_location: Option<Location>,
    pub initial_timezone: Option<String>,
    pub saving: bool,
    pub error: Option<String>,
}

impl ScheduleDefaultsEditor {
    pub fn new(location: Option<Location>, timezone: Option<String>) -> Self {
        let location_text = location.map(format_location).unwrap_or_default();
        let timezone_text = timezone.clone().unwrap_or_default();
        Self {
            timezone_custom: !timezone_text.trim().is_empty(),
            location_custom: !location_text.trim().is_empty(),
            location: location_text,
            timezone: timezone_text,
            initial_location: location,
            initial_timezone: timezone,
            saving: false,
            error: None,
        }
    }

    fn selections(&self) -> Result<(Option<Location>, Option<String>), String> {
        let timezone = if self.timezone_custom {
            let timezone = nonempty(&self.timezone)
                .ok_or_else(|| "Choose a timezone or select Default.".to_string())?;
            if timezone.parse::<Tz>().is_err() {
                return Err(format!("Unknown timezone '{timezone}'."));
            }
            Some(timezone)
        } else {
            None
        };
        let location = if self.location_custom {
            match parse_location(&self.location)? {
                Some(location) => Some(location),
                None => return Err("Set a location or select Default.".to_string()),
            }
        } else {
            None
        };
        Ok((location, timezone))
    }

    fn is_dirty(&self) -> bool {
        match self.selections() {
            Ok((location, timezone)) => {
                location != self.initial_location || timezone != self.initial_timezone
            }
            Err(_) => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocationTarget {
    Defaults,
    Schedule,
}

#[derive(Debug)]
pub(crate) struct LocationDialog {
    pub target: LocationTarget,
    pub latitude: f64,
    pub longitude: f64,
    pub elevation: Option<f64>,
    pub date: CivilDate,
    pub date_picker_generation: u64,
    pub timezone: String,
    pub preview: Option<SchedulesSolarPreviewResult>,
    pub previewing: bool,
    pub preview_generation: u64,
    pub preview_error: Option<String>,
    pub error: Option<String>,
    pub preview_due: Option<f64>,
}

impl LocationDialog {
    pub fn new(target: LocationTarget, location: Option<Location>, timezone: String) -> Self {
        let (latitude, longitude, elevation) = location
            .map(|location| (location.latitude, location.longitude, location.elevation))
            .unwrap_or((0.0, 0.0, None));
        Self {
            target,
            latitude,
            longitude,
            elevation,
            date: today_date(),
            date_picker_generation: 0,
            timezone,
            preview: None,
            previewing: false,
            preview_generation: 0,
            preview_error: None,
            error: None,
            preview_due: Some(0.0),
        }
    }

    fn active_location(&self) -> Option<Location> {
        let location = Location::new(self.latitude, self.longitude, self.elevation);
        location.is_valid().then_some(location)
    }
}

fn split_date_value(
    value: Option<ScheduleDate>,
    timezone: Option<Tz>,
) -> (String, bool, u32, u32, u32) {
    let Some(value) = value else {
        return (String::new(), false, 0, 0, 0);
    };
    match value {
        ScheduleDate::Date(date) => (date.to_string(), false, 0, 0, 0),
        ScheduleDate::DateTime(value) => {
            let (date, time) = match timezone {
                Some(timezone) => {
                    let local = value.with_timezone(&timezone);
                    (local.date_naive(), local.time())
                }
                None => (value.date_naive(), value.time()),
            };
            (
                date.to_string(),
                true,
                time.hour(),
                time.minute(),
                time.second(),
            )
        }
    }
}

fn today_date() -> CivilDate {
    let today = Local::now().date_naive();
    CivilDate::new(today.year() as i16, today.month() as i8, today.day() as i8)
        .unwrap_or_else(|_| CivilDate::new(2026, 1, 1).expect("valid fallback date"))
}

fn civil_to_naive(date: CivilDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(
        i32::from(date.year()),
        date.month() as u32,
        date.day() as u32,
    )
    .unwrap_or_else(|| Local::now().date_naive())
}

fn date_picker_button(
    ui: &mut Ui,
    id_salt: &str,
    generation: &mut u64,
    date: &mut CivilDate,
    hover: &str,
    label: &str,
) -> bool {
    let salt = format!("{id_salt}-{generation}");
    let area_id = ui.make_persistent_id(Some(salt.as_str()));
    let was_open = ui.ctx().memory(|memory| {
        memory
            .areas()
            .is_visible(&LayerId::new(Order::Foreground, area_id))
    });
    let response = ui
        .add(
            DatePickerButton::new(date)
                .id_salt(&salt)
                .show_icon(false)
                .format(label),
        )
        .on_hover_text(hover);
    if response.clicked() && was_open {
        *generation = generation.wrapping_add(1);
    }
    response.changed()
}

fn date_field(
    ui: &mut Ui,
    id_salt: &str,
    date: &mut String,
    valid: &mut String,
    generation: &mut u64,
) {
    let response = ui.add(
        egui::TextEdit::singleline(date)
            .hint_text("YYYY-MM-DD")
            .desired_width(110.0),
    );
    if response.changed() {
        remember_valid_date(date, valid);
    }
    let commit = response.lost_focus()
        || (response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)));
    if commit {
        commit_date_input(date, valid);
    }
    let mut picked = picker_date(date);
    if date_picker_button(
        ui,
        id_salt,
        generation,
        &mut picked,
        "Choose date",
        "Pick date",
    ) {
        *date = civil_to_naive(picked).to_string();
        *valid = date.clone();
    }
}

fn time_editor(ui: &mut Ui, hour: &mut u32, minute: &mut u32, second: &mut u32) {
    let width = 38.0;
    ui.add_sized(
        [width, 22.0],
        egui::DragValue::new(hour).range(0..=23).speed(1.0),
    );
    ui.label(":");
    ui.add_sized(
        [width, 22.0],
        egui::DragValue::new(minute).range(0..=59).speed(1.0),
    );
    ui.label(":");
    ui.add_sized(
        [width, 22.0],
        egui::DragValue::new(second).range(0..=59).speed(1.0),
    );
}

fn picker_date(value: &str) -> CivilDate {
    let date = value.trim().split(['T', ' ']).next().unwrap_or_default();
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .and_then(|date| {
            CivilDate::new(date.year() as i16, date.month() as i8, date.day() as i8).ok()
        })
        .unwrap_or_else(today_date)
}

impl SchedulesUi {
    pub fn filtered(&self) -> Vec<&ScheduleInfo> {
        let needle = self.search.trim().to_ascii_lowercase();
        self.schedules
            .iter()
            .filter(|info| {
                needle.is_empty()
                    || info.schedule.name.to_ascii_lowercase().contains(&needle)
                    || info.schedule.id.contains(&needle)
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimeWindowRow {
    pub from_hour: u32,
    pub from_minute: u32,
    pub from_second: u32,
    pub to_hour: u32,
    pub to_minute: u32,
    pub to_second: u32,
}

impl TimeWindowRow {
    fn from_window(window: TimeWindow) -> Self {
        Self {
            from_hour: window.from.hour(),
            from_minute: window.from.minute(),
            from_second: window.from.second(),
            to_hour: window.to.hour(),
            to_minute: window.to.minute(),
            to_second: window.to.second(),
        }
    }

    fn new(from: &str, to: &str) -> Self {
        let default = |value: &str| NaiveTime::parse_from_str(value, "%H:%M").unwrap_or_default();
        Self {
            from_hour: default(from).hour(),
            from_minute: default(from).minute(),
            from_second: 0,
            to_hour: default(to).hour(),
            to_minute: default(to).minute(),
            to_second: 0,
        }
    }

    fn window(self) -> TimeWindow {
        let at = |hour: u32, minute: u32, second: u32| {
            NaiveTime::from_hms_opt(hour, minute, second).unwrap_or_default()
        };
        TimeWindow {
            from: at(self.from_hour, self.from_minute, self.from_second),
            to: at(self.to_hour, self.to_minute, self.to_second),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DateRangeRow {
    pub from_date: String,
    pub from_date_valid: String,
    pub from_picker_generation: u64,
    pub to_date: String,
    pub to_date_valid: String,
    pub to_picker_generation: u64,
}

impl DateRangeRow {
    fn from_range(range: DateRange) -> Self {
        let from = range.from.to_string();
        let to = range.to.to_string();
        Self {
            from_date_valid: from.clone(),
            from_date: from,
            from_picker_generation: 0,
            to_date_valid: to.clone(),
            to_date: to,
            to_picker_generation: 0,
        }
    }

    fn new() -> Self {
        let today = today_date().to_string();
        Self {
            from_date_valid: today.clone(),
            from_date: today.clone(),
            from_picker_generation: 0,
            to_date_valid: today.clone(),
            to_date: today,
            to_picker_generation: 0,
        }
    }

    fn range(&self) -> Result<DateRange, String> {
        let from = parse_date_only(&self.from_date, "Exception start")?;
        let to = parse_date_only(&self.to_date, "Exception end")?;
        if to < from {
            return Err("Exception end must be on or after the start date.".to_string());
        }
        Ok(DateRange { from, to })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpecialDayKind {
    LastDay,
    LastWeekday,
    NearestWeekday,
    NthWeekday,
}

impl SpecialDayKind {
    const ALL: [Self; 4] = [
        Self::LastDay,
        Self::LastWeekday,
        Self::NearestWeekday,
        Self::NthWeekday,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::LastDay => "Last day of month",
            Self::LastWeekday => "Last weekday",
            Self::NearestWeekday => "Nearest weekday",
            Self::NthWeekday => "Nth / last weekday",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AtRowKind {
    Time,
    HourlyMinute,
    Solar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OffsetUnit {
    Seconds,
    Minutes,
    Hours,
}

impl OffsetUnit {
    const ALL: [Self; 3] = [Self::Seconds, Self::Minutes, Self::Hours];

    const fn suffix(self) -> &'static str {
        match self {
            Self::Seconds => "s",
            Self::Minutes => "m",
            Self::Hours => "h",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AtRow {
    pub kind: AtRowKind,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub solar: SolarEvent,
    pub offset_amount: i64,
    pub offset_unit: OffsetUnit,
}

impl AtRow {
    fn time(value: &str) -> Self {
        let time = parse_time_of_day(value);
        Self {
            kind: AtRowKind::Time,
            hour: time.hour(),
            minute: time.minute(),
            second: time.second(),
            solar: SolarEvent::Sunrise,
            offset_amount: 0,
            offset_unit: OffsetUnit::Minutes,
        }
    }

    fn from_spec(spec: &AtSpec) -> Self {
        match spec.parse() {
            Ok(ParsedAt::Time(time)) => Self {
                kind: AtRowKind::Time,
                hour: time.hour(),
                minute: time.minute(),
                second: time.second(),
                solar: SolarEvent::Sunrise,
                offset_amount: 0,
                offset_unit: OffsetUnit::Minutes,
            },
            Ok(ParsedAt::HourlyMinute(minute)) => Self {
                kind: AtRowKind::HourlyMinute,
                hour: 0,
                minute: u32::from(minute),
                second: 0,
                solar: SolarEvent::Sunrise,
                offset_amount: 0,
                offset_unit: OffsetUnit::Minutes,
            },
            Ok(ParsedAt::Solar {
                event,
                offset_seconds,
            }) => {
                let (offset_amount, offset_unit) = offset_parts(offset_seconds);
                Self {
                    kind: AtRowKind::Solar,
                    hour: 0,
                    minute: 0,
                    second: 0,
                    solar: event,
                    offset_amount,
                    offset_unit,
                }
            }
            Err(_) => Self::time(&spec.display()),
        }
    }

    fn to_spec(&self) -> Result<AtSpec, String> {
        match self.kind {
            AtRowKind::Time => {
                let text = format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second);
                let spec = AtSpec::Text(text.clone());
                match spec.parse() {
                    Ok(ParsedAt::Time(time)) => Ok(AtSpec::Text(time_text(time))),
                    _ => Err(format!("Invalid time '{text}'; use HH:MM or HH:MM:SS.")),
                }
            }
            AtRowKind::HourlyMinute => Ok(AtSpec::Text(format!(":{}", self.minute))),
            AtRowKind::Solar => {
                let offset = (self.offset_amount != 0)
                    .then(|| format!("{}{}", self.offset_amount, self.offset_unit.suffix()));
                let spec = AtSpec::Structured(AtStructured {
                    time: None,
                    solar: Some(self.solar),
                    offset,
                });
                spec.parse()
                    .map_err(|error| format!("Solar at entry: {error}"))?;
                Ok(spec)
            }
        }
    }
}

fn parse_time_of_day(value: &str) -> NaiveTime {
    NaiveTime::parse_from_str(value.trim(), "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(value.trim(), "%H:%M"))
        .unwrap_or_else(|_| NaiveTime::from_hms_opt(12, 0, 0).expect("valid default time"))
}

fn offset_parts(seconds: i64) -> (i64, OffsetUnit) {
    if seconds != 0 && seconds % 3600 == 0 {
        (seconds / 3600, OffsetUnit::Hours)
    } else if seconds != 0 && seconds % 60 == 0 {
        (seconds / 60, OffsetUnit::Minutes)
    } else {
        (seconds, OffsetUnit::Seconds)
    }
}

fn format_offset_seconds(seconds: i64) -> String {
    if seconds == 0 {
        return String::new();
    }
    let sign = if seconds < 0 { "-" } else { "+" };
    let absolute = seconds.unsigned_abs();
    if absolute.is_multiple_of(3600) {
        format!("{sign}{}h", absolute / 3600)
    } else if absolute.is_multiple_of(60) {
        format!("{sign}{}m", absolute / 60)
    } else {
        format!("{sign}{absolute}s")
    }
}

#[derive(Debug)]
pub(crate) struct ScheduleEditor {
    pub draft: ScheduleDefinition,
    pub original: Option<ScheduleDefinition>,
    pub initial: ScheduleDefinition,
    pub is_new: bool,
    pub id_auto: bool,
    pub timezone: String,
    pub timezone_custom: bool,
    pub fallback_timezone: Option<String>,
    pub location: String,
    pub location_custom: bool,
    pub from_date: String,
    pub from_date_valid: String,
    pub from_picker_generation: u64,
    pub from_time_enabled: bool,
    pub from_hour: u32,
    pub from_minute: u32,
    pub from_second: u32,
    pub until_date: String,
    pub until_date_valid: String,
    pub until_picker_generation: u64,
    pub until_time_enabled: bool,
    pub until_hour: u32,
    pub until_minute: u32,
    pub until_second: u32,
    pub jitter: String,
    pub jitter_enabled: bool,
    pub at_rows: Vec<AtRow>,
    pub every: String,
    pub days: String,
    pub day_match: DayMatch,
    pub months: Vec<Month>,
    pub years: Vec<i32>,
    pub special_kind: SpecialDayKind,
    pub special_day: u8,
    pub special_nth: i8,
    pub special_weekday: Weekday,
    pub repeat_at: Vec<String>,
    pub allow: Vec<TimeWindowRow>,
    pub deny: Vec<TimeWindowRow>,
    pub except: Vec<DateRangeRow>,
    pub error: Option<String>,
    pub saving: bool,
    pub confirm_delete: bool,
    pub preview: Vec<ScheduleOccurrence>,
    pub preview_error: Option<String>,
    pub previewing: bool,
    pub preview_generation: u64,
}

impl ScheduleEditor {
    pub fn new(
        schedule: ScheduleDefinition,
        is_new: bool,
        fallback_timezone: Option<String>,
    ) -> Self {
        let timezone = schedule.timezone.clone().unwrap_or_default();
        let location = schedule.location.map(format_location).unwrap_or_default();
        let effective_timezone = if timezone.trim().is_empty() {
            fallback_timezone.as_deref()
        } else {
            Some(timezone.as_str())
        }
        .and_then(|timezone| timezone.parse::<Tz>().ok());
        let (from_date, from_time_enabled, from_hour, from_minute, from_second) =
            split_date_value(schedule.from, effective_timezone);
        let (until_date, until_time_enabled, until_hour, until_minute, until_second) =
            split_date_value(schedule.until, effective_timezone);
        let jitter_enabled = schedule.jitter.is_some();
        let jitter = schedule
            .jitter
            .map(|jitter| jitter.to_string())
            .unwrap_or_else(|| "30s".to_string());
        let at_rows = schedule.at.iter().map(AtRow::from_spec).collect();
        let every = schedule.every.map(every_text).unwrap_or_default();
        let days = schedule
            .days
            .iter()
            .map(DaySpec::display)
            .collect::<Vec<_>>()
            .join(", ");
        let day_match = schedule.day_match;
        let months = schedule.months.clone();
        let years = schedule.years.clone();
        let repeat_at = schedule.repeat_at.iter().map(ToString::to_string).collect();
        let allow = schedule
            .allow
            .iter()
            .cloned()
            .map(TimeWindowRow::from_window)
            .collect();
        let deny = schedule
            .deny
            .iter()
            .cloned()
            .map(TimeWindowRow::from_window)
            .collect();
        let except = schedule
            .except
            .iter()
            .cloned()
            .map(DateRangeRow::from_range)
            .collect();
        Self {
            draft: schedule.clone(),
            original: (!is_new).then_some(schedule.clone()),
            initial: schedule,
            is_new,
            id_auto: is_new,
            timezone_custom: !timezone.trim().is_empty(),
            fallback_timezone,
            location_custom: !location.trim().is_empty(),
            timezone,
            location,
            from_date_valid: from_date.clone(),
            from_date,
            from_picker_generation: 0,
            from_time_enabled,
            from_hour,
            from_minute,
            from_second,
            until_date_valid: until_date.clone(),
            until_date,
            until_picker_generation: 0,
            until_time_enabled,
            until_hour,
            until_minute,
            until_second,
            jitter,
            jitter_enabled,
            at_rows,
            every,
            days,
            day_match,
            months,
            years,
            special_kind: SpecialDayKind::LastDay,
            special_day: 1,
            special_nth: 1,
            special_weekday: Weekday::Mon,
            repeat_at,
            allow,
            deny,
            except,
            error: None,
            saving: false,
            confirm_delete: false,
            preview: Vec::new(),
            preview_error: None,
            previewing: false,
            preview_generation: 0,
        }
    }

    pub fn build(&self) -> Result<ScheduleDefinition, String> {
        let mut schedule = self.draft.clone();
        schedule.name = schedule.name.trim().to_string();
        if schedule.name.is_empty() {
            return Err("Name must not be empty.".to_string());
        }
        schedule.timezone = if self.timezone_custom {
            let timezone = self.timezone.trim();
            if timezone.is_empty() {
                return Err("Choose a timezone or select Default.".to_string());
            }
            if timezone.parse::<Tz>().is_err() {
                return Err(format!("Unknown timezone '{timezone}'."));
            }
            Some(timezone.to_string())
        } else {
            None
        };
        schedule.location = if self.location_custom {
            match parse_location(&self.location)? {
                Some(location) => Some(location),
                None => return Err("Set a location or select Default.".to_string()),
            }
        } else {
            None
        };
        let timezone = self.effective_timezone();
        schedule.from = build_date_value(
            &self.from_date,
            self.from_time_enabled,
            self.from_hour,
            self.from_minute,
            self.from_second,
            timezone,
        )?;
        schedule.until = build_date_value(
            &self.until_date,
            self.until_time_enabled,
            self.until_hour,
            self.until_minute,
            self.until_second,
            timezone,
        )?;
        schedule.jitter = if self.jitter_enabled {
            match parse_duration(&self.jitter)? {
                Some(jitter) => Some(jitter),
                None => return Err("Jitter must be set when jitter is enabled.".to_string()),
            }
        } else {
            None
        };
        let at: Vec<AtSpec> = self
            .at_rows
            .iter()
            .map(AtRow::to_spec)
            .collect::<Result<_, _>>()?;
        let every = if self.every.trim().is_empty() {
            None
        } else {
            Some(ScheduleEvery::parse(&self.every)?)
        };
        if at.is_empty() && every.is_none() && self.from_date.trim().is_empty() {
            return Err("Schedule needs From, At, or Every.".to_string());
        }
        if !at.is_empty() && every.is_some_and(ScheduleEvery::uses_elapsed_time) {
            return Err(
                "Elapsed-time every (s/m/h) cannot be combined with At entries.".to_string(),
            );
        }
        if every.is_some_and(ScheduleEvery::uses_elapsed_time)
            && (!self.days.trim().is_empty() || !self.months.is_empty() || !self.years.is_empty())
        {
            return Err(
                "Date filters cannot be combined with an elapsed-time every interval.".to_string(),
            );
        }
        if every.is_some() && self.from_date.trim().is_empty() {
            return Err("Every requires a From date.".to_string());
        }
        let mut months = self.months.clone();
        months.sort_by_key(|month| month.number());
        schedule.at = one_or_many(at);
        schedule.every = every;
        schedule.days = parse_days(&self.days)?;
        schedule.day_match = self.day_match;
        schedule.months = months;
        schedule.years = {
            let mut years = self.years.clone();
            years.sort_unstable();
            years.dedup();
            years
        };
        schedule.repeat_at = one_or_many(parse_repeat_offsets(&self.repeat_at)?);
        schedule.allow = one_or_many(
            self.allow
                .iter()
                .map(|window| window.window())
                .collect::<Vec<_>>(),
        );
        schedule.deny = one_or_many(
            self.deny
                .iter()
                .map(|window| window.window())
                .collect::<Vec<_>>(),
        );
        schedule.except = one_or_many(
            self.except
                .iter()
                .map(DateRangeRow::range)
                .collect::<Result<Vec<_>, _>>()?,
        );
        Ok(schedule)
    }

    pub fn is_dirty(&self) -> bool {
        let original = self.original.as_ref().unwrap_or(&self.initial);
        match self.build() {
            Ok(schedule) => normalized(&schedule) != normalized(original),
            Err(_) => true,
        }
    }

    fn effective_timezone(&self) -> Option<Tz> {
        let timezone = if self.timezone_custom {
            self.timezone.trim()
        } else {
            self.fallback_timezone.as_deref().unwrap_or("")
        };
        timezone.parse().ok()
    }
}

fn build_date_value(
    date: &str,
    time_enabled: bool,
    hour: u32,
    minute: u32,
    second: u32,
    timezone: Option<Tz>,
) -> Result<Option<ScheduleDate>, String> {
    let date = date.trim();
    if date.is_empty() {
        if time_enabled {
            return Err("Choose a date before enabling Time.".to_string());
        }
        return Ok(None);
    }
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| "Date must use YYYY-MM-DD.".to_string())?;
    if !time_enabled {
        return Ok(Some(ScheduleDate::Date(date)));
    }
    let time = NaiveTime::from_hms_opt(hour, minute, second)
        .ok_or_else(|| "Time must be a valid 24-hour time.".to_string())?;
    let timezone = timezone.ok_or_else(|| "Set a timezone before enabling Time.".to_string())?;
    let local = date.and_time(time);
    let value = match timezone.from_local_datetime(&local) {
        chrono::LocalResult::Single(value) => value,
        chrono::LocalResult::Ambiguous(_, _) => {
            return Err("Time is ambiguous in the schedule timezone.".to_string());
        }
        chrono::LocalResult::None => {
            return Err("Time does not exist in the schedule timezone.".to_string());
        }
    };
    Ok(Some(ScheduleDate::DateTime(value.fixed_offset())))
}

fn one_or_many<T>(mut values: Vec<T>) -> OneOrMany<T> {
    if values.len() == 1 {
        OneOrMany::One(values.pop().expect("one value"))
    } else {
        OneOrMany::Many(values)
    }
}

fn time_text(time: chrono::NaiveTime) -> String {
    let text = time.format("%H:%M:%S").to_string();
    text.strip_suffix(":00").unwrap_or(&text).to_string()
}

fn normalize_at(spec: &AtSpec) -> AtSpec {
    match spec.parse() {
        Ok(ParsedAt::Time(time)) => AtSpec::Text(time_text(time)),
        Ok(ParsedAt::HourlyMinute(minute)) => AtSpec::Text(format!(":{minute}")),
        Ok(ParsedAt::Solar {
            event,
            offset_seconds,
        }) => AtSpec::Structured(AtStructured {
            time: None,
            solar: Some(event),
            offset: nonempty(&format_offset_seconds(offset_seconds)),
        }),
        Err(_) => spec.clone(),
    }
}

fn normalized(schedule: &ScheduleDefinition) -> ScheduleDefinition {
    let mut schedule = schedule.clone();
    schedule.at = one_or_many(schedule.at.iter().map(normalize_at).collect());
    schedule.months.sort_by_key(|month| month.number());
    schedule.years.sort_unstable();
    schedule.years.dedup();
    let mut repeat_at: Vec<_> = schedule.repeat_at.iter().copied().collect();
    repeat_at.sort_unstable();
    repeat_at.dedup();
    schedule.repeat_at = one_or_many(repeat_at);
    schedule.allow = one_or_many(schedule.allow.iter().cloned().collect());
    schedule.deny = one_or_many(schedule.deny.iter().cloned().collect());
    schedule.except = one_or_many(schedule.except.iter().cloned().collect());
    schedule
}

fn weekday_short(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "mon",
        Weekday::Tue => "tue",
        Weekday::Wed => "wed",
        Weekday::Thu => "thu",
        Weekday::Fri => "fri",
        Weekday::Sat => "sat",
        Weekday::Sun => "sun",
    }
}

fn day_parts(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn day_is_weekday(part: &str, weekday: Weekday) -> bool {
    DaySpec::parse(part).is_ok_and(|spec| spec == DaySpec::Weekday(weekday))
}

fn day_is_month_day(part: &str, day: u8) -> bool {
    DaySpec::parse(part).is_ok_and(|spec| spec == DaySpec::MonthDay(day))
}

fn special_day_spec(kind: SpecialDayKind, day: u8, nth: i8, weekday: Weekday) -> DaySpec {
    match kind {
        SpecialDayKind::LastDay => DaySpec::LastDay,
        SpecialDayKind::LastWeekday => DaySpec::LastWeekday,
        SpecialDayKind::NearestWeekday => DaySpec::NearestWeekday(day),
        SpecialDayKind::NthWeekday => DaySpec::NthWeekday { nth, weekday },
    }
}

fn weekday_name(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

fn ordinal(nth: i8) -> String {
    match nth {
        1 => "1st".to_string(),
        2 => "2nd".to_string(),
        3 => "3rd".to_string(),
        4 => "4th".to_string(),
        5 => "5th".to_string(),
        -1 => "Last".to_string(),
        -2 => "2nd-last".to_string(),
        -3 => "3rd-last".to_string(),
        -4 => "4th-last".to_string(),
        -5 => "5th-last".to_string(),
        _ => nth.to_string(),
    }
}

fn special_day_label(part: &str) -> String {
    match DaySpec::parse(part) {
        Ok(DaySpec::LastDay) => "Last day of month".to_string(),
        Ok(DaySpec::LastWeekday) => "Last weekday".to_string(),
        Ok(DaySpec::NearestWeekday(day)) => format!("Nearest weekday to day {day}"),
        Ok(DaySpec::NthWeekday { nth, weekday }) => {
            format!("{} {}", ordinal(nth), weekday_name(weekday))
        }
        _ => part.to_string(),
    }
}

fn is_special_day_part(part: &str) -> bool {
    matches!(
        DaySpec::parse(part),
        Ok(DaySpec::LastDay
            | DaySpec::LastWeekday
            | DaySpec::NearestWeekday(_)
            | DaySpec::NthWeekday { .. })
    )
}

fn every_text(every: ScheduleEvery) -> String {
    match every {
        ScheduleEvery::Duration(duration) => duration.to_string(),
        ScheduleEvery::Days(days) => format!("{days}d"),
        ScheduleEvery::Weeks(weeks) => format!("{weeks}w"),
        ScheduleEvery::Months(months) => format!("{months}mo"),
        ScheduleEvery::Years(years) => format!("{years}y"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EveryUnit {
    Seconds,
    Minutes,
    Hours,
    Days,
    Weeks,
    Months,
    Years,
}

impl EveryUnit {
    const ALL: [Self; 7] = [
        Self::Seconds,
        Self::Minutes,
        Self::Hours,
        Self::Days,
        Self::Weeks,
        Self::Months,
        Self::Years,
    ];

    const fn suffix(self) -> &'static str {
        match self {
            Self::Seconds => "s",
            Self::Minutes => "m",
            Self::Hours => "h",
            Self::Days => "d",
            Self::Weeks => "w",
            Self::Months => "mo",
            Self::Years => "y",
        }
    }
}

fn every_parts(value: &str) -> Option<(u32, EveryUnit)> {
    let value = value.trim();
    for unit in [
        EveryUnit::Years,
        EveryUnit::Months,
        EveryUnit::Weeks,
        EveryUnit::Days,
        EveryUnit::Hours,
        EveryUnit::Minutes,
        EveryUnit::Seconds,
    ] {
        if let Some(amount) = value.strip_suffix(unit.suffix())
            && let Ok(amount) = amount.trim().parse::<u32>()
        {
            return Some((amount, unit));
        }
    }
    match ScheduleEvery::parse(value).ok()? {
        ScheduleEvery::Duration(duration) => {
            let seconds = duration.as_std().as_secs();
            if seconds >= 3600 && seconds.is_multiple_of(3600) {
                Some((u32::try_from(seconds / 3600).ok()?, EveryUnit::Hours))
            } else if seconds >= 60 && seconds.is_multiple_of(60) {
                Some((u32::try_from(seconds / 60).ok()?, EveryUnit::Minutes))
            } else {
                Some((u32::try_from(seconds).ok()?, EveryUnit::Seconds))
            }
        }
        ScheduleEvery::Days(days) => Some((days, EveryUnit::Days)),
        ScheduleEvery::Weeks(weeks) => Some((weeks, EveryUnit::Weeks)),
        ScheduleEvery::Months(months) => Some((months, EveryUnit::Months)),
        ScheduleEvery::Years(years) => Some((years, EveryUnit::Years)),
    }
}

fn every_editor(ui: &mut Ui, every: &mut String) {
    let mut enabled = !every.trim().is_empty();
    if ui.checkbox(&mut enabled, "Use every interval").changed() {
        if enabled {
            *every = "1d".to_string();
        } else {
            every.clear();
        }
    }
    let (mut amount, mut unit) = every_parts(every).unwrap_or((1, EveryUnit::Days));
    let mut changed = false;
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            changed |= ui
                .add(
                    egui::DragValue::new(&mut amount)
                        .range(1..=u32::MAX)
                        .speed(1.0),
                )
                .changed();
            egui::ComboBox::from_id_salt("schedule-every-unit")
                .selected_text(unit.suffix())
                .width(64.0)
                .show_ui(ui, |ui| {
                    for candidate in EveryUnit::ALL {
                        if ui
                            .selectable_value(&mut unit, candidate, candidate.suffix())
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });
        });
    });
    if changed {
        *every = format!("{amount}{}", unit.suffix());
    }
}

fn parse_days(value: &str) -> Result<Vec<DaySpec>, String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(DaySpec::parse)
        .collect()
}

fn parse_repeat_offsets(values: &[String]) -> Result<Vec<HumanDuration>, String> {
    let mut offsets = Vec::with_capacity(values.len());
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            return Err("Repeat offset must not be empty.".to_string());
        }
        let duration = HumanDuration::from_str(value)
            .map_err(|error| format!("Invalid repeat offset '{value}': {error}"))?;
        if duration.as_std().is_zero() {
            return Err("Repeat offsets must be greater than 0.".to_string());
        }
        if offsets.contains(&duration) {
            return Err("Repeat offsets must be unique.".to_string());
        }
        offsets.push(duration);
    }
    offsets.sort_unstable();
    Ok(offsets)
}

fn month_label(month: Month) -> &'static str {
    match month {
        Month::Jan => "Jan",
        Month::Feb => "Feb",
        Month::Mar => "Mar",
        Month::Apr => "Apr",
        Month::May => "May",
        Month::Jun => "Jun",
        Month::Jul => "Jul",
        Month::Aug => "Aug",
        Month::Sep => "Sep",
        Month::Oct => "Oct",
        Month::Nov => "Nov",
        Month::Dec => "Dec",
    }
}

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    if app.schedules.editor.is_some() {
        editor_ui(app, ui);
    } else {
        list_ui(app, ui);
    }
    if app.schedules.location_dialog.is_some() {
        location_dialog(app, ui);
    }
}

fn list_ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let live = app.gateway.phase().is_live();
    let schedules: Vec<ScheduleInfo> = app.schedules.filtered().into_iter().cloned().collect();
    let total = app.schedules.schedules.len();
    let mut selected = app
        .schedules
        .selected
        .filter(|index| *index < schedules.len());

    widgets::page_header(
        ui,
        theme,
        "Schedules",
        Some(&format!("{} configured", total)),
        |ui| {
            if ui.add_enabled(live, egui::Button::new("Refresh")).clicked() {
                app.refresh_schedules();
            }
            if ui
                .add_enabled(ready, egui::Button::new("Add schedule"))
                .clicked()
            {
                app.open_schedule_editor(None);
            }
            if ui
                .add_enabled(ready, egui::Button::new("Defaults"))
                .on_hover_text("Shared location and timezone")
                .clicked()
            {
                app.open_schedule_defaults();
            }
        },
    );
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.schedules.search)
                .hint_text("Search schedules…")
                .desired_width(360.0),
        );
        if app.schedules.loading {
            ui.spinner();
        }
        if let Some(error) = &app.schedules.error {
            ui.label(RichText::new(error).color(palette.red));
        }
    });
    ui.add_space(4.0);
    ui.label(RichText::new(format!("{total} schedules")).color(palette.muted));
    ui.add_space(4.0);

    let nav = if app.dialog.is_none() && !schedules.is_empty() && app.schedules.editor.is_none() {
        widgets::list_nav_global(ui)
    } else {
        None
    };
    let list_focus = widgets::list_focus(ui, theme, "schedules-list", nav.is_some(), false);
    let focus_row_actions = widgets::list_tab_pressed(ui, list_focus.id);
    let action_row = selected.unwrap_or(0);
    if focus_row_actions {
        selected = Some(action_row);
    }
    let mut action_focus_id = None;
    let mut activate = None;
    if let Some(nav) = nav {
        let next = nav.apply(selected, schedules.len());
        selected = Some(next);
        if nav == widgets::ListNav::Activate {
            activate = schedules.get(next).map(|info| info.schedule.clone());
        }
    }
    if list_focus.focused
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    {
        activate = schedules
            .get(selected.unwrap_or(0))
            .map(|info| info.schedule.clone());
    }
    let mut clicked_selection = None;
    let scroll_to = nav
        .map(|_| selected.unwrap_or(0))
        .or_else(|| focus_row_actions.then_some(action_row));

    egui::ScrollArea::horizontal()
        .id_salt("schedules-horizontal")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.set_min_width(900.0);
            let mut table = TableBuilder::new(ui)
                .id_salt("schedules-table")
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::CLICK)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(200.0).at_least(150.0))
                .column(Column::initial(280.0).at_least(180.0))
                .column(Column::initial(170.0).at_least(130.0))
                .column(Column::initial(180.0).at_least(130.0))
                .column(Column::remainder().at_least(170.0));
            if let Some(index) = scroll_to {
                table = table.scroll_to_row(index, Some(egui::Align::Center));
            }
            table
                .header(28.0, |mut header| {
                    for title in ["Name / ID", "Rule", "Next", "Used by", "Actions"] {
                        header.col(|ui| {
                            ui.label(RichText::new(title).strong());
                        });
                    }
                })
                .body(|body| {
                    body.rows(42.0, schedules.len(), |mut row| {
                        let index = row.index();
                        let info = &schedules[index];
                        let schedule = &info.schedule;
                        row.set_selected(selected == Some(index));
                        row.col(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&schedule.name).strong());
                                ui.label(RichText::new(&schedule.id).small().color(palette.muted));
                            });
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(summary(schedule)).small());
                        });
                        row.col(|ui| {
                            if let Some(next) = &info.next {
                                ui.label(RichText::new(&next.local).small());
                            } else {
                                ui.label(RichText::new("—").color(palette.muted));
                            }
                        });
                        row.col(|ui| {
                            let used = if info.used_by.is_empty() {
                                "—".to_string()
                            } else {
                                info.used_by.join(", ")
                            };
                            ui.label(RichText::new(used).small());
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                let edit = ui.add_enabled(ready, egui::Button::new("Edit").small());
                                if index == action_row {
                                    action_focus_id = Some(edit.id);
                                }
                                if edit.clicked() {
                                    app.open_schedule_editor(Some(schedule.clone()));
                                }
                                let blocked = !info.used_by.is_empty();
                                if ui
                                    .add_enabled(
                                        ready && !blocked,
                                        egui::Button::new("Delete").small(),
                                    )
                                    .on_hover_text(if blocked {
                                        "Remove the schedule from its tasks first"
                                    } else {
                                        "Delete schedule"
                                    })
                                    .clicked()
                                {
                                    app.schedules.confirm_delete = Some(schedule.id.clone());
                                }
                            });
                        });
                        if row.response().clicked() {
                            clicked_selection = Some(index);
                        }
                    });
                });
        });

    if let Some(index) = clicked_selection {
        selected = Some(index);
        ui.memory_mut(|memory| memory.request_focus(list_focus.id));
    }
    if focus_row_actions && let Some(id) = action_focus_id {
        ui.memory_mut(|memory| memory.request_focus(id));
    }
    app.schedules.selected = selected;
    if let Some(schedule) = activate {
        app.open_schedule_editor(Some(schedule));
    }

    if schedules.is_empty() {
        widgets::empty_state(
            ui,
            theme,
            if total == 0 {
                "No schedules"
            } else {
                "No schedules match the current filter"
            },
            "Schedules are shared by task triggers.",
        );
    }

    if let Some(schedule_id) = app.schedules.confirm_delete.clone() {
        let detail = format!("Delete schedule '{schedule_id}'?");
        match widgets::confirm_modal(
            ui.ctx(),
            theme,
            "delete-schedule",
            "Delete schedule",
            &detail,
            "Delete schedule",
        ) {
            Some(true) => app.delete_schedule(&schedule_id),
            Some(false) => app.schedules.confirm_delete = None,
            None => {}
        }
    }
    if app.schedules.defaults_editor.is_some() {
        defaults_modal(app, ui);
    }
}

fn defaults_modal(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let Some(mut editor) = app.schedules.defaults_editor.take() else {
        return;
    };
    let mut keep = true;
    let mut save = false;
    let mut close = false;
    let mut edit_location = false;
    let response = egui::Modal::new(egui::Id::unique("hodeco-schedule-defaults"))
        .frame(widgets::modal_frame(theme))
        .show(ui.ctx(), |ui| {
            widgets::dialog_width(ui, 600.0);
            widgets::dialog_header(
                ui,
                theme,
                "Schedule defaults",
                Some("Applied to schedules that do not set their own values."),
            );
            widgets::dialog_body(ui, 360.0, |ui| {
                widgets::control_group_fill(ui, "Defaults", |ui| {
                    egui::Grid::new("schedule-defaults-grid")
                        .num_columns(2)
                        .spacing([18.0, 4.0])
                        .show(ui, |ui| {
                            widgets::field_label(ui, theme, "Timezone");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "Default uses the server host timezone; Custom pins an IANA zone.",
                                |ui| {
                                    timezone_control(
                                        ui,
                                        "schedule-defaults-timezone",
                                        &mut editor.timezone,
                                        &mut editor.timezone_custom,
                                    );
                                },
                            );
                            ui.end_row();
                            widgets::field_label(ui, theme, "Location");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "None leaves the shared location unset; Custom coordinates are used by solar schedules.",
                                |ui| {
                                    ui.horizontal(|ui| {
                                        ui.radio_value(
                                            &mut editor.location_custom,
                                            false,
                                            "None",
                                        );
                                        ui.radio_value(
                                            &mut editor.location_custom,
                                            true,
                                            "Custom",
                                        );
                                        if ui
                                            .add_enabled(
                                                ready && editor.location_custom,
                                                egui::Button::new("Set location…"),
                                            )
                                            .clicked()
                                        {
                                            edit_location = true;
                                        }
                                        if editor.location_custom {
                                            match parse_location(&editor.location).ok().flatten() {
                                                Some(location) => {
                                                    ui.label(
                                                        RichText::new(format_location(location))
                                                            .monospace(),
                                                    );
                                                }
                                                None => {
                                                    ui.label(
                                                        RichText::new("Not set")
                                                            .color(palette.muted),
                                                    );
                                                }
                                            }
                                        }
                                    });
                                },
                            );
                            ui.end_row();
                        });
                });
            });
            widgets::dialog_footer(ui, theme, editor.error.as_deref(), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                let dirty = editor.is_dirty();
                let valid = editor.selections().is_ok();
                if ui
                    .add_enabled(
                        ready && !editor.saving && dirty && valid,
                        egui::Button::new(if editor.saving { "Saving…" } else { "Save" }),
                    )
                    .on_disabled_hover_text(if !dirty {
                        "No changes to save"
                    } else if !valid {
                        "Choose a valid timezone and location"
                    } else {
                        "The gateway is not connected"
                    })
                    .clicked()
                {
                    save = true;
                }
                if editor.saving {
                    ui.spinner();
                }
            });
            false
        });
    if response.should_close() && !save {
        close = true;
    }
    if save {
        match editor.selections() {
            Ok((location, timezone)) => {
                editor.error = None;
                editor.saving = true;
                if !app.save_schedule_defaults(location, timezone) {
                    editor.saving = false;
                    editor.error = Some("The gateway is not connected.".to_string());
                }
            }
            Err(error) => editor.error = Some(error),
        }
    }
    if close {
        keep = false;
    }
    if keep {
        app.schedules.defaults_editor = Some(editor);
    }
    if edit_location && keep {
        app.open_location_dialog(LocationTarget::Defaults);
    }
}

fn location_dialog(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let Some(mut dialog) = app.schedules.location_dialog.take() else {
        return;
    };
    let now = ui.input(|input| input.time);
    let mut keep = true;
    let mut save = false;
    let mut close = false;
    let mut request = None;

    if dialog.preview_due.is_some_and(|due| now >= due) {
        dialog.preview_due = None;
        if ready {
            if let Some(location) = dialog.active_location() {
                dialog.preview_generation = dialog.preview_generation.wrapping_add(1);
                dialog.previewing = true;
                dialog.preview_error = None;
                request = Some((
                    location,
                    civil_to_naive(dialog.date),
                    dialog.timezone.clone(),
                    dialog.preview_generation,
                ));
            } else {
                dialog.preview = None;
                dialog.previewing = false;
                dialog.preview_error = None;
            }
        } else {
            dialog.preview_due = Some(now + 1.0);
            dialog.previewing = false;
        }
    }

    let mut dirty = false;
    let response = egui::Modal::new(egui::Id::unique("hodeco-location-dialog"))
        .frame(widgets::modal_frame(theme))
        .show(ui.ctx(), |ui| {
            widgets::dialog_width(ui, 640.0);
            widgets::dialog_header(
                ui,
                theme,
                "Location",
                Some("Coordinates and a solar preview for the selected date."),
            );
            ui.columns(2, |columns| {
                widgets::control_group_fill(&mut columns[0], "Coordinates", |ui| {
                    let field_width = (ui.available_width() - 84.0).max(100.0);
                    egui::Grid::new("location-coordinate-grid")
                        .num_columns(2)
                        .spacing([14.0, 6.0])
                        .show(ui, |ui| {
                            widgets::field_label(ui, theme, "Latitude");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "−90 to 90; south is negative.",
                                |ui| {
                                    dirty |= ui
                                        .add_sized(
                                            [field_width, 22.0],
                                            egui::DragValue::new(&mut dialog.latitude)
                                                .range(-90.0..=90.0)
                                                .speed(0.01)
                                                .fixed_decimals(4),
                                        )
                                        .changed();
                                },
                            );
                            ui.end_row();
                            widgets::field_label(ui, theme, "Longitude");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "−180 to 180; west is negative.",
                                |ui| {
                                    dirty |= ui
                                        .add_sized(
                                            [field_width, 22.0],
                                            egui::DragValue::new(&mut dialog.longitude)
                                                .range(-180.0..=180.0)
                                                .speed(0.01)
                                                .fixed_decimals(4),
                                        )
                                        .changed();
                                },
                            );
                            ui.end_row();
                            widgets::field_label(ui, theme, "Elevation");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "Optional horizon-dip correction.",
                                |ui| {
                                    let mut has_elevation = dialog.elevation.is_some();
                                    if ui
                                        .checkbox(&mut has_elevation, "Include elevation")
                                        .changed()
                                    {
                                        dialog.elevation = has_elevation.then_some(0.0);
                                        dirty = true;
                                    }
                                    if let Some(elevation) = &mut dialog.elevation {
                                        dirty |= ui
                                            .add_sized(
                                                [field_width, 22.0],
                                                egui::DragValue::new(elevation)
                                                    .range(-500.0..=10_000.0)
                                                    .speed(1.0)
                                                    .suffix(" m"),
                                            )
                                            .changed();
                                    }
                                },
                            );
                            ui.end_row();
                        });
                });
                widgets::control_group_fill(&mut columns[1], "Solar preview", |ui| {
                    ui.horizontal(|ui| {
                        let date_label = dialog.date.to_string();
                        if date_picker_button(
                            ui,
                            "location-solar-date",
                            &mut dialog.date_picker_generation,
                            &mut dialog.date,
                            "Choose the preview date",
                            &date_label,
                        ) {
                            dirty = true;
                        }
                        if ui.button("Today").clicked() {
                            dialog.date = today_date();
                            dirty = true;
                        }
                        if dialog.previewing {
                            ui.spinner();
                        }
                    });
                    let timezone = dialog
                        .preview
                        .as_ref()
                        .map(|preview| preview.timezone.as_str())
                        .unwrap_or(dialog.timezone.as_str());
                    if timezone.is_empty() {
                        ui.label(
                            RichText::new("Times use the server timezone.")
                                .small()
                                .color(palette.muted),
                        );
                    } else {
                        ui.label(
                            RichText::new(format!("Times shown in {timezone}."))
                                .small()
                                .color(palette.muted),
                        );
                    }
                    if !ready {
                        ui.label(
                            RichText::new("Connect to a gateway to preview solar events.")
                                .small()
                                .color(palette.muted),
                        );
                    } else if let Some(error) = &dialog.preview_error {
                        ui.label(RichText::new(error).color(palette.red));
                    } else if dialog.active_location().is_none() {
                        ui.label(
                            RichText::new("Enter a valid location to preview.")
                                .small()
                                .color(palette.muted),
                        );
                    } else if let Some(preview) = &dialog.preview {
                        egui::Grid::new("solar-preview-grid")
                            .num_columns(2)
                            .spacing([18.0, 4.0])
                            .show(ui, |ui| {
                                ui.label(RichText::new("Event").strong());
                                ui.label(RichText::new("Local time").strong());
                                ui.end_row();
                                for event in &preview.events {
                                    ui.label(RichText::new(event.event.label()).strong());
                                    ui.label(
                                        RichText::new(event.local.as_deref().unwrap_or("—"))
                                            .monospace(),
                                    );
                                    ui.end_row();
                                    if let Some(reason) = &event.reason {
                                        ui.label("");
                                        ui.label(
                                            RichText::new(reason).small().color(palette.muted),
                                        );
                                        ui.end_row();
                                    }
                                }
                            });
                        ui.label(
                            RichText::new("Preview times use the same timezone as the schedule.")
                                .small()
                                .color(palette.muted),
                        );
                    } else if dialog.previewing {
                        ui.label(RichText::new("Calculating…").small().color(palette.muted));
                    } else {
                        ui.label(
                            RichText::new("Preview updates after the location changes.")
                                .small()
                                .color(palette.muted),
                        );
                    }
                });
            });

            widgets::dialog_footer(ui, theme, dialog.error.as_deref(), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui
                    .add_enabled(ready, egui::Button::new("Apply"))
                    .on_hover_text("Update this form; save the parent editor afterwards")
                    .clicked()
                {
                    save = true;
                }
            });
            false
        });
    if response.should_close() && !save {
        close = true;
    }

    if dirty {
        dialog.preview_due = Some(now + 0.35);
        dialog.error = None;
    }
    if save {
        if let Some(location) = dialog.active_location() {
            let text = format_location(location);
            match dialog.target {
                LocationTarget::Defaults => {
                    if app.schedules.defaults_editor.is_none() {
                        app.open_schedule_defaults();
                    }
                    if let Some(editor) = app.schedules.defaults_editor.as_mut() {
                        editor.location = text;
                        editor.location_custom = true;
                    }
                }
                LocationTarget::Schedule => {
                    if let Some(editor) = app.schedules.editor.as_mut() {
                        editor.location = text;
                        editor.location_custom = true;
                    }
                }
            }
            keep = false;
        } else {
            dialog.error = Some("Location is not valid.".to_string());
        }
    }
    if close {
        keep = false;
    }
    if keep {
        app.schedules.location_dialog = Some(dialog);
    }
    if let Some((location, date, timezone, generation)) = request
        && !app.preview_solar(location, date, timezone, generation)
        && let Some(dialog) = app.schedules.location_dialog.as_mut()
    {
        dialog.previewing = false;
        dialog.preview_error = Some("The gateway is not connected.".to_string());
    }
}

fn editor_ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let Some(mut editor) = app.schedules.editor.take() else {
        return;
    };
    let mut keep = true;
    let mut close = false;
    let mut save = false;
    let mut preview = false;
    let mut delete = false;
    let mut discard = false;
    let mut cancel_delete = false;
    let mut edit_location = false;
    let validation_error = editor.build().err();
    let header_error = validation_error.clone().or_else(|| editor.error.clone());
    let close_blocked = validation_error.is_some() || (editor.error.is_some() && editor.is_dirty());
    let defaults_location = app.schedules.defaults_location;
    let defaults_timezone = app.schedules.defaults_timezone.clone();
    let timezone_note = match &defaults_timezone {
        Some(timezone) => format!("Default inherits {timezone}."),
        None => "Default uses the server host timezone.".to_string(),
    };
    let used_by = app
        .schedules
        .schedules
        .iter()
        .find(|info| info.schedule.id == editor.draft.id)
        .map(|info| info.used_by.clone())
        .unwrap_or_default();
    let can_delete = !editor.is_new && used_by.is_empty();

    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !editor.saving && !close_blocked,
                egui::Button::new("← Schedules"),
            )
            .on_disabled_hover_text(
                "Resolve the error before leaving; use Discard to close without saving",
            )
            .clicked()
        {
            close = true;
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new(if editor.is_new {
                "New schedule"
            } else {
                "Edit schedule"
            })
            .size(18.0)
            .strong(),
        );
        if !editor.is_new {
            ui.label(RichText::new(&editor.draft.id).small().color(palette.muted));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(!editor.saving && !close_blocked, egui::Button::new("Close"))
                .on_hover_text("Close the editor; unsaved changes are protected")
                .on_disabled_hover_text(
                    "Resolve the error before closing; use Discard to leave without saving",
                )
                .clicked()
            {
                close = true;
            }
            ui.add_space(18.0);
            let dirty = editor.is_dirty();
            if ui
                .add_enabled(
                    ready
                        && !editor.saving
                        && validation_error.is_none()
                        && (editor.is_new || dirty),
                    egui::Button::new("Save"),
                )
                .on_disabled_hover_text(if validation_error.is_some() {
                    "Fix the validation error before saving"
                } else if !editor.is_new && !dirty {
                    "No changes to save"
                } else {
                    "The gateway is not connected"
                })
                .clicked()
            {
                save = true;
            }
            ui.add_space(4.0);
            if ui
                .add_enabled(editor.is_dirty(), egui::Button::new("Discard"))
                .clicked()
            {
                discard = true;
            }
            ui.add_space(8.0);
            if ui
                .add_enabled(
                    ready && can_delete,
                    egui::Button::new(RichText::new("Delete schedule").color(palette.red)),
                )
                .on_hover_text(if can_delete {
                    "Delete schedule"
                } else if editor.is_new {
                    "Save the schedule before deleting it"
                } else {
                    "Remove the schedule from its tasks first"
                })
                .clicked()
            {
                editor.confirm_delete = true;
            }
            if let Some(error) = &header_error {
                ui.add_space(12.0);
                ui.add(egui::Label::new(RichText::new(error).color(palette.red).size(13.5)).wrap())
                    .on_hover_text(error);
            }
        });
    });
    ui.separator();

    ui.horizontal_top(|ui| {
        let available = ui.available_width();
        let rail_width = 350.0;
        let form_width = (available - rail_width - 18.0).max(480.0);
        ui.vertical(|ui| {
            ui.set_width(form_width);
            ui.spacing_mut().text_edit_width = (form_width - 180.0).clamp(260.0, 640.0);
            egui::ScrollArea::vertical()
                .id_salt("schedule-form-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    widgets::control_group_fill(ui, "Identity", |ui| {
                        egui::Grid::new("schedule-identity")
                            .num_columns(2)
                            .spacing([18.0, 4.0])
                            .show(ui, |ui| {
                                widgets::field_label(ui, theme, "Name");
                                widgets::field_with_note(
                                    ui,
                                    theme,
                                    "Display name in the schedules list.",
                                    |ui| {
                                        let response = ui.add(egui::TextEdit::singleline(
                                            &mut editor.draft.name,
                                        ));
                                        if response.changed() && editor.is_new && editor.id_auto {
                                            editor.draft.id =
                                                id_from_name("schedule", editor.draft.name.trim());
                                        }
                                    },
                                );
                                ui.end_row();
                                widgets::field_label(ui, theme, "ID");
                                if editor.is_new {
                                    widgets::field_with_note(
                                        ui,
                                        theme,
                                        if editor.id_auto {
                                            "Auto-generated from the name; edit to override."
                                        } else {
                                            "Custom ID; it cannot be changed after creation."
                                        },
                                        |ui| {
                                            let response = widgets::auto_width_text_edit(
                                                ui,
                                                &mut editor.draft.id,
                                                "generated from the name",
                                                180.0,
                                                360.0,
                                            );
                                            if response.changed() {
                                                editor.id_auto =
                                                    editor.draft.id.trim().is_empty();
                                            }
                                            if editor.id_auto
                                                && editor.draft.id.trim().is_empty()
                                            {
                                                editor.draft.id = id_from_name(
                                                    "schedule",
                                                    editor.draft.name.trim(),
                                                );
                                            }
                                        },
                                    );
                                } else {
                                    ui.label(RichText::new(&editor.draft.id).monospace());
                                }
                                ui.end_row();
                                widgets::field_label(ui, theme, "Description");
                                widgets::field_with_note(
                                    ui,
                                    theme,
                                    "Optional notes for operators.",
                                    |ui| {
                                        ui.add(
                                            egui::TextEdit::multiline(
                                                &mut editor.draft.description,
                                            )
                                            .desired_rows(2),
                                        );
                                    },
                                );
                                ui.end_row();
                            });
                    });
                    widgets::control_group_fill(ui, "Timing", |ui| {
                        egui::Grid::new("schedule-timing")
                            .num_columns(2)
                            .spacing([18.0, 4.0])
                            .show(ui, |ui| {
                                widgets::field_label(ui, theme, "Timezone");
                                widgets::field_with_note(
                                    ui,
                                    theme,
                                    &timezone_note,
                                    |ui| {
                                        if timezone_control(
                                            ui,
                                            "schedule-editor-timezone",
                                            &mut editor.timezone,
                                            &mut editor.timezone_custom,
                                        ) {
                                            editor.preview.clear();
                                            editor.preview_error = None;
                                            editor.previewing = false;
                                            editor.preview_generation = editor
                                                .preview_generation
                                                .wrapping_add(1);
                                        }
                                    },
                                );
                                ui.end_row();
                                widgets::field_label(ui, theme, "Location");
                                widgets::field_with_note(
                                    ui,
                                    theme,
                                    "Default inherits the shared location; custom coordinates are required for solar events.",
                                    |ui| {
                                        ui.horizontal(|ui| {
                                            ui.radio_value(
                                                &mut editor.location_custom,
                                                false,
                                                "Default",
                                            );
                                            ui.radio_value(
                                                &mut editor.location_custom,
                                                true,
                                                "Custom",
                                            );
                                            if ui
                                                .add_enabled(
                                                    ready && editor.location_custom,
                                                    egui::Button::new("Set location…"),
                                                )
                                                .clicked()
                                            {
                                                edit_location = true;
                                            }
                                            if editor.location_custom {
                                                match parse_location(&editor.location)
                                                    .ok()
                                                    .flatten()
                                                {
                                                    Some(location) => {
                                                        ui.label(
                                                            RichText::new(format_location(
                                                                location,
                                                            ))
                                                            .monospace(),
                                                        );
                                                    }
                                                    None => {
                                                        ui.label(
                                                            RichText::new("Not set")
                                                                .color(palette.muted),
                                                        );
                                                    }
                                                }
                                            } else {
                                                match defaults_location {
                                                    Some(location) => {
                                                        ui.label(
                                                            RichText::new(format!(
                                                                "Inherited: {}",
                                                                format_location(location)
                                                            ))
                                                            .color(palette.muted),
                                                        );
                                                    }
                                                    None => {
                                                        ui.label(
                                                            RichText::new(
                                                                "No shared location is configured.",
                                                            )
                                                            .color(palette.muted),
                                                        );
                                                    }
                                                }
                                            }
                                        });
                                    },
                                );
                                ui.end_row();
                                widgets::field_label(ui, theme, "From");
                                widgets::field_with_note(
                                    ui,
                                    theme,
                                    "Date-only uses the schedule timezone; optional time uses the same timezone. Anchor for every.",
                                    |ui| {
                                        ui.horizontal(|ui| {
                                            let response = ui.add(
                                                egui::TextEdit::singleline(&mut editor.from_date)
                                                    .hint_text("YYYY-MM-DD")
                                                    .desired_width(130.0),
                                            );
                                            if response.changed() {
                                                remember_valid_date(
                                                    &editor.from_date,
                                                    &mut editor.from_date_valid,
                                                );
                                            }
                                            let commit = response.lost_focus()
                                                || (response.has_focus()
                                                    && ui.input(|input| {
                                                        input.key_pressed(egui::Key::Enter)
                                                    }));
                                            if commit {
                                                commit_date_input(
                                                    &mut editor.from_date,
                                                    &mut editor.from_date_valid,
                                                );
                                            }
                                            let mut date = picker_date(&editor.from_date);
                                            if date_picker_button(
                                                ui,
                                                "schedule-from-date",
                                                &mut editor.from_picker_generation,
                                                &mut date,
                                                "Choose the from date",
                                                "Pick date",
                                            ) {
                                                editor.from_date =
                                                    civil_to_naive(date).to_string();
                                                editor.from_date_valid = editor.from_date.clone();
                                            }
                                            ui.checkbox(&mut editor.from_time_enabled, "Time");
                                            ui.add_enabled_ui(
                                                editor.from_time_enabled,
                                                |ui| {
                                                    ui.horizontal(|ui| {
                                                        time_editor(
                                                            ui,
                                                            &mut editor.from_hour,
                                                            &mut editor.from_minute,
                                                            &mut editor.from_second,
                                                        );
                                                    });
                                                },
                                            );
                                        });
                                    },
                                );
                                ui.end_row();
                                widgets::field_label(ui, theme, "Until");
                                widgets::field_with_note(
                                    ui,
                                    theme,
                                    "Exclusive end; date-only uses the schedule timezone. Time is optional.",
                                    |ui| {
                                        ui.horizontal(|ui| {
                                            let response = ui.add(
                                                egui::TextEdit::singleline(&mut editor.until_date)
                                                    .hint_text("YYYY-MM-DD")
                                                    .desired_width(130.0),
                                            );
                                            if response.changed() {
                                                remember_valid_date(
                                                    &editor.until_date,
                                                    &mut editor.until_date_valid,
                                                );
                                            }
                                            let commit = response.lost_focus()
                                                || (response.has_focus()
                                                    && ui.input(|input| {
                                                        input.key_pressed(egui::Key::Enter)
                                                    }));
                                            if commit {
                                                commit_date_input(
                                                    &mut editor.until_date,
                                                    &mut editor.until_date_valid,
                                                );
                                            }
                                            let mut date = picker_date(&editor.until_date);
                                            if date_picker_button(
                                                ui,
                                                "schedule-until-date",
                                                &mut editor.until_picker_generation,
                                                &mut date,
                                                "Choose the until date",
                                                "Pick date",
                                            ) {
                                                editor.until_date =
                                                    civil_to_naive(date).to_string();
                                                editor.until_date_valid =
                                                    editor.until_date.clone();
                                            }
                                            ui.checkbox(&mut editor.until_time_enabled, "Time");
                                            ui.add_enabled_ui(
                                                editor.until_time_enabled,
                                                |ui| {
                                                    ui.horizontal(|ui| {
                                                        time_editor(
                                                            ui,
                                                            &mut editor.until_hour,
                                                            &mut editor.until_minute,
                                                            &mut editor.until_second,
                                                        );
                                                    });
                                                },
                                            );
                                        });
                                    },
                                );
                                ui.end_row();
                            });
                    });
                    widgets::control_group_fill(ui, "Recurrence", |ui| {
                            ui.label(RichText::new("At").strong());
                            ui.label(
                                RichText::new(
                                    "Fixed times, :MM hourly marks, or solar events. Leave empty when using every.",
                                )
                                .small(),
                            );
                            let mut remove = None;
                            for (index, at) in editor.at_rows.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt(("schedule-at-kind", index))
                                        .selected_text(match at.kind {
                                            AtRowKind::Time => "Fixed time",
                                            AtRowKind::HourlyMinute => "Hourly minute",
                                            AtRowKind::Solar => "Solar event",
                                        })
                                        .width(130.0)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut at.kind,
                                                AtRowKind::Time,
                                                "Fixed time",
                                            );
                                            ui.selectable_value(
                                                &mut at.kind,
                                                AtRowKind::HourlyMinute,
                                                "Hourly minute",
                                            );
                                            ui.selectable_value(
                                                &mut at.kind,
                                                AtRowKind::Solar,
                                                "Solar event",
                                            );
                                        });
                                    match at.kind {
                                        AtRowKind::Time => {
                                            time_editor(
                                                ui,
                                                &mut at.hour,
                                                &mut at.minute,
                                                &mut at.second,
                                            );
                                        }
                                        AtRowKind::HourlyMinute => {
                                            ui.add(
                                                egui::DragValue::new(&mut at.minute)
                                                    .range(0..=59)
                                                    .prefix(":"),
                                            );
                                        }
                                        AtRowKind::Solar => {
                                            egui::ComboBox::from_id_salt((
                                                "schedule-at-solar",
                                                index,
                                            ))
                                            .selected_text(at.solar.label())
                                            .width(130.0)
                                            .show_ui(ui, |ui| {
                                                for event in SolarEvent::ALL {
                                                    ui.selectable_value(
                                                        &mut at.solar,
                                                        event,
                                                        event.label(),
                                                    );
                                                }
                                            });
                                            ui.add(
                                                egui::DragValue::new(&mut at.offset_amount)
                                                    .range(-10_000..=10_000)
                                                    .speed(1.0),
                                            );
                                            egui::ComboBox::from_id_salt((
                                                "schedule-at-offset-unit",
                                                index,
                                            ))
                                            .selected_text(at.offset_unit.suffix())
                                            .width(56.0)
                                            .show_ui(ui, |ui| {
                                                for unit in OffsetUnit::ALL {
                                                    ui.selectable_value(
                                                        &mut at.offset_unit,
                                                        unit,
                                                        unit.suffix(),
                                                    );
                                                }
                                            });

                                        }
                                    }
                                    if ui.small_button("Remove").clicked() {
                                        remove = Some(index);
                                    }
                                });
                            }
                            if let Some(index) = remove {
                                editor.at_rows.remove(index);
                            }
                            if ui.button("Add at entry").clicked() {
                                editor.at_rows.push(AtRow::time("12:00"));
                            }
                            ui.add_space(6.0);
                            widgets::field_label(ui, theme, "Every");
                            every_editor(ui, &mut editor.every);
                            ui.label(
                                RichText::new(
                                    "Elapsed-time every (s/m/h) cannot be combined with At entries; use Repeat for intervals after At. Every requires From.",
                                )
                                .small()
                                .color(palette.muted),
                            );
                            ui.add_space(6.0);
                            ui.label(RichText::new("Repeat after").strong());
                            ui.label(
                                RichText::new(
                                    "Each offset adds one extra run after every scheduled occurrence. Repeats that land on or after the next scheduled occurrence are skipped; Until also stops them.",
                                )
                                .small()
                                .color(palette.muted),
                            );
                            let mut remove_repeat = None;
                            for (index, offset) in editor.repeat_at.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    widgets::duration_editor(
                                        ui,
                                        offset,
                                        std::time::Duration::from_secs(1),
                                    );
                                    if ui.small_button("Remove").clicked() {
                                        remove_repeat = Some(index);
                                    }
                                });
                            }
                            if let Some(index) = remove_repeat {
                                editor.repeat_at.remove(index);
                            }
                            if ui.button("Add repeat offset").clicked() {
                                editor.repeat_at.push("1m".to_string());
                            }
                            if editor.repeat_at.is_empty() {
                                ui.label(
                                    RichText::new("No repeats.").small().color(palette.muted),
                                );
                            }
                            ui.add_space(4.0);
                            widgets::field_label(ui, theme, "Weekdays");
                            ui.horizontal_wrapped(|ui| {
                                for weekday in [
                                    Weekday::Mon,
                                    Weekday::Tue,
                                    Weekday::Wed,
                                    Weekday::Thu,
                                    Weekday::Fri,
                                    Weekday::Sat,
                                    Weekday::Sun,
                                ] {
                                    let label = weekday_short(weekday);
                                    let mut parts = day_parts(&editor.days);
                                    let selected = parts
                                        .iter()
                                        .any(|part| day_is_weekday(part, weekday));
                                    if ui.selectable_label(selected, label).clicked() {
                                        if selected {
                                            parts.retain(|part| {
                                                !day_is_weekday(part, weekday)
                                            });
                                        } else {
                                            parts.push(label.to_string());
                                        }
                                        editor.days = parts.join(", ");
                                    }
                                }
                            });
                            widgets::field_label(ui, theme, "Month days");
                            ui.horizontal_wrapped(|ui| {
                                for day in 1..=31u8 {
                                    let mut parts = day_parts(&editor.days);
                                    let selected = parts
                                        .iter()
                                        .any(|part| day_is_month_day(part, day));
                                    if ui.selectable_label(selected, day.to_string()).clicked() {
                                        if selected {
                                            parts.retain(|part| {
                                                !day_is_month_day(part, day)
                                            });
                                        } else {
                                            parts.push(day.to_string());
                                        }
                                        editor.days = parts.join(", ");
                                    }
                                }
                            });
                            ui.label(RichText::new("Special day rules").strong());
                            ui.label(
                                RichText::new(
                                    "Special rules combine with weekday and month-day chips using Match.",
                                )
                                .small()
                                .color(palette.muted),
                            );
                            let mut remove_special = None;
                            for part in day_parts(&editor.days) {
                                if !is_special_day_part(&part)
                                    && DaySpec::parse(&part).is_ok()
                                {
                                    continue;
                                }
                                ui.horizontal(|ui| {
                                    if DaySpec::parse(&part).is_err() {
                                        ui.label(
                                            RichText::new(format!("Invalid: {part}"))
                                                .color(palette.red),
                                        );
                                    } else {
                                        ui.label(special_day_label(&part));
                                    }
                                    if ui.small_button("Remove").clicked() {
                                        remove_special = Some(part.clone());
                                    }
                                });
                            }
                            if let Some(part) = remove_special {
                                let mut parts = day_parts(&editor.days);
                                if let Some(index) =
                                    parts.iter().position(|candidate| candidate == &part)
                                {
                                    parts.remove(index);
                                }
                                editor.days = parts.join(", ");
                            }
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt("special-day-kind")
                                    .selected_text(editor.special_kind.label())
                                    .width(150.0)
                                    .show_ui(ui, |ui| {
                                        for kind in SpecialDayKind::ALL {
                                            ui.selectable_value(
                                                &mut editor.special_kind,
                                                kind,
                                                kind.label(),
                                            );
                                        }
                                    });
                                match editor.special_kind {
                                    SpecialDayKind::NearestWeekday => {
                                        ui.add(
                                            egui::DragValue::new(&mut editor.special_day)
                                                .range(1..=31)
                                                .prefix("day "),
                                        )
                                        .on_hover_text(
                                            "Matches the weekday closest to this day of the month. Saturday moves to Friday; Sunday moves to Monday.",
                                        );
                                    }
                                    SpecialDayKind::NthWeekday => {
                                        egui::ComboBox::from_id_salt(
                                            "special-day-nth-position",
                                        )
                                        .selected_text(ordinal(editor.special_nth))
                                        .width(90.0)
                                        .show_ui(ui, |ui| {
                                            for nth in [1_i8, 2, 3, 4, 5, -1, -2, -3, -4, -5] {
                                                ui.selectable_value(
                                                    &mut editor.special_nth,
                                                    nth,
                                                    ordinal(nth),
                                                );
                                            }
                                        })
                                        .response
                                        .on_hover_text(
                                            "Negative positions count from the end: Last = last Monday, 2nd-last = second-to-last Monday. A month with fewer occurrences does not match.",
                                        );
                                        egui::ComboBox::from_id_salt(
                                            "special-day-weekday",
                                        )
                                        .selected_text(weekday_name(
                                            editor.special_weekday,
                                        ))
                                        .width(110.0)
                                        .show_ui(ui, |ui| {
                                            for weekday in [
                                                Weekday::Mon,
                                                Weekday::Tue,
                                                Weekday::Wed,
                                                Weekday::Thu,
                                                Weekday::Fri,
                                                Weekday::Sat,
                                                Weekday::Sun,
                                            ] {
                                                ui.selectable_value(
                                                    &mut editor.special_weekday,
                                                    weekday,
                                                    weekday_name(weekday),
                                                );
                                            }
                                        });
                                    }
                                    _ => {}
                                }
                                if ui.button("Add special rule").clicked() {
                                    let spec = special_day_spec(
                                        editor.special_kind,
                                        editor.special_day,
                                        editor.special_nth,
                                        editor.special_weekday,
                                    );
                                    let mut parts = day_parts(&editor.days);
                                    let value = spec.display();
                                    if !parts.contains(&value) {
                                        parts.push(value);
                                        editor.days = parts.join(", ");
                                    }
                                }
                            });
                            ui.horizontal(|ui| {
                                widgets::field_label(ui, theme, "Match");
                                egui::ComboBox::from_id_salt("schedule-day-match")
                                    .selected_text(match editor.day_match {
                                        DayMatch::Any => "Any",
                                        DayMatch::All => "All",
                                    })
                                    .width(80.0)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut editor.day_match,
                                            DayMatch::Any,
                                            "Any",
                                        );
                                        ui.selectable_value(
                                            &mut editor.day_match,
                                            DayMatch::All,
                                            "All",
                                        );
                                    });
                                ui.label(
                                    RichText::new(
                                        "Any: match any selected day rule. All: satisfy every rule.",
                                    )
                                    .small()
                                    .color(palette.muted),
                                );
                            });
                            ui.add_space(4.0);
                            widgets::field_label(ui, theme, "Months");
                            ui.horizontal_wrapped(|ui| {
                                for month in Month::ALL {
                                    let selected = editor.months.contains(&month);
                                    if ui
                                        .selectable_label(selected, month_label(month))
                                        .clicked()
                                    {
                                        if selected {
                                            editor.months.retain(|candidate| *candidate != month);
                                        } else {
                                            editor.months.push(month);
                                        }
                                    }
                                }
                            });
                            ui.label(
                                RichText::new("No selection means every month.")
                                    .small()
                                    .color(palette.muted),
                            );
                            ui.add_space(4.0);
                            ui.label(RichText::new("Years").strong());
                            ui.label(
                                RichText::new("Leave empty to match every year.")
                                    .small()
                                    .color(palette.muted),
                            );
                            let mut remove_year = None;
                            for (index, year) in editor.years.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::DragValue::new(year)
                                            .range(1..=9999)
                                            .speed(1.0),
                                    );
                                    if ui.small_button("Remove").clicked() {
                                        remove_year = Some(index);
                                    }
                                });
                            }
                            if let Some(index) = remove_year {
                                editor.years.remove(index);
                            }
                            if ui.button("Add year").clicked() {
                                editor.years.push(Local::now().year());
                            }
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                let jitter_toggle =
                                    ui.checkbox(&mut editor.jitter_enabled, "Add jitter");
                                if jitter_toggle.changed()
                                    && editor.jitter_enabled
                                    && editor.jitter.trim().is_empty()
                                {
                                    editor.jitter = "30s".to_string();
                                }
                                ui.add_enabled_ui(editor.jitter_enabled, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("±").monospace());
                                        widgets::duration_editor(
                                            ui,
                                            &mut editor.jitter,
                                            std::time::Duration::from_secs(1),
                                        );
                                    });
                                });
                            });
                            ui.label(
                                RichText::new(
                                    "Positive magnitude; offsets range from −jitter to +jitter, clamped to From/Until.",
                                )
                                .small()
                                .color(palette.muted),
                            );
                    });
                    widgets::control_group_fill(ui, "Windows and exceptions", |ui| {
                        ui.label(RichText::new("Allow windows").strong());
                        ui.label(
                            RichText::new(
                                "Inclusive windows in the schedule timezone; leave empty to allow all times.",
                            )
                            .small()
                            .color(palette.muted),
                        );
                        let mut remove = None;
                        for (index, window) in editor.allow.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                time_editor(
                                    ui,
                                    &mut window.from_hour,
                                    &mut window.from_minute,
                                    &mut window.from_second,
                                );
                                ui.label("to");
                                time_editor(
                                    ui,
                                    &mut window.to_hour,
                                    &mut window.to_minute,
                                    &mut window.to_second,
                                );
                                if ui.small_button("Remove").clicked() {
                                    remove = Some(index);
                                }
                            });
                        }
                        if let Some(index) = remove {
                            editor.allow.remove(index);
                        }
                        if ui.button("Add allow window").clicked() {
                            editor.allow.push(TimeWindowRow::new("16:00", "22:00"));
                        }
                        if editor.allow.is_empty() {
                            ui.label(RichText::new("None").small().color(palette.muted));
                        }
                        ui.add_space(6.0);

                        ui.label(RichText::new("Deny windows").strong());
                        ui.label(
                            RichText::new(
                                "Inclusive windows in the schedule timezone; deny takes precedence over allow.",
                            )
                            .small()
                            .color(palette.muted),
                        );
                        let mut remove = None;
                        for (index, window) in editor.deny.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                time_editor(
                                    ui,
                                    &mut window.from_hour,
                                    &mut window.from_minute,
                                    &mut window.from_second,
                                );
                                ui.label("to");
                                time_editor(
                                    ui,
                                    &mut window.to_hour,
                                    &mut window.to_minute,
                                    &mut window.to_second,
                                );
                                if ui.small_button("Remove").clicked() {
                                    remove = Some(index);
                                }
                            });
                        }
                        if let Some(index) = remove {
                            editor.deny.remove(index);
                        }
                        if ui.button("Add deny window").clicked() {
                            editor.deny.push(TimeWindowRow::new("12:00", "13:00"));
                        }
                        if editor.deny.is_empty() {
                            ui.label(RichText::new("None").small().color(palette.muted));
                        }
                        ui.add_space(6.0);

                        ui.label(RichText::new("Exceptions").strong());
                        ui.label(
                            RichText::new(
                                "Skip occurrences whose local date falls in these inclusive ranges. Dates use the schedule timezone; both ends are required.",
                            )
                            .small()
                            .color(palette.muted),
                        );
                        let mut remove = None;
                        for (index, range) in editor.except.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                date_field(
                                    ui,
                                    &format!("exception-from-{index}"),
                                    &mut range.from_date,
                                    &mut range.from_date_valid,
                                    &mut range.from_picker_generation,
                                );
                                ui.label("to");
                                date_field(
                                    ui,
                                    &format!("exception-to-{index}"),
                                    &mut range.to_date,
                                    &mut range.to_date_valid,
                                    &mut range.to_picker_generation,
                                );
                                if ui.small_button("Remove").clicked() {
                                    remove = Some(index);
                                }
                            });
                        }
                        if let Some(index) = remove {
                            editor.except.remove(index);
                        }
                        if ui.button("Add exception").clicked() {
                            editor.except.push(DateRangeRow::new());
                        }
                        if editor.except.is_empty() {
                            ui.label(RichText::new("None").small().color(palette.muted));
                        }
                    });
                });
        });
        ui.vertical(|ui| {
            ui.set_width(rail_width);
            egui::ScrollArea::vertical()
                .id_salt("schedule-rail-scroll")
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_width(rail_width);
                    let demo_solar = app.gateway.is_demo()
                        && editor.draft.at.iter().any(|at| {
                            at.parse()
                                .is_ok_and(|parsed| matches!(parsed, ParsedAt::Solar { .. }))
                        });
                    egui::Frame::group(ui.style())
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.set_min_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Preview").strong());
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .add_enabled(
                                                    ready && !editor.previewing && !demo_solar,
                                                    egui::Button::new("Refresh"),
                                                )
                                                .on_disabled_hover_text(
                                                    "Solar previews require the gateway",
                                                )
                                                .clicked()
                                            {
                                                preview = true;
                                            }
                                            if editor.previewing {
                                                ui.spinner();
                                            }
                                        },
                                    );
                                });
                                let timezone = editor.timezone.trim();
                                if timezone.is_empty() {
                                    ui.label(
                                        RichText::new("Times use the schedule timezone.")
                                            .small()
                                            .color(palette.muted),
                                    );
                                } else {
                                    ui.label(
                                        RichText::new(format!("Times in {timezone}."))
                                            .small()
                                            .color(palette.muted),
                                    );
                                }
                                if let Some(error) = &editor.preview_error {
                                    ui.label(RichText::new(error).color(palette.red));
                                }
                                ui.add_space(2.0);
                                ui.separator();
                                ui.add_space(2.0);
                                if demo_solar {
                                    ui.label(
                                        RichText::new(
                                            "Preview requires the gateway for solar schedules.",
                                        )
                                        .small()
                                        .color(palette.muted),
                                    );
                                } else if editor.preview.is_empty() {
                                    ui.label(
                                        RichText::new(
                                            "Press Refresh to preview the next occurrences.",
                                        )
                                        .small()
                                        .color(palette.muted),
                                    );
                                }
                                for occurrence in &editor.preview {
                                    if demo_solar {
                                        break;
                                    }
                                    let color = if occurrence.skipped {
                                        palette.amber
                                    } else {
                                        palette.text
                                    };
                                    let suffix = match occurrence.kind {
                                        OccurrenceKind::Scheduled => String::new(),
                                        OccurrenceKind::Repeat => occurrence
                                            .repeat_index
                                            .map(|index| format!(" · repeat {index}"))
                                            .unwrap_or_else(|| " · repeat".to_string()),
                                    };
                                    ui.label(
                                        RichText::new(format!("{}{suffix}", occurrence.local))
                                            .small()
                                            .color(color),
                                    );
                                    if let Some(reason) = &occurrence.reason {
                                        ui.label(
                                            RichText::new(reason).small().color(palette.muted),
                                        );
                                    }
                                }
                            });
                        });
                    ui.add_space(6.0);
                    widgets::control_group_fill(ui, "Used by", |ui| {
                        ui.add_space(2.0);
                        if used_by.is_empty() {
                            ui.label(RichText::new("None").color(palette.muted));
                        } else {
                            for task in &used_by {
                                ui.label(task);
                            }
                        }
                    });
                });
        });
    });

    if save && let Ok(schedule) = editor.build() {
        editor.error = None;
        editor.saving = true;
        if !app.save_schedule(schedule, editor.is_new) {
            editor.saving = false;
            editor.error = Some("The gateway is not connected.".to_string());
        }
    }
    if preview {
        match editor.build() {
            Ok(schedule) => {
                editor.previewing = true;
                editor.preview_error = None;
                editor.preview_generation = editor.preview_generation.wrapping_add(1);
                if !app.preview_schedule(&schedule, editor.preview_generation) {
                    editor.previewing = false;
                    editor.preview_error = Some("The gateway is not connected.".to_string());
                }
            }
            Err(error) => {
                editor.preview.clear();
                editor.preview_error = Some(error);
            }
        }
    }
    if editor.confirm_delete {
        let detail = format!(
            "Delete schedule '{}'? It is still used by tasks.",
            editor.draft.id
        );
        match widgets::confirm_modal(
            ui.ctx(),
            theme,
            "delete-schedule-editor",
            "Delete schedule",
            &detail,
            "Delete anyway",
        ) {
            Some(true) => delete = true,
            Some(false) => cancel_delete = true,
            None => {}
        }
    }
    if delete {
        let id = editor.draft.id.clone();
        app.delete_schedule(&id);
    }
    if cancel_delete {
        editor.confirm_delete = false;
    }
    if discard {
        keep = false;
    }
    if close {
        if editor.is_dirty() {
            editor.error =
                Some("Save or discard the changes before leaving the editor.".to_string());
        } else {
            keep = false;
        }
    }
    if keep {
        app.schedules.editor = Some(editor);
    }
    if edit_location && keep {
        app.open_location_dialog(LocationTarget::Schedule);
    }
}

fn summary(schedule: &ScheduleDefinition) -> String {
    let mut parts = Vec::new();
    let at = schedule
        .at
        .iter()
        .map(|entry| entry.display())
        .collect::<Vec<_>>()
        .join(", ");
    if !at.is_empty() {
        parts.push(at);
    }
    if let Some(every) = schedule.every {
        parts.push(format!("every {}", every_label(every)));
    }
    if !schedule.days.is_empty() {
        parts.push(
            schedule
                .days
                .iter()
                .map(|day| day.display())
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    if schedule.at.is_empty() && schedule.every.is_none() {
        parts.push("one-shot".to_string());
    }
    if !schedule.repeat_at.is_empty() {
        let offsets = schedule
            .repeat_offsets()
            .iter()
            .map(|offset| HumanDuration::from_std(*offset).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        if !offsets.is_empty() {
            parts.push(format!("repeat after {offsets}"));
        }
    }
    parts.join(" · ")
}

fn every_label(every: hodeco_contract::domain::ScheduleEvery) -> String {
    match every {
        hodeco_contract::domain::ScheduleEvery::Duration(duration) => duration.to_string(),
        hodeco_contract::domain::ScheduleEvery::Days(days) => format!("{days}d"),
        hodeco_contract::domain::ScheduleEvery::Weeks(weeks) => format!("{weeks}w"),
        hodeco_contract::domain::ScheduleEvery::Months(months) => format!("{months}mo"),
        hodeco_contract::domain::ScheduleEvery::Years(years) => format!("{years}y"),
    }
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn format_location(location: Location) -> String {
    match location.elevation {
        Some(elevation) => format!(
            "{}, {}, {}",
            location.latitude, location.longitude, elevation
        ),
        None => format!("{}, {}", location.latitude, location.longitude),
    }
}

pub(crate) fn parse_location(value: &str) -> Result<Option<Location>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
    if parts.len() < 2 || parts.len() > 3 {
        return Err("Location must be 'latitude, longitude[, elevation]'.".to_string());
    }
    let latitude = parts[0]
        .parse::<f64>()
        .map_err(|_| "Invalid latitude.".to_string())?;
    let longitude = parts[1]
        .parse::<f64>()
        .map_err(|_| "Invalid longitude.".to_string())?;
    let elevation = parts
        .get(2)
        .map(|value| value.parse::<f64>())
        .transpose()
        .map_err(|_| "Invalid elevation.".to_string())?;
    Ok(Some(Location::new(latitude, longitude, elevation)))
}

fn timezone_items() -> &'static [(Tz, String)] {
    static ITEMS: OnceLock<Vec<(Tz, String)>> = OnceLock::new();
    ITEMS.get_or_init(|| {
        let mut items: Vec<(Tz, String)> = TZ_VARIANTS
            .iter()
            .map(|timezone| (*timezone, timezone.name().to_string()))
            .collect();
        items.sort_by(|left, right| left.1.cmp(&right.1));
        items
    })
}

fn timezone_control(ui: &mut Ui, id_salt: &str, timezone: &mut String, custom: &mut bool) -> bool {
    let before = timezone.clone();
    let before_custom = *custom;
    ui.horizontal(|ui| {
        ui.radio_value(custom, false, "Default");
        ui.radio_value(custom, true, "Custom");
        if !*custom {
            return;
        }
        let selected = timezone.trim().parse::<Tz>().ok();
        let mut choice = selected;
        let selected_text = selected
            .map(|timezone| timezone.name().to_string())
            .unwrap_or_else(|| "Select timezone".to_string());
        widgets::searchable_combo(
            ui,
            id_salt,
            selected_text,
            (ui.available_width() - 4.0).clamp(180.0, 280.0),
            &mut choice,
            widgets::ComboOptions {
                any_label: None,
                emphasize: &[],
            },
            timezone_items(),
        );
        if choice != selected {
            *timezone = choice
                .map(|timezone| timezone.name().to_string())
                .unwrap_or_default();
        }
    });
    *timezone != before || *custom != before_custom
}

fn parse_date_only(value: &str, label: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .map_err(|_| format!("{label} must use YYYY-MM-DD."))
}

fn remember_valid_date(value: &str, last_valid: &mut String) {
    if value.trim().is_empty() || NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").is_ok() {
        *last_valid = value.to_string();
    }
}

fn commit_date_input(value: &mut String, last_valid: &mut String) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        value.clear();
        last_valid.clear();
        return;
    }
    match NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        Ok(date) => {
            *value = date.to_string();
            *last_valid = value.clone();
        }
        Err(_) => *value = last_valid.clone(),
    }
}

fn parse_duration(value: &str) -> Result<Option<HumanDuration>, String> {
    let value = value.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        HumanDuration::from_str(value)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;
    use std::time::Duration;

    #[test]
    fn opening_a_single_value_schedule_is_not_dirty() {
        let schedule = ScheduleDefinition {
            id: "sunset".to_string(),
            name: "Sunset".to_string(),
            at: OneOrMany::One(AtSpec::Text("sunset - 15m".to_string())),
            repeat_at: OneOrMany::One(HumanDuration::from_std(Duration::from_secs(60))),
            allow: OneOrMany::One(TimeWindow {
                from: NaiveTime::from_hms_opt(16, 0, 0).unwrap(),
                to: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
            }),
            ..ScheduleDefinition::default()
        };
        let editor = ScheduleEditor::new(schedule, false, None);
        assert!(!editor.is_dirty());
    }

    #[test]
    fn opening_unsorted_repeat_offsets_is_not_dirty() {
        let schedule = ScheduleDefinition {
            id: "repeat".to_string(),
            name: "Repeat".to_string(),
            at: OneOrMany::One(AtSpec::Text("09:00".to_string())),
            repeat_at: OneOrMany::Many(vec![
                HumanDuration::from_std(Duration::from_secs(60 * 60)),
                HumanDuration::from_std(Duration::from_secs(30 * 60)),
            ]),
            ..ScheduleDefinition::default()
        };
        let editor = ScheduleEditor::new(schedule, false, None);
        assert!(!editor.is_dirty());
    }

    #[test]
    fn validates_timezone_names() {
        assert!("Europe/Berlin".parse::<Tz>().is_ok());
        assert!("Mars/Olympus".parse::<Tz>().is_err());
        assert!(timezone_items().iter().any(|(_, name)| name == "UTC"));
    }

    #[test]
    fn repeat_offsets_build_in_ascending_order() {
        let mut editor = ScheduleEditor::new(
            ScheduleDefinition {
                name: "Test".to_string(),
                at: OneOrMany::One(AtSpec::Text("09:00".to_string())),
                ..ScheduleDefinition::default()
            },
            true,
            None,
        );
        editor.repeat_at = vec!["1m".to_string(), "30s".to_string()];
        let offsets = editor.build().unwrap().repeat_offsets();
        assert_eq!(
            offsets,
            vec![Duration::from_secs(30), Duration::from_secs(60)]
        );
    }

    #[test]
    fn repeat_offsets_reject_duplicates() {
        let mut editor = ScheduleEditor::new(
            ScheduleDefinition {
                name: "Test".to_string(),
                at: OneOrMany::One(AtSpec::Text("09:00".to_string())),
                ..ScheduleDefinition::default()
            },
            true,
            None,
        );
        editor.repeat_at = vec!["30s".to_string(), "30s".to_string()];
        let error = editor.build().unwrap_err();
        assert!(error.contains("Repeat offsets must be unique"), "{error}");
    }

    #[test]
    fn repeat_requires_a_base_occurrence() {
        let mut editor = ScheduleEditor::new(
            ScheduleDefinition {
                name: "Test".to_string(),
                ..ScheduleDefinition::default()
            },
            true,
            None,
        );
        editor.repeat_at = vec!["20m".to_string()];
        let error = editor.build().unwrap_err();
        assert!(
            error.contains("Schedule needs From, At, or Every"),
            "{error}"
        );
    }

    #[test]
    fn schedule_jitter_is_only_saved_when_enabled() {
        let mut editor = ScheduleEditor::new(
            ScheduleDefinition {
                name: "Test".to_string(),
                at: OneOrMany::One(AtSpec::Text("09:00".to_string())),
                ..ScheduleDefinition::default()
            },
            true,
            None,
        );
        editor.jitter = "30s".to_string();
        assert!(!editor.jitter_enabled);
        assert_eq!(editor.build().unwrap().jitter, None);

        editor.jitter_enabled = true;
        assert_eq!(
            editor.build().unwrap().jitter.unwrap().as_std(),
            Duration::from_secs(30)
        );

        editor.jitter.clear();
        assert!(editor.build().is_err());
    }

    #[test]
    fn date_inputs_commit_valid_values_and_restore_invalid_ones() {
        let mut value = "2026-01-01".to_string();
        let mut last_valid = value.clone();

        value = "2026-02-03".to_string();
        remember_valid_date(&value, &mut last_valid);
        commit_date_input(&mut value, &mut last_valid);
        assert_eq!(value, "2026-02-03");

        value = "not a date".to_string();
        commit_date_input(&mut value, &mut last_valid);
        assert_eq!(value, last_valid);

        value.clear();
        commit_date_input(&mut value, &mut last_valid);
        assert!(value.is_empty());
        assert!(last_valid.is_empty());
    }

    #[test]
    fn elapsed_every_accepts_long_durations() {
        let mut editor = ScheduleEditor::new(
            ScheduleDefinition {
                name: "Test".to_string(),
                ..ScheduleDefinition::default()
            },
            true,
            None,
        );
        editor.at_rows.clear();
        editor.from_date = "2026-11-19".to_string();
        editor.every = "1800m".to_string();
        let schedule = editor.build().unwrap();
        assert!(matches!(schedule.every, Some(ScheduleEvery::Duration(_))));
    }

    #[test]
    fn every_parts_keep_the_literal_unit() {
        assert_eq!(every_parts("60m"), Some((60, EveryUnit::Minutes)));
        assert_eq!(every_parts("60s"), Some((60, EveryUnit::Seconds)));
        assert_eq!(every_parts("1 h"), Some((1, EveryUnit::Hours)));
        assert_eq!(every_parts("1mo"), Some((1, EveryUnit::Months)));
    }

    #[test]
    fn structured_recurrence_and_windows_build() {
        let mut editor = ScheduleEditor::new(
            ScheduleDefinition {
                name: "Test".to_string(),
                ..ScheduleDefinition::default()
            },
            true,
            None,
        );
        editor.at_rows = vec![
            AtRow::time("09:30"),
            AtRow {
                kind: AtRowKind::Solar,
                hour: 0,
                minute: 0,
                second: 0,
                solar: SolarEvent::Sunset,
                offset_amount: -15,
                offset_unit: OffsetUnit::Minutes,
            },
            AtRow {
                kind: AtRowKind::HourlyMinute,
                hour: 0,
                minute: 15,
                second: 0,
                solar: SolarEvent::Sunrise,
                offset_amount: 0,
                offset_unit: OffsetUnit::Minutes,
            },
        ];
        editor.years = vec![2027, 2026];
        editor.repeat_at = vec!["30s".to_string(), "1m".to_string()];
        editor.allow = vec![TimeWindowRow::new("16:00", "22:00")];
        editor.deny = vec![TimeWindowRow::new("12:00", "13:00")];
        editor.except = vec![DateRangeRow::from_range(DateRange {
            from: NaiveDate::from_ymd_opt(2026, 12, 24).unwrap(),
            to: NaiveDate::from_ymd_opt(2026, 12, 26).unwrap(),
        })];

        let schedule = editor.build().unwrap();
        assert_eq!(
            schedule.at.iter().map(AtSpec::display).collect::<Vec<_>>(),
            vec!["09:30", "sunset -15m", ":15"]
        );
        assert_eq!(schedule.years, vec![2026, 2027]);
        assert_eq!(schedule.repeat_offsets().len(), 2);
        assert_eq!(schedule.allow.iter().count(), 1);
        assert_eq!(schedule.deny.iter().count(), 1);
        assert_eq!(schedule.except.iter().count(), 1);
    }

    #[test]
    fn negative_nth_weekday_builder_round_trips() {
        let spec = special_day_spec(SpecialDayKind::NthWeekday, 1, -2, Weekday::Mon);
        assert_eq!(spec.display(), "-2mon");
        assert_eq!(special_day_label("-2mon"), "2nd-last Monday");
        assert_eq!(DaySpec::parse("-2mon").unwrap(), spec);
        assert_eq!(special_day_label(&spec.display()), "2nd-last Monday");
    }

    #[test]
    fn date_and_time_use_the_schedule_timezone() {
        let mut editor = ScheduleEditor::new(
            ScheduleDefinition {
                name: "Test".to_string(),
                ..ScheduleDefinition::default()
            },
            true,
            Some("Europe/Berlin".to_string()),
        );
        editor.from_date = "2026-01-01".to_string();
        let date_only = editor.build().unwrap().from.unwrap();
        assert_eq!(
            date_only,
            ScheduleDate::Date(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap())
        );

        editor.from_time_enabled = true;
        editor.from_hour = 18;
        editor.from_minute = 30;
        editor.from_second = 0;
        let timed = editor.build().unwrap().from.unwrap();
        match timed {
            ScheduleDate::DateTime(value) => {
                assert_eq!(value.to_rfc3339(), "2026-01-01T18:30:00+01:00");
            }
            ScheduleDate::Date(_) => panic!("expected a timestamp"),
        }
    }

    #[test]
    fn default_timezone_and_location_are_not_stored() {
        let location = Location::new(52.52, 13.405, Some(34.0));
        let mut editor = ScheduleEditor::new(
            ScheduleDefinition {
                name: "Test".to_string(),
                timezone: Some("Europe/Berlin".to_string()),
                location: Some(location),
                at: OneOrMany::One(AtSpec::Text("09:00".to_string())),
                ..ScheduleDefinition::default()
            },
            false,
            None,
        );
        assert!(editor.timezone_custom);
        assert!(editor.location_custom);
        let schedule = editor.build().unwrap();
        assert_eq!(schedule.timezone.as_deref(), Some("Europe/Berlin"));
        assert_eq!(schedule.location, Some(location));

        editor.timezone_custom = false;
        editor.location_custom = false;
        let schedule = editor.build().unwrap();
        assert_eq!(schedule.timezone, None);
        assert_eq!(schedule.location, None);
    }

    #[test]
    fn custom_timezone_and_location_require_values() {
        let mut editor = ScheduleEditor::new(
            ScheduleDefinition {
                name: "Test".to_string(),
                ..ScheduleDefinition::default()
            },
            true,
            None,
        );
        editor.timezone_custom = true;
        let error = editor.build().unwrap_err();
        assert!(error.contains("Choose a timezone"), "{error}");

        editor.timezone = "Europe/Berlin".to_string();
        editor.location_custom = true;
        let error = editor.build().unwrap_err();
        assert!(error.contains("Set a location"), "{error}");
    }

    #[test]
    fn defaults_selection_requires_custom_values() {
        let mut editor = ScheduleDefaultsEditor::new(None, None);
        assert_eq!(editor.selections().unwrap(), (None, None));
        assert!(!editor.is_dirty());

        editor.timezone_custom = true;
        assert!(editor.is_dirty());
        let error = editor.selections().unwrap_err();
        assert!(error.contains("Choose a timezone"), "{error}");

        editor.timezone = "Europe/Berlin".to_string();
        editor.location_custom = true;
        let error = editor.selections().unwrap_err();
        assert!(error.contains("Set a location"), "{error}");

        editor.location = "52.52, 13.405, 34".to_string();
        let (location, timezone) = editor.selections().unwrap();
        assert_eq!(location.unwrap().latitude, 52.52);
        assert_eq!(timezone.as_deref(), Some("Europe/Berlin"));
        assert!(editor.is_dirty());
    }

    #[test]
    fn unchanged_defaults_are_not_dirty() {
        let editor = ScheduleDefaultsEditor::new(
            Some(Location::new(52.52, 13.405, Some(34.0))),
            Some("Europe/Berlin".to_string()),
        );
        assert_eq!(
            editor.selections().unwrap(),
            (
                Some(Location::new(52.52, 13.405, Some(34.0))),
                Some("Europe/Berlin".to_string())
            )
        );
        assert!(!editor.is_dirty());
    }
}
