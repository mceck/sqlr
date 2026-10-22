//! Release helper for the `sqlr-drivers` registry.
//!
//! ```text
//! sign-index generate <dir>                        new key pair (secret.key, public.key)
//! sign-index artifact <secret.key> <file>          prints {"sha256": …, "signature": …}
//! sign-index index    <secret.key> <index.json>    writes <index.json>.minisig
//! ```
//!
//! The public key goes into sqlr (`REGISTRY_PUBLIC_KEY`); the secret key stays with the
//! maintainers. Its password comes from `SIGN_INDEX_PASSWORD`.

use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::Path;
use std::process::ExitCode;

use sha2::{Digest, Sha256};

/// The key's password, from `SIGN_INDEX_PASSWORD` (never a command-line argument).
fn password() -> Result<String, String> {
    std::env::var("SIGN_INDEX_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "set SIGN_INDEX_PASSWORD to the secret key's password".to_string())
}

fn secret(path: &str) -> Result<minisign::SecretKey, String> {
    minisign::SecretKey::from_file(path, Some(password()?)).map_err(|e| format!("{path}: {e}"))
}

fn sign(key: &minisign::SecretKey, data: &[u8]) -> Result<String, String> {
    minisign::sign(None, key, Cursor::new(data), Some("sqlr driver registry"), None)
        .map(|s| s.into_string())
        .map_err(|e| e.to_string())
}

fn run(args: &[String]) -> Result<(), String> {
    match args {
        [cmd, dir] if cmd == "generate" => {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            let secret_path = Path::new(dir).join("secret.key");
            let public_path = Path::new(dir).join("public.key");
            minisign::KeyPair::generate_encrypted_keypair(Some(password()?))
                .map_err(|e| e.to_string())
                .and_then(|pair| {
                    let pk = pair.pk.to_box().map_err(|e| e.to_string())?.into_string();
                    std::fs::write(
                        &secret_path,
                        pair.sk.to_box(Some("sqlr driver registry")).map_err(|e| e.to_string())?.into_string(),
                    ).map_err(|e| e.to_string())?;
                    std::fs::write(&public_path, pk).map_err(|e| e.to_string())
                })?;
            println!("wrote {} and {}", secret_path.display(), public_path.display());
            Ok(())
        }
        [cmd, key, file] if cmd == "artifact" => {
            let key = secret(key)?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(
                &mut BufReader::new(File::open(file).map_err(|e| format!("{file}: {e}"))?),
                &mut bytes,
            )
            .map_err(|e| e.to_string())?;
            let sha: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
            let signature = sign(&key, &bytes)?;
            println!("{{\"sha256\": \"{sha}\", \"signature\": {:?}}}", signature);
            Ok(())
        }
        [cmd, key, file] if cmd == "index" => {
            let key = secret(key)?;
            let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
            let out = format!("{file}.minisig");
            std::fs::write(&out, sign(&key, &bytes)?).map_err(|e| e.to_string())?;
            println!("wrote {out}");
            Ok(())
        }
        _ => Err("usage: sign-index generate <dir> | artifact <secret.key> <file> | index <secret.key> <index.json>".into()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("sign-index: {message}");
            ExitCode::FAILURE
        }
    }
}
