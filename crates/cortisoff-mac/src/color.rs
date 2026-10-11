//! Kelvin to per-channel multipliers, display transfer tables, and the
//! baseline encoding. No system call, so it is tested anywhere.

/// Per-channel multiplier, peak channel always 1.0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Rgb {
    #[cfg(test)]
    pub const NEUTRAL: Rgb = Rgb {
        r: 1.0,
        g: 1.0,
        b: 1.0,
    };
}

/// The temperature at which the curve below is the identity: every channel at
/// full, so the table is left exactly as the display's profile set it. Where
/// a colour claim rests, and where a fade with no `from` starts.
pub const NEUTRAL_KELVIN: i64 = 6600;

/// Tanner Helland's blackbody approximation. The same curve as the Windows
/// host, so a plugin tints both systems alike.
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

/// One display's transfer table as CoreGraphics hands it over: three
/// channels of the same length, each entry from 0.0 to 1.0. The length is
/// the display's own, 256 on most and 1024 on some.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub red: Vec<f32>,
    pub green: Vec<f32>,
    pub blue: Vec<f32>,
}

impl Table {
    /// The straight line, which is what a display shows with no profile.
    pub fn identity(samples: usize) -> Self {
        let samples = samples.max(2);
        let line: Vec<f32> = (0..samples)
            .map(|i| i as f32 / (samples - 1) as f32)
            .collect();
        Self {
            red: line.clone(),
            green: line.clone(),
            blue: line,
        }
    }

    pub fn len(&self) -> usize {
        self.red.len()
    }

    /// Three channels of the same, non-zero length.
    pub fn is_valid(&self) -> bool {
        !self.red.is_empty()
            && self.red.len() == self.green.len()
            && self.red.len() == self.blue.len()
    }

    /// This table under a tint: each entry scaled by its channel's
    /// multiplier. Scaling the captured table rather than writing a fresh
    /// line keeps the display's calibration, which a ColorSync profile puts
    /// in exactly this table.
    pub fn tinted(&self, c: &Rgb) -> Self {
        let scale = |channel: &[f32], m: f32| -> Vec<f32> {
            channel.iter().map(|v| (v * m).clamp(0.0, 1.0)).collect()
        };
        Self {
            red: scale(&self.red, c.r),
            green: scale(&self.green, c.g),
            blue: scale(&self.blue, c.b),
        }
    }
}

/// A captured baseline, as it goes into the journal.
///
/// It has to survive the process dying, so it cannot be "6600 kelvin": a
/// calibrated display does not sit at the identity. Every entry of every
/// channel goes in, bit for bit, keyed by the display id, which CoreGraphics
/// keeps across restarts for the same panel on the same port.
/// `id=samples:hex;id=samples:hex`, eight hex digits per `f32`.
pub fn encode(baseline: &[(u32, Table)]) -> String {
    let mut out = String::new();
    for (id, table) in baseline.iter().filter(|(_, t)| t.is_valid()) {
        if !out.is_empty() {
            out.push(';');
        }
        out.push_str(&format!("{id}={}:", table.len()));
        for v in table.red.iter().chain(&table.green).chain(&table.blue) {
            out.push_str(&format!("{:08x}", v.to_bits()));
        }
    }
    out
}

/// The entries that parse. A damaged one is dropped rather than fatal, so a
/// truncated journal still puts back every display it can.
pub fn decode(encoded: &str) -> Vec<(u32, Table)> {
    encoded
        .split(';')
        .filter(|part| !part.is_empty())
        .filter_map(decode_one)
        .collect()
}

fn decode_one(part: &str) -> Option<(u32, Table)> {
    let (id, rest) = part.split_once('=')?;
    let (samples, hex) = rest.split_once(':')?;
    let id: u32 = id.parse().ok()?;
    let samples: usize = samples.parse().ok()?;
    if samples == 0 || hex.len() != samples * 3 * 8 || !hex.is_ascii() {
        return None;
    }
    let mut values = Vec::with_capacity(samples * 3);
    for i in 0..samples * 3 {
        let bits = u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok()?;
        let v = f32::from_bits(bits);
        if !v.is_finite() {
            return None;
        }
        values.push(v);
    }
    let blue = values.split_off(samples * 2);
    let green = values.split_off(samples);
    Some((
        id,
        Table {
            red: values,
            green,
            blue,
        },
    ))
}

/// What putting a baseline back came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Restored {
    /// Every display that is here took its table back.
    Done,
    /// These displays are here and refused their table.
    Failed(Vec<u32>),
    /// Nothing in the entry parses. The caller falls back to the profiles.
    Unreadable,
}

/// Writes every captured table back. A display that is not here now is not
/// a failure: it was unplugged since the capture, and counting it would keep
/// the journal dirty for a screen nobody can put back. It is still written,
/// because a display can come back between the enumeration and the write.
pub fn restore(
    encoded: &str,
    present: &[u32],
    mut write: impl FnMut(u32, &Table) -> bool,
) -> Restored {
    let entries = decode(encoded);
    if entries.is_empty() {
        return if encoded.trim().is_empty() {
            Restored::Done
        } else {
            Restored::Unreadable
        };
    }
    let failed: Vec<u32> = entries
        .iter()
        .filter(|(id, table)| !write(*id, table) && present.contains(id))
        .map(|(id, _)| *id)
        .collect();
    if failed.is_empty() {
        Restored::Done
    } else {
        Restored::Failed(failed)
    }
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

    /// A tint must never dim the screen, which is what normalising on the
    /// peak channel buys.
    #[test]
    fn every_temperature_keeps_one_channel_at_full_scale() {
        for k in [1500.0, 3400.0, 6500.0, 9000.0, 20000.0] {
            let c = from_kelvin(k);
            let peak = c.r.max(c.g).max(c.b);
            assert!((peak - 1.0).abs() < 1e-6, "{k} peaked at {peak}");
        }
    }

    #[test]
    fn the_neutral_point_leaves_a_table_alone() {
        let c = from_kelvin(NEUTRAL_KELVIN as f32);
        assert_eq!(c, Rgb::NEUTRAL);
        let table = Table::identity(256);
        assert_eq!(table.tinted(&c), table);
    }

    #[test]
    fn a_tint_scales_the_calibration_instead_of_replacing_it() {
        let mut calibrated = Table::identity(256);
        calibrated.red[255] = 0.9;
        calibrated.blue[128] = 0.4;
        let c = from_kelvin(3400.0);
        let tinted = calibrated.tinted(&c);
        assert_eq!(tinted.red[255], 0.9, "red is at full gain at 3400 K");
        assert!((tinted.blue[128] - 0.4 * c.b).abs() < 1e-6);
        assert_eq!(tinted.len(), 256);
    }

    #[test]
    fn identity_runs_from_black_to_white() {
        let t = Table::identity(1024);
        assert_eq!(t.red[0], 0.0);
        assert_eq!(t.blue[1023], 1.0);
        assert!(t.is_valid());
        assert!(!Table {
            red: vec![0.0; 3],
            green: vec![0.0; 2],
            blue: vec![0.0; 3],
        }
        .is_valid());
    }

    /// Bit for bit: a calibration that comes back rounded is not the one the
    /// user had.
    #[test]
    fn a_baseline_survives_a_round_trip_exactly() {
        let mut one = Table::identity(256);
        one.green[7] = 0.123_456_7;
        let two = Table::identity(1024).tinted(&from_kelvin(4200.0));
        let encoded = encode(&[(1, one.clone()), (69_733_378, two.clone())]);
        let decoded = decode(&encoded);
        assert_eq!(decoded, vec![(1, one), (69_733_378, two)]);
    }

    #[test]
    fn a_damaged_entry_is_skipped_and_the_others_survive() {
        let good = encode(&[(5, Table::identity(256))]);
        let decoded = decode(&format!("{good};6=256:deadbeef;x=1:00000000;7=1:zzzzzzzz"));
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, 5);
    }

    #[test]
    fn a_display_that_left_is_not_a_failed_restore() {
        let encoded = encode(&[(1, Table::identity(256)), (2, Table::identity(256))]);
        let mut written = Vec::new();
        let result = restore(&encoded, &[2], |id, _| {
            written.push(id);
            id == 2
        });
        assert_eq!(result, Restored::Done);
        assert_eq!(written, vec![1, 2], "both are attempted");
    }

    #[test]
    fn a_display_that_refuses_keeps_the_journal_dirty() {
        let encoded = encode(&[(1, Table::identity(256)), (2, Table::identity(256))]);
        let result = restore(&encoded, &[1, 2], |id, _| id == 2);
        assert_eq!(result, Restored::Failed(vec![1]));
    }

    #[test]
    fn an_unreadable_entry_is_reported_as_such() {
        assert_eq!(restore("garbage", &[1], |_, _| true), Restored::Unreadable);
        assert_eq!(restore("", &[1], |_, _| true), Restored::Done);
    }
}
