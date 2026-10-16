//! What the platform has said so far, folded into one value: the focused
//! application, idle, lock, power, sound and the clock. Plugins read it
//! through `cx:world()`, the window shows it, and the signal mask says which
//! part an event moved so only the plugins watching that part are woken.

use cortisoff_host::{AppRef, Event};

/// Which parts of the world a condition reads, as a bitmask.
///
/// `SetWinEventHook` fires on far more than an application switch, so an event
/// that changes nothing must cost nothing, and an event that changes the focused
/// application must not wake the plugins that only watch the battery.
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
    /// An application is fullscreen: exclusive, or a borderless window over
    /// its whole monitor such as a game or a film. A maximised window is not.
    pub fullscreen: bool,
    /// Every output showing a fullscreen application, focused or not, in the
    /// colour actuator's names. Empty where the platform cannot tell.
    pub filled: Vec<String>,
    /// The focused window fills its monitor: maximised, borderless or
    /// exclusive fullscreen. Where the wallpaper cannot be seen.
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
            Event::Fullscreen { exclusive, outputs } => {
                let v = *exclusive;
                let moved = v != self.fullscreen || *outputs != self.filled;
                self.changed(moved, Signals::FULLSCREEN, |w| {
                    w.fullscreen = v;
                    w.filled = outputs.clone();
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
