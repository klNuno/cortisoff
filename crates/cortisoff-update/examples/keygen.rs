//! Generate the release signing key. Writes the public key over
//! `crates/cortisoff-update/update.pub` and the encrypted secret key and its
//! password as two files in a folder you name, then prints the public key
//! only. Store the two files where they belong and delete them.
//!
//! cargo run --release -p cortisoff-update --example keygen -- <empty folder>

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;

fn main() {
    if let Err(e) = run() {
        eprintln!("keygen: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let out = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: keygen <empty folder for the secret key and password>")?;
    fs::create_dir_all(&out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let secret_path = out.join("secret.key");
    let password_path = out.join("password");
    for path in [&secret_path, &password_path] {
        if path.exists() {
            return Err(format!(
                "{} already exists, refusing to overwrite it",
                path.display()
            ));
        }
    }

    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|e| format!("no randomness: {e}"))?;
    let password = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);

    let pair = minisign::KeyPair::generate_encrypted_keypair(Some(password.clone()))
        .map_err(|e| format!("cannot generate the key: {e}"))?;
    let secret = pair
        .sk
        .to_box(Some("cortisoff release signing key"))
        .map_err(|e| format!("cannot encode the secret key: {e}"))?
        .into_string();
    let public = pair
        .pk
        .to_box()
        .map_err(|e| format!("cannot encode the public key: {e}"))?
        .into_string();

    write_new(&secret_path, &secret)?;
    write_new(&password_path, &password)?;
    let pub_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("update.pub");
    fs::write(&pub_path, &public)
        .map_err(|e| format!("cannot write {}: {e}", pub_path.display()))?;

    println!("public key written to {}:", pub_path.display());
    print!("{public}");
    println!("secret key and password written under {}", out.display());
    Ok(())
}

fn write_new(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    file.write_all(text.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}
