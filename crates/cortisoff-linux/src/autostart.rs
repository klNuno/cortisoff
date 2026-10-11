//! Starting the daemon with the desktop session, through the XDG autostart
//! directory that GNOME, KDE, Xfce and the other freedesktop sessions read.
//!
//! One file per user, no service manager and no root: the session starts it
//! after the desktop is up, inside the session, with its `DISPLAY` set.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const FILE: &str = "cortisoff.desktop";

pub fn enable(exe: &Path) -> Result<(), String> {
    let dir =
        dir().ok_or("neither XDG_CONFIG_HOME nor HOME says where this user's settings live")?;
    let entry = desktop_entry(exe)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let file = dir.join(FILE);
    std::fs::write(&file, entry).map_err(|e| format!("could not write {}: {e}", file.display()))
}

pub fn disable() -> Result<(), String> {
    let Some(file) = dir().map(|d| d.join(FILE)) else {
        return Ok(());
    };
    match std::fs::remove_file(&file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove {}: {e}", file.display())),
    }
}

/// The file is there and the session will run it. A session's own settings
/// switch an entry off by writing `Hidden=true` or GNOME's flag into it
/// rather than deleting it, so those count as off.
pub fn enabled() -> bool {
    dir()
        .and_then(|d| std::fs::read_to_string(d.join(FILE)).ok())
        .is_some_and(|text| runs(&text))
}

fn runs(text: &str) -> bool {
    !text.lines().map(str::trim).any(|line| {
        line.eq_ignore_ascii_case("Hidden=true")
            || line.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
    })
}

fn dir() -> Option<PathBuf> {
    dir_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// The base directory spec ignores a relative `XDG_CONFIG_HOME`, and so does
/// every session reading it.
fn dir_from(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let config = xdg_config_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".config"))
        })?;
    Some(config.join("autostart"))
}

fn desktop_entry(exe: &Path) -> Result<String, String> {
    let exe = exe.to_str().ok_or_else(|| {
        format!(
            "{} is not valid UTF-8, which a .desktop file must be",
            exe.display()
        )
    })?;
    if exe.chars().any(char::is_control) {
        return Err(format!(
            "{exe:?} holds a control character, which an Exec line cannot carry"
        ));
    }
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=cortisoff\n\
         Exec={} --daemon\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n",
        exec_argument(exe)
    ))
}

/// One quoted `Exec` argument. The desktop entry spec escapes in two
/// layers: inside the quotes `"`, `` ` ``, `$` and `\` take a backslash, and
/// then the value as a whole doubles every backslash, because string values
/// are unescaped before the quotes are read. A `%` is a field code unless
/// doubled.
fn exec_argument(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    let mut value = String::with_capacity(quoted.len() + 4);
    value.push('"');
    for c in quoted.chars() {
        match c {
            '\\' => value.push_str("\\\\"),
            '%' => value.push_str("%%"),
            c => value.push(c),
        }
    }
    value.push('"');
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_starts_the_daemon_from_the_given_path() {
        let entry = desktop_entry(Path::new("/opt/cortisoff/cortisoff")).unwrap();
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nType=Application\n"));
        assert!(entry.contains("\nExec=\"/opt/cortisoff/cortisoff\" --daemon\n"));
        assert!(runs(&entry));
    }

    #[test]
    fn spaces_and_reserved_characters_survive_both_escaping_layers() {
        assert_eq!(exec_argument("/home/a b/my app"), "\"/home/a b/my app\"");
        assert_eq!(exec_argument("/x/$HOME"), r#""/x/\\$HOME""#);
        assert_eq!(exec_argument(r"/x/back\slash"), r#""/x/back\\\\slash""#);
        assert_eq!(exec_argument("/x/say\"hi\""), r#""/x/say\\"hi\\"""#);
        assert_eq!(exec_argument("/x/100%"), "\"/x/100%%\"");
    }

    #[test]
    fn a_path_with_a_newline_is_refused() {
        assert!(desktop_entry(Path::new("/tmp/evil\nExec=rm")).is_err());
    }

    #[test]
    fn an_entry_the_session_switched_off_does_not_count() {
        assert!(!runs("[Desktop Entry]\nExec=x\nHidden=true\n"));
        assert!(!runs("[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n"));
    }

    #[test]
    fn the_directory_follows_xdg_config_home_then_home() {
        let some = |s: &str| Some(OsString::from(s));
        assert_eq!(
            dir_from(some("/cfg"), some("/home/u")),
            Some(PathBuf::from("/cfg/autostart"))
        );
        assert_eq!(
            dir_from(some("relative"), some("/home/u")),
            Some(PathBuf::from("/home/u/.config/autostart")),
            "a relative XDG_CONFIG_HOME is ignored"
        );
        assert_eq!(
            dir_from(None, some("/home/u")),
            Some(PathBuf::from("/home/u/.config/autostart"))
        );
        assert_eq!(dir_from(None, None), None);
    }
}
