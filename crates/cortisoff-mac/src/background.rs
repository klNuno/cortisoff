//! Keeping the daemon out of the Dock.
//!
//! The daemon runs from the app bundle, and the bundle's `Info.plist` makes
//! a regular app of it: the window needs its Dock icon. A regular process
//! that reaches the window server (a Carbon hotkey, a CoreGraphics call)
//! becomes a foreground app, with a Dock icon that opens nothing, a place in
//! Cmd-Tab, and loginwindow's quit event at logout, which ended the daemon
//! before it could hand the screens back. A prohibited activation policy,
//! set before any of that, makes it a background process instead. Only the
//! daemon path calls this, so the window keeps its icon.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

/// Make this process a background one for the rest of its life, and say
/// whether it now is. `false` off the main thread, where `NSApplication` may
/// not be touched, or when macOS kept another policy.
///
/// The policy the process ends with decides, not the setter's answer: the
/// setter has said NO on a CI runner.
pub fn background_only() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    app.activationPolicy() == NSApplicationActivationPolicy::Prohibited
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test threads are never the main thread: nothing is touched there.
    /// `tests/background.rs` runs it on the main thread.
    #[test]
    fn off_the_main_thread_it_does_nothing() {
        assert!(MainThreadMarker::new().is_none());
        assert!(!background_only());
    }
}
