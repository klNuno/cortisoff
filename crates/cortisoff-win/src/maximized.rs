//! Whether the focused window fills its monitor.
//!
//! `SHQueryUserNotificationState` only knows about exclusive D3D fullscreen,
//! and almost nothing runs that way any more: games default to borderless, a
//! browser is maximised, a film is a maximised player. A plugin that says "not
//! while an app is fullscreen" means all three, so this reads the window
//! itself: maximised, or its frame covering the whole monitor it sits on.
//!
//! It also remembers which monitors a fullscreen window fills after the focus
//! has left it, so a game stays fullscreen on its own monitor while the user
//! types in a chat on another ([`Filled`]).

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetWindowLongW, GetWindowRect, IsIconic, IsWindow, IsWindowVisible, IsZoomed,
    GWL_EXSTYLE, GWL_STYLE, WS_CAPTION, WS_EX_TOOLWINDOW, WS_THICKFRAME,
};

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

/// How the focused window sits on its monitor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fill {
    /// Maximised, or covering the whole monitor.
    pub maximized: bool,
    /// Covering the whole monitor, taskbar included, without a maximised
    /// frame: a borderless game, a film or a browser in fullscreen. A
    /// maximised window with its frame is not this: it leaves the taskbar,
    /// and a browser maximised all evening is not a film.
    pub fullscreen: bool,
}

/// How `hwnd` fills its monitor. An invalid or vanished window fills
/// nothing, which is what a plugin wants on a fast alt-tab.
pub fn fill(hwnd: HWND) -> Fill {
    if hwnd.is_invalid() || is_shell(hwnd) {
        return Fill::default();
    }
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    classify(Window {
        zoomed: unsafe { IsZoomed(hwnd) }.as_bool(),
        covers: covers(hwnd),
        framed: style & (WS_CAPTION.0 | WS_THICKFRAME.0) != 0,
        tool: ex & WS_EX_TOOLWINDOW.0 != 0,
    })
}

/// What [`fill`] reads off a window, so its rule runs in a test.
#[derive(Debug, Clone, Copy, Default)]
struct Window {
    zoomed: bool,
    covers: bool,
    /// A title bar or a sizing border. Chromium and Firefox drop both when
    /// they go fullscreen and keep the maximised state they came from.
    framed: bool,
    /// Kept out of the taskbar and alt-tab: an overlay such as the
    /// screenshot tool's, one per monitor, not an app.
    tool: bool,
}

/// Fullscreen is covering the monitor while not maximised, or maximised
/// with the frame gone. With the taskbar set to hide, a plain maximised
/// window covers its monitor too, and is still not a film.
fn classify(w: Window) -> Fill {
    if w.tool {
        return Fill::default();
    }
    Fill {
        maximized: w.zoomed || w.covers,
        fullscreen: w.covers && (!w.zoomed || !w.framed),
    }
}

fn covers(hwnd: HWND) -> bool {
    let mut window = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut window) }.is_err() {
        return false;
    }
    monitor(hwnd).is_some_and(|(bounds, _)| contains(&window, &bounds))
}

/// The bounds and device name (`\\.\DISPLAY1`) of the monitor `hwnd` sits
/// on. The name is the one `screen::device_names` gives, so the colour
/// actuator can match an output to it.
fn monitor(hwnd: HWND) -> Option<(RECT, String)> {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
        return None;
    }
    let name = &info.szDevice;
    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    Some((
        info.monitorInfo.rcMonitor,
        String::from_utf16_lossy(&name[..end]),
    ))
}

/// The device name of the monitor `hwnd` sits on, `None` for no window.
pub fn output(hwnd: HWND) -> Option<String> {
    if hwnd.is_invalid() {
        return None;
    }
    monitor(hwnd).map(|(_, device)| device)
}

/// How a remembered window stands now, read from Windows.
fn look(hwnd: isize) -> Look {
    let hwnd = HWND(hwnd as *mut _);
    let shown = unsafe {
        IsWindow(hwnd).as_bool() && IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool()
    };
    if !shown {
        return Look::default();
    }
    Look {
        shown,
        covers: fill(hwnd).fullscreen,
        output: output(hwnd),
    }
}

/// How a remembered window stands now. Plain data, so the rule that reads it
/// runs in a test without a desktop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Look {
    /// Still a window, visible and not minimised. Exclusive fullscreen
    /// minimises on alt-tab, which is how it leaves.
    pub shown: bool,
    /// Still covering its monitor without being maximised.
    pub covers: bool,
    /// The monitor it sits on now, which may not be the one it was seen on.
    pub output: Option<String>,
}

/// Every window seen fullscreen while it had the focus and still showing so,
/// with the monitor it fills.
///
/// `app.fullscreen` was about the focused window alone, so clicking into a
/// chat on the second monitor ended a game's fullscreen on the first, and
/// night light warmed the game again. A monitor stays listed while its window
/// stays on top of it: the focus on another monitor keeps it, a window focused
/// on the same monitor sits over the game and drops it.
#[derive(Debug, Default)]
pub struct Filled {
    windows: Vec<(isize, String)>,
}

impl Filled {
    /// Look at every remembered window again. `fills` says whether the
    /// focused window is fullscreen, borderless or exclusive.
    pub fn refresh(&mut self, foreground: HWND, fills: bool) {
        let output = output(foreground);
        self.update(foreground.0 as isize, fills, output.as_deref(), look);
    }

    /// The rule behind [`Filled::refresh`], with Windows passed in as `look`.
    ///
    /// The focused window is remembered while it fills, on the monitor it
    /// sits on. Any other is kept while it still shows, still covers its
    /// monitor, and that monitor is not the one holding the focus; it follows
    /// its window to another monitor.
    pub fn update(
        &mut self,
        foreground: isize,
        fills: bool,
        foreground_output: Option<&str>,
        look: impl Fn(isize) -> Look,
    ) {
        self.windows.retain_mut(|(hwnd, device)| {
            // The focused window is decided below from `fills`, not from
            // `look`: exclusive fullscreen need not match the monitor's
            // rectangle.
            if *hwnd == foreground {
                return false;
            }
            let now = look(*hwnd);
            match now.output {
                Some(output)
                    if now.shown && now.covers && foreground_output != Some(output.as_str()) =>
                {
                    *device = output;
                    true
                }
                _ => false,
            }
        });
        if let (true, Some(output)) = (fills && foreground != 0, foreground_output) {
            self.windows.push((foreground, output.to_owned()));
        }
    }

    /// The remembered windows, for the hooks that hear them change.
    pub fn windows(&self) -> impl Iterator<Item = isize> + '_ {
        self.windows.iter().map(|(hwnd, _)| *hwnd)
    }

    /// Every monitor showing a fullscreen window, sorted and without
    /// repeats, so two equal states compare equal.
    pub fn outputs(&self) -> Vec<String> {
        let mut out: Vec<String> = self.windows.iter().map(|(_, d)| d.clone()).collect();
        out.sort_unstable();
        out.dedup();
        out
    }
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

    fn window(zoomed: bool, covers: bool, framed: bool, tool: bool) -> Fill {
        classify(Window {
            zoomed,
            covers,
            framed,
            tool,
        })
    }

    #[test]
    fn a_borderless_game_is_fullscreen() {
        assert!(window(false, true, false, false).fullscreen);
    }

    /// Helium on a video: maximised still, frame gone, exactly the monitor.
    #[test]
    fn a_browser_fullscreen_from_maximised_is_fullscreen() {
        let fill = window(true, true, false, false);
        assert!(fill.fullscreen && fill.maximized);
    }

    /// With the taskbar hidden a maximised window hangs over the whole
    /// monitor, frame and all.
    #[test]
    fn a_maximised_window_over_a_hidden_taskbar_is_not() {
        let fill = window(true, true, true, false);
        assert!(!fill.fullscreen && fill.maximized);
    }

    /// The screenshot tool lays one topmost tool window over each monitor
    /// while a region is picked.
    #[test]
    fn a_tool_window_over_the_monitor_is_neither() {
        assert_eq!(window(false, true, false, true), Fill::default());
    }

    const GAME: isize = 10;
    const CHAT: isize = 20;
    const BROWSER: isize = 30;
    const FILM: isize = 40;

    fn on(output: &str) -> Look {
        Look {
            shown: true,
            covers: true,
            output: Some(output.to_owned()),
        }
    }

    fn windowed(output: &str) -> Look {
        Look {
            covers: false,
            ..on(output)
        }
    }

    /// A focused game filling the first monitor.
    fn playing() -> Filled {
        let mut filled = Filled::default();
        filled.update(GAME, true, Some("D1"), |_| on("D1"));
        filled
    }

    #[test]
    fn the_focused_fullscreen_window_lists_its_monitor() {
        assert_eq!(playing().outputs(), ["D1"]);
    }

    #[test]
    fn focus_on_another_monitor_keeps_the_game_listed() {
        let mut filled = playing();
        filled.update(CHAT, false, Some("D2"), |_| on("D1"));
        assert_eq!(filled.outputs(), ["D1"]);
        assert_eq!(filled.windows().collect::<Vec<_>>(), [GAME]);
    }

    #[test]
    fn focus_on_the_same_monitor_drops_it() {
        let mut filled = playing();
        filled.update(BROWSER, false, Some("D1"), |_| on("D1"));
        assert!(filled.outputs().is_empty());
    }

    #[test]
    fn a_closed_hidden_or_minimised_game_drops_out() {
        let mut filled = playing();
        filled.update(CHAT, false, Some("D2"), |_| Look::default());
        assert!(filled.outputs().is_empty());
    }

    #[test]
    fn a_game_back_in_a_window_drops_out() {
        let mut filled = playing();
        filled.update(CHAT, false, Some("D2"), |_| windowed("D1"));
        assert!(filled.outputs().is_empty());
    }

    #[test]
    fn the_focused_window_leaving_fullscreen_drops_out() {
        let mut filled = playing();
        filled.update(GAME, false, Some("D1"), |_| windowed("D1"));
        assert!(filled.outputs().is_empty());
    }

    #[test]
    fn a_game_moved_to_another_monitor_reports_the_new_one() {
        let mut filled = playing();
        filled.update(CHAT, false, Some("D3"), |_| on("D2"));
        assert_eq!(filled.outputs(), ["D2"]);
    }

    #[test]
    fn a_game_moved_under_the_focus_drops_out() {
        let mut filled = playing();
        filled.update(CHAT, false, Some("D2"), |_| on("D2"));
        assert!(filled.outputs().is_empty());
    }

    #[test]
    fn several_monitors_are_listed_sorted_and_once() {
        let look = |hwnd| match hwnd {
            GAME => on("D2"),
            _ => on("D1"),
        };
        let mut filled = Filled::default();
        filled.update(GAME, true, Some("D2"), look);
        filled.update(FILM, true, Some("D1"), look);
        assert_eq!(filled.outputs(), ["D1", "D2"]);
        // Two windows filling the same monitor list it once.
        filled.windows.push((BROWSER, "D1".to_owned()));
        filled.update(CHAT, false, Some("D3"), look);
        assert_eq!(filled.outputs(), ["D1", "D2"]);
    }

    #[test]
    fn exclusive_fullscreen_counts_off_the_monitor_rectangle() {
        let mut filled = Filled::default();
        filled.update(GAME, true, Some("D1"), |_| windowed("D1"));
        assert_eq!(filled.outputs(), ["D1"]);
    }

    #[test]
    fn no_focused_window_keeps_what_still_shows() {
        let mut filled = playing();
        filled.update(0, false, None, |_| on("D1"));
        assert_eq!(filled.outputs(), ["D1"]);
    }
}
