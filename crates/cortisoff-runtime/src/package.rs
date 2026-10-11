//! Where a plugin's files come from: inside the binary, a downloaded folder,
//! or a folder the user is writing it in.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use cortisoff_ipc::Origin;

use crate::manifest::Manifest;

/// The file an installer leaves beside a downloaded plugin, saying where it
/// came from so an update knows where to look.
pub const SOURCE_FILE: &str = ".cortisoff-source.json";

#[derive(Debug, Clone)]
pub enum Files {
    /// Path and text of every file, compiled in.
    Builtin(&'static [(&'static str, &'static str)]),
    Dir(PathBuf),
}

impl Files {
    /// A file of the plugin by its path inside it, `/` separated. Nothing
    /// outside the plugin can be named.
    pub fn read(&self, rel: &str) -> Result<String, String> {
        let clean = clean_path(rel).ok_or_else(|| format!("{rel:?} is outside the plugin"))?;
        match self {
            Files::Builtin(files) => files
                .iter()
                .find(|(path, _)| *path == clean)
                .map(|(_, text)| (*text).to_owned())
                .ok_or_else(|| format!("{clean} is not part of this plugin")),
            Files::Dir(dir) => std::fs::read_to_string(dir.join(&clean))
                .map_err(|e| format!("{clean}: {e}")),
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        match self {
            Files::Dir(dir) => Some(dir),
            Files::Builtin(_) => None,
        }
    }

    /// The newest modification time of any script or manifest in the
    /// folder, a few levels deep. What hot reload compares. `None` for a
    /// built-in plugin, which never changes under a running binary.
    pub fn stamp(&self) -> Option<SystemTime> {
        fn walk(dir: &Path, depth: u8, newest: &mut Option<SystemTime>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    if depth > 0 {
                        walk(&path, depth - 1, newest);
                    }
                    continue;
                }
                let script = matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("lua" | "toml")
                );
                if let (true, Ok(modified)) = (script, meta.modified()) {
                    if newest.is_none_or(|n| modified > n) {
                        *newest = Some(modified);
                    }
                }
            }
        }
        let mut newest = None;
        walk(self.dir()?, 3, &mut newest);
        newest
    }
}

/// `a/b.lua` for `a/b.lua`, `./a\b.lua`; `None` for anything that climbs out
/// or is absolute.
pub(crate) fn clean_path(rel: &str) -> Option<String> {
    let mut parts = Vec::new();
    for c in Path::new(&rel.replace('\\', "/")).components() {
        match c {
            Component::Normal(p) => parts.push(p.to_str()?.to_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub files: Files,
    pub origin: Origin,
    /// Where an installed copy was downloaded from.
    pub url: Option<String>,
}

impl Package {
    pub fn from_dir(dir: &Path, origin: Origin) -> Result<Package, String> {
        let files = Files::Dir(dir.to_path_buf());
        let manifest = Manifest::parse(&files.read("plugin.toml")?)
            .map_err(|e| format!("{}: plugin.toml: {e}", dir.display()))?;
        let url = std::fs::read_to_string(dir.join(SOURCE_FILE))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("url")?.as_str().map(str::to_owned));
        Ok(Package {
            manifest,
            files,
            origin,
            url,
        })
    }
}

macro_rules! builtin {
    ($dir:literal: $($file:literal),+) => {
        &[$((
            $file,
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins/", $dir, "/", $file)),
        )),+]
    };
}

/// The plugins shipped inside cortisoff.
pub fn builtin() -> Vec<Package> {
    const ALL: &[&[(&str, &str)]] = &[
        builtin!("night-light": "plugin.toml", "init.lua"),
        builtin!("display-profiles": "plugin.toml", "init.lua"),
        builtin!("wallpaper-pause": "plugin.toml", "init.lua"),
        builtin!("shortcuts": "plugin.toml", "init.lua"),
    ];
    ALL.iter()
        .map(|files| {
            let files = Files::Builtin(files);
            let manifest = Manifest::parse(&files.read("plugin.toml").expect("a manifest"))
                .expect("a built-in manifest parses");
            Package {
                manifest,
                files,
                origin: Origin::Builtin,
                url: None,
            }
        })
        .collect()
}

/// Every plugin cortisoff can see: built in, installed under `installed`,
/// then linked folders. A later one with the same id replaces an earlier
/// one, so a linked working copy of a built-in plugin is what runs. Returns
/// a sentence for each folder that could not be read.
pub fn discover(installed: &Path, links: &[PathBuf]) -> (Vec<Package>, Vec<String>) {
    let mut found: BTreeMap<String, Package> = BTreeMap::new();
    let mut order = Vec::new();
    let mut problems = Vec::new();
    let mut add = |p: Package, found: &mut BTreeMap<String, Package>| {
        if !found.contains_key(&p.manifest.id) {
            order.push(p.manifest.id.clone());
        }
        found.insert(p.manifest.id.clone(), p);
    };
    for p in builtin() {
        add(p, &mut found);
    }
    if let Ok(entries) = std::fs::read_dir(installed) {
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            match Package::from_dir(&dir, Origin::Installed) {
                Ok(p) => add(p, &mut found),
                Err(e) => problems.push(e),
            }
        }
    }
    for dir in links {
        match Package::from_dir(dir, Origin::Linked) {
            Ok(p) => add(p, &mut found),
            Err(e) => problems.push(e),
        }
    }
    let packages = order
        .into_iter()
        .filter_map(|id| found.remove(&id))
        .collect();
    (packages, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_plugin() {
        assert_eq!(clean_path("a/b.lua").as_deref(), Some("a/b.lua"));
        assert_eq!(clean_path("./a\\b.lua").as_deref(), Some("a/b.lua"));
        assert_eq!(clean_path("../x.lua"), None);
        assert_eq!(clean_path("/etc/passwd"), None);
        assert_eq!(clean_path(""), None);
    }

    #[test]
    fn every_builtin_parses_and_names_its_platforms() {
        let all = builtin();
        assert!(all.iter().any(|p| p.manifest.id == "night-light"));
        for p in &all {
            assert!(!p.manifest.platforms.is_empty());
            p.files.read(&p.manifest.entry).unwrap();
        }
    }

    #[test]
    fn a_linked_copy_replaces_the_builtin_one() {
        let dir = std::env::temp_dir().join(format!("cortisoff-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            "id = \"night-light\"\nname = \"Mine\"\nversion = \"9\"\nplatforms = [\"windows\", \"linux\", \"macos\"]\n",
        )
        .unwrap();
        let (found, problems) = discover(&dir.join("none"), std::slice::from_ref(&dir));
        assert!(problems.is_empty(), "{problems:?}");
        let night = found.iter().find(|p| p.manifest.id == "night-light").unwrap();
        assert_eq!(night.manifest.name, "Mine");
        assert_eq!(night.origin, Origin::Linked);
        assert_eq!(found.iter().filter(|p| p.manifest.id == "night-light").count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
