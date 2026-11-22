//! Command catalog list and modal editor.

use super::super::App;
use crate::ui::widgets;
use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use hodeco_contract::domain::{CommandDefinition, CommandInfo};
use hodeco_contract::utils::{HumanDuration, id_from_name};
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Default)]
pub(crate) struct CommandsUi {
    pub commands: Vec<CommandInfo>,
    pub loading: bool,
    pub error: Option<String>,
    pub search: String,
    pub selected: Option<usize>,
    pub editor: Option<CommandEditor>,
    pub confirm_delete: Option<String>,
}

impl CommandsUi {
    pub fn filtered(&self) -> Vec<&CommandInfo> {
        let needle = self.search.trim().to_ascii_lowercase();
        self.commands
            .iter()
            .filter(|info| {
                needle.is_empty()
                    || info.command.name.to_ascii_lowercase().contains(&needle)
                    || info.command.id.contains(&needle)
                    || info
                        .command
                        .executable
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .contains(&needle)
            })
            .collect()
    }
}

#[derive(Debug)]
pub(crate) struct CommandEditor {
    pub draft: CommandDefinition,
    pub is_new: bool,
    pub id_auto: bool,
    pub executable: String,
    pub working_dir: String,
    pub args: String,
    pub timeout: String,
    pub error: Option<String>,
    pub saving: bool,
    pub confirm_delete: bool,
    pub used_by: Vec<String>,
}

impl CommandEditor {
    pub fn new(command: CommandDefinition, is_new: bool, used_by: Vec<String>) -> Self {
        let executable = command.executable.display().to_string();
        let working_dir = command
            .working_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let timeout = command.timeout.to_string();
        let args = command.args.join("\n");
        Self {
            draft: command,
            is_new,
            id_auto: is_new,
            executable,
            working_dir,
            args,
            timeout,
            error: None,
            saving: false,
            confirm_delete: false,
            used_by,
        }
    }

    pub fn build(&self) -> Result<CommandDefinition, String> {
        let mut command = self.draft.clone();
        command.name = command.name.trim().to_string();
        if command.name.is_empty() {
            return Err("Name must not be empty.".to_string());
        }
        let executable = self.executable.trim();
        if executable.is_empty() {
            return Err("Executable must not be empty.".to_string());
        }
        command.executable = PathBuf::from(executable);
        command.working_dir = {
            let value = self.working_dir.trim();
            (!value.is_empty()).then(|| PathBuf::from(value))
        };
        command.args = self
            .args
            .lines()
            .filter_map(|line| {
                let value = line.trim();
                (!value.is_empty()).then(|| value.to_string())
            })
            .collect();
        command.timeout =
            HumanDuration::from_str(self.timeout.trim()).map_err(|error| error.to_string())?;
        Ok(command)
    }
}

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    list_ui(app, ui);
    if app.commands.editor.is_some() {
        editor_modal(app, ui);
    }
}

fn list_ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let live = app.gateway.phase().is_live();
    let commands: Vec<CommandInfo> = app.commands.filtered().into_iter().cloned().collect();
    let total = app.commands.commands.len();
    let mut selected = app
        .commands
        .selected
        .filter(|index| *index < commands.len());

    widgets::page_header(
        ui,
        theme,
        "Commands",
        Some(&format!("{} configured", total)),
        |ui| {
            if ui.add_enabled(live, egui::Button::new("Refresh")).clicked() {
                app.refresh_commands();
            }
            if ui
                .add_enabled(ready, egui::Button::new("Add command"))
                .clicked()
            {
                app.open_command_editor(None);
            }
        },
    );
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.commands.search)
                .hint_text("Search commands…")
                .desired_width(360.0),
        );
        if app.commands.loading {
            ui.spinner();
        }
        if let Some(error) = &app.commands.error {
            ui.label(RichText::new(error).color(palette.red));
        }
    });
    ui.add_space(4.0);
    ui.label(RichText::new(format!("{total} commands")).color(palette.muted));
    ui.add_space(4.0);

    let nav = if app.dialog.is_none() && !commands.is_empty() && app.commands.editor.is_none() {
        widgets::list_nav_global(ui)
    } else {
        None
    };
    let list_focus = widgets::list_focus(ui, theme, "commands-list", nav.is_some(), false);
    let focus_row_actions = widgets::list_tab_pressed(ui, list_focus.id);
    let action_row = selected.unwrap_or(0);
    if focus_row_actions {
        selected = Some(action_row);
    }
    let mut action_focus_id = None;
    let mut activate = None;
    if let Some(nav) = nav {
        let next = nav.apply(selected, commands.len());
        selected = Some(next);
        if nav == widgets::ListNav::Activate {
            activate = commands.get(next).map(|info| info.command.clone());
        }
    }
    if list_focus.focused
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    {
        activate = commands
            .get(selected.unwrap_or(0))
            .map(|info| info.command.clone());
    }
    let mut clicked_selection = None;
    let scroll_to = nav
        .map(|_| selected.unwrap_or(0))
        .or_else(|| focus_row_actions.then_some(action_row));

    egui::ScrollArea::horizontal()
        .id_salt("commands-horizontal")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.set_min_width(900.0);
            let mut table = TableBuilder::new(ui)
                .id_salt("commands-table")
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::CLICK)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(200.0).at_least(150.0))
                .column(Column::initial(320.0).at_least(200.0))
                .column(Column::initial(100.0).at_least(80.0))
                .column(Column::initial(180.0).at_least(130.0))
                .column(Column::remainder().at_least(170.0));
            if let Some(index) = scroll_to {
                table = table.scroll_to_row(index, Some(egui::Align::Center));
            }
            table
                .header(28.0, |mut header| {
                    for title in ["Name / ID", "Executable", "Timeout", "Used by", "Actions"] {
                        header.col(|ui| {
                            ui.label(RichText::new(title).strong());
                        });
                    }
                })
                .body(|body| {
                    body.rows(42.0, commands.len(), |mut row| {
                        let index = row.index();
                        let info = &commands[index];
                        let command = &info.command;
                        row.set_selected(selected == Some(index));
                        row.col(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&command.name).strong());
                                ui.label(RichText::new(&command.id).small().color(palette.muted));
                            });
                        });
                        row.col(|ui| {
                            ui.label(
                                RichText::new(command.executable.display().to_string())
                                    .monospace()
                                    .small(),
                            );
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(command.timeout.to_string()).small());
                        });
                        row.col(|ui| {
                            let used = if info.used_by.is_empty() {
                                "—".to_string()
                            } else {
                                info.used_by.join(", ")
                            };
                            ui.label(RichText::new(used).small());
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                let edit = ui.add_enabled(ready, egui::Button::new("Edit").small());
                                if index == action_row {
                                    action_focus_id = Some(edit.id);
                                }
                                if edit.clicked() {
                                    app.open_command_editor(Some(command.clone()));
                                }
                                let blocked = !info.used_by.is_empty();
                                if ui
                                    .add_enabled(
                                        ready && !blocked,
                                        egui::Button::new("Delete").small(),
                                    )
                                    .on_hover_text(if blocked {
                                        "Remove the command from its tasks first"
                                    } else {
                                        "Delete command"
                                    })
                                    .clicked()
                                {
                                    app.commands.confirm_delete = Some(command.id.clone());
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
    app.commands.selected = selected;
    if let Some(command) = activate {
        app.open_command_editor(Some(command));
    }

    if commands.is_empty() {
        widgets::empty_state(
            ui,
            theme,
            if total == 0 {
                "No commands"
            } else {
                "No commands match the current filter"
            },
            "Commands are shared by task actions and selectors.",
        );
    }

    if let Some(command_id) = app.commands.confirm_delete.clone() {
        let detail = format!("Delete command '{command_id}'?");
        match widgets::confirm_modal(
            ui.ctx(),
            theme,
            "delete-command",
            "Delete command",
            &detail,
            "Delete command",
        ) {
            Some(true) => app.delete_command(&command_id),
            Some(false) => app.commands.confirm_delete = None,
            None => {}
        }
    }
}

fn args_editor(ui: &mut Ui, value: &mut String) {
    let mut lines: Vec<String> = value.lines().map(ToString::to_string).collect();
    let mut remove = None;
    for (index, line) in lines.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let remove_width = widgets::small_button_width(ui, "Remove");
            let width = (ui.spacing().text_edit_width - remove_width - ui.spacing().item_spacing.x)
                .max(120.0);
            ui.add(
                egui::TextEdit::singleline(line)
                    .hint_text("argument")
                    .desired_width(width),
            );
            if ui.small_button("Remove").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        lines.remove(index);
    }
    if ui.button("Add argument").clicked() {
        lines.push(" ".to_string());
    }
    *value = lines.join("\n");
}

fn editor_modal(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let ready = app.gateway.phase() == crate::client::Phase::Ready;
    let Some(mut editor) = app.commands.editor.take() else {
        return;
    };
    let mut keep = true;
    let mut close = false;
    let mut save = false;
    let mut delete = false;

    let response = egui::Modal::new(egui::Id::unique("hodeco-command-editor"))
        .frame(widgets::modal_frame(theme))
        .show(ui.ctx(), |ui| {
            widgets::dialog_width(ui, 640.0);
            widgets::dialog_header(
                ui,
                theme,
                if editor.is_new {
                    "New command"
                } else {
                    "Edit command"
                },
                Some("Reusable command invoked by task actions and selectors."),
            );
            widgets::dialog_body(ui, 520.0, |ui| {
                ui.spacing_mut().text_edit_width = (ui.available_width() - 160.0).max(240.0);
                widgets::control_group_fill(ui, "Identity", |ui| {
                    egui::Grid::new("command-identity")
                        .num_columns(2)
                        .spacing([18.0, 4.0])
                        .show(ui, |ui| {
                            widgets::field_label(ui, theme, "Name");
                            widgets::field_with_note(
                                ui,
                                app.theme,
                                "Display name in the commands list.",
                                |ui| {
                                    let response =
                                        ui.add(egui::TextEdit::singleline(&mut editor.draft.name));
                                    if response.changed() && editor.is_new && editor.id_auto {
                                        editor.draft.id =
                                            id_from_name("command", editor.draft.name.trim());
                                    }
                                },
                            );
                            ui.end_row();
                            widgets::field_label(ui, theme, "ID");
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
                                        let response = ui.add(
                                            egui::TextEdit::singleline(&mut editor.draft.id)
                                                .hint_text("generated from the name"),
                                        );
                                        if response.changed() {
                                            editor.id_auto = editor.draft.id.trim().is_empty();
                                        }
                                        if editor.id_auto && editor.draft.id.trim().is_empty() {
                                            editor.draft.id =
                                                id_from_name("command", editor.draft.name.trim());
                                        }
                                    },
                                );
                            } else {
                                ui.label(RichText::new(&editor.draft.id).monospace());
                            }
                            ui.end_row();
                            widgets::field_label(ui, theme, "Description");
                            widgets::field_with_note(
                                ui,
                                app.theme,
                                "Optional notes for operators.",
                                |ui| {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut editor.draft.description)
                                            .desired_rows(2),
                                    );
                                },
                            );
                            ui.end_row();
                        });
                });
                widgets::control_group_fill(ui, "Process", |ui| {
                    egui::Grid::new("command-process")
                        .num_columns(2)
                        .spacing([18.0, 4.0])
                        .show(ui, |ui| {
                            widgets::field_label(ui, theme, "Executable");
                            widgets::field_with_note(
                                ui,
                                app.theme,
                                "Absolute path, or a command found on PATH.",
                                |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut editor.executable)
                                            .hint_text("/usr/local/bin/tool"),
                                    );
                                },
                            );
                            ui.end_row();
                            widgets::field_label(ui, theme, "Arguments");
                            widgets::field_with_note(
                                ui,
                                app.theme,
                                "One argv entry per line; task arguments are appended after these.",
                                |ui| {
                                    args_editor(ui, &mut editor.args);
                                },
                            );
                            ui.end_row();
                            widgets::field_label(ui, theme, "Working directory");
                            widgets::field_with_note(
                                ui,
                                app.theme,
                                "Optional; defaults to the service working directory.",
                                |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut editor.working_dir)
                                            .hint_text("service working directory"),
                                    );
                                },
                            );
                            ui.end_row();
                            widgets::field_label(ui, theme, "Timeout");
                            widgets::field_with_note(
                                ui,
                                app.theme,
                                "Maximum process runtime.",
                                |ui| {
                                    widgets::duration_editor(
                                        ui,
                                        &mut editor.timeout,
                                        std::time::Duration::from_secs(1),
                                    );
                                },
                            );
                            ui.end_row();
                        });
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Runs as: service account · input: one JSON document on stdin")
                        .small()
                        .color(palette.muted),
                );
                if !editor.used_by.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("Used by").strong());
                    for task in &editor.used_by {
                        ui.label(task);
                    }
                }
                if let Some(error) = &editor.error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(error).color(palette.red));
                }
            });
            widgets::dialog_footer(ui, |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui
                    .add_enabled(ready && !editor.saving, egui::Button::new("Save"))
                    .clicked()
                {
                    save = true;
                }
                if !editor.is_new {
                    let blocked = !editor.used_by.is_empty();
                    if ui
                        .add_enabled(
                            ready && !blocked,
                            egui::Button::new(RichText::new("Delete").color(palette.red)),
                        )
                        .on_hover_text(if blocked {
                            "Remove the command from its tasks first"
                        } else {
                            "Delete command"
                        })
                        .clicked()
                    {
                        editor.confirm_delete = true;
                    }
                }
            });
            false
        });
    if response.should_close() && !save {
        close = true;
    }
    if editor.confirm_delete {
        let detail = format!("Delete command '{}'?", editor.draft.id);
        match widgets::confirm_modal(
            ui.ctx(),
            theme,
            "delete-command-editor",
            "Delete command",
            &detail,
            "Delete command",
        ) {
            Some(true) => delete = true,
            Some(false) => editor.confirm_delete = false,
            None => {}
        }
    }

    if save {
        match editor.build() {
            Ok(command) => {
                editor.error = None;
                editor.saving = true;
                if !app.save_command(command, editor.is_new) {
                    editor.saving = false;
                    editor.error = Some("The gateway is not connected.".to_string());
                }
            }
            Err(error) => editor.error = Some(error),
        }
    }
    if delete {
        let id = editor.draft.id.clone();
        app.delete_command(&id);
    }
    if close {
        keep = false;
    }
    if keep {
        app.commands.editor = Some(editor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timeout_written_by_the_duration_editor() {
        let mut editor = CommandEditor::new(
            CommandDefinition {
                id: "test".to_string(),
                name: "Test".to_string(),
                executable: PathBuf::from("test"),
                ..CommandDefinition::default()
            },
            true,
            Vec::new(),
        );
        editor.timeout = "10 s".to_string();
        let command = editor.build().unwrap();
        assert_eq!(command.timeout.as_std(), std::time::Duration::from_secs(10));
    }
}
