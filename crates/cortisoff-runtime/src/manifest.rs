//! `plugin.toml`: what cortisoff needs to list a plugin without running it.

use serde::{Deserialize, Serialize};

/// The systems a plugin can name, as `std::env::consts::OS` spells them.
pub const PLATFORMS: &[&str] = &["windows", "linux", "macos"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Lowercase letters, digits, `-` and `_`. The folder name of an
    /// installed copy and the key its settings are stored under.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// Where it runs. A plugin says so itself; cortisoff never guesses.
    pub platforms: Vec<String>,
    #[serde(default = "default_entry")]
    pub entry: String,
    /// Asks for LuaJIT's FFI: native calls into any library, with no sandbox
    /// left. The window warns before such a plugin is switched on.
    #[serde(default)]
    pub ffi: bool,
}

fn default_entry() -> String {
    "init.lua".into()
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        let m: Manifest = toml::from_str(text).map_err(|e| e.to_string())?;
        m.validate()?;
        Ok(m)
    }

    pub fn supports(&self, os: &str) -> bool {
        self.platforms.iter().any(|p| p == os)
    }

    fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        {
            return Err(format!(
                "id {:?} must be 1 to 64 lowercase letters, digits, - or _",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            return Err("name is empty".into());
        }
        if self.platforms.is_empty() {
            return Err("platforms is empty: name at least one of windows, linux, macos".into());
        }
        if let Some(p) = self
            .platforms
            .iter()
            .find(|p| !PLATFORMS.contains(&p.as_str()))
        {
            return Err(format!(
                "platform {p:?} is not one of windows, linux, macos"
            ));
        }
        crate::package::clean_path(&self.entry)
            .ok_or_else(|| format!("entry {:?} must be a path inside the plugin", self.entry))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_names_its_platforms() {
        let m = Manifest::parse(
            "id = \"a\"\nname = \"A\"\nversion = \"1.0.0\"\nplatforms = [\"windows\"]\n",
        )
        .unwrap();
        assert!(m.supports("windows"));
        assert!(!m.supports("linux"));
        assert_eq!(m.entry, "init.lua");
        assert!(!m.ffi);
    }

    #[test]
    fn a_bad_manifest_says_what_is_wrong() {
        let bad = |s: &str| Manifest::parse(s).unwrap_err();
        assert!(
            bad("id = \"A b\"\nname = \"x\"\nversion = \"1\"\nplatforms = [\"linux\"]")
                .contains("lowercase")
        );
        assert!(
            bad("id = \"a\"\nname = \"x\"\nversion = \"1\"\nplatforms = [\"beos\"]")
                .contains("beos")
        );
        assert!(bad(
            "id = \"a\"\nname = \"x\"\nversion = \"1\"\nplatforms = [\"linux\"]\nentry = \"../x.lua\""
        )
        .contains("inside"));
    }
}
