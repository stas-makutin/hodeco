//! Tasks list, full-page task editor, and run-now dialog.

use super::super::{
    App, ConditionEditor, FilterDraft, PayloadConditionDraft, RawConditionDraft, RawLeafDraft,
};
use crate::model;
use crate::ui::widgets;
use egui::{PopupCloseBehavior, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use hodeco_contract::domain::{
    ActionKind, CommandAction, HttpMethod, HttpTrigger, MAX_TASK_LABEL_LEN, MAX_TASK_LABELS,
    MAX_TASK_START_DELAY, MessageAction, MessageActionPayload, MessageDirection, MessageTrigger,
    MessageTriggerWhen, NextCondition, NextRule, OneOrMany, OutputMode, PayloadExpr, PayloadLeaf,
    PayloadOffset, PayloadOp, ScheduleTrigger, ServiceRef, TaskAction, TaskDefinition, TaskInfo,
    TaskTrigger, TriggerType, validate_task_labels,
};
use hodeco_contract::utils::{Base64Bytes, HumanDuration, Size, id_from_name};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

const DEFAULT_HISTORY_ROWS: u32 = 200;
const LABEL_PICKER_HEIGHT: f32 = 360.0;

#[derive(Debug, Clone)]
pub(crate) struct LabelOption {
    pub key: String,
    pub label: String,
    pub count: usize,
}

#[derive(Debug)]
pub(crate) struct TasksUi {
    pub tasks: Vec<TaskInfo>,
    pub loading: bool,
    pub error: Option<String>,
    pub search: String,
    pub show_disabled: bool,
    pub selected: Option<usize>,
    pub selected_labels: BTreeSet<String>,
    pub label_filter_search: String,
    pub label_filter_was_open: bool,
    pub editor: Option<TaskEditor>,
    pub history: Option<super::task_history::TaskHistoryUi>,
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
                self.selected_labels.is_empty()
                    || info
                        .task
                        .labels
                        .iter()
                        .any(|label| self.selected_labels.contains(&label.trim().to_lowercase()))
            })
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

    /// Derives the case-insensitive label catalog and task counts.
    pub fn label_options(&self) -> Vec<LabelOption> {
        let mut options = BTreeMap::new();
        for info in &self.tasks {
            for label in &info.task.labels {
                let label = label.trim();
                if label.is_empty() {
                    continue;
                }
                let key = label.to_lowercase();
                options
                    .entry(key.clone())
                    .and_modify(|option: &mut LabelOption| option.count += 1)
                    .or_insert_with(|| LabelOption {
                        key,
                        label: label.to_string(),
                        count: 1,
                    });
            }
        }
        let mut options: Vec<LabelOption> = options.into_values().collect();
        options.sort_by_key(|option| option.label.to_lowercase());
        options
    }

    /// Drops filter selections whose labels no longer exist on any task.
    pub fn prune_selected_labels(&mut self) {
        if self.selected_labels.is_empty() {
            return;
        }
        let available: BTreeSet<String> = self
            .tasks
            .iter()
            .flat_map(|info| info.task.labels.iter())
            .map(|label| label.trim().to_lowercase())
            .collect();
        self.selected_labels.retain(|key| available.contains(key));
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
            selected_labels: BTreeSet::new(),
            label_filter_search: String::new(),
            label_filter_was_open: false,
            editor: None,
            history: None,
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
            Self::Limits => "Limits & History log",
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
    pub run_timeout: String,
    pub delay_enabled: bool,
    pub delay: String,
    pub jitter_enabled: bool,
    pub jitter: String,
    pub history_size: String,
    pub history_enabled: bool,
    pub error: Option<String>,
    pub saving: bool,
    pub confirm_delete: bool,
    pub run_now: Option<RunNow>,
    pub payload_filter: Option<PayloadFilterDialog>,
    pub label_search: String,
    pub label_picker_was_open: bool,
}

#[derive(Debug)]
pub(crate) struct PayloadFilterDialog {
    pub trigger_id: String,
    pub draft: FilterDraft,
    pub condition_editor: Option<ConditionEditor>,
    pub unsupported: bool,
    pub error: Option<String>,
}

impl PayloadFilterDialog {
    fn from_trigger(trigger: &MessageTrigger) -> Self {
        let (draft, unsupported) = match &trigger.payload {
            None => (FilterDraft::default(), false),
            Some(expression) => match payload_conditions_from_expr(expression) {
                Some(conditions) => (
                    FilterDraft {
                        payload_conditions: conditions,
                        ..FilterDraft::default()
                    },
                    false,
                ),
                None => (FilterDraft::default(), true),
            },
        };
        Self {
            trigger_id: trigger.id.clone(),
            draft,
            condition_editor: None,
            unsupported,
            error: None,
        }
    }
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
        let run_timeout = task.run_timeout.to_string();
        let delay_enabled = task.delay.is_some();
        let delay = task
            .delay
            .map(|value| value.to_string())
            .unwrap_or_default();
        let jitter_enabled = task.jitter.is_some();
        let jitter = task
            .jitter
            .map(|value| value.to_string())
            .unwrap_or_else(|| "30s".to_string());
        let history_size = task.history.max_size.to_string();
        let history_enabled = task.history.max_rows > 0;
        let mut draft = task.clone();
        if !history_enabled && draft.history.max_rows == 0 {
            draft.history.max_rows = DEFAULT_HISTORY_ROWS;
        }
        let mut initial = task.clone();
        let mut original = (!is_new).then_some(task.clone());
        if !history_enabled {
            initial.history.max_size = Size::from_bytes(0);
            if let Some(original) = original.as_mut() {
                original.history.max_size = Size::from_bytes(0);
            }
        }
        Self {
            draft,
            original,
            initial,
            is_new,
            id_auto: is_new,
            section: TaskSection::General,
            run_timeout,
            delay_enabled,
            delay,
            jitter_enabled,
            jitter,
            history_size,
            history_enabled,
            error: None,
            saving: false,
            confirm_delete: false,
            run_now: None,
            payload_filter: None,
            label_search: String::new(),
            label_picker_was_open: false,
        }
    }

    pub fn build(&self) -> Result<TaskDefinition, String> {
        let mut task = self.draft.clone();
        task.name = task.name.trim().to_string();
        if task.name.is_empty() {
            return Err("Name must not be empty.".to_string());
        }
        task.normalize_labels();
        if let Err(error) = validate_task_labels(&task.labels) {
            return Err(format!("Labels: {error}."));
        }
        for trigger in &mut task.triggers {
            let id = trigger_id_mut(trigger);
            *id = id.trim().to_string();
            if let TaskTrigger::Message(message) = trigger {
                trim_list(&mut message.protocols);
                trim_list(&mut message.transports);
            }
        }
        let mut trigger_ids = std::collections::HashSet::new();
        for (index, trigger) in task.triggers.iter().enumerate() {
            let label = format!("Trigger {}", index + 1);
            let id = trigger.id();
            if id.is_empty() {
                return Err(format!("{label} needs an ID."));
            }
            if !trigger_ids.insert(id.to_string()) {
                return Err(format!("Duplicate trigger ID '{id}'."));
            }
            if let TaskTrigger::Schedule(schedule) = trigger
                && schedule.schedule.trim().is_empty()
            {
                return Err(format!("{label} needs a schedule."));
            }
            if let TaskTrigger::Http(http) = trigger
                && http
                    .secret
                    .as_deref()
                    .is_some_and(|secret| secret.trim().is_empty())
            {
                return Err(format!("{label} has an empty secret."));
            }
        }
        if task
            .triggers
            .iter()
            .filter(|trigger| matches!(trigger, TaskTrigger::Http(_)))
            .count()
            > 1
        {
            return Err("A task can have only one HTTP trigger.".to_string());
        }
        task.run_timeout =
            HumanDuration::from_str(self.run_timeout.trim()).map_err(|error| error.to_string())?;
        if task.run_timeout.as_std() < std::time::Duration::from_secs(1) {
            return Err("Run timeout must be at least 1 second.".to_string());
        }
        task.delay = if self.delay_enabled {
            Some(
                parse_optional_duration(&self.delay, "Delay")?
                    .ok_or_else(|| "Delay must be set when start delay is enabled.".to_string())?,
            )
        } else {
            None
        };
        task.jitter = if self.delay_enabled && self.jitter_enabled {
            if self.jitter.trim().starts_with('-') {
                return Err(
                    "Jitter must be a positive magnitude; offsets are applied symmetrically as ±jitter."
                        .to_string(),
                );
            }
            Some(
                parse_optional_duration(&self.jitter, "Jitter")?
                    .ok_or_else(|| "Jitter must be set when jitter is enabled.".to_string())?,
            )
        } else {
            None
        };
        if let Some(delay) = task.delay {
            if delay.as_std() > MAX_TASK_START_DELAY {
                return Err("Delay must not exceed 24 hours.".to_string());
            }
            match task.jitter {
                Some(jitter) => {
                    if jitter.as_std() > MAX_TASK_START_DELAY {
                        return Err("Jitter must not exceed 24 hours.".to_string());
                    }
                    if jitter.as_std() > delay.as_std() {
                        return Err("Jitter must not exceed the delay.".to_string());
                    }
                    if delay.as_std() + jitter.as_std() > MAX_TASK_START_DELAY {
                        return Err("Delay plus jitter must not exceed 24 hours.".to_string());
                    }
                    if delay.as_std() + jitter.as_std() >= task.run_timeout.as_std() {
                        return Err("Delay plus jitter must be less than Run timeout.".to_string());
                    }
                }
                None => {
                    if delay.as_std() >= task.run_timeout.as_std() {
                        return Err("Delay must be less than Run timeout.".to_string());
                    }
                }
            }
        }
        for action in &mut task.actions {
            action.id = action.id.trim().to_string();
            if let ActionKind::Command(command) = &mut action.kind {
                trim_command_args(&mut command.args);
            }
        }
        for rule in &mut task.next {
            trim_command_args(&mut rule.args);
        }
        let mut action_ids = std::collections::HashSet::new();
        for (index, action) in task.actions.iter().enumerate() {
            let label = format!("Action {}", index + 1);
            if action.id.is_empty() {
                return Err(format!("{label} needs an ID."));
            }
            if !action_ids.insert(action.id.clone()) {
                return Err(format!("Duplicate action ID '{}'.", action.id));
            }
            match &action.kind {
                ActionKind::Command(command) if command.command.trim().is_empty() => {
                    return Err(format!("{label} needs a command."));
                }
                ActionKind::Message(message) => {
                    if let ServiceRef::Alias { alias } = &message.message.service
                        && alias.trim().is_empty()
                    {
                        return Err(format!("{label} needs a service."));
                    }
                }
                _ => {}
            }
        }
        for (index, rule) in task.next.iter().enumerate() {
            let label = format!("Next rule {}", index + 1);
            if rule.when == NextCondition::CommandResult {
                if rule.command.as_deref().is_none_or(str::is_empty) {
                    return Err(format!("{label} needs a selector command."));
                }
                if !rule.tasks.is_empty() {
                    return Err(format!("{label}: command-result uses cases, not tasks."));
                }
            } else if rule.tasks.is_empty() {
                return Err(format!("{label} needs at least one target task."));
            }
        }
        if task
            .next
            .iter()
            .filter(|rule| rule.when == NextCondition::CommandResult)
            .count()
            > 1
        {
            return Err("Only one command-result rule is allowed.".to_string());
        }
        if self.history_enabled {
            if task.history.max_rows == 0 {
                task.history.max_rows = DEFAULT_HISTORY_ROWS;
            }
            task.history.max_size =
                Size::from_str(self.history_size.trim()).map_err(|error| error.to_string())?;
            if task.history.max_size < Size::from_bytes(1024) {
                return Err("History size must be at least 1 KiB.".to_string());
            }
        } else {
            task.history.max_rows = 0;
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
    if app.tasks.history.is_some() {
        super::task_history::ui(app, ui);
        return;
    }
    if app.tasks.editor.is_some() {
        editor_ui(app, ui);
        return;
    }
    list_ui(app, ui);
}

fn truncated_label(label: &str, max_chars: usize) -> String {
    let mut chars = label.chars();
    let mut text: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        text.push('…');
    }
    text
}

fn matching_labels<'a>(options: &'a [LabelOption], search: &str) -> Vec<&'a LabelOption> {
    let needle = search.trim().to_lowercase();
    options
        .iter()
        .filter(|option| needle.is_empty() || option.label.to_lowercase().contains(&needle))
        .collect()
}

fn label_filter_ui(app: &mut App, ui: &mut Ui) {
    app.tasks.prune_selected_labels();
    let theme = app.theme;
    let palette = app.palette();
    let options = app.tasks.label_options();

    ui.horizontal(|ui| {
        ui.label(RichText::new("Labels:").size(13.5).color(palette.muted));
        // Match ComboBox's derived popup id so the search text resets when the
        // picker closes.
        let popup_id = ui
            .make_persistent_id(egui::IdSalt::new("task-label-filter"))
            .with("popup");
        let was_open = app.tasks.label_filter_was_open;
        let search_before = app.tasks.label_filter_search.clone();
        egui::ComboBox::from_id_salt("task-label-filter")
            .selected_text("Filter by label…")
            .width(180.0)
            .height(LABEL_PICKER_HEIGHT)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show_ui(ui, |ui| {
                ui.set_min_width(300.0);
                ui.set_max_width(340.0);
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                let search = ui.add(
                    egui::TextEdit::singleline(&mut app.tasks.label_filter_search)
                        .hint_text("Filter labels…")
                        .char_limit(MAX_TASK_LABEL_LEN)
                        .desired_width(ui.available_width()),
                );
                if !was_open {
                    search.request_focus();
                }
                ui.separator();
                let search_changed = app.tasks.label_filter_search != search_before;
                let visible = matching_labels(&options, &app.tasks.label_filter_search);
                let list = egui::ScrollArea::vertical()
                    .max_height(widgets::LIST_MAX_HEIGHT)
                    .min_scrolled_height(widgets::LIST_MAX_HEIGHT)
                    .auto_shrink([false, false]);
                let list = if !was_open || search_changed {
                    list.vertical_scroll_offset(0.0)
                } else {
                    list
                };
                list.show(ui, |ui| {
                    if visible.is_empty() {
                        ui.label(
                            RichText::new(if options.is_empty() {
                                "No labels yet."
                            } else {
                                "No labels match."
                            })
                            .color(palette.muted),
                        );
                    } else {
                        for option in visible {
                            let mut checked = app.tasks.selected_labels.contains(&option.key);
                            if ui
                                .checkbox(
                                    &mut checked,
                                    format!("{} ({})", option.label, option.count),
                                )
                                .changed()
                            {
                                if checked {
                                    app.tasks.selected_labels.insert(option.key.clone());
                                } else {
                                    app.tasks.selected_labels.remove(&option.key);
                                }
                            }
                        }
                    }
                });
            });
        let is_open = egui::Popup::is_id_open(ui.ctx(), popup_id);
        if was_open && !is_open {
            app.tasks.label_filter_search.clear();
        }
        app.tasks.label_filter_was_open = is_open;
        if !app.tasks.selected_labels.is_empty() && ui.small_button("Clear").clicked() {
            app.tasks.selected_labels.clear();
        }
    });

    if !app.tasks.selected_labels.is_empty() {
        let mut remove = Vec::new();
        ui.horizontal_wrapped(|ui| {
            for option in &options {
                if app.tasks.selected_labels.contains(&option.key)
                    && widgets::removable_chip_button(ui, theme, &option.label, widgets::Tone::Info)
                {
                    remove.push(option.key.clone());
                }
            }
        });
        for key in remove {
            app.tasks.selected_labels.remove(&key);
        }
    }
}

fn list_ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let live = app.gateway.phase().is_live();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let total = app.tasks.tasks.len();
    let running = app.tasks.tasks.iter().filter(|task| task.running).count();
    let disabled = app
        .tasks
        .tasks
        .iter()
        .filter(|task| !task.task.enabled)
        .count();

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
    label_filter_ui(app, ui);
    ui.add_space(2.0);

    let filtered = app.tasks.filtered();
    let filtered_count = filtered.len();
    let rows: Vec<TaskInfo> = filtered.into_iter().cloned().collect();
    let mut selected = app.tasks.selected.filter(|index| *index < rows.len());

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
        let next = nav.apply(selected, rows.len());
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
            ui.set_min_width(1170.0);
            let mut table = TableBuilder::new(ui)
                .id_salt("tasks-table")
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::CLICK)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(170.0).at_least(140.0))
                .column(Column::initial(65.0).at_least(60.0))
                .column(Column::initial(180.0).at_least(150.0))
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
                        "Labels",
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
                            if task.labels.is_empty() {
                                ui.label(RichText::new("—").color(palette.muted));
                            } else {
                                let mut labels = task.labels.clone();
                                labels.sort_by_key(|label| label.to_lowercase());
                                let shown = labels.iter().take(2);
                                let response = ui
                                    .horizontal(|ui| {
                                        for label in shown {
                                            widgets::chip(
                                                ui,
                                                theme,
                                                &truncated_label(label.trim(), 10),
                                                widgets::Tone::Neutral,
                                            );
                                        }
                                        if labels.len() > 2 {
                                            ui.label(
                                                RichText::new(format!("+{}", labels.len() - 2))
                                                    .small()
                                                    .color(palette.muted),
                                            );
                                        }
                                    })
                                    .response;
                                response.on_hover_text(labels.join("\n"));
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
                                let running_run =
                                    info.running.then_some(info.running_run_id).flatten();
                                let cancelling =
                                    running_run.is_some_and(|id| app.runs.cancelling.contains(&id));
                                let cancel = ui.add_enabled(
                                    ready && running_run.is_some() && !cancelling,
                                    egui::Button::new("Cancel").small(),
                                );
                                if cancel.on_hover_text("Stop the active run").clicked()
                                    && let Some(run_id) = running_run
                                {
                                    app.cancel_task_run(&task.id, run_id);
                                }
                                if ui
                                    .add_enabled(ready, egui::Button::new("History").small())
                                    .on_hover_text("Open persistent run history")
                                    .clicked()
                                {
                                    app.open_task_history(&task.id);
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
                "Change the search, labels, or show disabled tasks.",
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
    let detail = format!(
        "Delete task '{task_id}'? This cannot be undone. The task history file is removed too."
    );
    match widgets::confirm_modal(
        ui.ctx(),
        app.theme,
        "delete-task",
        "Delete task",
        &detail,
        "Delete task",
    ) {
        Some(true) => app.delete_task(task_id),
        Some(false) => app.tasks.confirm_delete = None,
        None => {}
    }
}

fn editor_ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let Some(mut editor) = app.tasks.editor.take() else {
        return;
    };
    let runtime = app.tasks.task(&editor.draft.id).cloned();
    let mut keep = true;
    let mut close = false;
    let mut save = false;
    let mut delete = false;
    let mut discard = false;
    let mut cancel_delete = false;
    let validation_error = editor.build().err();
    let header_error = validation_error.clone().or_else(|| editor.error.clone());
    let close_blocked = validation_error.is_some() || (editor.error.is_some() && editor.is_dirty());

    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !editor.saving && !close_blocked,
                egui::Button::new("← Tasks"),
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
                "New task"
            } else {
                "Edit task"
            })
            .size(18.0)
            .strong(),
        );
        if !editor.is_new {
            ui.label(RichText::new(&editor.draft.id).small().color(palette.muted));
            if let Some(info) = &runtime {
                if info.running {
                    ui.spinner();
                    ui.label("Running");
                }
                if header_error.is_none() {
                    if let Some(last) = &info.last_run {
                        ui.label(
                            RichText::new(format!("Last {}", model::format_datetime(last.time)))
                                .small()
                                .color(palette.muted),
                        );
                    }
                    if let Some(next) = &info.next {
                        ui.label(
                            RichText::new(format!("Next {}", model::format_datetime(next.at)))
                                .small()
                                .color(palette.muted),
                        );
                    }
                    if info.suppressed_runs > 0 {
                        ui.label(
                            RichText::new(format!("{} suppressed", info.suppressed_runs))
                                .small()
                                .color(palette.amber),
                        );
                    }
                }
            }
        }
        if !editor.is_new {
            ui.add_space(8.0);
            let run_now_enabled =
                ready && editor.draft.enabled && runtime.as_ref().is_some_and(|info| !info.running);
            if ui
                .add_enabled(run_now_enabled, egui::Button::new("Run now"))
                .on_disabled_hover_text("Enable the task and wait for the current run to finish")
                .clicked()
            {
                editor.run_now = Some(RunNow::default());
            }
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
            if ui
                .add_enabled(
                    ready && !editor.saving && validation_error.is_none(),
                    egui::Button::new("Save"),
                )
                .on_disabled_hover_text("Fix the validation error before saving")
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
            if let Some(error) = &header_error {
                ui.add_space(12.0);
                ui.add(egui::Label::new(RichText::new(error).color(palette.red).size(13.5)).wrap())
                    .on_hover_text(error);
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
                ui.set_min_width(ui.available_width());
                ui.vertical(|ui| match editor.section {
                    TaskSection::General => general_section(app, ui, &mut editor),
                    TaskSection::Triggers => triggers_section(app, ui, &mut editor),
                    TaskSection::Actions => actions_section(app, ui, &mut editor),
                    TaskSection::Next => next_section(app, ui, &mut editor),
                    TaskSection::Limits => limits_section(app, ui, &mut editor),
                });
            });
    });

    if editor.payload_filter.is_some() {
        payload_filter_dialog(app, ui, &mut editor);
    }

    if save && let Ok(task) = editor.build() {
        editor.error = None;
        editor.saving = true;
        if !app.save_task(task, editor.is_new) {
            editor.saving = false;
            editor.error = Some("The gateway is not connected.".to_string());
        }
    }
    if editor.run_now.is_some() {
        let task_id = editor.draft.id.clone();
        run_now_dialog(app, ui, &task_id, &mut editor.run_now);
    }
    if editor.confirm_delete {
        let detail = format!(
            "Delete task '{}'? This cannot be undone. The task history file is removed too.",
            editor.draft.id
        );
        match widgets::confirm_modal(
            ui.ctx(),
            theme,
            "delete-task-editor",
            "Delete task",
            &detail,
            "Delete task",
        ) {
            Some(true) => delete = true,
            Some(false) => cancel_delete = true,
            None => {}
        }
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
                Some("Save or discard the changes before leaving the editor.".to_string());
        } else {
            keep = false;
        }
    }
    if keep {
        app.tasks.editor = Some(editor);
    }
}

fn labels_editor(app: &mut App, ui: &mut Ui, editor: &mut TaskEditor) {
    let palette = app.palette();
    let mut options = app.tasks.label_options();
    for label in &editor.draft.labels {
        let label = label.trim();
        if label.is_empty() {
            continue;
        }
        let key = label.to_lowercase();
        if !options.iter().any(|option| option.key == key) {
            options.push(LabelOption {
                key,
                label: label.to_string(),
                count: 0,
            });
        }
    }
    options.sort_by_key(|option| option.label.to_lowercase());

    ui.set_width(420.0);
    ui.horizontal(|ui| {
        // Match ComboBox's derived popup id so the search text resets when the
        // picker closes.
        let popup_id = ui
            .make_persistent_id(egui::IdSalt::new("task-label-picker"))
            .with("popup");
        let was_open = editor.label_picker_was_open;
        let search_before = editor.label_search.clone();
        egui::ComboBox::from_id_salt("task-label-picker")
            .selected_text("Add label…")
            .width(180.0)
            .height(LABEL_PICKER_HEIGHT)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show_ui(ui, |ui| {
                ui.set_min_width(300.0);
                ui.set_max_width(340.0);
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                let search_response = ui.add(
                    egui::TextEdit::singleline(&mut editor.label_search)
                        .hint_text("Search or create…")
                        .char_limit(MAX_TASK_LABEL_LEN)
                        .desired_width(ui.available_width()),
                );
                if !was_open {
                    search_response.request_focus();
                }
                ui.separator();

                let search = editor.label_search.trim().to_string();
                let needle = search.to_lowercase();
                let exact = options
                    .iter()
                    .any(|option| option.label.to_lowercase() == needle);
                let row_height = ui.spacing().interact_size.y;
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), row_height),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        if !search.is_empty() && !exact {
                            let error = if search.chars().count() > MAX_TASK_LABEL_LEN {
                                Some("Label is too long.")
                            } else if search.chars().any(char::is_control) {
                                Some("Label contains control characters.")
                            } else if editor.draft.labels.len() >= MAX_TASK_LABELS {
                                Some("Maximum number of labels reached.")
                            } else {
                                None
                            };
                            if let Some(error) = error {
                                ui.label(RichText::new(error).small().color(palette.red));
                            } else if ui.button(format!("Create \"{search}\"")).clicked() {
                                editor.draft.labels.push(search.clone());
                                editor.label_search.clear();
                            }
                        } else if search.is_empty() {
                            ui.label(
                                RichText::new("Type to search or create…")
                                    .small()
                                    .color(palette.muted),
                            );
                        }
                    },
                );

                let search_changed = editor.label_search != search_before;
                let visible = matching_labels(&options, &search);
                let list = egui::ScrollArea::vertical()
                    .max_height(widgets::LIST_MAX_HEIGHT)
                    .min_scrolled_height(widgets::LIST_MAX_HEIGHT)
                    .auto_shrink([false, false]);
                let list = if !was_open || search_changed {
                    list.vertical_scroll_offset(0.0)
                } else {
                    list
                };
                list.show(ui, |ui| {
                    if visible.is_empty() {
                        ui.label(
                            RichText::new(if options.is_empty() {
                                "No labels yet."
                            } else {
                                "No labels match."
                            })
                            .color(palette.muted),
                        );
                    } else {
                        for option in visible {
                            let selected = editor
                                .draft
                                .labels
                                .iter()
                                .any(|label| label.trim().eq_ignore_ascii_case(&option.label));
                            let mut checked = selected;
                            let text = if option.count > 0 {
                                format!("{} ({})", option.label, option.count)
                            } else {
                                option.label.clone()
                            };
                            if ui.checkbox(&mut checked, text).changed() {
                                if checked {
                                    if !editor.draft.labels.iter().any(|label| {
                                        label.trim().eq_ignore_ascii_case(&option.label)
                                    }) {
                                        editor.draft.labels.push(option.label.clone());
                                    }
                                } else {
                                    editor.draft.labels.retain(|label| {
                                        !label.trim().eq_ignore_ascii_case(&option.label)
                                    });
                                }
                            }
                        }
                    }
                });
            });
        let is_open = egui::Popup::is_id_open(ui.ctx(), popup_id);
        if was_open && !is_open {
            editor.label_search.clear();
        }
        editor.label_picker_was_open = is_open;
    });

    let mut labels: Vec<String> = editor
        .draft
        .labels
        .iter()
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty())
        .collect();
    if labels.is_empty() {
        ui.label(RichText::new("No labels.").small().color(palette.muted));
    } else {
        labels.sort_by_key(|label| label.to_lowercase());
        let mut remove = None;
        egui::Frame::new()
            .fill(palette.panel)
            .stroke(egui::Stroke::new(1.0, palette.border))
            .corner_radius(4)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.set_min_width(400.0);
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for label in &labels {
                            ui.horizontal(|ui| {
                                ui.label(label);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("Remove").clicked() {
                                            remove = Some(label.clone());
                                        }
                                    },
                                );
                            });
                        }
                    });
            });
        if let Some(label) = remove {
            editor
                .draft
                .labels
                .retain(|candidate| !candidate.trim().eq_ignore_ascii_case(&label));
        }
    }
}

fn general_section(app: &mut App, ui: &mut Ui, editor: &mut TaskEditor) {
    widgets::control_group_fill(ui, "Details", |ui| {
        egui::Grid::new("task-details-grid")
            .num_columns(2)
            .spacing([18.0, 4.0])
            .show(ui, |ui| {
                widgets::field_label(ui, app.theme, "Name");
                widgets::field_with_note(
                    ui,
                    app.theme,
                    "Display name shown in the task list.",
                    |ui| {
                        let name_response = ui.add(
                            egui::TextEdit::singleline(&mut editor.draft.name).desired_width(420.0),
                        );
                        if name_response.changed() && editor.is_new && editor.id_auto {
                            editor.draft.id = id_from_name("task", editor.draft.name.trim());
                        }
                    },
                );
                ui.end_row();

                widgets::field_label(ui, app.theme, "ID");
                if editor.is_new {
                    widgets::field_with_note(
                        ui,
                        app.theme,
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
                                editor.id_auto = editor.draft.id.trim().is_empty();
                            }
                            if editor.id_auto && editor.draft.id.trim().is_empty() {
                                editor.draft.id = id_from_name("task", editor.draft.name.trim());
                            }
                        },
                    );
                } else {
                    ui.label(RichText::new(&editor.draft.id).monospace());
                }
                ui.end_row();

                widgets::field_label(ui, app.theme, "Description");
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

                widgets::field_label(ui, app.theme, "Labels");
                widgets::field_with_note(
                    ui,
                    app.theme,
                    "Optional labels for filtering. They do not affect how the task runs.",
                    |ui| labels_editor(app, ui, editor),
                );
                ui.end_row();

                widgets::field_label(ui, app.theme, "Enabled");
                widgets::field_with_note(
                    ui,
                    app.theme,
                    "Allow triggers to start this task.",
                    |ui| {
                        ui.checkbox(&mut editor.draft.enabled, "Enabled");
                    },
                );
                ui.end_row();
            });
    });
}

fn actions_section(app: &mut App, ui: &mut Ui, editor: &mut TaskEditor) {
    let palette = app.palette();
    let commands: Vec<(String, String, Vec<String>)> = app
        .commands
        .commands
        .iter()
        .map(|info| {
            (
                info.command.id.clone(),
                info.command.name.clone(),
                info.command.args.clone(),
            )
        })
        .collect();
    let services: Vec<(ServiceRef, String)> = app
        .services
        .iter()
        .map(|service| {
            let reference = model::service_ref(service);
            let label = model::service_ref_label(&reference);
            (reference, label)
        })
        .collect();
    ui.label(
        RichText::new(
            "Actions run sequentially. Command output is captured only when requested; message actions publish through a configured service.",
        )
        .small()
        .color(palette.muted),
    );
    ui.add_space(4.0);

    let mut remove = None;
    let mut compose_payload = None;
    for (index, action) in editor.draft.actions.iter_mut().enumerate() {
        widgets::control_group_fill(ui, &format!("Action {}", index + 1), |ui| {
            ui.horizontal(|ui| {
                widgets::field_label(ui, app.theme, "ID");
                widgets::auto_width_text_edit(ui, &mut action.id, "", 180.0, 360.0);
                let current = action_edit_kind(&action.kind);
                let mut kind = current;
                egui::ComboBox::from_id_salt(("action-kind", index))
                    .selected_text(action_kind_label(kind))
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut kind, ActionEditKind::Command, "Command");
                        ui.selectable_value(&mut kind, ActionEditKind::Message, "Message");
                    });
                if kind != current {
                    action.kind = match kind {
                        ActionEditKind::Command => ActionKind::Command(CommandAction {
                            command: String::new(),
                            args: Vec::new(),
                            output: OutputMode::Ignore,
                        }),
                        ActionEditKind::Message => ActionKind::Message(MessageAction {
                            message: MessageActionPayload {
                                service: ServiceRef::Alias {
                                    alias: String::new(),
                                },
                                payload: Base64Bytes::default(),
                                ttl: None,
                            },
                        }),
                    };
                }
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
            });

            match &mut action.kind {
                ActionKind::Command(command) => {
                    ui.horizontal(|ui| {
                        widgets::field_label(ui, app.theme, "Command");
                        egui::ComboBox::from_id_salt(("action-command", index))
                            .selected_text(
                                commands
                                    .iter()
                                    .find(|(id, _, _)| id == &command.command)
                                    .map(|(_, name, _)| name.clone())
                                    .unwrap_or_else(|| {
                                        if command.command.is_empty() {
                                            "Select command".to_string()
                                        } else {
                                            command.command.clone()
                                        }
                                    }),
                            )
                            .width(240.0)
                            .show_ui(ui, |ui| {
                                for (id, name, _) in &commands {
                                    if ui.selectable_label(&command.command == id, name).clicked() {
                                        command.command = id.clone();
                                    }
                                }
                            });
                    });
                    match commands.iter().find(|(id, _, _)| id == &command.command) {
                        Some((_, _, fixed)) if !fixed.is_empty() => {
                            ui.label(
                                RichText::new(format!("Fixed args: {}", fixed.join(" ")))
                                    .small()
                                    .color(palette.muted),
                            );
                        }
                        None if !command.command.is_empty() => {
                            ui.label(
                                RichText::new("Unknown command; choose a catalog entry.")
                                    .small()
                                    .color(palette.red),
                            );
                        }
                        _ => {}
                    }
                    widgets::field_label(ui, app.theme, "Extra arguments");
                    command_args_editor(ui, &mut command.args, "Add argument");
                    ui.horizontal(|ui| {
                        widgets::field_label(ui, app.theme, "Output");
                        egui::ComboBox::from_id_salt(("action-output", index))
                            .selected_text(command.output.label())
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for mode in OutputMode::ALL {
                                    ui.selectable_value(&mut command.output, mode, mode.label());
                                }
                            });
                    });
                }
                ActionKind::Message(message) => {
                    let service_label = services
                        .iter()
                        .find(|(reference, _)| {
                            model::refs_match(reference, &message.message.service)
                        })
                        .map(|(_, label)| label.clone())
                        .unwrap_or_default();
                    let ttl_label = message
                        .message
                        .ttl
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "Service default".to_string());
                    let service_text = if service_label.trim().is_empty() {
                        " "
                    } else {
                        service_label.as_str()
                    };
                    if ui.button("Compose message…").clicked() {
                        compose_payload = Some((
                            index,
                            Some(message.message.service.clone()),
                            message.message.payload.clone(),
                            message.message.ttl,
                        ));
                    }
                    egui::Grid::new(("action-message-summary", index))
                        .num_columns(2)
                        .spacing([12.0, 4.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("Service").small().color(palette.muted));
                            ui.label(service_text);
                            ui.end_row();
                            ui.label(RichText::new("TTL").small().color(palette.muted));
                            ui.label(ttl_label);
                            ui.end_row();
                            ui.label(RichText::new("Payload").small().color(palette.muted));
                            ui.label(format!(
                                "{} bytes",
                                message.message.payload.as_slice().len()
                            ));
                            ui.end_row();
                        });
                    if !message.message.payload.as_slice().is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(hodeco_codec::hex::spaced_hex(
                                    message.message.payload.as_slice(),
                                ))
                                .monospace()
                                .size(13.5),
                            )
                            .wrap(),
                        );
                    }
                }
            }
        });
    }
    if let Some(index) = remove {
        editor.draft.actions.remove(index);
    }
    if let Some((action_index, service, payload, ttl)) = compose_payload {
        app.open_task_message_builder(service, action_index, payload, ttl);
    }
    ui.horizontal(|ui| {
        if ui.button("Add command action").clicked() {
            let id = next_action_id(&editor.draft.actions);
            editor.draft.actions.push(TaskAction {
                id,
                kind: ActionKind::Command(CommandAction {
                    command: String::new(),
                    args: Vec::new(),
                    output: OutputMode::Ignore,
                }),
            });
        }
        if ui.button("Add message action").clicked() {
            let id = next_action_id(&editor.draft.actions);
            editor.draft.actions.push(TaskAction {
                id,
                kind: ActionKind::Message(MessageAction {
                    message: MessageActionPayload {
                        service: ServiceRef::Alias {
                            alias: String::new(),
                        },
                        payload: Base64Bytes::default(),
                        ttl: None,
                    },
                }),
            });
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionEditKind {
    Command,
    Message,
}

fn action_edit_kind(kind: &ActionKind) -> ActionEditKind {
    match kind {
        ActionKind::Command(_) => ActionEditKind::Command,
        ActionKind::Message(_) => ActionEditKind::Message,
    }
}

fn action_kind_label(kind: ActionEditKind) -> &'static str {
    match kind {
        ActionEditKind::Command => "Command",
        ActionEditKind::Message => "Message",
    }
}

fn next_action_id(actions: &[TaskAction]) -> String {
    (1..)
        .map(|index| format!("action-{index}"))
        .find(|id| !actions.iter().any(|action| &action.id == id))
        .expect("an unused action ID exists")
}

fn trim_command_args(args: &mut Vec<String>) {
    for arg in args.iter_mut() {
        *arg = arg.trim().to_string();
    }
    args.retain(|arg| !arg.is_empty());
}

fn command_args_editor(ui: &mut Ui, args: &mut Vec<String>, add_label: &str) {
    let argument_width = (ui.available_width() - 96.0).max(160.0);
    let mut remove_arg = None;
    for (arg_index, arg) in args.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(arg).desired_width(argument_width));
            if ui.small_button("Remove").clicked() {
                remove_arg = Some(arg_index);
            }
        });
    }
    if let Some(arg_index) = remove_arg {
        args.remove(arg_index);
    }
    if ui.button(add_label).clicked() {
        args.push(String::new());
    }
}

fn next_section(app: &mut App, ui: &mut Ui, editor: &mut TaskEditor) {
    let palette = app.palette();
    let targets: Vec<(String, String, bool)> = app
        .tasks
        .tasks
        .iter()
        .map(|info| {
            (
                info.task.id.clone(),
                info.task.name.clone(),
                info.task.enabled,
            )
        })
        .collect();
    let commands: Vec<(String, String)> = app
        .commands
        .commands
        .iter()
        .map(|info| (info.command.id.clone(), info.command.name.clone()))
        .collect();
    ui.label(
        RichText::new(
            "Next rules start other tasks after this run. Rules evaluate in order; command-result selects targets by exit code 0–254.",
        )
        .small()
        .color(palette.muted),
    );
    ui.add_space(4.0);

    let mut remove_rule = None;
    for (index, rule) in editor.draft.next.iter_mut().enumerate() {
        widgets::control_group_fill(ui, &format!("Rule {}", index + 1), |ui| {
            let mut selector_width = None;
            ui.horizontal(|ui| {
                let row_left = ui.cursor().min.x;
                widgets::field_label(ui, app.theme, "When");
                let current = rule.when;
                egui::ComboBox::from_id_salt(("next-when", index))
                    .selected_text(rule.when.label())
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for condition in NextCondition::ALL {
                            ui.selectable_value(&mut rule.when, condition, condition.label());
                        }
                    });
                if rule.when != current {
                    if rule.when == NextCondition::CommandResult {
                        rule.tasks = OneOrMany::Many(Vec::new());
                    } else {
                        rule.command = None;
                        rule.args.clear();
                        rule.cases.clear();
                    }
                }
                let remove = ui.small_button("Remove");
                if remove.clicked() {
                    remove_rule = Some(index);
                }
                selector_width = Some((remove.rect.right() - row_left).max(180.0));
            });
            let selector_width = selector_width.unwrap_or(260.0);
            if rule.when == NextCondition::CommandResult {
                ui.horizontal(|ui| {
                    widgets::field_label(ui, app.theme, "Selector");
                    egui::ComboBox::from_id_salt(("next-command", index))
                        .selected_text(
                            rule.command
                                .as_ref()
                                .and_then(|id| {
                                    commands
                                        .iter()
                                        .find(|(candidate, _)| candidate == id)
                                        .map(|(_, name)| name.clone())
                                })
                                .unwrap_or_else(|| "Select command".to_string()),
                        )
                        .width(240.0)
                        .show_ui(ui, |ui| {
                            for (id, name) in &commands {
                                if ui
                                    .selectable_label(
                                        rule.command.as_deref() == Some(id.as_str()),
                                        name,
                                    )
                                    .clicked()
                                {
                                    rule.command = Some(id.clone());
                                }
                            }
                        });
                });
                widgets::field_label(ui, app.theme, "Selector arguments");
                command_args_editor(ui, &mut rule.args, "Add selector argument");
                ui.add_space(4.0);
                widgets::field_label(ui, app.theme, "Cases");
                let mut cases: Vec<(u8, OneOrMany<String>)> = rule
                    .cases
                    .iter()
                    .map(|(code, targets)| (*code, targets.clone()))
                    .collect();
                let mut remove_case = None;
                let mut duplicate = false;
                let mut codes = Vec::new();
                let original_codes: Vec<u8> = cases.iter().map(|(code, _)| *code).collect();
                for (case_index, (code, target_tasks)) in cases.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        widgets::field_label(ui, app.theme, "Exit");
                        let mut candidate = *code;
                        let response = ui.add(egui::DragValue::new(&mut candidate).range(0..=254));
                        if response.changed()
                            && !original_codes
                                .iter()
                                .enumerate()
                                .any(|(other_index, existing)| {
                                    other_index != case_index && *existing == candidate
                                })
                        {
                            *code = candidate;
                        }
                        let remove_width = widgets::small_button_width(ui, "Remove");
                        let spacing = ui.spacing().item_spacing.x;
                        let fit_width = ui.available_width() - remove_width - spacing;
                        let selector_width = if fit_width.is_finite() {
                            selector_width.min(fit_width.max(180.0))
                        } else {
                            selector_width
                        };
                        target_tasks_editor(
                            ui,
                            target_tasks,
                            &targets,
                            &format!("next-case-{index}-{case_index}"),
                            selector_width,
                        );
                        if ui.small_button("Remove").clicked() {
                            remove_case = Some(case_index);
                        }
                    });
                    if codes.contains(code) {
                        duplicate = true;
                    } else {
                        codes.push(*code);
                    }
                }
                if let Some(case_index) = remove_case {
                    cases.remove(case_index);
                }
                if duplicate {
                    ui.label(
                        RichText::new("Duplicate exit codes; later entries replace earlier ones.")
                            .small()
                            .color(palette.amber),
                    );
                }
                if ui.button("Add case").clicked() {
                    let code = (0..=254u8)
                        .find(|candidate| !cases.iter().any(|(existing, _)| existing == candidate))
                        .unwrap_or(0);
                    cases.push((code, OneOrMany::Many(Vec::new())));
                }
                rule.cases = cases.into_iter().collect();
            } else {
                widgets::field_label(ui, app.theme, "Start tasks");
                target_tasks_editor(
                    ui,
                    &mut rule.tasks,
                    &targets,
                    &format!("next-tasks-{index}"),
                    selector_width.min(ui.available_width().max(180.0)),
                );
                if rule.tasks.iter().next().is_none() {
                    ui.label(
                        RichText::new("Select at least one target task.")
                            .small()
                            .color(palette.amber),
                    );
                }
            }
        });
    }
    if let Some(index) = remove_rule {
        editor.draft.next.remove(index);
    }
    if ui.button("Add next rule").clicked() {
        editor.draft.next.push(NextRule {
            when: NextCondition::AllSucceeded,
            ..NextRule::default()
        });
    }
}

fn target_tasks_editor(
    ui: &mut Ui,
    value: &mut OneOrMany<String>,
    targets: &[(String, String, bool)],
    id_salt: &str,
    width: f32,
) {
    let mut selected: Vec<String> = value.as_slice().to_vec();
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(task_selection_label(&selected, targets))
        .width(width)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            if targets.is_empty() {
                ui.label("No tasks available");
            }
            for (id, name, enabled) in targets {
                let label = if *enabled {
                    name.clone()
                } else {
                    format!("{name} (disabled)")
                };
                let mut checked = selected.contains(id);
                if ui.checkbox(&mut checked, label).changed() {
                    if checked {
                        selected.push(id.clone());
                    } else {
                        selected.retain(|candidate| candidate != id);
                    }
                }
            }
        });
    let disabled: Vec<&str> = selected
        .iter()
        .filter_map(|id| {
            targets
                .iter()
                .find(|(candidate, _, enabled)| candidate == id && !enabled)
                .map(|(_, name, _)| name.as_str())
        })
        .collect();
    if !disabled.is_empty() {
        ui.label(
            RichText::new(format!("Target disabled: {}", disabled.join(", ")))
                .small()
                .color(ui.visuals().warn_fg_color),
        );
    }
    if selected.as_slice() != value.as_slice() {
        *value = OneOrMany::Many(selected);
    }
}

fn task_selection_label(selected: &[String], targets: &[(String, String, bool)]) -> String {
    if selected.is_empty() {
        return "Select tasks".to_string();
    }
    let names: Vec<&str> = selected
        .iter()
        .map(|id| {
            targets
                .iter()
                .find(|(candidate, _, _)| candidate == id)
                .map(|(_, name, _)| name.as_str())
                .unwrap_or(id.as_str())
        })
        .collect();
    if names.len() <= 2 {
        names.join(", ")
    } else {
        format!("{} +{}", names[0], names.len() - 1)
    }
}

fn triggers_section(app: &mut App, ui: &mut Ui, editor: &mut TaskEditor) {
    let palette = app.palette();
    let task_id = editor.draft.id.clone();
    let has_http = editor
        .draft
        .triggers
        .iter()
        .any(|trigger| matches!(trigger, TaskTrigger::Http(_)));
    let schedules: Vec<(String, String, Option<String>)> = app
        .schedules
        .schedules
        .iter()
        .map(|info| {
            (
                info.schedule.id.clone(),
                info.schedule.name.clone(),
                info.next.as_ref().map(|next| next.local.clone()),
            )
        })
        .collect();
    let services: Vec<(ServiceRef, String)> = app
        .services
        .iter()
        .map(|service| {
            let reference = model::service_ref(service);
            let label = model::service_ref_label(&reference);
            (reference, label)
        })
        .collect();
    let protocols: Vec<(String, String)> = app
        .protocols
        .iter()
        .map(|protocol| (protocol.id.clone(), protocol.name.clone()))
        .collect();
    let transports: Vec<(String, String)> = app
        .transports
        .iter()
        .map(|transport| (transport.id.clone(), transport.name.clone()))
        .collect();
    ui.label(
        RichText::new(
            "Message, schedule, and HTTP triggers start this task. Manual and API runs are always available and do not need a trigger entry.",
        )
        .small()
        .color(palette.muted),
    );
    ui.add_space(4.0);

    let mut remove = None;
    let mut open_payload_filter = None;
    for (index, trigger) in editor.draft.triggers.iter_mut().enumerate() {
        widgets::control_group_fill(ui, &format!("Trigger {}", index + 1), |ui| {
            ui.horizontal(|ui| {
                widgets::field_label(ui, app.theme, "ID");
                widgets::auto_width_text_edit(ui, trigger_id_mut(trigger), "", 180.0, 360.0);
                let current = trigger_kind(trigger);
                let mut kind = current;
                egui::ComboBox::from_id_salt(("trigger-kind", index))
                    .selected_text(trigger_kind_label(kind))
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        for candidate in TriggerEditKind::ALL {
                            ui.selectable_value(
                                &mut kind,
                                candidate,
                                trigger_kind_label(candidate),
                            );
                        }
                    });
                if kind != current {
                    let id = trigger_id_mut(trigger).clone();
                    *trigger = default_trigger(kind, id);
                }
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
            });

            match trigger {
                TaskTrigger::Message(message) => {
                    ui.horizontal(|ui| {
                        widgets::field_label(ui, app.theme, "When");
                        egui::ComboBox::from_id_salt(("trigger-message-when", index))
                            .selected_text(message.when.label())
                            .width(210.0)
                            .show_ui(ui, |ui| {
                                for when in MessageTriggerWhen::ALL {
                                    ui.selectable_value(&mut message.when, when, when.label());
                                }
                            });
                    });
                    ui.label(
                        RichText::new(match message.when {
                            MessageTriggerWhen::Settled => {
                                "Starts at registration, then once when an outgoing message reaches a terminal state."
                            }
                            MessageTriggerWhen::New => "Starts only when a message is first registered.",
                            MessageTriggerWhen::StateChanged => {
                                "Starts on every message state transition."
                            }
                        })
                        .small()
                        .color(palette.muted),
                    );
                    widgets::field_label(ui, app.theme, "States");
                    ui.horizontal_wrapped(|ui| {
                        for state in model::MESSAGE_STATES {
                            let selected = message.states.contains(&state);
                            if ui
                                .selectable_label(selected, model::message_state_label(state))
                                .clicked()
                            {
                                if selected {
                                    message.states.retain(|candidate| *candidate != state);
                                } else {
                                    message.states.push(state);
                                }
                            }
                        }
                    });
                    ui.label(
                        RichText::new("No selection means any state.")
                            .small()
                            .color(palette.muted),
                    );
                    ui.horizontal(|ui| {
                        widgets::field_label(ui, app.theme, "Directions");
                        for direction in [MessageDirection::Incoming, MessageDirection::Outgoing] {
                            let mut selected = message.directions.contains(&direction);
                            if ui
                                .checkbox(
                                    &mut selected,
                                    match direction {
                                        MessageDirection::Incoming => "Incoming",
                                        MessageDirection::Outgoing => "Outgoing",
                                    },
                                )
                                .changed()
                            {
                                if selected {
                                    message.directions.push(direction);
                                } else {
                                    message
                                        .directions
                                        .retain(|candidate| *candidate != direction);
                                }
                            }
                        }
                    });
                    widgets::field_label(ui, app.theme, "Protocols");
                    string_refs_editor(
                        ui,
                        &mut message.protocols,
                        &protocols,
                        &format!("trigger-protocols-{index}"),
                    );
                    ui.label(
                        RichText::new("No selection means any protocol.")
                            .small()
                            .color(palette.muted),
                    );
                    widgets::field_label(ui, app.theme, "Transports");
                    string_refs_editor(
                        ui,
                        &mut message.transports,
                        &transports,
                        &format!("trigger-transports-{index}"),
                    );
                    ui.label(
                        RichText::new("No selection means any transport.")
                            .small()
                            .color(palette.muted),
                    );
                    widgets::field_label(ui, app.theme, "Services");
                    service_refs_editor(
                        ui,
                        &mut message.services,
                        &services,
                        &format!("trigger-services-{index}"),
                    );
                    ui.horizontal(|ui| {
                        widgets::field_label(ui, app.theme, "Payload");
                        match &message.payload {
                            None => {
                                ui.label(RichText::new("Any payload").small().color(palette.muted));
                            }
                            Some(expression) => {
                                ui.label(
                                    RichText::new(payload_summary(expression))
                                        .small()
                                        .color(palette.muted),
                                );
                            }
                        }
                        if ui.button("Edit filter…").clicked() {
                            open_payload_filter = Some(PayloadFilterDialog::from_trigger(message));
                        }
                        if message.payload.is_some() && ui.small_button("Clear").clicked() {
                            message.payload = None;
                        }
                    });
                }
                TaskTrigger::Schedule(schedule) => {
                    ui.horizontal(|ui| {
                        widgets::field_label(ui, app.theme, "Schedule");
                        egui::ComboBox::from_id_salt(("trigger-schedule", index))
                            .selected_text(
                                schedules
                                    .iter()
                                    .find(|(id, _, _)| id == &schedule.schedule)
                                    .map(|(_, name, next)| match next {
                                        Some(next) => format!("{name} · next {next}"),
                                        None => name.clone(),
                                    })
                                    .unwrap_or_else(|| {
                                        if schedule.schedule.is_empty() {
                                            "Select schedule".to_string()
                                        } else {
                                            schedule.schedule.clone()
                                        }
                                    }),
                            )
                            .width(320.0)
                            .show_ui(ui, |ui| {
                                for (id, name, next) in &schedules {
                                    let label = match next {
                                        Some(next) => format!("{name} · next {next}"),
                                        None => name.clone(),
                                    };
                                    if ui
                                        .selectable_label(&schedule.schedule == id, label)
                                        .clicked()
                                    {
                                        schedule.schedule = id.clone();
                                    }
                                }
                            });
                    });
                    if schedule.schedule.is_empty() {
                        ui.label(
                            RichText::new("Select a schedule for this trigger.")
                                .small()
                                .color(palette.red),
                        );
                    }
                    ui.horizontal(|ui| {
                        let mut limited = schedule.max_runs.is_some();
                        if ui.checkbox(&mut limited, "Max runs").changed() {
                            schedule.max_runs = limited.then_some(1);
                        }
                        if let Some(max_runs) = &mut schedule.max_runs {
                            ui.add(egui::DragValue::new(max_runs).range(1..=1_000_000));
                        }
                    });
                    ui.label(
                        RichText::new(
                            "API field maxRuns. Counts starts for this task and trigger in memory; it resets on restart, re-enable, and history purge.",
                        )
                        .small()
                        .color(palette.muted),
                    );
                }
                TaskTrigger::Http(http) => {
                    let path = if task_id.is_empty() {
                        "/api/hooks/{task}".to_string()
                    } else {
                        format!("/api/hooks/{task_id}")
                    };
                    ui.label(
                        RichText::new(format!("Endpoint: {path}"))
                            .monospace()
                            .small()
                            .color(palette.muted),
                    );
                    widgets::field_label(ui, app.theme, "Methods");
                    let mut get = http.methods.contains(&HttpMethod::Get);
                    let mut post = http.methods.contains(&HttpMethod::Post);
                    let mut methods_changed = false;
                    ui.horizontal(|ui| {
                        methods_changed |= ui.checkbox(&mut get, "GET").changed();
                        methods_changed |= ui.checkbox(&mut post, "POST").changed();
                    });
                    if methods_changed {
                        http.methods.clear();
                        if get {
                            http.methods.push(HttpMethod::Get);
                        }
                        if post {
                            http.methods.push(HttpMethod::Post);
                        }
                    }
                    ui.label(
                        RichText::new("No method selected allows both GET and POST.")
                            .small()
                            .color(palette.muted),
                    );
                    let mut require_secret = http.secret.is_some();
                    if ui.checkbox(&mut require_secret, "Require secret").changed() {
                        http.secret = require_secret.then(String::new);
                    }
                    if let Some(secret) = &mut http.secret {
                        ui.add(
                            egui::TextEdit::singleline(secret)
                                .hint_text("X-Hodeco-Secret")
                                .password(true)
                                .desired_width(280.0),
                        );
                    }
                }
            }
        });
    }
    if let Some(index) = remove {
        editor.draft.triggers.remove(index);
    }
    if let Some(dialog) = open_payload_filter {
        editor.payload_filter = Some(dialog);
    }
    ui.horizontal_wrapped(|ui| {
        for kind in TriggerEditKind::ALL {
            let enabled = !(kind == TriggerEditKind::Http && has_http);
            let response = ui.add_enabled(
                enabled,
                egui::Button::new(format!(
                    "Add {}",
                    trigger_kind_label(kind).to_ascii_lowercase()
                )),
            );
            let response = if kind == TriggerEditKind::Http && has_http {
                response.on_disabled_hover_text("A task can have only one HTTP trigger")
            } else {
                response
            };
            if response.clicked() {
                let id = next_trigger_id(&editor.draft.triggers, kind);
                editor.draft.triggers.push(default_trigger(kind, id));
            }
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriggerEditKind {
    Message,
    Schedule,
    Http,
}

impl TriggerEditKind {
    const ALL: [Self; 3] = [Self::Message, Self::Schedule, Self::Http];
}

fn trigger_kind(trigger: &TaskTrigger) -> TriggerEditKind {
    match trigger {
        TaskTrigger::Message(_) => TriggerEditKind::Message,
        TaskTrigger::Schedule(_) => TriggerEditKind::Schedule,
        TaskTrigger::Http(_) => TriggerEditKind::Http,
    }
}

fn trigger_kind_label(kind: TriggerEditKind) -> &'static str {
    match kind {
        TriggerEditKind::Message => "Message",
        TriggerEditKind::Schedule => "Schedule",
        TriggerEditKind::Http => "HTTP",
    }
}

fn trigger_id_mut(trigger: &mut TaskTrigger) -> &mut String {
    match trigger {
        TaskTrigger::Message(trigger) => &mut trigger.id,
        TaskTrigger::Schedule(trigger) => &mut trigger.id,
        TaskTrigger::Http(trigger) => &mut trigger.id,
    }
}

fn default_trigger(kind: TriggerEditKind, id: String) -> TaskTrigger {
    match kind {
        TriggerEditKind::Message => TaskTrigger::Message(MessageTrigger {
            id,
            ..Default::default()
        }),
        TriggerEditKind::Schedule => TaskTrigger::Schedule(ScheduleTrigger {
            id,
            schedule: String::new(),
            max_runs: None,
        }),
        TriggerEditKind::Http => TaskTrigger::Http(HttpTrigger {
            id,
            methods: Vec::new(),
            secret: None,
        }),
    }
}

fn next_trigger_id(triggers: &[TaskTrigger], kind: TriggerEditKind) -> String {
    let prefix = match kind {
        TriggerEditKind::Message => "message",
        TriggerEditKind::Schedule => "schedule",
        TriggerEditKind::Http => "http",
    };
    (1..)
        .map(|index| format!("{prefix}-{index}"))
        .find(|id| !triggers.iter().any(|trigger| trigger.id() == id))
        .expect("an unused trigger ID exists")
}

fn trim_list(values: &mut Vec<String>) {
    for value in values.iter_mut() {
        *value = value.trim().to_string();
    }
    values.retain(|value| !value.is_empty());
}

fn string_refs_editor(
    ui: &mut Ui,
    selected: &mut Vec<String>,
    options: &[(String, String)],
    id_salt: &str,
) {
    let label = if selected.is_empty() {
        "Any".to_string()
    } else if selected.len() <= 2 {
        selected
            .iter()
            .map(|value| {
                options
                    .iter()
                    .find(|(id, _)| id == value)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| value.clone())
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        format!(
            "{} +{}",
            options
                .iter()
                .find(|(id, _)| id == &selected[0])
                .map(|(_, label)| label.clone())
                .unwrap_or_else(|| selected[0].clone()),
            selected.len() - 1
        )
    };
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(label)
        .width(300.0)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            if options.is_empty() {
                ui.label("No entries reported by the gateway");
            }
            for (id, label) in options {
                let mut checked = selected.contains(id);
                if ui
                    .checkbox(&mut checked, format!("{label} ({id})"))
                    .changed()
                {
                    if checked {
                        selected.push(id.clone());
                    } else {
                        selected.retain(|candidate| candidate != id);
                    }
                }
            }
            for value in selected
                .iter()
                .filter(|value| !options.iter().any(|(id, _)| id == *value))
                .cloned()
                .collect::<Vec<_>>()
            {
                let mut checked = true;
                if ui
                    .checkbox(&mut checked, format!("{value} (unknown)"))
                    .changed()
                    && !checked
                {
                    selected.retain(|candidate| candidate != &value);
                }
            }
        });
}

fn service_refs_editor(
    ui: &mut Ui,
    selected: &mut Vec<ServiceRef>,
    services: &[(ServiceRef, String)],
    id_salt: &str,
) {
    let label = if selected.is_empty() {
        "Any service".to_string()
    } else if selected.len() <= 2 {
        selected
            .iter()
            .map(|reference| {
                services
                    .iter()
                    .find(|(candidate, _)| model::refs_match(candidate, reference))
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| model::service_ref_label(reference))
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        format!(
            "{} +{}",
            selected
                .first()
                .map(|reference| {
                    services
                        .iter()
                        .find(|(candidate, _)| model::refs_match(candidate, reference))
                        .map(|(_, label)| label.clone())
                        .unwrap_or_else(|| model::service_ref_label(reference))
                })
                .unwrap_or_default(),
            selected.len() - 1
        )
    };
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(label)
        .width(300.0)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            if services.is_empty() {
                ui.label("No services configured");
            }
            for (reference, label) in services {
                let mut checked = selected
                    .iter()
                    .any(|candidate| model::refs_match(candidate, reference));
                if ui.checkbox(&mut checked, label).changed() {
                    if checked {
                        selected.push(reference.clone());
                    } else {
                        selected.retain(|candidate| !model::refs_match(candidate, reference));
                    }
                }
            }
        });
}

fn payload_summary(expression: &PayloadExpr) -> String {
    let leaves = payload_leaf_count(expression);
    if leaves == 1 {
        "Filter: 1 byte pattern".to_string()
    } else {
        format!("Filter: {leaves} byte patterns")
    }
}

fn payload_leaf_count(expression: &PayloadExpr) -> usize {
    match expression {
        PayloadExpr::Leaf(_) => 1,
        PayloadExpr::Group(group) => group.args.iter().map(payload_leaf_count).sum(),
    }
}

fn payload_filter_dialog(app: &mut App, ui: &mut Ui, editor: &mut TaskEditor) {
    let Some(mut dialog) = editor.payload_filter.take() else {
        return;
    };
    let theme = app.theme;
    let palette = app.palette();
    let mut keep = true;
    let mut apply = false;
    let mut close = false;
    let response = egui::Modal::new(egui::Id::unique("hodeco-task-payload-filter"))
        .frame(widgets::modal_frame(theme))
        .show(ui.ctx(), |ui| {
            widgets::dialog_width(ui, 800.0);
            widgets::dialog_header(
                ui,
                theme,
                "Payload filter",
                Some("Match raw payload bytes for this message trigger."),
            );
            widgets::dialog_body(ui, 560.0, |ui| {
                if dialog.unsupported {
                    ui.label(
                        RichText::new(
                            "This trigger uses a payload expression the visual editor cannot represent. Applying replaces it with the conditions below.",
                        )
                        .size(13.5)
                        .color(palette.amber),
                    );
                    ui.add_space(6.0);
                }
                super::messages::payload_conditions_list(
                    app,
                    ui,
                    &mut dialog.draft,
                    &mut dialog.condition_editor,
                );
            });
            widgets::dialog_footer(ui, theme, dialog.error.as_deref(), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui
                    .add_enabled(!dialog.draft.exceeds_limits(), egui::Button::new("Apply"))
                    .clicked()
                {
                    apply = true;
                }
            });
            false
        });
    super::messages::payload_condition_modal(
        app,
        ui.ctx(),
        theme,
        &mut dialog.condition_editor,
        &mut dialog.draft,
    );
    if response.should_close() && !apply && dialog.condition_editor.is_none() {
        close = true;
    }
    if apply {
        match dialog.draft.payload_expr() {
            Ok(payload) => {
                let mut found = false;
                for trigger in &mut editor.draft.triggers {
                    if trigger.id() == dialog.trigger_id
                        && let TaskTrigger::Message(message) = trigger
                    {
                        message.payload = payload.clone();
                        found = true;
                    }
                }
                if found {
                    keep = false;
                } else {
                    dialog.error = Some("The trigger no longer exists.".to_string());
                }
            }
            Err(error) => dialog.error = Some(error),
        }
    }
    if close {
        keep = false;
    }
    if keep {
        editor.payload_filter = Some(dialog);
    }
}

fn payload_conditions_from_expr(expression: &PayloadExpr) -> Option<Vec<PayloadConditionDraft>> {
    let conditions: &[PayloadExpr] = match expression {
        PayloadExpr::Group(group) if group.op == PayloadOp::Or && !group.neg => &group.args,
        _ => std::slice::from_ref(expression),
    };
    conditions.iter().map(payload_condition_from_expr).collect()
}

fn payload_condition_from_expr(expression: &PayloadExpr) -> Option<PayloadConditionDraft> {
    match expression {
        PayloadExpr::Leaf(leaf) if !leaf.neg => {
            Some(PayloadConditionDraft::Raw(RawConditionDraft {
                leaves: vec![raw_leaf_draft(leaf)],
            }))
        }
        PayloadExpr::Group(group) if group.op == PayloadOp::And && !group.neg => {
            let leaves = group
                .args
                .iter()
                .map(|argument| match argument {
                    PayloadExpr::Leaf(leaf) if !leaf.neg => Some(raw_leaf_draft(leaf)),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            Some(PayloadConditionDraft::Raw(RawConditionDraft { leaves }))
        }
        _ => None,
    }
}

fn raw_leaf_draft(leaf: &PayloadLeaf) -> RawLeafDraft {
    let (anywhere, offset) = match leaf.offset {
        PayloadOffset::At(offset) => (false, offset.to_string()),
        PayloadOffset::Anywhere => (true, "0".to_string()),
    };
    RawLeafDraft {
        anywhere,
        offset,
        bytes: hodeco_codec::hex::spaced_hex(leaf.bytes.as_slice()),
        mask: leaf
            .mask
            .as_ref()
            .map(|mask| hodeco_codec::hex::spaced_hex(mask.as_slice()))
            .unwrap_or_default(),
    }
}

fn limits_section(app: &mut App, ui: &mut Ui, editor: &mut TaskEditor) {
    let palette = app.palette();
    if !editor.history_enabled && editor.draft.history.max_rows == 0 {
        editor.draft.history.max_rows = DEFAULT_HISTORY_ROWS;
    }
    let mut keep_history = editor.history_enabled;
    widgets::control_group_fill(ui, "Execution", |ui| {
        egui::Grid::new("task-limits-grid")
            .num_columns(2)
            .spacing([18.0, 4.0])
            .show(ui, |ui| {
            widgets::field_label(ui, app.theme, "Fail fast");
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

            widgets::field_label(ui, app.theme, "Run timeout");
            widgets::field_with_note(ui, app.theme,
                "Maximum total run time, including start delay; child processes are cancelled.",
                |ui| {
                    widgets::duration_editor(
                        ui,
                        &mut editor.run_timeout,
                        std::time::Duration::from_secs(1),
                    );
                },
            );
            ui.end_row();

            widgets::field_label(ui, app.theme, "Runs per minute");
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
    });

    ui.add_space(4.0);
    widgets::control_group_fill(ui, "Start delay", |ui| {
        let delay_toggle = ui.checkbox(&mut editor.delay_enabled, "Enable start delay");
        if delay_toggle.changed() && editor.delay_enabled && editor.delay.trim().is_empty() {
            editor.delay = "1m".to_string();
        }
        ui.label(
            RichText::new(
                "Wait before actions run. Applies to every start, including manual and child-task runs; the wait is included in Run timeout.",
            )
            .small()
            .color(palette.muted),
        );
        if editor.delay_enabled {
            ui.add_space(4.0);
            egui::Grid::new("task-start-delay-grid")
                .num_columns(2)
                .spacing([18.0, 4.0])
                .show(ui, |ui| {
                    widgets::field_label(ui, app.theme, "Delay");
                    widgets::field_with_note(
                        ui,
                        app.theme,
                        "The delay is reduced or extended by the jitter.",
                        |ui| {
                            widgets::duration_editor(
                                ui,
                                &mut editor.delay,
                                std::time::Duration::from_secs(1),
                            );
                        },
                    );
                    ui.end_row();
                });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let jitter_toggle = ui.checkbox(&mut editor.jitter_enabled, "Add jitter");
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
                    "Positive magnitude; offsets range from −jitter to +jitter. Must not exceed the delay.",
                )
                .small()
                .color(palette.muted),
            );
        }
    });

    ui.add_space(4.0);
    widgets::control_group_fill(ui, "History log", |ui| {
        if ui
            .checkbox(&mut keep_history, "Enable history log")
            .on_hover_text("Save completed runs to a per-task history file")
            .changed()
        {
            editor.history_enabled = keep_history;
            if keep_history {
                if editor.draft.history.max_rows == 0 {
                    editor.draft.history.max_rows = DEFAULT_HISTORY_ROWS;
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
                editor.draft.history.max_rows = DEFAULT_HISTORY_ROWS;
            }
        }
        ui.indent("task-history-settings", |ui| {
            ui.add_enabled_ui(keep_history, |ui| {
                egui::Grid::new("task-history-grid")
                    .num_columns(2)
                    .spacing([18.0, 4.0])
                    .show(ui, |ui| {
                        widgets::field_label(ui, app.theme, "Max size");
                        widgets::field_with_note(
                            ui,
                            app.theme,
                            "History file size threshold; compaction uses the shared factor and interval.",
                            |ui| {
                                size_editor(ui, &mut editor.history_size, 1024);
                            },
                        );
                        ui.end_row();
                        widgets::field_label(ui, app.theme, "Max rows");
                        widgets::field_with_note(
                            ui,
                            app.theme,
                            "Maximum retained runs for this task.",
                            |ui| {
                                ui.add(
                                    egui::DragValue::new(&mut editor.draft.history.max_rows)
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
                    "Completed runs are not saved to history; maxRuns still uses the in-memory trigger counter.",
                )
                .small()
                .color(palette.muted),
            );
        } else {
            ui.label(
                RichText::new("Max size must be at least 1 KiB.")
                    .small()
                    .color(palette.muted),
            );
        }
    });
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if ui
            .button("Reset limits to defaults")
            .on_hover_text("Fail fast on, 5m timeout, no delay, 60 runs/minute, history off")
            .clicked()
        {
            let defaults = TaskDefinition::default();
            editor.draft.fail_fast = defaults.fail_fast;
            editor.draft.max_runs_per_minute = defaults.max_runs_per_minute;
            editor.run_timeout = defaults.run_timeout.to_string();
            editor.delay_enabled = false;
            editor.delay.clear();
            editor.jitter_enabled = false;
            editor.jitter = "30s".to_string();
            editor.history_size = defaults.history.max_size.to_string();
            editor.history_enabled = defaults.history.max_rows > 0;
            editor.draft.history = defaults.history;
        }
        if !editor.is_new
            && ui
                .button("Purge history…")
                .on_hover_text("Delete all runs for this task; maxRuns counters reset")
                .clicked()
        {
            app.purge_task_history(&editor.draft.id);
        }
    });
}

fn parse_optional_duration(value: &str, label: &str) -> Result<Option<HumanDuration>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    HumanDuration::from_str(value)
        .map(Some)
        .map_err(|error| format!("{label}: {error}"))
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
            return Some((amount, unit));
        }
    }
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
    let theme = app.theme;
    let mut close = false;
    let mut run = false;
    let response = egui::Modal::new(egui::Id::unique("hodeco-run-now"))
        .frame(widgets::modal_frame(theme))
        .show(ui.ctx(), |ui| {
            widgets::dialog_width(ui, 540.0);
            widgets::dialog_header(
                ui,
                theme,
                "Run now",
                Some("Start the task once with an optional payload and metadata."),
            );
            widgets::dialog_body(ui, 420.0, |ui| {
                egui::Grid::new("run-now-grid")
                    .num_columns(2)
                    .spacing([18.0, 4.0])
                    .show(ui, |ui| {
                        widgets::field_label(ui, theme, "Payload");
                        egui::ComboBox::from_id_salt("run-payload-mode")
                            .selected_text(dialog.payload_mode.label())
                            .show_ui(ui, |ui| {
                                for mode in PayloadMode::ALL {
                                    ui.selectable_value(
                                        &mut dialog.payload_mode,
                                        mode,
                                        mode.label(),
                                    );
                                }
                            });
                        ui.end_row();
                        if dialog.payload_mode != PayloadMode::None {
                            widgets::field_label(ui, theme, "Value");
                            widgets::field_with_note(
                                ui,
                                theme,
                                "Payload becomes the run trigger payload; None leaves it empty.",
                                |ui| {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut dialog.payload)
                                            .desired_rows(3)
                                            .desired_width(360.0),
                                    );
                                },
                            );
                            ui.end_row();
                        }
                        widgets::field_label(ui, theme, "Metadata");
                        widgets::field_with_note(
                            ui,
                            theme,
                            "Optional JSON object. Commands receive it on stdin's run.metadata; it is also stored on the run trigger.",
                            |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut dialog.metadata)
                                        .hint_text("{ \"key\": \"value\" }")
                                        .desired_rows(3)
                                        .desired_width(360.0),
                                );
                            },
                        );
                        ui.end_row();
                    });
            });
            widgets::dialog_footer(ui, theme, dialog.error.as_deref(), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui
                    .add_enabled(!dialog.sending, egui::Button::new("Run"))
                    .clicked()
                {
                    run = true;
                }
            });
            false
        });
    if response.should_close() && !run {
        close = true;
    }

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
            running_run_id: None,
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
    fn filters_by_labels_with_or_semantics() {
        let mut evening = info("evening", true);
        evening.task.labels = vec!["lighting".to_string(), "automated".to_string()];
        let mut winter = info("winter", true);
        winter.task.labels = vec!["Lighting".to_string()];
        let plain = info("plain", true);
        let mut ui = TasksUi {
            tasks: vec![evening, winter, plain],
            ..TasksUi::default()
        };
        assert_eq!(ui.filtered().len(), 3);

        ui.selected_labels.insert("automated".to_string());
        assert_eq!(ui.filtered().len(), 1);
        assert_eq!(ui.filtered()[0].task.id, "evening");

        ui.selected_labels.insert("lighting".to_string());
        assert_eq!(ui.filtered().len(), 2);

        ui.selected_labels.clear();
        assert_eq!(ui.filtered().len(), 3);

        let options = ui.label_options();
        assert_eq!(options.len(), 2);
        assert_eq!(
            options
                .iter()
                .find(|option| option.key == "lighting")
                .unwrap()
                .count,
            2
        );
    }

    #[test]
    fn task_search_does_not_match_labels() {
        let mut ui = TasksUi {
            tasks: vec![info("plain", true)],
            ..TasksUi::default()
        };
        ui.tasks[0].task.labels = vec!["automated".to_string()];
        ui.search = "automated".to_string();
        assert!(ui.filtered().is_empty());
        ui.search = "plain".to_string();
        assert_eq!(ui.filtered().len(), 1);
    }

    #[test]
    fn filter_chips_wrap_in_a_narrow_row() {
        let mut height = 0.0;
        egui::__run_test_ui(|ui| {
            ui.set_max_width(100.0);
            let response = ui.horizontal_wrapped(|ui| {
                for index in 0..10 {
                    widgets::removable_chip_button(
                        ui,
                        crate::theme::UiTheme::Dark,
                        &format!("label-{index}"),
                        widgets::Tone::Info,
                    );
                }
            });
            height = response.response.rect.height();
        });
        assert!(height > 30.0, "chips did not wrap: height {height}");
    }

    #[test]
    fn label_picker_search_is_case_insensitive() {
        let options = vec![
            LabelOption {
                key: "automated".to_string(),
                label: "automated".to_string(),
                count: 1,
            },
            LabelOption {
                key: "lighting".to_string(),
                label: "lighting".to_string(),
                count: 2,
            },
        ];
        assert_eq!(matching_labels(&options, "").len(), 2);
        assert_eq!(matching_labels(&options, "AUTO").len(), 1);
        assert_eq!(matching_labels(&options, "light").len(), 1);
        assert!(matching_labels(&options, "missing").is_empty());
    }

    #[test]
    fn prunes_stale_selected_labels() {
        let mut ui = TasksUi {
            tasks: vec![info("plain", true)],
            ..TasksUi::default()
        };
        ui.selected_labels.insert("missing".to_string());
        ui.prune_selected_labels();
        assert!(ui.selected_labels.is_empty());
    }

    #[test]
    fn task_editor_validates_and_trims_labels() {
        let mut editor = TaskEditor::new(
            TaskDefinition {
                name: "Labeled".to_string(),
                labels: vec![" automated ".to_string(), "lighting".to_string()],
                ..TaskDefinition::default()
            },
            true,
        );
        let task = editor.build().unwrap();
        assert_eq!(task.labels, vec!["automated", "lighting"]);

        editor.draft.labels = vec!["Automated".to_string(), "automated".to_string()];
        assert!(editor.build().is_err());

        editor.draft.labels = vec!["x".repeat(MAX_TASK_LABEL_LEN + 1)];
        assert!(editor.build().is_err());

        editor.draft.labels = (0..=MAX_TASK_LABELS)
            .map(|index| format!("label-{index}"))
            .collect();
        assert!(editor.build().is_err());

        editor.draft.labels = vec![String::new()];
        assert!(editor.build().is_err());
    }

    #[test]
    fn empty_command_arguments_are_dropped_when_built() {
        let mut editor = TaskEditor::new(
            TaskDefinition {
                name: "Test".to_string(),
                ..TaskDefinition::default()
            },
            true,
        );
        editor.draft.actions.push(TaskAction {
            id: "action-1".to_string(),
            kind: ActionKind::Command(CommandAction {
                command: "cmd".to_string(),
                args: vec![String::new(), "  ".to_string(), " --json ".to_string()],
                output: OutputMode::Ignore,
            }),
        });
        let task = editor.build().unwrap();
        assert_eq!(task.actions[0].command().unwrap().args, vec!["--json"]);
    }

    #[test]
    fn empty_selector_arguments_are_dropped_when_built() {
        let mut editor = TaskEditor::new(
            TaskDefinition {
                name: "Test".to_string(),
                ..TaskDefinition::default()
            },
            true,
        );
        editor.draft.next.push(NextRule {
            when: NextCondition::CommandResult,
            command: Some("cmd".to_string()),
            args: vec![String::new(), "  ".to_string(), " --json ".to_string()],
            ..NextRule::default()
        });
        let task = editor.build().unwrap();
        assert_eq!(task.next[0].args, vec!["--json"]);
    }

    #[test]
    fn task_editor_validates_structured_sections() {
        let mut editor = TaskEditor::new(
            TaskDefinition {
                name: "Test".to_string(),
                ..TaskDefinition::default()
            },
            true,
        );
        assert!(editor.build().is_ok());
        editor.draft.actions.push(TaskAction {
            id: String::new(),
            kind: ActionKind::Command(CommandAction {
                command: "cmd".to_string(),
                args: Vec::new(),
                output: OutputMode::Ignore,
            }),
        });
        assert!(editor.build().is_err());
        editor.draft.actions[0].id = "action-1".to_string();
        assert!(editor.build().is_ok());
        editor.draft.triggers.push(TaskTrigger::Http(HttpTrigger {
            id: "http-1".to_string(),
            methods: Vec::new(),
            secret: None,
        }));
        assert!(editor.build().is_ok());
        editor.draft.triggers.push(TaskTrigger::Http(HttpTrigger {
            id: "http-1".to_string(),
            methods: Vec::new(),
            secret: None,
        }));
        assert!(editor.build().is_err());
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
            delayed_until: None,
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

    #[test]
    fn task_editor_validates_start_delay_and_jitter() {
        let mut editor = TaskEditor::new(
            TaskDefinition {
                name: "Delayed".to_string(),
                ..TaskDefinition::default()
            },
            true,
        );
        editor.run_timeout = "10m".to_string();
        editor.delay_enabled = true;
        editor.delay = "5m".to_string();
        editor.jitter_enabled = true;
        editor.jitter = "30s".to_string();
        let task = editor.build().unwrap();
        assert_eq!(
            task.delay.unwrap().as_std(),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(
            task.jitter.unwrap().as_std(),
            std::time::Duration::from_secs(30)
        );

        editor.run_timeout = "5m".to_string();
        assert!(editor.build().is_err());

        editor.run_timeout = "10m".to_string();
        editor.delay.clear();
        assert!(editor.build().is_err());

        editor.delay = "5m".to_string();
        editor.jitter = "6m".to_string();
        assert!(editor.build().is_err());

        editor.jitter = "-30s".to_string();
        let error = editor.build().unwrap_err();
        assert!(error.contains("positive magnitude"), "{error}");

        editor.delay_enabled = false;
        editor.jitter_enabled = false;
        let task = editor.build().unwrap();
        assert_eq!(task.delay, None);
        assert_eq!(task.jitter, None);
    }

    #[test]
    fn task_payload_filter_round_trips_raw_conditions() {
        let expression = PayloadExpr::or(vec![
            PayloadExpr::and(vec![
                PayloadExpr::leaf(PayloadOffset::At(2), vec![0x01, 0x02]),
                PayloadExpr::masked_leaf(PayloadOffset::Anywhere, vec![0xaa], vec![0x0f]),
            ]),
            PayloadExpr::leaf(PayloadOffset::At(0), vec![0xff]),
        ]);
        let conditions = payload_conditions_from_expr(&expression).expect("convertible");
        let draft = FilterDraft {
            payload_conditions: conditions,
            ..FilterDraft::default()
        };
        let round_tripped = draft.payload_expr().unwrap().expect("expression");
        assert!(round_tripped.matches(&[0x00, 0x00, 0x01, 0x02, 0xaa]));
        assert!(round_tripped.matches(&[0xff]));
        assert!(!round_tripped.matches(&[0x00, 0x00, 0x01, 0x02, 0xab]));
        assert!(
            payload_conditions_from_expr(
                &PayloadExpr::leaf(PayloadOffset::At(0), vec![0xff]).negated()
            )
            .is_none()
        );
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
        assert!(editor.build().is_err());
    }

    #[test]
    fn history_can_be_disabled_after_enabling_it() {
        let mut editor = TaskEditor::new(
            TaskDefinition {
                name: "Toggle".to_string(),
                ..TaskDefinition::default()
            },
            true,
        );
        assert!(!editor.history_enabled);
        assert!(!editor.is_dirty());
        editor.history_enabled = true;
        editor.draft.history.max_rows = DEFAULT_HISTORY_ROWS;
        editor.history_size = "100 KiB".to_string();
        let enabled = editor.build().unwrap();
        assert_eq!(enabled.history.max_rows, DEFAULT_HISTORY_ROWS);

        editor.history_enabled = false;
        editor.draft.history.max_rows = DEFAULT_HISTORY_ROWS;
        editor.history_size = "not a size".to_string();
        let disabled = editor.build().unwrap();
        assert_eq!(disabled.history.max_rows, 0);
        assert_eq!(disabled.history.max_size.as_bytes(), 0);
    }
}
