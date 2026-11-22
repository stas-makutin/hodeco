use crate::utils::{Base64Bytes, FileMode, HumanDuration, Size};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

pub type ParamValues = BTreeMap<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParamType {
    Bool,
    Int,
    Uint,
    String,
    Bytes,
    Size,
    Duration,
    FileMode,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ParamDef {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ParamType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(rename = "const", default, skip_serializing_if = "is_false")]
    pub is_const: bool,
    #[serde(rename = "enum", default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParamError {
    #[error("the parameter '{name}' is unknown")]
    Unknown { name: String },
    #[error("the required parameter '{name}' is missing")]
    Missing { name: String },
    #[error("the parameter '{name}' has an invalid value '{value}'")]
    Invalid { name: String, value: Value },
}

impl ParamDef {
    pub fn with_default(
        name: &str,
        kind: ParamType,
        default: impl Into<Value>,
        description: &str,
    ) -> Self {
        Self {
            name: name.to_string(),
            kind,
            default: Some(default.into()),
            required: false,
            is_const: false,
            enum_values: Vec::new(),
            min: None,
            max: None,
            description: Some(description.to_string()),
        }
    }

    pub fn validate_value(&self, value: &Value) -> Result<Value, ParamError> {
        let normalized = normalize(self.kind, value).ok_or_else(|| self.invalid(value))?;

        if !self.enum_values.is_empty() && !self.enum_values.contains(&normalized) {
            return Err(self.invalid(&normalized));
        }

        if let Some(min) = &self.min {
            let candidate =
                numeric_value(self.kind, &normalized).ok_or_else(|| self.invalid(&normalized))?;
            let bound = numeric_value(self.kind, min).ok_or_else(|| self.invalid(min))?;
            if candidate < bound {
                return Err(self.invalid(&normalized));
            }
        }
        if let Some(max) = &self.max {
            let candidate =
                numeric_value(self.kind, &normalized).ok_or_else(|| self.invalid(&normalized))?;
            let bound = numeric_value(self.kind, max).ok_or_else(|| self.invalid(max))?;
            if candidate > bound {
                return Err(self.invalid(&normalized));
            }
        }

        Ok(normalized)
    }

    fn invalid(&self, value: &Value) -> ParamError {
        ParamError::Invalid {
            name: self.name.clone(),
            value: value.clone(),
        }
    }
}

pub fn parse_param_values(
    defs: &[ParamDef],
    values: &ParamValues,
) -> Result<ParamValues, ParamError> {
    for name in values.keys() {
        if !defs.iter().any(|definition| &definition.name == name) {
            return Err(ParamError::Unknown { name: name.clone() });
        }
    }

    let mut result = ParamValues::new();
    for definition in defs {
        let input = if definition.is_const {
            definition
                .default
                .clone()
                .ok_or_else(|| ParamError::Missing {
                    name: definition.name.clone(),
                })?
        } else if let Some(value) = values.get(&definition.name) {
            value.clone()
        } else if let Some(default) = &definition.default {
            default.clone()
        } else if definition.required {
            return Err(ParamError::Missing {
                name: definition.name.clone(),
            });
        } else {
            continue;
        };

        result.insert(definition.name.clone(), definition.validate_value(&input)?);
    }

    Ok(result)
}

pub fn param_u64(values: &ParamValues, name: &str) -> Option<u64> {
    values.get(name).and_then(Value::as_u64)
}

pub fn param_str<'a>(values: &'a ParamValues, name: &str) -> Option<&'a str> {
    values.get(name).and_then(Value::as_str)
}

pub fn param_duration(values: &ParamValues, name: &str) -> Option<Duration> {
    let value = values.get(name)?;
    if let Some(text) = value.as_str() {
        return text
            .parse::<HumanDuration>()
            .ok()
            .map(HumanDuration::as_std);
    }
    value.as_u64().map(Duration::from_secs)
}

fn normalize(kind: ParamType, value: &Value) -> Option<Value> {
    match kind {
        ParamType::Bool => value.as_bool().map(Value::Bool),
        ParamType::Int => value.as_i64().map(Value::from),
        ParamType::Uint => value.as_u64().map(Value::from),
        ParamType::String => value.as_str().map(|text| Value::String(text.to_string())),
        ParamType::Bytes => {
            if value.as_str()?.parse::<Base64Bytes>().is_ok() {
                Some(value.clone())
            } else {
                None
            }
        }
        ParamType::Size => {
            if let Some(text) = value.as_str() {
                text.parse::<Size>()
                    .ok()
                    .map(|size| Value::String(size.to_string()))
            } else {
                value
                    .as_u64()
                    .map(Size::from_bytes)
                    .map(|size| Value::String(size.to_string()))
            }
        }
        ParamType::Duration => {
            if let Some(text) = value.as_str() {
                text.parse::<HumanDuration>()
                    .ok()
                    .map(|duration| Value::String(duration.to_string()))
            } else {
                value
                    .as_u64()
                    .map(Duration::from_secs)
                    .map(HumanDuration::from_std)
                    .map(|duration| Value::String(duration.to_string()))
            }
        }
        ParamType::FileMode => {
            if let Some(text) = value.as_str() {
                FileMode::from_str(text)
                    .ok()
                    .map(|mode| Value::String(mode.to_string()))
            } else {
                value
                    .as_u64()
                    .and_then(|mode| u32::try_from(mode).ok())
                    .map(FileMode::new)
                    .map(|mode| Value::String(mode.to_string()))
            }
        }
    }
}

fn numeric_value(kind: ParamType, value: &Value) -> Option<f64> {
    match kind {
        ParamType::Int => value.as_i64().map(|number| number as f64),
        ParamType::Uint => value.as_u64().map(|number| number as f64),
        ParamType::Size => param_number(value, |text| {
            text.parse::<Size>().ok().map(|size| size.as_bytes() as f64)
        }),
        ParamType::Duration => param_number(value, |text| {
            text.parse::<HumanDuration>()
                .ok()
                .map(|duration| duration.as_std().as_nanos() as f64)
        }),
        ParamType::FileMode => param_number(value, |text| {
            FileMode::from_str(text)
                .ok()
                .map(|mode| mode.as_u32() as f64)
        }),
        _ => None,
    }
}

fn param_number(value: &Value, parse: impl Fn(&str) -> Option<f64>) -> Option<f64> {
    if let Some(text) = value.as_str() {
        parse(text)
    } else {
        value.as_u64().map(|number| number as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bool_def() -> ParamDef {
        ParamDef::with_default("flag", ParamType::Bool, json!(false), "A flag")
    }

    fn uint_def() -> ParamDef {
        let mut definition = ParamDef::with_default("count", ParamType::Uint, json!(10), "A count");
        definition.min = Some(json!(1));
        definition.max = Some(json!(100));
        definition
    }

    fn duration_def() -> ParamDef {
        ParamDef::with_default("delay", ParamType::Duration, json!("3 s"), "A delay")
    }

    fn enum_def() -> ParamDef {
        let mut definition =
            ParamDef::with_default("mode", ParamType::String, json!("a"), "A mode");
        definition.enum_values = vec![json!("a"), json!("b")];
        definition
    }

    #[test]
    fn applies_defaults() {
        let values = parse_param_values(&[bool_def(), uint_def()], &ParamValues::new()).unwrap();
        assert_eq!(values.get("flag"), Some(&json!(false)));
        assert_eq!(values.get("count"), Some(&json!(10)));
    }

    #[test]
    fn rejects_unknown_parameters() {
        let error = parse_param_values(
            &[uint_def()],
            &ParamValues::from([("unknown".to_string(), json!(1))]),
        )
        .unwrap_err();
        assert!(matches!(error, ParamError::Unknown { .. }));
    }

    #[test]
    fn rejects_missing_required_parameters() {
        let mut definition = uint_def();
        definition.default = None;
        definition.required = true;
        let error = parse_param_values(&[definition], &ParamValues::new()).unwrap_err();
        assert!(matches!(error, ParamError::Missing { .. }));
    }

    #[test]
    fn validates_ranges_and_enums() {
        let values = ParamValues::from([("count".to_string(), json!(0))]);
        assert!(parse_param_values(&[uint_def()], &values).is_err());

        let values = ParamValues::from([("count".to_string(), json!(101))]);
        assert!(parse_param_values(&[uint_def()], &values).is_err());

        let values = ParamValues::from([("mode".to_string(), json!("c"))]);
        assert!(parse_param_values(&[enum_def()], &values).is_err());

        let values = ParamValues::from([("mode".to_string(), json!("b"))]);
        assert_eq!(
            parse_param_values(&[enum_def()], &values)
                .unwrap()
                .get("mode"),
            Some(&json!("b"))
        );
    }

    #[test]
    fn overrides_const_parameters() {
        let mut definition = uint_def();
        definition.is_const = true;
        let values = ParamValues::from([("count".to_string(), json!(42))]);
        let parsed = parse_param_values(&[definition], &values).unwrap();
        assert_eq!(parsed.get("count"), Some(&json!(10)));
    }

    #[test]
    fn normalizes_durations_and_sizes() {
        let values = ParamValues::from([("delay".to_string(), json!("1500 ms"))]);
        let parsed = parse_param_values(&[duration_def()], &values).unwrap();
        assert_eq!(parsed.get("delay"), Some(&json!("1500 ms")));
        assert_eq!(
            param_duration(&parsed, "delay"),
            Some(Duration::from_millis(1_500))
        );
    }

    #[test]
    fn parses_into_typed_values() {
        let values = ParamValues::from([
            ("flag".to_string(), json!(true)),
            ("count".to_string(), json!(25)),
        ]);
        let parsed = parse_param_values(&[bool_def(), uint_def()], &values).unwrap();
        assert_eq!(parsed.get("flag").and_then(Value::as_bool), Some(true));
        assert_eq!(param_u64(&parsed, "count"), Some(25));
    }
}
