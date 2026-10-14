//! `display.color_temp` on GNOME, by driving GNOME's own Night Light.
//!
//! Under Wayland the compositor owns the colour of the screen: a gamma ramp
//! set through XWayland never reaches the monitor, and Mutter offers no
//! client a gamma or colour matrix. gnome-settings-daemon does own one, its
//! Night Light, and it already fades, survives a monitor change and restores
//! itself. So this backend cooperates instead of fighting it: a claim turns
//! Night Light on with a manual schedule that covers the whole day
//! (`schedule-from` equal to `schedule-to`, which the daemon reads as always
//! on, with no edge to smear) and sets its temperature. A release puts the
//! user's own five keys back exactly as they were, a key left at its default
//! reset rather than written, so `dconf` ends as it was found.
//!
//! The settings outlive the process, so the actuator is not volatile and its
//! baseline goes to the journal before the first write.
//!
//! The keys go through the `gsettings` tool, which ships with GLib on every
//! GNOME system, and `dconf read` tells a key the user set from one at its
//! default. A write costs one short process, so a fade writes only when the
//! temperature crosses a 50 K step (the control's own step): Night Light
//! smooths each change over a few seconds itself.

use std::cell::Cell;
use std::process::{Command, Stdio};

use cortisoff_host::{ids, Actuator, ActuatorError, CapabilityId, Control, Value};

use crate::dbus::{self, Buses};

const SCHEMA: &str = "org.gnome.settings-daemon.plugins.color";
const DCONF_DIR: &str = "/org/gnome/settings-daemon/plugins/color/";

const ENABLED: &str = "night-light-enabled";
const AUTOMATIC: &str = "night-light-schedule-automatic";
const FROM: &str = "night-light-schedule-from";
const TO: &str = "night-light-schedule-to";
const TEMPERATURE: &str = "night-light-temperature";

/// Restored in this order: the schedule and the temperature before the
/// switch, so Night Light turning back on never shows this backend's
/// temperature for a moment.
const KEYS: [&str; 5] = [AUTOMATIC, FROM, TO, TEMPERATURE, ENABLED];

/// gnome-settings-daemon's neutral point, and where its schema's range ends.
pub const NEUTRAL_KELVIN: i64 = 6500;
const MIN_KELVIN: i64 = 1000;
/// Writes land on this grid. The control steps by the same amount.
const STEP_KELVIN: i64 = 50;

/// One key as the baseline holds it: `None` when it sat at its default, the
/// value in GVariant text otherwise.
type Saved = (String, Option<String>);

pub struct NightLight {
    /// The temperature last written while this process holds Night Light.
    /// `None` when the user's settings are in place.
    written: Cell<Option<i64>>,
}

impl NightLight {
    pub fn new(buses: &Buses) -> Result<Self, String> {
        let session = buses.session.as_ref().map_err(Clone::clone)?;
        if !dbus::has_owner(session, "org.gnome.SettingsDaemon.Color") {
            return Err("gnome-settings-daemon's colour plugin is not on the session bus, so there is no Night Light to drive".to_owned());
        }
        get(TEMPERATURE)
            .map_err(|e| format!("GNOME's Night Light settings cannot be read: {e}"))?;
        Ok(Self {
            written: Cell::new(None),
        })
    }

    pub fn backend_name(&self) -> &'static str {
        "gnome-night-light"
    }

    fn tint(&self, kelvin: i64) -> Result<(), ActuatorError> {
        let kelvin = on_grid(kelvin);
        let fail = |e: String| ActuatorError::on(self.id(), e);
        match self.written.get() {
            Some(k) if k == kelvin => return Ok(()),
            Some(_) => set(TEMPERATURE, &format!("uint32 {kelvin}")).map_err(fail)?,
            None => {
                // Temperature first, the switch last, so the first thing
                // Night Light shows is this claim and not the user's own.
                for (key, value) in [
                    (TEMPERATURE, format!("uint32 {kelvin}")),
                    (AUTOMATIC, "false".to_owned()),
                    (FROM, "0.0".to_owned()),
                    (TO, "0.0".to_owned()),
                    (ENABLED, "true".to_owned()),
                ] {
                    set(key, &value).map_err(fail)?;
                }
            }
        }
        self.written.set(Some(kelvin));
        Ok(())
    }

    fn restore(&self, encoded: &str) -> Result<(), ActuatorError> {
        let saved = decode(encoded);
        let mut failures = Vec::new();
        for key in KEYS {
            let Some((_, value)) = saved.iter().find(|(k, _)| k == key) else {
                continue;
            };
            let done = match value {
                Some(v) => set(key, v),
                None => reset(key),
            };
            if let Err(e) = done {
                failures.push(e);
            }
        }
        if !failures.is_empty() {
            return Err(ActuatorError::on(self.id(), failures.join("; ")));
        }
        self.written.set(None);
        Ok(())
    }
}

impl Actuator for NightLight {
    fn id(&self) -> CapabilityId {
        CapabilityId::new(ids::DISPLAY_COLOR_TEMP)
    }

    /// The five keys as the user left them, each with whether it was set at
    /// all. Settings that read as this backend's own (on, manual, an empty
    /// schedule window) were left by a run whose journal is gone, and are
    /// captured as defaults rather than kept for good.
    fn capture(&self) -> Result<Value, ActuatorError> {
        let mut saved: Vec<Saved> = Vec::new();
        for key in KEYS {
            let value = get(key).map_err(|e| ActuatorError::on(self.id(), e))?;
            let user_set = user_set(key).unwrap_or(true);
            saved.push((key.to_owned(), user_set.then_some(value)));
        }
        if looks_like_ours(&saved) {
            for (_, value) in &mut saved {
                *value = None;
            }
        }
        Ok(Value::Text(encode(&saved)))
    }

    fn apply(&self, value: &Value) -> Result<(), ActuatorError> {
        match value {
            Value::Int(kelvin) => self.tint(*kelvin),
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
        Some(Value::Int(NEUTRAL_KELVIN))
    }

    fn control(&self) -> Control {
        Control::Range {
            min: 1500,
            max: NEUTRAL_KELVIN,
            step: STEP_KELVIN,
            default: 3400,
            unit: Some("K".to_owned()),
        }
    }
}

fn on_grid(kelvin: i64) -> i64 {
    let k = kelvin.clamp(MIN_KELVIN, 10_000);
    ((k + STEP_KELVIN / 2) / STEP_KELVIN) * STEP_KELVIN
}

/// `key=-` for a default, `key=+value` for a value the user set, `;` between.
fn encode(saved: &[Saved]) -> String {
    saved
        .iter()
        .map(|(key, value)| match value {
            Some(v) => format!("{key}=+{v}"),
            None => format!("{key}=-"),
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// An entry that does not parse is skipped: a damaged journal must not stop
/// the rest of the restore. Only the known keys come back.
fn decode(encoded: &str) -> Vec<Saved> {
    encoded
        .split(';')
        .filter_map(|part| {
            let (key, rest) = part.split_once('=')?;
            if !KEYS.contains(&key) {
                return None;
            }
            match rest {
                "-" => Some((key.to_owned(), None)),
                v => v
                    .strip_prefix('+')
                    .filter(|v| !v.is_empty())
                    .map(|v| (key.to_owned(), Some(v.to_owned()))),
            }
        })
        .collect()
}

fn looks_like_ours(saved: &[Saved]) -> bool {
    let value = |key: &str| {
        saved
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_deref())
    };
    value(ENABLED) == Some("true")
        && value(AUTOMATIC) == Some("false")
        && value(FROM).is_some()
        && value(FROM) == value(TO)
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("{program} is not installed"),
            _ => format!("could not start {program}: {e}"),
        })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn get(key: &str) -> Result<String, String> {
    run("gsettings", &["get", SCHEMA, key])
}

fn set(key: &str, value: &str) -> Result<(), String> {
    run("gsettings", &["set", SCHEMA, key, value]).map(|_| ())
}

fn reset(key: &str) -> Result<(), String> {
    run("gsettings", &["reset", SCHEMA, key]).map(|_| ())
}

/// Whether the user's dconf database holds this key. `None` when `dconf` is
/// not installed, and the caller then keeps the value as if it were set.
fn user_set(key: &str) -> Option<bool> {
    run("dconf", &["read", &format!("{DCONF_DIR}{key}")])
        .ok()
        .map(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(pairs: &[(&str, Option<&str>)]) -> Vec<Saved> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.map(str::to_owned)))
            .collect()
    }

    #[test]
    fn a_baseline_keeps_which_keys_were_defaults() {
        let baseline = saved(&[
            (AUTOMATIC, None),
            (FROM, Some("20.0")),
            (TO, Some("6.5")),
            (TEMPERATURE, Some("uint32 2700")),
            (ENABLED, None),
        ]);
        assert_eq!(decode(&encode(&baseline)), baseline);
    }

    #[test]
    fn a_damaged_or_unknown_entry_is_skipped() {
        let decoded = decode("night-light-enabled=+true;bogus=+1;night-light-schedule-to=+;=;night-light-temperature=-");
        assert_eq!(
            decoded,
            saved(&[(ENABLED, Some("true")), (TEMPERATURE, None)])
        );
    }

    #[test]
    fn settings_this_backend_leaves_are_not_a_baseline() {
        let ours = saved(&[
            (AUTOMATIC, Some("false")),
            (FROM, Some("0.0")),
            (TO, Some("0.0")),
            (TEMPERATURE, Some("uint32 3200")),
            (ENABLED, Some("true")),
        ]);
        assert!(looks_like_ours(&ours));
        let users = saved(&[
            (AUTOMATIC, Some("false")),
            (FROM, Some("20.0")),
            (TO, Some("7.0")),
            (TEMPERATURE, Some("uint32 3200")),
            (ENABLED, Some("true")),
        ]);
        assert!(!looks_like_ours(&users));
    }

    #[test]
    fn writes_land_on_the_fifty_kelvin_grid() {
        assert_eq!(on_grid(3200), 3200);
        assert_eq!(on_grid(3224), 3200);
        assert_eq!(on_grid(3225), 3250);
        assert_eq!(on_grid(200), MIN_KELVIN);
        assert_eq!(on_grid(6600), 6600);
    }
}
