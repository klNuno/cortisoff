use cortisoff_host::Value;

/// The value at strength `s`, from 0 to 1, between two values. Only numbers
/// blend; anything else is already the target.
pub fn blend(from: &Value, to: &Value, s: f64) -> Value {
    let s = s.clamp(0.0, 1.0);
    match (from, to) {
        (Value::Int(a), Value::Int(b)) => {
            Value::Int((*a as f64 + (*b - *a) as f64 * s).round() as i64)
        }
        (Value::Float(a), Value::Float(b)) => Value::Float(a + (b - a) * s),
        (Value::Int(a), Value::Float(b)) => Value::Float(*a as f64 + (b - *a as f64) * s),
        (Value::Float(a), Value::Int(b)) => Value::Int((a + (*b as f64 - a) * s).round() as i64),
        _ => to.clone(),
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
}
