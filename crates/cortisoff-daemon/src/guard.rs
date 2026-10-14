//! Starting the daemon again after a crash, with nothing polling for it.
//!
//! `cortisoff --daemon` is two processes. The first, the guard, starts the
//! second with `--no-guard` and every other option passed through, then waits
//! for it to end: one handle parked in the kernel, no timer. A daemon that ends
//! on its own (a stop from the window or the CLI, the tray's Quit, the session
//! ending, another daemon already running) ends the guard with it. One that
//! crashed is started again, and so is one that ends with [`RESTART`]: it
//! has just put a new build in place of the executable.
//!
//! A stop sent to the guard itself (launchd at logout, `launchctl bootout`,
//! systemd, `kill`) is passed on to the daemon on unix, and the daemon it
//! ends is never started again, however it ended.
//!
//! The logon task used to cover crashes by trying to start the daemon every
//! five minutes. It could not tell a crash from a stop, so every stop the user
//! asked for was undone within five minutes.

use std::process::ExitStatus;
use std::time::{Duration, Instant};

/// A daemon that crashes sooner than this after starting counts toward giving
/// up: something about this machine or this build kills it at once.
const QUICK: Duration = Duration::from_secs(60);

/// Quick crashes in a row before the guard stops starting it again.
const GIVE_UP_AFTER: u32 = 3;

/// The exit code of a daemon that installed an update and wants the new
/// build started. 75 is `EX_TEMPFAIL`: nothing else in cortisoff exits with
/// it, and it sits far from the NTSTATUS range a crash uses.
pub const RESTART: i32 = 75;

/// Run `start` until it ends without crashing, and hand back how it ended.
/// `note` hears one line per crash, for the daemon's log. `pause` is the
/// breath between a crash and the next start. Once `stopped` says the guard
/// was asked to stop, the daemon that ends is the last one.
///
/// A daemon that ends with [`RESTART`] is started again at once: no note, no
/// pause, and it does not count toward giving up. `start` runs the path it
/// always ran, which now holds the new build.
pub fn supervise(
    mut start: impl FnMut() -> std::io::Result<ExitStatus>,
    mut note: impl FnMut(&str),
    pause: Duration,
    stopped: impl Fn() -> bool,
) -> Result<ExitStatus, String> {
    let mut quick = 0;
    loop {
        let began = Instant::now();
        let status = start().map_err(|e| format!("could not start the daemon: {e}"))?;
        // Ended by the stop the guard passed on, or by a crash on its way
        // out: either way the user wanted it gone.
        if stopped() {
            return Ok(status);
        }
        if status.code() == Some(RESTART) {
            continue;
        }
        let Some(how) = crash(&status) else {
            return Ok(status);
        };
        // A crash after a long run says nothing about the next start.
        quick = if began.elapsed() < QUICK {
            quick + 1
        } else {
            0
        };
        if quick >= GIVE_UP_AFTER {
            note(&format!(
                "the daemon crashed ({how}), {quick} times in a row within a minute of starting. \
                 It starts again at the next logon or from the window"
            ));
            return Ok(status);
        }
        note(&format!("the daemon crashed ({how}), starting it again"));
        std::thread::sleep(pause);
        if stopped() {
            return Ok(status);
        }
    }
}

/// How the process crashed, or `None` when it ended on its own.
///
/// On Windows a crash ends the process with an NTSTATUS error code: an access
/// violation, a stack overflow, or the fail-fast a Rust panic aborts with.
/// Two codes in that range are someone's decision instead:
/// `STATUS_CONTROL_C_EXIT`, and -1, which `Stop-Process` and .NET's
/// `Process.Kill` hand to `TerminateProcess`. A plain exit code, `taskkill /F`
/// included, is a decision too.
#[cfg(windows)]
pub fn crash(status: &ExitStatus) -> Option<String> {
    const CONTROL_C_EXIT: u32 = 0xC000_013A;
    const KILLED: u32 = u32::MAX;
    let code = status.code()? as u32;
    (code >= 0xC000_0000 && code != CONTROL_C_EXIT && code != KILLED)
        .then(|| format!("{code:#010X}"))
}

/// On unix a crash is a signal the process raised on itself; SIGTERM, SIGINT,
/// SIGHUP and SIGKILL come from someone who wanted it gone.
#[cfg(unix)]
pub fn crash(status: &ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    #[cfg(target_os = "linux")]
    const BUS_SYS: [i32; 2] = [7, 31];
    #[cfg(not(target_os = "linux"))]
    const BUS_SYS: [i32; 2] = [10, 12];
    let signal = status.signal()?;
    // ILL, TRAP, ABRT, FPE and SEGV share their numbers everywhere.
    ([4, 5, 6, 8, 11].contains(&signal) || BUS_SYS.contains(&signal))
        .then(|| format!("signal {signal}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A process that ends with this exit code, or on unix this signal.
    #[cfg(windows)]
    fn ending(code: i64) -> std::io::Result<ExitStatus> {
        Command::new("cmd")
            .args(["/C", &format!("exit {code}")])
            .status()
    }

    #[cfg(windows)]
    const CRASHED: i64 = 0xC000_0005_u32 as i32 as i64;

    #[cfg(unix)]
    fn ending(signal: i64) -> std::io::Result<ExitStatus> {
        let script = if signal == 0 {
            "exit 0".to_owned()
        } else {
            format!("kill -{signal} $$")
        };
        Command::new("sh").args(["-c", &script]).status()
    }

    #[cfg(unix)]
    const CRASHED: i64 = 11;

    /// A process that exits with this code, signals aside.
    #[cfg(windows)]
    fn exiting(code: i64) -> std::io::Result<ExitStatus> {
        ending(code)
    }

    #[cfg(unix)]
    fn exiting(code: i64) -> std::io::Result<ExitStatus> {
        Command::new("sh")
            .args(["-c", &format!("exit {code}")])
            .status()
    }

    #[test]
    fn a_crash_is_started_again_and_a_clean_end_is_not() {
        let mut runs = 0;
        let mut notes = Vec::new();
        let status = supervise(
            || {
                runs += 1;
                ending(if runs == 1 { CRASHED } else { 0 })
            },
            |line| notes.push(line.to_owned()),
            Duration::ZERO,
            || false,
        )
        .unwrap();
        assert!(status.success());
        assert_eq!(runs, 2);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("starting it again"), "{notes:?}");
    }

    #[test]
    fn a_daemon_that_keeps_crashing_at_once_is_left_alone() {
        let mut runs = 0;
        let mut notes = Vec::new();
        let status = supervise(
            || {
                runs += 1;
                ending(CRASHED)
            },
            |line| notes.push(line.to_owned()),
            Duration::ZERO,
            || false,
        )
        .unwrap();
        assert!(!status.success());
        assert_eq!(runs, GIVE_UP_AFTER);
        assert!(notes.last().unwrap().contains("in a row"), "{notes:?}");
    }

    /// A stop, a refusal and a kill are someone's decision: no restart.
    #[test]
    fn an_exit_code_is_not_a_crash() {
        for code in [0, 1, 2] {
            let mut runs = 0;
            let status = supervise(
                || {
                    runs += 1;
                    exiting(code)
                },
                |_| {},
                Duration::ZERO,
                || false,
            )
            .unwrap();
            assert_eq!(runs, 1, "exit {code}");
            assert_eq!(status.success(), code == 0);
        }
    }

    /// An update ends the daemon with `RESTART`: started again at once, with
    /// no note, and not counted toward giving up. Two quick crashes, an
    /// update, then a clean end: counted, the update would have been the
    /// third quick crash and the guard would have stopped there.
    #[test]
    fn an_update_restart_starts_it_again_without_counting() {
        let mut runs = 0;
        let mut notes = Vec::new();
        let status = supervise(
            || {
                runs += 1;
                match runs {
                    1 | 2 => ending(CRASHED),
                    3 => exiting(RESTART.into()),
                    _ => exiting(0),
                }
            },
            |line| notes.push(line.to_owned()),
            Duration::ZERO,
            || false,
        )
        .unwrap();
        assert!(status.success());
        assert_eq!(runs, 4);
        assert_eq!(notes.len(), 2, "one note per crash, none for the update");
        assert!(
            notes.iter().all(|n| n.contains("starting it again")),
            "{notes:?}"
        );
    }

    /// A stop passed on to the daemon may end it with a crash code, a kill
    /// after the grace period or a clean exit: none is started again, and
    /// neither is a crashed daemon whose pause the stop came in during.
    #[test]
    fn nothing_is_started_again_after_a_stop() {
        for ended in [CRASHED, 0] {
            let mut runs = 0;
            let mut notes = Vec::new();
            supervise(
                || {
                    runs += 1;
                    ending(ended)
                },
                |line| notes.push(line.to_owned()),
                Duration::ZERO,
                || true,
            )
            .unwrap();
            assert_eq!(runs, 1, "ended with {ended}");
            assert!(notes.is_empty(), "{notes:?}");
        }

        let stop = std::cell::Cell::new(false);
        let mut runs = 0;
        supervise(
            || {
                runs += 1;
                ending(CRASHED)
            },
            |_| stop.set(true),
            Duration::ZERO,
            || stop.get(),
        )
        .unwrap();
        assert_eq!(runs, 1, "the stop came in during the pause");

        let mut runs = 0;
        supervise(
            || {
                runs += 1;
                exiting(RESTART.into())
            },
            |_| {},
            Duration::ZERO,
            || true,
        )
        .unwrap();
        assert_eq!(runs, 1, "a stop wins over an update restart");
    }

    /// Someone else's signal ends the daemon: a stop, not a crash.
    #[cfg(unix)]
    #[test]
    fn stop_signals_are_not_crashes() {
        for signal in [1, 2, 9, 15] {
            let status = ending(signal).unwrap();
            assert_eq!(crash(&status), None, "signal {signal}");
        }
        assert!(crash(&ending(CRASHED).unwrap()).is_some());
    }

    #[test]
    fn restart_is_not_a_crash() {
        let status = exiting(RESTART.into()).unwrap();
        assert_eq!(status.code(), Some(RESTART));
        assert_eq!(crash(&status), None);
    }

    #[cfg(windows)]
    #[test]
    fn ctrl_c_and_stop_process_are_not_crashes_and_a_fail_fast_is() {
        let ctrl_c = ending(0xC000_013A_u32 as i32 as i64).unwrap();
        assert_eq!(crash(&ctrl_c), None);
        let stop_process = ending(-1).unwrap();
        assert_eq!(crash(&stop_process), None);
        let fail_fast = ending(0xC000_0409_u32 as i32 as i64).unwrap();
        assert_eq!(crash(&fail_fast).as_deref(), Some("0xC0000409"));
    }
}
