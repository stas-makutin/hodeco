//! Tasks list, full-page task editor, and run-now dialog.

use super::super::{App, TasksTab};
use crate::model;
use crate::ui::widgets;
use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use hodeco_contract::domain::{TaskDefinition, TaskInfo, TaskTrigger, TriggerType};
use hodeco_contract::utils::{Base64Bytes, HumanDuration, Size, id_from_name};
use serde_json::Value;
use std::str::FromStr;

#[derive(Debug)]
pub(crate) struct TasksUi {
    pub tasks: Vec<TaskInfo>,
    pub loading: bool,
    pub error: Option<String>,
    pub search: String,
    pub show_disabled: bool,
    pub selected: Option<usize>,
    pub editor: Option<TaskEditor>,
    pub confirm_delete: Option<String>,
    pub firing: bool,
}

impl TasksUi {
    pub fn task(&self, id: &str) -> Option<&TaskInfo> {
        self.tasks.iter().find(|task| task.task.id == id)
    }

    pub fn filtered(&self) -> Vec<&TaskInfo> {
        let needle = self.search.trim().to_ascii_lowercase();
        self.tasks
            .iter()
            .filter(|info| self.show_disabled || info.task.enabled)
            .filter(|info| {
                needle.is_empty()
                    || info.task.name.to_ascii_lowercase().contains(&needle)
                    || info.task.id.contains(&needle)
                    || info.task.description.to_ascii_lowercase().contains(&needle)
                    || info
                        .task
                        .triggers
                        .iter()
                        .any(|trigger| trigger.id().to_ascii_lowercase().contains(&needle))
            })
            .collect()
    }

    pub fn editor_dirty(&self) -> bool {
        self.editor.as_ref().is_some_and(TaskEditor::is_dirty)
    }
}

impl Default for TasksUi {
    fn default() -> Self {
        Self {
            tasks: Vec::new(),
            loading: false,
            error: None,
            search: String::new(),
            show_disabled: true,
            selected: None,
            editor: None,
            confirm_delete: None,
            firing: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskSection {
    General,
    Triggers,
    Actions,
    Next,
    Limits,
}

impl TaskSection {
    const ALL: [Self; 5] = [
        Self::General,
        Self::Triggers,
        Self::Actions,
        Self::Next,
        Self::Limits,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Triggers => "Triggers",
            Self::Actions => "Actions",
            Self::Next => "Next",
            Self::Limits => "Limits & History",
        }
    }
}

#[derive(Debug)]
pub(crate) struct TaskEditor {
    pub draft: TaskDefinition,
    pub original: Option<TaskDefinition>,
    pub initial: TaskDefinition,
    pub is_new: bool,
    pub id_auto: bool,
    pub section: TaskSection,
    pub triggers_json: String,
    pub actions_json: String,
    pub next_json: String,
    pub run_timeout: String,
    pub history_size: String,
    pub error: Option<String>,
    pub saving: bool,
    pub confirm_delete: bool,
    pub run_now: Option<RunNow>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RunNow {
    pub payload_mode: PayloadMode,
    pub payload: String,
    pub metadata: String,
    pub error: Option<String>,
    pub sending: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PayloadMode {
    #[default]
    None,
    Text,
    Hex,
    Base64,
}

impl PayloadMode {
    const ALL: [Self; 4] = [Self::None, Self::Text, Self::Hex, Self::Base64];

    const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Text => "Text",
            Self::Hex => "Hex",
            Self::Base64 => "Base64",
        }
    }
}

impl TaskEditor {
    pub fn new(task: TaskDefinition, is_new: bool) -> Self {
        let triggers_json =
            serde_json::to_string_pretty(&task.triggers).unwrap_or_else(|_| "[]".into());
        let actions_json =
            serde_json::to_string_pretty(&task.actions).unwrap_or_else(|_| "[]".into());
        let next_json = serde_json::to_string_pretty(&task.next).unwrap_or_else(|_| "[]".into());
        let run_timeout = task.run_timeout.to_string();
        let history_size = task.history.max_size.to_string();
        Self {
            draft: task.clone(),
            original: (!is_new).then_some(task.clone()),
            initial: task,
            is_new,
            id_auto: is_new,
            section: TaskSection::General,
            triggers_json,
            actions_json,
            next_json,
            run_timeout,
            history_size,
            error: None,
            saving: false,
            confirm_delete: false,
            run_now: None,
        }
    }

    pub fn build(&self) -> Result<TaskDefinition, String> {
        let mut task = self.draft.clone();
        task.name = task.name.trim().to_string();
        if task.name.is_empty() {
            return Err("Name must not be empty.".to_string());
        }
        task.triggers = serde_json::from_str::<Vec<TaskTrigger>>(&self.triggers_json)
            .map_err(|error| format!("Triggers JSON: {error}"))?;
        task.actions = serde_json::from_str(&self.actions_json)
            .map_err(|error| format!("Actions JSON: {error}"))?;
        task.next = serde_json::from_str(&self.next_json)
            .map_err(|error| format!("Next rules JSON: {error}"))?;
        task.run_timeout =
            HumanDuration::from_str(self.run_timeout.trim()).map_err(|error| error.to_string())?;
        if task.run_timeout.as_std() < std::time::Duration::from_secs(1) {
            return Err("Run timeout must be at least 1 second.".to_string());
        }
        if task.history.max_rows > 0 {
            task.history.max_size =
                Size::from_str(self.history_size.trim()).map_err(|error| error.to_string())?;
            if task.history.max_size < Size::from_bytes(1024) {
                return Err("History size must be at least 1 KiB.".to_string());
            }
        } else {
            task.history.max_size = Size::from_bytes(0);
        }
        Ok(task)
    }

    pub fn is_dirty(&self) -> bool {
        let original = self.original.as_ref().unwrap_or(&self.initial);
        match self.build() {
            Ok(task) => &task != original,
            Err(_) => true,
        }
    }
}

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    if app.tasks.editor.is_some() {
        editor_ui(app, ui);
        return;
    }
    list_ui(app, ui);
}

fn list_ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let live = app.gateway.phase().is_live();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let filtered = app.tasks.filtered();
    let total = app.tasks.tasks.len();
    let running = app.tasks.tasks.iter().filter(|task| task.running).count();
    let disabled = app
        .tasks
        .tasks
        .iter()
        .filter(|task| !task.task.enabled)
        .count();
    let filtered_count = filtered.len();
    let rows: Vec<TaskInfo> = filtered.into_iter().cloned().collect();
    let mut selected = app.tasks.selected.filter(|index| *index < rows.len());

    widgets::page_header(
        ui,
        theme,
        "Tasks",
        Some(&format!("{} configured", total)),
        |ui| {
            if ui.add_enabled(live, egui::Button::new("Refresh")).clicked() {
                app.refresh_tasks();
            }
            if ui
                .add_enabled(ready, egui::Button::new("Add task"))
                .clicked()
            {
                app.open_task_editor(None);
            }
        },
    );

    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.tasks.search)
                .hint_text("Search tasks…")
                .desired_width(360.0),
        );
        ui.add_space(16.0);
        ui.checkbox(&mut app.tasks.show_disabled, "Show disabled");
        ui.add_space(8.0);
        if app.tasks.loading {
            ui.spinner();
        }
        if let Some(error) = &app.tasks.error {
            ui.label(RichText::new(error).color(palette.red));
        }
    });
    ui.add_space(2.0);

    let status = if total == 0 {
        "0 tasks".to_string()
    } else if filtered_count == 0 {
        format!("0 of {total} tasks match")
    } else {
        format!("{total} tasks · {running} running · {disabled} disabled")
    };
    ui.label(RichText::new(status).color(palette.muted));
    ui.add_space(4.0);

    let nav = if app.dialog.is_none() && !rows.is_empty() {
        widgets::list_nav_global(ui)
    } else {
        None
    };
    let list_focus = widgets::list_focus(ui, theme, "tasks-list", nav.is_some(), false);
    let focus_row_actions = widgets::list_tab_pressed(ui, list_focus.id);
    let action_row = selected.unwrap_or(0);
    if focus_row_actions {
        selected = Some(action_row);
    }
    let mut action_focus_id = None;
    let mut activate = None;
    if let Some(nav) = nav {
        let last = rows.len().saturating_sub(1);
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
            activate = rows.get(next).map(|info| info.task.clone());
        }
    }
    if list_focus.focused
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    {
        activate = rows
            .get(selected.unwrap_or(0))
            .map(|info| info.task.clone());
    }
    let mut clicked_selection = None;
    let scroll_to = nav
        .map(|_| selected.unwrap_or(0))
        .or_else(|| focus_row_actions.then_some(action_row));

    egui::ScrollArea::horizontal()
        .id_salt("tasks-horizontal")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.set_min_width(980.0);
            let mut table = TableBuilder::new(ui)
                .id_salt("tasks-table")
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::CLICK)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(170.0).at_least(140.0))
                .column(Column::initial(65.0).at_least(60.0))
                .column(Column::initial(200.0).at_least(160.0))
                .column(Column::initial(150.0).at_least(125.0))
                .column(Column::initial(150.0).at_least(125.0))
                .column(Column::remainder().at_least(260.0));
            if let Some(index) = scroll_to {
                table = table.scroll_to_row(index, Some(egui::Align::Center));
            }
            table
                .header(28.0, |mut header| {
                    for title in [
                        "Name / ID",
                        "Enabled",
                        "Triggers",
                        "Last run",
                        "Next",
                        "Actions",
                    ] {
                        header.col(|ui| {
                            let response = ui.label(RichText::new(title).strong());
                            if title == "Triggers" {
                                response.on_hover_text(
                                    "msg = message, sched = schedule, HTTP = HTTP hook",
                                );
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(42.0, rows.len(), |mut row| {
                        let index = row.index();
                        let info = &rows[index];
                        let task = &info.task;
                        row.set_selected(selected == Some(index));
                        row.col(|ui| {
                            ui.vertical(|ui| {
                                let name = if task.enabled {
                                    RichText::new(&task.name).strong()
                                } else {
                                    RichText::new(&task.name).strong().color(palette.muted)
                                };
                                ui.label(name);
                                ui.label(RichText::new(&task.id).small().color(palette.muted));
                            });
                        });
                        row.col(|ui| {
                            let mut enabled = task.enabled;
                            if ui
                                .add_enabled(ready, egui::Checkbox::without_text(&mut enabled))
                                .on_hover_text("Enable or disable this task")
                                .changed()
                            {
                                app.set_task_enabled(task, enabled);
                            }
                        });
                        row.col(|ui| {
                            let mut message = 0;
                            let mut schedule = 0;
                            let mut http = 0;
                            for trigger in &task.triggers {
                                match trigger.kind() {
                                    TriggerType::Message => message += 1,
                                    TriggerType::Schedule => schedule += 1,
                                    TriggerType::Http => http += 1,
                                    TriggerType::Manual | TriggerType::Task => {}
                                }
                            }
                            let mut parts = Vec::new();
                            if message > 0 {
                                parts.push(format!("{message} message"));
                            }
                            if schedule > 0 {
                                parts.push(format!("{schedule} schedule"));
                            }
                            if http > 0 {
                                parts.push(format!("{http} HTTP"));
                            }
                            let summary = if parts.is_empty() {
                                "none".to_string()
                            } else {
                                parts.join(" · ")
                            };
                            let details = task
                                .triggers
                                .iter()
                                .map(|trigger| {
                                    format!("{}: {}", trigger.kind().label(), trigger.id())
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            let response = ui.label(RichText::new(summary).small());
                            if !details.is_empty() {
                                response.on_hover_text(details);
                            }
                        });
                        row.col(|ui| {
                            if let Some(last) = &info.last_run {
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(model::format_datetime(last.time)).small(),
                                    );
                                    let color = match last.outcome {
                                        hodeco_contract::domain::RunOutcome::Succeeded => {
                                            palette.green
                                        }
                                        hodeco_contract::domain::RunOutcome::Failed => palette.red,
                                        hodeco_contract::domain::RunOutcome::Cancelled => {
                                            palette.amber
                                        }
                                    };
                                    ui.label(
                                        RichText::new(last.outcome.label()).small().color(color),
                                    );
                                });
                            } else {
                                ui.label(RichText::new("—").color(palette.muted));
                            }
                        });
                        row.col(|ui| {
                            if let Some(next) = &info.next {
                                ui.label(RichText::new(model::format_datetime(next.at)).small());
                            } else {
                                ui.label(RichText::new("—").color(palette.muted));
                            }
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                let edit = ui.add_enabled(ready, egui::Button::new("Edit").small());
                                if index == action_row {
                                    action_focus_id = Some(edit.id);
                                }
                                if edit.clicked() {
                                    app.open_task_editor(Some(task.clone()));
                                }
                                if ui
                                    .add_enabled(
                                        ready && task.enabled && !info.running && !app.tasks.firing,
                                        egui::Button::new("Run").small(),
                                    )
                                    .on_hover_text("Run now")
                                    .clicked()
                                {
                                    app.fire_task(task);
                                }
                                if ui
                                    .add_enabled(ready, egui::Button::new("History").small())
                                    .on_hover_text("Open run history")
                                    .clicked()
                                {
                                    app.open_runs_for_task(&task.id);
                                }
                                if ui
                                    .add_enabled(ready, egui::Button::new("Delete").small())
                                    .on_hover_text("Delete task and history")
                                    .clicked()
                                {
                                    app.tasks.confirm_delete = Some(task.id.clone());
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
    app.tasks.selected = selected;
    if let Some(task) = activate {
        app.open_task_editor(Some(task));
    }

    if rows.is_empty() {
        let (title, detail) = if total == 0 {
            (
                "No tasks",
                "Create a task to automate commands and messages.",
            )
        } else {
            (
                "No tasks match the current filter",
                "Change the search or show disabled tasks.",
            )
        };
        let width = ui.available_width().max(200.0);
        egui::Frame::new()
            .fill(palette.panel)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 64.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        ui.add_space(10.0);
                        ui.label(RichText::new(title).color(palette.muted));
                        ui.label(RichText::new(detail).small().color(palette.muted));
                    },
                );
            });
    }

    if let Some(task_id) = app.tasks.confirm_delete.clone() {
        confirm_delete_dialog(app, ui, &task_id);
    }
}

fn confirm_delete_dialog(app: &mut App, ui: &mut Ui, task_id: &str) {
    let palette = app.palette();
    egui::Window::new("Delete task")
        .collapsible(false)
        .resizable(false)
        .min_width(280.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ui.ctx(), |ui| {
            ui.vertical_centered(|ui| {
                ui.label(format!("Delete task '{task_id}'?"));
                ui.label("This cannot be undone.");
                ui.label(
                    RichText::new("The task history file is removed too.")
                        .small()
                        .color(palette.muted),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let pad = ((ui.available_width() - 150.0) * 0.5).max(0.0);
                    ui.add_space(pad);
                    if ui.button("Delete").clicked() {
                        app.delete_task(task_id);
                    }
                    if ui.button("Cancel").clicked() {
                        app.tasks.confirm_delete = None;
                    }
                });
            });
        });
}

fn editor_ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let Some(mut editor) = app.tasks.editor.take() else {
        return;
    };
    let mut keep = true;
    let mut close = false;
    let mut save = false;
    let mut delete = false;
    let mut discard = false;
    let mut cancel_delete = false;
    let limits_valid = limits_valid(&editor);

    ui.horizontal(|ui| {
        if ui.button("← Tasks").clicked() {
            close = true;
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new(if editor.is_new {
                "New task"
            } else {
                "Edit task"
            })
            .size(18.0)
            .strong(),
        );
        if !editor.is_new {
            ui.label(RichText::new(&editor.draft.id).small().color(palette.muted));
            if let Some(info) = app.tasks.task(&editor.draft.id).cloned() {
                if info.running {
                    ui.spinner();
                    ui.label("Running");
                }
                if let Some(last) = &info.last_run {
                    ui.label(
                        RichText::new(format!("Last {}", model::format_datetime(last.time)))
                            .small()
                            .color(palette.muted),
                    );
                }
            }
        }
        if !editor.is_new {
            ui.add_space(8.0);
            if ui
                .add_enabled(ready, egui::Button::new("Run now"))
                .clicked()
            {
                editor.run_now = Some(RunNow::default());
            }
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
                .add_enabled(
                    ready && !editor.saving && limits_valid,
                    egui::Button::new("Save"),
                )
                .on_disabled_hover_text("Fix the limits fields before saving")
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
            if !editor.is_new
                && ui
                    .add_enabled(
                        ready,
                        egui::Button::new(RichText::new("Delete task").color(palette.red)),
                    )
                    .clicked()
            {
                editor.confirm_delete = true;
            }
        });
    });
    ui.separator();

    ui.horizontal_top(|ui| {
        ui.set_min_width(ui.available_width());
        ui.vertical(|ui| {
            ui.set_width(210.0);
            for section in TaskSection::ALL {
                if ui
                    .add_sized(
                        [ui.available_width(), 24.0],
                        egui::Button::selectable(editor.section == section, section.label()),
                    )
                    .clicked()
                {
                    editor.section = section;
                }
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("task-editor-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_width(640.0);
                ui.vertical(|ui| {
                    match editor.section {
                        TaskSection::General => general_section(app, ui, &mut editor),
                        TaskSection::Triggers => json_section(
                            ui,
                            "Triggers",
                            "JSON array of message, schedule, and HTTP triggers. IDs must be unique.",
                            &mut editor.triggers_json,
                        ),
                        TaskSection::Actions => json_section(
                            ui,
                            "Actions",
                            "JSON array of command and message actions. Actions run sequentially.",
                            &mut editor.actions_json,
                        ),
                        TaskSection::Next => json_section(
                            ui,
                            "Next rules",
                            "JSON array of next rules. command-result uses cases keyed by exit code 0–254.",
                            &mut editor.next_json,
                        ),
                        TaskSection::Limits => limits_section(app, ui, &mut editor),
                    }
                    if let Some(error) = &editor.error {
                        ui.add_space(8.0);
                        ui.label(RichText::new(error).color(palette.red));
                    }
                });
            });
    });

    if save {
        match editor.build() {
            Ok(task) => {
                editor.error = None;
                editor.saving = true;
                if !app.save_task(task, editor.is_new) {
                    editor.saving = false;
                    editor.error = Some("The gateway is not connected.".to_string());
                }
            }
            Err(error) => editor.error = Some(error),
        }
    }
    if editor.run_now.is_some() {
        let task_id = editor.draft.id.clone();
        run_now_dialog(app, ui, &task_id, &mut editor.run_now);
    }
    if editor.confirm_delete {
        egui::Window::new("Delete task")
            .collapsible(false)
            .resizable(false)
            .min_width(280.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ui.ctx(), |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(format!("Delete task '{}'?", editor.draft.id));
                    ui.label("This cannot be undone.");
                    ui.label(
                        RichText::new("The task history file is removed too.")
                            .small()
                            .color(palette.muted),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let pad = ((ui.available_width() - 150.0) * 0.5).max(0.0);
                        ui.add_space(pad);
                        if ui.button("Delete").clicked() {
                            delete = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel_delete = true;
                        }
                    });
                });
            });
    }
    if delete {
        let id = editor.draft.id.clone();
        app.delete_task(&id);
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
        app.tasks.editor = Some(editor);
    }
    let _ = theme;
}

fn general_section(app: &mut App, ui: &mut Ui, editor: &mut TaskEditor) {
    let palette = app.palette();
    egui::Grid::new("task-general-grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Name");
            widgets::field_with_note(
                ui,
                app.theme,
                "Display name shown in the task list.",
                |ui| {
                    let name_response = ui.add(
                        egui::TextEdit::singleline(&mut editor.draft.name)
                            .desired_width(ui.available_width().min(420.0)),
                    );
                    if name_response.changed() && editor.is_new && editor.id_auto {
                        editor.draft.id = id_from_name("task", editor.draft.name.trim());
                    }
                },
            );
            ui.end_row();

            ui.label("ID");
            if editor.is_new {
                ui.vertical(|ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut editor.draft.id)
                            .hint_text("generated from the name")
                            .desired_width(260.0),
                    );
                    if response.changed() {
                        editor.id_auto = editor.draft.id.trim().is_empty();
                    }
                    if editor.id_auto && editor.draft.id.trim().is_empty() {
                        editor.draft.id = id_from_name("task", editor.draft.name.trim());
                    }
                    ui.label(
                        RichText::new(if editor.id_auto {
                            "Auto-generated from the name; edit to override."
                        } else {
                            "Custom ID; it cannot be changed after creation."
                        })
                        .small()
                        .color(palette.muted),
                    );
                });
            } else {
                ui.label(RichText::new(&editor.draft.id).monospace());
            }
            ui.end_row();

            ui.label("Description");
            widgets::field_with_note(
                ui,
                app.theme,
                "Optional notes; included in task-list search.",
                |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut editor.draft.description)
                            .desired_rows(3)
                            .desired_width(420.0),
                    );
                },
            );
            ui.end_row();

            ui.label("Enabled");
            widgets::field_with_note(ui, app.theme, "Allow triggers to start this task.", |ui| {
                ui.add(egui::Checkbox::without_text(&mut editor.draft.enabled));
            });
            ui.end_row();
        });
    ui.add_space(8.0);
    ui.label(
        RichText::new("Triggers, actions, and next rules are edited as JSON below.")
            .small()
            .color(palette.muted),
    );
}

fn json_section(ui: &mut Ui, title: &str, detail: &str, value: &mut String) {
    ui.label(RichText::new(title).strong());
    ui.label(RichText::new(detail).small());
    ui.add_space(4.0);
    ui.add(
        egui::TextEdit::multiline(value)
            .font(egui::TextStyle::Monospace)
            .desired_rows(24)
            .desired_width(f32::INFINITY)
            .code_editor(),
    );
}

fn limits_valid(editor: &TaskEditor) -> bool {
    let duration_ok = HumanDuration::from_str(editor.run_timeout.trim())
        .is_ok_and(|duration| duration.as_std() >= std::time::Duration::from_secs(1));
    let history_ok = editor.draft.history.max_rows == 0
        || Size::from_str(editor.history_size.trim())
            .is_ok_and(|size| size >= Size::from_bytes(1024));
    duration_ok && history_ok
}

fn limits_section(app: &mut App, ui: &mut Ui, editor: &mut TaskEditor) {
    let palette = app.palette();
    let mut keep_history = editor.draft.history.max_rows > 0;
    egui::Grid::new("task-limits-grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Fail fast");
            ui.vertical(|ui| {
                ui.checkbox(
                    &mut editor.draft.fail_fast,
                    "Stop after the first failed action",
                );
                ui.label(
                    RichText::new(
                        "Unchecked: every action runs even if an earlier one fails; the run still reports failed.",
                    )
                    .small()
                    .color(palette.muted),
                );
            });
            ui.end_row();

            ui.label("Run timeout");
            widgets::field_with_note(ui, app.theme,
                "Maximum time for the whole run; child processes are cancelled.",
                |ui| {
                    duration_editor(
                        ui,
                        &mut editor.run_timeout,
                        std::time::Duration::from_secs(1),
                    );
                },
            );
            ui.end_row();

            ui.label("Runs per minute");
            widgets::field_with_note(ui, app.theme,
                "Rate limit for starts; extra triggers are suppressed, not queued.",
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut editor.draft.max_runs_per_minute)
                            .range(1..=10_000),
                    );
                },
            );
            ui.end_row();
        });

    ui.add_space(10.0);
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            // Frame inherits the parent layout; indent only works in a
            // top-down layout, so establish one explicitly.
            ui.vertical(|ui| {
                if ui
                    .checkbox(&mut keep_history, "Enable run history")
                    .on_hover_text("Write completed runs to tasks/<id>.tlog")
                    .changed()
                {
                    if keep_history {
                        if editor.draft.history.max_rows == 0 {
                            editor.draft.history.max_rows = 1000;
                        }
                        if editor
                            .history_size
                            .trim()
                            .parse::<Size>()
                            .map_or(true, |size| size.as_bytes() == 0)
                        {
                            editor.history_size = "100 KiB".to_string();
                        }
                    } else {
                        editor.draft.history.max_rows = 0;
                    }
                }
                ui.indent("task-history-settings", |ui| {
                    ui.add_enabled_ui(keep_history, |ui| {
                        egui::Grid::new("task-history-grid")
                            .num_columns(2)
                            .spacing([12.0, 6.0])
                            .show(ui, |ui| {
                                ui.label("Max size");
                                widgets::field_with_note(ui, app.theme,
                                    "TLOG size limit; compacted above twice this.",
                                    |ui| {
                                        size_editor(ui, &mut editor.history_size, 1024);
                                    },
                                );
                                ui.end_row();
                                ui.label("Max rows");
                                widgets::field_with_note(ui, app.theme,
                                    "Maximum retained runs per task.",
                                    |ui| {
                                        ui.add(
                                            egui::DragValue::new(
                                                &mut editor.draft.history.max_rows,
                                            )
                                            .range(1..=100_000),
                                        );
                                    },
                                );
                                ui.end_row();
                            });
                    });
                });
                if !keep_history {
                    ui.label(
                        RichText::new(
                            "Completed runs are not written to TLOG; maxRuns still uses the in-memory trigger counter.",
                        )
                        .small()
                        .color(palette.muted),
                    );
                } else {
                    ui.label(RichText::new("Max size must be at least 1 KiB.").small());
                }
            });
        });
    ui.add_space(6.0);
    ui.label(
        RichText::new("Schedule trigger maxRuns is configured on each schedule trigger, not here.")
            .small()
            .color(palette.muted),
    );
    ui.add_space(10.0);
    if !editor.is_new
        && ui
            .button("Purge history…")
            .on_hover_text("Delete all runs for this task; maxRuns counters reset")
            .clicked()
    {
        app.purge_task_history(&editor.draft.id);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeUnit {
    Seconds,
    Minutes,
    Hours,
    Days,
    Weeks,
}

impl TimeUnit {
    const ALL: [Self; 5] = [
        Self::Seconds,
        Self::Minutes,
        Self::Hours,
        Self::Days,
        Self::Weeks,
    ];

    const fn suffix(self) -> &'static str {
        match self {
            Self::Seconds => "s",
            Self::Minutes => "m",
            Self::Hours => "h",
            Self::Days => "d",
            Self::Weeks => "w",
        }
    }

    const fn nanos(self) -> u128 {
        match self {
            Self::Seconds => 1_000_000_000,
            Self::Minutes => 60_000_000_000,
            Self::Hours => 3_600_000_000_000,
            Self::Days => 86_400_000_000_000,
            Self::Weeks => 604_800_000_000_000,
        }
    }
}

fn parse_duration_parts(value: &str) -> Option<(u64, TimeUnit)> {
    let nanos = value.parse::<HumanDuration>().ok()?.as_std().as_nanos();
    if nanos == 0 {
        return Some((0, TimeUnit::Seconds));
    }
    for unit in [
        TimeUnit::Weeks,
        TimeUnit::Days,
        TimeUnit::Hours,
        TimeUnit::Minutes,
        TimeUnit::Seconds,
    ] {
        if nanos.is_multiple_of(unit.nanos()) {
            return Some((u64::try_from(nanos / unit.nanos()).ok()?, unit));
        }
    }
    None
}

fn duration_editor(ui: &mut Ui, value: &mut String, minimum: std::time::Duration) -> bool {
    let Some((mut amount, original_unit)) = parse_duration_parts(value) else {
        let response = ui.add(
            egui::TextEdit::singleline(value)
                .hint_text("5m")
                .desired_width(160.0),
        );
        return response.changed();
    };
    let min_nanos = minimum.as_nanos();
    let mut changed = false;
    ui.horizontal(|ui| {
        let response = ui.add(
            egui::DragValue::new(&mut amount)
                .range(1..=u64::MAX)
                .speed(1.0),
        );
        let mut unit = original_unit;
        egui::ComboBox::from_id_salt(ui.next_auto_id())
            .selected_text(unit.suffix())
            .width(56.0)
            .show_ui(ui, |ui| {
                for candidate in TimeUnit::ALL {
                    ui.selectable_value(&mut unit, candidate, candidate.suffix());
                }
            });
        if unit != original_unit {
            let nanos = u128::from(amount) * original_unit.nanos();
            amount = u64::try_from(nanos / unit.nanos()).unwrap_or(0);
        }
        let mut clamped = false;
        let nanos = u128::from(amount) * unit.nanos();
        if nanos < min_nanos {
            amount = u64::try_from(min_nanos.div_ceil(unit.nanos()))
                .unwrap_or(1)
                .max(1);
            clamped = true;
        }
        if response.changed() || unit != original_unit || clamped {
            *value = format!("{amount} {}", unit.suffix());
            changed = true;
        }
    });
    changed
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

    const fn suffix(self) -> &'static str {
        match self {
            Self::Bytes => "B",
            Self::Kibibytes => "KiB",
            Self::Mebibytes => "MiB",
            Self::Gibibytes => "GiB",
            Self::Tebibytes => "TiB",
        }
    }

    const fn bytes(self) -> u64 {
        match self {
            Self::Bytes => 1,
            Self::Kibibytes => 1024,
            Self::Mebibytes => 1024 * 1024,
            Self::Gibibytes => 1024 * 1024 * 1024,
            Self::Tebibytes => 1024 * 1024 * 1024 * 1024,
        }
    }
}

fn parse_size_parts(value: &str) -> Option<(u64, SizeUnit)> {
    let bytes = value.parse::<Size>().ok()?.as_bytes();
    for unit in [
        SizeUnit::Tebibytes,
        SizeUnit::Gibibytes,
        SizeUnit::Mebibytes,
        SizeUnit::Kibibytes,
        SizeUnit::Bytes,
    ] {
        if bytes.is_multiple_of(unit.bytes()) {
            return Some((bytes / unit.bytes(), unit));
        }
    }
    None
}

fn size_editor(ui: &mut Ui, value: &mut String, minimum_bytes: u64) -> bool {
    let Some((mut amount, original_unit)) = parse_size_parts(value) else {
        let response = ui.add(
            egui::TextEdit::singleline(value)
                .hint_text("100 KiB")
                .desired_width(160.0),
        );
        return response.changed();
    };
    let mut changed = false;
    ui.horizontal(|ui| {
        let response = ui.add(
            egui::DragValue::new(&mut amount)
                .range(1..=u64::MAX)
                .speed(1.0),
        );
        let mut unit = original_unit;
        egui::ComboBox::from_id_salt(ui.next_auto_id())
            .selected_text(unit.suffix())
            .width(64.0)
            .show_ui(ui, |ui| {
                for candidate in SizeUnit::ALL {
                    ui.selectable_value(&mut unit, candidate, candidate.suffix());
                }
            });
        if unit != original_unit {
            let bytes = amount.saturating_mul(original_unit.bytes());
            amount = bytes / unit.bytes();
        }
        let mut clamped = false;
        if amount.saturating_mul(unit.bytes()) < minimum_bytes {
            amount = minimum_bytes.div_ceil(unit.bytes()).max(1);
            clamped = true;
        }
        if response.changed() || unit != original_unit || clamped {
            *value = format!("{amount} {}", unit.suffix());
            changed = true;
        }
    });
    changed
}

fn run_now_dialog(app: &mut App, ui: &mut Ui, task_id: &str, slot: &mut Option<RunNow>) {
    let Some(mut dialog) = slot.take() else {
        return;
    };
    let mut close = false;
    let mut run = false;
    egui::Window::new("Run now")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ui.ctx(), |ui| {
            egui::Grid::new("run-now-grid")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Payload");
                    egui::ComboBox::from_id_salt("run-payload-mode")
                        .selected_text(dialog.payload_mode.label())
                        .show_ui(ui, |ui| {
                            for mode in PayloadMode::ALL {
                                ui.selectable_value(&mut dialog.payload_mode, mode, mode.label());
                            }
                        });
                    ui.end_row();
                    if dialog.payload_mode != PayloadMode::None {
                        ui.label("Value");
                        ui.add(
                            egui::TextEdit::multiline(&mut dialog.payload)
                                .desired_rows(3)
                                .desired_width(320.0),
                        );
                        ui.end_row();
                    }
                    ui.label("Metadata");
                    ui.add(
                        egui::TextEdit::multiline(&mut dialog.metadata)
                            .hint_text("{ \"key\": \"value\" }")
                            .desired_rows(3)
                            .desired_width(320.0),
                    );
                    ui.end_row();
                });
            ui.label(
                RichText::new(
                    "Payload becomes the run trigger payload; None leaves it empty.",
                )
                .small(),
            );
            ui.label(
                RichText::new(
                    "Metadata is an optional JSON object. Commands receive it on stdin's run.metadata; it is also stored on the run trigger.",
                )
                .small(),
            );
            if let Some(error) = &dialog.error {
                ui.label(RichText::new(error).color(app.palette().red));
            }
            ui.horizontal(|ui| {
                if ui.button("Run").clicked() {
                    run = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if run {
        match build_run_payload(&dialog) {
            Ok(payload) => {
                dialog.sending = true;
                app.fire_task_with_payload(task_id, payload);
                close = true;
            }
            Err(error) => dialog.error = Some(error),
        }
    }
    if !close {
        *slot = Some(dialog);
    }
}

fn build_run_payload(
    dialog: &RunNow,
) -> Result<(Option<Base64Bytes>, serde_json::Map<String, Value>), String> {
    let payload = match dialog.payload_mode {
        PayloadMode::None => None,
        PayloadMode::Text => Some(Base64Bytes::from(dialog.payload.as_bytes())),
        PayloadMode::Hex => Some(Base64Bytes::from(decode_hex(&dialog.payload)?)),
        PayloadMode::Base64 => Some(
            Base64Bytes::from_str(dialog.payload.trim())
                .map_err(|error| format!("Invalid base64: {error}"))?,
        ),
    };
    let metadata = if dialog.metadata.trim().is_empty() {
        serde_json::Map::new()
    } else {
        serde_json::from_str::<Value>(dialog.metadata.trim())
            .map_err(|error| format!("Metadata JSON: {error}"))?
            .as_object()
            .cloned()
            .ok_or_else(|| "Metadata must be a JSON object.".to_string())?
    };
    Ok((payload, metadata))
}

fn decode_hex(input: &str) -> Result<Vec<u8>, String> {
    let compact: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if !compact.len().is_multiple_of(2) {
        return Err("Hex payload must contain an even number of digits.".to_string());
    }
    let mut bytes = Vec::with_capacity(compact.len() / 2);
    for pair in compact.chunks(2) {
        let text = std::str::from_utf8(pair).map_err(|_| "Invalid hex payload.".to_string())?;
        bytes.push(u8::from_str_radix(text, 16).map_err(|_| "Invalid hex payload.".to_string())?);
    }
    Ok(bytes)
}

pub(crate) fn switch_to_runs(app: &mut App, task_id: &str) {
    app.tasks_tab = TasksTab::Runs;
    app.runs.filter_task = Some(task_id.to_string());
    app.refresh_runs();
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use hodeco_contract::domain::{RunSummary, TriggerRef, TriggerType};

    fn info(id: &str, enabled: bool) -> TaskInfo {
        TaskInfo {
            task: TaskDefinition {
                id: id.to_string(),
                name: format!("Task {id}"),
                enabled,
                ..TaskDefinition::default()
            },
            running: false,
            last_run: None,
            next: None,
            suppressed_runs: 0,
            last_suppressed_reason: None,
        }
    }

    #[test]
    fn filters_by_search_and_disabled_state() {
        let mut ui = TasksUi {
            tasks: vec![info("evening", true), info("winter", false)],
            ..TasksUi::default()
        };
        ui.search = "even".to_string();
        assert_eq!(ui.filtered().len(), 1);
        ui.search.clear();
        ui.show_disabled = false;
        assert_eq!(ui.filtered().len(), 1);
        assert_eq!(ui.filtered()[0].task.id, "evening");
    }

    #[test]
    fn task_editor_rejects_invalid_json_sections() {
        let editor = TaskEditor::new(
            TaskDefinition {
                name: "Test".to_string(),
                ..TaskDefinition::default()
            },
            true,
        );
        let mut editor = TaskEditor {
            triggers_json: "not json".to_string(),
            ..editor
        };
        assert!(editor.build().is_err());
        editor.triggers_json = "[]".to_string();
        assert!(editor.build().is_ok());
    }

    #[test]
    fn task_list_is_stable_when_empty() {
        let ui = TasksUi::default();
        assert!(ui.filtered().is_empty());
    }

    #[test]
    fn last_run_summary_is_used_for_display() {
        let now = Utc::now();
        let mut task = info("evening", true);
        task.last_run = Some(RunSummary {
            id: uuid::Uuid::new_v4(),
            task_id: "evening".to_string(),
            time: now,
            started_at: now,
            finished_at: now,
            trigger: TriggerRef {
                id: "manual".to_string(),
                kind: TriggerType::Manual,
            },
            outcome: hodeco_contract::domain::RunOutcome::Succeeded,
            action_count: 0,
            next_count: 0,
            error: None,
        });
        assert_eq!(task.last_run.unwrap().task_id, "evening");
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;

    #[test]
    fn disabled_history_ignores_the_size_field() {
        let mut task = TaskDefinition {
            name: "No history".to_string(),
            ..TaskDefinition::default()
        };
        task.history.max_rows = 0;
        let editor = TaskEditor {
            history_size: "not a size".to_string(),
            ..TaskEditor::new(task, true)
        };
        assert!(limits_valid(&editor));
        let built = editor.build().unwrap();
        assert_eq!(built.history.max_rows, 0);
        assert_eq!(built.history.max_size.as_bytes(), 0);
    }

    #[test]
    fn enabled_history_requires_at_least_one_kibibyte() {
        let mut task = TaskDefinition {
            name: "History".to_string(),
            ..TaskDefinition::default()
        };
        task.history.max_rows = 100;
        let editor = TaskEditor {
            history_size: "1 B".to_string(),
            ..TaskEditor::new(task, true)
        };
        assert!(!limits_valid(&editor));
        assert!(editor.build().is_err());
    }
}
