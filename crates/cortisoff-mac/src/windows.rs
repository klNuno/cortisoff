//! Windows and applications: what is on screen, who has the front, and the
//! window actions behind `window_action`.

use std::collections::HashMap;

use cortisoff_host::{AppRef, WindowAction, WindowInfo};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSScreen, NSWorkspace};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect,
};
use objc2_core_graphics::{
    kCGWindowBounds, kCGWindowLayer, kCGWindowName, kCGWindowNumber, kCGWindowOwnerName,
    kCGWindowOwnerPID, CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo,
    CGWindowListOption,
};

use crate::ax::{self, Element};
use crate::fit::Rect;

/// One normal window, as CoreGraphics lists it.
#[derive(Debug, Clone)]
pub struct Window {
    pub id: u32,
    pub pid: i32,
    pub owner: Option<String>,
    /// `None` without the Screen Recording permission, which macOS has
    /// required for other applications' titles since 10.15.
    pub title: Option<String>,
    pub bounds: Rect,
}

/// Every normal window on screen, front first. Layer 0 is where document
/// and game windows live; the menu bar, the Dock and overlays sit on other
/// layers and are not windows a plugin means.
pub fn on_screen() -> Vec<Window> {
    let options =
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(list) = CGWindowListCopyWindowInfo(options, 0) else {
        return Vec::new();
    };
    // SAFETY: CoreGraphics documents the array as holding one dictionary per
    // window, keyed by strings.
    let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(list) };
    list.iter().filter_map(|entry| parse(&entry)).collect()
}

fn parse(entry: &CFDictionary<CFString, CFType>) -> Option<Window> {
    // SAFETY: reading constants CoreGraphics exports.
    let (number, layer, pid, owner, name, bounds) = unsafe {
        (
            kCGWindowNumber,
            kCGWindowLayer,
            kCGWindowOwnerPID,
            kCGWindowOwnerName,
            kCGWindowName,
            kCGWindowBounds,
        )
    };
    if int(entry, layer)? != 0 {
        return None;
    }
    let frame = entry.get(bounds)?.downcast::<CFDictionary>().ok()?;
    let mut rect = CGRect::default();
    // SAFETY: a bounds dictionary as CoreGraphics wrote it, and a valid out
    // pointer.
    if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(&frame), &mut rect) } {
        return None;
    }
    // Helper windows of a few points are layer 0 too, and are never what
    // someone looking at the screen calls a window.
    if rect.size.width < 2.0 || rect.size.height < 2.0 {
        return None;
    }
    Some(Window {
        id: u32::try_from(int(entry, number)?).ok()?,
        pid: i32::try_from(int(entry, pid)?).ok()?,
        owner: text(entry, owner),
        title: text(entry, name),
        bounds: Rect {
            x: rect.origin.x,
            y: rect.origin.y,
            w: rect.size.width,
            h: rect.size.height,
        },
    })
}

fn int(entry: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<i64> {
    entry.get(key)?.downcast_ref::<CFNumber>()?.as_i64()
}

fn text(entry: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<String> {
    let value = entry.get(key)?;
    let value = value.downcast_ref::<CFString>()?.to_string();
    (!value.is_empty()).then_some(value)
}

/// The application with the keyboard focus.
pub fn front_pid() -> Option<i32> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|app| app.processIdentifier())
        .filter(|pid| *pid > 0)
}

/// The front window of `pid`: its first in front-to-back order.
pub fn front_of(windows: &[Window], pid: i32) -> Option<&Window> {
    windows.iter().find(|w| w.pid == pid)
}

/// The executable's full path, the same shape the Windows host reports, so a
/// plugin written against `exe` reads alike on both.
pub fn exe(pid: i32) -> Option<String> {
    let from_url = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .and_then(|app| app.executableURL())
        .and_then(|url| url.path())
        .map(|path| path.to_string());
    from_url.or_else(|| exe_from_proc(pid))
}

/// A process LaunchServices does not know, such as a command line tool,
/// still has a path in the kernel.
fn exe_from_proc(pid: i32) -> Option<String> {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is as long as the size passed.
    let len = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if len <= 0 {
        return None;
    }
    buffer.truncate(len as usize);
    String::from_utf8(buffer).ok()
}

/// Milliseconds since the Unix epoch at which `pid` started. Paired with the
/// pid in [`AppRef`] so a reused pid never inherits another process's name.
pub fn started_at_ms(pid: i32) -> Option<u64> {
    let launched = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .and_then(|app| app.launchDate())
        .map(|date| (date.timeIntervalSince1970() * 1000.0).max(0.0) as u64);
    launched.or_else(|| started_from_proc(pid))
}

fn started_from_proc(pid: i32) -> Option<u64> {
    // SAFETY: an all-zero `proc_bsdinfo` is a valid value to be overwritten.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: the buffer is a `proc_bsdinfo` of the size passed, which is
    // what this flavour writes.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (written == size).then(|| info.pbi_start_tvsec * 1000 + info.pbi_start_tvusec / 1000)
}

/// The application in front, as the engine wants it.
pub fn focused_app(windows: &[Window]) -> Option<AppRef> {
    let pid = front_pid()?;
    Some(AppRef {
        exe: exe(pid)?,
        title: front_of(windows, pid).and_then(|w| w.title.clone()),
        pid: pid as u32,
        started_at_ms: started_at_ms(pid).unwrap_or(0),
    })
}

/// What `windows()` hands a client: every normal window, front first.
pub fn list() -> Vec<WindowInfo> {
    let windows = on_screen();
    let front = front_pid();
    let focused = front.and_then(|pid| front_of(&windows, pid)).map(|w| w.id);
    let mut paths: HashMap<i32, String> = HashMap::new();
    windows
        .iter()
        .map(|w| WindowInfo {
            id: u64::from(w.id),
            title: w.title.clone().unwrap_or_default(),
            exe: paths
                .entry(w.pid)
                .or_insert_with(|| exe(w.pid).or_else(|| w.owner.clone()).unwrap_or_default())
                .clone(),
            pid: w.pid as u32,
            focused: Some(w.id) == focused,
        })
        .collect()
}

/// The frames `Maximize` replaced, so `Restore` can put a window back where
/// it was. macOS has no restore of its own for a frame set from outside.
#[derive(Default)]
pub struct Placements {
    before: HashMap<u32, Rect>,
}

/// The window behind a CoreGraphics number, through its owner. A minimised
/// window is off the on-screen list, so the owner is found among every
/// window, on screen or not.
fn locate(id: u32) -> Result<(i32, Element), String> {
    let pid = owner_of(id).ok_or_else(|| format!("no window has the id {id}"))?;
    if !ax::trusted() {
        return Err(ax::PERMISSION.to_owned());
    }
    let app = Element::application(pid).ok_or_else(|| ax::describe(ax::CANNOT_COMPLETE))?;
    let window = app
        .windows()?
        .into_iter()
        .find(|w| w.window_id() == Some(id))
        .ok_or_else(|| format!("window {id} is not reachable through Accessibility"))?;
    Ok((pid, window))
}

fn owner_of(id: u32) -> Option<i32> {
    let list = CGWindowListCopyWindowInfo(CGWindowListOption::OptionAll, 0)?;
    // SAFETY: as in `on_screen`.
    let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(list) };
    // SAFETY: reading constants CoreGraphics exports.
    let (number, pid) = unsafe { (kCGWindowNumber, kCGWindowOwnerPID) };
    list.iter()
        .find(|entry| int(entry, number) == Some(i64::from(id)))
        .and_then(|entry| int(&entry, pid))
        .and_then(|pid| i32::try_from(pid).ok())
}

fn activate(pid: i32) -> Result<(), String> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .ok_or_else(|| format!("process {pid} is gone"))?;
    // `ActivateIgnoringOtherApps` is deprecated in favour of cooperative
    // activation, which a background daemon cannot take part in: without it
    // the request is ignored whenever another application is in front.
    #[allow(deprecated)]
    let options = NSApplicationActivationOptions::ActivateAllWindows
        | NSApplicationActivationOptions::ActivateIgnoringOtherApps;
    if app.activateWithOptions(options) {
        Ok(())
    } else {
        Err(format!("macOS refused to bring process {pid} to the front"))
    }
}

impl Placements {
    pub fn act(&mut self, id: u64, action: WindowAction) -> Result<(), String> {
        let id = u32::try_from(id).map_err(|_| format!("no window has the id {id}"))?;
        if let WindowAction::Activate = action {
            // Activation works without the Accessibility permission; the
            // permission only adds raising the one window over its siblings.
            let pid = owner_of(id).ok_or_else(|| format!("no window has the id {id}"))?;
            if let Ok((_, window)) = locate(id) {
                if window.flag("AXMinimized") == Some(true) {
                    let _ = window.set_flag("AXMinimized", false);
                }
                let _ = window.raise();
                let _ = window.set_flag("AXMain", true);
            }
            return activate(pid);
        }
        let (_, window) = locate(id)?;
        match action {
            WindowAction::Activate => Ok(()),
            WindowAction::Minimize => window.set_flag("AXMinimized", true),
            WindowAction::Close => window.press("AXCloseButton"),
            WindowAction::Maximize => {
                let frame = window
                    .frame()
                    .ok_or_else(|| format!("window {id} will not report its frame"))?;
                let target = visible_frame_for(frame)
                    .ok_or_else(|| "no screen holds the window".to_owned())?;
                window.set_frame(target)?;
                self.before.entry(id).or_insert(frame);
                Ok(())
            }
            WindowAction::Restore => {
                if window.flag("AXMinimized") == Some(true) {
                    return window.set_flag("AXMinimized", false);
                }
                if window.flag("AXFullScreen") == Some(true) {
                    return window.set_flag("AXFullScreen", false);
                }
                match self.before.remove(&id) {
                    Some(frame) => window.set_frame(frame),
                    None => Ok(()),
                }
            }
        }
    }

    /// Forget windows that are gone, so the map does not grow over a session
    /// that lasts weeks.
    pub fn prune(&mut self, alive: &[Window]) {
        self.before
            .retain(|id, _| alive.iter().any(|w| w.id == *id) || owner_of(*id).is_some());
    }
}

/// The usable part of the screen a frame is mostly on: the screen minus the
/// menu bar and the Dock, converted from Cocoa's bottom-left coordinates to
/// the top-left ones window frames use.
fn visible_frame_for(window: Rect) -> Option<Rect> {
    let mtm = MainThreadMarker::new()?;
    let screens = NSScreen::screens(mtm);
    // The first screen is the one with the menu bar, whose frame starts at
    // the origin of both coordinate systems.
    let primary_height = screens.firstObject()?.frame().size.height;
    let flip = |r: objc2_foundation::NSRect| Rect {
        x: r.origin.x,
        y: primary_height - (r.origin.y + r.size.height),
        w: r.size.width,
        h: r.size.height,
    };
    screens
        .iter()
        .map(|s| (flip(s.frame()), flip(s.visibleFrame())))
        .max_by(|(a, _), (b, _)| {
            overlap(window, *a)
                .partial_cmp(&overlap(window, *b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, visible)| visible)
}

fn overlap(a: Rect, b: Rect) -> f64 {
    let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    w.max(0.0) * h.max(0.0)
}
