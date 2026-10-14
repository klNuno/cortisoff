//! Putting a new executable where the running one is.
//!
//! Windows refuses to delete or overwrite a running image but lets it be
//! renamed, so the running exe steps aside as `<name>.old.exe` and the next
//! start deletes it. Unix keeps the old inode alive for the running process,
//! so a rename over the path is enough.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Replace the running executable with `staged`, a verified download. On
/// macOS a staged bundle (`cortisoff-<version>.zip`) replaces the whole
/// `Cortisoff.app` the executable runs in instead.
///
/// A client staged with the app (see [`crate::download_pair`]) then
/// replaces the `cortisoff-cli` installed beside it, the app first so a
/// failed swap never leaves a new client talking to an old daemon. A client
/// that cannot be swapped stays at its version and the next release tries
/// again; the app update stands.
pub fn apply(staged: &Path) -> Result<(), String> {
    let exe = current_exe()?;
    #[cfg(target_os = "macos")]
    if let Some(version) = crate::staged_bundle_version(staged) {
        return crate::install::apply_bundle(staged, &exe, version);
    }
    apply_pair(staged, &exe).map(drop)
}

/// [`apply`] for the app at `exe`. Answers whether a staged client was put
/// in place too.
pub(crate) fn apply_pair(staged: &Path, exe: &Path) -> Result<bool, String> {
    replace(staged, exe)?;
    let Some(client) = crate::staged_client_for(staged).filter(|c| c.is_file()) else {
        return Ok(false);
    };
    let installed = exe.with_file_name(crate::install::CLI_EXE);
    let swapped = installed.is_file() && replace(&client, &installed).is_ok();
    let _ = fs::remove_file(&client);
    Ok(swapped)
}

/// Delete what [`apply`] moved aside. The next start calls it; a file still
/// held by a process that has not exited yet is left for the start after.
/// The client's copy goes too when nothing runs from it any more.
pub fn cleanup_previous() -> Result<(), String> {
    let exe = current_exe()?;
    let _ = cleanup_previous_at(&exe.with_file_name(crate::install::CLI_EXE));
    cleanup_previous_at(&exe)
}

/// Start the executable now at the running one's path, detached from this
/// process, so the caller can exit right after.
pub fn relaunch(args: &[String]) -> Result<(), String> {
    let exe = current_exe()?;
    let mut command = Command::new(&exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut command);
    command
        .spawn()
        .map(drop)
        .map_err(|e| format!("could not start {}: {e}", exe.display()))
}

pub(crate) fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot find the running executable: {e}"))
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(any(windows, unix)))]
fn detach(_command: &mut Command) {}

/// `cortisoff.exe` steps aside as `cortisoff.old.exe`, then
/// `cortisoff.old.1.exe` and so on while an earlier one is still running.
pub(crate) fn previous_path(exe: &Path, n: u32) -> PathBuf {
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = exe
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let name = if n == 0 {
        format!("{stem}.old{extension}")
    } else {
        format!("{stem}.old.{n}{extension}")
    };
    exe.with_file_name(name)
}

/// Every file [`previous_path`] can produce for `exe`, present in its folder.
pub(crate) fn previous_files(exe: &Path) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let candidates: Vec<PathBuf> = (0..64).map(|n| previous_path(exe, n)).collect();
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| candidates.iter().any(|c| same_name(c, path)))
        .collect()
}

fn same_name(a: &Path, b: &Path) -> bool {
    match (a.file_name(), b.file_name()) {
        (Some(a), Some(b)) if cfg!(windows) => {
            a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
        }
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

pub(crate) fn cleanup_previous_at(exe: &Path) -> Result<(), String> {
    let mut busy = Vec::new();
    for old in previous_files(exe) {
        if let Err(e) = fs::remove_file(&old) {
            busy.push(format!("{}: {e}", old.display()));
        }
    }
    if busy.is_empty() {
        Ok(())
    } else {
        Err(format!("left for the next start: {}", busy.join("; ")))
    }
}

/// Copy `source` over `target`, which may be running. The copy lands beside
/// the target first, so the final step is a rename on one volume.
pub(crate) fn replace(source: &Path, target: &Path) -> Result<(), String> {
    let dir = target
        .parent()
        .ok_or_else(|| format!("{} has no folder", target.display()))?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let incoming = dir.join(format!(".{name}.incoming"));
    let _ = fs::remove_file(&incoming);
    fs::copy(source, &incoming).map_err(|e| {
        format!(
            "cannot copy {} to {}: {e}",
            source.display(),
            incoming.display()
        )
    })?;
    let result = make_executable(&incoming).and_then(|()| swap_in(&incoming, target));
    if result.is_err() {
        let _ = fs::remove_file(&incoming);
    }
    result
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("cannot mark {} executable: {e}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn swap_in(incoming: &Path, target: &Path) -> Result<(), String> {
    if !target.exists() {
        return fs::rename(incoming, target)
            .map_err(|e| format!("cannot move the new build in: {e}"));
    }
    // A previous update's file may still be held by a process that has not
    // exited (a daemon started from the old build), so take the first free
    // name rather than failing on a locked `.old.exe`.
    let mut aside = None;
    for n in 0..64 {
        let candidate = previous_path(target, n);
        if candidate.exists() && fs::remove_file(&candidate).is_err() {
            continue;
        }
        aside = Some(candidate);
        break;
    }
    let aside = aside.ok_or("every name to move the running build aside is taken")?;
    fs::rename(target, &aside).map_err(|e| {
        format!(
            "cannot move {} aside to {}: {e}",
            target.display(),
            aside.display()
        )
    })?;
    if let Err(e) = fs::rename(incoming, target) {
        // Put the running build back, or the next start finds no executable.
        let _ = fs::rename(&aside, target);
        return Err(format!("cannot move the new build in: {e}"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn swap_in(incoming: &Path, target: &Path) -> Result<(), String> {
    fs::rename(incoming, target).map_err(|e| format!("cannot move the new build in: {e}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh empty folder under the system temp dir, removed on drop.
    pub(crate) struct Scratch(pub PathBuf);

    impl Scratch {
        pub fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "cortisoff-update-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn exe_name() -> String {
        format!("cortisoff{}", std::env::consts::EXE_SUFFIX)
    }

    #[test]
    fn previous_names_keep_the_extension() {
        let exe = Path::new("dir").join("cortisoff.exe");
        assert_eq!(
            previous_path(&exe, 0),
            Path::new("dir").join("cortisoff.old.exe")
        );
        assert_eq!(
            previous_path(&exe, 2),
            Path::new("dir").join("cortisoff.old.2.exe")
        );
        let bare = Path::new("bin").join("cortisoff");
        assert_eq!(
            previous_path(&bare, 0),
            Path::new("bin").join("cortisoff.old")
        );
    }

    #[test]
    fn apply_puts_the_new_build_in_place_and_cleanup_removes_the_old_one() {
        let scratch = Scratch::new("apply");
        let exe = scratch.0.join("install").join(exe_name());
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"old build").unwrap();
        let staged = scratch.0.join("download").join("cortisoff-1.2.0");
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::write(&staged, b"new build").unwrap();

        replace(&staged, &exe).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"new build");
        let old = previous_path(&exe, 0);
        if cfg!(windows) {
            assert_eq!(fs::read(&old).unwrap(), b"old build");
        } else {
            assert!(
                !old.exists(),
                "a rename over the path leaves nothing behind"
            );
        }
        let incoming = exe.with_file_name(format!(".{}.incoming", exe_name()));
        assert!(!incoming.exists());

        cleanup_previous_at(&exe).unwrap();
        assert!(!old.exists());
        assert_eq!(fs::read(&exe).unwrap(), b"new build");
        cleanup_previous_at(&exe).unwrap();
    }

    fn cli_name() -> String {
        crate::install::CLI_EXE.to_owned()
    }

    /// An installed app and client, and a staged pair for 1.2.0.
    fn installed_pair(scratch: &Scratch) -> (PathBuf, PathBuf, PathBuf) {
        let exe = scratch.0.join("install").join(exe_name());
        let cli = exe.with_file_name(cli_name());
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"old app").unwrap();
        fs::write(&cli, b"old client").unwrap();
        let staged = scratch
            .0
            .join("download")
            .join(format!("cortisoff-1.2.0{}", std::env::consts::EXE_SUFFIX));
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::write(&staged, b"new app").unwrap();
        fs::write(crate::staged_client_for(&staged).unwrap(), b"new client").unwrap();
        (exe, cli, staged)
    }

    #[test]
    fn apply_replaces_the_app_then_the_client() {
        let scratch = Scratch::new("apply-pair");
        let (exe, cli, staged) = installed_pair(&scratch);
        assert_eq!(apply_pair(&staged, &exe), Ok(true));
        assert_eq!(fs::read(&exe).unwrap(), b"new app");
        assert_eq!(fs::read(&cli).unwrap(), b"new client");
        assert!(!crate::staged_client_for(&staged).unwrap().exists());
        if cfg!(windows) {
            assert_eq!(fs::read(previous_path(&cli, 0)).unwrap(), b"old client");
        }
        cleanup_previous_at(&cli).unwrap();
        cleanup_previous_at(&exe).unwrap();
        assert!(previous_files(&cli).is_empty());
    }

    #[test]
    fn an_app_that_cannot_be_replaced_leaves_the_client_alone() {
        let scratch = Scratch::new("apply-pair-order");
        let (exe, cli, staged) = installed_pair(&scratch);
        // The app's staged file is gone: its swap fails first.
        fs::remove_file(&staged).unwrap();
        assert!(apply_pair(&staged, &exe).is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"old app");
        assert_eq!(fs::read(&cli).unwrap(), b"old client");
    }

    #[test]
    fn a_client_that_cannot_be_replaced_leaves_the_app_updated() {
        let scratch = Scratch::new("apply-pair-cli");
        let (exe, cli, staged) = installed_pair(&scratch);
        // A folder where the client should be: no swap can land there.
        fs::remove_file(&cli).unwrap();
        fs::create_dir(&cli).unwrap();
        assert_eq!(apply_pair(&staged, &exe), Ok(false));
        assert_eq!(fs::read(&exe).unwrap(), b"new app");
        assert!(cli.is_dir());
    }

    #[test]
    fn an_app_without_a_client_beside_it_updates_alone() {
        let scratch = Scratch::new("apply-alone");
        let (exe, cli, staged) = installed_pair(&scratch);
        fs::remove_file(&cli).unwrap();
        assert_eq!(apply_pair(&staged, &exe), Ok(false));
        assert_eq!(fs::read(&exe).unwrap(), b"new app");
        assert!(!cli.exists(), "an update installs no client of its own");
    }

    #[test]
    fn a_second_update_before_cleanup_still_applies() {
        let scratch = Scratch::new("twice");
        let exe = scratch.0.join(exe_name());
        fs::write(&exe, b"v1").unwrap();
        let staged = scratch.0.join("staged");
        fs::write(&staged, b"v2").unwrap();
        replace(&staged, &exe).unwrap();
        fs::write(&staged, b"v3").unwrap();
        replace(&staged, &exe).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"v3");
        cleanup_previous_at(&exe).unwrap();
        assert!(previous_files(&exe).is_empty());
    }

    #[test]
    fn a_missing_source_leaves_the_target_alone() {
        let scratch = Scratch::new("missing");
        let exe = scratch.0.join(exe_name());
        fs::write(&exe, b"running").unwrap();
        assert!(replace(&scratch.0.join("nothing"), &exe).is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"running");
        assert!(previous_files(&exe).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_new_build_is_executable() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new("mode");
        let exe = scratch.0.join("cortisoff");
        fs::write(&exe, b"old").unwrap();
        let staged = scratch.0.join("staged");
        fs::write(&staged, b"new").unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o600)).unwrap();
        replace(&staged, &exe).unwrap();
        let mode = fs::metadata(&exe).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }
}
