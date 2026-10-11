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
/// 3400 K: at 3200 K blue sits at 0.48 of full scale, the floor at 0.5. The
/// strongest ramp Windows accepts is this one, and it is what goes out when
/// the wanted one is refused. `None` when the ramp is already inside.
pub fn within_windows_range(ramp: &Ramp) -> Option<Ramp> {
    let mut out = *ramp;
    let mut changed = false;
    for (n, entry) in out.iter_mut().enumerate() {
        let i = (n % 256) as i32;
        let floor = ((i - 128).max(0) * 256) as u16;
        let ceiling = ((i + 128).min(255) * 256 + 255) as u16;
        let clamped = (*entry).clamp(floor, ceiling);
        if clamped != *entry {
            *entry = clamped;
            changed = true;
        }
    }
    changed.then_some(out)
}

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

    /// The Windows floor: 3400 K passes untouched, 3200 K loses a little
    /// blue at the top of the ramp and nothing else, 2500 K loses a lot.
    #[test]
    fn the_windows_floor_bites_on_blue_below_3400_kelvin() {
        assert!(within_windows_range(&ramp_for(&Rgb::NEUTRAL)).is_none());
        assert!(within_windows_range(&ramp_for(&from_kelvin(3400.0))).is_none());

        let wanted = ramp_for(&from_kelvin(3200.0));
        let sent = within_windows_range(&wanted).expect("3200 K is below the floor");
        assert_eq!(&sent[..512], &wanted[..512], "red and green are untouched");
        assert_eq!(sent[512 + 255], 127 * 256);
        assert_eq!(
            sent[512 + 128],
            wanted[512 + 128],
            "the floor is zero there"
        );

        let deep = within_windows_range(&ramp_for(&from_kelvin(2500.0))).unwrap();
        assert_eq!(deep[512 + 255], 127 * 256);
        assert!(deep[512 + 200] > ramp_for(&from_kelvin(2500.0))[512 + 200]);
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
