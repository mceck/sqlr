//! Document results and edits (spec §5.2 document family, §7.3, §7.4, §7.7).
//!
//! A document statement runs on a session in its execute order, like SQL. Its outcome
//! becomes a [`DocResult`]: pages of documents ([`DocChunk`]s) fetched `page_size` at a time
//! from the driver's stream, with "fetch more" and "fetch all" bounded by the result's
//! memory budget. Tabular outcomes (`distinct`, ES|QL, SQL) are ordinary rows results
//! (`Engine::result(q)`), values and write counts one document each.
//!
//! Edits are general jobs: a document is replaced by its `_id` (and, on Elasticsearch, by
//! the concrete `_index` of its hit, never an alias or pattern), inserted, or deleted.
//! Read-only sessions refuse them, and a document holding a type sqlr cannot write back
//! (JavaScript code, symbols, DBPointers) is never saved.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use sqlr_core::{
    CancelHandle, Connection, DocChunk, DocExecution, DocStream, DocTarget, DocValue, DocumentConn,
    DriverError, ErrorKind, EsRequest, HistoryEntry, HistoryOutcome, MongoOp, QueryLanguage,
    StatementClass, parse_es_console, parse_mongo_shell,
};

use crate::engine::{Command, Engine, Shared, not_connected};
use crate::job::{JobControl, JobId, JobKind, JobOutcome};
use crate::library::now_ms;
use crate::result::{QueryId, QueryStatus};
use crate::session::{SessionIo, SessionKey};
use crate::tx::{MessageLevel, READ_ONLY, RunMessage};

/// The per-result memory budget of document results by default (spec §7.4).
pub const DOC_RESULT_BUDGET: usize = 256 << 20;
/// Chars of a statement's first line in its Messages entry.
const MESSAGE_CHARS: usize = 80;
/// Statement text kept in the history.
const HISTORY_BYTES: usize = 64 * 1024;

/// A document statement's outcome as the UI sees it.
pub struct DocResult {
    pub text: String,
    pub status: QueryStatus,
    /// Pages of documents, in fetch order.
    pub chunks: Vec<DocChunk>,
    /// The collection (or index expression) whose documents these are, for editing; `None`
    /// unless the statement plainly reads documents (a `find` without a projection, a
    /// search).
    pub target: Option<DocTarget>,
    /// Why the documents cannot be edited; `None` when they can (each one by its `_id`).
    pub read_only: Option<String>,
    pub messages: Vec<RunMessage>,
    /// The statement returned rows: they are an ordinary rows result, `Engine::result(q)`.
    pub tabular: bool,
    /// Documents asked for by each fetch.
    pub page_size: usize,
    pub elapsed: Duration,
    /// The stream ended before it was exhausted (a later statement on the session took
    /// over, or the connection went away).
    pub cut_short: bool,
}

impl DocResult {
    /// An empty result of `text` (the engine's results start so).
    pub fn new(text: String, page_size: usize, status: QueryStatus) -> Self {
        Self {
            text,
            status,
            chunks: Vec::new(),
            target: None,
            read_only: Some("The statement is still running".into()),
            messages: Vec::new(),
            tabular: false,
            page_size: page_size.max(1),
            elapsed: Duration::ZERO,
            cut_short: false,
        }
    }

    /// Documents loaded.
    pub fn len(&self) -> usize {
        self.chunks.iter().map(DocChunk::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.iter().all(DocChunk::is_empty)
    }

    /// The `i`-th document loaded, decoded.
    pub fn doc(&self, mut i: usize) -> Option<DocValue> {
        for chunk in &self.chunks {
            if i < chunk.len() {
                return Some(chunk.doc(i));
            }
            i -= chunk.len();
        }
        None
    }

    /// Heap bytes the loaded documents take.
    pub fn byte_size(&self) -> usize {
        self.chunks.iter().map(DocChunk::byte_size).sum()
    }
}

/// The driver stream a result still reads from.
type DocStreamSlot = Arc<tokio::sync::Mutex<Option<Box<dyn DocStream>>>>;

struct DocEntry {
    result: Arc<RwLock<DocResult>>,
    key: SessionKey,
    io: Option<Arc<SessionIo>>,
    stream: DocStreamSlot,
    cancel_requested: bool,
    /// A later statement on the session took over: the stream closes at its next turn.
    superseded: bool,
    cancel: Option<Arc<dyn CancelHandle>>,
    started: Instant,
    started_at_ms: i64,
    recorded: bool,
}

#[derive(Default)]
pub(crate) struct DocStates {
    entries: Mutex<HashMap<QueryId, DocEntry>>,
    /// The per-result budget in bytes; 0 for [`DOC_RESULT_BUDGET`].
    budget: AtomicUsize,
}

impl DocStates {
    fn budget(&self) -> usize {
        match self.budget.load(Ordering::Relaxed) {
            0 => DOC_RESULT_BUDGET,
            n => n,
        }
    }
}

/// How a run or fetch of documents left the result.
enum PageEnd {
    More,
    Exhausted,
    Budget,
    Stopped,
    CutShort,
    Failed(DriverError),
}

/// The collection a statement reads plain documents of; `None` when its documents cannot be
/// edited (projections, aggregations, commands, writes).
fn edit_target(language: QueryLanguage, text: &str, database: Option<&str>) -> Option<DocTarget> {
    match language {
        QueryLanguage::MongoShell => match parse_mongo_shell(text).ok()?.first()? {
            (
                _,
                MongoOp::Find {
                    coll,
                    projection: None,
                    ..
                },
            ) => Some(DocTarget {
                database: database.map(str::to_string),
                collection: coll.clone(),
            }),
            _ => None,
        },
        QueryLanguage::EsConsole => match parse_es_console(text).ok()?.first()? {
            (_, EsRequest::Http { method, path, .. })
                if matches!(method.as_str(), "GET" | "POST") =>
            {
                let path = path.split('?').next().unwrap_or("");
                let mut segs = path.trim_start_matches('/').split('/');
                let index = segs
                    .next()
                    .filter(|s| !s.is_empty() && !s.starts_with('_'))?;
                (segs.next() == Some("_search") && segs.next().is_none()).then(|| DocTarget {
                    database: None,
                    collection: index.to_string(),
                })
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether an Elasticsearch index expression names more than one index.
fn is_pattern(index: &str) -> bool {
    index.contains(['*', ',', '?']) || index.starts_with('_') || index.is_empty()
}

/// The statement's first line, cut, for the Messages tab.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    match line.char_indices().nth(MESSAGE_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

/// Statements that may carry a password (`createUser`, `_security/user`, SQL password DDL)
/// are left out of the history.
pub(crate) fn may_hold_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("pwd")
        || lower.contains("password")
        || lower.contains("api_key")
        || lower.contains("identified by")
}

fn written_doc(
    matched: u64,
    modified: u64,
    inserted: u64,
    deleted: u64,
    ids: Vec<DocValue>,
) -> DocValue {
    let n = |v: u64| DocValue::Int64(i64::try_from(v).unwrap_or(i64::MAX));
    let mut fields = vec![
        ("matched".to_string(), n(matched)),
        ("modified".to_string(), n(modified)),
        ("inserted".to_string(), n(inserted)),
        ("deleted".to_string(), n(deleted)),
    ];
    if !ids.is_empty() {
        fields.push(("insertedIds".to_string(), DocValue::Array(ids)));
    }
    DocValue::Document(fields)
}

impl Engine {
    /// Runs one document statement (Mongo shell subset, Elasticsearch console request) on the
    /// session `key`, on `database` when given, and fetches its first `page` documents. The
    /// outcome is [`Engine::doc_result`]; a tabular one is also an ordinary rows result
    /// ([`Engine::result`]). A statement that may write is refused on a read-only session.
    /// Once it starts, the session's earlier document results stop fetching (`cut_short`),
    /// so a script's statements each keep their first page.
    pub fn run_doc(
        &self,
        key: &SessionKey,
        database: Option<&str>,
        text: String,
        page: usize,
    ) -> QueryId {
        let shared = &self.shared;
        let (q, io) = shared.start_doc_query(key);
        let status = match io {
            Some(_) => QueryStatus::Running,
            None => QueryStatus::Failed(not_connected()),
        };
        let result = Arc::new(RwLock::new(DocResult::new(text.clone(), page, status)));
        {
            let mut entries = shared.docs.entries.lock();
            entries.insert(
                q,
                DocEntry {
                    result,
                    key: key.clone(),
                    io: io.clone(),
                    stream: Arc::default(),
                    cancel_requested: false,
                    superseded: false,
                    cancel: None,
                    started: Instant::now(),
                    started_at_ms: now_ms(),
                    recorded: false,
                },
            );
        }
        match io {
            Some(io) => {
                let ticket = io.issue_ticket();
                let task = shared.clone().run_docs(
                    q,
                    key.clone(),
                    io,
                    ticket,
                    database.map(str::to_string),
                    text,
                );
                shared.send(Command::document(q, key.conn.clone(), task));
            }
            None => shared.settle_docs(q, QueryStatus::Failed(not_connected()), Vec::new()),
        }
        shared.repaint();
        q
    }

    /// The document result of `q`. Do not hold its guard while calling other `Engine`
    /// methods.
    pub fn doc_result(&self, q: QueryId) -> Option<Arc<RwLock<DocResult>>> {
        let entries = self.shared.docs.entries.lock();
        entries.get(&q).map(|e| e.result.clone())
    }

    /// Fetches the next page of documents (a no-op unless the result may have more).
    pub fn fetch_more_docs(&self, q: QueryId) {
        self.start_doc_fetch(q, false);
    }

    /// Fetches every remaining document, page by page, until the result's memory budget.
    pub fn fetch_all_docs(&self, q: QueryId) {
        self.start_doc_fetch(q, true);
    }

    fn start_doc_fetch(&self, q: QueryId, all: bool) {
        let Some(result) = self.doc_result(q) else {
            return;
        };
        {
            let mut r = result.write();
            if r.status != (QueryStatus::Ready { more: true }) {
                return;
            }
            r.status = QueryStatus::Running;
        }
        let task = self.shared.clone().fetch_docs(q, all);
        self.shared.send(Command::document(
            q,
            self.shared.doc_connection(q).unwrap_or_default(),
            task,
        ));
        self.shared.repaint();
    }

    /// Interrupts a running document statement or fetch.
    pub fn cancel_docs(&self, q: QueryId) {
        let handle = {
            let mut entries = self.shared.docs.entries.lock();
            let Some(e) = entries.get_mut(&q) else {
                return;
            };
            if e.result.read().status != QueryStatus::Running {
                return;
            }
            e.cancel_requested = true;
            e.cancel.clone()
        };
        if let Some(handle) = handle {
            self.shared.send(Command::Cancel(handle));
        }
    }

    /// Forgets the result (and its rows result, for a tabular one), closing its stream.
    pub fn close_docs(&self, q: QueryId) {
        let entry = self.shared.docs.entries.lock().remove(&q);
        let Some(entry) = entry else {
            return;
        };
        if entry.result.read().tabular {
            self.close_result(q);
        }
        if entry.result.read().status == QueryStatus::Running
            && let Some(handle) = entry.cancel
        {
            self.shared.send(Command::Cancel(handle));
        }
        let stream = entry.stream;
        self.shared.send(Command::Spawn(Box::pin(async move {
            if let Some(mut s) = stream.lock().await.take() {
                let _ = s.close().await;
            }
        })));
        self.shared.repaint();
    }

    /// Closes every document result of one MCP session (session end, timeout, token
    /// rotation or server stop).
    pub fn close_agent_docs(&self, agent_id: &str) {
        self.shared.close_agent_docs(agent_id);
    }

    /// Sets the per-result memory budget of document results (default
    /// [`DOC_RESULT_BUDGET`]); fetches stop once a result holds this many bytes.
    pub fn set_doc_budget(&self, bytes: usize) {
        self.shared.docs.budget.store(bytes, Ordering::Relaxed);
    }

    /// The class of the first statement of `text` on the connection's document driver, for
    /// confirmations; `Other` (treated as a write) when it is not connected.
    pub fn doc_classify(&self, conn: &str, text: &str) -> StatementClass {
        self.doc_conn(conn)
            .and_then(|c| c.document().map(|d| d.classify(text)))
            .unwrap_or(StatementClass::Other)
    }

    /// The query language of the connection's document driver, while it is connected.
    pub fn doc_language(&self, conn: &str) -> Option<QueryLanguage> {
        self.doc_conn(conn)
            .and_then(|c| c.document().map(DocumentConn::language))
    }

    /// The collection (or index) an explorer node of a document connection stands for, and
    /// the statement that opens its documents `page` at a time.
    pub fn open_doc_target(
        &self,
        conn: &str,
        path: &sqlr_core::NodePath,
        page: usize,
    ) -> Option<(DocTarget, String)> {
        let c = self.doc_conn(conn)?;
        let doc = c.document()?;
        let target = doc.node_target(path)?;
        let text = doc.open_target_text(&target, page);
        Some((target, text))
    }

    fn doc_conn(&self, conn: &str) -> Option<Arc<dyn Connection>> {
        let inner = self.shared.inner.lock();
        inner.sessions.get(&SessionKey::meta(conn))?.conn.clone()
    }

    /// Replaces the document `id` of `target` with `doc`, or inserts `doc` when `id` is
    /// `None`, as a [`JobKind::DocEdit`] job: [`JobOutcome::DocsWritten`], or `Failed` when
    /// the session is read-only, the document holds a type sqlr cannot write back, or (a
    /// replace) the document no longer exists. On Elasticsearch the document's `_index`
    /// (its hit's concrete index) is the target.
    pub fn save_doc(
        &self,
        key: &SessionKey,
        target: DocTarget,
        id: Option<DocValue>,
        doc: DocValue,
    ) -> JobId {
        self.doc_job(key, JobKind::DocEdit, DocWork::Save { target, id, doc })
    }

    /// Deletes the documents `ids` of `target` (a concrete index on Elasticsearch) as a
    /// [`JobKind::DocEdit`] job: [`JobOutcome::DocsWritten`] with how many were deleted.
    pub fn delete_docs(&self, key: &SessionKey, target: DocTarget, ids: Vec<DocValue>) -> JobId {
        self.doc_job(key, JobKind::DocEdit, DocWork::Delete { target, ids })
    }

    /// Runs one statement that changes no documents but the database's structure (index
    /// administration) as a [`JobKind::DocAdmin`] job: [`JobOutcome::Done`]. Refused on a
    /// read-only session.
    pub fn doc_admin(&self, key: &SessionKey, database: Option<String>, text: String) -> JobId {
        self.doc_job(key, JobKind::DocAdmin, DocWork::Admin { database, text })
    }

    fn doc_job(&self, key: &SessionKey, kind: JobKind, work: DocWork) -> JobId {
        let shared = &self.shared;
        let job = shared.jobs.create(kind);
        let id = job.id;
        let io = {
            let inner = shared.inner.lock();
            inner
                .sessions
                .get(key)
                .filter(|s| s.is_live())
                .map(|s| s.io.clone())
        };
        match io {
            None => shared.jobs.finish(id, JobOutcome::Failed(not_connected())),
            Some(io) => {
                let ticket = io.issue_ticket();
                let task = shared
                    .clone()
                    .run_doc_job(job, key.clone(), io, ticket, work);
                shared.send(Command::job(id, Some(key.conn.clone()), task));
            }
        }
        shared.repaint();
        id
    }
}

enum DocWork {
    Save {
        target: DocTarget,
        id: Option<DocValue>,
        doc: DocValue,
    },
    Delete {
        target: DocTarget,
        ids: Vec<DocValue>,
    },
    Admin {
        database: Option<String>,
        text: String,
    },
}

impl Shared {
    pub(crate) fn doc_connection(&self, q: QueryId) -> Option<String> {
        self.docs.entries.lock().get(&q).map(|e| e.key.conn.clone())
    }

    fn doc_entry(&self, q: QueryId) -> Option<(Arc<RwLock<DocResult>>, DocStreamSlot)> {
        let entries = self.docs.entries.lock();
        entries
            .get(&q)
            .map(|e| (e.result.clone(), e.stream.clone()))
    }

    /// Whether fetching `q` must stop: cancelled or closed, or superseded.
    fn doc_stop(&self, q: QueryId) -> Option<PageEnd> {
        let entries = self.docs.entries.lock();
        match entries.get(&q) {
            None => Some(PageEnd::Stopped),
            Some(e) if e.cancel_requested => Some(PageEnd::Stopped),
            Some(e) if e.superseded => Some(PageEnd::CutShort),
            Some(_) => None,
        }
    }

    /// Closes every document result of one MCP session.
    pub(crate) fn close_agent_docs(self: &Arc<Self>, agent_id: &str) {
        let ids: Vec<QueryId> = self
            .docs
            .entries
            .lock()
            .iter()
            .filter(|(_, e)| matches!(&e.key.slot, crate::session::Slot::Agent(a) if a == agent_id))
            .map(|(q, _)| *q)
            .collect();
        for q in ids {
            let Some(entry) = self.docs.entries.lock().remove(&q) else {
                continue;
            };
            if entry.result.read().tabular {
                self.close_query(q);
            }
            if entry.result.read().status == QueryStatus::Running
                && let Some(handle) = entry.cancel
            {
                self.send(Command::Cancel(handle));
            }
            let stream = entry.stream;
            self.send(Command::Spawn(Box::pin(async move {
                if let Some(mut s) = stream.lock().await.take() {
                    let _ = s.close().await;
                }
            })));
        }
    }

    /// Ends the results of a disconnected connection: their streams close.
    pub(crate) fn end_docs(&self, conn: &str) {
        let streams: Vec<(Arc<RwLock<DocResult>>, DocStreamSlot)> = {
            let mut entries = self.docs.entries.lock();
            entries
                .values_mut()
                .filter(|e| e.key.conn == conn)
                .map(|e| {
                    e.superseded = true;
                    (e.result.clone(), e.stream.clone())
                })
                .collect()
        };
        for (result, stream) in streams {
            {
                let mut r = result.write();
                if r.status == (QueryStatus::Ready { more: true }) {
                    r.status = QueryStatus::Ready { more: false };
                    r.cut_short = true;
                }
            }
            self.send(Command::Spawn(Box::pin(async move {
                if let Some(mut s) = stream.lock().await.take() {
                    let _ = s.close().await;
                }
            })));
        }
    }

    /// Closes the streams of the session's other (superseded) document results.
    async fn close_superseded(&self, key: &SessionKey, q: QueryId) {
        let others: Vec<(Arc<RwLock<DocResult>>, DocStreamSlot)> = {
            let entries = self.docs.entries.lock();
            entries
                .iter()
                .filter(|(id, e)| **id < q && e.key == *key && e.superseded)
                .map(|(_, e)| (e.result.clone(), e.stream.clone()))
                .collect()
        };
        for (result, stream) in others {
            // A fetch in progress holds the lock and stops at its next chunk.
            let Ok(mut slot) = stream.try_lock() else {
                continue;
            };
            if let Some(mut s) = slot.take() {
                let _ = s.close().await;
                let mut r = result.write();
                if r.status == (QueryStatus::Ready { more: true }) {
                    r.status = QueryStatus::Ready { more: false };
                    r.cut_short = true;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_docs(
        self: Arc<Self>,
        q: QueryId,
        key: SessionKey,
        io: Arc<SessionIo>,
        ticket: u64,
        database: Option<String>,
        text: String,
    ) {
        io.connect_finished().await;
        let turn = io.wait_turn(ticket).await;
        let mut slot = io.stream.lock().await;
        drop(turn);
        // The session holds one open stream: a rows result's closes, as for SQL.
        if let Some((prev, mut stream)) = slot.take() {
            let _ = stream.close().await;
            self.mark_complete(prev);
        }
        // Its turn came: the session's earlier document results stop (a script's earlier
        // statements have their first page by now).
        {
            let mut entries = self.docs.entries.lock();
            for (id, e) in entries.iter_mut() {
                if *id < q && e.key == key {
                    e.superseded = true;
                }
            }
        }
        self.close_superseded(&key, q).await;
        let Some((result, stream_slot)) = self.doc_entry(q) else {
            return;
        };
        if let Some(PageEnd::Stopped) = self.doc_stop(q) {
            self.settle_docs(q, QueryStatus::Cancelled, Vec::new());
            return;
        }
        let conn = match self.session_connection(&key, &io, None).await {
            Ok(conn) => conn,
            Err(e) => {
                self.settle_docs(q, QueryStatus::Failed(e), Vec::new());
                return;
            }
        };
        let Some(doc) = conn.document() else {
            let e = DriverError::new(
                ErrorKind::Unsupported,
                "this connection does not run document statements",
            );
            self.settle_docs(q, QueryStatus::Failed(e), Vec::new());
            return;
        };
        let read_only = {
            let mut inner = self.inner.lock();
            inner.session(&key, &io).is_some_and(|s| s.tx.read_only)
        };
        if read_only && doc.classify(&text).modifies() {
            let e = DriverError::new(ErrorKind::Permission, READ_ONLY);
            self.settle_docs(q, QueryStatus::Failed(e), Vec::new());
            return;
        }
        let target = edit_target(doc.language(), &text, database.as_deref());
        {
            let mut entries = self.docs.entries.lock();
            if let Some(e) = entries.get_mut(&q) {
                e.cancel = Some(conn.cancel_handle());
            }
        }
        let outcome = doc.run(database.as_deref(), &text).await;
        match outcome {
            Ok(DocExecution::Docs(stream)) => {
                // A view's documents cannot be written back (an unknown answer counts as one).
                let view = match &target {
                    Some(t) if !read_only => doc.is_view(t).await.unwrap_or(true),
                    _ => false,
                };
                {
                    let mut r = result.write();
                    r.read_only = match (&target, read_only) {
                        (_, true) => Some(READ_ONLY.to_string()),
                        (None, _) => Some(not_editable(doc.language())),
                        (Some(_), false) if view => Some("A view is read-only".into()),
                        (Some(_), false) => None,
                    };
                    r.target = target;
                }
                *stream_slot.lock().await = Some(stream);
                let page = result.read().page_size;
                self.fetch_doc_pages(q, &key, &io, &stream_slot, page, false)
                    .await;
            }
            Ok(DocExecution::Rows(stream)) => {
                {
                    let mut r = result.write();
                    r.tabular = true;
                    r.read_only = Some("Tabular results are not documents".into());
                }
                let page = result.read().page_size;
                self.adopt_rows(q, &key, &io, text.clone(), page, stream, &mut slot)
                    .await;
                let note = RunMessage::new(MessageLevel::Info, "The result is a table");
                self.settle_docs(q, QueryStatus::Ready { more: false }, vec![note]);
            }
            Ok(DocExecution::Written {
                matched,
                modified,
                inserted,
                deleted,
                ids,
            }) => {
                let summary = format!(
                    "{matched} matched · {modified} modified · {inserted} inserted · {deleted} deleted"
                );
                let doc = written_doc(matched, modified, inserted, deleted, ids);
                {
                    let mut r = result.write();
                    r.chunks.push(DocChunk::from_docs(&[doc]));
                    r.read_only = Some("A write's counts are not documents".into());
                }
                let note = RunMessage::new(MessageLevel::Info, summary);
                self.settle_docs(q, QueryStatus::Ready { more: false }, vec![note]);
            }
            Ok(DocExecution::Value(v)) => {
                let doc = match v {
                    d @ DocValue::Document(_) => d,
                    other => DocValue::Document(vec![("value".into(), other)]),
                };
                {
                    let mut r = result.write();
                    r.chunks.push(DocChunk::from_docs(&[doc]));
                    r.read_only = Some("A reply is not a stored document".into());
                }
                self.settle_docs(q, QueryStatus::Ready { more: false }, Vec::new());
            }
            Err(e) => {
                let notes: Vec<RunMessage> = self.on_error(&key, &io, &e).into_iter().collect();
                let status = if e.kind == ErrorKind::Cancelled {
                    QueryStatus::Cancelled
                } else {
                    QueryStatus::Failed(e)
                };
                self.settle_docs(q, status, notes);
            }
        }
    }

    async fn fetch_docs(self: Arc<Self>, q: QueryId, all: bool) {
        let Some((key, io)) = ({
            let entries = self.docs.entries.lock();
            entries
                .get(&q)
                .and_then(|e| Some((e.key.clone(), e.io.clone()?)))
        }) else {
            return;
        };
        let Some((result, stream_slot)) = self.doc_entry(q) else {
            return;
        };
        // Fetches keep the session's order with its statements.
        let _slot = io.stream.lock().await;
        let page = result.read().page_size;
        self.fetch_doc_pages(q, &key, &io, &stream_slot, page, all)
            .await;
    }

    /// Fetches one page (or, with `all`, every page until the budget) and settles the
    /// result's status.
    async fn fetch_doc_pages(
        self: &Arc<Self>,
        q: QueryId,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        stream_slot: &DocStreamSlot,
        page: usize,
        all: bool,
    ) {
        let Some((result, _)) = self.doc_entry(q) else {
            return;
        };
        let mut slot = stream_slot.lock().await;
        let budget = self.docs.budget();
        let end = loop {
            let end = match slot.as_mut() {
                None => PageEnd::CutShort,
                Some(stream) => {
                    self.fetch_doc_page(q, &result, stream.as_mut(), page, budget)
                        .await
                }
            };
            if let Some(stream) = slot.as_mut() {
                let notices = stream.take_notices();
                if !notices.is_empty() {
                    let mut r = result.write();
                    r.messages.extend(
                        notices
                            .into_iter()
                            .map(|n| RunMessage::new(MessageLevel::Notice, n)),
                    );
                }
            }
            match end {
                PageEnd::More if all => self.repaint(),
                end => break end,
            }
        };
        if !matches!(end, PageEnd::More)
            && let Some(mut stream) = slot.take()
        {
            let _ = stream.close().await;
        }
        drop(slot);
        let mut notes = Vec::new();
        let status = match end {
            PageEnd::More => QueryStatus::Ready { more: true },
            PageEnd::Exhausted => QueryStatus::Ready { more: false },
            PageEnd::Budget => {
                notes.push(RunMessage::new(
                    MessageLevel::Warning,
                    format!(
                        "Stopped at the result's memory budget ({} MB): export the collection \
                         to read every document",
                        budget >> 20
                    ),
                ));
                // The result ends here: it offers no more fetches.
                QueryStatus::Ready { more: false }
            }
            PageEnd::CutShort => {
                result.write().cut_short = true;
                QueryStatus::Ready { more: false }
            }
            PageEnd::Stopped => QueryStatus::Cancelled,
            PageEnd::Failed(e) => {
                notes.extend(self.on_error(key, io, &e));
                if e.kind == ErrorKind::Cancelled {
                    QueryStatus::Cancelled
                } else {
                    QueryStatus::Failed(e)
                }
            }
        };
        self.settle_docs(q, status, notes);
    }

    async fn fetch_doc_page(
        &self,
        q: QueryId,
        result: &Arc<RwLock<DocResult>>,
        stream: &mut dyn DocStream,
        page: usize,
        budget: usize,
    ) -> PageEnd {
        let mut fetched = 0;
        while fetched < page {
            if let Some(end) = self.doc_stop(q) {
                return end;
            }
            if result.read().byte_size() >= budget {
                return PageEnd::Budget;
            }
            match stream.next_chunk(page - fetched).await {
                Ok(Some(chunk)) => {
                    if chunk.is_empty() {
                        break;
                    }
                    fetched += chunk.len();
                    result.write().chunks.push(chunk);
                }
                Ok(None) => return PageEnd::Exhausted,
                Err(e) => return PageEnd::Failed(e),
            }
        }
        self.doc_stop(q).unwrap_or(PageEnd::More)
    }

    /// Sets the result's status; the first time it leaves `Running`, its outcome and
    /// `notes` join the messages and the statement is recorded in the history.
    pub(crate) fn settle_docs(
        self: &Arc<Self>,
        q: QueryId,
        status: QueryStatus,
        notes: Vec<RunMessage>,
    ) {
        let entry = {
            let mut entries = self.docs.entries.lock();
            entries.get_mut(&q).map(|e| {
                let first = !e.recorded && status != QueryStatus::Running;
                e.recorded |= first;
                if status != QueryStatus::Running {
                    e.cancel = None;
                }
                (
                    e.result.clone(),
                    e.started,
                    e.key.conn.clone(),
                    first.then(|| (e.key.conn.clone(), e.started_at_ms)),
                )
            })
        };
        let Some((result, started, conn, first)) = entry else {
            self.repaint();
            return;
        };
        let mut failure = None;
        let history = {
            let mut r = result.write();
            let settled = r.status == QueryStatus::Running && status != QueryStatus::Running;
            if settled && !r.tabular {
                let error = match &status {
                    QueryStatus::Failed(error) => Some(error.clone()),
                    QueryStatus::Cancelled => {
                        Some(DriverError::new(ErrorKind::Cancelled, "cancelled"))
                    }
                    _ => None,
                };
                if let Some(error) = error {
                    failure = Some((error, r.text.clone()));
                }
            }
            r.status = status;
            r.elapsed = started.elapsed();
            match first {
                Some((conn, at)) => {
                    let outcome = outcome_message(&r);
                    r.messages.push(outcome);
                    r.messages.extend(notes);
                    // A tabular result is recorded by its rows result.
                    (!r.tabular).then(|| history_of(conn, at, &r)).flatten()
                }
                None => {
                    r.messages.extend(notes);
                    None
                }
            }
        };
        if let Some((error, text)) = failure {
            let driver = self.inner.lock().driver_of(&conn);
            let sql = self
                .diagnostics_include_sql()
                .then(|| crate::log::truncate_sql(&text).0);
            super::engine::log_query_settled(
                crate::log::failure_level(error.kind),
                "document",
                &conn,
                driver.as_deref(),
                None,
                q.0,
                started.elapsed().as_millis() as u64,
                error.kind,
                &error.message,
                sql.as_deref(),
            );
        }
        if let Some(entry) = history {
            self.record_history(entry);
        }
        self.repaint();
    }

    async fn run_doc_job(
        self: Arc<Self>,
        job: JobControl,
        key: SessionKey,
        io: Arc<SessionIo>,
        ticket: u64,
        work: DocWork,
    ) {
        io.connect_finished().await;
        let turn = io.wait_turn(ticket).await;
        let _slot = io.stream.lock().await;
        drop(turn);
        let outcome = if job.is_cancelled() {
            JobOutcome::Cancelled
        } else {
            self.doc_work(&job, &key, &io, work).await
        };
        self.jobs.finish(job.id, outcome);
        self.repaint();
    }

    async fn doc_work(
        self: &Arc<Self>,
        job: &JobControl,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        work: DocWork,
    ) -> JobOutcome {
        let read_only = {
            let mut inner = self.inner.lock();
            match inner.session(key, io) {
                Some(s) => s.tx.read_only,
                None => return JobOutcome::Failed(not_connected()),
            }
        };
        if read_only {
            return JobOutcome::Failed(DriverError::new(ErrorKind::Permission, READ_ONLY));
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
        let es = doc.language() == QueryLanguage::EsConsole;
        let outcome = match work {
            DocWork::Save { target, id, doc: d } => save(doc, es, target, id, d).await,
            DocWork::Delete { target, ids } => delete(job, doc, es, target, ids).await,
            DocWork::Admin { database, text } => doc
                .run(database.as_deref(), &text)
                .await
                .map(|_| JobOutcome::Done),
        };
        job.set_cancel(None);
        match outcome {
            Ok(outcome) => outcome,
            Err(e) => {
                self.on_error(key, io, &e);
                if job.is_cancelled() || e.kind == ErrorKind::Cancelled {
                    JobOutcome::Cancelled
                } else {
                    JobOutcome::Failed(e)
                }
            }
        }
    }
}

fn not_editable(language: QueryLanguage) -> String {
    match language {
        QueryLanguage::EsConsole => {
            "Only the hits of a search on an index can be edited".to_string()
        }
        _ => "Only the documents of a find() without a projection can be edited".to_string(),
    }
}

/// The concrete index an Elasticsearch replace writes to: the caller's target (its hit's
/// own index), never a pattern. The document's `_index`, when present, must be that index:
/// editing it in the text cannot move the document (and an alias, which is not the hit's
/// index, is refused the same way).
fn es_index(target: &DocTarget, doc: Option<&DocValue>) -> Result<DocTarget, DriverError> {
    if is_pattern(&target.collection) {
        return Err(DriverError::new(
            ErrorKind::Other,
            format!(
                "\"{}\" names several indices: edits go to one concrete index",
                target.collection
            ),
        ));
    }
    if let Some(DocValue::String(index)) = doc.and_then(|d| d.get("_index"))
        && *index != target.collection
    {
        return Err(DriverError::new(
            ErrorKind::Other,
            format!(
                "`_index` cannot be changed: the document is in {index}, not {}",
                target.collection
            ),
        ));
    }
    Ok(target.clone())
}

async fn save(
    doc: &dyn DocumentConn,
    es: bool,
    target: DocTarget,
    id: Option<DocValue>,
    d: DocValue,
) -> Result<JobOutcome, DriverError> {
    if let Some(what) = d.unmodelled() {
        return Err(DriverError::new(
            ErrorKind::Unsupported,
            format!(
                "The document holds {what}, which sqlr cannot write back without loss; \
                 edit it in the shell instead"
            ),
        ));
    }
    let target = match (es, &id) {
        (true, Some(_)) => es_index(&target, Some(&d))?,
        // A new document goes to the target; its `_index`, if any, is dropped.
        (true, None) => es_index(&target, None)?,
        (false, _) => target,
    };
    match id {
        Some(id) => {
            let n = doc.replace_doc(&target, &id, &d).await?;
            if n == 0 {
                return Err(DriverError::new(
                    ErrorKind::Other,
                    format!(
                        "The document {} no longer exists (deleted meanwhile?): nothing was saved",
                        id.to_relaxed_json()
                    ),
                ));
            }
            Ok(JobOutcome::DocsWritten {
                requested: 1,
                written: n,
                id: None,
            })
        }
        None => {
            let id = doc.insert_doc(&target, &d).await?;
            Ok(JobOutcome::DocsWritten {
                requested: 1,
                written: 1,
                id: Some(id),
            })
        }
    }
}

async fn delete(
    job: &JobControl,
    doc: &dyn DocumentConn,
    es: bool,
    target: DocTarget,
    ids: Vec<DocValue>,
) -> Result<JobOutcome, DriverError> {
    if es && is_pattern(&target.collection) {
        return Err(DriverError::new(
            ErrorKind::Other,
            format!(
                "\"{}\" names several indices: deletes go to one concrete index",
                target.collection
            ),
        ));
    }
    let mut written = 0;
    for id in &ids {
        if job.is_cancelled() {
            break;
        }
        written += doc.delete_doc(&target, id).await?;
    }
    Ok(JobOutcome::DocsWritten {
        requested: ids.len() as u64,
        written,
        id: None,
    })
}

/// The Messages line of a result whose status just left `Running`.
fn outcome_message(r: &DocResult) -> RunMessage {
    let line = first_line(&r.text);
    let ms = r.elapsed.as_millis();
    match &r.status {
        QueryStatus::Failed(e) => {
            RunMessage::new(MessageLevel::Error, format!("{line} · {}", e.message))
        }
        QueryStatus::Cancelled => {
            RunMessage::new(MessageLevel::Warning, format!("{line} · cancelled"))
        }
        status => {
            let more = if *status == (QueryStatus::Ready { more: true }) {
                " (more available)"
            } else {
                ""
            };
            let n = r.len();
            let noun = if n == 1 { "document" } else { "documents" };
            RunMessage::new(
                MessageLevel::Info,
                format!("{line} · {n} {noun}{more} · {ms} ms"),
            )
        }
    }
}

fn history_of(conn_id: String, started_at_ms: i64, r: &DocResult) -> Option<HistoryEntry> {
    if may_hold_secret(&r.text) {
        return None;
    }
    let outcome = match &r.status {
        QueryStatus::Running => return None,
        QueryStatus::Ready { .. } => HistoryOutcome::Rows(r.len() as u64),
        QueryStatus::Cancelled => HistoryOutcome::Cancelled,
        QueryStatus::Failed(e) => HistoryOutcome::Failed(e.message.clone()),
    };
    let text = &r.text[..r.text.floor_char_boundary(HISTORY_BYTES)];
    Some(HistoryEntry {
        id: 0,
        conn_id,
        database: None,
        sql: text.to_string(),
        started_at_ms,
        duration_ms: u64::try_from(r.elapsed.as_millis()).unwrap_or(u64::MAX),
        outcome,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_targets_come_from_plain_reads() {
        let mongo = |t: &str| edit_target(QueryLanguage::MongoShell, t, Some("app"));
        assert_eq!(
            mongo("db.users.find({a: 1}).sort({b: 1})"),
            Some(DocTarget {
                database: Some("app".into()),
                collection: "users".into()
            })
        );
        assert_eq!(mongo("db.users.find({}, {a: 1})"), None);
        assert_eq!(mongo("db.users.aggregate([])"), None);
        assert_eq!(mongo("db.users.find("), None);
        let es = |t: &str| edit_target(QueryLanguage::EsConsole, t, None);
        assert_eq!(
            es("GET logs-*/_search\n{}").map(|t| t.collection),
            Some("logs-*".into())
        );
        assert_eq!(es("GET _search"), None);
        assert_eq!(es("GET logs/_count"), None);
        assert_eq!(es("DELETE logs/_doc/1"), None);
    }

    #[test]
    fn es_edits_go_to_the_callers_concrete_index() {
        let t = |c: &str| DocTarget {
            database: None,
            collection: c.into(),
        };
        let hit = DocValue::Document(vec![("_index".into(), DocValue::String("logs-b".into()))]);
        // The caller names the hit's index; the edited text cannot move the document.
        assert_eq!(
            es_index(&t("logs-b"), Some(&hit)).unwrap().collection,
            "logs-b"
        );
        let moved = es_index(&t("logs-a"), Some(&hit)).unwrap_err();
        assert!(
            moved.message.contains("`_index` cannot be changed"),
            "{}",
            moved.message
        );
        // An alias (not the hit's own index) or a pattern is refused.
        assert!(es_index(&t("logs"), Some(&hit)).is_err());
        assert!(es_index(&t("logs-*"), None).is_err());
        assert!(es_index(&t("logs-*"), Some(&hit)).is_err());
        assert!(es_index(&t("logs-b"), None).is_ok());
    }

    #[test]
    fn secrets_stay_out_of_the_history() {
        assert!(may_hold_secret(
            "db.runCommand({createUser: 'a', pwd: 'x', roles: []})"
        ));
        assert!(may_hold_secret(
            "POST _security/user/a\n{\"password\": \"x\"}"
        ));
        assert!(!may_hold_secret("db.users.find({})"));
    }
}
