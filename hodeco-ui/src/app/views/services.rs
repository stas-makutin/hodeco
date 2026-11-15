//! Services tab: service list and per-service actions.

use super::super::{App, Dialog};
use crate::ui::widgets;
use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let live = app.gateway.phase().is_live();
    let services = app.services.clone();

    widgets::page_header(
        ui,
        theme,
        "Services",
        Some(&format!("{} configured", services.len())),
        |ui| {
            if ui.add_enabled(live, egui::Button::new("Refresh")).clicked() {
                app.refresh_services();
            }
            if ui
                .add_enabled(ready, egui::Button::new("Send message"))
                .clicked()
            {
                app.open_builder(None);
            }
            if ui
                .add_enabled(ready, egui::Button::new("Add service"))
                .clicked()
            {
                let protocol = app
                    .protocols
                    .first()
                    .map(|protocol| protocol.id.clone())
                    .unwrap_or_default();
                let transport = app
                    .transports
                    .first()
                    .map(|transport| transport.id.clone())
                    .unwrap_or_default();
                app.open_add_service(protocol, transport, "");
            }
        },
    );

    if services.is_empty() {
        if !live {
            widgets::loading_state(ui, theme, "Loading services…");
        } else {
            widgets::empty_state(
                ui,
                theme,
                "No services configured",
                "Add a service manually or run discovery to find a port.",
            );
        }
        return;
    }

    let nav = if app.dialog.is_none() {
        widgets::list_nav_global(ui)
    } else {
        None
    };
    let list_focus = widgets::list_focus(ui, theme, "services-list", nav.is_some(), false);
    let focus_row_actions = widgets::list_tab_pressed(ui, list_focus.id);
    let selection_id = ui.scope_id().with("services-selected");
    let mut selected = ui.data(|data| data.get_temp::<usize>(selection_id));
    if nav.is_some() && !list_focus.focused {
        ui.ctx()
            .memory_mut(|memory| memory.request_focus(list_focus.id));
    }
    let mut activate = None;
    if let Some(nav) = nav {
        let last = services.len().saturating_sub(1);
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
            && let Some(service) = services.get(next)
        {
            activate = Some(service.clone());
        }
    }
    if let Some(service) = activate {
        app.open_service_status(&service);
    }
    let action_row = selected.unwrap_or(0);
    if focus_row_actions {
        selected = Some(action_row);
    }
    ui.data_mut(|data| {
        if let Some(selected) = selected {
            data.insert_temp(selection_id, selected);
        }
    });
    let scroll_to = nav
        .map(|_| selected.unwrap_or(0))
        .or_else(|| focus_row_actions.then_some(action_row));
    let mut action_focus_id = None;
    let mut clicked_selection = None;
    let ctx = ui.ctx().clone();

    egui::ScrollArea::horizontal()
        .id_salt("services-horizontal")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.set_min_width(950.0);
            let mut table = TableBuilder::new(ui)
                .id_salt("services-table")
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::CLICK)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(120.0).at_least(80.0))
                .column(Column::initial(95.0).at_least(70.0))
                .column(Column::initial(90.0).at_least(70.0))
                .column(Column::initial(110.0).at_least(80.0))
                .column(Column::initial(150.0).at_least(110.0))
                .column(Column::remainder().at_least(330.0));
            if let Some(index) = scroll_to {
                table = table.scroll_to_row(index, Some(egui::Align::Center));
            }
            table
                .header(28.0, |mut header| {
                    for title in [
                        "Alias",
                        "Protocol",
                        "Transport",
                        "Entry",
                        "State",
                        "Actions",
                    ] {
                        header.col(|ui| {
                            ui.label(RichText::new(title).strong());
                        });
                    }
                })
                .body(|body| {
                    body.rows(32.0, services.len(), |mut row| {
                        let index = row.index();
                        let service = &services[index];
                        row.set_selected(selected == Some(index));
                        row.col(|ui| match &service.alias {
                            Some(alias) => {
                                ui.label(RichText::new(alias).strong());
                            }
                            None => {
                                ui.label(RichText::new("—").color(palette.muted));
                            }
                        });
                        row.col(|ui| {
                            ui.label(&service.protocol);
                        });
                        row.col(|ui| {
                            ui.label(&service.transport);
                        });
                        row.col(|ui| {
                            ui.label(&service.entry);
                        });
                        row.col(|ui| {
                            let color =
                                crate::theme::service_state_color(theme, service.status.state);
                            let response = ui.label(
                                RichText::new(super::super::service_status_label(
                                    service.status.state,
                                ))
                                .color(color),
                            );
                            if let Some(error) = &service.status.error {
                                response.on_hover_text(error);
                            }
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                let status =
                                    ui.add_enabled(live, egui::Button::new("Status").small());
                                if index == action_row {
                                    action_focus_id = Some(status.id);
                                }
                                if status.clicked() {
                                    app.open_service_status(service);
                                }
                                if ui
                                    .add_enabled(live, egui::Button::new("Params").small())
                                    .clicked()
                                {
                                    app.open_service_params(service);
                                }
                                if ui
                                    .add_enabled(ready, egui::Button::new("Alias").small())
                                    .clicked()
                                {
                                    app.open_edit_alias(service);
                                }
                                if ui
                                    .add_enabled(ready, egui::Button::new("Send").small())
                                    .clicked()
                                {
                                    app.open_builder(Some(crate::model::service_ref(service)));
                                }
                                if ui
                                    .add_enabled(ready, egui::Button::new("Remove").small())
                                    .clicked()
                                {
                                    app.dialog = Some(Dialog::ConfirmRemove(service.clone()));
                                }
                            });
                        });
                        if row.response().clicked() {
                            clicked_selection = Some(index);
                            ctx.memory_mut(|memory| memory.request_focus(list_focus.id));
                        }
                    });
                });
        });
    if let Some(index) = clicked_selection {
        ui.data_mut(|data| data.insert_temp(selection_id, index));
    }
    if focus_row_actions && let Some(id) = action_focus_id {
        ui.memory_mut(|memory| memory.request_focus(id));
    }
}
