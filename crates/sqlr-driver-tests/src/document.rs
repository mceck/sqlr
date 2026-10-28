//! Reusable checks for the Document Family through the public connection interface.
use crate::{Connect, SuiteReport};
use sqlr_core::{
    DocExecution, DocTarget, DocValue, DocumentConn, DriverError, ErrorKind, NodePath,
};
use std::time::Duration;

/// A Document connection factory and statements for the four execution outcomes.
/// Setup creates 25 documents, ordered by `_id`, with `_id` and `n` from 1 to 25.
/// The view names a read-only view of that collection; the write inserts id 42 elsewhere.
pub struct DocumentTarget {
    pub connect: Connect,
    pub target: DocTarget,
    pub view: DocTarget,
    pub setup: Vec<String>,
    pub docs: String,
    pub rows: String,
    pub write: String,
    pub value: String,
    /// A slow statement to cancel; absent when server cancellation is unavailable.
    pub slow: Option<String>,
}

fn require(ok: bool, message: &str) -> Result<(), DriverError> {
    if ok {
        Ok(())
    } else {
        Err(DriverError::new(ErrorKind::Other, message))
    }
}

async fn collect(
    doc: &dyn DocumentConn,
    text: &str,
    page: usize,
) -> Result<(Vec<usize>, Vec<DocValue>), DriverError> {
    let DocExecution::Docs(mut stream) = doc.run(None, text).await? else {
        return Err(DriverError::new(
            ErrorKind::Other,
            "expected a document stream",
        ));
    };
    let mut sizes = Vec::new();
    let mut docs = Vec::new();
    while let Some(chunk) = stream.next_chunk(page).await? {
        sizes.push(chunk.len());
        docs.extend(chunk.docs());
    }
    stream.close().await?;
    Ok((sizes, docs))
}

/// Runs the same checks for in-process and Sidecar Document drivers.
pub async fn document_suite(target: &DocumentTarget) -> SuiteReport {
    let mut report = SuiteReport::default();
    macro_rules! check {
        ($name:literal, $body:expr) => {
            match $body.await {
                Ok(()) => report.passed.push($name.into()),
                Err(e) => report.failed.push(($name.into(), e.to_string())),
            }
        };
    }
    let conn = match (target.connect)().await {
        Ok(conn) => {
            report.passed.push("connect".into());
            conn
        }
        Err(e) => {
            report.failed.push(("connect".into(), e.to_string()));
            return report;
        }
    };
    let Some(doc) = conn.document() else {
        report
            .failed
            .push(("document".into(), "no Document interface".into()));
        return report;
    };
    check!("setup", async {
        for text in &target.setup {
            doc.run(None, text).await?;
        }
        Ok::<_, DriverError>(())
    });
    if !report.failed.is_empty() {
        return report;
    }
    check!("ping", async {
        require(!conn.ping().await?.product.is_empty(), "empty product name")
    });
    check!("browse", async {
        conn.browse(&NodePath::root()).await.map(|_| ())
    });
    check!("run_each_outcome", async {
        let (_, docs) = collect(doc, &target.docs, 100).await?;
        require(docs.len() == 25, "expected 25 documents")?;
        let DocExecution::Rows(mut rows) = doc.run(None, &target.rows).await? else {
            return Err(DriverError::new(ErrorKind::Other, "expected rows"));
        };
        require(!rows.columns().is_empty(), "expected columns")?;
        let mut count = 0;
        while let Some(chunk) = rows.next_chunk(7).await? {
            count += chunk.len();
        }
        rows.close().await?;
        require(count == 25, "expected 25 distinct rows")?;
        require(
            matches!(doc.run(None, &target.write).await?, DocExecution::Written {
            inserted: 1, ids, .. } if ids == [DocValue::Int32(42)]),
            "expected one inserted id 42",
        )?;
        require(
            matches!(
                doc.run(None, &target.value).await?,
                DocExecution::Value(DocValue::Int32(25) | DocValue::Int64(25))
            ),
            "expected count 25",
        )
    });
    check!("page_documents", async {
        for (page, expected) in [(10, vec![10, 10, 5]), (7, vec![7, 7, 7, 4])] {
            let (sizes, docs) = collect(doc, &target.docs, page).await?;
            require(sizes == expected, "unexpected document page sizes")?;
            require(
                docs[0].get("_id") == Some(&DocValue::Int32(1))
                    && docs[24].get("_id") == Some(&DocValue::Int32(25)),
                "document order changed",
            )?;
        }
        let DocExecution::Docs(mut stream) = doc.run(None, &target.docs).await? else {
            unreachable!()
        };
        stream.next_chunk(1).await?;
        stream.close().await?;
        stream.close().await?;
        require(
            stream.next_chunk(1).await?.is_none(),
            "closed cursor still returns documents",
        )
    });
    check!("write_by_id", async {
        let id = DocValue::ObjectId([0x71; 12]);
        let original = DocValue::Document(vec![
            ("_id".into(), id.clone()),
            ("n".into(), DocValue::Int64(500)),
        ]);
        require(
            doc.insert_doc(&target.target, &original).await? == id,
            "insert changed id",
        )?;
        let replacement = DocValue::Document(vec![
            ("_id".into(), id.clone()),
            ("n".into(), DocValue::Int64(600)),
            (
                "typed".into(),
                DocValue::Binary {
                    subtype: 128,
                    bytes: vec![1, 2, 3],
                },
            ),
        ]);
        require(
            doc.replace_doc(&target.target, &id, &replacement).await? == 1,
            "replace matched no document",
        )?;
        let (_, docs) = collect(doc, &target.docs, 100).await?;
        require(
            docs.iter().find(|d| d.get("_id") == Some(&id)) == Some(&replacement),
            "replacement lost typed fields",
        )?;
        require(
            doc.delete_doc(&target.target, &id).await? == 1,
            "delete matched no document",
        )?;
        require(
            doc.delete_doc(&target.target, &id).await? == 0,
            "delete was not idempotent",
        )
    });
    check!("is_view", async {
        require(
            doc.is_view(&target.view).await? && !doc.is_view(&target.target).await?,
            "view detection differs",
        )
    });
    check!("indexes", async {
        require(
            doc.indexes(&target.target)
                .await?
                .iter()
                .any(|v| v.get("name") == Some(&DocValue::String("_id_".into()))),
            "missing id index",
        )
    });
    check!("read_only", async {
        let id = DocValue::Int32(1);
        let value = DocValue::Document(vec![("_id".into(), id.clone())]);
        doc.set_read_only(true);
        let results = [
            doc.insert_doc(&target.target, &value).await.map(|_| ()),
            doc.replace_doc(&target.target, &id, &value)
                .await
                .map(|_| ()),
            doc.delete_doc(&target.target, &id).await.map(|_| ()),
            doc.run(None, &target.write).await.map(|_| ()),
        ];
        let readable = doc.run(None, &target.value).await.is_ok();
        doc.set_read_only(false);
        require(
            results
                .iter()
                .all(|r| r.as_ref().is_err_and(|e| e.kind == ErrorKind::Permission))
                && readable,
            "read-only did not refuse every write and allow reads",
        )
    });
    if let Some(slow) = &target.slow {
        check!("cancel", async {
            let runner = conn.clone();
            let text = slow.clone();
            let running =
                tokio::spawn(async move { runner.document().unwrap().run(None, &text).await });
            tokio::time::sleep(Duration::from_millis(200)).await;
            conn.cancel_handle().cancel().await?;
            let result = tokio::time::timeout(Duration::from_secs(15), running)
                .await
                .map_err(|_| DriverError::new(ErrorKind::Other, "cancel timed out"))?
                .map_err(|_| DriverError::new(ErrorKind::Other, "query task panicked"))?;
            require(
                result.is_err_and(|e| e.kind == ErrorKind::Cancelled),
                "slow statement was not cancelled",
            )?;
            require(
                !conn.ping().await?.product.is_empty(),
                "connection lost after cancel",
            )
        });
    } else {
        report
            .skipped
            .push(("cancel".into(), "no slow statement".into()));
    }
    report
}
