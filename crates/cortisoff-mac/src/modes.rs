//! Choosing a display mode, and the baseline encoding, with no system call.
//!
//! macOS lists modes in points, the "looks like" size System Settings shows,
//! and several modes can share one: a HiDPI mode and a low resolution one
//! both called 1440x900, at the same rate. The point size is what a rule
//! means by a resolution; the pixel size and the IOKit mode id are what tell
//! apart the modes that share it.

/// One mode as CoreGraphics reports it. Refresh is in millihertz because
/// 59.94 Hz exists, and zero means the panel did not say, which built-in
/// displays often do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    pub width: u32,
    pub height: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub refresh_mhz: u32,
    pub io_id: i32,
}

impl Setting {
    fn same_pixels(&self, other: &Setting) -> bool {
        self.pixel_width == other.pixel_width && self.pixel_height == other.pixel_height
    }

    fn same_size(&self, other: &Setting) -> bool {
        self.width == other.width && self.height == other.height
    }
}

/// The mode to switch to for a rule asking for `width`x`height` at
/// `refresh_mhz`, as an index into `modes`. A zero size keeps the current
/// one, a zero rate keeps the current rate when the size offers it and takes
/// the fastest otherwise. `None` when no mode has that size: a display is
/// left alone rather than moved to something near what was asked.
pub fn pick(
    modes: &[Setting],
    now: &Setting,
    width: u32,
    height: u32,
    refresh_mhz: u32,
) -> Option<usize> {
    let (w, h) = if width == 0 || height == 0 {
        (now.width, now.height)
    } else {
        (width, height)
    };
    let candidates = modes
        .iter()
        .enumerate()
        .filter(|(_, m)| m.width == w && m.height == h);
    // Among modes that tie, the one with the current pixel density wins, so
    // a refresh change never quietly drops a Retina display to low
    // resolution.
    let other_pixels = |m: &Setting| !m.same_pixels(now);
    if refresh_mhz == 0 {
        return candidates
            .min_by_key(|(_, m)| {
                (
                    m.refresh_mhz != now.refresh_mhz,
                    u32::MAX - m.refresh_mhz,
                    other_pixels(m),
                )
            })
            .map(|(i, _)| i);
    }
    candidates
        .min_by_key(|(_, m)| {
            (
                m.refresh_mhz.abs_diff(refresh_mhz),
                u32::MAX - m.refresh_mhz,
                other_pixels(m),
            )
        })
        .map(|(i, _)| i)
}

/// The mode a captured baseline names, as an index into `modes`: the same
/// IOKit mode when it is still offered, otherwise the same size, pixels and
/// rate, otherwise the same size and rate.
pub fn find_saved(modes: &[Setting], saved: &Setting) -> Option<usize> {
    modes
        .iter()
        .position(|m| m == saved)
        .or_else(|| {
            modes.iter().position(|m| {
                m.same_size(saved) && m.same_pixels(saved) && m.refresh_mhz == saved.refresh_mhz
            })
        })
        .or_else(|| {
            modes
                .iter()
                .position(|m| m.same_size(saved) && m.refresh_mhz == saved.refresh_mhz)
        })
}

/// `display=WxH@mhz/PWxPH#io;`, one entry per display.
pub fn encode(entries: &[(u32, Setting)]) -> String {
    entries
        .iter()
        .map(|(display, s)| {
            format!(
                "{display}={}x{}@{}/{}x{}#{};",
                s.width, s.height, s.refresh_mhz, s.pixel_width, s.pixel_height, s.io_id
            )
        })
        .collect()
}

/// The entries that parse. A mangled one is dropped rather than fatal, so a
/// damaged journal still restores what it can.
pub fn decode(encoded: &str) -> Vec<(u32, Setting)> {
    encoded
        .split(';')
        .filter(|e| !e.is_empty())
        .filter_map(decode_one)
        .collect()
}

fn decode_one(entry: &str) -> Option<(u32, Setting)> {
    let (display, rest) = entry.split_once('=')?;
    let (size, rest) = rest.split_once('@')?;
    let (refresh, rest) = rest.split_once('/')?;
    let (pixels, io) = rest.split_once('#')?;
    let (width, height) = size.split_once('x')?;
    let (pixel_width, pixel_height) = pixels.split_once('x')?;
    Some((
        display.parse().ok()?,
        Setting {
            width: width.parse().ok()?,
            height: height.parse().ok()?,
            pixel_width: pixel_width.parse().ok()?,
            pixel_height: pixel_height.parse().ok()?,
            refresh_mhz: refresh.parse().ok()?,
            io_id: io.parse().ok()?,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, scale: u32, refresh_mhz: u32, io_id: i32) -> Setting {
        Setting {
            width,
            height,
            pixel_width: width * scale,
            pixel_height: height * scale,
            refresh_mhz,
            io_id,
        }
    }

    fn table() -> Vec<Setting> {
        vec![
            mode(2560, 1440, 1, 60_000, 1),
            mode(2560, 1440, 1, 144_000, 2),
            mode(2560, 1440, 1, 240_000, 3),
            mode(1280, 720, 2, 60_000, 4),
            mode(1280, 720, 2, 240_000, 5),
            mode(1280, 720, 1, 240_000, 6),
            mode(1920, 1080, 1, 59_940, 7),
            mode(1920, 1080, 1, 60_000, 8),
        ]
    }

    #[test]
    fn a_rate_alone_keeps_the_size() {
        let modes = table();
        let now = modes[0];
        assert_eq!(pick(&modes, &now, 0, 0, 240_000), Some(2));
        assert_eq!(pick(&modes, &now, 0, 0, 144_000), Some(1));
    }

    /// 59.94 and 60 are both offered and each must find its own.
    #[test]
    fn a_fractional_rate_finds_its_own_mode() {
        let modes = table();
        let now = modes[0];
        assert_eq!(pick(&modes, &now, 1920, 1080, 59_940), Some(6));
        assert_eq!(pick(&modes, &now, 1920, 1080, 60_000), Some(7));
    }

    #[test]
    fn the_nearest_rate_wins_and_a_tie_goes_to_the_faster() {
        let modes = table();
        let now = modes[0];
        assert_eq!(pick(&modes, &now, 2560, 1440, 100_000), Some(0));
        assert_eq!(pick(&modes, &now, 2560, 1440, 130_000), Some(1));
        assert_eq!(pick(&modes, &now, 2560, 1440, 102_000), Some(1), "tie");
        assert_eq!(pick(&modes, &now, 2560, 1440, 192_000), Some(2), "tie");
    }

    /// A refresh change on a Retina mode stays Retina, even though a low
    /// resolution mode of the same size and rate is listed first.
    #[test]
    fn a_change_keeps_the_pixel_density() {
        let mut modes = table();
        modes.swap(4, 5);
        let now = mode(1280, 720, 2, 60_000, 4);
        let picked = pick(&modes, &now, 0, 0, 240_000).unwrap();
        assert_eq!(modes[picked].io_id, 5);
    }

    #[test]
    fn a_size_alone_keeps_the_rate_or_takes_the_fastest() {
        let modes = table();
        let now = modes[1];
        assert_eq!(pick(&modes, &now, 2560, 1440, 0), Some(1));
        let now = mode(3840, 2160, 1, 120_000, 99);
        let picked = pick(&modes, &now, 1920, 1080, 0).unwrap();
        assert_eq!(modes[picked].refresh_mhz, 60_000);
    }

    #[test]
    fn a_size_the_display_lacks_leaves_it_alone() {
        let modes = table();
        assert_eq!(pick(&modes, &modes[0], 800, 600, 60_000), None);
    }

    /// Built-in panels report a rate of zero; asking for 60 Hz there still
    /// lands on the mode rather than nowhere.
    #[test]
    fn a_panel_with_no_reported_rate_is_still_reachable() {
        let modes = vec![mode(1512, 982, 2, 0, 10), mode(1800, 1169, 2, 0, 11)];
        assert_eq!(pick(&modes, &modes[0], 1800, 1169, 60_000), Some(1));
    }

    #[test]
    fn a_saved_mode_is_found_by_id_then_by_shape() {
        let modes = table();
        assert_eq!(find_saved(&modes, &modes[4]), Some(4));
        let mut moved = modes[4];
        moved.io_id = 1234;
        assert_eq!(find_saved(&modes, &moved), Some(4));
        let mut rescaled = modes[0];
        rescaled.io_id = 77;
        rescaled.pixel_width = 1;
        assert_eq!(find_saved(&modes, &rescaled), Some(0));
        assert_eq!(find_saved(&modes, &mode(640, 480, 1, 60_000, 0)), None);
    }

    #[test]
    fn a_baseline_survives_being_written_and_read_back() {
        let entries = vec![(1, table()[4]), (69_733_378, table()[6])];
        let encoded = encode(&entries);
        assert_eq!(decode(&encoded), entries);
    }

    #[test]
    fn a_mangled_entry_is_dropped_and_the_rest_still_restores() {
        let good = encode(&[(2, table()[0])]);
        let decoded = decode(&format!("1=nonsense;{good}3=1x1@x/1x1#1;"));
        assert_eq!(decoded, vec![(2, table()[0])]);
    }
}
