//! A fake document-family driver: a collection held in memory.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use sqlr_core::{
    CancelHandle, Capabilities, Connection, ConnectionConfig, DocChunk, DocExecution, DocStream,
    DocTarget, DocValue, DocumentConn, Driver, DriverError, DriverManifest, ErrorKind, Family,
    MongoOp, Node, NodePath, QueryLanguage, Secrets, ServerInfo, StatementClass, parse_mongo_shell,
    read_only_error,
};

#[derive(Clone, Default)]
pub struct FakeDocScript {
    /// The collection: `run` returns all of it as documents; edits change it by `_id`.
    pub docs: Vec<DocValue>,
    /// `browse(path)` returns the nodes whose path is a direct child of `path`, in order.
    pub tree: Vec<Node>,
    /// Returned by `run` instead of documents.
    pub run_error: Option<DriverError>,
    /// An `insertMany` run inserts this many documents and then fails, as a partially
    /// applied ordered batch does.
    pub partial_insert_before_error: Option<usize>,
    /// When set, `run` returns this value instead of documents.
    pub value: Option<DocValue>,
    /// The collection names that are views (`is_view`).
    pub views: Vec<String>,
}

struct Shared {
    docs: Mutex<Vec<DocValue>>,
    script: FakeDocScript,
    runs: Mutex<Vec<(Option<String>, String)>>,
    read_only: AtomicBool,
}

pub struct FakeDocDriver {
    manifest: DriverManifest,
    shared: Arc<Shared>,
}

impl FakeDocDriver {
    pub fn new(script: FakeDocScript) -> Arc<Self> {
        Arc::new(Self {
            manifest: DriverManifest {
                id: "fakedoc".into(),
                name: "Fake documents".into(),
                family: Family::Document,
                capabilities: Capabilities {
                    editable_results: true,
                    ..Default::default()
                },
                fields: Vec::new(),
                dialect: "mongodb".into(),
                url_schemes: Vec::new(),
                default_port: None,
                editing: None,
            },
            shared: Arc::new(Shared {
                docs: Mutex::new(script.docs.clone()),
                script,
                runs: Mutex::new(Vec::new()),
                read_only: AtomicBool::new(false),
            }),
        })
    }

    /// Every `run` call as `(database, text)`, in order.
    pub fn runs(&self) -> Vec<(Option<String>, String)> {
        self.shared.runs.lock().clone()
    }

    /// Whether the connections are in read-only mode (`DocumentConn::set_read_only`).
    pub fn read_only(&self) -> bool {
        self.shared.read_only.load(Ordering::SeqCst)
    }

    /// The collection as it is now.
    pub fn docs(&self) -> Vec<DocValue> {
        self.shared.docs.lock().clone()
    }

    /// Replaces the collection (another client changed it).
    pub fn set_docs(&self, docs: Vec<DocValue>) {
        *self.shared.docs.lock() = docs;
    }
}

#[async_trait::async_trait]
impl Driver for FakeDocDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    async fn connect(
        &self,
        _cfg: &ConnectionConfig,
        _secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        Ok(Arc::new(FakeDocConnection {
            shared: self.shared.clone(),
        }))
    }
}

struct FakeDocConnection {
    shared: Arc<Shared>,
}

struct NoCancel;

#[async_trait::async_trait]
impl CancelHandle for NoCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl Connection for FakeDocConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        Ok(ServerInfo {
            product: "FakeDoc".into(),
            version: "1".into(),
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        let depth = path.segments().len();
        Ok(self
            .shared
            .script
            .tree
            .iter()
            .filter(|n| {
                n.path.segments().len() == depth + 1
                    && n.path.segments()[..depth] == *path.segments()
            })
            .cloned()
            .collect())
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(NoCancel)
    }

    fn document(&self) -> Option<&dyn DocumentConn> {
        Some(self)
    }
}

impl FakeDocConnection {
    fn check_write(&self) -> Result<(), DriverError> {
        if self.shared.read_only.load(Ordering::SeqCst) {
            Err(read_only_error())
        } else {
            Ok(())
        }
    }

    fn position(&self, id: &DocValue) -> Option<usize> {
        self.shared
            .docs
            .lock()
            .iter()
            .position(|d| d.get("_id") == Some(id))
    }
}

struct VecDocStream {
    docs: Vec<DocValue>,
    at: usize,
}

#[async_trait::async_trait]
impl DocStream for VecDocStream {
    async fn next_chunk(&mut self, max: usize) -> Result<Option<DocChunk>, DriverError> {
        if self.at >= self.docs.len() {
            return Ok(None);
        }
        let end = (self.at + max.max(1)).min(self.docs.len());
        let chunk = DocChunk::from_docs(&self.docs[self.at..end]);
        self.at = end;
        Ok(Some(chunk))
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        self.at = self.docs.len();
        Ok(())
    }
}

#[async_trait::async_trait]
impl DocumentConn for FakeDocConnection {
    fn language(&self) -> QueryLanguage {
        QueryLanguage::MongoShell
    }

    async fn run(&self, database: Option<&str>, text: &str) -> Result<DocExecution, DriverError> {
        self.shared
            .runs
            .lock()
            .push((database.map(String::from), text.to_string()));
        if self.classify(text).modifies() {
            self.check_write()?;
        }
        if let Some(e) = &self.shared.script.run_error {
            return Err(e.clone());
        }
        if let Some(v) = &self.shared.script.value {
            return Ok(DocExecution::Value(v.clone()));
        }
        // `insertMany` (what transfers into a collection run) adds the documents.
        if let Ok(ops) = parse_mongo_shell(text)
            && let Some((_, MongoOp::InsertMany { docs, .. })) = ops.first()
        {
            if let Some(n) = self.shared.script.partial_insert_before_error {
                let n = n.min(docs.len());
                self.shared.docs.lock().extend(docs[..n].iter().cloned());
                return Err(DriverError::new(
                    ErrorKind::Other,
                    "the batch failed after a prefix",
                ));
            }
            let ids = docs
                .iter()
                .map(|d| d.get("_id").cloned().unwrap_or(DocValue::Null))
                .collect();
            self.shared.docs.lock().extend(docs.iter().cloned());
            return Ok(DocExecution::Written {
                matched: 0,
                modified: 0,
                inserted: docs.len() as u64,
                deleted: 0,
                ids,
            });
        }
        Ok(DocExecution::Docs(Box::new(VecDocStream {
            docs: self.shared.docs.lock().clone(),
            at: 0,
        })))
    }

    fn split(&self, text: &str) -> Vec<Range<usize>> {
        QueryLanguage::MongoShell.split(text)
    }

    async fn is_view(&self, target: &DocTarget) -> Result<bool, DriverError> {
        Ok(self.shared.script.views.contains(&target.collection))
    }

    async fn replace_doc(
        &self,
        _target: &DocTarget,
        id: &DocValue,
        doc: &DocValue,
    ) -> Result<u64, DriverError> {
        self.check_write()?;
        let Some(i) = self.position(id) else {
            return Ok(0);
        };
        self.shared.docs.lock()[i] = doc.clone();
        Ok(1)
    }

    async fn insert_doc(
        &self,
        _target: &DocTarget,
        doc: &DocValue,
    ) -> Result<DocValue, DriverError> {
        self.check_write()?;
        let id = doc.get("_id").cloned().unwrap_or(DocValue::Null);
        self.shared.docs.lock().push(doc.clone());
        Ok(id)
    }

    async fn delete_doc(&self, _target: &DocTarget, id: &DocValue) -> Result<u64, DriverError> {
        self.check_write()?;
        let Some(i) = self.position(id) else {
            return Ok(0);
        };
        self.shared.docs.lock().remove(i);
        Ok(1)
    }

    async fn indexes(&self, _target: &DocTarget) -> Result<Vec<DocValue>, DriverError> {
        Ok(vec![DocValue::Document(vec![(
            "name".into(),
            DocValue::String("_id_".into()),
        )])])
    }

    fn open_target_text(&self, target: &DocTarget, page: usize) -> String {
        format!("db.{}.find({{}}).limit({page})", target.collection)
    }

    /// MongoDB's layout: `[db, folder, collection]` and `[db, folder, collection, index]`.
    fn node_target(&self, path: &NodePath) -> Option<DocTarget> {
        match path.segments() {
            [db, _, coll] | [db, _, coll, _] => Some(DocTarget {
                database: Some(db.clone()),
                collection: coll.clone(),
            }),
            _ => None,
        }
    }

    fn classify(&self, text: &str) -> StatementClass {
        match parse_mongo_shell(text) {
            Ok(ops) => ops
                .first()
                .map_or(StatementClass::Other, |(_, op)| op.class()),
            Err(_) => StatementClass::Other,
        }
    }

    fn set_read_only(&self, on: bool) {
        self.shared.read_only.store(on, Ordering::SeqCst);
    }
}
