//! The document family of the driver API (MongoDB, Elasticsearch): spec §5.2.

use std::ops::Range;

use crate::classify::StatementClass;
use crate::doc::DocValue;
use crate::docchunk::DocChunk;
use crate::driver::RowStream;
use crate::error::{DriverError, ErrorKind};
use crate::lang::QueryLanguage;
use crate::node::NodePath;

/// The error a read-only session returns for a statement that may write.
pub fn read_only_error() -> DriverError {
    DriverError::new(ErrorKind::Permission, "The session is read-only")
}

/// A collection (MongoDB) or an index, alias or data stream (Elasticsearch).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DocTarget {
    /// The database (MongoDB); `None` for the connection's current one, and always for
    /// Elasticsearch.
    pub database: Option<String>,
    /// The collection, index, alias or data stream name.
    pub collection: String,
}

/// What running one statement or request produced.
pub enum DocExecution {
    /// A stream of documents (a Doc chunk source).
    Docs(Box<dyn DocStream>),
    /// Tabular results: ES|QL, SQL, `distinct`, `show dbs`.
    Rows(Box<dyn RowStream>),
    /// A write statement's outcome.
    Written {
        /// Documents the filter matched.
        matched: u64,
        /// Documents actually changed.
        modified: u64,
        /// Documents inserted (or upserted).
        inserted: u64,
        /// Documents deleted.
        deleted: u64,
        /// The `_id`s of inserted (or upserted) documents.
        ids: Vec<DocValue>,
    },
    /// A single value: a count, a command reply, a response body.
    Value(DocValue),
}

/// Pages of documents produced by one statement, read on demand.
#[async_trait::async_trait]
pub trait DocStream: Send {
    /// The next page of at most `max` documents; `Ok(None)` once the result is exhausted.
    async fn next_chunk(&mut self, max: usize) -> Result<Option<DocChunk>, DriverError>;
    /// Releases server resources (cursor, point in time). Idempotent.
    async fn close(&mut self) -> Result<(), DriverError>;
    /// Messages for the user since the last call, such as a search restarted after its
    /// point in time expired.
    fn take_notices(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// The document-family connection of a driver: runs statements and edits single documents.
#[async_trait::async_trait]
pub trait DocumentConn: Send + Sync {
    /// [`QueryLanguage::MongoShell`] or [`QueryLanguage::EsConsole`].
    fn language(&self) -> QueryLanguage;
    /// Runs one statement (Mongo) or request (Elasticsearch). `database` overrides the
    /// connection's current database for this call.
    async fn run(&self, database: Option<&str>, text: &str) -> Result<DocExecution, DriverError>;
    /// Byte ranges of the statements in `text`.
    fn split(&self, text: &str) -> Vec<Range<usize>> {
        self.language().split(text)
    }
    /// Replaces the document with `id`; the number of documents matched (0 or 1).
    async fn replace_doc(
        &self,
        target: &DocTarget,
        id: &DocValue,
        doc: &DocValue,
    ) -> Result<u64, DriverError>;
    /// Inserts `doc`; returns its `_id` (generated when `doc` has none).
    async fn insert_doc(&self, target: &DocTarget, doc: &DocValue)
    -> Result<DocValue, DriverError>;
    /// Deletes the document with `id`; the number deleted (0 or 1).
    async fn delete_doc(&self, target: &DocTarget, id: &DocValue) -> Result<u64, DriverError>;
    /// Whether `target` is a read-only view (MongoDB): its documents cannot be edited.
    async fn is_view(&self, _target: &DocTarget) -> Result<bool, DriverError> {
        Ok(false)
    }
    /// Index descriptions (MongoDB `listIndexes`; Elasticsearch: mappings and settings).
    async fn indexes(&self, target: &DocTarget) -> Result<Vec<DocValue>, DriverError>;
    /// The text that opens `target`'s documents, `page` at a time.
    fn open_target_text(&self, target: &DocTarget, page: usize) -> String;
    /// The collection (or index, alias, data stream) the explorer node at `path` stands
    /// for, or holds (an index node: its collection); `None` for other nodes.
    fn node_target(&self, _path: &NodePath) -> Option<DocTarget> {
        None
    }
    /// The class of the first statement in `text`, for read-only sessions and
    /// confirmations; text that does not parse is [`StatementClass::Other`].
    fn classify(&self, text: &str) -> StatementClass;
    /// Turns read-only mode on or off: every statement that may write is refused with
    /// [`read_only_error`].
    fn set_read_only(&self, on: bool);
}
