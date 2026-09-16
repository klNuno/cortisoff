//! The TOML back end: turning rules back into the file a person edits.
//!
//! [`crate::parse_rules`] is the reader; this is its inverse, and the pair is
//! what lets the dashboard offer a switch on a rule without the user ever
//! seeing TOML. The output is the same shape the documentation teaches, dotted
//! keys and all, so a file the app wrote and a file a person wrote are
//! indistinguishable.
//!
//! Round trip, not byte equality: comments and key order a person chose are
//! not preserved, and there is no honest way to preserve them while also
//! letting the app restructure a condition. What is guaranteed, and tested, is
//! that parsing the output gives back the rules that went in.

use cortisoff_host::Value;

use crate::rule::{Condition, Rule};

/// Render a whole rule file.
///
/// The header says the file is machine written, because someone will open it
/// after clicking around in the dashboard and wonder where their comments went.
pub fn write_rules(rules: &[Rule]) -> String {
    let mut out = String::from(
        "# Your rules. cortisoff rewrites this file when you change something in\n\
         # the dashboard, so comments do not survive a round trip.\n\
         #\n\
         # Every key inside `when` has to hold at once, and a rule owns what it\n\
         # sets only while its `when` holds. The full list of conditions is in\n\
         # `rules.example.toml` in the repository.\n",
    );
    for rule in rules {
        out.push_str("\n[[rule]]\n");
        out.push_str(&write_rule(rule));
    }
    out
}

fn write_rule(rule: &Rule) -> String {
    let mut out = format!("name = {}\n", string(&rule.name));
    // Both are omitted at their default, so a plain rule stays as short as one
    // a person would have typed.
    if !rule.enabled {
        out.push_str("enabled = false\n");
    }
    if rule.priority != crate::priority::RULE {
        out.push_str(&format!("priority = {}\n", rule.priority));
    }
    for (key, value) in condition_pairs(&rule.when) {
        out.push_str(&format!("when.{key} = {value}\n"));
    }
    for assign in &rule.set {
        out.push_str(&format!(
            "set.{} = {}\n",
            assign.cap.as_str(),
            value(&assign.value)
        ));
    }
    if let Some(t) = &rule.transition {
        out.push_str(&format!(
            "transition.duration = {}\n",
            string(&crate::format_duration(t.seconds))
        ));
        if t.offset != 0 {
            out.push_str(&format!("transition.offset = {}\n", t.offset));
        }
        for start in &t.from {
            out.push_str(&format!(
                "transition.from.{} = {}\n",
                start.cap.as_str(),
                value(&start.value)
            ));
        }
    }
    out
}

/// One condition, flattened into the `key = value` pairs that go under `when`.
///
/// A pair's key can itself be dotted, which is how `not` nests: `Not(App)`
/// comes back as `not.app`. The caller decides whether to print them as lines
/// or as an inline table.
fn condition_pairs(condition: &Condition) -> Vec<(String, String)> {
    match condition {
        Condition::Always => Vec::new(),
        Condition::App {
            exe,
            title_contains,
        } => {
            let mut pairs = Vec::new();
            if let Some(exe) = exe {
                pairs.push(("app".to_owned(), string(exe)));
            }
            if let Some(title) = title_contains {
                pairs.push(("title".to_owned(), string(title)));
            }
            pairs
        }
        Condition::Fullscreen(v) => vec![("fullscreen".to_owned(), v.to_string())],
        Condition::Maximized(v) => vec![("maximized".to_owned(), v.to_string())],
        Condition::IdleFor(v) => vec![("idle_for".to_owned(), v.to_string())],
        Condition::Idle(v) => vec![("idle".to_owned(), v.to_string())],
        Condition::Locked(v) => vec![("locked".to_owned(), v.to_string())],
        Condition::OnBattery(v) => vec![("on_battery".to_owned(), v.to_string())],
        Condition::AudioActive(v) => vec![("audio".to_owned(), v.to_string())],
        Condition::Between { start, end } => {
            vec![("between".to_owned(), string(&format!("{start}-{end}")))]
        }
        Condition::Not(inner) => nested("not", inner),
        // Several keys in one `when` already mean all of them, so an `all`
        // flattens into its parent whenever the keys do not collide. Two
        // branches wanting `app` is what forces the explicit form.
        Condition::All(branches) => {
            let mut pairs: Vec<(String, String)> = Vec::new();
            for branch in branches {
                for pair in condition_pairs(branch) {
                    if pairs.iter().any(|(key, _)| *key == pair.0) {
                        return vec![("all".to_owned(), branch_array(branches))];
                    }
                    pairs.push(pair);
                }
            }
            pairs
        }
        Condition::Any(branches) => match shorthand(branches) {
            Some(pair) => vec![pair],
            None => vec![("any".to_owned(), branch_array(branches))],
        },
    }
}

/// `not = { app = "x" }` would work, but `not.app = "x"` is what the docs show
/// and what a person writes, so the key is pushed down instead.
fn nested(prefix: &str, inner: &Condition) -> Vec<(String, String)> {
    let pairs = condition_pairs(inner);
    if pairs.is_empty() {
        // `not` of a condition that always holds. Nothing can express it as a
        // dotted key, and an empty table is what parses back to it.
        return vec![(prefix.to_owned(), "{}".to_owned())];
    }
    pairs
        .into_iter()
        .map(|(key, value)| (format!("{prefix}.{key}"), value))
        .collect()
}

/// `app = ["vlc.exe", "mpv.exe"]` rather than an `any` of two tables. The
/// reader accepts a list on `app` and on `between`, and that is how anyone
/// writes them.
fn shorthand(branches: &[Condition]) -> Option<(String, String)> {
    let key = match branches.first()? {
        Condition::App {
            exe: Some(_),
            title_contains: None,
        } => "app",
        Condition::Between { .. } => "between",
        _ => return None,
    };
    let mut values = Vec::with_capacity(branches.len());
    for branch in branches {
        let pairs = condition_pairs(branch);
        match pairs.as_slice() {
            [(k, v)] if k == key => values.push(v.clone()),
            _ => return None,
        }
    }
    Some((key.to_owned(), format!("[{}]", values.join(", "))))
}

fn branch_array(branches: &[Condition]) -> String {
    let tables: Vec<String> = branches.iter().map(inline_table).collect();
    format!("[{}]", tables.join(", "))
}

fn inline_table(condition: &Condition) -> String {
    let pairs = condition_pairs(condition);
    if pairs.is_empty() {
        return "{}".to_owned();
    }
    let body: Vec<String> = pairs
        .into_iter()
        .map(|(key, value)| format!("{key} = {value}"))
        .collect();
    format!("{{ {} }}", body.join(", "))
}

fn value(value: &Value) -> String {
    match value {
        Value::Int(v) => v.to_string(),
        // `1` would come back as an integer, so a float always keeps a point.
        Value::Float(v) if v.fract() == 0.0 && v.is_finite() => format!("{v:.1}"),
        Value::Float(v) => v.to_string(),
        Value::Bool(v) => v.to_string(),
        Value::Text(v) => string(v),
        Value::Mode {
            width,
            height,
            refresh_mhz,
        } => {
            let size = if *width == 0 && *height == 0 {
                String::new()
            } else {
                format!("{width}x{height}")
            };
            let refresh = match refresh_mhz {
                0 => String::new(),
                mhz if mhz % 1000 == 0 => format!("@{}", mhz / 1000),
                mhz => format!("@{}", f64::from(*mhz) / 1000.0),
            };
            string(&format!("{size}{refresh}"))
        }
    }
}

/// A TOML basic string. Hand written because the `toml` crate stopped
/// rendering a bare `Value`, and because a rule named with a quote in it must
/// not produce a file that no longer parses.
fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_rules;

    /// The property that matters. Byte equality is not promised, but a file
    /// that went through the dashboard must mean exactly what it meant before.
    fn round_trip(source: &str) {
        let rules = parse_rules(source).expect("the fixture parses");
        let written = write_rules(&rules);
        let again = parse_rules(&written)
            .unwrap_or_else(|e| panic!("what we wrote does not parse: {e}\n---\n{written}"));
        assert_eq!(rules, again, "wrote:\n{written}");
    }

    #[test]
    fn every_condition_shape_survives_a_round_trip() {
        round_trip(
            r#"
[[rule]]
name = "vibrance in competitive games"
priority = 100
when.app = ["valorant.exe", "cs2.exe"]
when.fullscreen = true
when.maximized = true
set.display.vibrance = 100
set.display.mode = "@240"

[[rule]]
name = "blue light at night, unless a film is playing"
when.between = "20:00-07:00"
when.not.app = ["vlc.exe", "mpc-hc64.exe"]
set.display.color_temp = 3400

[[rule]]
name = "everything at once"
enabled = false
when.any = [{ idle = true }, { locked = true }, { on_battery = true, audio = false }]
when.title = "youtube"
set.wallpaper.playback = false
set.display.brightness = 0.5

[[rule]]
name = "a default"
set.display.color_temp = 6500

[[rule]]
name = "a fade"
when.between = "20:00-07:00"
set.display.color_temp = 3200
set.display.vibrance = 40
transition.duration = "1h30m"
transition.offset = 50
transition.from.display.color_temp = 6600
"#,
        );
    }

    /// Two branches wanting the same key cannot flatten, and the explicit
    /// `all` is the only shape left.
    #[test]
    fn colliding_keys_fall_back_to_an_explicit_all() {
        let rules = parse_rules(
            r#"
[[rule]]
name = "two apps, two ways"
when.all = [{ app = "a.exe" }, { not.app = "b.exe" }, { app = "a.exe" }]
set.display.vibrance = 50
"#,
        )
        .unwrap();
        let written = write_rules(&rules);
        assert!(written.contains("when.all = ["), "wrote:\n{written}");
        assert_eq!(rules, parse_rules(&written).unwrap());
    }

    #[test]
    fn a_disabled_rule_says_so_and_an_enabled_one_stays_silent() {
        let rules = parse_rules(
            "[[rule]]\nname = \"off\"\nenabled = false\nset.display.vibrance = 1\n\n\
             [[rule]]\nname = \"on\"\nset.display.vibrance = 2\n",
        )
        .unwrap();
        let written = write_rules(&rules);
        assert_eq!(written.matches("enabled = false").count(), 1);
        assert!(!written.contains("enabled = true"));
        assert_eq!(rules, parse_rules(&written).unwrap());
    }

    /// A name with a quote in it used to be the way to write a file that no
    /// longer loads.
    #[test]
    fn a_hostile_name_is_escaped_rather_than_breaking_the_file() {
        let rules = parse_rules(
            "[[rule]]\nname = \"a \\\"quoted\\\" name\\nwith a newline\"\nset.a.b = 1\n",
        )
        .unwrap();
        let written = write_rules(&rules);
        assert_eq!(rules, parse_rules(&written).unwrap(), "wrote:\n{written}");
    }

    #[test]
    fn the_example_file_survives_a_round_trip() {
        round_trip(include_str!("../../../rules.example.toml"));
    }
}
