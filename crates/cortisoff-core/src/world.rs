//! The state rules see, and the signal mask that keeps evaluation cheap.
//!
//! Rules are not written against events, they are written against a world. A
//! rule is a predicate: while it holds, the rule owns its claims; the moment it
//! stops holding, the claims are released. That is what makes "quit the game and
//! everything goes back" fall out of the design instead of being a feature.
//!
//! The alternative, running a handler per event, forces every rule author to
//! write the undo path by hand and to get it right on every exit path including
//! a crash. Nobody does.

use cortisoff_host::{AppRef, Event};

/// Which parts of the world a condition reads, as a bitmask.
///
/// `SetWinEventHook` fires on far more than an application switch, so an event
/// that changes nothing must cost nothing, and an event that changes the focused
/// application must not re-evaluate the rules that only watch the battery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Signals(u16);

impl Signals {
    pub const NONE: Signals = Signals(0);
    pub const APP: Signals = Signals(1 << 0);
    pub const FULLSCREEN: Signals = Signals(1 << 1);
    pub const IDLE: Signals = Signals(1 << 2);
    pub const LOCKED: Signals = Signals(1 << 3);
    pub const POWER: Signals = Signals(1 << 4);
    pub const AUDIO: Signals = Signals(1 << 5);
    pub const TIME: Signals = Signals(1 << 6);
    pub const MAXIMIZED: Signals = Signals(1 << 7);

    pub fn intersects(self, other: Signals) -> bool {
        self.0 & other.0 != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Signals {
    type Output = Signals;

    fn bitor(self, rhs: Signals) -> Signals {
        Signals(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Signals {
    fn bitor_assign(&mut self, rhs: Signals) {
        self.0 |= rhs.0;
    }
}

/// Everything the platform has told the core so far, folded into one value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct World {
    pub app: Option<AppRef>,
    /// An exclusive-fullscreen application owns the screen, which rules out the
    /// magnification backend.
    pub fullscreen: bool,
    /// The focused window fills its monitor: maximised, borderless or
    /// exclusive fullscreen. The condition a rule means by "an app is
    /// fullscreen", while `fullscreen` above is the exclusive case only.
    pub maximized: bool,
    pub idle: bool,
    pub idle_seconds: u32,
    pub locked: bool,
    pub on_battery: bool,
    pub audio_active: bool,
    /// Second of the local day. `None` until the host has said, and a
    /// schedule condition never holds on an unknown clock. Seconds rather than
    /// minutes, plus a subsecond fraction for fades.
    pub second_of_day: Option<u32>,
    /// Fraction of the current second, supplied by the host.
    pub millisecond: u16,
}

impl World {
    pub fn minute_of_day(&self) -> Option<u16> {
        self.second_of_day.map(|s| (s / 60) as u16)
    }

    /// Fold one event in.
    ///
    /// Returns the signal that actually moved, or `None` when the event repeats
    /// what is already known. That `None` is the cheap path and it is the common
    /// one: Windows re-announces the foreground window on events that are not a
    /// switch at all.
    pub fn absorb(&mut self, event: &Event) -> Option<Signals> {
        match event {
            Event::AppFocus(app) => {
                let next = Some(app.clone());
                self.changed(next != self.app, Signals::APP, |w| w.app = next)
            }
            Event::Fullscreen { exclusive } => {
                let v = *exclusive;
                self.changed(v != self.fullscreen, Signals::FULLSCREEN, |w| {
                    w.fullscreen = v
                })
            }
            Event::Maximized { maximized } => {
                let v = *maximized;
                self.changed(v != self.maximized, Signals::MAXIMIZED, |w| w.maximized = v)
            }
            Event::Idle { idle, seconds } => {
                let v = *idle;
                self.changed(
                    v != self.idle || *seconds != self.idle_seconds,
                    Signals::IDLE,
                    |w| {
                        w.idle = v;
                        w.idle_seconds = *seconds;
                    },
                )
            }
            Event::SessionLock { locked } => {
                let v = *locked;
                self.changed(v != self.locked, Signals::LOCKED, |w| w.locked = v)
            }
            Event::PowerSource { on_battery } => {
                let v = *on_battery;
                self.changed(v != self.on_battery, Signals::POWER, |w| w.on_battery = v)
            }
            Event::AudioActive { active } => {
                let v = *active;
                self.changed(v != self.audio_active, Signals::AUDIO, |w| {
                    w.audio_active = v
                })
            }
            Event::TimeOfDay {
                minute,
                second,
                millisecond,
            } => {
                let v = Some(u32::from(*minute) * 60 + u32::from(*second));
                self.changed(
                    v != self.second_of_day || *millisecond != self.millisecond,
                    Signals::TIME,
                    |w| {
                        w.millisecond = *millisecond;
                        w.second_of_day = v
                    },
                )
            }
            // These are not world fields. The engine handles them directly.
            Event::DisplayConfigChanged
            | Event::Reapply { .. }
            | Event::TimerFired { .. }
            | Event::Hotkey { .. }
            | Event::Shutdown => None,
        }
    }

    fn changed(
        &mut self,
        differs: bool,
        signal: Signals,
        set: impl FnOnce(&mut World),
    ) -> Option<Signals> {
        if !differs {
            return None;
        }
        set(self);
        Some(signal)
    }
}
