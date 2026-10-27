//! Cancellable typed clipboard jobs. No clipboard encoding or database reads on the UI.

use std::sync::Arc;

use sqlr_core::{
    ClipboardBuilder, ClipboardSource, CsvOptions, DriverError, ErrorKind, Execution,
    StatementClass, TextFormat, Value,
};

use super::{TransferRun, Work, cancelled_or};
use crate::engine::{Command, Engine, Shared, not_connected};
use crate::job::{JobControl, JobId, JobKind, JobOutcome, JobProgress};
use crate::result::QueryId;
use crate::session::{SessionIo, SessionKey};

impl Engine {
    /// Encodes an immutable selection, including pending values and unselected holes.
    pub fn copy_grid(&self, q: QueryId, source: ClipboardSource, format: TextFormat) -> JobId {
        let job = self.shared.jobs.create(JobKind::Clipboard);
        let id = job.id;
        job.set_total(source.order.len() as u64);
        self.shared.send(Command::Transfer(TransferRun {
            job,
            work: Work::CopyGrid { q, source, format },
        }));
        self.shared.repaint();
        id
    }

    /// Reads every row of a read-only query again, preserving its filters and SQL limits.
    pub fn copy_query(&self, key: &SessionKey, database: Option<String>, sql: String) -> JobId {
        let job = self.shared.jobs.create(JobKind::Clipboard);
        let id = job.id;
        if let Some(io) = self.live_io(key) {
            self.shared.send(Command::Transfer(TransferRun {
                job,
                work: Work::CopyQuery {
                    key: key.clone(),
                    ticket: io.issue_ticket(),
                    io,
                    database,
                    sql,
                },
            }));
        } else {
            self.shared
                .jobs
                .finish(id, JobOutcome::Failed(not_connected()));
        }
        self.shared.repaint();
        id
    }
}

impl Shared {
    pub(super) async fn copy_grid(
        &self,
        job: &JobControl,
        q: QueryId,
        source: ClipboardSource,
        format: TextFormat,
    ) -> JobOutcome {
        let conn = self.result_connection(q);
        let control = job.clone();
        let result = tokio::task::spawn_blocking(move || {
            let dialect = conn
                .as_ref()
                .and_then(|c| c.relational())
                .map(|c| c.dialect());
            source.encode(format, dialect, |rows, bytes| {
                if rows.is_multiple_of(256) {
                    control.progress(JobProgress {
                        rows: rows as u64,
                        bytes: bytes as u64,
                    });
                }
                !control.is_cancelled()
            })
        })
        .await;
        match result {
            Ok(Ok(Some(data))) if !job.is_cancelled() => {
                job.progress(JobProgress {
                    rows: data.rows.len() as u64,
                    bytes: data.text.len() as u64,
                });
                job_result(data)
            }
            Ok(Ok(Some(_))) => JobOutcome::Cancelled,
            Ok(Ok(None)) => JobOutcome::Cancelled,
            Ok(Err(e)) => JobOutcome::Failed(DriverError::new(ErrorKind::Other, e.to_string())),
            Err(error) => JobOutcome::Failed(DriverError::new(ErrorKind::Other, error.to_string())),
        }
    }

    pub(super) async fn copy_query(
        self: &Arc<Self>,
        job: &JobControl,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        database: Option<&str>,
        sql: &str,
    ) -> JobOutcome {
        if job.is_cancelled() {
            return JobOutcome::Cancelled;
        }
        let conn = match self.session_connection(key, io, database).await {
            Ok(conn) => conn,
            Err(error) => return JobOutcome::Failed(error),
        };
        let Some(rel) = conn.relational() else {
            return JobOutcome::Failed(DriverError::new(
                ErrorKind::Unsupported,
                "This connection does not run SQL",
            ));
        };
        if rel.dialect().syntax().classify_script(sql) != StatementClass::Read {
            return JobOutcome::Failed(DriverError::new(
                ErrorKind::Permission,
                "Only a statement that only reads can be re-run to copy all results",
            ));
        }
        job.set_cancel(Some(conn.cancel_handle()));
        let result = async {
            let mut stream = match rel.execute(sql).await {
                Ok(Execution::Rows(stream)) => stream,
                Ok(Execution::Affected(_)) => {
                    return JobOutcome::Failed(DriverError::new(
                        ErrorKind::Other,
                        "The statement returned no rows",
                    ));
                }
                Err(error) => return cancelled_or(job, error),
            };
            let mut builder = ClipboardBuilder::new(
                stream.columns().to_vec(),
                TextFormat::Csv(CsvOptions::tsv()),
                Some(rel.dialect()),
            );
            let outcome = async {
                loop {
                    if job.is_cancelled() {
                        return JobOutcome::Cancelled;
                    }
                    let chunk = match stream.next_chunk(super::EXPORT_CHUNK).await {
                        Ok(Some(chunk)) => chunk,
                        Ok(None) => break,
                        Err(error) => return cancelled_or(job, error),
                    };
                    for row in 0..chunk.len() {
                        if job.is_cancelled() {
                            return JobOutcome::Cancelled;
                        }
                        let values = (0..chunk.columns().len())
                            .map(|col| Some(Value::from_cell(chunk.cell(row, col))))
                            .collect();
                        if let Err(e) = builder.push(values) {
                            return JobOutcome::Failed(DriverError::new(
                                ErrorKind::Other,
                                e.to_string(),
                            ));
                        }
                    }
                    job.progress(JobProgress {
                        rows: builder.rows() as u64,
                        bytes: builder.bytes() as u64,
                    });
                    self.repaint();
                }
                job_result(builder.finish())
            }
            .await;
            let close = stream.close().await;
            if job.is_cancelled() {
                return JobOutcome::Cancelled;
            }
            if let Err(error) = close
                && matches!(outcome, JobOutcome::GridCopied(_))
            {
                return cancelled_or(job, error);
            }
            outcome
        }
        .await;
        job.set_cancel(None);
        if let JobOutcome::Failed(error) = &result {
            self.on_error(key, io, error);
        }
        result
    }
}

fn job_result(data: sqlr_core::GridClipboard) -> JobOutcome {
    JobOutcome::GridCopied(Arc::new(data))
}
