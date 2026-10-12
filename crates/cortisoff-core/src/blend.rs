use cortisoff_host::Value;

/// How a fade travels between two numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    /// Straight from one to the other.
    Linear,
    /// Straight in mireds, a million over the kelvin. Equal steps in mireds
    /// look equal; equal steps in kelvin barely move a cool screen and then
    /// rush through the warm end, which is a fade that seems to wait and then
    /// jump.
    Mired,
}

/// The value at strength `s`, from 0 to 1, between two values. Only numbers
/// blend; anything else is already the target.
pub fn blend(from: &Value, to: &Value, s: f64) -> Value {
    blend_on(Curve::Linear, from, to, s)
}

pub fn blend_on(curve: Curve, from: &Value, to: &Value, s: f64) -> Value {
    let s = s.clamp(0.0, 1.0);
    let (Some(a), Some(b)) = (number(from), number(to)) else {
        return to.clone();
    };
    let v = match curve {
        Curve::Mired if a > 0.0 && b > 0.0 => 1.0 / (1.0 / a + (1.0 / b - 1.0 / a) * s),
        _ => a + (b - a) * s,
    };
    match to {
        Value::Int(_) => Value::Int(v.round() as i64),
        _ => Value::Float(v),
    }
}

/// Progress eased in and out, so a fade leaves and lands softly instead of
/// starting and stopping at full speed.
pub fn ease(s: f64) -> f64 {
    let s = s.clamp(0.0, 1.0);
    s * s * (3.0 - 2.0 * s)
}

/// The steepest [`ease`] gets, at the middle.
pub const EASE_PEAK: f64 = 1.5;

/// How much faster than a straight line a curve moves at its steepest, for
/// timing frames: a mired fade moves fastest at its warm end, by the ratio of
/// its two ends.
pub fn peak_rate(curve: Curve, a: f64, b: f64) -> f64 {
    match curve {
        Curve::Mired if a > 0.0 && b > 0.0 => a.max(b) / a.min(b),
        _ => 1.0,
    }
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// Whether a value of this shape can fade towards `to`.
pub fn fadeable(from: &Value, to: &Value) -> bool {
    matches!(
        (from, to),
        (
            Value::Int(_) | Value::Float(_),
            Value::Int(_) | Value::Float(_)
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_blend_and_the_rest_switches() {
        assert_eq!(
            blend(&Value::Int(6600), &Value::Int(3200), 0.5),
            Value::Int(4900)
        );
        assert_eq!(
            blend(&Value::Bool(true), &Value::Bool(false), 0.1),
            Value::Bool(false)
        );
        assert!(fadeable(&Value::Int(1), &Value::Float(2.0)));
        assert!(!fadeable(&Value::Int(1), &Value::Text("a".into())));
    }

    /// Half way in mireds between 6600 K and 3200 K is about 4310 K, well
    /// below the 4900 K half way in kelvin: the warm end gets its share.
    #[test]
    fn a_mired_fade_gives_the_warm_end_its_share() {
        let half = blend_on(Curve::Mired, &Value::Int(6600), &Value::Int(3200), 0.5);
        assert_eq!(half, Value::Int(4310));
        assert_eq!(
            blend_on(Curve::Mired, &Value::Int(6600), &Value::Int(3200), 1.0),
            Value::Int(3200)
        );
        assert_eq!(peak_rate(Curve::Mired, 6600.0, 3200.0), 6600.0 / 3200.0);
    }

    #[test]
    fn easing_leaves_and_lands_softly() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(0.5), 0.5);
        assert_eq!(ease(1.0), 1.0);
        assert!(ease(0.1) < 0.1 && ease(0.9) > 0.9);
    }
}
