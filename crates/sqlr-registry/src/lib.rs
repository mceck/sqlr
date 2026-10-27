//! Driver files: the install layout on disk, versions, rollback, downloads of the driver
//! files published with a sqlr release on GitHub, and manual installs from a local file.
//! The signed-index code remains for tests and local mirrors; the app reads no index.
//!
//! Layout: `<data dir>/drivers/<id>/<version>/{driver.toml, manifest.json, <executable>, …}`
//! and `<data dir>/drivers/<id>/active`, which names the version in use. Nothing is fetched
//! until a caller asks.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod download;
mod index;
mod install;
mod manual;
mod release;
mod verify;

pub use download::Fetcher;
pub use index::{Artifact, MavenArtifact, RegistryEntry, is_newer, platform_key};
pub use install::{InstalledDriver, MAVEN_CENTRAL, Registry};
pub use manual::ManualInspection;
pub use release::{GITHUB_RELEASES, ReleaseSource};
pub use verify::{sha256_hex, verify_signature};

use sqlr_core::DriverError;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("cannot fetch {url}: {reason}")]
    Fetch { url: String, reason: String },
    #[error("the registry index is invalid: {0}")]
    BadIndex(String),
    #[error("signature check failed for {what}: {reason}")]
    Signature { what: String, reason: String },
    #[error("sha256 mismatch for {what}: expected {expected}, got {got}")]
    Sha256Mismatch {
        what: String,
        expected: String,
        got: String,
    },
    #[error("{url} redirects to another host ({to}); refused")]
    RedirectToOtherHost { url: String, to: String },
    #[error("the sqlr {version} release has no driver files: {reason}")]
    ReleaseUnavailable { version: String, reason: String },
    #[error("{id} has no build for {platform}")]
    NoBuildForPlatform { id: String, platform: String },
    #[error("unsafe driver archive: {0}")]
    UnsafeArchive(String),
    #[error("the driver does not start: {0}")]
    Probe(DriverError),
    #[error("{id} {version} is already installed")]
    AlreadyInstalled { id: String, version: String },
    #[error("{0} is not installed")]
    NotInstalled(String),
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for RegistryError {
    fn from(e: std::io::Error) -> Self {
        RegistryError::Io(e.to_string())
    }
}

/// The project's registry key (minisign). Registry indexes and artifacts must verify
/// against it. PLACEHOLDER: no registry is published yet, and the secret half of this key
/// was discarded; the maintainers replace it with their own public key (see
/// `tools/sign-index`) when they publish the first index.
pub const REGISTRY_PUBLIC_KEY: &str = "RWQRI4ohhT8/D7wkEhQknI8svAH6C/cOvTroHgMLG++lY2UgfZm8bFnX";
