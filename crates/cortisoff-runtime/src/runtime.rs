//! The daemon's half of every plugin: load it, start it, route events,
//! timers, hotkeys and HTTP answers to it, and hand the daemon back what it
//! asked for as [`Command`]s.
//!
//! One Lua state per plugin, so a plugin that breaks breaks alone and a
//! plugin switched off gives all its memory back.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::SystemTime;

use cortisoff_core::World;
use cortisoff_host::{
    ids, CapabilityId, CapabilitySet, Chord, Event, Host, Hotkey, LogLevel, Value as HostValue,
    Waker,
};
use cortisoff_ipc::{Origin, PluginReport};
use mlua::{Function, Lua, Table, Value};

use crate::package::{Files, Package};
use crate::sandbox::{self, Sandbox};
use crate::store::{self, Store};

/// What a call into a plugin needs from the daemon.
pub struct Env<'a> {
    pub host: &'a mut dyn Host,
    pub world: &'a World,
    pub probed: &'a CapabilitySet,
    /// [`Host::monotonic_ms`] when the batch started.
    pub now: u64,
}

/// What a plugin asked for that the daemon carries out after the call.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Claim {
        owner: u32,
        cap: CapabilityId,
        value: HostValue,
        priority: i32,
        fade_ms: u64,
    },
    Release {
        owner: u32,
        cap: CapabilityId,
        fade_ms: u64,
    },
    /// Everything this owner holds: it stopped.
    ReleaseAll {
        owner: u32,
        fade_ms: u64,
    },
    Run {
        program: String,
        args: Vec<String>,
    },
    Log {
        level: LogLevel,
        message: String,
    },
    /// This plugin's report moved: state, settings, error or running.
    Changed {
        id: String,
    },
    /// Plugins appeared or went away.
    ListChanged,
    /// Settings or a switch moved: write the store.
    Save,
}

/// Every kind of event a plugin can watch. All but `clock` are capability
/// ids, which is what the host is asked to install.
pub const KINDS: &[&str] = &[
    ids::APP_FOCUS,
    ids::APP_FULLSCREEN,
    ids::APP_MAXIMIZED,
    ids::USER_IDLE,
    ids::SESSION_LOCK,
    ids::POWER_SOURCE,
    ids::AUDIO_ACTIVE,
    ids::DISPLAY_CONFIG,
    "clock",
];

/// How long a release takes when a plugin is switched off: the screen eases
/// back instead of jumping.
const STOP_FADE_MS: u64 = 600;

const DAY_MS: u64 = 86_400_000;

pub(crate) struct Watch {
    pub kind: String,
    pub callback: Option<Function>,
    /// For `user.idle`: the threshold in seconds this watch cares about.
    pub idle_after: Option<u32>,
}

pub(crate) struct Live {
    pub sandbox: Sandbox,
    pub module: Table,
    pub settings: Table,
}

pub(crate) struct Slot {
    pub key: u32,
    pub pkg: Package,
    pub enabled: bool,
    /// What the user changed, the part the store keeps.
    pub stored: serde_json::Value,
    /// The plugin's defaults, once it has been loaded.
    pub defaults: serde_json::Value,
    /// Defaults with the user's changes over them: what the plugin sees.
    pub settings: serde_json::Value,
    pub state: serde_json::Value,
    pub error: Option<String>,
    pub live: Option<Live>,
    pub watches: Vec<Watch>,
    pub stamp: Option<SystemTime>,
}

pub(crate) struct Timer {
    pub id: u64,
    pub key: u32,
    pub due: u64,
    pub every: Option<u64>,
    /// Millisecond of the day, for `cx:at`.
    pub wall: Option<u64>,
    pub callback: Function,
}

pub(crate) struct Binding {
    pub id: u32,
    pub key: u32,
    pub chord: Chord,
    pub callback: Option<Function>,
}

pub(crate) struct HttpWait {
    pub id: u64,
    pub key: u32,
    pub callback: Function,
}

/// An HTTP request a plugin started, finished on its own thread.
pub struct HttpDone {
    pub id: u64,
    pub status: u16,
    pub body: String,
    pub error: Option<String>,
}

pub(crate) struct Shared {
    pub timers: Vec<Timer>,
    pub next_timer: u64,
    pub hotkeys: Vec<Binding>,
    pub next_hotkey: u32,
    pub hotkeys_dirty: bool,
    pub sources_dirty: bool,
    pub http: Vec<HttpWait>,
    pub next_http: u64,
    pub http_tx: Sender<HttpDone>,
    http_rx: Receiver<HttpDone>,
    pub waker: Option<Waker>,
}

impl Shared {
    fn forget(&mut self, key: u32) {
        self.timers.retain(|t| t.key != key);
        let before = self.hotkeys.len();
        self.hotkeys.retain(|h| h.key != key);
        self.hotkeys_dirty |= self.hotkeys.len() != before;
        self.http.retain(|h| h.key != key);
        self.sources_dirty = true;
    }
}

enum Call {
    Named(&'static str),
    Fn(Function),
}

type Build<'b> = &'b dyn Fn(&Lua) -> mlua::Result<Table>;

pub struct Runtime {
    slots: Vec<Slot>,
    shared: Shared,
    next_key: u32,
    os: &'static str,
}

impl Runtime {
    pub fn new(packages: Vec<Package>, store: &Store, waker: Option<Waker>) -> Self {
        let (http_tx, http_rx) = channel();
        let mut rt = Runtime {
            slots: Vec::new(),
            shared: Shared {
                timers: Vec::new(),
                next_timer: 1,
                hotkeys: Vec::new(),
                next_hotkey: 1,
                hotkeys_dirty: false,
                sources_dirty: true,
                http: Vec::new(),
                next_http: 1,
                http_tx,
                http_rx,
                waker,
            },
            next_key: 1,
            os: std::env::consts::OS,
        };
        for pkg in packages {
            rt.add(pkg, store);
        }
        rt
    }

    /// For tests: pretend to run on another system.
    pub fn with_os(mut self, os: &'static str) -> Self {
        self.os = os;
        self
    }

    fn add(&mut self, pkg: Package, store: &Store) {
        let entry = store.entry(&pkg.manifest.id);
        let key = self.next_key;
        self.next_key += 1;
        self.slots.push(Slot {
            key,
            enabled: entry.enabled,
            stored: entry.settings.clone(),
            defaults: serde_json::Value::Object(Default::default()),
            settings: entry.settings,
            state: serde_json::Value::Null,
            error: None,
            live: None,
            watches: Vec::new(),
            stamp: None,
            pkg,
        });
    }

    fn index(&self, id: &str) -> Result<usize, String> {
        self.slots
            .iter()
            .position(|s| s.pkg.manifest.id == id)
            .ok_or_else(|| format!("no plugin is called {id:?}"))
    }

    fn index_of_key(&self, key: u32) -> Option<usize> {
        self.slots.iter().position(|s| s.key == key)
    }

    /// The plugin's name for an owner key, for logs and claim reports.
    pub fn name_of(&self, key: u32) -> Option<&str> {
        self.slots
            .iter()
            .find(|s| s.key == key)
            .map(|s| s.pkg.manifest.name.as_str())
    }

    pub fn key_of(&self, id: &str) -> Option<u32> {
        self.index(id).ok().map(|i| self.slots[i].key)
    }

    /// Whether any plugin is running.
    pub fn active(&self) -> bool {
        self.slots.iter().any(|s| s.live.is_some())
    }

    /// Whether any running plugin is read from a folder, so hot reload has
    /// something to look at.
    pub fn watches_folders(&self) -> bool {
        self.slots
            .iter()
            .any(|s| s.live.is_some() && matches!(s.pkg.files, Files::Dir(_)))
    }

    pub fn start_all(&mut self, env: &mut Env) -> Vec<Command> {
        let mut out = Vec::new();
        for i in 0..self.slots.len() {
            out.extend(self.start(i, env));
        }
        out
    }

    /// Stops every plugin, for a clean exit. Nothing fades: the daemon is
    /// about to restore every baseline at once.
    pub fn shutdown(&mut self, env: &mut Env) -> Vec<Command> {
        let mut out = Vec::new();
        for i in 0..self.slots.len() {
            out.extend(self.stop(i, env, 0));
        }
        out
    }

    fn load(&mut self, i: usize) -> Result<(), String> {
        let slot = &mut self.slots[i];
        let pkg = &slot.pkg;
        let sandbox = Sandbox::new(&pkg.files, pkg.manifest.ffi).map_err(|e| e.to_string())?;
        let module = sandbox
            .load_module(&pkg.files, &pkg.manifest.entry)
            .map_err(describe)?;
        let lua = sandbox.lua.clone();
        let defaults = match module.get::<Value>("settings").map_err(describe)? {
            v @ Value::Table(_) => sandbox::to_json(&lua, v).map_err(describe)?,
            _ => serde_json::Value::Object(Default::default()),
        };
        let merged = store::merge(&defaults, &slot.stored);
        let settings = match sandbox::from_json(&lua, &merged).map_err(describe)? {
            Value::Table(t) => t,
            _ => lua.create_table().map_err(describe)?,
        };
        slot.defaults = defaults;
        slot.settings = merged;
        slot.live = Some(Live {
            sandbox,
            module,
            settings,
        });
        Ok(())
    }

    fn start(&mut self, i: usize, env: &mut Env) -> Vec<Command> {
        let slot = &mut self.slots[i];
        if !slot.enabled || slot.live.is_some() {
            return Vec::new();
        }
        let id = slot.pkg.manifest.id.clone();
        if !slot.pkg.manifest.supports(self.os) {
            slot.error = Some(format!(
                "made for {}, not {}",
                slot.pkg.manifest.platforms.join(", "),
                self.os
            ));
            return vec![Command::Changed { id }];
        }
        slot.stamp = slot.pkg.files.stamp();
        if let Err(e) = self.load(i) {
            let slot = &mut self.slots[i];
            slot.error = Some(e.clone());
            return vec![
                Command::Log {
                    level: LogLevel::Error,
                    message: format!("{} did not load: {e}", slot.pkg.manifest.name),
                },
                Command::Changed { id },
            ];
        }
        self.slots[i].error = None;
        self.shared.sources_dirty = true;
        let mut out = self.invoke(i, env, Call::Named("start"), None);
        out.push(Command::Changed { id });
        out
    }

    fn stop(&mut self, i: usize, env: &mut Env, fade_ms: u64) -> Vec<Command> {
        if self.slots[i].live.is_none() {
            return Vec::new();
        }
        let mut out = self.invoke(i, env, Call::Named("stop"), None);
        let slot = &mut self.slots[i];
        let key = slot.key;
        self.shared.forget(key);
        slot.watches.clear();
        slot.live = None;
        out.push(Command::ReleaseAll {
            owner: key,
            fade_ms,
        });
        out.push(Command::Changed {
            id: slot.pkg.manifest.id.clone(),
        });
        out
    }

    /// One call into a plugin with a fresh `cx`, and everything that
    /// follows from it: printed lines, an error, settings it changed, a
    /// state it published.
    fn invoke(&mut self, i: usize, env: &mut Env, call: Call, ev: Option<Build>) -> Vec<Command> {
        let Runtime {
            slots, shared, os, ..
        } = self;
        let Slot {
            key,
            pkg,
            stored,
            defaults,
            settings: current,
            state,
            error,
            live,
            watches,
            ..
        } = &mut slots[i];
        let mut out = Vec::new();
        let Some(live) = live.as_ref() else {
            return out;
        };
        let f = match call {
            Call::Named(name) => match sandbox::function(&live.module, name) {
                Some(f) => f,
                None => return out,
            },
            Call::Fn(f) => f,
        };
        let lua = live.sandbox.lua.clone();
        let settings = live.settings.clone();
        let mut published = None;
        let result = live.sandbox.guard(|| {
            lua.scope(|scope| {
                let cx = scope.create_userdata(crate::cx::Cx {
                    key: *key,
                    id: &pkg.manifest.id,
                    name: &pkg.manifest.name,
                    os: *os,
                    env: &mut *env,
                    shared: &mut *shared,
                    watches: &mut *watches,
                    out: &mut out,
                    published: &mut published,
                })?;
                match ev {
                    Some(build) => f.call::<()>((cx, settings, build(&lua)?)),
                    None => f.call::<()>((cx, settings)),
                }
            })
        });

        let name = &pkg.manifest.name;
        for line in live.sandbox.printed() {
            out.push(Command::Log {
                level: LogLevel::Info,
                message: format!("{name}: {line}"),
            });
        }
        let mut changed = false;
        if let Err(e) = result {
            let e = describe(e);
            out.push(Command::Log {
                level: LogLevel::Error,
                message: format!("{name}: {e}"),
            });
            *error = Some(e);
            changed = true;
        }
        if let Ok(now) = sandbox::to_json(&lua, Value::Table(live.settings.clone())) {
            if !store::same_json(&now, current) {
                *stored = store::changed(defaults, &now);
                *current = now;
                out.push(Command::Save);
                changed = true;
            }
        }
        if let Some(p) = published {
            if p != *state {
                *state = p;
                changed = true;
            }
        }
        if changed {
            out.push(Command::Changed {
                id: pkg.manifest.id.clone(),
            });
        }
        out
    }

    /// An event from the platform, to every plugin watching its kind.
    pub fn on_event(&mut self, env: &mut Env, event: &Event) -> Vec<Command> {
        let mut out = Vec::new();
        if matches!(event, Event::TimeOfDay { .. }) {
            self.rearm_wall(env);
        }
        let Some(kind) = kind_of(event) else {
            return out;
        };
        let build = |lua: &Lua| event_table(lua, kind, event);
        for i in 0..self.slots.len() {
            let calls: Vec<Call> = self.slots[i]
                .watches
                .iter()
                .filter(|w| w.kind == kind && idle_matches(w, event))
                .map(|w| match &w.callback {
                    Some(f) => Call::Fn(f.clone()),
                    None => Call::Named("on"),
                })
                .collect();
            for call in calls {
                out.extend(self.invoke(i, env, call, Some(&build)));
            }
        }
        out
    }

    /// When the earliest plugin timer is due, on the monotonic clock.
    pub fn next_due(&self) -> Option<u64> {
        self.shared.timers.iter().map(|t| t.due).min()
    }

    /// Fire every timer that is due.
    pub fn on_timer(&mut self, env: &mut Env) -> Vec<Command> {
        let now = env.now;
        let wall_now = wall_ms(env.host);
        let mut fire = Vec::new();
        for t in &mut self.shared.timers {
            if t.due > now + 2 {
                continue;
            }
            fire.push((t.key, t.id, t.callback.clone()));
            if let Some(every) = t.every {
                t.due = (t.due + every).max(now + 1);
            } else if let (Some(at), Some(wall)) = (t.wall, wall_now) {
                t.due = now + until(at, wall);
            } else if t.wall.is_some() {
                t.due = now + DAY_MS;
            } else {
                t.due = u64::MAX;
            }
        }
        self.shared.timers.retain(|t| t.due != u64::MAX);
        let mut out = Vec::new();
        for (key, id, callback) in fire {
            if let Some(i) = self.index_of_key(key) {
                let build = move |lua: &Lua| {
                    let t = lua.create_table()?;
                    t.set("kind", "timer")?;
                    t.set("id", id)?;
                    Ok(t)
                };
                out.extend(self.invoke(i, env, Call::Fn(callback), Some(&build)));
            }
        }
        out
    }

    /// The clock moved under the timers: resume, a time zone, DST. Every
    /// `cx:at` is measured again from the new time.
    fn rearm_wall(&mut self, env: &mut Env) {
        let Some(wall) = wall_ms(env.host) else {
            return;
        };
        for t in &mut self.shared.timers {
            if let Some(at) = t.wall {
                t.due = env.now + until(at, wall);
            }
        }
    }

    /// The hotkeys to register, once after they changed.
    pub fn take_hotkeys(&mut self) -> Option<Vec<Hotkey>> {
        if !std::mem::take(&mut self.shared.hotkeys_dirty) {
            return None;
        }
        Some(
            self.shared
                .hotkeys
                .iter()
                .map(|b| Hotkey {
                    id: b.id,
                    chord: b.chord,
                })
                .collect(),
        )
    }

    /// The system said no to some: tell the plugins that asked.
    pub fn hotkeys_refused(&mut self, refused: Vec<(u32, String)>) -> Vec<Command> {
        let mut out = Vec::new();
        for (id, reason) in refused {
            let Some(b) = self.shared.hotkeys.iter().find(|b| b.id == id) else {
                continue;
            };
            let (key, chord) = (b.key, b.chord);
            if let Some(i) = self.index_of_key(key) {
                let slot = &mut self.slots[i];
                let message = format!("{chord}: {reason}");
                out.push(Command::Log {
                    level: LogLevel::Warn,
                    message: format!("{}: {message}", slot.pkg.manifest.name),
                });
                slot.error = Some(message);
                out.push(Command::Changed {
                    id: slot.pkg.manifest.id.clone(),
                });
            }
        }
        out
    }

    pub fn on_hotkey(&mut self, env: &mut Env, id: u32) -> Vec<Command> {
        let Some(b) = self.shared.hotkeys.iter().find(|b| b.id == id) else {
            return Vec::new();
        };
        let (key, chord) = (b.key, b.chord);
        let call = match &b.callback {
            Some(f) => Call::Fn(f.clone()),
            None => Call::Named("on"),
        };
        let Some(i) = self.index_of_key(key) else {
            return Vec::new();
        };
        let build = move |lua: &Lua| {
            let t = lua.create_table()?;
            t.set("kind", "hotkey")?;
            t.set("id", id)?;
            t.set("chord", chord.to_string())?;
            Ok(t)
        };
        self.invoke(i, env, call, Some(&build))
    }

    /// Answers to `cx:http` that arrived since the last look.
    pub fn on_http(&mut self, env: &mut Env) -> Vec<Command> {
        let mut out = Vec::new();
        while let Ok(done) = self.shared.http_rx.try_recv() {
            let Some(at) = self.shared.http.iter().position(|w| w.id == done.id) else {
                continue;
            };
            let wait = self.shared.http.remove(at);
            let Some(i) = self.index_of_key(wait.key) else {
                continue;
            };
            let build = |lua: &Lua| {
                let t = lua.create_table()?;
                t.set("kind", "http")?;
                t.set("id", done.id)?;
                t.set("status", done.status)?;
                t.set("ok", done.error.is_none() && (200..300).contains(&done.status))?;
                t.set("body", lua.create_string(&done.body)?)?;
                t.set("error", done.error.clone())?;
                Ok(t)
            };
            out.extend(self.invoke(i, env, Call::Fn(wait.callback), Some(&build)));
        }
        out
    }

    /// What the host should watch, once after it changed: the sources every
    /// running plugin watches, and the idle thresholds they named.
    pub fn take_sources(&mut self) -> Option<(Vec<CapabilityId>, Vec<u32>)> {
        if !std::mem::take(&mut self.shared.sources_dirty) {
            return None;
        }
        let mut caps: Vec<CapabilityId> = Vec::new();
        let mut thresholds = Vec::new();
        for slot in self.slots.iter().filter(|s| s.live.is_some()) {
            for w in &slot.watches {
                if w.kind != "clock" && !caps.iter().any(|c| c.as_str() == w.kind) {
                    caps.push(CapabilityId::new(w.kind.clone()));
                }
                thresholds.extend(w.idle_after);
            }
        }
        thresholds.sort_unstable();
        thresholds.dedup();
        Some((caps, thresholds))
    }

    pub fn set_enabled(
        &mut self,
        env: &mut Env,
        id: &str,
        enabled: bool,
    ) -> Result<Vec<Command>, String> {
        let i = self.index(id)?;
        if self.slots[i].enabled == enabled {
            return Ok(Vec::new());
        }
        self.slots[i].enabled = enabled;
        let mut out = vec![Command::Save];
        if enabled {
            out.extend(self.start(i, env));
        } else {
            out.extend(self.stop(i, env, STOP_FADE_MS));
            self.slots[i].error = None;
        }
        out.push(Command::Changed { id: id.to_owned() });
        Ok(out)
    }

    /// New settings from the window. The same table the plugin already
    /// holds is refilled, so a reference it kept stays current, then the
    /// plugin hears `{ kind = "settings" }`.
    pub fn set_settings(
        &mut self,
        env: &mut Env,
        id: &str,
        settings: serde_json::Value,
    ) -> Result<Vec<Command>, String> {
        let i = self.index(id)?;
        let slot = &mut self.slots[i];
        let merged = store::merge(&slot.defaults, &settings);
        if store::same_json(&merged, &slot.settings) {
            return Ok(Vec::new());
        }
        slot.stored = store::changed(&slot.defaults, &merged);
        slot.settings = merged.clone();
        let mut out = vec![Command::Save, Command::Changed { id: id.to_owned() }];
        if let Some(live) = &slot.live {
            refill(&live.sandbox.lua, &live.settings, &merged).map_err(describe)?;
            let build = |lua: &Lua| {
                let t = lua.create_table()?;
                t.set("kind", "settings")?;
                Ok(t)
            };
            out.extend(self.invoke(i, env, Call::Named("on"), Some(&build)));
        }
        Ok(out)
    }

    /// Something the plugin's page sent its daemon half.
    pub fn message(
        &mut self,
        env: &mut Env,
        id: &str,
        name: &str,
        data: &serde_json::Value,
    ) -> Result<Vec<Command>, String> {
        let i = self.index(id)?;
        if self.slots[i].live.is_none() {
            return Err(format!("{id} is not running"));
        }
        let build = |lua: &Lua| {
            let t = lua.create_table()?;
            t.set("kind", "message")?;
            t.set("name", name)?;
            t.set("data", sandbox::from_json(lua, data)?)?;
            Ok(t)
        };
        Ok(self.invoke(i, env, Call::Named("on"), Some(&build)))
    }

    /// Reload every running plugin whose folder changed since it started.
    pub fn reload_changed(&mut self, env: &mut Env) -> Vec<Command> {
        let mut out = Vec::new();
        for i in 0..self.slots.len() {
            let slot = &self.slots[i];
            let Files::Dir(dir) = &slot.pkg.files else {
                continue;
            };
            if slot.live.is_none() && slot.error.is_none() {
                continue;
            }
            let stamp = slot.pkg.files.stamp();
            if stamp == slot.stamp {
                continue;
            }
            let dir = dir.clone();
            let origin = slot.pkg.origin;
            match Package::from_dir(&dir, origin) {
                Ok(pkg) if pkg.manifest.id == slot.pkg.manifest.id => {
                    out.extend(self.stop(i, env, 0));
                    let slot = &mut self.slots[i];
                    slot.pkg = pkg;
                    slot.stamp = stamp;
                    out.push(Command::Log {
                        level: LogLevel::Info,
                        message: format!("{} changed on disk, reloaded", slot.pkg.manifest.name),
                    });
                    out.extend(self.start(i, env));
                }
                Ok(_) => {
                    self.slots[i].stamp = stamp;
                }
                Err(e) => {
                    let slot = &mut self.slots[i];
                    slot.stamp = stamp;
                    slot.error = Some(e);
                    out.push(Command::Changed {
                        id: slot.pkg.manifest.id.clone(),
                    });
                }
            }
        }
        out
    }

    /// The plugin folders were read again. New ones are added, missing ones
    /// stopped and dropped, and one whose files or version moved restarts.
    pub fn replace(&mut self, env: &mut Env, packages: Vec<Package>, store: &Store) -> Vec<Command> {
        let mut out = vec![Command::ListChanged];
        let mut i = 0;
        while i < self.slots.len() {
            let id = self.slots[i].pkg.manifest.id.clone();
            if packages.iter().any(|p| p.manifest.id == id) {
                i += 1;
            } else {
                out.extend(self.stop(i, env, STOP_FADE_MS));
                self.slots.remove(i);
            }
        }
        for pkg in packages {
            match self.index(&pkg.manifest.id) {
                Ok(i) => {
                    let slot = &self.slots[i];
                    let moved = slot.pkg.origin != pkg.origin
                        || slot.pkg.files.dir() != pkg.files.dir()
                        || slot.pkg.manifest != pkg.manifest
                        || slot.pkg.files.stamp() != slot.stamp;
                    if moved {
                        out.extend(self.stop(i, env, 0));
                        self.slots[i].pkg = pkg;
                        self.slots[i].error = None;
                        out.extend(self.start(i, env));
                    }
                }
                Err(_) => {
                    self.add(pkg, store);
                    let i = self.slots.len() - 1;
                    out.extend(self.start(i, env));
                }
            }
        }
        out
    }

    /// Switches and settings into the store, for saving.
    pub fn store_into(&self, store: &mut Store) {
        for slot in &self.slots {
            store.plugins.insert(
                slot.pkg.manifest.id.clone(),
                store::Entry {
                    enabled: slot.enabled,
                    settings: slot.stored.clone(),
                },
            );
        }
    }

    pub fn reports(&self) -> Vec<PluginReport> {
        self.slots.iter().map(|s| self.report_slot(s)).collect()
    }

    pub fn report(&self, id: &str) -> Option<PluginReport> {
        self.index(id).ok().map(|i| self.report_slot(&self.slots[i]))
    }

    /// The owner key behind each report, in the same order as [`Self::reports`].
    pub fn keys(&self) -> Vec<u32> {
        self.slots.iter().map(|s| s.key).collect()
    }

    fn report_slot(&self, s: &Slot) -> PluginReport {
        let m = &s.pkg.manifest;
        PluginReport {
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            description: m.description.clone(),
            icon: m.icon.clone(),
            author: m.author.clone(),
            platforms: m.platforms.clone(),
            supported: m.supports(self.os),
            origin: s.pkg.origin,
            dir: s.pkg.files.dir().map(|d| d.display().to_string()),
            url: s.pkg.url.clone(),
            ffi: m.ffi,
            enabled: s.enabled,
            running: s.live.is_some(),
            error: s.error.clone(),
            settings: s.settings.clone(),
            state: s.state.clone(),
            holds: Vec::new(),
        }
    }

    /// Whether a plugin of this origin can be removed: built-in ones only
    /// switch off.
    pub fn origin(&self, id: &str) -> Option<Origin> {
        self.index(id).ok().map(|i| self.slots[i].pkg.origin)
    }

    pub fn dir(&self, id: &str) -> Option<std::path::PathBuf> {
        self.index(id)
            .ok()
            .and_then(|i| self.slots[i].pkg.files.dir().map(|d| d.to_path_buf()))
    }
}

/// An mlua error as one readable paragraph: the message and where, without
/// the Rust side of the stack.
pub(crate) fn describe(e: mlua::Error) -> String {
    let mut text = match e {
        mlua::Error::CallbackError { cause, .. } => return describe((*cause).clone()),
        mlua::Error::RuntimeError(m) | mlua::Error::SyntaxError { message: m, .. } => m,
        other => other.to_string(),
    };
    if let Some(cut) = text.find("\nstack traceback:") {
        text.truncate(cut);
    }
    if text.len() > 600 {
        let mut end = 600;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(" (cut)");
    }
    text
}

fn refill(lua: &Lua, table: &Table, json: &serde_json::Value) -> mlua::Result<()> {
    let keys: Vec<Value> = table
        .pairs::<Value, Value>()
        .filter_map(|p| p.ok().map(|(k, _)| k))
        .collect();
    for k in keys {
        table.raw_set(k, Value::Nil)?;
    }
    if let Value::Table(fresh) = sandbox::from_json(lua, json)? {
        for pair in fresh.pairs::<Value, Value>() {
            let (k, v) = pair?;
            table.raw_set(k, v)?;
        }
    }
    Ok(())
}

pub(crate) fn wall_ms(host: &dyn Host) -> Option<u64> {
    match host.time_of_day()? {
        Event::TimeOfDay {
            minute,
            second,
            millisecond,
        } => Some(
            u64::from(minute) * 60_000 + u64::from(second) * 1000 + u64::from(millisecond),
        ),
        _ => None,
    }
}

/// Milliseconds from `now` to the next `at`, both in milliseconds of the
/// day. Never under a second: a timer that fires a hair early must not fire
/// twice.
pub(crate) fn until(at: u64, now: u64) -> u64 {
    let d = (at % DAY_MS + DAY_MS - now % DAY_MS) % DAY_MS;
    if d < 1000 {
        d + DAY_MS
    } else {
        d
    }
}

fn kind_of(e: &Event) -> Option<&'static str> {
    Some(match e {
        Event::AppFocus(_) => ids::APP_FOCUS,
        Event::Fullscreen { .. } => ids::APP_FULLSCREEN,
        Event::Maximized { .. } => ids::APP_MAXIMIZED,
        Event::Idle { .. } => ids::USER_IDLE,
        Event::SessionLock { .. } => ids::SESSION_LOCK,
        Event::PowerSource { .. } => ids::POWER_SOURCE,
        Event::AudioActive { .. } => ids::AUDIO_ACTIVE,
        Event::DisplayConfigChanged => ids::DISPLAY_CONFIG,
        Event::TimeOfDay { .. } => "clock",
        _ => return None,
    })
}

fn idle_matches(w: &Watch, e: &Event) -> bool {
    match (w.idle_after, e) {
        (Some(after), Event::Idle { idle: true, seconds }) => *seconds == after,
        _ => true,
    }
}

fn event_table(lua: &Lua, kind: &str, e: &Event) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("kind", kind)?;
    match e {
        Event::AppFocus(app) => {
            t.set("exe", app.exe.as_str())?;
            t.set("title", app.title.clone())?;
            t.set("pid", app.pid)?;
        }
        Event::Fullscreen { exclusive } => t.set("exclusive", *exclusive)?,
        Event::Maximized { maximized } => t.set("maximized", *maximized)?,
        Event::Idle { idle, seconds } => {
            t.set("idle", *idle)?;
            t.set("seconds", *seconds)?;
        }
        Event::SessionLock { locked } => t.set("locked", *locked)?,
        Event::PowerSource { on_battery } => t.set("on_battery", *on_battery)?,
        Event::AudioActive { active } => t.set("active", *active)?,
        Event::TimeOfDay {
            minute,
            second,
            millisecond,
        } => {
            t.set("minute", *minute)?;
            t.set("second", *second)?;
            t.set("ms", *millisecond)?;
        }
        _ => {}
    }
    Ok(t)
}
