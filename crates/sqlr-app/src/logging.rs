//! Logging: stderr plus a daily rotating, owner-only file, an in-memory ring and the
//! `error!`-means-incident layer.
//!
//! The session identifier is a field on every stderr and file line. Secrets and server
//! values are never logged.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt, reload};

use crate::diagnostics::{
    Diagnostics, IncidentKind, IncidentRequest, LogLevel, LogLevelControl, ring,
};

/// Log files kept: today and the thirteen before it.
pub const LOG_FILES: usize = 14;
/// Failed SQL is recorded up to this many UTF-8 bytes.
pub const SQL_BYTES: usize = 4 * 1024;

/// What `init` returns: keep it alive for the process's lifetime.
pub struct Logging {
    /// Flushes the file writer when dropped.
    pub guard: Option<WorkerGuard>,
    /// Why the file is unusable, when it is: the app shows a persistent warning.
    pub file_error: Option<String>,
    /// The session-only level, also stored on the Diagnostics value.
    pub control: LogLevelControl,
}

impl Logging {
    /// The warning the status bar shows when diagnostics are limited to stderr.
    pub fn warning(&self) -> Option<String> {
        self.file_error.as_ref().map(|error| {
            format!(
                "Diagnostics are limited to stderr: the log directory is not writable ({error})"
            )
        })
    }
}

/// Installs the global subscriber. A second call keeps the first subscriber.
pub fn init(
    log_dir: Option<&Path>,
    session_id: &str,
    diagnostics: Option<Arc<Diagnostics>>,
) -> Logging {
    let (initial, env_controlled) = match EnvFilter::try_from_default_env() {
        Ok(filter) => (filter, true),
        Err(_) => (EnvFilter::new("info,rmcp=warn"), false),
    };
    let current = Arc::new(Mutex::new(LogLevel::Info));
    let (filter, reload_handle) = reload::Layer::new(initial);
    let set_current = current.clone();
    let control = LogLevelControl::new(current, env_controlled, move |level| {
        let filter = EnvFilter::new(format!("{},rmcp=warn", level.as_str()));
        reload_handle
            .reload(filter)
            .map_err(|e| format!("could not change the log level: {e}"))?;
        *set_current.lock().unwrap_or_else(|e| e.into_inner()) = level;
        Ok(())
    });
    if let Some(diagnostics) = &diagnostics {
        diagnostics.install_log_level_control(control.clone());
    }

    let stderr = fmt::layer()
        .with_writer(std::io::stderr)
        .event_format(SessionFormat {
            session: session_id.to_string(),
            diagnostics: diagnostics.clone(),
        });
    #[cfg(test)]
    let stderr = stderr.with_test_writer();
    let (file, guard, file_error) = match log_dir {
        Some(dir) => match DailyWriter::open(dir) {
            Ok((writer, guard)) => {
                let layer = fmt::layer()
                    .with_ansi(false)
                    .with_writer(writer)
                    .event_format(SessionFormat {
                        session: session_id.to_string(),
                        diagnostics: diagnostics.clone(),
                    });
                (Some(layer), Some(guard), None)
            }
            Err(error) => (None, None, Some(error)),
        },
        None => (None, None, None),
    };
    let capture = CaptureLayer {
        diagnostics: diagnostics.clone(),
    };
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(stderr)
        .with(file)
        .with(capture)
        .try_init();
    if let Some(error) = &file_error {
        tracing::warn!(
            error = %error,
            "cannot log to file; diagnostics are limited to stderr"
        );
    }
    Logging {
        guard,
        file_error,
        control,
    }
}

/// Prefixes each line with the session id, then delegates to tracing's standard format.
struct SessionFormat {
    session: String,
    diagnostics: Option<Arc<Diagnostics>>,
}

impl<S, N> FormatEvent<S, N> for SessionFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let mut inherited = BTreeMap::new();
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                if let Some(fields) = span.extensions().get::<SpanFields>() {
                    inherited.extend(fields.0.clone());
                }
            }
        }
        let include_sql = self.diagnostics.as_ref().is_none_or(|d| d.include_sql());
        let captured = Captured::new(event, include_sql, inherited);
        writeln!(writer, "session_id={} {}", self.session, captured.line())
    }
}

/// A daily file writer that opens each file owner-only and prunes to [`LOG_FILES`].
impl DailyWriter {
    fn open(
        dir: &Path,
    ) -> Result<(tracing_appender::non_blocking::NonBlocking, WorkerGuard), String> {
        crate::diagnostics::store::ensure_private_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let writer = DailyWriter {
            dir: dir.to_path_buf(),
            state: Mutex::new(DailyState::default()),
        };
        // Touch today's file now, so an unwritable directory is reported at startup rather
        // than at the first log line.
        writer
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ensure(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let (writer, guard) = tracing_appender::non_blocking(writer);
        Ok((writer, guard))
    }
}

#[derive(Default)]
struct DailyState {
    day: String,
    file: Option<File>,
}

impl DailyState {
    fn ensure(&mut self, dir: &Path) -> std::io::Result<()> {
        let day = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
        if self.file.is_none() || self.day != day {
            let path = dir.join(format!("sqlr.log.{day}"));
            let mut options = OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = file.metadata()?.permissions();
                perms.set_mode(0o600);
                std::fs::set_permissions(&path, perms)?;
            }
            self.file = Some(file);
            self.day = day;
            prune_logs(dir);
        }
        Ok(())
    }
}

struct DailyWriter {
    dir: PathBuf,
    state: Mutex<DailyState>,
}

impl Write for DailyWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.ensure(&self.dir)?;
        state.file.as_mut().expect("opened by ensure").write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(file) = state.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

fn prune_logs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            name.strip_prefix("sqlr.log.")
                .is_some_and(|day| day.len() == 10)
                .then_some((name, entry.path()))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in files.into_iter().skip(LOG_FILES) {
        let _ = std::fs::remove_file(path);
    }
}

/// The `error!`-means-incident ingestion layer, and the ring writer.
struct CaptureLayer {
    diagnostics: Option<Arc<Diagnostics>>,
}

#[derive(Default)]
struct SpanFields(BTreeMap<String, serde_json::Value>);

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanFields(visitor.fields));
        }
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor::default();
        values.record(&mut visitor);
        if let Some(span) = ctx.span(id)
            && let Some(fields) = span.extensions_mut().get_mut::<SpanFields>()
        {
            fields.0.extend(visitor.fields);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let include_sql = self
            .diagnostics
            .as_ref()
            .is_none_or(|diagnostics| diagnostics.include_sql());
        let mut inherited = BTreeMap::new();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(fields) = span.extensions().get::<SpanFields>() {
                    inherited.extend(fields.0.clone());
                }
            }
        }
        let captured = Captured::new(event, include_sql, inherited);
        ring::global().push(captured.line());
        if *event.metadata().level() != Level::ERROR {
            return;
        }
        let Some(diagnostics) = &self.diagnostics else {
            return;
        };
        diagnostics.queue(captured.request(event.metadata()));
    }
}

struct Captured {
    message: String,
    fields: BTreeMap<String, serde_json::Value>,
    line: String,
}

impl Captured {
    fn new(
        event: &Event<'_>,
        include_sql: bool,
        mut fields: BTreeMap<String, serde_json::Value>,
    ) -> Self {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let message = visitor.message.unwrap_or_default();
        fields.extend(visitor.fields);
        if !include_sql {
            fields.remove("sql");
        } else if let Some(sql) = fields.get("sql").and_then(serde_json::Value::as_str) {
            fields.insert("sql".into(), serde_json::Value::String(truncate_sql(sql).0));
        }
        let meta = event.metadata();
        let mut line = format!(
            "{} {:<5} {}: {}",
            jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ"),
            meta.level().as_str(),
            meta.target(),
            message.replace('\n', "\\n").replace('\r', "\\r")
        );
        for (key, value) in fields.iter().filter(|(key, _)| key.as_str() != "sql") {
            let value = match value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            line.push(' ');
            line.push_str(key);
            line.push('=');
            line.push_str(&value.replace(['\n', '\r'], " "));
        }
        // SQL is always last so a later report can omit SQL from historical log lines.
        if let Some(sql) = fields.get("sql").and_then(serde_json::Value::as_str) {
            line.push_str(" sql=");
            line.push_str(&serde_json::to_string(sql).unwrap_or_default());
        }
        Self {
            message,
            fields,
            line,
        }
    }

    fn line(&self) -> String {
        self.line.clone()
    }

    fn request(&self, metadata: &tracing::Metadata<'_>) -> IncidentRequest {
        let mut fields = self.fields.clone();
        let kind = IncidentKind::from_event(
            fields
                .remove("incident")
                .as_ref()
                .and_then(serde_json::Value::as_str),
        );
        let thread = fields
            .remove("thread")
            .and_then(|v| match v {
                serde_json::Value::String(text) => Some(text),
                _ => None,
            })
            .or_else(|| std::thread::current().name().map(str::to_string))
            .unwrap_or_else(|| "unnamed".into());
        let note = fields.remove("note").and_then(|v| match v {
            serde_json::Value::String(text) => Some(text),
            _ => None,
        });
        let error_kind = fields
            .get("error_kind")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        IncidentRequest {
            kind: Some(kind),
            message: self.message.clone(),
            target: Some(metadata.target().to_string()),
            thread: Some(thread),
            file: metadata.file().map(str::to_string),
            line: metadata.line(),
            error_kind,
            fields,
            note,
        }
    }
}

#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    fields: BTreeMap<String, serde_json::Value>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let text = format!("{value:?}");
        if field.name() == "message" {
            self.message = Some(text);
        } else {
            self.fields
                .insert(field.name().to_string(), serde_json::Value::String(text));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields.insert(
                field.name().to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::Bool(value));
    }
}

/// SQL as typed, capped at [`SQL_BYTES`] on a character boundary, with a marker.
pub fn truncate_sql(sql: &str) -> (String, bool) {
    if sql.len() <= SQL_BYTES {
        return (sql.to_string(), false);
    }
    let mut cut = SQL_BYTES;
    while cut > 0 && !sql.is_char_boundary(cut) {
        cut -= 1;
    }
    (format!("{}… (truncated)", &sql[..cut]), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::filter::LevelFilter;

    fn subscriber(diagnostics: Arc<Diagnostics>) -> impl Subscriber {
        tracing_subscriber::registry()
            .with(LevelFilter::INFO)
            .with(CaptureLayer {
                diagnostics: Some(diagnostics),
            })
    }

    #[test]
    fn error_is_an_incident_and_info_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let diagnostics = Arc::new(Diagnostics::open(Some(dir.path())));
        let subscriber = subscriber(diagnostics.clone());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "sqlr_engine::operation",
                op = "query",
                conn = "c1",
                error_kind = "Syntax",
                "query failed"
            );
            tracing::error!(
                target: "sqlr_app::test",
                incident = "sidecar_crash",
                driver = "hana",
                "driver exited"
            );
        });
        diagnostics.flush();
        let incidents = diagnostics.incidents();
        assert_eq!(incidents.len(), 1, "{incidents:?}");
        let incident = diagnostics.read_incident(&incidents[0].id).unwrap();
        assert_eq!(incident.kind, IncidentKind::SidecarCrash);
        assert_eq!(incident.message, "driver exited");
        assert_eq!(incident.fields["driver"], "hana");
        assert_eq!(incident.target.as_deref(), Some("sqlr_app::test"));
        diagnostics.clean_exit();
    }

    #[test]
    fn sql_follows_the_setting_in_incidents_and_the_ring() {
        let dir = tempfile::tempdir().unwrap();
        let diagnostics = Arc::new(Diagnostics::open(Some(dir.path())));
        let subscriber = subscriber(diagnostics.clone());
        diagnostics.set_include_sql(false);
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(sql = "select sentinel from t", "query failed");
        });
        diagnostics.flush();
        let incident = diagnostics
            .read_incident(&diagnostics.incidents()[0].id)
            .unwrap();
        assert!(!incident.fields.contains_key("sql"));
        assert!(
            !ring::global()
                .snapshot()
                .lines
                .iter()
                .any(|line| line.contains("sentinel"))
        );
        diagnostics.clean_exit();
    }

    #[test]
    fn incidents_inherit_operation_span_fields() {
        let dir = tempfile::tempdir().unwrap();
        let diagnostics = Arc::new(Diagnostics::open(Some(dir.path())));
        tracing::subscriber::with_default(subscriber(diagnostics.clone()), || {
            let span = tracing::info_span!(
                "operation",
                conn = "c1",
                driver = "fake",
                operation_id = 17_u64
            );
            let _entered = span.enter();
            tracing::error!("background operation failed");
        });
        diagnostics.flush();
        let incident = diagnostics
            .read_incident(&diagnostics.incidents()[0].id)
            .unwrap();
        assert_eq!(incident.fields["conn"], "c1");
        assert_eq!(incident.fields["operation_id"], 17);
    }

    #[test]
    fn failed_sql_is_truncated_at_a_character_boundary() {
        let sql = "é".repeat(SQL_BYTES);
        let (text, truncated) = truncate_sql(&sql);
        assert!(truncated);
        assert!(text.ends_with("… (truncated)"));
        assert!(text.is_char_boundary(text.len() - "… (truncated)".len()));
        assert!(text.len() <= SQL_BYTES + "… (truncated)".len());
        assert!(!truncate_sql("select 1").1);
    }

    #[test]
    fn log_files_are_pruned_to_fourteen() {
        let dir = tempfile::tempdir().unwrap();
        for day in 1..=20 {
            let name = format!("sqlr.log.2026-01-{day:02}");
            std::fs::write(dir.path().join(name), "line\n").unwrap();
        }
        std::fs::write(dir.path().join("unrelated.txt"), "keep").unwrap();
        prune_logs(dir.path());
        let mut logs: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| n.starts_with("sqlr.log."))
            .collect();
        logs.sort();
        assert_eq!(logs.len(), LOG_FILES);
        assert!(logs.last().unwrap().ends_with("2026-01-20"));
        assert!(dir.path().join("unrelated.txt").exists());
    }

    #[test]
    fn an_unusable_log_dir_reports_a_warning() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let dir = file.path().join("logs");
        let logging = init(Some(&dir), "test-session", None);
        assert!(logging.guard.is_none());
        assert!(logging.file_error.is_some());
        assert!(logging.warning().unwrap().contains("stderr"));
    }
}
