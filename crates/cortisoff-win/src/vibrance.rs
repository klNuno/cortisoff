//! `display.vibrance` on Windows, through NVAPI.
//!
//! Saturation is per output and lives in the driver, so it survives the
//! process exactly like the gamma ramp does. The baseline is every output's
//! current level, captured before the first apply and journaled, because a
//! user who has already moved the NVIDIA slider does not want it snapped back
//! to the driver default.
//!
//! AMD's ADL belongs behind this same capability, not behind a second one. A
//! plugin claims `display.vibrance` at 100 either way.

use cortisoff_host::{ids, Actuator, ActuatorError, CapabilityId, Control, Value};

use crate::nvapi::{self, Nvapi};

pub struct Vibrance {
    nvapi: Nvapi,
}

impl Vibrance {
    pub fn new() -> Option<Self> {
        Nvapi::open().map(|nvapi| Self { nvapi })
    }

    pub fn backend_name(&self) -> &'static str {
        "nvapi"
    }

    /// What the doctor shows: the range this driver reports, so a user writing
    /// a plugin knows whether 100 is the maximum or halfway.
    pub fn range(&self) -> Option<(i32, i32, i32)> {
        let first = self.nvapi.outputs.first()?;
        let info = self.nvapi.info(first)?;
        Some((info.min, info.max, info.default))
    }

    /// `name=level;name=level`, keyed on the device name so an output that is
    /// unplugged and comes back is still recognised.
    fn encode(&self) -> Option<String> {
        let mut out = String::new();
        for output in &self.nvapi.outputs {
            let info = self.nvapi.info(output)?;
            out.push_str(&format!("{}={};", output.device, info.current));
        }
        (!out.is_empty()).then_some(out)
    }

    fn restore(&self, encoded: &str) -> Result<(), ActuatorError> {
        for entry in encoded.split(';').filter(|s| !s.is_empty()) {
            let Some((device, level)) = entry.split_once('=') else {
                continue;
            };
            let Ok(level) = level.parse::<i32>() else {
                continue;
            };
            // An output named in the journal but absent today is skipped
            // rather than fatal: it is a monitor that is unplugged, and the
            // entry stays for the run where it comes back.
            if let Some(output) = self.nvapi.outputs.iter().find(|o| o.device == device) {
                let _ = self.nvapi.set_level(output, level);
            }
        }
        Ok(())
    }
}

impl Actuator for Vibrance {
    fn id(&self) -> CapabilityId {
        CapabilityId::new(ids::DISPLAY_VIBRANCE)
    }

    fn capture(&self) -> Result<Value, ActuatorError> {
        self.encode().map(Value::Text).ok_or_else(|| {
            ActuatorError::on(self.id(), "no NVIDIA output would report its vibrance")
        })
    }

    /// The level the first output sits at right now, which is what a plugin
    /// would set to change nothing. A fade with no `from` starts here.
    fn rest(&self) -> Option<Value> {
        let first = self.nvapi.outputs.first()?;
        let info = self.nvapi.info(first)?;
        Some(Value::Int(i64::from(info.current)))
    }

    /// The driver's own range, so a slider ends where the hardware does.
    fn control(&self) -> Control {
        let (min, max, default) = self.range().unwrap_or((0, 100, 50));
        Control::Range {
            min: i64::from(min),
            max: i64::from(max),
            step: 1,
            default: i64::from(default),
            unit: Some("%".to_owned()),
        }
    }

    fn apply(&self, value: &Value) -> Result<(), ActuatorError> {
        match value {
            Value::Int(level) => {
                let level = i32::try_from(*level).unwrap_or(i32::MAX);
                let mut failures = Vec::new();
                for output in &self.nvapi.outputs {
                    if let Err(code) = self.nvapi.set_level(output, level) {
                        failures.push(format!("{}: {}", output.device, nvapi::describe(code)));
                    }
                }
                if failures.is_empty() {
                    Ok(())
                } else {
                    Err(ActuatorError::on(self.id(), failures.join(", ")))
                }
            }
            // A captured baseline coming back, from a release or from the
            // journal after a crash.
            Value::Text(encoded) => self.restore(encoded),
            other => Err(ActuatorError::on(
                self.id(),
                format!("expected a vibrance level, got {other:?}"),
            )),
        }
    }

    /// The driver keeps it after the process dies, so the journal is the only
    /// thing that can undo a crash.
    fn volatile(&self) -> bool {
        false
    }
}
