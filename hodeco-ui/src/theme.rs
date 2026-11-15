//! Application theme and semantic colours.

use crate::client::Phase;
use egui::{Color32, Context, FontId, Stroke, TextStyle, ThemePreference, Visuals};
use hodeco_codec::parsed::FieldKind;
use hodeco_contract::domain::{MessageState, ServiceState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiTheme {
    #[default]
    Dark,
    Light,
}

impl UiTheme {
    pub fn other(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }

    pub fn apply(self, ctx: &Context) {
        let (theme, mut visuals) = match self {
            Self::Dark => (ThemePreference::Dark, Visuals::dark()),
            Self::Light => (ThemePreference::Light, Visuals::light()),
        };
        let palette = palette(self);
        visuals.panel_fill = palette.background;
        visuals.window_fill = palette.panel;
        visuals.faint_bg_color = palette.faint;
        visuals.extreme_bg_color = palette.input;
        visuals.code_bg_color = palette.faint;
        visuals.selection.bg_fill = match self {
            Self::Dark => palette.accent.gamma_multiply(0.45),
            Self::Light => Color32::from_rgb(0xd4, 0xe6, 0xff),
        };
        visuals.selection.stroke = Stroke::new(1.0, palette.accent);
        visuals.hyperlink_color = palette.accent;
        visuals.window_stroke.color = palette.border;
        visuals.widgets.noninteractive.bg_stroke.color = palette.border;
        visuals.widgets.inactive.bg_fill = palette.panel;
        visuals.widgets.inactive.weak_bg_fill = palette.faint;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, palette.border);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);
        visuals.widgets.hovered.bg_fill = palette.faint;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.accent);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, palette.text);
        visuals.widgets.active.bg_fill = palette.faint;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, palette.accent);
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, palette.text);
        visuals.widgets.open.bg_stroke = Stroke::new(1.0, palette.accent);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text);
        ctx.set_theme(theme);
        ctx.set_visuals(visuals);

        let theme_key = match self {
            Self::Dark => egui::Theme::Dark,
            Self::Light => egui::Theme::Light,
        };
        let mut style = (*ctx.style_of(theme_key)).clone();
        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(23.0)),
            (TextStyle::Body, FontId::proportional(15.5)),
            (TextStyle::Monospace, FontId::monospace(14.5)),
            (TextStyle::Button, FontId::proportional(15.5)),
            (TextStyle::Small, FontId::proportional(13.5)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 5.0);
        ctx.set_style_of(theme_key, style);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub background: Color32,
    pub panel: Color32,
    pub faint: Color32,
    pub input: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub green: Color32,
    pub amber: Color32,
    pub red: Color32,
    pub blue: Color32,
    pub purple: Color32,
    pub cyan: Color32,
}

#[cfg(not(target_arch = "wasm32"))]
const CJK_FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\NotoSansJP-VF.ttf",
    r"C:\Windows\Fonts\YuGothM.ttc",
    r"C:\Windows\Fonts\meiryo.ttc",
    r"C:\Windows\Fonts\msgothic.ttc",
    r"C:\Windows\Fonts\malgun.ttf",
    r"C:\Windows\Fonts\msyh.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
];

#[cfg(not(target_arch = "wasm32"))]
fn read_system_cjk_font() -> Option<(&'static str, Vec<u8>)> {
    CJK_FONT_CANDIDATES
        .iter()
        .find_map(|path| std::fs::read(path).ok().map(|bytes| (*path, bytes)))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn install_native_fallback_fonts(ctx: &Context) {
    // The bundled Latin/Cyrillic fonts have no CJK coverage. The web build can
    // fall back through the browser glyph rasterizer, but native egui cannot,
    // so install a system CJK font as the last fallback when one is available.
    let Some((path, bytes)) = read_system_cjk_font() else {
        log::info!("no system CJK font found; Japanese glyphs may render as boxes");
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "system-cjk".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("system-cjk".to_owned());
    }
    ctx.set_fonts(fonts);
    log::info!("loaded system CJK fallback font from {path}");
}

pub fn palette(theme: UiTheme) -> Palette {
    match theme {
        UiTheme::Dark => Palette {
            background: Color32::from_rgb(0x14, 0x17, 0x1c),
            panel: Color32::from_rgb(0x1b, 0x1f, 0x26),
            faint: Color32::from_rgb(0x23, 0x28, 0x31),
            input: Color32::from_rgb(0x10, 0x13, 0x18),
            border: Color32::from_rgb(0x31, 0x38, 0x44),
            text: Color32::from_rgb(0xe7, 0xea, 0xf0),
            muted: Color32::from_rgb(0x94, 0x9e, 0xae),
            accent: Color32::from_rgb(0x5f, 0xa8, 0xff),
            green: Color32::from_rgb(0x4c, 0xc3, 0x8a),
            amber: Color32::from_rgb(0xe5, 0xb4, 0x54),
            red: Color32::from_rgb(0xef, 0x6b, 0x73),
            blue: Color32::from_rgb(0x64, 0xa8, 0xff),
            purple: Color32::from_rgb(0xb4, 0x8d, 0xf3),
            cyan: Color32::from_rgb(0x5b, 0xc8, 0xc8),
        },
        UiTheme::Light => Palette {
            background: Color32::from_rgb(0xf3, 0xf5, 0xf8),
            panel: Color32::from_rgb(0xff, 0xff, 0xff),
            faint: Color32::from_rgb(0xe9, 0xed, 0xf3),
            input: Color32::from_rgb(0xff, 0xff, 0xff),
            border: Color32::from_rgb(0xd3, 0xd9, 0xe3),
            text: Color32::from_rgb(0x1d, 0x24, 0x30),
            muted: Color32::from_rgb(0x64, 0x70, 0x82),
            accent: Color32::from_rgb(0x1f, 0x6f, 0xeb),
            green: Color32::from_rgb(0x1a, 0x7f, 0x4b),
            amber: Color32::from_rgb(0xa1, 0x5c, 0x00),
            red: Color32::from_rgb(0xc0, 0x39, 0x2b),
            blue: Color32::from_rgb(0x1f, 0x6f, 0xeb),
            purple: Color32::from_rgb(0x7c, 0x3a, 0xed),
            cyan: Color32::from_rgb(0x0e, 0x74, 0x90),
        },
    }
}

pub fn service_state_color(theme: UiTheme, state: ServiceState) -> Color32 {
    let palette = palette(theme);
    match state {
        ServiceState::Running => palette.green,
        ServiceState::Starting => palette.amber,
        ServiceState::Error => palette.red,
        ServiceState::Stopped => palette.muted,
    }
}

pub fn message_state_color(theme: UiTheme, state: MessageState) -> Color32 {
    let palette = palette(theme);
    match state {
        MessageState::Incoming => palette.blue,
        MessageState::Outgoing => palette.green,
        MessageState::OutgoingPending => palette.amber,
        MessageState::OutgoingFailed | MessageState::OutgoingRejected => palette.red,
        MessageState::OutgoingTimedOut => palette.purple,
    }
}

pub fn phase_color(theme: UiTheme, phase: Phase) -> Color32 {
    let palette = palette(theme);
    match phase {
        Phase::Ready => palette.green,
        Phase::Syncing => palette.cyan,
        Phase::Connecting => palette.amber,
        Phase::Backoff => palette.red,
        Phase::Disconnected => palette.muted,
    }
}

pub fn field_color(theme: UiTheme, kind: FieldKind) -> Color32 {
    let palette = palette(theme);
    match kind {
        FieldKind::Header => palette.muted,
        FieldKind::Length => palette.amber,
        FieldKind::FrameType => palette.purple,
        FieldKind::Command => palette.accent,
        FieldKind::Node => palette.cyan,
        FieldKind::CommandClass => palette.blue,
        FieldKind::Value => palette.green,
        FieldKind::Checksum => palette.red,
        FieldKind::Control => palette.purple,
    }
}

pub fn field_label(kind: FieldKind) -> &'static str {
    match kind {
        FieldKind::Header => "Header",
        FieldKind::Length => "Length",
        FieldKind::FrameType => "Type",
        FieldKind::Command => "Command",
        FieldKind::Node => "Node",
        FieldKind::CommandClass => "Command class",
        FieldKind::Value => "Value",
        FieldKind::Checksum => "Checksum",
        FieldKind::Control => "Control",
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn system_cjk_font_renders_japanese_when_available() {
        if read_system_cjk_font().is_none() {
            return;
        }
        let ctx = Context::default();
        install_native_fallback_fonts(&ctx);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| {
            ctx.fonts_mut(|fonts| {
                assert!(fonts.has_glyph(&egui::FontId::proportional(15.5), '日'));
                assert!(fonts.has_glyph(&egui::FontId::monospace(14.0), '本'));
            });
        });
        output.textures_delta.clear();
    }
}
