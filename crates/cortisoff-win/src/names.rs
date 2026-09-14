//! Turning a window handle into something a rule can match on.
//!
//! Cached, because the foreground window changes far more often than the set
//! of running processes, and `QueryFullProcessImageNameW` opens a handle every
//! time. The cache is keyed on the PID *and* the process creation time:
//! Windows reuses PIDs, and a cache keyed on the PID alone eventually fires a
//! rule on the wrong application.

use std::collections::HashMap;

use cortisoff_host::AppRef;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HWND};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, GetWindowThreadProcessId};

#[derive(Debug, Default)]
pub struct Names {
    /// PID to creation time and executable path. An entry whose creation time
    /// no longer matches is a different process wearing the same number.
    cache: HashMap<u32, (u64, String)>,
}

impl Names {
    /// The application owning `hwnd`, or `None` when the window went away
    /// between the event and this call, which is normal on a fast alt-tab.
    pub fn resolve(&mut self, hwnd: HWND) -> Option<AppRef> {
        let mut pid = 0u32;
        // The return value is the thread id, and zero means the window is
        // already gone.
        if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) } == 0 || pid == 0 {
            return None;
        }

        let created = created_at_ms(pid)?;
        let exe = match self.cache.get(&pid) {
            Some((seen, exe)) if *seen == created => exe.clone(),
            _ => {
                let exe = image_path(pid)?;
                self.cache.insert(pid, (created, exe.clone()));
                exe
            }
        };

        Some(AppRef {
            exe,
            title: window_title(hwnd),
            pid,
            started_at_ms: created,
        })
    }

    /// Drop entries for processes that are gone. Called on a slow beat rather
    /// than per event: the cache is a few hundred bytes and the point is only
    /// to stop it growing across a session that lasts weeks.
    pub fn prune(&mut self) {
        self.cache
            .retain(|pid, (created, _)| created_at_ms(*pid) == Some(*created));
    }
}

/// A handle with the least access that answers both questions, so this works
/// against processes cortisoff has no business opening properly.
fn with_process<T>(
    pid: u32,
    f: impl FnOnce(windows::Win32::Foundation::HANDLE) -> Option<T>,
) -> Option<T> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let out = f(handle);
    unsafe { CloseHandle(handle) }.ok()?;
    out
}

fn created_at_ms(pid: u32) -> Option<u64> {
    with_process(pid, |handle| {
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
            .ok()?;
        let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        // FILETIME counts 100 nanosecond intervals. Milliseconds are plenty to
        // tell two processes apart, and they are what `AppRef` speaks.
        Some(ticks / 10_000)
    })
}

pub(crate) fn image_path(pid: u32) -> Option<String> {
    with_process(pid, |handle| {
        let mut buffer = [0u16; 260];
        let mut len = buffer.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buffer.as_mut_ptr()),
                &mut len,
            )
        }
        .ok()?;
        Some(String::from_utf16_lossy(&buffer[..len as usize]))
    })
}

fn window_title(hwnd: HWND) -> Option<String> {
    let mut buffer = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    (len > 0).then(|| String::from_utf16_lossy(&buffer[..len as usize]))
}
