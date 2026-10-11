//! The freedesktop launcher entry, as text. Pure so it is tested everywhere,
//! written to disk only on Linux.

use std::path::{Path, PathBuf};

/// `$XDG_DATA_HOME/applications/cortisoff.desktop`, falling back to
/// `~/.local/share` as the specification says when the variable is unset or
/// not absolute.
pub(crate) fn entry_path(home: &Path) -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local").join("share"));
    data.join("applications").join("cortisoff.desktop")
}

pub(crate) fn entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.5\n\
         Name=Cortisoff\n\
         Comment=Automates the small things that make using a computer worse\n\
         Exec={}\n\
         Terminal=false\n\
         Categories=Utility;Settings;\n\
         StartupWMClass=cortisoff\n",
        exec_value(exe)
    )
}

/// Quote the path the way the Exec key wants: inside double quotes, `"`, `` ` ``,
/// `$` and `\` take a backslash, then the value as a whole escapes its
/// backslashes again, and `%` is doubled so it is not read as a field code.
fn exec_value(exe: &Path) -> String {
    let mut quoted = String::from("\"");
    for c in exe.to_string_lossy().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted.replace('\\', "\\\\").replace('%', "%%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_launches_the_installed_app() {
        let text = entry(Path::new("/home/ana/.local/bin/cortisoff"));
        assert_eq!(
            text,
            "[Desktop Entry]\n\
             Type=Application\n\
             Version=1.5\n\
             Name=Cortisoff\n\
             Comment=Automates the small things that make using a computer worse\n\
             Exec=\"/home/ana/.local/bin/cortisoff\"\n\
             Terminal=false\n\
             Categories=Utility;Settings;\n\
             StartupWMClass=cortisoff\n"
        );
    }

    #[test]
    fn awkward_paths_survive_both_levels_of_escaping() {
        assert_eq!(
            exec_value(Path::new("/home/ana b/100%/$x/cortisoff")),
            "\"/home/ana b/100%%/\\\\$x/cortisoff\""
        );
        assert_eq!(exec_value(Path::new("/a\"b")), "\"/a\\\\\"b\"");
    }

    #[test]
    fn the_entry_goes_under_the_data_home() {
        let path = entry_path(Path::new("/home/ana"));
        assert!(
            path.ends_with("applications/cortisoff.desktop"),
            "{}",
            path.display()
        );
    }
}
