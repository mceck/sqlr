//! Installing a driver from a file the user picked (spec §6.4): nothing is trusted, nothing
//! runs, until the user has seen what the file is and confirmed.

use std::path::PathBuf;

use crate::RegistryError;
use crate::index::valid_id;
use crate::install::{InstalledDriver, Registry, extract_archive, read_driver_toml};
use crate::verify::sha256_hex;

/// What a driver archive is, read without running anything in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualInspection {
    pub file: PathBuf,
    pub sha256: String,
    pub size: u64,
    pub id: String,
    pub version: String,
}

impl Registry {
    /// Reads the archive's `driver.toml` and checksum. Installs nothing and starts nothing:
    /// the caller shows [`ManualInspection`] with a warning and calls
    /// [`Registry::confirm_manual`] only after an explicit "Install anyway".
    pub async fn install_manual(
        &self,
        file: &std::path::Path,
    ) -> Result<ManualInspection, RegistryError> {
        let bytes = tokio::fs::read(file).await?;
        let meta = read_driver_toml(file)?;
        if !valid_id(&meta.id) || semver::Version::parse(&meta.version).is_err() {
            return Err(RegistryError::UnsafeArchive(
                "driver.toml names an invalid id or version".into(),
            ));
        }
        Ok(ManualInspection {
            file: file.to_path_buf(),
            sha256: sha256_hex(&bytes),
            size: bytes.len() as u64,
            id: meta.id,
            version: meta.version,
        })
    }

    /// Installs what [`Registry::install_manual`] inspected, provided the file is still the
    /// one the user saw (same checksum).
    pub async fn confirm_manual(
        &self,
        inspection: &ManualInspection,
    ) -> Result<InstalledDriver, RegistryError> {
        let bytes = tokio::fs::read(&inspection.file).await?;
        if sha256_hex(&bytes) != inspection.sha256 {
            return Err(RegistryError::Sha256Mismatch {
                what: inspection.file.display().to_string(),
                expected: inspection.sha256.clone(),
                got: sha256_hex(&bytes),
            });
        }
        // Unpack the bytes just checked, from a private copy: the file on disk may change
        // again between the check and the unpacking.
        let staging = self.staging()?;
        let copy = staging.0.join("checked.tar.gz");
        tokio::fs::write(&copy, &bytes).await?;
        let unpacked = staging.0.join("unpacked");
        extract_archive(&copy, &unpacked)?;
        self.finish(
            staging,
            unpacked,
            Some((&inspection.id, &inspection.version)),
            "manual",
        )
        .await
    }
}
