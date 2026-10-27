//! A collection or index as a transfer target: rows become flat documents (one field per
//! mapped column), documents are inserted as they are.
//!
//! Documents go in batches of the import's batch size, one `insertMany` (MongoDB) or
//! `_bulk` request (Elasticsearch) each, through the driver's own statement path. There is
//! no transaction: after a failure the documents of earlier batches stay, and the error says
//! how many. With "skip bad rows", a failed batch is inserted again one document at a time
//! and the documents that fail are reported.

use std::sync::Arc;

use sqlr_core::{
    DocTarget, DocValue, DocumentConn, DriverError, ErrorKind, insert_many_text, row_to_doc,
};

use super::pipeline::Records;
use super::{ImportOptions, MAX_REJECTED};
use crate::engine::{Shared, not_connected};
use crate::job::{JobControl, JobOutcome, JobProgress};
use crate::session::{SessionIo, SessionKey};
use crate::tx::READ_ONLY;

/// How a batch insert went.
struct Batches<'a> {
    doc: &'a dyn DocumentConn,
    target: &'a DocTarget,
    skip_bad: bool,
    written: u64,
    rejected: Vec<(u64, String)>,
}

impl Batches<'_> {
    /// Inserts `docs` (each with its line); an error stops the transfer.
    async fn insert(&mut self, docs: Vec<(u64, DocValue)>) -> Result<(), DriverError> {
        // A document sqlr cannot write back without loss is never inserted.
        let mut kept = Vec::with_capacity(docs.len());
        for (line, d) in docs {
            match d.unmodelled() {
                None => kept.push((line, d)),
                Some(what) => {
                    let reason = format!(
                        "document {line} holds {what}, which sqlr cannot write without loss"
                    );
                    if !self.skip_bad {
                        return Err(DriverError::new(ErrorKind::Unsupported, reason));
                    }
                    if self.rejected.len() < MAX_REJECTED {
                        self.rejected.push((line, reason));
                    }
                }
            }
        }
        let docs = kept;
        if docs.is_empty() {
            return Ok(());
        }
        let only: Vec<DocValue> = docs.iter().map(|(_, d)| d.clone()).collect();
        let text = insert_many_text(self.doc.language(), self.target, &only)
            .map_err(|e| DriverError::new(ErrorKind::Other, e))?;
        match self.doc.run(self.target.database.as_deref(), &text).await {
            Ok(_) => {
                self.written += docs.len() as u64;
                Ok(())
            }
            Err(e) if !self.skip_bad || e.kind == ErrorKind::Connection => Err(e),
            Err(_) => {
                // Again one at a time, to find the documents that fail.
                for (line, d) in docs {
                    match self.doc.insert_doc(self.target, &d).await {
                        Ok(_) => self.written += 1,
                        Err(e) if e.kind == ErrorKind::Connection => return Err(e),
                        Err(e) => {
                            if self.rejected.len() < MAX_REJECTED {
                                self.rejected.push((line, e.message));
                            }
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

impl Shared {
    #[expect(
        clippy::too_many_arguments,
        reason = "the job, session, I/O, records, target, mapping and options are independent borrows of the transfer pipeline; grouping them would need a struct built at the single call site"
    )]
    pub(crate) async fn records_to_collection(
        self: &Arc<Self>,
        job: &JobControl,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        records: Records,
        target: &DocTarget,
        mapping: &[sqlr_core::ColumnMapping],
        options: &ImportOptions,
    ) -> JobOutcome {
        job.progress(JobProgress { rows: 0, bytes: 0 });
        {
            let mut inner = self.inner.lock();
            match inner.session(key, io) {
                None => return JobOutcome::Failed(not_connected()),
                Some(s) if s.tx.read_only => {
                    return JobOutcome::Failed(DriverError::new(ErrorKind::Permission, READ_ONLY));
                }
                Some(_) => {}
            }
        }
        if options.truncate_first {
            return JobOutcome::Failed(DriverError::new(
                ErrorKind::Unsupported,
                "Deleting a collection's documents first is not offered: drop it or delete them \
                 with a statement",
            ));
        }
        let conn = match self.session_connection(key, io, None).await {
            Ok(conn) => conn,
            Err(e) => return JobOutcome::Failed(e),
        };
        let Some(doc) = conn.document() else {
            return JobOutcome::Failed(DriverError::new(
                ErrorKind::Unsupported,
                "this connection has no documents",
            ));
        };
        job.set_cancel(Some(conn.cancel_handle()));
        let batch = options.batch.max(1);
        let mut out = Batches {
            doc,
            target,
            skip_bad: options.skip_bad_rows,
            written: 0,
            rejected: Vec::new(),
        };
        let mut read = 0u64;
        let mut pending: Vec<(u64, DocValue)> = Vec::new();
        let outcome: Result<(), DriverError> = async {
            match records {
                Records::Docs(mut reader) => {
                    while let Some(docs) = reader.recv().await {
                        let docs = docs.map_err(|e| DriverError::new(ErrorKind::Other, e))?;
                        for d in docs {
                            read += 1;
                            pending.push((read, d));
                            if pending.len() >= batch {
                                out.insert(std::mem::take(&mut pending)).await?;
                                job.progress(JobProgress {
                                    rows: read,
                                    bytes: 0,
                                });
                            }
                        }
                        if job.is_cancelled() {
                            return Ok(());
                        }
                    }
                }
                Records::Rows {
                    columns,
                    mut reader,
                    typed_text,
                } => {
                    // Without a mapping, every column under its own name.
                    let fields: Vec<(usize, String)> = if mapping.is_empty() {
                        columns
                            .iter()
                            .enumerate()
                            .map(|(i, c)| (i, c.name.clone()))
                            .collect()
                    } else {
                        mapping
                            .iter()
                            .filter_map(|m| Some((m.source, m.target.clone()?)))
                            .collect()
                    };
                    if fields.is_empty() {
                        return Err(DriverError::new(
                            ErrorKind::Other,
                            "No source column is mapped to a field",
                        ));
                    }
                    while let Some(rows) = reader.recv().await {
                        let rows = rows.map_err(|e| DriverError::new(ErrorKind::Other, e))?;
                        for (line, row) in rows {
                            read += 1;
                            pending.push((line, row_to_doc(&fields, &row, typed_text)));
                            if pending.len() >= batch {
                                out.insert(std::mem::take(&mut pending)).await?;
                                job.progress(JobProgress {
                                    rows: read,
                                    bytes: 0,
                                });
                            }
                        }
                        if job.is_cancelled() {
                            return Ok(());
                        }
                    }
                }
            }
            out.insert(std::mem::take(&mut pending)).await
        }
        .await;
        job.set_cancel(None);
        if job.is_cancelled() {
            return JobOutcome::Cancelled;
        }
        match outcome {
            Ok(()) => {
                job.progress(JobProgress {
                    rows: read,
                    bytes: 0,
                });
                JobOutcome::Imported {
                    read,
                    written: out.written,
                    rejected: out.rejected,
                }
            }
            Err(e) => {
                self.on_error(key, io, &e);
                let kept = match out.written {
                    0 => "; nothing was inserted".to_string(),
                    n => format!("; {n} documents inserted before the failure stay"),
                };
                JobOutcome::Failed(DriverError::new(e.kind, format!("{}{kept}", e.message)))
            }
        }
    }
}
