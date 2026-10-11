//! What the daemon and its clients say to each other.
//!
//! Every type here is plain data with a serde derive. The CLI, the window and
//! any third-party client speak this and nothing else. A plugin's settings
//! and the state it publishes cross as JSON the plugin shapes itself, so the
//! protocol never changes for a new plugin.

use cortisoff_host::{Action, AppRef, CapabilityId, Control, Event, LogLevel, Value, WindowInfo};
use serde::{Deserialize, Serialize};

/// Free-form data a plugin owns: its settings, the state it publishes, a
/// message from its page.
pub type Json = serde_json::Value;

/// What a client asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Everything `cortisoff-cli doctor` prints.
    Doctor,
    /// The live world, every plugin, and who holds what.
    State,
    /// Start receiving [`Notification`]s on this connection. What `trace`
    /// does, and what the window leaves open while it is on screen.
    Subscribe,
    Unsubscribe,
    /// A manual override, outranking every plugin until it is cleared.
    Set {
        cap: CapabilityId,
        value: Value,
    },
    /// Drop the manual override and let the plugins have the capability back.
    Clear {
        cap: CapabilityId,
    },
    /// Show a value while a slider is being dragged. Outranks even a manual
    /// override and lasts until [`Request::PreviewEnd`] or the connection
    /// closes, whichever comes first, so a window that crashes mid-drag
    /// leaves nothing behind.
    Preview {
        cap: CapabilityId,
        value: Value,
    },
    /// The drag ended. The capability eases back to whatever held it before.
    PreviewEnd {
        cap: CapabilityId,
    },
    /// Every plugin found, enabled or not.
    Plugins,
    /// Switch a plugin on or off. Off releases everything it holds.
    Enable {
        id: String,
        enabled: bool,
    },
    /// Replace a plugin's settings. The daemon stores them and tells the
    /// plugin.
    Settings {
        id: String,
        settings: Json,
    },
    /// Something a plugin's page sent its own daemon half: a button pressed,
    /// an app picked.
    Message {
        id: String,
        name: String,
        #[serde(default)]
        data: Json,
    },
    /// Read the plugin folders again: a client installed, updated or removed
    /// one on disk.
    Rescan,
    /// Load a plugin from a folder in place and reload it whenever a file in
    /// it changes. What a plugin author works with.
    Link {
        path: String,
    },
    /// Forget a linked folder or delete an installed plugin.
    Remove {
        id: String,
    },
    /// The open windows, for a page that lets the user pick an app.
    Windows,
    /// Exit cleanly, restoring every baseline on the way out.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Doctor(Box<DoctorReport>),
    State(Box<StateReport>),
    /// Named fields rather than newtypes: this enum is internally tagged and
    /// serde cannot put a tag next to a bare sequence. A newtype here
    /// compiles and then panics the daemon the first time a client asks.
    Plugins {
        plugins: Vec<PluginReport>,
    },
    Windows {
        windows: Vec<WindowInfo>,
    },
    /// Never a panic and never a dropped connection. A client that asks for
    /// something impossible gets a sentence it can print.
    Error {
        message: String,
    },
}

impl Response {
    pub fn error(message: impl Into<String>) -> Self {
        Response::Error {
            message: message.into(),
        }
    }
}

/// Pushed to subscribers, unprompted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "notify", rename_all = "snake_case")]
pub enum Notification {
    /// Something the platform observed.
    Event(Event),
    /// What the daemon pushed to the hardware about it.
    Action(Action),
    Log {
        level: LogLevel,
        message: String,
    },
    /// One plugin changed: it published a state, its settings moved, it
    /// failed or came back.
    Plugin(Box<PluginReport>),
    /// Plugins appeared or went away. Ask for [`Request::Plugins`] again.
    Plugins,
    /// The daemon is going down. No further notification follows.
    Closing,
}

/// Where a plugin's files come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Shipped inside cortisoff.
    Builtin,
    /// Downloaded into the data folder.
    Installed,
    /// A folder the user pointed at, reloaded as it changes.
    Linked,
}

/// One plugin, as the daemon runs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginReport {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    pub platforms: Vec<String>,
    /// Whether this system is one of `platforms`.
    pub supported: bool,
    pub origin: Origin,
    /// The folder the files are read from. `None` for a built-in plugin,
    /// whose files are inside the binary.
    #[serde(default)]
    pub dir: Option<String>,
    /// Where it was installed from, for updates.
    #[serde(default)]
    pub url: Option<String>,
    /// The plugin asked for native calls through LuaJIT's FFI.
    #[serde(default)]
    pub ffi: bool,
    pub enabled: bool,
    /// Loaded and started. False while disabled, unsupported or broken.
    pub running: bool,
    /// Why it is not running, or the last error a call into it raised.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub settings: Json,
    /// What the plugin last published with `cx:publish`.
    #[serde(default)]
    pub state: Json,
    /// Capabilities it holds right now.
    #[serde(default)]
    pub holds: Vec<CapabilityId>,
}

/// One capability as this machine reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityReport {
    pub id: CapabilityId,
    /// The implementation actually chosen, such as `nvapi` or `magnification`.
    pub backend: Option<String>,
    /// `None` when the capability is present. A sentence naming the missing
    /// piece otherwise, never the failing call.
    pub missing_because: Option<String>,
    /// Whether this capability can be written to. `app.focus` is something
    /// the machine tells cortisoff, not something cortisoff sets.
    pub settable: bool,
    /// A short name for a person, such as `Colour temperature`.
    pub label: String,
    /// What a client should draw to set it. `None` when it is not settable.
    pub control: Option<Control>,
}

impl CapabilityReport {
    pub fn is_available(&self) -> bool {
        self.missing_because.is_none()
    }
}

/// Who owns a capability at this instant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimReport {
    pub cap: CapabilityId,
    /// The plugin's name, `manual` or `preview`.
    pub owner: String,
    pub priority: i32,
    pub value: Value,
    /// Claims sitting underneath, highest priority first. What the screen
    /// falls back to when this one lets go.
    pub shadowed: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldReport {
    pub app: Option<AppRef>,
    pub fullscreen: bool,
    pub maximized: bool,
    pub idle: bool,
    pub locked: bool,
    pub on_battery: bool,
    pub audio_active: bool,
    pub minute_of_day: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StateReport {
    /// An enabled plugin or an unblocked manual claim; not a hardware readback.
    #[serde(default)]
    pub active: bool,
    pub world: WorldReport,
    pub plugins: Vec<PluginReport>,
    pub claims: Vec<ClaimReport>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub version: String,
    /// The daemon's own process id, for a client that shows what the daemon
    /// costs.
    pub pid: u32,
    pub capabilities: Vec<CapabilityReport>,
    pub plugins: Vec<PluginReport>,
    /// Capabilities whose baseline is still journaled, meaning a previous run
    /// died with a value applied and the restore has not completed.
    pub journal_dirty: Vec<CapabilityId>,
    /// Anything the user should know that is not an outright failure. A
    /// competing colour application poisoning the baseline lands here.
    pub warnings: Vec<String>,
}
