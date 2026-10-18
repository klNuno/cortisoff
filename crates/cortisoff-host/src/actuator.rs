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
/// Deliberately tiny. An actuator never sees priorities, plugins, other
/// actuators, or the reason it is being changed. That is what makes porting to
/// a new operating system a small, obvious job instead of a rewrite.
pub trait Actuator {
    fn id(&self) -> CapabilityId;

    /// Read the current state, so it can become the baseline.
    fn capture(&self) -> Result<Value, ActuatorError>;

    fn apply(&self, value: &Value) -> Result<(), ActuatorError>;

    /// True when one output can be held apart from the others, which is what
    /// [`Self::apply_output`] and [`Self::release_output`] need.
    fn per_output(&self) -> bool {
        false
    }

    /// Push a value onto one output only, over whatever [`Self::apply`] put
    /// on the rest. `output` is the actuator's own name for it, the one the
    /// platform reports in [`crate::Event::Fullscreen`]. The baseline is the
    /// whole actuator's, captured before the first apply of either kind.
    fn apply_output(&self, output: &str, _value: &Value) -> Result<(), ActuatorError> {
        Err(ActuatorError::on(
            self.id(),
            format!("cannot hold {output} apart from the other outputs"),
        ))
    }

    /// Let one output go: it shows the value [`Self::apply`] last put on the
    /// others, or its own baseline when nothing holds the whole capability.
    /// A restore of the baseline lets go of every output too.
    fn release_output(&self, output: &str) -> Result<(), ActuatorError> {
        Err(ActuatorError::on(
            self.id(),
            format!("cannot hold {output} apart from the other outputs"),
        ))
    }

    /// The hardware underneath moved: a monitor was plugged, unplugged, or
    /// woke up with a different identity. Hand back a baseline covering what
    /// is there now, or `None` when the one already captured still does.
    ///
    /// This exists because a baseline is captured once, on the first apply,
    /// and a monitor that arrives after it is not in it. Tinting that monitor
    /// with nothing to put back is how a screen stays orange: the release
    /// restores the monitors that were there at capture time and leaves the
    /// newcomer tinted, with no entry in the journal to fix it at the next
    /// start either.
    ///
    /// Called only when the platform reports a configuration change, never on
    /// the apply path: a fade steps twenty five times a second and must not
    /// pay for a monitor enumeration each time.
    fn recapture(&self) -> Option<Result<Value, ActuatorError>> {
        None
    }

    /// True when the operating system undoes this by itself once the process
    /// dies, which means it needs no baseline journal.
    ///
    /// The Windows magnification colour effect is volatile. A gamma ramp and a
    /// display mode are not, and they are exactly the two that leave a screen
    /// orange or stuck at 60 Hz after a crash.
    fn volatile(&self) -> bool;

    /// What a plugin would have to claim to get what the screen shows when nothing
    /// holds it, in a claim's own shape rather than the baseline's.
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
