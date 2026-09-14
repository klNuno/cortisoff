//! Detecting another application that is already driving the screen's colour.
//!
//! This exists because of a real incident, written up in `TRAPS.md`. f.lux,
//! LightBulb and the Windows Night Light re-apply their gamma ramp on a loop.
//! Capture a baseline while one runs and the journal records *its* tint as the
//! user's neutral, so the restore never returns the screen to normal and two
//! processes fight over the ramp forever.
//!
//! Everything here is arithmetic over a ramp, so every platform reuses it:
//! Windows `SetDeviceGammaRamp`, Linux RandR and macOS
//! `CGSetDisplayTransferByTable` all speak the same 3 by 256 table.

use serde::{Deserialize, Serialize};

/// A gamma ramp as every platform hands it over: red, green and blue, 256
/// entries each, full 16 bit range.
pub type GammaRamp = [[u16; 256]; 3];

/// Applications known to drive the screen's colour on a loop, by executable
/// file name, lowercase and without a path.
///
/// Not a blocklist. cortisoff never fights one of these, it refuses to
/// capture a baseline underneath one and says so.
pub const KNOWN_COLOUR_APPS: &[(&str, &str)] = &[
    ("flux.exe", "f.lux"),
    ("lightbulb.exe", "LightBulb"),
    ("gammy.exe", "Gammy"),
    ("iris.exe", "Iris"),
    ("sunsetscreen.exe", "SunsetScreen"),
    ("redshift", "Redshift"),
    ("gammastep", "Gammastep"),
    ("wlsunset", "wlsunset"),
    ("shifty", "Shifty"),
    // Both spellings ship. The releases are named `vibranceGUI.exe`, and what
    // also used by installations is `vibrance.GUI.exe`. Missing
    // the dotted one meant the gate said nothing while VibranceGUI was driving
    // the saturation.
    ("vibrancegui.exe", "VibranceGUI"),
    ("vibrance.gui.exe", "VibranceGUI"),
];

/// Which known applications are among the running processes.
///
/// Takes names rather than doing the enumeration, so the platform owns the OS
/// call and this stays testable.
pub fn identify_running<'a>(processes: impl IntoIterator<Item = &'a str>) -> Vec<&'static str> {
    let mut found = Vec::new();
    for process in processes {
        let name = process
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(process)
            .to_ascii_lowercase();
        for (exe, label) in KNOWN_COLOUR_APPS {
            if name == *exe && !found.contains(label) {
                found.push(*label);
            }
        }
    }
    found
}

/// What a ramp says about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RampVerdict {
    /// Largest gap between two channels at the same index. A colour filter
    /// pulls blue down and shows up here immediately; a monitor calibration
    /// profile usually does not.
    pub imbalance: u16,
    /// Largest deviation from the straight line through each channel's own
    /// endpoints.
    ///
    /// This is the number the T0 probe was missing. It sampled three indices
    /// near the top, so a filter bending the middle of the curve while leaving
    /// the endpoints at full scale read as perfectly linear.
    pub curvature: u16,
}

impl RampVerdict {
    /// Room for driver rounding and for a mild calibration profile, well below
    /// anything a colour filter does.
    pub const TOLERANCE: u16 = 512;

    /// Safe to capture as a baseline.
    pub fn looks_untouched(&self) -> bool {
        self.imbalance <= Self::TOLERANCE && self.curvature <= Self::TOLERANCE
    }
}

/// Measure a ramp without judging it. Judging is [`RampVerdict`]'s job and
/// reporting is the doctor's.
pub fn inspect(ramp: &GammaRamp) -> RampVerdict {
    let mut imbalance = 0u16;
    for i in 0..256 {
        let (lo, hi) = ramp.iter().fold((u16::MAX, 0u16), |(lo, hi), channel| {
            (lo.min(channel[i]), hi.max(channel[i]))
        });
        imbalance = imbalance.max(hi - lo);
    }

    let mut curvature = 0u16;
    for channel in ramp {
        let (first, last) = (i64::from(channel[0]), i64::from(channel[255]));
        for (i, &entry) in channel.iter().enumerate() {
            // The straight line through this channel's own endpoints, so a
            // dimmed but linear ramp scores zero here and only `imbalance`
            // can flag it.
            let expected = first + (last - first) * i as i64 / 255;
            let off = (i64::from(entry) - expected).unsigned_abs();
            curvature = curvature.max(u16::try_from(off).unwrap_or(u16::MAX));
        }
    }

    RampVerdict {
        imbalance,
        curvature,
    }
}

/// The identity ramp, for tests and for restoring a screen with no captured
/// baseline to fall back on.
pub fn identity_ramp() -> GammaRamp {
    let mut ramp = [[0u16; 256]; 3];
    for channel in &mut ramp {
        for (i, entry) in channel.iter_mut().enumerate() {
            *entry = (i as u16) * 257;
        }
    }
    ramp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both names ship, and only the dotted one is what runs on the machine
    /// this was found on.
    #[test]
    fn vibrancegui_is_recognised_under_either_spelling() {
        assert_eq!(
            super::identify_running(["vibranceGUI.exe"]),
            vec!["VibranceGUI"]
        );
        assert_eq!(
            super::identify_running([r"C:\tools\vibrance.GUI.exe"]),
            vec!["VibranceGUI"]
        );
    }

    fn scaled(ramp: &mut GammaRamp, channel: usize, factor: f64) {
        for entry in &mut ramp[channel] {
            *entry = (f64::from(*entry) * factor) as u16;
        }
    }

    #[test]
    fn an_identity_ramp_looks_untouched() {
        assert!(inspect(&identity_ramp()).looks_untouched());
    }

    /// What a blue light filter does. The whole reason this module exists.
    #[test]
    fn a_ramp_with_blue_pulled_down_is_flagged() {
        let mut ramp = identity_ramp();
        scaled(&mut ramp, 2, 0.7);
        let verdict = inspect(&ramp);
        assert!(!verdict.looks_untouched());
        assert!(verdict.imbalance > RampVerdict::TOLERANCE);
    }

    /// The trap exactly: endpoints untouched, middle bent. The T0 probe
    /// sampled near the top and called this linear.
    #[test]
    fn a_bend_in_the_middle_is_caught_even_with_clean_endpoints() {
        let mut ramp = identity_ramp();
        for channel in &mut ramp {
            for (i, entry) in channel.iter_mut().enumerate() {
                // A bump peaking at the midpoint and vanishing at both ends.
                let t = i as f64 / 255.0;
                let bump = (t * (1.0 - t) * 4.0) * 6000.0;
                *entry = entry.saturating_sub(bump as u16);
            }
        }
        assert_eq!(ramp[0][0], 0);
        assert_eq!(ramp[0][255], 255 * 257);
        let verdict = inspect(&ramp);
        // No colour cast at all, so only curvature can catch it.
        assert_eq!(verdict.imbalance, 0);
        assert!(verdict.curvature > RampVerdict::TOLERANCE);
        assert!(!verdict.looks_untouched());
    }

    /// A dimmed but perfectly linear grey ramp is a brightness setting, not a
    /// filter. Curvature must not fire on it.
    #[test]
    fn a_uniformly_dimmed_ramp_has_no_curvature() {
        let mut ramp = identity_ramp();
        for c in 0..3 {
            scaled(&mut ramp, c, 0.5);
        }
        let verdict = inspect(&ramp);
        assert!(verdict.curvature <= RampVerdict::TOLERANCE);
        assert_eq!(verdict.imbalance, 0);
    }

    #[test]
    fn a_known_application_is_found_by_file_name_in_any_case() {
        assert_eq!(
            identify_running([
                "explorer.exe",
                "C:\\Program Files\\LightBulb\\LightBulb.exe"
            ]),
            vec!["LightBulb"]
        );
    }

    #[test]
    fn an_application_running_twice_is_reported_once() {
        assert_eq!(identify_running(["flux.exe", "flux.exe"]), vec!["f.lux"]);
    }

    #[test]
    fn nothing_running_means_nothing_reported() {
        assert!(identify_running(["explorer.exe", "cargo.exe"]).is_empty());
    }
}
