//! Schedule catalog and recurrence DTOs.
//!
//! A schedule is a single recurrence rule plus optional repeat chain,
//! allow/deny windows, exceptions, and jitter. A task trigger references a
//! schedule by ID; the scheduler computes occurrences in the server process.

use crate::domain::task::OneOrMany;
use crate::utils::HumanDuration;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    pub elevation: Option<f64>,
}

impl Location {
    pub fn new(latitude: f64, longitude: f64, elevation: Option<f64>) -> Self {
        Self {
            latitude,
            longitude,
            elevation,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.latitude.is_finite()
            && self.longitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && (-180.0..=180.0).contains(&self.longitude)
            && self.elevation.is_none_or(|elevation| {
                elevation.is_finite() && (-500.0..=10_000.0).contains(&elevation)
            })
    }
}

impl Serialize for Location {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeTuple;
        let mut tuple = serializer.serialize_tuple(if self.elevation.is_some() { 3 } else { 2 })?;
        tuple.serialize_element(&self.latitude)?;
        tuple.serialize_element(&self.longitude)?;
        if let Some(elevation) = self.elevation {
            tuple.serialize_element(&elevation)?;
        }
        tuple.end()
    }
}

impl<'de> Deserialize<'de> for Location {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LocationVisitor;

        impl<'de> Visitor<'de> for LocationVisitor {
            type Value = Location;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("[latitude, longitude] or [latitude, longitude, elevation]")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let latitude: f64 = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let longitude: f64 = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(1, &self))?;
                let elevation: Option<f64> = sequence.next_element()?;
                if sequence.next_element::<serde_json::Value>()?.is_some() {
                    return Err(de::Error::invalid_length(4, &self));
                }
                Ok(Location {
                    latitude,
                    longitude,
                    elevation,
                })
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut latitude = None;
                let mut longitude = None;
                let mut elevation = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "latitude" | "lat" => latitude = Some(map.next_value()?),
                        "longitude" | "lon" | "lng" => longitude = Some(map.next_value()?),
                        "elevation" => elevation = Some(map.next_value()?),
                        other => {
                            return Err(de::Error::unknown_field(
                                other,
                                &["latitude", "longitude", "elevation"],
                            ));
                        }
                    }
                }
                Ok(Location {
                    latitude: latitude.ok_or_else(|| de::Error::missing_field("latitude"))?,
                    longitude: longitude.ok_or_else(|| de::Error::missing_field("longitude"))?,
                    elevation,
                })
            }
        }

        deserializer.deserialize_any(LocationVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ScheduleDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<ScheduleDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<ScheduleDate>,
    #[serde(default, skip_serializing_if = "OneOrMany::is_empty")]
    pub at: OneOrMany<AtSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<ScheduleEvery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<DaySpec>,
    #[serde(default, skip_serializing_if = "is_any_day_match")]
    pub day_match: DayMatch,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub months: Vec<Month>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub years: Vec<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jitter: Option<HumanDuration>,
    /// Offsets from each base occurrence. Each entry creates one extra run.
    #[serde(default, skip_serializing_if = "OneOrMany::is_empty")]
    pub repeat_at: OneOrMany<HumanDuration>,
    #[serde(default, skip_serializing_if = "OneOrMany::is_empty")]
    pub allow: OneOrMany<TimeWindow>,
    #[serde(default, skip_serializing_if = "OneOrMany::is_empty")]
    pub deny: OneOrMany<TimeWindow>,
    #[serde(default, skip_serializing_if = "OneOrMany::is_empty")]
    pub except: OneOrMany<DateRange>,
}

impl Default for ScheduleDefinition {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            timezone: None,
            location: None,
            from: None,
            until: None,
            at: OneOrMany::Many(Vec::new()),
            every: None,
            days: Vec::new(),
            day_match: DayMatch::Any,
            months: Vec::new(),
            years: Vec::new(),
            jitter: None,
            repeat_at: OneOrMany::Many(Vec::new()),
            allow: OneOrMany::Many(Vec::new()),
            deny: OneOrMany::Many(Vec::new()),
            except: OneOrMany::Many(Vec::new()),
        }
    }
}

impl ScheduleDefinition {
    /// Repeat offsets in ascending order, ignoring zero-length values.
    pub fn repeat_offsets(&self) -> Vec<std::time::Duration> {
        let mut offsets: Vec<_> = self
            .repeat_at
            .iter()
            .map(|duration| duration.as_std())
            .filter(|duration| !duration.is_zero())
            .collect();
        offsets.sort_unstable();
        offsets.dedup();
        offsets
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleDate {
    Date(NaiveDate),
    DateTime(DateTime<FixedOffset>),
}

impl ScheduleDate {
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
            return Ok(Self::Date(date));
        }
        DateTime::parse_from_rfc3339(value)
            .map(Self::DateTime)
            .map_err(|error| format!("invalid date or timestamp '{value}': {error}"))
    }

    pub fn as_date(self) -> NaiveDate {
        match self {
            Self::Date(date) => date,
            Self::DateTime(date) => date.date_naive(),
        }
    }
}

impl fmt::Display for ScheduleDate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Date(date) => write!(formatter, "{date}"),
            Self::DateTime(date) => write!(formatter, "{}", date.to_rfc3339()),
        }
    }
}

impl Serialize for ScheduleDate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ScheduleDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// `every` interval. Duration intervals (seconds, minutes, hours, or any
/// longer duration expressed with those units) use elapsed time; day, week,
/// month, and year units are anchored at the local `from` date so DST does not
/// shift the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleEvery {
    Duration(HumanDuration),
    Days(u32),
    Weeks(u32),
    Months(u32),
    Years(u32),
}

impl ScheduleEvery {
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        let lower = value.to_ascii_lowercase();
        for (suffix, kind) in [
            ("mo", 3_u8),
            ("month", 3),
            ("months", 3),
            ("y", 4_u8),
            ("year", 4),
            ("years", 4),
            ("w", 2_u8),
            ("week", 2),
            ("weeks", 2),
            ("d", 1_u8),
            ("day", 1),
            ("days", 1),
        ] {
            if let Some(number) = lower.strip_suffix(suffix)
                && let Ok(number) = number.trim().parse::<u32>()
                && number > 0
            {
                return Ok(match kind {
                    1 => Self::Days(number),
                    2 => Self::Weeks(number),
                    3 => Self::Months(number),
                    4 => Self::Years(number),
                    _ => unreachable!(),
                });
            }
        }
        let duration: HumanDuration = value
            .parse()
            .map_err(|error| format!("invalid every interval '{value}': {error}"))?;
        if duration.as_std().is_zero() {
            return Err("every interval must be greater than 0".to_string());
        }
        Ok(Self::Duration(duration))
    }

    /// True when the interval is continuous elapsed time instead of a calendar
    /// unit. Any [`Self::Duration`] qualifies, including values of 24 hours or
    /// more expressed with `s`, `m`, or `h`.
    pub fn uses_elapsed_time(self) -> bool {
        matches!(self, Self::Duration(_))
    }

    pub fn duration(self) -> Option<std::time::Duration> {
        match self {
            Self::Duration(duration) => Some(duration.as_std()),
            Self::Days(days) => Some(std::time::Duration::from_secs(u64::from(days) * 86_400)),
            Self::Weeks(weeks) => Some(std::time::Duration::from_secs(
                u64::from(weeks) * 7 * 86_400,
            )),
            Self::Months(_) | Self::Years(_) => None,
        }
    }
}

impl Serialize for ScheduleEvery {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = match self {
            Self::Duration(duration) => duration.to_string(),
            Self::Days(days) => format!("{days}d"),
            Self::Weeks(weeks) => format!("{weeks}w"),
            Self::Months(months) => format!("{months}mo"),
            Self::Years(years) => format!("{years}y"),
        };
        serializer.serialize_str(&value)
    }
}

impl<'de> Deserialize<'de> for ScheduleEvery {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DayMatch {
    #[default]
    Any,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AtSpec {
    Text(String),
    Structured(AtStructured),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct AtStructured {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solar: Option<SolarEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SolarEvent {
    Sunrise,
    Sunset,
    Dawn,
    Dusk,
    Noon,
    Nadir,
}

impl SolarEvent {
    pub const ALL: [Self; 6] = [
        Self::Sunrise,
        Self::Sunset,
        Self::Dawn,
        Self::Dusk,
        Self::Noon,
        Self::Nadir,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Sunrise => "Sunrise",
            Self::Sunset => "Sunset",
            Self::Dawn => "Dawn (civil)",
            Self::Dusk => "Dusk (civil)",
            Self::Noon => "Solar noon",
            Self::Nadir => "Solar nadir",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sunrise => "sunrise",
            Self::Sunset => "sunset",
            Self::Dawn => "dawn",
            Self::Dusk => "dusk",
            Self::Noon => "noon",
            Self::Nadir => "nadir",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|event| event.as_str().eq_ignore_ascii_case(value.trim()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedAt {
    Time(NaiveTime),
    HourlyMinute(u8),
    Solar {
        event: SolarEvent,
        offset_seconds: i64,
    },
}

impl AtSpec {
    pub fn parse(&self) -> Result<ParsedAt, String> {
        match self {
            Self::Text(text) => parse_at_text(text),
            Self::Structured(value) => {
                if let Some(solar) = value.solar {
                    let offset_seconds = value
                        .offset
                        .as_deref()
                        .map(parse_signed_duration)
                        .transpose()?
                        .unwrap_or(0);
                    return Ok(ParsedAt::Solar {
                        event: solar,
                        offset_seconds,
                    });
                }
                if let Some(time) = &value.time {
                    return parse_time_text(time);
                }
                Err("an at entry must contain a time or a solar event".to_string())
            }
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Structured(value) => {
                if let Some(solar) = value.solar {
                    match &value.offset {
                        Some(offset) => format!(
                            "{} {}{}",
                            solar.as_str(),
                            if offset.starts_with('-') || offset.starts_with('+') {
                                ""
                            } else {
                                "+"
                            },
                            offset
                        ),
                        None => solar.as_str().to_string(),
                    }
                } else {
                    value.time.clone().unwrap_or_default()
                }
            }
        }
    }
}

fn parse_at_text(text: &str) -> Result<ParsedAt, String> {
    let text = text.trim();
    if let Some(stripped) = text.strip_prefix(':') {
        let minute: u8 = stripped
            .parse()
            .map_err(|_| format!("invalid hourly minute '{text}'"))?;
        if minute > 59 {
            return Err(format!("invalid hourly minute '{text}'"));
        }
        return Ok(ParsedAt::HourlyMinute(minute));
    }
    if let Ok(time) = NaiveTime::parse_from_str(text, "%H:%M") {
        return Ok(ParsedAt::Time(time));
    }
    if let Ok(time) = NaiveTime::parse_from_str(text, "%H:%M:%S") {
        return Ok(ParsedAt::Time(time));
    }

    let lower = text.to_ascii_lowercase();
    for event in SolarEvent::ALL {
        let name = event.as_str();
        if lower == name {
            return Ok(ParsedAt::Solar {
                event,
                offset_seconds: 0,
            });
        }
        if let Some(rest) = lower.strip_prefix(name) {
            let rest = rest.trim();
            let offset_seconds = parse_signed_duration(rest)?;
            return Ok(ParsedAt::Solar {
                event,
                offset_seconds,
            });
        }
    }
    Err(format!("invalid at entry '{text}'"))
}

fn parse_time_text(text: &str) -> Result<ParsedAt, String> {
    parse_at_text(text)
}

/// Parses `+15m`, `-1h 30m`, or `15m` into seconds.
pub fn parse_signed_duration(input: &str) -> Result<i64, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(0);
    }
    let (sign, rest) = match input.as_bytes()[0] {
        b'+' => (1_i64, &input[1..]),
        b'-' => (-1_i64, &input[1..]),
        _ => (1_i64, input),
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return Ok(0);
    }
    let value: HumanDuration = rest
        .parse()
        .map_err(|error| format!("invalid offset '{input}': {error}"))?;
    let seconds = value.as_std().as_secs();
    i64::try_from(seconds)
        .map(|seconds| seconds * sign)
        .map_err(|_| format!("offset '{input}' is too large"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaySpec {
    Weekday(chrono::Weekday),
    MonthDay(u8),
    LastDay,
    LastWeekday,
    NearestWeekday(u8),
    NthWeekday { nth: i8, weekday: chrono::Weekday },
}

impl DaySpec {
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        let lower = value.to_ascii_lowercase();
        if lower == "last" {
            return Ok(Self::LastDay);
        }
        if lower == "lw" || lower == "lastweekday" {
            return Ok(Self::LastWeekday);
        }
        if let Some(day) = lower.strip_suffix('w')
            && let Ok(day) = day.parse::<u8>()
            && (1..=31).contains(&day)
        {
            return Ok(Self::NearestWeekday(day));
        }
        if let Some(weekday) = parse_weekday(&lower) {
            return Ok(Self::Weekday(weekday));
        }
        if let Ok(day) = lower.parse::<u8>()
            && (1..=31).contains(&day)
        {
            return Ok(Self::MonthDay(day));
        }
        // nth weekday: "1mon", "-1fri", "last mon"
        let compact = lower.replace(' ', "");
        let (nth_text, weekday_text) = if let Some(rest) = compact.strip_prefix("last") {
            ("-1", rest)
        } else {
            let split = compact
                .find(|character: char| character.is_alphabetic())
                .unwrap_or(compact.len());
            compact.split_at(split)
        };
        if let Ok(nth) = nth_text.parse::<i8>()
            && (1..=5).contains(&nth.unsigned_abs())
            && let Some(weekday) = parse_weekday(weekday_text)
        {
            return Ok(Self::NthWeekday { nth, weekday });
        }
        Err(format!("invalid day entry '{value}'"))
    }

    pub fn display(&self) -> String {
        match self {
            Self::Weekday(weekday) => weekday_label(*weekday).to_string(),
            Self::MonthDay(day) => day.to_string(),
            Self::LastDay => "last".to_string(),
            Self::LastWeekday => "lw".to_string(),
            Self::NearestWeekday(day) => format!("{day}w"),
            Self::NthWeekday { nth, weekday } => format!("{nth}{}", weekday_label(*weekday)),
        }
    }
}

impl Serialize for DaySpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.display())
    }
}

impl<'de> Deserialize<'de> for DaySpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = StringOrNumber::deserialize(deserializer)?;
        Self::parse(&value.0).map_err(de::Error::custom)
    }
}

fn parse_weekday(value: &str) -> Option<chrono::Weekday> {
    use chrono::Weekday;
    match value {
        "mon" | "monday" => Some(Weekday::Mon),
        "tue" | "tues" | "tuesday" => Some(Weekday::Tue),
        "wed" | "wednesday" => Some(Weekday::Wed),
        "thu" | "thur" | "thurs" | "thursday" => Some(Weekday::Thu),
        "fri" | "friday" => Some(Weekday::Fri),
        "sat" | "saturday" => Some(Weekday::Sat),
        "sun" | "sunday" => Some(Weekday::Sun),
        _ => None,
    }
}

fn weekday_label(weekday: chrono::Weekday) -> &'static str {
    use chrono::Weekday;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Month {
    #[serde(alias = "1")]
    Jan,
    #[serde(alias = "2")]
    Feb,
    #[serde(alias = "3")]
    Mar,
    #[serde(alias = "4")]
    Apr,
    #[serde(alias = "5")]
    May,
    #[serde(alias = "6")]
    Jun,
    #[serde(alias = "7")]
    Jul,
    #[serde(alias = "8")]
    Aug,
    #[serde(alias = "9")]
    Sep,
    #[serde(alias = "10")]
    Oct,
    #[serde(alias = "11")]
    Nov,
    #[serde(alias = "12")]
    Dec,
}

impl Month {
    pub const ALL: [Self; 12] = [
        Self::Jan,
        Self::Feb,
        Self::Mar,
        Self::Apr,
        Self::May,
        Self::Jun,
        Self::Jul,
        Self::Aug,
        Self::Sep,
        Self::Oct,
        Self::Nov,
        Self::Dec,
    ];

    pub const fn number(self) -> u32 {
        match self {
            Self::Jan => 1,
            Self::Feb => 2,
            Self::Mar => 3,
            Self::Apr => 4,
            Self::May => 5,
            Self::Jun => 6,
            Self::Jul => 7,
            Self::Aug => 8,
            Self::Sep => 9,
            Self::Oct => 10,
            Self::Nov => 11,
            Self::Dec => 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeWindow {
    pub from: NaiveTime,
    pub to: NaiveTime,
}

impl TimeWindow {
    pub fn parse(value: &str) -> Result<Self, String> {
        let (from, to) = value
            .split_once('~')
            .or_else(|| value.split_once('-'))
            .ok_or_else(|| format!("time window '{value}' must look like '16:00~22:00'"))?;
        Ok(Self {
            from: parse_time(from.trim())?,
            to: parse_time(to.trim())?,
        })
    }

    pub fn contains(&self, time: NaiveTime) -> bool {
        if self.from <= self.to {
            time >= self.from && time <= self.to
        } else {
            time >= self.from || time <= self.to
        }
    }
}

fn parse_time(value: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(value, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(value, "%H:%M:%S"))
        .map_err(|error| format!("invalid time '{value}': {error}"))
}

impl Serialize for TimeWindow {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!(
            "{}~{}",
            self.from.format("%H:%M"),
            self.to.format("%H:%M")
        ))
    }
}

impl<'de> Deserialize<'de> for TimeWindow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct WindowVisitor;

        impl<'de> Visitor<'de> for WindowVisitor {
            type Value = TimeWindow;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a time window like '16:00~22:00' or {from, to}")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                TimeWindow::parse(value).map_err(E::custom)
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut from = None;
                let mut to = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "from" => from = Some(map.next_value::<String>()?),
                        "to" => to = Some(map.next_value::<String>()?),
                        other => return Err(de::Error::unknown_field(other, &["from", "to"])),
                    }
                }
                Ok(TimeWindow {
                    from: parse_time(&from.ok_or_else(|| de::Error::missing_field("from"))?)
                        .map_err(de::Error::custom)?,
                    to: parse_time(&to.ok_or_else(|| de::Error::missing_field("to"))?)
                        .map_err(de::Error::custom)?,
                })
            }
        }

        deserializer.deserialize_any(WindowVisitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

impl DateRange {
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        let (from, to) = if let Some((from, to)) = value.split_once("..") {
            (from.trim(), to.trim())
        } else {
            (value, value)
        };
        Ok(Self {
            from: NaiveDate::parse_from_str(from, "%Y-%m-%d")
                .map_err(|error| format!("invalid date '{from}': {error}"))?,
            to: NaiveDate::parse_from_str(to, "%Y-%m-%d")
                .map_err(|error| format!("invalid date '{to}': {error}"))?,
        })
    }

    pub fn contains(&self, date: NaiveDate) -> bool {
        date >= self.from && date <= self.to
    }
}

impl Serialize for DateRange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.from == self.to {
            serializer.serialize_str(&self.from.to_string())
        } else {
            serializer.serialize_str(&format!("{}..{}", self.from, self.to))
        }
    }
}

impl<'de> Deserialize<'de> for DateRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RangeVisitor;

        impl<'de> Visitor<'de> for RangeVisitor {
            type Value = DateRange;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a date, 'from..to', or {from, to}")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                DateRange::parse(value).map_err(E::custom)
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut from = None;
                let mut to = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "from" => from = Some(map.next_value::<String>()?),
                        "to" => to = Some(map.next_value::<String>()?),
                        other => return Err(de::Error::unknown_field(other, &["from", "to"])),
                    }
                }
                let from = from.ok_or_else(|| de::Error::missing_field("from"))?;
                let to = to.ok_or_else(|| de::Error::missing_field("to"))?;
                Ok(DateRange {
                    from: NaiveDate::parse_from_str(&from, "%Y-%m-%d")
                        .map_err(de::Error::custom)?,
                    to: NaiveDate::parse_from_str(&to, "%Y-%m-%d").map_err(de::Error::custom)?,
                })
            }
        }

        deserializer.deserialize_any(RangeVisitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OccurrenceKind {
    Scheduled,
    Repeat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScheduleOccurrence {
    pub at: DateTime<chrono::Utc>,
    pub local: String,
    pub kind: OccurrenceKind,
    /// One-based index of the repeat after its base occurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_index: Option<u32>,
    /// Only present when the candidate was filtered out; omitted for normal
    /// executable occurrences and for `schedules.list` / `schedules.get`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub skipped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_any_day_match(value: &DayMatch) -> bool {
    *value == DayMatch::Any
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScheduleInfo {
    pub schedule: ScheduleDefinition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<ScheduleOccurrence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct StringOrNumber(String);

impl<'de> Deserialize<'de> for StringOrNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ValueVisitor;

        impl<'de> Visitor<'de> for ValueVisitor {
            type Value = StringOrNumber;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a string or integer")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StringOrNumber(value.to_string()))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StringOrNumber(value.to_string()))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StringOrNumber(value.to_string()))
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omits_false_skipped_from_occurrences() {
        let at = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let normal = ScheduleOccurrence {
            at,
            local: "2026-01-01 09:00:00 UTC".to_string(),
            kind: OccurrenceKind::Scheduled,
            repeat_index: None,
            skipped: false,
            reason: None,
        };
        let value = serde_json::to_value(&normal).unwrap();
        assert!(value.get("skipped").is_none());
        assert!(value.get("reason").is_none());
        assert!(value.get("repeatIndex").is_none());

        let filtered = ScheduleOccurrence {
            skipped: true,
            reason: Some("denied by time window".to_string()),
            ..normal
        };
        let value = serde_json::to_value(&filtered).unwrap();
        assert_eq!(value["skipped"], true);
        assert_eq!(value["reason"], "denied by time window");
    }

    #[test]
    fn parses_location_arrays_and_objects() {
        let location: Location = serde_yaml_ng::from_str("[52.52, 13.405, 34]").unwrap();
        assert_eq!(location, Location::new(52.52, 13.405, Some(34.0)));
        let location: Location = serde_yaml_ng::from_str("[52.52, 13.405]").unwrap();
        assert_eq!(location, Location::new(52.52, 13.405, None));
        let location: Location =
            serde_yaml_ng::from_str("{latitude: 1, longitude: 2, elevation: 3}").unwrap();
        assert_eq!(location, Location::new(1.0, 2.0, Some(3.0)));
    }

    #[test]
    fn parses_at_entries() {
        for (text, expected) in [
            (
                "09:30",
                ParsedAt::Time(NaiveTime::from_hms_opt(9, 30, 0).unwrap()),
            ),
            (":15", ParsedAt::HourlyMinute(15)),
            (
                "sunset - 15m",
                ParsedAt::Solar {
                    event: SolarEvent::Sunset,
                    offset_seconds: -900,
                },
            ),
            (
                "sunrise+5m",
                ParsedAt::Solar {
                    event: SolarEvent::Sunrise,
                    offset_seconds: 300,
                },
            ),
        ] {
            assert_eq!(AtSpec::Text(text.to_string()).parse().unwrap(), expected);
        }
    }

    #[test]
    fn parses_days() {
        use chrono::Weekday;
        assert_eq!(
            DaySpec::parse("mon").unwrap(),
            DaySpec::Weekday(Weekday::Mon)
        );
        assert_eq!(DaySpec::parse("15").unwrap(), DaySpec::MonthDay(15));
        assert_eq!(DaySpec::parse("last").unwrap(), DaySpec::LastDay);
        assert_eq!(DaySpec::parse("lw").unwrap(), DaySpec::LastWeekday);
        assert_eq!(DaySpec::parse("15w").unwrap(), DaySpec::NearestWeekday(15));
        assert_eq!(
            DaySpec::parse("2fri").unwrap(),
            DaySpec::NthWeekday {
                nth: 2,
                weekday: Weekday::Fri
            }
        );
        assert_eq!(
            DaySpec::parse("-1fri").unwrap(),
            DaySpec::NthWeekday {
                nth: -1,
                weekday: Weekday::Fri
            }
        );
        assert_eq!(
            DaySpec::parse("last monday").unwrap(),
            DaySpec::NthWeekday {
                nth: -1,
                weekday: Weekday::Mon
            }
        );
        assert_eq!(
            DaySpec::parse("-2mon").unwrap(),
            DaySpec::NthWeekday {
                nth: -2,
                weekday: Weekday::Mon
            }
        );
    }

    #[test]
    fn parses_windows_and_ranges() {
        let window = TimeWindow::parse("22:00~06:00").unwrap();
        assert!(window.contains(NaiveTime::from_hms_opt(23, 0, 0).unwrap()));
        assert!(window.contains(NaiveTime::from_hms_opt(5, 0, 0).unwrap()));
        assert!(!window.contains(NaiveTime::from_hms_opt(12, 0, 0).unwrap()));

        let range = DateRange::parse("2026-12-24..2026-12-26").unwrap();
        assert!(range.contains(NaiveDate::from_ymd_opt(2026, 12, 25).unwrap()));
    }

    #[test]
    fn parses_calendar_intervals() {
        assert_eq!(ScheduleEvery::parse("3d").unwrap(), ScheduleEvery::Days(3));
        assert_eq!(ScheduleEvery::parse("2w").unwrap(), ScheduleEvery::Weeks(2));
        assert_eq!(
            ScheduleEvery::parse("1mo").unwrap(),
            ScheduleEvery::Months(1)
        );
        assert_eq!(ScheduleEvery::parse("2y").unwrap(), ScheduleEvery::Years(2));
        assert!(ScheduleEvery::parse("30m").unwrap().uses_elapsed_time());
        assert!(ScheduleEvery::parse("30h").unwrap().uses_elapsed_time());
        assert!(ScheduleEvery::parse("1800m").unwrap().uses_elapsed_time());
        assert!(!ScheduleEvery::parse("1d").unwrap().uses_elapsed_time());
    }

    #[test]
    fn parses_schedule_yaml() {
        let yaml = r#"
location: [52.52, 13.405, 34]
timezone: Europe/Berlin
schedules:
  - id: before-sunset
    name: Before sunset
    timezone: Europe/Berlin
    from: 2026-01-01
    until: 2026-12-31
    at: "sunset - 15m"
    days: [mon, tue, wed, thu, fri]
    repeatAt: [30m, 1h]
"#;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct File {
            location: Option<Location>,
            timezone: Option<String>,
            schedules: Vec<ScheduleDefinition>,
        }
        let file: File = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(file.location.unwrap().elevation, Some(34.0));
        assert_eq!(file.timezone.as_deref(), Some("Europe/Berlin"));
        assert_eq!(file.schedules[0].id, "before-sunset");
        let offsets: Vec<_> = file.schedules[0]
            .repeat_at
            .iter()
            .map(|offset| offset.as_std())
            .collect();
        assert_eq!(
            offsets,
            vec![
                std::time::Duration::from_secs(30 * 60),
                std::time::Duration::from_secs(60 * 60)
            ]
        );
    }
}
