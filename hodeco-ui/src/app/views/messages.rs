//! Messages tab: store-wide find, filtered local list, and the shared viewer.

use super::super::{
    App, ConditionEditor, FilterDialogMode, FilterDraft, MessageFiltersDialog, MessageView,
    PayloadConditionDraft, RawConditionDraft, RawLeafDraft, SearchDirection, ZigbeeConditionDraft,
    ZigbeeFieldMatch, parse_hex_u16, zcl_field_spans,
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
use hodeco_codec::zigbee;
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
                metadata_editor(app, ui, &mut draft, &protocols, &transports, &services);
                ui.add_space(8.0);
                ui.separator();
                payload_conditions_list(app, ui, &mut draft, &mut dialog.editor);
            });
            widgets::dialog_footer(ui, theme, draft.error.as_deref(), |ui| {
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
                if ui.add_enabled(!over, egui::Button::new("Apply")).clicked() {
                    apply = true;
                }
            });
            false
        });

    payload_condition_modal(app, ctx, theme, &mut dialog.editor, &mut draft);

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

pub(crate) fn payload_conditions_list(
    app: &App,
    ui: &mut Ui,
    draft: &mut FilterDraft,
    editor: &mut Option<ConditionEditor>,
) {
    let palette = app.palette();
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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Remove").clicked() {
                        remove = Some(index);
                    }
                    if ui.small_button("Edit").clicked() {
                        edit = Some(index);
                    }
                });
            });
        });
    }
    if let Some(index) = remove {
        draft.payload_conditions.remove(index);
    }
    if let Some(index) = edit {
        let condition = draft.payload_conditions[index].clone();
        *editor = Some(ConditionEditor {
            index: Some(index),
            draft: condition,
            error: None,
        });
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("Add Z-Wave frame...").clicked() {
            *editor = Some(ConditionEditor {
                index: None,
                draft: PayloadConditionDraft::Frame(default_frame_condition()),
                error: None,
            });
        }
        if ui.button("Add ZigBee frame...").clicked() {
            *editor = Some(ConditionEditor {
                index: None,
                draft: PayloadConditionDraft::Zigbee(ZigbeeConditionDraft::default()),
                error: None,
            });
        }
        if ui.button("Add raw bytes...").clicked() {
            *editor = Some(ConditionEditor {
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
}

pub(crate) fn payload_condition_modal(
    app: &App,
    ctx: &egui::Context,
    theme: UiTheme,
    editor: &mut Option<ConditionEditor>,
    draft: &mut FilterDraft,
) {
    let Some(mut current) = editor.take() else {
        return;
    };
    let editor_title = match &current.draft {
        PayloadConditionDraft::Frame(_) => "Z-Wave frame condition",
        PayloadConditionDraft::Zigbee(_) => "ZigBee frame condition",
        PayloadConditionDraft::Raw(_) => "Raw bytes condition",
    };
    let mut save = false;
    let mut close = false;
    let editor_modal = egui::Modal::new(Id::unique("hodeco-payload-condition"))
        .frame(widgets::modal_frame(theme))
        .show(ctx, |ui| {
            widgets::dialog_width(ui, 860.0);
            widgets::dialog_header(ui, theme, editor_title, None);
            widgets::dialog_body(ui, 540.0, |ui| match &mut current.draft {
                PayloadConditionDraft::Frame(spec) => {
                    frame_condition_editor(app, ui, spec, &draft.services)
                }
                PayloadConditionDraft::Zigbee(spec) => {
                    zigbee_condition_editor(app, ui, spec, &draft.services, theme)
                }
                PayloadConditionDraft::Raw(raw) => raw_condition_editor(ui, raw, theme),
            });
            widgets::dialog_footer(ui, theme, current.error.as_deref(), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                let label = if current.index.is_some() {
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
        match current.draft.to_expr() {
            Ok(_) => {
                if let Some(index) = current.index {
                    if index < draft.payload_conditions.len() {
                        draft.payload_conditions[index] = current.draft;
                    }
                } else {
                    draft.payload_conditions.push(current.draft);
                }
            }
            Err(error) => {
                current.error = Some(error);
                *editor = Some(current);
            }
        }
    } else if !(close || editor_modal.should_close()) {
        *editor = Some(current);
    }
}

fn metadata_editor(
    app: &mut App,
    ui: &mut Ui,
    draft: &mut FilterDraft,
    protocols: &[(String, String)],
    transports: &[(String, String)],
    services: &[(ServiceRef, String)],
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
}

fn match_checkbox(ui: &mut Ui, include: &mut bool, enabled: bool) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // Keep the checkbox clear of the dialog scrollbar.
        ui.add_space(14.0);
        ui.add_enabled(enabled, egui::Checkbox::new(include, "Match in filter"))
            .on_hover_text(
                "Include this field in the condition. Unchecked keeps the selected value \
                 but does not match it.",
            );
    });
}

fn default_frame_condition() -> filter::FilterSpec {
    filter::FilterSpec {
        command_class: Some(hodeco_codec::builder::CC_BASIC),
        command: Some(hodeco_codec::builder::BASIC_SET),
        ..filter::FilterSpec::default()
    }
}

fn frame_condition_editor(
    app: &App,
    ui: &mut Ui,
    spec: &mut filter::FilterSpec,
    services: &[ServiceRef],
) {
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
        let known = zwave_known_nodes(app, services);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Node(s)").strong());
            if spec.frame == FrameKind::Broadcast {
                ui.label(RichText::new("0xFF (fixed)").monospace());
                spec.includes.node = true;
                match_checkbox(ui, &mut spec.includes.node, false);
            } else {
                let multi = matches!(spec.frame, FrameKind::SendData | FrameKind::SendDataMulti);
                let mut text = spec.nodes_text.clone();
                let mut changed = widgets::numeric_input(ui, &mut text, "e.g. 2, 7", 150.0, true);
                let current = text.trim().parse::<u8>().ok();
                if let Some(node) = known_node_picker(ui, &known, current, multi) {
                    if multi {
                        let mut nodes = model::parse_node_list(&text).unwrap_or_default();
                        if !nodes.contains(&node) {
                            nodes.push(node);
                            nodes.sort_unstable();
                        }
                        text = nodes
                            .iter()
                            .map(u8::to_string)
                            .collect::<Vec<_>>()
                            .join(", ");
                    } else {
                        text = node.to_string();
                    }
                    changed = true;
                }
                if changed {
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
                .map(|class| (class.id, command_class_label(class)))
                .collect();
            let selected_class = spec
                .command_class
                .and_then(|id| definitions.command_class(id));
            let selected_text = match selected_class {
                Some(class) => command_class_label(class),
                None => "Select a command class".to_string(),
            };
            let mut class_choice = spec.command_class;
            widgets::searchable_combo(
                ui,
                "condition-command-class",
                selected_text,
                320.0,
                &mut class_choice,
                widgets::ComboOptions {
                    any_label: None,
                    emphasize: &[],
                },
                &class_items,
            );
            if class_choice != spec.command_class {
                spec.command_class = class_choice;
                spec.command = None;
                spec.command_fields.clear();
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
            let mut command_choice = spec.command;
            if command_items.is_empty() {
                let mut text = command_choice
                    .map(|command| command.to_string())
                    .unwrap_or_default();
                if widgets::numeric_input(ui, &mut text, "command ID, e.g. 1", 140.0, false) {
                    command_choice = text.trim().parse::<u8>().ok();
                }
            } else {
                let selected_text = match class.and_then(|class| {
                    spec.command
                        .and_then(|id| class.commands.iter().find(|command| command.id == id))
                }) {
                    Some(command) => format!("{} (0x{:02x})", command.name, command.id),
                    None => "Select a command".to_string(),
                };
                widgets::searchable_combo(
                    ui,
                    "condition-command",
                    selected_text,
                    320.0,
                    &mut command_choice,
                    widgets::ComboOptions {
                        any_label: None,
                        emphasize: &[],
                    },
                    &command_items,
                );
            }
            if command_choice != spec.command {
                spec.command = command_choice;
                spec.command_fields.clear();
                if command_choice.is_some() {
                    spec.includes.command = true;
                    spec.includes.command_class = true;
                } else {
                    spec.includes.command = false;
                }
            }
            match_checkbox(ui, &mut spec.includes.command, spec.command.is_some());
        });
        if class.is_some_and(|class| class.commands.is_empty()) {
            ui.label(
                RichText::new(
                    "No named commands are defined for this class. Enter the command ID \
                     and put the parameters in Command data.",
                )
                .size(13.5)
                .color(palette.muted),
            );
        }

        let mut command_field_error = None;
        let command_field_spans = match spec.command_field_spans() {
            Ok(spans) => spans,
            Err(error) => {
                command_field_error = Some(error.to_string());
                Vec::new()
            }
        };
        if !command_field_spans.is_empty() {
            ui.add_space(6.0);
            let typed_enabled = spec.command_fields.values().any(|field| field.enabled);
            egui::CollapsingHeader::new(RichText::new("Command fields (optional)").strong())
                .default_open(typed_enabled)
                .show(ui, |ui| {
                    egui::Grid::new(("condition-command-fields", spec.command_class, spec.command))
                        .num_columns(3)
                        .spacing([10.0, 5.0])
                        .show(ui, |ui| {
                            for span in &command_field_spans {
                                let entry = spec
                                    .command_fields
                                    .entry(span.field.name.clone())
                                    .or_insert_with(|| default_command_field_match(&span.field));
                                ui.checkbox(&mut entry.enabled, "");
                                ui.label(format!(
                                    "{}{}",
                                    span.field.label,
                                    zcl_match_suffix(&span.field)
                                ));
                                command_field_widget(ui, &span.field, &mut entry.value);
                                ui.end_row();
                            }
                        });
                    ui.label(
                        RichText::new(
                            "Fixed-width fields are matched here; use extra command data \
                             for variable-length or vendor bytes.",
                        )
                        .size(13.5)
                        .color(palette.muted),
                    );
                });
        }
        if let Some(error) = command_field_error {
            ui.label(RichText::new(error).size(13.5).color(palette.red));
        }

        let typed_enabled = spec.command_fields.values().any(|field| field.enabled);
        if typed_enabled {
            spec.includes.command_data = true;
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(if typed_enabled {
                    "Extra command data"
                } else {
                    "Command data"
                })
                .strong(),
            );
            let mut data_text = spec.command_data_hex.clone();
            if widgets::hex_input(
                ui,
                &mut data_text,
                if typed_enabled {
                    "bytes appended after typed command fields"
                } else {
                    "bytes after the command, e.g. 63"
                },
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
                !spec.command_data_hex.trim().is_empty() || typed_enabled,
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
        if typed_enabled || !spec.command_data_hex.trim().is_empty() {
            match spec.command_payload() {
                Ok(payload) => {
                    ui.label(
                        RichText::new(format!("Command payload: {}", hex::spaced_hex(&payload)))
                            .monospace()
                            .size(13.0)
                            .color(palette.muted),
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
        }
    }

    if spec.frame.uses_tx_options() {
        ui.add_space(6.0);
        if spec.frame == FrameKind::Broadcast {
            spec.tx_options = hodeco_codec::builder::BROADCAST_TX_OPTIONS;
            ui.horizontal(|ui| {
                ui.label(RichText::new("TX options").strong());
                ui.label(
                    RichText::new(format!(
                        "0x{:02x} (fixed)",
                        hodeco_codec::builder::BROADCAST_TX_OPTIONS
                    ))
                    .monospace(),
                );
                match_checkbox(ui, &mut spec.includes.tx_options, true);
            });
        } else {
            let options = zwave_tx_option_values(&definitions, spec.frame);
            ui.horizontal(|ui| {
                ui.label(RichText::new("TX options").strong());
                let mut value = spec.tx_options;
                let mut changed = false;
                for (bit, name) in &options {
                    let mut enabled = value & bit != 0;
                    if ui.checkbox(&mut enabled, name).changed() {
                        if enabled {
                            value |= bit;
                        } else {
                            value &= !bit;
                        }
                        changed = true;
                    }
                }
                if changed {
                    spec.tx_options = value;
                }
                if options.is_empty() {
                    ui.add(DragValue::new(&mut spec.tx_options).range(0..=u8::MAX));
                }
                match_checkbox(ui, &mut spec.includes.tx_options, true);
            });
        }
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
            if let Some(field) =
                zwave_frame_field(&definitions, spec.frame, definitions::FieldRole::RxStatus)
                && field.kind == definitions::FieldType::Flags
                && !field.values.is_empty()
            {
                zwave_flags_widget(ui, field, &mut spec.rx_status);
            } else {
                ui.add(DragValue::new(&mut spec.rx_status).range(0..=u8::MAX));
            }
            match_checkbox(ui, &mut spec.includes.rx_status, true);
        });
    }
    if spec.frame.uses_tx_status() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("TX status").strong());
            if let Some(field) =
                zwave_frame_field(&definitions, spec.frame, definitions::FieldRole::TxStatus)
                && field.kind == definitions::FieldType::Enum
                && !field.values.is_empty()
            {
                let selected = field
                    .values
                    .iter()
                    .find(|item| item.value == spec.tx_status)
                    .map(|item| format!("{} (0x{:02x})", item.name, item.value))
                    .unwrap_or_else(|| format!("0x{:02x}", spec.tx_status));
                ComboBox::from_id_salt("condition-tx-status")
                    .selected_text(selected)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for item in &field.values {
                            ui.selectable_value(
                                &mut spec.tx_status,
                                item.value,
                                format!("{} (0x{:02x})", item.name, item.value),
                            );
                        }
                    });
            } else {
                ui.add(DragValue::new(&mut spec.tx_status).range(0..=u8::MAX));
            }
            match_checkbox(ui, &mut spec.includes.tx_status, true);
        });
    }

    ui.add_space(8.0);
    ui.separator();
    frame_structural(ui, spec, palette);
    frame_preview(ui, app.theme, spec);
}

fn zwave_tx_option_values(
    definitions: &definitions::ZWaveDefinitions,
    frame: FrameKind,
) -> Vec<(u8, String)> {
    frame
        .serial_command()
        .and_then(|id| definitions.serial_command(id))
        .and_then(|command| {
            command
                .fields
                .iter()
                .flat_map(definitions::LayoutField::fields)
                .find(|field| field.kind == definitions::FieldType::TxOptions)
        })
        .map(|field| {
            field
                .values
                .iter()
                .map(|value| (value.value, value.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn zwave_frame_field(
    definitions: &definitions::ZWaveDefinitions,
    frame: FrameKind,
    role: definitions::FieldRole,
) -> Option<&definitions::FieldDef> {
    let command = definitions.serial_command(frame.serial_command()?)?;
    let fields = if frame == FrameKind::SendDataCallback {
        command.callback.as_ref()?.fields.as_slice()
    } else {
        command.fields.as_slice()
    };
    fields
        .iter()
        .flat_map(definitions::LayoutField::fields)
        .find(|field| field.role == role)
}

fn zwave_flags_widget(ui: &mut Ui, field: &definitions::FieldDef, value: &mut u8) {
    ui.horizontal_wrapped(|ui| {
        for item in &field.values {
            let mut enabled = *value & item.value != 0;
            if ui.checkbox(&mut enabled, &item.name).changed() {
                if enabled {
                    *value |= item.value;
                } else {
                    *value &= !item.value;
                }
            }
        }
    });
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

fn zigbee_condition_editor(
    app: &App,
    ui: &mut Ui,
    spec: &mut ZigbeeConditionDraft,
    services: &[ServiceRef],
    theme: UiTheme,
) {
    let palette = crate::theme::palette(theme);
    ui.label(
        RichText::new(
            "Match the EZSP envelope, the ZCL frame, and typed ZCL fields. \
             Blank fields are wildcards; raw bytes remain available.",
        )
        .size(13.5)
        .color(palette.muted),
    );
    ui.add_space(6.0);
    let definitions = zigbee::current();
    let zigbee_services = zigbee_filter_services(app, services);
    let entity_keys: Vec<String> = zigbee_services
        .iter()
        .flat_map(|service| model::entity_service_keys(&app.services, service))
        .collect();
    let devices = zigbee_known_devices(&entity_keys);

    let items: Vec<(u16, String)> = definitions
        .commands
        .iter()
        .map(|command| {
            (
                command.id,
                format!("{} (0x{:04x})", command.name, command.id),
            )
        })
        .collect();
    ui.horizontal(|ui| {
        ui.label(RichText::new("EZSP command").strong());
        let previous = spec.command;
        let mut choice = spec.command;
        widgets::searchable_combo(
            ui,
            "zigbee-condition-command",
            spec.command
                .and_then(|id| definitions.command(id))
                .map(|command| format!("{} (0x{:04x})", command.name, command.id))
                .unwrap_or_else(|| "Any command".to_string()),
            460.0,
            &mut choice,
            widgets::ComboOptions {
                any_label: Some("Any command"),
                emphasize: &[],
            },
            &items,
        );
        if choice != previous {
            spec.command = choice;
            spec.ezsp_fields.clear();
            spec.ezsp_response = false;
            // Clear fields that no longer have a fixed location in the frame.
            if !spec.supports_network() {
                spec.network.clear();
            }
            if !spec.supports_group() {
                spec.group.clear();
            }
            if !spec.supports_cluster() {
                spec.cluster.clear();
            }
            if !spec.supports_endpoint() {
                spec.endpoint.clear();
            }
            if !spec.supports_zcl() {
                spec.zcl_general = false;
                spec.zcl_command = None;
                spec.zcl_fields.clear();
                spec.zcl_payload.clear();
            }
        }
    });
    if let Some(command) = spec.command.and_then(|id| definitions.command(id))
        && let Some(description) = &command.description
    {
        ui.label(RichText::new(description).size(13.0).color(palette.muted));
    }
    let has_network = spec.supports_network();
    let has_group = spec.supports_group();
    let has_cluster = spec.supports_cluster();
    let has_endpoint = spec.supports_endpoint();
    let has_zcl = spec.supports_zcl();
    if !has_network {
        spec.network.clear();
    }
    if !has_group {
        spec.group.clear();
    }
    if !has_cluster {
        spec.cluster.clear();
    }
    if !has_endpoint {
        spec.endpoint.clear();
    }
    if !has_zcl {
        spec.zcl_general = false;
        spec.zcl_command = None;
        spec.zcl_fields.clear();
        spec.zcl_payload.clear();
    }

    ui.add_space(4.0);
    let network_label = spec
        .command
        .and_then(|id| {
            definitions.field_layout_by_filter_slot(id, definitions::FilterSlot::Network)
        })
        .map(|(_, field)| field.label.clone())
        .unwrap_or_else(|| "Network address".to_string());
    let group_label = spec
        .command
        .and_then(|id| definitions.field_layout_by_filter_slot(id, definitions::FilterSlot::Group))
        .map(|(_, field)| field.label.clone())
        .unwrap_or_else(|| "Group ID".to_string());
    let group_description = spec
        .command
        .and_then(|id| definitions.field_layout_by_filter_slot(id, definitions::FilterSlot::Group))
        .and_then(|(_, field)| field.description.clone());
    let endpoint_label = spec
        .command
        .and_then(|id| {
            definitions.field_layout_by_filter_slot(id, definitions::FilterSlot::Endpoint)
        })
        .map(|(_, field)| field.label.clone())
        .unwrap_or_else(|| "Endpoint".to_string());
    let cluster_label = spec
        .command
        .and_then(|id| {
            definitions.field_layout_by_filter_slot(id, definitions::FilterSlot::Cluster)
        })
        .map(|(_, field)| field.label.clone())
        .unwrap_or_else(|| "Cluster ID".to_string());
    ui.horizontal(|ui| {
        ui.add_enabled_ui(has_network, |ui| {
            ui.label(network_label);
            widgets::hex_input(ui, &mut spec.network, "e.g. 1234", true, 2, 160.0);
            let mut chosen = None;
            ComboBox::from_id_salt("zigbee-condition-device")
                .selected_text("Known device")
                .width(200.0)
                .show_ui(ui, |ui| {
                    if devices.is_empty() {
                        ui.label(
                            RichText::new("No devices in the entity catalog")
                                .size(13.5)
                                .color(palette.muted),
                        );
                        return;
                    }
                    for (nwk, endpoint, label) in &devices {
                        if ui
                            .selectable_label(false, RichText::new(label).monospace())
                            .clicked()
                        {
                            chosen = Some((*nwk, *endpoint));
                        }
                    }
                });
            if let Some((nwk, endpoint)) = chosen {
                spec.network = format!("{nwk:04x}");
                if has_endpoint && let Some(endpoint) = endpoint {
                    spec.endpoint = format!("{endpoint:02x}");
                }
            }
        });
    });
    if has_network {
        let entity_catalog = entities::current();
        if let Some(nwk) = parse_hex_u16(&spec.network)
            && let Some(device) = entity_catalog.network_device(&entity_keys, nwk)
        {
            ui.label(
                RichText::new(device.display())
                    .monospace()
                    .size(13.0)
                    .color(palette.muted),
            );
        }
    }
    ui.add_enabled_ui(has_group, |ui| {
        ui.horizontal(|ui| {
            ui.label(group_label);
            widgets::hex_input(ui, &mut spec.group, "e.g. 0001", true, 2, 160.0);
            if let Some(description) = &group_description {
                ui.label(RichText::new(description).size(13.0).color(palette.muted));
            }
        });
    });
    ui.horizontal(|ui| {
        ui.add_enabled_ui(has_endpoint, |ui| {
            ui.label(endpoint_label);
            widgets::hex_input(ui, &mut spec.endpoint, "e.g. 01", true, 1, 120.0);
        });
        ui.add_space(12.0);
        ui.add_enabled_ui(has_cluster, |ui| {
            ui.label(cluster_label);
            widgets::hex_input(ui, &mut spec.cluster, "e.g. 0006", true, 2, 160.0);
            let mut chosen_cluster = None;
            ComboBox::from_id_salt("zigbee-condition-cluster")
                .selected_text("Known cluster")
                .width(160.0)
                .show_ui(ui, |ui| {
                    for cluster in &definitions.clusters {
                        if ui
                            .selectable_label(
                                parse_hex_u16(&spec.cluster) == Some(cluster.id),
                                format!("{} (0x{:04x})", cluster.name, cluster.id),
                            )
                            .clicked()
                        {
                            chosen_cluster = Some(cluster.id);
                        }
                    }
                });
            if let Some(cluster_id) = chosen_cluster {
                spec.cluster = format!("{cluster_id:04x}");
                if !spec.zcl_general {
                    spec.zcl_command = None;
                    spec.zcl_fields.clear();
                }
            }
            if let Some(cluster) = parse_hex_u16(&spec.cluster)
                && let Some(cluster) = definitions.cluster(cluster)
            {
                ui.label(RichText::new(&cluster.name).size(13.0).color(palette.muted));
            }
        });
    });
    ui.horizontal(|ui| {
        ui.label("EZSP parameters");
        widgets::hex_input(
            ui,
            &mut spec.params,
            "optional bytes after the EZSP command ID",
            true,
            0,
            f32::INFINITY,
        );
    });

    let can_choose_response = spec
        .command
        .and_then(|id| definitions.command(id))
        .is_some_and(|command| {
            !command.incoming_only
                && !command.fields.is_empty()
                && !command.response_fields.is_empty()
        });
    if !can_choose_response {
        spec.ezsp_response = false;
    }
    let mut extra_fields = spec.extra_field_candidates();
    if can_choose_response || !extra_fields.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new("Other EZSP fields").strong());
        if can_choose_response
            && ui
                .checkbox(&mut spec.ezsp_response, "Response fields")
                .changed()
        {
            spec.ezsp_fields.clear();
            extra_fields = spec.extra_field_candidates();
        }
        if !extra_fields.is_empty() {
            egui::Grid::new((
                "zigbee-condition-ezsp-fields",
                spec.command,
                spec.ezsp_response,
            ))
            .num_columns(3)
            .spacing([10.0, 5.0])
            .show(ui, |ui| {
                for field in extra_fields {
                    let entry = spec
                        .ezsp_fields
                        .entry(field.name.clone())
                        .or_insert_with(|| default_zcl_match(&field));
                    ui.checkbox(&mut entry.enabled, "");
                    ui.label(format!("{}{}", field.label, zcl_match_suffix(&field)));
                    ui.add(
                        TextEdit::singleline(&mut entry.value)
                            .hint_text(zcl_match_hint(&field))
                            .desired_width(220.0),
                    );
                    ui.end_row();
                }
            });
        }
    }

    if has_zcl {
        ui.add_space(8.0);
        ui.separator();
        ui.label(RichText::new("ZCL frame").strong());
        let mut direction_changed = false;
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut spec.zcl_general, "General command")
                .changed()
            {
                spec.zcl_command = None;
                spec.zcl_fields.clear();
            }
            direction_changed = ui
                .checkbox(&mut spec.zcl_direction_to_client, "Server → client")
                .changed();
            ui.checkbox(
                &mut spec.zcl_disable_default_response,
                "Disable default response",
            );
            ui.label("Manufacturer");
            widgets::hex_input(ui, &mut spec.zcl_manufacturer, "optional", false, 2, 100.0);
        });
        if direction_changed {
            spec.zcl_fields.clear();
        }

        if spec.zcl_general {
            if spec.zcl_command.is_some_and(|id| {
                !definitions.general_commands.iter().any(|command| {
                    command.id == id && command.direction.matches(spec.zcl_direction_to_client)
                })
            }) {
                spec.zcl_command = None;
                spec.zcl_fields.clear();
            }
            let general: Vec<(u8, String)> = definitions
                .general_commands
                .iter()
                .filter(|command| command.direction.matches(spec.zcl_direction_to_client))
                .map(|command| {
                    (
                        command.id,
                        format!("{} (0x{:02x})", command.name, command.id),
                    )
                })
                .collect();
            ui.horizontal(|ui| {
                ui.label(RichText::new("ZCL command").strong());
                let mut choice = spec.zcl_command;
                widgets::searchable_combo(
                    ui,
                    "zigbee-condition-general-command",
                    spec.zcl_command
                        .and_then(|id| definitions.general_command(id))
                        .map(|command| format!("{} (0x{:02x})", command.name, command.id))
                        .unwrap_or_else(|| "Any general command".to_string()),
                    420.0,
                    &mut choice,
                    widgets::ComboOptions {
                        any_label: Some("Any general command"),
                        emphasize: &[],
                    },
                    &general,
                );
                spec.zcl_command = choice;
            });
            if let Some(command) = spec
                .zcl_command
                .and_then(|id| definitions.general_command(id))
                && let Some(description) = &command.description
            {
                ui.label(RichText::new(description).size(13.0).color(palette.muted));
            }
        } else if let Some(cluster_id) = parse_hex_u16(&spec.cluster)
            && let Some(cluster) = definitions.cluster(cluster_id)
        {
            if spec
                .zcl_command
                .is_some_and(|id| !cluster.commands.iter().any(|command| command.id == id))
            {
                spec.zcl_command = None;
                spec.zcl_fields.clear();
            }
            let commands: Vec<(u8, String)> = cluster
                .commands
                .iter()
                .map(|command| {
                    (
                        command.id,
                        format!("{} (0x{:02x})", command.name, command.id),
                    )
                })
                .collect();
            ui.horizontal(|ui| {
                ui.label(RichText::new("ZCL command").strong());
                let previous = spec.zcl_command;
                let mut choice = spec.zcl_command;
                widgets::searchable_combo(
                    ui,
                    "zigbee-condition-zcl-command",
                    spec.zcl_command
                        .and_then(|id| {
                            cluster
                                .commands
                                .iter()
                                .find(|command| command.id == id)
                                .map(|command| format!("{} (0x{:02x})", command.name, command.id))
                        })
                        .unwrap_or_else(|| "Any cluster command".to_string()),
                    420.0,
                    &mut choice,
                    widgets::ComboOptions {
                        any_label: Some("Any cluster command"),
                        emphasize: &[],
                    },
                    &commands,
                );
                if choice != previous {
                    spec.zcl_command = choice;
                    spec.zcl_fields.clear();
                    if let Some(command_id) = choice
                        && let Some(command) =
                            cluster.commands.iter().find(|item| item.id == command_id)
                    {
                        let layout = if spec.zcl_direction_to_client {
                            &command.response_fields
                        } else {
                            &command.fields
                        };
                        for field in zcl_field_spans(layout)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|(field, _, _)| field)
                        {
                            spec.zcl_fields
                                .insert(field.name.clone(), default_zcl_match(&field));
                        }
                    }
                }
            });
            if let Some(command_id) = spec.zcl_command
                && let Some(command) = cluster.commands.iter().find(|item| item.id == command_id)
            {
                if let Some(description) = &command.description {
                    ui.label(RichText::new(description).size(13.0).color(palette.muted));
                }
                let layout = if spec.zcl_direction_to_client {
                    &command.response_fields
                } else {
                    &command.fields
                };
                if let Some(spans) = zcl_field_spans(layout) {
                    ui.add_space(4.0);
                    egui::Grid::new("zigbee-condition-zcl-fields")
                        .num_columns(3)
                        .spacing([10.0, 5.0])
                        .show(ui, |ui| {
                            for (field, _, _) in spans {
                                let entry = spec
                                    .zcl_fields
                                    .entry(field.name.clone())
                                    .or_insert_with(|| default_zcl_match(&field));
                                ui.checkbox(&mut entry.enabled, "");
                                ui.label(format!("{}{}", field.label, zcl_match_suffix(&field)));
                                ui.add(
                                    TextEdit::singleline(&mut entry.value)
                                        .hint_text(zcl_match_hint(&field))
                                        .desired_width(220.0),
                                );
                                ui.end_row();
                            }
                        });
                } else {
                    ui.label(
                        RichText::new(
                            "This command has variable-length fields; use the raw ZCL payload.",
                        )
                        .size(13.5)
                        .color(palette.muted),
                    );
                }
            }
        } else if !spec.cluster.trim().is_empty() {
            ui.label(
                RichText::new("Select a known cluster ID to list its ZCL commands.")
                    .size(13.5)
                    .color(palette.muted),
            );
        }

        let typed_enabled = spec.zcl_fields.values().any(|field| field.enabled);
        ui.horizontal(|ui| {
            ui.label(if typed_enabled {
                "Extra ZCL payload"
            } else if spec.zcl_command.is_some() {
                "ZCL command payload"
            } else {
                "Raw ZCL frame"
            });
            widgets::hex_input(
                ui,
                &mut spec.zcl_payload,
                if typed_enabled {
                    "bytes appended after the typed payload"
                } else if spec.zcl_command.is_some() {
                    "bytes after the ZCL command ID"
                } else {
                    "bytes from the ZCL frame-control byte"
                },
                true,
                0,
                f32::INFINITY,
            );
        });
    }

    if let Err(error) = spec.to_expr() {
        ui.add_space(4.0);
        ui.label(RichText::new(error).size(13.5).color(palette.red));
    }
}

pub(crate) fn service_is_zwave(app: &App, reference: &ServiceRef) -> bool {
    if let Some(service) = model::find_service(&app.services, reference) {
        return service.protocol == "zwave";
    }
    matches!(reference, ServiceRef::Key { protocol, .. } if protocol == "zwave")
}

pub(crate) fn zwave_known_nodes(app: &App, selected: &[ServiceRef]) -> Vec<(u8, String)> {
    let mut services: Vec<ServiceRef> = selected
        .iter()
        .filter(|reference| service_is_zwave(app, reference))
        .cloned()
        .collect();
    if services.is_empty() {
        services = app
            .services
            .iter()
            .filter(|service| service.protocol == "zwave")
            .map(model::service_ref)
            .collect();
    }
    let keys: Vec<String> = services
        .iter()
        .flat_map(|reference| model::entity_service_keys(&app.services, reference))
        .collect();
    let mut nodes: Vec<(u8, String)> = entities::current()
        .service_devices(&keys)
        .into_iter()
        .filter_map(|(service_device, device)| {
            service_device
                .service_id
                .as_node_id()
                .map(|node| (node, format!("{node:>3}  {}", device.display())))
        })
        .collect();
    nodes.sort_by_key(|(node, _)| *node);
    nodes.dedup_by_key(|(node, _)| *node);
    nodes
}

fn known_node_picker(
    ui: &mut Ui,
    known: &[(u8, String)],
    current: Option<u8>,
    multi: bool,
) -> Option<u8> {
    let selected_text = if multi {
        "Add known node".to_string()
    } else {
        current
            .and_then(|node| {
                known
                    .iter()
                    .find(|(known, _)| *known == node)
                    .map(|(_, label)| label.clone())
            })
            .unwrap_or_else(|| "Known node".to_string())
    };
    let mut choice = None;
    ComboBox::from_id_salt("condition-known-node")
        .selected_text(selected_text)
        .width(260.0)
        .show_ui(ui, |ui| {
            if known.is_empty() {
                ui.label(
                    RichText::new("No known Z-Wave nodes").color(ui.visuals().weak_text_color()),
                );
                return;
            }
            for (node, label) in known {
                if ui.selectable_label(current == Some(*node), label).clicked() {
                    choice = Some(*node);
                }
            }
        });
    choice
}

pub(crate) fn zigbee_filter_services(app: &App, selected: &[ServiceRef]) -> Vec<ServiceRef> {
    let mut services: Vec<ServiceRef> = selected
        .iter()
        .filter(|reference| service_is_zigbee(app, reference))
        .cloned()
        .collect();
    if services.is_empty() {
        services = app
            .services
            .iter()
            .filter(|service| service.protocol == "zigbee")
            .map(model::service_ref)
            .collect();
    }
    services
}

fn service_is_zigbee(app: &App, reference: &ServiceRef) -> bool {
    if let Some(service) = model::find_service(&app.services, reference) {
        return service.protocol == "zigbee";
    }
    matches!(reference, ServiceRef::Key { protocol, .. } if protocol == "zigbee")
}

pub(crate) fn zigbee_known_devices(keys: &[String]) -> Vec<(u16, Option<u8>, String)> {
    let catalog = entities::current();
    let mut devices: Vec<(u16, Option<u8>, String)> = catalog
        .service_devices(keys)
        .into_iter()
        .filter_map(|(service_device, device)| {
            let nwk = service_device.network_address()?;
            Some((
                nwk,
                service_device.endpoint,
                format!("0x{nwk:04x}  {}", device.display()),
            ))
        })
        .collect();
    devices.sort_by_key(|(nwk, endpoint, _)| (*nwk, *endpoint));
    devices.dedup_by_key(|(nwk, endpoint, _)| (*nwk, *endpoint));
    devices
}

fn command_class_label(class: &definitions::CommandClassDef) -> String {
    if class.commands.is_empty() {
        format!("{} (0x{:02x}) · raw only", class.name, class.id)
    } else {
        format!("{} (0x{:02x})", class.name, class.id)
    }
}

fn default_command_field_match(field: &definitions::FieldDef) -> filter::CommandFieldMatch {
    let value = if let Some(default) = &field.default_value {
        default.clone()
    } else {
        match field.kind {
            definitions::FieldType::Enum => field
                .values
                .first()
                .map(|value| value.value.to_string())
                .unwrap_or_else(|| "0".to_string()),
            definitions::FieldType::Bool => "false".to_string(),
            definitions::FieldType::Hex
            | definitions::FieldType::CommandClassData
            | definitions::FieldType::Text
            | definitions::FieldType::NodeList => String::new(),
            definitions::FieldType::Flags | definitions::FieldType::TxOptions => "0".to_string(),
            definitions::FieldType::Node => field
                .min
                .unwrap_or(1)
                .clamp(0, i64::from(u8::MAX))
                .to_string(),
            _ => "0".to_string(),
        }
    };
    filter::CommandFieldMatch {
        enabled: false,
        value,
    }
}

fn command_field_widget(ui: &mut Ui, field: &definitions::FieldDef, text: &mut String) {
    match field.kind {
        definitions::FieldType::Bool => {
            let mut value = command_field_bool(text);
            if ui.checkbox(&mut value, "On").changed() {
                *text = if value { "true" } else { "false" }.to_string();
            }
        }
        definitions::FieldType::Enum => {
            if field.values.is_empty() {
                widgets::numeric_input(ui, text, "0–255", 120.0, false);
            } else {
                let mut value = command_field_u8(field, text);
                let selected = field
                    .values
                    .iter()
                    .find(|item| item.value == value)
                    .map(|item| format!("{} (0x{:02x})", item.name, item.value))
                    .unwrap_or_else(|| format!("0x{value:02x}"));
                ComboBox::from_id_salt(("condition-command-enum", field.name.as_str()))
                    .selected_text(selected)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for item in &field.values {
                            ui.selectable_value(
                                &mut value,
                                item.value,
                                format!("{} (0x{:02x})", item.name, item.value),
                            );
                        }
                    });
                *text = value.to_string();
            }
        }
        definitions::FieldType::Flags | definitions::FieldType::TxOptions => {
            if field.values.is_empty() {
                widgets::numeric_input(ui, text, "0–255", 120.0, false);
            } else {
                let mut value = command_field_u8(field, text);
                ui.horizontal_wrapped(|ui| {
                    for item in &field.values {
                        let mut enabled = value & item.value != 0;
                        if ui.checkbox(&mut enabled, &item.name).changed() {
                            if enabled {
                                value |= item.value;
                            } else {
                                value &= !item.value;
                            }
                        }
                    }
                });
                *text = value.to_string();
            }
        }
        definitions::FieldType::Hex | definitions::FieldType::CommandClassData => {
            let width = field
                .length
                .or_else(|| field.kind.fixed_width())
                .unwrap_or(0);
            widgets::hex_input(
                ui,
                text,
                &command_field_hex_hint(width),
                width == 0 || width > 4,
                2,
                f32::INFINITY,
            );
        }
        definitions::FieldType::Node => {
            widgets::numeric_input(ui, text, "1–232, or 255 broadcast", 140.0, false);
        }
        definitions::FieldType::NodeList => {
            widgets::numeric_input(ui, text, "e.g. 2, 3, 5", 220.0, true);
        }
        definitions::FieldType::Uint8
        | definitions::FieldType::Uint16
        | definitions::FieldType::Uint32
        | definitions::FieldType::Duration
        | definitions::FieldType::CallbackId => {
            if field.format == definitions::FieldFormat::Hex {
                let width = field.kind.fixed_width().unwrap_or(1);
                widgets::hex_input(ui, text, &command_field_hex_hint(width), false, 1, 120.0);
            } else {
                widgets::numeric_input(ui, text, &command_field_range_hint(field), 120.0, false);
            }
        }
        definitions::FieldType::Int => {
            widgets::signed_numeric_input(ui, text, &command_field_range_hint(field), 140.0);
        }
        definitions::FieldType::Uint => {
            widgets::numeric_input(ui, text, &command_field_range_hint(field), 140.0, false);
        }
        definitions::FieldType::Text => {
            ui.add(TextEdit::singleline(text).desired_width(f32::INFINITY));
        }
    }
}

fn command_field_bool(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "1" | "on" | "true" | "yes"
    )
}

fn command_field_u8(field: &definitions::FieldDef, text: &str) -> u8 {
    let text = text.trim();
    text.parse::<u8>()
        .ok()
        .or_else(|| {
            field
                .values
                .iter()
                .find(|value| value.name.eq_ignore_ascii_case(text))
                .map(|value| value.value)
        })
        .unwrap_or_else(|| field.values.first().map(|value| value.value).unwrap_or(0))
}

fn command_field_hex_hint(width: usize) -> String {
    match width {
        1 => "1 hex byte".to_string(),
        2 => "2 hex bytes".to_string(),
        4 => "4 hex bytes".to_string(),
        _ => "hex bytes".to_string(),
    }
}

fn command_field_range_hint(field: &definitions::FieldDef) -> String {
    match (field.min, field.max) {
        (Some(min), Some(max)) => format!("{min}–{max}"),
        (Some(min), None) => format!("min {min}"),
        (None, Some(max)) => format!("max {max}"),
        (None, None) => "number".to_string(),
    }
}

fn default_zcl_match(field: &definitions::FieldDef) -> ZigbeeFieldMatch {
    if let Some(default) = &field.default_value {
        return ZigbeeFieldMatch {
            enabled: false,
            value: default.clone(),
        };
    }
    let value = match field.kind {
        definitions::FieldType::Bool
        | definitions::FieldType::Enum
        | definitions::FieldType::Flags
        | definitions::FieldType::Uint8
        | definitions::FieldType::Uint16
        | definitions::FieldType::Uint32
        | definitions::FieldType::Int
        | definitions::FieldType::Uint => "0".to_string(),
        _ => String::new(),
    };
    ZigbeeFieldMatch {
        enabled: false,
        value,
    }
}

fn zcl_match_suffix(field: &definitions::FieldDef) -> &'static str {
    match field.kind {
        definitions::FieldType::Hex => " (hex)",
        definitions::FieldType::Text => " (text)",
        _ if field.format == definitions::FieldFormat::Hex => " (hex)",
        _ => "",
    }
}

fn zcl_match_hint(field: &definitions::FieldDef) -> String {
    if !field.values.is_empty() {
        return field
            .values
            .iter()
            .map(|value| value.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
    }
    if let Some(description) = &field.description {
        return description.clone();
    }
    match field.kind {
        definitions::FieldType::Bool => "true or false".to_string(),
        definitions::FieldType::Hex => "hex bytes".to_string(),
        definitions::FieldType::Text => "text".to_string(),
        _ => "value or 0x hex".to_string(),
    }
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
                    widgets::field_label(ui, theme, "Offset");
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
            widgets::field_label(ui, theme, "Bytes");
            widgets::hex_input(
                ui,
                &mut leaf.bytes,
                "required hex, e.g. 01 03 — spaces and line breaks are ignored",
                true,
                3,
                width,
            );
            widgets::field_label(ui, theme, "Mask");
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
            let Some(parser) = model::message_parser(services, message) else {
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
            };
            let decoded = parser(message.payload.as_slice());
            let entity_keys = model::entity_service_keys(services, &message.service);
            let entities = entities::current();
            ui.add_space(4.0);
            if decoded.frames.is_empty() {
                ui.label(
                    RichText::new(format!(
                        "No {} frames recognised.",
                        protocol.unwrap_or("protocol")
                    ))
                    .color(palette.muted),
                );
            }
            const MAX_PARSED_FRAMES: usize = 64;
            for (index, frame) in decoded.frames.iter().take(MAX_PARSED_FRAMES).enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "Frame {} · {} · {} bytes",
                            index + 1,
                            frame.title,
                            frame.range.len()
                        ))
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
                if frame.summary != frame.title {
                    ui.label(
                        RichText::new(&frame.summary)
                            .size(13.5)
                            .color(palette.muted),
                    );
                }
                egui::Grid::new(("message-parsed-fields", index))
                    .num_columns(3)
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
                                RichText::new(format!(
                                    "[0x{:04x}..0x{:04x}]",
                                    field.range.start, field.range.end
                                ))
                                .monospace()
                                .size(13.5)
                                .color(palette.muted),
                            );
                            ui.label(
                                RichText::new(label)
                                    .strong()
                                    .size(14.0)
                                    .color(crate::theme::field_color(theme, field.kind)),
                            )
                            .on_hover_text(format!("{kind} field"));
                            let mut value = field.value.clone();
                            if protocol == Some("zigbee") && field.kind == FieldKind::Node {
                                let nwk = field
                                    .value
                                    .strip_prefix("0x")
                                    .and_then(|hex| u16::from_str_radix(hex, 16).ok())
                                    .or_else(|| field.value.parse::<u16>().ok());
                                if let Some(nwk) = nwk
                                    && let Some(device) = entities.network_device(&entity_keys, nwk)
                                {
                                    value = format!("{} — {}", field.value, device.display());
                                }
                            } else if field.kind == FieldKind::Node
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
            let json = model::pretty_json(message);
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
    fn new_frame_conditions_keep_the_basic_set_start() {
        let spec = default_frame_condition();
        assert_eq!(spec.command_class, Some(hodeco_codec::builder::CC_BASIC));
        assert_eq!(spec.command, Some(hodeco_codec::builder::BASIC_SET));
        assert!(!spec.includes.command_class);
        assert!(!spec.includes.command);
    }

    #[test]
    fn command_field_defaults_use_enum_values_not_labels() {
        let definitions = definitions::current();
        let command = definitions.command(0x25, 0x01).expect("Switch Binary Set");
        let field = command
            .fields
            .iter()
            .filter_map(definitions::LayoutField::field)
            .find(|field| field.name == "target")
            .expect("target field");
        let default = default_command_field_match(field);
        assert_eq!(default.value, "0");
        assert!(!default.enabled);
        assert_eq!(command_field_u8(field, "On"), 0xFF);
        assert_eq!(command_field_u8(field, "0"), 0);
    }

    #[test]
    fn zwave_filter_tx_options_come_from_definitions() {
        let definitions = definitions::current();
        let options = zwave_tx_option_values(&definitions, FrameKind::SendData);
        assert_eq!(options.len(), 5);
        assert!(
            options
                .iter()
                .any(|(value, name)| *value == 1 && name == "ACK")
        );
        assert!(options.iter().any(|(value, _)| *value == 0x20));
    }

    #[test]
    fn large_payload_views_are_virtualized() {
        let payload = vec![b'A'; 100_000];
        let decoded = hodeco_codec::zwave::decode(&payload);
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
