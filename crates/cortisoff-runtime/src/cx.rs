//! `cx`: everything a plugin can do to the machine, handed to each call of
//! its daemon half.
//!
//! A `cx` lives for one call. What it is asked to change on screen goes back
//! to the daemon as [`Command`]s, so the arbiter still decides who wins a
//! capability; what only reads, or only touches this plugin, happens here.

use std::time::Duration;

use cortisoff_core::priority;
use cortisoff_host::{CapabilityId, Chord, LogLevel, WindowAction};
use mlua::{Function, Lua, Table, UserData, UserDataMethods, Value, Variadic};

use crate::runtime::{until, wall_ms, Binding, Command, Env, HttpDone, HttpWait, Shared, Timer, Watch, KINDS};
use crate::sandbox;

/// Shortest repeat for `cx:every`. A plugin polling faster than this is a
/// plugin burning a core.
const MIN_EVERY_MS: u64 = 50;

/// Longest fade a claim may ask for: a day.
const MAX_FADE_MS: u64 = 86_400_000;

/// Largest HTTP answer handed to a plugin.
const HTTP_LIMIT: u64 = 4 * 1024 * 1024;

pub(crate) struct Cx<'a, 'e> {
    pub key: u32,
    pub id: &'a str,
    pub name: &'a str,
    pub os: &'static str,
    pub env: &'a mut Env<'e>,
    pub shared: &'a mut Shared,
    pub watches: &'a mut Vec<Watch>,
    pub out: &'a mut Vec<Command>,
    pub published: &'a mut Option<serde_json::Value>,
}

/// `true`, or `nil` and a sentence, the way Lua's own `io` answers.
type Answer = (Value, Option<String>);

fn ok() -> mlua::Result<Answer> {
    Ok((Value::Boolean(true), None))
}

fn refuse(why: impl Into<String>) -> mlua::Result<Answer> {
    Ok((Value::Nil, Some(why.into())))
}

fn answer(r: Result<(), String>) -> mlua::Result<Answer> {
    match r {
        Ok(()) => ok(),
        Err(e) => refuse(e),
    }
}

fn line(args: &Variadic<Value>) -> String {
    args.iter()
        .map(|v| v.to_string().unwrap_or_else(|_| format!("{v:?}")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn opt<T: mlua::FromLua>(t: &Option<Table>, key: &str) -> mlua::Result<Option<T>> {
    match t {
        Some(t) => t.get::<Option<T>>(key),
        None => Ok(None),
    }
}

fn fade_of(t: &Option<Table>) -> mlua::Result<u64> {
    let ms = opt::<f64>(t, "fade")?.unwrap_or(0.0);
    Ok((ms.max(0.0) as u64).min(MAX_FADE_MS))
}

/// `"20:30"`, `"7:05"` or a minute of the day.
fn minute_of(v: &Value) -> Result<u64, String> {
    match v {
        Value::Integer(m) => Ok((*m).rem_euclid(1440) as u64),
        Value::Number(m) => Ok((m.round() as i64).rem_euclid(1440) as u64),
        Value::String(s) => {
            let s = s.to_string_lossy();
            let (h, m) = s
                .split_once(':')
                .ok_or_else(|| format!("{s:?} is not HH:MM"))?;
            let h: u64 = h.trim().parse().map_err(|_| format!("{s:?} is not HH:MM"))?;
            let m: u64 = m.trim().parse().map_err(|_| format!("{s:?} is not HH:MM"))?;
            if h > 23 || m > 59 {
                return Err(format!("{s:?} is not a time of day"));
            }
            Ok(h * 60 + m)
        }
        other => Err(format!("a time of day, not a {}", other.type_name())),
    }
}

impl Cx<'_, '_> {
    fn timer(&mut self, due: u64, every: Option<u64>, wall: Option<u64>, callback: Function) -> u64 {
        let id = self.shared.next_timer;
        self.shared.next_timer += 1;
        self.shared.timers.push(Timer {
            id,
            key: self.key,
            due,
            every,
            wall,
            callback,
        });
        id
    }

    fn cap(&self, cap: &str) -> Result<CapabilityId, String> {
        let id = CapabilityId::new(cap);
        if !self.env.probed.has(&id) {
            let why = self
                .env
                .probed
                .reason_missing(&id)
                .map(str::to_owned)
                .unwrap_or_else(|| "this machine does not offer it".to_owned());
            return Err(format!("{cap}: {why}"));
        }
        if self.env.host.actuator(&id).is_none() {
            return Err(format!("{cap} is something to watch, not to set"));
        }
        Ok(id)
    }

    fn world(&self, lua: &Lua) -> mlua::Result<Table> {
        let w = self.env.world;
        let t = lua.create_table()?;
        if let Some(app) = &w.app {
            let a = lua.create_table()?;
            a.set("exe", app.exe.as_str())?;
            a.set("title", app.title.clone())?;
            a.set("pid", app.pid)?;
            t.set("app", a)?;
        }
        t.set("fullscreen", w.fullscreen)?;
        t.set("maximized", w.maximized)?;
        t.set("idle", w.idle)?;
        t.set("idle_seconds", w.idle_seconds)?;
        t.set("locked", w.locked)?;
        t.set("on_battery", w.on_battery)?;
        t.set("audio", w.audio_active)?;
        Ok(t)
    }
}

impl UserData for Cx<'_, '_> {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("id", |_, this, ()| Ok(this.id.to_owned()));
        m.add_method("platform", |_, this, ()| Ok(this.os));

        // ---- what to listen to -------------------------------------------

        m.add_method_mut(
            "watch",
            |_, this, (kind, a, b): (String, Value, Option<Table>)| {
                if !KINDS.contains(&kind.as_str()) {
                    return Err(mlua::Error::runtime(format!(
                        "cx:watch: {kind:?} is not one of {}",
                        KINDS.join(", ")
                    )));
                }
                let (callback, opts) = match a {
                    Value::Function(f) => (Some(f), b),
                    Value::Table(t) => (None, Some(t)),
                    Value::Nil => (None, b),
                    other => {
                        return Err(mlua::Error::runtime(format!(
                            "cx:watch: a function or options, not a {}",
                            other.type_name()
                        )))
                    }
                };
                let idle_after = opt::<f64>(&opts, "after")?.map(|s| s.max(1.0) as u32);
                if idle_after.is_some() && kind != cortisoff_host::ids::USER_IDLE {
                    return Err(mlua::Error::runtime("cx:watch: `after` is for user.idle"));
                }
                this.watches.push(Watch {
                    kind,
                    callback,
                    idle_after,
                });
                this.shared.sources_dirty = true;
                Ok(())
            },
        );

        m.add_method_mut("unwatch", |_, this, kind: String| {
            this.watches.retain(|w| w.kind != kind);
            this.shared.sources_dirty = true;
            Ok(())
        });

        // ---- time ----------------------------------------------------------

        m.add_method_mut("every", |_, this, (ms, f): (f64, Function)| {
            let every = (ms.max(0.0) as u64).max(MIN_EVERY_MS);
            let due = this.env.now + every;
            Ok(this.timer(due, Some(every), None, f))
        });

        m.add_method_mut("after", |_, this, (ms, f): (f64, Function)| {
            let due = this.env.now + ms.max(0.0) as u64;
            Ok(this.timer(due, None, None, f))
        });

        m.add_method_mut("at", |_, this, (when, f): (Value, Function)| {
            let minute = minute_of(&when).map_err(mlua::Error::runtime)?;
            let wall = wall_ms(this.env.host)
                .ok_or_else(|| mlua::Error::runtime("cx:at: this system gave no clock"))?;
            let at = minute * 60_000;
            let due = this.env.now + until(at, wall);
            Ok(this.timer(due, None, Some(at), f))
        });

        m.add_method_mut("cancel", |_, this, id: u64| {
            let key = this.key;
            this.shared.timers.retain(|t| !(t.key == key && t.id == id));
            Ok(())
        });

        // Minute of the local day with its fraction, and the monotonic
        // milliseconds that timers count in.
        m.add_method("now", |_, this, ()| {
            let minute = wall_ms(this.env.host).map(|ms| ms as f64 / 60_000.0);
            Ok((minute, this.env.now))
        });

        // ---- the screen and everything else the arbiter shares -------------

        m.add_method_mut(
            "claim",
            |lua, this, (cap, value, opts): (String, Value, Option<Table>)| {
                let id = match this.cap(&cap) {
                    Ok(id) => id,
                    Err(e) => return refuse(e),
                };
                let value = match sandbox::to_host(&value) {
                    Ok(v) => v,
                    Err(e) => return refuse(format!("{cap}: {e}")),
                };
                let _ = lua;
                let priority = opt::<i32>(&opts, "priority")?
                    .unwrap_or(priority::PLUGIN)
                    .clamp(priority::BACKGROUND, priority::MANUAL - 1);
                this.out.push(Command::Claim {
                    owner: this.key,
                    cap: id,
                    value,
                    priority,
                    fade_ms: fade_of(&opts)?,
                });
                ok()
            },
        );

        m.add_method_mut("release", |_, this, (cap, opts): (String, Option<Table>)| {
            this.out.push(Command::Release {
                owner: this.key,
                cap: CapabilityId::new(cap),
                fade_ms: fade_of(&opts)?,
            });
            Ok(())
        });

        m.add_method_mut("release_all", |_, this, opts: Option<Table>| {
            this.out.push(Command::ReleaseAll {
                owner: this.key,
                fade_ms: fade_of(&opts)?,
            });
            Ok(())
        });

        m.add_method("has", |_, this, cap: String| {
            Ok(this.env.probed.has(&CapabilityId::new(cap)))
        });

        // What the capability shows when nobody holds it, where that is a
        // number a fade can start from.
        m.add_method("rest", |lua, this, cap: String| {
            match this
                .env
                .host
                .actuator(&CapabilityId::new(cap))
                .and_then(|a| a.rest())
            {
                Some(v) => sandbox::from_host(lua, &v),
                None => Ok(Value::Nil),
            }
        });

        m.add_method("world", |lua, this, ()| this.world(lua));

        // ---- talking back --------------------------------------------------

        m.add_method_mut("publish", |lua, this, state: Value| {
            *this.published = Some(sandbox::to_json(lua, state)?);
            Ok(())
        });

        m.add_method_mut("log", |_, this, args: Variadic<Value>| {
            this.out.push(Command::Log {
                level: LogLevel::Info,
                message: format!("{}: {}", this.name, line(&args)),
            });
            Ok(())
        });

        m.add_method_mut("warn", |_, this, args: Variadic<Value>| {
            this.out.push(Command::Log {
                level: LogLevel::Warn,
                message: format!("{}: {}", this.name, line(&args)),
            });
            Ok(())
        });

        // ---- the desktop, AutoHotkey style -----------------------------------

        m.add_method_mut("run", |_, this, (program, args): (String, Variadic<String>)| {
            if program.trim().is_empty() {
                return Err(mlua::Error::runtime("cx:run: no program"));
            }
            this.out.push(Command::Run {
                program,
                args: args.into_iter().collect(),
            });
            Ok(())
        });

        m.add_method_mut("hotkey", |_, this, (chord, f): (String, Option<Function>)| {
            let chord: Chord = match chord.parse() {
                Ok(c) => c,
                Err(e) => return Ok((Value::Nil, Some(e))),
            };
            if let Some(b) = this.shared.hotkeys.iter().find(|b| b.chord == chord) {
                if b.key != this.key {
                    return Ok((Value::Nil, Some(format!("{chord} is taken by another plugin"))));
                }
            }
            let id = this.shared.next_hotkey;
            this.shared.next_hotkey += 1;
            this.shared.hotkeys.push(Binding {
                id,
                key: this.key,
                chord,
                callback: f,
            });
            this.shared.hotkeys_dirty = true;
            Ok((Value::Integer(i64::from(id) as _), None))
        });

        m.add_method_mut("unhotkey", |_, this, id: u32| {
            let key = this.key;
            let before = this.shared.hotkeys.len();
            this.shared.hotkeys.retain(|b| !(b.key == key && b.id == id));
            this.shared.hotkeys_dirty |= this.shared.hotkeys.len() != before;
            Ok(())
        });

        m.add_method_mut("send_keys", |_, this, chord: String| {
            match chord.parse::<Chord>() {
                Ok(c) => answer(this.env.host.send_keys(&c)),
                Err(e) => refuse(e),
            }
        });

        m.add_method_mut("type", |_, this, text: String| {
            answer(this.env.host.type_text(&text))
        });

        m.add_method_mut("notify", |_, this, (title, body): (String, Option<String>)| {
            answer(this.env.host.notify(&title, body.as_deref().unwrap_or("")))
        });

        m.add_method("clipboard", |_, this, ()| match this.env.host.clipboard() {
            Ok(text) => Ok((Some(text), None)),
            Err(e) => Ok((None, Some(e))),
        });

        m.add_method_mut("set_clipboard", |_, this, text: String| {
            answer(this.env.host.set_clipboard(&text))
        });

        m.add_method("windows", |lua, this, ()| {
            let list = lua.create_table()?;
            for (i, w) in this.env.host.windows().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("id", w.id)?;
                t.set("title", w.title)?;
                t.set("exe", w.exe)?;
                t.set("pid", w.pid)?;
                t.set("focused", w.focused)?;
                list.raw_set(i + 1, t)?;
            }
            Ok(list)
        });

        m.add_method_mut("window", |_, this, (id, action): (u64, String)| {
            match action.parse::<WindowAction>() {
                Ok(a) => answer(this.env.host.window_action(id, a)),
                Err(e) => refuse(e),
            }
        });

        // ---- the network -----------------------------------------------------

        // Starts the request on its own thread and returns its id; `f` hears
        // `{ kind = "http", ok, status, body, error }` when it ends.
        m.add_method_mut(
            "http",
            |_, this, (url, a, b): (String, Value, Option<Function>)| {
                let (opts, f) = match (a, b) {
                    (Value::Function(f), None) => (None, f),
                    (Value::Table(t), Some(f)) => (Some(t), f),
                    (Value::Nil, Some(f)) => (None, f),
                    _ => {
                        return Err(mlua::Error::runtime(
                            "cx:http(url, [options], function(cx, s, answer))",
                        ))
                    }
                };
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err(mlua::Error::runtime(format!("cx:http: {url:?} is not a web address")));
                }
                let method = opt::<String>(&opts, "method")?
                    .unwrap_or_else(|| "GET".into())
                    .to_ascii_uppercase();
                let body = opt::<mlua::LuaString>(&opts, "body")?.map(|s| s.as_bytes().to_vec());
                let timeout = opt::<f64>(&opts, "timeout")?.unwrap_or(15_000.0).clamp(500.0, 120_000.0);
                let mut headers = Vec::new();
                if let Some(h) = opt::<Table>(&opts, "headers")? {
                    for pair in h.pairs::<String, String>() {
                        headers.push(pair?);
                    }
                }
                let id = this.shared.next_http;
                this.shared.next_http += 1;
                this.shared.http.push(HttpWait {
                    id,
                    key: this.key,
                    callback: f,
                });
                let tx = this.shared.http_tx.clone();
                let waker = this.shared.waker.clone();
                let request = Request {
                    url,
                    method,
                    headers,
                    body,
                    timeout: Duration::from_millis(timeout as u64),
                };
                std::thread::Builder::new()
                    .name("cortisoff-http".into())
                    .spawn(move || {
                        let done = request.send(id);
                        let _ = tx.send(done);
                        if let Some(w) = waker {
                            w();
                        }
                    })
                    .map_err(mlua::Error::external)?;
                Ok(id)
            },
        );
    }
}

struct Request {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    timeout: Duration,
}

impl Request {
    fn send(self, id: u64) -> HttpDone {
        match self.try_send() {
            Ok((status, body)) => HttpDone {
                id,
                status,
                body,
                error: None,
            },
            Err(e) => HttpDone {
                id,
                status: 0,
                body: String::new(),
                error: Some(e),
            },
        }
    }

    fn try_send(self) -> Result<(u16, String), String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .user_agent(concat!("cortisoff/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let uri: ureq::http::Uri = self.url.parse().map_err(|e| format!("{}: {e}", self.url))?;
        let method: ureq::http::Method = self
            .method
            .parse()
            .map_err(|_| format!("{:?} is not an HTTP method", self.method))?;
        let mut req = ureq::http::Request::builder().method(method).uri(uri);
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let sent = match self.body {
            Some(body) => agent.run(req.body(body).map_err(|e| e.to_string())?),
            None => agent.run(req.body(()).map_err(|e| e.to_string())?),
        };
        let mut resp = sent.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .with_config()
            .limit(HTTP_LIMIT)
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok((status, body))
    }
}
