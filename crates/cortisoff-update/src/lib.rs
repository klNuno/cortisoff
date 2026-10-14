//! Installs cortisoff for the current user and keeps it up to date.
//!
//! cortisoff ships as one downloaded executable. On first run the app calls
//! [`install::install`], which copies it (and `cortisoff-cli` when it sits
//! beside it) to a per-user folder and registers it with the desktop, never
//! asking for elevation. Afterwards [`check`] reads a `latest.json` from the
//! GitHub releases, [`download`] fetches and verifies the new executable,
//! [`apply`] puts it in place of the running one and [`relaunch`] starts it.
//! The next start calls [`cleanup_previous`].
//!
//! A release is trusted only through its minisign signature, made by the key
//! whose public half is [`PUBLIC_KEY`], with a trusted comment naming the
//! artifact and the version (see [`trusted_comment`]). The release workflow
//! signs that way; nothing else is accepted.

mod feed;
pub mod install;
mod replace;
mod verify;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use feed::{artifact_name, platform_key, ClientBuild, Release};
pub use replace::{apply, cleanup_previous, relaunch};
pub use verify::{trusted_comment, verify, PUBLIC_KEY};

/// Where releases announce themselves.
pub const FEED_URL: &str =
    "https://github.com/klNuno/cortisoff/releases/latest/download/latest.json";

/// A feed bigger than this is not a feed.
const FEED_LIMIT: u64 = 1024 * 1024;
/// A download bigger than this is refused before it fills a disk.
const DOWNLOAD_LIMIT: u64 = 512 * 1024 * 1024;

fn agent(body_timeout: Duration) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .user_agent(format!(
            "cortisoff/{} (+https://github.com/klNuno/cortisoff)",
            env!("CARGO_PKG_VERSION")
        ))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(20)))
        .timeout_recv_body(Some(body_timeout))
        .build()
        .into()
}

/// Ask `feed` whether a version newer than `current` exists, and answer its
/// build for this platform. `Ok(None)` means up to date.
pub fn check(feed: &str, current: &str) -> Result<Option<Release>, String> {
    let json = read_feed(feed)?;
    let exe = replace::current_exe().ok();
    let in_bundle = exe.as_deref().is_some_and(install::runs_in_bundle);
    let has_client = exe.as_deref().and_then(install::client_to_update).is_some();
    let key = feed::update_key_for(platform_key(), in_bundle);
    let client = feed::client_key_for(platform_key(), has_client);
    feed::pick_with_client(&json, current, key.as_deref(), client.as_deref())
}

/// The feed published with release `version`, beside its downloads.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn release_feed_url(version: &str) -> String {
    format!("https://github.com/klNuno/cortisoff/releases/download/v{version}/latest.json")
}

/// The signed build of release `version` itself under the feed key `key`,
/// read from the feed that release published, so a copy of that version
/// can be installed from its download rather than from files on disk.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn release_of(version: &str, key: &str) -> Result<Release, String> {
    feed::pick_exact(&read_feed(&release_feed_url(version))?, version, key)
}

fn read_feed(url: &str) -> Result<String, String> {
    agent(Duration::from_secs(20))
        .get(url)
        .call()
        .map_err(|e| format!("cannot reach the update feed: {e}"))?
        .into_body()
        .into_with_config()
        .limit(FEED_LIMIT)
        .read_to_string()
        .map_err(|e| format!("cannot read the update feed: {e}"))
}

/// What a download is staged as: the bare executable, or on macOS the
/// zipped bundle, which [`apply`] recognises by this name.
fn staged_name(version: &str, bundle: bool) -> String {
    if bundle {
        format!("cortisoff-{version}.zip")
    } else {
        format!("cortisoff-{version}{}", std::env::consts::EXE_SUFFIX)
    }
}

/// What a release's client is staged as, beside the app it came with.
fn staged_client_name(version: &str) -> String {
    format!("cortisoff-cli-{version}{}", std::env::consts::EXE_SUFFIX)
}

/// The client staged with the bare executable `staged`, whether or not it
/// was downloaded: `cortisoff-cli-<version>` in the same folder.
pub(crate) fn staged_client_for(staged: &Path) -> Option<PathBuf> {
    let name = staged.file_name()?.to_str()?;
    let version = name
        .strip_prefix("cortisoff-")?
        .strip_suffix(std::env::consts::EXE_SUFFIX)?;
    if version.is_empty() || version.ends_with(".zip") || version.starts_with("cli-") {
        return None;
    }
    Some(staged.with_file_name(staged_client_name(version)))
}

/// The version a staged bundle was downloaded for, from its name, when it
/// is one.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn staged_bundle_version(staged: &Path) -> Option<&str> {
    let name = staged.file_name()?.to_str()?;
    let version = name.strip_prefix("cortisoff-")?.strip_suffix(".zip")?;
    (!version.is_empty()).then_some(version)
}

/// Download `release` into `dir` and verify it. Returns the path of the
/// verified executable, ready for [`apply`]. A file that fails the check is
/// deleted before this returns. The release's client, when it has one,
/// is staged beside it as in [`download_pair`].
pub fn download(release: &Release, dir: &Path) -> Result<PathBuf, String> {
    download_pair(release, dir).map(|(staged, _)| staged)
}

/// [`download`], also answering why the release's client was left out when
/// it was: its download or its signature failed. The app is staged either
/// way, so a broken client never holds an update back; [`apply`] then
/// replaces the app alone and the next release tries the client again.
pub fn download_pair(release: &Release, dir: &Path) -> Result<(PathBuf, Option<String>), String> {
    download_with(PUBLIC_KEY, release, dir)
}

fn download_with(
    public_key: &str,
    release: &Release,
    dir: &Path,
) -> Result<(PathBuf, Option<String>), String> {
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let staged = dir.join(staged_name(
        &release.version,
        feed::is_bundle_key(&release.platform),
    ));
    let staged = stage(
        public_key,
        &artifact_name(&release.platform),
        &release.url,
        &release.signature,
        &release.version,
        &staged,
    )?;
    let Some(client) = &release.client else {
        return Ok((staged, None));
    };
    // Only ever beside a bare executable, which is what apply looks for.
    let Some(client_staged) = staged_client_for(&staged) else {
        return Ok((staged, None));
    };
    let _ = fs::remove_file(&client_staged);
    let problem = stage(
        public_key,
        &artifact_name(&client.platform),
        &client.url,
        &client.signature,
        &release.version,
        &client_staged,
    )
    .err()
    .map(|e| format!("cortisoff-cli stays at its version, the app updates alone: {e}"));
    Ok((staged, problem))
}

/// Fetch `url` and keep it at `staged` only once its signature names
/// `artifact` and `version`. Nothing is left behind on failure.
fn stage(
    public_key: &str,
    artifact: &str,
    url: &str,
    signature: &str,
    version: &str,
    staged: &Path,
) -> Result<PathBuf, String> {
    let dir = staged
        .parent()
        .ok_or_else(|| format!("{} has no folder", staged.display()))?;
    let partial = dir.join(format!("{artifact}.part"));
    // Checked before a byte is fetched, so a malformed feed costs no download.
    verify::decode_signature(signature)?;

    match fetch_verified(public_key, url, signature, artifact, version, &partial) {
        Ok(_) => {
            let _ = fs::remove_file(staged);
            fs::rename(&partial, staged).map_err(|e| {
                let _ = fs::remove_file(&partial);
                format!("cannot stage the update: {e}")
            })?;
            Ok(staged.to_path_buf())
        }
        Err(e) => {
            let _ = fs::remove_file(&partial);
            Err(format!(
                "{e}; the download of {artifact} {version} was deleted"
            ))
        }
    }
}

fn fetch_verified(
    public_key: &str,
    url: &str,
    signature: &str,
    artifact: &str,
    version: &str,
    partial: &Path,
) -> Result<u64, String> {
    let response = agent(Duration::from_secs(600))
        .get(url)
        .call()
        .map_err(|e| format!("cannot download {url}: {e}"))?;
    let reader = response
        .into_body()
        .into_with_config()
        .limit(DOWNLOAD_LIMIT)
        .reader();
    let mut file = fs::File::create(partial)
        .map_err(|e| format!("cannot write {}: {e}", partial.display()))?;
    let written = verify::verify_into(public_key, signature, artifact, version, reader, &mut file)?;
    file.sync_all()
        .map_err(|e| format!("cannot write {}: {e}", partial.display()))?;
    if written == 0 {
        return Err("the download is empty".to_owned());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_staged_bundle_is_named_for_its_version() {
        let zip = staged_name("1.2.0", true);
        assert_eq!(zip, "cortisoff-1.2.0.zip");
        assert_eq!(
            staged_bundle_version(&Path::new("/tmp").join(&zip)),
            Some("1.2.0")
        );
        let bare = staged_name("1.2.0", false);
        assert_eq!(
            bare,
            format!("cortisoff-1.2.0{}", std::env::consts::EXE_SUFFIX)
        );
        assert_eq!(staged_bundle_version(Path::new(&bare)), None);
        assert_eq!(staged_bundle_version(Path::new("cortisoff-.zip")), None);
        assert_eq!(staged_bundle_version(Path::new("other-1.2.0.zip")), None);
    }

    #[test]
    fn the_feed_url_is_the_latest_release_asset() {
        assert!(
            FEED_URL.starts_with("https://github.com/klNuno/cortisoff/releases/latest/download/")
        );
        assert!(FEED_URL.ends_with("/latest.json"));
        // The release workflow uploads the same file to every release.
        assert_eq!(
            release_feed_url("0.3.0"),
            FEED_URL.replace("/latest/download/", "/download/v0.3.0/")
        );
    }

    #[test]
    fn a_malformed_signature_costs_no_download() {
        let scratch = replace::tests::Scratch::new("malformed");
        let release = Release {
            version: "9.9.9".into(),
            notes: String::new(),
            pub_date: None,
            platform: "linux-x86_64".into(),
            // Unroutable: reaching it would hang, not fail fast.
            url: "https://192.0.2.1/cortisoff".into(),
            signature: "not a signature".into(),
            client: None,
        };
        assert!(download(&release, &scratch.0).is_err());
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
    }

    /// Serves `body` once over plain HTTP on loopback and answers its URL.
    fn serve_once(body: Vec<u8>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let head = format!(
                "HTTP/1.1 200 OK
Content-Length: {}
Connection: close

",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        });
        format!("http://127.0.0.1:{port}/cortisoff-linux-x86_64")
    }

    fn release(url: String, signature: String) -> Release {
        Release {
            version: "1.2.0".into(),
            notes: String::new(),
            pub_date: None,
            platform: "linux-x86_64".into(),
            url,
            signature,
            client: None,
        }
    }

    fn client(url: String, signature: String) -> Option<ClientBuild> {
        Some(ClientBuild {
            platform: "linux-x86_64-cli".into(),
            url,
            signature,
        })
    }

    #[test]
    fn a_client_is_staged_beside_its_app() {
        let app = staged_name("1.2.0", false);
        let client = staged_client_for(&Path::new("tmp").join(&app)).unwrap();
        assert_eq!(
            client,
            Path::new("tmp").join(format!(
                "cortisoff-cli-1.2.0{}",
                std::env::consts::EXE_SUFFIX
            ))
        );
        assert_eq!(staged_client_for(Path::new("cortisoff-1.2.0.zip")), None);
        assert_eq!(staged_client_for(&client), None);
    }

    #[test]
    fn an_app_and_its_client_are_staged_together() {
        let keys = verify::tests::Keys::new();
        let scratch = replace::tests::Scratch::new("download-pair");
        let app_body = b"ELF new app".repeat(4000);
        let cli_body = b"ELF new client".repeat(3000);
        let mut pair = release(
            serve_once(app_body.clone()),
            keys.sign_for_feed(&app_body, "cortisoff-linux-x86_64", "1.2.0"),
        );
        pair.client = client(
            serve_once(cli_body.clone()),
            keys.sign_for_feed(&cli_body, "cortisoff-cli-linux-x86_64", "1.2.0"),
        );
        let (staged, problem) = download_with(&keys.public, &pair, &scratch.0).unwrap();
        assert_eq!(problem, None);
        assert_eq!(fs::read(&staged).unwrap(), app_body);
        let client = staged_client_for(&staged).unwrap();
        assert_eq!(fs::read(&client).unwrap(), cli_body);
        assert_eq!(
            fs::read_dir(&scratch.0).unwrap().count(),
            2,
            "both staged, no partial file left"
        );
    }

    #[test]
    fn a_client_that_fails_its_check_leaves_the_app_update_going() {
        let keys = verify::tests::Keys::new();
        let scratch = replace::tests::Scratch::new("download-pair-bad");
        let app_body = b"ELF new app".repeat(4000);
        let cli_body = b"ELF new client".repeat(3000);
        let mut pair = release(
            serve_once(app_body.clone()),
            keys.sign_for_feed(&app_body, "cortisoff-linux-x86_64", "1.2.0"),
        );
        // A client signed as the app: the name binding refuses it.
        pair.client = client(
            serve_once(cli_body.clone()),
            keys.sign_for_feed(&cli_body, "cortisoff-linux-x86_64", "1.2.0"),
        );
        let (staged, problem) = download_with(&keys.public, &pair, &scratch.0).unwrap();
        let problem = problem.expect("the client failure is reported");
        assert!(problem.contains("cortisoff-cli"), "{problem}");
        assert_eq!(fs::read(&staged).unwrap(), app_body);
        assert!(!staged_client_for(&staged).unwrap().exists());
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);

        // A client that cannot be reached at all: the same.
        let scratch = replace::tests::Scratch::new("download-pair-gone");
        let mut pair = release(
            serve_once(app_body.clone()),
            keys.sign_for_feed(&app_body, "cortisoff-linux-x86_64", "1.2.0"),
        );
        pair.client = client(
            "http://127.0.0.1:9/cortisoff-cli-linux-x86_64".into(),
            keys.sign_for_feed(&cli_body, "cortisoff-cli-linux-x86_64", "1.2.0"),
        );
        let (staged, problem) = download_with(&keys.public, &pair, &scratch.0).unwrap();
        assert!(problem.is_some());
        assert_eq!(fs::read(&staged).unwrap(), app_body);
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
    }

    #[test]
    fn a_verified_download_is_staged() {
        let keys = verify::tests::Keys::new();
        let scratch = replace::tests::Scratch::new("download-ok");
        let body = b"ELF new build".repeat(5000);
        let signature = keys.sign_for_feed(&body, "cortisoff-linux-x86_64", "1.2.0");
        let (staged, problem) = download_with(
            &keys.public,
            &release(serve_once(body.clone()), signature),
            &scratch.0,
        )
        .unwrap();
        assert_eq!(problem, None, "no client asked, none fetched");
        assert_eq!(fs::read(&staged).unwrap(), body);
        assert_eq!(
            fs::read_dir(&scratch.0).unwrap().count(),
            1,
            "no partial file left"
        );
    }

    #[test]
    fn a_bundle_download_is_checked_like_any_artifact_and_staged_as_a_zip() {
        let keys = verify::tests::Keys::new();
        let scratch = replace::tests::Scratch::new("download-app");
        let body = b"PK zipped bundle".repeat(3000);
        let signature = keys.sign_for_feed(&body, "cortisoff-darwin-x86_64.zip", "1.2.0");
        let mut app = release(serve_once(body.clone()), signature);
        app.platform = "darwin-x86_64-app".into();
        let (staged, _) = download_with(&keys.public, &app, &scratch.0).unwrap();
        assert_eq!(staged.file_name().unwrap(), "cortisoff-1.2.0.zip");
        assert_eq!(staged_bundle_version(&staged), Some("1.2.0"));
        assert_eq!(fs::read(&staged).unwrap(), body);

        // Signed under the bare executable's name: refused.
        let keys = verify::tests::Keys::new();
        let scratch = replace::tests::Scratch::new("download-app-bad");
        let signature = keys.sign_for_feed(&body, "cortisoff-darwin-x86_64", "1.2.0");
        let mut app = release(serve_once(body), signature);
        app.platform = "darwin-x86_64-app".into();
        assert!(download_with(&keys.public, &app, &scratch.0).is_err());
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
    }

    #[test]
    fn a_download_that_fails_the_signature_is_deleted() {
        let keys = verify::tests::Keys::new();
        let scratch = replace::tests::Scratch::new("download-bad");
        let signature = keys.sign_for_feed(
            b"the build that was signed",
            "cortisoff-linux-x86_64",
            "1.2.0",
        );
        let served = serve_once(b"something else entirely".to_vec());
        let error =
            download_with(&keys.public, &release(served, signature), &scratch.0).unwrap_err();
        assert!(error.contains("does not match"), "{error}");
        assert!(error.contains("deleted"), "{error}");
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
    }
}
