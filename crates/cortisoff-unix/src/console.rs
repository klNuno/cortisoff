//! Stopping on SIGINT and SIGTERM, so the daemon puts the screen back before
//! it dies.
//!
//! The same contract as `cortisoff_win::console`: a flag the loop reads, and
//! a wake so the loop reads it now. A signal handler may only call what is
//! async-signal-safe, which rules out calling the waker from it: the waker is
//! any closure a host chose, free to lock or allocate. So the handler sets
//! the flag and writes one byte to a self-pipe, and a small thread parked on
//! the other end of the pipe calls the waker. The pipe is what makes waking
//! safe from a signal; the thread is what lets it reach a `Host::poll` that
//! knows nothing about the pipe.
//!
//! SIGHUP is left alone on purpose. A daemon started with `nohup` asked to
//! outlive its terminal, and catching SIGHUP would undo that.

use std::fs::File;
use std::io::Read;
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use cortisoff_host::Waker;

static STOP: AtomicBool = AtomicBool::new(false);

/// Write end of the self-pipe, -1 until `watch_for_stop` made one.
static PIPE: AtomicI32 = AtomicI32::new(-1);

/// What a stop wakes. Set by `SocketServer::start`, read on the signal
/// thread and by `request_stop`.
static WAKER: Mutex<Option<Waker>> = Mutex::new(None);

/// Stop cleanly on SIGINT and SIGTERM, and hand back whether that is in
/// place. `false` when the handlers or the pipe could not be set up, which
/// means the loop has to be interrupted some other way.
pub fn watch_for_stop() -> bool {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    *INSTALLED.get_or_init(install)
}

/// True once something has asked the daemon to stop.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// The same request from somewhere that is not a signal: a tray menu, or the
/// session ending. Not a signal handler, so it wakes the loop directly.
pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
    wake();
}

pub(crate) fn set_waker(waker: Waker) {
    *WAKER.lock().unwrap_or_else(PoisonError::into_inner) = Some(waker);
}

/// Call the registered waker, if any. With none, the loop sees the flag at
/// its next timeout, the same fallback the Windows pump has when its event
/// could not be created.
fn wake() {
    let waker = WAKER.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(wake) = waker {
        wake();
    }
}

fn install() -> bool {
    let Some((read, write)) = self_pipe() else {
        return false;
    };
    // SAFETY: `read` is a fresh descriptor nothing else owns; the file
    // closes it if the thread never starts.
    let pipe = unsafe { File::from_raw_fd(read) };
    let forwarding = std::thread::Builder::new()
        .name("cortisoff-signals".to_owned())
        .spawn(move || forward(pipe));
    if forwarding.is_err() {
        // SAFETY: `write` is ours and published nowhere yet.
        unsafe { libc::close(write) };
        return false;
    }
    PIPE.store(write, Ordering::SeqCst);
    let interrupt = handle(libc::SIGINT);
    let terminate = handle(libc::SIGTERM);
    interrupt && terminate
}

/// Turn every byte the handler wrote into a call to the waker. Several
/// signals in a burst may arrive as one read, which is one wake, and one is
/// all the loop needs to see the flag.
fn forward(mut pipe: File) {
    let mut bytes = [0u8; 64];
    loop {
        match pipe.read(&mut bytes) {
            Ok(0) => return,
            Ok(_) => wake(),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

/// A pipe whose descriptors do not leak into a program the daemon runs, and
/// whose write end never blocks: a pipe already holding a byte says
/// everything a second byte would.
fn self_pipe() -> Option<(i32, i32)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two descriptors.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: both descriptors were just opened by `pipe`.
    unsafe {
        for fd in fds {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let flags = libc::fcntl(fds[1], libc::F_GETFL);
        libc::fcntl(fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    Some((fds[0], fds[1]))
}

fn handle(signal: libc::c_int) -> bool {
    // SAFETY: a zeroed `sigaction` is a valid starting point, every field
    // that matters is set below, and `on_signal` only does what a handler
    // may.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // Restarted rather than failed with EINTR: a signal lands on any
        // thread, and the socket threads have no use for the interruption.
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(signal, &action, std::ptr::null_mut()) == 0
    }
}

/// An atomic store and a `write(2)`, both async-signal-safe. A successful
/// write leaves `errno` alone and a failed one only happens on a full pipe,
/// which already carries the wake, so `errno` needs no saving here.
extern "C" fn on_signal(_signal: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
    let fd = PIPE.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = 1u8;
        // SAFETY: `fd` is the pipe's write end, open for the process's life.
        unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::time::Duration;

    /// One test, because the flag and the waker are process-wide and two
    /// tests touching them in parallel would race.
    #[test]
    fn a_signal_or_a_request_sets_the_flag_and_wakes_the_loop() {
        let (woke, woken) = channel();
        set_waker(Arc::new(move || {
            let _ = woke.send(());
        }));
        assert!(watch_for_stop(), "handlers installed");

        STOP.store(false, Ordering::SeqCst);
        // SAFETY: the handler for SIGTERM is installed just above, so this
        // sets a flag instead of ending the test process.
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        woken
            .recv_timeout(Duration::from_secs(5))
            .expect("SIGTERM woke the loop");
        assert!(stop_requested());

        STOP.store(false, Ordering::SeqCst);
        request_stop();
        woken
            .recv_timeout(Duration::from_secs(5))
            .expect("request_stop woke the loop");
        assert!(stop_requested());
        STOP.store(false, Ordering::SeqCst);
    }
}
