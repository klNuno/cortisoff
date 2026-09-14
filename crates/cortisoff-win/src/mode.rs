//! `display.mode` on Windows, through `ChangeDisplaySettingsEx`.
//!
//! Resolution and refresh rate, per output. A rule writes `set.display.mode =
//! "@240"` to change refresh only, or `"2560x1440@240"` to change both, and
//! the core has already turned either into a [`Value::Mode`] before this file
//! sees it.
//!
//! Two things make this less obvious than it looks.
//!
//! `DEVMODEW::dmDisplayFrequency` is an integer, so Windows reports 59.94 Hz as
//! 59 and 143.98 Hz as 143. A wanted rate in millihertz is therefore matched
//! against the whole one-hertz window a mode could be hiding in, never against
//! the truncated number. Asking for 59.94 finds the mode Windows calls 59, and
//! asking for 240 does not accidentally land on 239.
//!
//! An output that cannot do the wanted resolution is left alone rather than
//! moved to something near it. A second monitor is not a fallback surface for
//! a rule aimed at the one the game is on.

use std::mem::size_of;

use cortisoff_host::{ids, Actuator, ActuatorError, CapabilityId, Control, Value};
use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplaySettingsW, CDS_NORESET, CDS_UPDATEREGISTRY, DEVMODEW,
    DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
    ENUM_CURRENT_SETTINGS,
};

use crate::screen;

/// One graphics mode, in the terms Windows itself uses: whole hertz, because
/// that is all `DEVMODEW` carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

impl Setting {
    fn parse(text: &str) -> Option<Self> {
        let (size, hz) = text.split_once('@')?;
        let (width, height) = size.split_once('x')?;
        Some(Self {
            width: width.parse().ok()?,
            height: height.parse().ok()?,
            hz: hz.parse().ok()?,
        })
    }

    fn write(&self) -> String {
        format!("{}x{}@{}", self.width, self.height, self.hz)
    }

    /// How far this mode's refresh is from a wanted rate in millihertz.
    ///
    /// The mode's real rate is somewhere in `[hz, hz + 1)` because Windows
    /// truncated it, so anything inside that window is an exact match and
    /// nothing outside it is free.
    fn refresh_distance(&self, wanted_mhz: u32) -> u32 {
        let low = self.hz * 1000;
        let high = low + 999;
        if wanted_mhz < low {
            low - wanted_mhz
        } else {
            wanted_mhz.saturating_sub(high)
        }
    }
}

pub struct DisplayMode {
    /// Resolved once, at startup, the same way every other actuator here keys
    /// its journal entries.
    devices: Vec<String>,
}

impl DisplayMode {
    pub fn new() -> Option<Self> {
        let devices: Vec<String> = screen::device_names()
            .into_iter()
            .filter(|d| current(d).is_some())
            .collect();
        (!devices.is_empty()).then_some(Self { devices })
    }

    pub fn backend_name(&self) -> &'static str {
        "changedisplaysettings"
    }

    fn encode(&self) -> Option<String> {
        let mut out = String::new();
        for device in &self.devices {
            let now = current(device)?;
            out.push_str(&format!("{device}={};", now.write()));
        }
        (!out.is_empty()).then_some(out)
    }

    /// Every output that has something to change, and the mode to put it in.
    /// An output already in the right mode is absent, so a redundant apply
    /// costs no screen flash at all.
    fn plan(&self, want: impl Fn(&str, Setting) -> Option<Setting>) -> Vec<(&str, Setting)> {
        let mut changes = Vec::new();
        for device in &self.devices {
            let Some(now) = current(device) else { continue };
            let Some(target) = want(device, now) else {
                continue;
            };
            if target != now {
                changes.push((device.as_str(), target));
            }
        }
        changes
    }
}

impl Actuator for DisplayMode {
    fn id(&self) -> CapabilityId {
        CapabilityId::new(ids::DISPLAY_MODE)
    }

    fn capture(&self) -> Result<Value, ActuatorError> {
        self.encode()
            .map(Value::Text)
            .ok_or_else(|| ActuatorError::on(self.id(), "no output would report its display mode"))
    }

    fn apply(&self, value: &Value) -> Result<(), ActuatorError> {
        let changes = match value {
            Value::Mode {
                width,
                height,
                refresh_mhz,
            } => self.plan(|device, now| {
                let (w, h) = if *width == 0 || *height == 0 {
                    (now.width, now.height)
                } else {
                    (*width, *height)
                };
                let modes: Vec<Setting> = supported(device)
                    .into_iter()
                    .filter(|m| m.width == w && m.height == h)
                    .collect();
                if *refresh_mhz == 0 {
                    // Resolution only. Keep the rate the user is on if the new
                    // resolution offers it, otherwise take the fastest.
                    return modes
                        .iter()
                        .find(|m| m.hz == now.hz)
                        .copied()
                        .or_else(|| modes.iter().copied().max_by_key(|m| m.hz));
                }
                modes
                    .into_iter()
                    .min_by_key(|m| (m.refresh_distance(*refresh_mhz), u32::MAX - m.hz))
            }),
            // A captured baseline coming back, from a release or from the
            // journal after a crash.
            Value::Text(encoded) => {
                let wanted = decode(encoded);
                self.plan(|device, _| {
                    wanted
                        .iter()
                        .find(|(name, _)| name == device)
                        .map(|(_, mode)| *mode)
                })
            }
            other => {
                return Err(ActuatorError::on(
                    self.id(),
                    format!("expected a display mode, got {other:?}"),
                ))
            }
        };

        commit(&changes).map_err(|e| ActuatorError::on(self.id(), e))
    }

    /// The mode outlives the process, and a crash at 720p is the worst thing
    /// this daemon could leave behind. The journal is the only way back.
    fn volatile(&self) -> bool {
        false
    }

    fn control(&self) -> Control {
        Control::Text {
            hint: "2560x1440@240 or @240".to_owned(),
        }
    }
}

/// `name=WxH@hz;name=WxH@hz`. An entry that does not parse is dropped rather
/// than fatal: a mangled journal must not stop the rest of the restore.
fn decode(encoded: &str) -> Vec<(String, Setting)> {
    encoded
        .split(';')
        .filter(|s| !s.is_empty())
        .filter_map(|entry| {
            let (device, mode) = entry.rsplit_once('=')?;
            Some((device.to_owned(), Setting::parse(mode)?))
        })
        .collect()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn blank() -> DEVMODEW {
    DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    }
}

pub fn current(device: &str) -> Option<Setting> {
    let name = wide(device);
    let mut mode = blank();
    unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode) }
        .as_bool()
        .then_some(Setting {
            width: mode.dmPelsWidth,
            height: mode.dmPelsHeight,
            hz: mode.dmDisplayFrequency,
        })
}

/// Every mode this output offers at 32 bits per pixel. Nothing else is worth
/// switching to on a machine made after 2005, and letting an 8 bit mode into
/// the candidate list is how a rule ends up picking one.
pub fn supported(device: &str) -> Vec<Setting> {
    let name = wide(device);
    let mut out = Vec::new();
    for index in 0.. {
        let mut mode = blank();
        let ok = unsafe {
            EnumDisplaySettingsW(
                PCWSTR(name.as_ptr()),
                windows::Win32::Graphics::Gdi::ENUM_DISPLAY_SETTINGS_MODE(index),
                &mut mode,
            )
        };
        if !ok.as_bool() {
            break;
        }
        // 0 and 1 are the two "use the driver default" values, and neither is
        // a rate anything can be matched against.
        if mode.dmBitsPerPel != 32 || mode.dmDisplayFrequency <= 1 {
            continue;
        }
        let setting = Setting {
            width: mode.dmPelsWidth,
            height: mode.dmPelsHeight,
            hz: mode.dmDisplayFrequency,
        };
        if !out.contains(&setting) {
            out.push(setting);
        }
    }
    out
}

/// Every change staged with `CDS_NORESET`, then one commit.
///
/// Applying them one at a time works and costs one black screen per output.
/// Staging them costs one for the whole batch, which is the difference between
/// a rule firing on a game launch being invisible and being a nuisance.
fn commit(changes: &[(&str, Setting)]) -> Result<(), String> {
    if changes.is_empty() {
        return Ok(());
    }

    let mut failures = Vec::new();
    let mut staged = 0;
    for (device, target) in changes {
        let name = wide(device);
        let mut mode = blank();
        mode.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
        mode.dmPelsWidth = target.width;
        mode.dmPelsHeight = target.height;
        mode.dmDisplayFrequency = target.hz;

        let result = unsafe {
            ChangeDisplaySettingsExW(
                PCWSTR(name.as_ptr()),
                Some(&mode),
                None,
                CDS_UPDATEREGISTRY | CDS_NORESET,
                None,
            )
        };
        if result == DISP_CHANGE_SUCCESSFUL {
            staged += 1;
        } else {
            failures.push(format!("{device}: {}", describe(result.0)));
        }
    }

    if staged > 0 {
        // Null device and null mode: apply everything staged above.
        let result = unsafe {
            ChangeDisplaySettingsExW(
                PCWSTR::null(),
                None,
                None,
                windows::Win32::Graphics::Gdi::CDS_TYPE(0),
                None,
            )
        };
        if result != DISP_CHANGE_SUCCESSFUL {
            failures.push(format!(
                "the batch would not commit: {}",
                describe(result.0)
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join(", "))
    }
}

/// The codes worth a sentence. Anything else is reported as a number rather
/// than guessed at.
fn describe(code: i32) -> String {
    match code {
        -1 => "the graphics mode is not supported".to_owned(),
        -2 => "the mode needs a restart, which cortisoff will not ask for".to_owned(),
        -3 => "writing the mode to the registry failed".to_owned(),
        -4 => "the driver refused the mode".to_owned(),
        -5 => "another display change is already running".to_owned(),
        -6 => "the mode does not exist on this output".to_owned(),
        other => format!("ChangeDisplaySettingsEx returned {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hz: u32) -> Setting {
        Setting {
            width: 2560,
            height: 1440,
            hz,
        }
    }

    /// The whole reason refresh is carried in millihertz. Windows truncates
    /// 59.94 to 59, so a wanted 59_940 has to land on the mode called 59 and
    /// not on the one called 60.
    #[test]
    fn a_truncated_rate_matches_the_mode_it_was_truncated_from() {
        assert_eq!(at(59).refresh_distance(59_940), 0);
        assert_eq!(at(60).refresh_distance(59_940), 60);
        let modes = [at(59), at(60)];
        let best = modes
            .iter()
            .min_by_key(|m| m.refresh_distance(59_940))
            .unwrap();
        assert_eq!(best.hz, 59);
    }

    /// A round number must not drift downwards into the mode below it.
    #[test]
    fn a_whole_rate_matches_its_own_mode() {
        assert_eq!(at(240).refresh_distance(240_000), 0);
        assert_eq!(at(239).refresh_distance(240_000), 1);
        // 144 Hz could really be 144.999, so the gap is measured from the top of
        // its window, not from the truncated number.
        assert_eq!(at(144).refresh_distance(240_000), 95_001);
    }

    /// An output with nothing near the wanted rate still has a nearest, and
    /// the tie-break takes the faster of two equally near ones.
    #[test]
    fn the_nearest_mode_wins_and_ties_go_to_the_faster_one() {
        let modes = [at(60), at(144), at(120)];
        let best = modes
            .iter()
            .min_by_key(|m| (m.refresh_distance(132_000), u32::MAX - m.hz))
            .unwrap();
        // 120 is 11_001 below, 144 is 12_000 above, so 120 wins on distance.
        assert_eq!(best.hz, 120);
    }

    #[test]
    fn a_baseline_survives_being_written_and_read_back() {
        let encoded = r"\\.\DISPLAY1=2560x1440@240;\\.\DISPLAY2=1920x1080@60;";
        let decoded = decode(encoded);
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].0, r"\\.\DISPLAY1");
        assert_eq!(decoded[0].1, at(240));
        assert_eq!(decoded[1].1.height, 1080);
    }

    /// A device name holds no `=`, but the mode never does either, so the
    /// split has to come from the right to survive anything odd in a name.
    #[test]
    fn a_mangled_entry_is_dropped_and_the_rest_still_restores() {
        let decoded = decode(r"\\.\DISPLAY1=nonsense;\\.\DISPLAY2=1920x1080@60;");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, r"\\.\DISPLAY2");
    }
}
