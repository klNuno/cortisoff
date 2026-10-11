//! Which plugins are on, their settings, and the folders linked for
//! development. One JSON file the daemon owns: the window changes it only by
//! asking the daemon, so two processes never write it at once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub enabled: bool,
    /// What the user set. Defaults the plugin declares are not stored, so a
    /// new version's new defaults reach every key the user never touched.
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub plugins: BTreeMap<String, Entry>,
    #[serde(default)]
    pub links: Vec<PathBuf>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Store {
    /// Never fails: a missing file is a first run, and a file that does not
    /// parse is set aside and reported rather than losing the daemon.
    pub fn open(path: &Path) -> (Store, Option<String>) {
        let mut warning = None;
        let mut store = match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Store>(&text) {
                Ok(s) => s,
                Err(e) => {
                    let aside = path.with_extension("json.broken");
                    let _ = std::fs::rename(path, &aside);
                    warning = Some(format!(
                        "{} did not parse ({e}). It was moved to {} and every plugin starts \
                         from its defaults",
                        path.display(),
                        aside.display()
                    ));
                    Store::default()
                }
            },
            Err(_) => Store::default(),
        };
        store.path = Some(path.to_path_buf());
        (store, warning)
    }

    pub fn in_memory() -> Store {
        Store::default()
    }

    pub fn entry(&self, id: &str) -> Entry {
        self.plugins.get(id).cloned().unwrap_or_default()
    }

    /// Written beside and renamed over, so a crash mid-write never leaves
    /// half a file.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// `defaults` with every key of `set` laid over it, objects merged key by
/// key and everything else replaced.
pub fn merge(defaults: &serde_json::Value, set: &serde_json::Value) -> serde_json::Value {
    match (defaults, set) {
        (serde_json::Value::Object(d), serde_json::Value::Object(s)) => {
            let mut out = d.clone();
            for (k, v) in s {
                let merged = match d.get(k) {
                    Some(dv) => merge(dv, v),
                    None => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            serde_json::Value::Object(out)
        }
        (_, serde_json::Value::Null) => defaults.clone(),
        (_, s) => s.clone(),
    }
}

/// What of `now` differs from `defaults`: the part worth storing.
pub fn changed(defaults: &serde_json::Value, now: &serde_json::Value) -> serde_json::Value {
    match (defaults, now) {
        (serde_json::Value::Object(d), serde_json::Value::Object(n)) => {
            let mut out = serde_json::Map::new();
            for (k, v) in n {
                match d.get(k) {
                    Some(dv) if same(dv, v) => {}
                    Some(dv @ serde_json::Value::Object(_)) if v.is_object() => {
                        out.insert(k.clone(), changed(dv, v));
                    }
                    _ => {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
            serde_json::Value::Object(out)
        }
        _ if same(defaults, now) => serde_json::Value::Object(Default::default()),
        _ => now.clone(),
    }
}

/// Equal, counting `3200` and `3200.0` as the same number: Lua has one
/// number type and JSON written by hand has two.
fn same(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::Number(x), serde_json::Value::Number(y)) => x.as_f64() == y.as_f64(),
        (serde_json::Value::Array(x), serde_json::Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

pub(crate) fn same_json(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    same(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stored_values_lie_over_new_defaults() {
        let defaults = json!({ "night": 3200, "fade": 30, "screens": { "a": true, "b": true } });
        let set = json!({ "night": 2700, "screens": { "b": false } });
        assert_eq!(
            merge(&defaults, &set),
            json!({ "night": 2700, "fade": 30, "screens": { "a": true, "b": false } })
        );
    }

    #[test]
    fn only_what_moved_is_stored() {
        let defaults = json!({ "night": 3200, "fade": 30, "except": ["vlc.exe"] });
        let now = json!({ "night": 2700.0, "fade": 30.0, "except": ["vlc.exe"] });
        assert_eq!(changed(&defaults, &now), json!({ "night": 2700.0 }));
    }

    #[test]
    fn a_broken_file_is_set_aside() {
        let dir = std::env::temp_dir().join(format!("cortisoff-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plugins.json");
        std::fs::write(&path, "{ nope").unwrap();
        let (store, warning) = Store::open(&path);
        assert!(store.plugins.is_empty());
        assert!(warning.unwrap().contains("did not parse"));
        assert!(dir.join("plugins.json.broken").exists());

        let mut store = store;
        store.plugins.insert(
            "a".into(),
            Entry {
                enabled: true,
                settings: json!({ "x": 1 }),
            },
        );
        store.save().unwrap();
        let (again, warning) = Store::open(&path);
        assert!(warning.is_none());
        assert!(again.entry("a").enabled);
        std::fs::remove_dir_all(&dir).ok();
    }
}
