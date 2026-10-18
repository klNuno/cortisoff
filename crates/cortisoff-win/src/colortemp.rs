//! `display.color_temp` on Windows.
//!
//! Three backends behind one capability, picked monitor by monitor on every
//! apply. The gamma ramp is the default: it is the output LUT, so it tints
//! the mouse cursor and a game in exclusive fullscreen along with everything
//! else, which is what LightBulb and f.lux do. The magnification colour effect
//! goes through the compositor, and the compositor never draws the hardware
//! cursor, so a magnifier tint leaves a white arrow on an orange screen. It
//! stays as the fallback for a machine where no ramp can be written. Under HDR
//! neither software path is defined, so an output with HDR on is tinted by
//! its own panel through DDC/CI when the panel answers, and by its ramp when
//! it does not.
//!
//! The actuator reports itself non-volatile whatever backend it is using. The
//! dispatcher journals once, on the first apply, and cannot be told later that
//! the backend changed underneath it; claiming volatility because the current
//! backend happens to be the magnifier is how a crash leaves a screen orange
//! with nothing on disk to fix it.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use cortisoff_host::{ids, Actuator, ActuatorError, CapabilityId, Control, Value};
use windows::Win32::UI::Magnification::{
    MagInitialize, MagSetFullscreenColorEffect, MagUninitialize, MAGCOLOREFFECT,
};

use crate::color::{self, Ramp, Rgb};
use crate::ddc::{self, Gains, Landed};
use crate::screen;

pub struct ColorTemp {
    magnifier: bool,
    /// At least one monitor gave up its ramp when this was built.
    gamma: bool,
    /// The baseline as this process captured it. Panel gains are scaled from
    /// it, and a path losing a monitor mid-claim is put back from it rather
    /// than from a guess.
    captured: RefCell<Option<Captured>>,
    /// What is pushed right now, so the path losing a monitor can let go
    /// before the next one takes it, and the two never stack.
    live: RefCell<Live>,
    /// Panel gains go out on their own thread, so a slow panel never holds
    /// back the ramps of the other monitors.
    panels: ddc::Writer,
    /// Panels that accepted a write from this process. Their next writes are
    /// posted; a panel's first write is made in place, because its answer
    /// decides whether its monitor needs the ramp in the same apply.
    proven: RefCell<BTreeSet<String>>,
    /// The base gains of every panel this process is tinting, kept on disk
    /// until each is verified back.
    memory: ddc::Memory,
    /// What the whole capability holds, `None` for its baseline.
    whole: RefCell<Option<Rgb>>,
    /// Monitors held apart from the whole, by device name: the one a game
    /// fills, kept at its own colours while the others stay warm.
    apart: RefCell<BTreeMap<String, Rgb>>,
    /// `GdiIcmGammaRange` lifted. Read once: it takes a reboot to change.
    unlocked: bool,
}

struct Captured {
    ramps: Vec<(String, Ramp)>,
    gains: Vec<(String, Gains)>,
}

#[derive(Default)]
struct Live {
    magnifier: bool,
    /// Devices whose ramp carries the tint.
    gamma: BTreeSet<String>,
    /// The gains each panel last accepted. A fade step that rounds to the same
    /// integers sends nothing: blue goes from full gain to about half over a
    /// forty minute fade, so about fifty writes per panel instead of one
    /// per ten second step, two hundred and forty.
    panel: BTreeMap<String, [u32; 3]>,
    /// Every panel written this claim, accepted or not, which is what a
    /// restore has to put back.
    touched: BTreeSet<String>,
    /// Panels that refused a write. They are left to their ramp until the
    /// claim ends, rather than retried and refused on every fade step.
    refused: BTreeSet<String>,
}

/// Where a tint goes on one apply.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    /// The magnifier may carry the tint if no ramp and no panel takes it.
    /// It is all or nothing, one effect over every monitor, and it does not
    /// apply in exclusive fullscreen or mix with an HDR output's panel.
    magnifier: bool,
    /// Devices, `\\.\DISPLAY1`.
    gamma: Vec<String>,
    /// Physical monitors, `\\.\DISPLAY1#0`.
    panel: Vec<String>,
}

/// `outputs` is every monitor with whether HDR is on, `panels` every physical
/// monitor whose gain is captured and still usable.
///
/// Every monitor gets its own path: its panel when HDR is on and a gain is
/// known, its ramp otherwise. The magnifier is only the fallback, allowed on
/// an SDR desktop with no exclusive fullscreen, and taken when no ramp write
/// is accepted.
fn plan(outputs: &[(String, bool)], panels: &[String], magnifier: bool, fullscreen: bool) -> Plan {
    let any_hdr = outputs.iter().any(|(_, hdr)| *hdr);
    let mut out = Plan {
        magnifier: magnifier && !any_hdr && !fullscreen,
        ..Plan::default()
    };
    for (device, hdr) in outputs {
        let own: Vec<&String> = panels
            .iter()
            .filter(|key| ddc::device_of(key) == device)
            .collect();
        if *hdr && !own.is_empty() {
            out.panel.extend(own.into_iter().cloned());
        } else {
            out.gamma.push(device.clone());
        }
    }
    out
}

impl ColorTemp {
    /// `None` when the magnifier cannot be initialised and the machine has no
    /// monitor whose ramp can be read, which is when this capability honestly
    /// does not exist.
    pub fn new() -> Option<Self> {
        let magnifier = unsafe { MagInitialize() }.as_bool();
        let gamma = screen::device_names()
            .iter()
            .any(|name| screen::capture(name).is_some());
        (magnifier || gamma).then_some(Self {
            magnifier,
            gamma,
            captured: RefCell::new(None),
            live: RefCell::new(Live::default()),
            panels: ddc::Writer::new(),
            proven: RefCell::new(BTreeSet::new()),
            memory: ddc::Memory::open(),
            whole: RefCell::new(None),
            apart: RefCell::new(BTreeMap::new()),
            unlocked: screen::gamma_range_unlocked(),
        })
    }

    /// The path an SDR desktop takes. A ramp that can be read is one that
    /// can be written on every machine seen so far, and the magnifier is
    /// what is left when none can.
    pub fn backend_name(&self) -> &'static str {
        if self.gamma {
            "gamma"
        } else {
            "magnification"
        }
    }

    /// Push what every monitor should show now: its own colour when it is
    /// held apart, the whole capability's otherwise, its baseline when
    /// neither holds it.
    fn push(&self) -> Result<(), ActuatorError> {
        let hdr = screen::hdr_outputs();
        let outputs: Vec<(String, bool)> = screen::device_names()
            .into_iter()
            .map(|device| {
                let on = hdr.iter().any(|o| o.device == device && o.enabled);
                (device, on)
            })
            .collect();

        let captured = self.captured.borrow();
        let mut live = self.live.borrow_mut();
        // A posted write a panel refused since the last apply: its monitor
        // takes the ramp from this apply on, and its next write is made in
        // place again.
        for key in self.panels.take_refused() {
            live.panel.remove(&key);
            self.proven.borrow_mut().remove(&key);
            live.refused.insert(key);
        }
        let panels: Vec<String> = captured
            .iter()
            .flat_map(|c| c.gains.iter().map(|(key, _)| key.clone()))
            .filter(|key| !live.refused.contains(key))
            .collect();
        let apart = self.apart.borrow();
        let whole = *self.whole.borrow();
        // One effect over every monitor cannot leave one of them out.
        let magnifier = self.magnifier && apart.is_empty();
        let plan = plan(&outputs, &panels, magnifier, fullscreen_now());

        // Windows holds a ramp at about 3250 K. Every monitor is held there
        // with it, the HDR panel included, or the panel goes warmer than the
        // two ramps beside it and the desk shows two colours.
        let clamp = !self.unlocked && (!plan.gamma.is_empty() || !live.refused.is_empty());
        let colour = |device: &str| shown(whole, &apart, device, clamp);

        // Whichever path is losing a monitor lets go first, or two of them
        // multiply and the screen goes twice as warm as asked.
        if live.magnifier && !plan.magnifier {
            set_magnifier(&Rgb::NEUTRAL)?;
            live.magnifier = false;
        }
        let leaving: Vec<String> = live
            .gamma
            .iter()
            .filter(|device| !plan.gamma.contains(device))
            .cloned()
            .collect();
        // A ramp the driver refuses, as during a game's mode change, stays
        // live and is put back on a later apply.
        for device in leaving {
            if screen::write(&device, &baseline_ramp(captured.as_ref(), &device)) {
                live.gamma.remove(&device);
            }
        }
        let leaving: Vec<String> = live
            .touched
            .iter()
            .filter(|key| !plan.panel.contains(key))
            .cloned()
            .collect();
        for key in leaving {
            if let Some(base) = gains_of(captured.as_ref(), &key) {
                if self.panels.write_now(&key, ddc::currents(base)) == Landed::Verified {
                    self.memory.forget(&key);
                }
            }
            live.touched.remove(&key);
            live.panel.remove(&key);
        }

        // Whether any monitor wants a tint, and whether one took it.
        let mut wanted = false;
        let mut carried = false;
        for key in &plan.panel {
            let Some(base) = gains_of(captured.as_ref(), key) else {
                continue;
            };
            let device = ddc::device_of(key).to_owned();
            let gains = match colour(&device) {
                Some(c) => {
                    wanted = true;
                    ddc::targets(base, &c)
                }
                // Back to its own gains, if this process ever moved them.
                None if live.touched.contains(key) => ddc::currents(base),
                None => continue,
            };
            if live.panel.get(key) == Some(&gains) {
                carried = true;
                continue;
            }
            if live.touched.insert(key.clone()) {
                // Remembered before the first write, so a capture made while
                // this tint is on never takes it for the panel's own.
                self.memory.hold(key, base);
            }
            if self.proven.borrow().contains(key) {
                self.panels.post(key, gains);
                live.panel.insert(key.clone(), gains);
                carried = true;
            } else if self.panels.write_now(key, gains).took() {
                self.proven.borrow_mut().insert(key.clone());
                live.panel.insert(key.clone(), gains);
                carried = true;
            } else {
                // A panel that stops answering still gets a tint, from its
                // ramp, and is not asked again until the claim ends.
                live.panel.remove(key);
                live.refused.insert(key.clone());
                if let Some(c) = colour(&device) {
                    if push_ramp(&device, &c) {
                        live.gamma.insert(device);
                        carried = true;
                    }
                }
            }
        }
        for device in &plan.gamma {
            match colour(device) {
                Some(c) => {
                    wanted = true;
                    if push_ramp(device, &c) {
                        live.gamma.insert(device.clone());
                        carried = true;
                    }
                }
                // Its own ramp back, not the identity: a calibrated monitor
                // held apart from the night shows its profile. Live until the
                // write is taken, so a refusal is tried again.
                None => {
                    if live.gamma.contains(device)
                        && screen::write(device, &baseline_ramp(captured.as_ref(), device))
                    {
                        live.gamma.remove(device);
                    }
                }
            }
        }
        // The fallback comes on only when nothing else took the tint, and
        // goes off the moment a ramp does. That one call is the only time the
        // two overlap: dropping the magnifier before the writes would blink
        // the screen to neutral on every fade step of a machine that refuses
        // every ramp.
        let all = whole.filter(|c| !neutral(c));
        if carried && live.magnifier {
            set_magnifier(&Rgb::NEUTRAL)?;
            live.magnifier = false;
        } else if !carried && plan.magnifier {
            match all {
                Some(c) => {
                    set_magnifier(&c)?;
                    live.magnifier = true;
                    carried = true;
                }
                None if live.magnifier => {
                    set_magnifier(&Rgb::NEUTRAL)?;
                    live.magnifier = false;
                }
                None => {}
            }
        }

        if carried || !wanted {
            Ok(())
        } else {
            Err(ActuatorError::on(
                self.id(),
                "no monitor accepted the tint, through its gamma ramp or its panel",
            ))
        }
    }

    /// Put back exactly what was captured, monitor by monitor and panel by
    /// panel, and drop the magnifier effect whether or not it is live.
    fn restore(&self, encoded: &str) -> Result<(), ActuatorError> {
        self.restore_using(
            encoded,
            &screen::device_names(),
            set_magnifier,
            screen::write,
            |key, values| self.panels.write_now(key, values),
        )
    }

    /// `present` is every monitor on this machine right now. A baseline naming
    /// one that is not there is not a failure: it is a monitor that was
    /// unplugged or switched off since it was captured, and its entry is kept
    /// for the run where it comes back. Counting it as a failure is what left
    /// the journal permanently dirty and the doctor permanently complaining
    /// about a screen nobody can put back.
    fn restore_using(
        &self,
        encoded: &str,
        present: &[String],
        reset: impl FnOnce(&Rgb) -> Result<(), ActuatorError>,
        mut write_ramp: impl FnMut(&str, &Ramp) -> bool,
        mut write_panel: impl FnMut(&str, [u32; 3]) -> Landed,
    ) -> Result<(), ActuatorError> {
        let here = |device: &str| present.iter().any(|d| d == device);
        let mut failures = Vec::new();
        if self.magnifier {
            if let Err(error) = reset(&Rgb::NEUTRAL) {
                failures.push(error.to_string());
            }
        }
        for (device, ramp) in color::decode(encoded) {
            if !write_ramp(&device, &ramp) && here(&device) {
                failures.push(format!("gamma ramp on {device}"));
            }
        }
        // A panel this process captured but never wrote is left alone: DDC/CI
        // is slow and there is nothing to undo. A journal left by a crash comes
        // with no such knowledge, so every panel in it goes back.
        let known = self.captured.borrow().is_some();
        let mut live = self.live.borrow_mut();
        for (key, gains) in ddc::decode(encoded) {
            if known && !live.touched.contains(&key) {
                continue;
            }
            match write_panel(&key, ddc::currents(&gains)) {
                Landed::Verified => self.memory.forget(&key),
                // Taken and never read back, it may still show the night. It
                // stays remembered, so no capture takes the night for its own
                // gains, and the restore fails, so the journal keeps it for
                // the next start: a check posted to the writer does not
                // outlive the process.
                _ if here(ddc::device_of(&key)) => {
                    failures.push(format!("panel gains on {key}"));
                }
                _ => {}
            }
        }
        if !failures.is_empty() {
            return Err(ActuatorError::on(
                self.id(),
                format!("could not restore {}", failures.join(", ")),
            ));
        }
        // Keep both the baseline and touched-panel set until every output
        // has returned. A failed attempt must remain safe to retry, and its
        // error keeps the dispatcher's durable journal entry alive.
        self.captured.borrow_mut().take();
        *live = Live::default();
        // A baseline back is every monitor back, those held apart included.
        self.whole.borrow_mut().take();
        self.apart.borrow_mut().clear();
        Ok(())
    }
}

impl Actuator for ColorTemp {
    fn id(&self) -> CapabilityId {
        CapabilityId::new(ids::DISPLAY_COLOR_TEMP)
    }

    /// The whole ramp of every monitor, plus the gains of every panel on an
    /// output that can turn HDR on. Not a temperature.
    ///
    /// A calibrated monitor does not sit at a neutral ramp or a neutral gain,
    /// so "restore 6500 kelvin" would silently destroy the user's profile.
    /// This is opaque to the engine and it is what lands in the journal.
    /// Panels are read even while HDR is off, because the capture happens
    /// once per claim and HDR can be switched on in the middle of one.
    fn capture(&self) -> Result<Value, ActuatorError> {
        let ramps: Vec<(String, Ramp)> = screen::device_names()
            .into_iter()
            .filter_map(|name| screen::capture(&name).map(|ramp| (name, ramp)))
            .collect();
        if ramps.is_empty() {
            return Err(ActuatorError::on(
                self.id(),
                "no monitor would give up its gamma ramp",
            ));
        }
        let capable: Vec<String> = screen::hdr_outputs()
            .into_iter()
            .filter(|o| o.supported)
            .map(|o| o.device)
            .collect();
        let gains = if capable.is_empty() {
            Vec::new()
        } else {
            // Never read a panel while a write to it is still on the wire.
            self.panels.settle();
            ddc::capture(&capable)
        };
        self.panels.read_back(&gains);
        let gains = self.memory.correct(gains);

        let captured = Captured { ramps, gains };
        let encoded = encode(&captured);
        *self.captured.borrow_mut() = Some(captured);
        Ok(Value::Text(encoded))
    }

    /// A monitor arrived, or came back, while a tint was held.
    ///
    /// Only what is missing from the baseline is read. A monitor that is gone
    /// keeps its captured ramp, because that is what has to go back if it
    /// returns under the same name, and a monitor this claim is already
    /// tinting is skipped: its ramp is this process's own tint, and capturing
    /// that as a baseline is how a screen stays orange for good.
    fn recapture(&self) -> Option<Result<Value, ActuatorError>> {
        let mut held = self.captured.borrow_mut();
        // Nothing applied means nothing to grow: the next apply captures a
        // whole baseline of its own.
        let captured = held.as_mut()?;
        let mut live = self.live.borrow_mut();

        // A panel that refused a write while its monitor was renegotiating
        // its link is not a panel that cannot do DDC/CI. Without this, one
        // refusal during a display change left that monitor on the gamma path
        // until the claim ended, which for Night light is all night.
        live.refused.clear();

        let fresh: Vec<(String, Ramp)> = screen::device_names()
            .into_iter()
            .filter(|device| !captured.ramps.iter().any(|(d, _)| d == device))
            .filter(|device| !live.gamma.contains(device))
            .filter_map(|device| screen::capture(&device).map(|ramp| (device, ramp)))
            .collect();

        // Panels answer slowly, so they are only asked about on an output
        // that can turn HDR on and has nothing captured yet.
        let capable: Vec<String> = screen::hdr_outputs()
            .into_iter()
            .filter(|o| o.supported)
            .map(|o| o.device)
            .filter(|device| {
                !captured
                    .gains
                    .iter()
                    .any(|(key, _)| ddc::device_of(key) == device)
            })
            .collect();
        let gains = if capable.is_empty() {
            Vec::new()
        } else {
            // Never read a panel while a write to it is still on the wire.
            self.panels.settle();
            ddc::capture(&capable)
        };
        self.panels.read_back(&gains);
        let gains = self.memory.correct(gains);

        if fresh.is_empty() && gains.is_empty() {
            return None;
        }
        captured.ramps.extend(fresh);
        captured.gains.extend(gains);
        Some(Ok(Value::Text(encode(captured))))
    }

    fn apply(&self, value: &Value) -> Result<(), ActuatorError> {
        match value {
            Value::Int(kelvin) => {
                *self.whole.borrow_mut() = Some(color::from_kelvin(*kelvin as f32));
                self.push()
            }
            // A captured baseline coming back, from a release or from the
            // journal after a crash.
            Value::Text(encoded) => self.restore(encoded),
            other => Err(ActuatorError::on(
                self.id(),
                format!("expected a colour temperature in kelvin, got {other:?}"),
            )),
        }
    }

    /// Every monitor has a ramp, or a panel, of its own. Not the magnifier:
    /// one effect over every monitor cannot leave one out.
    fn per_output(&self) -> bool {
        self.gamma
    }

    /// `output` is a device name, `\\.\DISPLAY3`, as the fullscreen event
    /// reports it.
    fn apply_output(&self, output: &str, value: &Value) -> Result<(), ActuatorError> {
        let Value::Int(kelvin) = value else {
            return Err(ActuatorError::on(
                self.id(),
                format!("expected a colour temperature in kelvin, got {value:?}"),
            ));
        };
        if !self.gamma {
            return Err(ActuatorError::on(
                self.id(),
                format!("cannot hold {output} apart: the magnifier tints every monitor at once"),
            ));
        }
        self.apart
            .borrow_mut()
            .insert(output.to_owned(), color::from_kelvin(*kelvin as f32));
        self.push()
    }

    fn release_output(&self, output: &str) -> Result<(), ActuatorError> {
        if self.apart.borrow_mut().remove(output).is_none() {
            return Ok(());
        }
        self.push()
    }

    fn volatile(&self) -> bool {
        false
    }

    /// 6600 K, where the blackbody curve leaves every channel at full and the
    /// screen shows the monitor's own profile. Not read from the ramp: a
    /// calibrated ramp has no temperature to read.
    fn rest(&self) -> Option<Value> {
        Some(Value::Int(color::NEUTRAL_KELVIN))
    }

    /// Candlelight to the neutral point. Below 1500 K the gamma path is
    /// clamped by Windows anyway, and above 6600 K the ramp is the identity.
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

impl Drop for ColorTemp {
    fn drop(&mut self) {
        if self.magnifier {
            let _ = unsafe { MagUninitialize() };
        }
    }
}

/// The ramp as wanted and, when Windows refuses it, the strongest one it
/// accepts. Only reached past the clamp with the range lifted, or on a
/// driver stricter than the registry says.
fn push_ramp(device: &str, c: &Rgb) -> bool {
    if screen::write(device, &color::ramp_for(c)) {
        return true;
    }
    color::within_windows_range(c)
        .is_some_and(|inside| screen::write(device, &color::ramp_for(&inside)))
}

/// The colour `device` shows: its own when it is held apart, the whole
/// capability's otherwise, held inside the Windows range with every other
/// monitor when `clamp`. `None` for its own ramp and gains.
fn shown(
    whole: Option<Rgb>,
    apart: &BTreeMap<String, Rgb>,
    device: &str,
    clamp: bool,
) -> Option<Rgb> {
    let c = apart.get(device).copied().or(whole)?;
    let c = match clamp {
        true => color::within_windows_range(&c).unwrap_or(c),
        false => c,
    };
    (!neutral(&c)).then_some(c)
}

/// No tint at all: the curve leaves every channel at full. Such a monitor
/// shows its own ramp and gains, not the identity.
fn neutral(c: &Rgb) -> bool {
    c.r.min(c.g).min(c.b) >= 0.9999
}

/// The baseline as the journal holds it: every ramp, then every panel gain.
fn encode(captured: &Captured) -> String {
    let mut encoded = color::encode(&captured.ramps);
    if !captured.gains.is_empty() {
        encoded.push(';');
        encoded.push_str(&ddc::encode(&captured.gains));
    }
    encoded
}

fn baseline_ramp(captured: Option<&Captured>, device: &str) -> Ramp {
    captured
        .and_then(|c| c.ramps.iter().find(|(d, _)| d == device))
        .map(|(_, ramp)| *ramp)
        .unwrap_or_else(|| color::ramp_for(&Rgb::NEUTRAL))
}

fn gains_of<'a>(captured: Option<&'a Captured>, key: &str) -> Option<&'a Gains> {
    captured?
        .gains
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, g)| g)
}

/// Row-major 5 by 5, applied as a row vector times the matrix. Scaling the
/// diagonal tints without touching alpha or the translation row.
fn set_magnifier(c: &Rgb) -> Result<(), ActuatorError> {
    #[rustfmt::skip]
    let effect = MAGCOLOREFFECT {
        transform: [
            c.r, 0.0, 0.0, 0.0, 0.0,
            0.0, c.g, 0.0, 0.0, 0.0,
            0.0, 0.0, c.b, 0.0, 0.0,
            0.0, 0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 0.0, 0.0, 1.0,
        ],
    };
    unsafe { MagSetFullscreenColorEffect(&effect) }
        .ok()
        .map_err(|e: windows::core::Error| {
            ActuatorError::on(
                CapabilityId::new(ids::DISPLAY_COLOR_TEMP),
                format!("the magnification colour effect was refused: {e}"),
            )
        })
}

fn fullscreen_now() -> bool {
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    match unsafe { SHQueryUserNotificationState() } {
        Ok(state) => state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = r"\\.\DISPLAY1";
    const TWO: &str = r"\\.\DISPLAY2";

    fn restoration_fixture(known: bool) -> (ColorTemp, String) {
        let ramps = vec![(ONE.to_owned(), [123; 768]), (TWO.to_owned(), [456; 768])];
        let gains = vec![
            (
                ddc::key(ONE, 0),
                [ddc::Gain {
                    min: 0,
                    current: 42,
                    max: 100,
                }; 3],
            ),
            (
                ddc::key(TWO, 0),
                [ddc::Gain {
                    min: 0,
                    current: 57,
                    max: 100,
                }; 3],
            ),
        ];
        let encoded = format!("{};{}", color::encode(&ramps), ddc::encode(&gains));
        let actuator = ColorTemp {
            magnifier: false,
            gamma: true,
            captured: RefCell::new(known.then_some(Captured { ramps, gains })),
            live: RefCell::new(Live {
                touched: [ddc::key(ONE, 0)].into_iter().collect(),
                ..Live::default()
            }),
            panels: ddc::Writer::new(),
            proven: RefCell::new(BTreeSet::new()),
            memory: ddc::Memory::none(),
            whole: RefCell::new(None),
            apart: RefCell::new(BTreeMap::new()),
            unlocked: false,
        };
        (actuator, encoded)
    }

    #[test]
    fn restoration_failure_keeps_the_baseline_and_attempts_other_outputs() {
        let (actuator, encoded) = restoration_fixture(true);
        let mut ramps = Vec::new();
        let mut panels = Vec::new();
        let result = actuator.restore_using(
            &encoded,
            &both(),
            |_| panic!("no magnifier was initialized"),
            |device, ramp| {
                ramps.push((device.to_owned(), ramp[0]));
                device != ONE
            },
            |key, values| {
                panels.push((key.to_owned(), values));
                Landed::Missed
            },
        );
        let error = result.expect_err("a failed restoration must keep the journal dirty");
        assert!(error.message.contains(ONE));
        assert_eq!(ramps, vec![(ONE.to_owned(), 123), (TWO.to_owned(), 456)]);
        assert_eq!(panels, vec![(ddc::key(ONE, 0), [42; 3])]);
        assert!(actuator.captured.borrow().is_some());
        assert!(actuator.live.borrow().touched.contains(&ddc::key(ONE, 0)));

        panels.clear();
        actuator
            .restore_using(
                &encoded,
                &both(),
                |_| unreachable!(),
                |_, _| true,
                |key, values| {
                    panels.push((key.to_owned(), values));
                    Landed::Verified
                },
            )
            .unwrap();
        assert_eq!(
            panels,
            vec![(ddc::key(ONE, 0), [42; 3])],
            "retry must not touch an untouched panel"
        );
        assert!(actuator.captured.borrow().is_none());
        assert!(actuator.live.borrow().touched.is_empty());
    }

    #[test]
    fn restoration_after_a_crash_reports_every_failed_output() {
        let (actuator, encoded) = restoration_fixture(false);
        let mut panels = Vec::new();
        let error = actuator
            .restore_using(
                &encoded,
                &both(),
                |_| unreachable!(),
                |_, _| false,
                |key, _| {
                    panels.push(key.to_owned());
                    Landed::Missed
                },
            )
            .expect_err("failed recovery must not clear the journal");
        assert!(error.message.contains(ONE));
        assert!(error.message.contains(TWO));
        assert_eq!(panels, vec![ddc::key(ONE, 0), ddc::key(TWO, 0)]);
    }

    #[test]
    fn restoration_success_skips_panels_this_claim_never_changed() {
        let (actuator, encoded) = restoration_fixture(true);
        let mut panels = Vec::new();
        actuator
            .restore_using(
                &encoded,
                &both(),
                |_| unreachable!(),
                |_, _| true,
                |key, _| {
                    panels.push(key.to_owned());
                    Landed::Verified
                },
            )
            .unwrap();
        assert_eq!(panels, vec![ddc::key(ONE, 0)]);
        assert!(actuator.captured.borrow().is_none());
    }

    /// A panel that took its baseline back and could not be read is not
    /// proven: the restore fails, so the journal keeps it, and its remembered
    /// gains stay, so a capture made while it still shows the night takes
    /// them instead.
    #[test]
    fn a_restore_that_cannot_be_read_keeps_the_panel_remembered() {
        let (actuator, encoded) = restoration_fixture(true);
        let key = ddc::key(ONE, 0);
        let base = [ddc::Gain {
            min: 0,
            current: 42,
            max: 100,
        }; 3];
        let remembered = |actuator: &ColorTemp| {
            let shown = [ddc::Gain {
                min: 0,
                current: 7,
                max: 100,
            }; 3];
            ddc::currents(&actuator.memory.correct(vec![(key.clone(), shown)])[0].1)
        };
        actuator.memory.hold(&key, &base);
        actuator
            .restore_using(
                &encoded,
                &both(),
                |_| unreachable!(),
                |_, _| true,
                |_, _| Landed::Unread,
            )
            .expect_err("not read back is not restored");
        assert_eq!(remembered(&actuator), [42; 3]);

        let (actuator, encoded) = restoration_fixture(true);
        actuator.memory.hold(&key, &base);
        actuator
            .restore_using(
                &encoded,
                &both(),
                |_| unreachable!(),
                |_, _| true,
                |_, _| Landed::Verified,
            )
            .unwrap();
        assert_eq!(remembered(&actuator), [7; 3], "read back: forgotten");
    }

    /// The monitor that carried the baseline is switched off. Nothing can be
    /// written to it and nothing needs to be: the restore has to succeed, or
    /// the journal stays dirty for a screen that is no longer there.
    #[test]
    fn a_monitor_that_is_gone_is_not_a_failed_restore() {
        let (actuator, encoded) = restoration_fixture(true);
        let mut written = Vec::new();
        actuator
            .restore_using(
                &encoded,
                &[TWO.to_owned()],
                |_| unreachable!(),
                |device, _| {
                    written.push(device.to_owned());
                    device == TWO
                },
                |_, _| Landed::Missed,
            )
            .expect("a monitor that left is not a failure");
        // Both were attempted: a device can come back under the same name
        // between the enumeration and the write.
        assert_eq!(written, vec![ONE.to_owned(), TWO.to_owned()]);
        assert!(actuator.captured.borrow().is_none());
    }

    /// Every monitor the fixture describes, as the machine would report them.
    fn both() -> Vec<String> {
        vec![ONE.to_owned(), TWO.to_owned()]
    }

    fn outputs(hdr: &[bool]) -> Vec<(String, bool)> {
        [ONE, TWO]
            .iter()
            .zip(hdr)
            .map(|(d, on)| (d.to_string(), *on))
            .collect()
    }

    fn panels(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    /// The ramp tints the cursor, the magnifier does not. The magnifier is
    /// still allowed, as the fallback for a machine that refuses every ramp.
    #[test]
    fn an_sdr_desktop_goes_through_its_ramps_with_the_magnifier_in_reserve() {
        let p = plan(&outputs(&[false, false]), &panels(&[]), true, false);
        assert_eq!(p.gamma, vec![ONE, TWO]);
        assert!(p.panel.is_empty());
        assert!(p.magnifier, "the fallback is allowed on an SDR desktop");
    }

    #[test]
    fn the_fallback_is_not_allowed_where_the_magnifier_does_not_apply() {
        let fullscreen = plan(&outputs(&[false, false]), &panels(&[]), true, true);
        assert!(!fullscreen.magnifier);
        let hdr = plan(&outputs(&[false, true]), &panels(&[]), true, false);
        assert!(!hdr.magnifier);
        let none = plan(&outputs(&[false, false]), &panels(&[]), false, false);
        assert!(!none.magnifier);
    }

    #[test]
    fn exclusive_fullscreen_moves_every_monitor_to_its_ramp() {
        let p = plan(
            &outputs(&[false, false]),
            &panels(&[r"\\.\DISPLAY1#0"]),
            true,
            true,
        );
        assert!(!p.magnifier);
        assert_eq!(p.gamma, vec![ONE, TWO]);
        assert!(p.panel.is_empty(), "no HDR, so no panel: {p:?}");
    }

    #[test]
    fn an_hdr_output_with_a_known_gain_is_tinted_by_its_panel() {
        let p = plan(
            &outputs(&[true, false]),
            &panels(&[r"\\.\DISPLAY1#0", r"\\.\DISPLAY2#0"]),
            true,
            false,
        );
        assert!(!p.magnifier, "the magnifier covers every monitor at once");
        assert_eq!(p.panel, vec![r"\\.\DISPLAY1#0"]);
        assert_eq!(p.gamma, vec![TWO]);
    }

    /// HDR switched on after the capture, on a panel that did not answer:
    /// the ramp is the only thing left and it is better than nothing.
    #[test]
    fn an_hdr_output_without_a_gain_falls_back_to_its_ramp() {
        let p = plan(&outputs(&[true, false]), &panels(&[]), true, false);
        assert_eq!(p.gamma, vec![ONE, TWO]);
        assert!(p.panel.is_empty());
    }

    #[test]
    fn a_mirrored_output_tints_every_panel_behind_it() {
        let p = plan(
            &outputs(&[true, true]),
            &panels(&[r"\\.\DISPLAY1#0", r"\\.\DISPLAY1#1"]),
            false,
            false,
        );
        assert_eq!(p.panel, vec![r"\\.\DISPLAY1#0", r"\\.\DISPLAY1#1"]);
        assert_eq!(p.gamma, vec![TWO]);
    }

    #[test]
    fn without_a_magnifier_an_sdr_desktop_uses_the_ramps() {
        let p = plan(&outputs(&[false, false]), &panels(&[]), false, false);
        assert!(!p.magnifier);
        assert_eq!(p.gamma, vec![ONE, TWO]);
    }

    /// `DISPLAY1` is a prefix of `DISPLAY10`. A panel key must match its own
    /// device exactly or the tenth monitor's panel tints the first.
    #[test]
    fn a_device_name_prefix_does_not_claim_another_devices_panel() {
        let outs = vec![
            (ONE.to_string(), true),
            (r"\\.\DISPLAY10".to_string(), false),
        ];
        let p = plan(&outs, &panels(&[r"\\.\DISPLAY10#0"]), false, false);
        assert!(p.panel.is_empty(), "{p:?}");
        assert_eq!(p.gamma.len(), 2);
    }

    /// The night on every monitor but the one a game fills, which shows its
    /// own ramp. Every warm one gets the same colour, clamped or not.
    #[test]
    fn a_monitor_held_apart_shows_its_own_colours() {
        let warm = color::from_kelvin(3000.0);
        let night = Some(warm);
        let apart: BTreeMap<String, Rgb> = [(
            TWO.to_owned(),
            color::from_kelvin(color::NEUTRAL_KELVIN as f32),
        )]
        .into();
        assert_eq!(shown(night, &apart, TWO, true), None);
        let one = shown(night, &apart, ONE, true).unwrap();
        assert_eq!(Some(one), color::within_windows_range(&warm));
        assert_eq!(shown(night, &apart, ONE, false), night);
        // Nothing on the whole: only the monitor held apart is tinted.
        let warm: BTreeMap<String, Rgb> = [(TWO.to_owned(), color::from_kelvin(4000.0))].into();
        assert_eq!(shown(None, &warm, ONE, false), None);
        assert_eq!(
            shown(None, &warm, TWO, false),
            Some(color::from_kelvin(4000.0))
        );
    }

    #[test]
    fn six_thousand_six_hundred_is_no_tint_and_six_thousand_five_hundred_is() {
        assert!(neutral(&color::from_kelvin(color::NEUTRAL_KELVIN as f32)));
        assert!(!neutral(&color::from_kelvin(6500.0)));
    }
}
