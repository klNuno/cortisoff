//! Plugins written in Lua, loaded from a folder and reloaded when it changes.
//!
//! Spike. A plugin is a folder holding `plugin.toml`, readable without
//! running anything, and an entry script returning a table:
//!
//! ```lua
//! local plugin = { settings = { night = 3200 } }
//! function plugin.status(s) return { line = "...", detail = "...", state = "holding" } end
//! function plugin.page(ui, s) ui:card("Schedule", function(ui) ... end) end
//! return plugin
//! ```
//!
//! `page` draws with the same widgets as the shell, through `ui`, and paints
//! freely through `ui:canvas`. Settings live in a table the host owns, so a
//! reload keeps every value the user set. A script that fails to load keeps
//! the last version that worked, and the page says why.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{
    self, vec2, Align2, Color32, CornerRadius, Mesh, Painter, Pos2, Rect, RichText, Sense, Shape,
    Stroke, Ui,
};
use mlua::{Function, Lua, RegistryKey, Table, UserData, UserDataMethods, UserDataRegistry, Value};
use serde::Deserialize;

use crate::plugins::{PluginUi, Status};
use crate::theme::{self, glyph};
use crate::widgets::{self, Track};

/// `plugin.toml`: what the shell needs to list a plugin without running it.
#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub icon: Option<String>,
    /// `windows`, `linux`, `macos`: the values of `std::env::consts::OS`.
    pub platforms: Vec<String>,
    #[serde(default = "default_entry")]
    pub entry: String,
}

fn default_entry() -> String {
    "init.lua".into()
}

impl Manifest {
    pub fn supports(&self, os: &str) -> bool {
        self.platforms.iter().any(|p| p == os)
    }
}

pub struct LuaPlugin {
    dir: PathBuf,
    manifest: Option<Manifest>,
    lua: Lua,
    /// The table the script returned, from the last load that worked.
    module: Option<RegistryKey>,
    /// The plugin's settings, kept across reloads.
    settings: Option<RegistryKey>,
    loaded_at: Option<SystemTime>,
    checked: Option<Instant>,
    error: Option<String>,
}

impl LuaPlugin {
    /// Loads the plugin in `dir`. Never fails: what went wrong is shown on
    /// the plugin's page.
    pub fn load(dir: impl Into<PathBuf>, now_minute: u32) -> Self {
        let dir = dir.into();
        let lua = Lua::new();
        let mut plugin = Self {
            manifest: None,
            module: None,
            settings: None,
            loaded_at: None,
            checked: None,
            error: None,
            lua,
            dir,
        };
        if let Err(e) = plugin.install_api(now_minute) {
            plugin.error = Some(e.to_string());
            return plugin;
        }
        match std::fs::read_to_string(plugin.dir.join("plugin.toml"))
            .map_err(|e| e.to_string())
            .and_then(|s| toml::from_str::<Manifest>(&s).map_err(|e| e.to_string()))
        {
            Ok(m) => plugin.manifest = Some(m),
            Err(e) => {
                plugin.error = Some(format!("plugin.toml: {e}"));
                return plugin;
            }
        }
        plugin.refresh();
        plugin
    }

    pub fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn entry(&self) -> Option<PathBuf> {
        self.manifest.as_ref().map(|m| self.dir.join(&m.entry))
    }

    /// The `cortisoff` table every script sees.
    fn install_api(&self, now_minute: u32) -> mlua::Result<()> {
        let lua = &self.lua;
        let api = lua.create_table()?;
        api.set(
            "kelvin",
            lua.create_function(|_, k: f32| Ok(hex(theme::kelvin(k))))?,
        )?;
        api.set(
            "alpha",
            lua.create_function(|_, (c, a): (Value, f32)| Ok(hex(color(&c)?.gamma_multiply(a))))?,
        )?;
        // The prototype's clock is fixed so captures do not move.
        api.set("now", lua.create_function(move |_, ()| Ok(now_minute))?)?;
        api.set("platform", std::env::consts::OS)?;
        lua.globals().set("cortisoff", api)
    }

    /// Reloads the entry script if it changed on disk since the last load.
    pub fn refresh(&mut self) {
        let Some(entry) = self.entry() else { return };
        let modified = std::fs::metadata(&entry).and_then(|m| m.modified()).ok();
        if modified.is_some() && modified == self.loaded_at {
            return;
        }
        self.loaded_at = modified;
        match self.compile(&entry) {
            Ok(()) => self.error = None,
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn compile(&mut self, entry: &Path) -> mlua::Result<()> {
        let source = std::fs::read_to_string(entry).map_err(mlua::Error::external)?;
        let name = entry
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let module: Table = self.lua.load(&source).set_name(format!("@{name}")).eval()?;
        // New defaults first, then every value already set, so a reload
        // only adds what the new version introduced.
        let fresh = self.lua.create_table()?;
        if let Ok(defaults) = module.get::<Table>("settings") {
            for pair in defaults.pairs::<Value, Value>() {
                let (k, v) = pair?;
                fresh.set(k, v)?;
            }
        }
        if let Some(old) = &self.settings {
            let old: Table = self.lua.registry_value(old)?;
            for pair in old.pairs::<Value, Value>() {
                let (k, v) = pair?;
                fresh.set(k, v)?;
            }
        }
        self.settings = Some(self.lua.create_registry_value(fresh)?);
        self.module = Some(self.lua.create_registry_value(module)?);
        Ok(())
    }

    fn function(&self, name: &str) -> mlua::Result<Option<(Function, Table)>> {
        let (Some(module), Some(settings)) = (&self.module, &self.settings) else {
            return Ok(None);
        };
        let module: Table = self.lua.registry_value(module)?;
        let settings: Table = self.lua.registry_value(settings)?;
        Ok(module.get::<Option<Function>>(name)?.map(|f| (f, settings)))
    }

    fn lua_status(&self) -> mlua::Result<Status> {
        let Some((f, settings)) = self.function("status")? else {
            return Ok(Status {
                line: String::new(),
                detail: String::new(),
                color: theme::WAITING,
            });
        };
        let t: Table = f.call(settings)?;
        let state: Option<String> = t.get("state")?;
        Ok(Status {
            line: t.get::<Option<String>>("line")?.unwrap_or_default(),
            detail: t.get::<Option<String>>("detail")?.unwrap_or_default(),
            color: match state.as_deref() {
                Some("holding") => theme::HOLDING,
                Some("paused") => theme::PAUSED,
                Some("failed") => theme::FAILED,
                _ => theme::WAITING,
            },
        })
    }

    fn lua_page(&self, ui: &mut Ui) -> mlua::Result<()> {
        let Some((f, settings)) = self.function("page")? else {
            return Ok(());
        };
        self.lua.scope(|scope| {
            let handle = scope.create_userdata(UiRef(ui))?;
            f.call::<()>((handle, settings))
        })
    }
}

impl PluginUi for LuaPlugin {
    fn name(&self) -> &str {
        match &self.manifest {
            Some(m) => &m.name,
            None => self
                .dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("plugin"),
        }
    }

    fn icon(&self) -> char {
        icon(self.manifest.as_ref().and_then(|m| m.icon.as_deref()))
    }

    fn poll(&mut self) {
        // A stat every few hundred milliseconds, only while the window is open.
        if self
            .checked
            .is_some_and(|t| t.elapsed() < Duration::from_millis(300))
        {
            return;
        }
        self.checked = Some(Instant::now());
        self.refresh();
    }

    fn status(&self) -> Status {
        if let Some(m) = &self.manifest {
            if !m.supports(std::env::consts::OS) {
                return Status {
                    line: "Not for this system".into(),
                    detail: format!("Made for {}", m.platforms.join(", ")),
                    color: theme::WAITING,
                };
            }
        }
        if self.module.is_none() {
            return Status {
                line: "Failed to load".into(),
                detail: self.error.clone().unwrap_or_default(),
                color: theme::FAILED,
            };
        }
        self.lua_status().unwrap_or_else(|e| Status {
            line: "Script error".into(),
            detail: e.to_string(),
            color: theme::FAILED,
        })
    }

    fn page(&mut self, ui: &mut Ui) {
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        if let Some(error) = &self.error {
            error_card(ui, error);
        }
        if let Err(e) = self.lua_page(ui) {
            error_card(ui, &e.to_string());
        }
    }
}

fn error_card(ui: &mut Ui, error: &str) {
    widgets::card(ui, Some("This plugin has an error"), |ui| {
        ui.label(
            RichText::new(error)
                .font(egui::FontId::monospace(12.5))
                .color(theme::FAILED),
        );
        widgets::hint(
            ui,
            "The last version that loaded keeps running. Save the file to retry.",
        );
    });
    ui.add_space(12.0);
}

/// The `ui` a script draws with. Lives for one call, then Lua loses it.
struct UiRef<'a>(&'a mut Ui);

/// Runs `f` with a `ui` for `ui`, nested inside the current one.
fn nested(lua: &Lua, ui: &mut Ui, f: &Function) -> mlua::Result<()> {
    lua.scope(|scope| {
        let handle = scope.create_userdata(UiRef(ui))?;
        f.call::<()>(handle)
    })
}

impl UserData for UiRef<'_> {
    fn register(reg: &mut UserDataRegistry<Self>) {
        reg.add_method_mut(
            "header",
            |lua, this, (icon_name, title, summary, right): (String, String, String, Option<Function>)| {
                let mut out = Ok(());
                widgets::page_header(this.0, icon(Some(&icon_name)), &title, &summary, |ui| {
                    if let Some(right) = &right {
                        out = nested(lua, ui, right);
                    }
                });
                out
            },
        );
        reg.add_method_mut(
            "card",
            |lua, this, (title, f): (Option<String>, Function)| {
                widgets::card(this.0, title.as_deref(), |ui| nested(lua, ui, &f))
            },
        );
        reg.add_method_mut(
            "row",
            |lua, this, (label, hint, f): (String, Option<String>, Function)| {
                let mut out = Ok(());
                widgets::row(this.0, &label, hint.as_deref(), |ui| {
                    out = nested(lua, ui, &f)
                });
                out
            },
        );
        reg.add_method_mut("horizontal", |lua, this, f: Function| {
            this.0.horizontal(|ui| nested(lua, ui, &f)).inner
        });
        reg.add_method_mut("wrap", |lua, this, f: Function| {
            this.0.horizontal_wrapped(|ui| nested(lua, ui, &f)).inner
        });
        reg.add_method_mut(
            "vertical",
            |lua, this, (f, gap): (Function, Option<f32>)| {
                this.0
                    .vertical(|ui| {
                        if let Some(gap) = gap {
                            ui.spacing_mut().item_spacing.y = gap;
                        }
                        nested(lua, ui, &f)
                    })
                    .inner
            },
        );
        reg.add_method_mut("label", |_, this, (text, opts): (String, Option<Table>)| {
            let style = TextStyle::read(opts.as_ref(), 14.0, theme::TEXT)?;
            this.0
                .label(RichText::new(text).font(style.font).color(style.color));
            Ok(())
        });
        reg.add_method_mut("hint", |_, this, text: String| {
            widgets::hint(this.0, &text);
            Ok(())
        });
        reg.add_method_mut("space", |_, this, px: f32| {
            this.0.add_space(px);
            Ok(())
        });
        reg.add_method_mut("divider", |_, this, ()| {
            widgets::divider(this.0);
            Ok(())
        });
        reg.add_method_mut("width", |_, this, ()| Ok(this.0.available_width()));
        reg.add_method_mut("switch", |_, this, mut on: bool| {
            widgets::switch(this.0, &mut on);
            Ok(on)
        });
        reg.add_method_mut(
            "button",
            |_, this, (text, icon_name): (String, Option<String>)| {
                let glyph = icon_name.as_deref().map(|n| icon(Some(n)));
                Ok(widgets::button(this.0, glyph, &text).clicked())
            },
        );
        reg.add_method_mut(
            "chip",
            |_, this, (text, icon_name): (String, Option<String>)| {
                let glyph = icon_name.as_deref().map(|n| icon(Some(n)));
                Ok(widgets::chip(this.0, glyph, &text).clicked())
            },
        );
        reg.add_method_mut("field", |_, this, (text, width): (String, Option<f32>)| {
            Ok(widgets::field(this.0, &text, width.unwrap_or(96.0)).clicked())
        });
        reg.add_method_mut("slider", |_, this, (mut value, opts): (f32, Table)| {
            let min: f32 = opts.get("min")?;
            let max: f32 = opts.get("max")?;
            let step: f32 = opts.get::<Option<f32>>("step")?.unwrap_or(1.0);
            let track = match opts.get::<Option<String>>("track")?.as_deref() {
                Some("kelvin") => Track::Kelvin,
                _ => Track::Plain,
            };
            let text: Option<Function> = opts.get("text")?;
            widgets::slider(
                this.0,
                &mut value,
                min..=max,
                step,
                track,
                |v| match &text {
                    Some(f) => f.call::<String>(v).unwrap_or_else(|e| e.to_string()),
                    None => format!("{v}"),
                },
            );
            Ok(value)
        });
        // Free painting: `ui:canvas(width or nil, height, function(p) ... end)`.
        reg.add_method_mut(
            "canvas",
            |lua, this, (width, height, f): (Option<f32>, f32, Function)| {
                let width = width.unwrap_or_else(|| this.0.available_width());
                let (rect, _) = this
                    .0
                    .allocate_exact_size(vec2(width, height), Sense::hover());
                // Clipped to the page, not the canvas: a label may sit
                // above the area it describes, as with egui's own painter.
                let canvas = Canvas {
                    painter: this.0.painter().clone(),
                    rect,
                };
                lua.scope(|scope| f.call::<()>(scope.create_userdata(canvas)?))
            },
        );
    }
}

/// A painter whose coordinates start at the canvas' top left corner.
struct Canvas {
    painter: Painter,
    rect: Rect,
}

impl Canvas {
    fn at(&self, x: f32, y: f32) -> Pos2 {
        self.rect.min + vec2(x, y)
    }

    fn points(&self, flat: &Table) -> mlua::Result<Vec<Pos2>> {
        let n = flat.raw_len();
        let mut out = Vec::with_capacity(n / 2);
        for i in (1..n).step_by(2) {
            out.push(self.at(flat.raw_get(i)?, flat.raw_get(i + 1)?));
        }
        Ok(out)
    }
}

impl UserData for Canvas {
    fn register(reg: &mut UserDataRegistry<Self>) {
        reg.add_method("size", |_, this, ()| {
            Ok((this.rect.width(), this.rect.height()))
        });
        reg.add_method("line", |_, this, (flat, width, c): (Table, f32, Value)| {
            let points = this.points(&flat)?;
            this.painter
                .add(Shape::line(points, Stroke::new(width, color(&c)?)));
            Ok(())
        });
        reg.add_method(
            "vline",
            |_, this, (x, top, bottom, width, c): (f32, f32, f32, f32, Value)| {
                let at = this.at(x, top);
                this.painter.vline(
                    at.x,
                    at.y..=this.rect.min.y + bottom,
                    Stroke::new(width, color(&c)?),
                );
                Ok(())
            },
        );
        reg.add_method(
            "rect",
            |_, this, (x, y, w, h, c, radius): (f32, f32, f32, f32, Value, Option<u8>)| {
                let rect = Rect::from_min_size(this.at(x, y), vec2(w, h));
                this.painter
                    .rect_filled(rect, CornerRadius::same(radius.unwrap_or(0)), color(&c)?);
                Ok(())
            },
        );
        reg.add_method(
            "circle",
            |_, this, (x, y, r, fill, stroke, width): (f32, f32, f32, Value, Value, Option<f32>)| {
                let center = this.at(x, y);
                if !fill.is_nil() {
                    this.painter.circle_filled(center, r, color(&fill)?);
                }
                if !stroke.is_nil() {
                    this.painter.circle_stroke(
                        center,
                        r,
                        Stroke::new(width.unwrap_or(1.0), color(&stroke)?),
                    );
                }
                Ok(())
            },
        );
        reg.add_method(
            "text",
            |_, this, (x, y, text, opts): (f32, f32, String, Option<Table>)| {
                let style = TextStyle::read(opts.as_ref(), 13.0, theme::TEXT)?;
                let align = match opts.as_ref() {
                    Some(o) => o.get::<Option<String>>("align")?,
                    None => None,
                };
                this.painter.text(
                    this.at(x, y),
                    align2(align.as_deref()),
                    text,
                    style.font,
                    style.color,
                );
                Ok(())
            },
        );
        // A band between two polylines with one colour per point, top and
        // bottom: area fills, gradients, ribbons.
        reg.add_method(
            "strip",
            |_, this, (top, bottom, top_colors, bottom_colors): (Table, Table, Table, Option<Table>)| {
                let top = this.points(&top)?;
                let bottom = this.points(&bottom)?;
                let bottom_colors = bottom_colors.as_ref().unwrap_or(&top_colors);
                let mut mesh = Mesh::default();
                for (i, (a, b)) in top.iter().zip(&bottom).enumerate() {
                    let ca = color(&top_colors.raw_get::<Value>(i + 1)?)?;
                    let cb = color(&bottom_colors.raw_get::<Value>(i + 1)?)?;
                    mesh.colored_vertex(*a, ca);
                    mesh.colored_vertex(*b, cb);
                    if i > 0 {
                        let n = (i * 2) as u32;
                        mesh.add_triangle(n - 2, n - 1, n);
                        mesh.add_triangle(n - 1, n, n + 1);
                    }
                }
                this.painter.add(Shape::mesh(mesh));
                Ok(())
            },
        );
    }
}

struct TextStyle {
    font: egui::FontId,
    color: Color32,
}

impl TextStyle {
    fn read(opts: Option<&Table>, size: f32, fallback: Color32) -> mlua::Result<Self> {
        let Some(opts) = opts else {
            return Ok(Self {
                font: theme::body(size),
                color: fallback,
            });
        };
        let size = opts.get::<Option<f32>>("size")?.unwrap_or(size);
        let strong = opts.get::<Option<bool>>("strong")?.unwrap_or(false);
        let c: Value = opts.get("color")?;
        Ok(Self {
            font: if strong {
                theme::strong(size)
            } else {
                theme::body(size)
            },
            color: if c.is_nil() { fallback } else { color(&c)? },
        })
    }
}

/// `"lt"` is left top, `"cc"` centre centre, `"rb"` right bottom.
fn align2(code: Option<&str>) -> Align2 {
    let code = code.unwrap_or("lt").as_bytes();
    let h = match code.first() {
        Some(b'c') => egui::Align::Center,
        Some(b'r') => egui::Align::Max,
        _ => egui::Align::Min,
    };
    let v = match code.get(1) {
        Some(b'c') => egui::Align::Center,
        Some(b'b') => egui::Align::Max,
        _ => egui::Align::Min,
    };
    Align2([h, v])
}

/// A colour from a script: a theme name, `#rrggbb`, `#rrggbbaa`, or a
/// `0xrrggbb` number.
fn color(v: &Value) -> mlua::Result<Color32> {
    let bad = || mlua::Error::runtime(format!("not a colour: {v:?}"));
    match v {
        Value::Integer(n) => Ok(rgb(*n as u32)),
        Value::Number(n) => Ok(rgb(*n as u32)),
        Value::String(s) => {
            let s = s.to_str()?;
            let named = match &*s {
                "bg" => Some(theme::BG),
                "card" => Some(theme::CARD),
                "border" => Some(theme::CARD_BORDER),
                "field" => Some(theme::FIELD),
                "text" => Some(theme::TEXT),
                "muted" => Some(theme::MUTED),
                "subtle" => Some(theme::SUBTLE),
                "holding" => Some(theme::HOLDING),
                "waiting" => Some(theme::WAITING),
                "paused" => Some(theme::PAUSED),
                "failed" => Some(theme::FAILED),
                _ => None,
            };
            if let Some(c) = named {
                return Ok(c);
            }
            let hex = s.strip_prefix('#').ok_or_else(bad)?;
            let n = u32::from_str_radix(hex, 16).map_err(|_| bad())?;
            match hex.len() {
                6 => Ok(rgb(n)),
                8 => Ok(Color32::from_rgba_unmultiplied(
                    (n >> 24) as u8,
                    (n >> 16) as u8,
                    (n >> 8) as u8,
                    n as u8,
                )),
                _ => Err(bad()),
            }
        }
        _ => Err(bad()),
    }
}

fn rgb(n: u32) -> Color32 {
    Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8)
}

fn hex(c: Color32) -> String {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
}

fn icon(name: Option<&str>) -> char {
    match name.unwrap_or_default() {
        "home" => glyph::HOME,
        "settings" => glyph::SETTINGS,
        "add" => glyph::ADD,
        "moon" => glyph::MOON,
        "monitor" => glyph::MONITOR,
        "picture" => glyph::PICTURE,
        "pause" => glyph::PAUSE,
        "close" => glyph::CLOSE,
        "clock" => glyph::CLOCK,
        "game" => glyph::GAME,
        "volume" => glyph::VOLUME,
        "lock" => glyph::LOCK,
        "fullscreen" => glyph::FULLSCREEN,
        "arrow" => glyph::ARROW,
        _ => glyph::BOLT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_plugin(dir: &Path, script: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            "id = \"t\"\nname = \"Test\"\nversion = \"0.1.0\"\nplatforms = [\"windows\", \"linux\", \"macos\"]\n",
        )
        .unwrap();
        std::fs::write(dir.join("init.lua"), script).unwrap();
    }

    fn script(line: &str) -> String {
        format!(
            "return {{ settings = {{ n = 1 }}, status = function(s) return {{ line = '{line} ' .. s.n }} end }}"
        )
    }

    #[test]
    fn reload_keeps_settings_and_survives_a_broken_save() {
        let dir = std::env::temp_dir().join(format!("cortisoff-lua-{}", std::process::id()));
        write_plugin(&dir, &script("first"));
        let mut plugin = LuaPlugin::load(&dir, 0);
        assert_eq!(plugin.status().line, "first 1");

        // The user changed a setting; a new version of the script keeps it.
        let settings: Table = plugin
            .lua
            .registry_value(plugin.settings.as_ref().unwrap())
            .unwrap();
        settings.set("n", 5).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("init.lua"), script("second")).unwrap();
        plugin.refresh();
        assert_eq!(plugin.status().line, "second 5");

        // A save that does not parse keeps the last working version.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("init.lua"), "return {").unwrap();
        plugin.refresh();
        assert!(
            plugin.error().unwrap().contains("init.lua"),
            "{:?}",
            plugin.error()
        );
        assert_eq!(plugin.status().line, "second 5");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_names_its_platforms() {
        let m: Manifest = toml::from_str(
            "id = \"a\"\nname = \"A\"\nversion = \"1.0.0\"\nplatforms = [\"windows\"]\n",
        )
        .unwrap();
        assert!(m.supports("windows"));
        assert!(!m.supports("linux"));
        assert_eq!(m.entry, "init.lua");
    }
}
