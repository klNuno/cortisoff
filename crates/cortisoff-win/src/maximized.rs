//! Whether the focused window fills its monitor.
//!
//! `SHQueryUserNotificationState` only knows about exclusive D3D fullscreen,
//! and almost nothing runs that way any more: games default to borderless, a
//! browser is maximised, a film is a maximised player. A rule that says "not
//! while an app is fullscreen" means all three, so this reads the window
//! itself: maximised, or its frame covering the whole monitor it sits on.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowRect, IsZoomed};

/// The shell's own surfaces cover a monitor without anyone having maximised
/// anything: the desktop, the taskbars, the lock and logon screens.
const SHELL_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "LockScreenBackstopFrame",
    "Windows.UI.Core.CoreWindow",
];

/// True when `hwnd` is maximised or its frame covers the monitor it is on.
/// An invalid or vanished window answers false, which is what a rule wants
/// on a fast alt-tab.
pub fn covers_monitor(hwnd: HWND) -> bool {
    if hwnd.is_invalid() || is_shell(hwnd) {
        return false;
    }
    if unsafe { IsZoomed(hwnd) }.as_bool() {
        return true;
    }
    let mut window = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut window) }.is_err() {
        return false;
    }
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return false;
    }
    contains(&window, &info.rcMonitor)
}

fn is_shell(hwnd: HWND) -> bool {
    let mut buf = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return false;
    }
    let class = String::from_utf16_lossy(&buf[..len as usize]);
    SHELL_CLASSES.contains(&class.as_str())
}

/// `outer` covers `inner` entirely. A borderless window sits exactly on the
/// monitor; a maximised one hangs a few invisible pixels over every edge.
fn contains(outer: &RECT, inner: &RECT) -> bool {
    outer.left <= inner.left
        && outer.top <= inner.top
        && outer.right >= inner.right
        && outer.bottom >= inner.bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn a_borderless_window_and_a_maximised_one_both_cover_their_monitor() {
        let monitor = rect(0, 0, 2560, 1440);
        assert!(contains(&rect(0, 0, 2560, 1440), &monitor));
        assert!(contains(&rect(-8, -8, 2568, 1448), &monitor));
    }

    #[test]
    fn a_window_short_of_one_edge_does_not() {
        let monitor = rect(0, 0, 2560, 1440);
        assert!(!contains(&rect(0, 0, 2560, 1400), &monitor));
        assert!(!contains(&rect(100, 100, 2000, 1200), &monitor));
        // A window filling the second monitor covers nothing of the first.
        assert!(!contains(&rect(2560, 0, 5120, 1440), &monitor));
    }
}
