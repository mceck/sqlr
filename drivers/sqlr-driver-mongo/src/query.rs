//! Running shell statements: commands, cursors and the `DocumentConn` implementation.

use std::ops::Range;
use std::sync::Arc;

use bson::Bson;
use bson::raw::RawDocumentBuf;
use mongodb::Cursor;
use sqlr_core::{
    Cell, ChunkBuilder, ColumnKind, ColumnMeta, DisplayHint, DocChunk, DocExecution, DocStream,
    DocTarget, DocValue, DocumentConn, DriverError, ErrorKind, MongoOp, QueryLanguage, ResultChunk,
    RowStream, StatementClass, Value, new_object_id, parse_mongo_shell, read_only_error,
};

use crate::connect::{MongoConnection, Running, RunningTags};
use crate::ejson::{from_document, from_raw_doc, to_raw_doc};
use crate::errors::map_error;
use crate::write;

/// A command document from `(key, value)` pairs.
pub(crate) fn cmd(pairs: Vec<(&str, DocValue)>) -> DocValue {
    DocValue::Document(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// A fresh comment that tags one operation, so `killOp` can find it.
pub(crate) fn new_tag() -> String {
    let id = new_object_id();
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    format!("sqlr:{hex}")
}

/// Runs `command` on `db`, tagged with a comment so it can be cancelled; the reply as a
/// document.
pub(crate) async fn command(
    conn: &MongoConnection,
    db: &str,
    command: &DocValue,
) -> Result<DocValue, DriverError> {
    let mut command = command.clone();
    let tag = tag_command(&mut command);
    let _running = Running::start(&conn.running, &tag);
    raw_command(conn, db, &command).await
}

/// Runs `command` as given (the user's `db.runCommand`); it cannot be cancelled.
pub(crate) async fn raw_command(
    conn: &MongoConnection,
    db: &str,
    command: &DocValue,
) -> Result<DocValue, DriverError> {
    let reply = conn
        .client
        .database(db)
        .run_raw_command(to_raw_doc(command)?)
        .await
        .map_err(map_error)?;
    from_document(&reply)
}

/// Opens the cursor a command returns, tagged with `tag` while it runs.
async fn open_cursor(
    conn: &MongoConnection,
    db: &str,
    command: &DocValue,
    tag: &str,
) -> Result<Cursor<RawDocumentBuf>, DriverError> {
    let _running = Running::start(&conn.running, tag);
    let cursor = conn
        .client
        .database(db)
        .run_raw_cursor_command(to_raw_doc(command)?)
        .comment(Bson::String(tag.to_string()))
        .await
        .map_err(map_error)?;
    Ok(cursor.with_type::<RawDocumentBuf>())
}

/// Every document a cursor command returns (for small results: listings, indexes).
pub(crate) async fn cursor_docs(
    conn: &MongoConnection,
    db: &str,
    command: &DocValue,
) -> Result<Vec<DocValue>, DriverError> {
    let mut command = command.clone();
    let tag = tag_command(&mut command);
    let mut cursor = open_cursor(conn, db, &command, &tag).await?;
    let _running = Running::start(&conn.running, &tag);
    let mut out = Vec::new();
    while cursor.advance().await.map_err(map_error)? {
        out.push(from_raw_doc(cursor.current())?);
    }
    Ok(out)
}

/// A page of documents stops growing past this many bytes.
const CHUNK_BYTE_CAP: usize = 32 * 1024 * 1024;

/// Documents from a server cursor, a page at a time.
struct MongoDocStream {
    cursor: Option<Cursor<RawDocumentBuf>>,
    tag: String,
    running: RunningTags,
}

#[async_trait::async_trait]
impl DocStream for MongoDocStream {
    async fn next_chunk(&mut self, max: usize) -> Result<Option<DocChunk>, DriverError> {
        let Some(cursor) = self.cursor.as_mut() else {
            return Ok(None);
        };
        let _running = Running::start(&self.running, &self.tag);
        let mut chunk = DocChunk::default();
        // A page also ends once it holds CHUNK_BYTE_CAP bytes, so huge documents stay
        // bounded in memory (the next page continues from the cursor).
        while chunk.len() < max.max(1) && chunk.byte_size() < CHUNK_BYTE_CAP {
            match cursor.advance().await {
                Ok(true) => chunk
                    .push(&from_raw_doc(cursor.current())?)
                    .map_err(|e| DriverError::new(ErrorKind::Other, e.to_string()))?,
                Ok(false) => {
                    self.cursor = None;
                    break;
                }
                Err(e) => {
                    self.cursor = None;
                    return Err(map_error(e));
                }
            }
        }
        Ok((!chunk.is_empty()).then_some(chunk))
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        // Dropping an open cursor kills it on the server.
        self.cursor = None;
        Ok(())
    }
}

/// A small tabular result held in memory (`distinct`, `show dbs`, `show collections`).
pub(crate) struct MemRows {
    columns: Arc<[ColumnMeta]>,
    rows: Vec<Vec<Value>>,
    at: usize,
}

impl MemRows {
    pub(crate) fn new(columns: Vec<ColumnMeta>, rows: Vec<Vec<Value>>) -> Self {
        Self {
            columns: columns.into(),
            rows,
            at: 0,
        }
    }
}

#[async_trait::async_trait]
impl RowStream for MemRows {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }

    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        if self.at >= self.rows.len() {
            return Ok(None);
        }
        let end = (self.at + max_rows.max(1)).min(self.rows.len());
        let mut b = ChunkBuilder::new(self.columns.clone());
        for row in &self.rows[self.at..end] {
            let cells: Vec<Cell<'_>> = row.iter().map(Value::as_cell).collect();
            b.push_row(&cells)
                .map_err(|e| DriverError::new(ErrorKind::Protocol, format!("{e:?}")))?;
        }
        self.at = end;
        Ok(Some(b.finish()))
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        self.at = self.rows.len();
        Ok(())
    }
}

/// A document value as a grid cell, with the same presentation as the document table.
pub(crate) fn cell_value(v: &DocValue) -> Value {
    sqlr_core::doc_field_value(Some(v))
}

fn int_of(v: Option<&DocValue>) -> u64 {
    match v {
        Some(DocValue::Int32(i)) => (*i).max(0) as u64,
        Some(DocValue::Int64(i)) => (*i).max(0) as u64,
        Some(DocValue::Double(d)) => d.max(0.0) as u64,
        _ => 0,
    }
}

fn syntax_error(text: &str, e: sqlr_core::ShellError) -> DriverError {
    let chars = text[..e.position.min(text.len())].chars().count();
    DriverError::new(ErrorKind::Syntax, e.message).with_position(chars)
}

/// The `limit` and `singleBatch` fields a shell `limit()` adds: a negative limit asks for
/// one batch of `|limit|` documents.
fn find_limit(limit: Option<i64>) -> Result<Option<(i64, bool)>, DriverError> {
    Ok(match limit {
        Some(n) if n < 0 => Some((
            n.checked_neg().ok_or_else(|| {
                DriverError::new(ErrorKind::Syntax, "find() limit is out of range")
            })?,
            true,
        )),
        Some(n) if n > 0 => Some((n, false)),
        _ => None,
    })
}

/// The single statement of `text`.
fn parse_one(text: &str) -> Result<MongoOp, DriverError> {
    let mut ops = parse_mongo_shell(text).map_err(|e| syntax_error(text, e))?;
    let n = ops.len();
    match (ops.pop(), n) {
        (Some((_, op)), 1) => Ok(op),
        (_, 0) => Err(DriverError::new(ErrorKind::Syntax, "nothing to run")),
        (_, n) => Err(DriverError::new(
            ErrorKind::Syntax,
            format!("the text holds {n} statements: run them one at a time or as a script"),
        )),
    }
}

/// Merges `options` into `command` (keys already present win).
fn merge(command: &mut DocValue, options: Option<&DocValue>) {
    let (DocValue::Document(fields), Some(DocValue::Document(opts))) = (command, options) else {
        return;
    };
    for (k, v) in opts {
        if !fields.iter().any(|(f, _)| f == k) {
            fields.push((k.clone(), v.clone()));
        }
    }
}

/// The comment of `command` (to tag it), adding a fresh one when it has none.
fn tag_command(command: &mut DocValue) -> String {
    if let Some(DocValue::String(c)) = command.get("comment") {
        return c.clone();
    }
    let tag = new_tag();
    if let DocValue::Document(f) = command {
        f.push(("comment".into(), DocValue::String(tag.clone())));
    }
    tag
}

impl MongoConnection {
    async fn docs(&self, db: &str, mut command: DocValue) -> Result<DocExecution, DriverError> {
        let tag = tag_command(&mut command);
        let cursor = open_cursor(self, db, &command, &tag).await?;
        Ok(DocExecution::Docs(Box::new(MongoDocStream {
            cursor: Some(cursor),
            tag,
            running: self.running.clone(),
        })))
    }

    pub(crate) async fn run_op(
        &self,
        database: Option<&str>,
        op: MongoOp,
    ) -> Result<DocExecution, DriverError> {
        if self.is_read_only() && op.class().modifies() {
            return Err(read_only_error());
        }
        let db = database.map_or_else(|| self.current_database(), String::from);
        let db = db.as_str();
        let s = |v: &str| DocValue::String(v.to_string());
        Ok(match op {
            MongoOp::Find {
                coll,
                filter,
                projection,
                sort,
                limit,
                skip,
            } => {
                let mut c = vec![("find", s(&coll)), ("filter", filter)];
                if let Some(p) = projection {
                    c.push(("projection", p));
                }
                if let Some(o) = sort {
                    c.push(("sort", o));
                }
                if let Some(n) = skip {
                    c.push(("skip", DocValue::Int64(n as i64)));
                }
                if let Some((limit, single_batch)) = find_limit(limit)? {
                    c.push(("limit", DocValue::Int64(limit)));
                    if single_batch {
                        c.push(("singleBatch", DocValue::Bool(true)));
                    }
                }
                self.docs(db, cmd(c)).await?
            }
            MongoOp::Aggregate {
                coll,
                pipeline,
                options,
            } => {
                let mut c = cmd(vec![
                    ("aggregate", s(&coll)),
                    ("pipeline", DocValue::Array(pipeline)),
                ]);
                merge(&mut c, options.as_ref());
                if c.get("explain") == Some(&DocValue::Bool(true)) {
                    DocExecution::Value(command(self, db, &c).await?)
                } else {
                    if c.get("cursor").is_none() {
                        merge(&mut c, Some(&cmd(vec![("cursor", cmd(vec![]))])));
                    }
                    self.docs(db, c).await?
                }
            }
            MongoOp::CountDocuments {
                coll,
                filter,
                options,
            } => {
                let mut pipeline = vec![cmd(vec![("$match", filter)])];
                if let Some(o) = &options {
                    for (opt, stage) in [("skip", "$skip"), ("limit", "$limit")] {
                        if let Some(v) = o.get(opt) {
                            pipeline.push(cmd(vec![(stage, v.clone())]));
                        }
                    }
                }
                pipeline.push(cmd(vec![(
                    "$group",
                    cmd(vec![
                        ("_id", DocValue::Int32(1)),
                        ("n", cmd(vec![("$sum", DocValue::Int32(1))])),
                    ]),
                )]));
                let mut c = cmd(vec![
                    ("aggregate", s(&coll)),
                    ("pipeline", DocValue::Array(pipeline)),
                    ("cursor", cmd(vec![])),
                ]);
                // skip/limit are pipeline stages; the remaining count options belong
                // to the aggregate command (notably collation, hint and maxTimeMS).
                if let Some(DocValue::Document(opts)) = &options {
                    let opts = DocValue::Document(
                        opts.iter()
                            .filter(|(key, _)| !matches!(key.as_str(), "skip" | "limit"))
                            .cloned()
                            .collect(),
                    );
                    merge(&mut c, Some(&opts));
                }
                let docs = cursor_docs(self, db, &c).await?;
                DocExecution::Value(DocValue::Int64(
                    int_of(docs.first().and_then(|d| d.get("n"))) as i64,
                ))
            }
            MongoOp::EstimatedDocumentCount { coll } => {
                let reply = command(self, db, &cmd(vec![("count", s(&coll))])).await?;
                DocExecution::Value(DocValue::Int64(int_of(reply.get("n")) as i64))
            }
            MongoOp::Distinct {
                coll,
                field,
                filter,
            } => {
                let mut c = vec![("distinct", s(&coll)), ("key", s(&field))];
                if let Some(f) = filter {
                    c.push(("query", f));
                }
                let reply = command(self, db, &cmd(c)).await?;
                let values = match reply.get("values") {
                    Some(DocValue::Array(v)) => v.iter().map(|v| vec![cell_value(v)]).collect(),
                    _ => Vec::new(),
                };
                DocExecution::Rows(Box::new(MemRows::new(
                    vec![ColumnMeta::new(
                        &field,
                        "mixed",
                        ColumnKind::Mixed,
                        DisplayHint::Plain,
                    )],
                    values,
                )))
            }
            MongoOp::RunCommand(c) => DocExecution::Value(raw_command(self, db, &c).await?),
            MongoOp::GetCollectionNames => {
                let c = cmd(vec![
                    ("listCollections", DocValue::Int32(1)),
                    ("nameOnly", DocValue::Bool(true)),
                    ("authorizedCollections", DocValue::Bool(true)),
                ]);
                let mut names: Vec<String> = cursor_docs(self, db, &c)
                    .await?
                    .iter()
                    .filter_map(|d| match d.get("name") {
                        Some(DocValue::String(n)) => Some(n.clone()),
                        _ => None,
                    })
                    .collect();
                names.sort();
                DocExecution::Value(DocValue::Array(
                    names.into_iter().map(DocValue::String).collect(),
                ))
            }
            MongoOp::ShowDbs => {
                let reply = command(
                    self,
                    "admin",
                    &cmd(vec![("listDatabases", DocValue::Int32(1))]),
                )
                .await?;
                let mut rows: Vec<Vec<Value>> = match reply.get("databases") {
                    Some(DocValue::Array(dbs)) => dbs
                        .iter()
                        .map(|d| {
                            vec![
                                cell_value(d.get("name").unwrap_or(&DocValue::Null)),
                                Value::Int(int_of(d.get("sizeOnDisk")) as i64),
                                cell_value(d.get("empty").unwrap_or(&DocValue::Null)),
                            ]
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                rows.sort_by(|a, b| a[0].display().cmp(&b[0].display()));
                DocExecution::Rows(Box::new(MemRows::new(
                    vec![
                        ColumnMeta::new("name", "string", ColumnKind::Text, DisplayHint::Plain),
                        ColumnMeta::new("sizeOnDisk", "long", ColumnKind::Int, DisplayHint::Number),
                        ColumnMeta::new("empty", "bool", ColumnKind::Bool, DisplayHint::Plain),
                    ],
                    rows,
                )))
            }
            MongoOp::ShowCollections => {
                let c = cmd(vec![
                    ("listCollections", DocValue::Int32(1)),
                    ("nameOnly", DocValue::Bool(true)),
                    ("authorizedCollections", DocValue::Bool(true)),
                ]);
                let mut rows: Vec<Vec<Value>> = cursor_docs(self, db, &c)
                    .await?
                    .iter()
                    .map(|d| {
                        vec![
                            cell_value(d.get("name").unwrap_or(&DocValue::Null)),
                            cell_value(d.get("type").unwrap_or(&DocValue::Null)),
                        ]
                    })
                    .collect();
                rows.sort_by(|a, b| a[0].display().cmp(&b[0].display()));
                DocExecution::Rows(Box::new(MemRows::new(
                    vec![
                        ColumnMeta::new("name", "string", ColumnKind::Text, DisplayHint::Plain),
                        ColumnMeta::new("type", "string", ColumnKind::Text, DisplayHint::Plain),
                    ],
                    rows,
                )))
            }
            MongoOp::Use(name) => {
                self.set_current_database(&name);
                DocExecution::Value(DocValue::String(format!("switched to db {name}")))
            }
            MongoOp::GetIndexes { coll } => {
                self.docs(db, cmd(vec![("listIndexes", s(&coll))])).await?
            }
            other => write::run_write(self, db, other).await?,
        })
    }

    fn target_db(&self, target: &DocTarget) -> String {
        target
            .database
            .clone()
            .unwrap_or_else(|| self.current_database())
    }

    fn check_writable(&self) -> Result<(), DriverError> {
        if self.is_read_only() {
            Err(read_only_error())
        } else {
            Ok(())
        }
    }
}

#[async_trait::async_trait]
impl DocumentConn for MongoConnection {
    fn language(&self) -> QueryLanguage {
        QueryLanguage::MongoShell
    }

    async fn run(&self, database: Option<&str>, text: &str) -> Result<DocExecution, DriverError> {
        let op = parse_one(text)?;
        self.run_op(database, op).await
    }

    fn split(&self, text: &str) -> Vec<Range<usize>> {
        QueryLanguage::MongoShell.split(text)
    }

    async fn replace_doc(
        &self,
        target: &DocTarget,
        id: &DocValue,
        doc: &DocValue,
    ) -> Result<u64, DriverError> {
        self.check_writable()?;
        write::check_replacement(doc)?;
        let db = self.target_db(target);
        let (matched, ..) = write::update(
            self,
            &db,
            &target.collection,
            cmd(vec![("_id", id.clone())]),
            doc.clone(),
            false,
            None,
        )
        .await?;
        Ok(matched)
    }

    async fn insert_doc(
        &self,
        target: &DocTarget,
        doc: &DocValue,
    ) -> Result<DocValue, DriverError> {
        self.check_writable()?;
        let db = self.target_db(target);
        let (_, mut ids) =
            write::insert(self, &db, &target.collection, vec![doc.clone()], None).await?;
        Ok(ids.pop().unwrap_or(DocValue::Null))
    }

    async fn delete_doc(&self, target: &DocTarget, id: &DocValue) -> Result<u64, DriverError> {
        self.check_writable()?;
        let db = self.target_db(target);
        write::delete(
            self,
            &db,
            &target.collection,
            cmd(vec![("_id", id.clone())]),
            false,
        )
        .await
    }

    async fn is_view(&self, target: &DocTarget) -> Result<bool, DriverError> {
        let db = self.target_db(target);
        let listed = cursor_docs(
            self,
            &db,
            &cmd(vec![
                ("listCollections", DocValue::Int32(1)),
                (
                    "filter",
                    cmd(vec![("name", DocValue::String(target.collection.clone()))]),
                ),
                ("authorizedCollections", DocValue::Bool(true)),
            ]),
        )
        .await?;
        Ok(listed
            .iter()
            .any(|c| c.get("type") == Some(&DocValue::String("view".into()))))
    }

    async fn indexes(&self, target: &DocTarget) -> Result<Vec<DocValue>, DriverError> {
        let db = self.target_db(target);
        cursor_docs(
            self,
            &db,
            &cmd(vec![(
                "listIndexes",
                DocValue::String(target.collection.clone()),
            )]),
        )
        .await
    }

    fn open_target_text(&self, target: &DocTarget, page: usize) -> String {
        self.language().open_document_text(target, page)
    }

    /// `[db, folder, collection]` and its indexes `[db, folder, collection, index]`.
    fn node_target(&self, path: &sqlr_core::NodePath) -> Option<DocTarget> {
        match path.segments() {
            [db, _, coll] | [db, _, coll, _] => Some(DocTarget {
                database: Some(db.clone()),
                collection: coll.clone(),
            }),
            _ => None,
        }
    }

    fn classify(&self, text: &str) -> StatementClass {
        self.language().classify_document(text)
    }

    fn set_read_only(&self, on: bool) {
        self.set_read_only_flag(on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::doc_field_value;

    #[test]
    fn distinct_cells_read_like_the_document_table() {
        // `distinct` hands back BSON values outside a document; they must render exactly
        // as the same field renders in the document table.
        let values = [
            DocValue::ObjectId([0xab; 12]),
            DocValue::DateTime(0),
            DocValue::Decimal128("1.10".into()),
            DocValue::Binary {
                subtype: 0,
                bytes: vec![1, 2],
            },
            DocValue::String("text".into()),
        ];
        for value in &values {
            assert_eq!(cell_value(value), doc_field_value(Some(value)), "{value:?}");
        }
        assert_eq!(
            cell_value(&DocValue::ObjectId([0xab; 12])),
            Value::Text("abababababababababababab".into())
        );
    }

    #[test]
    fn find_limits_cover_zero_negative_and_the_i64_minimum() {
        assert_eq!(find_limit(None).unwrap(), None);
        assert_eq!(find_limit(Some(0)).unwrap(), None);
        assert_eq!(find_limit(Some(5)).unwrap(), Some((5, false)));
        assert_eq!(find_limit(Some(-5)).unwrap(), Some((5, true)));
        // `limit(NumberLong("-9223372036854775808"))`: |limit| does not fit in an i64.
        let error = find_limit(Some(i64::MIN)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Syntax);
    }
}
