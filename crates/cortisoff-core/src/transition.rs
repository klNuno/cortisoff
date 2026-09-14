//! A rule that fades in and out instead of switching.
//!
//! LightBulb's model, kept whole because it is the one people already have in
//! their fingers: a duration, and an offset saying where each fade sits
//! against the edges of the `between` window. At 0 % both fades happen inside
//! the window, so the screen starts warming at the start time, is fully warm
//! `duration` later, and is back to neutral exactly at the end time. At 100 %
//! both sit outside it, and 50 % centres them on the edges.
//!
//! The curve is a quarter sine going in and a quarter cosine coming out, again
//! LightBulb's: a linear fade reads as a jump at both ends.
//!
//! Everything here is arithmetic on seconds of the day. No clock is read, the
//! host pushes the time and the engine asks to be woken while a fade runs.

use std::f64::consts::FRAC_PI_2;

use cortisoff_host::{CapabilityId, Value};
use serde::{Deserialize, Serialize};

use crate::rule::Assign;

pub const DAY: u32 = 86_400;

/// Longer than this and the two fades of a short window swallow the whole
/// night. Twelve hours is already absurd, and it keeps the arithmetic inside
/// one day.
pub const MAX_SECONDS: u32 = 12 * 3600;

/// How a rule fades, as authored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    /// How long one fade takes.
    pub seconds: u32,
    /// Where the fade sits against the window's edges, 0 to 100.
    #[serde(default)]
    pub offset: u8,
    /// The value each faded capability has outside the window, which is where
    /// a fade starts from and where it ends. A capability the rule sets with
    /// no entry here is not faded: it switches the moment the fade begins.
    pub from: Vec<Assign>,
}

/// The two numbers the arithmetic needs, without the values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fade {
    pub seconds: u32,
    pub offset: u8,
}

impl Fade {
    /// A window that switches at its edges, which is what a rule without a
    /// transition is.
    pub const NONE: Fade = Fade {
        seconds: 0,
        offset: 0,
    };

    /// Duration and lead in seconds, clamped so both fades and the window fit
    /// in one day. The lead is how far the evening fade starts before the
    /// window does, and how far the morning one ends after it.
    fn clamped(self, window: u32) -> (u32, u32) {
        let seconds = self.seconds.min(MAX_SECONDS);
        let lead = (u64::from(seconds) * u64::from(self.offset.min(100)) / 100) as u32;
        let lead = lead.min((DAY - 1 - window) / 2);
        (seconds, lead)
    }
}

/// The pieces of one window: where its evening fade starts, how long the whole
/// thing lasts from there, and how long each fade is.
struct Shape {
    begin: u32,
    span: u32,
    seconds: u32,
}

fn shape(start: u16, end: u16, fade: Fade) -> Shape {
    let window = ((u32::from(end) + 1440 - u32::from(start)) % 1440) * 60;
    let (seconds, lead) = fade.clamped(window);
    Shape {
        begin: (u32::from(start) * 60 + DAY - lead) % DAY,
        span: window + 2 * lead,
        seconds,
    }
}

/// How strongly a `between` window holds at `t`, from 0 to 1.
///
/// With no fade this is exactly the window, end exclusive: 1 inside, 0
/// outside. With one, it rises along a quarter sine at the start and falls
/// along a quarter cosine at the end. When the window is shorter than the two
/// fades together, the lower of the two curves wins, so it never reaches full
/// strength rather than jumping.
pub fn window_strength(start: u16, end: u16, t: u32, fade: Fade) -> f64 {
    let s = shape(start, end, fade);
    let into = (t % DAY + DAY - s.begin) % DAY;
    if into >= s.span {
        return 0.0;
    }
    let rising = if into >= s.seconds {
        1.0
    } else {
        (f64::from(into) / f64::from(s.seconds) * FRAC_PI_2).sin()
    };
    let falling_from = i64::from(s.span) - i64::from(s.seconds);
    let past = i64::from(into) - falling_from;
    let falling = if past <= 0 {
        1.0
    } else {
        (past as f64 / f64::from(s.seconds) * FRAC_PI_2).cos()
    };
    rising.min(falling)
}

/// Every second of the day at which this window can change truth value or
/// start or stop fading.
pub fn edges(start: u16, end: u16, fade: Fade, out: &mut Vec<u32>) {
    let s = shape(start, end, fade);
    let at = |offset: i64| (i64::from(s.begin) + offset).rem_euclid(i64::from(DAY)) as u32;
    let span = i64::from(s.span);
    let seconds = i64::from(s.seconds);
    out.extend([at(0), at(seconds), at(span - seconds), at(span)]);
}

/// The two fades of a window, as `(start, length)` pairs. Empty without a
/// fade, because a switch needs no wakeup between its edges.
pub fn ramps(start: u16, end: u16, fade: Fade, out: &mut Vec<(u32, u32)>) {
    let s = shape(start, end, fade);
    if s.seconds == 0 {
        return;
    }
    let falling = (i64::from(s.begin) + i64::from(s.span) - i64::from(s.seconds))
        .rem_euclid(i64::from(DAY)) as u32;
    out.push((s.begin, s.seconds));
    out.push((falling, s.seconds));
}

/// How often a running fade is stepped. Two hundred and forty steps over the
/// fade, so a forty minute one moves every ten seconds, which no eye follows,
/// and never more often than every five seconds or less often than once a
/// minute.
pub fn step(seconds: u32) -> u32 {
    (seconds / 240).clamp(5, 60)
}

/// The value at strength `s`, between where the fade starts and what the rule
/// sets. Only numbers blend; anything else is already the target.
pub fn blend(from: &Value, to: &Value, s: f64) -> Value {
    match (from, to) {
        (Value::Int(a), Value::Int(b)) => {
            Value::Int((*a as f64 + (*b - *a) as f64 * s).round() as i64)
        }
        (Value::Float(a), Value::Float(b)) => Value::Float(a + (b - a) * s),
        _ => to.clone(),
    }
}

/// Whether a value of this shape can fade towards `to`.
pub fn fadeable(from: &Value, to: &Value) -> bool {
    matches!(
        (from, to),
        (Value::Int(_), Value::Int(_)) | (Value::Float(_), Value::Float(_))
    )
}

/// Give every fade a starting point the author left out.
///
/// A rule that fades and sets a capability without naming where it starts
/// from means "from wherever the screen is": `rest` answers that per
/// capability, from the actuator that owns it. The answer is only used when it
/// can blend towards what the rule sets, so a text `rest` under an integer
/// rule still switches. The rules as authored are not touched by this: the
/// daemon runs it on the copy the engine compiles, and the file keeps saying
/// "from the current value" by saying nothing.
pub fn start_from_rest(
    rules: &mut [crate::rule::Rule],
    rest: impl Fn(&CapabilityId) -> Option<Value>,
) {
    for rule in rules {
        let Some(transition) = rule.transition.as_mut() else {
            continue;
        };
        for set in &rule.set {
            if transition.from.iter().any(|f| f.cap == set.cap) {
                continue;
            }
            if let Some(value) = rest(&set.cap).filter(|value| fadeable(value, &set.value)) {
                transition.from.push(Assign {
                    cap: set.cap.clone(),
                    value,
                });
            }
        }
    }
}

/// `40m`, `1h`, `1h30m`, `90s`. Units in that order, each at most once.
pub fn parse_duration(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    let mut digits = String::new();
    let mut last_unit = 0;
    for c in text.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let (rank, scale) = match c {
            'h' => (1, 3600),
            'm' => (2, 60),
            's' => (3, 1),
            _ => return None,
        };
        if digits.is_empty() || rank <= last_unit {
            return None;
        }
        total += digits.parse::<u64>().ok()? * scale;
        digits.clear();
        last_unit = rank;
    }
    if !digits.is_empty() {
        return None;
    }
    u32::try_from(total).ok()
}

/// The inverse of [`parse_duration`], in the shortest spelling.
pub fn format_duration(seconds: u32) -> String {
    let (h, m, s) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if s > 0 || out.is_empty() {
        out.push_str(&format!("{s}s"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NIGHT: (u16, u16) = (20 * 60, 7 * 60);
    const LIGHTBULB: Fade = Fade {
        seconds: 40 * 60,
        offset: 0,
    };

    fn at(h: u32, m: u32) -> u32 {
        h * 3600 + m * 60
    }

    fn strength(t: u32, fade: Fade) -> f64 {
        window_strength(NIGHT.0, NIGHT.1, t, fade)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn without_a_fade_the_window_is_a_switch() {
        assert_eq!(strength(at(19, 59), Fade::NONE), 0.0);
        assert_eq!(strength(at(20, 0), Fade::NONE), 1.0);
        assert_eq!(strength(at(3, 0), Fade::NONE), 1.0);
        assert_eq!(strength(at(6, 59), Fade::NONE), 1.0);
        assert_eq!(strength(at(7, 0), Fade::NONE), 0.0);
    }

    /// Offset 0: both fades inside the window. Warm from 20:00 to 20:40, back
    /// from 06:20 to 07:00, and nothing outside.
    #[test]
    fn at_zero_offset_both_fades_sit_inside_the_window() {
        assert_eq!(strength(at(20, 0), LIGHTBULB), 0.0);
        assert!(close(
            strength(at(20, 20), LIGHTBULB),
            (FRAC_PI_2 * 0.5).sin()
        ));
        assert_eq!(strength(at(20, 40), LIGHTBULB), 1.0);
        assert_eq!(strength(at(6, 20), LIGHTBULB), 1.0);
        assert!(close(
            strength(at(6, 40), LIGHTBULB),
            (FRAC_PI_2 * 0.5).cos()
        ));
        assert_eq!(strength(at(7, 0), LIGHTBULB), 0.0);
        assert_eq!(strength(at(19, 59), LIGHTBULB), 0.0);
    }

    /// Offset 100: both fades outside. Fully warm at 20:00, fully neutral at
    /// 07:40.
    #[test]
    fn at_full_offset_both_fades_sit_outside_the_window() {
        let late = Fade {
            seconds: 40 * 60,
            offset: 100,
        };
        assert_eq!(strength(at(19, 20), late), 0.0);
        assert!(strength(at(19, 40), late) > 0.0);
        assert_eq!(strength(at(20, 0), late), 1.0);
        assert_eq!(strength(at(7, 0), late), 1.0);
        assert!(strength(at(7, 20), late) > 0.0);
        assert_eq!(strength(at(7, 40), late), 0.0);
    }

    /// A window shorter than its two fades never reaches full strength, and
    /// never jumps either.
    #[test]
    fn a_window_shorter_than_its_fades_peaks_below_full() {
        let fade = Fade {
            seconds: 60 * 60,
            offset: 0,
        };
        let peak = (0..=3600)
            .map(|t| window_strength(12 * 60, 13 * 60, at(12, 0) + t, fade))
            .fold(0.0, f64::max);
        assert!(peak < 1.0 && peak > 0.5, "peaked at {peak}");
    }

    #[test]
    fn edges_are_the_four_ends_of_the_two_fades() {
        let mut out = Vec::new();
        edges(NIGHT.0, NIGHT.1, LIGHTBULB, &mut out);
        assert_eq!(out, vec![at(20, 0), at(20, 40), at(6, 20), at(7, 0)]);
    }

    #[test]
    fn a_fade_steps_every_ten_seconds_over_forty_minutes() {
        assert_eq!(step(40 * 60), 10);
        assert_eq!(step(30), 5);
        assert_eq!(step(12 * 3600), 60);
    }

    #[test]
    fn a_blend_rounds_to_the_nearest_integer() {
        assert_eq!(
            blend(&Value::Int(6600), &Value::Int(3200), 0.5),
            Value::Int(4900)
        );
        assert_eq!(
            blend(&Value::Int(6600), &Value::Int(3200), 1.0),
            Value::Int(3200)
        );
        assert_eq!(
            blend(&Value::Float(0.0), &Value::Float(1.0), 0.25),
            Value::Float(0.25)
        );
    }

    #[test]
    fn a_fade_with_no_start_begins_where_the_screen_rests() {
        let temp = CapabilityId::new("display.color_temp");
        let vibrance = CapabilityId::new("display.vibrance");
        let mode = CapabilityId::new("display.mode");
        let assign = |cap: &CapabilityId, value: Value| Assign {
            cap: cap.clone(),
            value,
        };
        let mut rules = vec![
            crate::rule::Rule {
                name: "night".into(),
                when: crate::rule::Condition::Always,
                priority: 0,
                enabled: true,
                set: vec![
                    assign(&temp, Value::Int(3200)),
                    assign(&vibrance, Value::Int(80)),
                    assign(&mode, Value::Text("@60".into())),
                ],
                transition: Some(Transition {
                    seconds: 60,
                    offset: 0,
                    from: vec![assign(&vibrance, Value::Int(0))],
                }),
            },
            crate::rule::Rule {
                name: "switch".into(),
                when: crate::rule::Condition::Always,
                priority: 0,
                enabled: true,
                set: vec![assign(&temp, Value::Int(3200))],
                transition: None,
            },
        ];
        start_from_rest(&mut rules, |cap| {
            if *cap == temp {
                Some(Value::Int(6600))
            } else if *cap == vibrance {
                Some(Value::Int(50))
            } else {
                Some(Value::Text("current".into()))
            }
        });
        // The temperature was filled in, the authored vibrance start stays,
        // and a text rest under a text set does not fade.
        assert_eq!(
            rules[0].transition.as_ref().unwrap().from,
            vec![
                assign(&vibrance, Value::Int(0)),
                assign(&temp, Value::Int(6600))
            ]
        );
        assert!(rules[1].transition.is_none());
    }

    #[test]
    fn durations_parse_and_print_back() {
        for (text, seconds) in [("40m", 2400), ("1h", 3600), ("1h30m", 5400), ("90s", 90)] {
            assert_eq!(parse_duration(text), Some(seconds), "{text}");
        }
        assert_eq!(format_duration(2400), "40m");
        assert_eq!(format_duration(5400), "1h30m");
        assert_eq!(format_duration(90), "1m30s");
        for bad in ["", "40", "m", "30m1h", "1h1h", "forty minutes"] {
            assert_eq!(parse_duration(bad), None, "{bad:?} parsed");
        }
    }
}
