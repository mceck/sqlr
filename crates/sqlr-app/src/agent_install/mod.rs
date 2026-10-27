//! Agent installations: sqlr's MCP server entry written into each supported agent's
//! user-level configuration (Settings → Agents).
//!
//! [`Installations`] is the synchronous core: discovery, state, install, remove and the
//! rewrite of recorded installations. It reads and writes files, so the app only reaches it
//! through [`AgentInstaller`], a background worker that publishes a snapshot the UI thread
//! reads and accepts commands the UI thread sends.
//!
//! The installation registry (`agent-installations.json` in the data directory) records
//! each Agent installation by agent and file and never holds the token.

mod app;
mod edit;
mod files;
mod worker;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) use app::{AgentInstallUi, AgentRequest, AgentsView};
use edit::Entry;
pub use worker::{AgentInstaller, InstallCommand, InstallOutcome, InstallSnapshot};

/// The registry's file name in the data directory.
pub const REGISTRY_FILE: &str = "agent-installations.json";

/// A supported agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    ClaudeCode,
    CodexCli,
    GeminiCli,
    Cursor,
    VsCode,
    OpenCode,
}

impl AgentKind {
    pub const ALL: [Self; 6] = [
        Self::ClaudeCode,
        Self::CodexCli,
        Self::GeminiCli,
        Self::Cursor,
        Self::VsCode,
        Self::OpenCode,
    ];

    /// The name a person recognizes.
    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::CodexCli => "Codex CLI",
            Self::GeminiCli => "Gemini CLI",
            Self::Cursor => "Cursor",
            Self::VsCode => "VS Code",
            Self::OpenCode => "OpenCode",
        }
    }

    /// The user-level configuration file sqlr writes.
    pub fn config_file(self, env: &AgentEnvironment) -> PathBuf {
        let home = &env.home;
        match self {
            Self::ClaudeCode => home.join(".claude.json"),
            Self::CodexCli => home.join(".codex").join("config.toml"),
            Self::GeminiCli => home.join(".gemini").join("settings.json"),
            Self::Cursor => home.join(".cursor").join("mcp.json"),
            Self::VsCode => env.config_dir.join("Code").join("User").join("mcp.json"),
            Self::OpenCode => home.join(".config").join("opencode").join("opencode.json"),
        }
    }

    /// Paths whose presence means the agent has been set up on this machine.
    fn config_markers(self, env: &AgentEnvironment) -> Vec<PathBuf> {
        let home = &env.home;
        match self {
            Self::ClaudeCode => vec![home.join(".claude"), home.join(".claude.json")],
            Self::CodexCli => vec![home.join(".codex")],
            Self::GeminiCli => vec![home.join(".gemini")],
            Self::Cursor => vec![home.join(".cursor")],
            Self::VsCode => vec![env.config_dir.join("Code").join("User")],
            Self::OpenCode => vec![home.join(".config").join("opencode")],
        }
    }

    /// The executable's name.
    fn executable(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::CodexCli => "codex",
            Self::GeminiCli => "gemini",
            Self::Cursor => "cursor",
            Self::VsCode => "code",
            Self::OpenCode => "opencode",
        }
    }

    /// Where the agent's own installer puts it inside the home.
    fn own_bin_dirs(self, env: &AgentEnvironment) -> Vec<PathBuf> {
        match self {
            Self::ClaudeCode => vec![env.home.join(".claude").join("local")],
            Self::OpenCode => vec![env.home.join(".opencode").join("bin")],
            _ => Vec::new(),
        }
    }

    /// Desktop application bundles or folders, looked up in the application directories.
    fn applications(self) -> &'static [&'static str] {
        match self {
            Self::Cursor => &["Cursor.app", "cursor", "Cursor"],
            Self::VsCode => &["Visual Studio Code.app", "Microsoft VS Code"],
            _ => &[],
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::CodexCli => "codex-cli",
            Self::GeminiCli => "gemini-cli",
            Self::Cursor => "cursor",
            Self::VsCode => "vs-code",
            Self::OpenCode => "opencode",
        }
    }

    /// The configuration a person pastes by hand, in this agent's format.
    pub fn config_snippet(self, url: &str, token: &str) -> String {
        edit::snippet(self.format(), url, token)
    }
}

/// The directories discovery and installation depend on. The app uses
/// [`AgentEnvironment::from_os`]; tests point everything at a temporary home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEnvironment {
    /// The user's home directory.
    pub home: PathBuf,
    /// The platform configuration directory (`~/Library/Application Support`, `~/.config`,
    /// `%APPDATA%`), where VS Code keeps its user directory.
    pub config_dir: PathBuf,
    /// Directories outside the home that hold executables (`/usr/local/bin`, …).
    pub bin_dirs: Vec<PathBuf>,
    /// Directories that hold desktop applications (`/Applications`, …).
    pub app_dirs: Vec<PathBuf>,
}

impl AgentEnvironment {
    /// The current user's directories, with no file access.
    pub fn from_os() -> Option<Self> {
        let base = directories::BaseDirs::new()?;
        let home = base.home_dir().to_path_buf();
        let mut env = Self::in_home(&home);
        env.config_dir = base.config_dir().to_path_buf();
        if cfg!(windows) {
            env.bin_dirs = vec![base.config_dir().join("npm")];
            env.app_dirs = vec![base.data_local_dir().join("Programs")];
        } else {
            env.bin_dirs = [
                "/usr/local/bin",
                "/opt/homebrew/bin",
                "/usr/bin",
                "/snap/bin",
            ]
            .iter()
            .map(PathBuf::from)
            .collect();
            if cfg!(target_os = "macos") {
                env.app_dirs = vec![PathBuf::from("/Applications"), home.join("Applications")];
            }
        }
        Some(env)
    }

    /// Everything under `home`: the platform configuration directory at its default place
    /// inside it, and no system directories.
    pub fn in_home(home: &Path) -> Self {
        let config_dir = if cfg!(target_os = "macos") {
            home.join("Library").join("Application Support")
        } else if cfg!(windows) {
            home.join("AppData").join("Roaming")
        } else {
            home.join(".config")
        };
        Self {
            home: home.to_path_buf(),
            config_dir,
            bin_dirs: Vec::new(),
            app_dirs: Vec::new(),
        }
    }

    /// Whether the agent's configuration or installation is present. No process runs and
    /// `PATH` is not read: an app started from the desktop has a minimal one.
    pub fn detects(&self, kind: AgentKind) -> bool {
        if kind
            .config_markers(self)
            .iter()
            .any(|path| path.symlink_metadata().is_ok())
        {
            return true;
        }
        let home_bins = [
            self.home.join(".local").join("bin"),
            self.home.join(".npm-global").join("bin"),
            self.home.join(".bun").join("bin"),
            self.home.join(".volta").join("bin"),
            self.home.join("bin"),
        ];
        let executable = kind.executable();
        let names: Vec<String> = if cfg!(windows) {
            [".exe", ".cmd"]
                .iter()
                .map(|ext| format!("{executable}{ext}"))
                .collect()
        } else {
            vec![executable.to_string()]
        };
        let found_executable = kind
            .own_bin_dirs(self)
            .iter()
            .chain(&home_bins)
            .chain(&self.bin_dirs)
            .any(|dir| names.iter().any(|name| dir.join(name).is_file()));
        found_executable
            || self
                .app_dirs
                .iter()
                .any(|dir| kind.applications().iter().any(|app| dir.join(app).exists()))
    }
}

/// The MCP server an installation points at. `Debug` never shows the token.
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub url: String,
    token: Option<String>,
}

impl Endpoint {
    /// The running server's URL and token.
    pub fn running(url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: Some(token.into()),
        }
    }

    /// The URL the server would have on `port`, while it is not running.
    pub fn stopped(port: u16) -> Self {
        Self {
            url: format!("http://127.0.0.1:{port}/mcp"),
            token: None,
        }
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    pub fn is_running(&self) -> bool {
        self.token.is_some()
    }
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("url", &self.url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// An agent's installation state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentState {
    /// Neither its configuration nor its installation is present.
    NotDetected,
    /// Detected, with no `sqlr` entry.
    NotInstalled,
    /// The `sqlr` entry matches the running server's URL and token.
    Installed,
    /// A `sqlr` entry exists and differs from the running server.
    Outdated,
    /// The file exists and sqlr cannot read its format; the reason never quotes the file.
    ManualSetup(String),
}

impl AgentState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotDetected => "Not detected",
            Self::NotInstalled => "Not installed",
            Self::Installed => "Installed",
            Self::Outdated => "Outdated",
            Self::ManualSetup(_) => "Manual setup",
        }
    }
}

/// One row of Settings → Agents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRow {
    pub kind: AgentKind,
    pub file: PathBuf,
    /// The file as Settings shows it, with the home as `~`.
    pub display_file: String,
    pub state: AgentState,
    /// sqlr has written this file before (it is in the registry), so a write needs no
    /// new confirmation.
    pub recorded: bool,
}

/// Why an install, update or remove failed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The configuration exists and cannot be read as its format.
    #[error("{0}")]
    Format(String),
    /// A file could not be read or written.
    #[error("{path}: {source}", path = .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Install and Update write the running server's token.
    #[error("the MCP server is not running")]
    NotRunning,
    /// The installation registry could not be written as JSON.
    #[error("the installation registry cannot be saved: {0}")]
    Registry(#[source] serde_json::Error),
}

/// One Agent installation in the registry: no token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub agent: AgentKind,
    pub file: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    installations: Vec<Record>,
}

/// Discovery, state, install, remove and rewrite, against one environment and registry.
#[derive(Clone, Debug)]
pub struct Installations {
    env: AgentEnvironment,
    registry: PathBuf,
}

impl Installations {
    /// `data_dir` holds the installation registry.
    pub fn new(env: AgentEnvironment, data_dir: &Path) -> Self {
        Self {
            env,
            registry: data_dir.join(REGISTRY_FILE),
        }
    }

    /// Every supported agent with its state against `endpoint`.
    pub fn rows(&self, endpoint: &Endpoint) -> Vec<AgentRow> {
        let records = self.records();
        AgentKind::ALL
            .iter()
            .map(|&kind| {
                let file = kind.config_file(&self.env);
                let state = self.state(kind, &file, endpoint);
                let recorded = records
                    .iter()
                    .any(|record| record.agent == kind && record.file == file);
                let display_file = match file.strip_prefix(&self.env.home) {
                    Ok(relative) => Path::new("~").join(relative).display().to_string(),
                    Err(_) => file.display().to_string(),
                };
                AgentRow {
                    kind,
                    file,
                    display_file,
                    state,
                    recorded,
                }
            })
            .collect()
    }

    fn state(&self, kind: AgentKind, file: &Path, endpoint: &Endpoint) -> AgentState {
        let text = match files::read(file) {
            Ok(Some(text)) => text,
            Ok(None) if self.env.detects(kind) => return AgentState::NotInstalled,
            Ok(None) => return AgentState::NotDetected,
            Err(error) => return AgentState::ManualSetup(format!("cannot read the file: {error}")),
        };
        match edit::read_entry(kind.format(), &text, endpoint) {
            Ok(Entry::Absent) => AgentState::NotInstalled,
            Ok(Entry::Present { current: true }) => AgentState::Installed,
            Ok(Entry::Present { current: false }) => AgentState::Outdated,
            Err(reason) => AgentState::ManualSetup(reason),
        }
    }

    /// Writes sqlr's entry for the running server and records the installation. Install
    /// and Update are the same write.
    pub fn install(&self, kind: AgentKind, endpoint: &Endpoint) -> Result<(), InstallError> {
        let token = endpoint.token().ok_or(InstallError::NotRunning)?;
        let file = kind.config_file(&self.env);
        self.write_entry(kind, &file, &endpoint.url, token)?;
        let mut registry = self.load();
        if !registry
            .installations
            .iter()
            .any(|record| record.agent == kind && record.file == file)
        {
            registry.installations.push(Record { agent: kind, file });
        }
        self.store(&registry)
    }

    fn write_entry(
        &self,
        kind: AgentKind,
        file: &Path,
        url: &str,
        token: &str,
    ) -> Result<(), InstallError> {
        let existing = files::read(file).map_err(|source| io(file, source))?;
        let text = edit::install(kind.format(), existing.as_deref().unwrap_or(""), url, token)
            .map_err(InstallError::Format)?;
        if existing.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        if existing.is_some() {
            files::backup_once(file).map_err(|source| io(file, source))?;
        }
        files::write_atomic(file, &text).map_err(|source| io(file, source))
    }

    /// Deletes only sqlr's entry and drops the record.
    pub fn remove(&self, kind: AgentKind) -> Result<(), InstallError> {
        let file = kind.config_file(&self.env);
        if let Some(existing) = files::read(&file).map_err(|source| io(&file, source))?
            && let Some(text) =
                edit::remove(kind.format(), &existing).map_err(InstallError::Format)?
        {
            files::backup_once(&file).map_err(|source| io(&file, source))?;
            files::write_atomic(&file, &text).map_err(|source| io(&file, source))?;
        }
        let mut registry = self.load();
        let before = registry.installations.len();
        registry.installations.retain(|record| record.agent != kind);
        if registry.installations.len() == before {
            return Ok(());
        }
        self.store(&registry)
    }

    /// Rewrites every recorded installation whose entry differs from `endpoint`, after a
    /// token rotation or a port change. A record whose entry disappeared is dropped. The
    /// failures are returned, for the caller to log with the agent id and the error (the
    /// error never holds the token).
    pub fn rewrite_recorded(&self, endpoint: &Endpoint) -> Vec<(AgentKind, InstallError)> {
        let Some(token) = endpoint.token() else {
            return Vec::new();
        };
        let mut registry = self.load();
        let mut failures = Vec::new();
        let mut keep = Vec::new();
        let mut changed = false;
        for record in std::mem::take(&mut registry.installations) {
            let kind = record.agent;
            let result = files::read(&record.file)
                .map_err(|source| io(&record.file, source))
                .and_then(|text| match text {
                    None => Ok(Entry::Absent),
                    Some(text) => edit::read_entry(kind.format(), &text, endpoint)
                        .map_err(InstallError::Format),
                });
            let outcome = match result {
                Ok(Entry::Absent) => {
                    tracing::info!(
                        agent = kind.id(),
                        "the agent's sqlr entry is gone; its installation record is dropped"
                    );
                    changed = true;
                    continue;
                }
                Ok(Entry::Present { current: true }) => Ok(()),
                Ok(Entry::Present { current: false }) => {
                    self.write_entry(kind, &record.file, &endpoint.url, token)
                }
                Err(error) => Err(error),
            };
            match outcome {
                Ok(()) => tracing::debug!(agent = kind.id(), "agent installation is current"),
                Err(error) => failures.push((kind, error)),
            }
            keep.push(record);
        }
        registry.installations = keep;
        if changed && let Err(error) = self.store(&registry) {
            tracing::warn!(error = %error, "the agent installation registry could not be saved");
        }
        failures
    }

    /// The recorded installations.
    pub fn records(&self) -> Vec<Record> {
        self.load().installations
    }

    fn load(&self) -> Registry {
        match files::read(&self.registry) {
            Ok(None) => Registry::default(),
            Ok(Some(text)) => serde_json::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!(
                    error = %error,
                    "the agent installation registry is unreadable; starting empty"
                );
                Registry::default()
            }),
            Err(error) => {
                tracing::warn!(error = %error, "the agent installation registry cannot be read");
                Registry::default()
            }
        }
    }

    fn store(&self, registry: &Registry) -> Result<(), InstallError> {
        let text = serde_json::to_string_pretty(registry).map_err(InstallError::Registry)?;
        files::write_atomic(&self.registry, &format!("{text}\n"))
            .map_err(|source| io(&self.registry, source))
    }
}

fn io(path: &Path, source: std::io::Error) -> InstallError {
    InstallError::Io {
        path: path.to_path_buf(),
        source,
    }
}
