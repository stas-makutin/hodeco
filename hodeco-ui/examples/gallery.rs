//! Component gallery for the shared hodeco-ui building blocks.
//!
//! Run with:
//! `cargo run --example gallery`
//!
//! The gallery exercises theme colours, banners, pills, copy fields, tables,
//! parameter editors, message decoding, modals, and toasts. It does not connect
//! to a gateway.

use eframe::egui::{self, Align, Layout, RichText, Ui, ViewportBuilder};
use egui_extras::{Column, TableBuilder};
use hodeco_codec::hex;
use hodeco_codec::zwave;
use hodeco_contract::domain::{ParamDef, ParamType, ParamValues, ServiceState};
use hodeco_ui::theme::{self, UiTheme};
use hodeco_ui::ui::params::ParamEditor;
use hodeco_ui::ui::widgets::{self, Toast, Tone};
use serde_json::json;

struct Gallery {
    theme: UiTheme,
    applied_theme: Option<UiTheme>,
    show_modal: bool,
    toasts: Vec<Toast>,
    params: ParamEditor,
    param_result: Option<String>,
}

impl Default for Gallery {
    fn default() -> Self {
        let mut parity =
            ParamDef::with_default("parity", ParamType::String, json!("none"), "Serial parity");
        parity.enum_values = vec![json!("none"), json!("even"), json!("odd")];
        let mut defs = vec![
            ParamDef::with_default(
                "baudRate",
                ParamType::Uint,
                json!(115200),
                "Serial baud rate",
            ),
            parity,
            ParamDef::with_default(
                "readTimeout",
                ParamType::Duration,
                json!("5 s"),
                "Read timeout",
            ),
        ];
        defs[0].min = Some(json!(300));
        defs[0].max = Some(json!(4_000_000));
        Self {
            theme: UiTheme::Dark,
            applied_theme: None,
            show_modal: false,
            toasts: Vec::new(),
            params: ParamEditor::new(&defs, &ParamValues::new()),
            param_result: None,
        }
    }
}

impl Gallery {
    fn toast(&mut self, tone: Tone, text: impl Into<String>) {
        self.toasts.push(Toast {
            text: text.into(),
            tone,
        });
    }
}

impl eframe::App for Gallery {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.applied_theme != Some(self.theme) {
            self.theme.apply(ctx);
            self.applied_theme = Some(self.theme);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let theme = self.theme;
        let palette = theme::palette(theme);

        egui::Panel::top("gallery-top").show(ui, |ui| {
            egui::Frame::new()
                .fill(palette.panel)
                .inner_margin(egui::Margin::symmetric(12, 7))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("hodeco")
                                .size(18.0)
                                .strong()
                                .color(palette.accent),
                        );
                        ui.label(
                            RichText::new("component gallery")
                                .size(12.0)
                                .color(palette.muted),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .button(if theme == UiTheme::Dark {
                                    "Light"
                                } else {
                                    "Dark"
                                })
                                .clicked()
                            {
                                self.theme = theme.other();
                            }
                            if ui.button("Show modal").clicked() {
                                self.show_modal = true;
                            }
                            if ui.button("Toast").clicked() {
                                self.toast(Tone::Success, "Saved the component gallery state.");
                            }
                        });
                    });
                });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    widgets::page_header(
                        ui,
                        theme,
                        "Component gallery",
                        Some("Shared widgets, editors, tables, and message decoding"),
                        |ui| {
                            if ui.button("Success toast").clicked() {
                                self.toast(Tone::Success, "Service added.");
                            }
                            if ui.button("Error toast").clicked() {
                                self.toast(
                                    Tone::Error,
                                    "Failed to send message: service is not ready.",
                                );
                            }
                        },
                    );

                    section(ui, "Pills and status dots", |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for (tone, label) in [
                                (Tone::Neutral, "Stopped"),
                                (Tone::Info, "Starting"),
                                (Tone::Success, "Running"),
                                (Tone::Warning, "Pending"),
                                (Tone::Error, "Error"),
                            ] {
                                widgets::pill(ui, theme, label, tone);
                            }
                        });
                        ui.add_space(5.0);
                        ui.horizontal(|ui| {
                            for state in [
                                ServiceState::Stopped,
                                ServiceState::Starting,
                                ServiceState::Running,
                                ServiceState::Error,
                            ] {
                                widgets::status_dot(ui, theme::service_state_color(theme, state));
                                ui.label(RichText::new(format!("{state:?}")).size(11.0));
                            }
                        });
                    });

                    section(ui, "Banners", |ui| {
                        widgets::banner(
                            ui,
                            theme,
                            Tone::Info,
                            "Demo mode",
                            Some(
                                "Bundled fixtures are served in-process; no gateway is contacted.",
                            ),
                            |ui| {
                                if ui.button("Connect to gateway").clicked() {
                                    self.toast(Tone::Info, "Connect action");
                                }
                            },
                        );
                        widgets::banner(
                            ui,
                            theme,
                            Tone::Warning,
                            "Restarting",
                            Some("Waiting for the gateway to come back."),
                            |_ui| {},
                        );
                        widgets::banner(
                            ui,
                            theme,
                            Tone::Error,
                            "Protocol version mismatch",
                            Some("The gateway speaks 0.9; this UI expects 1.0."),
                            |_ui| {},
                        );
                    });

                    section(ui, "Read-only fields", |ui| {
                        widgets::copy_field(
                            ui,
                            theme,
                            "Endpoint",
                            "wss://gateway.example/prefix/api/ws",
                            false,
                        );
                        widgets::copy_field(
                            ui,
                            theme,
                            "Pretty JSON",
                            "{\n  \"service\": {\"alias\": \"ZW\"},\n  \"payload\": \"AQMAFel\"\n}",
                            true,
                        );
                        widgets::labeled_value(ui, theme, "Label", "Value");
                    });

                    section(ui, "Services table", |ui| {
                        let services = [
                            ("ZW", "zwave", "serial", "COM4", ServiceState::Running),
                            ("GW", "zwave", "serial", "COM3", ServiceState::Error),
                            ("—", "zwave", "serial", "COM7", ServiceState::Stopped),
                        ];
                        TableBuilder::new(ui)
                            .striped(true)
                            .resizable(true)
                            .max_scroll_height(160.0)
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::initial(90.0).at_least(60.0))
                            .column(Column::initial(90.0).at_least(60.0))
                            .column(Column::initial(90.0).at_least(60.0))
                            .column(Column::remainder().at_least(80.0))
                            .column(Column::initial(90.0).at_least(70.0))
                            .header(24.0, |mut header| {
                                for title in ["Alias", "Protocol", "Transport", "Entry", "State"] {
                                    header.col(|ui| {
                                        ui.label(RichText::new(title).strong());
                                    });
                                }
                            })
                            .body(|mut body| {
                                for (alias, protocol, transport, entry, state) in services {
                                    body.row(26.0, |mut row| {
                                        row.col(|ui| {
                                            ui.label(RichText::new(alias).monospace());
                                        });
                                        row.col(|ui| {
                                            ui.label(protocol);
                                        });
                                        row.col(|ui| {
                                            ui.label(transport);
                                        });
                                        row.col(|ui| {
                                            ui.label(RichText::new(entry).monospace());
                                        });
                                        row.col(|ui| {
                                            ui.label(
                                                RichText::new(format!("{state:?}")).color(
                                                    theme::service_state_color(theme, state),
                                                ),
                                            );
                                        });
                                    });
                                }
                            });
                    });

                    section(ui, "Structured parameter editor", |ui| {
                        ui.set_max_width(720.0);
                        self.params.ui(ui, theme);
                        ui.add_space(4.0);
                        if ui.button("Validate values").clicked() {
                            self.param_result = Some(match self.params.collect() {
                                Ok(values) => serde_json::to_string_pretty(&values)
                                    .unwrap_or_else(|_| "valid".to_string()),
                                Err(errors) => format!("invalid: {}", errors.join("; ")),
                            });
                        }
                        if let Some(result) = &self.param_result {
                            ui.label(RichText::new(result).monospace().size(11.0));
                        }
                    });

                    section(ui, "Hex dump and parsed decoder", |ui| {
                        let frame = zwave::response(&[0x15, 6, 3, 0, 1, 0, 1]);
                        ui.label(
                            RichText::new(hex::spaced_hex(&frame))
                                .monospace()
                                .color(palette.text),
                        );
                        ui.add_space(4.0);
                        for row in hex::dump(&frame, 16) {
                            ui.label(
                                RichText::new(format!(
                                    "{}  {:<47}  {}",
                                    row.offset_hex(),
                                    row.hex,
                                    row.ascii
                                ))
                                .monospace(),
                            );
                        }
                        ui.add_space(6.0);
                        let decoded = zwave::decode(&frame);
                        for (index, frame) in decoded.frames.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("Frame {} · {}", index + 1, frame.title))
                                        .strong(),
                                );
                                if frame.checksum_ok == Some(true) {
                                    ui.label(
                                        RichText::new("checksum OK")
                                            .size(11.0)
                                            .color(palette.green),
                                    );
                                }
                            });
                            egui::Grid::new(("gallery-fields", index))
                                .num_columns(3)
                                .spacing([14.0, 3.0])
                                .show(ui, |ui| {
                                    for field in &frame.fields {
                                        ui.label(
                                            RichText::new(theme::field_label(field.kind))
                                                .size(11.0)
                                                .color(theme::field_color(theme, field.kind)),
                                        );
                                        ui.label(RichText::new(&field.label).strong());
                                        ui.label(RichText::new(&field.value).monospace());
                                        ui.end_row();
                                    }
                                });
                        }
                        for issue in &decoded.issues {
                            ui.label(RichText::new(issue).size(11.0).color(palette.amber));
                        }
                    });

                    section(ui, "Empty and loading states", |ui| {
                        widgets::loading_state(ui, theme, "Loading messages…");
                        widgets::empty_state(
                            ui,
                            theme,
                            "No messages match the current search or filter",
                            "Change the search text, clear the filters, or load newer messages.",
                        );
                    });

                    ui.add_space(24.0);
                });
        });

        if self.show_modal {
            let mut close = false;
            let response = egui::Modal::new(egui::Id::unique("gallery-modal"))
                .frame(widgets::modal_frame(theme))
                .show(ui.ctx(), |ui| {
                    widgets::dialog_width(ui, 560.0);
                    widgets::dialog_header(
                        ui,
                        theme,
                        "Example dialog",
                        Some("Fixed header and footer, scrollable body."),
                    );
                    widgets::dialog_body(ui, 260.0, |ui| {
                        ui.label(RichText::new("Alias").strong());
                        let mut alias = String::new();
                        ui.add(
                            egui::TextEdit::singleline(&mut alias)
                                .hint_text("optional")
                                .desired_width(f32::INFINITY),
                        );
                        ui.label(
                            RichText::new("Long values use full-width fields with copy support.")
                                .size(11.0)
                                .color(palette.muted),
                        );
                        widgets::copy_field(ui, theme, "Payload", "01 03 00 15 E9", false);
                        for index in 0..12 {
                            ui.label(
                                RichText::new(format!("Scrollable body row {index}")).size(12.0),
                            );
                        }
                    });
                    widgets::dialog_footer(ui, |ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui
                            .add(egui::Button::new(
                                RichText::new("Save").color(palette.background).strong(),
                            ))
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
            if close || response.should_close() {
                self.show_modal = false;
            }
        }

        widgets::show_toasts(ui.ctx(), theme, &self.toasts);
    }
}

fn section(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    ui.add_space(10.0);
    ui.separator();
    widgets::section_header(ui, title, |_ui| {});
    body(ui);
    ui.add_space(2.0);
}

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("hodeco component gallery")
            .with_inner_size([1100.0, 860.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "hodeco component gallery",
        options,
        Box::new(|cc| Ok(Box::new(Gallery::new(cc)))),
    )
}

impl Gallery {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut gallery = Self::default();
        gallery.theme.apply(&cc.egui_ctx);
        gallery.applied_theme = Some(gallery.theme);
        gallery
    }
}
