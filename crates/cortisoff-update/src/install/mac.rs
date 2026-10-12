//! What installing on macOS needs from macOS itself: the per-user bundle in
//! `~/Applications`, the quarantine attribute and an ad hoc signature.
//!
//! A browser download carries `com.apple.quarantine`, and Gatekeeper asks
//! once before the first start. Once the user has said yes, the copy made
//! here drops the attribute, so the installed app, its LaunchAgent and every
//! later start run without asking again. An update swaps the executable
//! inside the bundle, which breaks the bundle's seal; `--register` from the
//! new build writes its version into `Info.plist` and signs the bundle again.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::bundle::{self, BUNDLE, ICON};
use super::{cli_beside, home, CLI_EXE};
use crate::replace;

const QUARANTINE: &str = "com.apple.quarantine";
const CODESIGN: &str = "/usr/bin/codesign";

/// `~/Applications/Cortisoff.app`.
pub fn bundle() -> PathBuf {
    home().join("Applications").join(BUNDLE)
}

/// Put the app at [`bundle`], from the bundle `exe` runs in or, for a bare
/// executable, a bundle built around it. Answers the installed executable.
pub fn install(exe: &Path, app_exe: &str) -> Result<PathBuf, String> {
    let target = bundle();
    let parent = target.parent().unwrap_or(Path::new("/"));
    fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let (incoming, _) = bundle::staging_names(&target);
    let _ = fs::remove_dir_all(&incoming);
    let built = match bundle::bundle_of(exe) {
        Some(from) => bundle::copy_tree(&from, &incoming)
            .map_err(|e| format!("cannot copy {}: {e}", from.display())),
        None => build_around(exe, app_exe, &incoming),
    };
    if let Err(e) = built.and_then(|()| bundle::swap_bundle(&incoming, &target)) {
        let _ = fs::remove_dir_all(&incoming);
        return Err(e);
    }
    Ok(bundle::executables(&target).join(app_exe))
}

/// A bundle around a bare executable, the client beside it when there is
/// one. `Info.plist` and the icon are written by [`register`].
fn build_around(exe: &Path, app_exe: &str, at: &Path) -> Result<(), String> {
    let executables = bundle::executables(at);
    replace::replace(exe, &executables.join(app_exe))?;
    if let Some(cli) = cli_beside(exe) {
        replace::replace(&cli, &executables.join(CLI_EXE))?;
    }
    Ok(())
}

/// Make the installed bundle describe this build: its version in
/// `Info.plist`, the icon when missing, no quarantine, and a signature that
/// covers what is now inside. Signing runs only when something changed or
/// the seal no longer holds, which is the state an update leaves.
pub fn register(app: &Path, version: &str) -> Result<(), String> {
    let Some(bundle) = bundle::bundle_of(app) else {
        return Ok(());
    };
    let contents = bundle.join("Contents");
    let plist = contents.join("Info.plist");
    let wanted = bundle::info_plist(version);
    let mut changed = false;
    if fs::read_to_string(&plist).ok().as_deref() != Some(wanted.as_str()) {
        fs::write(&plist, &wanted).map_err(|e| format!("cannot write {}: {e}", plist.display()))?;
        changed = true;
    }
    let icon = contents.join("Resources").join(ICON);
    if !icon.is_file() {
        let resources = contents.join("Resources");
        fs::create_dir_all(&resources)
            .map_err(|e| format!("cannot create {}: {e}", resources.display()))?;
        fs::write(&icon, bundle::icns())
            .map_err(|e| format!("cannot write {}: {e}", icon.display()))?;
        changed = true;
    }
    unquarantine(&bundle);
    // A broken seal does not stop a bundle that carries no quarantine from
    // starting, and an Apple Silicon executable keeps the linker's own
    // signature, so a failed signing costs an install nothing.
    if changed || !sealed(&bundle) {
        let _ = sign(&bundle);
    }
    Ok(())
}

/// Remove the bundle. A running daemon keeps its open executable; unix
/// unlinks it without complaint.
pub fn uninstall() -> Result<(), String> {
    let target = bundle();
    let (incoming, aside) = bundle::staging_names(&target);
    let _ = fs::remove_dir_all(incoming);
    let _ = fs::remove_dir_all(aside);
    match fs::remove_dir_all(&target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", target.display())),
    }
}

/// Drop the quarantine attribute from every file and folder in `root`.
/// Absent is the common case and not an error.
fn unquarantine(root: &Path) {
    let name = CString::new(QUARANTINE).expect("no NUL in a constant");
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        if let Ok(c) = CString::new(path.as_os_str().as_bytes()) {
            // SAFETY: two valid NUL-terminated strings; the call reads them
            // and keeps nothing. XATTR_NOFOLLOW leaves a link's target alone.
            unsafe { libc::removexattr(c.as_ptr(), name.as_ptr(), libc::XATTR_NOFOLLOW) };
        }
        let is_dir = fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
        if is_dir {
            if let Ok(entries) = fs::read_dir(&path) {
                stack.extend(entries.filter_map(Result::ok).map(|e| e.path()));
            }
        }
    }
}

fn codesign(args: &[&str], bundle: &Path) -> Result<bool, String> {
    Command::new(CODESIGN)
        .args(args)
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .map_err(|e| format!("cannot run {CODESIGN}: {e}"))
}

fn sealed(bundle: &Path) -> bool {
    codesign(&["--verify", "--deep", "--strict"], bundle).unwrap_or(false)
}

/// An ad hoc signature: no identity, so no notarization, but a sealed
/// bundle that Apple Silicon runs and that `codesign --verify` accepts.
fn sign(bundle: &Path) -> bool {
    codesign(&["--force", "--deep", "--sign", "-"], bundle).unwrap_or(false)
}
