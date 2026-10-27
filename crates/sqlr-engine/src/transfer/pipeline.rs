//! Transfers between tables, collections, indices and Redis keys.
//!
//! A source sends rows or documents in batches through a channel; a sink takes them:
//! - a file: rows through the export encoders; documents (a collection to JSON or JSON
//!   Lines) as relaxed Extended JSON, nesting kept;
//! - a table: rows through the import (mapped by name when no mapping is given);
//! - a collection: rows as flat documents, documents as they are (`write_docs`).
//!
//! Documents meeting a tabular side are flattened by the Table rule (columns from the first
//! page, nested values as JSON text). The job takes the turn of the session it writes on
//! (the source's session for a file), so it never runs between a session's statements.

use std::path::PathBuf;
use std::sync::Arc;

use sqlr_core::{
    Cell, ColumnMapping, ColumnMeta, DocValue, DriverError, Encoder, Endpoint, ErrorKind,
    Execution, StatementClass, TableRef, TextFormat, TransferSpec, Value,
};
use tokio::sync::mpsc;

use super::{ImportOptions, NewTable, OutFile, RowSource, TransferRun, Work, take_turn};
use crate::engine::{Command, Engine, Shared, not_connected};
use crate::job::{JobControl, JobId, JobKind, JobOutcome, JobProgress};
use crate::session::{SessionIo, SessionKey};

/// Keys read from a referenced table for the generator's foreign keys.
const FK_POOL: usize = 100_000;

/// Documents in batches, or why reading stopped.
pub(crate) type DocBatch = Result<Vec<DocValue>, String>;

/// What a source sends.
pub(crate) enum Records {
    Rows {
        columns: Vec<ColumnMeta>,
        reader: mpsc::Receiver<super::RowBatch>,
        /// Values are text to type (a delimited file).
        typed_text: bool,
    },
    Docs(mpsc::Receiver<DocBatch>),
}

/// A transfer for the runtime.
pub(crate) struct EndpointWork {
    source: Endpoint,
    target: Endpoint,
    mapping: Vec<ColumnMapping>,
    options: ImportOptions,
    /// The session whose turn the job takes, and its I/O.
    turn: Option<(SessionKey, Arc<SessionIo>, u64)>,
}

impl Engine {
    /// Looks into a transfer source as a job ([`JobOutcome::Probed`]): its columns with
    /// their types, its first rows and, for a workbook, its sheets. Nothing is written.
    pub fn probe_source(&self, source: Endpoint, options: ImportOptions) -> JobId {
        let job = self.shared.jobs.create(JobKind::Preview);
        let id = job.id;
        self.shared.send(Command::Transfer(TransferRun {
            job,
            work: Work::Probe { source, options },
        }));
        self.shared.repaint();
        id
    }

    /// Runs `spec` as a job: [`JobOutcome::Exported`] for a file target,
    /// [`JobOutcome::Imported`] for a table or collection. Sources: a file, a query that
    /// only reads, a collection or index (its `query`, else every document) or Redis keys;
    /// targets: a file, a table or a collection. Work runs on the connections' explorer
    /// sessions.
    pub fn transfer(&self, spec: TransferSpec) -> JobId {
        let TransferSpec {
            source,
            target,
            mapping,
            options,
        } = spec;
        let kind = match target {
            Endpoint::File { .. } | Endpoint::Workbook { .. } | Endpoint::Parquet { .. } => {
                JobKind::Export
            }
            _ => JobKind::Import,
        };
        let job = self.shared.jobs.create(kind);
        let id = job.id;
        // The job writes on the target's session; a file is written in the source's turn.
        let turn_conn = target.conn().or(source.conn()).map(str::to_string);
        let turn = match turn_conn {
            None => None,
            Some(conn) => {
                let key = SessionKey::meta(&conn);
                let io = {
                    let inner = self.shared.inner.lock();
                    inner
                        .sessions
                        .get(&key)
                        .filter(|s| s.is_live())
                        .map(|s| s.io.clone())
                };
                match io {
                    None => {
                        self.shared
                            .jobs
                            .finish(id, JobOutcome::Failed(not_connected()));
                        self.shared.repaint();
                        return id;
                    }
                    Some(io) => Some((key, io.clone(), io.issue_ticket())),
                }
            }
        };
        let work = EndpointWork {
            source,
            target,
            mapping,
            options,
            turn,
        };
        self.shared.send(Command::Transfer(TransferRun {
            job,
            work: Work::Endpoints(Box::new(work)),
        }));
        self.shared.repaint();
        id
    }
}

fn unsupported(message: &str) -> JobOutcome {
    JobOutcome::Failed(DriverError::new(ErrorKind::Unsupported, message))
}

/// The text columns of a row source known by name only (a file).
pub(super) fn text_columns(names: &[String]) -> Vec<ColumnMeta> {
    names
        .iter()
        .map(|n| {
            ColumnMeta::new(
                n,
                "text",
                sqlr_core::ColumnKind::Mixed,
                sqlr_core::DisplayHint::Plain,
            )
        })
        .collect()
}

impl Shared {
    pub(crate) async fn run_probe(
        self: Arc<Self>,
        job: &JobControl,
        source: Endpoint,
        options: ImportOptions,
    ) -> JobOutcome {
        let sheets = match &source {
            Endpoint::Workbook { path, .. } => {
                let path = path.clone();
                match tokio::task::spawn_blocking(move || super::xlsx::sheet_names(&path)).await {
                    Ok(Ok(sheets)) => sheets,
                    Ok(Err(e)) => return JobOutcome::Failed(DriverError::new(ErrorKind::Other, e)),
                    Err(e) => {
                        return JobOutcome::Failed(DriverError::new(
                            ErrorKind::Other,
                            e.to_string(),
                        ));
                    }
                }
            }
            _ => Vec::new(),
        };
        let records = match self.open_source(job, &source, None, false, &options).await {
            Ok(records) => records,
            Err(outcome) => return outcome,
        };
        let Records::Rows {
            columns,
            mut reader,
            ..
        } = records
        else {
            return unsupported("Documents have no columns to look into");
        };
        let mut rows: Vec<Vec<Value>> = Vec::new();
        while rows.len() < super::PREVIEW_ROWS {
            match reader.recv().await {
                Some(Ok(batch)) => rows.extend(batch.into_iter().map(|(_, row)| row)),
                Some(Err(e)) => return JobOutcome::Failed(DriverError::new(ErrorKind::Other, e)),
                None => break,
            }
        }
        // Dropping the reader stops the source.
        drop(reader);
        rows.truncate(super::PREVIEW_ROWS);
        let width = columns.len();
        for row in &mut rows {
            row.resize(width, Value::Null);
        }
        JobOutcome::Probed(crate::job::SourceProbe {
            columns,
            rows,
            sheets,
        })
    }

    pub(crate) async fn run_endpoints(
        self: Arc<Self>,
        job: &JobControl,
        work: EndpointWork,
    ) -> JobOutcome {
        let EndpointWork {
            source,
            target,
            mapping,
            options,
            turn,
        } = work;
        let _slot = match &turn {
            Some((_, io, ticket)) => Some(take_turn(&self, io, *ticket).await),
            None => None,
        };
        if job.is_cancelled() {
            return JobOutcome::Cancelled;
        }
        // Documents stay documents only between collections, or into a JSON file.
        let docs_wanted = match &target {
            Endpoint::Collection { .. } => matches!(source, Endpoint::Collection { .. }),
            Endpoint::File { format, .. } => {
                matches!(source, Endpoint::Collection { .. })
                    && matches!(format, TextFormat::Json | TextFormat::JsonLines)
            }
            _ => false,
        };
        let records = match self
            .open_source(job, &source, target.conn(), docs_wanted, &options)
            .await
        {
            Ok(records) => records,
            Err(outcome) => return outcome,
        };
        let source_dialect = match source.conn() {
            Some(conn) => self.driver_of(conn).unwrap_or_else(|| "file".into()),
            None => "file".into(),
        };
        match target {
            Endpoint::File { path, format } => {
                self.records_to_file(job, records, format, path).await
            }
            Endpoint::Table { conn, table } => {
                let Some((key, io, _)) = &turn else {
                    return JobOutcome::Failed(not_connected());
                };
                debug_assert_eq!(key.conn, conn);
                self.records_to_table(job, key, io, records, &table, &mapping, &options, None)
                    .await
            }
            Endpoint::NewTable { conn, table, ddl } => {
                let Some((key, io, _)) = &turn else {
                    return JobOutcome::Failed(not_connected());
                };
                debug_assert_eq!(key.conn, conn);
                let Records::Rows { columns, .. } = &records else {
                    return unsupported("Documents reach a table as rows");
                };
                let create = NewTable {
                    ddl,
                    from_dialect: source_dialect,
                    to_dialect: self.driver_of(&conn).unwrap_or_default(),
                    // The table gets the mapped columns, under their target names; with no
                    // mapping, every source column as it is.
                    columns: if mapping.is_empty() {
                        columns
                            .iter()
                            .map(|c| (c.name.clone(), c.type_name.clone(), None))
                            .collect()
                    } else {
                        mapping
                            .iter()
                            .filter_map(|m| {
                                let name = m.target.clone()?;
                                let source = columns.get(m.source)?;
                                Some((name, source.type_name.clone(), m.target_type.clone()))
                            })
                            .collect()
                    },
                };
                self.records_to_table(
                    job,
                    key,
                    io,
                    records,
                    &table,
                    &mapping,
                    &options,
                    Some(create),
                )
                .await
            }
            Endpoint::Workbook { path, .. } => {
                super::files::records_to_workbook(job, records, path).await
            }
            Endpoint::Parquet { path } => {
                super::files::records_to_parquet(job, records, path).await
            }
            Endpoint::Collection {
                conn, target: coll, ..
            } => {
                let Some((key, io, _)) = &turn else {
                    return JobOutcome::Failed(not_connected());
                };
                debug_assert_eq!(key.conn, conn);
                self.records_to_collection(job, key, io, records, &coll, &mapping, &options)
                    .await
            }
            Endpoint::RedisKeys { .. } => unsupported("Redis keys are a transfer source only"),
            Endpoint::Query { .. } => unsupported("A query is a transfer source only"),
            Endpoint::Generator(_) => unsupported("The data generator is a transfer source only"),
        }
    }

    /// Opens a source's records. `target_conn` is the connection the rows will be written
    /// on (one connection cannot read a statement and write at the same time); the error
    /// is the outcome the job ends with.
    // The error is the outcome the job ends with, which is large by nature.
    #[expect(
        clippy::result_large_err,
        reason = "the error is a `JobOutcome` the caller returns as is when the source cannot be opened (cancelled, unsupported or failed); boxing it would only add an allocation on a path taken once per job"
    )]
    pub(super) async fn open_source(
        self: &Arc<Self>,
        job: &JobControl,
        source: &Endpoint,
        target_conn: Option<&str>,
        docs_wanted: bool,
        options: &ImportOptions,
    ) -> Result<Records, JobOutcome> {
        let same = |conn: &str| {
            (target_conn == Some(conn)).then(|| {
                unsupported(
                    "The source and the target are on the same connection: use a statement \
                     such as INSERT … SELECT",
                )
            })
        };
        let batch = options.batch.clamp(1, 10_000);
        match source {
            Endpoint::File { path, format } => self
                .file_rows(path, format, options)
                .await
                .map_err(JobOutcome::Failed),
            Endpoint::Collection {
                conn,
                target: coll,
                query,
            } => self
                .collection_records(conn, coll, query.as_deref(), docs_wanted)
                .await
                .map_err(|e| super::cancelled_or(job, e)),
            Endpoint::RedisKeys { conn, db, pattern } => self
                .key_rows(conn, *db, pattern)
                .await
                .map_err(JobOutcome::Failed),
            Endpoint::Query {
                conn,
                database,
                sql,
            } => {
                if let Some(outcome) = same(conn) {
                    return Err(outcome);
                }
                self.query_rows(conn, database.as_deref(), sql)
                    .await
                    .map_err(|e| super::cancelled_or(job, e))
            }
            Endpoint::Table { conn, table } => {
                if let Some(outcome) = same(conn) {
                    return Err(outcome);
                }
                self.table_rows(conn, table)
                    .await
                    .map_err(|e| super::cancelled_or(job, e))
            }
            Endpoint::NewTable { .. } => Err(unsupported("A new table is a target only")),
            Endpoint::Generator(spec) => self
                .generator_rows(spec, target_conn, options.batch.clamp(1, 10_000))
                .await
                .map_err(JobOutcome::Failed),
            Endpoint::Workbook {
                path,
                sheet,
                header,
            } => super::files::workbook_rows(path.clone(), sheet.clone(), *header, batch)
                .await
                .map_err(JobOutcome::Failed),
            Endpoint::Parquet { path } => super::files::parquet_rows(path.clone(), batch)
                .await
                .map_err(JobOutcome::Failed),
        }
    }

    /// Rows from the test-data generator. The keys of the tables it references are read first
    /// from the target's connection (up to [`FK_POOL`] each), then rows are made in batches.
    async fn generator_rows(
        self: &Arc<Self>,
        spec: &sqlr_core::DataGenSpec,
        target_conn: Option<&str>,
        batch: usize,
    ) -> Result<Records, DriverError> {
        use sqlr_core::{Generator, RowGenerator};
        let mut pools = std::collections::BTreeMap::new();
        for c in &spec.columns {
            let Generator::ForeignKey { table, column } = &c.generator else {
                continue;
            };
            let key = RowGenerator::pool_key(table, column);
            if pools.contains_key(&key) {
                continue;
            }
            let conn = target_conn.ok_or_else(not_connected)?;
            let io = self.live_io(conn).ok_or_else(not_connected)?;
            let connection = self
                .session_connection(&SessionKey::meta(conn), &io, table.database.as_deref())
                .await?;
            let rel = connection.relational().ok_or_else(|| {
                DriverError::new(ErrorKind::Unsupported, "this connection does not run SQL")
            })?;
            let dialect = rel.dialect();
            let local = TableRef {
                database: None,
                ..table.clone()
            };
            let sql = format!(
                "SELECT {} FROM {} LIMIT {FK_POOL}",
                dialect.quote_ident(column),
                dialect.qualified(&local)
            );
            let mut values = Vec::new();
            if let Execution::Rows(mut stream) = rel.execute(&sql).await? {
                while let Some(chunk) = stream.next_chunk(super::EXPORT_CHUNK).await? {
                    values.extend((0..chunk.len()).map(|r| Value::from_cell(chunk.cell(r, 0))));
                }
                stream.close().await?;
            }
            pools.insert(key, values);
        }
        // Columns the database fills (`Default`) are not written.
        let keep: Vec<usize> = (0..spec.columns.len())
            .filter(|&i| spec.columns[i].generator != Generator::Default)
            .collect();
        let columns: Vec<ColumnMeta> = keep
            .iter()
            .map(|&i| {
                ColumnMeta::new(
                    &spec.columns[i].column,
                    "text",
                    sqlr_core::ColumnKind::Mixed,
                    sqlr_core::DisplayHint::Plain,
                )
            })
            .collect();
        let mut generator = RowGenerator::new(spec, pools);
        let (tx, rx) = mpsc::channel(2);
        tokio::task::spawn_blocking(move || {
            let mut line = 0u64;
            loop {
                match generator.next_batch(batch) {
                    Ok(rows) if rows.is_empty() => break,
                    Ok(rows) => {
                        let rows = rows
                            .into_iter()
                            .map(|row| {
                                line += 1;
                                (line, keep.iter().map(|&i| row[i].clone()).collect())
                            })
                            .collect();
                        if tx.blocking_send(Ok(rows)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.blocking_send(Err(e));
                        break;
                    }
                }
            }
        });
        Ok(Records::Rows {
            columns,
            reader: rx,
            typed_text: false,
        })
    }

    /// The driver id (`postgres`, `mysql`, `sqlite`…) of a connection.
    fn driver_of(&self, conn: &str) -> Option<String> {
        self.inner.lock().driver_of(conn)
    }

    /// Every row of `table` on `conn`'s explorer session.
    async fn table_rows(&self, conn: &str, table: &TableRef) -> Result<Records, DriverError> {
        let key = SessionKey::meta(conn);
        let io = self.live_io(conn).ok_or_else(not_connected)?;
        let connection = self
            .session_connection(&key, &io, table.database.as_deref())
            .await?;
        let rel = connection.relational().ok_or_else(|| {
            DriverError::new(ErrorKind::Unsupported, "this connection does not run SQL")
        })?;
        let local = TableRef {
            database: None,
            ..table.clone()
        };
        let sql = format!("SELECT * FROM {}", rel.dialect().qualified(&local));
        self.query_rows(conn, table.database.as_deref(), &sql).await
    }

    /// The rows of `sql` (a statement that only reads) on `conn`'s explorer session.
    async fn query_rows(
        &self,
        conn: &str,
        database: Option<&str>,
        sql: &str,
    ) -> Result<Records, DriverError> {
        let key = SessionKey::meta(conn);
        let io = self.live_io(conn).ok_or_else(not_connected)?;
        let connection = self.session_connection(&key, &io, database).await?;
        let rel = connection.relational().ok_or_else(|| {
            DriverError::new(ErrorKind::Unsupported, "this connection does not run SQL")
        })?;
        if rel.dialect().syntax().classify_script(sql) != StatementClass::Read {
            return Err(DriverError::new(
                ErrorKind::Permission,
                "Only a statement that only reads can be a transfer source",
            ));
        }
        let mut stream = match rel.execute(sql).await? {
            Execution::Rows(stream) => stream,
            Execution::Affected(_) => {
                return Err(DriverError::new(
                    ErrorKind::Other,
                    "the statement returned no rows to transfer",
                ));
            }
        };
        let columns: Vec<ColumnMeta> = stream.columns().to_vec();
        let (tx, rx) = mpsc::channel(2);
        let width = columns.len();
        tokio::spawn(async move {
            let mut line = 0u64;
            loop {
                let batch = match stream.next_chunk(super::EXPORT_CHUNK).await {
                    Ok(Some(chunk)) => Ok((0..chunk.len())
                        .map(|r| {
                            line += 1;
                            let row = (0..width)
                                .map(|c| Value::from_cell(chunk.cell(r, c)))
                                .collect();
                            (line, row)
                        })
                        .collect()),
                    Ok(None) => break,
                    Err(e) => Err(e.message),
                };
                let failed = batch.is_err();
                if tx.send(batch).await.is_err() || failed {
                    break;
                }
            }
            let _ = stream.close().await;
        });
        Ok(Records::Rows {
            columns,
            reader: rx,
            typed_text: false,
        })
    }

    /// The rows of a file: its columns from the first rows, then every row in batches.
    async fn file_rows(
        &self,
        path: &std::path::Path,
        format: &TextFormat,
        options: &ImportOptions,
    ) -> Result<Records, DriverError> {
        let names = match super::preview(path, format, options.encoding.clone()).await {
            JobOutcome::Preview { columns, .. } => columns,
            JobOutcome::Failed(e) => return Err(e),
            _ => Vec::new(),
        };
        let reader = super::spawn_reader(
            path.to_path_buf(),
            format.clone(),
            options.batch.clamp(1, 10_000),
            options.encoding.clone(),
        );
        Ok(Records::Rows {
            columns: text_columns(&names),
            reader,
            typed_text: matches!(format, TextFormat::Csv(_) | TextFormat::Tsv),
        })
    }

    async fn records_to_file(
        &self,
        job: &JobControl,
        records: Records,
        format: TextFormat,
        path: PathBuf,
    ) -> JobOutcome {
        let mut out = match OutFile::create(path).await {
            Ok(out) => out,
            Err(e) => return JobOutcome::Failed(e),
        };
        let mut written = 0u64;
        let failure = match records {
            Records::Docs(mut reader) => {
                let json = format == TextFormat::Json;
                if json {
                    out.pending.push('[');
                }
                let mut failure = None;
                while let Some(batch) = reader.recv().await {
                    if job.is_cancelled() {
                        out.discard().await;
                        return JobOutcome::Cancelled;
                    }
                    let docs = match batch {
                        Ok(docs) => docs,
                        Err(e) => {
                            failure = Some(e);
                            break;
                        }
                    };
                    for doc in docs {
                        if json {
                            out.pending
                                .push_str(if written > 0 { ",\n  " } else { "\n  " });
                            out.pending.push_str(&doc.to_relaxed_json());
                        } else {
                            out.pending.push_str(&doc.to_relaxed_json());
                            out.pending.push('\n');
                        }
                        written += 1;
                    }
                    if let Err(e) = out.flush(false).await {
                        out.discard().await;
                        return JobOutcome::Failed(e);
                    }
                    job.progress(JobProgress {
                        rows: written,
                        bytes: out.written(),
                    });
                }
                if json {
                    out.pending.push_str("\n]\n");
                }
                failure
            }
            Records::Rows {
                columns,
                mut reader,
                ..
            } => {
                let mut encoder = Encoder::new(format, &columns, None).with_spreadsheet_safe();
                if let Some(formats) = self.export_formats.lock().clone() {
                    encoder = encoder.with_formats(formats, &columns);
                }
                encoder.header(&mut out.pending);
                let mut failure = None;
                while let Some(batch) = reader.recv().await {
                    if job.is_cancelled() {
                        out.discard().await;
                        return JobOutcome::Cancelled;
                    }
                    let rows = match batch {
                        Ok(rows) => rows,
                        Err(e) => {
                            failure = Some(e);
                            break;
                        }
                    };
                    let mut cells: Vec<Cell<'_>> = Vec::with_capacity(columns.len());
                    for (_, row) in &rows {
                        cells.clear();
                        cells.extend(
                            (0..columns.len())
                                .map(|c| row.get(c).map_or(Cell::Null, Value::as_cell)),
                        );
                        encoder.row(&cells, &mut out.pending);
                        written += 1;
                    }
                    if let Err(e) = out.flush(false).await {
                        out.discard().await;
                        return JobOutcome::Failed(e);
                    }
                    job.progress(JobProgress {
                        rows: written,
                        bytes: out.written(),
                    });
                }
                encoder.footer(&mut out.pending);
                failure
            }
        };
        if let Some(e) = failure {
            out.discard().await;
            return JobOutcome::Failed(DriverError::new(ErrorKind::Other, e));
        }
        if job.is_cancelled() {
            out.discard().await;
            return JobOutcome::Cancelled;
        }
        let bytes = out.written();
        match out.finish().await {
            Ok(path) => {
                job.progress(JobProgress {
                    rows: written,
                    bytes,
                });
                JobOutcome::Exported {
                    rows: written,
                    path,
                }
            }
            Err(e) => JobOutcome::Failed(e),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the job, session, I/O, records, table, mapping, options and optional new table are independent borrows of the transfer pipeline; grouping them would need a struct built at the single call site"
    )]
    async fn records_to_table(
        self: &Arc<Self>,
        job: &JobControl,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        records: Records,
        table: &TableRef,
        mapping: &[ColumnMapping],
        options: &ImportOptions,
        create: Option<NewTable>,
    ) -> JobOutcome {
        let Records::Rows {
            columns, reader, ..
        } = records
        else {
            return unsupported("Documents reach a table as rows");
        };
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let rows = RowSource {
            reader,
            columns: Some(&names),
            path: None,
        };
        self.import(job, key, io, rows, table, mapping, options, create)
            .await
    }
}
