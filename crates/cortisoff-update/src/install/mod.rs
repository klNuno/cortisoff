//! Installing for the current user, never for the machine.
//!
//! - Windows: `%LOCALAPPDATA%\Programs\Cortisoff`, a Start Menu shortcut and a
//!   per-user entry in Settings > Apps, all under the user's own profile and
//!   `HKCU`, so no elevation is ever needed.
//! - Linux: `~/.local/bin` and a `.desktop` entry in
//!   `~/.local/share/applications`.
//! - macOS: `~/Applications/Cortisoff`.

#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
mod desktop;
#[cfg(windows)]
mod win;

use std::fs;
use std::path::{Path, PathBuf};

use crate::replace;

/// The app's file name once installed.
pub const APP_EXE: &str = if cfg!(windows) {
    "cortisoff.exe"
} else {
    "cortisoff"
};
/// The client's file name once installed.
pub const CLI_EXE: &str = if cfg!(windows) {
    "cortisoff-cli.exe"
} else {
    "cortisoff-cli"
};

/// The folder cortisoff installs to.
pub fn dir() -> PathBuf {
    #[cfg(windows)]
    {
        win::user_programs().join("Cortisoff")
    }
    #[cfg(target_os = "macos")]
    {
        home().join("Applications").join("Cortisoff")
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        home().join(".local").join("bin")
    }
}

#[cfg(not(windows))]
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Whether `exe` is the installed copy, which is what the app asks before
/// offering to install.
pub fn is_installed(exe: &Path) -> bool {
    same_path(exe, &dir().join(APP_EXE))
}

/// Install `exe`, and `cortisoff-cli` when it sits beside it, then register
/// with the desktop. Answers the installed app's path. Running it again from
/// the installed copy rewrites the shortcut and the uninstall entry, which is
/// how that entry's version follows an update.
pub fn install(exe: &Path) -> Result<PathBuf, String> {
    let dir = dir();
    let target = dir.join(APP_EXE);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    if !same_path(exe, &target) {
        replace::replace(exe, &target)?;
    }
    if let Some(cli) = cli_beside(exe) {
        let installed_cli = dir.join(CLI_EXE);
        if !same_path(&cli, &installed_cli) {
            replace::replace(&cli, &installed_cli)?;
        }
    }
    register(&target)?;
    Ok(target)
}

/// Undo [`install`]: the desktop registration, the client and the app. On
/// Windows a running installed app cannot delete itself, so a detached
/// `cmd` removes it as soon as this process has exited; the caller should
/// exit promptly after this returns.
pub fn uninstall() -> Result<(), String> {
    let dir = dir();
    let app = dir.join(APP_EXE);
    let mut problems = Vec::new();
    if let Err(e) = unregister() {
        problems.push(e);
    }
    for file in [dir.join(CLI_EXE)]
        .into_iter()
        .chain(replace::previous_files(&app))
    {
        remove_if_present(&file, &mut problems);
    }
    let running = replace::current_exe()
        .map(|exe| same_path(&exe, &app))
        .unwrap_or(false);
    #[cfg(windows)]
    {
        if running || (app.exists() && fs::remove_file(&app).is_err()) {
            if let Err(e) = win::delete_after_exit(&app, &dir) {
                problems.push(e);
            }
        } else {
            let _ = fs::remove_dir(&dir);
        }
    }
    #[cfg(not(windows))]
    {
        // Unix unlinks a running executable without complaint.
        let _ = running;
        remove_if_present(&app, &mut problems);
        if cfg!(target_os = "macos") {
            let _ = fs::remove_dir(&dir);
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

fn remove_if_present(path: &Path, problems: &mut Vec<String>) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => problems.push(format!("cannot remove {}: {e}", path.display())),
    }
}

/// The client next to a downloaded or installed app: `cortisoff-cli` as the
/// installer names it, or `cortisoff-cli-<platform>` as a release names it.
fn cli_beside(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let mut names = vec![CLI_EXE.to_owned()];
    if let Some(key) = crate::platform_key() {
        names.push(format!(
            "cortisoff-cli-{key}{}",
            std::env::consts::EXE_SUFFIX
        ));
    }
    names
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

fn same_path(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (a, b) = (canonical(a), canonical(b));
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

#[cfg(windows)]
fn register(app: &Path) -> Result<(), String> {
    win::create_shortcut(app)?;
    win::write_uninstall_entry(app)
}

#[cfg(windows)]
fn unregister() -> Result<(), String> {
    let shortcut = win::remove_shortcut();
    let entry = win::remove_uninstall_entry();
    shortcut.and(entry)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn register(app: &Path) -> Result<(), String> {
    let path = desktop::entry_path(&home());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    fs::write(&path, desktop::entry(app))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn unregister() -> Result<(), String> {
    let mut problems = Vec::new();
    remove_if_present(&desktop::entry_path(&home()), &mut problems);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

// A bare executable in ~/Applications is what the release ships for macOS;
// Finder and Spotlight find it there without any registration.
#[cfg(target_os = "macos")]
fn register(_app: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn unregister() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replace::tests::Scratch;

    #[test]
    fn the_install_folder_is_per_user() {
        let dir = dir();
        if cfg!(windows) {
            assert!(
                dir.ends_with(Path::new("Programs").join("Cortisoff")),
                "{}",
                dir.display()
            );
        } else if cfg!(target_os = "macos") {
            assert!(
                dir.ends_with(Path::new("Applications").join("Cortisoff")),
                "{}",
                dir.display()
            );
        } else {
            assert!(
                dir.ends_with(Path::new(".local").join("bin")),
                "{}",
                dir.display()
            );
        }
        assert!(!is_installed(
            Path::new("somewhere-else").join(APP_EXE).as_path()
        ));
    }

    #[test]
    fn the_client_is_found_under_either_name() {
        let scratch = Scratch::new("cli");
        let app = scratch.0.join("cortisoff-download");
        fs::write(&app, b"app").unwrap();
        assert_eq!(cli_beside(&app), None);
        if let Some(key) = crate::platform_key() {
            let released = scratch.0.join(format!(
                "cortisoff-cli-{key}{}",
                std::env::consts::EXE_SUFFIX
            ));
            fs::write(&released, b"cli").unwrap();
            assert_eq!(cli_beside(&app), Some(released));
        }
        let installed = scratch.0.join(CLI_EXE);
        fs::write(&installed, b"cli").unwrap();
        assert_eq!(cli_beside(&app), Some(installed));
    }

    #[test]
    fn paths_compare_the_way_the_file_system_does() {
        let scratch = Scratch::new("same");
        let file = scratch.0.join("Cortisoff.exe");
        fs::write(&file, b"x").unwrap();
        assert!(same_path(&file, &scratch.0.join(".").join("Cortisoff.exe")));
        if cfg!(windows) {
            assert!(same_path(&file, &scratch.0.join("cortisoff.EXE")));
        }
        assert!(!same_path(&file, &scratch.0.join("other.exe")));
    }
}
