//! Kelvin to per-channel multipliers, and the baseline encoding.
//!
//! Every backend needs the same three numbers, so the maths lives here once
//! and none of them owns it.

/// Per-channel multiplier, peak channel always 1.0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Rgb {
    pub const NEUTRAL: Rgb = Rgb {
        r: 1.0,
        g: 1.0,
        b: 1.0,
    };
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

/// One monitor's gamma ramp, in the layout `SetDeviceGammaRamp` wants: red,
/// green, blue, 256 entries each.
pub type Ramp = [u16; 768];

/// The ramp a colour temperature produces, before any monitor sees it.
pub fn ramp_for(c: &Rgb) -> Ramp {
    let mut ramp = [0u16; 768];
    for i in 0..256usize {
        let base = (i as f32) / 255.0;
        let v = |m: f32| ((base * m * 65535.0).round() as u32).min(65535) as u16;
        ramp[i] = v(c.r);
        ramp[256 + i] = v(c.g);
        ramp[512 + i] = v(c.b);
    }
    ramp
}

/// Windows refuses a ramp whose entry at index `i` strays more than 128 steps
/// from the identity, `(i - 128) * 256` to `(i + 128) * 256`, unless the
/// `GdiIcmGammaRange` value has lifted the clamp. A tint never goes above the
/// identity, so only the floor bites, and it bites on blue below about
/// 3250 K. The tightest index is the last one, so a straight channel stays
/// inside as long as its gain is at least [`MIN_GAIN`].
///
/// The strongest tint Windows accepts is the wanted one mixed with white just
/// enough to bring its lowest channel up to that floor. Mixing with white
/// keeps the hue: on the blackbody curve it lands on a slightly cooler
/// temperature, 3000 K on about 3250 K. Raising blue alone to the floor, which
/// is what this did until 0.3.0, kept red and green where they were and gave
/// back blue only: at 3000 K the screen turned salmon, and the warmer the
/// setting the pinker it got, 2000 K being close to magenta. Clamping the
/// entries one by one bent the blue curve, which showed as bands moving
/// across the screen through a fade. `None` when the colour is already inside.
pub fn within_windows_range(c: &Rgb) -> Option<Rgb> {
    let low = c.r.min(c.g).min(c.b);
    if low >= MIN_GAIN {
        return None;
    }
    let t = (1.0 - MIN_GAIN) / (1.0 - low);
    let mix = |g: f32| (1.0 - t * (1.0 - g)).max(MIN_GAIN);
    Some(Rgb {
        r: mix(c.r),
        g: mix(c.g),
        b: mix(c.b),
    })
}

/// `127 * 256 / 65535`, plus enough that rounding the last entry cannot land
/// it under the floor.
pub const MIN_GAIN: f32 = 0.4962;

/// A captured baseline, as it goes into the journal.
///
/// It has to survive the process dying, so it cannot be "6500 kelvin": a
/// calibrated monitor does not sit at a neutral ramp, and restoring an
/// identity ramp would quietly destroy the user's profile. The whole ramp goes
/// in, keyed by device name so a monitor that moved is not given another
/// monitor's curve.
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

pub fn decode(encoded: &str) -> Vec<(String, Ramp)> {
    let mut out = Vec::new();
    for part in encoded.split(';').filter(|p| !p.is_empty()) {
        let Some((name, hex)) = part.split_once('=') else {
            continue;
        };
        if hex.len() != 768 * 4 {
            continue;
        }
        let mut ramp = [0u16; 768];
        let mut ok = true;
        for (i, entry) in ramp.iter_mut().enumerate() {
            match u16::from_str_radix(&hex[i * 4..i * 4 + 4], 16) {
                Ok(v) => *entry = v,
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
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

    /// A tint must never dim the screen, which is what normalising on the peak
    /// channel buys.
    #[test]
    fn every_temperature_keeps_one_channel_at_full_scale() {
        for k in [1500.0, 3400.0, 6500.0, 9000.0, 20000.0] {
            let c = from_kelvin(k);
            let peak = c.r.max(c.g).max(c.b);
            assert!((peak - 1.0).abs() < 1e-6, "{k} peaked at {peak}");
        }
    }

    /// The Windows floor: 3400 K passes untouched, a warmer colour is mixed
    /// with white until its blue reaches the floor, keeping its hue, and the
    /// ramp it makes is still a straight line that Windows' rule accepts at
    /// every index.
    #[test]
    fn the_windows_floor_keeps_the_hue_and_the_ramp_straight() {
        assert!(within_windows_range(&Rgb::NEUTRAL).is_none());
        assert!(within_windows_range(&from_kelvin(3400.0)).is_none());

        for k in [3200.0, 3000.0, 2500.0, 1900.0] {
            let wanted = from_kelvin(k);
            let sent = within_windows_range(&wanted).expect("below the floor");
            assert_eq!(sent.r, 1.0);
            assert!((sent.b - MIN_GAIN).abs() < 1e-4, "{k} K blue {}", sent.b);
            // Still warm, never pink: green stays above blue, and the share
            // each channel lost to the tint keeps its proportion.
            assert!(sent.g > sent.b, "{k} K: {sent:?}");
            let lost = |w: f32, s: f32| (1.0 - s) / (1.0 - w);
            assert!((lost(wanted.g, sent.g) - lost(wanted.b, sent.b)).abs() < 1e-3);
            let ramp = ramp_for(&sent);
            for (n, &entry) in ramp.iter().enumerate() {
                let i = (n % 256) as i32;
                let floor = ((i - 128).max(0) * 256) as u16;
                assert!(entry >= floor, "{k} K, entry {n}: {entry} < {floor}");
            }
            // Straight: every blue entry is the same gain times the identity.
            let gain = f32::from(ramp[512 + 255]) / 65535.0;
            assert!((f32::from(ramp[512 + 100]) / (100.0 * 257.0) - gain).abs() < 0.001);
        }
    }

    /// What lifting blue alone did at 2000 K: green and blue ended level,
    /// which is pink. Mixing with white keeps a clear gap between them.
    #[test]
    fn a_warm_setting_under_the_floor_is_orange_not_pink() {
        let sent = within_windows_range(&from_kelvin(2000.0)).unwrap();
        assert!(sent.g - sent.b > 0.2, "{sent:?}");
    }

    #[test]
    fn a_neutral_ramp_is_the_identity() {
        let ramp = ramp_for(&Rgb::NEUTRAL);
        assert_eq!(ramp[0], 0);
        assert_eq!(ramp[255], 65535);
        assert_eq!(ramp[256 + 128], ramp[128]);
    }

    #[test]
    fn a_baseline_survives_a_round_trip() {
        let mut ramp = ramp_for(&from_kelvin(4000.0));
        ramp[7] = 1234;
        let encoded = encode(&[(r"\\.\DISPLAY1".to_owned(), ramp)]);
        let decoded = decode(&encoded);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, r"\\.\DISPLAY1");
        assert_eq!(decoded[0].1, ramp);
    }

    /// A truncated journal entry restores the monitors it can and drops the
    /// one it cannot, rather than refusing the whole restore.
    #[test]
    fn a_damaged_entry_is_skipped_and_the_others_survive() {
        let ramp = ramp_for(&Rgb::NEUTRAL);
        let good = encode(&[("a".to_owned(), ramp)]);
        let decoded = decode(&format!("{good};b=deadbeef"));
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, "a");
    }
}
