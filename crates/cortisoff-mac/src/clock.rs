//! The two clocks: one that only counts forward, one that says the hour.

use cortisoff_host::Event;

/// Milliseconds since boot, from `CLOCK_UPTIME_RAW`. It never goes
/// backwards and ignores the wall clock and the time zone. It stops while the
/// machine sleeps, like the run loop's own timeouts, so a host timer and the
/// wait that serves it count the same time.
pub fn monotonic_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid clock id and a valid out pointer.
    if unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut ts) } != 0 {
        return 0;
    }
    (ts.tv_sec as u64)
        .saturating_mul(1000)
        .saturating_add(ts.tv_nsec as u64 / 1_000_000)
}

/// The wall clock as the engine wants it, from `localtime_r`, so a daylight
/// saving change or a time zone set in System Settings shows up at the next
/// reading without the host tracking either.
pub fn time_of_day() -> Event {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid clock id and a valid out pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) };
    // SAFETY: an all-zero `tm` is a valid value to be overwritten.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call. `localtime_r` is the
    // reentrant form, safe beside anything else that reads the time zone.
    if unsafe { libc::localtime_r(&ts.tv_sec, &mut tm) }.is_null() {
        return Event::TimeOfDay {
            minute: 0,
            second: 0,
            millisecond: 0,
        };
    }
    Event::TimeOfDay {
        minute: (tm.tm_hour.clamp(0, 23) * 60 + tm.tm_min.clamp(0, 59)) as u16,
        // A leap second reads as 60; the engine wants 0 to 59.
        second: tm.tm_sec.clamp(0, 59) as u8,
        millisecond: (ts.tv_nsec / 1_000_000).clamp(0, 999) as u16,
    }
}
