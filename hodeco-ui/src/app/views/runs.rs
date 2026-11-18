//! Runs list and inline run detail pane.

use super::super::App;
use crate::model;
use crate::ui::widgets;
use chrono::{Duration, Utc};
use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use hodeco_contract::domain::{RunOutcome, RunSummary, TaskRun, TriggerType};

#[derive(Debug)]
pub(crate) struct RunsUi {
    pub runs: Vec<RunSummary>,
    pub loading: bool,
    pub loading_more: bool,
    pub next: Option<hodeco_contract::api::payloads::MessageTimeRange>,
    pub error: Option<String>,
    pub filter_task: Option<String>,
    pub filter_type: Option<TriggerType>,
    pub filter_outcome: Option<RunOutcome>,
    pub period_hours: u32,
    pub selected: Option<(String, uuid::Uuid)>,
    pub selected_index: Option<usize>,
    pub detail: Option<TaskRun>,
    pub detail_loading: bool,
    pub detail_error: Option<String>,
    pub generation: u64,
    pub pending_new: usize,
}

impl RunsUi {
    pub fn filter_matches(&self, run: &RunSummary) -> bool {
        self.filter_task
            .as_ref()
            .is_none_or(|task| task == &run.task_id)
            && self.filter_type.is_none_or(|kind| kind == run.trigger.kind)
            && self
                .filter_outcome
                .is_none_or(|outcome| outcome == run.outcome)
    }
}

impl Default for RunsUi {
    fn default() -> Self {
        Self {
            runs: Vec::new(),
            loading: false,
            loading_more: false,
            next: None,
            error: None,
            filter_task: None,
            filter_type: None,
            filter_outcome: None,
            period_hours: 24,
            selected: None,
            selected_index: None,
            detail: None,
            detail_loading: false,
            detail_error: None,
            generation: 0,
            pending_new: 0,
        }
    }
}

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let live = app.gateway.phase().is_live();
    let task_options: Vec<(String, String)> = app
        .tasks
        .tasks
        .iter()
        .map(|info| (info.task.id.clone(), info.task.name.clone()))
        .collect();

    widgets::page_header(
        ui,
        theme,
        "Runs",
        Some(&format!("{} loaded", app.runs.runs.len())),
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
                for outcome in RunOutcome::ALL {
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
                1 => "Last hour".to_string(),
                24 => "Last 24 hours".to_string(),
                168 => "Last 7 days".to_string(),
                720 => "Last 30 days".to_string(),
                hours => format!("Last {hours} h"),
            })
            .show_ui(ui, |ui| {
                for (hours, label) in [
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
        if app.runs.pending_new > 0
            && ui
                .button(format!("{} new run(s)", app.runs.pending_new))
                .clicked()
        {
            app.runs.pending_new = 0;
            app.refresh_runs();
        }
        if app.runs.next.is_some()
            && ui
                .add_enabled(!app.runs.loading_more, egui::Button::new("Load older runs"))
                .clicked()
        {
            app.load_more_runs();
        }
        if app.runs.loading_more {
            ui.spinner();
        }
        if let Some(error) = &app.runs.error {
            ui.label(RichText::new(error).color(palette.red));
        }
    });
    ui.add_space(4.0);

    let runs = app.runs.runs.clone();
    let mut selected = app.runs.selected_index.filter(|index| *index < runs.len());
    let nav = if app.dialog.is_none() && !runs.is_empty() {
        widgets::list_nav_global(ui)
    } else {
        None
    };
    let list_focus = widgets::list_focus(ui, theme, "runs-list", nav.is_some(), false);
    let mut activate = None;
    if let Some(nav) = nav {
        let last = runs.len().saturating_sub(1);
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
        if nav == widgets::ListNav::Activate
            && let Some(run) = runs.get(next)
        {
            activate = Some((run.task_id.clone(), run.id));
        }
    }
    if list_focus.focused
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        && let Some(run) = runs.get(selected.unwrap_or(0))
    {
        activate = Some((run.task_id.clone(), run.id));
    }
    let mut clicked_selection = None;
    let scroll_to = nav.map(|_| selected.unwrap_or(0));

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
            if let Some(index) = scroll_to {
                table = table.scroll_to_row(index, Some(egui::Align::Center));
            }
            table
                .header(28.0, |mut header| {
                    for title in [
                        "Time", "Task", "Trigger", "Outcome", "Duration", "Actions", "Next",
                        "Error",
                    ] {
                        header.col(|ui| {
                            ui.label(RichText::new(title).strong());
                        });
                    }
                })
                .body(|body| {
                    body.rows(30.0, runs.len(), |mut row| {
                        let index = row.index();
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
                            ui.label(RichText::new(format_duration(duration)).small());
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(run.action_count.to_string()).small());
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
                            clicked_selection = Some(index);
                        }
                    });
                });
        });

    if let Some(index) = clicked_selection {
        selected = Some(index);
        if let Some(run) = runs.get(index) {
            activate = Some((run.task_id.clone(), run.id));
        }
        ui.memory_mut(|memory| memory.request_focus(list_focus.id));
    }
    app.runs.selected_index = selected;
    if let Some((task_id, run_id)) = activate {
        app.select_run(&task_id, run_id);
    }

    if runs.is_empty() && !app.runs.loading {
        widgets::empty_state(
            ui,
            theme,
            "No runs",
            "Runs that match the current task, trigger, and period appear here.",
        );
    }

    if app.runs.selected.is_some() {
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Run detail").strong());
            if let Some((task, id)) = &app.runs.selected {
                ui.label(
                    RichText::new(format!("{task} · {id}"))
                        .small()
                        .color(palette.muted),
                );
            }
            if ui.button("Close").clicked() {
                app.runs.selected = None;
                app.runs.detail = None;
            }
            if let Some(detail) = &app.runs.detail
                && ui.button("Copy JSON").clicked()
                && let Ok(text) = serde_json::to_string_pretty(detail)
            {
                ui.ctx().copy_text(text);
            }
        });
        if app.runs.detail_loading {
            ui.spinner();
        }
        if let Some(error) = &app.runs.detail_error {
            ui.label(RichText::new(error).color(palette.red));
        }
        if let Some(detail) = &app.runs.detail {
            egui::ScrollArea::vertical()
                .id_salt("run-detail-scroll")
                .auto_shrink([false, false])
                .max_height(320.0)
                .show(ui, |ui| {
                    detail_ui(ui, detail, app);
                });
        }
    }
}

fn detail_ui(ui: &mut Ui, run: &TaskRun, app: &App) {
    let palette = app.palette();
    egui::Grid::new("run-detail-summary")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.label("Run");
            ui.label(RichText::new(run.summary.id.to_string()).monospace());
            ui.end_row();
            ui.label("Started");
            ui.label(model::format_datetime(run.summary.started_at));
            ui.end_row();
            ui.label("Finished");
            ui.label(model::format_datetime(run.summary.finished_at));
            ui.end_row();
            ui.label("Chain");
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
    ui.add_space(6.0);
    ui.label(RichText::new("Trigger").strong());
    ui.label(
        RichText::new(
            serde_json::to_string_pretty(&run.trigger).unwrap_or_else(|_| "{}".to_string()),
        )
        .monospace()
        .small(),
    );

    ui.add_space(6.0);
    ui.label(RichText::new("Actions").strong());
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
                ui.label(RichText::new(format!("message {:?}", message.message_id)).small());
            }
        });
        if let Some(error) = &action.error {
            ui.label(RichText::new(error).color(palette.red));
        }
        if let Some(command) = &action.command {
            if let Some(stdout) = &command.stdout
                && !stdout.is_empty()
            {
                ui.label(
                    RichText::new(format!("stdout: {stdout}"))
                        .monospace()
                        .small(),
                );
            }
            if let Some(stderr) = &command.stderr
                && !stderr.is_empty()
            {
                ui.label(
                    RichText::new(format!("stderr: {stderr}"))
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

    if let Some(selector) = &run.selector {
        ui.add_space(6.0);
        ui.label(RichText::new("Selector").strong());
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
    }
    ui.add_space(6.0);
    ui.label(RichText::new("Next tasks").strong());
    if run.next_tasks.is_empty() {
        ui.label(RichText::new("None started.").color(palette.muted));
    }
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
    if let Some(error) = &run.error {
        ui.add_space(6.0);
        ui.label(RichText::new(format!("{:?}: {}", error.stage, error.message)).color(palette.red));
    }
    ui.add_space(6.0);
    ui.collapsing("Raw JSON", |ui| {
        if let Ok(text) = serde_json::to_string_pretty(run) {
            let mut text = text;
            ui.add(
                egui::TextEdit::multiline(&mut text)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(12)
                    .code_editor(),
            );
        }
    });
}

fn format_duration(duration: chrono::TimeDelta) -> String {
    let seconds = duration.num_seconds().max(0);
    if seconds < 1 {
        format!("{} ms", duration.num_milliseconds().max(0))
    } else if seconds < 60 {
        format!("{seconds} s")
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// Builds the history request used by the Runs view.
pub(crate) fn history_params(app: &App) -> hodeco_contract::api::payloads::TasksHistoryParams {
    hodeco_contract::api::payloads::TasksHistoryParams {
        task_ids: app.runs.filter_task.iter().cloned().collect(),
        types: app.runs.filter_type.into_iter().collect(),
        outcomes: app.runs.filter_outcome.into_iter().collect(),
        from: Some(Utc::now() - Duration::hours(app.runs.period_hours as i64)),
        order: hodeco_contract::domain::MessageOrder::NewestFirst,
        limit: Some(50),
        ..Default::default()
    }
}
