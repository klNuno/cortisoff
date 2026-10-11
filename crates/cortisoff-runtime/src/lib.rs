//! Plugins for cortisoff: finding them, running their Lua in a sandbox, the
//! `cx` API their daemon half calls, their settings, and installing them
//! from the web.
//!
//! A plugin is a folder with a `plugin.toml` and Lua. It returns a table:
//!
//! ```lua
//! local plugin = { settings = { night = 3200 } }
//! function plugin.start(cx, s) cx:every(60000, function(cx, s) end) end
//! function plugin.on(cx, s, ev) end      -- events, settings, messages
//! function plugin.stop(cx, s) end
//! function plugin.status(s, state) return { line = "" } end  -- window
//! function plugin.page(ui, s, state) end                      -- window
//! return plugin
//! ```
//!
//! The daemon calls `start`, `on` and `stop`; the window calls `status` and
//! `page`, each in its own Lua state, and the two halves talk through
//! settings, `cx:publish` and `ui:send`.

mod cx;
pub mod install;
pub mod manifest;
pub mod package;
mod runtime;
pub mod sandbox;
pub mod store;

pub use manifest::Manifest;
pub use package::{builtin, discover, Files, Package};
pub use runtime::{Command, Env, HttpDone, Runtime, KINDS};
pub use sandbox::Sandbox;
pub use store::Store;
