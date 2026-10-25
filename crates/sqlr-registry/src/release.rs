//! Driver files published with a sqlr release: the release workflow uploads
//! `sqlr-driver-<id>_<version>_<platform>.tar.gz` for every platform beside the release's
//! `SHA256SUMS.txt`. A download is checked against that list before anything is unpacked.

use crate::RegistryError;
use crate::index::{platform_key, valid_id};
use crate::install::{InstalledDriver, Registry, extract_archive};
use crate::verify::sha256_hex;

/// Where the project's GitHub releases keep their files.
pub const GITHUB_RELEASES: &str = "https://github.com/mceck/sqlr/releases/download";

/// One release's files: `<base_url>/SHA256SUMS.txt` and the driver archives beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseSource {
    /// `https://…`, `http://…` (tests) or `file://…` (a local mirror).
    pub base_url: String,
    /// The version the release publishes: every driver in it has this version.
    pub version: String,
}

impl ReleaseSource {
    /// The GitHub release of sqlr `version` (tag `v<version>`).
    pub fn github(version: &str) -> Self {
        Self {
            base_url: format!("{GITHUB_RELEASES}/v{version}"),
            version: version.to_string(),
        }
    }

    /// The file name of driver `id` built for `platform` (see [`platform_key`]).
    pub fn asset_name(&self, id: &str, platform: &str) -> String {
        format!("sqlr-driver-{id}_{}_{platform}.tar.gz", self.version)
    }

    fn url(&self, file: &str) -> String {
        format!("{}/{file}", self.base_url.trim_end_matches('/'))
    }
}

/// The checksum `SHA256SUMS.txt` lists for `name` (`sha256sum` output: `<hex>  <name>`, or
/// `<hex> *<name>` in binary mode).
fn listed_sha256(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hex, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start();
        let file = file.strip_prefix('*').unwrap_or(file);
        (file == name && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    })
}

impl Registry {
    /// Downloads driver `id` of `release` for this platform, checks it against the release's
    /// `SHA256SUMS.txt`, and installs and activates it. Nothing stays on disk unless every
    /// check passed and the driver answered its handshake.
    pub async fn install_release(
        &self,
        release: &ReleaseSource,
        id: &str,
        progress: impl Fn(u64, Option<u64>) + Send + Sync,
    ) -> Result<InstalledDriver, RegistryError> {
        if !valid_id(id) {
            return Err(RegistryError::UnsafeArchive(format!(
                "{id:?} is not a driver id"
            )));
        }
        if self.version_dir(id, &release.version).exists() {
            return Err(RegistryError::AlreadyInstalled {
                id: id.to_string(),
                version: release.version.clone(),
            });
        }
        let unavailable = |reason: String| RegistryError::ReleaseUnavailable {
            version: release.version.clone(),
            reason,
        };
        let sums = self
            .fetcher()
            .fetch_bytes(&release.url("SHA256SUMS.txt"))
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        let sums = String::from_utf8(sums)
            .map_err(|_| unavailable("SHA256SUMS.txt is not text".into()))?;
        let platform = platform_key();
        let name = release.asset_name(id, &platform);
        let expected =
            listed_sha256(&sums, &name).ok_or_else(|| RegistryError::NoBuildForPlatform {
                id: id.to_string(),
                platform,
            })?;
        let staging = self.staging()?;
        let file = staging.0.join("artifact.tar.gz");
        self.fetcher()
            .download_to(&release.url(&name), &file, &progress)
            .await?;
        let got = sha256_hex(&tokio::fs::read(&file).await?);
        if got != expected {
            return Err(RegistryError::Sha256Mismatch {
                what: name,
                expected,
                got,
            });
        }
        let unpacked = staging.0.join("unpacked");
        extract_archive(&file, &unpacked)?;
        self.finish(staging, unpacked, Some((id, &release.version)), "release")
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_are_read_in_text_and_binary_mode() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  one.tar.gz\n{b} *two.tar.gz\nnot a line\n");
        assert_eq!(listed_sha256(&sums, "one.tar.gz"), Some(a));
        assert_eq!(listed_sha256(&sums, "two.tar.gz"), Some("b".repeat(64)));
        assert_eq!(listed_sha256(&sums, "one.tar"), None);
        assert_eq!(listed_sha256("xyz  one.tar.gz", "one.tar.gz"), None);
    }
}
