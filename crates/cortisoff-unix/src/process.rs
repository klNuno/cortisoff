//! The daemon's own process: once per session, and background work as far as
//! the scheduler is concerned. The window borrows the once-per-session part.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::paths;

/// Held for as long as this process is the session's daemon, or its window.
/// The lock goes with the descriptor, so a daemon that is killed releases it
/// on the spot and the next one starts without anyone cleaning up.
pub struct InstanceLock {
    _file: File,
}

/// `None` when another daemon already runs for this user, or when the
/// directory beside the socket is refused, which is no place to run from
/// either.
///
/// Two daemons would each capture the other's tint as a baseline and fight
/// over the screen forever. It is also what makes replacing a stale socket
/// safe: only the holder of this lock ever touches the socket path.
pub fn claim_session() -> Option<InstanceLock> {
    claim("")
}

/// `None` when the window is already open for this user. The second launch
/// is never what the person clicking wanted.
pub fn claim_dashboard() -> Option<InstanceLock> {
    claim("dashboard")
}

fn claim(role: &str) -> Option<InstanceLock> {
    let layout = paths::layout();
    paths::prepare(&layout, paths::own_uid()).ok()?;
    claim_at(&layout.lock(role))
}

/// Ask the open window to quit, for `--uninstall` run from a terminal: left
/// open, it keeps running from the app the removal deletes and says the
/// daemon is not running, as if it could simply be started again. The window
/// has no handler for SIGTERM, so it ends at once and its lock with it.
/// `false` when no window holds the lock.
pub fn close_dashboard() -> bool {
    let path = paths::layout().lock("dashboard");
    signal_holder(&path)
}

fn signal_holder(path: &Path) -> bool {
    if claim_at(path).is_some() {
        return false;
    }
    let pid = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<libc::pid_t>().ok());
    match pid {
        // SAFETY: plain integer arguments.
        Some(pid) if pid > 1 && pid as u32 != std::process::id() => unsafe {
            libc::kill(pid, libc::SIGTERM) == 0
        },
        _ => false,
    }
}

/// An exclusive `flock` on `path`, never waited for, with the holder's pid
/// written in the file for [`close_dashboard`]. `flock` belongs to the open
/// file, not to the process, so a second claim from the same process is
/// refused just like one from another.
pub(crate) fn claim_at(path: &Path) -> Option<InstanceLock> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .ok()?;
    // SAFETY: the descriptor is open for as long as `file` lives.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        return None;
    }
    // Only the holder writes, and only once it holds the lock.
    if file.set_len(0).is_ok() {
        let _ = file.write_all(std::process::id().to_string().as_bytes());
    }
    Some(InstanceLock { _file: file })
}

/// The unix stand-in for EcoQoS: nice 10, so anything a person is doing goes
/// first, and on Linux the lowest best-effort I/O priority. The daemon's work
/// is a few hundred microseconds when something changes and nothing
/// otherwise, so nothing it does notices.
///
/// Best-effort level 7 rather than the idle I/O class: idle I/O can wait as
/// long as anything else keeps the disk busy, and the baseline journal has to
/// reach the disk before the apply it protects, so a game loading would hold
/// up the very change its focus asked for.
pub fn efficiency_mode() -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // Linux keeps nice and I/O priority per thread, so every thread that
        // already exists is lowered, as far as it still exists: one may end
        // between the listing and the call, and needs nothing then. The
        // answer is the calling thread's, since every thread started from
        // here on inherits it.
        let threads: Vec<libc::id_t> = std::fs::read_dir("/proc/self/task")
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        for thread in threads {
            let _ = lower(thread);
            lower_io(thread);
        }
        lower_io(0);
        lower(0)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        lower(0)
    }
}

/// Nice 10 for `who`, the calling process or thread when 0. Never raises a
/// priority someone already lowered further.
fn lower(who: libc::id_t) -> bool {
    const NICE: libc::c_int = 10;
    // SAFETY: plain integer arguments. A -1 from `getpriority` may be an
    // error or a real nice of -1, and either way the answer is to set 10.
    unsafe {
        libc::getpriority(libc::PRIO_PROCESS, who) >= NICE
            || libc::setpriority(libc::PRIO_PROCESS, who, NICE) == 0
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn lower_io(who: libc::id_t) {
    const IOPRIO_WHO_PROCESS: libc::c_long = 1;
    const IOPRIO_CLASS_BE: libc::c_long = 2;
    const IOPRIO_CLASS_SHIFT: libc::c_long = 13;
    const LOWEST: libc::c_long = 7;
    // SAFETY: a raw syscall with integer arguments; a kernel without it
    // answers ENOSYS and the priority simply stays where it was.
    unsafe {
        libc::syscall(
            libc::SYS_ioprio_set,
            IOPRIO_WHO_PROCESS,
            who as libc::c_long,
            (IOPRIO_CLASS_BE << IOPRIO_CLASS_SHIFT) | LOWEST,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn a_second_claim_is_refused_while_the_first_lives() {
        let scratch = Scratch::new("lock");
        let path = scratch.join("cortisoff.lock");

        let first = claim_at(&path).expect("first claim");
        assert!(claim_at(&path).is_none(), "second claim refused");
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);

        drop(first);
        assert!(claim_at(&path).is_some(), "claim free again once dropped");
    }

    #[test]
    fn the_lock_holder_is_asked_to_quit_and_nobody_else() {
        use std::os::unix::process::ExitStatusExt;

        let scratch = Scratch::new("holder");
        let path = scratch.join("dashboard.lock");
        assert!(!signal_holder(&path), "a free lock has no holder");

        let held = claim_at(&path).expect("claim");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );
        assert!(!signal_holder(&path), "this process never signals itself");

        // Another process as the holder: its pid in the file.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        std::fs::write(&path, child.id().to_string()).unwrap();
        assert!(signal_holder(&path));
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
        drop(held);
    }

    #[test]
    fn efficiency_mode_lowers_this_process() {
        // A thread that exists before the call, parked until it is checked.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let (parked, release) = {
            let (told, tid) = std::sync::mpsc::channel();
            let (release, wait) = std::sync::mpsc::channel::<()>();
            let parked = std::thread::spawn(move || {
                // SAFETY: no argument and no failure mode.
                told.send(unsafe { libc::gettid() }).unwrap();
                let _ = wait.recv();
            });
            (tid.recv().unwrap(), (release, parked))
        };

        assert!(efficiency_mode());
        let nice = |who: libc::id_t| {
            // SAFETY: plain integer arguments.
            unsafe { libc::getpriority(libc::PRIO_PROCESS, who) }
        };
        assert!(nice(0) >= 10, "nice {}", nice(0));

        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let other = nice(parked as libc::id_t);
            release.0.send(()).unwrap();
            release.1.join().unwrap();
            assert!(other >= 10, "a thread started earlier kept nice {other}");
        }
    }
}
