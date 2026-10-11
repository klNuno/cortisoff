//! The Lua a plugin runs in, the same in the daemon and in the window.
//!
//! LuaJIT with the table, string, math and bit libraries and nothing that
//! reaches the machine: no `io`, `os`, `debug` or `package`, no way to load
//! bytecode, and a `require` that only finds files inside the plugin. The
//! machine is reached through `cx` in the daemon and `ui` in the window,
//! which is what lets cortisoff say what a plugin can do. A plugin asking for
//! `ffi` in its manifest gets LuaJIT's FFI and with it everything; the window
//! says so before it is switched on.
//!
//! The JIT compiler is off. Compiled LuaJIT code never calls hooks, and the
//! hook is what stops a plugin stuck in a loop before it freezes the daemon.
//! Plugin code is event handlers, and the interpreter runs those in
//! microseconds.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use cortisoff_host::Value as HostValue;
use mlua::{
    chunk::ChunkMode, Function, HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, Value,
    VmState,
};

use crate::package::Files;

/// Longest a single call into a plugin may run before it is stopped.
pub const BUDGET: Duration = Duration::from_secs(2);

/// Lines a plugin printed since the last drain.
#[derive(Default)]
struct Printed(Vec<String>);

/// A plugin's Lua state plus the watchdog deadline its hook reads.
pub struct Sandbox {
    pub lua: Lua,
    deadline: Rc<Cell<Option<Instant>>>,
}

impl Sandbox {
    pub fn new(files: &Files, ffi: bool) -> mlua::Result<Sandbox> {
        let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::BIT | StdLib::JIT;
        let lua = if ffi {
            // SAFETY: FFI is what the plugin asked for and what the user
            // accepted when switching it on; nothing else changes.
            unsafe { Lua::unsafe_new_with(libs | StdLib::FFI, LuaOptions::new()) }
        } else {
            Lua::new_with(libs, LuaOptions::new())?
        };
        lua.load("jit.off(); jit = nil").exec()?;

        let g = lua.globals();
        for name in ["dofile", "loadfile", "load", "loadstring", "module", "require"] {
            g.raw_set(name, Value::Nil)?;
        }
        if let Ok(string) = g.get::<Table>("string") {
            string.raw_set("dump", Value::Nil)?;
        }

        lua.set_app_data(Printed::default());
        g.set(
            "print",
            lua.create_function(|lua, args: mlua::Variadic<Value>| {
                let line = args
                    .iter()
                    .map(|v| v.to_string().unwrap_or_else(|_| format!("{v:?}")))
                    .collect::<Vec<_>>()
                    .join("\t");
                if let Some(mut printed) = lua.app_data_mut::<Printed>() {
                    if printed.0.len() < 256 {
                        printed.0.push(line);
                    }
                }
                Ok(())
            })?,
        )?;

        let files_for_require = files.clone();
        let loaded = lua.create_table()?;
        g.set(
            "require",
            lua.create_function(move |lua, name: String| {
                if let Some(v) = loaded.get::<Option<Value>>(name.as_str())? {
                    return Ok(v);
                }
                let rel = format!("{}.lua", name.replace('.', "/"));
                let text = files_for_require.read(&rel).map_err(mlua::Error::runtime)?;
                let v: Value = lua
                    .load(text)
                    .set_name(format!("@{rel}"))
                    .set_mode(ChunkMode::Text)
                    .call(name.as_str())?;
                let v = if v.is_nil() { Value::Boolean(true) } else { v };
                loaded.set(name.as_str(), v.clone())?;
                Ok(v)
            })?,
        )?;

        let api = lua.create_table()?;
        api.set("platform", std::env::consts::OS)?;
        api.set("version", env!("CARGO_PKG_VERSION"))?;
        let json = lua.create_table()?;
        json.set(
            "encode",
            lua.create_function(|lua, v: Value| {
                let j = to_json(lua, v)?;
                serde_json::to_string(&j).map_err(mlua::Error::external)
            })?,
        )?;
        json.set(
            "decode",
            lua.create_function(|lua, text: String| {
                let j: serde_json::Value =
                    serde_json::from_str(&text).map_err(mlua::Error::external)?;
                from_json(lua, &j)
            })?,
        )?;
        api.set("json", json)?;
        g.set("cortisoff", api)?;

        let deadline: Rc<Cell<Option<Instant>>> = Rc::default();
        let watch = deadline.clone();
        lua.set_hook(
            HookTriggers {
                every_nth_instruction: Some(20_000),
                ..HookTriggers::default()
            },
            move |_, _| match watch.get() {
                Some(d) if Instant::now() > d => Err(mlua::Error::runtime(format!(
                    "stopped after {} s without returning",
                    BUDGET.as_secs()
                ))),
                _ => Ok(VmState::Continue),
            },
        )?;

        Ok(Sandbox { lua, deadline })
    }

    /// Runs the plugin's entry file and returns the table it hands back.
    pub fn load_module(&self, files: &Files, entry: &str) -> mlua::Result<Table> {
        let text = files.read(entry).map_err(mlua::Error::runtime)?;
        self.guard(|| {
            self.lua
                .load(text)
                .set_name(format!("@{entry}"))
                .set_mode(ChunkMode::Text)
                .eval::<Table>()
        })
    }

    /// Runs `f` with the watchdog armed.
    pub fn guard<R>(&self, f: impl FnOnce() -> mlua::Result<R>) -> mlua::Result<R> {
        self.deadline.set(Some(Instant::now() + BUDGET));
        let out = f();
        self.deadline.set(None);
        out
    }

    /// What the plugin printed since the last call.
    pub fn printed(&self) -> Vec<String> {
        self.lua
            .app_data_mut::<Printed>()
            .map(|mut p| std::mem::take(&mut p.0))
            .unwrap_or_default()
    }
}

/// A function of the module, if the plugin defines it.
pub fn function(module: &Table, name: &str) -> Option<Function> {
    module.get::<Option<Function>>(name).ok().flatten()
}

/// Lua to JSON. Functions and userdata are skipped rather than refused, so a
/// settings table holding a helper still saves.
pub fn to_json(lua: &Lua, v: Value) -> mlua::Result<serde_json::Value> {
    lua.from_value_with(
        v,
        mlua::serde::DeserializeOptions::new()
            .deny_unsupported_types(false)
            .deny_recursive_tables(false),
    )
}

/// JSON to Lua. `null` becomes `nil`, never a sentinel a plugin would have
/// to know about.
pub fn from_json(lua: &Lua, j: &serde_json::Value) -> mlua::Result<Value> {
    lua.to_value_with(
        j,
        mlua::serde::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

/// What a plugin hands `cx:claim`, as a capability value. A whole number is
/// an integer, so `3200.0` from arithmetic reaches a colour temperature as
/// 3200. A table with `width`, `height` and `refresh` (Hz) is a display mode.
pub fn to_host(v: &Value) -> Result<HostValue, String> {
    match v {
        Value::Boolean(b) => Ok(HostValue::Bool(*b)),
        Value::Integer(i) => Ok(HostValue::Int(*i as i64)),
        Value::Number(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => Ok(HostValue::Int(*n as i64)),
        Value::Number(n) => Ok(HostValue::Float(*n)),
        Value::String(s) => Ok(HostValue::Text(s.to_string_lossy())),
        Value::Table(t) => {
            let num = |k: &str| t.get::<Option<f64>>(k).ok().flatten();
            let (width, height, refresh) = (num("width"), num("height"), num("refresh"));
            if width.is_none() && height.is_none() && refresh.is_none() {
                return Err("a table value needs width, height or refresh".into());
            }
            Ok(HostValue::Mode {
                width: width.unwrap_or(0.0) as u32,
                height: height.unwrap_or(0.0) as u32,
                refresh_mhz: (refresh.unwrap_or(0.0) * 1000.0).round() as u32,
            })
        }
        other => Err(format!("{} cannot be a capability value", other.type_name())),
    }
}

/// The other way, for `cx:rest` and for events.
pub fn from_host(lua: &Lua, v: &HostValue) -> mlua::Result<Value> {
    Ok(match v {
        HostValue::Int(i) => Value::Number(*i as f64),
        HostValue::Float(f) => Value::Number(*f),
        HostValue::Bool(b) => Value::Boolean(*b),
        HostValue::Text(s) => Value::String(lua.create_string(s)?),
        HostValue::Mode {
            width,
            height,
            refresh_mhz,
        } => {
            let t = lua.create_table()?;
            t.set("width", *width)?;
            t.set("height", *height)?;
            t.set("refresh", f64::from(*refresh_mhz) / 1000.0)?;
            Value::Table(t)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(script: &'static str) -> Files {
        let leaked: &'static [(&'static str, &'static str)] = Box::leak(Box::new([
            ("init.lua", script),
            ("lib/util.lua", "return { twice = function(x) return x * 2 end }"),
        ]));
        Files::Builtin(leaked)
    }

    #[test]
    fn nothing_reaches_the_machine() {
        let f = files("return {}");
        let s = Sandbox::new(&f, false).unwrap();
        let reach: bool = s
            .lua
            .load("return io ~= nil or os ~= nil or debug ~= nil or package ~= nil or load ~= nil or loadstring ~= nil or dofile ~= nil or string.dump ~= nil or ffi ~= nil or jit ~= nil")
            .eval()
            .unwrap();
        assert!(!reach);
    }

    #[test]
    fn require_reads_only_the_plugin() {
        let f = files("local u = require('lib.util') return { n = u.twice(21) }");
        let s = Sandbox::new(&f, false).unwrap();
        let m = s.load_module(&f, "init.lua").unwrap();
        assert_eq!(m.get::<i64>("n").unwrap(), 42);
        let err = s
            .lua
            .load("return require('..secret')")
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("not part of this plugin") || err.contains("outside"), "{err}");
    }

    #[test]
    fn a_plugin_stuck_in_a_loop_is_stopped() {
        let f = files("return {}");
        let s = Sandbox::new(&f, false).unwrap();
        let started = Instant::now();
        let err = s
            .guard(|| s.lua.load("while true do end").exec())
            .unwrap_err()
            .to_string();
        assert!(err.contains("without returning"), "{err}");
        assert!(started.elapsed() < BUDGET + Duration::from_secs(2));
    }

    #[test]
    fn values_cross_both_ways() {
        let f = files("return {}");
        let s = Sandbox::new(&f, false).unwrap();
        assert_eq!(to_host(&Value::Number(3200.0)), Ok(HostValue::Int(3200)));
        assert_eq!(to_host(&Value::Number(0.5)), Ok(HostValue::Float(0.5)));
        let mode: Value = s.lua.load("return { refresh = 59.94 }").eval().unwrap();
        assert_eq!(
            to_host(&mode),
            Ok(HostValue::Mode {
                width: 0,
                height: 0,
                refresh_mhz: 59_940
            })
        );
        let j = serde_json::json!({ "night": 3200, "except": ["vlc.exe"], "gone": null });
        let t = from_json(&s.lua, &j).unwrap();
        let back = to_json(&s.lua, t).unwrap();
        assert_eq!(back["night"].as_f64(), Some(3200.0));
        assert_eq!(back["except"][0], "vlc.exe");
        assert!(back.get("gone").is_none());
    }

    #[test]
    fn print_is_collected_not_written() {
        let f = files("return {}");
        let s = Sandbox::new(&f, false).unwrap();
        s.lua.load("print('a', 1, true)").exec().unwrap();
        assert_eq!(s.printed(), vec!["a\t1\ttrue".to_owned()]);
        assert!(s.printed().is_empty());
    }
}
