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
            if let Some(config) = &app.config {
                let json = crate::model::pretty_json(config);
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
    let mut jump_to: Option<&'static str> = None;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Jump to").small().color(palette(theme).muted));
        for title in [
            "Paths",
            "Catalog files",
            "Log",
            "HTTP server",
            "Message log",
            "Task engine",
            "Task history log",
        ] {
            if ui.link(title).clicked() {
                jump_to = Some(title);
            }
        }
    });
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .id_salt("configuration-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            widgets::keyboard_scroll(ui, ui.rect_contains_pointer(ui.max_rect()));
            section(ui, theme, "Paths", jump_to == Some("Paths"), |ui| {
                widgets::control_group(ui, "Runtime", |ui| {
                    grid(ui, theme, |ui| {
                        text_row(
                            ui,
                            theme,
                            "Working directory",
                            &mut draft.working_dir,
                            config.working_dir.display(),
                            &mut changed,
                        );
                        note_row(
                            ui,
                            theme,
                            "Base directory for relative paths and runtime data.",
                        );
                    });
                });
            });

            section(ui, theme, "Catalog files", jump_to == Some("Catalog files"), |ui| {
                widgets::control_group(ui, "Paths", |ui| {
                    grid(ui, theme, |ui| {
                        text_row(
                            ui,
                            theme,
                            "Services",
                            &mut draft.files.services,
                            config.files.services.display(),
                            &mut changed,
                        );
                        note_row(ui, theme, "Path to services.yaml, relative to config.yaml.");
                        text_row(
                            ui,
                            theme,
                            "Tasks",
                            &mut draft.files.tasks,
                            config.files.tasks.display(),
                            &mut changed,
                        );
                        note_row(ui, theme, "Path to tasks.yaml.");
                        text_row(
                            ui,
                            theme,
                            "Schedules",
                            &mut draft.files.schedules,
                            config.files.schedules.display(),
                            &mut changed,
                        );
                        note_row(ui, theme, "Path to schedules.yaml.");
                        text_row(
                            ui,
                            theme,
                            "Commands",
                            &mut draft.files.commands,
                            config.files.commands.display(),
                            &mut changed,
                        );
                        note_row(ui, theme, "Path to commands.yaml.");
                    });
                });
            });

            section(ui, theme, "Log", jump_to == Some("Log"), |ui| {
                widgets::control_group(ui, "Files", |ui| {
                    grid(ui, theme, |ui| {
                        text_row(
                            ui,
                            theme,
                            "Directory",
                            &mut draft.log.dir,
                            config.log.dir.display(),
                            &mut changed,
                        );
                        note_row(ui, theme, "Directory for rolling log files.");
                        text_row(
                            ui,
                            theme,
                            "File",
                            &mut draft.log.file,
                            &config.log.file,
                            &mut changed,
                        );
                        note_row(ui, theme, "Log file name, placed inside the log directory.");
                        octal_row(
                            ui,
                            theme,
                            "Directory mode",
                            &mut draft.log.dir_mode,
                            config.log.dir_mode,
                            &mut changed,
                        );
                        note_row(ui, theme, "Unix permissions used when creating the directory.");
                        octal_row(
                            ui,
                            theme,
                            "File mode",
                            &mut draft.log.file_mode,
                            config.log.file_mode,
                            &mut changed,
                        );
                        note_row(ui, theme, "Unix permissions used for new log files.");
                    });
                });
                widgets::control_group(ui, "Rotation", |ui| {
                    grid(ui, theme, |ui| {
                        size_row(
                            ui,
                            theme,
                            "Max size",
                            &mut draft.log.max_size,
                            config.log.max_size,
                            &mut changed,
                        );
                        note_row(ui, theme, "Rotate the active file when it reaches this size.");
                        duration_row(
                            ui,
                            theme,
                            "Max age",
                            &mut draft.log.max_age,
                            config.log.max_age,
                            &mut changed,
                        );
                        note_row(ui, theme, "Rotate the active file after this age.");
                        number_row_u32(
                            ui,
                            theme,
                            "Backups",
                            &mut draft.log.backups,
                            config.log.backups,
                            &mut changed,
                        );
                        note_row(ui, theme, "Maximum retained rotated files.");
                        number_row_u32(
                            ui,
                            theme,
                            "Backup days",
                            &mut draft.log.backup_days,
                            config.log.backup_days,
                            &mut changed,
                        );
                        note_row(ui, theme, "Delete backups older than this many days.");
                        ui.label(field_label(theme, "Archive"));
                        let archive_changed = draft.log.archive != config.log.archive;
                        ui.horizontal(|ui| {
                            changed |= ComboBox::from_id_salt("config-log-archive")
                                .selected_text(archive_label(draft.log.archive))
                                .show_ui(ui, |ui| {
                                    let mut changed = false;
                                    changed |= ui
                                        .selectable_value(
                                            &mut draft.log.archive,
                                            Archive::None,
                                            "None",
                                        )
                                        .changed();
                                    changed |= ui
                                        .selectable_value(
                                            &mut draft.log.archive,
                                            Archive::Zip,
                                            "Zip",
                                        )
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
                        note_row(ui, theme, "Optionally compress rotated files as ZIP archives.");
                    });
                });
            });

            section(ui, theme, "HTTP server", jump_to == Some("HTTP server"), |ui| {
                widgets::control_group(ui, "Listener", |ui| {
                    grid(ui, theme, |ui| {
                        text_row(
                            ui,
                            theme,
                            "Bind",
                            &mut draft.http_server.bind,
                            &config.http_server.bind,
                            &mut changed,
                        );
                        note_row(ui, theme, "Interface to bind; 0.0.0.0 listens on all interfaces.");
                        number_row_u16(
                            ui,
                            theme,
                            "Port",
                            &mut draft.http_server.port,
                            config.http_server.port,
                            &mut changed,
                        );
                        note_row(ui, theme, "TCP port for HTTP and WebSocket clients.");
                        number_row_u32(
                            ui,
                            theme,
                            "Max connections",
                            &mut draft.http_server.max_connections,
                            config.http_server.max_connections,
                            &mut changed,
                        );
                        note_row(
                            ui,
                            theme,
                            "Concurrent accepted connections; 0 removes the limit.",
                        );
                        number_row_u32(
                            ui,
                            theme,
                            "Max header bytes",
                            &mut draft.http_server.max_header_bytes,
                            config.http_server.max_header_bytes,
                            &mut changed,
                        );
                        note_row(ui, theme, "Maximum size of request headers.");
                    });
                });
                widgets::control_group(ui, "Timeouts", |ui| {
                    grid(ui, theme, |ui| {
                        duration_row(
                            ui,
                            theme,
                            "Read timeout",
                            &mut draft.http_server.read_timeout,
                            config.http_server.read_timeout,
                            &mut changed,
                        );
                        note_row(ui, theme, "Maximum time to read the request body.");
                        duration_row(
                            ui,
                            theme,
                            "Read header timeout",
                            &mut draft.http_server.read_header_timeout,
                            config.http_server.read_header_timeout,
                            &mut changed,
                        );
                        note_row(ui, theme, "Maximum time to read request headers.");
                        duration_row(
                            ui,
                            theme,
                            "Write timeout",
                            &mut draft.http_server.write_timeout,
                            config.http_server.write_timeout,
                            &mut changed,
                        );
                        note_row(ui, theme, "Maximum time to write a response.");
                        duration_row(
                            ui,
                            theme,
                            "Idle timeout",
                            &mut draft.http_server.idle_timeout,
                            config.http_server.idle_timeout,
                            &mut changed,
                        );
                        note_row(ui, theme, "Keep-alive idle timeout.");
                    });
                });
                widgets::control_group(ui, "Static assets", |ui| {
                    grid(ui, theme, |ui| {
                        text_row(
                            ui,
                            theme,
                            "Directory",
                            &mut draft.http_server.static_assets.dir,
                            config.http_server.static_assets.dir.display(),
                            &mut changed,
                        );
                        note_row(ui, theme, "Directory containing the built web UI.");
                        text_row(
                            ui,
                            theme,
                            "Index",
                            &mut draft.http_server.static_assets.index,
                            &config.http_server.static_assets.index,
                            &mut changed,
                        );
                        note_row(ui, theme, "Index file served for the UI root.");
                        bool_row(
                            ui,
                            theme,
                            "SPA fallback",
                            &mut draft.http_server.static_assets.spa_fallback,
                            config.http_server.static_assets.spa_fallback,
                            &mut changed,
                        );
                        note_row(ui, theme, "Serve the index for unknown non-API paths.");
                        bool_row(
                            ui,
                            theme,
                            "Precompressed assets",
                            &mut draft.http_server.static_assets.precompressed,
                            config.http_server.static_assets.precompressed,
                            &mut changed,
                        );
                        note_row(ui, theme, "Prefer sibling .br or .gz files when supported.");
                    });
                });
            });

            section(ui, theme, "Message log", jump_to == Some("Message log"), |ui| {
                widgets::control_group(ui, "Storage", |ui| {
                    grid(ui, theme, |ui| {
                        size_row(
                            ui,
                            theme,
                            "Max size",
                            &mut draft.message_log.max_size,
                            config.message_log.max_size,
                            &mut changed,
                        );
                        note_row(
                            ui,
                            theme,
                            "History is compacted after growing beyond this size times the compaction factor.",
                        );
                        text_row(
                            ui,
                            theme,
                            "File",
                            &mut draft.message_log.file,
                            config.message_log.file.display(),
                            &mut changed,
                        );
                        note_row(ui, theme, "File used to persist message history.");
                        octal_row(
                            ui,
                            theme,
                            "File mode",
                            &mut draft.message_log.file_mode,
                            config.message_log.file_mode,
                            &mut changed,
                        );
                        note_row(ui, theme, "Unix permissions for new log files.");
                        duration_row(
                            ui,
                            theme,
                            "Flush interval",
                            &mut draft.message_log.auto_persist,
                            config.message_log.auto_persist,
                            &mut changed,
                        );
                        note_row(ui, theme, "How often buffered message history is written to disk.");
                    });
                });
                widgets::control_group(ui, "Compaction", |ui| {
                    grid(ui, theme, |ui| {
                        text_row(
                            ui,
                            theme,
                            "Compaction factor",
                            &mut draft.message_log.compaction_factor,
                            config.message_log.compaction_factor.to_string(),
                            &mut changed,
                        );
                        note_row(ui, theme, "Compact above maxSize × factor.");
                        duration_row(
                            ui,
                            theme,
                            "Compact interval",
                            &mut draft.message_log.compact_interval,
                            config.message_log.compact_interval,
                            &mut changed,
                        );
                        note_row(ui, theme, "Compact after this age even if the size threshold is not reached.");
                        corrupt_row(
                            ui,
                            theme,
                            "On corrupt",
                            &mut draft.message_log.on_corrupt,
                            config.message_log.on_corrupt,
                            &mut changed,
                            "config-message-log-corrupt",
                        );
                        note_row(ui, theme, "Ignore replaces a corrupt file; Fail refuses startup.");
                    });
                });
            });

            section(ui, theme, "Task engine", jump_to == Some("Task engine"), |ui| {
                widgets::control_group(ui, "Safety limits", |ui| {
                    grid(ui, theme, |ui| {
                        size_row(
                            ui,
                            theme,
                            "Max trigger payload",
                            &mut draft.task_engine.max_trigger_payload_size,
                            config.task_engine.max_trigger_payload_size,
                            &mut changed,
                        );
                        note_row(
                            ui,
                            theme,
                            "Largest accepted trigger payload; the hard cap may be lower.",
                        );
                        number_row_u32(
                            ui,
                            theme,
                            "Max concurrent runs",
                            &mut draft.task_engine.max_concurrent_runs,
                            config.task_engine.max_concurrent_runs,
                            &mut changed,
                        );
                        note_row(
                            ui,
                            theme,
                            "Global cap across tasks; excess triggers are suppressed.",
                        );
                        number_row_u32(
                            ui,
                            theme,
                            "Max chain depth",
                            &mut draft.task_engine.max_chain_depth,
                            config.task_engine.max_chain_depth,
                            &mut changed,
                        );
                        note_row(
                            ui,
                            theme,
                            "Maximum next-task depth from one trigger.",
                        );
                        number_row_u32(
                            ui,
                            theme,
                            "Max fan out",
                            &mut draft.task_engine.max_fan_out,
                            config.task_engine.max_fan_out,
                            &mut changed,
                        );
                        note_row(
                            ui,
                            theme,
                            "Maximum net tasks started from one trigger fan-out.",
                        );
                        number_row_u32(
                            ui,
                            theme,
                            "Run feed rows",
                            &mut draft.task_engine.run_feed.max_rows,
                            config.task_engine.run_feed.max_rows,
                            &mut changed,
                        );
                        note_row(
                            ui,
                            theme,
                            "Recent completed runs kept in memory and returned by tasks.runFeed.",
                        );
                        size_row(
                            ui,
                            theme,
                            "Run feed size",
                            &mut draft.task_engine.run_feed.max_size,
                            config.task_engine.run_feed.max_size,
                            &mut changed,
                        );
                        note_row(
                            ui,
                            theme,
                            "Byte cap for recent runs; the oldest run is evicted first.",
                        );
                    });
                });
            });

            section(ui, theme, "Task history log", jump_to == Some("Task history log"), |ui| {
                widgets::control_group(ui, "Storage", |ui| {
                    grid(ui, theme, |ui| {
                        text_row(
                            ui,
                            theme,
                            "Directory",
                            &mut draft.task_log.dir,
                            config.task_log.dir.display(),
                            &mut changed,
                        );
                        note_row(ui, theme, "Directory for per-task history files.");
                        octal_row(
                            ui,
                            theme,
                            "File mode",
                            &mut draft.task_log.file_mode,
                            config.task_log.file_mode,
                            &mut changed,
                        );
                        note_row(ui, theme, "Unix permissions for new history files.");
                    });
                });
                widgets::control_group(ui, "Compaction", |ui| {
                    grid(ui, theme, |ui| {
                        text_row(
                            ui,
                            theme,
                            "Compaction factor",
                            &mut draft.task_log.compaction_factor,
                            config.task_log.compaction_factor.to_string(),
                            &mut changed,
                        );
                        note_row(ui, theme, "Compact above each task's history maxSize × factor.");
                        duration_row(
                            ui,
                            theme,
                            "Compact interval",
                            &mut draft.task_log.compact_interval,
                            config.task_log.compact_interval,
                            &mut changed,
                        );
                        note_row(ui, theme, "Checked on a task's next run.");
                        corrupt_row(
                            ui,
                            theme,
                            "On corrupt",
                            &mut draft.task_log.on_corrupt,
                            config.task_log.on_corrupt,
                            &mut changed,
                            "config-task-log-corrupt",
                        );
                        note_row(ui, theme, "Ignore skips a corrupt task file; Fail refuses startup.");
                    });
                });
                ui.label(
                    RichText::new(
                        "Per-task history size and row limits are configured in the task editor. These compaction settings apply to every history file.",
                    )
                    .size(13.0)
                    .color(palette(theme).muted),
                );
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

fn section(ui: &mut Ui, _theme: UiTheme, title: &str, jump: bool, body: impl FnOnce(&mut Ui)) {
    ui.add_space(4.0);
    let response = egui::CollapsingHeader::new(RichText::new(title).size(15.0).strong())
        .default_open(true)
        .open(jump.then_some(true))
        .show(ui, body);
    if jump {
        ui.scroll_to_rect(response.header_response.rect, Some(egui::Align::TOP));
    }
}

fn grid(ui: &mut Ui, _theme: UiTheme, body: impl FnOnce(&mut Ui)) {
    egui::Grid::new(ui.next_auto_id())
        .num_columns(2)
        .spacing([18.0, 4.0])
        .show(ui, body);
}

fn note_row(ui: &mut Ui, theme: UiTheme, note: &str) {
    ui.label("");
    ui.label(RichText::new(note).small().color(palette(theme).muted));
    ui.end_row();
}

fn field_label(theme: UiTheme, label: &str) -> RichText {
    widgets::field_label_text(theme, label)
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
    let value = value.trim();
    for unit in [
        TimeUnit::Weeks,
        TimeUnit::Days,
        TimeUnit::Hours,
        TimeUnit::Milliseconds,
        TimeUnit::Microseconds,
        TimeUnit::Nanoseconds,
        TimeUnit::Minutes,
        TimeUnit::Seconds,
    ] {
        if let Some(amount) = value.strip_suffix(unit.suffix())
            && let Ok(amount) = amount.trim().parse::<u64>()
        {
            return Some(TimeParts { amount, unit });
        }
    }
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
    let value = value.trim();
    for unit in [
        SizeUnit::Tebibytes,
        SizeUnit::Gibibytes,
        SizeUnit::Mebibytes,
        SizeUnit::Kibibytes,
        SizeUnit::Bytes,
    ] {
        if let Some(amount) = value.strip_suffix(unit.suffix())
            && let Ok(amount) = amount.trim().parse::<u64>()
        {
            return Some(SizeParts { amount, unit });
        }
    }
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
            parts.unit = unit;
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
            parts.unit = unit;
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

fn corrupt_row(
    ui: &mut Ui,
    theme: UiTheme,
    label: &str,
    value: &mut CorruptPolicy,
    previous: CorruptPolicy,
    changed: &mut bool,
    id_salt: &str,
) {
    ui.label(field_label(theme, label));
    ui.horizontal(|ui| {
        let selection_changed = ComboBox::from_id_salt(id_salt)
            .selected_text(corrupt_label(*value))
            .show_ui(ui, |ui| {
                let mut changed = false;
                changed |= ui
                    .selectable_value(value, CorruptPolicy::Ignore, "Ignore")
                    .changed();
                changed |= ui
                    .selectable_value(value, CorruptPolicy::Fail, "Fail")
                    .changed();
                changed
            })
            .inner
            .unwrap_or(false);
        *changed |= selection_changed;
        if previous_hint(ui, theme, *value != previous, corrupt_label(previous)) {
            *value = previous;
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
