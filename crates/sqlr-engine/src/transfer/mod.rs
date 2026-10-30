//! Export, import, file preview and clipboard jobs (§8.3, Decisions "Export" and
//! "Import"). Files are read and written here, never on the UI thread; an export writes a
//! temporary file next to the target and renames it only once it is complete.
//!
//! Transfers between NoSQL endpoints (`Engine::transfer`) reuse these jobs:
//! `pipeline` routes a source to a sink, `read_docs` and `read_keys` read collections and
//! Redis keys, `write_docs` inserts into collections; tables and files are written by the
//! import and export code below.

mod clipboard;
mod convert;
mod encoding;
mod error;
mod files;
mod parquet;
mod pipeline;
mod read_docs;
mod read_keys;
mod write_docs;
mod write_table;
mod xlsx;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqlr_core::{
    Cell, ColumnMeta, Connection, Dialect, DriverError, Encoder, ErrorKind, Execution, RowStream,
    StatementClass, TableInfo, TableRef, TextFormat, Transform, Value, read_rows,
};

use sqlr_core::ColumnMapping;
pub use sqlr_core::ImportOptions;

use self::convert::{apply_transform, utc_naive};
use self::error::{ConvertError, MappingError, TransferFileError};
use self::write_table::RejectedFile;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::engine::{Shared, not_connected};
use crate::job::{JobControl, JobOutcome, JobProgress};
use crate::result::QueryId;
use crate::session::{SessionIo, SessionKey};
use crate::tx::{READ_ONLY, run_sql};

/// Rows per chunk of a re-run export.
const EXPORT_CHUNK: usize = 500;
/// Loaded rows encoded per result lock.
const LOADED_BATCH: usize = 1000;
/// Rows shown by a file preview.
pub const PREVIEW_ROWS: usize = 100;
/// Rejected rows an import report keeps.
const MAX_REJECTED: usize = 1000;
/// Encoded text written to the file at a time.
const WRITE_BYTES: usize = 256 * 1024;

/// What an export writes.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportSource {
    /// The rows the result has loaded, in fetch order.
    Loaded(QueryId),
    /// Every row of `sql`, run again on the session `key` (on the server database
    /// `database` when given) and streamed to the file.
    Rerun {
        key: SessionKey,
        database: Option<String>,
        sql: String,
    },
}

/// A transfer job for the runtime.
pub(crate) struct TransferRun {
    pub job: JobControl,
    pub work: Work,
}

pub(crate) enum Work {
    CopyGrid {
        q: QueryId,
        source: sqlr_core::ClipboardSource,
        format: TextFormat,
    },
    CopyQuery {
        key: SessionKey,
        io: Arc<SessionIo>,
        ticket: u64,
        database: Option<String>,
        sql: String,
    },
    ExportLoaded {
        q: QueryId,
        format: TextFormat,
        path: PathBuf,
    },
    ExportRerun {
        key: SessionKey,
        io: Arc<SessionIo>,
        ticket: u64,
        database: Option<String>,
        sql: String,
        format: TextFormat,
        path: PathBuf,
    },
    Import {
        key: SessionKey,
        io: Arc<SessionIo>,
        ticket: u64,
        path: PathBuf,
        format: TextFormat,
        target: TableRef,
        mapping: Vec<ColumnMapping>,
        options: ImportOptions,
    },
    Preview {
        path: PathBuf,
        format: TextFormat,
    },
    Copy {
        q: QueryId,
        rows: Vec<usize>,
        cols: Vec<usize>,
        format: TextFormat,
    },
    SaveText {
        path: PathBuf,
        text: String,
    },
    /// A transfer between endpoints (`pipeline`).
    Endpoints(Box<pipeline::EndpointWork>),
    /// Looking into a transfer source (`Engine::probe_source`).
    Probe {
        source: sqlr_core::Endpoint,
        options: ImportOptions,
    },
}

fn io_error(path: &Path, e: &std::io::Error) -> DriverError {
    DriverError::new(ErrorKind::Other, format!("{}: {e}", path.display()))
}

/// The hidden file an export writes before it is complete.
fn partial_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "export".into(), |n| n.to_string_lossy().into_owned());
    path.with_file_name(format!(".{name}.sqlr-partial"))
}

/// An export's file: written to a partial file, renamed into place on `finish`, removed on
/// `discard`.
struct OutFile {
    file: tokio::fs::File,
    partial: PathBuf,
    target: PathBuf,
    pending: String,
    bytes: u64,
}

impl OutFile {
    async fn create(target: PathBuf) -> Result<Self, DriverError> {
        let partial = partial_path(&target);
        let file = tokio::fs::File::create(&partial)
            .await
            .map_err(|e| io_error(&partial, &e))?;
        Ok(Self {
            file,
            partial,
            target,
            pending: String::new(),
            bytes: 0,
        })
    }

    /// Writes the pending text once it is large enough (always with `force`).
    async fn flush(&mut self, force: bool) -> Result<(), DriverError> {
        if self.pending.is_empty() || (!force && self.pending.len() < WRITE_BYTES) {
            return Ok(());
        }
        self.file
            .write_all(self.pending.as_bytes())
            .await
            .map_err(|e| io_error(&self.partial, &e))?;
        self.bytes += self.pending.len() as u64;
        self.pending.clear();
        Ok(())
    }

    async fn finish(mut self) -> Result<PathBuf, DriverError> {
        self.flush(true).await?;
        self.file
            .flush()
            .await
            .map_err(|e| io_error(&self.partial, &e))?;
        drop(self.file);
        tokio::fs::rename(&self.partial, &self.target)
            .await
            .map_err(|e| io_error(&self.target, &e))?;
        Ok(self.target)
    }

    async fn discard(self) {
        drop(self.file);
        let _ = tokio::fs::remove_file(&self.partial).await;
    }

    fn written(&self) -> u64 {
        self.bytes + self.pending.len() as u64
    }
}

/// The SQL type an imported value is converted to before it is bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Int,
    Float,
    Bool,
    Bytes,
    /// A date and time without a zone: an offset in the text is folded into UTC.
    Datetime,
    /// Anything else: the server converts the text.
    Text,
}

impl Target {
    fn of(type_name: &str) -> Self {
        let t = type_name.to_ascii_lowercase();
        let base = t
            .split(|c: char| c == '(' || c.is_whitespace())
            .next()
            .unwrap_or("");
        match base {
            "int" | "integer" | "int2" | "int4" | "int8" | "smallint" | "bigint" | "tinyint"
            | "mediumint" | "serial" | "bigserial" | "smallserial" | "serial4" | "serial8" => {
                Target::Int
            }
            "real" | "float" | "float4" | "float8" | "double" => Target::Float,
            "bool" | "boolean" => Target::Bool,
            "blob" | "tinyblob" | "mediumblob" | "longblob" | "bytea" | "binary" | "varbinary" => {
                Target::Bytes
            }
            "datetime" | "datetime2" | "timestamp" if !t.contains("with time zone") => {
                Target::Datetime
            }
            _ => Target::Text,
        }
    }
}

/// `text` (`0x` + hex digits) as bytes.
fn parse_hex(text: &str) -> Option<Vec<u8>> {
    let hex = text.strip_prefix("0x")?;
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

/// `value` converted for a column of type `target`; the reason when it cannot be.
fn convert(column: &str, value: Value, target: Target) -> Result<Value, ConvertError> {
    let bad = |what: &'static str, v: &Value| {
        let shown: String = v.display().chars().take(40).collect();
        ConvertError {
            column: column.to_string(),
            shown,
            what,
        }
    };
    Ok(match (target, value) {
        (_, Value::Null) => Value::Null,
        (Target::Text, v) => v,
        (Target::Int, Value::Text(t)) => match t.trim().parse::<i64>() {
            Ok(i) => Value::Int(i),
            // Beyond i64 (unsigned 64-bit columns): the server decides.
            Err(_) if t.trim().parse::<u64>().is_ok() => Value::Text(t.trim().to_string()),
            Err(_) => return Err(bad("an integer", &Value::Text(t))),
        },
        (Target::Int, Value::Int(i)) => Value::Int(i),
        (Target::Int, Value::Bool(b)) => Value::Int(i64::from(b)),
        (Target::Int, Value::Float(f)) if f.fract() == 0.0 && f.abs() < 9.0e18 => {
            Value::Int(f as i64)
        }
        (Target::Float, Value::Text(t)) => match t.trim().parse::<f64>() {
            Ok(f) => Value::Float(f),
            Err(_) => return Err(bad("a number", &Value::Text(t))),
        },
        (Target::Float, Value::Float(f)) => Value::Float(f),
        (Target::Float, Value::Int(i)) => Value::Float(i as f64),
        (Target::Bool, Value::Text(t)) => match t.trim().to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" | "y" | "on" => Value::Bool(true),
            "false" | "f" | "0" | "no" | "n" | "off" => Value::Bool(false),
            _ => return Err(bad("a boolean", &Value::Text(t))),
        },
        (Target::Bool, Value::Bool(b)) => Value::Bool(b),
        (Target::Bool, Value::Int(i)) if i == 0 || i == 1 => Value::Bool(i == 1),
        (Target::Bytes, Value::Text(t)) => match parse_hex(&t) {
            Some(bytes) => Value::Bytes(bytes),
            None => Value::Bytes(t.into_bytes()),
        },
        (Target::Bytes, Value::Bytes(b)) => Value::Bytes(b),
        (Target::Datetime, Value::Text(t)) => Value::Text(utc_naive(&t)),
        (Target::Datetime, v) => v,
        (Target::Int, v) => return Err(bad("an integer", &v)),
        (Target::Float, v) => return Err(bad("a number", &v)),
        (Target::Bool, v) => return Err(bad("a boolean", &v)),
        (Target::Bytes, v) => return Err(bad("binary data", &v)),
    })
}

/// Waits for the session's turn and takes its stream lock (closing the stream a result
/// still holds), so the job never runs concurrently with the session's queries.
pub(crate) async fn take_turn<'a>(
    shared: &Shared,
    io: &'a Arc<SessionIo>,
    ticket: u64,
) -> tokio::sync::MutexGuard<'a, crate::session::StreamSlot> {
    io.connect_finished().await;
    let turn = io.wait_turn(ticket).await;
    let mut slot = io.stream.lock().await;
    drop(turn);
    if let Some((prev, mut stream)) = slot.take() {
        let _ = stream.close().await;
        shared.mark_complete(prev);
    }
    slot
}

impl Shared {
    pub(crate) async fn run_transfer(self: Arc<Self>, run: TransferRun) {
        let TransferRun { job, work } = run;
        let outcome = match work {
            Work::CopyGrid { q, source, format } => self.copy_grid(&job, q, source, format).await,
            Work::CopyQuery {
                key,
                io,
                ticket,
                database,
                sql,
            } => {
                let _slot = take_turn(&self, &io, ticket).await;
                self.copy_query(&job, &key, &io, database.as_deref(), &sql)
                    .await
            }
            Work::ExportLoaded { q, format, path } => {
                self.export_loaded(&job, q, format, path).await
            }
            Work::ExportRerun {
                key,
                io,
                ticket,
                database,
                sql,
                format,
                path,
            } => {
                let _slot = take_turn(&self, &io, ticket).await;
                self.export_rerun(&job, &key, &io, database.as_deref(), &sql, format, path)
                    .await
            }
            Work::Import {
                key,
                io,
                ticket,
                path,
                format,
                target,
                mapping,
                options,
            } => {
                let _slot = take_turn(&self, &io, ticket).await;
                let reader = spawn_reader(
                    path.clone(),
                    format,
                    options.batch.clamp(1, 10_000),
                    options.encoding.clone(),
                );
                let rows = RowSource {
                    reader,
                    columns: None,
                    path: Some(&path),
                };
                self.import(&job, &key, &io, rows, &target, &mapping, &options, None)
                    .await
            }
            Work::Preview { path, format } => preview(&path, &format, None).await,
            Work::Copy {
                q,
                rows,
                cols,
                format,
            } => self.copy(&job, q, rows, cols, format).await,
            Work::SaveText { path, text } => save_text(path, text).await,
            Work::Endpoints(work) => self.clone().run_endpoints(&job, *work).await,
            Work::Probe { source, options } => self.clone().run_probe(&job, source, options).await,
        };
        self.jobs.finish(job.id, outcome);
        self.repaint();
    }

    /// The dialect of the session a result ran on, while it is connected.
    fn result_connection(&self, q: QueryId) -> Option<Arc<dyn Connection>> {
        let key = self.result_key(q)?;
        let inner = self.inner.lock();
        inner.sessions.get(&key)?.conn.clone()
    }

    async fn export_loaded(
        &self,
        job: &JobControl,
        q: QueryId,
        format: TextFormat,
        path: PathBuf,
    ) -> JobOutcome {
        let Some(result) = self.result(q) else {
            return JobOutcome::Failed(DriverError::new(ErrorKind::Other, "the result is closed"));
        };
        let conn = self.result_connection(q);
        let dialect = conn
            .as_ref()
            .and_then(|c| c.relational())
            .map(|r| r.dialect());
        let (columns, total) = {
            let r = result.read();
            (r.rows.columns().to_vec(), r.rows.row_count())
        };
        let mut out = match OutFile::create(path).await {
            Ok(out) => out,
            Err(e) => return JobOutcome::Failed(e),
        };
        let mut encoder = Encoder::new(format, &columns, dialect).with_spreadsheet_safe();
        if let Some(formats) = self.export_formats.lock().clone() {
            encoder = encoder.with_formats(formats, &columns);
        }
        encoder.header(&mut out.pending);
        let mut row = 0;
        while row < total {
            if job.is_cancelled() {
                out.discard().await;
                return JobOutcome::Cancelled;
            }
            let end = total.min(row + LOADED_BATCH);
            {
                let r = result.read();
                let mut cells: Vec<Cell<'_>> = Vec::with_capacity(columns.len());
                for i in row..end {
                    cells.clear();
                    cells.extend(
                        (0..columns.len()).map(|c| r.rows.cell(i, c).unwrap_or(Cell::Null)),
                    );
                    encoder.row(&cells, &mut out.pending);
                }
            }
            row = end;
            if let Err(e) = out.flush(false).await {
                out.discard().await;
                return JobOutcome::Failed(e);
            }
            job.progress(JobProgress {
                rows: row as u64,
                bytes: out.written(),
            });
        }
        encoder.footer(&mut out.pending);
        let bytes = out.written();
        match out.finish().await {
            Ok(path) => {
                job.progress(JobProgress {
                    rows: total as u64,
                    bytes,
                });
                JobOutcome::Exported {
                    rows: total as u64,
                    path,
                }
            }
            Err(e) => JobOutcome::Failed(e),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the job, session, I/O, database, SQL, format and path are independent values of one export run; grouping them would need a struct built at the single call site"
    )]
    async fn export_rerun(
        self: &Arc<Self>,
        job: &JobControl,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        database: Option<&str>,
        sql: &str,
        format: TextFormat,
        path: PathBuf,
    ) -> JobOutcome {
        if job.is_cancelled() {
            return JobOutcome::Cancelled;
        }
        let conn = match self.session_connection(key, io, database).await {
            Ok(conn) => conn,
            Err(e) => return JobOutcome::Failed(e),
        };
        let Some(rel) = conn.relational() else {
            return JobOutcome::Failed(DriverError::new(
                ErrorKind::Unsupported,
                "this connection does not run SQL",
            ));
        };
        // Review: an export must never run a statement that changes data a second time.
        if rel.dialect().syntax().classify_script(sql) != StatementClass::Read {
            return JobOutcome::Failed(DriverError::new(
                ErrorKind::Permission,
                "Only a statement that only reads can be re-run for an export; export the \
                 loaded rows instead",
            ));
        }
        job.set_cancel(Some(conn.cancel_handle()));
        let mut stream = match rel.execute(sql).await {
            Ok(Execution::Rows(stream)) => stream,
            Ok(Execution::Affected(_)) => {
                return JobOutcome::Failed(DriverError::new(
                    ErrorKind::Other,
                    "the statement returned no rows to export",
                ));
            }
            Err(e) => {
                self.on_error(key, io, &e);
                return cancelled_or(job, e);
            }
        };
        let outcome = self
            .stream_to_file(job, stream.as_mut(), rel.dialect(), format, path)
            .await;
        let _ = stream.close().await;
        job.set_cancel(None);
        if let JobOutcome::Failed(e) = &outcome {
            self.on_error(key, io, e);
        }
        outcome
    }

    async fn stream_to_file(
        &self,
        job: &JobControl,
        stream: &mut dyn RowStream,
        dialect: &dyn Dialect,
        format: TextFormat,
        path: PathBuf,
    ) -> JobOutcome {
        let columns = stream.columns();
        let mut out = match OutFile::create(path).await {
            Ok(out) => out,
            Err(e) => return JobOutcome::Failed(e),
        };
        let mut encoder = Encoder::new(format, &columns, Some(dialect)).with_spreadsheet_safe();
        if let Some(formats) = self.export_formats.lock().clone() {
            encoder = encoder.with_formats(formats, &columns);
        }
        encoder.header(&mut out.pending);
        let mut rows = 0u64;
        loop {
            if job.is_cancelled() {
                out.discard().await;
                return JobOutcome::Cancelled;
            }
            let chunk = match stream.next_chunk(EXPORT_CHUNK).await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(e) => {
                    out.discard().await;
                    return cancelled_or(job, e);
                }
            };
            let mut cells: Vec<Cell<'_>> = Vec::with_capacity(columns.len());
            for i in 0..chunk.len() {
                cells.clear();
                cells.extend((0..columns.len()).map(|c| chunk.cell(i, c)));
                encoder.row(&cells, &mut out.pending);
            }
            rows += chunk.len() as u64;
            if let Err(e) = out.flush(false).await {
                out.discard().await;
                return JobOutcome::Failed(e);
            }
            job.progress(JobProgress {
                rows,
                bytes: out.written(),
            });
        }
        if job.is_cancelled() {
            out.discard().await;
            return JobOutcome::Cancelled;
        }
        encoder.footer(&mut out.pending);
        let bytes = out.written();
        match out.finish().await {
            Ok(path) => {
                job.progress(JobProgress { rows, bytes });
                JobOutcome::Exported { rows, path }
            }
            Err(e) => JobOutcome::Failed(e),
        }
    }

    /// Writes `rows` into `target`: creates it first when `create` says so, then inserts (or
    /// upserts) every mapped row in one transaction, or in one per batch.
    #[expect(
        clippy::too_many_arguments,
        reason = "the job, session, I/O, row source, target, mapping, options and optional new table are independent borrows of the transfer pipeline; grouping them would need a struct built at the single call site"
    )]
    pub(super) async fn import(
        self: &Arc<Self>,
        job: &JobControl,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        rows: RowSource<'_>,
        target: &TableRef,
        mapping: &[ColumnMapping],
        options: &ImportOptions,
        create: Option<NewTable>,
    ) -> JobOutcome {
        let RowSource {
            reader,
            columns: source_columns,
            path,
        } = rows;
        job.progress(JobProgress { rows: 0, bytes: 0 });
        let conn = match self
            .session_connection(key, io, target.database.as_deref())
            .await
        {
            Ok(conn) => conn,
            Err(e) => return JobOutcome::Failed(e),
        };
        {
            let mut inner = self.inner.lock();
            let transactions = inner.supports_transactions(&key.conn);
            let Some(s) = inner.session(key, io) else {
                return JobOutcome::Failed(not_connected());
            };
            if s.tx.read_only {
                return JobOutcome::Failed(DriverError::new(ErrorKind::Permission, READ_ONLY));
            }
            if !transactions {
                return JobOutcome::Failed(DriverError::new(
                    ErrorKind::Unsupported,
                    "Importing into this SQL driver requires transaction support",
                ));
            }
            if s.tx.is_open() {
                return JobOutcome::Failed(DriverError::new(
                    ErrorKind::Other,
                    "A transaction is open on this session; commit or roll back first",
                ));
            }
        }
        let Some(rel) = conn.relational() else {
            return JobOutcome::Failed(DriverError::new(
                ErrorKind::Unsupported,
                "this connection does not run SQL",
            ));
        };
        let dialect = rel.dialect();
        let local = TableRef {
            database: None,
            ..target.clone()
        };
        if let Some(new) = &create {
            let statements = match &new.ddl {
                Some(ddl) => write_table::split_statements(dialect, ddl),
                None => match write_table::create_table_statements(
                    dialect,
                    &new.from_dialect,
                    &new.to_dialect,
                    &local,
                    &new.columns,
                ) {
                    Ok((statements, warnings)) => {
                        for warning in warnings {
                            job.log(warning);
                        }
                        statements
                    }
                    Err(e) => return JobOutcome::Failed(e),
                },
            };
            for sql in statements {
                job.log(sql.clone());
                if let Err(e) = run_sql(&conn, &sql).await {
                    self.on_error(key, io, &e);
                    return cancelled_or(job, e);
                }
            }
        }
        let info = match rel.describe(&local).await {
            Ok(info) => info,
            Err(e) => return JobOutcome::Failed(e),
        };
        // Without a mapping, source columns go to the table's columns of the same name.
        let by_name: Vec<ColumnMapping>;
        let mapping = match source_columns {
            Some(names) if mapping.is_empty() => {
                by_name = names
                    .iter()
                    .enumerate()
                    .filter_map(|(i, n)| {
                        let column = info
                            .columns
                            .iter()
                            .find(|c| c.name.eq_ignore_ascii_case(n))?;
                        Some(ColumnMapping::new(i, Some(column.name.clone())))
                    })
                    .collect();
                &by_name[..]
            }
            _ => mapping,
        };
        let columns = match mapped_columns(&info, mapping) {
            Ok(columns) => columns,
            Err(e) => {
                return JobOutcome::Failed(DriverError::new(ErrorKind::Other, e.to_string()));
            }
        };
        let table = dialect.qualified(&local);
        let names: Vec<String> = columns
            .iter()
            .map(|c| dialect.quote_ident(&c.name))
            .collect();
        let placeholders: Vec<String> = (1..=columns.len())
            .map(|i| dialect.placeholder(i))
            .collect();
        let mut insert = format!(
            "INSERT INTO {table} ({}) VALUES ({})",
            names.join(", "),
            placeholders.join(", ")
        );
        if let Some(upsert_key) = &options.upsert_key {
            if let Some(missing) = upsert_key
                .iter()
                .find(|k| !columns.iter().any(|c| c.name.eq_ignore_ascii_case(k)))
            {
                return JobOutcome::Failed(DriverError::new(
                    ErrorKind::Other,
                    format!("The upsert key column \"{missing}\" is not among the mapped columns"),
                ));
            }
            let update: Vec<String> = columns
                .iter()
                .filter(|c| !upsert_key.iter().any(|k| k.eq_ignore_ascii_case(&c.name)))
                .map(|c| c.name.clone())
                .collect();
            let raw: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
            insert = match dialect.upsert_row_sql(&table, &raw, &placeholders, upsert_key, &update)
            {
                Some(sql) => sql,
                None => {
                    return JobOutcome::Failed(DriverError::new(
                        ErrorKind::Unsupported,
                        "This connection cannot upsert",
                    ));
                }
            };
        }
        let toggle = if options.disable_constraints {
            match dialect.constraints_toggle_sql() {
                Some(toggle) => Some(toggle),
                None => {
                    return JobOutcome::Failed(DriverError::new(
                        ErrorKind::Unsupported,
                        "This connection cannot switch its constraints off for a load",
                    ));
                }
            }
        } else {
            None
        };
        // The session's own settings the load relies on, set for its length.
        let session = dialect.load_session_sql();
        if let Some((set, _)) = &session {
            for sql in set {
                if let Err(e) = run_sql(&conn, sql).await {
                    return JobOutcome::Failed(DriverError::new(
                        e.kind,
                        format!("Could not prepare the session: {}", e.message),
                    ));
                }
            }
        }
        if let Some((off, _)) = &toggle {
            for sql in off {
                if let Err(e) = run_sql(&conn, sql).await {
                    return JobOutcome::Failed(DriverError::new(
                        e.kind,
                        format!("Could not switch constraints off: {}", e.message),
                    ));
                }
            }
        }
        let load = Load {
            shared: self,
            job,
            key,
            io,
            conn: &conn,
            table: &table,
            columns: &columns,
            insert: &insert,
            options,
            source_names: source_columns,
        };
        let outcome = load.run(reader, path).await;
        // Constraints come back on whatever happened, cancel included: a cancel aimed at
        // the load must not land on the statements that restore them.
        job.set_cancel(None);
        if let Some((_, on)) = &toggle {
            for sql in on {
                if let Err(e) = run_sql(&conn, sql).await {
                    job.log(format!(
                        "Could not switch constraints back on: {}",
                        e.message
                    ));
                }
            }
        }
        if let Some((_, reset)) = &session {
            for sql in reset {
                if let Err(e) = run_sql(&conn, sql).await {
                    job.log(format!("Could not restore the session: {}", e.message));
                }
            }
        }
        outcome
    }

    /// Encodes `rows` × `cols` of a result on a blocking thread, taking the result's lock
    /// for one batch of rows at a time.
    async fn copy(
        &self,
        job: &JobControl,
        q: QueryId,
        rows: Vec<usize>,
        cols: Vec<usize>,
        format: TextFormat,
    ) -> JobOutcome {
        let Some(result) = self.result(q) else {
            return JobOutcome::Failed(DriverError::new(ErrorKind::Other, "the result is closed"));
        };
        let conn = self.result_connection(q);
        let job = job.clone();
        let encoded = tokio::task::spawn_blocking(move || {
            let dialect = conn
                .as_ref()
                .and_then(|c| c.relational())
                .map(|r| r.dialect());
            let columns: Vec<ColumnMeta> = {
                let r = result.read();
                let all = r.rows.columns();
                cols.iter().filter_map(|&c| all.get(c).cloned()).collect()
            };
            let mut encoder = Encoder::new(format, &columns, dialect).with_spreadsheet_safe();
            let mut out = String::new();
            encoder.header(&mut out);
            for batch in rows.chunks(LOADED_BATCH) {
                if job.is_cancelled() {
                    return None;
                }
                let r = result.read();
                let mut cells: Vec<Cell<'_>> = Vec::with_capacity(cols.len());
                for &row in batch {
                    cells.clear();
                    cells.extend(
                        cols.iter()
                            .map(|&c| r.rows.cell(row, c).unwrap_or(Cell::Null)),
                    );
                    encoder.row(&cells, &mut out);
                }
            }
            encoder.footer(&mut out);
            job.progress(JobProgress {
                rows: rows.len() as u64,
                bytes: out.len() as u64,
            });
            Some(out)
        })
        .await;
        match encoded {
            Ok(Some(text)) => JobOutcome::Copied(text),
            Ok(None) => JobOutcome::Cancelled,
            Err(e) => JobOutcome::Failed(DriverError::new(ErrorKind::Other, e.to_string())),
        }
    }
}

/// The import's transaction on one connection.
struct ImportTx<'a> {
    shared: &'a Arc<Shared>,
    key: &'a SessionKey,
    io: &'a Arc<SessionIo>,
    conn: &'a Arc<dyn Connection>,
}

impl ImportTx<'_> {
    async fn run(&mut self, sql: &str) -> Result<(), DriverError> {
        let outcome = run_sql(self.conn, sql).await;
        if let Err(e) = &outcome {
            self.shared.on_error(self.key, self.io, e);
        }
        outcome
    }

    async fn insert(&mut self, sql: &str, params: &[Value]) -> Result<(), DriverError> {
        let rel = self.conn.relational().ok_or_else(not_connected)?;
        match rel.execute_with(sql, params).await {
            Ok(Execution::Affected(_)) => Ok(()),
            Ok(Execution::Rows(mut rows)) => {
                let _ = rows.close().await;
                Ok(())
            }
            Err(e) => {
                self.shared.on_error(self.key, self.io, &e);
                Err(e)
            }
        }
    }

    async fn rollback(&mut self) {
        let _ = run_sql(self.conn, "ROLLBACK").await;
    }
}

/// `Cancelled` when the job was cancelled (the error is the interrupted statement's), else
/// `Failed(e)`.
fn cancelled_or(job: &JobControl, e: DriverError) -> JobOutcome {
    if job.is_cancelled() || e.kind == ErrorKind::Cancelled {
        JobOutcome::Cancelled
    } else {
        JobOutcome::Failed(e)
    }
}

/// The mapped source columns with their conversions.
fn mapped_columns(
    info: &TableInfo,
    mapping: &[ColumnMapping],
) -> Result<Vec<MappedColumn>, MappingError> {
    let mut columns: Vec<MappedColumn> = Vec::new();
    for m in mapping {
        let Some(name) = &m.target else {
            continue;
        };
        let column = info
            .column(name)
            .ok_or_else(|| MappingError::UnknownColumn {
                table: info.table.name.clone(),
                column: name.clone(),
            })?;
        if columns.iter().any(|c| &c.name == name) {
            return Err(MappingError::MappedTwice {
                column: name.clone(),
            });
        }
        columns.push(MappedColumn {
            src: m.source,
            name: column.name.clone(),
            target: Target::of(m.target_type.as_deref().unwrap_or(&column.type_name)),
            transform: m.transform.clone(),
        });
    }
    if columns.is_empty() {
        return Err(MappingError::NothingMapped);
    }
    Ok(columns)
}

/// What a table load needs once the table and the mapping are settled.
struct Load<'a> {
    shared: &'a Arc<Shared>,
    job: &'a JobControl,
    key: &'a SessionKey,
    io: &'a Arc<SessionIo>,
    conn: &'a Arc<dyn Connection>,
    table: &'a str,
    columns: &'a [MappedColumn],
    insert: &'a str,
    options: &'a ImportOptions,
    source_names: Option<&'a [String]>,
}

impl Load<'_> {
    /// The row loop: converts each row, inserts it (inside a savepoint when bad rows are
    /// skipped), commits per batch or once, and reports what happened.
    async fn run(&self, reader: mpsc::Receiver<RowBatch>, path: Option<&Path>) -> JobOutcome {
        let mut rejects = self
            .options
            .rejected_file
            .clone()
            .map(|p| RejectedFile::new(p, self.source_names.map(<[String]>::to_vec)));
        let outcome = self.run_rows(reader, path, &mut rejects).await;
        // The rejected rows are on disk whatever became of the load: that is when the user
        // needs them.
        if let Some(file) = rejects
            && let Err(e) = file.finish().await
        {
            self.job.log(e.message);
        }
        outcome
    }

    async fn run_rows(
        &self,
        mut reader: mpsc::Receiver<RowBatch>,
        path: Option<&Path>,
        rejects: &mut Option<RejectedFile>,
    ) -> JobOutcome {
        let Self {
            shared,
            job,
            key,
            io,
            conn,
            table,
            columns,
            insert,
            options,
            ..
        } = *self;
        let Some(rel) = conn.relational() else {
            return JobOutcome::Failed(not_connected());
        };
        let dialect = rel.dialect();
        let savepoint = dialect.savepoint_sql("sqlr_row");
        let rollback_to = dialect.rollback_to_sql("sqlr_row");
        let release = dialect.release_savepoint_sql("sqlr_row");
        let begin = dialect.begin_sql();
        job.set_cancel(Some(conn.cancel_handle()));

        let mut session = ImportTx {
            shared,
            key,
            io,
            conn,
        };
        if let Err(e) = session.run(begin).await {
            return JobOutcome::Failed(e);
        }
        if options.truncate_first
            && let Err(e) = session.run(&format!("DELETE FROM {table}")).await
        {
            session.rollback().await;
            return cancelled_or(job, e);
        }
        let mut read = 0u64;
        let mut written = 0u64;
        let mut committed = 0u64;
        let mut in_batch = 0usize;
        let mut rejected: Vec<(u64, String)> = Vec::new();
        let mut rejected_count = 0u64;
        let batch = options.batch.max(1);
        let kept_note = |committed: u64| {
            if committed > 0 {
                format!("; {committed} rows committed by earlier batches stay")
            } else {
                "; nothing was imported".to_string()
            }
        };
        // Rows arrive in batches while the source is read (a file on a blocking thread).
        while let Some(chunk) = reader.recv().await {
            let rows = match chunk {
                Ok(rows) => rows,
                Err(e) => {
                    session.rollback().await;
                    return JobOutcome::Failed(DriverError::new(
                        ErrorKind::Other,
                        format!("{e}{}", kept_note(committed)),
                    ));
                }
            };
            for (line, row) in rows {
                read += 1;
                if job.is_cancelled() {
                    session.rollback().await;
                    if committed > 0 {
                        job.log(format!(
                            "{committed} rows committed by earlier batches stay"
                        ));
                    }
                    return JobOutcome::Cancelled;
                }
                let params: Result<Vec<Value>, ConvertError> = columns
                    .iter()
                    .map(|c| {
                        let mut value = row.get(c.src).cloned().unwrap_or(Value::Null);
                        if let Some(transform) = &c.transform {
                            value = apply_transform(value, transform);
                        }
                        convert(&c.name, value, c.target)
                    })
                    .collect();
                let failure = match params {
                    Err(reason) => Some(reason.to_string()),
                    Ok(params) => {
                        let protected = options.skip_bad_rows;
                        if protected && let Err(e) = session.run(&savepoint).await {
                            session.rollback().await;
                            return cancelled_or(job, e);
                        }
                        match session.insert(insert, &params).await {
                            Ok(()) => {
                                // A server without RELEASE keeps the savepoint until the
                                // transaction ends: nothing to run.
                                let released = if protected {
                                    match &release {
                                        Some(release) => session.run(release).await,
                                        None => Ok(()),
                                    }
                                } else {
                                    Ok(())
                                };
                                if let Err(e) = released {
                                    session.rollback().await;
                                    return cancelled_or(job, e);
                                }
                                None
                            }
                            Err(e) if e.kind == ErrorKind::Connection || job.is_cancelled() => {
                                session.rollback().await;
                                if committed > 0 && job.is_cancelled() {
                                    job.log(format!(
                                        "{committed} rows committed by earlier batches stay"
                                    ));
                                }
                                return cancelled_or(job, e);
                            }
                            Err(e) => {
                                if protected {
                                    let undo = match session.run(&rollback_to).await {
                                        Ok(()) => match &release {
                                            Some(release) => session.run(release).await,
                                            None => Ok(()),
                                        },
                                        Err(e) => Err(e),
                                    };
                                    if let Err(e) = undo {
                                        session.rollback().await;
                                        return JobOutcome::Failed(e);
                                    }
                                }
                                Some(e.message)
                            }
                        }
                    }
                };
                match failure {
                    None => {
                        written += 1;
                        in_batch += 1;
                    }
                    Some(reason) if options.skip_bad_rows => {
                        rejected_count += 1;
                        if rejected.len() < MAX_REJECTED {
                            rejected.push((line, reason.clone()));
                        }
                        if let Some(file) = rejects.as_mut()
                            && let Err(e) = file.add(line, &reason, &row).await
                        {
                            session.rollback().await;
                            return JobOutcome::Failed(e);
                        }
                        if options
                            .max_rejected
                            .is_some_and(|max| rejected_count >= max)
                        {
                            session.rollback().await;
                            let rows = if rejected_count == 1 { "row" } else { "rows" };
                            return JobOutcome::Failed(DriverError::new(
                                ErrorKind::Other,
                                format!(
                                    "Stopped after {rejected_count} rejected {rows} (line {line}: \
                                     {reason}){}",
                                    kept_note(committed)
                                ),
                            ));
                        }
                    }
                    Some(reason) => {
                        session.rollback().await;
                        return JobOutcome::Failed(DriverError::new(
                            ErrorKind::Other,
                            format!("Line {line}: {reason}{}", kept_note(committed)),
                        ));
                    }
                }
                if options.commit_per_batch && in_batch >= batch {
                    let next = match session.run("COMMIT").await {
                        Ok(()) => session.run(begin).await,
                        Err(e) => Err(e),
                    };
                    if let Err(e) = next {
                        session.rollback().await;
                        return JobOutcome::Failed(e);
                    }
                    committed = written;
                    in_batch = 0;
                }
                if read.is_multiple_of(100) {
                    job.progress(JobProgress {
                        rows: read,
                        bytes: 0,
                    });
                }
            }
        }
        if let Err(e) = session.run("COMMIT").await {
            session.rollback().await;
            return JobOutcome::Failed(e);
        }
        job.set_cancel(None);
        let bytes = match path {
            Some(path) => tokio::fs::metadata(path).await.map_or(0, |m| m.len()),
            None => 0,
        };
        job.progress(JobProgress { rows: read, bytes });
        JobOutcome::Imported {
            read,
            written,
            rejected,
        }
    }
}

/// A source column bound to a target column: the conversion it gets on the way.
struct MappedColumn {
    src: usize,
    name: String,
    target: Target,
    transform: Option<Transform>,
}

/// The table a transfer creates: the source's columns and dialect, for the type mapping.
pub(crate) struct NewTable {
    /// The user's `CREATE TABLE`, replacing the generated one.
    pub ddl: Option<String>,
    /// The source's dialect id (`postgres`, `mysql`, `sqlite`) or `file`.
    pub from_dialect: String,
    pub to_dialect: String,
    /// `(name, source type, forced type)` of each column the table gets.
    pub columns: Vec<(String, String, Option<String>)>,
}

/// Writes `text` to `path` (through a partial file, like an export).
async fn save_text(path: PathBuf, text: String) -> JobOutcome {
    let lines = text.lines().count() as u64;
    let mut out = match OutFile::create(path).await {
        Ok(out) => out,
        Err(e) => return JobOutcome::Failed(e),
    };
    out.pending = text;
    match out.finish().await {
        Ok(path) => JobOutcome::Exported { rows: lines, path },
        Err(e) => JobOutcome::Failed(e),
    }
}

/// A batch of decoded rows (with their lines), or why reading stopped.
pub(crate) type RowBatch = Result<Vec<(u64, Vec<Value>)>, TransferFileError>;

/// Where an import's rows come from: a file read on a blocking thread, or a collection or
/// Redis keys read by a transfer.
pub(crate) struct RowSource<'a> {
    pub reader: mpsc::Receiver<RowBatch>,
    /// The source's column names, when known: rows map to the table's columns of the same
    /// name when no mapping is given.
    pub columns: Option<&'a [String]>,
    /// The file read, for the final byte count.
    pub path: Option<&'a Path>,
}

/// Reads the file at `path` on a blocking thread, sending its rows in batches of `size`;
/// reading stops when the receiver is dropped.
pub(crate) fn spawn_reader(
    path: PathBuf,
    format: TextFormat,
    size: usize,
    encoding: Option<String>,
) -> mpsc::Receiver<RowBatch> {
    let (tx, rx) = mpsc::channel(2);
    tokio::task::spawn_blocking(move || {
        let file = match encoding::open_text(&path, encoding.as_deref()) {
            Ok(file) => file,
            Err(e) => {
                let _ = tx.blocking_send(Err(TransferFileError::File {
                    path: path.clone(),
                    source: e,
                }));
                return;
            }
        };
        let mut batch = Vec::with_capacity(size);
        let outcome = read_rows(&format, file, |_, line, row| {
            batch.push((line, row));
            batch.len() < size || tx.blocking_send(Ok(std::mem::take(&mut batch))).is_ok()
        });
        if !batch.is_empty() {
            let _ = tx.blocking_send(Ok(batch));
        }
        if let Err(e) = outcome {
            let _ = tx.blocking_send(Err(e.into()));
        }
    });
    rx
}

/// The column names and first rows of a file, read on a blocking thread that stops after
/// [`PREVIEW_ROWS`].
async fn preview(path: &Path, format: &TextFormat, encoding: Option<String>) -> JobOutcome {
    let (path, format) = (path.to_path_buf(), format.clone());
    let read = tokio::task::spawn_blocking(move || {
        let file = encoding::open_text(&path, encoding.as_deref()).map_err(|source| {
            TransferFileError::File {
                path: path.clone(),
                source,
            }
        })?;
        let mut rows = Vec::new();
        let columns = read_rows(&format, file, |_, _, row| {
            rows.push(row);
            rows.len() < PREVIEW_ROWS
        })?;
        for row in &mut rows {
            row.resize(columns.len(), Value::Null);
        }
        Ok::<_, TransferFileError>((columns, rows))
    })
    .await;
    match read {
        Ok(Ok((columns, rows))) => JobOutcome::Preview { columns, rows },
        Ok(Err(e)) => JobOutcome::Failed(DriverError::new(ErrorKind::Other, e.to_string())),
        Err(e) => JobOutcome::Failed(DriverError::new(ErrorKind::Other, e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_follow_the_column_type() {
        assert_eq!(Target::of("INTEGER"), Target::Int);
        assert_eq!(Target::of("bigint unsigned"), Target::Int);
        assert_eq!(Target::of("int(11)"), Target::Int);
        assert_eq!(Target::of("interval"), Target::Text);
        assert_eq!(Target::of("point"), Target::Text);
        assert_eq!(Target::of("double precision"), Target::Float);
        assert_eq!(Target::of("bytea"), Target::Bytes);
        assert_eq!(Target::of("numeric(10,2)"), Target::Text);
        assert_eq!(Target::of("datetime(6)"), Target::Datetime);
        assert_eq!(Target::of("timestamp"), Target::Datetime);
        assert_eq!(Target::of("timestamp without time zone"), Target::Datetime);
        assert_eq!(Target::of("timestamp with time zone"), Target::Text);
        assert_eq!(Target::of("timestamptz"), Target::Text);
        assert_eq!(
            convert(
                "c",
                Value::Text("2024-03-09 12:00:00+02".into()),
                Target::Datetime
            ),
            Ok(Value::Text("2024-03-09 10:00:00".into()))
        );
        let t = |s: &str| Value::Text(s.into());
        assert_eq!(convert("c", t(" 42 "), Target::Int), Ok(Value::Int(42)));
        assert_eq!(
            convert("c", t("18446744073709551615"), Target::Int),
            Ok(t("18446744073709551615"))
        );
        assert_eq!(
            convert("c", t("abc"), Target::Int).unwrap_err().to_string(),
            "column c: \"abc\" is not an integer"
        );
        assert_eq!(
            convert("c", t("NaN"), Target::Float).map(|v| v.is_null()),
            Ok(false)
        );
        assert_eq!(
            convert("c", t("0x00ff"), Target::Bytes),
            Ok(Value::Bytes(vec![0, 255]))
        );
        assert_eq!(
            convert("c", t("0x"), Target::Bytes),
            Ok(Value::Bytes(vec![]))
        );
        assert_eq!(
            convert("c", t("hi"), Target::Bytes),
            Ok(Value::Bytes(b"hi".to_vec()))
        );
        assert_eq!(convert("c", t("yes"), Target::Bool), Ok(Value::Bool(true)));
        assert_eq!(convert("c", Value::Null, Target::Int), Ok(Value::Null));
        assert_eq!(
            convert("c", Value::Float(3.0), Target::Int),
            Ok(Value::Int(3))
        );
        assert!(convert("c", Value::Float(3.5), Target::Int).is_err());
    }

    #[test]
    fn partial_file_is_hidden_next_to_the_target() {
        assert_eq!(
            partial_path(Path::new("/tmp/a/out.csv")),
            PathBuf::from("/tmp/a/.out.csv.sqlr-partial")
        );
    }
}
