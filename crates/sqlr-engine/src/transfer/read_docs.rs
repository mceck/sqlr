//! A collection or index as a transfer source: its documents, read through the driver's
//! stream in pages, as documents or as rows by the Table rule.

use std::sync::Arc;

use sqlr_core::{
    ColumnMeta, DocExecution, DocStream, DocTarget, DriverError, ErrorKind, doc_column_metas,
    doc_columns, doc_row,
};
use tokio::sync::mpsc;

use super::EXPORT_CHUNK;
use super::pipeline::Records;
use crate::engine::{Shared, not_connected};
use crate::session::SessionKey;

impl Shared {
    /// Opens `query` (else every document of `target`) on `conn`'s explorer session. With
    /// `docs`, documents are sent as they are; otherwise as rows under the columns of the
    /// first page.
    pub(crate) async fn collection_records(
        self: &Arc<Self>,
        conn: &str,
        target: &DocTarget,
        query: Option<&str>,
        docs: bool,
    ) -> Result<Records, DriverError> {
        let key = SessionKey::meta(conn);
        let io = self.live_io(conn).ok_or_else(not_connected)?;
        let connection = self.connected(&key, &io).await.ok_or_else(not_connected)?;
        let doc = connection.document().ok_or_else(|| {
            DriverError::new(ErrorKind::Unsupported, "this connection has no documents")
        })?;
        // `page` 0 asks the driver for every document (no limit).
        let text = match query {
            Some(q) if doc.classify(q) != sqlr_core::StatementClass::Read => {
                return Err(DriverError::new(
                    ErrorKind::Permission,
                    "Only a statement that only reads can be a transfer source",
                ));
            }
            Some(q) => q.to_string(),
            None => doc.open_target_text(target, 0),
        };
        let DocExecution::Docs(stream) = doc.run(target.database.as_deref(), &text).await? else {
            return Err(DriverError::new(
                ErrorKind::Other,
                "the statement returned no documents to transfer",
            ));
        };
        if docs {
            let (tx, rx) = mpsc::channel(2);
            tokio::spawn(send_docs(stream, tx));
            return Ok(Records::Docs(rx));
        }
        let mut stream = stream;
        let first = match stream.next_chunk(EXPORT_CHUNK).await {
            Ok(first) => first
                .map(|c| c.docs().collect::<Vec<_>>())
                .unwrap_or_default(),
            Err(e) => {
                let _ = stream.close().await;
                return Err(e);
            }
        };
        let names = doc_columns(&first);
        let columns: Vec<ColumnMeta> = doc_column_metas(&names).to_vec();
        let (tx, rx) = mpsc::channel(2);
        tokio::spawn(send_rows(stream, names, first, tx));
        Ok(Records::Rows {
            columns,
            reader: rx,
            typed_text: false,
        })
    }
}

async fn send_docs(mut stream: Box<dyn DocStream>, tx: mpsc::Sender<super::pipeline::DocBatch>) {
    loop {
        let batch = match stream.next_chunk(EXPORT_CHUNK).await {
            Ok(Some(chunk)) => Ok(chunk.docs().collect()),
            Ok(None) => break,
            Err(e) => Err(e.into()),
        };
        let failed = batch.is_err();
        if tx.send(batch).await.is_err() || failed {
            break;
        }
    }
    let _ = stream.close().await;
}

async fn send_rows(
    mut stream: Box<dyn DocStream>,
    names: Vec<String>,
    first: Vec<sqlr_core::DocValue>,
    tx: mpsc::Sender<super::RowBatch>,
) {
    let mut line = 0u64;
    let mut rows_of = |docs: Vec<sqlr_core::DocValue>| -> Vec<(u64, Vec<sqlr_core::Value>)> {
        docs.iter()
            .map(|d| {
                line += 1;
                (line, doc_row(&names, d))
            })
            .collect()
    };
    let mut pending = Some(rows_of(first));
    loop {
        let batch = match pending.take() {
            Some(rows) => Ok(rows),
            None => match stream.next_chunk(EXPORT_CHUNK).await {
                Ok(Some(chunk)) => Ok(rows_of(chunk.docs().collect())),
                Ok(None) => break,
                Err(e) => Err(e.into()),
            },
        };
        let failed = batch.is_err();
        if tx.send(batch).await.is_err() || failed {
            break;
        }
    }
    let _ = stream.close().await;
}
