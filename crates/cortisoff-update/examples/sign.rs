//! Sign release executables the way the updater checks them. Reads the
//! encrypted secret key from `MINISIGN_SECRET_KEY` and its password from
//! `MINISIGN_PASSWORD`, writes `<file>.sig` next to each file, then verifies
//! every signature against the public key compiled into the library, so a
//! secret that does not match `update.pub` fails the release here rather
//! than on a user's machine.
//!
//! cargo run --release -p cortisoff-update --example sign -- <version> <file>...

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    if let Err(e) = run() {
        eprintln!("sign: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let version = args
        .next()
        .and_then(|v| v.into_string().ok())
        .ok_or("usage: sign <version> <file>...")?;
    let version = version.strip_prefix('v').unwrap_or(&version).to_owned();
    let files: Vec<PathBuf> = args.map(PathBuf::from).collect();
    if files.is_empty() {
        return Err("usage: sign <version> <file>...".to_owned());
    }

    let secret =
        std::env::var("MINISIGN_SECRET_KEY").map_err(|_| "MINISIGN_SECRET_KEY is not set")?;
    let password =
        std::env::var("MINISIGN_PASSWORD").map_err(|_| "MINISIGN_PASSWORD is not set")?;
    let secret = minisign::SecretKeyBox::from_string(secret.trim())
        .and_then(|b| b.into_secret_key(Some(password)))
        .map_err(|_| "the secret key cannot be read or the password is wrong")?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    for file in &files {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{} has no usable file name", file.display()))?
            .to_owned();
        let comment = cortisoff_update::trusted_comment(&name, &version, timestamp);
        let data =
            fs::File::open(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let signature = minisign::sign(None, &secret, data, Some(&comment), None)
            .map_err(|e| format!("cannot sign {name}: {e}"))?
            .into_string();
        let check =
            fs::File::open(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        cortisoff_update::verify(check, &signature, &name, &version).map_err(|e| {
            format!("{name}: the signature does not verify against update.pub ({e})")
        })?;
        let mut sig_path = file.clone().into_os_string();
        sig_path.push(".sig");
        fs::write(&sig_path, &signature)
            .map_err(|e| format!("cannot write the signature of {name}: {e}"))?;
        println!("signed {name} for {version}");
    }
    Ok(())
}
