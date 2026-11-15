use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;
use std::time::Duration as StdDuration;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind}: '{value}'")]
pub struct ParseError {
    pub kind: &'static str,
    pub value: String,
}

impl ParseError {
    fn new(kind: &'static str, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Size(pub u64);

impl Size {
    pub const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(self) -> u64 {
        self.0
    }
}

impl FromStr for Size {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ParseError::new("size", input));
        }

        let split = input
            .find(|character: char| !character.is_ascii_digit() && character != '.')
            .unwrap_or(input.len());
        let (number, unit) = input.split_at(split);
        let unit = unit.trim();
        if number.is_empty() {
            return Err(ParseError::new("size", input));
        }

        let value: f64 = number.parse().map_err(|_| ParseError::new("size", input))?;
        if !value.is_finite() || value < 0.0 {
            return Err(ParseError::new("size", input));
        }

        let multiplier = match unit.to_ascii_lowercase().replace(' ', "").as_str() {
            "" | "b" => 1_u64,
            "kb" => 1_000,
            "kib" => 1_024,
            "mb" => 1_000_000,
            "mib" => 1_048_576,
            "gb" => 1_000_000_000,
            "gib" => 1_073_741_824,
            "tb" => 1_000_000_000_000,
            "tib" => 1_099_511_627_776,
            _ => return Err(ParseError::new("size", input)),
        };

        let bytes = value * multiplier as f64;
        if bytes > u64::MAX as f64 {
            return Err(ParseError::new("size", input));
        }

        Ok(Self(bytes.round() as u64))
    }
}

impl fmt::Display for Size {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: &[(u64, &str)] = &[
            (1_099_511_627_776, "TiB"),
            (1_073_741_824, "GiB"),
            (1_048_576, "MiB"),
            (1_024, "KiB"),
            (1, "B"),
        ];

        for &(unit, suffix) in UNITS {
            if self.0 >= unit && self.0.is_multiple_of(unit) {
                return write!(formatter, "{} {}", self.0 / unit, suffix);
            }
        }

        write!(formatter, "{} B", self.0)
    }
}

impl Serialize for Size {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SizeVisitor;

        impl Visitor<'_> for SizeVisitor {
            type Value = Size;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a byte size string or an integer number of bytes")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                value.parse().map_err(E::custom)
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Size(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value < 0 {
                    return Err(E::custom("size cannot be negative"));
                }
                Ok(Size(value as u64))
            }
        }

        deserializer.deserialize_any(SizeVisitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct HumanDuration(pub StdDuration);

impl HumanDuration {
    pub const fn from_std(duration: StdDuration) -> Self {
        Self(duration)
    }

    pub const fn as_std(self) -> StdDuration {
        self.0
    }
}

impl FromStr for HumanDuration {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ParseError::new("duration", input));
        }

        let mut rest = input;
        let mut total_nanos: u128 = 0;
        let mut parsed_any = false;

        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }

            let number_end = rest
                .find(|character: char| !character.is_ascii_digit() && character != '.')
                .unwrap_or(rest.len());
            if number_end == 0 {
                return Err(ParseError::new("duration", input));
            }

            let value: f64 = rest[..number_end]
                .parse()
                .map_err(|_| ParseError::new("duration", input))?;
            if !value.is_finite() || value < 0.0 {
                return Err(ParseError::new("duration", input));
            }

            rest = rest[number_end..].trim_start();
            let unit_end = rest
                .find(|character: char| character.is_ascii_digit() || character == '.')
                .unwrap_or(rest.len());
            let unit = rest[..unit_end].trim();
            if unit.is_empty() {
                return Err(ParseError::new("duration", input));
            }

            let multiplier: u128 = match unit.to_ascii_lowercase().as_str() {
                "ns" | "nsec" | "nsecs" | "nanosecond" | "nanoseconds" => 1,
                "us" | "usec" | "usecs" | "microsecond" | "microseconds" => 1_000,
                "ms" | "msec" | "msecs" | "millisecond" | "milliseconds" => 1_000_000,
                "s" | "sec" | "secs" | "second" | "seconds" => 1_000_000_000,
                "m" | "min" | "mins" | "minute" | "minutes" => 60_000_000_000,
                "h" | "hr" | "hrs" | "hour" | "hours" => 3_600_000_000_000,
                "d" | "day" | "days" => 86_400_000_000_000,
                "w" | "week" | "weeks" => 604_800_000_000_000,
                _ => return Err(ParseError::new("duration", input)),
            };

            let segment = value * multiplier as f64;
            if segment > u128::MAX as f64 {
                return Err(ParseError::new("duration", input));
            }
            total_nanos = total_nanos
                .checked_add(segment.round() as u128)
                .ok_or_else(|| ParseError::new("duration", input))?;

            rest = &rest[unit_end..];
            parsed_any = true;
        }

        if !parsed_any || total_nanos > u64::MAX as u128 {
            return Err(ParseError::new("duration", input));
        }

        Ok(Self(StdDuration::from_nanos(total_nanos as u64)))
    }
}

impl fmt::Display for HumanDuration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nanos = self.0.as_nanos();
        if nanos == 0 {
            return write!(formatter, "0 s");
        }

        const UNITS: &[(u128, &str)] = &[
            (604_800_000_000_000, "w"),
            (86_400_000_000_000, "d"),
            (3_600_000_000_000, "h"),
            (60_000_000_000, "m"),
            (1_000_000_000, "s"),
            (1_000_000, "ms"),
            (1_000, "us"),
            (1, "ns"),
        ];

        for &(unit, suffix) in UNITS {
            if nanos >= unit && nanos.is_multiple_of(unit) {
                return write!(formatter, "{} {}", nanos / unit, suffix);
            }
        }

        write!(formatter, "{} ns", nanos)
    }
}

impl Serialize for HumanDuration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for HumanDuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct DurationVisitor;

        impl Visitor<'_> for DurationVisitor {
            type Value = HumanDuration;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a duration string or an integer number of seconds")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                value.parse().map_err(E::custom)
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(HumanDuration(StdDuration::from_secs(value)))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value < 0 {
                    return Err(E::custom("duration cannot be negative"));
                }
                Ok(HumanDuration(StdDuration::from_secs(value as u64)))
            }
        }

        deserializer.deserialize_any(DurationVisitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileMode(pub u32);

impl FileMode {
    pub const fn new(mode: u32) -> Self {
        Self(mode)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl Default for FileMode {
    fn default() -> Self {
        Self(0o644)
    }
}

impl FromStr for FileMode {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        let digits = input
            .strip_prefix("0o")
            .or_else(|| input.strip_prefix("0O"))
            .unwrap_or(input);
        if digits.is_empty() {
            return Err(ParseError::new("file mode", input));
        }

        u32::from_str_radix(digits, 8)
            .map(Self)
            .map_err(|_| ParseError::new("file mode", input))
    }
}

impl fmt::Display for FileMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0{:o}", self.0)
    }
}

impl Serialize for FileMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for FileMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FileModeVisitor;

        impl Visitor<'_> for FileModeVisitor {
            type Value = FileMode;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an octal file mode string or an integer mode")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                value.parse().map_err(E::custom)
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                u32::try_from(value)
                    .map(FileMode)
                    .map_err(|_| E::custom("file mode is too large"))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value < 0 {
                    return Err(E::custom("file mode cannot be negative"));
                }
                u32::try_from(value)
                    .map(FileMode)
                    .map_err(|_| E::custom("file mode is too large"))
            }
        }

        deserializer.deserialize_any(FileModeVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!("100 MiB".parse::<Size>().unwrap().as_bytes(), 104_857_600);
        assert_eq!("10MiB".parse::<Size>().unwrap().as_bytes(), 10_485_760);
        assert_eq!("1.5 KiB".parse::<Size>().unwrap().as_bytes(), 1_536);
        assert_eq!("0".parse::<Size>().unwrap().as_bytes(), 0);
        assert_eq!("1 KB".parse::<Size>().unwrap().as_bytes(), 1_000);
        assert!("10 XB".parse::<Size>().is_err());
        assert!("".parse::<Size>().is_err());
    }

    #[test]
    fn displays_sizes() {
        assert_eq!(Size::from_bytes(10_485_760).to_string(), "10 MiB");
        assert_eq!(Size::from_bytes(1_024).to_string(), "1 KiB");
        assert_eq!(Size::from_bytes(1_000).to_string(), "1000 B");
        assert_eq!(Size::from_bytes(0).to_string(), "0 B");
    }

    #[test]
    fn parses_durations() {
        assert_eq!(
            "6 h".parse::<HumanDuration>().unwrap().as_std(),
            StdDuration::from_secs(6 * 3_600)
        );
        assert_eq!(
            "1h 30m".parse::<HumanDuration>().unwrap().as_std(),
            StdDuration::from_secs(90 * 60)
        );
        assert_eq!(
            "1.5s".parse::<HumanDuration>().unwrap().as_std(),
            StdDuration::from_millis(1_500)
        );
        assert_eq!(
            "1 d".parse::<HumanDuration>().unwrap().as_std(),
            StdDuration::from_secs(86_400)
        );
        assert!("6".parse::<HumanDuration>().is_err());
        assert!("6x".parse::<HumanDuration>().is_err());
        assert!("".parse::<HumanDuration>().is_err());
    }

    #[test]
    fn displays_durations() {
        assert_eq!(
            HumanDuration::from_std(StdDuration::from_secs(6 * 3_600)).to_string(),
            "6 h"
        );
        assert_eq!(
            HumanDuration::from_std(StdDuration::from_secs(90)).to_string(),
            "90 s"
        );
        assert_eq!(
            HumanDuration::from_std(StdDuration::from_millis(1_500)).to_string(),
            "1500 ms"
        );
        assert_eq!(HumanDuration::default().to_string(), "0 s");
    }

    #[test]
    fn parses_and_displays_file_modes() {
        assert_eq!("0644".parse::<FileMode>().unwrap().as_u32(), 0o644);
        assert_eq!("755".parse::<FileMode>().unwrap().as_u32(), 0o755);
        assert_eq!("0o600".parse::<FileMode>().unwrap().as_u32(), 0o600);
        assert_eq!(FileMode::new(0o644).to_string(), "0644");
        assert!("08".parse::<FileMode>().is_err());
    }

    #[test]
    fn serializes_sizes_and_durations() {
        let size = Size::from_bytes(10_485_760);
        let json = serde_json::to_string(&size).unwrap();
        assert_eq!(json, "\"10 MiB\"");
        assert_eq!(serde_json::from_str::<Size>("\"10 MiB\"").unwrap(), size);
        assert_eq!(serde_json::from_str::<Size>("10485760").unwrap(), size);

        let duration = HumanDuration::from_std(StdDuration::from_secs(6 * 3_600));
        let json = serde_json::to_string(&duration).unwrap();
        assert_eq!(json, "\"6 h\"");
        assert_eq!(
            serde_json::from_str::<HumanDuration>("\"6 h\"").unwrap(),
            duration
        );
        assert_eq!(
            serde_json::from_str::<HumanDuration>("21600").unwrap(),
            duration
        );
    }

    #[test]
    fn serializes_file_modes() {
        let mode = FileMode::new(0o644);
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"0644\"");
        assert_eq!(serde_json::from_str::<FileMode>("\"0644\"").unwrap(), mode);
        assert_eq!(serde_json::from_str::<FileMode>("420").unwrap(), mode);
    }
}
