//! RPC console: send arbitrary protocol methods and inspect raw responses.
//!
//! This is a diagnostics surface, not the primary workflow. Invalid JSON is
//! rejected locally and never sent.

use super::super::{App, ConsoleOutcome};
use crate::theme::UiTheme;
use crate::ui::widgets::{self, Tone};
use egui::{ComboBox, RichText, TextEdit, Ui};
use hodeco_contract::api::{ApiError, Method, example_params};
use serde_json::{Value, json};

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let live = app.gateway.phase().is_live();

    widgets::page_header(
        ui,
        theme,
        "Console",
        Some("Raw JSON-RPC diagnostics"),
        |ui| {
            if ui.button("Clear history").clicked() {
                app.console.entries.clear();
                app.console.expand_id = None;
                app.console.error = None;
            }
        },
    );

    widgets::banner(
        ui,
        theme,
        Tone::Warning,
        "Diagnostics surface",
        Some(
            "Requests use the same WebSocket session as the rest of the UI. Mutations take effect immediately and are not protected by the normal workflows.",
        ),
        |_ui| {},
    );

    ui.horizontal(|ui| {
        ui.label(RichText::new("Method").strong());
        let previous_method = app.console.method.clone();
        ComboBox::from_id_salt("console-method")
            .selected_text(app.console.method.clone())
            .width(260.0)
            .show_ui(ui, |ui| {
                for method in Method::ALL.into_iter().filter(|method| {
                    !matches!(method, Method::EventsSubscribe | Method::EventsUnsubscribe)
                }) {
                    ui.selectable_value(
                        &mut app.console.method,
                        method.as_str().to_string(),
                        method.as_str(),
                    );
                }
            });
        if app.console.method != previous_method {
            app.console.params = example_json(&app.console.method);
            app.console.error = None;
        }
        ui.add_space(12.0);
        if ui
            .add_enabled(
                live && !app.console.pending,
                egui::Button::new(if app.console.pending {
                    "Waiting…"
                } else {
                    "Send"
                }),
            )
            .clicked()
        {
            app.send_console_request();
        }
        if ui
            .button("Example")
            .on_hover_text("Load params generated from the hodeco-contract types")
            .clicked()
        {
            app.console.params = example_json(&app.console.method);
            app.console.error = None;
        }
        let request = request_text(&app.console.method, &app.console.params);
        widgets::copy_with_feedback(ui, &request, "Copy request");
        if app.console.pending {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(
                RichText::new("waiting for the gateway")
                    .size(13.5)
                    .color(palette.muted),
            );
        }
        if !live {
            ui.label(
                RichText::new("sending requires a live connection")
                    .size(13.5)
                    .color(palette.amber),
            );
        }
    });

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Request").strong());
        ui.label(
            RichText::new("{}")
                .monospace()
                .size(12.5)
                .color(palette.muted),
        );
    });
    ui.add(
        TextEdit::multiline(&mut app.console.params)
            .desired_width(f32::INFINITY)
            .desired_rows(9)
            .font(egui::TextStyle::Monospace)
            .code_editor(),
    );
    if let Some(error) = &app.console.error {
        ui.add_space(4.0);
        ui.label(RichText::new(error).color(palette.red));
    }

    ui.add_space(6.0);
    widgets::section_header(
        ui,
        &format!("History ({})", app.console.entries.len()),
        |_ui| {},
    );

    if app.console.entries.is_empty() {
        widgets::empty_state(
            ui,
            theme,
            "No requests yet",
            "Send a method to see its raw response here.",
        );
        return;
    }

    let expand_id = app.console.expand_id;
    let mut expand_rect = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            widgets::keyboard_scroll(ui, ui.rect_contains_pointer(ui.max_rect()));
            for entry in app.console.entries.iter().rev() {
                let (tone, label) = outcome_label(&entry.outcome);
                let header = format!(
                    "{}   {}   →   {}",
                    crate::model::format_datetime(entry.time),
                    entry.method,
                    label
                );
                let mut collapsing = egui::CollapsingHeader::new(
                    RichText::new(header).color(tone.color(theme)),
                )
                .id_salt(("console-entry", entry.id));
                if Some(entry.id) == expand_id {
                    collapsing = collapsing.open(Some(true));
                }
                let response = collapsing.show(ui, |ui| {
                    widgets::copy_field(ui, theme, "Request", &entry.params, true);
                    ui.add_space(4.0);
                    match &entry.outcome {
                        ConsoleOutcome::Pending => {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(14.0));
                                ui.label("Waiting for the response…");
                            });
                        }
                        ConsoleOutcome::Success(value) => {
                            let text = serde_json::to_string_pretty(value)
                                .unwrap_or_else(|_| value.to_string());
                            widgets::copy_field(ui, theme, "Response", &text, true);
                        }
                        ConsoleOutcome::Error(error) => {
                            error_block(ui, theme, error);
                        }
                        ConsoleOutcome::Timeout => {
                            ui.label(
                                RichText::new(
                                    "No response before the request timeout. The mutation outcome may be unknown.",
                                )
                                .color(palette.amber),
                            );
                        }
                        ConsoleOutcome::LocalError(error) => {
                            ui.label(RichText::new(error).color(palette.red));
                        }
                    }
                });
                if Some(entry.id) == expand_id {
                    expand_rect = Some(response.header_response.rect);
                }
            }
            if let Some(rect) = expand_rect {
                ui.scroll_to_rect(rect, Some(egui::Align::TOP));
            }
        });
    if expand_id.is_some() {
        app.console.expand_id = None;
    }
}

fn error_block(ui: &mut Ui, theme: UiTheme, error: &ApiError) {
    let json = serde_json::to_string_pretty(error).unwrap_or_else(|_| error.message.clone());
    widgets::copy_field(ui, theme, "Response", &json, true);
}

fn outcome_label(outcome: &ConsoleOutcome) -> (Tone, &'static str) {
    match outcome {
        ConsoleOutcome::Pending => (Tone::Info, "pending"),
        ConsoleOutcome::Success(_) => (Tone::Success, "success"),
        ConsoleOutcome::Error(_) => (Tone::Error, "error"),
        ConsoleOutcome::Timeout => (Tone::Warning, "timeout"),
        ConsoleOutcome::LocalError(_) => (Tone::Error, "not sent"),
    }
}

fn example_json(method: &str) -> String {
    let value = method
        .parse::<Method>()
        .map(example_params)
        .unwrap_or_else(|_| json!({}));
    crate::model::pretty_json(&value)
}

fn request_text(method: &str, params: &str) -> String {
    let method = method.trim();
    let parsed = if params.trim().is_empty() {
        Ok(json!({}))
    } else {
        serde_json::from_str::<Value>(params.trim())
    };
    match parsed {
        Ok(params) => serde_json::to_string_pretty(&json!({
            "method": method,
            "params": params,
        }))
        .unwrap_or_else(|_| params.to_string()),
        Err(_) => format!("{method}\n{params}"),
    }
}
