//! Signature checks. A release is trusted because its executable carries a
//! minisign signature by the key whose public half is compiled in here, and
//! because the signed trusted comment names the file and version the feed
//! promised, so an old signed build cannot be replayed as a new one.

use std::io::{Read, Write};

use base64::Engine;
use minisign_verify::{PublicKey, Signature};

/// The release public key, the text of a minisign `.pub` file.
pub const PUBLIC_KEY: &str = include_str!("../update.pub");

/// The trusted comment a release signature carries. The release workflow
/// signs with exactly this, and [`verify`] reads the `file` and `version`
/// fields back out of it.
pub fn trusted_comment(artifact: &str, version: &str, timestamp: u64) -> String {
    format!("timestamp:{timestamp}\tfile:{artifact}\tversion:{version}")
}

/// Check `data` against `signature` (a feed's signature field) with the
/// compiled-in [`PUBLIC_KEY`]. `artifact` and `version` are what the signed
/// comment has to name.
pub fn verify(
    data: impl Read,
    signature: &str,
    artifact: &str,
    version: &str,
) -> Result<(), String> {
    verify_into(
        PUBLIC_KEY,
        signature,
        artifact,
        version,
        data,
        std::io::sink(),
    )
    .map(|_| ())
}

/// Accepts the signature as Tauri's feed carries it, base64 of the `.sig`
/// file, or as the `.sig` text itself.
pub(crate) fn decode_signature(field: &str) -> Result<Signature, String> {
    let field = field.trim();
    let text = if field.starts_with("untrusted comment:") {
        field.to_owned()
    } else {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(field)
            .map_err(|e| format!("the signature is not base64: {e}"))?;
        String::from_utf8(bytes).map_err(|_| "the signature is not text".to_owned())?
    };
    Signature::decode(&text).map_err(|e| format!("the signature is malformed: {e}"))
}

/// Stream `data` into `sink` while hashing it, then verify. Returns the byte
/// count. The caller owns whatever `sink` wrote and must discard it on error.
pub(crate) fn verify_into(
    public_key: &str,
    signature: &str,
    artifact: &str,
    version: &str,
    mut data: impl Read,
    mut sink: impl Write,
) -> Result<u64, String> {
    let key = PublicKey::decode(public_key)
        .map_err(|e| format!("the built-in release key is unreadable: {e}"))?;
    let signature = decode_signature(signature)?;
    let mut verifier = key.verify_stream(&signature).map_err(|e| match e {
        minisign_verify::Error::UnexpectedKeyId => {
            "the download is signed by another key than cortisoff's".to_owned()
        }
        other => format!("the signature cannot be checked: {other}"),
    })?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = match data.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("the download broke off: {e}")),
        };
        verifier.update(&buffer[..read]);
        sink.write_all(&buffer[..read])
            .map_err(|e| format!("the download could not be written: {e}"))?;
        total += read as u64;
    }
    sink.flush()
        .map_err(|e| format!("the download could not be written: {e}"))?;
    verifier
        .finalize()
        .map_err(|_| "the signature does not match the downloaded file".to_owned())?;
    // Only trustworthy now: finalize checked the global signature over it.
    check_comment(signature.trusted_comment(), artifact, version)?;
    Ok(total)
}

fn check_comment(comment: &str, artifact: &str, version: &str) -> Result<(), String> {
    let field = |name: &str| {
        comment
            .split('\t')
            .find_map(|part| part.strip_prefix(name)?.strip_prefix(':'))
    };
    match field("file") {
        Some(file) if file == artifact => {}
        Some(file) => return Err(format!("the signature is for {file}, not for {artifact}")),
        None => return Err("the signature names no file".to_owned()),
    }
    let wanted = version.strip_prefix('v').unwrap_or(version);
    match field("version") {
        Some(signed) if signed.strip_prefix('v').unwrap_or(signed) == wanted => Ok(()),
        Some(signed) => Err(format!(
            "the signature is for version {signed}, the feed announced {wanted}"
        )),
        None => Err("the signature names no version".to_owned()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct Keys {
        pub public: String,
        secret: minisign::SecretKey,
        pk: minisign::PublicKey,
    }

    impl Keys {
        pub fn new() -> Self {
            let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
            Keys {
                public: pair.pk.to_box().unwrap().into_string(),
                secret: pair.sk,
                pk: pair.pk,
            }
        }

        pub fn sign_comment(&self, data: &[u8], comment: &str) -> String {
            minisign::sign(Some(&self.pk), &self.secret, data, Some(comment), None)
                .unwrap()
                .into_string()
        }

        /// A signature as the release job publishes it in the feed.
        pub fn sign_for_feed(&self, data: &[u8], artifact: &str, version: &str) -> String {
            let text = self.sign_comment(data, &trusted_comment(artifact, version, 1_760_000_000));
            base64::engine::general_purpose::STANDARD.encode(text)
        }
    }

    const FILE: &str = "cortisoff-windows-x86_64.exe";

    fn check(keys: &Keys, data: &[u8], signature: &str, version: &str) -> Result<u64, String> {
        let mut out = Vec::new();
        let result = verify_into(&keys.public, signature, FILE, version, data, &mut out);
        if result.is_ok() {
            assert_eq!(out, data, "what was verified is what was written");
        }
        result
    }

    #[test]
    fn a_good_signature_verifies_in_either_spelling() {
        let keys = Keys::new();
        let data = b"MZ the new build".repeat(10_000);
        let feed = keys.sign_for_feed(&data, FILE, "1.2.0");
        assert_eq!(check(&keys, &data, &feed, "1.2.0"), Ok(data.len() as u64));
        let raw = keys.sign_comment(&data, &trusted_comment(FILE, "1.2.0", 1));
        assert_eq!(check(&keys, &data, &raw, "v1.2.0"), Ok(data.len() as u64));
    }

    #[test]
    fn a_tampered_file_does_not_verify() {
        let keys = Keys::new();
        let data = b"MZ the new build".to_vec();
        let signature = keys.sign_for_feed(&data, FILE, "1.2.0");
        let mut tampered = data.clone();
        tampered[3] ^= 1;
        let error = check(&keys, &tampered, &signature, "1.2.0").unwrap_err();
        assert!(error.contains("does not match"), "{error}");
    }

    #[test]
    fn another_key_does_not_verify() {
        let keys = Keys::new();
        let stranger = Keys::new();
        let data = b"MZ".to_vec();
        let signature = stranger.sign_for_feed(&data, FILE, "1.2.0");
        let error = check(&keys, &data, &signature, "1.2.0").unwrap_err();
        assert!(error.contains("another key"), "{error}");
    }

    #[test]
    fn an_old_build_replayed_as_a_new_version_is_refused() {
        let keys = Keys::new();
        let data = b"MZ the old build".to_vec();
        let signature = keys.sign_for_feed(&data, FILE, "1.0.0");
        let error = check(&keys, &data, &signature, "1.2.0").unwrap_err();
        assert!(error.contains("version 1.0.0"), "{error}");
    }

    #[test]
    fn another_platforms_build_is_refused() {
        let keys = Keys::new();
        let data = b"\x7fELF".to_vec();
        let signature = keys.sign_for_feed(&data, "cortisoff-linux-x86_64", "1.2.0");
        let error = check(&keys, &data, &signature, "1.2.0").unwrap_err();
        assert!(error.contains("cortisoff-linux-x86_64"), "{error}");
    }

    #[test]
    fn a_signature_without_the_release_fields_is_refused() {
        let keys = Keys::new();
        let data = b"MZ".to_vec();
        let bare = keys.sign_comment(&data, "timestamp:1");
        assert!(check(&keys, &data, &bare, "1.2.0").is_err());
    }

    #[test]
    fn garbage_signatures_are_errors() {
        let keys = Keys::new();
        assert!(check(&keys, b"MZ", "", "1.0.0").is_err());
        assert!(check(&keys, b"MZ", "not base64 at all!", "1.0.0").is_err());
        assert!(check(&keys, b"MZ", "dW50cnVzdGVkIGNvbW1lbnQ=", "1.0.0").is_err());
    }

    #[test]
    fn the_shipped_public_key_parses() {
        PublicKey::decode(PUBLIC_KEY).expect("update.pub is a minisign public key");
    }
}
