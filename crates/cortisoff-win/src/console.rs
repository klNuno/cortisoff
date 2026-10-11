//! Catching Ctrl-C so the daemon can put the screen back before it dies.
//!
//! Without this the process is killed where it stands and the user is left
//! with whatever tint was applied, exactly like the crash case the journal
//! exists for. The journal would recover it on the next start, but a user who
//! pressed Ctrl-C should not have to start the thing again to get their
//! colours back.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Console::SetConsoleCtrlHandler;

static STOP: AtomicBool = AtomicBool::new(false);

/// Ask for a clean stop on Ctrl-C, Ctrl-Break and console close, and hand back
/// the flag to watch. `false` when the handler could not be installed, which
/// means the loop has to be interrupted some other way.
pub fn watch_for_stop() -> bool {
    unsafe { SetConsoleCtrlHandler(Some(handler), true) }.is_ok()
}

/// True once the user has asked the daemon to stop.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::Relaxed)
}

/// The same request from somewhere that is not a console: the tray menu, or
/// Windows asking whether the session may end.
pub fn request_stop() {
    STOP.store(true, Ordering::Relaxed);
}

/// Borrow the terminal this process was started from, if any.
///
/// A release daemon has no console of its own, so that a logon task does not
/// open a black window. Started from a terminal it would then print nowhere,
/// and `cortisoff --daemon --doctor` would look like it did nothing.
pub fn attach_parent_console() -> bool {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok()
}

/// Windows gives a console close roughly five seconds before killing the
/// process, and the handler runs on its own thread. Setting a flag and
/// returning immediately keeps the shutdown on the main thread, where the host
/// and its actuators live.
unsafe extern "system" fn handler(_kind: u32) -> BOOL {
    STOP.store(true, Ordering::Relaxed);
    true.into()
}
