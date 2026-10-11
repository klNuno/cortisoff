//! Installing a plugin from the web: a GitHub repository, or any link to a
//! zip. The window runs this, then asks the daemon to look at the plugin
//! folder again, so a download never blocks the daemon's loop.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::manifest::Manifest;
use crate::package::SOURCE_FILE;

/// Largest archive accepted. A plugin is scripts and a few images.
const ARCHIVE_LIMIT: u64 = 32 * 1024 * 1024;

/// What was installed, and where.
#[derive(Debug, Clone)]
pub struct Installed {
    pub manifest: Manifest,
    pub dir: PathBuf,
    /// Whether it replaced an older copy.
    pub replaced: bool,
}

/// Where a link points, once read.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// `owner/repo`, from `https://github.com/owner/repo`, its `.git` form
    /// or the two words alone.
    GitHub { owner: String, repo: String },
    /// A direct link to a zip.
    Zip(String),
}

impl Source {
    pub fn parse(text: &str) -> Result<Source, String> {
        let t = text.trim().trim_end_matches('/');
        if t.is_empty() {
            return Err("paste a GitHub repository or a link to a zip".into());
        }
        let lower = t.to_ascii_lowercase();
        if lower.ends_with(".zip")
            && (lower.starts_with("https://") || lower.starts_with("http://"))
        {
            return Ok(Source::Zip(t.to_owned()));
        }
        let rest = ["https://github.com/", "http://github.com/", "github.com/"]
            .iter()
            .find_map(|p| lower.starts_with(p).then(|| &t[p.len()..]))
            .unwrap_or(t);
        if rest.contains("://") {
            return Err(format!(
                "{t:?} is neither a GitHub repository nor a link to a zip"
            ));
        }
        let mut parts = rest.split('/');
        let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else {
            return Err(format!("{t:?}: expected owner/repository"));
        };
        let repo = repo.trim_end_matches(".git");
        let ok = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        };
        if !ok(owner) || !ok(repo) {
            return Err(format!("{t:?}: expected owner/repository"));
        }
        Ok(Source::GitHub {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
        })
    }

    /// The link stored beside the plugin, for updating it later.
    pub fn url(&self) -> String {
        match self {
            Source::GitHub { owner, repo } => format!("https://github.com/{owner}/{repo}"),
            Source::Zip(url) => url.clone(),
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .user_agent(concat!("cortisoff/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str, limit: u64) -> Result<(u16, Vec<u8>), String> {
    let mut resp = agent
        .get(url)
        .header(
            "Accept",
            "application/vnd.github+json, application/zip, */*",
        )
        .call()
        .map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status().as_u16();
    let mut bytes = Vec::new();
    resp.body_mut()
        .with_config()
        .limit(limit)
        .reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{url}: {e}"))?;
    Ok((status, bytes))
}

/// The zip to download for a source. For a repository: an asset called
/// `plugin.zip` on its latest release, else that release's source, else the
/// default branch.
fn archive_url(agent: &ureq::Agent, source: &Source) -> Result<String, String> {
    let (owner, repo) = match source {
        Source::Zip(url) => return Ok(url.clone()),
        Source::GitHub { owner, repo } => (owner, repo),
    };
    let api = format!("https://api.github.com/repos/{owner}/{repo}");
    let (status, body) = get(agent, &format!("{api}/releases/latest"), 1024 * 1024)?;
    if status == 200 {
        let release: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| format!("GitHub answered oddly: {e}"))?;
        let asset = release["assets"].as_array().and_then(|assets| {
            assets.iter().find_map(|a| {
                (a["name"].as_str() == Some("plugin.zip"))
                    .then(|| a["browser_download_url"].as_str().map(str::to_owned))
                    .flatten()
            })
        });
        if let Some(url) = asset.or_else(|| release["zipball_url"].as_str().map(str::to_owned)) {
            return Ok(url);
        }
    } else if status != 404 {
        return Err(format!("GitHub answered {status} for {owner}/{repo}"));
    }
    let (status, _) = get(agent, &api, 1024 * 1024)?;
    match status {
        200 => Ok(format!("{api}/zipball")),
        404 => Err(format!("{owner}/{repo} does not exist, or is private")),
        s => Err(format!("GitHub answered {s} for {owner}/{repo}")),
    }
}

/// Downloads, checks and installs a plugin under `plugins`, replacing an
/// older copy of the same id. Blocks: run it off the UI thread.
pub fn install(link: &str, plugins: &Path) -> Result<Installed, String> {
    let source = Source::parse(link)?;
    let agent = agent();
    let url = archive_url(&agent, &source)?;
    let (status, bytes) = get(&agent, &url, ARCHIVE_LIMIT)?;
    if status != 200 {
        return Err(format!("{url} answered {status}"));
    }
    install_zip(&bytes, &source.url(), plugins)
}

/// The part of [`install`] after the download, for tests and for a zip the
/// user already has.
pub fn install_zip(bytes: &[u8], url: &str, plugins: &Path) -> Result<Installed, String> {
    std::fs::create_dir_all(plugins).map_err(|e| format!("{}: {e}", plugins.display()))?;
    let incoming = plugins.join(format!(".incoming-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&incoming);
    let result = unpack(bytes, &incoming).and_then(|()| settle(&incoming, url, plugins));
    let _ = std::fs::remove_dir_all(&incoming);
    result
}

fn unpack(bytes: &[u8], into: &Path) -> Result<(), String> {
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("not a zip: {e}"))?;
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| e.to_string())?;
        // A name that climbs out of the folder is dropped, never followed.
        let Some(rel) = file.enclosed_name() else {
            continue;
        };
        let path = into.join(rel);
        if file.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            continue;
        }
        total += file.size();
        if total > ARCHIVE_LIMIT * 4 {
            return Err("the archive unpacks to more than 128 MB".into());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out =
            std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        std::io::copy(&mut file, &mut out).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The folder holding the shallowest `plugin.toml`: a GitHub archive wraps
/// everything in one `owner-repo-sha/` folder, and a repository may keep
/// its plugin in a subfolder.
fn find_root(dir: &Path, depth: u8) -> Option<PathBuf> {
    if dir.join("plugin.toml").is_file() {
        return Some(dir.to_path_buf());
    }
    if depth == 0 {
        return None;
    }
    let mut subs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subs.sort();
    subs.iter()
        .filter_map(|d| find_root(d, depth - 1).map(|r| (r.components().count(), r)))
        .min_by_key(|(n, _)| *n)
        .map(|(_, r)| r)
}

fn settle(incoming: &Path, url: &str, plugins: &Path) -> Result<Installed, String> {
    let root = find_root(incoming, 3)
        .ok_or("no plugin.toml in the archive: this is not a cortisoff plugin")?;
    let text = std::fs::read_to_string(root.join("plugin.toml")).map_err(|e| e.to_string())?;
    let manifest = Manifest::parse(&text).map_err(|e| format!("plugin.toml: {e}"))?;
    std::fs::read_to_string(root.join(&manifest.entry)).map_err(|_| {
        format!(
            "plugin.toml names {} but the archive has no such file",
            manifest.entry
        )
    })?;
    let source = serde_json::json!({ "url": url, "version": manifest.version });
    std::fs::write(root.join(SOURCE_FILE), source.to_string()).map_err(|e| e.to_string())?;

    let dir = plugins.join(&manifest.id);
    let old = plugins.join(format!(".old-{}", manifest.id));
    let replaced = dir.exists();
    if replaced {
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(&dir, &old).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if let Err(e) = std::fs::rename(&root, &dir) {
        if replaced {
            let _ = std::fs::rename(&old, &dir);
        }
        return Err(format!("{}: {e}", dir.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(Installed {
        manifest,
        dir,
        replaced,
    })
}

/// Removes an installed plugin's folder. Refuses anything outside `plugins`.
pub fn uninstall(dir: &Path, plugins: &Path) -> Result<(), String> {
    let (Ok(dir), Ok(plugins)) = (dir.canonicalize(), plugins.canonicalize()) else {
        return Err(format!("{} is gone already", dir.display()));
    };
    if dir.parent() != Some(plugins.as_path()) {
        return Err(format!("{} is not an installed plugin", dir.display()));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn links_are_read_the_ways_people_paste_them() {
        let gh = |o: &str, r: &str| Source::GitHub {
            owner: o.into(),
            repo: r.into(),
        };
        assert_eq!(
            Source::parse("klNuno/night").unwrap(),
            gh("klNuno", "night")
        );
        assert_eq!(
            Source::parse("https://github.com/a/b.git").unwrap(),
            gh("a", "b")
        );
        assert_eq!(
            Source::parse(" github.com/a/b/tree/main ").unwrap(),
            gh("a", "b")
        );
        assert_eq!(
            Source::parse("https://x.dev/p.zip").unwrap(),
            Source::Zip("https://x.dev/p.zip".into())
        );
        assert!(Source::parse("https://gitlab.com/a/b").is_err());
        assert!(Source::parse("nope").is_err());
    }

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, text) in files {
            w.start_file(*name, opts).unwrap();
            w.write_all(text.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn a_github_archive_installs_under_its_id() {
        let plugins =
            std::env::temp_dir().join(format!("cortisoff-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&plugins);
        let manifest =
            "id = \"clock\"\nname = \"Clock\"\nversion = \"1.0.0\"\nplatforms = [\"windows\"]\n";
        let bytes = zip_of(&[
            ("me-clock-abc123/README.md", "hi"),
            ("me-clock-abc123/plugin.toml", manifest),
            ("me-clock-abc123/init.lua", "return {}"),
            ("me-clock-abc123/../../evil.lua", "return {}"),
        ]);
        let done = install_zip(&bytes, "https://github.com/me/clock", &plugins).unwrap();
        assert_eq!(done.manifest.id, "clock");
        assert!(!done.replaced);
        assert!(plugins.join("clock/init.lua").is_file());
        assert!(plugins.join("clock").join(SOURCE_FILE).is_file());
        assert!(!plugins.join("evil.lua").exists());

        let again = install_zip(&bytes, "https://github.com/me/clock", &plugins).unwrap();
        assert!(again.replaced);
        let left: Vec<_> = std::fs::read_dir(&plugins).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "only the plugin folder is left behind");

        uninstall(&plugins.join("clock"), &plugins).unwrap();
        assert!(!plugins.join("clock").exists());
        std::fs::remove_dir_all(&plugins).ok();
    }

    #[test]
    fn an_archive_without_a_plugin_says_so() {
        let plugins = std::env::temp_dir().join(format!("cortisoff-noplug-{}", std::process::id()));
        let err = install_zip(&zip_of(&[("a/readme.md", "x")]), "u", &plugins).unwrap_err();
        assert!(err.contains("no plugin.toml"), "{err}");
        std::fs::remove_dir_all(&plugins).ok();
    }
}
