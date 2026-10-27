//! Workbook and Parquet endpoints of a transfer: their rows are read on a blocking thread
//! into the same batches the other sources send, and written from batches on one.

use std::path::{Path, PathBuf};

use sqlr_core::{ColumnMeta, DriverError, ErrorKind, Value};
use tokio::sync::mpsc;

use super::error::TransferFileError;
use super::pipeline::{Records, text_columns};
use super::{RowBatch, parquet, partial_path, xlsx};
use crate::job::{JobControl, JobOutcome, JobProgress};

fn failed(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Other, message)
}

/// The rows of a workbook sheet: it is read whole (Excel caps a sheet at about a million
/// rows) and sent in batches of `batch`.
pub(super) async fn workbook_rows(
    path: PathBuf,
    sheet: Option<String>,
    header: bool,
    batch: usize,
) -> Result<Records, DriverError> {
    let loaded =
        tokio::task::spawn_blocking(move || xlsx::read_sheet(&path, sheet.as_deref(), header))
            .await
            .map_err(|e| failed(e.to_string()))?
            .map_err(|e| failed(e.to_string()))?;
    let (names, rows) = loaded;
    let (tx, rx) = mpsc::channel::<RowBatch>(2);
    let first_line = if header { 2 } else { 1 };
    tokio::spawn(async move {
        let mut chunk = Vec::with_capacity(batch);
        for (line, row) in (first_line..).zip(rows) {
            chunk.push((line, row));
            if chunk.len() >= batch && tx.send(Ok(std::mem::take(&mut chunk))).await.is_err() {
                return;
            }
        }
        if !chunk.is_empty() {
            let _ = tx.send(Ok(chunk)).await;
        }
    });
    Ok(Records::Rows {
        columns: text_columns(&names),
        reader: rx,
        typed_text: false,
    })
}

/// The rows of a Parquet file, streamed in batches of `batch`.
pub(super) async fn parquet_rows(path: PathBuf, batch: usize) -> Result<Records, DriverError> {
    let probe = path.clone();
    let names = tokio::task::spawn_blocking(move || parquet::column_names(&probe))
        .await
        .map_err(|e| failed(e.to_string()))?
        .map_err(|e| failed(e.to_string()))?;
    let (tx, rx) = mpsc::channel::<RowBatch>(2);
    tokio::task::spawn_blocking(move || {
        let mut chunk = Vec::with_capacity(batch);
        let outcome = parquet::read_rows(&path, |line, row| {
            chunk.push((line, row));
            chunk.len() < batch || tx.blocking_send(Ok(std::mem::take(&mut chunk))).is_ok()
        });
        if !chunk.is_empty() {
            let _ = tx.blocking_send(Ok(chunk));
        }
        if let Err(e) = outcome {
            let _ = tx.blocking_send(Err(e));
        }
    });
    Ok(Records::Rows {
        columns: text_columns(&names),
        reader: rx,
        typed_text: false,
    })
}

/// A file written row by row on a blocking thread.
trait RowSink: Send {
    fn row(&mut self, row: Vec<Value>) -> Result<(), TransferFileError>;
    fn finish(self: Box<Self>) -> Result<(), TransferFileError>;
}

struct XlsxSink {
    writer: xlsx::SheetWriter,
    path: PathBuf,
}

impl RowSink for XlsxSink {
    fn row(&mut self, row: Vec<Value>) -> Result<(), TransferFileError> {
        self.writer.write_row(&row)
    }

    fn finish(self: Box<Self>) -> Result<(), TransferFileError> {
        let Self { writer, path } = *self;
        writer.save(&path)
    }
}

struct ParquetSink(parquet::ParquetWriter);

impl RowSink for ParquetSink {
    fn row(&mut self, row: Vec<Value>) -> Result<(), TransferFileError> {
        self.0.write_row(row)
    }

    fn finish(self: Box<Self>) -> Result<(), TransferFileError> {
        self.0.finish()
    }
}

/// Writes `records` to a workbook at `path` (through a partial file, renamed when done).
pub(super) async fn records_to_workbook(
    job: &JobControl,
    records: Records,
    path: PathBuf,
) -> JobOutcome {
    let Records::Rows {
        columns, reader, ..
    } = records
    else {
        return JobOutcome::Failed(failed("Documents are written as JSON, not as a workbook"));
    };
    let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    let partial = partial_path(&path);
    let sink = match xlsx::SheetWriter::new(&names) {
        Ok(writer) => Box::new(XlsxSink {
            writer,
            path: partial.clone(),
        }) as Box<dyn RowSink>,
        Err(e) => return JobOutcome::Failed(failed(e.to_string())),
    };
    sink_rows(job, reader, sink, partial, path).await
}

/// Writes `records` to a Parquet file at `path`.
pub(super) async fn records_to_parquet(
    job: &JobControl,
    records: Records,
    path: PathBuf,
) -> JobOutcome {
    let Records::Rows {
        columns, reader, ..
    } = records
    else {
        return JobOutcome::Failed(failed("Documents are written as JSON, not as Parquet"));
    };
    let partial = partial_path(&path);
    let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    let kinds = parquet_kinds(&columns);
    let sink = match parquet::ParquetWriter::create(&partial, names, kinds) {
        Ok(writer) => Box::new(ParquetSink(writer)) as Box<dyn RowSink>,
        Err(e) => return JobOutcome::Failed(failed(e.to_string())),
    };
    sink_rows(job, reader, sink, partial, path).await
}

fn parquet_kinds(columns: &[ColumnMeta]) -> Vec<parquet::Kind> {
    columns.iter().map(|c| parquet::Kind::of(c.kind)).collect()
}

/// Feeds batches to a sink on a blocking thread, then moves its file into place; a failure
/// or a cancel removes the partial file.
async fn sink_rows(
    job: &JobControl,
    mut reader: mpsc::Receiver<RowBatch>,
    mut sink: Box<dyn RowSink>,
    partial: PathBuf,
    path: PathBuf,
) -> JobOutcome {
    let (tx, mut rx) = mpsc::channel::<Vec<Vec<Value>>>(2);
    let writer = tokio::task::spawn_blocking(move || {
        while let Some(rows) = rx.blocking_recv() {
            for row in rows {
                sink.row(row)?;
            }
        }
        sink.finish()
    });
    let mut written = 0u64;
    let mut failure: Option<TransferFileError> = None;
    let mut cancelled = false;
    while let Some(batch) = reader.recv().await {
        if job.is_cancelled() {
            cancelled = true;
            break;
        }
        match batch {
            Ok(rows) => {
                written += rows.len() as u64;
                let rows: Vec<Vec<Value>> = rows.into_iter().map(|(_, r)| r).collect();
                if tx.send(rows).await.is_err() {
                    // The writer stopped: its error comes out of the join below.
                    break;
                }
                job.progress(JobProgress {
                    rows: written,
                    bytes: 0,
                });
            }
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }
    drop(tx);
    let wrote = writer
        .await
        .map_err(TransferFileError::from)
        .and_then(|r| r);
    let remove = |p: &Path| {
        let _ = std::fs::remove_file(p);
    };
    if cancelled {
        remove(&partial);
        return JobOutcome::Cancelled;
    }
    if let Some(e) = failure.or(wrote.err()) {
        remove(&partial);
        return JobOutcome::Failed(failed(e.to_string()));
    }
    if let Err(e) = tokio::fs::rename(&partial, &path).await {
        remove(&partial);
        return JobOutcome::Failed(failed(format!("{}: {e}", path.display())));
    }
    let bytes = tokio::fs::metadata(&path).await.map_or(0, |m| m.len());
    job.progress(JobProgress {
        rows: written,
        bytes,
    });
    JobOutcome::Exported {
        rows: written,
        path,
    }
}
