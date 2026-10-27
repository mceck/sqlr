//! Local diagnostics: structured failure logs, incident files, the session marker and lock,
//! the bounded log ring, the panic hook, bundles and the Diagnostics window state.
//!
//! Nothing in this module performs a network request and nothing depends on the local SQLite
//! database, the vault or the configuration, so the worst failures remain reportable.

mod bundle;
mod hook;
mod issue;
mod model;
pub(crate) mod ring;
pub(crate) mod store;
pub mod ui;

use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Instant;

pub use bundle::{BUNDLE_BYTES, BundleContext, home_dir};
pub use hook::PanicHookGuard;
pub use issue::{IssueDraft, NEW_ISSUE, draft as issue_draft};
pub use model::{
    ApplicationInfo, BundleInputs, BundleOutline, ConnectionInfo, DriverInfo, Environment,
    Incident, IncidentKind, IncidentSummary, InterfaceInfo, OperationSnapshot, SessionSummary,
    StartupNotice, TabInfo,
};
pub use ring::{LogRing, RingSnapshot};
pub use store::{
    INCIDENT_DAYS, INCIDENT_FILES, SessionMarker, now_ms, timestamp_prefix, write_private_atomic,
};
pub use ui::{DiagnosticsUi, ReportProblemDialog};

use model::{INCIDENT_SCHEMA, Incident as IncidentRecord};

/// A failure that happened while the process was starting.
pub const STARTUP_TITLE: &str = "sqlr could not start";

/// The levels the Diagnostics window can select for the running session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    pub const ALL: [Self; 4] = [Self::Error, Self::Warn, Self::Info, Self::Debug];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "Errors only",
            Self::Warn => "Warnings",
            Self::Info => "Info",
            Self::Debug => "Debug",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == text)
    }
}

/// A session-only handle to the logging filter. `RUST_LOG` wins and disables the setter.
#[derive(Clone)]
pub struct LogLevelControl {
    set: Arc<dyn Fn(LogLevel) -> Result<(), String> + Send + Sync>,
    current: Arc<Mutex<LogLevel>>,
    pub env_controlled: bool,
}

impl LogLevelControl {
    pub fn new(
        current: Arc<Mutex<LogLevel>>,
        env_controlled: bool,
        set: impl Fn(LogLevel) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            set: Arc::new(set),
            current,
            env_controlled,
        }
    }

    pub fn current(&self) -> LogLevel {
        *self.current.lock()
    }

    pub fn set(&self, level: LogLevel) -> Result<(), String> {
        if self.env_controlled {
            return Err("RUST_LOG controls the log level for this launch".into());
        }
        (self.set)(level)
    }
}

/// One event to record. The tracing layer, the panic hook, startup handling and Report a
/// Problem all build this and let [`Diagnostics`] assign the id/time/environment.
#[derive(Clone, Debug, Default)]
pub struct IncidentRequest {
    pub kind: Option<IncidentKind>,
    pub message: String,
    pub target: Option<String>,
    pub thread: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub error_kind: Option<String>,
    pub fields: BTreeMap<String, serde_json::Value>,
    pub note: Option<String>,
}

impl IncidentRequest {
    pub fn new(kind: IncidentKind, message: impl Into<String>) -> Self {
        Self {
            kind: Some(kind),
            message: message.into(),
            ..Self::default()
        }
    }
}

enum WriterMsg {
    Write(Box<IncidentRecord>, PathBuf),
    Flush(Sender<()>),
    Work(Box<dyn FnOnce() + Send>),
    Stop,
}

struct Writer {
    tx: Sender<WriterMsg>,
    thread: JoinHandle<()>,
}

/// The one initialized diagnostics value the application holds.
pub struct Diagnostics {
    /// `None` when the data directory is unknown or not writable; diagnostics then stay
    /// in memory and on stderr.
    dir: Option<PathBuf>,
    session: SessionSummary,
    started: Instant,
    include_sql: AtomicBool,
    lock: Mutex<Option<store::SessionLock>>,
    writer: Mutex<Option<Writer>>,
    log_level: Mutex<Option<LogLevelControl>>,
    environment: Arc<OnceLock<Environment>>,
    graphics_backend: Mutex<Option<String>>,
    notice: Mutex<Option<StartupNotice>>,
    cleaned: AtomicBool,
    /// A panic was recorded for this session: a later Drop must keep the marker.
    panicked: AtomicBool,
}

fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn short_suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

impl Diagnostics {
    /// A diagnostics value for a session that has no data directory (tests, unusual
    /// launches): it keeps the session id and ring, but has nowhere to write.
    pub fn disabled() -> Self {
        let diagnostics = Self {
            dir: None,
            session: SessionSummary {
                id: new_session_id(),
                started_ms: store::now_ms(),
                uptime_s: 0,
                pid: std::process::id(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                data_dir: None,
                other_instance: false,
            },
            started: Instant::now(),
            include_sql: AtomicBool::new(true),
            lock: Mutex::new(None),
            writer: Mutex::new(None),
            log_level: Mutex::new(None),
            environment: Arc::new(OnceLock::new()),
            graphics_backend: Mutex::new(None),
            notice: Mutex::new(None),
            cleaned: AtomicBool::new(false),
            panicked: AtomicBool::new(false),
        };
        diagnostics.spawn_writer();
        diagnostics.spawn_environment();
        diagnostics
    }

    /// Opens the diagnostics directory under `data_dir`. Never fails: a directory that
    /// cannot be created leaves diagnostics in memory and on stderr.
    pub fn open(data_dir: Option<&Path>) -> Self {
        let mut diagnostics = Self::disabled();
        let Some(data_dir) = data_dir else {
            return diagnostics;
        };
        let dir = data_dir.join("diagnostics");
        if store::ensure_private_dir(&dir).is_err() {
            return diagnostics;
        }
        diagnostics.dir = Some(dir.clone());
        diagnostics.session.data_dir = Some(data_dir.display().to_string());
        store::prune(&dir);

        let lock = store::SessionLock::acquire(&dir);
        let other = lock.other_instance();
        diagnostics.session.other_instance = other;
        if !other && lock.holder() {
            let previous = lock.read_marker();
            let marker = SessionMarker {
                session_id: diagnostics.session.id.clone(),
                pid: diagnostics.session.pid,
                started_ms: diagnostics.session.started_ms,
                version: diagnostics.session.version.clone(),
            };
            let _ = lock.write_marker(&marker);
            if let Some(previous) = previous {
                let notice = previous_notice(&dir, &previous, diagnostics.session.id.clone());
                *diagnostics.notice.lock() = Some(notice);
            }
        }
        *diagnostics.lock.lock() = Some(lock);
        diagnostics
    }

    fn spawn_writer(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("sqlr-diagnostics".into())
            .spawn(move || writer_loop(&rx))
            .ok();
        if let Some(thread) = thread {
            *self.writer.lock() = Some(Writer { tx, thread });
        }
    }

    fn spawn_environment(&self) {
        let slot = self.environment.clone();
        std::thread::Builder::new()
            .name("sqlr-environment".into())
            .spawn(move || {
                let mut environment = Environment::basic();
                environment.os_version = os_version();
                environment.locale = locale();
                environment.resident_memory_mb = resident_memory_mb();
                let _ = slot.set(environment);
            })
            .ok();
    }

    pub fn enabled(&self) -> bool {
        self.dir.is_some()
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    pub fn data_dir(&self) -> Option<PathBuf> {
        self.dir
            .as_ref()
            .and_then(|d| d.parent())
            .map(Path::to_path_buf)
    }

    pub fn session_id(&self) -> &str {
        &self.session.id
    }

    pub fn session(&self) -> SessionSummary {
        let mut session = self.session.clone();
        session.uptime_s = self.started.elapsed().as_secs();
        session
    }

    pub fn other_instance(&self) -> bool {
        self.session.other_instance
    }

    pub fn environment(&self) -> Environment {
        let mut environment = self
            .environment
            .get()
            .cloned()
            .unwrap_or_else(Environment::basic);
        environment.graphics_backend = self.graphics_backend.lock().clone();
        environment
    }

    pub fn set_graphics_backend(&self, backend: impl Into<String>) {
        *self.graphics_backend.lock() = Some(backend.into());
    }

    pub fn include_sql(&self) -> bool {
        self.include_sql.load(Ordering::Relaxed)
    }

    pub fn set_include_sql(&self, include: bool) {
        self.include_sql.store(include, Ordering::Relaxed);
    }

    pub fn install_log_level_control(&self, control: LogLevelControl) {
        *self.log_level.lock() = Some(control);
    }

    pub fn log_level(&self) -> Option<LogLevel> {
        self.log_level.lock().as_ref().map(LogLevelControl::current)
    }

    pub fn log_level_env_controlled(&self) -> bool {
        self.log_level
            .lock()
            .as_ref()
            .is_some_and(|c| c.env_controlled)
    }

    pub fn set_log_level(&self, level: LogLevel) -> Result<(), String> {
        let control = self.log_level.lock().clone();
        match control {
            Some(control) => control.set(level),
            None => Err("logging is not initialized".into()),
        }
    }

    /// The notice for the previous session, once. The Startup banner takes it.
    pub fn take_startup_notice(&self) -> Option<StartupNotice> {
        self.notice.lock().take()
    }

    /// Applies the SQL setting to an incident before it is written: the same switch governs
    /// logs, incidents and bundles.
    fn sanitize_request(&self, mut request: IncidentRequest) -> IncidentRequest {
        if !self.include_sql() {
            request.fields.remove("sql");
        } else if let Some(sql) = request.fields.get_mut("sql")
            && let Some(text) = sql.as_str()
        {
            *sql = serde_json::Value::String(crate::logging::truncate_sql(text).0);
        }
        request
    }

    fn make_incident(&self, request: IncidentRequest) -> IncidentRecord {
        let request = self.sanitize_request(request);
        let at_ms = store::now_ms();
        let id = format!(
            "{}-{}-{}",
            store::timestamp_prefix(at_ms),
            std::process::id(),
            short_suffix()
        );
        let mut environment = self.environment();
        environment.graphics_backend = self.graphics_backend.lock().clone();
        let logs = ring::global()
            .snapshot()
            .excerpt()
            .into_iter()
            .map(|line| {
                if !self.include_sql()
                    || request.fields.get("include_sql") == Some(&serde_json::Value::Bool(false))
                {
                    bundle::without_sql_log(&line)
                } else {
                    line
                }
            })
            .collect();
        IncidentRecord {
            schema: INCIDENT_SCHEMA,
            id,
            session_id: self.session.id.clone(),
            at_ms,
            kind: request.kind.unwrap_or(IncidentKind::Error),
            message: request.message,
            target: request.target,
            thread: request.thread,
            file: request.file,
            line: request.line,
            error_kind: request.error_kind,
            fields: request.fields,
            note: request.note,
            acknowledged: false,
            environment,
            logs,
            backtrace: None,
        }
    }

    /// Records an incident on the caller's thread. Used by the panic hook, startup handling
    /// and manual reports; the tracing layer queues instead.
    pub fn record_now(&self, request: IncidentRequest) -> Option<IncidentRecord> {
        let incident = self.make_incident(request);
        let Some(dir) = &self.dir else {
            return None;
        };
        match store::write(dir, &incident) {
            Ok(_) => Some(incident),
            Err(e) => {
                eprintln!("sqlr: could not write the diagnostics report: {e}");
                None
            }
        }
    }

    /// Queues an incident for the writer thread, so the tracing layer never blocks.
    pub fn queue(&self, request: IncidentRequest) {
        let incident = self.make_incident(request);
        let Some(dir) = self.dir.clone() else {
            return;
        };
        let writer = self.writer.lock();
        if !writer.as_ref().is_some_and(|writer| {
            writer
                .tx
                .send(WriterMsg::Write(Box::new(incident), dir))
                .is_ok()
        }) {
            eprintln!("sqlr: the diagnostics writer is unavailable");
        }
    }

    /// Runs file and bundle work on the diagnostics writer, in order with incident writes.
    pub fn background(&self, work: impl FnOnce() + Send + 'static) -> bool {
        let writer = self.writer.lock();
        writer
            .as_ref()
            .is_some_and(|writer| writer.tx.send(WriterMsg::Work(Box::new(work))).is_ok())
    }

    /// Waits until every queued incident reached the disk.
    pub fn flush(&self) {
        let writer = self.writer.lock();
        let Some(writer) = writer.as_ref() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        if writer.tx.send(WriterMsg::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
        }
    }

    pub fn record_startup_failure(&self, message: impl Into<String>) -> Option<IncidentRecord> {
        self.record_now(IncidentRequest {
            kind: Some(IncidentKind::Startup),
            message: message.into(),
            target: Some("sqlr_app::startup".into()),
            thread: Some("main".into()),
            ..IncidentRequest::default()
        })
    }

    pub fn record_manual(
        &self,
        note: String,
        include_sql: bool,
        include_titles: bool,
    ) -> Option<IncidentRecord> {
        let mut fields = BTreeMap::new();
        fields.insert(
            "include_sql".into(),
            serde_json::Value::Bool(include_sql && self.include_sql()),
        );
        fields.insert(
            "include_titles".into(),
            serde_json::Value::Bool(include_titles),
        );
        self.record_now(IncidentRequest {
            kind: Some(IncidentKind::Manual),
            message: first_line(&note).unwrap_or_else(|| "Manual report".into()),
            target: Some("sqlr_app::report".into()),
            thread: Some(
                std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_string(),
            ),
            fields,
            note: Some(note),
            ..IncidentRequest::default()
        })
    }

    pub fn incidents(&self) -> Vec<IncidentSummary> {
        self.dir
            .as_deref()
            .map(store::summaries)
            .unwrap_or_default()
    }

    pub fn read_incident(&self, id: &str) -> Option<IncidentRecord> {
        store::read(self.dir.as_deref()?, id)
    }

    pub fn read_incident_json(&self, id: &str) -> Option<String> {
        store::read_json(self.dir.as_deref()?, id)
    }

    pub fn acknowledge(&self, id: &str) {
        if let Some(dir) = &self.dir {
            let _ = store::acknowledge(dir, id);
            {
                let mut notice = self.notice.lock();
                if notice.as_ref().is_some_and(|n| n.incident().id == id) {
                    *notice = None;
                }
            }
        }
    }

    pub fn prune(&self) {
        if let Some(dir) = &self.dir {
            store::prune(dir);
        }
    }

    /// Builds a bundle in memory. Called on a worker thread by the window and directly by
    /// tests; never on the interface thread for a large report.
    pub fn bundle(&self, inputs: &BundleInputs) -> (String, BundleOutline) {
        let mut inputs = inputs.clone();
        inputs.include_sql &= self.include_sql();
        if let Some(incident) = &inputs.incident {
            inputs.include_sql &=
                incident.fields.get("include_sql") != Some(&serde_json::Value::Bool(false));
            inputs.include_titles &=
                incident.fields.get("include_titles") != Some(&serde_json::Value::Bool(false));
        }
        let session = self.session();
        let mut environment = self.environment();
        environment.resident_memory_mb = resident_memory_mb();
        let ring = ring::global().snapshot();
        let recent = self.incidents();
        let data_dir = self.data_dir();
        let context = BundleContext {
            session: &session,
            environment: &environment,
            ring: &ring,
            recent: &recent,
            data_dir: data_dir.as_deref(),
            home: bundle::home_dir(),
        };
        bundle::render(&inputs, &context)
    }

    /// Writes a bundle to a user-chosen file, owner-only, without changing its directory's
    /// permissions.
    pub fn save_bundle(&self, path: &Path, text: &str) -> std::io::Result<()> {
        store::write_user_file(path, text)
    }

    /// Marks this session as one that recorded a crash; its marker is kept for the next
    /// launch even if the process unwinds through the application's destructor.
    pub(crate) fn note_panic(&self) {
        self.panicked.store(true, Ordering::Release);
    }

    /// Removes the session marker: the next launch is clean. A panic leaves it in place.
    pub fn clean_exit(&self) {
        if self.cleaned.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.panicked.load(Ordering::Acquire) {
            return;
        }
        let lock = self.lock.lock();
        if let Some(lock) = lock.as_ref() {
            let _ = lock.clear_marker();
        }
    }

    /// The opened incidents directory, for tests and the window.
    pub fn incidents_dir(&self) -> Option<PathBuf> {
        self.dir.clone()
    }
}

impl Drop for Diagnostics {
    fn drop(&mut self) {
        self.clean_exit();
        let writer = self.writer.get_mut();
        if let Some(writer) = writer.take() {
            let _ = writer.tx.send(WriterMsg::Stop);
            if writer.thread.thread().id() != std::thread::current().id() {
                let _ = writer.thread.join();
            }
        }
    }
}

fn writer_loop(rx: &Receiver<WriterMsg>) {
    while let Ok(message) = rx.recv() {
        match message {
            WriterMsg::Write(incident, dir) => {
                if let Err(e) = store::write(&dir, &incident) {
                    eprintln!("sqlr: could not write an incident: {e}");
                }
            }
            WriterMsg::Flush(done) => {
                let _ = done.send(());
            }
            WriterMsg::Work(work) => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            }
            WriterMsg::Stop => break,
        }
    }
}

fn previous_notice(dir: &Path, previous: &SessionMarker, current_session: String) -> StartupNotice {
    if let Some(incident) =
        store::find_unacknowledged(dir, IncidentKind::Panic, &previous.session_id)
    {
        let summary = incident.summary();
        let json = store::read_json(dir, &incident.id).unwrap_or_default();
        return StartupNotice::Panic {
            incident: summary,
            json,
        };
    }
    // No panic report: say only that the session did not close cleanly, and keep an
    // incident so Dismiss can be remembered like any other.
    let at_ms = store::now_ms();
    let incident = IncidentRecord {
        schema: INCIDENT_SCHEMA,
        id: format!(
            "{}-{}-{}",
            store::timestamp_prefix(at_ms),
            std::process::id(),
            short_suffix()
        ),
        session_id: previous.session_id.clone(),
        at_ms,
        kind: IncidentKind::UncleanShutdown,
        message: format!(
            "The previous session (pid {}) did not close cleanly and left no crash report.",
            previous.pid
        ),
        target: Some("sqlr_app::diagnostics".into()),
        thread: None,
        file: None,
        line: None,
        error_kind: None,
        fields: BTreeMap::from([
            (
                "previous_version".into(),
                serde_json::Value::String(previous.version.clone()),
            ),
            (
                "current_session".into(),
                serde_json::Value::String(current_session),
            ),
        ]),
        note: None,
        acknowledged: false,
        environment: Environment::basic(),
        logs: Vec::new(),
        backtrace: None,
    };
    let _ = store::write(dir, &incident);
    let summary = incident.summary();
    let json = store::read_json(dir, &incident.id).unwrap_or_default();
    StartupNotice::Unclean {
        incident: summary,
        json,
    }
}

fn first_line(text: &str) -> Option<String> {
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim();
    if line.is_empty() {
        None
    } else if line.chars().count() > 120 {
        Some(line.chars().take(120).collect::<String>() + "…")
    } else {
        Some(line.to_string())
    }
}

fn os_version() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!text.is_empty()).then_some(text)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let output = std::process::Command::new("uname")
            .arg("-sr")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!text.is_empty()).then_some(text)
    }
}

fn locale() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok().filter(|v| !v.is_empty()))
}

fn resident_memory_mb() -> Option<f64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kib: f64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;
    Some(kib / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostics(dir: &Path) -> Diagnostics {
        Diagnostics::open(Some(dir))
    }

    #[test]
    fn startup_failures_are_saved_as_incidents() {
        let dir = tempfile::tempdir().unwrap();
        let diag = diagnostics(dir.path());
        let incident = diag
            .record_startup_failure("the graphics backend could not initialize")
            .expect("written");
        assert_eq!(incident.kind, IncidentKind::Startup);
        assert_eq!(diag.incidents().len(), 1);
        let read = diag.read_incident(&incident.id).unwrap();
        assert_eq!(read.message, "the graphics backend could not initialize");
        assert_eq!(read.session_id, diag.session_id());
    }

    #[test]
    fn a_manual_report_is_listed_and_keeps_its_note() {
        let dir = tempfile::tempdir().unwrap();
        let diag = diagnostics(dir.path());
        let incident = diag
            .record_manual(
                "I was applying a migration\nand it stopped".into(),
                true,
                false,
            )
            .unwrap();
        assert_eq!(incident.kind, IncidentKind::Manual);
        assert_eq!(
            incident.note.as_deref(),
            Some("I was applying a migration\nand it stopped")
        );
        assert_eq!(
            incident.fields["include_titles"],
            serde_json::Value::Bool(false)
        );
        let listed = diag.incidents();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, incident.id);
    }

    #[test]
    fn a_clean_exit_leaves_no_notice_and_an_unclean_one_does() {
        let dir = tempfile::tempdir().unwrap();
        {
            let diag = diagnostics(dir.path());
            diag.clean_exit();
        }
        {
            let diag = diagnostics(dir.path());
            assert!(diag.take_startup_notice().is_none());
            diag.clean_exit();
        }

        // Simulate a killed process: the marker stays.
        let marker = SessionMarker {
            session_id: "dead-session".into(),
            pid: 999,
            started_ms: 1,
            version: env!("CARGO_PKG_VERSION").into(),
        };
        store::write_private_atomic(
            &dir.path().join("diagnostics").join("session.json"),
            &serde_json::to_string(&marker).unwrap(),
        )
        .unwrap();
        let diag = diagnostics(dir.path());
        let notice = diag.take_startup_notice().expect("unclean notice");
        assert!(!notice.is_panic());
        assert!(matches!(notice, StartupNotice::Unclean { .. }));
        assert_eq!(diag.incidents().len(), 1);
        assert_eq!(diag.incidents()[0].kind, IncidentKind::UncleanShutdown);
    }

    #[test]
    fn a_previous_panic_shows_its_report_and_dismiss_remembers() {
        let dir = tempfile::tempdir().unwrap();
        let diagnostics_dir = dir.path().join("diagnostics");
        store::ensure_private_dir(&diagnostics_dir).unwrap();
        let marker = SessionMarker {
            session_id: "crashed-session".into(),
            pid: 777,
            started_ms: 1,
            version: env!("CARGO_PKG_VERSION").into(),
        };
        store::write_private_atomic(
            &diagnostics_dir.join("session.json"),
            &serde_json::to_string(&marker).unwrap(),
        )
        .unwrap();
        let at = now_ms();
        let incident = IncidentRecord {
            schema: INCIDENT_SCHEMA,
            id: format!("{}-1-abc", timestamp_prefix(at)),
            session_id: "crashed-session".into(),
            at_ms: at,
            kind: IncidentKind::Panic,
            message: "assertion failed".into(),
            target: None,
            thread: Some("main".into()),
            file: Some("src/app.rs".into()),
            line: Some(12),
            error_kind: None,
            fields: BTreeMap::new(),
            note: None,
            acknowledged: false,
            environment: Environment::basic(),
            logs: vec!["before the crash".into()],
            backtrace: None,
        };
        store::write(&diagnostics_dir, &incident).unwrap();
        let diag = diagnostics(dir.path());
        let notice = diag.take_startup_notice().expect("panic notice");
        assert!(notice.is_panic());
        assert_eq!(notice.incident().id, incident.id);
        assert!(notice.json().contains("assertion failed"));
        diag.acknowledge(&incident.id);
        diag.clean_exit();
        drop(diag);
        let diag = diagnostics(dir.path());
        assert!(diag.take_startup_notice().is_none(), "dismissed");
    }

    #[test]
    fn a_live_second_instance_does_not_make_the_first_look_crashed() {
        let dir = tempfile::tempdir().unwrap();
        let first = diagnostics(dir.path());
        assert!(!first.other_instance());
        {
            let second = diagnostics(dir.path());
            assert!(second.other_instance());
            assert!(second.take_startup_notice().is_none());
            second.clean_exit();
        }
        // The first instance's marker survived the second instance's clean exit.
        let marker = std::fs::read_to_string(dir.path().join("diagnostics/session.json")).unwrap();
        assert!(marker.contains(first.session_id()));
        first.clean_exit();
        assert!(!dir.path().join("diagnostics/session.json").exists());
    }

    #[test]
    fn the_queue_writes_when_flushed() {
        let dir = tempfile::tempdir().unwrap();
        let diag = diagnostics(dir.path());
        diag.queue(IncidentRequest::new(IncidentKind::Error, "queued failure"));
        diag.flush();
        assert_eq!(diag.incidents().len(), 1);
        assert_eq!(diag.incidents()[0].message, "queued failure");
    }

    #[test]
    fn sql_in_an_incident_follows_the_setting() {
        let dir = tempfile::tempdir().unwrap();
        let diag = diagnostics(dir.path());
        let request = || {
            let mut request = IncidentRequest::new(IncidentKind::Error, "failed");
            request
                .fields
                .insert("sql".into(), serde_json::Value::String("select 1".into()));
            request
        };
        let with = diag.record_now(request()).unwrap();
        assert!(
            dir.path()
                .join("diagnostics")
                .join(format!("{}.json", with.id))
                .exists()
        );
        diag.set_include_sql(false);
        let without = diag.record_now(request()).unwrap();
        assert!(!without.fields.contains_key("sql"));
    }

    #[test]
    fn sql_opt_out_applies_to_old_logs_operation_labels_and_manual_reports() {
        let dir = tempfile::tempdir().unwrap();
        let diag = diagnostics(dir.path());
        ring::global().push("INFO query failed conn=c1 sql=select private_sentinel from t");
        let manual = diag
            .record_manual("reproduce this".into(), false, false)
            .unwrap();
        assert!(
            !serde_json::to_string(&manual)
                .unwrap()
                .contains("private_sentinel")
        );
        diag.set_include_sql(false);
        let inputs = BundleInputs {
            include_sql: true,
            operations: vec![OperationSnapshot {
                kind: "Query".into(),
                connection: None,
                label: "select private_sentinel from t".into(),
                duration_ms: 1,
                outcome: "failed".into(),
                error: None,
            }],
            ..Default::default()
        };
        let (text, _) = diag.bundle(&inputs);
        assert!(!text.contains("private_sentinel"), "{text}");
    }

    #[test]
    fn incident_sql_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let diag = diagnostics(dir.path());
        let mut request = IncidentRequest::new(IncidentKind::Error, "failed");
        request
            .fields
            .insert("sql".into(), serde_json::json!("é".repeat(5000)));
        let incident = diag.record_now(request).unwrap();
        assert!(incident.fields["sql"].as_str().unwrap().len() < 4200);
    }

    #[test]
    fn bundles_use_the_session_and_the_recent_incidents() {
        let dir = tempfile::tempdir().unwrap();
        let diag = diagnostics(dir.path());
        diag.record_manual("something broke".into(), true, true);
        let inputs = BundleInputs::default();
        let (text, outline) = diag.bundle(&inputs);
        assert!(text.contains(diag.session_id()));
        assert!(text.contains("something broke"));
        assert!(outline.incidents >= 1);
    }
}
