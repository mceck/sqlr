//! Fetching the index, installing, activating and removing drivers.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlr_core::DriverManifest;
use sqlr_sidecar::{SidecarDriver, SidecarSpec};

use crate::download::Fetcher;
use crate::index::{RegistryEntry, parse_index, platform_key, valid_id};
use crate::verify::{sha256_hex, verify_signature};
use crate::{Artifact, RegistryError};

/// Where JDBC driver jars are downloaded from unless the registry is told otherwise.
pub const MAVEN_CENTRAL: &str = "https://repo1.maven.org/maven2";

/// What an install's `driver.toml` says.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DriverToml {
    pub id: String,
    pub version: String,
    /// The executable, relative to the install directory.
    pub executable: String,
    /// Arguments the executable is started with (the JDBC launcher takes its preset here).
    #[serde(default)]
    pub args: Vec<String>,
    /// `"registry"`, `"release"` or `"manual"`.
    #[serde(default)]
    pub source: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstalledDriver {
    pub id: String,
    /// Every installed version, oldest first.
    pub versions: Vec<String>,
    pub active: String,
    /// The active version's directory.
    pub path: PathBuf,
    pub manifest: DriverManifest,
    /// The active version was installed from a local file, not downloaded.
    pub manual: bool,
}

impl InstalledDriver {
    fn meta(&self) -> Option<DriverToml> {
        toml::from_str(&std::fs::read_to_string(self.path.join("driver.toml")).ok()?).ok()
    }

    /// The executable of the active version.
    pub fn executable(&self) -> Option<PathBuf> {
        Some(self.path.join(self.meta()?.executable))
    }

    /// The arguments the executable is started with.
    pub fn args(&self) -> Vec<String> {
        self.meta().map(|m| m.args).unwrap_or_default()
    }
}

pub struct Registry {
    data_dir: PathBuf,
    index_url: String,
    public_key: String,
    fetcher: Fetcher,
    handshake_timeout: Duration,
    maven_base: String,
}

/// Removes a staging directory when dropped, whatever happened.
pub(crate) struct Staging(pub PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Registry {
    pub fn new(data_dir: PathBuf, index_url: String, public_key: &str) -> Self {
        Self {
            data_dir,
            index_url,
            public_key: public_key.to_string(),
            fetcher: Fetcher::new(),
            handshake_timeout: Duration::from_secs(10),
            maven_base: MAVEN_CENTRAL.to_string(),
        }
    }

    /// Where JDBC driver jars come from (`https://…` or `file://…`); Maven Central by default.
    pub fn with_maven_base(mut self, base: impl Into<String>) -> Self {
        self.maven_base = base.into().trim_end_matches('/').to_string();
        self
    }

    pub fn drivers_dir(&self) -> PathBuf {
        self.data_dir.join("drivers")
    }

    pub fn index_url(&self) -> &str {
        &self.index_url
    }

    pub fn fetcher(&self) -> &Fetcher {
        &self.fetcher
    }

    pub(crate) fn staging(&self) -> Result<Staging, RegistryError> {
        let dir = self
            .drivers_dir()
            .join(".staging")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir)?;
        Ok(Staging(dir))
    }

    /// Fetches the index and its signature (`<index url>.minisig`) and checks the signature
    /// against the registry key before reading a byte of the index.
    pub async fn fetch_index(&self) -> Result<Vec<RegistryEntry>, RegistryError> {
        let bytes = self.fetcher.fetch_bytes(&self.index_url).await?;
        let sig_url = format!("{}.minisig", self.index_url);
        let signature = self.fetcher.fetch_bytes(&sig_url).await?;
        let signature = String::from_utf8(signature).map_err(|_| RegistryError::Signature {
            what: "the registry index".into(),
            reason: "the signature file is not text".into(),
        })?;
        verify_signature(&self.public_key, &bytes, &signature, "the registry index")?;
        parse_index(&bytes, &platform_key())
    }

    /// Downloads, verifies and installs `entry` for this platform, then makes it active.
    /// Nothing stays on disk unless every check passed and the driver answered its handshake.
    pub async fn install(
        &self,
        entry: &RegistryEntry,
        progress: impl Fn(u64, Option<u64>) + Send + Sync,
    ) -> Result<InstalledDriver, RegistryError> {
        let artifact =
            entry
                .artifact
                .as_ref()
                .ok_or_else(|| RegistryError::NoBuildForPlatform {
                    id: entry.id.clone(),
                    platform: platform_key(),
                })?;
        if self.version_dir(&entry.id, &entry.version).exists() {
            return Err(RegistryError::AlreadyInstalled {
                id: entry.id.clone(),
                version: entry.version.clone(),
            });
        }
        let staging = self.staging()?;
        let file = staging.0.join("artifact.tar.gz");
        self.fetcher
            .download_to(&artifact.url, &file, &progress)
            .await?;
        self.check_artifact(&file, artifact, &entry.id)?;
        let unpacked = staging.0.join("unpacked");
        extract_archive(&file, &unpacked)?;
        self.fetch_jars(entry, &unpacked.join("jars"), &progress)
            .await?;
        self.finish(
            staging,
            unpacked,
            Some((&entry.id, &entry.version)),
            "registry",
        )
        .await
    }

    /// Downloads the entry's JDBC jars (`group:artifact:version`) into `dir`, each checked
    /// against the sha256 the signed index names. A jar is code that runs in the bridge, so
    /// an unchecked one is never kept.
    async fn fetch_jars(
        &self,
        entry: &RegistryEntry,
        dir: &Path,
        progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
    ) -> Result<(), RegistryError> {
        if entry.jdbc.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(dir)?;
        for jar in &entry.jdbc {
            let plain = |s: &str| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
                    && !s.starts_with('.')
            };
            if !plain(&jar.group) || !plain(&jar.artifact) || !plain(&jar.version) {
                return Err(RegistryError::BadIndex(format!(
                    "{}: the jar {}:{}:{} has an unsafe name",
                    entry.id, jar.group, jar.artifact, jar.version
                )));
            }
            let file_name = format!("{}-{}.jar", jar.artifact, jar.version);
            let url = format!(
                "{}/{}/{}/{}/{file_name}",
                self.maven_base,
                jar.group.replace('.', "/"),
                jar.artifact,
                jar.version
            );
            let dest = dir.join(&file_name);
            self.fetcher.download_to(&url, &dest, progress).await?;
            let got = sha256_hex(&std::fs::read(&dest)?);
            if !got.eq_ignore_ascii_case(jar.sha256.trim()) {
                return Err(RegistryError::Sha256Mismatch {
                    what: file_name,
                    expected: jar.sha256.clone(),
                    got,
                });
            }
        }
        Ok(())
    }

    fn check_artifact(
        &self,
        file: &Path,
        artifact: &Artifact,
        id: &str,
    ) -> Result<(), RegistryError> {
        let bytes = std::fs::read(file)?;
        let got = sha256_hex(&bytes);
        if !got.eq_ignore_ascii_case(artifact.sha256.trim()) {
            return Err(RegistryError::Sha256Mismatch {
                what: id.to_string(),
                expected: artifact.sha256.clone(),
                got,
            });
        }
        verify_signature(&self.public_key, &bytes, &artifact.signature, id)
    }

    pub(crate) fn version_dir(&self, id: &str, version: &str) -> PathBuf {
        self.drivers_dir().join(id).join(version)
    }

    /// Validates the unpacked driver, starts it once to read its manifest and moves it into
    /// place. `expect` pins the id and version the index or release promised.
    pub(crate) async fn finish(
        &self,
        staging: Staging,
        unpacked: PathBuf,
        expect: Option<(&str, &str)>,
        source: &str,
    ) -> Result<InstalledDriver, RegistryError> {
        let text = std::fs::read_to_string(unpacked.join("driver.toml"))
            .map_err(|_| RegistryError::UnsafeArchive("driver.toml is missing".into()))?;
        let mut meta: DriverToml = toml::from_str(&text)
            .map_err(|e| RegistryError::UnsafeArchive(format!("driver.toml: {e}")))?;
        if !valid_id(&meta.id) || semver::Version::parse(&meta.version).is_err() {
            return Err(RegistryError::UnsafeArchive(
                "driver.toml names an invalid id or version".into(),
            ));
        }
        if let Some((id, version)) = expect
            && (meta.id != id || meta.version != version)
        {
            return Err(RegistryError::UnsafeArchive(format!(
                "the archive holds {} {}, expected {id} {version}",
                meta.id, meta.version
            )));
        }
        let exe = safe_relative(&meta.executable).ok_or_else(|| {
            RegistryError::UnsafeArchive("the executable path escapes the install".into())
        })?;
        let exe_path = unpacked.join(&exe);
        if !exe_path.is_file() {
            return Err(RegistryError::UnsafeArchive(format!(
                "the executable {} is not in the archive",
                meta.executable
            )));
        }
        make_executable(&exe_path)?;
        let target = self.version_dir(&meta.id, &meta.version);
        if target.exists() {
            return Err(RegistryError::AlreadyInstalled {
                id: meta.id,
                version: meta.version,
            });
        }
        let mut spec = SidecarSpec::new(meta.id.clone(), meta.version.clone(), exe_path);
        spec.args = meta.args.clone();
        spec.handshake_timeout = self.handshake_timeout;
        let (_, manifest) = SidecarDriver::probe(spec)
            .await
            .map_err(RegistryError::Probe)?;
        if manifest.id != meta.id {
            return Err(RegistryError::UnsafeArchive(format!(
                "the driver calls itself {:?}, its archive {:?}",
                manifest.id, meta.id
            )));
        }
        meta.source = source.to_string();
        std::fs::write(
            unpacked.join("driver.toml"),
            toml::to_string(&meta).expect("driver.toml serializes"),
        )?;
        std::fs::write(
            unpacked.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).expect("a manifest serializes"),
        )?;
        std::fs::create_dir_all(target.parent().expect("a version dir has a parent"))?;
        std::fs::rename(&unpacked, &target)?;
        drop(staging);
        self.write_active(&meta.id, &meta.version)?;
        self.installed_one(&meta.id)?
            .ok_or_else(|| RegistryError::NotInstalled(meta.id.clone()))
    }

    fn write_active(&self, id: &str, version: &str) -> Result<(), RegistryError> {
        let dir = self.drivers_dir().join(id);
        let tmp = dir.join(".active.tmp");
        std::fs::write(&tmp, version)?;
        std::fs::rename(tmp, dir.join("active"))?;
        Ok(())
    }

    /// Every installed driver, by id.
    pub fn installed(&self) -> Result<Vec<InstalledDriver>, RegistryError> {
        let dir = self.drivers_dir();
        let Ok(read) = std::fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut ids: Vec<String> = read
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| valid_id(n))
            .collect();
        ids.sort();
        let mut out = Vec::new();
        for id in ids {
            match self.installed_one(&id) {
                Ok(Some(driver)) => out.push(driver),
                Ok(None) => {}
                Err(e) => tracing::warn!(driver = %id, error = %e, "installed driver unreadable"),
            }
        }
        Ok(out)
    }

    fn installed_one(&self, id: &str) -> Result<Option<InstalledDriver>, RegistryError> {
        let dir = self.drivers_dir().join(id);
        let mut versions: Vec<semver::Version> = std::fs::read_dir(&dir)?
            .flatten()
            .filter(|e| e.path().join("driver.toml").is_file())
            .filter_map(|e| semver::Version::parse(&e.file_name().into_string().ok()?).ok())
            .collect();
        // `semver::Version` ignores build metadata, so break ties by the full spelling to
        // keep the active version deterministic.
        versions.sort_by(|a, b| a.cmp(b).then_with(|| a.to_string().cmp(&b.to_string())));
        let Some(newest) = versions.last().map(ToString::to_string) else {
            return Ok(None);
        };
        let versions: Vec<String> = versions.iter().map(ToString::to_string).collect();
        let active = std::fs::read_to_string(dir.join("active"))
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|v| versions.contains(v))
            .unwrap_or(newest);
        let path = dir.join(&active);
        let manifest: DriverManifest =
            serde_json::from_slice(&std::fs::read(path.join("manifest.json"))?)
                .map_err(|e| RegistryError::Io(format!("manifest.json of {id}: {e}")))?;
        let meta: DriverToml = toml::from_str(&std::fs::read_to_string(path.join("driver.toml"))?)
            .map_err(|e| RegistryError::Io(format!("driver.toml of {id}: {e}")))?;
        Ok(Some(InstalledDriver {
            id: id.to_string(),
            versions,
            active,
            path,
            manifest,
            manual: meta.source == "manual",
        }))
    }

    /// Makes `version` the one in use (rollback is activating an older one).
    pub fn activate(&self, id: &str, version: &str) -> Result<(), RegistryError> {
        if !valid_id(id)
            || semver::Version::parse(version).is_err()
            || !self.version_dir(id, version).join("driver.toml").is_file()
        {
            return Err(RegistryError::NotInstalled(format!("{id} {version}")));
        }
        self.write_active(id, version)
    }

    /// Removes one version, or every version with `None`. Removing the active version
    /// activates the newest that remains.
    pub fn remove(&self, id: &str, version: Option<&str>) -> Result<(), RegistryError> {
        if !valid_id(id) || !self.drivers_dir().join(id).is_dir() {
            return Err(RegistryError::NotInstalled(id.to_string()));
        }
        let dir = self.drivers_dir().join(id);
        let Some(version) = version else {
            std::fs::remove_dir_all(dir)?;
            return Ok(());
        };
        if semver::Version::parse(version).is_err() || !dir.join(version).is_dir() {
            return Err(RegistryError::NotInstalled(format!("{id} {version}")));
        }
        std::fs::remove_dir_all(dir.join(version))?;
        match self.installed_one(id) {
            Ok(Some(rest)) => self.write_active(id, &rest.active),
            // Only a directory with no readable version left is removed wholesale. An
            // unreadable remaining version must not take the other versions with it.
            Ok(None) => {
                std::fs::remove_dir_all(dir)?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

/// `path` when it stays inside the directory it is relative to.
pub(crate) fn safe_relative(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    (!path.is_empty()
        && p.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)))
    .then(|| p.to_path_buf())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), RegistryError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), RegistryError> {
    Ok(())
}

/// The most an unpacked driver may weigh.
const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_DRIVER_TOML_BYTES: u64 = 64 * 1024;

fn checked_archive_size(total: u64, size: u64) -> Result<u64, RegistryError> {
    total
        .checked_add(size)
        .filter(|&size| size <= MAX_UNPACKED_BYTES)
        .ok_or_else(|| {
            RegistryError::UnsafeArchive("the archive exceeds the size limit (2 GiB)".into())
        })
}

fn is_driver_toml(path: &Path) -> bool {
    path.components()
        .filter(|c| *c != Component::CurDir)
        .collect::<PathBuf>()
        == Path::new("driver.toml")
}

fn check_manifest_size(path: &Path, size: u64) -> Result<(), RegistryError> {
    if is_driver_toml(path) && size > MAX_DRIVER_TOML_BYTES {
        return Err(RegistryError::UnsafeArchive(
            "driver.toml exceeds the size limit (64 KiB)".into(),
        ));
    }
    Ok(())
}

/// Unpacks a `.tar.gz` driver archive into `dest`. Only regular files and directories under
/// `dest` are written: absolute paths, `..`, links and device files refuse the archive.
pub(crate) fn extract_archive(file: &Path, dest: &Path) -> Result<(), RegistryError> {
    let reader = std::io::BufReader::new(std::fs::File::open(file)?);
    // Bound decoded bytes too: tar consumes extension headers and padding internally,
    // before it yields entries whose declared sizes we can inspect.
    let mut archive =
        tar::Archive::new(flate2::read::GzDecoder::new(reader).take(MAX_UNPACKED_BYTES));
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);
    std::fs::create_dir_all(dest)?;
    let mut total = 0u64;
    let unsafe_archive = |m: String| RegistryError::UnsafeArchive(m);
    for entry in archive
        .entries()
        .map_err(|e| unsafe_archive(format!("not a tar.gz archive: {e}")))?
    {
        let mut entry = entry.map_err(|e| unsafe_archive(e.to_string()))?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(unsafe_archive(format!(
                "an entry of type {kind:?} is not allowed"
            )));
        }
        let path = entry
            .path()
            .map_err(|e| unsafe_archive(e.to_string()))?
            .into_owned();
        if safe_relative(&path.to_string_lossy()).is_none() {
            return Err(unsafe_archive(format!(
                "{} leaves the install",
                path.display()
            )));
        }
        total = checked_archive_size(total, entry.size())?;
        check_manifest_size(&path, entry.size())?;
        entry
            .unpack_in(dest)
            .map_err(|e| unsafe_archive(e.to_string()))?;
    }
    Ok(())
}

/// Reads `driver.toml` from an archive without unpacking anything else.
pub(crate) fn read_driver_toml(bytes: &[u8]) -> Result<DriverToml, RegistryError> {
    let mut archive =
        tar::Archive::new(flate2::read::GzDecoder::new(bytes).take(MAX_UNPACKED_BYTES));
    let bad = |m: String| RegistryError::UnsafeArchive(m);
    let mut total = 0;
    for entry in archive
        .entries()
        .map_err(|e| bad(format!("not a tar.gz archive: {e}")))?
    {
        let mut entry = entry.map_err(|e| bad(e.to_string()))?;
        total = checked_archive_size(total, entry.size())?;
        let path = entry.path().map_err(|e| bad(e.to_string()))?.into_owned();
        check_manifest_size(&path, entry.size())?;
        if entry.header().entry_type().is_file() && is_driver_toml(&path) {
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|e| bad(e.to_string()))?;
            return toml::from_str(&text).map_err(|e| bad(format!("driver.toml: {e}")));
        }
    }
    Err(bad("driver.toml is missing".into()))
}
