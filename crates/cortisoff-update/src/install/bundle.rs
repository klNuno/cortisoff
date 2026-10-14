//! The macOS app bundle, as plain data and file moves.
//!
//! Nothing here calls macOS, so every piece is tested on each system; the
//! calls that need macOS (the quarantine attribute, `codesign`) are in
//! `mac.rs`. The release workflow builds the same bundle from the same
//! `Info.plist` template (`packaging/macos/bundle.sh`), so a bundle the
//! installer writes and one a release ships do not differ.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The bundle's folder name, in `~/Applications`.
pub const BUNDLE: &str = "Cortisoff.app";
/// The icon file `Info.plist` names (`CFBundleIconFile`).
pub const ICON: &str = "cortisoff.icns";
/// `CFBundleIdentifier`, which TCC files its permission rows under.
pub const BUNDLE_ID: &str = "dev.mtsu.cortisoff";

const TEMPLATE: &str = include_str!("../../../../packaging/macos/Info.plist");
const LOGO: &[u8] = include_bytes!("../../../../assets/icon.png");

/// `Info.plist` for `version`. Line endings are pinned so a checkout that
/// converted them still writes the bytes the release signed.
pub fn info_plist(version: &str) -> String {
    TEMPLATE.replace("\r\n", "\n").replace("@VERSION@", version)
}

/// The version an `Info.plist` gives (`CFBundleShortVersionString`), read
/// without a plist parser: the value is the next `<string>` after its key.
pub fn plist_version(text: &str) -> Option<String> {
    let after_key = &text[text.find("<key>CFBundleShortVersionString</key>")?..];
    let start = after_key.find("<string>")? + "<string>".len();
    let rest = &after_key[start..];
    let value = rest[..rest.find("</string>")?].trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// The app's icon in Apple's `icns` container: one 512 px PNG under the
/// `ic09` type, which every macOS since 10.7 reads and scales. The release
/// builds a full set with `iconutil`; this one serves a bundle the installer
/// has to make from a bare executable.
pub fn icns() -> Vec<u8> {
    icns_from(&[(*b"ic09", LOGO)])
}

fn icns_from(entries: &[([u8; 4], &[u8])]) -> Vec<u8> {
    let total = 8 + entries.iter().map(|(_, d)| 8 + d.len()).sum::<usize>();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&(total as u32).to_be_bytes());
    for (kind, data) in entries {
        out.extend_from_slice(kind);
        out.extend_from_slice(&((8 + data.len()) as u32).to_be_bytes());
        out.extend_from_slice(data);
    }
    out
}

/// The bundle an executable runs from, when it sits where a bundle keeps
/// its executable: `<name>.app/Contents/MacOS/<exe>`.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_app = bundle
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"));
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && is_app)
        .then(|| bundle.to_path_buf())
}

/// `Contents/MacOS` under `bundle`.
pub fn executables(bundle: &Path) -> PathBuf {
    bundle.join("Contents").join("MacOS")
}

/// The two names a bundle steps through while it is replaced.
pub fn staging_names(bundle: &Path) -> (PathBuf, PathBuf) {
    let name = bundle
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (
        bundle.with_file_name(format!(".{name}.incoming")),
        bundle.with_file_name(format!(".{name}.old")),
    )
}

/// The folder an update unpacks into, beside the bundle so the swap is a
/// rename on one volume.
pub fn update_dir(bundle: &Path) -> PathBuf {
    let name = bundle
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    bundle.with_file_name(format!(".{name}.update"))
}

/// Whether a bundle an update unpacked may replace the installed one: the
/// app executable is there, its `Info.plist` names the version the signed
/// feed announced, and `sealed` (`codesign --verify --deep --strict` on
/// macOS) accepts its signature.
pub fn check_staged(
    staged: &Path,
    version: &str,
    app_exe: &str,
    sealed: impl FnOnce(&Path) -> bool,
) -> Result<(), String> {
    if !executables(staged).join(app_exe).is_file() {
        return Err(format!(
            "the downloaded app has no {app_exe} in {}",
            executables(staged).display()
        ));
    }
    let plist = staged.join("Contents").join("Info.plist");
    let found = fs::read_to_string(&plist)
        .ok()
        .and_then(|text| plist_version(&text));
    match found {
        Some(found) if found == version => {}
        Some(found) => {
            return Err(format!(
                "the downloaded app is version {found}, the release said {version}"
            ))
        }
        None => return Err("the downloaded app does not say its version".to_owned()),
    }
    if !sealed(staged) {
        return Err("the downloaded app's signature does not verify".to_owned());
    }
    Ok(())
}

/// Copy a folder tree, symbolic links as links. A signed bundle keeps
/// links (frameworks do), and following one would copy what it points at
/// and break the seal.
pub fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_symlink() {
            copy_link(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_link(from: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(from)?, to)
}

#[cfg(not(unix))]
fn copy_link(from: &Path, to: &Path) -> io::Result<()> {
    fs::copy(from, to).map(drop)
}

/// Put the bundle built at `incoming` where `bundle` is. The old bundle
/// steps aside first and goes once the new one is in; a running daemon
/// keeps its executable open by inode, so nothing it holds breaks. When the
/// new one cannot move in, the old one moves back.
pub fn swap_bundle(incoming: &Path, bundle: &Path) -> Result<(), String> {
    let (_, aside) = staging_names(bundle);
    let _ = fs::remove_dir_all(&aside);
    let had_one = bundle.exists();
    if had_one {
        fs::rename(bundle, &aside).map_err(|e| {
            format!(
                "cannot move {} aside to {}: {e}",
                bundle.display(),
                aside.display()
            )
        })?;
    }
    if let Err(e) = fs::rename(incoming, bundle) {
        if had_one {
            let _ = fs::rename(&aside, bundle);
        }
        return Err(format!("cannot move the new app in: {e}"));
    }
    let _ = fs::remove_dir_all(&aside);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replace::tests::Scratch;

    #[test]
    fn the_plist_names_the_bundle_and_carries_the_version() {
        let text = info_plist("1.2.3");
        assert!(!text.contains('@'), "every placeholder is filled");
        assert!(!text.contains('\r'));
        for line in [
            "<key>CFBundleExecutable</key>\n\t<string>cortisoff</string>",
            "<key>CFBundleIdentifier</key>\n\t<string>dev.mtsu.cortisoff</string>",
            "<key>CFBundleShortVersionString</key>\n\t<string>1.2.3</string>",
            "<key>CFBundleVersion</key>\n\t<string>1.2.3</string>",
            "<key>CFBundlePackageType</key>\n\t<string>APPL</string>",
            "<key>CFBundleIconFile</key>\n\t<string>cortisoff</string>",
            "<key>LSMinimumSystemVersion</key>",
        ] {
            assert!(text.contains(line), "missing {line:?}");
        }
        assert_eq!(text.matches("<dict>").count(), 1);
        assert!(text.trim_end().ends_with("</plist>"));
        assert!(text.contains(&format!("<string>{BUNDLE_ID}</string>")));
        // The icon the plist names is the one the installer writes.
        assert_eq!(ICON, "cortisoff.icns");
    }

    #[test]
    fn the_installed_version_is_read_back_from_the_plist() {
        assert_eq!(
            plist_version(&info_plist("1.2.3")).as_deref(),
            Some("1.2.3")
        );
        let spaced =
            "<dict>\n  <key>CFBundleShortVersionString</key>\n  <string> 0.4.0 </string>\n</dict>";
        assert_eq!(plist_version(spaced).as_deref(), Some("0.4.0"));
        assert_eq!(
            plist_version("<dict><key>CFBundleVersion</key><string>1</string></dict>"),
            None
        );
        assert_eq!(
            plist_version("<key>CFBundleShortVersionString</key><string></string>"),
            None
        );
        assert_eq!(plist_version("<key>CFBundleShortVersionString</key>"), None);
    }

    #[test]
    fn the_icon_is_a_well_formed_icns() {
        let icns = icns();
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize,
            icns.len()
        );
        assert_eq!(&icns[8..12], b"ic09");
        assert_eq!(
            u32::from_be_bytes(icns[12..16].try_into().unwrap()) as usize,
            icns.len() - 8
        );
        assert_eq!(&icns[16..24], b"\x89PNG\r\n\x1a\n");
        // ic09 means 512 px: the width in the PNG header says so.
        assert_eq!(u32::from_be_bytes(icns[32..36].try_into().unwrap()), 512);

        let two = icns_from(&[(*b"ic08", b"ab"), (*b"ic09", b"cde")]);
        assert_eq!(two.len(), 8 + 10 + 11);
        assert_eq!(&two[18..22], b"ic09");
        assert_eq!(u32::from_be_bytes(two[22..26].try_into().unwrap()), 11);
    }

    #[test]
    fn only_an_executable_inside_a_bundle_has_one() {
        let exe = Path::new("/Users/u/Applications/Cortisoff.app/Contents/MacOS/cortisoff");
        assert_eq!(
            bundle_of(exe),
            Some(PathBuf::from("/Users/u/Applications/Cortisoff.app"))
        );
        let translocated = Path::new(
            "/private/var/folders/x/T/AppTranslocation/1/d/Cortisoff.app/Contents/MacOS/cortisoff",
        );
        assert!(bundle_of(translocated).is_some());
        assert_eq!(
            bundle_of(Path::new("/Users/u/Downloads/cortisoff-darwin-x86_64")),
            None
        );
        assert_eq!(
            bundle_of(Path::new("/x/Cortisoff/Contents/MacOS/cortisoff")),
            None
        );
        assert_eq!(
            bundle_of(Path::new("/x/Cortisoff.app/Contents/cortisoff")),
            None
        );
        assert_eq!(bundle_of(Path::new("cortisoff")), None);
        assert_eq!(
            executables(Path::new("/a/Cortisoff.app")),
            Path::new("/a/Cortisoff.app").join("Contents").join("MacOS")
        );
    }

    fn bundle_in(dir: &Path, marker: &[u8]) -> PathBuf {
        let bundle = dir.join(BUNDLE);
        fs::create_dir_all(executables(&bundle)).unwrap();
        fs::create_dir_all(bundle.join("Contents").join("Resources")).unwrap();
        fs::write(executables(&bundle).join("cortisoff"), marker).unwrap();
        fs::write(bundle.join("Contents").join("Info.plist"), b"plist").unwrap();
        bundle
    }

    #[test]
    fn a_bundle_is_copied_whole_and_swapped_in() {
        let scratch = Scratch::new("bundle-swap");
        let from = bundle_in(&scratch.0.join("download"), b"new");
        let installed = bundle_in(&scratch.0.join("Applications"), b"old");
        fs::write(installed.join("Contents").join("stale"), b"x").unwrap();

        let (incoming, aside) = staging_names(&installed);
        copy_tree(&from, &incoming).unwrap();
        swap_bundle(&incoming, &installed).unwrap();

        assert_eq!(
            fs::read(executables(&installed).join("cortisoff")).unwrap(),
            b"new"
        );
        assert!(installed.join("Contents").join("Info.plist").is_file());
        assert!(installed.join("Contents").join("Resources").is_dir());
        assert!(
            !installed.join("Contents").join("stale").exists(),
            "nothing of the old bundle stays"
        );
        assert!(!incoming.exists());
        assert!(!aside.exists());
    }

    #[test]
    fn a_first_install_has_nothing_to_move_aside() {
        let scratch = Scratch::new("bundle-first");
        let from = bundle_in(&scratch.0.join("download"), b"new");
        let target = scratch.0.join("Applications").join(BUNDLE);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let (incoming, _) = staging_names(&target);
        copy_tree(&from, &incoming).unwrap();
        swap_bundle(&incoming, &target).unwrap();
        assert_eq!(
            fs::read(executables(&target).join("cortisoff")).unwrap(),
            b"new"
        );
    }

    #[test]
    fn a_failed_swap_puts_the_old_bundle_back() {
        let scratch = Scratch::new("bundle-fail");
        let installed = bundle_in(&scratch.0.join("Applications"), b"old");
        let missing = scratch.0.join("never-built");
        assert!(swap_bundle(&missing, &installed).is_err());
        assert_eq!(
            fs::read(executables(&installed).join("cortisoff")).unwrap(),
            b"old"
        );
    }

    #[test]
    fn a_staged_bundle_must_be_whole_the_announced_version_and_sealed() {
        let scratch = Scratch::new("staged");
        let staged = bundle_in(&scratch.0.join("update"), b"new");
        fs::write(
            staged.join("Contents").join("Info.plist"),
            info_plist("0.4.0"),
        )
        .unwrap();

        assert_eq!(
            check_staged(&staged, "0.4.0", "cortisoff", |_| true),
            Ok(())
        );

        let wrong = check_staged(&staged, "0.5.0", "cortisoff", |_| true).unwrap_err();
        assert!(
            wrong.contains("0.4.0") && wrong.contains("0.5.0"),
            "{wrong}"
        );

        let unsigned = check_staged(&staged, "0.4.0", "cortisoff", |_| false).unwrap_err();
        assert!(unsigned.contains("signature"), "{unsigned}");

        // The seal is asked about the staged bundle, not some other path.
        let mut asked = None;
        let _ = check_staged(&staged, "0.4.0", "cortisoff", |p| {
            asked = Some(p.to_path_buf());
            true
        });
        assert_eq!(asked.as_deref(), Some(staged.as_path()));

        fs::write(staged.join("Contents").join("Info.plist"), b"<dict></dict>").unwrap();
        assert!(check_staged(&staged, "0.4.0", "cortisoff", |_| true)
            .unwrap_err()
            .contains("version"));

        fs::remove_file(executables(&staged).join("cortisoff")).unwrap();
        assert!(check_staged(&staged, "0.4.0", "cortisoff", |_| true)
            .unwrap_err()
            .contains("cortisoff"));
    }

    #[test]
    fn an_update_unpacks_beside_the_bundle() {
        assert_eq!(
            update_dir(Path::new("/h/Applications/Cortisoff.app")),
            Path::new("/h/Applications/.Cortisoff.app.update")
        );
    }

    #[test]
    fn staging_names_sit_beside_the_bundle_and_hide() {
        let (incoming, aside) = staging_names(Path::new("/h/Applications/Cortisoff.app"));
        assert_eq!(
            incoming,
            Path::new("/h/Applications/.Cortisoff.app.incoming")
        );
        assert_eq!(aside, Path::new("/h/Applications/.Cortisoff.app.old"));
    }
}
