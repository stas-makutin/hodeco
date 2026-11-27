//! Modal dialogs: connection, service mutations, service parameters, message
//! details, and server restart confirmation.

use super::super::{
    AddServiceDialog, App, ConnectDialog, Dialog, EditAliasDialog, MessageDialog, MessageView,
    ServiceParamsDialog,
};
use super::builder;
use super::messages;
use crate::client::{Phase, RequestTag};
use crate::model;
use crate::theme::UiTheme;
use crate::ui::widgets::{self};
use egui::{ComboBox, Context, Id, RichText, TextEdit, Ui};
use hodeco_contract::api::Method;
use hodeco_contract::domain::{ServiceInfo, ServiceRef};
use serde_json::json;

pub(crate) fn show(app: &mut App, ctx: &Context, dialog: &mut Dialog, keep_open: &mut bool) {
    match dialog {
        Dialog::Connect(connect) => show_connect(app, ctx, connect, keep_open),
        Dialog::AddService(add) => show_add_service(app, ctx, add, keep_open),
        Dialog::EditAlias(edit) => show_edit_alias(app, ctx, edit, keep_open),
        Dialog::ConfirmRemove(service) => show_confirm_remove(app, ctx, service, keep_open),
        Dialog::ConfirmRestart => show_confirm_restart(app, ctx, keep_open),
        Dialog::ServiceStatus { service } => {
            show_service_status(app, ctx, service.clone(), keep_open)
        }
        Dialog::ServiceParams(service) => show_service_params(app, ctx, service, keep_open),
        Dialog::ConfigDirty => show_config_dirty(app, ctx, keep_open),
        Dialog::Message(dialog) => show_message(app, ctx, dialog, keep_open),
        Dialog::MessageFilters(dialog) => {
            messages::show_filters_dialog(app, ctx, dialog, keep_open)
        }
        Dialog::Builder(builder_dialog) => {
            *keep_open = builder::show(app, ctx, builder_dialog);
        }
    }
}

/// Runs one modal with the shared frame and fixed width. The body closure
/// returns whether it requested the dialog to close.
fn modal(
    ctx: &Context,
    theme: UiTheme,
    id: &str,
    width: f32,
    body: impl FnOnce(&mut Ui) -> bool,
) -> bool {
    let mut close = false;
    let response = egui::Modal::new(Id::unique(id))
        .frame(widgets::modal_frame(theme))
        .show(ctx, |ui| {
            widgets::dialog_width(ui, width);
            close = body(ui);
        });
    if response.should_close() {
        close = true;
    }
    !close
}

fn show_connect(app: &mut App, ctx: &Context, dialog: &mut ConnectDialog, keep_open: &mut bool) {
    let theme = app.theme;
    let palette = app.palette();
    let mut connect = false;
    let mut cancel = false;

    let keep = modal(ctx, theme, "hodeco-connect", 600.0, |ui| {
        widgets::dialog_header(
            ui,
            theme,
            "Connection",
            Some("Connect to a hodeco gateway or use the bundled fixtures."),
        );
        widgets::dialog_body(ui, 360.0, |ui| {
            ui.label(RichText::new("Gateway endpoint").strong());
            ui.add(
                TextEdit::singleline(&mut dialog.endpoint)
                    .hint_text(crate::client::endpoint::DEFAULT_NATIVE_ENDPOINT)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
            ui.label(
                RichText::new(
                    "The web build defaults to this page's origin. Use ?gateway_url=… or this dialog only for remote gateways.",
                )
                .size(13.5)
                .color(palette.muted),
            );

            ui.add_space(8.0);
            ui.checkbox(&mut dialog.demo, "Demo mode (bundled fixtures)");
            if dialog.demo {
                ui.label(
                    RichText::new("Demo mode ignores the endpoint and stays in-process.")
                        .size(13.5)
                        .color(palette.muted),
                );
            }
        });
        widgets::dialog_footer(ui, theme, dialog.error.as_deref(), |ui| {
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            if ui.button("Connect").clicked() {
                connect = true;
            }
        });
        false
    });

    *keep_open = keep;
    if cancel {
        *keep_open = false;
    }
    if connect {
        match app.connect(&dialog.endpoint, dialog.demo) {
            Ok(()) => *keep_open = false,
            Err(error) => dialog.error = Some(error),
        }
    }
}

fn show_add_service(
    app: &mut App,
    ctx: &Context,
    dialog: &mut AddServiceDialog,
    keep_open: &mut bool,
) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == Phase::Ready;
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

    let key = (dialog.protocol.clone(), dialog.transport.clone());
    if dialog
        .editor_key
        .as_ref()
        .is_some_and(|existing| existing != &key)
    {
        dialog.editor = None;
        dialog.editor_key = None;
    }
    if !key.0.is_empty() && !key.1.is_empty() {
        app.ensure_protocol_params(&key.0, &key.1);
    }
    let definitions = app.protocol_params.get(&key).cloned();
    let definition_error = app.protocol_params_errors.get(&key).cloned();
    if let Some(definitions) = &definitions
        && dialog.editor_key.as_ref() != Some(&key)
    {
        dialog.editor = Some(crate::ui::params::ParamEditor::new_overrides(
            definitions,
            &hodeco_contract::domain::ParamValues::new(),
        ));
        dialog.editor_key = Some(key.clone());
    }

    let mut cancel = false;
    let mut submit = false;

    let keep = modal(ctx, theme, "hodeco-add-service", 720.0, |ui| {
        widgets::dialog_header(
            ui,
            theme,
            "Add service",
            Some("Create a service from a protocol, transport, and entry."),
        );
        widgets::dialog_body(ui, 560.0, |ui| {
            ui.label(RichText::new("Alias").strong());
            ui.add(
                TextEdit::singleline(&mut dialog.alias)
                    .hint_text("optional; leave empty for no alias")
                    .desired_width(f32::INFINITY),
            );
            ui.label(
                RichText::new(
                    "An empty alias means the service has no alias. It is never stored or sent as an empty string.",
                )
                .size(13.5)
                .color(palette.muted),
            );

            ui.add_space(8.0);
            ui.label(RichText::new("Protocol").strong());
            let protocol_text = protocols
                .iter()
                .find(|(id, _)| id == &dialog.protocol)
                .map(|(_, label)| label.clone())
                .unwrap_or_else(|| "Select protocol".to_string());
            ComboBox::from_id_salt("add-service-protocol")
                .selected_text(protocol_text)
                .show_ui(ui, |ui| {
                    for (id, label) in &protocols {
                        ui.selectable_value(&mut dialog.protocol, id.clone(), label);
                    }
                });

            ui.add_space(8.0);
            ui.label(RichText::new("Transport").strong());
            let supported: Vec<(String, String)> = transports
                .iter()
                .filter(|(id, _)| {
                    model::protocol_supports_transport(&app.protocols, &dialog.protocol, id)
                })
                .cloned()
                .collect();
            if !supported.iter().any(|(id, _)| id == &dialog.transport) {
                dialog.transport = supported
                    .first()
                    .map(|(id, _)| id.clone())
                    .unwrap_or_default();
            }
            let transport_text = supported
                .iter()
                .find(|(id, _)| id == &dialog.transport)
                .map(|(_, label)| label.clone())
                .unwrap_or_else(|| "No compatible transport".to_string());
            ComboBox::from_id_salt("add-service-transport")
                .selected_text(transport_text)
                .show_ui(ui, |ui| {
                    for (id, label) in &supported {
                        ui.selectable_value(&mut dialog.transport, id.clone(), label);
                    }
                });

            ui.add_space(8.0);
            ui.label(RichText::new("Entry").strong());
            ui.add(
                TextEdit::singleline(&mut dialog.entry)
                    .hint_text("e.g. COM4 or /dev/ttyUSB0")
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
            if protocols.is_empty() || transports.is_empty() {
                ui.label(
                    RichText::new("Catalogs are still loading; adding a service is unavailable.")
                        .size(13.5)
                        .color(palette.amber),
                );
            }

            ui.add_space(10.0);
            ui.separator();
            ui.label(RichText::new("Parameters").strong());
            ui.add_space(4.0);
            if let Some(definitions) = &definitions {
                if definitions.is_empty() {
                    ui.label(
                        RichText::new(
                            "No parameters are exposed for this protocol/transport pair.",
                        )
                        .size(13.5)
                        .color(palette.muted),
                    );
                } else if let Some(editor) = &mut dialog.editor {
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

        let can_submit = ready
            && !dialog.pending
            && !dialog.protocol.is_empty()
            && !dialog.transport.is_empty()
            && !dialog.entry.trim().is_empty()
            && (definitions.is_some() || definition_error.is_some());
        let submit_hint = if !ready {
            "The gateway must be synced before adding a service."
        } else if dialog.pending {
            "Waiting for the gateway to add the service."
        } else if dialog.protocol.is_empty() || dialog.transport.is_empty() {
            "Choose a protocol and transport."
        } else if dialog.entry.trim().is_empty() {
            "Enter the transport entry, for example COM4 or /dev/ttyUSB0."
        } else {
            "Waiting for the parameter definitions."
        };
        widgets::dialog_footer(ui, theme, dialog.error.as_deref(), |ui| {
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            let response = ui.add_enabled(
                can_submit,
                egui::Button::new(if dialog.pending {
                    "Adding…"
                } else {
                    "Add service"
                }),
            );
            let response = if can_submit {
                response
            } else {
                response.on_disabled_hover_text(submit_hint)
            };
            if response.clicked() {
                submit = true;
            }
            if dialog.pending {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
        false
    });

    *keep_open = keep;
    if cancel {
        *keep_open = false;
    }
    if submit {
        dialog.pending = true;
        let params = match dialog.editor.as_mut() {
            Some(editor) => match editor.collect() {
                Ok(params) => params,
                Err(errors) => {
                    dialog.pending = false;
                    dialog.error = Some(errors.join("; "));
                    return;
                }
            },
            None => hodeco_contract::domain::ParamValues::new(),
        };
        if !app.add_service(dialog, params) {
            dialog.pending = false;
        }
    }
}

fn show_edit_alias(
    app: &mut App,
    ctx: &Context,
    dialog: &mut EditAliasDialog,
    keep_open: &mut bool,
) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == Phase::Ready;
    let mut cancel = false;
    let mut submit = false;

    let keep = modal(ctx, theme, "hodeco-edit-alias", 560.0, |ui| {
        widgets::dialog_header(ui, theme, "Change alias", Some(dialog.label.as_str()));
        widgets::dialog_body(ui, 260.0, |ui| {
            ui.label(RichText::new("New alias").strong());
            ui.add(
                TextEdit::singleline(&mut dialog.alias)
                    .hint_text("leave empty to clear the alias")
                    .desired_width(f32::INFINITY),
            );
            ui.label(
                RichText::new(
                    "An empty alias removes the alias entirely; the service keeps its composite key.",
                )
                .size(13.5)
                .color(palette.muted),
            );
        });
        widgets::dialog_footer(ui, theme, dialog.error.as_deref(), |ui| {
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            if ui
                .add_enabled(
                    ready && !dialog.pending,
                    egui::Button::new(if dialog.pending {
                        "Saving…"
                    } else {
                        "Save alias"
                    }),
                )
                .clicked()
            {
                submit = true;
            }
            if dialog.pending {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
        false
    });

    *keep_open = keep;
    if cancel {
        *keep_open = false;
    }
    if submit {
        dialog.pending = true;
        if !app.save_alias(dialog) {
            dialog.pending = false;
        }
    }
}

fn show_confirm_remove(
    app: &mut App,
    ctx: &Context,
    service: &mut ServiceInfo,
    keep_open: &mut bool,
) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == Phase::Ready;
    let reference = model::service_ref(service);
    let display = match &service.alias {
        Some(alias) => alias.clone(),
        None => model::composite_key(&reference),
    };
    let composite = model::composite_key(&reference);
    let mut cancel = false;
    let mut remove = false;

    let keep = modal(ctx, theme, "hodeco-remove-service", 560.0, |ui| {
        widgets::dialog_header(
            ui,
            theme,
            "Remove service",
            Some(&format!("Remove {display}?")),
        );
        widgets::dialog_body(ui, 300.0, |ui| {
            egui::Grid::new("remove-service-grid")
                .num_columns(2)
                .spacing([18.0, 5.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Alias").size(13.5).color(palette.muted));
                    ui.label(RichText::new(service.alias.as_deref().unwrap_or("—")).monospace());
                    ui.end_row();
                    ui.label(
                        RichText::new("Composite key")
                            .size(13.5)
                            .color(palette.muted),
                    );
                    ui.label(RichText::new(&composite).monospace());
                    ui.end_row();
                    ui.label(RichText::new("Status").size(13.5).color(palette.muted));
                    ui.label(
                        RichText::new(super::super::service_status_label(service.status.state))
                            .color(crate::theme::service_state_color(
                                theme,
                                service.status.state,
                            )),
                    );
                    ui.end_row();
                });
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "The service will be removed from the running registry and from the configuration. This cannot be undone.",
                )
                .color(palette.amber),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new("The server does not need a restart for this change.")
                    .size(13.5)
                    .color(palette.muted),
            );
        });
        widgets::dialog_footer(ui, theme, None, |ui| {
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            if ui
                .add_enabled(ready, egui::Button::new("Remove service"))
                .clicked()
            {
                remove = true;
            }
        });
        false
    });

    *keep_open = keep;
    if cancel {
        *keep_open = false;
    }
    if remove {
        app.rpc(
            RequestTag::ServiceRemove,
            Method::ServicesRemove,
            json!({ "service": reference }),
        );
        *keep_open = false;
    }
}

fn show_confirm_restart(app: &mut App, ctx: &Context, keep_open: &mut bool) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == Phase::Ready;
    let services = app.services.len();
    let mut cancel = false;
    let mut restart = false;

    let keep = modal(ctx, theme, "hodeco-restart", 560.0, |ui| {
        widgets::dialog_header(
            ui,
            theme,
            "Restart server?",
            Some("The gateway will stop, save state, and start again."),
        );
        widgets::dialog_body(ui, 320.0, |ui| {
            ui.label(
                RichText::new(
                    "The gateway performs a graceful shutdown and then starts a new process. Connections drop while it restarts.",
                )
                .color(palette.amber),
            );
            ui.add_space(8.0);
            ui.label(format!("Active services: {services}"));
            ui.label("The message buffer is persisted during a graceful shutdown.");
            ui.add_space(8.0);
            ui.label(
                RichText::new("The UI reconnects automatically when the server is back.")
                    .size(13.5)
                    .color(palette.muted),
            );
        });
        widgets::dialog_footer(ui, theme, None, |ui| {
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            if ui
                .add_enabled(ready, egui::Button::new("Restart server"))
                .clicked()
            {
                restart = true;
            }
        });
        false
    });

    *keep_open = keep;
    if cancel {
        *keep_open = false;
    }
    if restart {
        app.restart_server();
        *keep_open = false;
    }
}

fn show_service_status(app: &mut App, ctx: &Context, reference: ServiceRef, keep_open: &mut bool) {
    let theme = app.theme;
    let palette = app.palette();
    let live = app.gateway.phase().is_live();
    let service = model::find_service(&app.services, &reference).cloned();
    let mut close = false;
    let mut refresh = false;

    let keep = modal(ctx, theme, "hodeco-service-status", 620.0, |ui| {
        widgets::dialog_header(
            ui,
            theme,
            "Service status",
            Some(&model::composite_key(&reference)),
        );
        widgets::dialog_body(ui, 360.0, |ui| {
            let Some(service) = &service else {
                ui.label(
                    RichText::new("This service is no longer in the registry.")
                        .color(palette.amber),
                );
                return;
            };
            egui::Grid::new("service-status-grid")
                .num_columns(2)
                .spacing([18.0, 5.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Alias").size(13.5).color(palette.muted));
                    ui.label(RichText::new(service.alias.as_deref().unwrap_or("—")).monospace());
                    ui.end_row();
                    ui.label(RichText::new("Composite").size(13.5).color(palette.muted));
                    ui.label(RichText::new(model::service_key_label(service)).monospace());
                    ui.end_row();
                    ui.label(RichText::new("State").size(13.5).color(palette.muted));
                    ui.label(
                        RichText::new(super::super::service_status_label(service.status.state))
                            .color(crate::theme::service_state_color(
                                theme,
                                service.status.state,
                            )),
                    );
                    ui.end_row();
                    if let Some(detail) = &service.status.detail {
                        ui.label(RichText::new("Detail").size(13.5).color(palette.muted));
                        ui.label(RichText::new(detail).monospace());
                        ui.end_row();
                    }
                });
            ui.add_space(10.0);
            match &service.status.error {
                Some(error) => {
                    ui.label(RichText::new("Last error").strong().color(palette.red));
                    widgets::copy_field(ui, theme, "Error text", error, true);
                }
                None => {
                    ui.label(
                        RichText::new("No error is reported for this service.")
                            .color(palette.muted),
                    );
                }
            }
        });
        widgets::dialog_footer(ui, theme, None, |ui| {
            if ui.button("Close").clicked() {
                close = true;
            }
            if ui
                .add_enabled(live, egui::Button::new("Refresh status"))
                .clicked()
            {
                refresh = true;
            }
        });
        false
    });

    *keep_open = keep;
    if close {
        *keep_open = false;
    }
    if refresh {
        app.rpc(
            RequestTag::ServiceStatus {
                service: reference.clone(),
            },
            Method::ServicesStatus,
            json!({"service": reference}),
        );
    }
}

fn show_service_params(
    app: &mut App,
    ctx: &Context,
    dialog: &mut ServiceParamsDialog,
    keep_open: &mut bool,
) {
    let theme = app.theme;
    let palette = app.palette();
    let key = (
        dialog.service.protocol.clone(),
        dialog.service.transport.clone(),
    );
    app.ensure_protocol_params(&key.0, &key.1);
    let definitions = app.protocol_params.get(&key).cloned();
    let definition_error = app.protocol_params_errors.get(&key).cloned();
    if let Some(defs) = &definitions
        && dialog.editor_key.as_ref() != Some(&key)
    {
        dialog.editor = Some(crate::ui::params::ParamEditor::new_overrides(
            defs,
            &dialog.service.params,
        ));
        dialog.editor_key = Some(key.clone());
    }

    let mut close = false;
    let mut save = false;
    let composite = model::service_key_label(&dialog.service);
    let keep = modal(ctx, theme, "hodeco-service-params", 720.0, |ui| {
        widgets::dialog_header(ui, theme, "Service parameters", Some(&composite));
        widgets::dialog_body(ui, 500.0, |ui| {
            egui::Grid::new("service-params-summary")
                .num_columns(2)
                .spacing([18.0, 5.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Alias").size(13.5).color(palette.muted));
                    ui.label(
                        RichText::new(dialog.service.alias.as_deref().unwrap_or("—")).monospace(),
                    );
                    ui.end_row();
                    ui.label(RichText::new("Protocol").size(13.5).color(palette.muted));
                    ui.label(RichText::new(&dialog.service.protocol).monospace());
                    ui.end_row();
                    ui.label(RichText::new("Transport").size(13.5).color(palette.muted));
                    ui.label(RichText::new(&dialog.service.transport).monospace());
                    ui.end_row();
                    ui.label(RichText::new("Entry").size(13.5).color(palette.muted));
                    ui.label(RichText::new(&dialog.service.entry).monospace());
                    ui.end_row();
                    ui.label(RichText::new("Status").size(13.5).color(palette.muted));
                    ui.label(
                        RichText::new(super::super::service_status_label(
                            dialog.service.status.state,
                        ))
                        .color(crate::theme::service_state_color(
                            theme,
                            dialog.service.status.state,
                        )),
                    );
                    ui.end_row();
                });
            if let Some(error) = &dialog.service.status.error {
                ui.add_space(6.0);
                ui.label(RichText::new(error).color(palette.red));
            }

            ui.add_space(10.0);
            ui.separator();
            ui.label(RichText::new("Parameters").strong());
            ui.add_space(4.0);
            if let Some(defs) = &definitions {
                if defs.is_empty() {
                    ui.label(
                        RichText::new(
                            "No parameters are exposed for this protocol/transport pair.",
                        )
                        .size(13.5)
                        .color(palette.muted),
                    );
                } else if let Some(editor) = &mut dialog.editor {
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
        let dirty = dialog
            .editor
            .as_ref()
            .is_some_and(|editor| editor.is_dirty());
        widgets::dialog_footer(ui, theme, dialog.error.as_deref(), |ui| {
            if ui.button("Cancel").clicked() {
                close = true;
            }
            if ui
                .add_enabled(
                    definitions.is_some() && dirty && !dialog.pending,
                    egui::Button::new(if dialog.pending { "Saving…" } else { "Save" }),
                )
                .clicked()
            {
                save = true;
            }
            if dialog.pending {
                ui.add(egui::Spinner::new().size(14.0));
            }
            let json = model::pretty_json(&dialog.service);
            widgets::copy_with_feedback(ui, &json, "Copy JSON");
        });
        false
    });

    *keep_open = keep;
    if close {
        *keep_open = false;
    }
    if save && let Some(editor) = &mut dialog.editor {
        match editor.collect() {
            Ok(params) => {
                dialog.pending = true;
                dialog.error = None;
                let service = model::service_ref(&dialog.service);
                let params = serde_json::to_value(params).unwrap_or_default();
                if !app.rpc(
                    RequestTag::ServiceParams {
                        service: service.clone(),
                    },
                    Method::ServicesParams,
                    json!({"service": service, "params": params}),
                ) {
                    dialog.pending = false;
                }
            }
            Err(errors) => {
                dialog.error = Some(errors.join("; "));
            }
        }
    }
}

fn show_config_dirty(app: &mut App, ctx: &Context, keep_open: &mut bool) {
    let theme = app.theme;
    let ready = app.gateway.phase() == Phase::Ready;
    let saving = app.config_ui.saving;
    let mut cancel = false;
    let mut discard = false;
    let mut save = false;

    let keep = modal(ctx, theme, "hodeco-config-dirty", 560.0, |ui| {
        widgets::dialog_header(
            ui,
            theme,
            "Unsaved configuration changes",
            Some("Save the draft before leaving the Configuration tab?"),
        );
        widgets::dialog_body(ui, 220.0, |ui| {
            ui.label(
                "The configuration draft contains changes that have not been written to the gateway.",
            );
        });
        widgets::dialog_footer(ui, theme, app.config_ui.error.as_deref(), |ui| {
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            if ui
                .add_enabled(!saving, egui::Button::new("Discard and continue"))
                .clicked()
            {
                discard = true;
            }
            if ui
                .add_enabled(ready && !saving, egui::Button::new("Save and continue"))
                .clicked()
            {
                save = true;
            }
            if saving {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
        false
    });

    *keep_open = keep;
    if cancel {
        app.pending_tab = None;
        *keep_open = false;
    }
    if discard {
        app.discard_config_draft();
        let tab = app.pending_tab.take();
        *keep_open = false;
        if let Some(tab) = tab {
            app.tab = tab;
        }
    }
    if save {
        app.save_config_draft();
    }
    if !*keep_open && !app.config_ui.saving {
        app.pending_tab = None;
    }
}

fn show_message(app: &mut App, ctx: &Context, dialog: &mut MessageDialog, keep_open: &mut bool) {
    let theme = app.theme;
    let services = app.services.clone();
    let message = app
        .messages
        .message_by_id(dialog.id)
        .cloned()
        .or_else(|| {
            app.messages
                .selected_message()
                .filter(|message| message.id == dialog.id)
                .cloned()
        })
        .or_else(|| dialog.message.clone());
    let mut close = false;
    let mut use_filter = false;
    let mut retry = false;

    let keep = modal(ctx, theme, "hodeco-message-detail", 900.0, |ui| {
        match &message {
            Some(message) => {
                widgets::dialog_header(
                    ui,
                    theme,
                    &format!("Message {}", model::short_id(message.id)),
                    Some(model::message_summary(&services, message).as_str()),
                );
                ui.horizontal(|ui| {
                    ui.label(RichText::new("View").size(13.5).color(app.palette().muted));
                    let has_parser = model::message_has_parser(&services, message);
                    for view in MessageView::ALL {
                        if view == MessageView::Parsed && !has_parser {
                            continue;
                        }
                        if ui
                            .add(egui::Button::selectable(
                                app.messages.view == view,
                                view.label(),
                            ))
                            .clicked()
                        {
                            app.messages.view = view;
                        }
                    }
                });
                ui.add_space(4.0);
                ui.separator();
                let view = app.messages.view;
                let encoding = app.messages.text_encoding;
                let mut next_encoding = None;
                widgets::dialog_body(ui, 560.0, |ui| {
                    next_encoding =
                        messages::render_message(ui, theme, &services, message, view, encoding);
                });
                if let Some(encoding) = next_encoding {
                    app.messages.text_encoding = encoding;
                }
                widgets::dialog_footer(ui, theme, None, |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    if ui
                        .button("Search for payload")
                        .on_hover_text("Find this payload anywhere in the filtered messages")
                        .clicked()
                    {
                        use_filter = true;
                    }
                    widgets::copy_with_feedback(
                        ui,
                        &hodeco_codec::hex::spaced_hex(message.payload.as_slice()),
                        "Copy payload",
                    );
                    let json = model::pretty_json(message);
                    widgets::copy_with_feedback(ui, &json, "Copy JSON");
                });
            }
            None => {
                let subtitle = if dialog.loading {
                    "Fetching it with messages.get…"
                } else if dialog.error.is_some() {
                    "The message could not be loaded."
                } else {
                    "This message is no longer in the loaded buffer."
                };
                widgets::dialog_header(ui, theme, "Message details", Some(subtitle));
                widgets::dialog_body(ui, 180.0, |ui| {
                    if dialog.loading {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(14.0));
                            ui.label("Loading message…");
                        });
                    } else if dialog.error.is_none() {
                        ui.label(
                            "It may have been dropped or removed after a reconnect. Close this dialog and refresh the message list.",
                        );
                    }
                });
                widgets::dialog_footer(ui, theme, dialog.error.as_deref(), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    if ui
                        .add_enabled(!dialog.loading, egui::Button::new("Retry"))
                        .clicked()
                    {
                        retry = true;
                    }
                });
            }
        }
        false
    });

    *keep_open = keep;
    if close {
        *keep_open = false;
    }
    if retry {
        app.fetch_message(dialog.id);
    }
    if use_filter {
        if let Some(message) = message {
            app.search_for_payload(message.payload.as_slice().to_vec());
        }
        *keep_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, Pos2, RawInput, Rect, vec2};

    fn base_input() -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            ..Default::default()
        }
    }

    fn input_at(pos: Pos2, pressed: bool) -> RawInput {
        let mut input = base_input();
        input.events.push(Event::PointerMoved(pos));
        input.events.push(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        });
        input
    }

    #[test]
    fn modal_does_not_close_on_the_click_that_opened_it() {
        let ctx = egui::Context::default();
        let mut clicked = false;
        let mut keep_open = false;

        let mut rect = None;
        let mut output = ctx.run_ui(base_input(), |ui| {
            rect = Some(ui.button("Open").rect);
        });
        output.textures_delta.clear();
        let pos = rect.expect("button rect").center();

        let mut output = ctx.run_ui(input_at(pos, true), |ui| {
            let _ = ui.button("Open");
        });
        output.textures_delta.clear();

        let mut output = ctx.run_ui(input_at(pos, false), |ui| {
            let ctx = ui.ctx().clone();
            if ui.button("Open").clicked() {
                clicked = true;
                keep_open = modal(&ctx, UiTheme::Dark, "test-modal", 320.0, |ui| {
                    ui.label("Modal");
                    false
                });
            }
        });
        output.textures_delta.clear();

        assert!(clicked, "the opening button did not register a click");
        assert!(keep_open, "the modal closed on the click that opened it");
    }
}
