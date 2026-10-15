//! Shift at logon: the way out of a plugin that misbehaves from the moment
//! the session opens, as MSI Afterburner does.
//!
//! The logon task starts `cortisoff --daemon --logon`. Before anything
//! else, that start watches the Shift key for [`WATCH`]. Held for
//! [`HOLD`] in a row, the daemon does not start at all and leaves a marker
//! (`Paths::held`). The window opened by hand afterwards reads the marker
//! and starts the daemon with `--held`: every plugin listed, none running,
//! the logo off, until the user turns the culprit off and clicks it.
//!
//! The watch is also the wait the desktop gets before any plugin claims a
//! screen, which the logon task used to do with a ten second delay.

use std::time::Duration;

/// How long a logon start watches for Shift.
pub const WATCH: Duration = Duration::from_secs(10);

/// Held this long without a break, Shift counts. A capital letter typed
/// in the first seconds of the session does not.
pub const HOLD: Duration = Duration::from_secs(1);

/// The time between two looks at the key.
pub const SAMPLE: Duration = Duration::from_millis(50);

/// Whether Shift was held for `hold` in a row within `watch`. `held` reads
/// the key, `sleep` waits [`SAMPLE`]. Answers as soon as the hold is long
/// enough; otherwise watches the whole time.
pub fn shift_held_through(
    watch: Duration,
    hold: Duration,
    mut held: impl FnMut() -> bool,
    mut sleep: impl FnMut(Duration),
) -> bool {
    let samples = watch.as_millis() / SAMPLE.as_millis();
    let need = (hold.as_millis() / SAMPLE.as_millis()).max(1);
    let mut run = 0;
    for _ in 0..samples {
        run = if held() { run + 1 } else { 0 };
        if run >= need {
            return true;
        }
        sleep(SAMPLE);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays a key that is down for the samples `down` names.
    fn watch(down: impl Fn(usize) -> bool) -> (bool, usize) {
        let mut i = 0;
        let mut slept = 0;
        let held = shift_held_through(
            WATCH,
            HOLD,
            || {
                let d = down(i);
                i += 1;
                d
            },
            |_| slept += 1,
        );
        (held, slept)
    }

    #[test]
    fn shift_held_from_the_start_answers_after_one_second() {
        let (held, slept) = watch(|_| true);
        assert!(held);
        assert_eq!(slept, 19, "twenty looks, fifty milliseconds apart");
    }

    /// The sign-in screen has the input at first: the key reads up until
    /// the desktop takes over, then down.
    #[test]
    fn shift_seen_only_once_the_desktop_appears_still_counts() {
        let (held, _) = watch(|i| i >= 80);
        assert!(held);
    }

    #[test]
    fn a_capital_letter_typed_is_not_a_hold() {
        let (held, slept) = watch(|i| i % 40 < 5);
        assert!(!held);
        assert_eq!(slept, 200, "the whole ten seconds, the desktop's wait");
    }

    #[test]
    fn no_shift_waits_the_whole_watch() {
        let (held, slept) = watch(|_| false);
        assert!(!held);
        assert_eq!(slept, 200);
    }
}
