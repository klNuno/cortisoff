//! The TOML front end: what a user actually types.
//!
//! The compiled [`Condition`] tree is a tagged enum, and serializing that
//! directly would make a rule file look like a syntax tree. This module is the
//! translation, and it is the whole plugin story: a community module is one of
//! these files.
//!
//! ```toml
//! [[rule]]
//! name = "vibrance in competitive games"
//! priority = 100
//! when.app = ["valorant.exe", "cs2.exe"]
//! set.display.vibrance = 100
//! set.display.mode = "@240"
//!
//! [[rule]]
//! name = "blue light at night"
//! when.between = "20:00-07:00"
//! set.display.color_temp = 3400
//! ```
//!
//! Every key inside `when` must hold at once. That covers nearly every real
//! rule, and `any` plus `not` are there for the rest. Parsing takes a string
//! and never touches a disk, so the daemon owns file reading and this stays
//! testable anywhere.

use cortisoff_host::{CapabilityId, Value};

use crate::rule::{Assign, ClockTime, Condition, Rule};
use crate::transition::{self, Fade, Transition};

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigError {
    /// Where in the document, as the user would point at it: `rule "night",
    /// when.between`.
    pub at: String,
    pub message: String,
}

impl ConfigError {
    fn new(at: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            at: at.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.at.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.at, self.message)
        }
    }
}

impl std::error::Error for ConfigError {}

/// Parse one rule file.
///
/// A malformed file is rejected whole rather than partly loaded: half a rule
/// pack behaves in ways nobody can reason about, and the daemon reports the
/// error through `cortisoff doctor` instead.
pub fn parse_rules(source: &str) -> Result<Vec<Rule>, ConfigError> {
    let doc: toml::Table = source
        .parse()
        .map_err(|e: toml::de::Error| ConfigError::new("", e.message().to_owned()))?;

    let mut rules = Vec::new();
    for (key, value) in &doc {
        if key != "rule" {
            return Err(ConfigError::new(
                "",
                format!("unknown top level key `{key}`, expected `[[rule]]`"),
            ));
        }
        let items = value
            .as_array()
            .ok_or_else(|| ConfigError::new("rule", "expected `[[rule]]`, an array of tables"))?;
        for (i, item) in items.iter().enumerate() {
            rules.push(parse_rule(item, i)?);
        }
    }
    Ok(rules)
}

fn parse_rule(value: &toml::Value, index: usize) -> Result<Rule, ConfigError> {
    let table = value
        .as_table()
        .ok_or_else(|| ConfigError::new(format!("rule #{}", index + 1), "expected a table"))?;

    // Named first, so every later error can point at the rule by name.
    let name = match table.get("name") {
        Some(toml::Value::String(s)) if !s.trim().is_empty() => s.clone(),
        Some(_) => {
            return Err(ConfigError::new(
                format!("rule #{}", index + 1),
                "`name` must be a non-empty string",
            ))
        }
        None => {
            return Err(ConfigError::new(
                format!("rule #{}", index + 1),
                "missing `name`. It is what `cortisoff trace` and `doctor` print",
            ))
        }
    };
    let at = |suffix: &str| format!("rule {name:?}, {suffix}");

    let mut priority = crate::priority::RULE;
    let mut when = Condition::Always;
    let mut set = Vec::new();
    let mut enabled = true;
    let mut fade = None;

    for (key, value) in table {
        match key.as_str() {
            "name" => {}
            "enabled" => {
                enabled = value
                    .as_bool()
                    .ok_or_else(|| ConfigError::new(at("enabled"), "expected true or false"))?;
            }
            "priority" => {
                priority = value
                    .as_integer()
                    .and_then(|v| i32::try_from(v).ok())
                    .ok_or_else(|| {
                        ConfigError::new(at("priority"), "expected an integer that fits in 32 bits")
                    })?;
            }
            "when" => when = parse_when(value, &at("when"))?,
            "set" => set = parse_set(value, &at("set"))?,
            "transition" => fade = Some(parse_transition(value, &at("transition"))?),
            other => {
                return Err(ConfigError::new(
                    at(&format!("`{other}`")),
                    "unknown key, expected one of `name`, `enabled`, `priority`, `when`, `set`, \
                     `transition`",
                ))
            }
        }
    }

    // Checked once everything is read, because TOML does not promise that
    // `transition` comes after the `when` and `set` it refers to.
    if let Some(t) = &fade {
        let mut edges = Vec::new();
        when.clock_edges(Fade::NONE, &mut edges);
        if edges.is_empty() {
            return Err(ConfigError::new(
                at("transition"),
                "a transition fades across a `between` window, and this rule has none",
            ));
        }
        for start in &t.from {
            let here = at(&format!("transition.from.{}", start.cap));
            let Some(target) = set.iter().find(|a| a.cap == start.cap) else {
                return Err(ConfigError::new(
                    here,
                    "fades a capability this rule does not set",
                ));
            };
            if !transition::fadeable(&start.value, &target.value) {
                return Err(ConfigError::new(
                    here,
                    "only a number fades, and it has to be the same kind of number the rule sets",
                ));
            }
        }
    }

    Ok(Rule {
        name,
        when,
        priority,
        enabled,
        set,
        transition: fade,
    })
}

/// `duration`, an optional `offset` and the values each fade starts from.
fn parse_transition(value: &toml::Value, at: &str) -> Result<Transition, ConfigError> {
    let table = value
        .as_table()
        .ok_or_else(|| ConfigError::new(at, "expected a table with `duration`"))?;

    let mut seconds = None;
    let mut offset = 0;
    let mut from = Vec::new();
    for (key, value) in table {
        let here = format!("{at}.{key}");
        match key.as_str() {
            "duration" => {
                let text = as_str(value, &here)?;
                let parsed = transition::parse_duration(text).ok_or_else(|| {
                    ConfigError::new(
                        &here,
                        format!("expected a duration like \"40m\" or \"1h30m\", got {text:?}"),
                    )
                })?;
                if parsed == 0 || parsed > transition::MAX_SECONDS {
                    return Err(ConfigError::new(
                        &here,
                        "a fade lasts between one second and twelve hours",
                    ));
                }
                seconds = Some(parsed);
            }
            "offset" => {
                offset = value
                    .as_integer()
                    .filter(|v| (0..=100).contains(v))
                    .map(|v| v as u8)
                    .ok_or_else(|| {
                        ConfigError::new(&here, "expected a percentage from 0 to 100")
                    })?;
            }
            "from" => from = parse_set(value, &here)?,
            other => {
                return Err(ConfigError::new(
                    &here,
                    format!("unknown key `{other}`, expected `duration`, `offset` or `from`"),
                ))
            }
        }
    }

    let seconds =
        seconds.ok_or_else(|| ConfigError::new(at, "missing `duration`, such as \"40m\""))?;
    Ok(Transition {
        seconds,
        offset,
        from,
    })
}

/// Every key in the table must hold at once.
fn parse_when(value: &toml::Value, at: &str) -> Result<Condition, ConfigError> {
    let table = value
        .as_table()
        .ok_or_else(|| ConfigError::new(at, "expected a table of conditions"))?;

    let mut parts = Vec::new();
    for (key, value) in table {
        let at = format!("{at}.{key}");
        let part = match key.as_str() {
            "app" => any_of(value, &at, |s, at| {
                Ok(Condition::App {
                    exe: Some(non_empty(s, at)?.to_owned()),
                    title_contains: None,
                })
            })?,
            "title" => Condition::App {
                exe: None,
                title_contains: Some(non_empty(as_str(value, &at)?, &at)?.to_owned()),
            },
            "fullscreen" => Condition::Fullscreen(as_bool(value, &at)?),
            "maximized" => Condition::Maximized(as_bool(value, &at)?),
            "idle" => Condition::Idle(as_bool(value, &at)?),
            "locked" => Condition::Locked(as_bool(value, &at)?),
            "on_battery" => Condition::OnBattery(as_bool(value, &at)?),
            "audio" => Condition::AudioActive(as_bool(value, &at)?),
            "between" => any_of(value, &at, parse_between)?,
            "not" => Condition::Not(Box::new(parse_when(value, &at)?)),
            "any" => Condition::Any(parse_branches(value, &at)?),
            // Rarely written by hand, because listing several keys in one
            // `when` already means all of them. It exists because two branches
            // can want the same key, `app` twice inside a `not` for instance,
            // and because it is what `write_rules` emits in that case.
            "all" => Condition::All(parse_branches(value, &at)?),
            other => {
                return Err(ConfigError::new(
                    &at,
                    format!(
                        "unknown condition `{other}`, expected one of `app`, `title`, \
                         `fullscreen`, `maximized`, `idle`, `locked`, `on_battery`, `audio`, \
                         `between`, `any`, `all`, `not`"
                    ),
                ))
            }
        };
        parts.push(part);
    }

    Ok(match parts.len() {
        // An omitted or empty `when` is a rule that always holds, which is how
        // a default is written.
        0 => Condition::Always,
        1 => parts.pop().expect("length checked"),
        _ => Condition::All(parts),
    })
}

fn parse_branches(value: &toml::Value, at: &str) -> Result<Vec<Condition>, ConfigError> {
    let items = value
        .as_array()
        .ok_or_else(|| ConfigError::new(at, "expected an array of condition tables"))?;
    let mut branches = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        branches.push(parse_when(item, &format!("{at}[{i}]"))?);
    }
    Ok(branches)
}

/// One string, or an array of them meaning any. `app = "vlc.exe"` and
/// `app = ["vlc.exe", "mpv.exe"]` are the same shape to the user.
fn any_of(
    value: &toml::Value,
    at: &str,
    one: impl Fn(&str, &str) -> Result<Condition, ConfigError>,
) -> Result<Condition, ConfigError> {
    match value {
        toml::Value::String(s) => one(s, at),
        toml::Value::Array(items) => {
            if items.is_empty() {
                return Err(ConfigError::new(
                    at,
                    "empty list. A condition that can never hold is a typo, not a rule",
                ));
            }
            let mut branches = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let at = format!("{at}[{i}]");
                branches.push(one(as_str(item, &at)?, &at)?);
            }
            Ok(Condition::Any(branches))
        }
        _ => Err(ConfigError::new(at, "expected a string or a list of them")),
    }
}

fn parse_between(s: &str, at: &str) -> Result<Condition, ConfigError> {
    let (start, end) = s
        .split_once('-')
        .ok_or_else(|| ConfigError::new(at, format!("expected \"HH:MM-HH:MM\", got {s:?}")))?;
    let parse = |half: &str| -> Result<ClockTime, ConfigError> {
        half.trim()
            .parse::<ClockTime>()
            .map_err(|e| ConfigError::new(at, e))
    };
    let (start, end) = (parse(start)?, parse(end)?);
    if start == end {
        return Err(ConfigError::new(
            at,
            "start equals end, so the window is empty. For a whole day, omit `between`",
        ));
    }
    Ok(Condition::Between { start, end })
}

/// Capability ids are dotted, and TOML nests on dots. `set.display.vibrance`
/// therefore arrives as nested tables and is flattened straight back.
fn parse_set(value: &toml::Value, at: &str) -> Result<Vec<Assign>, ConfigError> {
    let table = value
        .as_table()
        .ok_or_else(|| ConfigError::new(at, "expected a table of capability assignments"))?;
    let mut out = Vec::new();
    flatten_set(table, String::new(), at, &mut out)?;
    Ok(out)
}

fn flatten_set(
    table: &toml::Table,
    prefix: String,
    at: &str,
    out: &mut Vec<Assign>,
) -> Result<(), ConfigError> {
    for (key, value) in table {
        let id = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(inner) => flatten_set(inner, id, at, out)?,
            leaf => out.push(Assign {
                cap: CapabilityId::new(&id),
                value: parse_value(leaf, &format!("{at}.{id}"))?,
            }),
        }
    }
    Ok(())
}

fn parse_value(value: &toml::Value, at: &str) -> Result<Value, ConfigError> {
    Ok(match value {
        toml::Value::Integer(v) => Value::Int(*v),
        toml::Value::Float(v) => Value::Float(*v),
        toml::Value::Boolean(v) => Value::Bool(*v),
        // A string shaped like a display mode is one. Nothing else in the
        // value language uses `1920x1080` or a leading `@`, so the two cannot
        // collide.
        toml::Value::String(s) => match parse_mode(s) {
            Some(mode) => mode,
            None if looks_like_mode(s) => {
                return Err(ConfigError::new(
                    at,
                    format!(
                        "{s:?} looks like a display mode but does not parse. \
                         Expected \"2560x1440@144\", \"2560x1440\" or \"@144\""
                    ),
                ))
            }
            None => Value::Text(s.clone()),
        },
        _ => {
            return Err(ConfigError::new(
                at,
                "expected an integer, float, boolean or string",
            ))
        }
    })
}

fn looks_like_mode(s: &str) -> bool {
    s.starts_with('@') || s.split_once('x').is_some_and(|(w, _)| digits(w))
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `2560x1440@144`, `2560x1440`, or `@144` to change refresh rate only.
///
/// Zero width and height mean "whatever the monitor is on now", which is what
/// almost every refresh rate rule actually wants. Refresh is stored in
/// millihertz because 59.94 Hz exists.
fn parse_mode(s: &str) -> Option<Value> {
    let (size, refresh) = match s.split_once('@') {
        Some((size, refresh)) => (size, Some(refresh)),
        None => (s, None),
    };
    let (width, height) = if size.is_empty() {
        (0, 0)
    } else {
        let (w, h) = size.split_once('x')?;
        (w.parse().ok()?, h.parse().ok()?)
    };
    let refresh_mhz = match refresh {
        None => 0,
        Some(r) => {
            let hz: f64 = r
                .trim_end_matches("Hz")
                .trim_end_matches("hz")
                .parse()
                .ok()?;
            if !(hz.is_finite() && (0.0..=10_000.0).contains(&hz)) {
                return None;
            }
            (hz * 1000.0).round() as u32
        }
    };
    // `""` splits into nothing on both halves. Reject it here rather than
    // returning a mode that changes nothing.
    if width == 0 && height == 0 && refresh_mhz == 0 {
        return None;
    }
    Some(Value::Mode {
        width,
        height,
        refresh_mhz,
    })
}

fn as_str<'a>(value: &'a toml::Value, at: &str) -> Result<&'a str, ConfigError> {
    value
        .as_str()
        .ok_or_else(|| ConfigError::new(at, "expected a string"))
}

fn as_bool(value: &toml::Value, at: &str) -> Result<bool, ConfigError> {
    value
        .as_bool()
        .ok_or_else(|| ConfigError::new(at, "expected true or false"))
}

fn non_empty<'a>(s: &'a str, at: &str) -> Result<&'a str, ConfigError> {
    if s.trim().is_empty() {
        return Err(ConfigError::new(at, "expected a non-empty string"));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(source: &str) -> Rule {
        let mut rules = parse_rules(source).expect("should parse");
        assert_eq!(rules.len(), 1);
        rules.pop().unwrap()
    }

    fn err(source: &str) -> String {
        parse_rules(source).expect_err("should fail").to_string()
    }

    fn cap(s: &str) -> CapabilityId {
        CapabilityId::new(s)
    }

    #[test]
    fn a_minimal_rule_parses() {
        let r = one(r#"
            [[rule]]
            name = "night"
            when.between = "20:00-07:00"
            set.display.color_temp = 3400
        "#);
        assert_eq!(r.name, "night");
        assert_eq!(r.priority, crate::priority::RULE);
        assert_eq!(
            r.when,
            Condition::Between {
                start: ClockTime(20 * 60),
                end: ClockTime(7 * 60),
            }
        );
        assert_eq!(
            r.set,
            vec![Assign {
                cap: cap("display.color_temp"),
                value: Value::Int(3400),
            }]
        );
    }

    /// The reason `set` is nested rather than quoted: capability ids are
    /// dotted, and TOML already nests on dots.
    #[test]
    fn dotted_capability_ids_come_from_nested_tables() {
        let r = one(r#"
            [[rule]]
            name = "x"
            set.display.vibrance = 100
            set.wallpaper.playback = false
        "#);
        assert_eq!(
            r.set,
            vec![
                Assign {
                    cap: cap("display.vibrance"),
                    value: Value::Int(100),
                },
                Assign {
                    cap: cap("wallpaper.playback"),
                    value: Value::Bool(false),
                },
            ]
        );
    }

    #[test]
    fn several_conditions_in_one_table_all_have_to_hold() {
        let r = one(r#"
            [[rule]]
            name = "x"
            when = { app = "vlc.exe", fullscreen = true }
            set.display.vibrance = 50
        "#);
        assert_eq!(
            r.when,
            Condition::All(vec![
                Condition::App {
                    exe: Some("vlc.exe".into()),
                    title_contains: None
                },
                Condition::Fullscreen(true),
            ])
        );
    }

    /// `fullscreen` is the exclusive case, `maximized` the one people mean:
    /// the two are separate keys and separate conditions.
    #[test]
    fn maximised_is_its_own_condition() {
        let r = one(r#"
            [[rule]]
            name = "x"
            when.not.maximized = true
            set.display.color_temp = 3400
        "#);
        assert_eq!(r.when, Condition::Not(Box::new(Condition::Maximized(true))));
        assert_eq!(r.when.signals(), crate::world::Signals::MAXIMIZED);
    }

    #[test]
    fn a_list_of_applications_means_any_of_them() {
        let r = one(r#"
            [[rule]]
            name = "x"
            when.app = ["valorant.exe", "cs2.exe"]
            set.display.vibrance = 100
        "#);
        let Condition::Any(branches) = r.when else {
            panic!("expected Any, got {:?}", r.when);
        };
        assert_eq!(branches.len(), 2);
    }

    #[test]
    fn any_and_not_nest_arbitrarily() {
        let r = one(r#"
            [[rule]]
            name = "x"
            when.not.idle = true
            when.any = [{ app = "vlc.exe" }, { audio = true }]
            set.display.vibrance = 10
        "#);
        assert_eq!(
            r.when,
            Condition::All(vec![
                Condition::Any(vec![
                    Condition::App {
                        exe: Some("vlc.exe".into()),
                        title_contains: None
                    },
                    Condition::AudioActive(true),
                ]),
                Condition::Not(Box::new(Condition::Idle(true))),
            ])
        );
    }

    /// A rule with no `when` is a default. It holds always and is expected to
    /// be outranked.
    #[test]
    fn an_omitted_when_is_a_rule_that_always_holds() {
        let r = one(r#"
            [[rule]]
            name = "default"
            priority = -100
            set.display.vibrance = 50
        "#);
        assert_eq!(r.when, Condition::Always);
        assert_eq!(r.priority, crate::priority::BACKGROUND);
    }

    #[test]
    fn display_modes_parse_in_all_three_shapes() {
        let r = one(r#"
            [[rule]]
            name = "x"
            set.a.full = "2560x1440@144"
            set.a.size_only = "2560x1440"
            set.a.refresh_only = "@240"
            set.a.fractional = "1920x1080@59.94"
        "#);
        let v = |name: &str| {
            r.set
                .iter()
                .find(|a| a.cap.as_str() == name)
                .map(|a| a.value.clone())
                .unwrap()
        };
        assert_eq!(
            v("a.full"),
            Value::Mode {
                width: 2560,
                height: 1440,
                refresh_mhz: 144_000
            }
        );
        assert_eq!(
            v("a.size_only"),
            Value::Mode {
                width: 2560,
                height: 1440,
                refresh_mhz: 0
            }
        );
        // Zero size means "keep the resolution the monitor is on", which is
        // what a refresh rate rule wants.
        assert_eq!(
            v("a.refresh_only"),
            Value::Mode {
                width: 0,
                height: 0,
                refresh_mhz: 240_000
            }
        );
        // 59.94 Hz is real hardware. Rounding it to 60 picks the wrong mode.
        assert_eq!(
            v("a.fractional"),
            Value::Mode {
                width: 1920,
                height: 1080,
                refresh_mhz: 59_940
            }
        );
    }

    #[test]
    fn an_ordinary_string_stays_text() {
        let r = one(r#"
            [[rule]]
            name = "x"
            set.wallpaper.preset = "aquarium"
        "#);
        assert_eq!(r.set[0].value, Value::Text("aquarium".into()));
    }

    /// A typo in a mode must not silently become a text value that no actuator
    /// understands.
    #[test]
    fn a_string_shaped_like_a_broken_mode_is_an_error() {
        assert!(err(r#"
            [[rule]]
            name = "x"
            set.display.mode = "2560x"
        "#)
        .contains("looks like a display mode"));
    }

    /// The single biggest rule authoring pain, made loud.
    #[test]
    fn an_unknown_key_names_itself_and_lists_what_was_expected() {
        let e = err(r#"
            [[rule]]
            name = "x"
            when.fullscren = true
            set.display.vibrance = 1
        "#);
        assert!(e.contains("fullscren"), "{e}");
        assert!(e.contains("fullscreen"), "{e}");
        assert!(e.contains("rule \"x\""), "{e}");
    }

    #[test]
    fn a_rule_without_a_name_is_rejected() {
        assert!(err("[[rule]]\nset.display.vibrance = 1\n").contains("missing `name`"));
    }

    #[test]
    fn a_malformed_time_window_says_what_was_expected() {
        assert!(err(r#"
            [[rule]]
            name = "x"
            when.between = "20:00 to 07:00"
            set.display.color_temp = 1
        "#)
        .contains("HH:MM-HH:MM"));
        assert!(err(r#"
            [[rule]]
            name = "x"
            when.between = "20:00-20:00"
            set.display.color_temp = 1
        "#)
        .contains("empty"));
    }

    #[test]
    fn an_unknown_top_level_key_is_rejected_rather_than_ignored() {
        assert!(err("[settings]\nfoo = 1\n").contains("[[rule]]"));
    }

    /// The shipped example is compiled in and parsed here, so it cannot drift
    /// away from the parser the way a documented example silently does.
    ///
    /// It also settles T3's requirement at T1: if a shipped feature could not
    /// be written in this format, the format would be wrong.
    #[test]
    fn the_shipped_example_file_parses() {
        let source = include_str!("../../../rules.example.toml");
        let rules = parse_rules(source).expect("the shipped example must parse");
        assert_eq!(rules.len(), 4);
        assert!(rules.iter().all(|r| !r.set.is_empty()));
        assert!(rules.iter().any(|r| r.transition.is_some()));
    }

    #[test]
    fn a_transition_parses_with_its_duration_offset_and_starting_values() {
        let r = one(r#"
            [[rule]]
            name = "night"
            when.between = "20:00-07:00"
            set.display.color_temp = 3200
            transition.duration = "40m"
            transition.offset = 25
            transition.from.display.color_temp = 6600
        "#);
        assert_eq!(
            r.transition,
            Some(Transition {
                seconds: 40 * 60,
                offset: 25,
                from: vec![Assign {
                    cap: cap("display.color_temp"),
                    value: Value::Int(6600),
                }],
            })
        );
    }

    /// Each of these would load a rule that silently never fades.
    #[test]
    fn a_transition_that_cannot_fade_anything_is_an_error() {
        let base = "[[rule]]\nname = \"x\"\nset.display.color_temp = 3200\n";
        let no_window = format!("{base}when.idle = true\ntransition.duration = \"40m\"\n");
        assert!(err(&no_window).contains("between"), "{}", err(&no_window));

        let window = format!("{base}when.between = \"20:00-07:00\"\n");
        let stray = format!(
            "{window}transition.duration = \"40m\"\ntransition.from.display.vibrance = 50\n"
        );
        assert!(err(&stray).contains("does not set"), "{}", err(&stray));

        let text = format!(
            "{window}transition.duration = \"40m\"\ntransition.from.display.color_temp = \"warm\"\n"
        );
        assert!(err(&text).contains("only a number"), "{}", err(&text));

        let words = format!("{window}transition.duration = \"forty\"\n");
        assert!(err(&words).contains("40m"), "{}", err(&words));

        let offset = format!("{window}transition.duration = \"40m\"\ntransition.offset = 150\n");
        assert!(err(&offset).contains("0 to 100"), "{}", err(&offset));

        let missing = format!("{window}transition.offset = 10\n");
        assert!(
            err(&missing).contains("missing `duration`"),
            "{}",
            err(&missing)
        );
    }
}
