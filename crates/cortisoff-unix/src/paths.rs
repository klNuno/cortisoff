//! Where the socket and the lock files live, and the checks that make their
//! directory safe to use.
//!
//! A unix socket is only as private as the directory it sits in. macOS has
//! long ignored the mode of a socket file, and on Linux anyone who can write
//! to the directory can unlink the socket and put their own in its place, to
//! read every frame or answer in the daemon's name. So the directory is the
//! boundary: `$XDG_RUNTIME_DIR` and macOS's `$TMPDIR` are made by the system
//! for this user alone, and the `/tmp` fallback is made here, 0700. A
//! directory of the expected name that belongs to someone else is refused,
//! never used: on a shared `/tmp` that is exactly what an attack looks like.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Where the files go, worked out before anything on disk is touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layout {
    pub dir: PathBuf,
    /// `cortisoff` in a directory of our own, `cortisoff-<uid>` in macOS's
    /// `$TMPDIR`, which other software shares with us.
    pub stem: String,
    /// True for the `/tmp` fallback, which this crate creates and may
    /// tighten. False for a directory the system made, left as found.
    pub ours: bool,
}

impl Layout {
    pub fn socket(&self) -> PathBuf {
        self.dir.join(format!("{}.sock", self.stem))
    }

    /// A lock file beside the socket. An empty role is the daemon's.
    pub fn lock(&self, role: &str) -> PathBuf {
        if role.is_empty() {
            self.dir.join(format!("{}.lock", self.stem))
        } else {
            self.dir.join(format!("{}-{role}.lock", self.stem))
        }
    }
}

/// The socket a daemon of this user listens on, or would. Touches nothing on
/// disk, so a doctor report or an error message can name it.
pub fn socket_path() -> PathBuf {
    layout().socket()
}

pub(crate) fn layout() -> Layout {
    let get = |key: &str| std::env::var_os(key);
    if cfg!(target_os = "macos") {
        mac_layout(get, own_uid())
    } else {
        linux_layout(get, own_uid())
    }
}

/// Linux, and any unix that is not macOS: `$XDG_RUNTIME_DIR/cortisoff.sock`,
/// or `/tmp/cortisoff-<uid>/cortisoff.sock` where the login has no runtime
/// directory, a bare ssh session or a minimal container.
pub(crate) fn linux_layout(get: impl Fn(&str) -> Option<OsString>, uid: u32) -> Layout {
    match absolute(get("XDG_RUNTIME_DIR")) {
        Some(dir) => Layout {
            dir,
            stem: "cortisoff".to_owned(),
            ours: false,
        },
        None => fallback(uid),
    }
}

/// macOS: `$TMPDIR/cortisoff-<uid>.sock`. `$TMPDIR` is a per-user directory
/// under `/var/folders` that launchd sets for every login, so it is the
/// runtime directory macOS never named as one. The uid in the file name
/// keeps two users apart if someone points `$TMPDIR` at a shared directory.
pub(crate) fn mac_layout(get: impl Fn(&str) -> Option<OsString>, uid: u32) -> Layout {
    match absolute(get("TMPDIR")) {
        Some(dir) => Layout {
            dir,
            stem: format!("cortisoff-{uid}"),
            ours: false,
        },
        None => fallback(uid),
    }
}

fn fallback(uid: u32) -> Layout {
    Layout {
        dir: PathBuf::from(format!("/tmp/cortisoff-{uid}")),
        stem: "cortisoff".to_owned(),
        ours: true,
    }
}

/// The XDG spec says a relative value must be ignored, and an empty one is
/// what an unset variable looks like once a shell has exported it anyway.
fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    let path = PathBuf::from(value?);
    path.is_absolute().then_some(path)
}

/// The effective uid, the one the kernel compares a socket's owner with.
pub(crate) fn own_uid() -> u32 {
    // SAFETY: no argument and no failure mode.
    unsafe { libc::geteuid() }
}

/// Make the directory ready for the socket and the locks: created 0700 when
/// it is the fallback and missing, tightened back to 0700 if it was opened
/// up since, since nothing but cortisoff lives there, and refused when it is
/// not a real directory belonging to `uid`.
pub(crate) fn prepare(layout: &Layout, uid: u32) -> Result<(), String> {
    if layout.ours {
        match fs::DirBuilder::new().mode(0o700).create(&layout.dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("could not create {}: {e}", layout.dir.display())),
        }
    }
    let meta = check_owner(&layout.dir, uid)?;
    // The umask can take bits away from what `mode` asked for, and a person
    // can loosen it later; either way the result has to be 0700 exactly.
    if layout.ours && meta.mode() & 0o777 != 0o700 {
        fs::set_permissions(&layout.dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("could not restrict {}: {e}", layout.dir.display()))?;
    }
    Ok(())
}

/// `dir` must be a directory, not a link to one, and belong to `uid`.
pub(crate) fn check_owner(dir: &Path, uid: u32) -> Result<fs::Metadata, String> {
    let meta =
        fs::symlink_metadata(dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?;
    if !meta.file_type().is_dir() {
        return Err(format!(
            "{} is not a directory, refusing to put the socket there",
            dir.display()
        ));
    }
    if meta.uid() != uid {
        return Err(format!(
            "{} belongs to uid {}, not to this user ({uid}), refusing to put the socket there",
            dir.display(),
            meta.uid()
        ));
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn linux_uses_the_runtime_dir() {
        let layout = linux_layout(env(&[("XDG_RUNTIME_DIR", "/run/user/1000")]), 1000);
        assert_eq!(layout.socket(), Path::new("/run/user/1000/cortisoff.sock"));
        assert_eq!(layout.lock(""), Path::new("/run/user/1000/cortisoff.lock"));
        assert_eq!(
            layout.lock("dashboard"),
            Path::new("/run/user/1000/cortisoff-dashboard.lock")
        );
        assert!(!layout.ours);
    }

    #[test]
    fn linux_falls_back_to_a_private_tmp_dir() {
        for vars in [
            &[][..],
            &[("XDG_RUNTIME_DIR", "")][..],
            &[("XDG_RUNTIME_DIR", "relative/dir")][..],
        ] {
            let layout = linux_layout(env(vars), 1000);
            assert_eq!(
                layout.socket(),
                Path::new("/tmp/cortisoff-1000/cortisoff.sock")
            );
            assert!(layout.ours);
        }
    }

    #[test]
    fn macos_uses_tmpdir_with_the_uid_in_the_name() {
        let layout = mac_layout(env(&[("TMPDIR", "/var/folders/ab/xyz/T/")]), 501);
        assert_eq!(
            layout.socket(),
            Path::new("/var/folders/ab/xyz/T/cortisoff-501.sock")
        );
        assert_eq!(
            layout.lock(""),
            Path::new("/var/folders/ab/xyz/T/cortisoff-501.lock")
        );
        assert!(!layout.ours);

        let bare = mac_layout(env(&[]), 501);
        assert_eq!(
            bare.socket(),
            Path::new("/tmp/cortisoff-501/cortisoff.sock")
        );
        assert!(bare.ours);
    }

    #[test]
    fn the_fallback_dir_is_created_private_and_kept_private() {
        let scratch = Scratch::new("dir");
        let layout = Layout {
            dir: scratch.join("own"),
            stem: "cortisoff".to_owned(),
            ours: true,
        };
        prepare(&layout, own_uid()).expect("created");
        let mode = |dir: &Path| fs::metadata(dir).unwrap().mode() & 0o777;
        assert_eq!(mode(&layout.dir), 0o700);

        fs::set_permissions(&layout.dir, fs::Permissions::from_mode(0o755)).unwrap();
        prepare(&layout, own_uid()).expect("tightened");
        assert_eq!(mode(&layout.dir), 0o700);
    }

    #[test]
    fn a_dir_owned_by_someone_else_is_refused() {
        let scratch = Scratch::new("own");
        let someone_else = own_uid().wrapping_add(1);
        let refused = check_owner(scratch.path(), someone_else).unwrap_err();
        assert!(refused.contains("belongs to uid"), "{refused}");

        let layout = Layout {
            dir: scratch.path().to_path_buf(),
            stem: "cortisoff".to_owned(),
            ours: true,
        };
        assert!(prepare(&layout, someone_else).is_err());
    }

    #[test]
    fn a_link_to_a_dir_is_refused() {
        let scratch = Scratch::new("lnk");
        let real = scratch.join("real");
        fs::create_dir(&real).unwrap();
        let link = scratch.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let refused = check_owner(&link, own_uid()).unwrap_err();
        assert!(refused.contains("not a directory"), "{refused}");
    }
}
