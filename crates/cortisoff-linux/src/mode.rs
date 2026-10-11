//! `display.mode` on X11, through RandR.
//!
//! A plugin's mode lands on the primary output, the one the user marked as
//! such, or the first lit one when none is. The baseline holds every lit
//! output, so a crash at the wrong resolution is put right whichever output
//! it hit.
//!
//! RandR has no refresh rate field, only modelines, so rates are computed
//! in millihertz from the timings and matched there: 59.94 Hz is its own
//! mode and must not be confused with 60.

use std::rc::Rc;

use cortisoff_host::{ids, Actuator, ActuatorError, CapabilityId, Control, Value};
use x11rb::protocol::randr::{ConnectionExt as _, Rotation, SetConfig};
use x11rb::protocol::xproto::ConnectionExt as _;

use crate::randr::{self, Candidate, Head, Layout};
use crate::x11::Display;

/// One output's mode as the journal holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    pub width: u16,
    pub height: u16,
    pub mhz: u32,
}

impl Setting {
    fn parse(text: &str) -> Option<Self> {
        let (size, mhz) = text.split_once('@')?;
        let (width, height) = size.split_once('x')?;
        Some(Self {
            width: width.parse().ok()?,
            height: height.parse().ok()?,
            mhz: mhz.parse().ok()?,
        })
    }
}

/// `name=WxH@millihertz;` per output.
pub fn encode(settings: &[(String, Setting)]) -> String {
    settings
        .iter()
        .map(|(name, s)| format!("{name}={}x{}@{};", s.width, s.height, s.mhz))
        .collect()
}

/// An entry that does not parse is dropped rather than fatal: a mangled
/// journal must not stop the rest of the restore.
pub fn decode(encoded: &str) -> Vec<(String, Setting)> {
    encoded
        .split(';')
        .filter(|s| !s.is_empty())
        .filter_map(|entry| {
            let (name, mode) = entry.rsplit_once('=')?;
            Some((name.to_owned(), Setting::parse(mode)?))
        })
        .collect()
}

pub struct DisplayMode {
    x: Rc<Display>,
}

impl DisplayMode {
    pub fn new(x: Rc<Display>) -> Result<Self, String> {
        if !x.randr {
            return Err(
                "the X server has no RandR 1.2, which is what sets a mode per monitor".to_owned(),
            );
        }
        if x.xwayland {
            return Err("this X server is XWayland: its modes are emulated for X11 games and never reach the monitor".to_owned());
        }
        if randr::heads(&x).is_empty() {
            return Err("no lit monitor reports a mode through RandR".to_owned());
        }
        Ok(Self { x })
    }

    pub fn backend_name(&self) -> &'static str {
        "randr"
    }

    fn current(layout: &Layout, head: &Head) -> Option<Candidate> {
        layout
            .modes
            .iter()
            .find(|m| m.id == head.mode)
            .map(Candidate::of)
    }

    /// The head a plugin's mode is meant for.
    fn target<'a>(&self, layout: &'a Layout) -> Option<&'a Head> {
        let primary = randr::primary(&self.x);
        layout
            .heads
            .iter()
            .find(|h| Some(h.output) == primary || h.outputs.iter().any(|o| Some(*o) == primary))
            .or_else(|| layout.heads.first())
    }

    /// Switch one head, growing the screen first when the new mode does not
    /// fit in it and shrinking it after when the layout got smaller, which
    /// is the order the server accepts.
    fn switch(&self, layout: &Layout, head: &Head, to: Candidate) -> Result<(), String> {
        let conn = &self.x.conn;
        let rotation = u16::from(head.rotation);
        let turned =
            rotation & (u16::from(Rotation::ROTATE90) | u16::from(Rotation::ROTATE270)) != 0;
        let (w, h) = if turned {
            (to.height, to.width)
        } else {
            (to.width, to.height)
        };
        let reach = |x: i16, len: u16| (i32::from(x) + i32::from(len)).max(0) as u32;
        let mut right = reach(head.x, w);
        let mut bottom = reach(head.y, h);
        for other in layout.heads.iter().filter(|o| o.crtc != head.crtc) {
            right = right.max(reach(other.x, other.width));
            bottom = bottom.max(reach(other.y, other.height));
        }
        let geometry = conn
            .get_geometry(self.x.root)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let (now_w, now_h) = (u32::from(geometry.width), u32::from(geometry.height));

        if right > now_w || bottom > now_h {
            self.resize(right.max(now_w), bottom.max(now_h))?;
        }
        let status = conn
            .randr_set_crtc_config(
                head.crtc,
                x11rb::CURRENT_TIME,
                layout.config_timestamp,
                head.x,
                head.y,
                to.id,
                head.rotation,
                &head.outputs,
            )
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| format!("{}: {e}", head.name))?
            .status;
        if status != SetConfig::SUCCESS {
            return Err(format!("{}: {}", head.name, describe(status)));
        }
        if right < now_w || bottom < now_h {
            self.resize(right, bottom)?;
        }
        Ok(())
    }

    /// Keeps the density the screen had at connect time, so text does not
    /// change size because a monitor changed mode.
    fn resize(&self, width: u32, height: u32) -> Result<(), String> {
        let (px_w, px_h) = self.x.screen_px;
        let (mm_w, mm_h) = self.x.screen_mm;
        let mm = |px: u32, ref_px: u16, ref_mm: u16| {
            if ref_px == 0 {
                px * 254 / 960
            } else {
                px * u32::from(ref_mm) / u32::from(ref_px)
            }
        };
        let (w, h) = (
            u16::try_from(width).map_err(|_| format!("a {width} pixel wide screen"))?,
            u16::try_from(height).map_err(|_| format!("a {height} pixel high screen"))?,
        );
        self.x
            .conn
            .randr_set_screen_size(
                self.x.root,
                w,
                h,
                mm(width, px_w, mm_w),
                mm(height, px_h, mm_h),
            )
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| format!("the screen would not resize to {width}x{height}: {e}"))
    }
}

fn describe(status: SetConfig) -> String {
    match status {
        SetConfig::INVALID_CONFIG_TIME => {
            "the layout changed while switching, try again".to_owned()
        }
        SetConfig::INVALID_TIME => "another client changed the mode first".to_owned(),
        SetConfig::FAILED => "the driver refused the mode".to_owned(),
        other => format!("RandR answered {}", u8::from(other)),
    }
}

impl Actuator for DisplayMode {
    fn id(&self) -> CapabilityId {
        CapabilityId::new(ids::DISPLAY_MODE)
    }

    fn capture(&self) -> Result<Value, ActuatorError> {
        let layout = randr::layout(&self.x)
            .ok_or_else(|| ActuatorError::on(self.id(), "RandR would not list the monitors"))?;
        let settings: Vec<(String, Setting)> = layout
            .heads
            .iter()
            .filter_map(|h| {
                let now = Self::current(&layout, h)?;
                Some((
                    h.name.clone(),
                    Setting {
                        width: now.width,
                        height: now.height,
                        mhz: now.mhz,
                    },
                ))
            })
            .collect();
        if settings.is_empty() {
            return Err(ActuatorError::on(
                self.id(),
                "no output would report its display mode",
            ));
        }
        Ok(Value::Text(encode(&settings)))
    }

    fn apply(&self, value: &Value) -> Result<(), ActuatorError> {
        let layout = randr::layout(&self.x)
            .ok_or_else(|| ActuatorError::on(self.id(), "RandR would not list the monitors"))?;
        let mut changes: Vec<(&Head, Candidate)> = Vec::new();
        match value {
            Value::Mode {
                width,
                height,
                refresh_mhz,
            } => {
                let Some(head) = self.target(&layout) else {
                    return Err(ActuatorError::on(self.id(), "no monitor is lit"));
                };
                let Some(now) = Self::current(&layout, head) else {
                    return Err(ActuatorError::on(
                        self.id(),
                        format!("{} reports no current mode", head.name),
                    ));
                };
                // An output that cannot do the wanted size is left alone, as
                // on Windows: a near miss is not what anyone asked for.
                let offered = randr::offered(&self.x, head.output, &layout);
                if let Some(to) = randr::pick(&offered, now, *width, *height, *refresh_mhz) {
                    if to.id != now.id {
                        changes.push((head, to));
                    }
                }
            }
            // A captured baseline coming back, from a release or from the
            // journal after a crash. An output that left is skipped.
            Value::Text(encoded) => {
                for (name, wanted) in decode(encoded) {
                    let Some(head) = layout.heads.iter().find(|h| h.name == name) else {
                        continue;
                    };
                    let Some(now) = Self::current(&layout, head) else {
                        continue;
                    };
                    let offered = randr::offered(&self.x, head.output, &layout);
                    let to = randr::pick(
                        &offered,
                        now,
                        u32::from(wanted.width),
                        u32::from(wanted.height),
                        wanted.mhz,
                    );
                    if let Some(to) = to.filter(|to| to.id != now.id) {
                        changes.push((head, to));
                    }
                }
            }
            other => {
                return Err(ActuatorError::on(
                    self.id(),
                    format!("expected a display mode, got {other:?}"),
                ))
            }
        }

        let failures: Vec<String> = changes
            .into_iter()
            .filter_map(|(head, to)| self.switch(&layout, head, to).err())
            .collect();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(ActuatorError::on(self.id(), failures.join(", ")))
        }
    }

    /// A mode outlives the process, and a crash at 720p is the worst thing
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_baseline_survives_being_written_and_read_back() {
        let settings = vec![
            (
                "DP-1".to_owned(),
                Setting {
                    width: 2560,
                    height: 1440,
                    mhz: 239_970,
                },
            ),
            (
                "HDMI-A-0".to_owned(),
                Setting {
                    width: 1920,
                    height: 1080,
                    mhz: 59_940,
                },
            ),
        ];
        let encoded = encode(&settings);
        assert_eq!(encoded, "DP-1=2560x1440@239970;HDMI-A-0=1920x1080@59940;");
        assert_eq!(decode(&encoded), settings);
    }

    #[test]
    fn a_mangled_entry_is_dropped_and_the_rest_still_restores() {
        let decoded = decode("DP-1=nonsense;eDP-1=1920x1200@60001;=;HDMI-1=1x1@");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, "eDP-1");
        assert_eq!(decoded[0].1.mhz, 60_001);
    }
}
