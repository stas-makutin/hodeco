//! Full-page per-task persistent run history.

use super::super::App;
use super::runs;
use crate::client::{RequestTag, RunPageKind};
use crate::model;
use crate::ui::widgets;
use chrono::{Duration, Utc};
use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use hodeco_contract::api::Method;
use hodeco_contract::api::payloads::{MessageTimeRange, TasksHistoryParams};
use hodeco_contract::domain::{MessageOrder, RunOutcome, RunSummary, TaskRun, TriggerType};
use serde_json::json;
use uuid::Uuid;

pub(crate) const PAGE_LIMIT: u32 = 50;
pub(crate) const WINDOW_CAP: usize = 1_000;

#[derive(Debug, Clone)]
pub struct TaskHistoryUi {
    pub task_id: String,
    pub task_name: String,
    /// Completed runs in oldest-first display order.
    pub runs: Vec<RunSummary>,
    pub older_next: Option<MessageTimeRange>,
    pub newer_next: Option<MessageTimeRange>,
    pub loading: bool,
    pub loading_older: bool,
    pub loading_newer: bool,
    pub error: Option<String>,
    pub selected: Option<Uuid>,
    pub detail: Option<TaskRun>,
    pub detail_loading: bool,
    pub detail_error: Option<String>,
    pub generation: u64,
    pub at_bottom: bool,
    pub scroll_to_bottom: bool,
    pub scroll_to_selected: bool,
    pub scroll_anchor: Option<Uuid>,
    pub visible_start: u64,
    pub filter_type: Option<TriggerType>,
    pub filter_outcome: Option<RunOutcome>,
    pub period_hours: u32,
}

impl TaskHistoryUi {
    pub fn new(task_id: String, task_name: String) -> Self {
        Self {
            task_id,
            task_name,
            runs: Vec::new(),
            older_next: None,
            newer_next: None,
            loading: false,
            loading_older: false,
            loading_newer: false,
            error: None,
            selected: None,
            detail: None,
            detail_loading: false,
            detail_error: None,
            generation: 0,
            at_bottom: true,
            scroll_to_bottom: true,
            scroll_to_selected: false,
            scroll_anchor: None,
            visible_start: 0,
            filter_type: None,
            filter_outcome: None,
            period_hours: 0,
        }
    }

    fn clear_selection(&mut self) {
        self.selected = None;
        self.detail = None;
        self.detail_loading = false;
        self.detail_error = None;
        self.scroll_to_selected = false;
    }
}

fn page_params(dialog: &TaskHistoryUi, kind: RunPageKind) -> TasksHistoryParams {
    let period_from =
        (dialog.period_hours > 0).then(|| Utc::now() - Duration::hours(dialog.period_hours as i64));
    let (from, until, order) = match kind {
        RunPageKind::Refresh => (period_from, None, MessageOrder::NewestFirst),
        RunPageKind::Older => {
            let bounds = dialog.older_next.as_ref();
            (
                bounds.and_then(|bounds| bounds.from).or(period_from),
                bounds.and_then(|bounds| bounds.until),
                MessageOrder::NewestFirst,
            )
        }
        RunPageKind::Newer => {
            let bounds = dialog.newer_next.as_ref();
            (
                bounds.and_then(|bounds| bounds.from).or(period_from),
                bounds.and_then(|bounds| bounds.until),
                MessageOrder::OldestFirst,
            )
        }
    };
    TasksHistoryParams {
        task_ids: vec![dialog.task_id.clone()],
        types: dialog.filter_type.into_iter().collect(),
        outcomes: dialog.filter_outcome.into_iter().collect(),
        from,
        until,
        order,
        limit: Some(PAGE_LIMIT),
        include_feed: true,
        ..Default::default()
    }
}

pub(crate) fn request_page(app: &mut App, dialog: &mut TaskHistoryUi, kind: RunPageKind) {
    if kind != RunPageKind::Refresh && dialog.loading {
        return;
    }
    if kind == RunPageKind::Older
        && (dialog.loading_older || dialog.loading_newer || dialog.older_next.is_none())
    {
        return;
    }
    if kind == RunPageKind::Newer
        && (dialog.loading_newer || dialog.loading_older || dialog.newer_next.is_none())
    {
        return;
    }
    dialog.generation = dialog.generation.wrapping_add(1);
    let generation = dialog.generation;
    let params = serde_json::to_value(page_params(dialog, kind)).unwrap_or_else(|_| json!({}));
    if app.rpc(
        RequestTag::TaskHistoryPage { generation, kind },
        Method::TasksHistory,
        params,
    ) {
        match kind {
            RunPageKind::Refresh => {
                dialog.loading = true;
                dialog.older_next = None;
                dialog.newer_next = None;
                dialog.scroll_anchor = None;
                dialog.scroll_to_bottom = dialog.selected.is_none();
                dialog.at_bottom = true;
            }
            RunPageKind::Older => dialog.loading_older = true,
            RunPageKind::Newer => dialog.loading_newer = true,
        }
        dialog.error = None;
    } else {
        dialog.loading = false;
        dialog.loading_older = false;
        dialog.loading_newer = false;
        dialog.error = Some("The gateway is not connected.".to_string());
    }
}

pub(crate) fn request_detail(app: &mut App, dialog: &mut TaskHistoryUi, run_id: Uuid) {
    dialog.detail = None;
    dialog.detail_error = None;
    dialog.detail_loading = true;
    if !app.rpc(
        RequestTag::TaskHistoryRunGet {
            task_id: dialog.task_id.clone(),
            run_id,
        },
        Method::TasksGetRun,
        json!({ "taskId": dialog.task_id, "runId": run_id }),
    ) {
        dialog.detail_loading = false;
        dialog.detail_error = Some("The gateway is not connected.".to_string());
    }
}

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    let Some(mut history) = app.tasks.history.take() else {
        return;
    };
    let theme = app.theme;
    let palette = app.palette();
    let mut back = false;
    let mut refresh = false;
    let mut load_older = false;
    let mut load_newer = false;
    let mut select = None;

    if app.dialog.is_none()
        && !ui.ctx().any_popup_open()
        && ui.input(|input| input.key_pressed(egui::Key::Escape))
    {
        if history.selected.is_some() {
            history.clear_selection();
        } else {
            back = true;
        }
    }

    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new("← Tasks"))
            .on_hover_text("Back to tasks")
            .clicked()
        {
            back = true;
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Run history").size(18.0).strong());
        ui.label(RichText::new(&history.task_name).color(palette.muted));
        ui.label(RichText::new(&history.task_id).small().color(palette.muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Close").clicked() {
                back = true;
            }
            ui.add_space(18.0);
            if ui
                .add_enabled(!history.loading, egui::Button::new("Refresh"))
                .clicked()
            {
                refresh = true;
            }
            ui.add_space(12.0);
            ui.label(
                RichText::new(format!("{} loaded", history.runs.len()))
                    .small()
                    .color(palette.muted),
            );
        });
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Trigger");
        egui::ComboBox::from_id_salt("task-history-type-filter")
            .selected_text(
                history
                    .filter_type
                    .map(|kind| kind.label())
                    .unwrap_or("Any"),
            )
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(history.filter_type.is_none(), "Any")
                    .clicked()
                {
                    history.filter_type = None;
                    refresh = true;
                }
                for kind in [
                    TriggerType::Message,
                    TriggerType::Schedule,
                    TriggerType::Http,
                    TriggerType::Manual,
                    TriggerType::Task,
                ] {
                    if ui
                        .selectable_label(history.filter_type == Some(kind), kind.label())
                        .clicked()
                    {
                        history.filter_type = Some(kind);
                        refresh = true;
                    }
                }
            });
        ui.label("Outcome");
        egui::ComboBox::from_id_salt("task-history-outcome-filter")
            .selected_text(
                history
                    .filter_outcome
                    .map(|outcome| outcome.label())
                    .unwrap_or("Any"),
            )
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(history.filter_outcome.is_none(), "Any")
                    .clicked()
                {
                    history.filter_outcome = None;
                    refresh = true;
                }
                for outcome in RunOutcome::ALL {
                    if ui
                        .selectable_label(history.filter_outcome == Some(outcome), outcome.label())
                        .clicked()
                    {
                        history.filter_outcome = Some(outcome);
                        refresh = true;
                    }
                }
            });
        ui.label("Period");
        egui::ComboBox::from_id_salt("task-history-period")
            .selected_text(match history.period_hours {
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
                        .selectable_label(history.period_hours == hours, label)
                        .clicked()
                    {
                        history.period_hours = hours;
                        refresh = true;
                    }
                }
            });
    });
    if let Some(error) = &history.error {
        ui.label(RichText::new(error).color(palette.red));
    }
    if history.loading || history.loading_older || history.loading_newer {
        ui.horizontal(|ui| {
            ui.spinner();
        });
    }

    let mut selected_row = history
        .selected
        .and_then(|id| history.runs.iter().position(|run| run.id == id));
    let total = history.runs.len();
    let nav = if total > 0 && app.dialog.is_none() && !ui.ctx().any_popup_open() {
        widgets::list_nav_global(ui)
    } else {
        None
    };
    if let Some(nav) = nav {
        let last = total.saturating_sub(1);
        let current = selected_row.unwrap_or((history.visible_start as usize).min(last));
        let target = match nav {
            widgets::ListNav::Up => selected_row.map_or(current, |index| index.saturating_sub(1)),
            widgets::ListNav::Down => selected_row.map_or(current, |index| (index + 1).min(last)),
            widgets::ListNav::PageUp => {
                selected_row.map_or(current, |index| index.saturating_sub(10))
            }
            widgets::ListNav::PageDown => {
                selected_row.map_or(current, |index| (index + 10).min(last))
            }
            widgets::ListNav::Home => 0,
            widgets::ListNav::End => last,
            widgets::ListNav::Activate => current,
        };
        select = history.runs.get(target).map(|run| run.id);
        selected_row = Some(target);
        if let Some(id) = select {
            if history.selected != Some(id) {
                history.detail = None;
                history.detail_error = None;
                history.detail_loading = true;
            }
            history.selected = Some(id);
            history.scroll_to_selected = true;
        }
    }

    let list_focus =
        widgets::list_focus_with_tab(ui, theme, "task-history-list", nav.is_some(), false, false);
    if nav.is_some() && !list_focus.focused {
        ui.ctx()
            .memory_mut(|memory| memory.request_focus(list_focus.id));
    }
    if list_focus.focused
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        && let Some(row) = selected_row
    {
        select = history.runs.get(row).map(|run| run.id);
    }

    let full_rect = ui.available_rect_before_wrap();
    let detail_height = if history.selected.is_some() {
        300.0
    } else {
        0.0
    };
    let gap = if detail_height > 0.0 { 8.0 } else { 0.0 };
    let table_height = (full_rect.height() - detail_height - gap).max(120.0);
    let table_rect =
        egui::Rect::from_min_size(full_rect.min, egui::vec2(full_rect.width(), table_height));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(table_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            if total == 0 {
                if history.loading {
                    ui.label(RichText::new("Loading persistent history…").color(palette.muted));
                } else {
                    ui.label(
                        RichText::new(
                            "No run history. Recent completed runs appear here until the gateway restarts; enable history for this task to keep runs.",
                        )
                        .color(palette.muted),
                    );
                }
            } else {
                let mut first_visible = None;
                let mut last_visible = None;
                let scroll_to = if history.scroll_to_selected {
                    selected_row.map(|index| (index, egui::Align::Center))
                } else if let Some(anchor) = history.scroll_anchor.take() {
                    history
                        .runs
                        .iter()
                        .position(|run| run.id == anchor)
                        .map(|index| (index, egui::Align::Center))
                } else if history.scroll_to_bottom {
                    Some((total - 1, egui::Align::BOTTOM))
                } else {
                    None
                };
                history.scroll_to_selected = false;
                history.scroll_to_bottom = false;

                ui.style_mut().interaction.selectable_labels = false;
                let mut table = TableBuilder::new(ui)
                    .id_salt("task-history-table")
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::CLICK)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::initial(150.0).at_least(120.0))
                    .column(Column::initial(160.0).at_least(110.0))
                    .column(Column::initial(100.0).at_least(80.0))
                    .column(Column::initial(90.0).at_least(70.0))
                    .column(Column::initial(80.0).at_least(60.0))
                    .column(Column::initial(80.0).at_least(60.0))
                    .column(Column::remainder().at_least(140.0));
                if let Some((index, align)) = scroll_to {
                    table = table.scroll_to_row(index, Some(align));
                }
                table
                    .max_scroll_height(table_height)
                    .header(26.0, |mut header| {
                        for (title, hover) in [
                            ("Time", "When the run was recorded"),
                            ("Trigger", "Trigger ID and type"),
                            ("Outcome", "Run result"),
                            ("Duration", "Start to finish"),
                            ("Actions run", "Number of actions executed"),
                            ("Next tasks", "Number of next tasks started"),
                            ("Error", "Run error, if any"),
                        ] {
                            header.col(|ui| {
                                ui.label(RichText::new(title).strong()).on_hover_text(hover);
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(28.0, total, |mut row| {
                            let index = row.index();
                            if first_visible.is_none() {
                                first_visible = Some(index);
                            }
                            last_visible = Some(index);
                            let run = &history.runs[index];
                            row.set_selected(history.selected == Some(run.id));
                            row.col(|ui| {
                                ui.label(RichText::new(model::format_datetime(run.time)).small());
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
                                if history.selected == Some(run.id) {
                                    history.clear_selection();
                                    selected_row = None;
                                } else {
                                    history.selected = Some(run.id);
                                    history.scroll_to_selected = true;
                                    selected_row = Some(index);
                                    select = Some(run.id);
                                }
                            }
                        });
                    });

                history.visible_start = first_visible.unwrap_or(0) as u64;
                history.at_bottom =
                    total == 0 || last_visible.is_some_and(|last| last.saturating_add(1) >= total);
                if first_visible == Some(0) && history.older_next.is_some() && !history.loading_older {
                    load_older = true;
                } else if last_visible.is_some_and(|last| last.saturating_add(1) >= total)
                    && history.newer_next.is_some()
                    && !history.loading_newer
                {
                    load_newer = true;
                }
            }
        },
    );
    if history.selected.is_some() {
        let detail_rect = egui::Rect::from_min_max(
            egui::pos2(
                full_rect.min.x,
                (table_rect.bottom() + gap).min(full_rect.bottom()),
            ),
            full_rect.max,
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(detail_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                detail_frame(app, ui, &mut history);
            },
        );
    }

    if let Some(run_id) = select {
        history.selected = Some(run_id);
        request_detail(app, &mut history, run_id);
    }
    if refresh {
        request_page(app, &mut history, RunPageKind::Refresh);
    } else if load_older {
        request_page(app, &mut history, RunPageKind::Older);
    } else if load_newer {
        request_page(app, &mut history, RunPageKind::Newer);
    }

    if back {
        app.tasks.history = None;
    } else {
        app.tasks.history = Some(history);
    }
}

fn detail_frame(app: &App, ui: &mut Ui, history: &mut TaskHistoryUi) {
    let Some(run_id) = history.selected else {
        return;
    };
    let palette = app.palette();
    let mut close = false;
    egui::Frame::new()
        .fill(palette.panel)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(280.0);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Run detail").strong());
                ui.label(
                    RichText::new(run_id.to_string())
                        .small()
                        .color(palette.muted),
                );
                if history.detail_loading {
                    ui.spinner();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    if let Some(run) = &history.detail
                        && ui.button("Copy JSON").clicked()
                    {
                        ui.ctx().copy_text(model::pretty_json(run));
                    }
                });
            });
            if let Some(error) = &history.detail_error {
                ui.label(RichText::new(error).color(palette.red));
            } else if let Some(run) = history.detail.clone() {
                egui::ScrollArea::vertical()
                    .id_salt("task-history-detail")
                    .max_height(235.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        runs::detail_ui(ui, &run, app);
                    });
            }
        });
    if close {
        history.clear_selection();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn paging_bounds_round_trip_like_messages_list() {
        let mut dialog = TaskHistoryUi::new("task".to_string(), "Task".to_string());
        dialog.older_next = Some(MessageTimeRange {
            from: None,
            until: Some(Utc.timestamp_opt(100, 0).unwrap()),
        });
        let params = page_params(&dialog, RunPageKind::Older);
        assert_eq!(params.order, MessageOrder::NewestFirst);
        assert_eq!(params.until, dialog.older_next.as_ref().unwrap().until);

        dialog.newer_next = Some(MessageTimeRange {
            from: Some(Utc.timestamp_opt(101, 0).unwrap()),
            until: None,
        });
        let params = page_params(&dialog, RunPageKind::Newer);
        assert_eq!(params.order, MessageOrder::OldestFirst);
        assert_eq!(params.from, dialog.newer_next.as_ref().unwrap().from);
    }

    #[test]
    fn filters_are_sent_to_api() {
        let mut dialog = TaskHistoryUi::new("task".to_string(), "Task".to_string());
        dialog.filter_type = Some(TriggerType::Schedule);
        dialog.filter_outcome = Some(RunOutcome::Failed);
        dialog.period_hours = 1;

        let params = page_params(&dialog, RunPageKind::Refresh);
        assert_eq!(params.task_ids, vec!["task".to_string()]);
        assert_eq!(params.types, vec![TriggerType::Schedule]);
        assert_eq!(params.outcomes, vec![RunOutcome::Failed]);
        assert!(params.from.is_some());
        assert!(params.until.is_none());
        assert!(params.include_feed);
    }
}
