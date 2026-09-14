//! Turning what a person typed into a [`Value`].
//!
//! It lives here rather than in a client because there is more than one
//! client. The CLI takes `cortisoff set display.mode @240` and the dashboard
//! takes the same string out of a text field, and the two agreeing is not
//! something to leave to two copies of the same twenty lines.
//!
//! The daemon still owns what a capability accepts. This only decides the
//! shape, never whether the shape is welcome.

use cortisoff_host::Value;

/// Never fails. Anything that is not a number, a boolean or a display mode is
/// text, and the daemon answers with a sentence if the capability wanted
/// something else.
pub fn parse_value(raw: &str) -> Value {
    let raw = raw.trim();
    if let Ok(n) = raw.parse::<i64>() {
        return Value::Int(n);
    }
    match raw {
        "true" | "on" | "yes" => return Value::Bool(true),
        "false" | "off" | "no" => return Value::Bool(false),
        _ => {}
    }
    if let Some(mode) = parse_mode(raw) {
        return mode;
    }
    Value::Text(raw.to_owned())
}

/// `2560x1440@240`, `@240` and `1920x1080` are the three shapes, the same
/// three a rule file takes. Refresh is carried in millihertz because 59.94 Hz
/// is a real mode and rounding it to 60 picks a different one.
fn parse_mode(raw: &str) -> Option<Value> {
    let (size, refresh) = match raw.split_once('@') {
        Some((size, refresh)) => (size, Some(refresh)),
        None if raw.contains('x') => (raw, None),
        None => return None,
    };

    let (width, height) = match size {
        "" => (0, 0),
        size => {
            let (w, h) = size.split_once('x')?;
            (w.trim().parse().ok()?, h.trim().parse().ok()?)
        }
    };

    let refresh_mhz = match refresh {
        None => 0,
        Some(refresh) => {
            let hz: f64 = refresh.trim().parse().ok()?;
            if !hz.is_finite() || hz <= 0.0 {
                return None;
            }
            (hz * 1000.0).round() as u32
        }
    };

    if width == 0 && height == 0 && refresh_mhz == 0 {
        return None;
    }
    Some(Value::Mode {
        width,
        height,
        refresh_mhz,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_stays_a_number() {
        assert_eq!(parse_value("3400"), Value::Int(3400));
        assert_eq!(parse_value(" 100 "), Value::Int(100));
    }

    #[test]
    fn the_words_a_person_types_for_yes_and_no_all_work() {
        for yes in ["true", "on", "yes"] {
            assert_eq!(parse_value(yes), Value::Bool(true));
        }
        for no in ["false", "off", "no"] {
            assert_eq!(parse_value(no), Value::Bool(false));
        }
    }

    #[test]
    fn the_three_display_mode_shapes_parse() {
        assert_eq!(
            parse_value("2560x1440@240"),
            Value::Mode {
                width: 2560,
                height: 1440,
                refresh_mhz: 240_000
            }
        );
        assert_eq!(
            parse_value("@240"),
            Value::Mode {
                width: 0,
                height: 0,
                refresh_mhz: 240_000
            }
        );
        assert_eq!(
            parse_value("1920x1080"),
            Value::Mode {
                width: 1920,
                height: 1080,
                refresh_mhz: 0
            }
        );
    }

    /// 59.94 Hz is real hardware and the whole reason refresh is millihertz.
    #[test]
    fn a_fractional_rate_keeps_its_fraction() {
        assert_eq!(
            parse_value("@59.94"),
            Value::Mode {
                width: 0,
                height: 0,
                refresh_mhz: 59_940
            }
        );
    }

    /// Anything else is text, and the daemon is the one that says no.
    #[test]
    fn a_word_is_text_and_a_broken_mode_is_text_too() {
        assert_eq!(parse_value("paused"), Value::Text("paused".to_owned()));
        assert_eq!(parse_value("2560x"), Value::Text("2560x".to_owned()));
        assert_eq!(parse_value("@abc"), Value::Text("@abc".to_owned()));
    }
}
