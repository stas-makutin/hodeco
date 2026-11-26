//! Shared widget building blocks.
//!
//! These helpers keep the views visually consistent and centralise the places
//! where colours from [`crate::theme`] are turned into egui paint calls.

use crate::theme::{UiTheme, palette};
use egui::{
    Align, Align2, Color32, ComboBox, Context, Frame, Id, Label, Layout, Margin, Popup,
    PopupCloseBehavior, Response, RichText, ScrollArea, Stroke, TextEdit, TextStyle, Ui, Vec2,
};
use hodeco_contract::utils::HumanDuration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Info,
    Success,
    Warning,
    Error,
}

impl Tone {
    pub fn color(self, theme: UiTheme) -> Color32 {
        let palette = palette(theme);
        match self {
            Self::Neutral => palette.muted,
            Self::Info => palette.accent,
            Self::Success => palette.green,
            Self::Warning => palette.amber,
            Self::Error => palette.red,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub tone: Tone,
}

#[derive(Debug, Clone, Copy)]
pub struct ComboOptions<'a, T> {
    pub any_label: Option<&'a str>,
    pub emphasize: &'a [T],
}

/// Maximum height of the scrollable result list inside a [`searchable_combo`]
/// popup.
pub(crate) const LIST_MAX_HEIGHT: f32 = 270.0;

/// A combo box with a filter field in its popup.
///
/// The filter text is kept in egui's temporary data so callers do not need to
/// add UI-only fields to their state. This is intended for catalogs with many
/// entries, such as the Z-Wave command-class and command pickers.
pub fn searchable_combo<T: Copy + PartialEq>(
    ui: &mut Ui,
    id_salt: &str,
    selected_text: String,
    width: f32,
    selected: &mut Option<T>,
    options: ComboOptions<'_, T>,
    items: &[(T, String)],
) {
    let ComboOptions {
        any_label,
        emphasize,
    } = options;
    let filter_id = ui.make_persistent_id((id_salt, "filter"));
    // `ComboBox` stores its salt as an `IdSalt` and then derives the button
    // id from that, so reproduce the same id to address its popup state.
    let button_id = ui.make_persistent_id(egui::IdSalt::new(id_salt));
    let popup_id = button_id.with("popup");
    let was_open = Popup::is_id_open(ui.ctx(), popup_id);
    let mut filter = ui
        .data_mut(|data| data.get_temp::<String>(filter_id))
        .unwrap_or_default();
    let filter_before = filter.clone();

    ComboBox::from_id_salt(id_salt)
        .selected_text(selected_text)
        .width(width)
        .height(360.0)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            // ComboBox normally lets long labels expand its popup. Cap the
            // content at the width captured on open so focusing the filter
            // does not resize the whole dropdown.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            // Keep the filter outside the list's scroll area so it remains
            // visible while the user scrolls through the catalog.
            const CLEAR_WIDTH: f32 = 58.0;
            let row_width_id = ui.make_persistent_id((id_salt, "filter-row-width"));
            // The popup sizes itself from its content. Asking for the full
            // available width every frame creates a feedback loop once the
            // Clear button appears, so keep the width captured when it opens.
            let row_width = if was_open {
                ui.data(|data| data.get_temp::<f32>(row_width_id))
                    .unwrap_or_else(|| ui.available_width())
            } else {
                let width = ui.available_width();
                ui.data_mut(|data| data.insert_temp(row_width_id, width));
                width
            };
            ui.set_max_width(row_width);
            // egui popups are laid out inside the previous frame's rect
            // (`egui::containers::Area::content_ui`) and can therefore only
            // shrink while they are open, never grow back. Reserve the full
            // list height on every frame so filtering down to a few matches
            // — then clearing the filter — keeps the dropdown at a stable
            // size instead of leaving it permanently shrunk.
            ui.set_min_height(
                ui.spacing().interact_size.y + ui.spacing().item_spacing.y * 2.0 + LIST_MAX_HEIGHT,
            );
            let mut clear_clicked = false;
            // Decide this before the TextEdit consumes input. Otherwise the
            // first typed character adds Clear while the filter still occupies
            // the full width, making the popup grow by the button width.
            let show_clear = !filter.is_empty();
            ui.horizontal(|ui| {
                let reserved = if show_clear {
                    CLEAR_WIDTH + ui.spacing().item_spacing.x
                } else {
                    0.0
                };
                let filter_width = (row_width - reserved).max(80.0);
                let filter_response = ui.add_sized(
                    [filter_width, ui.spacing().interact_size.y],
                    TextEdit::singleline(&mut filter).hint_text("Filter…"),
                );
                if !was_open {
                    filter_response.request_focus();
                }
                if show_clear {
                    let clear_response = ui.add_sized(
                        [CLEAR_WIDTH, ui.spacing().interact_size.y],
                        egui::Button::new("Clear").small(),
                    );
                    if clear_response.clicked() {
                        clear_clicked = true;
                    }
                }
            });
            if clear_clicked {
                filter.clear();
            }
            ui.separator();

            let needle = filter.trim().to_ascii_lowercase();
            let matches =
                |label: &str| needle.is_empty() || label.to_ascii_lowercase().contains(&needle);

            let list = ScrollArea::vertical()
                .max_height(LIST_MAX_HEIGHT)
                .auto_shrink([false, true]);
            let list = if !was_open || filter != filter_before {
                // A new search or a reopened popup should start at the first
                // result instead of retaining the previous scroll position.
                list.vertical_scroll_offset(0.0)
            } else {
                list
            };
            list.show(ui, |ui| {
                if let Some(any_label) = any_label
                    && matches(any_label)
                    && ui.selectable_label(selected.is_none(), any_label).clicked()
                {
                    *selected = None;
                    filter.clear();
                    Popup::close_id(ui.ctx(), popup_id);
                }
                for (value, label) in items {
                    if !matches(label) {
                        continue;
                    }
                    // `RichText::strong` only changes the text colour in
                    // egui, and this theme maps that colour to normal text.
                    // Use the accent colour for a visible emphasis instead.
                    let text = if emphasize.contains(value) {
                        RichText::new(label.as_str()).color(ui.visuals().hyperlink_color)
                    } else {
                        RichText::new(label.as_str())
                    };
                    if ui
                        .selectable_label(*selected == Some(*value), text)
                        .clicked()
                    {
                        *selected = Some(*value);
                        filter.clear();
                        Popup::close_id(ui.ctx(), popup_id);
                    }
                }
            });
        });

    ui.data_mut(|data| data.insert_temp(filter_id, filter));
}

pub fn pill(ui: &mut Ui, theme: UiTheme, text: &str, tone: Tone) {
    let color = tone.color(theme);
    Frame::new()
        .fill(color.gamma_multiply(0.14))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.55)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).size(14.0).strong());
        });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListNav {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Activate,
}

impl ListNav {
    /// Shared selection arithmetic for list keyboard navigation. An empty
    /// selection starts at the first row; `Activate` keeps the current
    /// selection (or the first row) so the caller can act on it.
    pub fn apply(self, selected: Option<usize>, len: usize) -> usize {
        let last = len.saturating_sub(1);
        match self {
            Self::Up => selected.map_or(0, |current| current.saturating_sub(1)),
            Self::Down => selected.map_or(0, |current| (current + 1).min(last)),
            Self::PageUp => selected.map_or(0, |current| current.saturating_sub(10)),
            Self::PageDown => selected.map_or(0, |current| (current + 10).min(last)),
            Self::Home => 0,
            Self::End => last,
            Self::Activate => selected.unwrap_or(0),
        }
    }
}

/// A focusable but non-clickable region that makes a table/list a Tab stop.
pub struct ListFocus {
    pub id: Id,
    pub rect: egui::Rect,
    pub focused: bool,
    pub hovered: bool,
}

/// A passive list region for views whose keyboard navigation is logical rather
/// than egui-focus based. It never claims or consumes keyboard focus.
pub fn list_region(
    ui: &mut Ui,
    theme: UiTheme,
    salt: impl std::hash::Hash + std::fmt::Debug,
    highlight: bool,
) -> ListFocus {
    let rect = ui.available_rect_before_wrap();
    let id = ui.scope_id().with(("list-region", salt));
    let response = ui.interact(rect, id, egui::Sense::hover());
    if highlight {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            4.0,
            Stroke::new(1.0, palette(theme).accent.gamma_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
    }
    ListFocus {
        id,
        rect,
        focused: response.has_focus(),
        hovered: response.hovered(),
    }
}

pub fn list_focus(
    ui: &mut Ui,
    theme: UiTheme,
    salt: impl std::hash::Hash + std::fmt::Debug,
    claim_focus: bool,
    highlight: bool,
) -> ListFocus {
    list_focus_with_tab(ui, theme, salt, claim_focus, highlight, true)
}

/// Like [`list_focus`], but lets the caller decide whether Tab stays locked to
/// the list for manual handoff to a row action.
pub fn list_focus_with_tab(
    ui: &mut Ui,
    theme: UiTheme,
    salt: impl std::hash::Hash + std::fmt::Debug,
    claim_focus: bool,
    highlight: bool,
    lock_tab: bool,
) -> ListFocus {
    let rect = ui.available_rect_before_wrap();
    let id = ui.scope_id().with(("list-focus", salt));
    if claim_focus {
        ui.memory_mut(|memory| memory.request_focus(id));
    }
    let response = ui.interact(rect, id, egui::Sense::focusable_noninteractive());
    // Arrow keys drive the list selection, not egui's widget-focus navigation.
    // Without this filter, Up/Down can move focus to a nearby button while the
    // list itself still handles the selection.
    ui.memory_mut(|memory| {
        memory.set_focus_lock_filter(
            id,
            egui::EventFilter {
                tab: lock_tab,
                vertical_arrows: true,
                escape: true,
                ..Default::default()
            },
        );
    });
    if response.has_focus() || highlight {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            4.0,
            Stroke::new(1.0, palette(theme).accent.gamma_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
    }
    ListFocus {
        id,
        rect,
        focused: response.has_focus(),
        hovered: response.hovered(),
    }
}

/// True when Tab was pressed while the list had keyboard focus.
pub fn list_tab_pressed(ui: &Ui, id: Id) -> bool {
    ui.memory(|memory| memory.had_focus_last_frame(id))
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
}

/// Keyboard navigation for a focused or hovered list. Text fields keep their
/// usual key handling because hover navigation is disabled while any widget has
/// focus; an explicitly focused list still receives the keys.
pub fn list_nav(ui: &Ui, focused: bool, hovered: bool) -> Option<ListNav> {
    if !focused && (!hovered || ui.ctx().memory(|memory| memory.focused().is_some())) {
        return None;
    }
    read_list_nav(ui, true)
}

/// Arrow navigation for a list that owns the page context. This keeps working
/// after Tab lands on a control above or below the list, while leaving text
/// fields and open popups in control of their own keys.
pub fn list_nav_global(ui: &Ui) -> Option<ListNav> {
    if ui.ctx().text_edit_focused() || ui.ctx().any_popup_open() {
        return None;
    }
    read_list_nav(ui, false)
}

fn read_list_nav(ui: &Ui, allow_activate: bool) -> Option<ListNav> {
    ui.input_mut(|input| {
        if input.modifiers.any() {
            return None;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            Some(ListNav::Up)
        } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            Some(ListNav::Down)
        } else if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
            Some(ListNav::PageUp)
        } else if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
            Some(ListNav::PageDown)
        } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
            Some(ListNav::Home)
        } else if input.consume_key(egui::Modifiers::NONE, egui::Key::End) {
            Some(ListNav::End)
        } else if allow_activate && input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            Some(ListNav::Activate)
        } else {
            None
        }
    })
}

/// Scrolls the current scroll area with arrow/page keys when no widget has
/// keyboard focus. Call this from inside the scroll area's content closure.
pub fn keyboard_scroll(ui: &Ui, active: bool) {
    if !active || ui.ctx().memory(|memory| memory.focused().is_some()) {
        return;
    }
    let page = (ui.available_height() * 0.8).max(60.0);
    let delta = ui.input(|input| {
        if input.modifiers.any() {
            return 0.0;
        }
        if input.key_pressed(egui::Key::ArrowUp) {
            36.0
        } else if input.key_pressed(egui::Key::ArrowDown) {
            -36.0
        } else if input.key_pressed(egui::Key::PageUp) {
            page
        } else if input.key_pressed(egui::Key::PageDown) {
            -page
        } else {
            0.0
        }
    });
    if delta != 0.0 {
        ui.scroll_with_delta(Vec2::new(0.0, delta));
    }
}

/// Scrolls a newly focused control into the nearest scroll area. This makes
/// Tab navigation usable inside tall dialogs.
pub fn scroll_on_focus(response: &egui::Response) {
    if response.gained_focus() {
        response.scroll_to_me(Some(Align::Center));
    }
}

pub fn status_dot(ui: &mut Ui, color: Color32) {
    let radius = 4.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(radius * 2.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

pub fn page_header(
    ui: &mut Ui,
    theme: UiTheme,
    title: &str,
    subtitle: Option<&str>,
    actions: impl FnOnce(&mut Ui),
) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(24.0).strong());
        ui.add_space(8.0);
        if let Some(subtitle) = subtitle {
            ui.label(
                RichText::new(subtitle)
                    .color(palette(theme).muted)
                    .size(14.0),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), actions);
    });
    ui.add_space(6.0);
}

pub fn section_header(ui: &mut Ui, title: &str, actions: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(15.0).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), actions);
    });
    ui.add_space(2.0);
}

/// A persistent tinted message with a left accent bar and an optional action row.
pub fn banner(
    ui: &mut Ui,
    theme: UiTheme,
    tone: Tone,
    title: &str,
    detail: Option<&str>,
    actions: impl FnOnce(&mut Ui),
) {
    let color = tone.color(theme);
    let palette = palette(theme);
    Frame::new()
        .fill(color.gamma_multiply(if theme == UiTheme::Dark { 0.12 } else { 0.09 }))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.4)))
        .corner_radius(6)
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).strong().color(color));
                ui.with_layout(Layout::right_to_left(Align::Center), actions);
            });
            if let Some(detail) = detail {
                ui.label(RichText::new(detail).color(palette.text).size(14.0));
            }
        });
    ui.add_space(4.0);
}

pub fn empty_state(ui: &mut Ui, theme: UiTheme, title: &str, detail: &str) {
    let palette = palette(theme);
    ui.add_space(36.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).size(16.0).color(palette.text));
        ui.add_space(4.0);
        ui.label(RichText::new(detail).size(14.0).color(palette.muted));
    });
    ui.add_space(36.0);
}

pub fn loading_state(ui: &mut Ui, theme: UiTheme, text: &str) {
    ui.add_space(24.0);
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(16.0));
        ui.label(RichText::new(text).color(palette(theme).muted));
    });
    ui.add_space(24.0);
}

pub fn labeled_value(ui: &mut Ui, theme: UiTheme, label: &str, value: &str) {
    let palette = palette(theme);
    ui.label(RichText::new(label).size(13.5).color(palette.muted));
    ui.label(RichText::new(value).size(14.0));
}

/// Muted form label used by the configuration and automation editors.
pub fn field_label_text(theme: UiTheme, label: &str) -> RichText {
    RichText::new(label).size(13.5).color(palette(theme).muted)
}

/// Draws a muted form label.
pub fn field_label(ui: &mut Ui, theme: UiTheme, label: &str) {
    ui.label(field_label_text(theme, label));
}

/// Draws the muted one-line help text that follows a form field.
pub fn field_note(ui: &mut Ui, theme: UiTheme, note: &str) {
    ui.label(RichText::new(note).small().color(palette(theme).muted));
}

/// A form control followed by a muted one-line description.
pub fn field_with_note(ui: &mut Ui, theme: UiTheme, note: &str, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        add(ui);
        field_note(ui, theme, note);
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeUnit {
    Seconds,
    Minutes,
    Hours,
    Days,
    Weeks,
}

impl TimeUnit {
    const ALL: [Self; 5] = [
        Self::Seconds,
        Self::Minutes,
        Self::Hours,
        Self::Days,
        Self::Weeks,
    ];

    const fn suffix(self) -> &'static str {
        match self {
            Self::Seconds => "s",
            Self::Minutes => "m",
            Self::Hours => "h",
            Self::Days => "d",
            Self::Weeks => "w",
        }
    }

    const fn nanos(self) -> u128 {
        match self {
            Self::Seconds => 1_000_000_000,
            Self::Minutes => 60_000_000_000,
            Self::Hours => 3_600_000_000_000,
            Self::Days => 86_400_000_000_000,
            Self::Weeks => 604_800_000_000_000,
        }
    }
}

fn parse_duration_parts(value: &str) -> Option<(u64, TimeUnit)> {
    let value = value.trim();
    for unit in [
        TimeUnit::Weeks,
        TimeUnit::Days,
        TimeUnit::Hours,
        TimeUnit::Minutes,
        TimeUnit::Seconds,
    ] {
        if let Some(amount) = value.strip_suffix(unit.suffix())
            && let Ok(amount) = amount.trim().parse::<u64>()
        {
            return Some((amount, unit));
        }
    }
    let nanos = value.parse::<HumanDuration>().ok()?.as_std().as_nanos();
    if nanos == 0 {
        return Some((0, TimeUnit::Seconds));
    }
    for unit in [
        TimeUnit::Weeks,
        TimeUnit::Days,
        TimeUnit::Hours,
        TimeUnit::Minutes,
        TimeUnit::Seconds,
    ] {
        if nanos.is_multiple_of(unit.nanos()) {
            return Some((u64::try_from(nanos / unit.nanos()).ok()?, unit));
        }
    }
    None
}

/// Number-plus-unit duration editor shared by task and schedule forms.
pub fn duration_editor(ui: &mut Ui, value: &mut String, minimum: std::time::Duration) -> bool {
    let Some((mut amount, original_unit)) = parse_duration_parts(value) else {
        let response = ui.add(
            egui::TextEdit::singleline(value)
                .hint_text("5m")
                .desired_width(160.0),
        );
        return response.changed();
    };
    let min_nanos = minimum.as_nanos();
    let mut changed = false;
    ui.horizontal(|ui| {
        let response = ui.add(
            egui::DragValue::new(&mut amount)
                .range(1..=u64::MAX)
                .speed(1.0),
        );
        let mut unit = original_unit;
        egui::ComboBox::from_id_salt(ui.next_auto_id())
            .selected_text(unit.suffix())
            .width(56.0)
            .show_ui(ui, |ui| {
                for candidate in TimeUnit::ALL {
                    ui.selectable_value(&mut unit, candidate, candidate.suffix());
                }
            });
        let mut clamped = false;
        let nanos = u128::from(amount) * unit.nanos();
        if nanos < min_nanos {
            amount = u64::try_from(min_nanos.div_ceil(unit.nanos()))
                .unwrap_or(1)
                .max(1);
            clamped = true;
        }
        if response.changed() || unit != original_unit || clamped {
            *value = format!("{amount} {}", unit.suffix());
            changed = true;
        }
    });
    changed
}

/// A bordered group of related form controls with a section title.
///
/// Long editors split fields into groups instead of one flat form.
pub fn control_group(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    control_group_inner(ui, title, false, add);
}

/// Same as [`control_group`], but the group fills the available width.
/// Use this in dialogs where the group should align with the dialog edges.
pub fn control_group_fill(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    control_group_inner(ui, title, true, add);
}

fn control_group_inner(ui: &mut Ui, title: &str, fill: bool, add: impl FnOnce(&mut Ui)) {
    ui.add_space(2.0);
    Frame::group(ui.style())
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                if fill {
                    let width = ui.available_width();
                    if width.is_finite() && width > 0.0 {
                        ui.set_min_width(width);
                    }
                }
                ui.label(RichText::new(title).strong());
                ui.add_space(6.0);
                add(ui);
            });
        });
    ui.add_space(6.0);
}

pub fn copy_button(ui: &mut Ui, value: &str) -> bool {
    copy_with_feedback(ui, value, "Copy")
}

/// Copy action with a short-lived inline confirmation. Clipboard access on the
/// web is asynchronous and can be denied outside a secure context, so silent
/// buttons are indistinguishable from broken ones.
pub fn copy_with_feedback(ui: &mut Ui, value: &str, label: &str) -> bool {
    let id = ui.scope_id().with(("copy-feedback", label));
    let response = ui.small_button(label).on_hover_text("Copy to clipboard");
    let clicked = response.clicked();
    let mut copied = false;
    if clicked {
        copied = copy_text(ui, value);
        if copied {
            // Read input before taking the data write lock. Re-entering the
            // context while `data_mut` holds it deadlocks on wasm.
            let now = ui.input(|input| input.time);
            ui.data_mut(|data| data.insert_temp(id, now));
        }
    }
    let recent = ui
        .data(|data| data.get_temp::<f64>(id))
        .is_some_and(|at| ui.input(|input| input.time) - at < 2.0);
    if recent {
        ui.label(RichText::new("Copied").size(13.5).color(Color32::GREEN));
    } else if clicked && !copied {
        ui.label(RichText::new("Copy failed").size(13.5).color(Color32::RED));
    }
    clicked
}

fn copy_text(ui: &Ui, value: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        // Browsers without a secure context (plain HTTP on a LAN address) have
        // no navigator.clipboard. Use the legacy selection-based path.
        if web_sys::window().is_some_and(|window| !window.is_secure_context()) {
            return copy_text_fallback(value);
        }
    }
    ui.ctx().copy_text(value.to_string());
    true
}

#[cfg(target_arch = "wasm32")]
fn copy_text_fallback(value: &str) -> bool {
    use wasm_bindgen::JsCast;

    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return false;
    };
    let Ok(element) = document.create_element("textarea") else {
        return false;
    };
    let Ok(textarea) = element.dyn_into::<web_sys::HtmlTextAreaElement>() else {
        return false;
    };
    textarea.set_value(value);
    let _ = textarea.set_attribute(
        "style",
        "position:fixed;left:-10000px;top:0;opacity:0;pointer-events:none;",
    );
    let Some(body) = document.body() else {
        return false;
    };
    if body.append_child(&textarea).is_err() {
        return false;
    }
    textarea.select();
    let copied = document
        .dyn_into::<web_sys::HtmlDocument>()
        .ok()
        .and_then(|document| document.exec_command("copy").ok())
        .unwrap_or(false);
    let _ = textarea.remove();
    copied
}

struct HexBuffer<'a> {
    text: &'a mut String,
    /// Cursor position after an insertion that re-spaced the text.
    cursor_after: Option<usize>,
}

struct HexBufferMarker;

/// Character index just after the first `digits` hex digits in `text`.
fn cursor_after_digits(text: &str, digits: usize) -> usize {
    if digits == 0 {
        return 0;
    }
    let mut seen = 0;
    for (index, character) in text.chars().enumerate() {
        if character.is_ascii_hexdigit() {
            seen += 1;
            if seen == digits {
                return index + 1;
            }
        }
    }
    text.chars().count()
}

impl egui::TextBuffer for HexBuffer<'_> {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.text
    }

    fn insert_text(&mut self, input: &str, char_index: egui::text::CharIndex) -> usize {
        let digits: String = {
            let mut output = String::new();
            let mut chars = input.chars().peekable();
            while let Some(character) = chars.next() {
                if character == '0' && matches!(chars.peek(), Some('x' | 'X')) {
                    chars.next();
                    continue;
                }
                if character.is_ascii_hexdigit() {
                    output.push(character.to_ascii_lowercase());
                }
            }
            output
        };
        if digits.is_empty() {
            return 0;
        }

        let mut chars: Vec<char> = self.text.chars().collect();
        let digits_before = chars
            .iter()
            .take(char_index.0.min(chars.len()))
            .filter(|character| character.is_ascii_hexdigit())
            .count();
        let at_end = char_index.0 >= chars.len();
        let inserted = if at_end {
            let mut inserted = 0;
            for digit in digits.chars() {
                chars.push(digit);
                inserted += 1;
                let digit_count = chars
                    .iter()
                    .filter(|character| character.is_ascii_hexdigit())
                    .count();
                if digit_count.is_multiple_of(2) && chars.last().is_some_and(|c| *c != ' ') {
                    chars.push(' ');
                    inserted += 1;
                }
            }
            *self.text = chars.into_iter().collect();
            inserted
        } else {
            let index = char_index.0.min(chars.len());
            for (offset, digit) in digits.chars().enumerate() {
                chars.insert(index + offset, digit);
            }
            let digits: String = chars
                .into_iter()
                .filter(|character| character.is_ascii_hexdigit())
                .collect();
            *self.text = crate::model::normalize_hex_text(&digits);
            digits.len()
        };
        let total_digits = digits_before + digits.chars().count();
        self.cursor_after = Some(if at_end {
            self.text.chars().count()
        } else {
            cursor_after_digits(self.text, total_digits)
        });
        inserted
    }

    fn delete_char_range(&mut self, char_range: std::ops::Range<egui::text::CharIndex>) {
        <String as egui::TextBuffer>::delete_char_range(self.text, char_range);
    }

    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<HexBufferMarker>()
    }
}

/// Hex text field that rejects non-hex characters at the buffer level and
/// inserts a space every two digits while typing or pasting at the end.
pub fn hex_input(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    multiline: bool,
    rows: usize,
    width: f32,
) -> bool {
    hex_input_response(ui, text, hint, multiline, rows, width).0
}

/// Same as [`hex_input`], but also returns the response so the caller can
/// restore focus or detect Enter.
pub fn hex_input_response(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    multiline: bool,
    rows: usize,
    width: f32,
) -> (bool, egui::Response) {
    let mut buffer = HexBuffer {
        text,
        cursor_after: None,
    };
    let edit = if multiline {
        TextEdit::multiline(&mut buffer)
            .desired_width(width)
            .desired_rows(rows)
            .font(TextStyle::Monospace)
    } else {
        TextEdit::singleline(&mut buffer)
            .desired_width(width)
            .font(TextStyle::Monospace)
    };
    let mut output = edit.hint_text(hint).show(ui);
    if output.response.changed()
        && let Some(cursor) = buffer.cursor_after.take()
    {
        output
            .state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(cursor),
            )));
        output.state.store(ui.ctx(), output.response.id);
    }
    let changed = output.response.changed();
    let response = output.response.response;
    (changed, response)
}

/// Numeric text field. Non-digit characters are rejected before they can enter
/// the buffer; separators can be allowed for comma-separated lists.
pub fn numeric_input(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    width: f32,
    separators: bool,
) -> bool {
    numeric_text_input(ui, text, hint, width, separators, false)
}

/// Numeric input that also accepts a single leading minus sign.
pub fn signed_numeric_input(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> bool {
    numeric_text_input(ui, text, hint, width, false, true)
}

fn numeric_text_input(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    width: f32,
    separators: bool,
    allow_negative: bool,
) -> bool {
    let edit = TextEdit::singleline(text)
        .hint_text(hint)
        .desired_width(width);
    let mut output = edit.show(ui);
    if output.response.changed() {
        let allowed = |character: char| {
            character.is_ascii_digit()
                || (separators && matches!(character, ',' | ';' | ' '))
                || (allow_negative && character == '-')
        };
        let previous_chars = text.chars().count();
        let cursor_index = output.cursor_range.map_or(previous_chars, |range| {
            range.primary.index.0.min(previous_chars)
        });
        let cursor_was_at_end = cursor_index >= previous_chars;
        let allowed_before = text
            .chars()
            .take(cursor_index)
            .filter(|c| allowed(*c))
            .count();
        let filtered: String = text.chars().filter(|c| allowed(*c)).collect();
        let filtered = if allow_negative {
            normalize_signed(&filtered)
        } else {
            filtered
        };
        if *text != filtered {
            *text = filtered;
            let new_len = text.chars().count();
            let cursor = if cursor_was_at_end {
                new_len
            } else {
                allowed_before.min(new_len)
            };
            let cursor = egui::text::CCursor::new(cursor);
            output
                .state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(cursor)));
            output.state.store(ui.ctx(), output.response.id);
        }
        true
    } else {
        false
    }
}

fn normalize_signed(text: &str) -> String {
    let negative = text.contains('-');
    let digits: String = text
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect();
    if negative {
        format!("-{digits}")
    } else {
        digits
    }
}

/// Compact, non-interactive chip used for tags and labels.
pub fn chip(ui: &mut Ui, theme: UiTheme, text: &str, tone: Tone) {
    let color = tone.color(theme);
    Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(9)
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(12.0).color(color));
        });
}

/// Removable filter chip rendered as a single button.
///
/// Unlike [`removable_chip`], this is one widget and therefore wraps correctly
/// inside [`Ui::horizontal_wrapped`]. Clicking anywhere on the chip removes it.
pub fn removable_chip_button(ui: &mut Ui, theme: UiTheme, text: &str, tone: Tone) -> bool {
    let color = tone.color(theme);
    ui.add(
        egui::Button::new(RichText::new(format!("{text}  ×")).size(13.5).color(color))
            .fill(color.gamma_multiply(0.12))
            .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
            .corner_radius(9)
            .small(),
    )
    .on_hover_text("Remove filter")
    .clicked()
}

/// Removable filter chip. Returns true when the user clicked the `x`.
pub fn removable_chip(ui: &mut Ui, theme: UiTheme, text: &str, tone: Tone) -> bool {
    let color = tone.color(theme);
    let mut removed = false;
    Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(9)
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).size(13.5).color(color));
                if ui
                    .add(
                        egui::Button::new(RichText::new("×").size(14.0).color(color))
                            .frame(false)
                            .min_size(egui::vec2(16.0, 18.0)),
                    )
                    .on_hover_text("Remove filter")
                    .clicked()
                {
                    removed = true;
                }
            });
        });
    removed
}

/// Width of a [`Ui::small_button`] with the given label, including padding.
pub fn small_button_width(ui: &Ui, text: &str) -> f32 {
    let font = TextStyle::Button.resolve(ui.style());
    let color = ui.visuals().text_color();
    let galley = ui.fonts_mut(|fonts| fonts.layout_no_wrap(text.to_owned(), font, color));
    galley.size().x + 2.0 * ui.spacing().button_padding.x
}

/// Single-line text field that grows with its contents up to `max_width`.
///
/// egui's `TextEdit` only accepts a fixed desired width, so this measures the
/// current text (or hint) and applies the clamp before laying out the field.
pub fn auto_width_text_edit(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    min_width: f32,
    max_width: f32,
) -> Response {
    let font_id = TextStyle::Body.resolve(ui.style());
    let measured = if text.is_empty() { hint } else { text.as_str() };
    let galley = ui.fonts_mut(|fonts| {
        fonts.layout_no_wrap(measured.to_string(), font_id, Color32::PLACEHOLDER)
    });
    let width = (galley.size().x + 18.0).clamp(min_width, max_width);
    ui.add(
        TextEdit::singleline(text)
            .hint_text(hint)
            .desired_width(width),
    )
}

/// Full-width read-only value field with a copy action. Long values scroll
/// horizontally; multiline values wrap instead.
pub fn copy_field(ui: &mut Ui, theme: UiTheme, label: &str, value: &str, multiline: bool) {
    let palette = palette(theme);
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(13.5).color(palette.muted));
        copy_button(ui, value);
    });
    let available = (ui.available_width() - 10.0).max(120.0);
    egui::Frame::new()
        .fill(palette.input)
        .inner_margin(Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.set_width(available);
            let text = RichText::new(value).monospace().color(palette.text);
            if multiline {
                ui.add(Label::new(text).selectable(true).wrap());
            } else {
                egui::ScrollArea::horizontal()
                    .id_salt(ui.next_auto_id())
                    .show(ui, |ui| {
                        ui.add(
                            Label::new(text)
                                .selectable(true)
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
            }
        });
}

/// Fixed dialog header.
pub fn dialog_header(ui: &mut Ui, theme: UiTheme, title: &str, subtitle: Option<&str>) {
    ui.label(RichText::new(title).size(21.0).strong());
    if let Some(subtitle) = subtitle {
        ui.label(
            RichText::new(subtitle)
                .size(14.0)
                .color(palette(theme).muted),
        );
    }
    ui.add_space(4.0);
    ui.separator();
}

/// Scrollable dialog body with a bounded height so the footer stays visible.
/// The height is also capped by the viewport so the modal never grows past the
/// window at the minimum supported size (960x600).
pub fn dialog_body(ui: &mut Ui, max_height: f32, body: impl FnOnce(&mut Ui)) {
    let viewport_cap = (ui.ctx().content_rect().height() - 220.0).max(120.0);
    let max_height = max_height.min(viewport_cap);
    ScrollArea::vertical()
        .max_height(max_height)
        .auto_shrink([false, true])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show(ui, |ui| {
            keyboard_scroll(ui, ui.rect_contains_pointer(ui.max_rect()));
            body(ui);
        });
}

/// Fixed dialog footer with right-aligned actions and an optional error on the
/// same row. The error is truncated before it can squeeze or move the actions.
pub fn dialog_footer(
    ui: &mut Ui,
    theme: UiTheme,
    error: Option<&str>,
    footer: impl FnOnce(&mut Ui),
) {
    ui.add_space(4.0);
    ui.separator();
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            footer(ui);
            if let Some(error) = error {
                let remaining = ui.available_width();
                if remaining > 8.0 {
                    ui.allocate_ui_with_layout(
                        Vec2::new(remaining, ui.spacing().interact_size.y),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.add_space(8.0);
                            ui.add(
                                Label::new(RichText::new(error).color(palette(theme).red))
                                    .truncate(),
                            );
                        },
                    );
                }
            }
        });
    });
}

pub fn dialog_width(ui: &mut Ui, width: f32) {
    ui.set_min_width(width);
    ui.set_max_width(width);
}

/// Standard confirmation modal with Cancel and a destructive action.
/// Returns `Some(true)` to confirm, `Some(false)` to cancel, or `None` while
/// the dialog is still open.
pub fn confirm_modal(
    ctx: &Context,
    theme: UiTheme,
    id: &str,
    title: &str,
    detail: &str,
    confirm_label: &str,
) -> Option<bool> {
    let palette = palette(theme);
    let mut result = None;
    let response = egui::Modal::new(Id::unique(id))
        .frame(modal_frame(theme))
        .show(ctx, |ui| {
            dialog_width(ui, 400.0);
            dialog_header(ui, theme, title, None);
            dialog_body(ui, 180.0, |ui| {
                ui.label(detail);
            });
            dialog_footer(ui, theme, None, |ui| {
                if ui.button("Cancel").clicked() {
                    result = Some(false);
                }
                if ui
                    .button(RichText::new(confirm_label).color(palette.red))
                    .clicked()
                {
                    result = Some(true);
                }
            });
            false
        });
    if response.should_close() && result.is_none() {
        result = Some(false);
    }
    result
}

/// Standard modal frame. `Modal` already supplies a backdrop and window frame.
pub fn modal_frame(theme: UiTheme) -> Frame {
    let palette = palette(theme);
    Frame::new()
        .fill(palette.panel)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(8)
        .inner_margin(Margin::same(14))
}

pub fn show_toasts(ctx: &Context, theme: UiTheme, toasts: &[Toast]) {
    if toasts.is_empty() {
        return;
    }
    let palette = palette(theme);
    egui::Area::new(Id::unique("hodeco-toasts"))
        .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -28.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_max_width(460.0);
            for toast in toasts.iter().rev() {
                let color = toast.tone.color(theme);
                Frame::new()
                    .fill(palette.panel)
                    .stroke(Stroke::new(1.0, color))
                    .corner_radius(6)
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(toast.text.clone()).color(palette.text));
                        });
                    });
                ui.add_space(4.0);
            }
        });
}

/// Renders a parameter value with a type-appropriate read-only representation.
pub fn param_value_text(
    kind: hodeco_contract::domain::ParamType,
    value: &serde_json::Value,
) -> String {
    use hodeco_contract::domain::ParamType;
    match kind {
        ParamType::Bool => value
            .as_bool()
            .map(|value| if value { "true" } else { "false" }.to_string())
            .unwrap_or_else(|| value.to_string()),
        ParamType::Int | ParamType::Uint => value.to_string(),
        ParamType::String => value.as_str().unwrap_or_default().to_string(),
        ParamType::Bytes => value.as_str().unwrap_or_default().to_string(),
        ParamType::Size | ParamType::Duration | ParamType::FileMode => value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::TextBuffer as _;

    #[test]
    fn dialog_footer_error_stays_on_the_action_row() {
        let ctx = egui::Context::default();
        let mut heights = Vec::new();
        for error in [None, Some("a service for COM4 already exists")] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    ui.vertical(|ui| {
                        ui.set_width(720.0);
                        let top = ui.cursor().top();
                        dialog_footer(ui, UiTheme::Dark, error, |ui| {
                            let _ = ui.button("Cancel");
                            let _ = ui.button("Add service");
                        });
                        heights.push(ui.min_rect().bottom() - top);
                    });
                },
            );
            output.textures_delta.clear();
        }
        assert!((heights[0] - heights[1]).abs() < 1.0);
    }

    #[test]
    fn signed_numbers_keep_one_leading_minus() {
        assert_eq!(normalize_signed("-5"), "-5");
        assert_eq!(normalize_signed("5-"), "-5");
        assert_eq!(normalize_signed("--5"), "-5");
        assert_eq!(normalize_signed("1-2"), "-12");
        assert_eq!(normalize_signed("42"), "42");
    }

    #[test]
    fn hex_buffer_rejects_noise_and_groups_at_the_end() {
        let mut text = String::new();
        let cursor = {
            let mut buffer = HexBuffer {
                text: &mut text,
                cursor_after: None,
            };
            assert_eq!(buffer.insert_text("ab0x10g", egui::text::CharIndex(0)), 6);
            buffer.cursor_after
        };
        assert_eq!(text, "ab 10 ");
        assert_eq!(cursor, Some(text.chars().count()));

        {
            let mut buffer = HexBuffer {
                text: &mut text,
                cursor_after: None,
            };
            assert_eq!(buffer.insert_text("xyz", egui::text::CharIndex(6)), 0);
        }
        assert_eq!(text, "ab 10 ");
    }

    #[test]
    fn duration_parts_keep_the_literal_unit() {
        assert_eq!(parse_duration_parts("60 m"), Some((60, TimeUnit::Minutes)));
        assert_eq!(parse_duration_parts("60 s"), Some((60, TimeUnit::Seconds)));
        assert_eq!(parse_duration_parts("1 h"), Some((1, TimeUnit::Hours)));
    }

    #[test]
    fn hex_buffer_keeps_the_cursor_after_a_mid_string_insert() {
        let mut text = "01 03".to_string();
        let cursor = {
            let mut buffer = HexBuffer {
                text: &mut text,
                cursor_after: None,
            };
            buffer.insert_text("a", egui::text::CharIndex(3));
            buffer.cursor_after
        };
        assert_eq!(text, "01 a0 3");
        assert_eq!(cursor, Some(4));
    }
}
