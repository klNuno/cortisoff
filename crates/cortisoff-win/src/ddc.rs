//! DDC/CI RGB gain: the panel tints itself.
//!
//! The only colour path left on an output with HDR on, where the gamma ramp is
//! undefined and Night Light is switched off, because Windows is not involved
//! in the result. It pays for that on the wire: the T0 probe measured 185 ms to
//! read three channels on one monitor. So nothing here sits on a focus path, a
//! capture only asks outputs that can turn HDR on, and a fade step goes out only
//! when the integer gain it rounds to has changed.
//!
//! Panels do not sit at a neutral gain. The probe read 51/45/48, 100/92/96 and
//! 50/50/50 on three monitors. A tint is therefore the captured gain scaled
//! down, never a fraction of the maximum, and letting go means writing the
//! captured gain back, never a default.

use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorRedGreenOrBlueGain, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, SetMonitorRedGreenOrBlueGain, MC_BLUE_GAIN, MC_GAIN_TYPE,
    MC_GREEN_GAIN, MC_RED_GAIN, PHYSICAL_MONITOR,
};
use windows::Win32::Graphics::Gdi::HMONITOR;

use crate::color::Rgb;
use crate::screen;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gain {
    pub min: u32,
    pub current: u32,
    pub max: u32,
}

/// Red, green and blue, in that order.
pub type Gains = [Gain; 3];

const CHANNELS: [MC_GAIN_TYPE; 3] = [MC_RED_GAIN, MC_GREEN_GAIN, MC_BLUE_GAIN];

/// Marks a panel's entry in the baseline text. The ramp decoder already skips
/// an entry that is not 768 ramp values, so the two share one string and a
/// journal written before this backend existed still restores.
const PREFIX: &str = "ddc:";

/// A physical monitor behind a GDI device, `\\.\DISPLAY1#0`. One device drives
/// several when the output is mirrored.
pub fn key(device: &str, index: usize) -> String {
    format!("{device}#{index}")
}

pub fn device_of(key: &str) -> &str {
    key.rsplit_once('#').map_or(key, |(device, _)| device)
}

/// Every channel of every physical monitor behind `devices` that answers.
/// Around 185 ms a panel, so read the module comment before calling it from
/// anywhere new.
pub fn capture(devices: &[String]) -> Vec<(String, Gains)> {
    let mut out = Vec::new();
    for (device, h) in screen::handles() {
        if !devices.contains(&device) {
            continue;
        }
        let Some(physical) = open(h) else {
            continue;
        };
        for (i, pm) in physical.iter().enumerate() {
            if let (Some(r), Some(g), Some(b)) = (
                read(pm, CHANNELS[0]),
                read(pm, CHANNELS[1]),
                read(pm, CHANNELS[2]),
            ) {
                out.push((key(&device, i), [r, g, b]));
            }
        }
        close(&physical);
    }
    out
}

/// Writes red, green and blue to the panel `key` names. True only when all
/// three were accepted: a panel that takes red and refuses blue is not
/// carrying the tint.
pub fn write(key: &str, values: [u32; 3]) -> bool {
    let Some((device, index)) = key
        .rsplit_once('#')
        .and_then(|(d, i)| Some((d, i.parse::<usize>().ok()?)))
    else {
        return false;
    };
    let Some((_, h)) = screen::handles().into_iter().find(|(d, _)| d == device) else {
        return false;
    };
    let Some(physical) = open(h) else {
        return false;
    };
    let accepted = physical.get(index).is_some_and(|pm| {
        let mut all = true;
        for (channel, value) in CHANNELS.iter().zip(values) {
            // Every channel is attempted even after a refusal, so a panel is
            // never left with red at the new gain and blue at the old one.
            let ok = unsafe { SetMonitorRedGreenOrBlueGain(pm.hPhysicalMonitor, *channel, value) };
            all &= ok != 0;
        }
        all
    });
    close(&physical);
    accepted
}

/// The gains a colour temperature asks of one panel. Always scaled from the
/// captured gain, so fade steps never compound, and clamped to what the panel
/// said it accepts.
pub fn targets(base: &Gains, c: &Rgb) -> [u32; 3] {
    let scale = |g: &Gain, m: f32| {
        let wanted = (g.current as f32 * m.clamp(0.0, 1.0)).round() as u32;
        wanted.clamp(g.min, g.max.max(g.min))
    };
    [
        scale(&base[0], c.r),
        scale(&base[1], c.g),
        scale(&base[2], c.b),
    ]
}

pub fn currents(base: &Gains) -> [u32; 3] {
    base.map(|g| g.current)
}

/// `ddc:\\.\DISPLAY1#0=0/51/100,0/45/100,0/48/100`, entries joined by `;`.
pub fn encode(gains: &[(String, Gains)]) -> String {
    gains
        .iter()
        .map(|(key, g)| {
            let channels: Vec<String> = g
                .iter()
                .map(|c| format!("{}/{}/{}", c.min, c.current, c.max))
                .collect();
            format!("{PREFIX}{key}={}", channels.join(","))
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// The panel entries of a baseline, skipping ramps and anything damaged.
pub fn decode(encoded: &str) -> Vec<(String, Gains)> {
    encoded
        .split(';')
        .filter_map(|part| {
            let (key, channels) = part.strip_prefix(PREFIX)?.split_once('=')?;
            let parsed: Vec<Gain> = channels
                .split(',')
                .map(|c| {
                    let mut n = c.split('/').map(|v| v.parse::<u32>().ok());
                    let gain = Gain {
                        min: n.next()??,
                        current: n.next()??,
                        max: n.next()??,
                    };
                    n.next().is_none().then_some(gain)
                })
                .collect::<Option<_>>()?;
            let gains: Gains = parsed.try_into().ok()?;
            Some((key.to_owned(), gains))
        })
        .collect()
}

fn open(h: HMONITOR) -> Option<Vec<PHYSICAL_MONITOR>> {
    let mut n = 0u32;
    unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut n) }.ok()?;
    if n == 0 {
        return None;
    }
    let mut buf = vec![PHYSICAL_MONITOR::default(); n as usize];
    unsafe { GetPhysicalMonitorsFromHMONITOR(h, &mut buf) }.ok()?;
    Some(buf)
}

fn close(buf: &[PHYSICAL_MONITOR]) {
    unsafe {
        let _ = DestroyPhysicalMonitors(buf);
    }
}

fn read(pm: &PHYSICAL_MONITOR, channel: MC_GAIN_TYPE) -> Option<Gain> {
    let (mut min, mut current, mut max) = (0u32, 0u32, 0u32);
    let ok = unsafe {
        GetMonitorRedGreenOrBlueGain(
            pm.hPhysicalMonitor,
            channel,
            &mut min,
            &mut current,
            &mut max,
        )
    };
    (ok != 0).then_some(Gain { min, current, max })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{self, from_kelvin, ramp_for};

    fn gain(min: u32, current: u32, max: u32) -> Gain {
        Gain { min, current, max }
    }

    /// The probe's second monitor, deliberately not neutral.
    fn calibrated() -> Gains {
        [gain(0, 100, 100), gain(0, 92, 100), gain(0, 96, 100)]
    }

    #[test]
    fn a_warm_tint_scales_the_captured_gain_not_the_maximum() {
        let c = from_kelvin(3200.0);
        let t = targets(&calibrated(), &c);
        assert_eq!(t[0], 100);
        assert_eq!(t[1], (92.0 * c.g).round() as u32);
        assert_eq!(t[2], (96.0 * c.b).round() as u32);
        assert!(t[2] < 96 && t[2] < t[1]);
    }

    /// Every fade step scales the same captured gain, so running 3200 K twice
    /// lands on the same numbers instead of going warmer each time.
    #[test]
    fn repeated_steps_do_not_compound() {
        let c = from_kelvin(3200.0);
        assert_eq!(targets(&calibrated(), &c), targets(&calibrated(), &c));
    }

    #[test]
    fn neutral_writes_the_captured_gain_back() {
        let base = [gain(0, 51, 100), gain(0, 45, 100), gain(0, 48, 100)];
        assert_eq!(targets(&base, &Rgb::NEUTRAL), currents(&base));
    }

    #[test]
    fn a_target_stays_inside_what_the_panel_accepts() {
        let base = [gain(20, 30, 40), gain(20, 30, 40), gain(20, 30, 40)];
        let t = targets(&base, &from_kelvin(1000.0));
        assert!(t.iter().all(|v| (20..=40).contains(v)), "{t:?}");
    }

    /// A panel reporting its range backwards must not panic the daemon.
    #[test]
    fn an_inverted_range_does_not_panic() {
        let base = [gain(50, 10, 5); 3];
        let _ = targets(&base, &from_kelvin(3200.0));
    }

    #[test]
    fn a_key_names_its_device() {
        let k = key(r"\\.\DISPLAY2", 1);
        assert_eq!(k, r"\\.\DISPLAY2#1");
        assert_eq!(device_of(&k), r"\\.\DISPLAY2");
    }

    #[test]
    fn gains_survive_a_round_trip() {
        let gains = vec![
            (key(r"\\.\DISPLAY1", 0), calibrated()),
            (key(r"\\.\DISPLAY3", 0), [gain(0, 50, 100); 3]),
        ];
        assert_eq!(decode(&encode(&gains)), gains);
    }

    /// Ramps and gains share one baseline string. Each decoder has to take its
    /// own entries and nothing else, or a restore writes a ramp to a panel.
    #[test]
    fn ramps_and_gains_share_a_baseline_without_mixing() {
        let ramp = ramp_for(&from_kelvin(5000.0));
        let gains = vec![(key(r"\\.\DISPLAY1", 0), calibrated())];
        let text = format!(
            "{};{}",
            color::encode(&[(r"\\.\DISPLAY1".to_owned(), ramp)]),
            encode(&gains)
        );
        let ramps = color::decode(&text);
        assert_eq!(ramps.len(), 1);
        assert_eq!(ramps[0].0, r"\\.\DISPLAY1");
        assert_eq!(decode(&text), gains);
    }

    #[test]
    fn a_damaged_panel_entry_is_skipped() {
        let good = encode(&[(key("a", 0), calibrated())]);
        let text = format!("{good};ddc:b#0=0/50/100,0/50;ddc:c#0=1/2/3/4,0/1/2,0/1/2");
        let decoded = decode(&text);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, "a#0");
    }
}
