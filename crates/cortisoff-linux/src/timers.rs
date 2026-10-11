//! The host's timers: the engine's one-shot ids and the host's own beats in
//! one queue, served by one wait.
//!
//! A timer may fire anywhere between its due time and its due time plus its
//! tolerance. The wait ends at the earliest of those latest moments, and
//! every timer already due by then fires in the same wakeup. That is the
//! whole coalescing scheme, and it is why a thirty second power check with
//! fifteen seconds of slack rides along with whatever else wakes the process
//! instead of waking it on its own.

use std::collections::BTreeMap;
use std::time::Duration;

/// Who asked. The engine's ids live in their own variant, so no host beat
/// can collide with one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Engine(u32),
    /// The next idle threshold, or the next check of one.
    Idle,
    /// `power.source`, re-read every thirty seconds while a plugin reads it.
    Power,
    /// `audio.active`, same.
    Audio,
    /// The next push of a display change while the driver settles.
    Settle,
    /// The next change of the UTC offset: a daylight saving edge.
    Clock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    due: u64,
    slack: u64,
}

/// Times are milliseconds on whatever clock the caller reads. The queue
/// never reads one itself, which is what lets the tests drive it.
#[derive(Debug, Default)]
pub struct Timers {
    pending: BTreeMap<Key, Entry>,
}

impl Timers {
    /// Arming a key that is pending replaces it, as the host contract says.
    pub fn set(&mut self, key: Key, now: u64, after: Duration, tolerance: Duration) {
        let entry = Entry {
            due: now.saturating_add(millis(after)),
            slack: millis(tolerance),
        };
        self.pending.insert(key, entry);
    }

    pub fn cancel(&mut self, key: Key) {
        self.pending.remove(&key);
    }

    #[cfg(test)]
    pub fn is_set(&self, key: Key) -> bool {
        self.pending.contains_key(&key)
    }

    /// The latest moment the next wait may end.
    pub fn wake_at(&self) -> Option<u64> {
        self.pending
            .values()
            .map(|e| e.due.saturating_add(e.slack))
            .min()
    }

    /// Every timer due by `now`, earliest first, removed from the queue. Two
    /// timers due at the same millisecond come out in key order, so a batch
    /// is the same on every run.
    pub fn take_due(&mut self, now: u64) -> Vec<Key> {
        let mut due: Vec<(u64, Key)> = self
            .pending
            .iter()
            .filter(|(_, e)| e.due <= now)
            .map(|(k, e)| (e.due, *k))
            .collect();
        due.sort_unstable();
        for (_, key) in &due {
            self.pending.remove(key);
        }
        due.into_iter().map(|(_, key)| key).collect()
    }
}

fn millis(d: Duration) -> u64 {
    d.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn due_timers_come_out_earliest_first_and_once() {
        let mut t = Timers::default();
        t.set(Key::Engine(7), 0, ms(300), ms(0));
        t.set(Key::Power, 0, ms(100), ms(0));
        t.set(Key::Engine(2), 0, ms(200), ms(0));
        assert_eq!(t.take_due(50), vec![]);
        assert_eq!(t.take_due(250), vec![Key::Power, Key::Engine(2)]);
        assert_eq!(t.take_due(250), vec![], "a one-shot timer fires once");
        assert_eq!(t.take_due(1_000), vec![Key::Engine(7)]);
        assert_eq!(t.wake_at(), None);
    }

    #[test]
    fn a_tie_is_broken_by_key_so_a_batch_is_stable() {
        let mut t = Timers::default();
        t.set(Key::Settle, 0, ms(100), ms(0));
        t.set(Key::Engine(9), 0, ms(100), ms(0));
        t.set(Key::Engine(1), 0, ms(100), ms(0));
        assert_eq!(
            t.take_due(100),
            vec![Key::Engine(1), Key::Engine(9), Key::Settle]
        );
    }

    #[test]
    fn arming_a_pending_key_replaces_it() {
        let mut t = Timers::default();
        t.set(Key::Engine(1), 0, ms(100), ms(0));
        t.set(Key::Engine(1), 0, ms(900), ms(0));
        assert_eq!(t.take_due(500), vec![]);
        assert_eq!(t.take_due(900), vec![Key::Engine(1)]);
    }

    #[test]
    fn a_cancelled_timer_never_fires() {
        let mut t = Timers::default();
        t.set(Key::Idle, 0, ms(100), ms(0));
        t.cancel(Key::Idle);
        assert!(!t.is_set(Key::Idle));
        assert_eq!(t.take_due(10_000), vec![]);
    }

    /// The slack is what batches: the wait ends at the earliest latest
    /// moment, and the timer with slack fires early, alongside it, rather
    /// than waking the process a second time.
    #[test]
    fn tolerance_lets_a_slow_beat_ride_along_with_an_earlier_one() {
        let mut t = Timers::default();
        t.set(Key::Power, 0, ms(30_000), ms(15_000));
        t.set(Key::Engine(3), 0, ms(40_000), ms(0));
        assert_eq!(t.wake_at(), Some(40_000));
        assert_eq!(t.take_due(40_000), vec![Key::Power, Key::Engine(3)]);

        t.set(Key::Power, 0, ms(30_000), ms(15_000));
        assert_eq!(t.wake_at(), Some(45_000), "alone, it uses its slack");
    }

    #[test]
    fn a_huge_duration_does_not_wrap_around() {
        let mut t = Timers::default();
        t.set(Key::Engine(1), u64::MAX - 5, Duration::MAX, Duration::MAX);
        assert_eq!(t.wake_at(), Some(u64::MAX));
        assert_eq!(t.take_due(u64::MAX - 1), vec![]);
    }
}
