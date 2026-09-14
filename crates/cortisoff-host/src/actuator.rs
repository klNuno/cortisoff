use crate::capability::{CapabilityId, Control};
use crate::value::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct ActuatorError {
    pub cap: Option<CapabilityId>,
    pub message: String,
}

impl ActuatorError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            cap: None,
            message: message.into(),
        }
    }

    pub fn on(cap: CapabilityId, message: impl Into<String>) -> Self {
        Self {
            cap: Some(cap),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ActuatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.cap {
            Some(c) => write!(f, "{c}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ActuatorError {}

/// One thing on this machine that holds state and can be changed.
///
/// Deliberately tiny. An actuator never sees priorities, rules, other
/// actuators, or the reason it is being changed. That is what makes porting to
/// a new operating system a small, obvious job instead of a rewrite.
pub trait Actuator {
    fn id(&self) -> CapabilityId;

    /// Read the current state, so it can become the baseline.
    fn capture(&self) -> Result<Value, ActuatorError>;

    fn apply(&self, value: &Value) -> Result<(), ActuatorError>;

    /// True when the operating system undoes this by itself once the process
    /// dies, which means it needs no baseline journal.
    ///
    /// The Windows magnification colour effect is volatile. A gamma ramp and a
    /// display mode are not, and they are exactly the two that leave a screen
    /// orange or stuck at 60 Hz after a crash.
    fn volatile(&self) -> bool;

    /// What a rule would have to set to get what the screen shows when nothing
    /// holds it, in a rule's own shape rather than the baseline's.
    ///
    /// This is where a fade with no `from` starts. A colour temperature rests
    /// at the point where its curve leaves the ramp alone, vibrance at the
    /// level the driver reports right now. A capability with no sensible
    /// number here, a display mode or a wallpaper, returns `None` and
    /// switches.
    fn rest(&self) -> Option<Value> {
        None
    }

    /// What a client should draw to set this. A slider needs its range from
    /// the hardware, not from a table in the client, which is why this is a
    /// method and not a constant.
    fn control(&self) -> Control {
        Control::Text {
            hint: String::new(),
        }
    }
}
