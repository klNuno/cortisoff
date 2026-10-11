use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A capability's stable public name, such as `display.vibrance`.
///
/// Strings live at the edges: plugins, IPC, the doctor report. Inside the
/// engine they are interned to a [`CapIndex`] so the hot path indexes a vector
/// instead of hashing a string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CapabilityId(pub String);

impl CapabilityId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Index into the engine's capability table. Valid only for the set it came
/// from, which is built once at load and never mutated afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapIndex(pub u16);

/// Why a capability the user asked for is not there.
///
/// This string reaches the user through `cortisoff doctor`, so it names the
/// missing piece rather than the failing call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unavailable {
    pub id: CapabilityId,
    pub reason: String,
}

/// What one machine can actually do, probed at startup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilitySet {
    available: BTreeMap<CapabilityId, Option<String>>,
    unavailable: Vec<Unavailable>,
}

impl CapabilitySet {
    pub fn new() -> Self {
        Self::default()
    }

    /// `backend` names the implementation actually chosen, such as `nvapi` or
    /// `ddcci`. It is reported verbatim by the doctor, because "vibrance works"
    /// and "vibrance works through NVAPI" are different bug reports.
    pub fn available(mut self, id: CapabilityId, backend: Option<&str>) -> Self {
        self.available.insert(id, backend.map(str::to_owned));
        self
    }

    pub fn missing(mut self, id: CapabilityId, reason: impl Into<String>) -> Self {
        self.unavailable.push(Unavailable {
            id,
            reason: reason.into(),
        });
        self
    }

    pub fn has(&self, id: &CapabilityId) -> bool {
        self.available.contains_key(id)
    }

    pub fn backend(&self, id: &CapabilityId) -> Option<&str> {
        self.available.get(id).and_then(|b| b.as_deref())
    }

    pub fn ids(&self) -> impl Iterator<Item = &CapabilityId> {
        self.available.keys()
    }

    pub fn gaps(&self) -> &[Unavailable] {
        &self.unavailable
    }

    pub fn reason_missing(&self, id: &CapabilityId) -> Option<&str> {
        self.unavailable
            .iter()
            .find(|u| &u.id == id)
            .map(|u| u.reason.as_str())
    }
}

/// The shape of a settable capability's value, for a client that draws a
/// control. Reported by the actuator, so the dashboard never carries a table
/// of ranges that drifts away from the hardware: a new capability gets its
/// slider, switch or text box from this alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Control {
    /// An integer on a slider. `unit` is the suffix a client prints, such as
    /// `K` or `%`; a client may pick a themed track from it.
    Range {
        min: i64,
        max: i64,
        step: i64,
        default: i64,
        unit: Option<String>,
    },
    /// On or off.
    Switch,
    /// Free text with a documented shape. `hint` is an example a client can
    /// show empty, such as `2560x1440@240`.
    Text { hint: String },
}

/// A short name for a capability, for a client that shows it to a person.
/// Falls back to the id with its dots read as spaces, so an id this table
/// does not know is still shown and still usable.
pub fn label(id: &str) -> String {
    let known = match id {
        ids::APP_FOCUS => "Focused app",
        ids::APP_FULLSCREEN => "Exclusive fullscreen",
        ids::APP_MAXIMIZED => "Maximised window",
        ids::USER_IDLE => "Nobody there",
        ids::SESSION_LOCK => "Session lock",
        ids::DISPLAY_CONFIG => "Monitor changes",
        ids::POWER_SOURCE => "Power source",
        ids::AUDIO_ACTIVE => "Sound playing",
        ids::DISPLAY_VIBRANCE => "Vibrance",
        ids::DISPLAY_COLOR_TEMP => "Colour temperature",
        ids::DISPLAY_MODE => "Display mode",
        ids::WALLPAPER_PLAYBACK => "Wallpaper playback",
        _ => return id.replace(['.', '_'], " "),
    };
    known.to_owned()
}

/// The ids the project ships with. A platform may report any subset, and a
/// community rule may name one that no platform provides yet.
pub mod ids {
    pub const APP_FOCUS: &str = "app.focus";
    pub const APP_FULLSCREEN: &str = "app.fullscreen";
    pub const APP_MAXIMIZED: &str = "app.maximized";
    pub const USER_IDLE: &str = "user.idle";
    pub const SESSION_LOCK: &str = "session.lock";
    pub const DISPLAY_CONFIG: &str = "display.config";
    pub const POWER_SOURCE: &str = "power.source";
    pub const AUDIO_ACTIVE: &str = "audio.active";

    pub const DISPLAY_VIBRANCE: &str = "display.vibrance";
    pub const DISPLAY_COLOR_TEMP: &str = "display.color_temp";
    pub const DISPLAY_MODE: &str = "display.mode";
    pub const WALLPAPER_PLAYBACK: &str = "wallpaper.playback";
}
