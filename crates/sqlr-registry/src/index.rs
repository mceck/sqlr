//! The index format: `{ "format": 1, "drivers": [ … ] }`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::RegistryError;

/// A downloadable build of a driver for one platform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    /// The minisign signature (text of the `.minisig` file) of the artifact file.
    pub signature: String,
}

/// A JDBC driver jar fetched from Maven Central.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MavenArtifact {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct IndexFile {
    pub format: u32,
    pub drivers: Vec<RawEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RawEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub min_protocol: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub artifacts: BTreeMap<String, Artifact>,
    #[serde(default)]
    pub jdbc: Option<RawJdbc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RawJdbc {
    #[serde(default)]
    pub maven: Vec<MavenArtifact>,
}

/// One driver of the index, resolved for this platform.
#[derive(Clone, Debug, PartialEq)]
pub struct RegistryEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub min_protocol: String,
    pub family: String,
    pub description: String,
    pub homepage: String,
    pub license: String,
    /// The build for this platform, if there is one.
    pub artifact: Option<Artifact>,
    /// Every platform the driver is built for.
    pub platforms: Vec<String>,
    pub jdbc: Vec<MavenArtifact>,
}

/// `<os>-<arch>`: `macos-aarch64`, `linux-x86_64`, `windows-x86_64`, …
pub fn platform_key() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

pub(crate) fn parse_index(
    bytes: &[u8],
    platform: &str,
) -> Result<Vec<RegistryEntry>, RegistryError> {
    let file: IndexFile =
        serde_json::from_slice(bytes).map_err(|e| RegistryError::BadIndex(e.to_string()))?;
    if file.format != 1 {
        return Err(RegistryError::BadIndex(format!(
            "index format {} is not supported (this sqlr reads format 1)",
            file.format
        )));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw in file.drivers {
        if !valid_id(&raw.id) {
            return Err(RegistryError::BadIndex(format!(
                "driver id {:?} is not a plain name",
                raw.id
            )));
        }
        if semver::Version::parse(&raw.version).is_err() {
            return Err(RegistryError::BadIndex(format!(
                "{}: {:?} is not a semantic version",
                raw.id, raw.version
            )));
        }
        if !seen.insert(raw.id.clone()) {
            return Err(RegistryError::BadIndex(format!(
                "{} is listed twice",
                raw.id
            )));
        }
        out.push(RegistryEntry {
            artifact: raw.artifacts.get(platform).cloned(),
            platforms: raw.artifacts.keys().cloned().collect(),
            jdbc: raw.jdbc.map(|j| j.maven).unwrap_or_default(),
            id: raw.id,
            name: raw.name,
            version: raw.version,
            min_protocol: raw.min_protocol,
            family: raw.family,
            description: raw.description,
            homepage: raw.homepage,
            license: raw.license,
        });
    }
    Ok(out)
}

/// Ids name directories: lowercase letters, digits, `-` and `_`, never empty or dotted.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Whether `available` is a newer semantic version than `installed`.
pub fn is_newer(available: &str, installed: &str) -> bool {
    match (
        semver::Version::parse(available),
        semver::Version::parse(installed),
    ) {
        (Ok(a), Ok(i)) => a > i,
        _ => false,
    }
}
