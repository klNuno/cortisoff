//! The event sources: notification observers, the display reconfiguration
//! callback and the power source notification, each pushing a [`Raw`] into
//! the shared queue the poll drains.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2_core_foundation::{
    kCFRunLoopDefaultMode, CFRetained, CFRunLoop, CFRunLoopSource, CFString,
};
use objc2_core_graphics::{
    CGDisplayChangeSummaryFlags, CGDisplayRegisterReconfigurationCallback,
    CGDisplayRemoveReconfigurationCallback, CGError, CGSessionCopyCurrentDictionary,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSNotificationName};
use objc2_io_kit::{
    IOPSCopyPowerSourcesInfo, IOPSGetProvidingPowerSourceType, IOPSNotificationCreateRunLoopSource,
};

use crate::runloop::{Raw, Shared};

type Token = Retained<ProtocolObject<dyn NSObjectProtocol>>;

/// Block observers on one or more notification centres, removed on drop.
#[derive(Default)]
pub struct Observers {
    tokens: Vec<(Retained<NSNotificationCenter>, Token)>,
}

impl Observers {
    /// Pushes `raw` whenever `center` posts `name`. The block runs on
    /// whatever thread posts, which the queue is built for.
    pub fn observe(
        &mut self,
        center: Retained<NSNotificationCenter>,
        name: &NSNotificationName,
        shared: &Arc<Shared>,
        raw: Raw,
    ) {
        let shared = Arc::clone(shared);
        let block = RcBlock::new(move |_: NonNull<NSNotification>| shared.push(raw));
        // SAFETY: no object filter and no queue, so the block runs where the
        // notification is posted; the block only touches thread-safe state.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
        };
        self.tokens.push((center, token));
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

impl Drop for Observers {
    fn drop(&mut self) {
        for (center, token) in self.tokens.drain(..) {
            let token: &AnyObject = token.as_ref();
            // SAFETY: a token this centre handed out.
            unsafe { center.removeObserver(token) };
        }
    }
}

unsafe extern "C-unwind" fn on_display(
    _display: u32,
    flags: CGDisplayChangeSummaryFlags,
    info: *mut c_void,
) {
    // Every change comes twice: once before, with this flag, and once after.
    // Only the second describes the new layout.
    if flags.contains(CGDisplayChangeSummaryFlags::BeginConfigurationFlag) {
        return;
    }
    // SAFETY: `info` is the `Shared` registered below, alive until removal.
    unsafe { Shared::from_info(info) }.push(Raw::DisplayChanged);
}

/// The display reconfiguration callback, for as long as this lives.
pub struct DisplayWatch {
    shared: Arc<Shared>,
}

impl DisplayWatch {
    pub fn new(shared: Arc<Shared>) -> Result<Self, String> {
        // SAFETY: the callback matches the documented type, and the info
        // pointer stays valid until `Drop` removes the registration.
        let status =
            unsafe { CGDisplayRegisterReconfigurationCallback(Some(on_display), shared.as_info()) };
        if status == CGError::Success {
            Ok(Self { shared })
        } else {
            Err(format!(
                "CGDisplayRegisterReconfigurationCallback failed with {}",
                status.0
            ))
        }
    }
}

impl Drop for DisplayWatch {
    fn drop(&mut self) {
        // SAFETY: the same pair that was registered.
        unsafe { CGDisplayRemoveReconfigurationCallback(Some(on_display), self.shared.as_info()) };
    }
}

unsafe extern "C-unwind" fn on_power(info: *mut c_void) {
    // SAFETY: `info` is the `Shared` the source was created with.
    unsafe { Shared::from_info(info) }.push(Raw::Power);
}

/// IOKit's power source notification, a run loop source that fires on any
/// change to any power source: plugging in, unplugging, and the battery
/// level ticking down. The host compares against the last reading, so the
/// level ticks cost one comparison each.
pub struct PowerWatch {
    _shared: Arc<Shared>,
    run_loop: CFRetained<CFRunLoop>,
    source: CFRetained<CFRunLoopSource>,
}

impl PowerWatch {
    /// `None` when IOKit hands out no source, and the host falls back to a
    /// slow timer.
    pub fn new(shared: Arc<Shared>) -> Option<Self> {
        // SAFETY: the callback matches the documented type, and the context
        // pointer stays valid as long as the source is scheduled.
        let source =
            unsafe { IOPSNotificationCreateRunLoopSource(Some(on_power), shared.as_info()) }?;
        let run_loop = CFRunLoop::current()?;
        // SAFETY: reading a constant CoreFoundation exports.
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
        Some(Self {
            _shared: shared,
            run_loop,
            source,
        })
    }
}

impl Drop for PowerWatch {
    fn drop(&mut self) {
        // SAFETY: reading a constant CoreFoundation exports.
        self.run_loop
            .remove_source(Some(&self.source), unsafe { kCFRunLoopDefaultMode });
        self.source.invalidate();
    }
}

/// True on battery, false on AC. `None` when IOKit has no answer, which is
/// what a desktop Mac without a UPS can give.
pub fn on_battery() -> Option<bool> {
    let snapshot = IOPSCopyPowerSourcesInfo()?;
    // SAFETY: a snapshot from `IOPSCopyPowerSourcesInfo`, as documented.
    let kind = unsafe { IOPSGetProvidingPowerSourceType(Some(&snapshot)) }?;
    // `kIOPMBatteryPowerKey`. A UPS reports its own kind; a Mac running on
    // one is not on battery in the sense a plugin means.
    Some(kind.to_string() == "Battery Power")
}

/// Whether the session is locked right now, for the first poll's snapshot.
/// The distributed notifications only say when it changes.
pub fn locked() -> bool {
    let Some(session) = CGSessionCopyCurrentDictionary() else {
        return false;
    };
    // SAFETY: the session dictionary is documented as keyed by strings.
    let session: CFRetained<
        objc2_core_foundation::CFDictionary<CFString, objc2_core_foundation::CFType>,
    > = unsafe { CFRetained::cast_unchecked(session) };
    let key = CFString::from_static_str("CGSSessionScreenIsLocked");
    session
        .get(&key)
        .and_then(|v| {
            v.downcast_ref::<objc2_core_foundation::CFBoolean>()
                .map(|b| b.as_bool())
        })
        .unwrap_or(false)
}
