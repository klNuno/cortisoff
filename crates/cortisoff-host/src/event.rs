use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::capability::CapabilityId;
use crate::value::Value;

/// The application that owns the foreground.
///
/// `pid` is paired with `started_at_ms` on purpose: Windows reuses PIDs, and a
/// name cache keyed on the PID alone eventually fires a rule on the wrong
/// application.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppRef {
    pub exe: String,
    pub title: Option<String>,
    pub pid: u32,
    pub started_at_ms: u64,
}

/// Everything a platform can tell the core.
///
/// Adding a variant here is the only way to add an event source, and it forces
/// every platform to say explicitly whether it can produce it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    AppFocus(AppRef),
    /// An exclusive-fullscreen D3D application took or released the screen.
    Fullscreen {
        exclusive: bool,
    },
    /// The focused window fills its monitor, or stopped doing so: maximised,
    /// borderless fullscreen or exclusive fullscreen alike. What a person
    /// means by "an app is fullscreen", where [`Event::Fullscreen`] is the
    /// narrower thing the colour backend needs to know.
    Maximized {
        maximized: bool,
    },
    Idle {
        idle: bool,
        /// Last crossed inactivity threshold. Zero on resumed activity.
        #[serde(default)]
        seconds: u32,
    },
    SessionLock {
        locked: bool,
    },
    DisplayConfigChanged,
    /// The platform can no longer vouch for what it applied on one
    /// capability, and whoever holds it has to push it again. Sent when the
    /// colour backend changes underneath a held tint: a game taking exclusive
    /// fullscreen leaves the magnifier's effect off the screen, and only the
    /// gamma ramp still reaches it.
    Reapply {
        cap: CapabilityId,
    },
    PowerSource {
        on_battery: bool,
    },
    AudioActive {
        active: bool,
    },
    /// Minute of the local day, 0 to 1439, and the second inside it. Pushed
    /// at startup, when the engine's schedule timer fires, and after anything
    /// that moves the clock underneath a sleeping timer: resume from sleep, a
    /// timezone or DST change. Never pushed every minute; the engine asks for
    /// exactly the wakeups its schedule rules need, with subsecond frames
    /// while a rule fades and only schedule edges otherwise.
    TimeOfDay {
        minute: u16,
        #[serde(default)]
        second: u8,
        #[serde(default)]
        millisecond: u16,
    },
    /// A one-shot engine timer fired.
    TimerFired {
        id: TimerId,
    },
    /// The daemon is going down cleanly. Every claim is released so the host
    /// restores baselines before the process exits.
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TimerId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Info,
    Warn,
    Error,
}

/// What the core asks the host to do.
///
/// Order inside one batch is significant: the host executes them in sequence,
/// which is what lets a baseline journal write land before the apply it
/// protects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Push a value onto a capability. The host captures and journals the
    /// baseline on the first apply, before touching anything.
    Apply {
        cap: CapabilityId,
        value: Value,
    },
    /// Nobody holds this capability any more. The host puts back what it
    /// captured and clears the journal entry.
    RestoreBaseline {
        cap: CapabilityId,
    },
    SetTimer {
        id: TimerId,
        after: Duration,
        /// Slack the OS may use to batch this wakeup with other timers. The
        /// difference between an idle check that costs nothing and one that
        /// wakes the CPU on its own.
        tolerance: Duration,
    },
    CancelTimer {
        id: TimerId,
    },
    Run {
        program: String,
        args: Vec<String>,
    },
    Log {
        level: LogLevel,
        message: String,
    },
}
