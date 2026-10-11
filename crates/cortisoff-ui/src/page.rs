//! A plugin's window half.
//!
//! A plugin's `init.lua` returns a table whose `status(s, st)` and
//! `page(ui, s, st)` functions are its window half. They run here, in the
//! same sandbox the daemon half runs in ([`Sandbox`]), but in a Lua state of
//! their own: the two halves only share the settings `s`, the state `st` the
//! daemon half last published, and the messages `ui:send` posts.
//!
//! `page` is called every frame with a fresh copy of the settings. It edits
//! that table in place; whatever differs afterwards goes back to the shell
//! as [`Effect::Settings`]. Drawing goes through `ui`, whose controls are
//! the ones in [`widgets`], and free painting through `ui:canvas`.
//!
//! Nothing a plugin does here can take the window down: a Lua error in
//! `page` draws a red card in place of the page, one in `status` makes it
//! return `None`, and both say why through [`Page::error`].

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use cortisoff_host::{CapabilityId, Value};
use cortisoff_ipc::{Origin, PluginReport};
use cortisoff_runtime::sandbox::{self, Sandbox};
use cortisoff_runtime::{builtin, store, Files, Manifest, Package};
use egui::layers::ShapeIdx;
use egui::{
    vec2, Align2, Color32, CornerRadius, Frame, Margin, Mesh, Painter, Pos2, Rect, RichText, Sense,
    Shape, Stroke, Ui, UiBuilder,
};
use mlua::{Function, Lua, Table, UserData, UserDataMethods, UserDataRegistry, Value as LuaValue};

use crate::theme::{self, glyph};
use crate::widgets::{self, Track};

type Json = serde_json::Value;

/// How long settings the page sent win over what the daemon reports, so an
/// echo of an older value never pulls a slider back mid-drag.
const ECHO: Duration = Duration::from_secs(1);

/// How often a plugin read from a folder is checked for edits.
const RESTAT: Duration = Duration::from_millis(300);

/// How a plugin reads at a glance, from its `status` function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Holding,
    Waiting,
    Paused,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub line: String,
    pub detail: String,
    pub state: State,
}

/// What a page asked of the daemon while it was drawn. The shell turns each
/// into a request.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// The page edited its settings table: the whole new table, defaults
    /// included.
    Settings(serde_json::Value),
    /// `ui:send(name, data)`, for the plugin's daemon half.
    Send {
        name: String,
        data: serde_json::Value,
    },
    /// A slider with `preview = cap` is being dragged.
    Preview { cap: CapabilityId, value: Value },
    /// That drag ended.
    PreviewEnd { cap: CapabilityId },
}

/// The script, once it ran.
struct Loaded {
    sandbox: Sandbox,
    module: Table,
    /// Its `settings` table, as JSON.
    defaults: Json,
}

/// What, when it changes, means the script has to be loaded again.
#[derive(Debug, Clone, PartialEq)]
struct Key {
    version: String,
    origin: Origin,
    dir: Option<String>,
    /// Only for a plugin asking for FFI, which runs once it is switched on.
    ffi_on: bool,
    stamp: Option<SystemTime>,
}

impl Key {
    fn of(report: &PluginReport, stamp: Option<SystemTime>) -> Key {
        Key {
            version: report.version.clone(),
            origin: report.origin,
            dir: report.dir.clone(),
            ffi_on: report.ffi && report.enabled,
            stamp,
        }
    }
}

/// One plugin's window half, kept between frames.
pub struct Page {
    name: String,
    key: Option<Key>,
    files: Option<Files>,
    loaded: Option<Loaded>,
    /// Why the script is not run at all, said in place of the page.
    blocked: Option<String>,
    load_error: Option<String>,
    page_error: Option<String>,
    status_error: Option<String>,
    /// The settings the page shows: defaults with the user's values over.
    settings: Json,
    state: Json,
    /// When the page last sent its settings.
    emitted: Option<Instant>,
    checked: Option<Instant>,
    /// What `cortisoff.now()` answers.
    now: Rc<Cell<f64>>,
    /// Sliders with a preview being dragged, by widget.
    previewing: Vec<(egui::Id, CapabilityId)>,
}

impl Page {
    /// Load the plugin's files the report points at: the built-in copy for
    /// a built-in plugin, its folder otherwise.
    pub fn new(report: &PluginReport) -> Page {
        let mut page = Page {
            name: report.name.clone(),
            key: None,
            files: None,
            loaded: None,
            blocked: None,
            load_error: None,
            page_error: None,
            status_error: None,
            settings: Json::Object(Default::default()),
            state: Json::Null,
            emitted: None,
            checked: None,
            now: Rc::new(Cell::new(0.0)),
            previewing: Vec::new(),
        };
        page.reload(report);
        page.adopt(report);
        page
    }

    /// Fresh settings and state from the daemon. Reloads the script when the
    /// plugin's version, folder or files changed.
    pub fn sync(&mut self, report: &PluginReport) {
        let mut stamp = self.key.as_ref().and_then(|k| k.stamp);
        if let Some(files) = self.files.as_ref().filter(|f| f.dir().is_some()) {
            if self.checked.is_none_or(|t| t.elapsed() >= RESTAT) {
                self.checked = Some(Instant::now());
                stamp = files.stamp();
            }
        }
        if self.key.as_ref() != Some(&Key::of(report, stamp)) {
            self.reload(report);
        }
        self.adopt(report);
    }

    /// The plugin's `status(s, st)`. `None` when it has none or it failed.
    /// `now` is the local minute of the day, fractional.
    pub fn status(&mut self, now: f64) -> Option<Status> {
        self.now.set(now);
        let loaded = self.loaded.as_ref()?;
        let f = sandbox::function(&loaded.module, "status")?;
        let lua = &loaded.sandbox.lua;
        let out = loaded.sandbox.guard(|| {
            let s = settings_table(lua, &self.settings)?;
            let st = sandbox::from_json(lua, &self.state)?;
            read_status(f.call::<LuaValue>((s, st))?)
        });
        drop(loaded.sandbox.printed());
        match out {
            Ok(status) => {
                self.status_error = None;
                Some(status)
            }
            Err(e) => {
                self.status_error = Some(describe(&e));
                None
            }
        }
    }

    /// The plugin's `page(ui, s, st)`, drawn into `ui`.
    pub fn draw(&mut self, ui: &mut Ui, now: f64) -> Vec<Effect> {
        self.now.set(now);
        let mut effects = Vec::new();
        let mut settings = None;
        let active = RefCell::new(Vec::new());
        if let Some(why) = &self.blocked {
            note_card(ui, why);
        } else {
            if let Some(e) = &self.load_error {
                let footnote = match (&self.loaded, self.files.as_ref().and_then(Files::dir)) {
                    (Some(_), _) => Some("The last version that loaded is drawn below."),
                    (None, Some(_)) => Some("Save the file to try again."),
                    (None, None) => None,
                };
                error_card(ui, "This plugin did not load", e, footnote);
            }
            if let Some(loaded) = &self.loaded {
                match sandbox::function(&loaded.module, "page") {
                    None => note_card(ui, &format!("{} has nothing to set.", self.name)),
                    Some(f) => {
                        let sink = Sink {
                            effects: RefCell::new(Vec::new()),
                            previewing: &self.previewing,
                            active: &active,
                        };
                        let out = run_page(ui, loaded, &f, &self.settings, &self.state, &sink);
                        effects = sink.effects.into_inner();
                        drop(loaded.sandbox.printed());
                        match out {
                            Ok(new) => {
                                self.page_error = None;
                                if !same(&new, &self.settings) {
                                    let new = tidy(new);
                                    self.settings = new.clone();
                                    self.emitted = Some(Instant::now());
                                    settings = Some(new);
                                }
                            }
                            Err(e) => {
                                let message = describe(&e);
                                error_card(ui, "This page stopped with an error", &message, None);
                                self.page_error = Some(message);
                            }
                        }
                    }
                }
            }
        }
        let active = active.into_inner();
        for (id, cap) in &self.previewing {
            if !active.iter().any(|(a, _)| a == id) {
                effects.push(Effect::PreviewEnd { cap: cap.clone() });
            }
        }
        self.previewing = active;
        if let Some(s) = settings {
            effects.push(Effect::Settings(s));
        }
        effects
    }

    /// Why the window half cannot run: a load error or the last error a
    /// call raised.
    pub fn error(&self) -> Option<&str> {
        self.load_error
            .as_deref()
            .or(self.page_error.as_deref())
            .or(self.status_error.as_deref())
    }

    /// Reads the files again and runs the script. A script that fails keeps
    /// the last one that loaded, and says why.
    fn reload(&mut self, report: &PluginReport) {
        self.name = report.name.clone();
        let (files, manifest) = match resolve(report) {
            Ok(found) => found,
            Err(e) => {
                self.key = Some(Key::of(report, None));
                self.files = None;
                self.loaded = None;
                self.load_error = Some(e);
                return;
            }
        };
        self.key = Some(Key::of(report, files.stamp()));
        self.checked = Some(Instant::now());
        let ffi = report.ffi || manifest.ffi;
        if ffi && !report.enabled {
            self.blocked = Some(format!(
                "{} makes native calls outside the sandbox, so its page only runs once it is \
                 switched on.",
                manifest.name
            ));
            self.loaded = None;
            self.load_error = None;
        } else {
            self.blocked = None;
            match load(&files, &manifest, ffi, self.now.clone()) {
                Ok(loaded) => {
                    self.loaded = Some(loaded);
                    self.load_error = None;
                    self.page_error = None;
                    self.status_error = None;
                }
                Err(e) => self.load_error = Some(describe(&e)),
            }
        }
        self.files = Some(files);
    }

    fn adopt(&mut self, report: &PluginReport) {
        self.name = report.name.clone();
        self.state = report.state.clone();
        if self.emitted.is_some_and(|t| t.elapsed() < ECHO) {
            return;
        }
        self.emitted = None;
        let empty = Json::Object(Default::default());
        let defaults = self.loaded.as_ref().map_or(&empty, |l| &l.defaults);
        self.settings = tidy(store::merge(defaults, &report.settings));
    }
}

/// The plugin's files and manifest, from the binary or from its folder.
fn resolve(report: &PluginReport) -> Result<(Files, Manifest), String> {
    if report.origin == Origin::Builtin {
        return builtin()
            .into_iter()
            .find(|p| p.manifest.id == report.id)
            .map(|p| (p.files, p.manifest))
            .ok_or_else(|| format!("No built-in plugin is called {}.", report.id));
    }
    let dir = report
        .dir
        .as_deref()
        .ok_or_else(|| format!("{} names no folder to read it from.", report.name))?;
    Package::from_dir(Path::new(dir), report.origin).map(|p| (p.files, p.manifest))
}

fn load(files: &Files, manifest: &Manifest, ffi: bool, now: Rc<Cell<f64>>) -> mlua::Result<Loaded> {
    let sandbox = Sandbox::new(files, ffi)?;
    install_window_api(&sandbox.lua, now)?;
    let module = sandbox.load_module(files, &manifest.entry)?;
    let defaults = match module.get::<LuaValue>("settings")? {
        v @ LuaValue::Table(_) => tidy(to_json(&sandbox.lua, v)?),
        _ => Json::Object(Default::default()),
    };
    drop(sandbox.printed());
    Ok(Loaded {
        sandbox,
        module,
        defaults,
    })
}

/// What only the window half sees on the `cortisoff` table.
fn install_window_api(lua: &Lua, now: Rc<Cell<f64>>) -> mlua::Result<()> {
    let api: Table = lua.globals().get("cortisoff")?;
    api.set(
        "kelvin",
        lua.create_function(|_, k: f32| Ok(hex(theme::kelvin(k))))?,
    )?;
    api.set(
        "alpha",
        lua.create_function(|_, (c, a): (LuaValue, f32)| {
            Ok(hex(color(&c)?.gamma_multiply(a.clamp(0.0, 1.0))))
        })?,
    )?;
    api.set("now", lua.create_function(move |_, ()| Ok(now.get()))?)?;
    Ok(())
}

/// Runs `page` in a child of `ui`. When it raises an error, everything it
/// drew is wiped and takes no room, so the error card stands in its place.
fn run_page(
    ui: &mut Ui,
    loaded: &Loaded,
    f: &Function,
    settings: &Json,
    state: &Json,
    sink: &Sink,
) -> mlua::Result<Json> {
    let lua = &loaded.sandbox.lua;
    let start = ui.painter().add(Shape::Noop);
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt("plugin-page")
            .max_rect(ui.available_rect_before_wrap())
            .layout(*ui.layout()),
    );
    let out = loaded.sandbox.guard(|| {
        let s = settings_table(lua, settings)?;
        let st = sandbox::from_json(lua, state)?;
        lua.scope(|scope| {
            let handle = scope.create_userdata(UiRef {
                ui: &mut child,
                sink,
            })?;
            f.call::<()>((handle, s.clone(), st))
        })?;
        to_json(lua, LuaValue::Table(s))
    });
    if out.is_ok() {
        ui.advance_cursor_after_rect(child.min_rect());
    } else {
        let end = ui.painter().add(Shape::Noop);
        for i in start.0 + 1..end.0 {
            ui.painter().set(ShapeIdx(i), Shape::Noop);
        }
    }
    out
}

fn settings_table(lua: &Lua, settings: &Json) -> mlua::Result<Table> {
    match sandbox::from_json(lua, settings)? {
        LuaValue::Table(t) => Ok(t),
        _ => lua.create_table(),
    }
}

/// Lua to JSON, an empty table being an empty list: a list the user emptied
/// is far more common in settings than an empty map.
fn to_json(lua: &Lua, v: LuaValue) -> mlua::Result<Json> {
    use mlua::LuaSerdeExt;
    lua.from_value_with(
        v,
        mlua::serde::DeserializeOptions::new()
            .deny_unsupported_types(false)
            .deny_recursive_tables(false)
            .encode_empty_tables_as_array(true),
    )
}

fn read_status(v: LuaValue) -> mlua::Result<Status> {
    let LuaValue::Table(t) = v else {
        return Err(mlua::Error::runtime(format!(
            "status returned {}, not a table",
            v.type_name()
        )));
    };
    let state = match t.get::<Option<String>>("state")?.as_deref() {
        Some("holding") => State::Holding,
        Some("paused") => State::Paused,
        Some("failed") => State::Failed,
        _ => State::Waiting,
    };
    Ok(Status {
        line: t.get::<Option<String>>("line")?.unwrap_or_default(),
        detail: t.get::<Option<String>>("detail")?.unwrap_or_default(),
        state,
    })
}

/// A Lua error as one paragraph a person can read: the message the script
/// raised, without the Rust wrapping or the stack traceback.
fn describe(e: &mlua::Error) -> String {
    let text = match e {
        mlua::Error::CallbackError { cause, .. } => return describe(cause),
        mlua::Error::WithContext { context, cause } => format!("{context}: {}", describe(cause)),
        mlua::Error::BadArgument { to, pos, cause, .. } => match to {
            Some(to) => format!("bad argument #{pos} to {to}: {}", describe(cause)),
            None => format!("bad argument #{pos}: {}", describe(cause)),
        },
        mlua::Error::RuntimeError(m)
        | mlua::Error::MemoryError(m)
        | mlua::Error::SyntaxError { message: m, .. } => m.clone(),
        other => other.to_string(),
    };
    let head = text.split("stack traceback:").next().unwrap_or(&text);
    let mut line = head.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.len() > 600 {
        let mut end = 600;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
        line.push_str(" (cut)");
    }
    line
}

/// Equal as settings: numbers by value, so `3200` is `3200.0`, and an empty
/// list the same as an empty map, which Lua cannot tell apart.
fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => x.as_f64() == y.as_f64(),
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        (Json::Array(l), Json::Object(m)) | (Json::Object(m), Json::Array(l)) => {
            l.is_empty() && m.is_empty()
        }
        _ => a == b,
    }
}

/// Whole numbers written as integers, so a stored `3200.0` from Lua reads
/// `3200` like the default it replaces.
fn tidy(v: Json) -> Json {
    match v {
        Json::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 9.0e15 => Json::from(f as i64),
            _ => Json::Number(n),
        },
        Json::Array(a) => Json::Array(a.into_iter().map(tidy).collect()),
        Json::Object(o) => Json::Object(o.into_iter().map(|(k, v)| (k, tidy(v))).collect()),
        other => other,
    }
}

fn note_card(ui: &mut Ui, text: &str) {
    widgets::card(ui, None, |ui| widgets::hint(ui, text));
    ui.add_space(12.0);
}

fn error_card(ui: &mut Ui, title: &str, message: &str, footnote: Option<&str>) {
    Frame::new()
        .fill(theme::CARD.lerp_to_gamma(theme::FAILED, 0.07))
        .stroke(Stroke::new(1.0, theme::FAILED.gamma_multiply(0.45)))
        .corner_radius(theme::RADIUS_CARD)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(glyph::WARNING.to_string())
                        .font(theme::icon(14.0))
                        .color(theme::FAILED),
                );
                ui.label(
                    RichText::new(title)
                        .font(theme::strong(14.0))
                        .color(theme::FAILED),
                );
            });
            ui.add_space(2.0);
            ui.label(
                RichText::new(message)
                    .font(egui::FontId::monospace(12.5))
                    .color(theme::TEXT),
            );
            if let Some(footnote) = footnote {
                ui.add_space(2.0);
                widgets::hint(ui, footnote);
            }
        });
    ui.add_space(12.0);
}

/// Where the `ui` handles of one frame leave what they were asked.
struct Sink<'p> {
    effects: RefCell<Vec<Effect>>,
    /// Preview sliders dragged on the frame before.
    previewing: &'p [(egui::Id, CapabilityId)],
    /// Preview sliders dragged on this frame.
    active: &'p RefCell<Vec<(egui::Id, CapabilityId)>>,
}

impl Sink<'_> {
    fn push(&self, effect: Effect) {
        self.effects.borrow_mut().push(effect);
    }
}

/// The `ui` a script draws with. Lives for one call, then Lua loses it.
struct UiRef<'a, 'p> {
    ui: &'a mut Ui,
    sink: &'a Sink<'p>,
}

/// Runs `f` with a `ui` for `ui`, nested inside the current one.
fn nested(lua: &Lua, ui: &mut Ui, sink: &Sink, f: &Function) -> mlua::Result<()> {
    lua.scope(|scope| {
        let handle = scope.create_userdata(UiRef { ui, sink })?;
        f.call::<()>(handle)
    })
}

/// A glyph from an icon name, nothing for `nil`.
fn icon(name: Option<String>) -> Option<char> {
    name.as_deref().map(glyph::named)
}

impl UserData for UiRef<'_, '_> {
    fn register(reg: &mut UserDataRegistry<Self>) {
        reg.add_method_mut(
            "card",
            |lua, this, (title, f): (Option<String>, Function)| {
                let sink = this.sink;
                widgets::card(this.ui, title.as_deref(), |ui| nested(lua, ui, sink, &f))
            },
        );
        reg.add_method_mut(
            "row",
            |lua, this, (label, hint, f): (String, Option<String>, Function)| {
                let sink = this.sink;
                let mut out = Ok(());
                widgets::row(this.ui, &label, hint.as_deref(), |ui| {
                    out = nested(lua, ui, sink, &f);
                });
                out
            },
        );
        reg.add_method_mut("horizontal", |lua, this, f: Function| {
            let sink = this.sink;
            this.ui.horizontal(|ui| nested(lua, ui, sink, &f)).inner
        });
        reg.add_method_mut("wrap", |lua, this, f: Function| {
            let sink = this.sink;
            this.ui
                .horizontal_wrapped(|ui| nested(lua, ui, sink, &f))
                .inner
        });
        reg.add_method_mut(
            "vertical",
            |lua, this, (f, gap): (Function, Option<f32>)| {
                let sink = this.sink;
                this.ui
                    .vertical(|ui| {
                        if let Some(gap) = gap {
                            ui.spacing_mut().item_spacing.y = gap;
                        }
                        nested(lua, ui, sink, &f)
                    })
                    .inner
            },
        );
        reg.add_method_mut("right", |lua, this, f: Function| {
            let sink = this.sink;
            widgets::trailing(this.ui, |ui| nested(lua, ui, sink, &f))
        });
        reg.add_method_mut("label", |_, this, (text, opts): (String, Option<Table>)| {
            let style = TextStyle::read(opts.as_ref(), 14.0, theme::TEXT)?;
            this.ui
                .label(RichText::new(text).font(style.font).color(style.color));
            Ok(())
        });
        reg.add_method_mut("hint", |_, this, text: String| {
            widgets::hint(this.ui, &text);
            Ok(())
        });
        reg.add_method_mut("space", |_, this, px: Option<f32>| {
            this.ui.add_space(px.unwrap_or(8.0));
            Ok(())
        });
        reg.add_method_mut("divider", |_, this, ()| {
            widgets::divider(this.ui);
            Ok(())
        });
        reg.add_method_mut("width", |_, this, ()| Ok(this.ui.available_width()));
        reg.add_method_mut("switch", |_, this, on: Option<bool>| {
            let mut on = on.unwrap_or(false);
            widgets::switch(this.ui, &mut on);
            Ok(on)
        });
        reg.add_method_mut(
            "button",
            |_, this, (text, name): (String, Option<String>)| {
                Ok(widgets::button(this.ui, icon(name), &text).clicked())
            },
        );
        reg.add_method_mut("chip", |_, this, (text, name): (String, Option<String>)| {
            Ok(widgets::chip_button(this.ui, icon(name), &text).clicked())
        });
        reg.add_method_mut(
            "slider",
            |lua, this, (value, opts): (Option<f64>, Option<Table>)| {
                let opts = match opts {
                    Some(o) => o,
                    None => lua.create_table()?,
                };
                let min = opts.get::<Option<f64>>("min")?.unwrap_or(0.0);
                let max = opts
                    .get::<Option<f64>>("max")?
                    .filter(|m| *m > min)
                    .unwrap_or(min + 100.0);
                let step = opts
                    .get::<Option<f64>>("step")?
                    .filter(|s| *s > 0.0)
                    .unwrap_or(1.0);
                let track = match opts.get::<Option<String>>("track")?.as_deref() {
                    Some("kelvin") => Track::Kelvin,
                    _ => Track::Plain,
                };
                let preview: Option<String> = opts.get("preview")?;
                let text: Option<Function> = opts.get("text")?;
                let original = value.unwrap_or(min);
                let mut v = original as f32;
                let response = widgets::slider(
                    this.ui,
                    &mut v,
                    min as f32..=max as f32,
                    step as f32,
                    track,
                    |v| {
                        let v = snap(f64::from(v), min, step);
                        match &text {
                            Some(f) => f.call::<String>(v).unwrap_or_else(|e| describe(&e)),
                            None => number(v),
                        }
                    },
                );
                let changed = response.changed();
                let out = if changed {
                    snap(f64::from(v), min, step)
                } else {
                    original
                };
                if let Some(cap) = preview {
                    let id = response.id;
                    let down = response.is_pointer_button_down_on() || response.dragged();
                    let was = this.sink.previewing.iter().any(|(i, _)| *i == id);
                    if down && (changed || was) {
                        let cap = CapabilityId::new(cap);
                        if changed {
                            if let Ok(value) = sandbox::to_host(&LuaValue::Number(out)) {
                                this.sink.push(Effect::Preview {
                                    cap: cap.clone(),
                                    value,
                                });
                            }
                        }
                        this.sink.active.borrow_mut().push((id, cap));
                    }
                }
                Ok(out)
            },
        );
        reg.add_method_mut("time", |_, this, minute: Option<f64>| {
            let mut minute = minute.unwrap_or(0.0);
            widgets::time_field(this.ui, &mut minute);
            Ok(minute)
        });
        reg.add_method_mut(
            "text",
            |_, this, (value, opts): (Option<String>, Option<Table>)| {
                let (hint, width, mono) = match &opts {
                    Some(o) => (
                        o.get::<Option<String>>("hint")?,
                        o.get::<Option<f32>>("width")?,
                        o.get::<Option<bool>>("mono")?.unwrap_or(false),
                    ),
                    None => (None, None, false),
                };
                let width = match width {
                    Some(w) if w > 0.0 => w,
                    Some(_) => this.ui.available_width(),
                    None => 160.0,
                };
                let mut value = value.unwrap_or_default();
                widgets::text_field(this.ui, &mut value, hint.as_deref(), width, mono);
                Ok(value)
            },
        );
        reg.add_method_mut(
            "select",
            |_, this, (value, options, labels): (LuaValue, Table, Option<Table>)| {
                let options = options
                    .sequence_values::<LuaValue>()
                    .collect::<mlua::Result<Vec<_>>>()?;
                let key = |v: &LuaValue| v.to_string().unwrap_or_default();
                let names = options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| {
                        labels
                            .as_ref()
                            .and_then(|l| {
                                l.get::<Option<String>>(o.clone())
                                    .ok()
                                    .flatten()
                                    .or_else(|| l.raw_get::<Option<String>>(i + 1).ok().flatten())
                            })
                            .unwrap_or_else(|| key(o))
                    })
                    .collect::<Vec<_>>();
                let current = key(&value);
                let selected = options.iter().position(|o| key(o) == current);
                let fallback = if value.is_nil() {
                    "Choose".to_owned()
                } else {
                    current
                };
                Ok(
                    match widgets::select(this.ui, &names, selected, &fallback) {
                        Some(i) => options[i].clone(),
                        None => value,
                    },
                )
            },
        );
        reg.add_method_mut(
            "send",
            |lua, this, (name, data): (String, Option<LuaValue>)| {
                let data = match data {
                    Some(v) if !v.is_nil() => tidy(to_json(lua, v)?),
                    _ => Json::Null,
                };
                this.sink.push(Effect::Send { name, data });
                Ok(())
            },
        );
        // Free painting: `ui:canvas(width or nil, height, function(p) ... end)`.
        reg.add_method_mut(
            "canvas",
            |lua, this, (width, height, f): (Option<f32>, f32, Function)| {
                let width = width.unwrap_or_else(|| this.ui.available_width());
                let (rect, _) = this
                    .ui
                    .allocate_exact_size(vec2(width, height), Sense::hover());
                // Clipped to the page, not the canvas: a label may sit
                // above the area it describes, as with egui's own painter.
                let canvas = Canvas {
                    painter: this.ui.painter().clone(),
                    rect,
                };
                lua.scope(|scope| f.call::<()>(scope.create_userdata(canvas)?))
            },
        );
    }
}

/// `x` on the grid of `step` starting at `min`, without the float noise the
/// division leaves.
fn snap(x: f64, min: f64, step: f64) -> f64 {
    let v = ((x - min) / step).round() * step + min;
    (v * 1e9).round() / 1e9
}

fn number(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
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

    /// `{ x1, y1, x2, y2, ... }` as points.
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
        reg.add_method(
            "line",
            |_, this, (flat, width, c): (Table, Option<f32>, LuaValue)| {
                let points = this.points(&flat)?;
                this.painter.add(Shape::line(
                    points,
                    Stroke::new(width.unwrap_or(1.0), color(&c)?),
                ));
                Ok(())
            },
        );
        reg.add_method(
            "vline",
            |_, this, (x, top, bottom, width, c): (f32, f32, f32, Option<f32>, LuaValue)| {
                let at = this.at(x, top);
                this.painter.vline(
                    at.x,
                    at.y..=this.rect.min.y + bottom,
                    Stroke::new(width.unwrap_or(1.0), color(&c)?),
                );
                Ok(())
            },
        );
        reg.add_method(
            "hline",
            |_, this, (left, right, y, width, c): (f32, f32, f32, Option<f32>, LuaValue)| {
                let at = this.at(left, y);
                this.painter.hline(
                    at.x..=this.rect.min.x + right,
                    at.y,
                    Stroke::new(width.unwrap_or(1.0), color(&c)?),
                );
                Ok(())
            },
        );
        reg.add_method(
            "rect",
            |_, this, (x, y, w, h, c, radius): (f32, f32, f32, f32, LuaValue, Option<f32>)| {
                let rect = Rect::from_min_size(this.at(x, y), vec2(w, h));
                let radius = radius.unwrap_or(0.0).clamp(0.0, 255.0) as u8;
                this.painter
                    .rect_filled(rect, CornerRadius::same(radius), color(&c)?);
                Ok(())
            },
        );
        reg.add_method(
            "circle",
            |_, this, (x, y, r, fill, stroke, width): (f32, f32, f32, LuaValue, LuaValue, Option<f32>)| {
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
                    let ca = color(&top_colors.raw_get::<LuaValue>(i + 1)?)?;
                    let cb = color(&bottom_colors.raw_get::<LuaValue>(i + 1)?)?;
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
        let mono = opts.get::<Option<bool>>("mono")?.unwrap_or(false);
        let c: LuaValue = opts.get("color")?;
        Ok(Self {
            font: if mono {
                egui::FontId::monospace(size)
            } else if strong {
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

/// A colour from a script: a theme name, `#rrggbb`, `#rrggbbaa` (what
/// `cortisoff.kelvin` and `cortisoff.alpha` return), or a `0xrrggbb` number.
fn color(v: &LuaValue) -> mlua::Result<Color32> {
    let bad = || {
        mlua::Error::runtime(format!(
            "not a colour: {}",
            v.to_string().unwrap_or_default()
        ))
    };
    match v {
        LuaValue::Integer(n) => Ok(rgb(*n as u32)),
        LuaValue::Number(n) => Ok(rgb(*n as u32)),
        LuaValue::String(s) => {
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
                "transparent" => Some(Color32::TRANSPARENT),
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

#[cfg(test)]
mod tests;
