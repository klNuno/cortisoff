//! Clocks: the one the timers run on, and the wall clock the engine hears.
//!
//! Timers and [`cortisoff_host::Host::monotonic_ms`] read `CLOCK_BOOTTIME`,
//! not `CLOCK_MONOTONIC`. The second stops while the machine sleeps, so a
//! 20:00 schedule edge armed at 09:00 on a laptop that slept two hours in
//! between would fire at 22:00. The first keeps counting through a suspend,
//! which is also what `GetTickCount64` does on Windows, so a plugin sees the
//! same clock on both.

use cortisoff_host::Event;
use rustix::time::{clock_gettime, ClockId, Timespec};

pub fn boot_ms() -> u64 {
    millis(clock_gettime(ClockId::Boottime))
}

pub fn mono_ms() -> u64 {
    millis(clock_gettime(ClockId::Monotonic))
}

fn millis(t: Timespec) -> u64 {
    (t.tv_sec.max(0) as u64) * 1000 + (t.tv_nsec.max(0) as u64) / 1_000_000
}

/// Seconds since the epoch, and the millisecond inside the second.
fn wall() -> (i64, u16) {
    let t = clock_gettime(ClockId::Realtime);
    (t.tv_sec, (t.tv_nsec / 1_000_000).clamp(0, 999) as u16)
}

pub fn wall_s() -> i64 {
    wall().0
}

extern "C" {
    /// Declared here because the libc crate carries it for no unix target,
    /// as in `cortisoff-unix`'s clock.
    fn tzset();
}

/// Read the time zone again. glibc's `localtime_r` does not, so without
/// this a zone changed while the daemon runs never reaches a schedule. It
/// costs one `stat` of the zone file, so it runs once per clock event and
/// once per daylight saving search, not once per conversion.
pub fn refresh_zone() {
    // SAFETY: no arguments; it rereads `TZ` and the zone file into the C
    // library's own state under the library's own lock, as `localtime_r`
    // already does on its first call.
    unsafe { tzset() }
}

fn local_tm(epoch_s: i64) -> Option<libc::tm> {
    let t = epoch_s as libc::time_t;
    // SAFETY: `tm` is plain data that `localtime_r` fills in. Both pointers
    // are valid for the duration of the call and nothing else holds them.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let filled = unsafe { libc::localtime_r(&t, &mut tm) };
    (!filled.is_null()).then_some(tm)
}

/// The local time right now as the engine hears it. `None` only when the C
/// library cannot convert the clock, which no machine has shown so far.
pub fn time_of_day() -> Option<Event> {
    refresh_zone();
    let (seconds, millisecond) = wall();
    let tm = local_tm(seconds)?;
    Some(clock_event(tm.tm_hour, tm.tm_min, tm.tm_sec, millisecond))
}

/// A leap second reads as second 60, which the event does not carry.
fn clock_event(hour: i32, minute: i32, second: i32, millisecond: u16) -> Event {
    let hour = hour.clamp(0, 23) as u16;
    let minute = minute.clamp(0, 59) as u16;
    Event::TimeOfDay {
        minute: hour * 60 + minute,
        second: second.clamp(0, 59) as u8,
        millisecond,
    }
}

/// Seconds east of UTC at `epoch_s`, daylight saving included.
pub fn utc_offset(epoch_s: i64) -> Option<i64> {
    local_tm(epoch_s).map(|tm| tm.tm_gmtoff)
}

/// When the UTC offset next changes, searched a step at a time up to
/// `steps` steps ahead, then to the second inside the step that changed.
///
/// Nothing on Linux announces a daylight saving edge to a process, and an
/// engine timer armed across one fires an hour early or late. A host timer
/// at the edge itself pushes the time again at that moment, so the cost is
/// two wakeups a year, not a periodic check. A zone without daylight saving
/// finds nothing and arms nothing.
pub fn next_offset_change(
    from: i64,
    step: i64,
    steps: u32,
    offset_at: impl Fn(i64) -> Option<i64>,
) -> Option<i64> {
    let start = offset_at(from)?;
    let mut low = from;
    for n in 1..=i64::from(steps) {
        let high = from.checked_add(step.checked_mul(n)?)?;
        if offset_at(high)? == start {
            low = high;
            continue;
        }
        // The offset is `start` at `low` and something else at `high`.
        let mut high = high;
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            if offset_at(middle)? == start {
                low = middle;
            } else {
                high = middle;
            }
        }
        return Some(high);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    #[test]
    fn the_clock_event_counts_minutes_from_midnight() {
        assert_eq!(
            clock_event(20, 5, 7, 250),
            Event::TimeOfDay {
                minute: 20 * 60 + 5,
                second: 7,
                millisecond: 250
            }
        );
    }

    #[test]
    fn a_leap_second_is_read_as_the_last_ordinary_one() {
        let Event::TimeOfDay { second, .. } = clock_event(23, 59, 60, 0) else {
            unreachable!()
        };
        assert_eq!(second, 59);
    }

    /// Paris in spring: +1 h until the edge, +2 h from it.
    #[test]
    fn the_next_offset_change_is_found_to_the_second() {
        let edge = 1_000_000 + 37 * DAY + 4_321;
        let offset = |t: i64| Some(if t < edge { 3_600 } else { 7_200 });
        assert_eq!(next_offset_change(1_000_000, DAY, 200, offset), Some(edge));
    }

    #[test]
    fn a_zone_without_daylight_saving_arms_nothing() {
        assert_eq!(next_offset_change(0, DAY, 200, |_| Some(0)), None);
    }

    #[test]
    fn an_edge_past_the_horizon_is_left_for_a_later_search() {
        let edge = 300 * DAY;
        let offset = |t: i64| Some(if t < edge { 0 } else { 3_600 });
        assert_eq!(next_offset_change(0, DAY, 200, offset), None);
    }

    /// The search must stop at the first edge even when a second one,
    /// months later, brings the offset back to where it started.
    #[test]
    fn the_first_of_two_edges_wins() {
        let spring = 50 * DAY + 10;
        let autumn = 260 * DAY + 20;
        let offset = |t: i64| {
            Some(if (spring..autumn).contains(&t) {
                7_200
            } else {
                3_600
            })
        };
        assert_eq!(next_offset_change(0, DAY, 365, offset), Some(spring));
    }
}
