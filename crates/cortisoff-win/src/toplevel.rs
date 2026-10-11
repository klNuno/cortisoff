//! The windows a person can see and switch to, and what a plugin can do to
//! one of them.

use std::collections::HashMap;
use std::ffi::c_void;

use cortisoff_host::{WindowAction, WindowInfo};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, EnumWindows, GetForegroundWindow, GetWindow, GetWindowLongW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, PostMessageW, SetForegroundWindow, ShowWindowAsync, GWL_EXSTYLE, GW_OWNER,
    SHOW_WINDOW_CMD, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, WM_CLOSE, WS_EX_TOOLWINDOW,
};

/// Every top-level window that passes [`listed`], front to back.
pub fn list() -> Vec<WindowInfo> {
    let foreground = unsafe { GetForegroundWindow() };
    let mut exes: HashMap<u32, String> = HashMap::new();
    let mut out = Vec::new();
    for hwnd in top_level() {
        // The cheap test first: most top-level windows are invisible
        // helpers, and they need no DWM call and no title.
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            continue;
        }
        let title = title(hwnd);
        let shape = Shape {
            visible: true,
            owned: unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok_and(|o| !o.is_invalid()),
            tool: (unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOOLWINDOW.0) != 0,
            cloaked: cloaked(hwnd),
            titled: !title.is_empty(),
        };
        if !listed(shape) {
            continue;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let exe = exes
            .entry(pid)
            .or_insert_with(|| {
                crate::names::image_path(pid)
                    .map(|path| exe_name(&path))
                    .unwrap_or_default()
            })
            .clone();
        out.push(WindowInfo {
            id: hwnd.0 as usize as u64,
            title,
            exe,
            pid,
            focused: hwnd == foreground,
        });
    }
    out
}

/// Do `action` to the window `id` names.
pub fn act(id: u64, action: WindowAction) -> Result<(), String> {
    let hwnd = HWND(id as usize as *mut c_void);
    if id == 0 || !unsafe { IsWindow(hwnd) }.as_bool() {
        return Err(format!("no window has the id {id} any more"));
    }
    // The asynchronous form throughout: the window belongs to another
    // thread, and a synchronous ShowWindow waits for that thread, so a hung
    // application would freeze the daemon's pump with it.
    match action {
        WindowAction::Activate => activate(hwnd),
        WindowAction::Minimize => show(hwnd, SW_MINIMIZE, "minimize"),
        WindowAction::Maximize => show(hwnd, SW_MAXIMIZE, "maximize"),
        WindowAction::Restore => show(hwnd, SW_RESTORE, "restore"),
        WindowAction::Close => unsafe { PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)) }
            .map_err(|e| {
                format!(
                    "the window would not take the close request: {}",
                    e.message()
                )
            }),
    }
}

fn show(hwnd: HWND, command: SHOW_WINDOW_CMD, verb: &str) -> Result<(), String> {
    if unsafe { ShowWindowAsync(hwnd, command) }.as_bool() {
        Ok(())
    } else {
        Err(format!("Windows refused to {verb} that window"))
    }
}

/// Restore if minimized, then bring to the front. Windows only lets the
/// process that received the last input take the foreground; a hotkey
/// press counts, a timer firing does not. The plain call is tried first,
/// and the trick only when it is refused.
fn activate(hwnd: HWND) -> Result<(), String> {
    if unsafe { IsIconic(hwnd) }.as_bool() {
        let _ = unsafe { ShowWindowAsync(hwnd, SW_RESTORE) };
    }
    if unsafe { SetForegroundWindow(hwnd) }.as_bool() {
        return Ok(());
    }
    // Some applications bring themselves forward when restored; let that
    // process do it too.
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid != 0 {
        let _ = unsafe { AllowSetForegroundWindow(pid) };
    }
    // A synthetic Alt makes this process the one that sent the last input.
    crate::input::alt_tap()?;
    if unsafe { SetForegroundWindow(hwnd) }.as_bool() {
        Ok(())
    } else {
        Err("Windows refused to bring that window to the front".to_owned())
    }
}

/// What decides whether a window is one a person switches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shape {
    visible: bool,
    /// A dialog or a palette belonging to another window.
    owned: bool,
    /// `WS_EX_TOOLWINDOW`: kept out of the taskbar and alt-tab on purpose.
    tool: bool,
    /// Visible to the window manager and not drawn: a window on another
    /// virtual desktop, a suspended store application.
    cloaked: bool,
    titled: bool,
}

fn listed(shape: Shape) -> bool {
    shape.visible && !shape.owned && !shape.tool && !shape.cloaked && shape.titled
}

/// `C:\Program Files\App\App.EXE` to `app.exe`, the form rules match on.
fn exe_name(path: &str) -> String {
    path.rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .to_lowercase()
}

/// Every top-level window, in z-order from the top. Collected first and
/// looked at afterwards, so the callback does nothing that could block.
fn top_level() -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, out: LPARAM) -> BOOL {
        (*(out.0 as *mut Vec<HWND>)).push(hwnd);
        TRUE
    }
    let mut out: Vec<HWND> = Vec::new();
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut out as *mut Vec<HWND> as isize)) };
    out
}

fn cloaked(hwnd: HWND) -> bool {
    let mut value = 0u32;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut value as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        )
    }
    .is_ok()
        && value != 0
}

/// The whole title. Reading another process's window reads the caption
/// Windows keeps for it and sends that window nothing, so a hung
/// application cannot stall this.
fn title(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Shape = Shape {
        visible: true,
        owned: false,
        tool: false,
        cloaked: false,
        titled: true,
    };

    #[test]
    fn a_plain_titled_window_is_listed() {
        assert!(listed(PLAIN));
    }

    #[test]
    fn each_trait_alone_keeps_a_window_out() {
        for shape in [
            Shape {
                visible: false,
                ..PLAIN
            },
            Shape {
                owned: true,
                ..PLAIN
            },
            Shape {
                tool: true,
                ..PLAIN
            },
            Shape {
                cloaked: true,
                ..PLAIN
            },
            Shape {
                titled: false,
                ..PLAIN
            },
        ] {
            assert!(!listed(shape), "{shape:?}");
        }
    }

    #[test]
    fn an_executable_is_its_lowercase_file_name() {
        assert_eq!(
            exe_name(r"C:\Program Files\Mozilla Firefox\Firefox.EXE"),
            "firefox.exe"
        );
        assert_eq!(exe_name("C:/tools/Code.exe"), "code.exe");
        assert_eq!(exe_name("notepad.exe"), "notepad.exe");
        assert_eq!(exe_name(""), "");
    }

    /// Read-only: enumerates the desktop of whoever runs the tests.
    #[test]
    fn the_listed_windows_are_titled_and_at_most_one_has_the_focus() {
        let windows = list();
        assert!(windows.iter().all(|w| !w.title.is_empty() && w.id != 0));
        assert!(windows.iter().filter(|w| w.focused).count() <= 1);
        let mut ids: Vec<u64> = windows.iter().map(|w| w.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), windows.len());
    }

    /// Zero only: any other number could name a real window on the desktop
    /// running the tests, and this action closes it.
    #[test]
    fn an_id_that_names_no_window_is_refused_without_acting() {
        let error = act(0, WindowAction::Close).unwrap_err();
        assert!(error.contains("no window"), "{error}");
    }
}
