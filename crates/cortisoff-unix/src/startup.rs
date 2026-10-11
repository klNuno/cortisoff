//! What the daemon needs to know before it touches anything: who else is
//! running. Feeds `cortisoff_daemon::Interference`, like its Windows twin.

/// Executable names of everything running, lowercased, `redshift` shaped.
/// Empty where this system offers no way to list processes.
///
/// A snapshot rather than a live view: the gate runs once at startup and a
/// process that appears a second later is the reload's problem, not this
/// call's.
pub fn running_processes() -> Vec<String> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        linux::running_processes()
    }
    #[cfg(target_os = "macos")]
    {
        macos::running_processes()
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        Vec::new()
    }
}

/// The last component of a path, lowercased.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn base(path: &std::path::Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().to_lowercase())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux {
    use std::fs;
    use std::path::Path;

    pub fn running_processes() -> Vec<String> {
        let Ok(entries) = fs::read_dir("/proc") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let numeric = entry
                .file_name()
                .to_str()
                .is_some_and(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()));
            if !numeric {
                continue;
            }
            let dir = entry.path();
            let exe = fs::read_link(dir.join("exe"))
                .ok()
                .and_then(|p| exe_name(&p));
            let comm = fs::read_to_string(dir.join("comm"))
                .ok()
                .map(|c| c.trim_end_matches('\n').to_lowercase())
                .filter(|c| !c.is_empty());
            names_for(exe, comm, &mut out);
        }
        out
    }

    /// The file name behind `/proc/<pid>/exe`, without the ` (deleted)` the
    /// kernel appends once a running binary has been replaced on disk, which
    /// is every process after a package upgrade.
    fn exe_name(link: &Path) -> Option<String> {
        let name = super::base(link)?;
        Some(
            name.strip_suffix(" (deleted)")
                .map(str::to_owned)
                .unwrap_or(name),
        )
    }

    /// The executable's own name when its link can be read, which it can for
    /// this user's processes, and `comm` beside it when the two disagree: a
    /// script's executable is its interpreter, `python3`, while its `comm` is
    /// the script's name, the one a person and the known-application list
    /// recognise. `comm` is cut at fifteen bytes, so it is not repeated when
    /// it is only the start of the full name. Other users' processes keep
    /// their link private and answer with `comm` alone.
    pub(super) fn names_for(exe: Option<String>, comm: Option<String>, out: &mut Vec<String>) {
        match (exe, comm) {
            (Some(exe), Some(comm)) => {
                let differs = !exe.starts_with(&comm);
                out.push(exe);
                if differs {
                    out.push(comm);
                }
            }
            (Some(name), None) | (None, Some(name)) => out.push(name),
            (None, None) => {}
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub fn running_processes() -> Vec<String> {
        // SAFETY: a null buffer asks for the count only.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return Vec::new();
        }
        // Room for processes started between the two calls.
        let mut pids = vec![0 as libc::c_int; count as usize + 64];
        let bytes = (pids.len() * std::mem::size_of::<libc::c_int>()) as libc::c_int;
        // SAFETY: `pids` holds `bytes` bytes.
        let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast::<c_void>(), bytes) };
        if listed <= 0 {
            return Vec::new();
        }
        pids.truncate(listed as usize);
        pids.into_iter().filter_map(name).collect()
    }

    /// The executable's file name from its full path, which `proc_name`
    /// would cut at thirty-two bytes, and `proc_name` where the path is
    /// refused.
    fn name(pid: libc::c_int) -> Option<String> {
        let mut path = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: `path` holds the size passed.
        let len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        if len > 0 {
            let path = Path::new(std::ffi::OsStr::from_bytes(&path[..len as usize]));
            if let Some(name) = super::base(path) {
                return Some(name);
            }
        }
        let mut short = [0u8; 64];
        // SAFETY: `short` holds the size passed.
        let len = unsafe { libc::proc_name(pid, short.as_mut_ptr().cast(), short.len() as u32) };
        (len > 0).then(|| String::from_utf8_lossy(&short[..len as usize]).to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    #[test]
    fn this_process_is_listed() {
        let me = std::env::current_exe().unwrap();
        let me = base(&me).unwrap();
        let running = running_processes();
        assert!(running.contains(&me), "{me} not in {running:?}");
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn a_script_is_listed_by_both_names_and_a_cut_name_once() {
        let mut out = Vec::new();
        linux::names_for(
            Some("python3.12".into()),
            Some("redshift-gtk".into()),
            &mut out,
        );
        linux::names_for(
            Some("gammastep-indicator".into()),
            Some("gammastep-indi".into()),
            &mut out,
        );
        linux::names_for(None, Some("kworker/0:1".into()), &mut out);
        assert_eq!(
            out,
            [
                "python3.12",
                "redshift-gtk",
                "gammastep-indicator",
                "kworker/0:1"
            ]
        );
    }
}
