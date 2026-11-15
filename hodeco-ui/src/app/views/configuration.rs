//! Configuration tab: structured editor for the full persisted configuration.

use super::super::{App, Dialog};
use crate::theme::{UiTheme, palette};
use crate::ui::widgets::{self, Tone};
use egui::{ComboBox, DragValue, RichText, TextEdit, TextStyle, Ui};
use hodeco_contract::config::{Archive, CorruptPolicy};
use hodeco_contract::utils::{HumanDuration, Size};

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let live = app.gateway.phase().is_live();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;

    if let Some(error) = &app.config_ui.error {
        let error = error.clone();
        widgets::banner(
            ui,
            theme,
            Tone::Error,
            "Configuration error",
            Some(&error),
            |_ui| {},
        );
    }
    if !app.config_ui.restart_required.is_empty() {
        let fields = app.config_ui.restart_required.join(", ");
        widgets::banner(
            ui,
            theme,
            Tone::Info,
            "Configuration saved; restart required",
            Some(&format!(
                "The file was written. Restart the gateway to apply: {fields}."
            )),
            |ui| {
                if ui.button("Restart gateway").clicked() {
                    app.dialog = Some(Dialog::ConfirmRestart);
                }
            },
        );
    } else if app.config_ui.saved {
        widgets::banner(
            ui,
            theme,
            Tone::Success,
            "Configuration saved",
            Some("The persisted configuration was updated."),
            |_ui| {},
        );
    }
    let dirty = app.config_ui.dirty;
    let saving = app.config_ui.saving;
    widgets::page_header(
        ui,
        theme,
        "Configuration",
        Some("Persisted gateway configuration; saving never restarts the gateway"),
        |ui| {
            if ui
                .add_enabled(
                    ready && dirty && !saving,
                    egui::Button::new(if saving { "Saving…" } else { "Save" }),
                )
                .clicked()
            {
                app.save_config_draft();
            }
            if ui
                .add_enabled(dirty && !saving, egui::Button::new("Discard"))
                .clicked()
            {
                app.discard_config_draft();
            }
            if ui
                .add_enabled(live && !dirty && !saving, egui::Button::new("Refresh"))
                .on_hover_text("Reload the persisted configuration")
                .clicked()
            {
                app.refresh_config();
            }
            if let Some(config) = &app.config
                && let Ok(json) = serde_json::to_string_pretty(config)
            {
                widgets::copy_with_feedback(ui, &json, "Copy JSON");
            }
        },
    );

    let Some(config) = app.config.clone() else {
        if live {
            widgets::loading_state(ui, theme, "Loading configuration…");
        } else {
            widgets::empty_state(
                ui,
                theme,
                "No configuration loaded",
                "Connect to a gateway to read its effective configuration.",
            );
        }
        return;
    };

    if app.config_ui.draft.is_none() {
        app.config_ui.load(&config);
    }

    let Some(draft) = app.config_ui.draft.as_mut() else {
        return;
    };

    let mut changed = false;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            widgets::keyboard_scroll(ui, ui.rect_contains_pointer(ui.max_rect()));
            section(ui, theme, "Paths", |ui| {
                grid(ui, theme, |ui| {
                    text_row(
                        ui,
                        theme,
                        "Working directory",
                        &mut draft.working_dir,
                        config.working_dir.display(),
                        &mut changed,
                    );
                });
            });

            section(ui, theme, "Log", |ui| {
                grid(ui, theme, |ui| {
                    text_row(
                        ui,
                        theme,
                        "Directory",
                        &mut draft.log.dir,
                        config.log.dir.display(),
                        &mut changed,
                    );
                    text_row(
                        ui,
                        theme,
                        "File",
                        &mut draft.log.file,
                        &config.log.file,
                        &mut changed,
                    );
                    octal_row(
                        ui,
                        theme,
                        "Directory mode",
                        &mut draft.log.dir_mode,
                        config.log.dir_mode,
                        &mut changed,
                    );
                    octal_row(
                        ui,
                        theme,
                        "File mode",
                        &mut draft.log.file_mode,
                        config.log.file_mode,
                        &mut changed,
                    );
                    size_row(
                        ui,
                        theme,
                        "Max size",
                        &mut draft.log.max_size,
                        config.log.max_size,
                        &mut changed,
                    );
                    duration_row(
                        ui,
                        theme,
                        "Max age",
                        &mut draft.log.max_age,
                        config.log.max_age,
                        &mut changed,
                    );
                    number_row_u32(
                        ui,
                        theme,
                        "Backups",
                        &mut draft.log.backups,
                        config.log.backups,
                        &mut changed,
                    );
                    number_row_u32(
                        ui,
                        theme,
                        "Backup days",
                        &mut draft.log.backup_days,
                        config.log.backup_days,
                        &mut changed,
                    );
                    ui.label(field_label(theme, "Archive"));
                    let archive_changed = draft.log.archive != config.log.archive;
                    ui.horizontal(|ui| {
                        changed |= ComboBox::from_id_salt("config-log-archive")
                            .selected_text(archive_label(draft.log.archive))
                            .show_ui(ui, |ui| {
                                let mut changed = false;
                                changed |= ui
                                    .selectable_value(&mut draft.log.archive, Archive::None, "None")
                                    .changed();
                                changed |= ui
                                    .selectable_value(&mut draft.log.archive, Archive::Zip, "Zip")
                                    .changed();
                                changed
                            })
                            .inner
                            .unwrap_or(false);
                        if previous_hint(
                            ui,
                            theme,
                            archive_changed,
                            archive_label(config.log.archive),
                        ) {
                            draft.log.archive = config.log.archive;
                            changed = true;
                        }
                    });
                    ui.end_row();
                });
            });

            section(ui, theme, "HTTP server", |ui| {
                grid(ui, theme, |ui| {
                    text_row(
                        ui,
                        theme,
                        "Bind",
                        &mut draft.http_server.bind,
                        &config.http_server.bind,
                        &mut changed,
                    );
                    number_row_u16(
                        ui,
                        theme,
                        "Port",
                        &mut draft.http_server.port,
                        config.http_server.port,
                        &mut changed,
                    );
                    number_row_u32(
                        ui,
                        theme,
                        "Max connections",
                        &mut draft.http_server.max_connections,
                        config.http_server.max_connections,
                        &mut changed,
                    );
                    duration_row(
                        ui,
                        theme,
                        "Read timeout",
                        &mut draft.http_server.read_timeout,
                        config.http_server.read_timeout,
                        &mut changed,
                    );
                    duration_row(
                        ui,
                        theme,
                        "Read header timeout",
                        &mut draft.http_server.read_header_timeout,
                        config.http_server.read_header_timeout,
                        &mut changed,
                    );
                    duration_row(
                        ui,
                        theme,
                        "Write timeout",
                        &mut draft.http_server.write_timeout,
                        config.http_server.write_timeout,
                        &mut changed,
                    );
                    duration_row(
                        ui,
                        theme,
                        "Idle timeout",
                        &mut draft.http_server.idle_timeout,
                        config.http_server.idle_timeout,
                        &mut changed,
                    );
                    number_row_u32(
                        ui,
                        theme,
                        "Max header bytes",
                        &mut draft.http_server.max_header_bytes,
                        config.http_server.max_header_bytes,
                        &mut changed,
                    );
                    text_row(
                        ui,
                        theme,
                        "Static assets directory",
                        &mut draft.http_server.static_assets.dir,
                        config.http_server.static_assets.dir.display(),
                        &mut changed,
                    );
                    text_row(
                        ui,
                        theme,
                        "Static index",
                        &mut draft.http_server.static_assets.index,
                        &config.http_server.static_assets.index,
                        &mut changed,
                    );
                    bool_row(
                        ui,
                        theme,
                        "SPA fallback",
                        &mut draft.http_server.static_assets.spa_fallback,
                        config.http_server.static_assets.spa_fallback,
                        &mut changed,
                    );
                    bool_row(
                        ui,
                        theme,
                        "Precompressed assets",
                        &mut draft.http_server.static_assets.precompressed,
                        config.http_server.static_assets.precompressed,
                        &mut changed,
                    );
                });
            });

            section(ui, theme, "Message log", |ui| {
                grid(ui, theme, |ui| {
                    size_row(
                        ui,
                        theme,
                        "Max size",
                        &mut draft.message_log.max_size,
                        config.message_log.max_size,
                        &mut changed,
                    );
                    text_row(
                        ui,
                        theme,
                        "File",
                        &mut draft.message_log.file,
                        config.message_log.file.display(),
                        &mut changed,
                    );
                    octal_row(
                        ui,
                        theme,
                        "File mode",
                        &mut draft.message_log.file_mode,
                        config.message_log.file_mode,
                        &mut changed,
                    );
                    duration_row(
                        ui,
                        theme,
                        "Auto persist",
                        &mut draft.message_log.auto_persist,
                        config.message_log.auto_persist,
                        &mut changed,
                    );
                    ui.label(field_label(theme, "On corrupt"));
                    let corrupt_changed =
                        draft.message_log.on_corrupt != config.message_log.on_corrupt;
                    ui.horizontal(|ui| {
                        changed |= ComboBox::from_id_salt("config-message-log-corrupt")
                            .selected_text(corrupt_label(draft.message_log.on_corrupt))
                            .show_ui(ui, |ui| {
                                let mut changed = false;
                                changed |= ui
                                    .selectable_value(
                                        &mut draft.message_log.on_corrupt,
                                        CorruptPolicy::Ignore,
                                        "Ignore",
                                    )
                                    .changed();
                                changed |= ui
                                    .selectable_value(
                                        &mut draft.message_log.on_corrupt,
                                        CorruptPolicy::Fail,
                                        "Fail",
                                    )
                                    .changed();
                                changed
                            })
                            .inner
                            .unwrap_or(false);
                        if previous_hint(
                            ui,
                            theme,
                            corrupt_changed,
                            corrupt_label(config.message_log.on_corrupt),
                        ) {
                            draft.message_log.on_corrupt = config.message_log.on_corrupt;
                            changed = true;
                        }
                    });
                    ui.end_row();
                });
            });
        });

    if changed {
        app.config_ui.saved = false;
        app.config_ui.error = None;
    }
    let dirty = match (&app.config, app.config_ui.draft.as_mut()) {
        (Some(config), Some(draft)) => draft.differs_from(config),
        _ => false,
    };
    app.config_ui.dirty = dirty;
}

fn section(ui: &mut Ui, _theme: UiTheme, title: &str, body: impl FnOnce(&mut Ui)) {
    ui.add_space(4.0);
    egui::CollapsingHeader::new(RichText::new(title).size(15.0).strong())
        .default_open(true)
        .show(ui, body);
}

fn grid(ui: &mut Ui, _theme: UiTheme, body: impl FnOnce(&mut Ui)) {
    egui::Grid::new(ui.next_auto_id())
        .num_columns(2)
        .spacing([18.0, 4.0])
        .show(ui, body);
}

fn field_label(theme: UiTheme, label: &str) -> RichText {
    RichText::new(label).size(13.5).color(palette(theme).muted)
}

/// Shows the previous value while a field differs from the loaded config and
/// always offers the same small Reset button used by service parameters.
fn previous_hint(
    ui: &mut Ui,
    theme: UiTheme,
    changed: bool,
    previous: impl std::fmt::Display,
) -> bool {
    let text = previous.to_string();
    let mut reset = false;
    ui.horizontal(|ui| {
        if changed {
            let chars: Vec<char> = text.chars().collect();
            let shown = if chars.len() > 56 {
                format!("{}…", chars.iter().take(56).collect::<String>())
            } else {
                text.clone()
            };
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "was: {}",
                        if shown.is_empty() {
                            "(empty)".to_string()
                        } else {
                            shown
                        }
                    ))
                    .size(13.5)
                    .color(palette(theme).muted),
                )
                .truncate(),
            )
            .on_hover_text(if text.is_empty() {
                "empty".to_string()
            } else {
                text.clone()
            });
        }
        reset = ui
            .add_enabled(changed, egui::Button::new("Reset").small())
            .on_hover_text("Reset to the loaded value")
            .clicked();
    });
    reset
}

fn text_row(
    ui: &mut Ui,
    theme: UiTheme,
    label: &str,
    value: &mut String,
    previous: impl std::fmt::Display,
    changed: &mut bool,
) {
    let previous = previous.to_string();
    ui.label(field_label(theme, label));
    ui.horizontal(|ui| {
        let response = ui.add(
            TextEdit::singleline(value)
                .desired_width(420.0)
                .font(TextStyle::Monospace),
        );
        widgets::scroll_on_focus(&response);
        *changed |= response.changed();
        if previous_hint(ui, theme, *value != previous, &previous) {
            *value = previous.clone();
            *changed = true;
        }
    });
    ui.end_row();
}

fn bool_row(
    ui: &mut Ui,
    theme: UiTheme,
    label: &str,
    value: &mut bool,
    previous: bool,
    changed: &mut bool,
) {
    ui.label(field_label(theme, label));
    ui.horizontal(|ui| {
        *changed |= ui.checkbox(value, "Enabled").changed();
        if previous_hint(
            ui,
            theme,
            *value != previous,
            if previous { "Enabled" } else { "Disabled" },
        ) {
            *value = previous;
            *changed = true;
        }
    });
    ui.end_row();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeUnit {
    Nanoseconds,
    Microseconds,
    Milliseconds,
    Seconds,
    Minutes,
    Hours,
    Days,
    Weeks,
}

impl TimeUnit {
    const ALL: [Self; 8] = [
        Self::Nanoseconds,
        Self::Microseconds,
        Self::Milliseconds,
        Self::Seconds,
        Self::Minutes,
        Self::Hours,
        Self::Days,
        Self::Weeks,
    ];

    fn suffix(self) -> &'static str {
        match self {
            Self::Nanoseconds => "ns",
            Self::Microseconds => "us",
            Self::Milliseconds => "ms",
            Self::Seconds => "s",
            Self::Minutes => "m",
            Self::Hours => "h",
            Self::Days => "d",
            Self::Weeks => "w",
        }
    }

    fn nanos(self) -> u128 {
        match self {
            Self::Nanoseconds => 1,
            Self::Microseconds => 1_000,
            Self::Milliseconds => 1_000_000,
            Self::Seconds => 1_000_000_000,
            Self::Minutes => 60_000_000_000,
            Self::Hours => 3_600_000_000_000,
            Self::Days => 86_400_000_000_000,
            Self::Weeks => 604_800_000_000_000,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TimeParts {
    amount: u64,
    unit: TimeUnit,
}

fn parse_time_parts(value: &str) -> Option<TimeParts> {
    let nanos = value.parse::<HumanDuration>().ok()?.as_std().as_nanos();
    if nanos == 0 {
        return Some(TimeParts {
            amount: 0,
            unit: TimeUnit::Seconds,
        });
    }
    for unit in [
        TimeUnit::Weeks,
        TimeUnit::Days,
        TimeUnit::Hours,
        TimeUnit::Minutes,
        TimeUnit::Seconds,
        TimeUnit::Milliseconds,
        TimeUnit::Microseconds,
        TimeUnit::Nanoseconds,
    ] {
        if nanos.is_multiple_of(unit.nanos()) {
            return Some(TimeParts {
                amount: u64::try_from(nanos / unit.nanos()).ok()?,
                unit,
            });
        }
    }
    None
}

fn format_time(parts: TimeParts) -> String {
    format!("{} {}", parts.amount, parts.unit.suffix())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SizeUnit {
    Bytes,
    Kibibytes,
    Mebibytes,
    Gibibytes,
    Tebibytes,
}

impl SizeUnit {
    const ALL: [Self; 5] = [
        Self::Bytes,
        Self::Kibibytes,
        Self::Mebibytes,
        Self::Gibibytes,
        Self::Tebibytes,
    ];

    fn suffix(self) -> &'static str {
        match self {
            Self::Bytes => "B",
            Self::Kibibytes => "KiB",
            Self::Mebibytes => "MiB",
            Self::Gibibytes => "GiB",
            Self::Tebibytes => "TiB",
        }
    }

    fn bytes(self) -> u64 {
        match self {
            Self::Bytes => 1,
            Self::Kibibytes => 1_024,
            Self::Mebibytes => 1_048_576,
            Self::Gibibytes => 1_073_741_824,
            Self::Tebibytes => 1_099_511_627_776,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SizeParts {
    amount: u64,
    unit: SizeUnit,
}

fn parse_size_parts(value: &str) -> Option<SizeParts> {
    let bytes = value.parse::<Size>().ok()?.as_bytes();
    if bytes == 0 {
        return Some(SizeParts {
            amount: 0,
            unit: SizeUnit::Bytes,
        });
    }
    for unit in [
        SizeUnit::Tebibytes,
        SizeUnit::Gibibytes,
        SizeUnit::Mebibytes,
        SizeUnit::Kibibytes,
        SizeUnit::Bytes,
    ] {
        if bytes.is_multiple_of(unit.bytes()) {
            return Some(SizeParts {
                amount: bytes / unit.bytes(),
                unit,
            });
        }
    }
    None
}

fn format_size(parts: SizeParts) -> String {
    format!("{} {}", parts.amount, parts.unit.suffix())
}

fn duration_row(
    ui: &mut Ui,
    theme: UiTheme,
    label: &str,
    value: &mut String,
    previous: impl std::fmt::Display,
    changed: &mut bool,
) {
    let previous = previous.to_string();
    ui.label(field_label(theme, label));
    if let Some(mut parts) = parse_time_parts(value) {
        ui.horizontal(|ui| {
            let response = ui.add(
                DragValue::new(&mut parts.amount)
                    .range(0..=u64::MAX)
                    .speed(1.0),
            );
            let original_unit = parts.unit;
            let mut unit = parts.unit;
            ComboBox::from_id_salt(ui.next_auto_id())
                .selected_text(unit.suffix())
                .width(64.0)
                .show_ui(ui, |ui| {
                    for candidate in TimeUnit::ALL {
                        ui.selectable_value(&mut unit, candidate, candidate.suffix());
                    }
                });
            if unit != original_unit {
                let nanos = u128::from(parts.amount) * original_unit.nanos();
                parts.amount = nanos
                    .checked_div(unit.nanos())
                    .and_then(|amount| u64::try_from(amount).ok())
                    .unwrap_or(0);
                parts.unit = unit;
            }
            if response.changed() || unit != original_unit {
                *value = format_time(parts);
                *changed = true;
            }
            if previous_hint(ui, theme, value.trim() != previous.trim(), &previous) {
                *value = previous.clone();
                *changed = true;
            }
        });
    } else {
        let response = ui.add(
            TextEdit::singleline(value)
                .desired_width(420.0)
                .font(TextStyle::Monospace),
        );
        widgets::scroll_on_focus(&response);
        *changed |= response.changed();
        if previous_hint(ui, theme, value.trim() != previous.trim(), &previous) {
            *value = previous.clone();
            *changed = true;
        }
    }
    ui.end_row();
}

fn size_row(
    ui: &mut Ui,
    theme: UiTheme,
    label: &str,
    value: &mut String,
    previous: impl std::fmt::Display,
    changed: &mut bool,
) {
    let previous = previous.to_string();
    ui.label(field_label(theme, label));
    if let Some(mut parts) = parse_size_parts(value) {
        ui.horizontal(|ui| {
            let response = ui.add(
                DragValue::new(&mut parts.amount)
                    .range(0..=u64::MAX)
                    .speed(1.0),
            );
            let original_unit = parts.unit;
            let mut unit = parts.unit;
            ComboBox::from_id_salt(ui.next_auto_id())
                .selected_text(unit.suffix())
                .width(64.0)
                .show_ui(ui, |ui| {
                    for candidate in SizeUnit::ALL {
                        ui.selectable_value(&mut unit, candidate, candidate.suffix());
                    }
                });
            if unit != original_unit {
                let bytes = parts.amount.saturating_mul(original_unit.bytes());
                parts.amount = bytes / unit.bytes();
                parts.unit = unit;
            }
            if response.changed() || unit != original_unit {
                *value = format_size(parts);
                *changed = true;
            }
            if previous_hint(ui, theme, value.trim() != previous.trim(), &previous) {
                *value = previous.clone();
                *changed = true;
            }
        });
    } else {
        let response = ui.add(
            TextEdit::singleline(value)
                .desired_width(420.0)
                .font(TextStyle::Monospace),
        );
        widgets::scroll_on_focus(&response);
        *changed |= response.changed();
        if previous_hint(ui, theme, value.trim() != previous.trim(), &previous) {
            *value = previous.clone();
            *changed = true;
        }
    }
    ui.end_row();
}

fn number_row_u16(
    ui: &mut Ui,
    theme: UiTheme,
    label: &str,
    value: &mut String,
    previous: impl std::fmt::Display,
    changed: &mut bool,
) {
    number_row(ui, theme, label, value, previous, changed, u16::MAX as u64)
}

fn number_row_u32(
    ui: &mut Ui,
    theme: UiTheme,
    label: &str,
    value: &mut String,
    previous: impl std::fmt::Display,
    changed: &mut bool,
) {
    number_row(ui, theme, label, value, previous, changed, u32::MAX as u64)
}

fn number_row(
    ui: &mut Ui,
    theme: UiTheme,
    label: &str,
    value: &mut String,
    previous: impl std::fmt::Display,
    changed: &mut bool,
    max: u64,
) {
    let previous = previous.to_string();
    ui.label(field_label(theme, label));
    if let Ok(mut number) = value.trim().parse::<u64>() {
        ui.horizontal(|ui| {
            let response = ui.add(DragValue::new(&mut number).range(0..=max).speed(1.0));
            if response.changed() {
                *value = number.to_string();
                *changed = true;
            }
            if previous_hint(ui, theme, value.trim() != previous.trim(), &previous) {
                *value = previous.clone();
                *changed = true;
            }
        });
    } else {
        ui.horizontal(|ui| {
            let response = ui.add(
                TextEdit::singleline(value)
                    .desired_width(180.0)
                    .font(TextStyle::Monospace),
            );
            widgets::scroll_on_focus(&response);
            *changed |= response.changed();
            if previous_hint(ui, theme, value.trim() != previous.trim(), &previous) {
                *value = previous.clone();
                *changed = true;
            }
        });
    }
    ui.end_row();
}

fn octal_row(
    ui: &mut Ui,
    theme: UiTheme,
    label: &str,
    value: &mut String,
    previous: impl std::fmt::Display,
    changed: &mut bool,
) {
    let previous = previous.to_string();
    ui.label(field_label(theme, label));
    ui.horizontal(|ui| {
        let response = ui.add(
            TextEdit::singleline(value)
                .hint_text("0644")
                .desired_width(120.0)
                .font(TextStyle::Monospace),
        );
        widgets::scroll_on_focus(&response);
        if response.changed() {
            let filtered: String = value
                .chars()
                .filter(|character| matches!(character, '0'..='7'))
                .collect();
            if *value != filtered {
                *value = filtered;
            }
            *changed = true;
        }
        if previous_hint(ui, theme, value.trim() != previous.trim(), &previous) {
            *value = previous.clone();
            *changed = true;
        }
    });
    ui.end_row();
}

fn archive_label(archive: Archive) -> &'static str {
    match archive {
        Archive::None => "None",
        Archive::Zip => "Zip",
    }
}

fn corrupt_label(policy: CorruptPolicy) -> &'static str {
    match policy {
        CorruptPolicy::Ignore => "Ignore",
        CorruptPolicy::Fail => "Fail",
    }
}
