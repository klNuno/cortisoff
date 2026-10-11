//! The host's own timers, kept in the host rather than in the run loop.
//!
//! A `CFRunLoopTimer` fires on its own and leaves the run loop running, so
//! the poll would need a source for every timer just to come back out. Here
//! the poll asks for the next deadline, runs the loop at most that long, and
//! takes whatever is due when it returns, whatever woke it.

use cortisoff_host::TimerId;

/// Who asked. The engine's ids and the host's own beats never collide, so
/// the host needs no reserved id range the way Win32 timer ids do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Beat {
    Engine(TimerId),
    /// The next inactivity threshold, or the next second while away.
    Idle,
    /// Whether the focused window fills its display. macOS has no event
    /// for a window being resized, so this is a coarse poll.
    Window,
    /// The next push of a display change while the configuration settles.
    Settle,
    /// The power source, when IOKit would not hand over its notification.
    Power,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    beat: Beat,
    due: u64,
    latest: u64,
}

#[derive(Debug, Default)]
pub struct Timers {
    pending: Vec<Pending>,
}

impl Timers {
    /// Due `after_ms` from `now_ms`, and late by at most `tolerance_ms`.
    /// Arming a beat that is pending replaces it.
    pub fn arm(&mut self, beat: Beat, now_ms: u64, after_ms: u64, tolerance_ms: u64) {
        self.cancel(beat);
        let due = now_ms.saturating_add(after_ms);
        self.pending.push(Pending {
            beat,
            due,
            latest: due.saturating_add(tolerance_ms),
        });
    }

    pub fn cancel(&mut self, beat: Beat) {
        self.pending.retain(|p| p.beat != beat);
    }

    pub fn is_armed(&self, beat: Beat) -> bool {
        self.pending.iter().any(|p| p.beat == beat)
    }

    /// When the poll has to be back: the first moment one of them would be
    /// later than its tolerance allows. Waking there rather than at the
    /// earliest due time is what lets a 30 s power check with 15 s of slack
    /// ride along with whatever else wakes the loop in between.
    pub fn deadline(&self) -> Option<u64> {
        self.pending.iter().map(|p| p.latest).min()
    }

    /// Every beat due by `now_ms`, removed, earliest first. Two due at the
    /// same moment come out in a fixed order, so a test can rely on it.
    pub fn take_due(&mut self, now_ms: u64) -> Vec<Beat> {
        let mut due: Vec<Pending> = self
            .pending
            .iter()
            .copied()
            .filter(|p| p.due <= now_ms)
            .collect();
        self.pending.retain(|p| p.due > now_ms);
        due.sort_by_key(|p| (p.due, p.beat));
        due.into_iter().map(|p| p.beat).collect()
    }
}

/// The last inactivity threshold crossed, so one event goes out per
/// crossing rather than one per poll.
pub fn idle_bucket(seconds: u32, thresholds: &[u32]) -> u32 {
    thresholds
        .iter()
        .copied()
        .filter(|t| *t <= seconds)
        .max()
        .unwrap_or(0)
}

/// Seconds until the next idle check: near the next threshold while
/// present, every second while away, so a key press resumes things without
/// a long delay. Never more than fifteen, because input that was missed
/// while the loop slept is only ever seen by a check.
pub fn idle_poll_after(seconds: u32, thresholds: &[u32]) -> u32 {
    if idle_bucket(seconds, thresholds) > 0 {
        return 1;
    }
    thresholds
        .iter()
        .filter_map(|t| t.checked_sub(seconds))
        .min()
        .unwrap_or(15)
        .clamp(1, 15)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(n: u32) -> Beat {
        Beat::Engine(TimerId(n))
    }

    #[test]
    fn due_timers_come_out_earliest_first_and_only_once() {
        let mut t = Timers::default();
        t.arm(engine(2), 0, 300, 0);
        t.arm(Beat::Idle, 0, 100, 0);
        t.arm(engine(1), 0, 200, 0);
        assert_eq!(t.take_due(150), vec![Beat::Idle]);
        assert_eq!(t.take_due(400), vec![engine(1), engine(2)]);
        assert!(t.take_due(1_000).is_empty());
    }

    #[test]
    fn a_tie_resolves_in_a_fixed_order() {
        let mut t = Timers::default();
        t.arm(Beat::Power, 0, 50, 0);
        t.arm(engine(9), 0, 50, 0);
        t.arm(Beat::Idle, 0, 50, 0);
        assert_eq!(t.take_due(50), vec![engine(9), Beat::Idle, Beat::Power]);
    }

    /// The engine re-arms what it still needs, and re-arming moves the timer
    /// rather than adding a second one.
    #[test]
    fn rearming_replaces_and_cancelling_removes() {
        let mut t = Timers::default();
        t.arm(engine(1), 0, 100, 0);
        t.arm(engine(1), 0, 500, 0);
        assert!(t.take_due(200).is_empty());
        assert_eq!(t.take_due(500), vec![engine(1)]);

        t.arm(Beat::Window, 0, 100, 0);
        assert!(t.is_armed(Beat::Window));
        t.cancel(Beat::Window);
        assert!(!t.is_armed(Beat::Window));
        assert!(t.take_due(1_000).is_empty());
        assert_eq!(t.deadline(), None);
    }

    /// The poll wakes when the strictest timer runs out of slack, and takes
    /// everything already due with it.
    #[test]
    fn tolerance_batches_wakeups() {
        let mut t = Timers::default();
        t.arm(Beat::Power, 0, 30_000, 15_000);
        t.arm(engine(1), 0, 40_000, 0);
        assert_eq!(t.deadline(), Some(40_000));
        assert_eq!(t.take_due(40_000), vec![Beat::Power, engine(1)]);

        t.arm(Beat::Idle, 0, 1_000, 100);
        t.arm(engine(2), 0, 1_050, 0);
        assert_eq!(t.deadline(), Some(1_050));
        assert_eq!(t.take_due(1_050), vec![Beat::Idle, engine(2)]);
    }

    #[test]
    fn a_huge_delay_does_not_overflow() {
        let mut t = Timers::default();
        t.arm(engine(1), u64::MAX - 5, u64::MAX, u64::MAX);
        assert_eq!(t.deadline(), Some(u64::MAX));
        assert!(t.take_due(u64::MAX - 1).is_empty());
    }

    #[test]
    fn each_configured_delay_has_its_own_crossing() {
        let thresholds = [60, 120, 300];
        assert_eq!(idle_bucket(59, &thresholds), 0);
        assert_eq!(idle_bucket(60, &thresholds), 60);
        assert_eq!(idle_bucket(119, &thresholds), 60);
        assert_eq!(idle_bucket(120, &thresholds), 120);
        assert_eq!(idle_bucket(0, &thresholds), 0);
        assert_eq!(idle_poll_after(59, &thresholds), 1);
        assert_eq!(idle_poll_after(120, &thresholds), 1);
        assert_eq!(idle_poll_after(0, &thresholds), 15);
        assert_eq!(idle_poll_after(50, &thresholds), 10);
    }
}
