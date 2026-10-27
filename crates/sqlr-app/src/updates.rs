//! The update check and the update prompt (spec §12).
//!
//! At most once a day sqlr asks GitHub's releases API whether a release newer than the one
//! running exists. A newer version opens a modal with **Install** and **Skip this version**;
//! a version skipped once is not proposed automatically again, and Preferences → General
//! switches the daily check off. **Check for updates now** (Preferences → General, Help and
//! the command palette) runs the same check on demand, even when the daily check is off.
//!
//! Nothing is downloaded before **Install**, an automatic check's network failure is silent
//! (the app behaves exactly as if the feature were absent), and the archive is checked
//! against the release's `SHA256SUMS.txt` before it replaces anything; a release without
//! that file is not installed. Install rewrites what is running — the macOS `.app` bundle
//! and a Linux binary in a folder this user can write — and then offers to restart. Where
//! that cannot be done (Windows, a system folder) the same button opens the release page.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::RichText;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::app::SqlrApp;
use sqlr_ui::ToastKind;
use sqlr_ui::theme::{self, Spacing};

/// Why checking for, downloading or installing an update failed. The text is what the
/// update prompt and the logs show.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// A file or directory operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The HTTP request or the body transfer failed.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    /// The update state could not be serialized.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The release JSON did not parse.
    #[error("the release did not parse: {source}")]
    ReleaseParse {
        #[source]
        source: serde_json::Error,
    },
    /// The release tag is not a version.
    #[error("{tag} is not a version: {source}")]
    BadTag {
        tag: String,
        #[source]
        source: semver::Error,
    },
    /// The TLS configuration could not be built.
    #[error("TLS setup failed: {0}")]
    Tls(#[source] rustls::Error),
    /// The HTTP client could not be built.
    #[error("no HTTP client: {0}")]
    NoClient(#[source] reqwest::Error),
    /// A release URL did not parse.
    #[error("invalid release URL: {0}")]
    InvalidUrl(#[source] <reqwest::Url as std::str::FromStr>::Err),
    /// A release URL is not on GitHub.
    #[error("the release points outside GitHub: {url}")]
    OutsideGithub { url: String },
    /// The release list is over the size bound.
    #[error("the release list is too large")]
    ListTooLarge,
    /// A small text answer is over the size bound.
    #[error("the answer is too large")]
    AnswerTooLarge,
    /// The download is over the size bound.
    #[error("the download is implausibly large")]
    DownloadTooLarge,
    /// The install was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// The release ships no checksum list.
    #[error("release {version} has no SHA256SUMS.txt")]
    NoChecksums { version: String },
    /// The checksum list has no line for the archive.
    #[error("{list} lists no checksum for {asset}")]
    NoChecksum { list: String, asset: String },
    /// The archive does not match its checksum.
    #[error("the download does not match its checksum (expected {expected}, got {got})")]
    ChecksumMismatch { expected: String, got: String },
    /// The release has no archive for this platform.
    #[error("the release has no {os} build")]
    NoBuild { os: &'static str },
    /// Windows cannot install in place.
    #[error("automatic install is not available on Windows")]
    WindowsUnsupported,
    /// The unpacked archive holds nothing installable.
    #[error("the archive holds nothing this platform can install")]
    EmptyArchive,
    /// The updater thread could not start.
    #[error("the updater did not start: {0}")]
    Spawn(#[source] std::io::Error),
    /// The new executable could not be written.
    #[error("the new binary could not be written: {0}")]
    WriteBinary(#[source] std::io::Error),
    /// The new app bundle could not be opened.
    #[error("the new app did not start: {0}")]
    RelaunchApp(#[source] std::io::Error),
    /// The new executable could not be started.
    #[error("the new program did not start: {0}")]
    RelaunchProgram(#[source] std::io::Error),
    /// A path is not valid UTF-8.
    #[error("a path here is not valid UTF-8")]
    NotUtf8,
    /// A helper program could not run.
    #[error("{program} could not run: {source}")]
    Run {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// A helper program exited with an error.
    #[error("{program} failed: {reason}")]
    Failed { program: String, reason: String },
}

/// The releases API: the latest *stable* release (GitHub leaves out drafts and
/// prereleases), which is the only thing ever offered.
pub const API_URL: &str = "https://api.github.com/repos/mceck/sqlr/releases/latest";
/// The setting Preferences → General toggles.
pub const SETTING: &str = "updates.check_on_start";
/// What today's check remembered, next to `settings.toml`.
const STATE_FILE: &str = "updates.json";
/// One check a day; a check that could not reach GitHub tries again on the next launch.
const DAY_S: i64 = 86_400;
const USER_AGENT: &str = concat!("sqlr/", env!("CARGO_PKG_VERSION"));
/// A redirect may only stay on GitHub and on its download CDN.
const HOSTS: &[&str] = &[
    "api.github.com",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
const CDN_SUFFIX: &str = ".githubusercontent.com";
const MAX_REDIRECTS: usize = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The API and `SHA256SUMS.txt` are small answers; they must not hang the worker.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// Generous: an archive can weigh tens of megabytes on a slow link.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// The largest answer the two text requests accept, a guard against a wrong URL.
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// The largest archive, a guard against a wrong URL.
const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
/// How often a download reports progress, in repaints as much as in bytes.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// How deep the archives are searched for the app or the binary they carry.
const UNPACK_DEPTH: usize = 4;

/// The archive the release ships for this platform, by the name the release workflow
/// publishes (`sqlr_<version>_<platform>.<ext>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// What the latest release offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Latest {
    /// The tag without its `v`, parsed as a version by [`parse_latest`].
    pub version: String,
    /// The release page, which is also where Install falls back to.
    pub url: String,
    pub assets: Vec<Asset>,
}

/// The platforms the release workflow builds for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Linux,
    /// Linux running from an AppImage, which replaces itself with the new AppImage.
    LinuxAppImage,
    Windows,
}

/// This build's platform.
pub fn platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Macos
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else if appimage().is_some() {
        Platform::LinuxAppImage
    } else {
        Platform::Linux
    }
}

/// The `.AppImage` file this process runs from: the AppImage runtime exports its path
/// as `APPIMAGE`, while `current_exe` points into its read-only mount.
fn appimage() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("APPIMAGE").filter(|v| !v.is_empty())?);
    path.is_file().then_some(path)
}

impl Platform {
    /// The platform as the release workflow spells it in an asset name.
    fn tag(&self) -> &'static str {
        match self {
            Platform::Macos if cfg!(target_arch = "aarch64") => "macos-arm64.dmg",
            Platform::Macos => "macos-x86_64.dmg",
            Platform::Linux if cfg!(target_arch = "aarch64") => "linux-aarch64.tar.gz",
            Platform::Linux => "linux-x86_64.tar.gz",
            Platform::LinuxAppImage if cfg!(target_arch = "aarch64") => "linux-aarch64.AppImage",
            Platform::LinuxAppImage => "linux-x86_64.AppImage",
            Platform::Windows => "windows-x86_64.zip",
        }
    }
}

/// What an install replaces here, and why not when it cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The `.app` bundle sqlr runs from (macOS).
    Bundle(PathBuf),
    /// The executable itself, in a folder this user may write (Linux).
    Binary(PathBuf),
}

/// Everything the update prompt and its workers pass around; polled from
/// [`SqlrApp::poll_updates`](SqlrApp::poll_updates).
#[derive(Debug)]
pub enum Msg {
    /// The check's answer; an `Err` is logged and never shown, unless the person asked
    /// for the check explicitly. `manual` is that request.
    Checked {
        result: Result<Latest, UpdateError>,
        manual: bool,
    },
    /// Bytes downloaded so far, reported about ten times a second.
    Progress { done: u64, total: Option<u64> },
    /// The new version replaced the running one.
    Installed { version: String },
    /// The install stopped; the prompt comes back with the reason.
    Failed { error: UpdateError },
    /// The download was cancelled from the prompt.
    Cancelled,
}

/// The update prompt's state. It starts and ends [`UpdateFlow::Idle`].
#[derive(Debug, Default)]
pub enum UpdateFlow {
    /// Nothing to show: not due, no newer version, skipped, or dismissed for now.
    #[default]
    Idle,
    /// A newer version exists; the modal offers Install and Skip this version. `error` is
    /// why the last install attempt stopped, shown above the buttons.
    Available {
        latest: Latest,
        error: Option<String>,
    },
    /// Downloading and replacing; the modal shows progress instead of the buttons.
    Installing {
        latest: Latest,
        name: String,
        done: u64,
        total: u64,
    },
    /// Installed; the modal offers to restart into it.
    Installed { version: String },
}

/// What the check remembers between launches, in `<config dir>/updates.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The version the person answered **Skip this version** with; not offered
    /// automatically again, but an explicit check shows it.
    pub skipped_version: String,
    /// Unix seconds of the last check that ran to an answer; 0 when never checked.
    pub last_check_at: i64,
}

impl State {
    /// The stored state; an unreadable file is treated as no memory at all.
    pub fn load(dir: &Path) -> Self {
        let Ok(bytes) = std::fs::read(dir.join(STATE_FILE)) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// The stored state, written beside `settings.toml`. A write is a temp file and a
    /// rename, so an interrupted save leaves the previous state in place. The temp name is
    /// unique per write: a check finishing while a skip is saved must not share it.
    pub fn save(&self, dir: &Path) -> Result<(), UpdateError> {
        use std::sync::atomic::AtomicU64;

        static NEXT_TMP: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self)?;
        let path = dir.join(STATE_FILE);
        let mut tmp_name = path.as_os_str().to_owned();
        tmp_name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            NEXT_TMP.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp = PathBuf::from(tmp_name);
        if let Err(error) = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error.into());
        }
        Ok(())
    }

    /// Records a finished check. Only an answer resets the day: a check that could not
    /// reach GitHub runs again on the next launch.
    pub fn record_check(&mut self, now: i64, reached: bool) {
        if reached {
            self.last_check_at = now;
        }
    }

    /// Whether a check is due: never more than once a day, and always on the first launch.
    pub fn due(&self, now: i64) -> bool {
        self.last_check_at <= 0 || now.saturating_sub(self.last_check_at) >= DAY_S
    }
}

/// The release parsed from the API's JSON. A body that does not parse, or a tag that is not
/// a version, is an error: nothing is ever offered on a guess.
pub fn parse_latest(bytes: &[u8]) -> Result<Latest, UpdateError> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        html_url: String,
        #[serde(default)]
        assets: Vec<AssetJson>,
    }
    #[derive(Deserialize)]
    struct AssetJson {
        name: String,
        browser_download_url: String,
        #[serde(default)]
        size: u64,
    }

    let release: Release =
        serde_json::from_slice(bytes).map_err(|source| UpdateError::ReleaseParse { source })?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    semver::Version::parse(&version).map_err(|source| UpdateError::BadTag {
        tag: release.tag_name.clone(),
        source,
    })?;
    Ok(Latest {
        version,
        url: release.html_url,
        assets: release
            .assets
            .into_iter()
            .map(|a| Asset {
                name: a.name,
                url: a.browser_download_url,
                size: a.size,
            })
            .collect(),
    })
}

/// Whether `candidate` is strictly newer than `current`. A tag that does not parse is never
/// newer: a broken release cannot push an upgrade.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn version(value: &str) -> Option<semver::Version> {
        semver::Version::parse(value.trim().trim_start_matches('v')).ok()
    }
    match (version(candidate), version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// Whether the check's answer is worth proposing: newer than what runs, and not the version
/// that was answered **Skip this version**. An explicit check ignores the skip: the person
/// asked for it, so a skipped release is shown again.
pub fn should_offer(latest: &Latest, current: &str, skipped: &str) -> bool {
    is_newer(&latest.version, current) && skipped != latest.version
}

/// What a finished check offers, given who asked for it.
pub fn offer_on_check(latest: &Latest, current: &str, skipped: &str, manual: bool) -> bool {
    should_offer(latest, current, skipped) || (manual && is_newer(&latest.version, current))
}

/// This platform's archive in `latest`, by the name the release workflow publishes.
pub fn pick_asset(latest: &Latest, platform: Platform) -> Option<&Asset> {
    let suffix = platform.tag();
    latest
        .assets
        .iter()
        .find(|asset| asset.name.ends_with(suffix))
}

/// The `SHA256SUMS.txt` line for `name`, if the release lists one. `sha256sum` prints
/// `hash  name`, optionally with a `*` before the name for its binary mode.
pub fn checksum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let hash = fields.next()?;
        let file = fields.next()?.trim_start_matches('*');
        (file == name).then(|| hash.to_ascii_lowercase())
    })
}

/// What an install can replace here, and a sentence about why not when it cannot. The
/// prompt turns the error into **Open release page**.
pub fn target() -> Result<Target, &'static str> {
    match platform() {
        Platform::Windows => Err("automatic install is not available on Windows"),
        Platform::Macos => {
            let exe = std::env::current_exe().map_err(|_| "this build's own path is unknown")?;
            let bundle = exe
                .ancestors()
                .find(|path| {
                    path.extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
                })
                .ok_or("sqlr is not running from an app bundle")?;
            Ok(Target::Bundle(bundle.to_path_buf()))
        }
        Platform::LinuxAppImage => {
            let file = appimage().ok_or("this AppImage's own path is unknown")?;
            let dir = file
                .parent()
                .ok_or("this AppImage's own folder is unknown")?;
            if !writable(dir) {
                return Err("this AppImage sits in a folder only an administrator can write");
            }
            Ok(Target::Binary(file))
        }
        Platform::Linux => {
            let exe = std::env::current_exe().map_err(|_| "this build's own path is unknown")?;
            let dir = exe.parent().ok_or("this build's own folder is unknown")?;
            if !writable(dir) {
                return Err("this copy of sqlr sits in a folder only an administrator can write");
            }
            Ok(Target::Binary(exe))
        }
    }
}

/// Whether this user can create a file in `dir`. Trying is the honest answer: a permission
/// bit ignores groups and ACLs.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".sqlr-write-test-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// The HTTPS client: ring, the system's roots, and redirects that may only stay on GitHub
/// and its CDN (`github.com` sends downloads to `…githubusercontent.com`).
fn client() -> Result<reqwest::blocking::Client, UpdateError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(UpdateError::Tls)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        let url = attempt.url();
        let allowed = attempt.previous().len() < MAX_REDIRECTS
            && url.scheme() == "https"
            && is_allowed_host(url.host_str().unwrap_or_default());
        if allowed {
            attempt.follow()
        } else {
            attempt.error("a redirect left GitHub")
        }
    });
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(policy)
        .tls_backend_preconfigured(tls)
        .build()
        .map_err(UpdateError::NoClient)
}

fn is_allowed_host(host: &str) -> bool {
    HOSTS.contains(&host) || host.ends_with(CDN_SUFFIX)
}

/// Parses a URL the release points at, and refuses anything that is not an HTTPS URL on
/// GitHub or its download CDN. The API's answer is trusted over TLS; this is the second
/// check, so a compromised body cannot send the updater to an arbitrary server.
fn release_url(url: &str) -> Result<reqwest::Url, UpdateError> {
    let parsed = reqwest::Url::parse(url).map_err(UpdateError::InvalidUrl)?;
    if parsed.scheme() != "https" || !parsed.host_str().is_some_and(is_allowed_host) {
        return Err(UpdateError::OutsideGithub {
            url: url.to_string(),
        });
    }
    Ok(parsed)
}

/// The latest release, as the API describes it.
pub fn fetch_latest() -> Result<Latest, UpdateError> {
    let response = client()?
        .get(release_url(API_URL)?)
        .header("Accept", "application/vnd.github+json")
        .timeout(CHECK_TIMEOUT)
        .send()?
        .error_for_status()?;
    let bytes = response.bytes()?;
    if bytes.len() > MAX_TEXT_BYTES {
        return Err(UpdateError::ListTooLarge);
    }
    parse_latest(&bytes)
}

/// Downloads `asset` to `dest`, reporting progress about ten times a second and stopping
/// as soon as `cancel` is set. The body is written through [`Progress`], so a cancelled
/// transfer stops mid-flight instead of after the last byte.
fn download(
    asset: &Asset,
    dest: &Path,
    tx: &Sender<Msg>,
    repaint: &egui::Context,
    cancel: &Arc<AtomicBool>,
) -> Result<(), UpdateError> {
    let mut response = client()?
        .get(release_url(&asset.url)?)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()?
        .error_for_status()?;
    let total = response
        .content_length()
        .or((asset.size > 0).then_some(asset.size));
    let total = match total {
        Some(total) if total <= MAX_ASSET_BYTES => Some(total),
        Some(_) => return Err(UpdateError::DownloadTooLarge),
        None => None,
    };
    let file = std::fs::File::create(dest)?;
    let mut progress = Progress {
        file,
        tx: tx.clone(),
        repaint: repaint.clone(),
        cancel: cancel.clone(),
        done: 0,
        total,
        last: Instant::now(),
    };
    response.copy_to(&mut progress)?;
    // The last write may have been before `PROGRESS_EVERY`; report where it ended.
    let _ = progress.tx.send(Msg::Progress {
        done: progress.done,
        total,
    });
    progress.repaint.request_repaint();
    Ok(())
}

/// The body's destination, which reports progress and honours the cancel flag.
struct Progress {
    file: std::fs::File,
    tx: Sender<Msg>,
    repaint: egui::Context,
    cancel: Arc<AtomicBool>,
    done: u64,
    total: Option<u64>,
    last: Instant,
}

impl std::io::Write for Progress {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            // Not `ErrorKind::Interrupted`: `io::copy` retries that one forever.
            return Err(std::io::Error::other("cancelled"));
        }
        // The Content-Length was optional and could lie; the bytes actually received are
        // the guard that keeps a wrong URL from filling the disk.
        if self.done.saturating_add(buf.len() as u64) > MAX_ASSET_BYTES {
            return Err(std::io::Error::other("the download is implausibly large"));
        }
        let written = self.file.write(buf)?;
        self.done += written as u64;
        if self.last.elapsed() >= PROGRESS_EVERY {
            self.last = Instant::now();
            let _ = self.tx.send(Msg::Progress {
                done: self.done,
                total: self.total,
            });
            self.repaint.request_repaint();
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// The archive against the release's `SHA256SUMS.txt`. The release workflow always ships
/// that file, and the replacement is code: a release that cannot be checked is not
/// installed.
fn verify(latest: &Latest, asset: &Asset, file: &Path) -> Result<(), UpdateError> {
    let sums = latest
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case("SHA256SUMS.txt"))
        .ok_or_else(|| UpdateError::NoChecksums {
            version: latest.version.clone(),
        })?;
    let text = get(&sums.url, CHECK_TIMEOUT)?;
    let text = String::from_utf8_lossy(&text);
    let expected = checksum_for(&text, &asset.name).ok_or_else(|| UpdateError::NoChecksum {
        list: sums.name.clone(),
        asset: asset.name.clone(),
    })?;
    let got = sha256_file(file)?;
    if !got.eq_ignore_ascii_case(&expected) {
        return Err(UpdateError::ChecksumMismatch { expected, got });
    }
    Ok(())
}

/// A bounded GET for the two small text files this feature reads.
fn get(url: &str, timeout: Duration) -> Result<Vec<u8>, UpdateError> {
    let response = client()?
        .get(release_url(url)?)
        .timeout(timeout)
        .send()?
        .error_for_status()?;
    let bytes = response.bytes()?;
    if bytes.len() > MAX_TEXT_BYTES {
        return Err(UpdateError::AnswerTooLarge);
    }
    Ok(bytes.to_vec())
}

/// The file's sha-256 as lower-case hex, the spelling `sha256sum` prints.
pub fn sha256_file(path: &Path) -> Result<String, UpdateError> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Downloads and verifies `latest`'s archive for this platform, unpacks it and replaces
/// `target`. The prompt learns the outcome through `tx`.
pub fn spawn_install(
    target: Target,
    latest: Latest,
    tx: Sender<Msg>,
    repaint: egui::Context,
    cancel: Arc<AtomicBool>,
) -> Result<(), UpdateError> {
    std::thread::Builder::new()
        .name("sqlr-update".into())
        .spawn(move || {
            let result = install(&target, &latest, &tx, &repaint, &cancel);
            let message = match result {
                Ok(()) => Msg::Installed {
                    version: latest.version.clone(),
                },
                // A cancelled transfer fails like any other; only the flag says which it
                // was, so the prompt can go back to the buttons without a reason.
                Err(_) if cancel.load(Ordering::Relaxed) => Msg::Cancelled,
                Err(error) => Msg::Failed { error },
            };
            let _ = tx.send(message);
            repaint.request_repaint();
        })
        .map(|_| ())
        .map_err(UpdateError::Spawn)
}

/// Stops an install at the next safe point. Once the replacement has begun it is allowed
/// to finish: half a bundle is worse than a finished one.
fn check_cancel(cancel: &AtomicBool) -> Result<(), UpdateError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(UpdateError::Cancelled);
    }
    Ok(())
}

/// The download, the checks, the unpacking and the replacement itself.
fn install(
    target: &Target,
    latest: &Latest,
    tx: &Sender<Msg>,
    repaint: &egui::Context,
    cancel: &Arc<AtomicBool>,
) -> Result<(), UpdateError> {
    let asset = pick_asset(latest, platform()).ok_or_else(|| UpdateError::NoBuild {
        os: std::env::consts::OS,
    })?;
    let temp = tempfile::Builder::new().prefix("sqlr-update-").tempdir()?;
    let archive = temp.path().join(&asset.name);
    download(asset, &archive, tx, repaint, cancel)?;
    check_cancel(cancel)?;
    verify(latest, asset, &archive)?;
    check_cancel(cancel)?;
    let unpacked = unpack(&archive, &temp.path().join("unpacked"))?;
    check_cancel(cancel)?;
    match target {
        Target::Bundle(bundle) => install_bundle(&unpacked, bundle),
        Target::Binary(exe) => install_binary(&unpacked, exe),
    }
}

/// Unpacks the archive and returns the `.app` or the binary inside it.
fn unpack(archive: &Path, into: &Path) -> Result<PathBuf, UpdateError> {
    std::fs::create_dir_all(into)?;
    match platform() {
        // The download is the new AppImage itself.
        Platform::LinuxAppImage => Ok(archive.to_path_buf()),
        Platform::Macos => {
            let mount = into.join("mount");
            let app = into.join("app");
            std::fs::create_dir_all(&mount)?;
            std::fs::create_dir_all(&app)?;
            run(
                "hdiutil",
                &[
                    "attach",
                    "-nobrowse",
                    "-readonly",
                    "-noverify",
                    "-mountpoint",
                    path(&mount)?,
                    path(archive)?,
                ],
            )?;
            // The image is detached whether or not the copy worked.
            let copied = find(&mount, UNPACK_DEPTH, &|name, is_file| {
                !is_file && name.ends_with(".app")
            })
            .and_then(|found| {
                let dest = app.join(found.file_name().unwrap_or_default());
                run("ditto", &[path(&found)?, path(&dest)?]).map(|()| dest)
            });
            let _ = run("hdiutil", &["detach", "-force", path(&mount)?]);
            copied
        }
        Platform::Linux => {
            run("tar", &["-xzf", path(archive)?, "-C", path(into)?])?;
            find(into, UNPACK_DEPTH, &|name, is_file| {
                is_file && name == "sqlr"
            })
        }
        Platform::Windows => Err(UpdateError::WindowsUnsupported),
    }
}

/// The first path under `root`, at most `depth` levels down, whose name `wanted` accepts.
/// The predicate is a trait object: a generic `impl Fn` nests a new type at every recursion
/// level and exceeds the compiler's instantiation limit.
fn find(
    root: &Path,
    depth: usize,
    wanted: &dyn Fn(&str, bool) -> bool,
) -> Result<PathBuf, UpdateError> {
    if depth == 0 {
        return Err(UpdateError::EmptyArchive);
    }
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(root)?.flatten() {
        let is_file = entry.file_type().is_ok_and(|t| t.is_file());
        let name = entry.file_name().to_string_lossy().into_owned();
        if wanted(&name, is_file) {
            return Ok(entry.path());
        }
        if !is_file {
            dirs.push(entry.path());
        }
    }
    for dir in dirs {
        if let Ok(found) = find(&dir, depth - 1, wanted) {
            return Ok(found);
        }
    }
    Err(UpdateError::EmptyArchive)
}

/// Replaces the bundle. The running one is moved aside first and put back when the new
/// copy fails, so a failed install never leaves this Mac without a sqlr app: `/Applications`
/// is written through `osascript` when this user cannot write it there.
fn install_bundle(new: &Path, bundle: &Path) -> Result<(), UpdateError> {
    let backup = beside(bundle, ".old");
    if backup.exists() {
        std::fs::remove_dir_all(&backup)?;
    }
    let writable_here = bundle.parent().is_some_and(writable);
    if writable_here {
        std::fs::rename(bundle, &backup)?;
        if let Err(error) = run("ditto", &[path(new)?, path(bundle)?]) {
            let _ = std::fs::rename(&backup, bundle);
            return Err(error);
        }
        let _ = std::fs::remove_dir_all(&backup);
        return Ok(());
    }
    // One administrator-password script, with the same way back as above. The script is
    // passed as an argument, not pasted into the AppleScript source: a path may contain a
    // quote or a backslash, and only the shell quoting below should interpret it.
    let bundle_q = shell_quote(path(bundle)?);
    let backup_q = shell_quote(path(&backup)?);
    let new_q = shell_quote(path(new)?);
    let shell = format!(
        "/bin/rm -rf {backup_q} && /bin/mv {bundle_q} {backup_q} \
         && ( /usr/bin/ditto {new_q} {bundle_q} || ( /bin/rm -rf {bundle_q} && /bin/mv {backup_q} {bundle_q} && exit 1 ) ) \
         && /bin/rm -rf {backup_q}"
    );
    run(
        "osascript",
        &[
            "-e",
            "on run argv",
            "-e",
            "do shell script (item 1 of argv) with administrator privileges",
            "-e",
            "end run",
            "--",
            &shell,
        ],
    )
}

/// Replaces the executable: rename the running one aside, copy the new one in its place,
/// then drop the old one.
fn install_binary(new: &Path, exe: &Path) -> Result<(), UpdateError> {
    let backup = beside(exe, ".old");
    if backup.exists() {
        let _ = std::fs::remove_file(&backup);
    }
    std::fs::rename(exe, &backup)?;
    if let Err(error) = std::fs::copy(new, exe).and_then(|_| set_executable(exe)) {
        let _ = std::fs::rename(&backup, exe);
        return Err(UpdateError::WriteBinary(error));
    }
    let _ = std::fs::remove_file(&backup);
    Ok(())
}

/// The executable bit `tar` already set on the unpacked copy, restated here: the file was
/// just written, and it is the one thing the install cannot get wrong.
#[cfg(unix)]
fn set_executable(exe: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Starts the copy that was just installed. The caller closes this one afterwards.
pub fn relaunch(target: &Target) -> Result<(), UpdateError> {
    match target {
        Target::Bundle(bundle) => std::process::Command::new("open")
            .arg("-n")
            .arg(bundle)
            .spawn()
            .map(|_| ())
            .map_err(UpdateError::RelaunchApp),
        Target::Binary(exe) => std::process::Command::new(exe)
            .args(std::env::args().skip(1))
            .spawn()
            .map(|_| ())
            .map_err(UpdateError::RelaunchProgram),
    }
}

/// The sibling `path` with `suffix` appended to its name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

fn path(value: &Path) -> Result<&str, UpdateError> {
    value.to_str().ok_or(UpdateError::NotUtf8)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Runs a program to completion, its standard error as the reason when it fails.
fn run(program: &str, args: &[&str]) -> Result<(), UpdateError> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|source| UpdateError::Run {
            program: program.to_string(),
            source,
        })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(UpdateError::Failed {
        program: program.to_string(),
        reason: stderr.trim().trim_end_matches('.').to_string(),
    })
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// What a press in the update modal does; the modal itself never touches the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    None,
    Install,
    Skip,
    /// Esc or a click outside: close for now, remember nothing.
    Dismiss,
    Cancel,
    Restart,
    OpenPage,
    /// The release-notes link: a new tab, prompt still open.
    OpenNotes,
}

impl SqlrApp {
    /// Starts today's check when the setting allows it and a day has not passed since the
    /// last one. The worker is silent by contract: no toast, no dialog, no log above debug
    /// for an ordinary network failure.
    pub(crate) fn start_update_check(&mut self, ctx: &egui::Context) {
        if !self.layered.get(SETTING, None).0.as_bool() {
            return;
        }
        if !self.update_state.due(unix_now()) {
            return;
        }
        self.start_check(ctx, false);
    }

    /// **Check for updates now**, from Preferences → General or Help. It ignores the daily
    /// gate and the automatic-check setting, because the person asked for it, and it
    /// reports both a failure and "you are up to date".
    pub(crate) fn check_updates_now(&mut self, ctx: &egui::Context) {
        if matches!(
            self.update,
            UpdateFlow::Installing { .. } | UpdateFlow::Installed { .. }
        ) {
            self.notify_as(
                ToastKind::Info,
                "An update is already in progress; finish it before checking again.",
            );
            return;
        }
        if self.update_rx.is_some() {
            self.notify_as(ToastKind::Info, "An update check is already running.");
            return;
        }
        self.start_check(ctx, true);
    }

    /// The one worker both entry points use. `manual` only changes what happens to the
    /// answer: an automatic check stays silent, an explicit one reports.
    fn start_check(&mut self, ctx: &egui::Context, manual: bool) {
        if self.update_rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.update_rx = Some(rx);
        let repaint = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("sqlr-update-check".into())
            .spawn(move || {
                let result = fetch_latest();
                let _ = tx.send(Msg::Checked { result, manual });
                repaint.request_repaint();
            });
        if spawned.is_err() {
            // No worker, no answer: leave the next launch free to try again.
            self.update_rx = None;
            if manual {
                self.notify("The update check could not start.");
            }
        }
    }

    /// The worker's messages this frame: they may open the prompt, move the progress bar
    /// or put the prompt back with a reason. A terminal message, or a closed channel, ends
    /// the in-flight operation, so a later check or install can start.
    pub(crate) fn poll_updates(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.update_rx.take() else {
            return;
        };
        let mut messages = Vec::new();
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(message) => messages.push(message),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        for message in messages {
            finished |= matches!(
                message,
                Msg::Checked { .. } | Msg::Installed { .. } | Msg::Failed { .. } | Msg::Cancelled
            );
            self.apply_update(message, ctx);
        }
        if !finished && self.update_rx.is_none() {
            self.update_rx = Some(rx);
        }
    }

    fn apply_update(&mut self, message: Msg, ctx: &egui::Context) {
        match message {
            Msg::Checked { result, manual } => {
                let reached = result.is_ok();
                self.update_state.record_check(unix_now(), reached);
                match result {
                    Ok(latest) => {
                        // Only an answer resets the day; a failed check runs again next
                        // launch, as the feature's contract promises.
                        self.save_update_state(ctx);
                        let current = env!("CARGO_PKG_VERSION");
                        let offer = offer_on_check(
                            &latest,
                            current,
                            &self.update_state.skipped_version,
                            manual,
                        );
                        if offer {
                            self.update = UpdateFlow::Available {
                                latest,
                                error: None,
                            };
                        } else if manual {
                            self.notify_as(
                                ToastKind::Info,
                                format!("sqlr {current} is up to date."),
                            );
                        }
                    }
                    Err(error) => {
                        tracing::debug!(target: "sqlr_updates", error = %error, "the update check failed");
                        if manual {
                            self.toasts.push(
                                ToastKind::Error,
                                format!("The update check failed: {error}"),
                            );
                        }
                    }
                }
            }
            Msg::Progress { done, total } => {
                if let UpdateFlow::Installing {
                    done: slot,
                    total: slot_total,
                    ..
                } = &mut self.update
                {
                    *slot = done;
                    if let Some(total) = total {
                        *slot_total = total;
                    }
                }
            }
            Msg::Installed { version } => {
                self.update_cancel = None;
                self.update = UpdateFlow::Installed { version };
            }
            Msg::Failed { error } => self.stop_installing(Some(error.to_string())),
            Msg::Cancelled => self.stop_installing(None),
        }
    }

    /// Puts the prompt back on **Install** after a failed or cancelled install, with the
    /// reason when there is one.
    fn stop_installing(&mut self, error: Option<String>) {
        self.update_cancel = None;
        let latest = match &self.update {
            UpdateFlow::Installing { latest, .. } => latest.clone(),
            _ => return,
        };
        self.update = UpdateFlow::Available { latest, error };
    }

    /// Writes the skipped version and last check beside `settings.toml` on a worker
    /// thread, the way settings saves run. One save runs at a time; a request made
    /// meanwhile is saved once it ends, so an older state cannot land last.
    fn save_update_state(&mut self, ctx: &egui::Context) {
        if self.update_state_save.is_some() {
            self.update_state_dirty = true;
            return;
        }
        let state = self.update_state.clone();
        let dir = self.config_dir.clone();
        let (sender, receiver) = channel();
        self.update_state_save = Some(receiver);
        let repaint = ctx.clone();
        let _ = std::thread::Builder::new()
            .name("sqlr-update-state".into())
            .spawn(move || {
                let _ = sender.send(state.save(&dir));
                repaint.request_repaint();
            });
    }

    /// Shows the outcome of a finished update-state save, then starts the one asked for
    /// meanwhile. A file that does not save costs the next launch a repeated check, not a
    /// toast.
    pub(crate) fn poll_update_state_save(&mut self, ctx: &egui::Context) {
        let Some(receiver) = &self.update_state_save else {
            return;
        };
        let failure = match receiver.try_recv() {
            Ok(result) => result.err().map(|error| error.to_string()),
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some("the save thread stopped".to_string())
            }
        };
        self.update_state_save = None;
        if let Some(error) = failure {
            tracing::debug!(target: "sqlr_updates", error = %error, "the update state did not save");
        }
        if std::mem::take(&mut self.update_state_dirty) {
            self.save_update_state(ctx);
        }
    }

    /// The update modal (spec §12): **Install** and **Skip this version**, progress while
    /// it runs, **Restart now** once the new version is in place.
    pub(crate) fn update_ui(&mut self, ctx: &egui::Context) {
        if matches!(self.update, UpdateFlow::Idle) {
            return;
        }
        let flow = std::mem::replace(&mut self.update, UpdateFlow::Idle);
        let action = modal(&flow, ctx);
        self.update = match action {
            Action::None => flow,
            Action::Dismiss => UpdateFlow::Idle,
            Action::Skip => {
                if let UpdateFlow::Available { latest, .. } = &flow {
                    self.update_state.skipped_version = latest.version.clone();
                    self.save_update_state(ctx);
                }
                UpdateFlow::Idle
            }
            Action::OpenPage => {
                if let UpdateFlow::Available { latest, .. } = &flow {
                    ctx.open_url(egui::OpenUrl::new_tab(latest.url.as_str()));
                }
                UpdateFlow::Idle
            }
            Action::OpenNotes => {
                if let UpdateFlow::Available { latest, .. } = &flow {
                    ctx.open_url(egui::OpenUrl::new_tab(latest.url.as_str()));
                }
                flow
            }
            Action::Cancel => {
                if let Some(cancel) = &self.update_cancel {
                    cancel.store(true, Ordering::Relaxed);
                }
                flow
            }
            Action::Install => self.begin_install(flow, ctx),
            Action::Restart => self.restart(flow, ctx),
        };
    }

    /// Downloads, checks and replaces; until it finishes the prompt shows progress.
    fn begin_install(&mut self, flow: UpdateFlow, ctx: &egui::Context) -> UpdateFlow {
        let available = match &flow {
            UpdateFlow::Available { latest, .. } => Some(latest.clone()),
            _ => None,
        };
        let Some(latest) = available else {
            return flow;
        };
        let picked = (target(), pick_asset(&latest, platform()));
        let (target, asset) = match picked {
            (Ok(target), Some(asset)) => (target, asset.clone()),
            (Err(why), _) => {
                return UpdateFlow::Available {
                    latest,
                    error: Some(why.to_string()),
                };
            }
            (Ok(_), None) => {
                return UpdateFlow::Available {
                    latest,
                    error: Some("the release has no build for this platform".into()),
                };
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.update_cancel = Some(cancel.clone());
        self.update_target = Some(target.clone());
        let (tx, rx) = channel();
        self.update_rx = Some(rx);
        if let Err(error) = spawn_install(target, latest.clone(), tx, ctx.clone(), cancel) {
            self.update_cancel = None;
            self.update_target = None;
            self.update_rx = None;
            return UpdateFlow::Available {
                latest,
                error: Some(error.to_string()),
            };
        }
        UpdateFlow::Installing {
            latest,
            name: asset.name.clone(),
            done: 0,
            total: asset.size,
        }
    }

    /// Starts the installed version and closes this one.
    fn restart(&mut self, flow: UpdateFlow, ctx: &egui::Context) -> UpdateFlow {
        let Some(target) = self.update_target.take() else {
            return flow;
        };
        match relaunch(&target) {
            Ok(()) => {
                // The person asked for the restart: no prompts on the way out.
                self.quit_confirmed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                UpdateFlow::Idle
            }
            Err(error) => {
                self.toasts.push(
                    ToastKind::Error,
                    format!("The new version is installed; restart sqlr by hand: {error}"),
                );
                UpdateFlow::Idle
            }
        }
    }

    /// The update prompt's state, for tests.
    #[doc(hidden)]
    pub fn update_flow_for_tests(&self) -> &'static str {
        match self.update {
            UpdateFlow::Idle => "idle",
            UpdateFlow::Available { .. } => "available",
            UpdateFlow::Installing { .. } => "installing",
            UpdateFlow::Installed { .. } => "installed",
        }
    }

    /// Offers `version` as if today's check had found it, for tests.
    #[doc(hidden)]
    pub fn offer_update_for_tests(&mut self, version: &str) {
        self.update = UpdateFlow::Available {
            latest: Latest {
                version: version.to_string(),
                url: format!("https://github.com/mceck/sqlr/releases/tag/v{version}"),
                assets: vec![],
            },
            error: None,
        };
    }

    /// Delivers a check's answer without the network, for tests.
    #[doc(hidden)]
    pub fn apply_check_for_tests(
        &mut self,
        ctx: &egui::Context,
        version: Option<&str>,
        error: Option<&str>,
        manual: bool,
    ) {
        let result = match version {
            Some(version) => Ok(Latest {
                version: version.to_string(),
                url: format!("https://github.com/mceck/sqlr/releases/tag/v{version}"),
                assets: vec![],
            }),
            None => Err(std::io::Error::other(error.unwrap_or("no answer")).into()),
        };
        self.apply_update(Msg::Checked { result, manual }, ctx);
    }

    /// The version **Skip this version** recorded, empty when none was skipped, for tests.
    #[doc(hidden)]
    pub fn skipped_version_for_tests(&self) -> String {
        self.update_state.skipped_version.clone()
    }

    /// The prompt's primary button, which depends on whether this install can replace
    /// itself here, for tests.
    #[doc(hidden)]
    pub fn update_primary_label_for_tests(&self) -> &'static str {
        if crate::updates::target().is_ok() {
            "Install"
        } else {
            "Open release page"
        }
    }
}

/// Draws the modal for `flow` and returns what was pressed; it never mutates the app, so
/// the caller can act on the answer without borrowing itself.
fn modal(flow: &UpdateFlow, ctx: &egui::Context) -> Action {
    let mut action = Action::None;
    match flow {
        UpdateFlow::Idle => Action::None,
        UpdateFlow::Available { latest, error } => {
            let install = target();
            let response = theme::Dialog::new(
                "update_available",
                theme::icons::REFRESH,
                "Update available",
            )
            .width(480.0)
            .show(ctx, |d| {
                d.body(|ui| {
                    let p = theme::palette(ui.ctx());
                    ui.label(format!(
                        "sqlr {} is available (you are running {}).",
                        latest.version,
                        env!("CARGO_PKG_VERSION")
                    ));
                    if ui
                        .hyperlink_to("Release notes", latest.url.as_str())
                        .clicked()
                    {
                        action = Action::OpenNotes;
                    }
                    if let Some(error) = error {
                        ui.label(
                            RichText::new(format!("The update did not install: {error}"))
                                .color(p.error),
                        );
                    }
                    if let Err(why) = &install {
                        ui.label(RichText::new(*why).color(p.text_muted));
                    }
                });
                d.footer(|f| {
                    if f.secondary("Skip this version", true) {
                        action = Action::Skip;
                    }
                    match &install {
                        Ok(_) => {
                            if f.primary("Install", None) {
                                action = Action::Install;
                            }
                        }
                        Err(_) => {
                            if f.primary("Open release page", None) {
                                action = Action::OpenPage;
                            }
                        }
                    }
                    f.cancel_as("Later");
                });
            });
            if action == Action::None && response.closed {
                action = Action::Dismiss;
            }
            action
        }
        UpdateFlow::Installing {
            latest,
            name,
            done,
            total,
        } => {
            let title = format!("Installing sqlr {}", latest.version);
            let response = theme::Dialog::new("update_installing", theme::icons::REFRESH, &title)
                .width(480.0)
                .show(ctx, |d| {
                    d.body(|ui| {
                        let fraction = if *total > 0 {
                            (*done as f32 / *total as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .show_percentage()
                                .desired_width(ui.available_width()),
                        );
                        ui.add_space(Spacing::S);
                        ui.label(RichText::new(name).color(theme::palette(ui.ctx()).text_muted));
                    });
                    d.footer(|f| f.cancel());
                });
            if response.closed {
                action = Action::Cancel;
            }
            action
        }
        UpdateFlow::Installed { version } => {
            let title = format!("sqlr {version} is installed");
            let response = theme::Dialog::new("update_installed", theme::icons::SUCCESS, &title)
                .width(480.0)
                .show(ctx, |d| {
                    d.body(|ui| {
                        ui.label("Restart sqlr to start using it.");
                    });
                    d.footer(|f| {
                        if f.primary("Restart now", None) {
                            action = Action::Restart;
                        }
                        f.cancel_as("Later");
                    });
                });
            if action == Action::None && response.closed {
                action = Action::Dismiss;
            }
            action
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.to_string(),
            url: format!("https://github.com/mceck/sqlr/releases/download/v1/{name}"),
            size: 42,
        }
    }

    fn release(names: &[&str]) -> Latest {
        Latest {
            version: "0.2.0".into(),
            url: "https://github.com/mceck/sqlr/releases/tag/v0.2.0".into(),
            assets: names.iter().map(|name| asset(name)).collect(),
        }
    }

    #[test]
    fn newer_versions_win() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.1.9"));
        assert!(is_newer("0.2.0", "0.2.0-rc.1"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
        assert!(!is_newer("0.2.0", "not-a-version"));
    }

    #[test]
    fn each_platform_finds_its_archive() {
        let latest = release(&[
            "sqlr_0.2.0_macos-arm64.dmg",
            "sqlr_0.2.0_macos-x86_64.dmg",
            "sqlr_0.2.0_linux-x86_64.tar.gz",
            "sqlr_0.2.0_linux-x86_64.AppImage",
            "sqlr_0.2.0_linux-aarch64.tar.gz",
            "sqlr_0.2.0_linux-aarch64.AppImage",
            "sqlr_0.2.0_windows-x86_64.zip",
            "SHA256SUMS.txt",
        ]);
        let linux_arch = if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "x86_64"
        };
        let mac = if cfg!(target_arch = "aarch64") {
            "sqlr_0.2.0_macos-arm64.dmg"
        } else {
            "sqlr_0.2.0_macos-x86_64.dmg"
        };
        assert_eq!(
            pick_asset(&latest, Platform::Macos).map(|a| a.name.as_str()),
            Some(mac)
        );
        assert_eq!(
            pick_asset(&latest, Platform::Linux).map(|a| a.name.as_str()),
            Some(format!("sqlr_0.2.0_linux-{linux_arch}.tar.gz").as_str())
        );
        assert_eq!(
            pick_asset(&latest, Platform::LinuxAppImage).map(|a| a.name.as_str()),
            Some(format!("sqlr_0.2.0_linux-{linux_arch}.AppImage").as_str())
        );
        assert_eq!(
            pick_asset(&latest, Platform::Windows).map(|a| a.name.as_str()),
            Some("sqlr_0.2.0_windows-x86_64.zip")
        );
        assert!(pick_asset(&release(&["SHA256SUMS.txt"]), Platform::Linux).is_none());
    }

    #[test]
    fn a_release_parses_and_its_tag_must_be_a_version() {
        let body = br#"{
            "tag_name": "v0.2.0",
            "html_url": "https://github.com/mceck/sqlr/releases/tag/v0.2.0",
            "assets": [
                {"name": "sqlr_0.2.0_linux-x86_64.tar.gz",
                 "browser_download_url": "https://github.com/mceck/sqlr/releases/download/v0.2.0/sqlr_0.2.0_linux-x86_64.tar.gz",
                 "size": 1234}
            ]
        }"#;
        let latest = parse_latest(body).unwrap();
        assert_eq!(latest.version, "0.2.0");
        assert_eq!(latest.assets.len(), 1);
        assert_eq!(latest.assets[0].size, 1234);

        assert!(parse_latest(b"not json at all").is_err());
        let no_version = br#"{"tag_name": "latest", "html_url": "u", "assets": []}"#;
        assert!(parse_latest(no_version).is_err());
    }

    #[test]
    fn checksums_are_found_by_name() {
        let sums = "0f3c…a1  sqlr_0.2.0_linux-x86_64.tar.gz\n1b2d…c9 *sqlr_0.2.0_macos-arm64.dmg\n";
        assert_eq!(
            checksum_for(sums, "sqlr_0.2.0_linux-x86_64.tar.gz"),
            Some("0f3c…a1".into())
        );
        assert_eq!(
            checksum_for(sums, "sqlr_0.2.0_macos-arm64.dmg"),
            Some("1b2d…c9".into())
        );
        assert_eq!(checksum_for(sums, "something-else.tar.gz"), None);
    }

    #[test]
    fn only_a_real_improvement_is_offered() {
        let latest = |version: &str| Latest {
            version: version.to_string(),
            url: String::new(),
            assets: vec![],
        };
        assert!(should_offer(&latest("0.2.0"), "0.1.0", ""));
        assert!(!should_offer(&latest("0.1.0"), "0.1.0", ""));
        assert!(!should_offer(&latest("0.1.5"), "0.1.0", "0.1.5"));
        assert!(should_offer(&latest("0.2.0"), "0.1.0", "0.1.5"));

        // An explicit check shows a skipped release again, but never an older one.
        assert!(offer_on_check(&latest("0.2.0"), "0.1.0", "0.2.0", true));
        assert!(!offer_on_check(&latest("0.1.0"), "0.1.0", "", true));
    }

    #[test]
    fn a_check_runs_once_a_day_and_survives_a_restart() {
        let mut state = State::default();
        assert!(state.due(1_000));
        state.last_check_at = 1_000;
        assert!(!state.due(1_000 + DAY_S - 1));
        assert!(state.due(1_000 + DAY_S));

        state.skipped_version = "0.2.0".into();
        let dir = tempfile::tempdir().unwrap();
        state.save(dir.path()).unwrap();
        assert_eq!(State::load(dir.path()), state);

        std::fs::write(dir.path().join(STATE_FILE), "garbage").unwrap();
        assert_eq!(State::load(dir.path()), State::default());
    }

    #[test]
    fn a_failed_check_is_retried_on_the_next_launch() {
        let mut state = State::default();
        state.record_check(1_000, false);
        assert_eq!(state.last_check_at, 0);
        assert!(state.due(1_000));

        state.record_check(1_000, true);
        assert_eq!(state.last_check_at, 1_000);
        assert!(!state.due(1_000 + DAY_S - 1));
    }

    #[test]
    fn release_urls_stay_on_github_over_https() {
        assert!(release_url(API_URL).is_ok());
        assert!(release_url("https://github.com/mceck/sqlr/releases/download/v1/a.zip").is_ok());
        assert!(release_url("https://objects.githubusercontent.com/a.zip").is_ok());
        assert!(release_url("http://github.com/mceck/sqlr/releases/download/v1/a.zip").is_err());
        assert!(release_url("https://example.com/a.zip").is_err());
        assert!(release_url("not a URL").is_err());
    }

    #[test]
    fn the_archive_is_searched_for_the_bundle_or_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("stage").join("sqlr.app");
        std::fs::create_dir_all(&bundle).unwrap();
        let found = find(dir.path(), UNPACK_DEPTH, &|name, is_file| {
            !is_file && name.ends_with(".app")
        })
        .unwrap();
        assert_eq!(found, bundle);

        let binary = dir.path().join("bin").join("sqlr");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
        let found = find(dir.path(), UNPACK_DEPTH, &|name, is_file| {
            is_file && name == "sqlr"
        })
        .unwrap();
        assert_eq!(found, binary);

        assert!(find(dir.path(), 1, &|name, _| name == "missing").is_err());
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;

    #[test]
    fn update_errors_keep_their_user_visible_messages() {
        assert_eq!(
            UpdateError::ChecksumMismatch {
                expected: "aa".into(),
                got: "bb".into(),
            }
            .to_string(),
            "the download does not match its checksum (expected aa, got bb)"
        );
        assert_eq!(
            UpdateError::NoChecksums {
                version: "1.2.3".into(),
            }
            .to_string(),
            "release 1.2.3 has no SHA256SUMS.txt"
        );
        assert_eq!(
            UpdateError::Failed {
                program: "tar".into(),
                reason: "bad archive".into(),
            }
            .to_string(),
            "tar failed: bad archive"
        );
        assert_eq!(
            UpdateError::OutsideGithub {
                url: "https://example.com".into(),
            }
            .to_string(),
            "the release points outside GitHub: https://example.com"
        );
        assert_eq!(UpdateError::Cancelled.to_string(), "cancelled");
        let parse = parse_latest(b"not json").unwrap_err().to_string();
        assert!(parse.starts_with("the release did not parse: "), "{parse}");
        let tag = parse_latest(br#"{"tag_name":"nightly","html_url":"u"}"#)
            .unwrap_err()
            .to_string();
        assert!(tag.starts_with("nightly is not a version: "), "{tag}");
    }
}
