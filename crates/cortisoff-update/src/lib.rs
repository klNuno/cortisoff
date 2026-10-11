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

pub use feed::{artifact_name, platform_key, Release};
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
    let json = agent(Duration::from_secs(20))
        .get(feed)
        .call()
        .map_err(|e| format!("cannot reach the update feed: {e}"))?
        .into_body()
        .into_with_config()
        .limit(FEED_LIMIT)
        .read_to_string()
        .map_err(|e| format!("cannot read the update feed: {e}"))?;
    feed::pick(&json, current, platform_key())
}

/// Download `release` into `dir` and verify it. Returns the path of the
/// verified executable, ready for [`apply`]. A file that fails the check is
/// deleted before this returns.
pub fn download(release: &Release, dir: &Path) -> Result<PathBuf, String> {
    download_with(PUBLIC_KEY, release, dir)
}

fn download_with(public_key: &str, release: &Release, dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let artifact = artifact_name(&release.platform);
    let staged = dir.join(format!(
        "cortisoff-{}{}",
        release.version,
        std::env::consts::EXE_SUFFIX
    ));
    let partial = dir.join(format!("{artifact}.part"));
    // Checked before a byte is fetched, so a malformed feed costs no download.
    verify::decode_signature(&release.signature)?;

    let result = fetch_verified(public_key, release, &artifact, &partial);
    match result {
        Ok(_) => {
            let _ = fs::remove_file(&staged);
            fs::rename(&partial, &staged).map_err(|e| {
                let _ = fs::remove_file(&partial);
                format!("cannot stage the update: {e}")
            })?;
            Ok(staged)
        }
        Err(e) => {
            let _ = fs::remove_file(&partial);
            Err(format!(
                "{e}; the download of {artifact} {} was deleted",
                release.version
            ))
        }
    }
}

fn fetch_verified(
    public_key: &str,
    release: &Release,
    artifact: &str,
    partial: &Path,
) -> Result<u64, String> {
    let response = agent(Duration::from_secs(600))
        .get(&release.url)
        .call()
        .map_err(|e| format!("cannot download {}: {e}", release.url))?;
    let reader = response
        .into_body()
        .into_with_config()
        .limit(DOWNLOAD_LIMIT)
        .reader();
    let mut file = fs::File::create(partial)
        .map_err(|e| format!("cannot write {}: {e}", partial.display()))?;
    let written = verify::verify_into(
        public_key,
        &release.signature,
        artifact,
        &release.version,
        reader,
        &mut file,
    )?;
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
    fn the_feed_url_is_the_latest_release_asset() {
        assert!(
            FEED_URL.starts_with("https://github.com/klNuno/cortisoff/releases/latest/download/")
        );
        assert!(FEED_URL.ends_with("/latest.json"));
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
        }
    }

    #[test]
    fn a_verified_download_is_staged() {
        let keys = verify::tests::Keys::new();
        let scratch = replace::tests::Scratch::new("download-ok");
        let body = b"ELF new build".repeat(5000);
        let signature = keys.sign_for_feed(&body, "cortisoff-linux-x86_64", "1.2.0");
        let staged = download_with(
            &keys.public,
            &release(serve_once(body.clone()), signature),
            &scratch.0,
        )
        .unwrap();
        assert_eq!(fs::read(&staged).unwrap(), body);
        assert_eq!(
            fs::read_dir(&scratch.0).unwrap().count(),
            1,
            "no partial file left"
        );
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
