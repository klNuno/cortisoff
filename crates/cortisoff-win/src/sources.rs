//! The hooks, and the queue they push into.
//!
//! A callback runs on Windows' terms, not the daemon's: it can fire while the
//! daemon is in the middle of applying something, and a slow one makes the
//! whole desktop feel sluggish. So a callback does the least possible work,
//! pushing a raw record into a thread local queue, and every lookup that could
//! block (opening a process, reading a name) happens later in `poll`.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Shell::NIN_SELECT;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetForegroundWindow, EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND,
    PBT_APMRESUMEAUTOMATIC, PBT_POWERSETTINGCHANGE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    WM_CONTEXTMENU, WM_DISPLAYCHANGE, WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION,
    WM_TIMECHANGE, WM_TIMER, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};

/// The keyboard twin of `NIN_SELECT`, Enter on a focused tray icon.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

/// What a callback managed to say without doing any work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raw {
    Foreground(isize),
    /// The foreground window moved or resized, which is where a maximise, a
    /// restore and a switch to borderless happen without the focus moving.
    Moved,
    Lock(bool),
    DisplayChanged,
    /// The clock moved underneath a sleeping timer: a resume, a timezone
    /// change, DST. Whatever the engine was waiting for is now at the wrong
    /// time, so it is asked again.
    ClockMoved,
    Timer(usize),
    /// On battery, or back on mains.
    Power(bool),
    /// The tray icon was clicked, or selected from the keyboard.
    TrayOpen,
    /// The tray icon was right clicked, at this screen position.
    TrayMenu(i32, i32),
    /// Explorer restarted and every tray icon is gone.
    TaskbarCreated,
}

thread_local! {
    /// One session, one daemon, one thread owning the pump, so a thread local
    /// is the whole synchronisation story. No lock is taken inside a callback,
    /// which is the property that matters.
    static QUEUE: RefCell<Vec<Raw>> = const { RefCell::new(Vec::new()) };
}

pub fn push(raw: Raw) {
    QUEUE.with(|q| q.borrow_mut().push(raw));
}

/// The same, for a record that arrives in bursts and means the same thing
/// each time: a window being dragged reports its position at every frame,
/// and one entry in the queue is as good as sixty.
fn push_once(raw: Raw) {
    QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if q.last() != Some(&raw) {
            q.push(raw);
        }
    });
}

pub fn drain() -> Vec<Raw> {
    QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// The foreground window changed.
///
/// `WINEVENT_OUTOFCONTEXT` keeps this out of every other process's address
/// space, which is the difference between a hook and an injected DLL.
/// `WINEVENT_SKIPOWNPROCESS` stops the daemon's own windows from firing it.
pub fn hook_foreground() -> Option<HWINEVENTHOOK> {
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    (!hook.is_invalid()).then_some(hook)
}

pub fn unhook_foreground(hook: HWINEVENTHOOK) {
    let _ = unsafe { UnhookWinEvent(hook) };
}

unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // OBJID_WINDOW with no child. Anything else is a control inside a window
    // reporting for itself, which is not an application switch.
    const OBJID_WINDOW: i32 = 0;
    const CHILDID_SELF: i32 = 0;
    if id_object != OBJID_WINDOW || id_child != CHILDID_SELF || hwnd.is_invalid() {
        return;
    }
    push(Raw::Foreground(hwnd.0 as isize));
}

/// The foreground window moved or changed size. `EVENT_OBJECT_LOCATIONCHANGE`
/// fires for every window, every caret and the mouse cursor, so the callback
/// keeps only a top-level record about the window that has the focus and
/// coalesces the burst a drag produces.
pub fn hook_location() -> Option<HWINEVENTHOOK> {
    let hook = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(on_location),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    (!hook.is_invalid()).then_some(hook)
}

unsafe extern "system" fn on_location(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    const OBJID_WINDOW: i32 = 0;
    const CHILDID_SELF: i32 = 0;
    if id_object != OBJID_WINDOW || id_child != CHILDID_SELF || hwnd.is_invalid() {
        return;
    }
    if hwnd != GetForegroundWindow() {
        return;
    }
    push_once(Raw::Moved);
}

pub fn watch_session(window: HWND) -> bool {
    unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) }.is_ok()
}

pub fn unwatch_session(window: HWND) {
    let _ = unsafe { WTSUnRegisterSessionNotification(window) };
}

/// The daemon window's procedure. Same rule as the hook: record and get out.
pub unsafe extern "system" fn wndproc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_WTSSESSION_CHANGE => {
            match wparam.0 as u32 {
                WTS_SESSION_LOCK => push(Raw::Lock(true)),
                WTS_SESSION_UNLOCK => push(Raw::Lock(false)),
                _ => {}
            }
            LRESULT(0)
        }
        // A monitor was plugged, unplugged or changed mode. Anything believed
        // to be applied may have landed on hardware that is gone.
        WM_DISPLAYCHANGE => {
            push(Raw::DisplayChanged);
            LRESULT(0)
        }
        WM_TIMECHANGE => {
            push(Raw::ClockMoved);
            LRESULT(0)
        }
        WM_TIMER => {
            push(Raw::Timer(wparam.0));
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            match wparam.0 as u32 {
                PBT_POWERSETTINGCHANGE => {
                    if let Some(on_battery) = crate::power::read_change(lparam) {
                        push(Raw::Power(on_battery));
                    }
                }
                // Every timer slept with the machine. A fade that should have
                // finished an hour ago has to be told what time it is.
                PBT_APMRESUMEAUTOMATIC => push(Raw::ClockMoved),
                _ => {}
            }
            LRESULT(1)
        }
        // The session is ending. The loop sees the flag as soon as this
        // returns and puts every screen back before Windows gets to the
        // end-session message; whatever it misses, the journal restores at
        // the next start.
        WM_QUERYENDSESSION => {
            crate::console::request_stop();
            LRESULT(1)
        }
        WM_ENDSESSION => LRESULT(0),
        m if m == crate::tray::TRAY_MESSAGE => {
            // Version 4: the event in the low word of lparam, the anchor
            // point in wparam.
            let event = (lparam.0 as u32) & 0xFFFF;
            let x = (wparam.0 & 0xFFFF) as u16 as i16 as i32;
            let y = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            match event {
                NIN_SELECT | NIN_KEYSELECT => push(Raw::TrayOpen),
                WM_CONTEXTMENU => push(Raw::TrayMenu(x, y)),
                _ => {}
            }
            LRESULT(0)
        }
        m if m != 0 && m == crate::tray::taskbar_created() => {
            push(Raw::TaskbarCreated);
            LRESULT(0)
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}
