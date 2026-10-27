//! TLS: a custom CA only works for `hdbconnect` as DER files in a directory
//! (`ServerCerts::Directory`); the `Direct` and `Environment` variants hand PEM text to
//! rustls as if it were DER. So a PEM CA bundle is converted here.

use std::path::Path;

use sqlr_driver_sdk::{DriverError, ErrorKind};
use tempfile::TempDir;

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    sqlr_driver_sdk::base64_decode(&compact).ok()
}

/// Why a PEM CA bundle cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum PemError {
    #[error("an END without a BEGIN")]
    EndWithoutBegin,
    #[error("a certificate is not valid base64")]
    InvalidBase64,
    #[error("an empty certificate")]
    EmptyCertificate,
    #[error("a certificate has no END line")]
    NoEnd,
    #[error("no certificate found")]
    NoCertificate,
}

/// Every `CERTIFICATE` block of `pem`, DER-encoded.
fn certificates(pem: &str) -> Result<Vec<Vec<u8>>, PemError> {
    let mut found = Vec::new();
    let mut body: Option<String> = None;
    for line in pem.lines() {
        let line = line.trim();
        if line == "-----BEGIN CERTIFICATE-----" {
            body = Some(String::new());
        } else if line == "-----END CERTIFICATE-----" {
            let text = body.take().ok_or(PemError::EndWithoutBegin)?;
            let der = base64_decode(&text).ok_or(PemError::InvalidBase64)?;
            if der.is_empty() {
                return Err(PemError::EmptyCertificate);
            }
            found.push(der);
        } else if let Some(text) = &mut body {
            text.push_str(line);
        }
    }
    if body.is_some() {
        return Err(PemError::NoEnd);
    }
    if found.is_empty() {
        return Err(PemError::NoCertificate);
    }
    Ok(found)
}

/// Writes the certificates of the PEM file `path` as `ca0.cer`, `ca1.cer`, … in a fresh
/// temporary directory, which `ServerCerts::Directory` can read. The directory is removed
/// when the returned value drops, so keep it as long as connections may be opened.
pub fn pem_to_der_dir(path: &Path) -> Result<TempDir, DriverError> {
    let fail = |m: String| DriverError::new(ErrorKind::Connection, m);
    let pem = std::fs::read_to_string(path)
        .map_err(|e| fail(format!("cannot read the CA file {}: {e}", path.display())))?;
    let certs = certificates(&pem).map_err(|e| fail(format!("{}: {e}", path.display())))?;
    let dir = tempfile::Builder::new()
        .prefix("sqlr-hana-ca")
        .tempdir()
        .map_err(|e| fail(e.to_string()))?;
    for (i, der) in certs.iter().enumerate() {
        std::fs::write(dir.path().join(format!("ca{i}.cer")), der)
            .map_err(|e| fail(e.to_string()))?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    // DER of a tiny SEQUENCE { INTEGER 5 }: 30 03 02 01 05.
    const ONE: &str = "-----BEGIN CERTIFICATE-----\nMAMCAQU=\n-----END CERTIFICATE-----\n";
    // 30 03 02 01 06.
    const TWO: &str = "-----BEGIN CERTIFICATE-----\nMAMC\nAQY=\n-----END CERTIFICATE-----\n";

    #[test]
    fn pem_ca_converted_to_der_directory() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), format!("# bundle\n{ONE}\n{TWO}")).unwrap();
        let dir = pem_to_der_dir(file.path()).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("ca0.cer")).unwrap(),
            [0x30, 0x03, 0x02, 0x01, 0x05]
        );
        assert_eq!(
            std::fs::read(dir.path().join("ca1.cer")).unwrap(),
            [0x30, 0x03, 0x02, 0x01, 0x06]
        );
        let kept = dir.path().to_path_buf();
        drop(dir);
        assert!(!kept.exists(), "the directory goes away with its owner");
    }

    #[test]
    fn pem_errors_keep_their_messages() {
        assert_eq!(
            certificates("-----END CERTIFICATE-----")
                .unwrap_err()
                .to_string(),
            "an END without a BEGIN"
        );
        assert_eq!(
            certificates("nothing here").unwrap_err().to_string(),
            "no certificate found"
        );
        assert_eq!(
            certificates("-----BEGIN CERTIFICATE-----\nMAMC")
                .unwrap_err()
                .to_string(),
            "a certificate has no END line"
        );
    }

    #[test]
    fn a_certificate_with_an_impossible_base64_length_is_refused() {
        // Five base64 characters leave a 6-bit group with no byte to fill: no decoder may
        // turn that into DER bytes silently.
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            "-----BEGIN CERTIFICATE-----\nAAAAA\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        assert!(pem_to_der_dir(file.path()).is_err());
    }

    #[test]
    fn broken_ca_files_are_refused() {
        let file = tempfile::NamedTempFile::new().unwrap();
        for bad in [
            "",
            "not a certificate",
            "-----BEGIN CERTIFICATE-----\nMAMCAQU=\n",
            "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n",
            "-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n",
        ] {
            std::fs::write(file.path(), bad).unwrap();
            assert!(pem_to_der_dir(file.path()).is_err(), "{bad:?}");
        }
        assert!(pem_to_der_dir(Path::new("/no/such/ca.pem")).is_err());
    }
}
