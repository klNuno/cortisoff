//! `display.color_temp` on X11, through each CRTC's RandR gamma ramp.
//!
//! One backend. The ramp is the output LUT, so it tints the cursor and a
//! fullscreen game along with everything else, and X has no envelope like
//! the one Windows puts around `SetDeviceGammaRamp`: 2500 K is 2500 K.
//!
//! A ramp outlives the process that set it, exactly as on Windows: the X
//! server keeps it until someone writes another. So the actuator is not
//! volatile and its baseline goes to the journal before the first tint.
//!
//! XWayland accepts a ramp and the compositor never shows it, so the
//! capability is reported missing there rather than silently doing nothing.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use cortisoff_host::{ids, Actuator, ActuatorError, CapabilityId, Control, Value};
use x11rb::protocol::randr::ConnectionExt as _;

use crate::color::{self, Ramp, Rgb};
use crate::randr::{self, Head};
use crate::x11::Display;

pub struct ColorTemp {
    x: Rc<Display>,
    /// The baseline as this process captured it, output by output.
    captured: RefCell<Option<Vec<(String, Ramp)>>>,
    /// Outputs whose ramp carries this process's tint right now. Never read
    /// as a baseline: that is how a screen stays orange for good.
    live: RefCell<BTreeSet<String>>,
}

impl ColorTemp {
    /// The reason it cannot exist here, as the doctor shows it.
    pub fn new(x: Rc<Display>) -> Result<Self, String> {
        if !x.randr {
            return Err(
                "the X server has no RandR 1.2, which is what carries a gamma ramp per monitor"
                    .to_owned(),
            );
        }
        if x.xwayland {
            return Err("this X server is XWayland: a gamma ramp set through it never reaches the monitor, the Wayland compositor owns colour".to_owned());
        }
        if !randr::heads(&x).iter().any(|h| h.gamma_size > 0) {
            return Err("no lit monitor reports a gamma ramp through RandR".to_owned());
        }
        Ok(Self {
            x,
            captured: RefCell::new(None),
            live: RefCell::new(BTreeSet::new()),
        })
    }

    pub fn backend_name(&self) -> &'static str {
        "randr-gamma"
    }

    fn read(&self, head: &Head) -> Option<Ramp> {
        if head.gamma_size == 0 {
            return None;
        }
        let g = self
            .x
            .conn
            .randr_get_crtc_gamma(head.crtc)
            .ok()?
            .reply()
            .ok()?;
        if g.red.is_empty() || g.red.len() != g.green.len() || g.red.len() != g.blue.len() {
            return None;
        }
        let mut ramp = g.red;
        ramp.extend(g.green);
        ramp.extend(g.blue);
        Some(ramp)
    }

    /// Checked, so a refused ramp is a failure and not a hope.
    fn write(&self, head: &Head, ramp: &[u16]) -> bool {
        let n = usize::from(head.gamma_size);
        if n == 0 || ramp.len() != n * 3 {
            return false;
        }
        self.x
            .conn
            .randr_set_crtc_gamma(head.crtc, &ramp[..n], &ramp[n..2 * n], &ramp[2 * n..])
            .ok()
            .and_then(|cookie| cookie.check().ok())
            .is_some()
    }

    fn apply_tint(&self, c: &Rgb) -> Result<(), ActuatorError> {
        let heads = randr::heads(&self.x);
        let mut live = self.live.borrow_mut();
        let mut carried = false;
        for head in heads.iter().filter(|h| h.gamma_size > 0) {
            let ramp = color::ramp_for(c, usize::from(head.gamma_size));
            if self.write(head, &ramp) {
                live.insert(head.name.clone());
                carried = true;
            }
        }
        if carried {
            Ok(())
        } else {
            Err(ActuatorError::on(
                self.id(),
                "no monitor accepted the tint through its gamma ramp",
            ))
        }
    }

    fn restore(&self, encoded: &str) -> Result<(), ActuatorError> {
        let heads = randr::heads(&self.x);
        let present: Vec<(String, usize)> = heads
            .iter()
            .map(|h| (h.name.clone(), usize::from(h.gamma_size)))
            .collect();
        let failures = restore_using(encoded, &present, |name, ramp| {
            heads
                .iter()
                .find(|h| h.name == name)
                .is_some_and(|h| self.write(h, ramp))
        });
        if !failures.is_empty() {
            // The baseline stays, so a retry has something to put back, and
            // the error keeps the journal entry alive.
            return Err(ActuatorError::on(
                self.id(),
                format!("could not restore {}", failures.join(", ")),
            ));
        }
        self.captured.borrow_mut().take();
        self.live.borrow_mut().clear();
        Ok(())
    }
}

/// Put every captured ramp back on its output. `present` is every lit
/// output now with its gamma size. An output that left is not a failure: its
/// entry stays in the journal for the run where it comes back. One whose
/// gamma size changed gets its curve resampled rather than refused.
fn restore_using(
    encoded: &str,
    present: &[(String, usize)],
    mut write: impl FnMut(&str, &[u16]) -> bool,
) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, ramp) in color::decode(encoded) {
        let Some((_, size)) = present.iter().find(|(n, _)| *n == name) else {
            continue;
        };
        if *size == 0 {
            failures.push(format!("gamma ramp on {name}, which has none now"));
            continue;
        }
        let ramp = color::resample(&ramp, *size);
        if !write(&name, &ramp) {
            failures.push(format!("gamma ramp on {name}"));
        }
    }
    failures
}

impl Actuator for ColorTemp {
    fn id(&self) -> CapabilityId {
        CapabilityId::new(ids::DISPLAY_COLOR_TEMP)
    }

    /// The whole ramp of every lit output. Not a temperature: a calibrated
    /// monitor does not sit at an identity ramp, and restoring one would
    /// destroy the user's profile.
    fn capture(&self) -> Result<Value, ActuatorError> {
        let ramps: Vec<(String, Ramp)> = randr::heads(&self.x)
            .iter()
            .filter_map(|h| self.read(h).map(|ramp| (h.name.clone(), ramp)))
            .collect();
        if ramps.is_empty() {
            return Err(ActuatorError::on(
                self.id(),
                "no monitor would give up its gamma ramp",
            ));
        }
        let encoded = color::encode(&ramps);
        *self.captured.borrow_mut() = Some(ramps);
        Ok(Value::Text(encoded))
    }

    /// A monitor arrived, or came back, while a tint was held. Only what is
    /// missing from the baseline is read, and never an output this claim is
    /// already tinting.
    fn recapture(&self) -> Option<Result<Value, ActuatorError>> {
        let mut held = self.captured.borrow_mut();
        let captured = held.as_mut()?;
        let live = self.live.borrow();
        let fresh: Vec<(String, Ramp)> = randr::heads(&self.x)
            .iter()
            .filter(|h| !captured.iter().any(|(name, _)| *name == h.name))
            .filter(|h| !live.contains(&h.name))
            .filter_map(|h| self.read(h).map(|ramp| (h.name.clone(), ramp)))
            .collect();
        if fresh.is_empty() {
            return None;
        }
        captured.extend(fresh);
        Some(Ok(Value::Text(color::encode(captured))))
    }

    fn apply(&self, value: &Value) -> Result<(), ActuatorError> {
        match value {
            Value::Int(kelvin) => self.apply_tint(&color::from_kelvin(*kelvin as f32)),
            // A captured baseline coming back, from a release or from the
            // journal after a crash.
            Value::Text(encoded) => self.restore(encoded),
            other => Err(ActuatorError::on(
                self.id(),
                format!("expected a colour temperature in kelvin, got {other:?}"),
            )),
        }
    }

    fn volatile(&self) -> bool {
        false
    }

    fn rest(&self) -> Option<Value> {
        Some(Value::Int(color::NEUTRAL_KELVIN))
    }

    /// The same range as on Windows, so a plugin's slider means the same on
    /// both.
    fn control(&self) -> Control {
        Control::Range {
            min: 1500,
            max: color::NEUTRAL_KELVIN,
            step: 50,
            default: 3400,
            unit: Some("K".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> String {
        color::encode(&[
            ("DP-1".to_owned(), vec![100; 768]),
            ("HDMI-1".to_owned(), vec![200; 768]),
        ])
    }

    #[test]
    fn every_present_output_gets_its_own_curve_back() {
        let mut written = Vec::new();
        let failures = restore_using(
            &baseline(),
            &[("HDMI-1".to_owned(), 256), ("DP-1".to_owned(), 256)],
            |name, ramp| {
                written.push((name.to_owned(), ramp[0]));
                true
            },
        );
        assert!(failures.is_empty());
        assert_eq!(
            written,
            vec![("DP-1".to_owned(), 100), ("HDMI-1".to_owned(), 200)]
        );
    }

    /// The monitor that carried the baseline was unplugged. Nothing can be
    /// written to it and nothing has to be.
    #[test]
    fn an_output_that_left_is_not_a_failed_restore() {
        let mut written = Vec::new();
        let failures = restore_using(&baseline(), &[("DP-1".to_owned(), 256)], |name, _| {
            written.push(name.to_owned());
            true
        });
        assert!(failures.is_empty());
        assert_eq!(written, vec!["DP-1".to_owned()]);
    }

    #[test]
    fn a_refused_write_is_reported_and_the_others_still_go_back() {
        let mut written = Vec::new();
        let failures = restore_using(
            &baseline(),
            &[("DP-1".to_owned(), 256), ("HDMI-1".to_owned(), 256)],
            |name, _| {
                written.push(name.to_owned());
                name != "DP-1"
            },
        );
        assert_eq!(failures, vec!["gamma ramp on DP-1".to_owned()]);
        assert_eq!(written.len(), 2);
    }

    #[test]
    fn a_gamma_size_that_changed_gets_the_curve_resampled() {
        let mut sizes = Vec::new();
        let failures = restore_using(&baseline(), &[("DP-1".to_owned(), 1024)], |_, ramp| {
            sizes.push(ramp.len());
            true
        });
        assert!(failures.is_empty());
        assert_eq!(sizes, vec![3072]);
    }
}
