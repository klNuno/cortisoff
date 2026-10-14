//! The end of an absence as an event: a listen-only event tap, installed
//! only while the user is away and removed at the first input.
//!
//! macOS has no "input resumed" notification. `CGEventSource` answers how
//! long since the last input but never calls back, so without this the host
//! sampled it every second for as long as the user was away. The tap is the
//! twin of the raw input the Windows host registers while away: it wakes
//! the loop once, at the first mouse move or key, and is gone after it.
//!
//! A listen-only tap on mouse events needs no permission. Keyboard events
//! need Input Monitoring (or Accessibility); without either the tap watches
//! the mouse alone and the host keeps a short recheck for the keyboard.

use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;

use crate::runloop::{Raw, Shared};

type MachPort = *mut c_void;
type RunLoopSource = *mut c_void;
type EventTapProxy = *mut c_void;
type EventRef = *mut c_void;
type TapCallback = unsafe extern "C" fn(EventTapProxy, u32, EventRef, *mut c_void) -> EventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> MachPort;
    fn CGEventTapEnable(tap: MachPort, enable: bool);
    fn CGPreflightListenEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: MachPort,
        order: isize,
    ) -> RunLoopSource;
    fn CFMachPortInvalidate(port: MachPort);
    fn CFRunLoopGetMain() -> *mut c_void;
    fn CFRunLoopAddSource(run_loop: *mut c_void, source: RunLoopSource, mode: *const c_void);
    fn CFRunLoopRemoveSource(run_loop: *mut c_void, source: RunLoopSource, mode: *const c_void);
    fn CFRunLoopSourceInvalidate(source: RunLoopSource);
    fn CFRelease(cf: *const c_void);
    static kCFRunLoopDefaultMode: *const c_void;
}

/// `kCGSessionEventTap`, `kCGHeadInsertEventTap`, `kCGEventTapOptionListenOnly`.
const SESSION_TAP: u32 = 1;
const HEAD_INSERT: u32 = 0;
const LISTEN_ONLY: u32 = 1;

/// `CGEventType` values: the buttons, moves, drags and the wheel.
const MOUSE: [u32; 10] = [1, 2, 3, 4, 5, 6, 7, 22, 25, 27];
/// Key down and modifier changes.
const KEYS: [u32; 2] = [10, 12];

fn mask(types: &[u32]) -> u64 {
    types.iter().fold(0, |m, t| m | (1u64 << t))
}

unsafe extern "C" fn on_input(
    _proxy: EventTapProxy,
    _kind: u32,
    event: EventRef,
    info: *mut c_void,
) -> EventRef {
    // Any call, a disabled-by-timeout notice included, means the host should
    // look again; it removes the tap and samples the real idle time.
    // SAFETY: `info` is the `Shared` the tap was created with, kept alive by
    // the `InputWatch` that removes the tap before letting it go.
    unsafe { Shared::from_info(info) }.push(Raw::Input);
    event
}

/// A listen-only tap, for as long as this lives.
pub struct InputWatch {
    _shared: Arc<Shared>,
    port: MachPort,
    source: RunLoopSource,
    keys: bool,
}

impl InputWatch {
    /// `None` when WindowServer refuses the tap, which leaves the host on
    /// its one second recheck.
    pub fn new(shared: Arc<Shared>) -> Option<Self> {
        // SAFETY: a plain query, no prompt.
        let keys = unsafe { CGPreflightListenEventAccess() } || crate::ax::trusted();
        let interest = if keys {
            mask(&MOUSE) | mask(&KEYS)
        } else {
            mask(&MOUSE)
        };
        // SAFETY: a callback of the documented type and an info pointer that
        // outlives the tap (`Drop` invalidates it first).
        let port = unsafe {
            CGEventTapCreate(
                SESSION_TAP,
                HEAD_INSERT,
                LISTEN_ONLY,
                interest,
                on_input,
                shared.as_info(),
            )
        };
        if port.is_null() {
            return None;
        }
        // SAFETY: a valid port from the call above.
        let source = unsafe { CFMachPortCreateRunLoopSource(ptr::null(), port, 0) };
        if source.is_null() {
            // SAFETY: the port created above, released once.
            unsafe {
                CFMachPortInvalidate(port);
                CFRelease(port);
            }
            return None;
        }
        // SAFETY: the main run loop, which the poll runs, a source just
        // made, and a mode constant CoreFoundation exports.
        unsafe {
            CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopDefaultMode);
            CGEventTapEnable(port, true);
        }
        Some(Self {
            _shared: shared,
            port,
            source,
            keys,
        })
    }

    /// Whether key presses reach the tap, or the mouse alone.
    pub fn sees_keys(&self) -> bool {
        self.keys
    }
}

impl Drop for InputWatch {
    fn drop(&mut self) {
        // SAFETY: the port and source `new` made, each torn down once and in
        // this order, so no callback runs after the info pointer is gone.
        unsafe {
            CGEventTapEnable(self.port, false);
            CFRunLoopRemoveSource(CFRunLoopGetMain(), self.source, kCFRunLoopDefaultMode);
            CFRunLoopSourceInvalidate(self.source);
            CFMachPortInvalidate(self.port);
            CFRelease(self.source);
            CFRelease(self.port);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_masks_name_the_event_types() {
        // kCGEventLeftMouseDown and kCGEventMouseMoved.
        assert_ne!(mask(&MOUSE) & (1 << 1), 0);
        assert_ne!(mask(&MOUSE) & (1 << 5), 0);
        // kCGEventScrollWheel.
        assert_ne!(mask(&MOUSE) & (1 << 22), 0);
        // Key down stays out of the mouse mask: it needs a permission.
        assert_eq!(mask(&MOUSE) & (1 << 10), 0);
        assert_eq!(mask(&KEYS), (1 << 10) | (1 << 12));
    }
}
