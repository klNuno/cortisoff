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

/// A glyph from [`glyph`]: Segoe Fluent Icons on Windows, the bundled
/// Phosphor icons elsewhere.
pub fn icon(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

/// Segoe Fluent Icons code points, which Segoe MDL2 Assets shares on Windows
/// 10 and the bundled icons copy everywhere.
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
    let faces = system_faces();
    let mut fonts = FontDefinitions::default();
    // Each face goes in front of its family, ahead of egui's own.
    let mut add = |name: &str, data: Option<FontData>, family: FontFamily| {
        if let Some(data) = data {
            fonts.font_data.insert(name.to_owned(), Arc::new(data));
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, name.to_owned());
        }
    };
    add("text", faces.text, FontFamily::Proportional);
    add("strong", faces.strong, FontFamily::Name("strong".into()));
    add("mono", faces.mono, FontFamily::Monospace);
    // The bundled icons answer every glyph the system font lacks, and every
    // glyph on a system without one.
    let icons = FontFamily::Name("icons".into());
    add(
        "icons-bundled",
        Some(FontData::from_static(BUNDLED_ICONS)),
        icons.clone(),
    );
    add("icons", faces.icons, icons);

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

/// Phosphor Regular (MIT, `fonts/icons-LICENSE.txt`) cut down to the glyphs
/// in [`glyph`], each moved to its Segoe Fluent code point, so one `char`
/// draws with either font. 8 KB, made by `fonts/make_icons.py`.
const BUNDLED_ICONS: &[u8] = include_bytes!("../fonts/icons.ttf");

/// The faces the window draws with, taken from the system. A missing one
/// leaves egui's bundled fonts, and [`BUNDLED_ICONS`] for the icons.
#[derive(Default)]
struct Faces {
    text: Option<FontData>,
    strong: Option<FontData>,
    mono: Option<FontData>,
    icons: Option<FontData>,
}

/// Segoe UI, its semibold, Consolas and Segoe Fluent Icons: on every
/// Windows install.
#[cfg(windows)]
fn system_faces() -> Faces {
    let dir = std::env::var_os("WINDIR").map(|w| std::path::PathBuf::from(w).join("Fonts"));
    let read = |names: &[&str]| {
        let dir = dir.as_ref()?;
        names
            .iter()
            .find_map(|n| std::fs::read(dir.join(n)).ok())
            .map(FontData::from_owned)
    };
    // Consolas sits about three points higher than Segoe UI on the same
    // line, which shows in a field holding one of each.
    let mono = read(&["consola.ttf"]).map(|f| {
        f.tweak(egui::FontTweak {
            y_offset_factor: 0.2,
            ..Default::default()
        })
    });
    Faces {
        text: read(&["segoeui.ttf"]),
        strong: read(&["seguisb.ttf", "segoeuib.ttf"]),
        mono,
        icons: read(&["SegoeIcons.ttf", "segmdl2.ttf"]),
    }
}

/// San Francisco, the system face since 10.11, one variable file for every
/// weight, and SF Mono beside it.
#[cfg(target_os = "macos")]
fn system_faces() -> Faces {
    let dir = std::path::Path::new("/System/Library/Fonts");
    let read = |name: &str| std::fs::read(dir.join(name)).ok();
    let sans = read("SFNS.ttf");
    Faces {
        text: sans.clone().map(|b| weighted(b, 0, 400.0)),
        strong: sans.map(|b| weighted(b, 0, 600.0)),
        mono: read("SFNSMono.ttf")
            .map(|b| weighted(b, 0, 400.0))
            .or_else(|| read("Menlo.ttc").map(FontData::from_owned)),
        icons: None,
    }
}

/// The desktop's interface and monospace fonts. GNOME and the desktops
/// built on its settings name them; fontconfig's `sans-serif` is often
/// another face (Noto Sans where GNOME shows Ubuntu Sans), so it only
/// answers where nothing names one.
#[cfg(all(unix, not(target_os = "macos")))]
fn system_faces() -> Faces {
    let ui = desktop_font("font-name");
    let sans = ui
        .as_deref()
        .and_then(fc_match)
        .or_else(|| fc_match("sans-serif"));
    let strong = match &sans {
        // One variable file holds the semibold too.
        Some((bytes, index)) if has_weight_axis(bytes, *index) => {
            Some(weighted(bytes.clone(), *index, 600.0))
        }
        _ => fc_match(&format!(
            "{}:weight=180",
            ui.as_deref().unwrap_or("sans-serif")
        ))
        .map(|(b, i)| face(b, i)),
    };
    let mono = desktop_font("monospace-font-name")
        .as_deref()
        .and_then(fc_match)
        .or_else(|| fc_match("monospace"));
    Faces {
        text: sans.map(|(b, i)| weighted(b, i, 400.0)),
        strong,
        mono: mono.map(|(b, i)| weighted(b, i, 400.0)),
        icons: None,
    }
}

/// A font family from `org.gnome.desktop.interface`, on a desktop that reads
/// those settings. Elsewhere gsettings, when installed, answers GNOME's
/// defaults, which are not what that desktop shows.
#[cfg(all(unix, not(target_os = "macos")))]
fn desktop_font(key: &str) -> Option<String> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let gnome_settings = desktop.split(':').any(|d| {
        ["GNOME", "Unity", "Budgie", "Pantheon"]
            .iter()
            .any(|g| d.eq_ignore_ascii_case(g))
    });
    if !gnome_settings {
        return None;
    }
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    font_family(String::from_utf8(out.stdout).ok()?.trim())
}

/// The family in a Pango font name as gsettings prints it: `'Ubuntu Sans 11'`
/// is `Ubuntu Sans`.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn font_family(value: &str) -> Option<String> {
    let value = value.trim().trim_matches('\'').trim();
    let name = match value.rsplit_once(' ') {
        Some((name, size)) if size.parse::<f32>().is_ok() => name.trim(),
        _ => value,
    };
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(not(any(windows, unix)))]
fn system_faces() -> Faces {
    Faces::default()
}

/// The file and face index fontconfig picks for `pattern`. `None` without
/// fontconfig, which leaves egui's fonts.
#[cfg(all(unix, not(target_os = "macos")))]
fn fc_match(pattern: &str) -> Option<(Vec<u8>, u32)> {
    let out = std::process::Command::new("fc-match")
        .args(["--format=%{file}\n%{index}", pattern])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let mut lines = text.lines();
    let file = lines.next()?;
    let index = lines.next().and_then(|i| i.parse().ok()).unwrap_or(0);
    // Variable faces carry their instance in the upper half of the index.
    Some((std::fs::read(file).ok()?, index & 0xffff))
}

#[cfg(unix)]
fn face(bytes: Vec<u8>, index: u32) -> FontData {
    FontData {
        index,
        ..FontData::from_owned(bytes)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn has_weight_axis(bytes: &[u8], index: u32) -> bool {
    face(bytes.to_vec(), index)
        .variation_axes()
        .iter()
        .any(|a| a.tag == egui::epaint::text::Tag::new(b"wght"))
}

/// `bytes` at `weight` when it is a variable font with a weight axis, as it
/// is otherwise.
#[cfg(unix)]
fn weighted(bytes: Vec<u8>, index: u32, weight: f32) -> FontData {
    let data = face(bytes, index);
    let wght = data
        .variation_axes()
        .into_iter()
        .find(|a| a.tag == egui::epaint::text::Tag::new(b"wght"));
    match wght {
        Some(axis) => {
            let w = weight.clamp(axis.range.min, axis.range.max);
            data.tweak(egui::FontTweak {
                coords: egui::epaint::text::VariationCoords::new([(b"wght", w)]),
                ..Default::default()
            })
        }
        None => data,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::text::{Fonts, TextOptions};

    /// What gsettings prints for GNOME's fonts, down to the family fontconfig
    /// is asked for.
    #[test]
    fn a_pango_font_name_gives_its_family() {
        assert_eq!(
            font_family("'Ubuntu Sans 11'\n").as_deref(),
            Some("Ubuntu Sans")
        );
        assert_eq!(
            font_family("'Adwaita Mono 11'").as_deref(),
            Some("Adwaita Mono")
        );
        assert_eq!(
            font_family("'Cantarell 10.5'").as_deref(),
            Some("Cantarell")
        );
        assert_eq!(font_family("'Inter'").as_deref(), Some("Inter"));
        assert_eq!(font_family("''"), None);
    }

    /// Every glyph the window names exists in the bundled icons, so a system
    /// without Segoe Fluent Icons draws no empty box.
    #[test]
    fn the_bundled_icons_hold_every_glyph() {
        let mut defs = FontDefinitions::empty();
        defs.font_data.insert(
            "icons".into(),
            Arc::new(FontData::from_static(BUNDLED_ICONS)),
        );
        defs.families
            .insert(FontFamily::Proportional, vec!["icons".into()]);
        let mut fonts = Fonts::new(TextOptions::default(), defs);
        let font = FontId::proportional(16.0);

        let glyphs: Vec<char> = include_str!("theme.rs")
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub const "))
            .filter_map(|l| l.split_once(r": char = '\u{"))
            .filter_map(|(_, rest)| u32::from_str_radix(rest.split('}').next()?, 16).ok())
            .filter_map(char::from_u32)
            .collect();
        assert!(glyphs.len() >= 29, "{glyphs:?}");
        let missing: Vec<String> = glyphs
            .iter()
            .filter(|&&c| !fonts.has_glyph(&font, c))
            .map(|c| format!("U+{:04X}", *c as u32))
            .collect();
        assert!(missing.is_empty(), "missing from icons.ttf: {missing:?}");
    }
}
