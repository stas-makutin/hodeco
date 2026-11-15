//! Structured parameter editors driven by the contract's [`ParamDef`].
//!
//! The editor backs the service-parameters dialog and the Configuration
//! service drafts, and is also exercised by the component gallery.
//!
//! Service parameters store explicit overrides only. Every editable field
//! therefore starts at the effective value (the stored override or the
//! definition default), and `collect` emits only values that differ from the
//! definition default. There is no "Override" checkbox in the service editor.
//! The builder's TTL editor uses [`duration_override_editor`] because an empty
//! TTL means "inherit the service or protocol default".

use crate::theme::{UiTheme, palette};
use egui::{ComboBox, DragValue, RichText, TextEdit, Ui};
use hodeco_contract::domain::{ParamDef, ParamError, ParamType, ParamValues};
use hodeco_contract::utils::HumanDuration;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurationUnit {
    Nanoseconds,
    Microseconds,
    Milliseconds,
    Seconds,
    Minutes,
    Hours,
}

impl DurationUnit {
    const ALL: [Self; 6] = [
        Self::Nanoseconds,
        Self::Microseconds,
        Self::Milliseconds,
        Self::Seconds,
        Self::Minutes,
        Self::Hours,
    ];

    fn suffix(self) -> &'static str {
        match self {
            Self::Nanoseconds => "ns",
            Self::Microseconds => "us",
            Self::Milliseconds => "ms",
            Self::Seconds => "s",
            Self::Minutes => "m",
            Self::Hours => "h",
        }
    }

    fn as_nanos(self) -> u128 {
        match self {
            Self::Nanoseconds => 1,
            Self::Microseconds => 1_000,
            Self::Milliseconds => 1_000_000,
            Self::Seconds => 1_000_000_000,
            Self::Minutes => 60_000_000_000,
            Self::Hours => 3_600_000_000_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DurationDraft {
    amount: u64,
    unit: DurationUnit,
}

impl DurationDraft {
    fn from_std(duration: Duration) -> Self {
        let nanos = duration.as_nanos();
        if nanos == 0 {
            return Self {
                amount: 0,
                unit: DurationUnit::Seconds,
            };
        }
        for unit in [
            DurationUnit::Hours,
            DurationUnit::Minutes,
            DurationUnit::Seconds,
            DurationUnit::Milliseconds,
            DurationUnit::Microseconds,
            DurationUnit::Nanoseconds,
        ] {
            if nanos.is_multiple_of(unit.as_nanos()) {
                return Self {
                    amount: (nanos / unit.as_nanos()) as u64,
                    unit,
                };
            }
        }
        Self {
            amount: nanos.max(1) as u64,
            unit: DurationUnit::Nanoseconds,
        }
    }

    fn from_value(value: Option<&Value>) -> Self {
        let duration = match value {
            Some(Value::String(text)) => text
                .parse::<HumanDuration>()
                .ok()
                .map(HumanDuration::as_std),
            Some(value) => value.as_u64().map(Duration::from_secs),
            None => None,
        };
        Self::from_std(duration.unwrap_or_default())
    }

    fn to_value(self) -> Value {
        Value::String(
            HumanDuration::from_std(Duration::from_nanos(
                self.amount.saturating_mul(self.unit.as_nanos() as u64),
            ))
            .to_string(),
        )
    }
}

/// Optional duration editor for values that can inherit a default.
///
/// The checkbox controls whether an explicit value is present. The editor uses
/// the same integer-and-unit pair as service and configuration parameters.
pub fn duration_override_editor(
    ui: &mut Ui,
    id_salt: &str,
    label: &str,
    value: &mut Option<HumanDuration>,
    default: Option<HumanDuration>,
) -> bool {
    let mut changed = false;
    let mut enabled = value.is_some();
    ui.horizontal(|ui| {
        if ui
            .checkbox(&mut enabled, format!("Override {label}"))
            .changed()
        {
            if enabled {
                *value = Some(
                    value
                        .or(default)
                        .unwrap_or_else(|| HumanDuration::from_std(Duration::ZERO)),
                );
            } else {
                *value = None;
            }
            changed = true;
        }
        if let Some(current) = *value {
            let mut draft = DurationDraft::from_std(current.as_std());
            let response = ui.add(
                DragValue::new(&mut draft.amount)
                    .range(0..=u64::MAX)
                    .speed(1.0),
            );
            let original_unit = draft.unit;
            let mut unit = original_unit;
            ComboBox::from_id_salt((id_salt, "unit"))
                .selected_text(unit.suffix())
                .width(60.0)
                .show_ui(ui, |ui| {
                    for candidate in DurationUnit::ALL {
                        ui.selectable_value(&mut unit, candidate, candidate.suffix());
                    }
                });
            draft.unit = unit;
            if response.changed() || unit != original_unit {
                let nanos = (draft.amount as u128).saturating_mul(draft.unit.as_nanos());
                *value = Some(HumanDuration::from_std(Duration::from_nanos(
                    nanos.min(u64::MAX as u128) as u64,
                )));
                changed = true;
            }
        }
    });
    changed
}

#[derive(Debug, Clone)]
enum Draft {
    Bool(bool),
    Int(i64),
    Uint(u64),
    Duration(DurationDraft),
    Text(String),
}

#[derive(Debug, Clone)]
pub struct ParamEdit {
    pub def: ParamDef,
    draft: Draft,
    /// Explicit override at editor creation. Used only for dirty detection.
    initial: Option<Value>,
    /// Effective loaded value, used for the `was:` hint and reset.
    previous: Value,
    pub error: Option<String>,
}

impl ParamEdit {
    pub fn new(def: ParamDef, value: Option<&Value>) -> Self {
        let source = value.or(def.default.as_ref());
        let draft = draft_from_source(&def, source);
        let initial = value
            .cloned()
            .filter(|value| def.default.as_ref() != Some(value));
        let mut edit = Self {
            def,
            draft,
            initial,
            previous: Value::Null,
            error: None,
        };
        edit.previous = edit.value();
        edit
    }

    pub fn value(&self) -> Value {
        match &self.draft {
            Draft::Bool(value) => Value::Bool(*value),
            Draft::Int(value) => json!(value),
            Draft::Uint(value) => json!(value),
            Draft::Duration(duration) => duration.to_value(),
            Draft::Text(text) => Value::String(text.clone()),
        }
    }

    /// The explicit override represented by the current draft, if any.
    pub fn override_value(&self) -> Option<Value> {
        if self.def.is_const {
            return None;
        }
        let value = self.value();
        if self.def.default.as_ref() == Some(&value) {
            return None;
        }
        if self.initial.is_none() && self.def.default.is_none() && is_empty_value(&value) {
            return None;
        }
        Some(value)
    }

    pub fn is_dirty(&self) -> bool {
        self.override_value().as_ref() != self.initial.as_ref()
    }

    pub fn validate(&self) -> Result<Value, ParamError> {
        self.def.validate_value(&self.value())
    }

    fn reset_to_previous(&mut self) {
        self.draft = draft_from_source(&self.def, Some(&self.previous));
    }

    /// Renders one parameter. Labels and descriptions sit above the value so
    /// they always start at the left edge and do not move when a Reset button
    /// appears.
    pub fn ui(&mut self, ui: &mut Ui, theme: UiTheme) -> bool {
        let palette = palette(theme);
        let mut changed = false;
        let enabled = !self.def.is_const;
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&self.def.name).strong());
                if self.def.required {
                    ui.label(RichText::new("required").size(13.5).color(palette.amber));
                }
                if self.def.is_const {
                    ui.label(
                        RichText::new("fixed by server")
                            .size(13.5)
                            .strong()
                            .color(palette.amber),
                    );
                }
            });
            ui.horizontal_wrapped(|ui| {
                if let Some(description) = &self.def.description {
                    ui.label(RichText::new(description).size(14.0).color(palette.text));
                    ui.label(RichText::new("·").size(14.0).color(palette.border));
                }
                let default = self
                    .def
                    .default
                    .as_ref()
                    .map(display_value)
                    .unwrap_or_else(|| "—".to_string());
                ui.label(
                    RichText::new(format!("default: {default}"))
                        .size(14.0)
                        .color(palette.muted),
                );
            });
            ui.horizontal(|ui| {
                ui.add_enabled_ui(enabled, |ui| {
                    changed |= self.draft_ui(ui);
                });
                if enabled {
                    let field_changed = self.value() != self.previous;
                    if field_changed {
                        let text = display_value(&self.previous);
                        let chars: Vec<char> = text.chars().collect();
                        let shown = if chars.len() > 56 {
                            format!("{}…", chars.iter().take(56).collect::<String>())
                        } else {
                            text.clone()
                        };
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("was: {shown}"))
                                    .size(13.5)
                                    .color(palette.muted),
                            )
                            .truncate(),
                        )
                        .on_hover_text(text);
                    }
                    if ui
                        .add_enabled(field_changed, egui::Button::new("Reset").small())
                        .on_hover_text("Reset to the loaded value")
                        .clicked()
                    {
                        self.reset_to_previous();
                        changed = true;
                    }
                }
            });
            if let Some(error) = &self.error {
                ui.label(RichText::new(error).size(14.0).color(palette.red));
            }
        });
        changed
    }

    fn draft_ui(&mut self, ui: &mut Ui) -> bool {
        match (&mut self.draft, self.def.kind) {
            (Draft::Bool(value), ParamType::Bool) => ui.checkbox(value, "Enabled").changed(),
            (Draft::Int(value), ParamType::Int) => {
                let min = self.def.min.as_ref().and_then(Value::as_i64);
                let max = self.def.max.as_ref().and_then(Value::as_i64);
                spin_i64(ui, value, min, max)
            }
            (Draft::Uint(value), ParamType::Uint) => {
                let min = self.def.min.as_ref().and_then(Value::as_u64);
                let max = self.def.max.as_ref().and_then(Value::as_u64);
                spin_u64(ui, value, min, max)
            }
            (Draft::Duration(duration), ParamType::Duration) => {
                let response = ui.add(
                    DragValue::new(&mut duration.amount)
                        .range(0..=u64::MAX)
                        .speed(1.0),
                );
                let original_unit = duration.unit;
                let mut unit = original_unit;
                ComboBox::from_id_salt(("param-duration-unit", &self.def.name))
                    .selected_text(unit.suffix())
                    .width(60.0)
                    .show_ui(ui, |ui| {
                        for candidate in DurationUnit::ALL {
                            ui.selectable_value(&mut unit, candidate, candidate.suffix());
                        }
                    });
                duration.unit = unit;
                response.changed() || unit != original_unit
            }
            (Draft::Text(text), ParamType::String) if !self.def.enum_values.is_empty() => {
                let mut changed = false;
                ComboBox::from_id_salt(("param-enum", &self.def.name))
                    .selected_text(text.clone())
                    .show_ui(ui, |ui| {
                        for option in &self.def.enum_values {
                            let label = option
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| option.to_string());
                            if ui.selectable_value(text, label.clone(), label).changed() {
                                changed = true;
                            }
                        }
                    });
                changed
            }
            (Draft::Text(text), kind) => {
                let hint = match kind {
                    ParamType::Bytes => "base64",
                    ParamType::Size => "e.g. 64 MiB",
                    ParamType::Duration => "e.g. 500 ms",
                    ParamType::FileMode => "e.g. 0644",
                    _ => "",
                };
                let width = (ui.available_width() - 18.0).max(180.0);
                let response = ui.add(
                    TextEdit::singleline(text)
                        .hint_text(hint)
                        .desired_width(width),
                );
                crate::ui::widgets::scroll_on_focus(&response);
                response.changed()
            }
            _ => false,
        }
    }
}

fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::String(text) => text.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

fn draft_from_source(def: &ParamDef, source: Option<&Value>) -> Draft {
    match def.kind {
        ParamType::Bool => Draft::Bool(source.and_then(Value::as_bool).unwrap_or(false)),
        ParamType::Int => Draft::Int(source.and_then(Value::as_i64).unwrap_or(0)),
        ParamType::Uint => Draft::Uint(source.and_then(Value::as_u64).unwrap_or(0)),
        ParamType::Duration => Draft::Duration(DurationDraft::from_value(source)),
        ParamType::String | ParamType::Bytes | ParamType::Size | ParamType::FileMode => {
            Draft::Text(match source {
                Some(Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            })
        }
    }
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn spin_i64(ui: &mut Ui, value: &mut i64, min: Option<i64>, max: Option<i64>) -> bool {
    let min = min.unwrap_or(i64::MIN);
    let max = max.unwrap_or(i64::MAX);
    ui.add(DragValue::new(value).range(min..=max)).changed()
}

fn spin_u64(ui: &mut Ui, value: &mut u64, min: Option<u64>, max: Option<u64>) -> bool {
    let min = min.unwrap_or(0);
    let max = max.unwrap_or(u64::MAX);
    ui.add(DragValue::new(value).range(min..=max)).changed()
}

#[derive(Debug, Clone, Default)]
pub struct ParamEditor {
    pub edits: Vec<ParamEdit>,
}

impl ParamEditor {
    pub fn new(defs: &[ParamDef], values: &ParamValues) -> Self {
        Self {
            edits: defs
                .iter()
                .map(|definition| ParamEdit::new(definition.clone(), values.get(&definition.name)))
                .collect(),
        }
    }

    /// Both service dialogs and Configuration drafts store explicit overrides
    /// only; this is kept as a named constructor for call-site clarity.
    pub fn new_overrides(defs: &[ParamDef], values: &ParamValues) -> Self {
        Self::new(defs, values)
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    pub fn is_dirty(&self) -> bool {
        self.edits.iter().any(ParamEdit::is_dirty)
    }

    /// Renders every parameter in definition order.
    pub fn ui(&mut self, ui: &mut Ui, theme: UiTheme) -> bool {
        let mut changed = false;
        for edit in &mut self.edits {
            changed |= edit.ui(ui, theme);
            ui.add_space(4.0);
        }
        changed
    }

    /// Validates current overrides and returns only explicit values. Errors are
    /// also stored on the affected rows for inline display.
    pub fn collect(&mut self) -> Result<ParamValues, Vec<String>> {
        let mut values = ParamValues::new();
        let mut errors = Vec::new();
        for edit in &mut self.edits {
            match edit.override_value() {
                None => {
                    edit.error = None;
                }
                Some(value) => match edit.def.validate_value(&value) {
                    Ok(value) => {
                        edit.error = None;
                        values.insert(edit.def.name.clone(), value);
                    }
                    Err(error) => {
                        let message = error.to_string();
                        edit.error = Some(message.clone());
                        errors.push(message);
                    }
                },
            }
        }
        if errors.is_empty() {
            Ok(values)
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_validate_against_the_definition() {
        let def = ParamDef::with_default("count", ParamType::Uint, json!(4), "count");
        let mut edit = ParamEdit::new(def, Some(&json!(8)));
        assert_eq!(edit.validate().unwrap(), json!(8));
        edit.draft = Draft::Uint(0);
        let mut def = edit.def.clone();
        def.min = Some(json!(1));
        edit.def = def;
        assert!(edit.validate().is_err());
    }

    #[test]
    fn collect_normalizes_durations() {
        let def = ParamDef::with_default("delay", ParamType::Duration, json!("3 s"), "delay");
        let mut editor = ParamEditor::new(&[def], &ParamValues::new());
        let values = editor.collect().unwrap();
        assert_eq!(values.get("delay"), None);
        editor.edits[0].draft = Draft::Duration(DurationDraft {
            amount: 4,
            unit: DurationUnit::Seconds,
        });
        let values = editor.collect().unwrap();
        assert_eq!(values.get("delay"), Some(&json!("4 s")));

        editor.edits[0].draft = Draft::Duration(DurationDraft::from_value(Some(&json!("1500 ms"))));
        let values = editor.collect().unwrap();
        assert_eq!(values.get("delay"), Some(&json!("1500 ms")));
    }

    #[test]
    fn editor_only_collects_values_that_differ_from_defaults() {
        let defs = vec![
            ParamDef::with_default("baudRate", ParamType::Uint, json!(115_200), "baud"),
            ParamDef::with_default("parity", ParamType::String, json!("none"), "parity"),
        ];
        let values = ParamValues::from([("baudRate".to_string(), json!(57_600))]);
        let mut editor = ParamEditor::new_overrides(&defs, &values);
        assert!(!editor.is_dirty());

        let values = editor.collect().unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values.get("baudRate"), Some(&json!(57_600)));
        assert_eq!(values.get("parity"), None);

        editor.edits[0].reset_to_previous();
        assert!(!editor.is_dirty());
        assert_eq!(
            editor.collect().unwrap().get("baudRate"),
            Some(&json!(57_600))
        );
    }

    #[test]
    fn reset_to_previous_clears_dirty_for_a_default_value() {
        let def = ParamDef::with_default("count", ParamType::Uint, json!(4), "count");
        let mut editor = ParamEditor::new_overrides(&[def], &ParamValues::new());
        editor.edits[0].draft = Draft::Uint(9);
        assert!(editor.is_dirty());
        editor.edits[0].reset_to_previous();
        assert!(!editor.is_dirty());
    }

    #[test]
    fn reset_clears_dirty_when_the_loaded_override_equals_the_default() {
        let def = ParamDef::with_default("count", ParamType::Uint, json!(4), "count");
        let values = ParamValues::from([("count".to_string(), json!(4))]);
        let mut editor = ParamEditor::new_overrides(&[def], &values);
        assert!(!editor.is_dirty());
        editor.edits[0].draft = Draft::Uint(9);
        assert!(editor.is_dirty());
        editor.edits[0].reset_to_previous();
        assert!(!editor.is_dirty());
    }

    #[test]
    fn reset_restores_the_loaded_previous_value() {
        let def = ParamDef::with_default("count", ParamType::Uint, json!(4), "count");
        let mut edit = ParamEdit::new(def, Some(&json!(8)));
        assert_eq!(edit.previous, json!(8));
        assert_eq!(edit.value(), json!(8));
        edit.draft = Draft::Uint(12);
        assert_ne!(edit.value(), edit.previous);
        edit.reset_to_previous();
        assert_eq!(edit.value(), json!(8));
    }

    #[test]
    fn const_fields_are_never_collected() {
        let mut def = ParamDef::with_default("dataBits", ParamType::String, json!("8"), "bits");
        def.is_const = true;
        let mut editor = ParamEditor::new(&[def], &ParamValues::new());
        assert_eq!(editor.collect().unwrap().len(), 0);
        assert!(!editor.is_dirty());
    }
}
