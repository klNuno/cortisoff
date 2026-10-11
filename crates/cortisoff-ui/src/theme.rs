//! Colours, type and spacing, in one place.
//!
//! The chrome is monochrome. Hue is kept for status (holding, waiting, paused,
//! failed) and for data whose colour is the point, such as a colour
//! temperature. Everything here is a constant so a capture and the real window
//! cannot disagree.

use egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};
use std::sync::Arc;

pub const BG: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x11);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x13, 0x14, 0x17);
pub const CARD: Color32 = Color32::from_rgb(0x18, 0x19, 0x1d);
pub const CARD_BORDER: Color32 = Color32::from_rgb(0x24, 0x26, 0x2b);
pub const FIELD: Color32 = Color32::from_rgb(0x21, 0x23, 0x28);
pub const FIELD_HOVER: Color32 = Color32::from_rgb(0x2a, 0x2c, 0x32);
pub const SELECTED: Color32 = Color32::from_rgb(0x22, 0x24, 0x29);

pub const TEXT: Color32 = Color32::from_rgb(0xec, 0xed, 0xef);
pub const MUTED: Color32 = Color32::from_rgb(0xa0, 0xa3, 0xa9);
pub const SUBTLE: Color32 = Color32::from_rgb(0x6c, 0x6f, 0x76);

pub const HOLDING: Color32 = Color32::from_rgb(0x3d, 0xd6, 0x8c);
pub const WAITING: Color32 = Color32::from_rgb(0x8b, 0x8e, 0x95);
pub const PAUSED: Color32 = Color32::from_rgb(0xf5, 0xa5, 0x24);
pub const FAILED: Color32 = Color32::from_rgb(0xf3, 0x5b, 0x5b);

pub const RADIUS_CARD: CornerRadius = CornerRadius::same(10);
pub const RADIUS_CONTROL: CornerRadius = CornerRadius::same(6);

/// The semibold face, for titles and values a person reads first.
pub fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("strong".into()))
}

pub fn body(size: f32) -> FontId {
    FontId::proportional(size)
}

/// A glyph from the system icon font.
pub fn icon(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

/// Segoe Fluent Icons code points, which Segoe MDL2 Assets shares on Windows 10.
pub mod glyph {
    pub const HOME: char = '\u{E80F}';
    pub const SETTINGS: char = '\u{E713}';
    pub const ADD: char = '\u{E710}';
    pub const MOON: char = '\u{E708}';
    pub const MONITOR: char = '\u{E7F4}';
    pub const PICTURE: char = '\u{E8B9}';
    pub const BOLT: char = '\u{E945}';
    pub const PAUSE: char = '\u{E769}';
    pub const CHEVRON_RIGHT: char = '\u{E76C}';
    pub const CHEVRON_DOWN: char = '\u{E70D}';
    pub const CLOSE: char = '\u{E711}';
    pub const CLOCK: char = '\u{E823}';
    pub const GAME: char = '\u{E7FC}';
    pub const VOLUME: char = '\u{E767}';
    pub const LOCK: char = '\u{E72E}';
    pub const FULLSCREEN: char = '\u{E740}';
    pub const ARROW: char = '\u{E72A}';
    pub const PLUGIN: char = '\u{EA86}';
    pub const CHECK: char = '\u{E73E}';
    pub const KEYBOARD: char = '\u{E765}';
    pub const PLAY: char = '\u{E768}';
    pub const DELETE: char = '\u{E74D}';
    pub const EDIT: char = '\u{E70F}';
    pub const SEARCH: char = '\u{E721}';
    pub const WARNING: char = '\u{E7BA}';
    pub const REFRESH: char = '\u{E72C}';
    pub const SUN: char = '\u{E706}';
    pub const POWER: char = '\u{E7E8}';
    pub const BATTERY: char = '\u{E83F}';

    /// The glyph for an icon name a plugin uses, or a generic plugin glyph.
    pub fn named(name: &str) -> char {
        match name {
            "home" => HOME,
            "settings" => SETTINGS,
            "add" => ADD,
            "moon" => MOON,
            "monitor" => MONITOR,
            "picture" => PICTURE,
            "bolt" => BOLT,
            "pause" => PAUSE,
            "close" => CLOSE,
            "clock" => CLOCK,
            "game" => GAME,
            "volume" => VOLUME,
            "lock" => LOCK,
            "fullscreen" => FULLSCREEN,
            "arrow" => ARROW,
            "plugin" => PLUGIN,
            "check" => CHECK,
            "keyboard" => KEYBOARD,
            "play" => PLAY,
            "delete" => DELETE,
            "edit" => EDIT,
            "search" => SEARCH,
            "warning" => WARNING,
            "refresh" => REFRESH,
            "sun" => SUN,
            "power" => POWER,
            "battery" => BATTERY,
            _ => PLUGIN,
        }
    }
}

/// Fonts and style. Called once, before the first frame.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, strong(22.0)),
            (TextStyle::Body, body(14.0)),
            (TextStyle::Button, body(14.0)),
            (TextStyle::Small, body(12.5)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.combo_height = 240.0;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.window_stroke = Stroke::new(1.0, CARD_BORDER);
        v.window_corner_radius = RADIUS_CARD;
        v.menu_corner_radius = RADIUS_CONTROL;
        v.extreme_bg_color = FIELD;
        v.faint_bg_color = CARD;
        v.selection.bg_fill = Color32::from_rgb(0x3a, 0x3d, 0x45);
        v.selection.stroke = Stroke::new(1.0, TEXT);
        v.hyperlink_color = TEXT;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CARD;
        w.noninteractive.weak_bg_fill = CARD;
        w.noninteractive.bg_stroke = Stroke::new(1.0, CARD_BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, MUTED);
        for (state, fill) in [
            (&mut w.inactive, FIELD),
            (&mut w.hovered, FIELD_HOVER),
            (&mut w.active, FIELD_HOVER),
            (&mut w.open, FIELD_HOVER),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::NONE;
            state.corner_radius = RADIUS_CONTROL;
            state.fg_stroke = Stroke::new(1.0, TEXT);
            state.expansion = 0.0;
        }
    });
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: Option<Vec<u8>>, family: FontFamily| {
        if let Some(bytes) = bytes {
            fonts
                .font_data
                .insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, name.to_owned());
        }
    };
    add(
        "segoe",
        system_font(&["segoeui.ttf"]),
        FontFamily::Proportional,
    );
    add(
        "segoe-strong",
        system_font(&["seguisb.ttf", "segoeuib.ttf"]),
        FontFamily::Name("strong".into()),
    );
    add(
        "icons",
        system_font(&["SegoeIcons.ttf", "segmdl2.ttf"]),
        FontFamily::Name("icons".into()),
    );
    add(
        "consolas",
        system_font(&["consola.ttf"]),
        FontFamily::Monospace,
    );
    // Consolas sits about three points higher than Segoe UI on the same
    // line, which shows in a field holding one of each.
    if let Some(consolas) = fonts.font_data.get_mut("consolas") {
        Arc::make_mut(consolas).tweak.y_offset_factor = 0.2;
    }

    // A family that names no font panics at the first glyph, so every named
    // family falls back to the proportional list.
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    for name in ["strong", "icons"] {
        fonts
            .families
            .entry(FontFamily::Name(name.into()))
            .or_default()
            .extend(fallback.iter().cloned());
    }
    fonts
}

/// A font from the Windows font directory. `None` elsewhere, or when it is
/// missing, and egui's bundled fonts stand in.
fn system_font(names: &[&str]) -> Option<Vec<u8>> {
    let dir = std::env::var_os("WINDIR").map(|w| std::path::PathBuf::from(w).join("Fonts"))?;
    names.iter().find_map(|n| std::fs::read(dir.join(n)).ok())
}

/// The colour a black body glows at, which is what a colour temperature looks
/// like on a screen. Tanner Helland's fit, good to a few percent from 1000 to
/// 40000 K, which is all a swatch needs.
pub fn kelvin(k: f32) -> Color32 {
    let t = k.clamp(1000.0, 40000.0) / 100.0;
    let r = if t <= 66.0 {
        255.0
    } else {
        329.698_73 * (t - 60.0).powf(-0.133_204_76)
    };
    let g = if t <= 66.0 {
        99.470_8 * t.ln() - 161.119_57
    } else {
        288.122_16 * (t - 60.0).powf(-0.075_514_85)
    };
    let b = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_73 * (t - 10.0).ln() - 305.044_8
    };
    let c = |x: f32| x.clamp(0.0, 255.0) as u8;
    Color32::from_rgb(c(r), c(g), c(b))
}
