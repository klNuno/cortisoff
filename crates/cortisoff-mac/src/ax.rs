//! The Accessibility API: how one application reads and moves another's
//! windows on macOS. Every call here needs the Accessibility permission,
//! which the user grants by hand in System Settings; without it each call
//! fails with `kAXErrorAPIDisabled` and the caller says so in a sentence.

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use objc2_core_foundation::{
    CFArray, CFBoolean, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize,
};

use crate::fit::Rect;

/// What every refused call comes back as when the permission is missing.
pub const PERMISSION: &str = "macOS has not given cortisoff the Accessibility permission; \
turn it on in System Settings, Privacy & Security, Accessibility, then restart cortisoff";

pub type AxError = i32;
const SUCCESS: AxError = 0;
const API_DISABLED: AxError = -25211;
pub const CANNOT_COMPLETE: AxError = -25204;
const ATTRIBUTE_UNSUPPORTED: AxError = -25205;
const ACTION_UNSUPPORTED: AxError = -25206;

/// `kAXValueCGPointType` and `kAXValueCGSizeType`.
const VALUE_POINT: u32 = 1;
const VALUE_SIZE: u32 = 2;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXUIElementCreateApplication(pid: libc::pid_t) -> *mut CFType;
    fn AXUIElementCopyAttributeValue(
        element: &CFType,
        attribute: &CFString,
        value: *mut *mut CFType,
    ) -> AxError;
    fn AXUIElementSetAttributeValue(
        element: &CFType,
        attribute: &CFString,
        value: &CFType,
    ) -> AxError;
    fn AXUIElementPerformAction(element: &CFType, action: &CFString) -> AxError;
    fn AXValueCreate(kind: u32, value: *const c_void) -> *mut CFType;
    fn AXValueGetValue(value: &CFType, kind: u32, out: *mut c_void) -> u8;
    /// Private, and the only link from an Accessibility window to the
    /// CoreGraphics window number. Window managers such as yabai and
    /// Rectangle have relied on it for a decade; it is exported by every
    /// macOS release since 10.5.
    fn _AXUIElementGetWindow(element: &CFType, out: *mut u32) -> AxError;
}

/// Whether the user granted the permission. Cheap, and asks nothing: the
/// variant that shows the system prompt would open a window, which a daemon
/// must not do on its own.
pub fn trusted() -> bool {
    // SAFETY: no arguments, no state.
    unsafe { AXIsProcessTrusted() != 0 }
}

pub fn describe(error: AxError) -> String {
    match error {
        API_DISABLED => PERMISSION.to_owned(),
        CANNOT_COMPLETE => "the application did not answer the Accessibility request".to_owned(),
        ATTRIBUTE_UNSUPPORTED | ACTION_UNSUPPORTED => {
            "the window does not support that action".to_owned()
        }
        other => format!("the Accessibility API failed with {other}"),
    }
}

fn check(error: AxError) -> Result<(), String> {
    if error == SUCCESS {
        Ok(())
    } else {
        Err(describe(error))
    }
}

/// A +1 reference from a `Create` or `Copy` call, released on drop.
fn owned(ptr: *mut CFType) -> Option<CFRetained<CFType>> {
    // SAFETY: every caller passes the result of a function that returns an
    // owned reference or null.
    NonNull::new(ptr).map(|p| unsafe { CFRetained::from_raw(p) })
}

/// One application or one window, as the Accessibility API sees it.
pub struct Element(CFRetained<CFType>);

impl Element {
    pub fn application(pid: i32) -> Option<Self> {
        // SAFETY: any pid is accepted; a dead one yields an element whose
        // calls fail.
        owned(unsafe { AXUIElementCreateApplication(pid) }).map(Self)
    }

    fn copy(&self, attribute: &str) -> Result<CFRetained<CFType>, String> {
        let name = CFString::from_str(attribute);
        let mut out: *mut CFType = ptr::null_mut();
        // SAFETY: a valid element, attribute and out pointer. The value comes
        // back owned.
        check(unsafe { AXUIElementCopyAttributeValue(&self.0, &name, &mut out) })?;
        owned(out).ok_or_else(|| describe(ATTRIBUTE_UNSUPPORTED))
    }

    fn set(&self, attribute: &str, value: &CFType) -> Result<(), String> {
        let name = CFString::from_str(attribute);
        // SAFETY: a valid element, attribute and value.
        check(unsafe { AXUIElementSetAttributeValue(&self.0, &name, value) })
    }

    fn perform(&self, action: &str) -> Result<(), String> {
        let name = CFString::from_str(action);
        // SAFETY: a valid element and action name.
        check(unsafe { AXUIElementPerformAction(&self.0, &name) })
    }

    /// The application's windows, minimised ones included, which is what
    /// makes un-minimising reachable at all: CoreGraphics no longer lists a
    /// minimised window as on screen.
    pub fn windows(&self) -> Result<Vec<Element>, String> {
        let list = self.copy("AXWindows")?;
        let list = list
            .downcast::<CFArray>()
            .map_err(|_| describe(ATTRIBUTE_UNSUPPORTED))?;
        // SAFETY: `AXWindows` is documented as an array of elements.
        let list: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(list) };
        Ok(list.iter().map(Element).collect())
    }

    /// The CoreGraphics window number of this window.
    pub fn window_id(&self) -> Option<u32> {
        let mut id = 0u32;
        // SAFETY: a valid element and out pointer.
        let status = unsafe { _AXUIElementGetWindow(&self.0, &mut id) };
        (status == SUCCESS && id != 0).then_some(id)
    }

    pub fn flag(&self, attribute: &str) -> Option<bool> {
        self.copy(attribute)
            .ok()?
            .downcast_ref::<CFBoolean>()
            .map(|b| b.as_bool())
    }

    pub fn set_flag(&self, attribute: &str, value: bool) -> Result<(), String> {
        self.set(attribute, CFBoolean::new(value))
    }

    /// Clicks the button an attribute names, such as `AXCloseButton`.
    pub fn press(&self, button: &str) -> Result<(), String> {
        let button = self.copy(button)?;
        Element(button).perform("AXPress")
    }

    pub fn raise(&self) -> Result<(), String> {
        self.perform("AXRaise")
    }

    /// Position and size, in the global top-left coordinates CoreGraphics
    /// uses for window bounds.
    pub fn frame(&self) -> Option<Rect> {
        let mut origin = CGPoint { x: 0.0, y: 0.0 };
        let mut size = CGSize {
            width: 0.0,
            height: 0.0,
        };
        let position = self.copy("AXPosition").ok()?;
        let extent = self.copy("AXSize").ok()?;
        // SAFETY: both values are AXValues of the documented kinds, and the
        // out pointers have the matching layout.
        let read = unsafe {
            AXValueGetValue(&position, VALUE_POINT, (&mut origin as *mut CGPoint).cast()) != 0
                && AXValueGetValue(&extent, VALUE_SIZE, (&mut size as *mut CGSize).cast()) != 0
        };
        read.then_some(Rect {
            x: origin.x,
            y: origin.y,
            w: size.width,
            h: size.height,
        })
    }

    /// Moves first and sizes second, then moves again: an application that
    /// clamps its size to the screen it is on would otherwise keep the old
    /// screen's limit.
    pub fn set_frame(&self, frame: Rect) -> Result<(), String> {
        let rect = CGRect {
            origin: CGPoint {
                x: frame.x,
                y: frame.y,
            },
            size: CGSize {
                width: frame.w,
                height: frame.h,
            },
        };
        // SAFETY: the pointers have the layout the kinds name, and the
        // values are copied.
        let position =
            owned(unsafe { AXValueCreate(VALUE_POINT, (&rect.origin as *const CGPoint).cast()) })
                .ok_or_else(|| describe(CANNOT_COMPLETE))?;
        // SAFETY: as above.
        let size =
            owned(unsafe { AXValueCreate(VALUE_SIZE, (&rect.size as *const CGSize).cast()) })
                .ok_or_else(|| describe(CANNOT_COMPLETE))?;
        self.set("AXPosition", &position)?;
        self.set("AXSize", &size)?;
        self.set("AXPosition", &position)
    }
}
