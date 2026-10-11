//! What ends a blocked poll besides the X server: another thread, a due
//! timer, and the wall clock being set.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;

use rustix::io::Errno;
use rustix::pipe::{pipe_with, PipeFlags};
use rustix::time::{
    timerfd_create, timerfd_settime, Itimerspec, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags,
    Timespec,
};

/// A self-pipe. Any thread writes a byte, the poll sees the read end ready.
pub struct WakePipe {
    read: OwnedFd,
    write: Arc<OwnedFd>,
}

impl WakePipe {
    pub fn new() -> Result<Self, String> {
        let (read, write) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)
            .map_err(|e| format!("could not create the wake pipe: {e}"))?;
        Ok(Self {
            read,
            write: Arc::new(write),
        })
    }

    /// Callable from any thread, any number of times. A full pipe already
    /// holds a wakeup, so a refused write loses nothing.
    pub fn waker(&self) -> cortisoff_host::Waker {
        let write = Arc::clone(&self.write);
        Arc::new(move || {
            let _ = rustix::io::write(&*write, &[1]);
        })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.read.as_fd()
    }

    /// Empty the pipe so the next poll blocks again, and say whether anyone
    /// knocked. Many wakes before one poll are one wakeup.
    pub fn drain(&self) -> bool {
        let mut sink = [0u8; 64];
        let mut knocked = false;
        while let Ok(n) = rustix::io::read(&self.read, &mut sink) {
            knocked |= n > 0;
            if n < sink.len() {
                break;
            }
        }
        knocked
    }
}

/// A timer on `CLOCK_BOOTTIME`, armed at the host's next wake.
///
/// `poll(2)` measures its timeout on a clock that stops during a suspend, so
/// a timeout alone would oversleep by however long the lid was closed. This
/// timer is already expired when the machine wakes up, and the poll returns.
pub struct Alarm {
    fd: OwnedFd,
}

impl Alarm {
    pub fn new() -> Option<Self> {
        timerfd_create(
            TimerfdClockId::Boottime,
            TimerfdFlags::CLOEXEC | TimerfdFlags::NONBLOCK,
        )
        .ok()
        .map(|fd| Self { fd })
    }

    /// Arm at `at_ms` on the boot clock, or disarm with `None`.
    pub fn arm(&self, at_ms: Option<u64>) {
        let value = match at_ms {
            // Zero disarms a timerfd, so a deadline at the very start of the
            // clock is moved one nanosecond on.
            Some(ms) => Timespec {
                tv_sec: (ms / 1000) as i64,
                tv_nsec: ((ms % 1000) * 1_000_000).max(1) as _,
            },
            None => zero(),
        };
        let _ = timerfd_settime(
            &self.fd,
            TimerfdTimerFlags::ABSTIME,
            &Itimerspec {
                it_interval: zero(),
                it_value: value,
            },
        );
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    pub fn drain(&self) {
        let mut count = [0u8; 8];
        let _ = rustix::io::read(&self.fd, &mut count);
    }
}

/// Reports the wall clock being set: by hand, by NTP stepping it, or by
/// anything else calling `settimeofday`. Each one is a moment the engine's
/// schedule may have been armed from the wrong time.
pub struct ClockWatch {
    fd: OwnedFd,
}

/// 2200-01-01. Far enough that the timer never expires on its own; it is
/// only there to be cancelled.
const NEVER: i64 = 7_258_118_400;

impl ClockWatch {
    pub fn new() -> Option<Self> {
        let fd = timerfd_create(
            TimerfdClockId::Realtime,
            TimerfdFlags::CLOEXEC | TimerfdFlags::NONBLOCK,
        )
        .ok()?;
        let watch = Self { fd };
        watch.arm().then_some(watch)
    }

    fn arm(&self) -> bool {
        timerfd_settime(
            &self.fd,
            TimerfdTimerFlags::ABSTIME | TimerfdTimerFlags::CANCEL_ON_SET,
            &Itimerspec {
                it_interval: zero(),
                it_value: Timespec {
                    tv_sec: NEVER,
                    tv_nsec: 0,
                },
            },
        )
        .is_ok()
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// True when the clock was set since the last call. The timer is armed
    /// again, because a cancelled one reports nothing more.
    pub fn moved(&self) -> bool {
        let mut count = [0u8; 8];
        match rustix::io::read(&self.fd, &mut count) {
            Err(Errno::CANCELED) => {
                self.arm();
                true
            }
            _ => false,
        }
    }
}

fn zero() -> Timespec {
    Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    }
}
