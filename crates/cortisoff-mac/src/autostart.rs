//! Start at login through a per-user LaunchAgent.
//!
//! `~/Library/LaunchAgents/dev.mtsu.cortisoff.plist` with `RunAtLoad`, which
//! launchd reads at the next login. Writing it needs no permission and no
//! prompt, and removing it is the whole uninstall. The path is the running
//! executable's, which for an installed copy is inside
//! `~/Applications/Cortisoff.app`, so the agent follows the per-user install.

use std::path::{Path, PathBuf};

pub const LABEL: &str = "dev.mtsu.cortisoff";

/// The agent's property list. The path is escaped because it is the one part
/// a user controls: an `&` in a folder name is enough to make launchd refuse
/// the whole file.
pub fn plist(exe: &Path) -> String {
    let exe = xml_escape(&exe.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>--daemon</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
</dict>
</plist>
"#
    )
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn agent_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| "HOME is not set, so there is no LaunchAgents folder to write".to_owned())?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

/// Writes the agent for `exe`, replacing an older one. Through a temporary
/// file and a rename, so launchd never reads half a plist at login.
pub fn enable(exe: &Path) -> Result<(), String> {
    let path = agent_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let temporary = path.with_extension("plist.tmp");
    std::fs::write(&temporary, plist(exe))
        .map_err(|e| format!("could not write {}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, &path).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("could not install {}: {e}", path.display())
    })
}

/// Removes the agent. Already gone is success.
pub fn disable() -> Result<(), String> {
    let path = agent_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove {}: {e}", path.display())),
    }
}

pub fn enabled() -> bool {
    agent_path().is_ok_and(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_runs_the_daemon_at_login() {
        let text = plist(Path::new(
            "/Users/u/Applications/Cortisoff.app/Contents/MacOS/cortisoff",
        ));
        assert!(text.contains("<string>dev.mtsu.cortisoff</string>"));
        assert!(text.contains(
            "<array>\n\t\t<string>/Users/u/Applications/Cortisoff.app/Contents/MacOS/cortisoff</string>\n\t\t<string>--daemon</string>\n\t</array>"
        ));
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(text.trim_end().ends_with("</plist>"));
    }

    #[test]
    fn a_path_cannot_break_the_xml() {
        let text = plist(Path::new("/Users/a&b/<x>/\"q\"/it's/cortisoff"));
        assert!(text.contains(
            "<string>/Users/a&amp;b/&lt;x&gt;/&quot;q&quot;/it&apos;s/cortisoff</string>"
        ));
        assert_eq!(
            text.matches('<').count(),
            plist(Path::new("/x")).matches('<').count()
        );
    }
}
