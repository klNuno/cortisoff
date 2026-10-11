use serde::{Deserialize, Serialize};

/// What an actuator holds.
///
/// Deliberately small. A capability that does not fit one of these variants is
/// a sign the capability is doing two things and should be split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    /// Vibrance level, colour temperature in kelvin, percentages.
    Int(i64),
    Float(f64),
    /// Wallpaper playback, night mode on or off.
    Bool(bool),
    Text(String),
    /// Resolution and refresh rate. Refresh is in millihertz because 59.94 Hz
    /// exists and rounding it to 60 picks the wrong mode on real hardware.
    Mode {
        width: u32,
        height: u32,
        refresh_mhz: u32,
    },
}

impl Value {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// Two values of different shapes on the same capability is a plugin bug.
    pub fn same_shape(&self, other: &Value) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}
