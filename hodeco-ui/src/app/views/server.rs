//! Server tab: server identity, uptime, endpoint, and the protocol/transport
//! catalog.

use super::super::{App, Dialog};
use crate::theme::{UiTheme, palette};
use crate::ui::widgets::{self, Tone};
use egui::{RichText, Ui};
use hodeco_contract::api::payloads::ServerInfoResult;
use hodeco_contract::domain::{ParamDef, ProtocolInfo, TransportInfo};

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let live = app.gateway.phase().is_live();
    let server = app.server.clone();
    let protocols = app.protocols.clone();
    let transports = app.transports.clone();
    let diagnostics = server
        .as_ref()
        .map(|server| diagnostics_text(app, server, &protocols, &transports));

    widgets::page_header(
        ui,
        theme,
        "Server",
        Some("Gateway identity, runtime state, and protocol catalog"),
        |ui| {
            if let Some(diagnostics) = &diagnostics {
                widgets::copy_with_feedback(ui, diagnostics, "Copy diagnostics");
            }
            if ui.add_enabled(live, egui::Button::new("Refresh")).clicked() {
                app.refresh_server();
            }
        },
    );

    let Some(server) = server else {
        if live {
            widgets::loading_state(ui, theme, "Loading server information…");
        } else {
            widgets::empty_state(
                ui,
                theme,
                "No server information",
                "Connect to a gateway to read its identity and catalog.",
            );
        }
        return;
    };

    if app.protocol_mismatch {
        widgets::banner(
            ui,
            theme,
            Tone::Error,
            "Protocol version mismatch",
            Some(&format!(
                "This UI expects {}, but the gateway reports {}. Some operations may fail.",
                crate::model::EXPECTED_PROTOCOL_VERSION,
                server.protocol_version
            )),
            |_ui| {},
        );
    }

    egui::Grid::new("server-summary")
        .num_columns(2)
        .spacing([20.0, 5.0])
        .show(ui, |ui| {
            value_row(
                ui,
                theme,
                "Version",
                if server.version == format!("{}-demo", env!("CARGO_PKG_VERSION")) {
                    format!("hodeco {} (demo)", env!("CARGO_PKG_VERSION"))
                } else {
                    format!("hodeco {}", server.version)
                },
            );
            value_row(ui, theme, "Protocol version", &server.protocol_version);
            value_row(
                ui,
                theme,
                "Started",
                format!(
                    "{} local (uptime {})",
                    crate::model::format_datetime(server.started_at),
                    crate::model::format_uptime(server.started_at, app.now)
                ),
            );
            value_row(
                ui,
                theme,
                "Endpoint",
                if app.gateway.is_demo() {
                    "demo fixtures (in-process)".to_string()
                } else {
                    app.gateway.endpoint().to_string()
                },
            );
            ui.label(RichText::new("Status").size(13.5).color(palette.muted));
            let color = crate::theme::phase_color(theme, app.gateway.phase());
            ui.label(
                RichText::new(app.gateway.phase().label())
                    .color(color)
                    .strong(),
            );
            ui.end_row();
            value_row(
                ui,
                theme,
                "Configured services",
                app.services.len().to_string(),
            );
            value_row(ui, theme, "Messages", app.messages.store_total.to_string());
        });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let restart = ui.button("Restart server…").on_hover_text(
                "Stop the gateway process; an external supervisor must start it again.",
            );
            if restart.clicked() {
                app.dialog = Some(Dialog::ConfirmRestart);
            }
        });
    });
    ui.label(
        RichText::new(
            "The catalog is read-only in this build. Protocol and transport defaults are shown with their effective default values.",
        )
        .size(13.5)
        .color(palette.muted),
    );

    ui.add_space(8.0);
    ui.separator();
    widgets::section_header(ui, "Catalog", |_ui| {});

    if protocols.is_empty() && transports.is_empty() {
        widgets::empty_state(
            ui,
            theme,
            "Catalog is empty",
            "No protocols or transports were reported by the gateway.",
        );
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::CollapsingHeader::new(format!("Protocols ({})", protocols.len()))
                .default_open(true)
                .show(ui, |ui| {
                    if protocols.is_empty() {
                        ui.label(RichText::new("No protocols.").color(palette.muted));
                    }
                    for protocol in &protocols {
                        catalog_entry(
                            ui,
                            theme,
                            &protocol.name,
                            &protocol.id,
                            protocol.description.as_deref(),
                            &protocol.params,
                        );
                    }
                });
            egui::CollapsingHeader::new(format!("Transports ({})", transports.len()))
                .default_open(true)
                .show(ui, |ui| {
                    if transports.is_empty() {
                        ui.label(RichText::new("No transports.").color(palette.muted));
                    }
                    for transport in &transports {
                        catalog_entry(
                            ui,
                            theme,
                            &transport.name,
                            &transport.id,
                            transport.description.as_deref(),
                            &transport.params,
                        );
                    }
                });
        });
}

fn diagnostics_text(
    app: &App,
    server: &ServerInfoResult,
    protocols: &[ProtocolInfo],
    transports: &[TransportInfo],
) -> String {
    let endpoint = if app.gateway.is_demo() {
        "demo fixtures (in-process)".to_string()
    } else {
        app.gateway.endpoint().to_string()
    };
    let protocol_list = protocols
        .iter()
        .map(|protocol| protocol.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let transport_list = transports
        .iter()
        .map(|transport| transport.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "\
hodeco diagnostics
endpoint: {endpoint}
phase: {}
version: {}
protocol version: {} (UI expects {})
started: {}
uptime: {}
services: {}
loaded messages: {}
protocols: {protocol_list}
transports: {transport_list}
",
        app.gateway.phase().label(),
        server.version,
        server.protocol_version,
        crate::model::EXPECTED_PROTOCOL_VERSION,
        crate::model::format_datetime(server.started_at),
        crate::model::format_uptime(server.started_at, app.now),
        app.services.len(),
        app.messages.store_total,
    )
}

fn catalog_entry(
    ui: &mut Ui,
    theme: UiTheme,
    name: &str,
    id: &str,
    description: Option<&str>,
    params: &[ParamDef],
) {
    let palette = palette(theme);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(name).strong());
        ui.label(
            RichText::new(format!("({id})"))
                .monospace()
                .color(palette.muted),
        );
    });
    if let Some(description) = description {
        ui.label(RichText::new(description).size(13.5).color(palette.muted));
    }
    if params.is_empty() {
        ui.label(
            RichText::new("No parameters.")
                .size(13.5)
                .color(palette.muted),
        );
    } else {
        egui::Grid::new(("catalog-params", id, params.len()))
            .num_columns(3)
            .spacing([16.0, 3.0])
            .show(ui, |ui| {
                for def in params {
                    ui.label(RichText::new(&def.name).monospace());
                    let default = def
                        .default
                        .as_ref()
                        .map(|value| widgets::param_value_text(def.kind, value))
                        .unwrap_or_else(|| "—".to_string());
                    ui.label(RichText::new(default).monospace());
                    let mut notes = Vec::new();
                    if def.required {
                        notes.push("required".to_string());
                    }
                    if def.is_const {
                        notes.push("fixed".to_string());
                    }
                    if let Some(description) = &def.description {
                        notes.push(description.clone());
                    }
                    ui.label(
                        RichText::new(notes.join(" · "))
                            .size(13.5)
                            .color(palette.muted),
                    );
                    ui.end_row();
                }
            });
    }
    ui.add_space(2.0);
}

fn value_row(ui: &mut Ui, theme: UiTheme, label: &str, value: impl Into<String>) {
    let palette = palette(theme);
    ui.label(RichText::new(label).size(13.5).color(palette.muted));
    ui.label(RichText::new(value.into()).monospace());
    ui.end_row();
}
