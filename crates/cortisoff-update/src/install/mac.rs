//! What installing on macOS needs from macOS itself: the per-user bundle in
//! `~/Applications`, the quarantine attribute and the bundle's signature.
//!
//! A browser download carries `com.apple.quarantine`, and Gatekeeper asks
//! once before the first start. Once the user has said yes, the copy made
//! here drops the attribute, so the installed app, its LaunchAgent and every
//! later start run without asking again.
//!
//! Releases sign the bundle with one stable certificate, and an update
//! replaces the whole bundle with the next signed one, so the designated
//! requirement macOS files permission grants under stays the same. Nothing
//! here signs a released bundle again: an ad hoc signature would give it a
//! new requirement at each update. Only a bundle built here around a bare
//! executable (the move from v0.2.0) is signed ad hoc, since it has no
//! signature at all.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::bundle::{self, BUNDLE, BUNDLE_ID, ICON};
use super::{cli_beside, home, CLI_EXE};
use crate::replace;

const QUARANTINE: &str = "com.apple.quarantine";
const CODESIGN: &str = "/usr/bin/codesign";
const DITTO: &str = "/usr/bin/ditto";
const TCCUTIL: &str = "/usr/bin/tccutil";
const LAUNCHCTL: &str = "/bin/launchctl";

/// `~/Applications/Cortisoff.app`.
pub fn bundle() -> PathBuf {
    home().join("Applications").join(BUNDLE)
}

/// `~/Applications/Cortisoff`, where v0.2.0 put a bare executable and its
/// client.
pub fn legacy_dir() -> PathBuf {
    home().join("Applications").join("Cortisoff")
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
    // The bundle replaces what v0.2.0 installed; a daemon still running
    // from there keeps its open executable until it exits.
    super::remove_legacy();
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

/// Make the installed bundle ready to run: no quarantine, and for a bundle
/// that carries no valid signature (one built here around a bare
/// executable) this build's `Info.plist`, the icon and an ad hoc signature.
/// A sealed bundle, which every release and every update is, is left as
/// signed: writing into it would break the seal, and signing it again would
/// change the requirement macOS keeps permission grants under.
pub fn register(app: &Path, version: &str) -> Result<(), String> {
    let Some(bundle) = bundle::bundle_of(app) else {
        return Ok(());
    };
    unquarantine(&bundle);
    if sealed(&bundle) {
        return Ok(());
    }
    let contents = bundle.join("Contents");
    let plist = contents.join("Info.plist");
    let wanted = bundle::info_plist(version);
    if fs::read_to_string(&plist).ok().as_deref() != Some(wanted.as_str()) {
        fs::write(&plist, &wanted).map_err(|e| format!("cannot write {}: {e}", plist.display()))?;
    }
    let icon = contents.join("Resources").join(ICON);
    if !icon.is_file() {
        let resources = contents.join("Resources");
        fs::create_dir_all(&resources)
            .map_err(|e| format!("cannot create {}: {e}", resources.display()))?;
        fs::write(&icon, bundle::icns())
            .map_err(|e| format!("cannot write {}: {e}", icon.display()))?;
    }
    // A bundle that carries no quarantine starts with a broken seal too,
    // and an Apple Silicon executable keeps the linker's own signature, so
    // a failed signing costs an install nothing.
    let _ = sign(&bundle);
    Ok(())
}

/// Replace the bundle `exe` runs in with the one in `zip`: unpacked with
/// `ditto` beside it, checked (executable, `version`, a signature that
/// verifies), then swapped in, the old one moving back if the new one
/// cannot. The unpacked copy goes either way.
pub fn apply_update(zip: &Path, exe: &Path, version: &str, app_exe: &str) -> Result<(), String> {
    let installed = bundle::bundle_of(exe)
        .ok_or_else(|| format!("{} is not inside an app bundle", exe.display()))?;
    let unpack = bundle::update_dir(&installed);
    let _ = fs::remove_dir_all(&unpack);
    fs::create_dir_all(&unpack).map_err(|e| format!("cannot create {}: {e}", unpack.display()))?;
    let result = unpack_and_swap(zip, &unpack, &installed, version, app_exe);
    let _ = fs::remove_dir_all(&unpack);
    result
}

fn unpack_and_swap(
    zip: &Path,
    unpack: &Path,
    installed: &Path,
    version: &str,
    app_exe: &str,
) -> Result<(), String> {
    let unpacked = Command::new(DITTO)
        .args(["-x", "-k"])
        .arg(zip)
        .arg(unpack)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run {DITTO}: {e}"))?;
    if !unpacked.success() {
        return Err(format!("cannot unpack {}", zip.display()));
    }
    let staged = unpack.join(BUNDLE);
    bundle::check_staged(&staged, version, app_exe, sealed)?;
    bundle::swap_bundle(&staged, installed)
}

/// Remove the bundle, what v0.2.0 left, and the permission rows macOS keeps
/// for the app. A running daemon keeps its open executable; unix unlinks it
/// without complaint.
pub fn uninstall() -> Result<(), String> {
    let target = bundle();
    let (incoming, aside) = bundle::staging_names(&target);
    let _ = fs::remove_dir_all(incoming);
    let _ = fs::remove_dir_all(aside);
    super::remove_legacy();
    forget_permissions();
    unload_agent();
    match fs::remove_dir_all(&target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", target.display())),
    }
}

/// `launchctl bootout gui/<uid>/dev.mtsu.cortisoff`: removing the
/// LaunchAgent's file leaves its job loaded until logout, naming a program
/// that is gone, which a `kickstart` would still try. A job that is not
/// loaded makes it fail, which is the state wanted.
fn unload_agent() {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let _ = Command::new(LAUNCHCTL)
        .args(["bootout", &format!("gui/{uid}/{BUNDLE_ID}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// `tccutil reset All dev.mtsu.cortisoff`: the Accessibility, Input
/// Monitoring and Screen Recording rows stay in System Settings after the
/// app is gone, and a reinstall, signed with a new ad hoc hash, would then
/// meet a row that shows as on and grants nothing. The user's own rows need
/// no administrator. A failure leaves the rows, which is the old state.
fn forget_permissions() {
    let _ = Command::new(TCCUTIL)
        .args(["reset", "All", BUNDLE_ID])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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
