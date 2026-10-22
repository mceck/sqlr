//! sha256 and minisign checks.

use sha2::{Digest, Sha256};

use crate::RegistryError;

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Checks `signature` (the text of a `.minisig` file) over `data` against `public_key` (the
/// base64 key line, or the whole `.pub` file).
pub fn verify_signature(
    public_key: &str,
    data: &[u8],
    signature: &str,
    what: &str,
) -> Result<(), RegistryError> {
    let fail = |reason: String| RegistryError::Signature {
        what: what.to_string(),
        reason,
    };
    let key = minisign_verify::PublicKey::decode(public_key)
        .or_else(|_| minisign_verify::PublicKey::from_base64(public_key.trim()))
        .map_err(|e| fail(format!("the registry key is unreadable: {e}")))?;
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|e| fail(format!("the signature is unreadable: {e}")))?;
    key.verify(data, &signature, false)
        .map_err(|e| fail(e.to_string()))
}
