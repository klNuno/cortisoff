//! The daemon's own process: once per session, and background work as far as
//! the scheduler is concerned. The dashboard borrows the once-per-session part.

use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, FILETIME, HANDLE,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, K32EmptyWorkingSet, PROCESS_MEMORY_COUNTERS,
};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, GetProcessTimes, OpenProcess, ProcessPowerThrottling,
    SetProcessInformation, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

/// Held for as long as this process is the session's daemon, or its dashboard.
pub struct InstanceLock(HANDLE);

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// `None` when another daemon already runs in this session.
///
/// Two of them would each capture the other's tint as a baseline and fight
/// over the screen forever, and the pipe alone does not stop a second one from
/// starting: the logon task, the dashboard and a terminal can all launch one.
pub fn claim_session() -> Option<InstanceLock> {
    claim("cortisoff-daemon")
}

/// `None` when a dashboard is already open in this session.
///
/// The dashboard is a window on the daemon and two of them would both be
/// right, so the second launch is never what the person clicking wanted.
pub fn claim_dashboard() -> Option<InstanceLock> {
    claim("cortisoff-dashboard")
}

/// Bring the open dashboard to the front, for the launch that lost the claim.
/// The person double-clicked the icon, so the window they asked for is the
/// one that comes up. `false` when no dashboard window is found. The class
/// is winit's default, and the title is the one the app sets.
pub fn raise_dashboard() -> bool {
    let Ok(hwnd) = (unsafe { FindWindowW(w!("Window Class"), w!("cortisoff")) }) else {
        return false;
    };
    if hwnd.is_invalid() {
        return false;
    }
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
    true
}

fn claim(what: &str) -> Option<InstanceLock> {
    let name: Vec<u16> = format!("Local\\{what}-{}", crate::session_id())
        .encode_utf16()
        .chain([0])
        .collect();
    let handle = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let _ = unsafe { CloseHandle(handle) };
        return None;
    }
    Some(InstanceLock(handle))
}

/// What one process has cost so far. `cpu_ms` only grows, so a rate needs
/// two readings; `rss_bytes` is the working set right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessUsage {
    pub cpu_ms: u64,
    pub rss_bytes: u64,
}

/// Read what a process costs, by pid. `None` when it is gone or belongs to
/// someone the caller may not look at.
pub fn usage(pid: u32) -> Option<ProcessUsage> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let read = || {
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
            .ok()?;
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        unsafe { GetProcessMemoryInfo(handle, &mut counters, counters.cb) }.ok()?;
        Some(ProcessUsage {
            cpu_ms: (hundred_ns(kernel) + hundred_ns(user)) / 10_000,
            rss_bytes: counters.WorkingSetSize as u64,
        })
    };
    let out = read();
    let _ = unsafe { CloseHandle(handle) };
    out
}

fn hundred_ns(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

/// EcoQoS: Windows may run this process on efficiency cores and let its
/// timers slip. The daemon's work is a few hundred microseconds when something
/// changes and nothing otherwise, so nothing it does notices, and a laptop
/// does.
pub fn efficiency_mode() -> bool {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: 1,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };
    unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const c_void,
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .is_ok()
}

/// Hand this process's resident pages back to Windows. For a process that is
/// about to wait on one handle for hours, the guard, every page it touched
/// while starting is memory it will not read again. A page it does need comes
/// back on the next touch.
pub fn shed_memory() {
    let _ = unsafe { K32EmptyWorkingSet(GetCurrentProcess()) };
}
