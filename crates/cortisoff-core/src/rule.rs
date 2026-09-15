//! What a rule is, and what it compiles into.
//!
//! A rule names capabilities by string. Whether this machine has them is not
//! known until it is probed, so compilation is where a rule either binds to a
//! capability index or becomes [`RuleState::Unsupported`]. It never fails to
//! load and it never panics: a rule asking for NVIDIA vibrance on an AMD laptop
//! is a line in `cortisoff doctor`, not an error.

use cortisoff_host::{CapIndex, CapabilityId, Value};
use serde::{Deserialize, Serialize};

use crate::table::CapTable;
use crate::transition::{self, Fade, Transition};
use crate::world::{Signals, World};

/// A predicate over the [`World`].
///
/// Adjacently tagged rather than internally tagged, which matters twice: an
/// internal tag cannot serialize `Fullscreen(true)` at all, because the
/// variant's content is a bool and there is nowhere to put the tag, and the
/// recursion through `All` makes the compiler chase the wrapper type serde
/// generates for an internal tag until it gives up. `{"when": "fullscreen",
/// "value": true}` is also the shape a client reads most easily.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "when", content = "value", rename_all = "snake_case")]
pub enum Condition {
    /// Holds unconditionally. A rule setting a default that everything else
    /// outranks.
    Always,
    /// Matches the focused application. Both fields are optional and both must
    /// hold when present.
    App {
        /// Matched against the executable's file name, case-insensitively,
        /// because the path a platform reports is not portable and users write
        /// `chrome.exe`, not a path.
        exe: Option<String>,
        title_contains: Option<String>,
    },
    /// An exclusive-fullscreen application owns the screen. The narrow case;
    /// a rule that should stop for "an app in fullscreen" wants `Maximized`.
    Fullscreen(bool),
    /// The focused window fills its monitor: maximised, borderless fullscreen
    /// or exclusive fullscreen. Named for the common case a person writes it
    /// for, a browser or a game taking the whole screen.
    Maximized(bool),
    Idle(bool),
    Locked(bool),
    OnBattery(bool),
    AudioActive(bool),
    /// Holds between two times of day, end exclusive. A range whose start is
    /// after its end crosses midnight: `20:00` to `07:00` holds at 23:00 and
    /// at 03:00. Start equal to end is an empty range, never a full day.
    Between {
        start: ClockTime,
        end: ClockTime,
    },
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
}

impl Condition {
    pub fn eval(&self, w: &World) -> bool {
        self.strength(w, Fade::NONE) > 0.0
    }

    /// How strongly this holds, from 0 to 1.
    ///
    /// Every condition but a time window is 0 or 1. A window with a fade
    /// rises and falls along its edges, and the rest follows: `all` is the
    /// weakest part, `any` the strongest, `not` the complement. A rule holds
    /// while this is above zero, and a faded value sits this far between
    /// where it starts and what the rule sets.
    pub fn strength(&self, w: &World, fade: Fade) -> f64 {
        let flag = |held: bool| if held { 1.0 } else { 0.0 };
        match self {
            Condition::Always => 1.0,
            Condition::App {
                exe,
                title_contains,
            } => {
                let Some(app) = &w.app else {
                    return 0.0;
                };
                let exe_ok = exe
                    .as_deref()
                    .is_none_or(|want| file_name(&app.exe).eq_ignore_ascii_case(want));
                let title_ok = title_contains.as_deref().is_none_or(|want| {
                    app.title
                        .as_deref()
                        .is_some_and(|t| contains_ignore_case(t, want))
                });
                flag(exe_ok && title_ok)
            }
            Condition::Fullscreen(v) => flag(w.fullscreen == *v),
            Condition::Maximized(v) => flag(w.maximized == *v),
            Condition::Idle(v) => flag(w.idle == *v),
            Condition::Locked(v) => flag(w.locked == *v),
            Condition::OnBattery(v) => flag(w.on_battery == *v),
            Condition::AudioActive(v) => flag(w.audio_active == *v),
            Condition::Between { start, end } => match w.second_of_day {
                None => 0.0,
                Some(t) => transition::window_strength_ms(
                    start.0,
                    end.0,
                    t * 1000 + u32::from(w.millisecond),
                    fade,
                ),
            },
            Condition::All(cs) => cs.iter().map(|c| c.strength(w, fade)).fold(1.0, f64::min),
            Condition::Any(cs) => cs.iter().map(|c| c.strength(w, fade)).fold(0.0, f64::max),
            Condition::Not(c) => 1.0 - c.strength(w, fade),
        }
    }

    /// Which parts of the world this reads, so the engine can skip it when
    /// something else moved.
    pub fn signals(&self) -> Signals {
        match self {
            Condition::Always => Signals::NONE,
            Condition::App { .. } => Signals::APP,
            Condition::Fullscreen(_) => Signals::FULLSCREEN,
            Condition::Maximized(_) => Signals::MAXIMIZED,
            Condition::Idle(_) => Signals::IDLE,
            Condition::Locked(_) => Signals::LOCKED,
            Condition::OnBattery(_) => Signals::POWER,
            Condition::AudioActive(_) => Signals::AUDIO,
            Condition::Between { .. } => Signals::TIME,
            Condition::All(cs) | Condition::Any(cs) => {
                cs.iter().fold(Signals::NONE, |acc, c| acc | c.signals())
            }
            Condition::Not(c) => c.signals(),
        }
    }

    /// Every second of the day at which this condition can change truth value
    /// or start or stop fading. The engine wakes at exactly these and at no
    /// other, except to step a fade that is running.
    pub fn clock_edges(&self, fade: Fade, out: &mut Vec<u32>) {
        match self {
            Condition::Between { start, end } => transition::edges(start.0, end.0, fade, out),
            Condition::All(cs) | Condition::Any(cs) => {
                for c in cs {
                    c.clock_edges(fade, out);
                }
            }
            Condition::Not(c) => c.clock_edges(fade, out),
            _ => {}
        }
    }

    /// The fades inside this condition, as `(start, length)` in seconds of
    /// the day.
    pub fn ramps(&self, fade: Fade, out: &mut Vec<(u32, u32)>) {
        match self {
            Condition::Between { start, end } => transition::ramps(start.0, end.0, fade, out),
            Condition::All(cs) | Condition::Any(cs) => {
                for c in cs {
                    c.ramps(fade, out);
                }
            }
            Condition::Not(c) => c.ramps(fade, out),
            _ => {}
        }
    }
}

/// A time of day at minute resolution, authored as `"HH:MM"`. Stored as
/// minutes since midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockTime(pub u16);

impl std::str::FromStr for ClockTime {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parsed = s.split_once(':').and_then(|(h, m)| {
            let h: u16 = h.parse().ok()?;
            let m: u16 = m.parse().ok()?;
            (h <= 23 && m <= 59).then_some(ClockTime(h * 60 + m))
        });
        parsed.ok_or_else(|| format!("expected a time of day like \"22:30\", got {s:?}"))
    }
}

impl std::fmt::Display for ClockTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}", self.0 / 60, self.0 % 60)
    }
}

impl Serialize for ClockTime {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ClockTime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// One capability set to one value while the rule holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assign {
    pub cap: CapabilityId,
    pub value: Value,
}

/// A rule as authored, before it meets a machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    pub when: Condition,
    #[serde(default = "default_priority")]
    pub priority: i32,
    /// A rule the user switched off. It stays in the file with everything it
    /// was configured with, which is the difference between a switch and a
    /// delete, and it costs nothing while off: the engine never evaluates it
    /// and the host installs no source for what it reads.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    pub set: Vec<Assign>,
    /// Fade in and out across the rule's time window instead of switching at
    /// its edges. Omitted on the wire when absent, so a client that has never
    /// heard of it sends a rule back unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<Transition>,
}

fn default_priority() -> i32 {
    crate::priority::RULE
}

fn enabled_by_default() -> bool {
    true
}

/// Why a rule is not doing anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RuleState {
    Active,
    /// Switched off by the user. Checked before anything else, so a rule can be
    /// switched off without its capabilities existing on this machine.
    Disabled,
    /// At least one capability the rule names is absent on this machine. The
    /// rule stays loaded and stays listed, it simply never claims.
    Unsupported {
        missing: Vec<CapabilityId>,
    },
    /// The rule sets nothing at all. Loud on purpose: silently loading a rule
    /// that can never do anything is how a user spends an evening debugging a
    /// typo in a capability name.
    Empty,
}

/// A rule bound to one machine's capability table.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub id: u32,
    pub name: String,
    pub when: Condition,
    pub priority: i32,
    /// Precomputed from `when`, so the engine never walks the condition tree of
    /// a rule the current event cannot affect.
    pub reads: Signals,
    pub set: Vec<(CapIndex, Value)>,
    /// Where each entry of `set` fades from, in the same order. `None` for a
    /// value that switches.
    pub from: Vec<Option<Value>>,
    pub fade: Fade,
    pub state: RuleState,
}

impl Compiled {
    /// Bind a rule to a table. `id` is the owner identity in the claim stack, so
    /// it must be stable for the lifetime of the load.
    pub fn bind(id: u32, rule: Rule, table: &CapTable) -> Compiled {
        let starts = rule
            .transition
            .as_ref()
            .map(|t| t.from.clone())
            .unwrap_or_default();
        let fade = rule.transition.as_ref().map_or(Fade::NONE, |t| Fade {
            seconds: t.seconds,
            offset: t.offset,
        });
        let mut set = Vec::with_capacity(rule.set.len());
        let mut from = Vec::with_capacity(rule.set.len());
        let mut missing = Vec::new();
        for a in rule.set {
            match table.index_of(&a.cap) {
                Some(i) => {
                    from.push(
                        starts
                            .iter()
                            .find(|s| s.cap == a.cap && transition::fadeable(&s.value, &a.value))
                            .map(|s| s.value.clone()),
                    );
                    set.push((i, a.value));
                }
                None => missing.push(a.cap),
            }
        }
        let state = if !rule.enabled {
            RuleState::Disabled
        } else if !missing.is_empty() {
            RuleState::Unsupported { missing }
        } else if set.is_empty() {
            RuleState::Empty
        } else {
            RuleState::Active
        };
        Compiled {
            id,
            reads: rule.when.signals(),
            name: rule.name,
            when: rule.when,
            priority: rule.priority,
            set,
            from,
            fade,
            state,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, RuleState::Active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_time_parses_from_hh_mm_and_prints_back() {
        let t: ClockTime = "20:05".parse().unwrap();
        assert_eq!(t, ClockTime(20 * 60 + 5));
        assert_eq!(t.to_string(), "20:05");
    }

    #[test]
    fn a_malformed_clock_time_is_an_error_not_a_guess() {
        for bad in ["24:00", "12:60", "noon", "12", "1200", ""] {
            assert!(bad.parse::<ClockTime>().is_err(), "{bad:?} parsed");
        }
    }

    /// End exclusive on both shapes, and the wraparound range crosses
    /// midnight.
    #[test]
    fn between_holds_inside_and_wraps_across_midnight() {
        let wrap = Condition::Between {
            start: "20:00".parse().unwrap(),
            end: "07:00".parse().unwrap(),
        };
        let mut w = World::default();
        let holds = |w: &World| wrap.eval(w);

        w.second_of_day = Some(23 * 3600);
        assert!(holds(&w));
        w.second_of_day = Some(3 * 3600);
        assert!(holds(&w));
        w.second_of_day = Some(12 * 3600);
        assert!(!holds(&w));
        // End exclusive: 07:00 itself is outside, and a second before is in.
        w.second_of_day = Some(7 * 3600);
        assert!(!holds(&w));
        w.second_of_day = Some(7 * 3600 - 1);
        assert!(holds(&w));
    }
}
