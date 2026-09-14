//! The gate between "something else is already driving this screen" and the
//! first baseline capture.
//!
//! `cortisoff-host::interference` answers whether the screen looks touched.
//! This decides what to do about it, which is always the same thing: leave the
//! capability alone and say so. Fighting f.lux for the gamma ramp is how both
//! applications end up flickering, and capturing a baseline underneath it is
//! how the user's neutral becomes orange forever.

use std::collections::BTreeSet;

use cortisoff_host::{ids, inspect, CapabilityId, GammaRamp};

/// What the daemon refuses to touch this run, and why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Interference {
    blocked: BTreeSet<CapabilityId>,
    /// One sentence per reason, printed by `cortisoff doctor` verbatim.
    pub warnings: Vec<String>,
}

impl Interference {
    /// Nothing in the way. What a machine with no colour utility installed
    /// gets, and what the tests of everything downstream use.
    pub fn clear() -> Self {
        Self::default()
    }

    /// `processes` is every running executable's name or path, and `ramp` is
    /// the current gamma ramp when the platform can read one. Both come from
    /// the host, so this stays arithmetic and stays tested without an OS.
    pub fn assess<'a>(
        processes: impl IntoIterator<Item = &'a str>,
        ramp: Option<&GammaRamp>,
    ) -> Self {
        let mut out = Self::default();

        for app in cortisoff_host::identify_running(processes) {
            // VibranceGUI drives saturation, the rest drive the ramp. Naming
            // the capability rather than blocking everything means a f.lux
            // user still gets refresh rate switching on focus.
            let cap = if app == "VibranceGUI" {
                ids::DISPLAY_VIBRANCE
            } else {
                ids::DISPLAY_COLOR_TEMP
            };
            out.block(
                cap,
                format!(
                    "{app} is running and drives {cap} on a loop. cortisoff will not touch it \
                     this run: capturing a baseline underneath it would record {app}'s setting \
                     as your normal screen. close it and reload"
                ),
            );
        }

        // Evidence beats the name list. A colour utility nobody has heard of,
        // or one renamed by the user, still bends the ramp.
        if let Some(ramp) = ramp {
            let verdict = inspect(ramp);
            if !verdict.looks_untouched() && !out.blocks_id(ids::DISPLAY_COLOR_TEMP) {
                out.block(
                    ids::DISPLAY_COLOR_TEMP,
                    format!(
                        "the gamma ramp is not neutral (channel imbalance {}, curvature {}), so \
                         something is already filtering this screen. cortisoff will not touch \
                         {} this run",
                        verdict.imbalance,
                        verdict.curvature,
                        ids::DISPLAY_COLOR_TEMP
                    ),
                );
            }
        }

        out
    }

    fn block(&mut self, cap: &str, warning: String) {
        self.blocked.insert(CapabilityId::new(cap));
        self.warnings.push(warning);
    }

    fn blocks_id(&self, cap: &str) -> bool {
        self.blocked.contains(&CapabilityId::new(cap))
    }

    pub fn blocks(&self, cap: &CapabilityId) -> bool {
        self.blocked.contains(cap)
    }

    pub fn blocked(&self) -> impl Iterator<Item = &CapabilityId> {
        self.blocked.iter()
    }

    pub fn is_clear(&self) -> bool {
        self.blocked.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortisoff_host::identity_ramp;

    fn cap(s: &str) -> CapabilityId {
        CapabilityId::new(s)
    }

    #[test]
    fn a_clean_machine_blocks_nothing() {
        let gate = Interference::assess(["explorer.exe"], Some(&identity_ramp()));
        assert!(gate.is_clear());
        assert!(gate.warnings.is_empty());
    }

    #[test]
    fn a_running_filter_blocks_colour_temperature_only() {
        let gate = Interference::assess(["lightbulb.exe"], None);
        assert!(gate.blocks(&cap(ids::DISPLAY_COLOR_TEMP)));
        // Refresh rate and vibrance have nothing to do with the gamma ramp,
        // so they keep working while LightBulb runs.
        assert!(!gate.blocks(&cap(ids::DISPLAY_MODE)));
        assert!(!gate.blocks(&cap(ids::DISPLAY_VIBRANCE)));
    }

    #[test]
    fn vibrancegui_blocks_vibrance_rather_than_the_ramp() {
        let gate = Interference::assess(["vibrancegui.exe"], None);
        assert!(gate.blocks(&cap(ids::DISPLAY_VIBRANCE)));
        assert!(!gate.blocks(&cap(ids::DISPLAY_COLOR_TEMP)));
    }

    /// The list can never be complete, so the ramp itself has the last word.
    #[test]
    fn a_bent_ramp_blocks_even_with_no_known_process_running() {
        let mut ramp = identity_ramp();
        for entry in &mut ramp[2] {
            *entry = (f64::from(*entry) * 0.6) as u16;
        }
        let gate = Interference::assess(["explorer.exe"], Some(&ramp));
        assert!(gate.blocks(&cap(ids::DISPLAY_COLOR_TEMP)));
        assert_eq!(gate.warnings.len(), 1);
    }

    /// The user gets told once, with the name of the culprit, not twice with
    /// the second sentence naming nothing.
    #[test]
    fn a_named_process_and_a_bent_ramp_produce_one_warning() {
        let mut ramp = identity_ramp();
        for entry in &mut ramp[2] {
            *entry = (f64::from(*entry) * 0.6) as u16;
        }
        let gate = Interference::assess(["flux.exe"], Some(&ramp));
        assert_eq!(gate.warnings.len(), 1);
        assert!(gate.warnings[0].contains("f.lux"));
    }

    #[test]
    fn a_platform_that_cannot_read_a_ramp_still_uses_the_name_list() {
        let gate = Interference::assess(["gammastep"], None);
        assert!(gate.blocks(&cap(ids::DISPLAY_COLOR_TEMP)));
    }
}
