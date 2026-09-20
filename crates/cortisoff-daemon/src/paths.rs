//! Where the rule file, the journal and the socket live on each platform.
//!
//! Resolved from environment variables through an injected lookup, so the
//! layout of all three platforms is tested on any one of them. Nothing here
//! touches the filesystem: it computes paths and hands them back.

use std::path::PathBuf;

/// Which layout to compute. Chosen once at startup from the running platform,
/// and named explicitly so a test can ask for the other two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Windows,
    MacOs,
    Xdg,
}

impl Layout {
    /// The layout of the platform this binary is running on.
    pub fn native() -> Self {
        if cfg!(windows) {
            Layout::Windows
        } else if cfg!(target_os = "macos") {
            Layout::MacOs
        } else {
            Layout::Xdg
        }
    }
}

/// Everything the daemon needs from the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Hand-edited by the user, so it lives with configuration rather than
    /// with state.
    pub rules: PathBuf,
    /// Rewritten by the daemon and meaningless to a human, so it lives with
    /// state and is never in a backup the user restores by hand.
    pub journal: PathBuf,
    /// Why the daemon is or is not running, kept next to the journal because
    /// it describes this machine's runs and nothing the user authored.
    pub log: PathBuf,
}

/// Missing environment, named so the message can say which variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathError(pub String);

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PathError {}

impl Paths {
    /// The real thing, from this process's environment.
    pub fn from_env() -> Result<Self, PathError> {
        Self::resolve(Layout::native(), |k| std::env::var(k).ok())
    }

    /// `env` returns a variable's value, or `None` when it is unset or empty.
    /// An empty value counts as unset: `HOME=` would otherwise put the rule
    /// file at `/.config/cortisoff`.
    pub fn resolve(
        layout: Layout,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, PathError> {
        let get = |key: &str| env(key).filter(|v| !v.is_empty()).map(PathBuf::from);
        let need = |key: &str| {
            get(key).ok_or_else(|| {
                PathError(format!(
                    "{key} is not set, so cortisoff cannot find its own files"
                ))
            })
        };

        let (config, state) = match layout {
            // Roaming for the rules so they follow a domain profile, local for
            // the journal because it describes this machine's hardware.
            Layout::Windows => (need("APPDATA")?, need("LOCALAPPDATA")?),
            Layout::MacOs => {
                let home = need("HOME")?;
                let support = home.join("Library").join("Application Support");
                (support.clone(), support)
            }
            Layout::Xdg => {
                let home = || need("HOME");
                let config = match get("XDG_CONFIG_HOME") {
                    Some(p) => p,
                    None => home()?.join(".config"),
                };
                let state = match get("XDG_STATE_HOME") {
                    Some(p) => p,
                    None => home()?.join(".local").join("state"),
                };
                (config, state)
            }
        };

        Ok(Paths {
            rules: config.join("cortisoff").join("rules.toml"),
            journal: state.join("cortisoff").join("journal.json"),
            log: state.join("cortisoff").join("cortisoffd.log"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn windows_splits_roaming_rules_from_local_state() {
        let p = Paths::resolve(
            Layout::Windows,
            env(&[
                ("APPDATA", r"C:\Users\x\AppData\Roaming"),
                ("LOCALAPPDATA", r"C:\Users\x\AppData\Local"),
            ]),
        )
        .unwrap();
        assert!(p.rules.ends_with("cortisoff/rules.toml"));
        assert!(p.rules.starts_with(r"C:\Users\x\AppData\Roaming"));
        assert!(p.journal.starts_with(r"C:\Users\x\AppData\Local"));
        // The log describes this machine's runs, so it sits with the journal
        // rather than with the roaming rules.
        assert!(p.log.ends_with("cortisoff/cortisoffd.log"));
        assert!(p.log.starts_with(r"C:\Users\x\AppData\Local"));
    }

    #[test]
    fn xdg_falls_back_to_home_when_the_variables_are_absent() {
        let p = Paths::resolve(Layout::Xdg, env(&[("HOME", "/home/x")])).unwrap();
        assert_eq!(
            p.rules,
            PathBuf::from("/home/x/.config/cortisoff/rules.toml")
        );
        assert_eq!(
            p.journal,
            PathBuf::from("/home/x/.local/state/cortisoff/journal.json")
        );
    }

    #[test]
    fn xdg_variables_win_over_home() {
        let p = Paths::resolve(
            Layout::Xdg,
            env(&[("HOME", "/home/x"), ("XDG_CONFIG_HOME", "/cfg")]),
        )
        .unwrap();
        assert_eq!(p.rules, PathBuf::from("/cfg/cortisoff/rules.toml"));
        // The one that was not overridden still falls back to HOME.
        assert!(p.journal.starts_with("/home/x"));
    }

    #[test]
    fn macos_puts_both_under_application_support() {
        let p = Paths::resolve(Layout::MacOs, env(&[("HOME", "/Users/x")])).unwrap();
        assert!(p.rules.starts_with("/Users/x/Library/Application Support"));
        assert!(p
            .journal
            .starts_with("/Users/x/Library/Application Support"));
    }

    /// `HOME=` is not a home directory, and treating it as one writes the rule
    /// file to the filesystem root.
    #[test]
    fn an_empty_variable_counts_as_unset() {
        let err = Paths::resolve(Layout::Xdg, env(&[("HOME", "")])).unwrap_err();
        assert!(err.0.contains("HOME"));
    }

    #[test]
    fn a_missing_variable_names_itself() {
        let err = Paths::resolve(Layout::Windows, env(&[("APPDATA", "C:\r")])).unwrap_err();
        assert!(err.0.contains("LOCALAPPDATA"));
    }
}
