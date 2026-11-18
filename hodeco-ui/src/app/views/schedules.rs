//! Schedules list and full-page schedule editor.

use super::super::App;
use crate::ui::widgets;
use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use hodeco_contract::domain::{
    DateRange, Location, LoopRule, ScheduleDate, ScheduleDefinition, ScheduleInfo,
    ScheduleOccurrence, ScheduleRule, TimeWindow,
};
use hodeco_contract::utils::HumanDuration;
use std::str::FromStr;

#[derive(Debug, Default)]
pub(crate) struct SchedulesUi {
    pub schedules: Vec<ScheduleInfo>,
    pub loading: bool,
    pub error: Option<String>,
    pub search: String,
    pub selected: Option<usize>,
    pub confirm_delete: Option<String>,
    pub editor: Option<ScheduleEditor>,
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

#[derive(Debug)]
pub(crate) struct ScheduleEditor {
    pub draft: ScheduleDefinition,
    pub original: Option<ScheduleDefinition>,
    pub initial: ScheduleDefinition,
    pub is_new: bool,
    pub timezone: String,
    pub location: String,
    pub from: String,
    pub until: String,
    pub jitter: String,
    pub on_json: String,
    pub loop_json: String,
    pub allow: String,
    pub deny: String,
    pub except: String,
    pub error: Option<String>,
    pub saving: bool,
    pub confirm_delete: bool,
    pub preview: Vec<ScheduleOccurrence>,
    pub preview_error: Option<String>,
    pub previewing: bool,
    pub preview_generation: u64,
}

impl ScheduleEditor {
    pub fn new(schedule: ScheduleDefinition, is_new: bool) -> Self {
        let timezone = schedule.timezone.clone().unwrap_or_default();
        let location = schedule
            .location
            .map(|location| match location.elevation {
                Some(elevation) => {
                    format!(
                        "{}, {}, {}",
                        location.latitude, location.longitude, elevation
                    )
                }
                None => format!("{}, {}", location.latitude, location.longitude),
            })
            .unwrap_or_default();
        let from = schedule
            .from
            .map(|date| date.to_string())
            .unwrap_or_default();
        let until = schedule
            .until
            .map(|date| date.to_string())
            .unwrap_or_default();
        let jitter = schedule
            .jitter
            .map(|jitter| jitter.to_string())
            .unwrap_or_default();
        let on_json = schedule
            .on
            .as_ref()
            .map(|rule| serde_json::to_string_pretty(rule).unwrap_or_default())
            .unwrap_or_default();
        let loop_json = schedule
            .loop_rule
            .as_ref()
            .map(|rule| serde_json::to_string_pretty(rule).unwrap_or_default())
            .unwrap_or_default();
        let allow = schedule
            .allow
            .iter()
            .map(window_text)
            .collect::<Vec<_>>()
            .join("\n");
        let deny = schedule
            .deny
            .iter()
            .map(window_text)
            .collect::<Vec<_>>()
            .join("\n");
        let except = schedule
            .except
            .iter()
            .map(range_text)
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            draft: schedule.clone(),
            original: (!is_new).then_some(schedule.clone()),
            initial: schedule,
            is_new,
            timezone,
            location,
            from,
            until,
            jitter,
            on_json,
            loop_json,
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
        schedule.timezone = nonempty(&self.timezone);
        schedule.location = parse_location(&self.location)?;
        schedule.from = parse_date(&self.from)?;
        schedule.until = parse_date(&self.until)?;
        schedule.jitter = parse_duration(&self.jitter)?;
        schedule.on = if self.on_json.trim().is_empty() {
            None
        } else {
            Some(
                serde_json::from_str::<ScheduleRule>(&self.on_json)
                    .map_err(|error| format!("Schedule rule JSON: {error}"))?,
            )
        };
        schedule.loop_rule = if self.loop_json.trim().is_empty() {
            None
        } else {
            Some(
                serde_json::from_str::<LoopRule>(&self.loop_json)
                    .map_err(|error| format!("Loop JSON: {error}"))?,
            )
        };
        schedule.allow = parse_windows(&self.allow)?.into();
        schedule.deny = parse_windows(&self.deny)?.into();
        schedule.except = parse_ranges(&self.except)?.into();
        Ok(schedule)
    }

    pub fn is_dirty(&self) -> bool {
        let original = self.original.as_ref().unwrap_or(&self.initial);
        match self.build() {
            Ok(schedule) => &schedule != original,
            Err(_) => true,
        }
    }
}

fn window_text(window: &TimeWindow) -> String {
    format!(
        "{}~{}",
        window.from.format("%H:%M"),
        window.to.format("%H:%M")
    )
}

fn range_text(range: &DateRange) -> String {
    if range.from == range.to {
        range.from.to_string()
    } else {
        format!("{}..{}", range.from, range.to)
    }
}

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    if app.schedules.editor.is_some() {
        editor_ui(app, ui);
    } else {
        list_ui(app, ui);
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
        let last = schedules.len().saturating_sub(1);
        let next = match nav {
            widgets::ListNav::Up => selected.map_or(0, |current| current.saturating_sub(1)),
            widgets::ListNav::Down => selected.map_or(0, |current| (current + 1).min(last)),
            widgets::ListNav::PageUp => selected.map_or(0, |current| current.saturating_sub(10)),
            widgets::ListNav::PageDown => selected.map_or(0, |current| (current + 10).min(last)),
            widgets::ListNav::Home => 0,
            widgets::ListNav::End => last,
            widgets::ListNav::Activate => selected.unwrap_or(0),
        };
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
        egui::Window::new("Delete schedule")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ui.ctx(), |ui| {
                ui.label(format!("Delete schedule '{schedule_id}'?"));
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        app.delete_schedule(&schedule_id);
                    }
                    if ui.button("Cancel").clicked() {
                        app.schedules.confirm_delete = None;
                    }
                });
            });
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
    let used_by = app
        .schedules
        .schedules
        .iter()
        .find(|info| info.schedule.id == editor.draft.id)
        .map(|info| info.used_by.clone())
        .unwrap_or_default();
    let can_delete = !editor.is_new && used_by.is_empty();

    ui.horizontal(|ui| {
        if ui.button("← Schedules").clicked() {
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
                .button("Close")
                .on_hover_text("Close the editor; unsaved changes are protected")
                .clicked()
            {
                close = true;
            }
            ui.add_space(18.0);
            if ui
                .add_enabled(ready && !editor.saving, egui::Button::new("Save"))
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
        });
    });
    ui.separator();

    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(500.0);
            ui.spacing_mut().text_edit_width = 300.0;
            egui::ScrollArea::vertical()
                .id_salt("schedule-form-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("schedule-general")
                        .num_columns(2)
                        .spacing([10.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Name");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "Display name in the schedules list.",
                                |ui| {
                                    ui.add(egui::TextEdit::singleline(
                                        &mut editor.draft.name,
                                    ));
                                },
                            );
                            ui.end_row();
                            ui.label("ID");
                            if editor.is_new {
                                widgets::field_with_note(
                                    ui,
                                    theme,
                                    "Generated from the name when left empty; immutable after creation.",
                                    |ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut editor.draft.id)
                                                .hint_text("generated from the name"),
                                        );
                                    },
                                );
                            } else {
                                ui.label(RichText::new(&editor.draft.id).monospace());
                            }
                            ui.end_row();
                            ui.label("Description");
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
                            ui.label("Timezone");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "IANA zone; inherits the schedules.yaml default.",
                                |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut editor.timezone)
                                            .hint_text("inherit default"),
                                    );
                                },
                            );
                            ui.end_row();
                            ui.label("Location");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "Latitude, longitude, optional elevation; required for solar events.",
                                |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut editor.location)
                                            .hint_text("lat, lon, elevation"),
                                    );
                                },
                            );
                            ui.end_row();
                            ui.label("From");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "Earliest local date or timestamp; anchor for on.every.",
                                |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut editor.from)
                                            .hint_text("2026-01-01 or timestamp"),
                                    );
                                },
                            );
                            ui.end_row();
                            ui.label("Until");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "Exclusive end date or timestamp.",
                                |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut editor.until)
                                            .hint_text("exclusive"),
                                    );
                                },
                            );
                            ui.end_row();
                            ui.label("Jitter");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "Deterministic ± offset, clamped to from/until.",
                                |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut editor.jitter)
                                            .hint_text("e.g. 2m"),
                                    );
                                },
                            );
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    ui.label(RichText::new("Rule (JSON)").strong());
                    ui.label(
                        RichText::new("at entries accept 09:00, :15, or \"sunset - 15m\".").small(),
                    );
                    ui.add(
                        egui::TextEdit::multiline(&mut editor.on_json)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(12)
                            .code_editor(),
                    );
                    ui.add_space(8.0);
                    ui.label(RichText::new("Loop (JSON)").strong());
                    ui.label(
                        RichText::new(
                            "Optional extra attempts after each occurrence; repeat is additional to the base occurrence.",
                        )
                        .small(),
                    );
                    ui.add(
                        egui::TextEdit::multiline(&mut editor.loop_json)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(5)
                            .code_editor(),
                    );
                    ui.add_space(8.0);
                    for (label, note, value) in [
                        (
                            "Allow windows",
                            "Inclusive local-time windows; leave empty to allow all times.",
                            &mut editor.allow,
                        ),
                        (
                            "Deny windows",
                            "Inclusive local-time windows; deny takes precedence over allow.",
                            &mut editor.deny,
                        ),
                        (
                            "Exceptions",
                            "Dates or inclusive ranges that skip occurrences, one per line.",
                            &mut editor.except,
                        ),
                    ] {
                        ui.label(RichText::new(label).strong());
                        ui.label(RichText::new(note).small());
                        ui.add(
                            egui::TextEdit::multiline(value)
                                .hint_text("one per line")
                                .desired_rows(2)
                                .code_editor(),
                        );
                    }
                    if let Some(error) = &editor.error {
                        ui.add_space(8.0);
                        ui.label(RichText::new(error).color(palette.red));
                    }
                });
        });
        ui.separator();
        ui.vertical(|ui| {
            ui.set_width(360.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Preview").strong());
                if ui
                    .add_enabled(ready && !editor.previewing, egui::Button::new("Refresh"))
                    .clicked()
                {
                    preview = true;
                }
                if editor.previewing {
                    ui.spinner();
                }
            });
            ui.add_space(4.0);
            if let Some(error) = &editor.preview_error {
                ui.label(RichText::new(error).color(palette.red));
            }
            egui::ScrollArea::vertical()
                .id_salt("schedule-preview-scroll")
                .auto_shrink([false, false])
                .max_height(460.0)
                .show(ui, |ui| {
                    if editor.preview.is_empty() {
                        ui.label(
                            RichText::new("Press Refresh to preview the next occurrences.")
                                .small()
                                .color(palette.muted),
                        );
                    }
                    for occurrence in &editor.preview {
                        let color = if occurrence.skipped {
                            palette.amber
                        } else {
                            palette.text
                        };
                        ui.label(RichText::new(&occurrence.local).small().color(color));
                        if let Some(reason) = &occurrence.reason {
                            ui.label(RichText::new(reason).small().color(palette.muted));
                        }
                    }
                });
            ui.add_space(8.0);
            ui.label(RichText::new("Used by").strong());
            if used_by.is_empty() {
                ui.label(RichText::new("—").color(palette.muted));
            }
            for task in &used_by {
                ui.label(task);
            }
        });
    });

    if save {
        match editor.build() {
            Ok(schedule) => {
                editor.error = None;
                editor.saving = true;
                if !app.save_schedule(schedule, editor.is_new) {
                    editor.saving = false;
                    editor.error = Some("The gateway is not connected.".to_string());
                }
            }
            Err(error) => editor.error = Some(error),
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
            Err(error) => editor.preview_error = Some(error),
        }
    }
    if editor.confirm_delete {
        egui::Window::new("Delete schedule")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "Delete schedule '{}'? It is still used by tasks.",
                    editor.draft.id
                ));
                ui.horizontal(|ui| {
                    if ui.button("Delete anyway").clicked() {
                        delete = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel_delete = true;
                    }
                });
            });
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
                Some("Save, discard, or cancel the changes before leaving the editor.".to_string());
        } else {
            keep = false;
        }
    }
    if keep {
        app.schedules.editor = Some(editor);
    }
    let _ = theme;
}

fn summary(schedule: &ScheduleDefinition) -> String {
    let mut parts = Vec::new();
    if let Some(rule) = &schedule.on {
        let at = rule
            .at
            .iter()
            .map(|entry| entry.display())
            .collect::<Vec<_>>()
            .join(", ");
        if !at.is_empty() {
            parts.push(at);
        }
        if let Some(every) = rule.every {
            parts.push(format!("every {}", every_label(every)));
        }
        if !rule.days.is_empty() {
            parts.push(
                rule.days
                    .iter()
                    .map(|day| day.display())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
    }
    if schedule.on.is_none() {
        parts.push("one-shot".to_string());
    }
    if schedule.loop_rule.is_some() {
        parts.push("loop".to_string());
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

fn parse_location(value: &str) -> Result<Option<Location>, String> {
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

fn parse_date(value: &str) -> Result<Option<ScheduleDate>, String> {
    let value = value.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        ScheduleDate::parse(value).map(Some)
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

fn parse_windows(value: &str) -> Result<Vec<TimeWindow>, String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(TimeWindow::parse)
        .collect()
}

fn parse_ranges(value: &str) -> Result<Vec<DateRange>, String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(DateRange::parse)
        .collect()
}
