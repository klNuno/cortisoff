//! Waiting for a daemon to start listening, for the window.
//!
//! Windows parks the window on a named event the daemon sets once its pipe
//! listens. A unix socket has no such signal, so on Linux the window watches
//! the socket's directory with inotify instead: the daemon's `bind` creates
//! the file and its `chmod` to 0600 follows `listen`, so that second event
//! means a connect will be accepted. macOS has no inotify, and a kqueue on the
//! directory only hears the file appear, a moment before it listens; it
//! keeps the short retry.

use std::time::Duration;

/// How long one inotify wait lasts at most. A watch that misses its event, a
/// directory that is replaced, costs this much and no more.
#[cfg(any(target_os = "linux", target_os = "android"))]
const LIMIT: Duration = Duration::from_secs(30);

/// Where inotify cannot be used, how often the socket is tried again.
const RETRY: Duration = Duration::from_millis(700);

/// Block until a daemon may be listening on this user's socket. The caller
/// tries to connect afterwards, and waits again if it was not one.
pub fn wait_for_daemon() {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if linux::wait(LIMIT) {
        return;
    }
    std::thread::sleep(RETRY);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux {
    use std::ffi::{CString, OsStr};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use crate::paths;

    /// False when the directory cannot be watched, and the caller falls back
    /// to sleeping.
    pub fn wait(limit: Duration) -> bool {
        let layout = paths::layout();
        let socket = layout.socket();
        let Some(name) = socket.file_name() else {
            return false;
        };
        let Some(watch) = Watch::new(&layout.dir) else {
            return false;
        };
        // The daemon may have started between the caller's failed connect
        // and the watch. Asked after the watch exists, so nothing is lost.
        // The same connect as the caller's, owner check included, so a socket
        // the caller refuses cannot turn this into a spin.
        if crate::socket::connect().is_some() {
            return true;
        }
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            match watch.next(left) {
                Some(names) if names.iter().any(|n| n.as_os_str() == name) => return true,
                Some(_) => {}
                None => return true,
            }
        }
    }

    struct Watch {
        fd: OwnedFd,
    }

    impl Watch {
        fn new(dir: &Path) -> Option<Self> {
            let path = CString::new(dir.as_os_str().as_bytes()).ok()?;
            // SAFETY: plain system calls. The descriptor is owned from here
            // on and closed with the watch.
            let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
            if fd < 0 {
                return None;
            }
            // SAFETY: `fd` is a fresh descriptor nothing else owns.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let mask = libc::IN_CREATE | libc::IN_ATTRIB | libc::IN_MOVED_TO;
            // SAFETY: `path` is a valid C string for the duration of the call.
            let added = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), mask) };
            (added >= 0).then_some(Self { fd })
        }

        /// The names events arrived for, within `limit`. `None` on a timeout
        /// or an error, which the caller treats as a reason to look again.
        fn next(&self, limit: Duration) -> Option<Vec<std::path::PathBuf>> {
            let mut pfd = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = limit.as_millis().min(i32::MAX as u128) as i32;
            // SAFETY: one valid pollfd for the duration of the call.
            let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
            if ready <= 0 {
                return None;
            }
            let mut buf = [0u8; 4096];
            // SAFETY: `buf` is valid for its whole length.
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return Some(Vec::new());
            }
            Some(names(&buf[..n as usize]))
        }
    }

    /// The names in a buffer of `inotify_event` records.
    fn names(mut buf: &[u8]) -> Vec<std::path::PathBuf> {
        const HEADER: usize = std::mem::size_of::<libc::inotify_event>();
        let mut out = Vec::new();
        while buf.len() >= HEADER {
            // SAFETY: at least HEADER bytes remain; read unaligned because
            // the kernel packs records back to back in a byte buffer.
            let event: libc::inotify_event =
                unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
            let end = HEADER + event.len as usize;
            if end > buf.len() {
                break;
            }
            let raw = &buf[HEADER..end];
            let name = raw.split(|b| *b == 0).next().unwrap_or_default();
            if !name.is_empty() {
                out.push(OsStr::from_bytes(name).into());
            }
            buf = &buf[end..];
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::scratch::Scratch;

        #[test]
        fn a_socket_appearing_ends_the_wait() {
            let dir = Scratch::new("ready");
            let watch = Watch::new(dir.path()).expect("inotify watch");
            let path = dir.join("cortisoff.sock");
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            let names = watch.next(Duration::from_secs(5)).expect("an event");
            assert!(names.iter().any(|n| n.as_os_str() == "cortisoff.sock"));
            drop(listener);
        }

        #[test]
        fn nothing_happening_times_out() {
            let dir = Scratch::new("quiet");
            let watch = Watch::new(dir.path()).expect("inotify watch");
            assert!(watch.next(Duration::from_millis(50)).is_none());
        }
    }
}
