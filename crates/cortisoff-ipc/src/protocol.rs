//! What the daemon and its clients say to each other.
//!
//! Every type here is plain data with a serde derive. The CLI, the Tauri
//! dashboard and any third-party client speak this and nothing else, which is
//! what keeps the daemon free of a webview and free of a windowing crate.

use cortisoff_core::Rule;
use cortisoff_host::{Action, AppRef, CapabilityId, Control, Event, LogLevel, Value};
use serde::{Deserialize, Serialize};

/// What a client asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Everything `cortisoff doctor` prints.
    Doctor,
    /// The live world plus who holds what.
    State,
    /// Start receiving [`Notification`]s on this connection. What `cortisoff
    /// trace` does, and what the dashboard leaves open while it is on screen.
    Subscribe,
    Unsubscribe,
    /// A manual override, outranking every rule until it is cleared. The
    /// dashboard's sliders and `cortisoff set`.
    Set {
        cap: CapabilityId,
        value: Value,
    },
    /// Drop the manual override and let the rules have the capability back.
    Clear {
        cap: CapabilityId,
    },
    /// Show a value while a slider is being dragged. Outranks even a manual
    /// override and lasts until [`Request::PreviewEnd`] or the connection
    /// closes, whichever comes first, so a dashboard that crashes mid-drag
    /// leaves nothing behind.
    Preview {
        cap: CapabilityId,
        value: Value,
    },
    /// The drag ended. The capability fades back to whatever held it before
    /// the preview, a rule, a manual override or the screen's own rest.
    PreviewEnd {
        cap: CapabilityId,
    },
    /// The rules as authored, which is what an editor needs and what
    /// [`DoctorReport::rules`] deliberately is not: that one says what this
    /// machine did with them.
    Rules,
    /// Replace the rule file with these, then reload.
    ///
    /// The whole set, not one rule, because that is what makes switching a
    /// rule off, editing it, adding one and deleting one the same operation,
    /// with no identity to keep in sync between the file and the client. A set
    /// that does not load leaves the previous file in place.
    SaveRules {
        rules: Vec<Rule>,
    },
    /// Re-read the rule files. A syntax error leaves the running rules alone
    /// and comes back as [`Response::Error`].
    Reload,
    /// Exit cleanly, restoring every baseline on the way out.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Doctor(Box<DoctorReport>),
    State(Box<StateReport>),
    /// A named field rather than a newtype, because this enum is internally
    /// tagged and serde cannot put a tag next to a bare sequence. A newtype
    /// here compiles and then panics the daemon the first time a client asks
    /// for the rules.
    Rules {
        rules: Vec<Rule>,
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
    /// A rule's condition changed truth value. The line `cortisoff trace`
    /// exists for.
    RuleEdge {
        rule: String,
        holding: bool,
    },
    /// What the engine decided to do about it.
    Action(Action),
    Log {
        level: LogLevel,
        message: String,
    },
    /// The daemon is going down. No further notification follows.
    Closing,
}

/// One capability as this machine reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityReport {
    pub id: CapabilityId,
    /// The implementation actually chosen, such as `nvapi` or `magnification`.
    /// "vibrance works" and "vibrance works through NVAPI" are different bug
    /// reports.
    pub backend: Option<String>,
    /// `None` when the capability is present. A sentence naming the missing
    /// piece otherwise, never the failing call.
    pub missing_because: Option<String>,
    /// Whether this capability can be written to. `app.focus` and
    /// `session.lock` are things the machine tells cortisoff, not things
    /// cortisoff sets, and a client offering an override on one of them is
    /// offering something the daemon will refuse.
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RuleStateReport {
    Active,
    /// Switched off by the user. Separate from the others because it is the one
    /// a client can undo, and the only one that is not a problem.
    Disabled,
    Unsupported {
        missing: Vec<CapabilityId>,
    },
    Empty,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleReport {
    pub name: String,
    pub priority: i32,
    pub state: RuleStateReport,
    /// Whether its condition holds right now.
    pub holding: bool,
    pub sets: Vec<CapabilityId>,
}

/// Who owns a capability at this instant, and what the user's screen had
/// before anyone touched it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimReport {
    pub cap: CapabilityId,
    /// The rule's name, or `manual`.
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
    /// At least one unblocked claim is held; not a hardware readback.
    #[serde(default)]
    pub active: bool,
    pub world: WorldReport,
    pub rules: Vec<RuleReport>,
    pub claims: Vec<ClaimReport>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub version: String,
    /// The daemon's own process id, for a client that shows what the daemon
    /// costs.
    pub pid: u32,
    pub capabilities: Vec<CapabilityReport>,
    pub rules: Vec<RuleReport>,
    /// Capabilities whose baseline is still journaled, meaning a previous run
    /// died with a value applied and the restore has not completed.
    pub journal_dirty: Vec<CapabilityId>,
    /// Anything the user should know that is not an outright failure. A
    /// competing colour application poisoning the baseline lands here.
    pub warnings: Vec<String>,
}
