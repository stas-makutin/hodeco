//! Runs list and inline run detail pane.

use super::super::{App, MessageView};
use crate::model;
use crate::ui::widgets;
use chrono::{DateTime, Duration, Utc};
use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use hodeco_contract::api::payloads::MessageTimeRange;
use hodeco_contract::domain::{
    Message, RunOutcome, RunSummary, RunTrigger, TaskRun, TriggerRef, TriggerType,
};
use hodeco_contract::utils::Base64Bytes;
use std::collections::BTreeSet;

pub(crate) const RUN_PAGE_LIMIT: u32 = 50;
pub(crate) const RUN_LIST_CAP: usize = 1_000;

pub(crate) fn sorted_runs(mut runs: Vec<RunSummary>) -> Vec<RunSummary> {
    runs.sort_by_key(|run| (run.time, run.id));
    runs.dedup_by_key(|run| run.id);
    runs
}

#[derive(Debug, Clone)]
pub(crate) struct RunningRun {
    pub id: uuid::Uuid,
    pub task_id: String,
    pub trigger: TriggerRef,
    pub started_at: DateTime<Utc>,
    pub delayed_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
enum RunActivation {
    Running(uuid::Uuid),
    History(String, uuid::Uuid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunOutcomeFilter {
    Running,
    Outcome(RunOutcome),
}

impl RunOutcomeFilter {
    pub(crate) const ALL: [Self; 4] = [
        Self::Running,
        Self::Outcome(RunOutcome::Succeeded),
        Self::Outcome(RunOutcome::Failed),
        Self::Outcome(RunOutcome::Cancelled),
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Outcome(outcome) => outcome.label(),
        }
    }
}

pub(crate) fn clear_run_selection(app: &mut App) {
    app.runs.selected = None;
    app.runs.selected_running = None;
    app.runs.selected_index = None;
    app.runs.detail = None;
    app.runs.detail_loading = false;
    app.runs.detail_error = None;
}

fn activation_matches(app: &App, activation: &RunActivation) -> bool {
    match activation {
        RunActivation::Running(id) => app.runs.selected_running == Some(*id),
        RunActivation::History(task_id, run_id) => app
            .runs
            .selected
            .as_ref()
            .is_some_and(|(task, run)| task == task_id && run == run_id),
    }
}

fn apply_run_activation(app: &mut App, activation: RunActivation) {
    match activation {
        RunActivation::Running(id) => {
            app.runs.selected = None;
            app.runs.selected_running = Some(id);
            app.runs.detail = None;
            app.runs.detail_loading = false;
            app.runs.detail_error = None;
        }
        RunActivation::History(task_id, run_id) => app.select_run(&task_id, run_id),
    }
}

#[derive(Debug)]
pub(crate) struct RunsUi {
    /// Completed runs in oldest-first display order.
    pub runs: Vec<RunSummary>,
    pub running: Vec<RunningRun>,
    pub loading: bool,
    pub loading_older: bool,
    pub loading_newer: bool,
    pub older_next: Option<MessageTimeRange>,
    pub newer_next: Option<MessageTimeRange>,
    pub feed_rows: u64,
    pub feed_bytes: u64,
    pub feed_max_rows: u32,
    pub feed_max_size: u64,
    pub error: Option<String>,
    pub filter_task: Option<String>,
    pub filter_type: Option<TriggerType>,
    pub filter_outcome: Option<RunOutcomeFilter>,
    pub period_hours: u32,
    pub at_bottom: bool,
    pub scroll_to_bottom: bool,
    pub scroll_anchor: Option<uuid::Uuid>,
    pub visible_start: u64,
    pub selected: Option<(String, uuid::Uuid)>,
    pub selected_running: Option<uuid::Uuid>,
    pub selected_index: Option<usize>,
    pub detail: Option<TaskRun>,
    pub detail_loading: bool,
    pub detail_error: Option<String>,
    pub cancelling: BTreeSet<uuid::Uuid>,
    pub generation: u64,
    pub pending_new: usize,
    /// Finish events received while a feed page is in flight.
    pub pending_feed: Vec<RunSummary>,
    /// Runs dropped while a feed page is in flight.
    pub pending_dropped: std::collections::HashSet<uuid::Uuid>,
}

impl RunsUi {
    pub fn filter_matches(&self, run: &RunSummary) -> bool {
        self.filter_task
            .as_ref()
            .is_none_or(|task| task == &run.task_id)
            && self.filter_type.is_none_or(|kind| kind == run.trigger.kind)
            && self.filter_outcome.is_none_or(|filter| match filter {
                RunOutcomeFilter::Running => false,
                RunOutcomeFilter::Outcome(outcome) => outcome == run.outcome,
            })
    }

    pub fn running_matches(&self, run: &RunningRun) -> bool {
        self.filter_task
            .as_ref()
            .is_none_or(|task| task == &run.task_id)
            && self.filter_type.is_none_or(|kind| kind == run.trigger.kind)
            && !matches!(self.filter_outcome, Some(RunOutcomeFilter::Outcome(_)))
    }
}

impl Default for RunsUi {
    fn default() -> Self {
        Self {
            runs: Vec::new(),
            running: Vec::new(),
            loading: false,
            loading_older: false,
            loading_newer: false,
            older_next: None,
            newer_next: None,
            feed_rows: 0,
            feed_bytes: 0,
            feed_max_rows: 1_000,
            feed_max_size: 4 * 1024 * 1024,
            error: None,
            filter_task: None,
            filter_type: None,
            filter_outcome: None,
            period_hours: 24,
            at_bottom: true,
            scroll_to_bottom: false,
            scroll_anchor: None,
            visible_start: 0,
            selected: None,
            selected_running: None,
            selected_index: None,
            detail: None,
            detail_loading: false,
            detail_error: None,
            cancelling: BTreeSet::new(),
            generation: 0,
            pending_new: 0,
            pending_feed: Vec::new(),
            pending_dropped: std::collections::HashSet::new(),
        }
    }
}

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let live = app.gateway.phase().is_live();
    if (app.runs.selected.is_some() || app.runs.selected_running.is_some())
        && app.dialog.is_none()
        && !ui.ctx().any_popup_open()
        && ui.input(|input| input.key_pressed(egui::Key::Escape))
    {
        clear_run_selection(app);
        ui.ctx().memory_mut(|memory| {
            if let Some(focused) = memory.focused() {
                memory.surrender_focus(focused);
            }
            memory.stop_text_input();
        });
    }
    run_context(app, ui);

    let task_options: Vec<(String, String)> = app
        .tasks
        .tasks
        .iter()
        .map(|info| (info.task.id.clone(), info.task.name.clone()))
        .collect();
    let visible_completed = app
        .runs
        .runs
        .iter()
        .filter(|run| app.runs.filter_matches(run))
        .count();

    widgets::page_header(
        ui,
        theme,
        "Runs",
        Some(&if app.runs.running.is_empty() {
            format!("{visible_completed} recent")
        } else {
            format!(
                "{visible_completed} recent · {} running",
                app.runs.running.len()
            )
        }),
        |ui| {
            if ui.add_enabled(live, egui::Button::new("Refresh")).clicked() {
                app.refresh_runs();
            }
        },
    );

    ui.horizontal_wrapped(|ui| {
        ui.label("Task");
        egui::ComboBox::from_id_salt("runs-task-filter")
            .selected_text(
                app.runs
                    .filter_task
                    .as_ref()
                    .and_then(|id| task_options.iter().find(|(task, _)| task == id))
                    .map(|(_, name)| name.clone())
                    .unwrap_or_else(|| "Any".to_string()),
            )
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(app.runs.filter_task.is_none(), "Any")
                    .clicked()
                {
                    app.runs.filter_task = None;
                    app.refresh_runs();
                }
                for (id, name) in &task_options {
                    if ui
                        .selectable_label(app.runs.filter_task.as_deref() == Some(id), name)
                        .clicked()
                    {
                        app.runs.filter_task = Some(id.clone());
                        app.refresh_runs();
                    }
                }
            });
        ui.label("Trigger");
        egui::ComboBox::from_id_salt("runs-type-filter")
            .selected_text(
                app.runs
                    .filter_type
                    .map(|kind| kind.label())
                    .unwrap_or("Any"),
            )
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(app.runs.filter_type.is_none(), "Any")
                    .clicked()
                {
                    app.runs.filter_type = None;
                    app.refresh_runs();
                }
                for kind in [
                    TriggerType::Message,
                    TriggerType::Schedule,
                    TriggerType::Http,
                    TriggerType::Manual,
                    TriggerType::Task,
                ] {
                    if ui
                        .selectable_label(app.runs.filter_type == Some(kind), kind.label())
                        .clicked()
                    {
                        app.runs.filter_type = Some(kind);
                        app.refresh_runs();
                    }
                }
            });
        ui.label("Outcome");
        egui::ComboBox::from_id_salt("runs-outcome-filter")
            .selected_text(
                app.runs
                    .filter_outcome
                    .map(|outcome| outcome.label())
                    .unwrap_or("Any"),
            )
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(app.runs.filter_outcome.is_none(), "Any")
                    .clicked()
                {
                    app.runs.filter_outcome = None;
                    app.refresh_runs();
                }
                for outcome in RunOutcomeFilter::ALL {
                    if ui
                        .selectable_label(app.runs.filter_outcome == Some(outcome), outcome.label())
                        .clicked()
                    {
                        app.runs.filter_outcome = Some(outcome);
                        app.refresh_runs();
                    }
                }
            });
        ui.label("Period");
        egui::ComboBox::from_id_salt("runs-period")
            .selected_text(match app.runs.period_hours {
                0 => "All time".to_string(),
                1 => "Last hour".to_string(),
                24 => "Last 24 hours".to_string(),
                168 => "Last 7 days".to_string(),
                720 => "Last 30 days".to_string(),
                hours => format!("Last {hours} h"),
            })
            .show_ui(ui, |ui| {
                for (hours, label) in [
                    (0, "All time"),
                    (1, "Last hour"),
                    (24, "Last 24 hours"),
                    (168, "Last 7 days"),
                    (720, "Last 30 days"),
                ] {
                    if ui
                        .selectable_label(app.runs.period_hours == hours, label)
                        .clicked()
                    {
                        app.runs.period_hours = hours;
                        app.refresh_runs();
                    }
                }
            });
        if app.runs.loading {
            ui.spinner();
        }
        ui.label(
            RichText::new(format!(
                "Feed: {} / {}",
                app.runs.feed_rows, app.runs.feed_max_rows
            ))
            .small()
            .color(palette.muted),
        )
        .on_hover_text(format!(
            "{} / {} bytes used",
            app.runs.feed_bytes, app.runs.feed_max_size
        ));
        if app.runs.pending_new > 0
            && ui
                .button(format!("{} new run(s)", app.runs.pending_new))
                .clicked()
        {
            app.runs.pending_new = 0;
            app.runs.scroll_anchor = None;
            app.runs.scroll_to_bottom = true;
            app.runs.at_bottom = true;
        }
        if app.runs.loading_older || app.runs.loading_newer {
            ui.spinner();
        }
        if let Some(error) = &app.runs.error {
            ui.label(RichText::new(error).color(palette.red));
        }
    });
    ui.add_space(4.0);

    let mut running: Vec<RunningRun> = app
        .runs
        .running
        .iter()
        .filter(|run| app.runs.running_matches(run))
        .cloned()
        .collect();
    running.sort_by_key(|run| run.started_at);
    let runs: Vec<RunSummary> = app
        .runs
        .runs
        .iter()
        .filter(|run| app.runs.filter_matches(run))
        .cloned()
        .collect();
    let completed_count = runs.len();
    let running_count = running.len();
    let total = completed_count + running_count;
    let activation_for = |index: usize| -> Option<RunActivation> {
        if index < completed_count {
            runs.get(index)
                .map(|run| RunActivation::History(run.task_id.clone(), run.id))
        } else {
            running
                .get(index - completed_count)
                .map(|run| RunActivation::Running(run.id))
        }
    };
    let selected_by_id = app
        .runs
        .selected_running
        .and_then(|id| {
            running
                .iter()
                .position(|run| run.id == id)
                .map(|index| index + completed_count)
        })
        .or_else(|| {
            app.runs.selected.as_ref().and_then(|(task, id)| {
                runs.iter()
                    .position(|run| run.task_id == *task && run.id == *id)
            })
        });
    if (app.runs.selected.is_some() || app.runs.selected_running.is_some())
        && selected_by_id.is_none()
    {
        clear_run_selection(app);
    }
    let mut selected =
        selected_by_id.or_else(|| app.runs.selected_index.filter(|index| *index < total));
    let nav = if app.dialog.is_none() && total > 0 {
        widgets::list_nav_global(ui)
    } else {
        None
    };
    let mut activate = None;
    if let Some(nav) = nav {
        let next = nav.apply(selected, total);
        selected = Some(next);
        activate = activation_for(next);
    }
    if let Some(activation) = activate.take() {
        apply_run_activation(app, activation);
    }
    if running_count > 0 {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
    }
    let mut first_visible_index = None;
    let mut last_visible_index = None;
    let mut action_focus_id = None;
    let scroll_to = if nav.is_some() {
        selected.map(|index| (index, egui::Align::Center))
    } else if let Some(anchor) = app.runs.scroll_anchor.take() {
        runs.iter()
            .position(|run| run.id == anchor)
            .map(|index| (index, egui::Align::Center))
    } else if app.runs.scroll_to_bottom && total > 0 {
        Some((total - 1, egui::Align::BOTTOM))
    } else {
        None
    };
    app.runs.scroll_to_bottom = false;

    let cancel_available = selected
        .and_then(|index| index.checked_sub(completed_count))
        .and_then(|index| running.get(index))
        .is_some_and(|run| !app.runs.cancelling.contains(&run.id));
    let list_focus = widgets::list_focus_with_tab(
        ui,
        theme,
        "runs-list",
        nav.is_some(),
        false,
        cancel_available,
    );
    if nav.is_some() && !list_focus.focused {
        ui.ctx()
            .memory_mut(|memory| memory.request_focus(list_focus.id));
    }
    let focus_row_actions = cancel_available && widgets::list_tab_pressed(ui, list_focus.id);
    if list_focus.focused
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        && let Some(activation) = selected.and_then(activation_for)
    {
        apply_run_activation(app, activation);
    }

    // Row clicks must win over text selection in the cells, as in Messages.
    ui.style_mut().interaction.selectable_labels = false;
    let ctx = ui.ctx().clone();

    egui::ScrollArea::horizontal()
        .id_salt("runs-horizontal")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.set_min_width(1050.0);
            let mut table = TableBuilder::new(ui)
                .id_salt("runs-table")
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::CLICK)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(150.0).at_least(120.0))
                .column(Column::initial(160.0).at_least(120.0))
                .column(Column::initial(140.0).at_least(100.0))
                .column(Column::initial(100.0).at_least(80.0))
                .column(Column::initial(90.0).at_least(70.0))
                .column(Column::initial(70.0).at_least(50.0))
                .column(Column::initial(70.0).at_least(50.0))
                .column(Column::remainder().at_least(160.0));
            if let Some((index, align)) = scroll_to {
                table = table.scroll_to_row(index, Some(align));
            }
            table
                .header(28.0, |mut header| {
                    for (title, hover) in [
                        ("Time", "When the run was recorded"),
                        ("Task", "Task that produced the run"),
                        ("Trigger", "Trigger ID and type"),
                        ("Outcome", "Run result"),
                        ("Duration", "Start to finish"),
                        ("Control", "Row actions; Cancel is available while running"),
                        ("Next tasks", "Number of next tasks started by this run"),
                        ("Error", "Run error, if any"),
                    ] {
                        header.col(|ui| {
                            ui.label(RichText::new(title).strong()).on_hover_text(hover);
                        });
                    }
                })
                .body(|body| {
                    body.rows(30.0, total, |mut row| {
                        let index = row.index();
                        if first_visible_index.is_none() {
                            first_visible_index = Some(index);
                        }
                        last_visible_index = Some(index);
                        if index < completed_count {
                            let run = &runs[index];
                            row.set_selected(selected == Some(index));
                            row.col(|ui| {
                                ui.label(RichText::new(model::format_datetime(run.time)).small());
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(&run.task_id).small());
                            });
                            row.col(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {}",
                                        run.trigger.kind.label(),
                                        run.trigger.id
                                    ))
                                    .small(),
                                );
                            });
                            row.col(|ui| {
                                let color = match run.outcome {
                                    RunOutcome::Succeeded => palette.green,
                                    RunOutcome::Failed => palette.red,
                                    RunOutcome::Cancelled => palette.amber,
                                };
                                ui.label(RichText::new(run.outcome.label()).small().color(color));
                            });
                            row.col(|ui| {
                                let duration = run.finished_at - run.started_at;
                                ui.label(RichText::new(model::format_duration(duration)).small());
                            });
                            row.col(|ui| {
                                ui.label(RichText::new("—").color(palette.muted));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(run.next_count.to_string()).small());
                            });
                            row.col(|ui| {
                                if let Some(error) = &run.error {
                                    ui.label(RichText::new(error).small().color(palette.red));
                                } else {
                                    ui.label(RichText::new("—").color(palette.muted));
                                }
                            });
                            if row.response().clicked() {
                                let activation =
                                    RunActivation::History(run.task_id.clone(), run.id);
                                if activation_matches(app, &activation) {
                                    clear_run_selection(app);
                                    selected = None;
                                } else {
                                    apply_run_activation(app, activation);
                                    selected = Some(index);
                                }
                                ctx.memory_mut(|memory| memory.request_focus(list_focus.id));
                            }
                        } else {
                            let run = &running[index - completed_count];
                            row.set_selected(selected == Some(index));
                            row.col(|ui| {
                                ui.label(
                                    RichText::new(model::format_datetime(run.started_at)).small(),
                                );
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(&run.task_id).small());
                            });
                            row.col(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {}",
                                        run.trigger.kind.label(),
                                        run.trigger.id
                                    ))
                                    .small(),
                                );
                            });
                            let now = Utc::now();
                            let delayed = run
                                .delayed_until
                                .is_some_and(|delayed_until| delayed_until > now);
                            row.col(|ui| {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(
                                        RichText::new(if delayed { "Delayed" } else { "Running" })
                                            .small(),
                                    );
                                });
                            });
                            row.col(|ui| {
                                let response = ui.label(
                                    RichText::new(model::format_duration(now - run.started_at))
                                        .small(),
                                );
                                if let Some(delayed_until) = run.delayed_until {
                                    response.on_hover_text(format!(
                                        "Actions start at {}",
                                        model::format_datetime(delayed_until)
                                    ));
                                }
                            });
                            let mut cancel_clicked = false;
                            row.col(|ui| {
                                let cancelling = app.runs.cancelling.contains(&run.id);
                                let response = ui
                                    .push_id(("run-cancel", run.id), |ui| {
                                        ui.add_enabled(
                                            !cancelling,
                                            egui::Button::new("Cancel").small(),
                                        )
                                    })
                                    .inner;
                                if !cancelling && selected == Some(index) {
                                    action_focus_id = Some(response.id);
                                }
                                if response.on_hover_text("Stop this run").clicked() {
                                    app.cancel_task_run(&run.task_id, run.id);
                                    cancel_clicked = true;
                                }
                            });
                            row.col(|ui| {
                                ui.label(RichText::new("—").color(palette.muted));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new("—").color(palette.muted));
                            });
                            if !cancel_clicked && row.response().clicked() {
                                let activation = RunActivation::Running(run.id);
                                if activation_matches(app, &activation) {
                                    clear_run_selection(app);
                                    selected = None;
                                } else {
                                    apply_run_activation(app, activation);
                                    selected = Some(index);
                                }
                                ctx.memory_mut(|memory| memory.request_focus(list_focus.id));
                            }
                        }
                    });
                });
        });
    // Tab from the selected running row moves to its Cancel action.
    if focus_row_actions {
        if let Some(id) = action_focus_id {
            ui.memory_mut(|memory| memory.request_focus(id));
        } else {
            ui.memory_mut(|memory| {
                if let Some(focused) = memory.focused() {
                    memory.surrender_focus(focused);
                }
            });
        }
    }

    app.runs.visible_start = first_visible_index.unwrap_or(0) as u64;
    app.runs.at_bottom =
        total == 0 || last_visible_index.is_some_and(|last| last.saturating_add(1) >= total);
    if app.runs.at_bottom {
        app.runs.pending_new = 0;
    }
    if !app.runs.loading_older
        && !matches!(app.runs.filter_outcome, Some(RunOutcomeFilter::Running))
        && first_visible_index == Some(0)
        && app.runs.older_next.is_some()
    {
        app.load_older_runs();
    } else if !app.runs.loading_newer
        && completed_count > 0
        && last_visible_index.is_some_and(|last| last.saturating_add(1) >= completed_count)
        && app.runs.newer_next.is_some()
    {
        app.load_newer_runs();
    }
    app.runs.selected_index = selected;

    if total == 0 && !app.runs.loading {
        widgets::empty_state(
            ui,
            theme,
            "No runs",
            "Runs that match the current task, trigger, and period appear here.",
        );
    }
}

fn run_context(app: &mut App, ui: &mut Ui) {
    let selected_running = app
        .runs
        .selected_running
        .and_then(|id| app.runs.running.iter().find(|run| run.id == id).cloned());
    if app.runs.selected.is_none() && selected_running.is_none() {
        return;
    }
    let palette = app.palette();
    let mut close = false;
    let mut refresh = false;
    let mut cancel = None;
    egui::Panel::bottom("hodeco-run-context")
        .resizable(true)
        .default_size(280.0)
        .min_size(140.0)
        .frame(
            egui::Frame::new()
                .fill(palette.panel)
                .stroke(egui::Stroke::new(1.0, palette.border))
                .inner_margin(egui::Margin::symmetric(12, 8)),
        )
        .show(ui, |ui| {
            ui.painter().rect_filled(ui.max_rect(), 0.0, palette.panel);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Run detail").strong());
                let run_id = selected_running
                    .as_ref()
                    .map(|run| run.id)
                    .or_else(|| app.runs.selected.as_ref().map(|(_, id)| *id));
                if let Some(run_id) = run_id {
                    ui.label(
                        RichText::new(run_id.to_string())
                            .small()
                            .color(palette.muted),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("Close")
                        .on_hover_text("Close run detail")
                        .clicked()
                    {
                        close = true;
                    }
                    ui.add_space(18.0);
                    if ui
                        .button("Refresh")
                        .on_hover_text("Refresh the selected run detail")
                        .clicked()
                    {
                        refresh = true;
                    }
                    if let Some(run) = &selected_running {
                        let cancelling = app.runs.cancelling.contains(&run.id);
                        if ui
                            .add_enabled(!cancelling, egui::Button::new("Cancel run"))
                            .on_hover_text("Stop this run and skip its remaining next tasks")
                            .clicked()
                        {
                            cancel = Some((run.task_id.clone(), run.id));
                        }
                    }
                    if selected_running.is_none()
                        && let Some(detail) = &app.runs.detail
                        && ui.button("Copy JSON").clicked()
                    {
                        ui.ctx().copy_text(model::pretty_json(detail));
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(run) = &selected_running {
                        running_detail_ui(ui, run, app);
                    } else if app.runs.detail_loading {
                        ui.spinner();
                    } else if let Some(error) = &app.runs.detail_error {
                        ui.label(RichText::new(error).color(palette.red));
                    } else if let Some(detail) = &app.runs.detail {
                        detail_ui(ui, detail, app);
                    }
                });
            ui.painter().rect_stroke(
                ui.max_rect(),
                0.0,
                egui::Stroke::new(1.0, palette.border),
                egui::StrokeKind::Inside,
            );
        });
    if close {
        clear_run_selection(app);
    }
    if refresh {
        if let Some((task_id, run_id)) = app.runs.selected.clone() {
            app.select_run(&task_id, run_id);
        } else {
            app.refresh_runs();
        }
    }
    if let Some((task_id, run_id)) = cancel {
        app.cancel_task_run(&task_id, run_id);
    }
}

fn running_detail_ui(ui: &mut Ui, run: &RunningRun, app: &App) {
    let palette = app.palette();
    let now = Utc::now();
    let delayed = run.delayed_until.is_some_and(|until| until > now);
    egui::CollapsingHeader::new(RichText::new("General").strong())
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("running-run-detail-summary")
                .num_columns(2)
                .min_col_width(200.0)
                .spacing([18.0, 5.0])
                .show(ui, |ui| {
                    detail_label(ui, &palette, "Run");
                    ui.label(RichText::new(run.id.to_string()).monospace());
                    ui.end_row();
                    detail_label(ui, &palette, "Task");
                    ui.label(&run.task_id);
                    ui.end_row();
                    detail_label(ui, &palette, "Trigger type");
                    ui.label(run.trigger.kind.label());
                    ui.end_row();
                    detail_label(ui, &palette, "Trigger ID");
                    ui.label(RichText::new(&run.trigger.id).monospace());
                    ui.end_row();
                    detail_label(ui, &palette, "Started");
                    ui.label(model::format_datetime(run.started_at));
                    ui.end_row();
                    detail_label(ui, &palette, "Elapsed");
                    ui.label(model::format_duration(now - run.started_at));
                    ui.end_row();
                    detail_label(ui, &palette, "State");
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(if delayed { "Delayed" } else { "Running" });
                    });
                    ui.end_row();
                    if let Some(delayed_until) = run.delayed_until {
                        detail_label(ui, &palette, "Delayed until");
                        ui.label(model::format_datetime(delayed_until));
                        ui.end_row();
                        detail_label(ui, &palette, "Start delay");
                        ui.label(model::format_duration(delayed_until - run.started_at));
                        ui.end_row();
                    }
                });
        });
    ui.add_space(6.0);
    ui.label(
        RichText::new("Actions and output become available after the run finishes.")
            .small()
            .color(palette.muted),
    );
}

fn trigger_detail_ui(ui: &mut Ui, trigger: &RunTrigger, app: &App) {
    let palette = app.palette();
    let message = match trigger {
        RunTrigger::Message(trigger) => Some(Message {
            id: trigger.message_id,
            time: trigger.time,
            service: trigger.service.clone(),
            state: trigger.state,
            payload: trigger
                .payload
                .clone()
                .unwrap_or_else(|| Base64Bytes::from(Vec::new())),
        }),
        _ => None,
    };

    egui::Grid::new("run-trigger-summary")
        .num_columns(2)
        .min_col_width(200.0)
        .spacing([18.0, 5.0])
        .show(ui, |ui| {
            detail_label(ui, &palette, "Type");
            ui.label(trigger.kind().label());
            ui.end_row();
            detail_label(ui, &palette, "ID");
            ui.label(RichText::new(trigger.id()).monospace());
            ui.end_row();
            match trigger {
                RunTrigger::Message(trigger) => {
                    detail_label(ui, &palette, "Message ID");
                    ui.label(RichText::new(trigger.message_id.to_string()).monospace());
                    ui.end_row();
                    detail_label(ui, &palette, "Time");
                    ui.label(model::format_datetime(trigger.time));
                    ui.end_row();
                    detail_label(ui, &palette, "Service (display)");
                    ui.label(model::display_service(&app.services, &trigger.service));
                    ui.end_row();
                    detail_label(ui, &palette, "Service (composite)");
                    ui.label(model::composite_key(&trigger.service));
                    ui.end_row();
                    detail_label(ui, &palette, "State");
                    ui.label(model::message_state_label(trigger.state));
                    ui.end_row();
                    detail_label(ui, &palette, "Protocol");
                    ui.label(&trigger.protocol);
                    ui.end_row();
                    detail_label(ui, &palette, "Transport");
                    ui.label(&trigger.transport);
                    ui.end_row();
                    detail_label(ui, &palette, "Entry");
                    ui.label(RichText::new(&trigger.entry).monospace());
                    ui.end_row();
                    if let Some(message) = &message {
                        detail_label(ui, &palette, "Payload size");
                        ui.label(format!("{} bytes", message.payload.len()));
                        ui.end_row();
                        detail_label(ui, &palette, "Decoded");
                        ui.label(model::message_summary(&app.services, message));
                        ui.end_row();
                        if !message.payload.as_slice().is_empty() {
                            detail_label(ui, &palette, "Payload");
                            ui.label(
                                RichText::new(model::message_preview(message, 64)).monospace(),
                            )
                            .on_hover_text(
                                hodeco_codec::hex::spaced_hex(message.payload.as_slice()),
                            );
                            ui.end_row();
                        }
                    }
                }
                RunTrigger::Schedule(trigger) => {
                    detail_label(ui, &palette, "Schedule");
                    ui.label(RichText::new(&trigger.schedule_id).monospace());
                    ui.end_row();
                    detail_label(ui, &palette, "Occurrence");
                    ui.label(model::format_datetime(trigger.occurrence_at));
                    ui.end_row();
                }
                RunTrigger::Http(trigger) => {
                    detail_label(ui, &palette, "Method");
                    ui.label(trigger.method.as_str());
                    ui.end_row();
                    detail_label(ui, &palette, "Path");
                    ui.label(RichText::new(&trigger.path).monospace());
                    ui.end_row();
                    if let Some(query) = &trigger.query {
                        detail_label(ui, &palette, "Query");
                        ui.label(RichText::new(query).monospace());
                        ui.end_row();
                    }
                    if let Some(remote) = &trigger.remote {
                        detail_label(ui, &palette, "Remote");
                        ui.label(remote);
                        ui.end_row();
                    }
                    if !trigger.headers.is_empty() {
                        detail_label(ui, &palette, "Headers");
                        ui.label(trigger.headers.len().to_string());
                        ui.end_row();
                    }
                    if let Some(payload) = &trigger.payload {
                        detail_label(ui, &palette, "Payload");
                        ui.label(
                            RichText::new(model::hex_preview(payload.as_slice(), 64)).monospace(),
                        )
                        .on_hover_text(hodeco_codec::hex::spaced_hex(payload.as_slice()));
                        ui.end_row();
                    }
                }
                RunTrigger::Manual(trigger) => {
                    detail_label(ui, &palette, "Source");
                    ui.label("Manual or API");
                    ui.end_row();
                    if let Some(payload) = &trigger.payload {
                        detail_label(ui, &palette, "Payload");
                        ui.label(
                            RichText::new(model::hex_preview(payload.as_slice(), 64)).monospace(),
                        )
                        .on_hover_text(hodeco_codec::hex::spaced_hex(payload.as_slice()));
                        ui.end_row();
                    }
                    if !trigger.metadata.is_empty() {
                        detail_label(ui, &palette, "Metadata");
                        ui.label(RichText::new(model::pretty_json(&trigger.metadata)).monospace());
                        ui.end_row();
                    }
                }
                RunTrigger::Task(trigger) => {
                    detail_label(ui, &palette, "Rule");
                    ui.label(trigger.rule.label());
                    ui.end_row();
                    if let Some(payload) = &trigger.payload {
                        detail_label(ui, &palette, "Payload");
                        ui.label(
                            RichText::new(model::hex_preview(payload.as_slice(), 64)).monospace(),
                        )
                        .on_hover_text(hodeco_codec::hex::spaced_hex(payload.as_slice()));
                        ui.end_row();
                    }
                }
            }
        });

    if let Some(message) = &message
        && !message.payload.as_slice().is_empty()
        && model::message_has_parser(&app.services, message)
    {
        egui::CollapsingHeader::new(RichText::new("Parsed message").strong())
            .default_open(false)
            .show(ui, |ui| {
                let _ = super::messages::render_message(
                    ui,
                    app.theme,
                    &app.services,
                    message,
                    MessageView::Parsed,
                    model::TextEncoding::Ascii,
                );
            });
    }

    ui.collapsing("Raw trigger JSON", |ui| {
        let mut text = model::pretty_json(trigger);
        ui.add(
            egui::TextEdit::multiline(&mut text)
                .font(egui::TextStyle::Monospace)
                .text_color(ui.visuals().text_color())
                .desired_rows(10)
                .desired_width(ui.available_width())
                .interactive(false),
        );
    });
}

/// Muted label matching the Messages detail pane. Section headers keep their
/// stronger `CollapsingHeader` styling so labels and sections stay distinct.
fn detail_label(ui: &mut Ui, palette: &crate::theme::Palette, text: &str) {
    ui.label(RichText::new(text).size(13.5).color(palette.muted));
}

pub(crate) fn detail_ui(ui: &mut Ui, run: &TaskRun, app: &App) {
    let palette = app.palette();
    egui::CollapsingHeader::new(RichText::new("General").strong())
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("run-detail-summary")
                .num_columns(2)
                .min_col_width(200.0)
                .spacing([18.0, 5.0])
                .show(ui, |ui| {
                    detail_label(ui, &palette, "Run");
                    ui.label(RichText::new(run.summary.id.to_string()).monospace());
                    ui.end_row();
                    detail_label(ui, &palette, "Task");
                    ui.label(&run.summary.task_id);
                    ui.end_row();
                    detail_label(ui, &palette, "Outcome");
                    let color = match run.summary.outcome {
                        RunOutcome::Succeeded => palette.green,
                        RunOutcome::Failed => palette.red,
                        RunOutcome::Cancelled => palette.amber,
                    };
                    ui.label(RichText::new(run.summary.outcome.label()).color(color));
                    ui.end_row();
                    detail_label(ui, &palette, "Started");
                    ui.label(model::format_datetime(run.summary.started_at));
                    ui.end_row();
                    if let Some(delayed_until) = run.summary.delayed_until {
                        detail_label(ui, &palette, "Delayed until");
                        ui.label(model::format_datetime(delayed_until));
                        ui.end_row();
                        detail_label(ui, &palette, "Start delay");
                        ui.label(model::format_duration(
                            delayed_until - run.summary.started_at,
                        ));
                        ui.end_row();
                    }
                    detail_label(ui, &palette, "Finished");
                    ui.label(model::format_datetime(run.summary.finished_at));
                    ui.end_row();
                    detail_label(ui, &palette, "Chain");
                    ui.label(format!(
                        "{} · depth {}{}",
                        run.chain.chain_id,
                        run.chain.depth,
                        run.chain
                            .parent_run_id
                            .map(|parent| format!(" · parent {parent}"))
                            .unwrap_or_default()
                    ));
                    ui.end_row();
                });
        });

    egui::CollapsingHeader::new(RichText::new("Trigger").strong())
        .default_open(true)
        .show(ui, |ui| {
            trigger_detail_ui(ui, &run.trigger, app);
        });

    egui::CollapsingHeader::new(RichText::new("Actions").strong())
        .default_open(false)
        .show(ui, |ui| {
            if run.actions.is_empty() {
                ui.label(RichText::new("No actions executed.").color(palette.muted));
            }
            for action in &run.actions {
                let color = if action.succeeded() {
                    palette.green
                } else {
                    palette.red
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&action.id).strong());
                    ui.label(
                        RichText::new(match action.status {
                            hodeco_contract::domain::ActionOutcome::Succeeded => "succeeded",
                            hodeco_contract::domain::ActionOutcome::Failed => "failed",
                        })
                        .color(color),
                    );
                    if let Some(command) = &action.command {
                        ui.label(
                            RichText::new(format!(
                                "{} {:?} exit {:?}",
                                command.command_id, command.args, command.exit_code
                            ))
                            .small(),
                        );
                    }
                    if let Some(message) = &action.message {
                        ui.label(
                            RichText::new(format!("message {:?}", message.message_id)).small(),
                        );
                    }
                });
                if let Some(error) = &action.error {
                    ui.label(RichText::new(error).color(palette.red));
                }
                if let Some(command) = &action.command {
                    if command.stdout.is_none() {
                        ui.label(
                            RichText::new("Output: not captured")
                                .small()
                                .color(palette.muted),
                        );
                    }
                    if let Some(stdout) = &command.stdout
                        && !stdout.as_slice().is_empty()
                    {
                        ui.label(
                            RichText::new(output_text("stdout", stdout))
                                .monospace()
                                .small(),
                        );
                    }
                    if command.output_truncated {
                        ui.label(
                            RichText::new("Output was truncated at the 8 KiB run limit.")
                                .small()
                                .color(palette.amber),
                        );
                    }
                }
            }
        });

    if let Some(selector) = &run.selector {
        egui::CollapsingHeader::new(RichText::new("Selector").strong())
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} · {:?} · exit {:?}{}",
                        selector.command_id,
                        selector.outcome,
                        selector.exit_code,
                        selector
                            .error
                            .as_ref()
                            .map(|error| format!(" · {error}"))
                            .unwrap_or_default()
                    ))
                    .small(),
                );
            });
    }
    if !run.next_tasks.is_empty() {
        egui::CollapsingHeader::new(RichText::new("Next tasks").strong())
            .default_open(false)
            .show(ui, |ui| {
                for next in &run.next_tasks {
                    ui.label(
                        RichText::new(format!(
                            "{} · run {} · {}",
                            next.task_id,
                            next.run_id,
                            next.rule.label()
                        ))
                        .small(),
                    );
                }
            });
    }
    if let Some(error) = &run.error {
        egui::CollapsingHeader::new(RichText::new("Error").strong())
            .default_open(true)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!("{:?}: {}", error.stage, error.message))
                        .color(palette.red),
                );
            });
    }
    egui::CollapsingHeader::new(RichText::new("Raw JSON").strong())
        .default_open(false)
        .show(ui, |ui| {
            let mut text = model::pretty_json(run);
            ui.add(
                egui::TextEdit::multiline(&mut text)
                    .font(egui::TextStyle::Monospace)
                    .text_color(ui.visuals().text_color())
                    .desired_rows(12)
                    .desired_width(ui.available_width())
                    .interactive(false)
                    .code_editor(),
            );
        });
}

fn output_text(label: &str, bytes: &hodeco_contract::utils::Base64Bytes) -> String {
    let raw = bytes.as_slice();
    match std::str::from_utf8(raw) {
        Ok(text)
            if !text
                .chars()
                .any(|character| character.is_control() && !character.is_whitespace()) =>
        {
            format!("{label}: {text}")
        }
        _ => format!("{label} (hex): {}", hodeco_codec::hex::spaced_hex(raw)),
    }
}

/// Builds a `tasks.runFeed` page request.
pub(crate) fn run_feed_params(
    app: &App,
    kind: crate::client::RunPageKind,
) -> Result<hodeco_contract::api::payloads::TasksRunFeedParams, String> {
    use hodeco_contract::domain::MessageOrder;

    let period_from = (app.runs.period_hours > 0)
        .then(|| Utc::now() - Duration::hours(app.runs.period_hours as i64));
    let (from, until, order) = match kind {
        crate::client::RunPageKind::Refresh => (period_from, None, MessageOrder::NewestFirst),
        crate::client::RunPageKind::Older => {
            let bounds = app
                .runs
                .older_next
                .as_ref()
                .ok_or_else(|| "no older run page is available".to_string())?;
            (
                bounds.from.or(period_from),
                bounds.until,
                MessageOrder::NewestFirst,
            )
        }
        crate::client::RunPageKind::Newer => {
            let bounds = app
                .runs
                .newer_next
                .as_ref()
                .ok_or_else(|| "no newer run page is available".to_string())?;
            (
                bounds.from.or(period_from),
                bounds.until,
                MessageOrder::OldestFirst,
            )
        }
    };
    Ok(hodeco_contract::api::payloads::TasksRunFeedParams {
        task_ids: app.runs.filter_task.iter().cloned().collect(),
        types: app.runs.filter_type.into_iter().collect(),
        outcomes: match app.runs.filter_outcome {
            Some(RunOutcomeFilter::Outcome(outcome)) => vec![outcome],
            _ => Vec::new(),
        },
        from,
        until,
        order,
        limit: Some(RUN_PAGE_LIMIT),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn run(id: uuid::Uuid, time: i64) -> RunSummary {
        RunSummary {
            id,
            task_id: "task".to_string(),
            time: Utc.timestamp_opt(time, 0).unwrap(),
            started_at: Utc.timestamp_opt(time, 0).unwrap(),
            delayed_until: None,
            finished_at: Utc.timestamp_opt(time + 1, 0).unwrap(),
            trigger: TriggerRef {
                id: "manual".to_string(),
                kind: TriggerType::Manual,
            },
            outcome: RunOutcome::Succeeded,
            action_count: 0,
            next_count: 0,
            error: None,
        }
    }

    #[test]
    fn running_outcome_filter_matches_only_active_rows() {
        let mut ui = RunsUi {
            filter_outcome: Some(RunOutcomeFilter::Running),
            ..RunsUi::default()
        };
        let summary = run(uuid::Uuid::new_v4(), 10);
        assert!(!ui.filter_matches(&summary));
        let running = RunningRun {
            id: uuid::Uuid::new_v4(),
            task_id: "task".to_string(),
            trigger: TriggerRef {
                id: "manual".to_string(),
                kind: TriggerType::Manual,
            },
            started_at: Utc.timestamp_opt(10, 0).unwrap(),
            delayed_until: None,
        };
        assert!(ui.running_matches(&running));

        ui.filter_outcome = Some(RunOutcomeFilter::Outcome(RunOutcome::Failed));
        assert!(!ui.filter_matches(&summary));
        assert!(!ui.running_matches(&running));
    }

    #[test]
    fn sorts_oldest_first_and_drops_duplicate_ids() {
        let id = uuid::Uuid::new_v4();
        let runs = sorted_runs(vec![
            run(id, 3),
            run(uuid::Uuid::new_v4(), 1),
            run(id, 3),
            run(uuid::Uuid::new_v4(), 2),
        ]);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].time.timestamp(), 1);
        assert_eq!(runs[2].time.timestamp(), 3);
    }
}
