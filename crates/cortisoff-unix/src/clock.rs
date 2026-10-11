//! The wall clock, local, for the plugins' clock events and the
//! daemon's log, so the two never disagree about the hour across a
//! daylight-saving change.

use std::time::{SystemTime, UNIX_EPOCH};

use cortisoff_host::Event;

extern "C" {
    /// Declared here because the libc crate carries it for no unix target.
    /// Present in glibc, musl and libSystem alike.
    fn tzset();
}

/// The wall clock, local, as year, month, day, hour, minute, second and
/// millisecond: the same tuple and integer types as
/// `cortisoff_win::local_time`.
///
/// `tzset` before every reading, because `localtime_r` is not required to
/// look at the zone again once it has read it, and glibc does not: without
/// it a timezone changed while the daemon runs would never reach the
/// schedule. It costs one `stat` of the zone file.
pub fn local_time() -> (u16, u8, u8, u8, u8, u8, u16) {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = since.as_secs() as libc::time_t;
    // SAFETY: an all-zero `tm` is a valid value to be overwritten.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `seconds` and `tm` are live for both calls. UTC only if the
    // local conversion fails, which takes a time far outside any calendar.
    unsafe {
        tzset();
        if libc::localtime_r(&seconds, &mut tm).is_null() {
            libc::gmtime_r(&seconds, &mut tm);
        }
    }
    (
        (tm.tm_year + 1900) as u16,
        (tm.tm_mon + 1) as u8,
        tm.tm_mday as u8,
        tm.tm_hour as u8,
        tm.tm_min as u8,
        tm.tm_sec as u8,
        since.subsec_millis() as u16,
    )
}

/// The wall clock as the engine reads it. Both unix hosts answer
/// `Host::time_of_day` and their clock events with this. A leap second reads
/// as the 59th, since the engine counts sixty to a minute.
pub fn time_of_day_event() -> Event {
    let (_, _, _, hour, minute, second, millisecond) = local_time();
    Event::TimeOfDay {
        minute: u16::from(hour) * 60 + u16::from(minute),
        second: second.min(59),
        millisecond,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_time_is_a_real_date() {
        let (year, month, day, hour, minute, second, millisecond) = local_time();
        assert!(year >= 2024, "{year}");
        assert!((1..=12).contains(&month));
        assert!((1..=31).contains(&day));
        assert!(hour < 24);
        assert!(minute < 60);
        assert!(second <= 60);
        assert!(millisecond < 1000);
    }

    #[test]
    fn the_clock_event_counts_minutes_of_the_day() {
        let Event::TimeOfDay {
            minute,
            second,
            millisecond,
        } = time_of_day_event()
        else {
            panic!("not a clock event");
        };
        assert!(minute < 24 * 60);
        assert!(second < 60);
        assert!(millisecond < 1000);
    }
}
