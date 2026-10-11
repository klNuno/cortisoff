//! RandR: which CRTC lights which output, and the modes an output can take.

use x11rb::protocol::randr::{self, ConnectionExt as _, ModeFlag, ModeInfo};

use crate::x11::Display;

/// One lit CRTC, named after the first output it drives. The name, `DP-1`
/// or `eDP-1`, is what keys a baseline: CRTC ids are reassigned when a
/// monitor comes back, output names are not.
#[derive(Debug, Clone)]
pub struct Head {
    pub crtc: randr::Crtc,
    pub output: randr::Output,
    pub name: String,
    pub outputs: Vec<randr::Output>,
    pub x: i16,
    pub y: i16,
    /// As laid out on the screen, so rotated.
    pub width: u16,
    pub height: u16,
    pub mode: randr::Mode,
    pub rotation: randr::Rotation,
    /// Entries per channel in this CRTC's gamma ramp, zero when it has none.
    pub gamma_size: u16,
}

pub struct Layout {
    pub heads: Vec<Head>,
    pub modes: Vec<ModeInfo>,
    pub config_timestamp: u32,
}

/// Every lit CRTC, in the order the server lists them.
pub fn heads(x: &Display) -> Vec<Head> {
    layout(x).map(|l| l.heads).unwrap_or_default()
}

pub fn layout(x: &Display) -> Option<Layout> {
    if !x.randr {
        return None;
    }
    let conn = &x.conn;
    let res = conn
        .randr_get_screen_resources_current(x.root)
        .ok()?
        .reply()
        .ok()?;
    let stamp = res.config_timestamp;
    let mut heads = Vec::new();
    for crtc in &res.crtcs {
        let Some(info) = conn
            .randr_get_crtc_info(*crtc, stamp)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            continue;
        };
        let Some(&output) = info.outputs.first() else {
            continue;
        };
        if info.mode == x11rb::NONE {
            continue;
        }
        let name = conn
            .randr_get_output_info(output, stamp)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|o| String::from_utf8_lossy(&o.name).into_owned())
            .unwrap_or_else(|| format!("crtc-{crtc}"));
        let gamma_size = conn
            .randr_get_crtc_gamma_size(*crtc)
            .ok()
            .and_then(|c| c.reply().ok())
            .map_or(0, |g| g.size);
        heads.push(Head {
            crtc: *crtc,
            output,
            name,
            outputs: info.outputs,
            x: info.x,
            y: info.y,
            width: info.width,
            height: info.height,
            mode: info.mode,
            rotation: info.rotation,
            gamma_size,
        });
    }
    Some(Layout {
        heads,
        modes: res.modes,
        config_timestamp: stamp,
    })
}

/// The output the user marked primary, or none.
pub fn primary(x: &Display) -> Option<randr::Output> {
    x.conn
        .randr_get_output_primary(x.root)
        .ok()?
        .reply()
        .ok()
        .map(|p| p.output)
        .filter(|o| *o != x11rb::NONE)
}

/// The modes one output offers, as `(mode id, width, height, millihertz)`.
pub fn offered(x: &Display, output: randr::Output, layout: &Layout) -> Vec<Candidate> {
    let Some(info) = x
        .conn
        .randr_get_output_info(output, layout.config_timestamp)
        .ok()
        .and_then(|c| c.reply().ok())
    else {
        return Vec::new();
    };
    info.modes
        .iter()
        .filter_map(|id| layout.modes.iter().find(|m| m.id == *id))
        .map(Candidate::of)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub id: u32,
    pub width: u16,
    pub height: u16,
    pub mhz: u32,
}

impl Candidate {
    pub fn of(m: &ModeInfo) -> Self {
        let flags = u32::from(m.mode_flags);
        let has = |flag: ModeFlag| flags & u32::from(flag) != 0;
        Self {
            id: m.id,
            width: m.width,
            height: m.height,
            mhz: refresh_mhz(
                m.dot_clock,
                m.htotal,
                m.vtotal,
                has(ModeFlag::INTERLACE),
                has(ModeFlag::DOUBLE_SCAN),
            ),
        }
    }
}

/// The refresh rate a modeline produces, in millihertz, the way `xrandr`
/// computes it. RandR carries no rate, only the timings: 59.94 Hz is a pixel
/// clock that does not divide evenly, and rounding it to 60 picks the wrong
/// mode.
pub fn refresh_mhz(
    dot_clock: u32,
    htotal: u16,
    vtotal: u16,
    interlace: bool,
    double_scan: bool,
) -> u32 {
    let mut lines = u64::from(vtotal);
    if double_scan {
        lines *= 2;
    }
    let dots_per_frame = u64::from(htotal) * lines;
    if dots_per_frame == 0 {
        return 0;
    }
    let mut mhz = (u64::from(dot_clock) * 1000 + dots_per_frame / 2) / dots_per_frame;
    if interlace {
        mhz *= 2;
    }
    mhz.min(u64::from(u32::MAX)) as u32
}

/// The mode to switch to, or `None` when the output offers nothing at the
/// wanted size. A zero size keeps the current one, a zero rate keeps the
/// current rate if the size offers it and takes the fastest otherwise. A
/// wanted rate picks the nearest mode, the faster of two equally near.
pub fn pick(
    offered: &[Candidate],
    now: Candidate,
    width: u32,
    height: u32,
    mhz: u32,
) -> Option<Candidate> {
    let (w, h) = if width == 0 || height == 0 {
        (u32::from(now.width), u32::from(now.height))
    } else {
        (width, height)
    };
    let sized: Vec<&Candidate> = offered
        .iter()
        .filter(|m| u32::from(m.width) == w && u32::from(m.height) == h)
        .collect();
    if mhz == 0 {
        return sized
            .iter()
            .find(|m| m.mhz.abs_diff(now.mhz) < 500)
            .or_else(|| sized.iter().max_by_key(|m| m.mhz))
            .map(|m| **m);
    }
    sized
        .into_iter()
        .min_by_key(|m| (m.mhz.abs_diff(mhz), u32::MAX - m.mhz))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modeline_gives_its_rate_in_millihertz() {
        // CEA 1080p60 and its NTSC sibling.
        assert_eq!(refresh_mhz(148_500_000, 2200, 1125, false, false), 60_000);
        assert_eq!(refresh_mhz(148_351_648, 2200, 1125, false, false), 59_940);
        // 1080i: half the lines per field, twice the rate.
        assert_eq!(refresh_mhz(74_250_000, 2200, 1125, true, false), 60_000);
        assert_eq!(refresh_mhz(0, 0, 0, false, false), 0);
    }

    fn mode(id: u32, width: u16, height: u16, mhz: u32) -> Candidate {
        Candidate {
            id,
            width,
            height,
            mhz,
        }
    }

    fn offered() -> Vec<Candidate> {
        vec![
            mode(1, 2560, 1440, 59_951),
            mode(2, 2560, 1440, 143_973),
            mode(3, 2560, 1440, 239_970),
            mode(4, 1920, 1080, 60_000),
            mode(5, 1920, 1080, 59_940),
            mode(6, 1920, 1080, 120_000),
        ]
    }

    #[test]
    fn a_rate_finds_the_nearest_mode_at_the_current_size() {
        let now = offered()[0];
        assert_eq!(pick(&offered(), now, 0, 0, 240_000).unwrap().id, 3);
        assert_eq!(pick(&offered(), now, 0, 0, 144_000).unwrap().id, 2);
    }

    #[test]
    fn fifty_nine_point_nine_four_is_not_sixty() {
        let now = offered()[0];
        assert_eq!(pick(&offered(), now, 1920, 1080, 59_940).unwrap().id, 5);
        assert_eq!(pick(&offered(), now, 1920, 1080, 60_000).unwrap().id, 4);
    }

    #[test]
    fn a_size_alone_keeps_the_rate_or_takes_the_fastest() {
        let at_60 = mode(9, 2560, 1440, 60_000);
        assert_eq!(pick(&offered(), at_60, 1920, 1080, 0).unwrap().id, 4);
        let at_240 = offered()[2];
        assert_eq!(pick(&offered(), at_240, 1920, 1080, 0).unwrap().id, 6);
    }

    #[test]
    fn a_size_the_output_does_not_offer_changes_nothing() {
        assert_eq!(pick(&offered(), offered()[0], 3840, 2160, 60_000), None);
    }

    #[test]
    fn a_tie_goes_to_the_faster_mode() {
        let modes = [mode(1, 800, 600, 50_000), mode(2, 800, 600, 70_000)];
        assert_eq!(pick(&modes, modes[0], 0, 0, 60_000).unwrap().id, 2);
    }
}
