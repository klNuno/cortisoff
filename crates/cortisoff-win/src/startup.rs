//! What the daemon needs to know before it touches anything: who else is
//! running, and what the screen already looks like.
//!
//! Both answers feed `cortisoff_daemon::Interference`, which decides which
//! capabilities this run is allowed to drive.

use std::path::PathBuf;

use cortisoff_host::GammaRamp;
use windows::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

use crate::screen;

/// Executable names of everything running, lowercased, `lightbulb.exe` shaped.
///
/// A snapshot rather than a live view: the gate runs once at startup and a
/// process that appears a second later is the reload's problem, not this
/// call's.
pub fn running_processes() -> Vec<String> {
    let mut out = Vec::new();
    each_process(|_, name| out.push(name));
    out
}

/// Full image paths of the running processes whose file name is one of
/// `names`, any case. Only those are opened, so asking about two executables
/// costs two handles and not three hundred.
pub fn process_paths(names: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    each_process(|pid, name| {
        if names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            if let Some(path) = crate::names::image_path(pid) {
                out.push(PathBuf::from(path));
            }
        }
    });
    out
}

fn each_process(mut f: impl FnMut(u32, String)) {
    let snapshot = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        Ok(h) if h != INVALID_HANDLE_VALUE => h,
        _ => return,
    };

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
        loop {
            let end = entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len());
            f(
                entry.th32ProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..end]).to_lowercase(),
            );
            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }
    unsafe {
        let _ = CloseHandle(snapshot);
    }
}

/// The first monitor whose ramp can be read, in the shape the interference
/// check wants.
///
/// One monitor is enough: every colour utility worth detecting applies to all
/// of them, and reading three costs three device contexts for no extra
/// evidence.
pub fn current_ramp() -> Option<GammaRamp> {
    let flat = screen::device_names()
        .into_iter()
        .find_map(|name| screen::capture(&name))?;
    let mut ramp = [[0u16; 256]; 3];
    for (channel, slot) in ramp.iter_mut().enumerate() {
        slot.copy_from_slice(&flat[channel * 256..(channel + 1) * 256]);
    }
    Some(ramp)
}
