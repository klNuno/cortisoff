//! The bridge between callbacks and the poll.
//!
//! Every observer, whatever thread it runs on, pushes a [`Raw`] here and
//! pokes the run loop. The poke is a version 0 run loop source signalled and
//! then woken, Apple's documented pattern for a custom input source: a
//! signal survives arriving before the loop starts waiting, which a bare
//! `CFRunLoopWakeUp` or `CFRunLoopStop` does not, and handling the source is
//! what makes `CFRunLoopRunInMode` return when asked to stop after one.

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use objc2_core_foundation::{
    kCFRunLoopDefaultMode, CFRetained, CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext,
};

/// What a callback saw, before the host turns it into events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raw {
    /// Another application came to the front.
    Focus,
    Lock(bool),
    DisplayChanged,
    /// The wall clock moved underneath a sleeping timer: a wake from sleep,
    /// a clock set by hand, a time zone change.
    ClockMoved,
    Power,
    Hotkey(u32),
    /// Input while the user was away, from the activity tap.
    Input,
    /// The user switched keyboard layouts.
    Layout,
}

pub struct Shared {
    queue: Mutex<Vec<Raw>>,
    woken: AtomicBool,
    run_loop: CFRetained<CFRunLoop>,
    source: CFRetained<CFRunLoopSource>,
}

// SAFETY: the run loop and the source are only touched from another thread
// through `CFRunLoopSourceSignal` and `CFRunLoopWakeUp`, which Apple documents
// as the way another thread hands work to a run loop, and through
// `CFRunLoopRemoveSource` and `CFRunLoopSourceInvalidate` in `Drop`, which
// take the run loop's own lock. Everything else is behind the mutex or atomic.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

unsafe extern "C-unwind" fn perform(_info: *mut c_void) {
    // Handling the source is the point; there is nothing to do in it.
}

impl Shared {
    /// Built on the thread whose run loop the poll runs, which is the
    /// calling thread.
    pub fn new() -> Result<Arc<Self>, String> {
        let run_loop =
            CFRunLoop::current().ok_or_else(|| "this thread has no run loop".to_owned())?;
        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: ptr::null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(perform),
        };
        // SAFETY: the context is a valid version 0 context for the duration
        // of the call, which copies it.
        let source = unsafe { CFRunLoopSource::new(None, 0, &mut context) }
            .ok_or_else(|| "CFRunLoopSourceCreate refused the wake source".to_owned())?;
        // SAFETY: reading a constant CoreFoundation exports.
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
        Ok(Arc::new(Self {
            queue: Mutex::new(Vec::new()),
            woken: AtomicBool::new(false),
            run_loop,
            source,
        }))
    }

    /// From any callback, on any thread.
    pub fn push(&self, raw: Raw) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(raw);
        self.poke();
    }

    /// From the waker, on any thread: the poll returns even with nothing
    /// queued, so the daemon gets to whatever another thread prepared.
    pub fn wake(&self) {
        self.woken.store(true, Ordering::SeqCst);
        self.poke();
    }

    fn poke(&self) {
        self.source.signal();
        self.run_loop.wake_up();
    }

    /// True when the poll has something to hand back or was asked to return.
    pub fn pending(&self) -> bool {
        self.woken.load(Ordering::SeqCst)
            || !self
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
    }

    /// Everything queued, in arrival order, and clears the wake flag.
    pub fn take(&self) -> Vec<Raw> {
        self.woken.store(false, Ordering::SeqCst);
        std::mem::take(&mut *self.queue.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// The opaque pointer a C callback gets back. Valid while the host holds
    /// its `Arc`, which outlives every registration made with it.
    pub fn as_info(self: &Arc<Self>) -> *mut c_void {
        Arc::as_ptr(self).cast_mut().cast()
    }

    /// The other side of [`Shared::as_info`].
    ///
    /// # Safety
    ///
    /// `info` must come from `as_info` on a `Shared` that is still alive.
    pub unsafe fn from_info<'a>(info: *mut c_void) -> &'a Shared {
        // SAFETY: the caller's contract.
        unsafe { &*info.cast::<Shared>() }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: reading a constant CoreFoundation exports.
        self.run_loop
            .remove_source(Some(&self.source), unsafe { kCFRunLoopDefaultMode });
        self.source.invalidate();
    }
}
