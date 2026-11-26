//! Discovery tab: choose a protocol/transport pair, run discovery, and add a
//! discovered entry as a service.

use super::super::App;
use crate::ui::widgets::{self, Tone};
use egui::{ComboBox, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use hodeco_contract::domain::{DiscoveryState, TransportInfo};

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let running = app
        .discovery
        .current
        .as_ref()
        .is_some_and(|discovery| discovery.state == DiscoveryState::Running);
    let protocols = app.protocols.clone();
    let transports = app.transports.clone();

    widgets::page_header(
        ui,
        theme,
        "Discovery",
        Some("Find transport entries and add them as services"),
        |ui| {
            if ui
                .add_enabled(
                    ready && !running,
                    egui::Button::new(if app.discovery.starting {
                        "Starting…"
                    } else {
                        "Start discovery"
                    }),
                )
                .clicked()
            {
                app.start_discovery();
            }
            if ui
                .add_enabled(running, egui::Button::new("Cancel"))
                .clicked()
            {
                app.cancel_discovery();
            }
        },
    );

    ui.horizontal(|ui| {
        ui.label("Protocol");
        let selected_protocol = if app.discovery.protocol.is_empty() {
            "Select protocol".to_string()
        } else {
            protocols
                .iter()
                .find(|protocol| protocol.id == app.discovery.protocol)
                .map(|protocol| protocol.name.clone())
                .unwrap_or_else(|| app.discovery.protocol.clone())
        };
        ComboBox::from_id_salt("discovery-protocol")
            .selected_text(selected_protocol)
            .show_ui(ui, |ui| {
                for protocol in &protocols {
                    ui.selectable_value(
                        &mut app.discovery.protocol,
                        protocol.id.clone(),
                        protocol.name.clone(),
                    );
                }
            });
        let supported: Vec<&TransportInfo> =
            crate::model::supported_transports(&protocols, &app.discovery.protocol, &transports);
        if !supported
            .iter()
            .any(|transport| transport.id == app.discovery.transport)
        {
            app.discovery.transport = supported
                .first()
                .map(|transport| transport.id.clone())
                .unwrap_or_default();
        }
        ui.add_space(12.0);
        ui.label("Transport");
        let selected_transport = if app.discovery.transport.is_empty() {
            "No compatible transport".to_string()
        } else {
            supported
                .iter()
                .find(|transport| transport.id == app.discovery.transport)
                .map(|transport| transport.name.clone())
                .unwrap_or_else(|| app.discovery.transport.clone())
        };
        ComboBox::from_id_salt("discovery-transport")
            .selected_text(selected_transport)
            .show_ui(ui, |ui| {
                for transport in &supported {
                    ui.selectable_value(
                        &mut app.discovery.transport,
                        transport.id.clone(),
                        transport.name.clone(),
                    );
                }
            });
    });
    let key = (
        app.discovery.protocol.clone(),
        app.discovery.transport.clone(),
    );
    if app
        .discovery
        .editor_key
        .as_ref()
        .is_some_and(|existing| existing != &key)
    {
        app.discovery.editor = None;
        app.discovery.editor_key = None;
    }
    if !key.0.is_empty() && !key.1.is_empty() {
        app.ensure_protocol_params(&key.0, &key.1);
    }
    let definitions = app.protocol_params.get(&key).cloned();
    let definition_error = app.protocol_params_errors.get(&key).cloned();
    if let Some(definitions) = &definitions
        && app.discovery.editor_key.as_ref() != Some(&key)
    {
        app.discovery.editor = Some(crate::ui::params::ParamEditor::new_overrides(
            definitions,
            &hodeco_contract::domain::ParamValues::new(),
        ));
        app.discovery.editor_key = Some(key.clone());
    }

    if !key.0.is_empty() && !key.1.is_empty() {
        ui.add_space(6.0);
        egui::CollapsingHeader::new("Probe parameters")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(
                        "Optional serial settings used for the discovery probe. Values equal to the defaults are omitted.",
                    )
                    .size(13.5)
                    .color(palette.muted),
                );
                if let Some(definitions) = &definitions {
                    if definitions.is_empty() {
                        ui.label(
                            RichText::new("No parameters are exposed for this pair.")
                                .size(13.5)
                                .color(palette.muted),
                        );
                    } else if let Some(editor) = &mut app.discovery.editor {
                        editor.ui(ui, theme);
                    }
                } else if let Some(error) = &definition_error {
                    ui.label(RichText::new(error).size(13.5).color(palette.red));
                } else {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(
                            RichText::new("Loading parameter definitions…")
                                .size(13.5)
                                .color(palette.muted),
                        );
                    });
                }
            });
    }

    if app.protocols.is_empty() || app.transports.is_empty() {
        ui.label(
            RichText::new("Catalogs are still loading; discovery is unavailable.")
                .size(13.5)
                .color(palette.muted),
        );
    } else if !ready {
        ui.label(
            RichText::new("Start is disabled until the session is synced.")
                .size(13.5)
                .color(palette.amber),
        );
    }

    if let Some(error) = &app.discovery.error {
        ui.add_space(6.0);
        widgets::banner(
            ui,
            theme,
            Tone::Error,
            "Discovery error",
            Some(error),
            |_ui| {},
        );
    }

    ui.add_space(8.0);
    let viewing_earlier = app.discovery.viewing.is_some();
    let current_running = app
        .discovery
        .current
        .as_ref()
        .is_some_and(|current| current.state == DiscoveryState::Running);
    let shown = app
        .discovery
        .viewing
        .and_then(|id| {
            app.discovery
                .history
                .iter()
                .find(|entry| entry.info.id == id)
                .map(|entry| entry.info.clone())
        })
        .or_else(|| app.discovery.current.clone());
    match shown {
        Some(discovery) => {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} / {}", discovery.protocol, discovery.transport))
                        .strong(),
                );
                let (tone, label) = match discovery.state {
                    DiscoveryState::Running => (Tone::Info, "Running"),
                    DiscoveryState::Finished => (Tone::Success, "Finished"),
                    DiscoveryState::Cancelled => (Tone::Neutral, "Cancelled"),
                    DiscoveryState::Failed => (Tone::Error, "Failed"),
                };
                widgets::pill(ui, theme, label, tone);
                ui.label(
                    RichText::new(format!("{} entries", discovery.entries.len()))
                        .size(13.5)
                        .color(palette.muted),
                );
                if viewing_earlier {
                    ui.label(
                        RichText::new("earlier scan")
                            .size(14.0)
                            .color(palette.muted),
                    );
                    if ui.button("Show current").clicked() {
                        app.discovery.viewing = None;
                    }
                    if current_running {
                        widgets::pill(ui, theme, "current scan still running", Tone::Info);
                    }
                }
            });
            ui.add_space(4.0);

            if discovery.entries.is_empty() {
                widgets::empty_state(
                    ui,
                    theme,
                    if discovery.state == DiscoveryState::Running {
                        "Scanning…"
                    } else {
                        "No entries found"
                    },
                    "Discovery results appear here as the scan progresses.",
                );
            } else {
                let nav = if app.dialog.is_none() {
                    widgets::list_nav_global(ui)
                } else {
                    None
                };
                let list_focus =
                    widgets::list_focus(ui, theme, "discovery-list", nav.is_some(), false);
                let focus_row_actions = widgets::list_tab_pressed(ui, list_focus.id);
                let selection_id = ui.scope_id().with("discovery-selected");
                let mut selected = ui.data(|data| data.get_temp::<usize>(selection_id));
                if nav.is_some() && !list_focus.focused {
                    ui.ctx()
                        .memory_mut(|memory| memory.request_focus(list_focus.id));
                }
                let mut activate = None;
                if let Some(nav) = nav {
                    let next = nav.apply(selected, discovery.entries.len());
                    selected = Some(next);
                    if nav == widgets::ListNav::Activate {
                        activate = discovery.entries.get(next).cloned();
                    }
                }
                if let Some(entry) = activate
                    && entry.available
                {
                    app.open_add_service(
                        discovery.protocol.clone(),
                        discovery.transport.clone(),
                        entry.entry,
                    );
                }
                ui.data_mut(|data| {
                    if let Some(selected) = selected {
                        data.insert_temp(selection_id, selected);
                    }
                });
                let action_row = selected.unwrap_or(0);
                if focus_row_actions {
                    selected = Some(action_row);
                }
                let scroll_to = nav
                    .map(|_| selected.unwrap_or(0))
                    .or_else(|| focus_row_actions.then_some(action_row));
                let mut action_focus_id = None;
                let mut clicked_selection = None;
                let ctx = ui.ctx().clone();
                egui::ScrollArea::horizontal()
                    .id_salt("discovery-horizontal")
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.set_min_width(600.0);
                        let mut table = TableBuilder::new(ui)
                            .id_salt("discovery-table-v2")
                            .striped(true)
                            .resizable(true)
                            .sense(egui::Sense::CLICK)
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::initial(120.0).at_least(90.0))
                            .column(Column::initial(100.0).at_least(84.0))
                            .column(Column::exact(126.0))
                            .column(Column::remainder().at_least(140.0).clip(true));
                        if let Some(index) = scroll_to {
                            table = table.scroll_to_row(index, Some(egui::Align::Center));
                        }
                        table
                            .header(28.0, |mut header| {
                                for title in ["Entry", "Status", "Actions", "Description"] {
                                    header.col(|ui| {
                                        ui.label(RichText::new(title).strong());
                                    });
                                }
                            })
                            .body(|body| {
                                body.rows(32.0, discovery.entries.len(), |mut row| {
                                    let index = row.index();
                                    let entry = &discovery.entries[index];
                                    row.set_selected(selected == Some(index));
                                    row.col(|ui| {
                                        ui.label(RichText::new(&entry.entry).monospace());
                                    });
                                    row.col(|ui| {
                                        if entry.available {
                                            ui.label(
                                                RichText::new("Available").color(palette.green),
                                            );
                                        } else {
                                            let response = ui.label(
                                                RichText::new("Unavailable").color(palette.muted),
                                            );
                                            if let Some(error) = &entry.error {
                                                response.on_hover_text(error);
                                            }
                                        }
                                    });
                                    row.col(|ui| {
                                        let enabled = ready && entry.available;
                                        let response = ui.add_enabled(
                                            enabled,
                                            egui::Button::new("Add service").small(),
                                        );
                                        let response = if enabled {
                                            response
                                        } else {
                                            response.on_disabled_hover_text(if ready {
                                                entry.error.as_deref().unwrap_or(
                                                    "Only available entries can be added.",
                                                )
                                            } else {
                                                "The gateway must be synced before adding a service."
                                            })
                                        };
                                        if index == action_row {
                                            action_focus_id = Some(response.id);
                                        }
                                        if response.clicked() {
                                            app.open_add_service(
                                                discovery.protocol.clone(),
                                                discovery.transport.clone(),
                                                entry.entry.clone(),
                                            );
                                        }
                                    });
                                    row.col(|ui| {
                                        ui.label(entry.description.as_deref().unwrap_or("—"));
                                    });
                                    if row.response().clicked() {
                                        clicked_selection = Some(index);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(list_focus.id)
                                        });
                                    }
                                });
                            });
                    });
                if let Some(index) = clicked_selection {
                    ui.data_mut(|data| data.insert_temp(selection_id, index));
                }
                if focus_row_actions {
                    ui.data_mut(|data| data.insert_temp(selection_id, action_row));
                    if let Some(id) = action_focus_id {
                        ui.memory_mut(|memory| memory.request_focus(id));
                    }
                }
            }
        }
        None => {
            widgets::empty_state(
                ui,
                theme,
                "No discovery yet",
                "Select a protocol and transport, then start discovery.",
            );
        }
    }

    if app.discovery.history.len() > 1 {
        ui.add_space(8.0);
        egui::CollapsingHeader::new(format!("Recent scans ({})", app.discovery.history.len()))
            .default_open(false)
            .show(ui, |ui| {
                let shown_id = app
                    .discovery
                    .viewing
                    .or_else(|| app.discovery.current.as_ref().map(|current| current.id));
                let mut view = None;
                for entry in app.discovery.history.iter().rev() {
                    let discovery = &entry.info;
                    let is_shown = shown_id == Some(discovery.id);
                    let (tone, label) = match discovery.state {
                        DiscoveryState::Running => (Tone::Info, "Running"),
                        DiscoveryState::Finished => (Tone::Success, "Finished"),
                        DiscoveryState::Cancelled => (Tone::Neutral, "Cancelled"),
                        DiscoveryState::Failed => (Tone::Error, "Failed"),
                    };
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(crate::model::format_datetime(entry.seen_at))
                                .monospace()
                                .size(13.5)
                                .color(palette.muted),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} / {}",
                                discovery.protocol, discovery.transport
                            ))
                            .monospace(),
                        );
                        widgets::pill(ui, theme, label, tone);
                        ui.label(
                            RichText::new(format!("{} entries", discovery.entries.len()))
                                .size(13.5)
                                .color(palette.muted),
                        );
                        if is_shown {
                            ui.label(RichText::new("shown").size(14.0).color(palette.green));
                        } else if ui.small_button("View").clicked() {
                            view = Some(discovery.id);
                        }
                    });
                }
                if let Some(id) = view {
                    let current_id = app.discovery.current.as_ref().map(|current| current.id);
                    app.discovery.viewing = (current_id != Some(id)).then_some(id);
                }
            });
    }
}
