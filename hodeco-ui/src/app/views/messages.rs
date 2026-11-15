//! Messages tab: store-wide find, filtered local list, and the shared viewer.

use super::super::{
    App, ConditionEditor, FilterDialogMode, MessageFiltersDialog, MessageView,
    PayloadConditionDraft, RawConditionDraft, RawLeafDraft, SearchDirection,
};
use crate::model;
use crate::theme::UiTheme;
use crate::ui::widgets::{self, Tone};
use egui::text::LayoutJob;
use egui::{Color32, ComboBox, DragValue, FontId, Id, RichText, TextEdit, Ui};
use egui_extras::{Column, TableBuilder};
use hodeco_codec::definitions;
use hodeco_codec::entities;
use hodeco_codec::filter::{self, FrameKind, Includes};
use hodeco_codec::hex;
use hodeco_codec::parsed::{DecodeResult, FieldKind};
use hodeco_codec::zwave;
use hodeco_contract::domain::{Message, MessageDirection, MessageState, ServiceInfo, ServiceRef};

pub(crate) fn ui(app: &mut App, ui: &mut Ui) {
    let theme = app.theme;
    let palette = app.palette();
    let live = app.gateway.phase().is_live();
    let filter_count = app.messages.filters.count();
    let store_total = app.messages.store_total;
    let subtitle = if store_total == 1 {
        "1 stored message".to_string()
    } else {
        format!("{store_total} stored messages")
    };
    let has_dialog = app.dialog.is_some();
    if !has_dialog
        && !ui.ctx().any_popup_open()
        && ui.input(|input| input.key_pressed(egui::Key::Escape))
    {
        app.messages.clear_selection();
        app.messages.invalidate_search();
    }
    // The list owns keyboard navigation once armed. Clear any egui widget focus
    // so arrow keys cannot be consumed by focus navigation before the list sees
    // them. Search fields re-arm themselves when clicked.
    if app.messages.list_keyboard
        && !has_dialog
        && !ui.ctx().any_popup_open()
        && ui.ctx().memory(|memory| memory.focused()).is_some()
    {
        ui.ctx().memory_mut(|memory| memory.stop_text_input());
    }
    let f3 = if !has_dialog {
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::SHIFT, egui::Key::F3) {
                Some(SearchDirection::Previous)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::F3) {
                Some(SearchDirection::Next)
            } else {
                None
            }
        })
    } else {
        None
    };

    // Read list navigation before the context panel is rendered so a focused
    // detail control cannot swallow the arrow key first.
    let list_nav = if app.dialog.is_none() {
        widgets::list_nav_global(ui)
    } else {
        None
    };

    widgets::page_header(ui, theme, "Messages", Some(&subtitle), |ui| {
        if ui.add_enabled(live, egui::Button::new("Refresh")).clicked() {
            app.refresh_messages();
        }
        if ui
            .add_enabled(live, egui::Button::new("Send message"))
            .clicked()
        {
            app.open_builder(None);
        }
    });

    let mut search_changed = false;
    let mut mode_changed = false;
    let mut clear_search = false;
    let mut find_next = false;
    let mut find_previous = false;
    let mut open_filters = false;
    let mut open_options = false;
    let mut search_id = None;
    ui.horizontal(|ui| {
        let mut mode = app.messages.search_mode;
        ComboBox::from_id_salt("message-search-mode")
            .selected_text(mode.label())
            .width(112.0)
            .show_ui(ui, |ui| {
                for candidate in model::PayloadMode::ALL {
                    ui.selectable_value(&mut mode, candidate, candidate.label());
                }
            });
        if mode != app.messages.search_mode {
            app.messages.search_mode = mode;
            if mode.is_binary() {
                app.messages.search = model::normalize_hex_text(&app.messages.search);
            }
            mode_changed = true;
        }

        if app.messages.search_mode.is_binary() {
            let (changed, response) = widgets::hex_input_response(
                ui,
                &mut app.messages.search,
                app.messages.search_mode.hint(),
                false,
                1,
                320.0,
            );
            search_changed |= changed;
            search_id = Some(response.id);
            if response.has_focus() {
                app.messages.list_keyboard = false;
            }
            if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                find_next = true;
            }
        } else {
            let response = ui.add(
                TextEdit::singleline(&mut app.messages.search)
                    .hint_text(app.messages.search_mode.hint())
                    .desired_width(320.0),
            );
            search_id = Some(response.id);
            search_changed |= response.changed();
            if response.has_focus() {
                app.messages.list_keyboard = false;
            }
            if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                find_next = true;
            }
        }
        if !app.messages.search.is_empty() && ui.small_button("Clear").clicked() {
            app.messages.search.clear();
            clear_search = true;
        }
        let options_count = app.messages.options.count();
        let options_label = if options_count == 0 {
            "Options...".to_string()
        } else {
            format!("Options... ({options_count})")
        };
        let mut options = egui::Button::new(options_label);
        if options_count > 0 {
            options = options.fill(palette.accent.gamma_multiply(0.25));
        }
        open_options = ui
            .add(options)
            .on_hover_text("Additional search conditions ANDed with the search text")
            .clicked();
        find_next |= ui
            .button("Find")
            .on_hover_text("Find the next match in the filtered messages (F3)")
            .clicked();
        find_previous = ui
            .button("Find previous")
            .on_hover_text("Find the previous match (Shift+F3)")
            .clicked();
        let (status_text, status_color, status_tooltip) =
            if let Some(error) = &app.messages.search_error {
                (
                    "Invalid search".to_string(),
                    palette.red,
                    Some(error.clone()),
                )
            } else if app.messages.searching {
                (
                    "Searching…".to_string(),
                    palette.muted,
                    Some("Searching the filtered messages.".to_string()),
                )
            } else if let Some(status) = &app.messages.search_status {
                ("Not found".to_string(), palette.amber, Some(status.clone()))
            } else {
                (String::new(), palette.muted, None)
            };
        let status = ui.add_sized(
            [96.0, ui.spacing().interact_size.y],
            egui::Label::new(RichText::new(status_text).size(13.5).color(status_color)),
        );
        if let Some(tooltip) = status_tooltip {
            status.on_hover_text(tooltip);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if filter_count == 0 {
                "Filters".to_string()
            } else {
                format!("Filters ({filter_count})")
            };
            open_filters = ui.button(label).clicked();
        });
    });
    if mode_changed {
        app.message_search_mode_changed();
    }
    if search_changed || clear_search {
        app.message_search_text_changed();
    }
    if find_next {
        app.messages.list_keyboard = false;
        app.find_message(SearchDirection::Next);
    }
    if find_previous {
        app.messages.list_keyboard = false;
        app.find_message(SearchDirection::Previous);
    }
    if let Some(direction) = f3 {
        app.messages.list_keyboard = false;
        app.find_message(direction);
        find_next = true;
    }
    if (find_next || find_previous)
        && let Some(id) = search_id
    {
        ui.ctx().memory_mut(|memory| memory.request_focus(id));
    }
    if open_filters {
        app.messages.list_keyboard = false;
        app.open_message_filters();
    }
    if open_options {
        app.messages.list_keyboard = false;
        app.open_search_options();
    }
    filter_chips(app, ui, theme);

    if let Some(error) = &app.messages.error {
        let message = error.clone();
        widgets::banner(
            ui,
            theme,
            Tone::Error,
            "Could not load messages",
            Some(&message),
            |ui| {
                if ui.button("Retry").clicked() {
                    app.refresh_messages();
                }
            },
        );
    }

    ui_context(app, ui);

    egui::CentralPanel::default().show(ui, |ui| {
        if app.messages.messages.is_empty() {
            if app.messages.loading {
                widgets::loading_state(ui, theme, "Loading messages…");
            } else if store_total == 0 {
                widgets::empty_state(
                    ui,
                    theme,
                    "No messages",
                    "Messages appear here as the gateway receives or sends them.",
                );
            } else {
                widgets::empty_state(
                    ui,
                    theme,
                    "No messages match the current filter",
                    "Search does not hide rows; adjust Filters or refresh the list.",
                );
                if filter_count > 0 {
                    ui.vertical_centered(|ui| {
                        if ui.button("Clear filters").clicked() {
                            app.clear_filters();
                        }
                    });
                }
            }
            return;
        }

        // Row selection should work anywhere in a row. Selectable labels would
        // otherwise consume the click and start a text selection instead.
        ui.style_mut().interaction.selectable_labels = false;

        let services = app.services.clone();
        let compact = ui.available_width() < 1200.0;
        let table_height = (ui.available_height() - 58.0).max(80.0);
        let payload_char_width = ui
            .fonts_mut(|fonts| fonts.glyph_width(&FontId::monospace(13.5), '0'))
            .max(1.0);
        let list_focus =
            widgets::list_region(ui, theme, "messages-list", app.messages.list_keyboard);
        if list_nav.is_some() {
            app.messages.list_keyboard = true;
        }
        if app.dialog.is_some() || ui.ctx().text_edit_focused() || ui.ctx().any_popup_open() {
            app.messages.list_keyboard = false;
        }
        let pointer_pressed_list = ui.ctx().input(|input| {
            input.pointer.primary_pressed()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| list_focus.rect.contains(pos))
        });
        if pointer_pressed_list {
            app.messages.list_keyboard = true;
            // Clicking the list must also take keyboard focus away from the
            // search field; otherwise arrow keys keep editing the query.
            ui.ctx().memory_mut(|memory| memory.stop_text_input());
        }

        let total = app.messages.messages.len();
        let last_index = total.saturating_sub(1);
        let mut selected_row = app.messages.selected_row();
        if let Some(nav) = list_nav {
            let target = nav_target(
                nav,
                selected_row,
                app.messages.visible_start.min(last_index as u64) as usize,
                last_index,
            );
            if let Some(message) = app.messages.message_at(target).cloned() {
                if nav == widgets::ListNav::Activate {
                    app.open_message(message.id);
                } else {
                    app.messages.selected = Some(message.id);
                    app.messages.selected_message = Some(message);
                    app.messages.scroll_to_selected = true;
                    app.messages.invalidate_search();
                    selected_row = Some(target);
                }
            }
        }

        let mut first_visible_index = None;
        let mut last_visible_index = None;
        let row_painter = ui.painter().clone();
        let current_color = palette.accent;
        let request_scroll_selected = app.messages.scroll_to_selected;
        if request_scroll_selected {
            app.messages.scroll_to_selected = false;
        }
        let scroll_to = if request_scroll_selected {
            selected_row
        } else if let Some(anchor) = app.messages.scroll_anchor.take() {
            app.messages
                .messages
                .iter()
                .position(|message| message.id == anchor)
        } else if app.messages.scroll_to_bottom {
            Some(last_index)
        } else {
            None
        };
        let force_bottom = app.messages.scroll_to_bottom && scroll_to.is_none();
        app.messages.scroll_to_bottom = false;

        egui::ScrollArea::horizontal()
            .id_salt("messages-horizontal")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.set_min_width(if compact { 640.0 } else { 940.0 });
                let mut table = TableBuilder::new(ui)
                    .id_salt("messages-table")
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::CLICK)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::initial(150.0).at_least(132.0))
                    .column(Column::initial(110.0).at_least(80.0))
                    .column(Column::initial(86.0).at_least(70.0))
                    .column(Column::initial(52.0).at_least(42.0));
                if compact {
                    table = table.column(Column::remainder().at_least(180.0).clip(true));
                } else {
                    table = table
                        .column(Column::initial(300.0).at_least(140.0).clip(true))
                        .column(Column::remainder().at_least(180.0).clip(true));
                }
                if let Some(index) = scroll_to {
                    table = table.scroll_to_row(index, Some(egui::Align::Center));
                } else if force_bottom {
                    table = table.scroll_to_row(last_index, Some(egui::Align::BOTTOM));
                }
                table
                    .max_scroll_height(table_height)
                    .header(28.0, |mut header| {
                        let mut titles = vec!["Time", "Service", "State", "Bytes", "Summary"];
                        if !compact {
                            titles.push("Payload");
                        }
                        for title in titles {
                            header.col(|ui| {
                                ui.label(RichText::new(title).strong());
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
                            let message = app.messages.messages[index].clone();
                            let is_current = selected_row == Some(index);
                            let is_selected = app.messages.selected == Some(message.id);
                            row.set_selected(is_selected);
                            row.col(|ui| {
                                ui.label(
                                    RichText::new(model::format_datetime(message.time)).monospace(),
                                );
                            });
                            row.col(|ui| {
                                ui.label(model::display_service(&services, &message.service));
                            });
                            row.col(|ui| {
                                ui.label(
                                    RichText::new(model::message_state_label(message.state)).color(
                                        crate::theme::message_state_color(theme, message.state),
                                    ),
                                );
                            });
                            row.col(|ui| {
                                ui.label(message.payload.len().to_string());
                            });
                            row.col(|ui| {
                                ui.add(
                                    egui::Label::new(model::message_summary(&services, &message))
                                        .truncate(),
                                );
                            });
                            if !compact {
                                row.col(|ui| {
                                    let max_chars = (ui.available_width() / payload_char_width)
                                        .floor()
                                        .max(1.0)
                                        as usize;
                                    let limit = max_chars.saturating_sub(2) / 3;
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(model::message_preview(
                                                &message,
                                                limit.max(1),
                                            ))
                                            .monospace()
                                            .size(13.5),
                                        )
                                        .truncate(),
                                    );
                                });
                            }
                            let response = row.response();
                            if response.double_clicked() {
                                app.messages.selected = Some(message.id);
                                app.messages.selected_message = Some(message.clone());
                                app.messages.scroll_to_selected = true;
                                app.messages.invalidate_search();
                                reset_view_for_message(app, &services);
                                app.open_message(message.id);
                            } else if response.clicked() {
                                if app.messages.selected == Some(message.id) {
                                    app.messages.clear_selection();
                                } else {
                                    app.messages.selected = Some(message.id);
                                    app.messages.selected_message = Some(message.clone());
                                    app.messages.scroll_to_selected = true;
                                }
                                app.messages.invalidate_search();
                            }
                            if (is_current || row.response().hovered()) && !is_selected {
                                let rect = row.response().rect;
                                row_painter.rect_filled(
                                    egui::Rect::from_min_max(
                                        rect.left_top(),
                                        egui::pos2(rect.left() + 3.0, rect.bottom()),
                                    ),
                                    0.0,
                                    current_color,
                                );
                            }
                        });
                    });
            });

        app.messages.visible_start = first_visible_index.unwrap_or(0) as u64;
        app.messages.at_bottom =
            total == 0 || last_visible_index.is_some_and(|last| last.saturating_add(1) >= total);
        if !app.messages.loading_older
            && first_visible_index == Some(0)
            && app.messages.older_next.is_some()
        {
            app.request_message_page(crate::client::MessagePageKind::Older);
        }
        if !app.messages.loading_newer
            && last_visible_index.is_some_and(|last| last + 1 >= total)
            && app.messages.newer_next.is_some()
        {
            app.request_message_page(crate::client::MessagePageKind::Newer);
        }
    });
}

#[derive(Debug, Clone)]
enum FilterChipAction {
    State(MessageState),
    Direction(MessageDirection),
    Protocol(String),
    Transport(String),
    Service(ServiceRef),
    Payload(usize),
}

fn filter_chips(app: &mut App, ui: &mut Ui, theme: UiTheme) {
    if app.messages.filters.is_empty() {
        return;
    }
    let palette = app.palette();
    let mut chips: Vec<(String, FilterChipAction)> = Vec::new();
    for state in &app.messages.filters.states {
        chips.push((
            format!("State: {}", model::message_state_label(*state)),
            FilterChipAction::State(*state),
        ));
    }
    for direction in &app.messages.filters.directions {
        chips.push((
            format!("Direction: {}", direction_label(*direction)),
            FilterChipAction::Direction(*direction),
        ));
    }
    for protocol in &app.messages.filters.protocols {
        let label = app
            .protocols
            .iter()
            .find(|candidate| candidate.id == *protocol)
            .map(|candidate| candidate.name.clone())
            .unwrap_or_else(|| protocol.clone());
        chips.push((
            format!("Protocol: {label}"),
            FilterChipAction::Protocol(protocol.clone()),
        ));
    }
    for transport in &app.messages.filters.transports {
        let label = app
            .transports
            .iter()
            .find(|candidate| candidate.id == *transport)
            .map(|candidate| candidate.name.clone())
            .unwrap_or_else(|| transport.clone());
        chips.push((
            format!("Transport: {label}"),
            FilterChipAction::Transport(transport.clone()),
        ));
    }
    for service in &app.messages.filters.services {
        chips.push((
            format!(
                "Service: {}",
                model::display_service(&app.services, service)
            ),
            FilterChipAction::Service(service.clone()),
        ));
    }
    for (index, condition) in app.messages.filters.payload_conditions.iter().enumerate() {
        chips.push((condition.summary(), FilterChipAction::Payload(index)));
    }

    let mut remove = None;
    let mut clear = false;
    ui.horizontal(|ui| {
        if ui.small_button("Clear filters").clicked() {
            clear = true;
        }
        ui.label(
            RichText::new(format!("{} selected", chips.len()))
                .size(13.5)
                .color(palette.muted),
        );
    });
    if !chips.is_empty() {
        let available = ui.available_width();
        let font = FontId::proportional(13.5);
        let widths: Vec<f32> = chips
            .iter()
            .map(|(label, _)| {
                ui.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(label.clone(), font.clone(), Color32::WHITE)
                        .size()
                        .x
                }) + 36.0
            })
            .collect();
        let mut start = 0;
        while start < chips.len() {
            let mut end = start;
            let mut row_width = 0.0;
            while end < chips.len() {
                let width = widths[end];
                if end > start && row_width + width > available {
                    break;
                }
                row_width += width + 6.0;
                end += 1;
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (index, (label, action)) in chips.iter().enumerate().take(end).skip(start) {
                    if widgets::removable_chip(ui, theme, label, Tone::Info) {
                        remove = Some((index, action.clone()));
                    }
                }
            });
            start = end;
        }
    }

    if clear {
        app.clear_filters();
        return;
    }
    let Some((_, action)) = remove else {
        return;
    };
    let filters = &mut app.messages.filters;
    match action {
        FilterChipAction::State(state) => filters.states.retain(|candidate| *candidate != state),
        FilterChipAction::Direction(direction) => filters
            .directions
            .retain(|candidate| *candidate != direction),
        FilterChipAction::Protocol(protocol) => {
            filters.protocols.retain(|candidate| *candidate != protocol)
        }
        FilterChipAction::Transport(transport) => filters
            .transports
            .retain(|candidate| *candidate != transport),
        FilterChipAction::Service(service) => filters
            .services
            .retain(|candidate| !model::refs_match(candidate, &service)),
        FilterChipAction::Payload(index) => {
            if index < filters.payload_conditions.len() {
                filters.payload_conditions.remove(index);
            }
        }
    }
    app.filters_changed();
}

fn nav_target(
    nav: widgets::ListNav,
    selected: Option<usize>,
    visible_start: usize,
    last: usize,
) -> usize {
    let current = selected.unwrap_or(visible_start.min(last));
    match nav {
        widgets::ListNav::Up => selected.map_or(current, |index| index.saturating_sub(1)),
        widgets::ListNav::Down => selected.map_or(current, |index| (index + 1).min(last)),
        widgets::ListNav::PageUp => selected.map_or(current, |index| index.saturating_sub(20)),
        widgets::ListNav::PageDown => selected.map_or(current, |index| (index + 20).min(last)),
        widgets::ListNav::Home => 0,
        widgets::ListNav::End => last,
        widgets::ListNav::Activate => current,
    }
}

fn direction_label(direction: MessageDirection) -> &'static str {
    match direction {
        MessageDirection::Incoming => "Incoming",
        MessageDirection::Outgoing => "Outgoing",
    }
}

/// Inline message context panel above the message table. It is shown before the
/// table's central panel so the table area is reduced by the panel height.
fn ui_context(app: &mut App, ui: &mut Ui) {
    let Some(message) = app.messages.selected_message().cloned() else {
        return;
    };
    let theme = app.theme;
    let palette = app.palette();
    let services = app.services.clone();

    egui::Panel::bottom("hodeco-message-context")
        .resizable(true)
        .default_size(280.0)
        .min_size(140.0)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Message {}", model::short_id(message.id))).strong(),
                );
                ui.separator();
                ui.label(RichText::new("View").size(13.5).color(palette.muted));
                let has_parser = model::message_has_parser(&services, &message);
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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").on_hover_text("Close context").clicked() {
                        app.messages.clear_selection();
                        app.messages.invalidate_search();
                    }
                    if ui.button("Expand").clicked() {
                        app.open_message(message.id);
                    }
                    widgets::copy_with_feedback(
                        ui,
                        &hex::spaced_hex(message.payload.as_slice()),
                        "Copy payload",
                    );
                });
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(encoding) = render_message(
                        ui,
                        theme,
                        &services,
                        &message,
                        app.messages.view,
                        app.messages.text_encoding,
                    ) {
                        app.messages.text_encoding = encoding;
                    }
                });
        });
}

fn reset_view_for_message(app: &mut App, services: &[ServiceInfo]) {
    if app.messages.view != MessageView::Parsed {
        return;
    }
    if let Some(message) = app.messages.selected_message()
        && !model::message_has_parser(services, message)
    {
        app.messages.view = MessageView::Summary;
    }
}

pub(crate) fn show_filters_dialog(
    app: &mut App,
    ctx: &egui::Context,
    dialog: &mut MessageFiltersDialog,
    keep_open: &mut bool,
) {
    let theme = app.theme;
    let palette = app.palette();
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
    let services: Vec<(ServiceRef, String)> = app
        .services
        .iter()
        .map(|service| {
            let reference = model::service_ref(service);
            let label = match &service.alias {
                Some(alias) => format!("{alias} ({})", service.entry),
                None => model::service_key_label(service),
            };
            (reference, label)
        })
        .collect();
    let mode = dialog.mode;
    let mut draft = match mode {
        FilterDialogMode::Filters => std::mem::take(&mut app.messages.draft),
        FilterDialogMode::SearchOptions => std::mem::take(&mut app.messages.options_draft),
    };

    let mut cancel = false;
    let mut apply = false;
    let mut clear = false;
    let title = match mode {
        FilterDialogMode::Filters => "Message filters",
        FilterDialogMode::SearchOptions => "Search options",
    };
    let description = match mode {
        FilterDialogMode::Filters => {
            "Filters control which messages are loaded into the local list."
        }
        FilterDialogMode::SearchOptions => {
            "These conditions are ANDed with the search text and with Filters. They do not hide messages."
        }
    };
    let over = draft.exceeds_limits();
    let keep = egui::Modal::new(Id::unique("hodeco-message-filters"))
        .frame(widgets::modal_frame(theme))
        .show(ctx, |ui| {
            widgets::dialog_width(ui, 800.0);
            widgets::dialog_header(ui, theme, title, Some(description));
            widgets::dialog_body(ui, 560.0, |ui| {
                metadata_editor(app, ui, &mut draft, &protocols, &transports, &services, theme);
                ui.add_space(8.0);
                ui.separator();
                ui.label(RichText::new("Payload matches any of").strong());
                ui.label(
                    RichText::new(
                        "Each card is one OR condition. Fields inside a card are ANDed. Unchecked frame fields are omitted.",
                    )
                    .size(13.5)
                    .color(palette.muted),
                );
                let mut edit = None;
                let mut remove = None;
                for (index, condition) in draft.payload_conditions.iter().enumerate() {
                    ui.add_space(4.0);
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(condition.summary()).strong());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Remove").clicked() {
                                        remove = Some(index);
                                    }
                                    if ui.small_button("Edit").clicked() {
                                        edit = Some(index);
                                    }
                                },
                            );
                        });
                    });
                }
                if let Some(index) = remove {
                    draft.payload_conditions.remove(index);
                }
                if let Some(index) = edit {
                    let condition = draft.payload_conditions[index].clone();
                    dialog.editor = Some(ConditionEditor {
                        index: Some(index),
                        draft: condition,
                        error: None,
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Add Z-Wave frame...").clicked() {
                        dialog.editor = Some(ConditionEditor {
                            index: None,
                            draft: PayloadConditionDraft::Frame(filter::FilterSpec::default()),
                            error: None,
                        });
                    }
                    if ui.button("Add raw bytes...").clicked() {
                        dialog.editor = Some(ConditionEditor {
                            index: None,
                            draft: PayloadConditionDraft::Raw(RawConditionDraft::new()),
                            error: None,
                        });
                    }
                });
                let leaves = draft.leaf_count();
                let bytes = draft.byte_count();
                let over = draft.exceeds_limits();
                ui.label(
                    RichText::new(format!(
                        "{leaves}/{} match sequences · {bytes}/{} pattern bytes",
                        hodeco_contract::domain::MAX_PAYLOAD_LEAVES,
                        hodeco_contract::domain::MAX_PAYLOAD_TOTAL_BYTES
                    ))
                    .size(13.5)
                    .color(if over { palette.red } else { palette.muted }),
                );
                if let Some(error) = &draft.error {
                    ui.label(RichText::new(error).size(13.5).color(palette.red));
                }
            });
            widgets::dialog_footer(ui, |ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                let clear_label = match mode {
                    FilterDialogMode::Filters => "Clear filters",
                    FilterDialogMode::SearchOptions => "Clear options",
                };
                if ui.button(clear_label).clicked() {
                    clear = true;
                }
                if ui
                    .add_enabled(!over, egui::Button::new("Apply"))
                    .clicked()
                {
                    apply = true;
                }
            });
            false
        });

    let mut save_editor = false;
    let mut close_editor = false;
    if let Some(editor) = dialog.editor.as_mut() {
        let editor_title = match &editor.draft {
            PayloadConditionDraft::Frame(_) => "Z-Wave frame condition",
            PayloadConditionDraft::Raw(_) => "Raw bytes condition",
        };
        let mut save = false;
        let mut close = false;
        let editor_modal = egui::Modal::new(Id::unique("hodeco-payload-condition"))
            .frame(widgets::modal_frame(theme))
            .show(ctx, |ui| {
                widgets::dialog_width(ui, 860.0);
                widgets::dialog_header(ui, theme, editor_title, None);
                widgets::dialog_body(ui, 540.0, |ui| {
                    match &mut editor.draft {
                        PayloadConditionDraft::Frame(spec) => frame_condition_editor(app, ui, spec),
                        PayloadConditionDraft::Raw(raw) => raw_condition_editor(ui, raw, theme),
                    }
                    if let Some(error) = &editor.error {
                        ui.add_space(6.0);
                        ui.label(RichText::new(error).size(13.5).color(palette.red));
                    }
                });
                widgets::dialog_footer(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    let label = if editor.index.is_some() {
                        "Save condition"
                    } else {
                        "Add condition"
                    };
                    if ui.button(label).clicked() {
                        save = true;
                    }
                });
                false
            });
        if save {
            match editor.draft.to_expr() {
                Ok(_) => save_editor = true,
                Err(error) => editor.error = Some(error),
            }
        } else if close || editor_modal.should_close() {
            close_editor = true;
        }
    }
    if save_editor {
        let editor = dialog.editor.take().expect("editor exists");
        if let Some(index) = editor.index {
            draft.payload_conditions[index] = editor.draft;
        } else {
            draft.payload_conditions.push(editor.draft);
        }
    } else if close_editor {
        dialog.editor = None;
    }

    if keep.should_close() {
        *keep_open = false;
    }
    if cancel {
        *keep_open = false;
    }
    if clear {
        *keep_open = false;
        match mode {
            FilterDialogMode::Filters => app.clear_filters(),
            FilterDialogMode::SearchOptions => app.clear_search_options(),
        }
        return;
    }
    if apply {
        match mode {
            FilterDialogMode::Filters => {
                app.messages.draft = draft;
                app.apply_filters();
                if app.messages.draft.error.is_none() {
                    *keep_open = false;
                }
            }
            FilterDialogMode::SearchOptions => {
                app.messages.options_draft = draft;
                app.apply_search_options();
                if app.messages.options_draft.error.is_none() {
                    *keep_open = false;
                }
            }
        }
        return;
    }
    match mode {
        FilterDialogMode::Filters => app.messages.draft = draft,
        FilterDialogMode::SearchOptions => app.messages.options_draft = draft,
    }
}

fn metadata_editor(
    app: &mut App,
    ui: &mut Ui,
    draft: &mut super::super::FilterDraft,
    protocols: &[(String, String)],
    transports: &[(String, String)],
    services: &[(ServiceRef, String)],
    theme: UiTheme,
) {
    let palette = app.palette();
    ui.label(RichText::new("State").strong());
    ui.horizontal_wrapped(|ui| {
        for state in super::super::message_states() {
            let mut checked = draft.states.contains(state);
            if ui
                .checkbox(&mut checked, model::message_state_label(*state))
                .changed()
            {
                if checked {
                    draft.states.push(*state);
                } else {
                    draft.states.retain(|value| value != state);
                }
            }
        }
    });

    ui.add_space(4.0);
    ui.label(RichText::new("Protocol").strong());
    egui::ScrollArea::vertical()
        .id_salt("message-filter-protocols")
        .max_height(130.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (id, label) in protocols {
                let mut checked = draft.protocols.contains(id);
                if ui
                    .checkbox(&mut checked, format!("{label} ({id})"))
                    .changed()
                {
                    if checked {
                        draft.protocols.push(id.clone());
                    } else {
                        draft.protocols.retain(|candidate| candidate != id);
                    }
                }
            }
            ui.horizontal(|ui| {
                if ui.small_button("All").clicked() {
                    draft.protocols = protocols.iter().map(|(id, _)| id.clone()).collect();
                }
                if ui.small_button("Clear").clicked() {
                    draft.protocols.clear();
                }
            });
        });

    ui.add_space(4.0);
    ui.label(RichText::new("Transport").strong());
    egui::ScrollArea::vertical()
        .id_salt("message-filter-transports")
        .max_height(100.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (id, label) in transports {
                let mut checked = draft.transports.contains(id);
                if ui
                    .checkbox(&mut checked, format!("{label} ({id})"))
                    .changed()
                {
                    if checked {
                        draft.transports.push(id.clone());
                    } else {
                        draft.transports.retain(|candidate| candidate != id);
                    }
                }
            }
            ui.horizontal(|ui| {
                if ui.small_button("All").clicked() {
                    draft.transports = transports.iter().map(|(id, _)| id.clone()).collect();
                }
                if ui.small_button("Clear").clicked() {
                    draft.transports.clear();
                }
            });
        });

    ui.add_space(4.0);
    ui.label(RichText::new("Service").strong());
    ui.horizontal(|ui| {
        ui.add(
            TextEdit::singleline(&mut draft.service_query)
                .hint_text("Filter the service list…")
                .desired_width(280.0),
        );
        if !draft.service_query.is_empty() && ui.small_button("Clear").clicked() {
            draft.service_query.clear();
        }
    });
    let query = draft.service_query.trim().to_lowercase();
    egui::ScrollArea::vertical()
        .id_salt("message-filter-services")
        .max_height(150.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if services.is_empty() {
                ui.label(
                    RichText::new("No services known")
                        .size(13.5)
                        .color(palette.muted),
                );
            }
            let mut shown = 0usize;
            for (reference, label) in services {
                if !query.is_empty() && !label.to_lowercase().contains(&query) {
                    continue;
                }
                shown += 1;
                let mut checked = draft
                    .services
                    .iter()
                    .any(|candidate| model::refs_match(candidate, reference));
                if ui.checkbox(&mut checked, label).changed() {
                    if checked {
                        draft.services.push(reference.clone());
                    } else {
                        draft
                            .services
                            .retain(|candidate| !model::refs_match(candidate, reference));
                    }
                }
            }
            if shown == 0 {
                ui.label(
                    RichText::new("No services match the filter")
                        .size(13.5)
                        .color(palette.muted),
                );
            }
        });
    let _ = theme;
}

fn match_checkbox(ui: &mut Ui, include: &mut bool, enabled: bool) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // Keep the checkbox clear of the dialog scrollbar.
        ui.add_space(14.0);
        ui.add_enabled(enabled, egui::Checkbox::new(include, "Match in filter"));
    });
}

fn frame_condition_editor(app: &App, ui: &mut Ui, spec: &mut filter::FilterSpec) {
    let palette = app.palette();
    let definitions = definitions::current();
    ui.label(RichText::new("Frame type").strong());
    ComboBox::from_id_salt("condition-frame-type")
        .selected_text(spec.frame.label())
        .width(360.0)
        .show_ui(ui, |ui| {
            for kind in FrameKind::ALL {
                ui.selectable_value(&mut spec.frame, kind, kind.label());
            }
        });
    ui.label(
        RichText::new(format!(
            "{} frame · one OR condition",
            spec.frame.direction_label()
        ))
        .size(13.5)
        .color(palette.muted),
    );
    ui.add_space(6.0);

    if spec.frame.is_control() {
        ui.label(
            RichText::new("Control frames are matched by their one-byte ID.")
                .size(13.5)
                .color(palette.muted),
        );
        frame_preview(ui, app.theme, spec);
        return;
    }

    if spec.frame.uses_node() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Node(s)").strong());
            if spec.frame == FrameKind::Broadcast {
                ui.label(RichText::new("0xFF (fixed)").monospace());
                spec.includes.node = true;
                match_checkbox(ui, &mut spec.includes.node, false);
            } else {
                let mut text = spec.nodes_text.clone();
                if widgets::numeric_input(ui, &mut text, "e.g. 2, 7", 150.0, true) {
                    spec.nodes_text = text;
                    spec.nodes = model::parse_node_list(&spec.nodes_text).unwrap_or_default();
                    spec.includes.node = !spec.nodes.is_empty();
                }
                match_checkbox(ui, &mut spec.includes.node, !spec.nodes.is_empty());
            }
        });
        if spec.frame != FrameKind::Broadcast {
            if spec.frame == FrameKind::SendDataMulti {
                ui.label(
                    RichText::new(
                        "SendDataMulti matches the exact node set only; a single-node expansion is not added.",
                    )
                    .size(13.5)
                    .color(palette.muted),
                );
            } else if spec.nodes.len() > 1 {
                ui.label(
                    RichText::new("Each listed node becomes a separate OR condition.")
                        .size(13.5)
                        .color(palette.muted),
                );
            }
        }
    }

    if spec.frame.uses_command_data() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Command class").strong());
            let class_items: Vec<(u8, String)> = definitions
                .command_classes
                .iter()
                .map(|class| (class.id, format!("{} (0x{:02x})", class.name, class.id)))
                .collect();
            let selected_class = spec
                .command_class
                .and_then(|id| definitions.command_class(id));
            let selected_text = match selected_class {
                Some(class) => format!("{} (0x{:02x})", class.name, class.id),
                None => "Any command class".to_string(),
            };
            let mut class_choice = spec.command_class;
            widgets::searchable_combo(
                ui,
                "condition-command-class",
                selected_text,
                320.0,
                &mut class_choice,
                widgets::ComboOptions {
                    any_label: Some("Any command class"),
                    emphasize: &[],
                },
                &class_items,
            );
            if class_choice != spec.command_class {
                spec.command_class = class_choice;
                spec.command = None;
                spec.includes.command_class = class_choice.is_some();
                spec.includes.command = false;
            }
            match_checkbox(
                ui,
                &mut spec.includes.command_class,
                spec.command_class.is_some(),
            );
        });

        let class = spec
            .command_class
            .and_then(|id| definitions.command_class(id));
        ui.horizontal(|ui| {
            ui.label(RichText::new("Command").strong());
            let command_items: Vec<(u8, String)> = class
                .map(|class| {
                    class
                        .commands
                        .iter()
                        .map(|command| {
                            (
                                command.id,
                                format!("{} (0x{:02x})", command.name, command.id),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let selected_text = match class.and_then(|class| {
                spec.command
                    .and_then(|id| class.commands.iter().find(|command| command.id == id))
            }) {
                Some(command) => format!("{} (0x{:02x})", command.name, command.id),
                None => "Any command".to_string(),
            };
            let mut command_choice = spec.command;
            widgets::searchable_combo(
                ui,
                "condition-command",
                selected_text,
                320.0,
                &mut command_choice,
                widgets::ComboOptions {
                    any_label: Some("Any command"),
                    emphasize: &[],
                },
                &command_items,
            );
            if command_choice != spec.command {
                spec.command = command_choice;
                if command_choice.is_some() {
                    spec.includes.command = true;
                    spec.includes.command_class = true;
                } else {
                    spec.includes.command = false;
                }
            }
            match_checkbox(ui, &mut spec.includes.command, spec.command.is_some());
        });

        ui.horizontal(|ui| {
            ui.label(RichText::new("Command data").strong());
            let mut data_text = spec.command_data_hex.clone();
            if widgets::hex_input(
                ui,
                &mut data_text,
                "bytes after the command, e.g. 63",
                false,
                1,
                300.0,
            ) {
                spec.command_data_hex = data_text;
                spec.command_data =
                    hodeco_codec::parse_hex(&spec.command_data_hex).unwrap_or_default();
                spec.includes.command_data = !spec.command_data_hex.trim().is_empty();
            }
            match_checkbox(
                ui,
                &mut spec.includes.command_data,
                !spec.command_data_hex.trim().is_empty(),
            );
        });
        if !spec.command_data_hex.trim().is_empty()
            && hodeco_codec::parse_hex(&spec.command_data_hex).is_err()
        {
            ui.label(
                RichText::new("command data needs an even number of hex digits")
                    .size(13.5)
                    .color(palette.red),
            );
        }
    }

    if spec.frame.uses_tx_options() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("TX options").strong());
            let mut ack = spec.tx_options & hodeco_codec::builder::TX_OPTION_ACK != 0;
            let mut no_route = spec.tx_options & hodeco_codec::builder::TX_OPTION_NO_ROUTE != 0;
            if ui.checkbox(&mut ack, "ACK").changed()
                || ui.checkbox(&mut no_route, "NO_ROUTE").changed()
            {
                let mut value = 0;
                if ack {
                    value |= hodeco_codec::builder::TX_OPTION_ACK;
                }
                if no_route {
                    value |= hodeco_codec::builder::TX_OPTION_NO_ROUTE;
                }
                spec.tx_options = value;
            }
            match_checkbox(ui, &mut spec.includes.tx_options, true);
        });
    }

    if spec.frame.uses_callback_id() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Callback ID").strong());
            ui.add(DragValue::new(&mut spec.callback_id).range(0..=u8::MAX));
            match_checkbox(ui, &mut spec.includes.callback_id, true);
        });
    }
    if spec.frame.uses_rx_status() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("RX status").strong());
            ui.add(DragValue::new(&mut spec.rx_status).range(0..=u8::MAX));
            match_checkbox(ui, &mut spec.includes.rx_status, true);
        });
    }
    if spec.frame.uses_tx_status() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("TX status").strong());
            ui.add(DragValue::new(&mut spec.tx_status).range(0..=u8::MAX));
            match_checkbox(ui, &mut spec.includes.tx_status, true);
        });
    }

    ui.add_space(8.0);
    ui.separator();
    frame_structural(ui, spec, palette);
    frame_preview(ui, app.theme, spec);
}

fn frame_structural(ui: &mut Ui, spec: &mut filter::FilterSpec, palette: crate::theme::Palette) {
    if spec.requires_derived_length() {
        spec.includes.derived_length = true;
    }
    ui.label(RichText::new("Structural bytes").strong());
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut spec.includes.sof, "SOF");
        ui.checkbox(&mut spec.includes.frame_length, "Frame length");
        ui.checkbox(&mut spec.includes.frame_type, "Frame type");
        ui.checkbox(&mut spec.includes.serial_command, "Serial command");
        ui.add_enabled(
            !spec.requires_derived_length(),
            egui::Checkbox::new(&mut spec.includes.derived_length, "Derived lengths"),
        )
        .on_disabled_hover_text(
            "Required because a later matched field follows variable-length data.",
        );
        ui.checkbox(&mut spec.includes.checksum, "Checksum");
    });
    ui.horizontal(|ui| {
        if ui
            .button("Exact frame")
            .on_hover_text("Include every byte of the generated frame.")
            .clicked()
        {
            spec.includes = Includes::exact();
        }
        if ui.button("Defaults").clicked() {
            spec.includes = Includes::default();
        }
    });
    ui.label(
        RichText::new(
            "Frame type and serial command are included by default. Exact frame is not an exact payload-length match.",
        )
        .size(13.5)
        .color(palette.muted),
    );
}

fn frame_preview(ui: &mut Ui, theme: UiTheme, spec: &filter::FilterSpec) {
    let palette = crate::theme::palette(theme);
    ui.add_space(6.0);
    ui.label(RichText::new("Condition preview").strong());
    match spec.conditions() {
        Ok(conditions) => {
            let exact = conditions.iter().all(|condition| condition.exact);
            let plural = conditions.len() > 1;
            let badge = match (exact, plural) {
                (true, true) => "Exact frames",
                (true, false) => "Exact frame",
                (false, true) => "Partial frames",
                (false, false) => "Partial frame",
            };
            ui.horizontal(|ui| {
                widgets::pill(
                    ui,
                    theme,
                    badge,
                    if exact { Tone::Success } else { Tone::Warning },
                );
                ui.label(
                    RichText::new(format!("{} condition(s)", conditions.len()))
                        .size(13.5)
                        .color(palette.muted),
                );
            });
            ui.label(
                RichText::new(filter::format_conditions(&conditions))
                    .monospace()
                    .size(14.0)
                    .color(palette.text),
            );
        }
        Err(error) => {
            ui.label(
                RichText::new(error.to_string())
                    .size(13.5)
                    .color(palette.red),
            );
        }
    }
    ui.label(
        RichText::new(
            "The gateway only matches raw serial payload bytes; it does not interpret Z-Wave fields.",
        )
        .size(13.5)
        .color(palette.muted),
    );
}

fn raw_condition_editor(ui: &mut Ui, raw: &mut RawConditionDraft, theme: UiTheme) {
    let palette = crate::theme::palette(theme);
    ui.label(
        RichText::new(
            "Each match sequence is ANDed with the others. Use offset or Anywhere; masks are optional.",
        )
        .size(13.5)
        .color(palette.muted),
    );
    let mut remove = None;
    for (index, leaf) in raw.leaves.iter_mut().enumerate() {
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Match sequence {}", index + 1)).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Remove").clicked() {
                        remove = Some(index);
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut leaf.anywhere, "Anywhere");
                if !leaf.anywhere {
                    ui.label("Offset");
                    let mut offset = leaf.offset.trim().parse::<u32>().unwrap_or(0);
                    if ui
                        .add(DragValue::new(&mut offset).range(0..=u32::MAX))
                        .changed()
                    {
                        leaf.offset = offset.to_string();
                    }
                }
            });
            let width = ui.available_width().max(240.0);
            ui.label("Bytes");
            widgets::hex_input(
                ui,
                &mut leaf.bytes,
                "required hex, e.g. 01 03 — spaces and line breaks are ignored",
                true,
                3,
                width,
            );
            ui.label("Mask");
            widgets::hex_input(
                ui,
                &mut leaf.mask,
                "optional hex; empty = exact",
                true,
                2,
                width,
            );
            match leaf.preview() {
                Ok(preview) => {
                    ui.label(RichText::new(preview).monospace().size(14.0));
                }
                Err(_) if leaf.bytes.trim().is_empty() => {
                    ui.label(
                        RichText::new("Enter bytes to preview this match sequence.")
                            .size(13.5)
                            .color(palette.muted),
                    );
                }
                Err(error) => {
                    ui.label(RichText::new(error).size(13.5).color(palette.red));
                }
            }
            if leaf.mask.trim().is_empty() {
                ui.label(
                    RichText::new("Mask absent: all bytes are compared exactly.")
                        .size(13.0)
                        .color(palette.muted),
                );
            } else if leaf
                .mask
                .trim()
                .chars()
                .all(|character| character == '0' || character == ' ')
            {
                ui.label(
                    RichText::new("All-zero mask: effectively a length constraint.")
                        .size(13.0)
                        .color(palette.amber),
                );
            }
            for warning in leaf.warnings() {
                ui.label(RichText::new(warning).size(13.0).color(palette.amber));
            }
        });
    }
    if let Some(index) = remove {
        raw.leaves.remove(index);
    }
    if ui.button("Add match sequence").clicked() {
        raw.leaves.push(RawLeafDraft::default());
    }
    if raw.leaves.is_empty() {
        ui.label(
            RichText::new("Add at least one match sequence.")
                .size(13.5)
                .color(palette.red),
        );
    }
}
/// Renders one message in the selected view. Used by both the inline context
/// and the expanded modal so the two stay identical.
pub(crate) fn render_message(
    ui: &mut Ui,
    theme: UiTheme,
    services: &[ServiceInfo],
    message: &Message,
    view: MessageView,
    text_encoding: model::TextEncoding,
) -> Option<model::TextEncoding> {
    let palette = crate::theme::palette(theme);
    match view {
        MessageView::Summary => {
            ui.add_space(4.0);
            egui::Grid::new("message-summary-grid")
                .num_columns(2)
                .spacing([18.0, 5.0])
                .show(ui, |ui| {
                    summary_row(ui, &palette, "Time", &model::format_datetime(message.time));
                    summary_row(ui, &palette, "Message ID", &message.id.to_string());
                    summary_row(
                        ui,
                        &palette,
                        "Service (display)",
                        &model::display_service(services, &message.service),
                    );
                    summary_row(
                        ui,
                        &palette,
                        "Service (composite)",
                        &model::composite_key(&message.service),
                    );
                    summary_row(
                        ui,
                        &palette,
                        "State",
                        model::message_state_label(message.state),
                    );
                    summary_row(
                        ui,
                        &palette,
                        "Payload size",
                        &format!("{} bytes", message.payload.len()),
                    );
                    summary_row(
                        ui,
                        &palette,
                        "Decoded",
                        &model::message_summary(services, message),
                    );
                });
            ui.add_space(6.0);
            let preview = payload_preview(message.payload.as_slice(), 128);
            widgets::copy_field(ui, theme, "Payload", &preview, message.payload.len() > 24);
            None
        }
        MessageView::Hex => hex_view(ui, theme, message.payload.as_slice(), text_encoding),
        MessageView::Parsed => {
            let protocol = model::message_protocol(services, message);
            if protocol != Some("zwave") {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(match protocol {
                        Some(protocol) => format!(
                            "No protocol-specific parser for '{protocol}'. Showing raw bytes."
                        ),
                        None => "No protocol-specific parser for this service. Showing raw bytes."
                            .to_string(),
                    })
                    .color(palette.muted),
                );
                ui.add_space(4.0);
                return hex_view(ui, theme, message.payload.as_slice(), text_encoding);
            }
            let decoded = zwave::decode(message.payload.as_slice());
            let entity_keys = model::entity_service_keys(services, &message.service);
            let entities = entities::current();
            ui.add_space(4.0);
            if decoded.frames.is_empty() {
                ui.label(RichText::new("No Z-Wave frames recognised.").color(palette.muted));
            }
            const MAX_PARSED_FRAMES: usize = 64;
            for (index, frame) in decoded.frames.iter().take(MAX_PARSED_FRAMES).enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Frame {} · {}", index + 1, frame.title))
                            .strong()
                            .size(14.0),
                    );
                    match frame.checksum_ok {
                        Some(true) => {
                            ui.label(RichText::new("checksum OK").size(13.5).color(palette.green));
                        }
                        Some(false) => {
                            ui.label(RichText::new("bad checksum").size(13.5).color(palette.red));
                        }
                        None => {}
                    }
                });
                egui::Grid::new(("message-parsed-fields", index))
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for field in &frame.fields {
                            let kind = crate::theme::field_label(field.kind);
                            let label = if field.label.trim().is_empty() {
                                kind.to_string()
                            } else {
                                field.label.clone()
                            };
                            ui.label(
                                RichText::new(label)
                                    .strong()
                                    .size(14.0)
                                    .color(crate::theme::field_color(theme, field.kind)),
                            )
                            .on_hover_text(format!("{kind} field"));
                            let mut value = field.value.clone();
                            if field.kind == FieldKind::Node
                                && let Ok(node) = field.value.parse::<u8>()
                                && let Some(device) = entities.node_label(&entity_keys, node)
                            {
                                value = format!("{} — {}", field.value, device.display());
                            }
                            ui.label(RichText::new(value).monospace().size(14.0));
                            ui.end_row();
                        }
                    });
                ui.add_space(4.0);
            }
            if decoded.frames.len() > MAX_PARSED_FRAMES {
                ui.label(
                    RichText::new(format!(
                        "Showing the first {MAX_PARSED_FRAMES} of {} frames. Use Hex to inspect the full payload.",
                        decoded.frames.len()
                    ))
                    .size(13.5)
                    .color(palette.amber),
                );
            }
            if !decoded.issues.is_empty() {
                for issue in &decoded.issues {
                    ui.label(RichText::new(issue).size(13.5).color(palette.amber));
                }
            }
            if !decoded.frames.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new("Coloured bytes").strong());
                return colored_dump(
                    ui,
                    theme,
                    message.payload.as_slice(),
                    &decoded,
                    text_encoding,
                );
            }
            None
        }
        MessageView::Json => {
            ui.label(
                RichText::new("Payload is Base64 (RFC 4648) in this JSON representation.")
                    .size(13.5)
                    .color(palette.muted),
            );
            let json = serde_json::to_string_pretty(message)
                .unwrap_or_else(|error| format!("could not serialize message: {error}"));
            widgets::copy_field(ui, theme, "Message JSON", &json, true);
            None
        }
    }
}

fn summary_row(ui: &mut Ui, palette: &crate::theme::Palette, label: &str, value: &str) {
    ui.label(RichText::new(label).size(13.5).color(palette.muted));
    ui.label(RichText::new(value).monospace().size(14.0));
    ui.end_row();
}

fn payload_preview(payload: &[u8], limit: usize) -> String {
    if payload.len() <= limit {
        hex::spaced_hex(payload)
    } else {
        format!(
            "{} … ({} bytes; use Hex for the full payload)",
            hex::spaced_hex(&payload[..limit]),
            payload.len()
        )
    }
}

/// Renders the shared Offset/Hex header with the text-encoding picker placed
/// exactly at the start of the decoded-text column. The prefix width is
/// computed from the widest glyph used by any hex-column character so the
/// header stays aligned if the monospace font changes.
fn hex_header(
    ui: &mut Ui,
    theme: UiTheme,
    encoding: model::TextEncoding,
) -> Option<model::TextEncoding> {
    const PREFIX_CHARS: f32 = 60.0;
    let palette = crate::theme::palette(theme);
    let font = FontId::monospace(14.0);
    let glyph_width = ui.fonts_mut(|fonts| {
        "0123456789abcdef "
            .chars()
            .map(|character| fonts.glyph_width(&font, character))
            .fold(0.0_f32, f32::max)
    });
    let prefix_width = glyph_width * PREFIX_CHARS;
    let mut selected = encoding;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let height = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(
            egui::Vec2::new(prefix_width, height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    RichText::new(format!("{:<8}  {:<48}", "Offset", "Hex"))
                        .monospace()
                        .size(14.0)
                        .color(palette.muted),
                );
            },
        );
        ComboBox::from_id_salt("hex-text-encoding")
            .selected_text(RichText::new(selected.label()).monospace().size(14.0))
            .width(160.0)
            .show_ui(ui, |ui| {
                for candidate in model::TextEncoding::ALL {
                    ui.selectable_value(&mut selected, candidate, candidate.label());
                }
            })
            .response
            .on_hover_text("Text decoding for the right-hand column");
    });
    (selected != encoding).then_some(selected)
}

fn format_hex_16(chunk: &[u8]) -> String {
    let mut output = String::with_capacity(48);
    for (index, byte) in chunk.iter().enumerate() {
        if index > 0 {
            output.push(' ');
        }
        if index == 8 {
            output.push(' ');
        }
        output.push_str(&format!("{byte:02x}"));
    }
    while output.len() < 48 {
        output.push(' ');
    }
    output
}

fn hex_view(
    ui: &mut Ui,
    theme: UiTheme,
    payload: &[u8],
    encoding: model::TextEncoding,
) -> Option<model::TextEncoding> {
    let palette = crate::theme::palette(theme);
    let next = hex_header(ui, theme, encoding);
    let active = next.unwrap_or(encoding);
    if payload.is_empty() {
        ui.label(RichText::new("Empty payload").color(palette.muted));
        return next;
    }

    let rows = payload.len().div_ceil(16);
    egui::ScrollArea::vertical()
        .id_salt("message-hex-rows")
        .max_height(360.0)
        .auto_shrink([false, true])
        .show_rows(ui, 20.0, rows, |ui, range| {
            widgets::keyboard_scroll(ui, ui.rect_contains_pointer(ui.max_rect()));
            for row in range {
                let start = row * 16;
                let end = (start + 16).min(payload.len());
                let chunk = &payload[start..end];
                let text = active.decode_row(payload, start, end);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(
                        RichText::new(format!("{start:08x}  {}  ", format_hex_16(chunk)))
                            .monospace()
                            .size(14.0)
                            .color(palette.text),
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(text)
                                .monospace()
                                .size(14.0)
                                .color(palette.text),
                        )
                        .wrap_mode(egui::TextWrapMode::Extend),
                    );
                });
            }
        });
    next
}

fn colored_dump(
    ui: &mut Ui,
    theme: UiTheme,
    payload: &[u8],
    decoded: &DecodeResult,
    encoding: model::TextEncoding,
) -> Option<model::TextEncoding> {
    let palette = crate::theme::palette(theme);
    let next = hex_header(ui, theme, encoding);
    let active = next.unwrap_or(encoding);
    let mut colors: Vec<Option<Color32>> = vec![None; payload.len()];
    for frame in &decoded.frames {
        for field in &frame.fields {
            let color = crate::theme::field_color(theme, field.kind);
            for offset in field.range.clone() {
                if offset < colors.len() {
                    colors[offset] = Some(color);
                }
            }
        }
    }
    let font = FontId::monospace(14.0);
    let rows = payload.len().div_ceil(16);
    egui::ScrollArea::vertical()
        .id_salt("message-coloured-bytes")
        .max_height(320.0)
        .auto_shrink([false, true])
        .show_rows(ui, 20.0, rows, |ui, range| {
            widgets::keyboard_scroll(ui, ui.rect_contains_pointer(ui.max_rect()));
            for row in range {
                let start = row * 16;
                let end = (start + 16).min(payload.len());
                let chunk = &payload[start..end];
                let mut job = LayoutJob::default();
                job.append(
                    &format!("{start:08x}  "),
                    0.0,
                    egui::TextFormat {
                        font_id: font.clone(),
                        color: palette.muted,
                        ..Default::default()
                    },
                );
                for (column, byte) in chunk.iter().enumerate() {
                    let color = colors[start + column].unwrap_or(palette.text);
                    job.append(
                        &format!("{byte:02x}"),
                        0.0,
                        egui::TextFormat {
                            font_id: font.clone(),
                            color,
                            ..Default::default()
                        },
                    );
                    if column + 1 < chunk.len() {
                        job.append(
                            " ",
                            0.0,
                            egui::TextFormat {
                                font_id: font.clone(),
                                color: palette.text,
                                ..Default::default()
                            },
                        );
                    }
                    if column == 7 {
                        job.append(
                            " ",
                            0.0,
                            egui::TextFormat {
                                font_id: font.clone(),
                                color: palette.border,
                                ..Default::default()
                            },
                        );
                    }
                }
                for _ in chunk.len()..16 {
                    job.append(
                        "   ",
                        0.0,
                        egui::TextFormat {
                            font_id: font.clone(),
                            color: palette.text,
                            ..Default::default()
                        },
                    );
                }
                if chunk.len() < 8 {
                    job.append(
                        " ",
                        0.0,
                        egui::TextFormat {
                            font_id: font.clone(),
                            color: palette.border,
                            ..Default::default()
                        },
                    );
                }
                job.append(
                    "  ",
                    0.0,
                    egui::TextFormat {
                        font_id: font.clone(),
                        color: palette.border,
                        ..Default::default()
                    },
                );
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add(egui::Label::new(job).wrap_mode(egui::TextWrapMode::Extend));
                    ui.add(
                        egui::Label::new(
                            RichText::new(active.decode_row(payload, start, end))
                                .monospace()
                                .size(14.0)
                                .color(palette.muted),
                        )
                        .wrap_mode(egui::TextWrapMode::Extend),
                    );
                });
            }
        });
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_targets_move_from_selection_or_visible_start() {
        assert_eq!(
            nav_target(widgets::ListNav::Down, None, 5, 9),
            5,
            "first Down selects the first visible row"
        );
        assert_eq!(nav_target(widgets::ListNav::Down, Some(5), 0, 9), 6);
        assert_eq!(nav_target(widgets::ListNav::Up, Some(0), 0, 9), 0);
        assert_eq!(nav_target(widgets::ListNav::PageDown, Some(1), 0, 30), 21);
        assert_eq!(nav_target(widgets::ListNav::PageUp, Some(21), 0, 30), 1);
        assert_eq!(nav_target(widgets::ListNav::Home, Some(7), 0, 9), 0);
        assert_eq!(nav_target(widgets::ListNav::End, Some(7), 0, 9), 9);
        assert_eq!(
            nav_target(widgets::ListNav::Down, None, 99, 9),
            9,
            "stale visible start is clamped"
        );
    }

    #[test]
    fn large_payload_views_are_virtualized() {
        let payload = vec![b'A'; 100_000];
        let decoded = zwave::decode(&payload);
        egui::__run_test_ui(|ui| {
            hex_view(ui, UiTheme::Dark, &payload, model::TextEncoding::Ascii);
            colored_dump(
                ui,
                UiTheme::Dark,
                &payload,
                &decoded,
                model::TextEncoding::Ascii,
            );
        });
        assert!(payload_preview(&payload, 128).contains("100000 bytes"));
    }
}
