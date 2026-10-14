//! The release feed: a `latest.json` in the shape the Tauri updater reads, so
//! the release tooling and habits of the other projects carry over unchanged.

use std::collections::BTreeMap;

use serde::Deserialize;

/// One release, narrowed to the build for this platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The version the feed announces, without a leading `v`.
    pub version: String,
    /// Release notes as written in the feed, empty when it carries none.
    pub notes: String,
    /// RFC 3339 publication date, when the feed gives one.
    pub pub_date: Option<String>,
    /// The platform key the entry was picked for, `windows-x86_64` and so on.
    pub platform: String,
    /// Where the bare executable is downloaded from.
    pub url: String,
    /// The minisign signature of that executable, as the feed spells it:
    /// base64 of the `.sig` file like Tauri, or the `.sig` text itself.
    pub signature: String,
    /// The same release's `cortisoff-cli`, when the installed app has a
    /// client beside it to update and the feed carries one.
    pub client: Option<ClientBuild>,
}

/// Where a release's `cortisoff-cli` comes from, checked like the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientBuild {
    /// The client's feed key, `windows-x86_64-cli` and so on.
    pub platform: String,
    pub url: String,
    pub signature: String,
}

#[derive(Deserialize)]
struct Feed {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    pub_date: Option<String>,
    platforms: BTreeMap<String, Entry>,
}

#[derive(Deserialize)]
struct Entry {
    url: String,
    signature: String,
}

/// The feed key of the build running now, `None` on a platform no release
/// ships for.
pub fn platform_key() -> Option<&'static str> {
    platform_key_for(std::env::consts::OS, std::env::consts::ARCH)
}

pub(crate) fn platform_key_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("windows-x86_64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("macos", "aarch64") => Some("darwin-aarch64"),
        ("macos", "x86_64") => Some("darwin-x86_64"),
        _ => None,
    }
}

/// The file name a release publishes the app under for `platform`, which is
/// also the name its signature's trusted comment carries. A macOS `-app` key
/// is the zipped, signed `Cortisoff.app`.
pub fn artifact_name(platform: &str) -> String {
    if let Some(base) = client_base(platform) {
        let suffix = if base.starts_with("windows-") {
            ".exe"
        } else {
            ""
        };
        format!("cortisoff-cli-{base}{suffix}")
    } else if platform.starts_with("windows-") {
        format!("cortisoff-{platform}.exe")
    } else if let Some(base) = bundle_base(platform) {
        format!("cortisoff-{base}.zip")
    } else {
        format!("cortisoff-{platform}")
    }
}

/// `darwin-x86_64` for `darwin-x86_64-app`: the platform a bundle key
/// stands for.
fn bundle_base(platform: &str) -> Option<&str> {
    platform
        .strip_suffix("-app")
        .filter(|base| base.starts_with("darwin-"))
}

/// The feed key to update from. A macOS build running inside an app bundle
/// takes the whole bundle (`darwin-<arch>-app`), so its client, its
/// `Info.plist` and its signature move together; a bare executable, which
/// is what v0.2.0 installed, takes the bare key.
pub(crate) fn update_key_for(platform: Option<&str>, in_bundle: bool) -> Option<String> {
    let platform = platform?;
    if in_bundle && platform.starts_with("darwin-") {
        Some(format!("{platform}-app"))
    } else {
        Some(platform.to_owned())
    }
}

/// `windows-x86_64` for `windows-x86_64-cli`: the platform a client key
/// stands for. macOS ships its client inside the bundle, so has none.
fn client_base(platform: &str) -> Option<&str> {
    platform
        .strip_suffix("-cli")
        .filter(|base| base.starts_with("windows-") || base.starts_with("linux-"))
}

/// The feed key of the client to update with the app: on Windows and Linux,
/// only when an installed client sits beside the app. Nothing on macOS,
/// whose bundle carries its client.
pub(crate) fn client_key_for(platform: Option<&str>, has_client: bool) -> Option<String> {
    let key = format!("{}-cli", platform?);
    (has_client && client_base(&key).is_some()).then_some(key)
}

/// Whether `platform` is a key whose artifact is a zipped bundle.
pub(crate) fn is_bundle_key(platform: &str) -> bool {
    bundle_base(platform).is_some()
}

pub(crate) fn parse_version(text: &str) -> Result<semver::Version, String> {
    let trimmed = text.trim();
    let bare = trimmed.strip_prefix('v').unwrap_or(trimmed);
    semver::Version::parse(bare).map_err(|e| format!("`{text}` is not a version: {e}"))
}

#[cfg(test)]
pub(crate) fn pick(
    json: &str,
    current: &str,
    platform: Option<&str>,
) -> Result<Option<Release>, String> {
    pick_with_client(json, current, platform, None)
}

/// Read a feed and answer the release to install, if the feed is newer than
/// `current`. A newer release with no build for `platform` is an error rather
/// than silence: the release pipeline always builds every key, so a missing
/// one is a broken release someone should hear about. The client's entry,
/// read under `client` when given, is optional: without it the app updates
/// alone, as it did before releases carried one.
pub(crate) fn pick_with_client(
    json: &str,
    current: &str,
    platform: Option<&str>,
    client: Option<&str>,
) -> Result<Option<Release>, String> {
    let feed: Feed =
        serde_json::from_str(json).map_err(|e| format!("the update feed is not readable: {e}"))?;
    let offered = parse_version(&feed.version)?;
    let running = parse_version(current)?;
    if offered <= running {
        return Ok(None);
    }
    let Some(platform) = platform else {
        return Err(format!(
            "version {offered} is out, but no build is published for {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    };
    let entry = feed
        .platforms
        .get(platform)
        .ok_or_else(|| format!("version {offered} has no build for {platform}"))?;
    // The signature is what protects the download; https keeps the feed's
    // promise about where it comes from honest as well.
    if !entry.url.starts_with("https://") {
        return Err(format!(
            "version {offered} points at a non-https download: {}",
            entry.url
        ));
    }
    let client = client.and_then(|key| {
        let entry = feed.platforms.get(key)?;
        entry.url.starts_with("https://").then(|| ClientBuild {
            platform: key.to_owned(),
            url: entry.url.clone(),
            signature: entry.signature.trim().to_owned(),
        })
    });
    Ok(Some(Release {
        version: offered.to_string(),
        notes: feed.notes.unwrap_or_default(),
        pub_date: feed.pub_date,
        platform: platform.to_owned(),
        url: entry.url.clone(),
        signature: entry.signature.trim().to_owned(),
        client,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // What tauri-action writes, keys and all.
    const TAURI_SHAPED: &str = r#"{
      "version": "v1.4.0",
      "notes": "Faster fades.",
      "pub_date": "2026-10-11T20:15:03.123Z",
      "platforms": {
        "windows-x86_64": {
          "signature": "dW50cnVzdGVkIGNvbW1lbnQ=",
          "url": "https://github.com/klNuno/cortisoff/releases/download/v1.4.0/cortisoff-windows-x86_64.exe"
        },
        "linux-x86_64": {
          "signature": "bGludXg=",
          "url": "https://github.com/klNuno/cortisoff/releases/download/v1.4.0/cortisoff-linux-x86_64"
        },
        "darwin-aarch64": {
          "signature": "bWFj",
          "url": "https://github.com/klNuno/cortisoff/releases/download/v1.4.0/cortisoff-darwin-aarch64"
        }
      }
    }"#;

    #[test]
    fn a_tauri_shaped_feed_yields_this_platforms_entry() {
        let release = pick(TAURI_SHAPED, "1.3.9", Some("linux-x86_64"))
            .unwrap()
            .expect("1.4.0 is newer");
        assert_eq!(release.version, "1.4.0");
        assert_eq!(release.notes, "Faster fades.");
        assert_eq!(
            release.pub_date.as_deref(),
            Some("2026-10-11T20:15:03.123Z")
        );
        assert_eq!(release.platform, "linux-x86_64");
        assert!(release.url.ends_with("/cortisoff-linux-x86_64"));
        assert_eq!(release.signature, "bGludXg=");
    }

    #[test]
    fn notes_and_date_are_optional() {
        let json = r#"{"version":"2.0.0","platforms":{"windows-x86_64":{"url":"https://x/y.exe","signature":"s"}}}"#;
        let release = pick(json, "1.0.0", Some("windows-x86_64"))
            .unwrap()
            .unwrap();
        assert_eq!(release.notes, "");
        assert_eq!(release.pub_date, None);
    }

    #[test]
    fn the_same_or_an_older_version_is_no_update() {
        assert_eq!(pick(TAURI_SHAPED, "1.4.0", Some("linux-x86_64")), Ok(None));
        assert_eq!(pick(TAURI_SHAPED, "v1.4.0", Some("linux-x86_64")), Ok(None));
        assert_eq!(pick(TAURI_SHAPED, "1.10.0", Some("linux-x86_64")), Ok(None));
    }

    #[test]
    fn versions_compare_as_semver_not_as_text() {
        let newer = |feed: &str, current: &str| {
            parse_version(feed).unwrap() > parse_version(current).unwrap()
        };
        assert!(newer("1.10.0", "1.9.9"));
        assert!(newer("v2.0.0", "1.99.99"));
        assert!(newer("1.0.0", "1.0.0-beta.3"));
        assert!(!newer("1.0.0-beta.3", "1.0.0"));
        assert!(!newer("0.9.0", "v0.10.0"));
        assert!(parse_version("1.0").is_err());
        assert!(parse_version("latest").is_err());
    }

    #[test]
    fn a_newer_release_missing_this_platform_says_so() {
        let error = pick(TAURI_SHAPED, "1.0.0", Some("darwin-x86_64")).unwrap_err();
        assert!(error.contains("darwin-x86_64"), "{error}");
        assert!(pick(TAURI_SHAPED, "1.0.0", None).is_err());
    }

    #[test]
    fn a_plain_http_download_is_refused() {
        let json = r#"{"version":"2.0.0","platforms":{"linux-x86_64":{"url":"http://x/y","signature":"s"}}}"#;
        assert!(pick(json, "1.0.0", Some("linux-x86_64")).is_err());
    }

    #[test]
    fn a_broken_feed_is_an_error_not_a_panic() {
        assert!(pick("<html>rate limited</html>", "1.0.0", Some("linux-x86_64")).is_err());
        assert!(pick(r#"{"version":"1.0.0"}"#, "0.1.0", Some("linux-x86_64")).is_err());
        assert!(pick(TAURI_SHAPED, "not-a-version", Some("linux-x86_64")).is_err());
    }

    #[test]
    fn platform_keys_match_the_release_matrix() {
        assert_eq!(
            platform_key_for("windows", "x86_64"),
            Some("windows-x86_64")
        );
        assert_eq!(platform_key_for("linux", "x86_64"), Some("linux-x86_64"));
        assert_eq!(platform_key_for("macos", "aarch64"), Some("darwin-aarch64"));
        assert_eq!(platform_key_for("macos", "x86_64"), Some("darwin-x86_64"));
        assert_eq!(platform_key_for("linux", "aarch64"), None);
        assert_eq!(platform_key_for("windows", "aarch64"), None);
        assert_eq!(
            platform_key(),
            platform_key_for(std::env::consts::OS, std::env::consts::ARCH)
        );
    }

    #[test]
    fn artifact_names_match_the_release_workflow() {
        assert_eq!(
            artifact_name("windows-x86_64"),
            "cortisoff-windows-x86_64.exe"
        );
        assert_eq!(artifact_name("linux-x86_64"), "cortisoff-linux-x86_64");
        assert_eq!(artifact_name("darwin-aarch64"), "cortisoff-darwin-aarch64");
        assert_eq!(
            artifact_name("darwin-aarch64-app"),
            "cortisoff-darwin-aarch64.zip"
        );
        assert_eq!(
            artifact_name("darwin-x86_64-app"),
            "cortisoff-darwin-x86_64.zip"
        );
        // Only macOS ships a bundle.
        assert_eq!(
            artifact_name("linux-x86_64-app"),
            "cortisoff-linux-x86_64-app"
        );
        assert_eq!(
            artifact_name("windows-x86_64-cli"),
            "cortisoff-cli-windows-x86_64.exe"
        );
        assert_eq!(
            artifact_name("linux-x86_64-cli"),
            "cortisoff-cli-linux-x86_64"
        );
        // The mac client is inside the bundle: no key of its own.
        assert_eq!(
            artifact_name("darwin-x86_64-cli"),
            "cortisoff-darwin-x86_64-cli"
        );
        assert!(is_bundle_key("darwin-x86_64-app"));
        assert!(!is_bundle_key("darwin-x86_64"));
        assert!(!is_bundle_key("linux-x86_64-app"));
    }

    #[test]
    fn a_bundle_reads_its_app_key_and_a_bare_executable_the_bare_one() {
        assert_eq!(
            update_key_for(Some("darwin-x86_64"), true).as_deref(),
            Some("darwin-x86_64-app")
        );
        assert_eq!(
            update_key_for(Some("darwin-aarch64"), true).as_deref(),
            Some("darwin-aarch64-app")
        );
        // v0.2.0's bare install keeps the key it always read.
        assert_eq!(
            update_key_for(Some("darwin-x86_64"), false).as_deref(),
            Some("darwin-x86_64")
        );
        // Windows and Linux are untouched whatever the flag says.
        assert_eq!(
            update_key_for(Some("windows-x86_64"), true).as_deref(),
            Some("windows-x86_64")
        );
        assert_eq!(
            update_key_for(Some("linux-x86_64"), true).as_deref(),
            Some("linux-x86_64")
        );
        assert_eq!(update_key_for(None, true), None);
    }

    #[test]
    fn a_feed_with_both_mac_keys_serves_each_its_own() {
        let json = r#"{"version":"2.0.0","platforms":{
            "darwin-x86_64":{"url":"https://x/cortisoff-darwin-x86_64","signature":"bare"},
            "darwin-x86_64-app":{"url":"https://x/cortisoff-darwin-x86_64.zip","signature":"app"}}}"#;
        let app = pick(json, "1.0.0", Some("darwin-x86_64-app"))
            .unwrap()
            .unwrap();
        assert_eq!(app.signature, "app");
        assert!(app.url.ends_with(".zip"));
        assert_eq!(artifact_name(&app.platform), "cortisoff-darwin-x86_64.zip");
        let bare = pick(json, "1.0.0", Some("darwin-x86_64")).unwrap().unwrap();
        assert_eq!(bare.signature, "bare");
    }

    #[test]
    fn the_client_key_is_read_only_beside_an_installed_client() {
        assert_eq!(
            client_key_for(Some("windows-x86_64"), true).as_deref(),
            Some("windows-x86_64-cli")
        );
        assert_eq!(
            client_key_for(Some("linux-x86_64"), true).as_deref(),
            Some("linux-x86_64-cli")
        );
        assert_eq!(client_key_for(Some("windows-x86_64"), false), None);
        assert_eq!(client_key_for(Some("linux-x86_64"), false), None);
        assert_eq!(client_key_for(Some("darwin-x86_64"), true), None);
        assert_eq!(client_key_for(None, true), None);
    }

    #[test]
    fn a_feed_with_a_client_key_carries_its_build() {
        let json = r#"{"version":"2.0.0","platforms":{
            "linux-x86_64":{"url":"https://x/cortisoff-linux-x86_64","signature":"app"},
            "linux-x86_64-cli":{"url":"https://x/cortisoff-cli-linux-x86_64","signature":" cli "}}}"#;
        let release = pick_with_client(
            json,
            "1.0.0",
            Some("linux-x86_64"),
            Some("linux-x86_64-cli"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(release.signature, "app");
        let client = release.client.expect("the client entry");
        assert_eq!(client.platform, "linux-x86_64-cli");
        assert_eq!(client.signature, "cli");
        assert_eq!(
            artifact_name(&client.platform),
            "cortisoff-cli-linux-x86_64"
        );

        // No client beside the app: the entry is not even read.
        let alone = pick_with_client(json, "1.0.0", Some("linux-x86_64"), None)
            .unwrap()
            .unwrap();
        assert_eq!(alone.client, None);
    }

    #[test]
    fn a_feed_without_a_client_key_still_updates_the_app() {
        let release = pick_with_client(
            TAURI_SHAPED,
            "1.0.0",
            Some("windows-x86_64"),
            Some("windows-x86_64-cli"),
        )
        .unwrap()
        .unwrap();
        assert!(release.url.ends_with("cortisoff-windows-x86_64.exe"));
        assert_eq!(release.client, None);
        let json = r#"{"version":"2.0.0","platforms":{
            "linux-x86_64":{"url":"https://x/a","signature":"s"},
            "linux-x86_64-cli":{"url":"http://x/c","signature":"s"}}}"#;
        let plain = pick_with_client(
            json,
            "1.0.0",
            Some("linux-x86_64"),
            Some("linux-x86_64-cli"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plain.client, None, "a plain http client is not fetched");
    }
}
