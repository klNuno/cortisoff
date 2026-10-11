//! Kelvin to per-channel multipliers, ramps of any size, and the baseline
//! encoding.
//!
//! The kelvin curve is the same function as `cortisoff-win/src/color.rs`,
//! copied rather than shared because the host crate holds no maths and the
//! two platform crates never depend on each other. A RandR CRTC reports its
//! own gamma size, 256 on most drivers and 1024 or 4096 on some, so ramps
//! here are vectors, not the fixed 768 entries Windows uses.

/// Per-channel multiplier, peak channel always 1.0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

/// The temperature at which the curve below is the identity: every channel at
/// full, so the ramp is left exactly as the monitor's profile set it. Where
/// a colour claim rests, and where a fade with no `from` starts.
pub const NEUTRAL_KELVIN: i64 = 6600;

/// Tanner Helland's blackbody approximation.
pub fn from_kelvin(kelvin: f32) -> Rgb {
    let t = kelvin.clamp(1000.0, 40000.0) / 100.0;

    let r = if t <= 66.0 {
        255.0
    } else {
        329.698_73 * (t - 60.0).powf(-0.133_204_76)
    };
    let g = if t <= 66.0 {
        99.470_8 * t.ln() - 161.119_57
    } else {
        288.122_16 * (t - 60.0).powf(-0.075_514_85)
    };
    let b = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_73 * (t - 10.0).ln() - 305.044_8
    };

    let (r, g, b) = (
        r.clamp(0.0, 255.0),
        g.clamp(0.0, 255.0),
        b.clamp(0.0, 255.0),
    );

    // Normalised on the peak channel: a colour temperature tints, it never
    // dims. Brightness is a different capability and a different claim.
    let peak = r.max(g).max(b).max(1.0);
    Rgb {
        r: r / peak,
        g: g / peak,
        b: b / peak,
    }
}

/// One CRTC's ramp: red, then green, then blue, `size` entries each.
pub type Ramp = Vec<u16>;

/// The ramp a colour temperature produces at a given gamma size.
pub fn ramp_for(c: &Rgb, size: usize) -> Ramp {
    let mut ramp = vec![0u16; size * 3];
    let top = size.saturating_sub(1).max(1) as f32;
    for i in 0..size {
        let base = i as f32 / top;
        let v = |m: f32| ((base * m * 65535.0).round() as u32).min(65535) as u16;
        ramp[i] = v(c.r);
        ramp[size + i] = v(c.g);
        ramp[2 * size + i] = v(c.b);
    }
    ramp
}

/// The same curve at another size, each channel resampled on its own.
///
/// A CRTC can come back from a mode change, or a driver update, with a
/// different gamma size. Its captured ramp is still the user's curve, and
/// interpolating it is closer to the truth than refusing to restore it.
pub fn resample(ramp: &[u16], size: usize) -> Ramp {
    let from = ramp.len() / 3;
    if from == size || from == 0 {
        return ramp.to_vec();
    }
    let mut out = vec![0u16; size * 3];
    for channel in 0..3 {
        let src = &ramp[channel * from..(channel + 1) * from];
        for i in 0..size {
            let pos = if size > 1 {
                i as f64 * (from - 1) as f64 / (size - 1) as f64
            } else {
                0.0
            };
            let low = pos.floor() as usize;
            let high = (low + 1).min(from - 1);
            let frac = pos - low as f64;
            let v = f64::from(src[low]) * (1.0 - frac) + f64::from(src[high]) * frac;
            out[channel * size + i] = v.round().clamp(0.0, 65535.0) as u16;
        }
    }
    out
}

/// A captured baseline, as it goes into the journal: every output's whole
/// ramp, keyed by output name so a monitor that moved to another CRTC still
/// gets its own curve back. `name=hex` per output, `;` between them, four
/// hex digits per entry, the gamma size read back from the length.
pub fn encode(baseline: &[(String, Ramp)]) -> String {
    let mut out = String::new();
    for (name, ramp) in baseline {
        if !out.is_empty() {
            out.push(';');
        }
        out.push_str(name);
        out.push('=');
        for entry in ramp {
            out.push_str(&format!("{entry:04x}"));
        }
    }
    out
}

/// An entry that does not parse is skipped and the others survive: a
/// damaged journal must not stop the rest of the restore.
pub fn decode(encoded: &str) -> Vec<(String, Ramp)> {
    let mut out = Vec::new();
    for part in encoded.split(';').filter(|p| !p.is_empty()) {
        let Some((name, hex)) = part.rsplit_once('=') else {
            continue;
        };
        if name.is_empty() || hex.is_empty() || hex.len() % 12 != 0 || !hex.is_ascii() {
            continue;
        }
        let ramp: Option<Ramp> = (0..hex.len() / 4)
            .map(|i| u16::from_str_radix(&hex[i * 4..i * 4 + 4], 16).ok())
            .collect();
        if let Some(ramp) = ramp {
            out.push((name.to_owned(), ramp));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_warm_temperature_pulls_blue_down_and_leaves_red_alone() {
        let c = from_kelvin(3400.0);
        assert_eq!(c.r, 1.0);
        assert!(c.b < c.g && c.g < c.r);
    }

    #[test]
    fn every_temperature_keeps_one_channel_at_full_scale() {
        for k in [1500.0, 3400.0, 6500.0, 9000.0, 20000.0] {
            let c = from_kelvin(k);
            let peak = c.r.max(c.g).max(c.b);
            assert!((peak - 1.0).abs() < 1e-6, "{k} peaked at {peak}");
        }
    }

    #[test]
    fn the_neutral_point_is_the_identity_at_any_size() {
        let neutral = from_kelvin(NEUTRAL_KELVIN as f32);
        for size in [256, 1024, 4096] {
            let ramp = ramp_for(&neutral, size);
            assert_eq!(ramp.len(), size * 3);
            assert_eq!(ramp[0], 0);
            assert_eq!(ramp[size - 1], 65535);
            assert_eq!(ramp[3 * size - 1], 65535);
        }
    }

    #[test]
    fn a_baseline_of_mixed_sizes_survives_a_round_trip() {
        let mut small = ramp_for(&from_kelvin(4000.0), 256);
        small[7] = 1234;
        let large = ramp_for(&from_kelvin(6600.0), 1024);
        let baseline = vec![
            ("DP-1".to_owned(), small.clone()),
            ("HDMI-A-0".to_owned(), large.clone()),
        ];
        let decoded = decode(&encode(&baseline));
        assert_eq!(decoded, baseline);
    }

    #[test]
    fn a_damaged_entry_is_skipped_and_the_others_survive() {
        let good = encode(&[("eDP-1".to_owned(), ramp_for(&from_kelvin(5000.0), 256))]);
        let decoded = decode(&format!("{good};DP-2=deadbeef;=0000;DP-3=zzzzzzzzzzzz"));
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, "eDP-1");
    }

    #[test]
    fn resampling_keeps_the_ends_and_the_shape() {
        let ramp = ramp_for(&from_kelvin(3400.0), 256);
        let big = resample(&ramp, 1024);
        assert_eq!(big.len(), 3072);
        assert_eq!(big[0], ramp[0]);
        assert_eq!(big[1023], ramp[255]);
        assert_eq!(big[2048 + 1023], ramp[512 + 255]);
        let back = resample(&big, 256);
        for (a, b) in back.iter().zip(&ramp) {
            assert!(a.abs_diff(*b) <= 1, "{a} against {b}");
        }
    }
}
