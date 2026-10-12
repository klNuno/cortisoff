//! Global hotkeys through Carbon's `RegisterEventHotKey`.
//!
//! Still the only public API for a system-wide shortcut that needs no
//! permission: an event tap sees every key but needs Input Monitoring, and
//! `NSEvent` global monitors cannot swallow the chord. Carbon queues each
//! press as an event on the main thread's event queue. With no
//! `NSApplication` draining that queue, [`Hotkeys::drain`] dispatches it
//! after each run loop pass, which is what calls the handler below.

use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;

use cortisoff_host::Hotkey;

use crate::keys;
use crate::layout::Layout;
use crate::runloop::{Raw, Shared};

type OsStatus = i32;
type EventRef = *mut c_void;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerUpp =
    Option<unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OsStatus>;

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct EventTypeSpec {
    class: u32,
    kind: u32,
}

const NO_ERR: OsStatus = 0;
/// `kEventClassKeyboard`, `'keyb'`.
const CLASS_KEYBOARD: u32 = 0x6B65_7962;
/// `kEventHotKeyPressed`.
const HOT_KEY_PRESSED: u32 = 5;
/// `kEventParamDirectObject`, `'----'`.
const PARAM_DIRECT_OBJECT: u32 = 0x2D2D_2D2D;
/// `typeEventHotKeyID`, `'hkid'`.
const TYPE_HOT_KEY_ID: u32 = 0x686B_6964;
/// Ours, `'cort'`, so a press of somebody else's hotkey dispatched through
/// the same handler is ignored.
const SIGNATURE: u32 = 0x636F_7274;
/// `eventHotKeyExistsErr` and `eventHotKeyInvalidErr`.
const HOT_KEY_EXISTS: OsStatus = -9878;
const HOT_KEY_INVALID: OsStatus = -9879;
/// The most events one drain dispatches, so a flood cannot hold the poll.
const DRAIN_LIMIT: usize = 64;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn GetEventDispatcherTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerUpp,
        num_types: usize,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out: *mut EventHandlerRef,
    ) -> OsStatus;
    fn RemoveEventHandler(handler: EventHandlerRef) -> OsStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        out: *mut EventHotKeyRef,
    ) -> OsStatus;
    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OsStatus;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OsStatus;
    fn ReceiveNextEvent(
        num_types: usize,
        list: *const EventTypeSpec,
        timeout: f64,
        pull: u8,
        out: *mut EventRef,
    ) -> OsStatus;
    fn SendEventToEventTarget(event: EventRef, target: EventTargetRef) -> OsStatus;
    fn ReleaseEvent(event: EventRef);
}

unsafe extern "C" fn on_hotkey(
    _call: EventHandlerCallRef,
    event: EventRef,
    info: *mut c_void,
) -> OsStatus {
    let mut id = EventHotKeyId {
        signature: 0,
        id: 0,
    };
    // SAFETY: a hotkey event Carbon is dispatching, and an out buffer of the
    // size named.
    let status = unsafe {
        GetEventParameter(
            event,
            PARAM_DIRECT_OBJECT,
            TYPE_HOT_KEY_ID,
            ptr::null_mut(),
            std::mem::size_of::<EventHotKeyId>(),
            ptr::null_mut(),
            (&mut id as *mut EventHotKeyId).cast(),
        )
    };
    if status == NO_ERR && id.signature == SIGNATURE {
        // SAFETY: `info` is the `Shared` the handler was installed with, and
        // the handler is removed before that `Arc` is dropped.
        unsafe { Shared::from_info(info) }.push(Raw::Hotkey(id.id));
    }
    NO_ERR
}

pub struct Hotkeys {
    shared: Arc<Shared>,
    handler: EventHandlerRef,
    registered: Vec<EventHotKeyRef>,
    /// The last set asked for, registered again when the layout changes.
    wanted: Vec<Hotkey>,
}

impl Hotkeys {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            handler: ptr::null_mut(),
            registered: Vec::new(),
            wanted: Vec::new(),
        }
    }

    /// The handler goes in on first use, so a session with no hotkeys never
    /// touches Carbon at all.
    fn install(&mut self) -> Result<(), String> {
        if !self.handler.is_null() {
            return Ok(());
        }
        let spec = EventTypeSpec {
            class: CLASS_KEYBOARD,
            kind: HOT_KEY_PRESSED,
        };
        let mut handler: EventHandlerRef = ptr::null_mut();
        // SAFETY: a valid target, handler and spec, and `info` outlives the
        // handler (see `Drop`).
        let status = unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                Some(on_hotkey),
                1,
                &spec,
                self.shared.as_info(),
                &mut handler,
            )
        };
        if status != NO_ERR || handler.is_null() {
            return Err(format!(
                "Carbon would not install a hotkey handler ({status})"
            ));
        }
        self.handler = handler;
        Ok(())
    }

    fn clear(&mut self) {
        for hot_key in self.registered.drain(..) {
            // SAFETY: a reference `RegisterEventHotKey` handed out and that
            // was not released yet.
            unsafe { UnregisterEventHotKey(hot_key) };
        }
    }

    /// Exactly these hotkeys, replacing every earlier set, on the keys that
    /// type them in the active layout. Returns the ones refused, with why.
    pub fn set(&mut self, wanted: &[Hotkey]) -> Vec<(u32, String)> {
        self.clear();
        self.wanted = wanted.to_vec();
        if wanted.is_empty() {
            return Vec::new();
        }
        if let Err(e) = self.install() {
            return wanted.iter().map(|h| (h.id, e.clone())).collect();
        }
        let layout = Layout::current();
        let mut refused = Vec::new();
        for hotkey in wanted {
            let Some(code) = keys::code(hotkey.chord.key, layout.as_ref()) else {
                refused.push((hotkey.id, keys::SYSTEM_KEY.to_owned()));
                continue;
            };
            let id = EventHotKeyId {
                signature: SIGNATURE,
                id: hotkey.id,
            };
            let mut out: EventHotKeyRef = ptr::null_mut();
            // SAFETY: valid arguments and out pointer.
            let status = unsafe {
                RegisterEventHotKey(
                    u32::from(code),
                    keys::carbon_modifiers(hotkey.chord.mods),
                    id,
                    GetApplicationEventTarget(),
                    0,
                    &mut out,
                )
            };
            match status {
                NO_ERR if !out.is_null() => self.registered.push(out),
                HOT_KEY_EXISTS => refused.push((
                    hotkey.id,
                    "another application already owns that shortcut".to_owned(),
                )),
                HOT_KEY_INVALID => {
                    refused.push((hotkey.id, "macOS does not accept that shortcut".to_owned()))
                }
                other => refused.push((hotkey.id, format!("macOS refused the shortcut ({other})"))),
            }
        }
        refused
    }

    pub fn active(&self) -> bool {
        !self.registered.is_empty()
    }

    /// Registers the same set again after the user switched layouts, so
    /// `ctrl+a` follows the A. What was refused the first time was already
    /// reported, and a chord another app took meanwhile is refused quietly.
    pub fn follow_layout(&mut self) {
        let wanted = std::mem::take(&mut self.wanted);
        let _ = self.set(&wanted);
    }

    /// Dispatches what Carbon queued since the last pass, which is where a
    /// hotkey press reaches [`on_hotkey`].
    pub fn drain(&self) {
        if !self.active() {
            return;
        }
        for _ in 0..DRAIN_LIMIT {
            let mut event: EventRef = ptr::null_mut();
            // SAFETY: no type filter, no wait, and a valid out pointer. A
            // pulled event is owned and released below.
            let status = unsafe { ReceiveNextEvent(0, ptr::null(), 0.0, 1, &mut event) };
            if status != NO_ERR || event.is_null() {
                break;
            }
            // SAFETY: an event this thread owns, sent to the standard
            // dispatcher, then released once.
            unsafe {
                SendEventToEventTarget(event, GetEventDispatcherTarget());
                ReleaseEvent(event);
            }
        }
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.clear();
        if !self.handler.is_null() {
            // SAFETY: the handler `install` got, removed once.
            unsafe { RemoveEventHandler(self.handler) };
        }
    }
}
