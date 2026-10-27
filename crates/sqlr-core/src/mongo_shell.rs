//! The Mongo shell subset sqlr runs without a JavaScript engine (spec §5.6).
//!
//! Statements: `db.<coll>` / `db.getCollection("<name>")` / `db["<name>"]` followed by one
//! collection method (`find` with chained `.sort()`, `.limit()`, `.skip()`,
//! `.projection()`; `aggregate`, `countDocuments`, `estimatedDocumentCount`, `distinct`, the
//! insert/update/replace/delete methods, `findOneAndUpdate`, `createIndex`, `dropIndex`,
//! `getIndexes`, `drop`), `db.runCommand({…})`, `db.getCollectionNames()`,
//! `show dbs|collections`, `use <db>`. Literals are relaxed JSON plus the shell's helpers
//! (`ObjectId`, `ISODate`, `new Date`, `NumberLong`, `NumberInt`, `NumberDecimal`, `UUID`,
//! `BinData`, `Timestamp`, `/regex/flags`, `MinKey`, `MaxKey`) and Extended JSON wrappers.
//! Anything else is an error naming the construct and pointing to `db.runCommand`.

use std::fmt;
use std::ops::Range;

use crate::classify::StatementClass;
use crate::doc::{
    DocParseError, DocValue, MAX_DOC_DEPTH, base64_decode, check_decimal, decode_wrapper,
    int_value, new_object_id, parse_iso_date, parse_object_id, parse_uuid,
};
use crate::lang::{QueryLanguage, split_mongo_shell};
use crate::syntax::{LexState, Token, TokenKind};

/// A parse error at a byte offset of the parsed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellError {
    /// What is wrong, in words for the user.
    pub message: String,
    /// Byte offset in the parsed text where the error was found.
    pub position: usize,
}

impl ShellError {
    /// An error with `message` at byte offset `position`.
    pub fn new(message: impl Into<String>, position: usize) -> Self {
        Self {
            message: message.into(),
            position,
        }
    }
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ShellError {}

const RAW_MODE: &str = "sqlr runs a subset of the mongo shell without a JavaScript engine; \
                        use db.runCommand({...}) for anything else";

/// One Mongo shell statement parsed into the operation a Mongo Driver runs.
#[derive(Clone, Debug, PartialEq)]
pub enum MongoOp {
    /// A `find` with its chained cursor modifiers.
    Find {
        /// The target collection.
        coll: String,
        /// The filter document.
        filter: DocValue,
        /// The projection document, when given.
        projection: Option<DocValue>,
        /// The sort document, when given.
        sort: Option<DocValue>,
        /// Maximum number of documents to return.
        limit: Option<i64>,
        /// Number of documents to skip.
        skip: Option<u64>,
    },
    /// An `aggregate` pipeline.
    Aggregate {
        /// The target collection.
        coll: String,
        /// The aggregation stages, in order.
        pipeline: Vec<DocValue>,
        /// The options document, when given.
        options: Option<DocValue>,
    },
    /// A `countDocuments` count.
    CountDocuments {
        /// The target collection.
        coll: String,
        /// The filter document.
        filter: DocValue,
        /// The options document, when given.
        options: Option<DocValue>,
    },
    /// An `estimatedDocumentCount` count from collection metadata.
    EstimatedDocumentCount {
        /// The target collection.
        coll: String,
    },
    /// A `distinct` over one field.
    Distinct {
        /// The target collection.
        coll: String,
        /// The field whose distinct values are listed.
        field: String,
        /// The filter document.
        filter: Option<DocValue>,
    },
    /// An `insertOne`.
    InsertOne {
        /// The target collection.
        coll: String,
        /// The document to insert.
        doc: DocValue,
    },
    /// An `insertMany`.
    InsertMany {
        /// The target collection.
        coll: String,
        /// The documents to insert.
        docs: Vec<DocValue>,
        /// The options document, when given.
        options: Option<DocValue>,
    },
    /// An `updateOne`.
    UpdateOne {
        /// The target collection.
        coll: String,
        /// The filter document.
        filter: DocValue,
        /// A document of update operators, or a pipeline (an array).
        update: DocValue,
        /// The options document, when given.
        options: Option<DocValue>,
    },
    /// An `updateMany`.
    UpdateMany {
        /// The target collection.
        coll: String,
        /// The filter document.
        filter: DocValue,
        /// The update operators document, or an update pipeline.
        update: DocValue,
        /// The options document, when given.
        options: Option<DocValue>,
    },
    /// A `replaceOne`.
    ReplaceOne {
        /// The target collection.
        coll: String,
        /// The filter document.
        filter: DocValue,
        /// The document that replaces the match.
        replacement: DocValue,
        /// The options document, when given.
        options: Option<DocValue>,
    },
    /// A `deleteOne`.
    DeleteOne {
        /// The target collection.
        coll: String,
        /// The filter document.
        filter: DocValue,
    },
    /// A `deleteMany`.
    DeleteMany {
        /// The target collection.
        coll: String,
        /// The filter document.
        filter: DocValue,
    },
    /// A `findOneAndUpdate`.
    FindOneAndUpdate {
        /// The target collection.
        coll: String,
        /// The filter document.
        filter: DocValue,
        /// The update operators document, or an update pipeline.
        update: DocValue,
        /// The options document, when given.
        options: Option<DocValue>,
    },
    /// A `createIndex`.
    CreateIndex {
        /// The target collection.
        coll: String,
        /// The index key document.
        keys: DocValue,
        /// The options document, when given.
        options: Option<DocValue>,
    },
    /// `index` is the index name (a string) or its key document.
    DropIndex {
        /// The target collection.
        coll: String,
        /// The index name or its key document.
        index: DocValue,
    },
    /// A `getIndexes` listing.
    GetIndexes {
        /// The target collection.
        coll: String,
    },
    /// A collection `drop`.
    Drop {
        /// The target collection.
        coll: String,
    },
    /// A `db.runCommand` with its command document.
    RunCommand(DocValue),
    /// `db.getCollectionNames()`.
    GetCollectionNames,
    /// `show dbs`.
    ShowDbs,
    /// `show collections`.
    ShowCollections,
    /// `use <db>`: switches the current database to the named one.
    Use(String),
}

const COLLECTION_METHODS: &[&str] = &[
    "aggregate",
    "countDocuments",
    "createIndex",
    "deleteMany",
    "deleteOne",
    "distinct",
    "drop",
    "dropIndex",
    "estimatedDocumentCount",
    "find",
    "findOne",
    "findOneAndUpdate",
    "getIndexes",
    "insertMany",
    "insertOne",
    "replaceOne",
    "updateMany",
    "updateOne",
];

const CURSOR_METHODS: &[&str] = &["limit", "pretty", "projection", "skip", "sort", "toArray"];

/// Commands that only read, for classifying `db.runCommand`.
const READ_COMMANDS: &[&str] = &[
    "aggregate",
    "buildInfo",
    "collStats",
    "connectionStatus",
    "count",
    "currentOp",
    "dataSize",
    "dbHash",
    "dbStats",
    "distinct",
    "explain",
    "find",
    "getMore",
    "getParameter",
    "hello",
    "hostInfo",
    "isMaster",
    "listCollections",
    "listDatabases",
    "listIndexes",
    "ping",
    "serverStatus",
    "validate",
];

fn is_empty_doc(v: &DocValue) -> bool {
    matches!(v, DocValue::Document(f) if f.is_empty())
}

/// Whether a `delete` or `update` command's `list` (`deletes`, `updates`) has a statement
/// with an empty filter `q` that reaches every matching document (`many`).
fn every_document(cmd: &DocValue, list: &str, many: impl Fn(&DocValue) -> bool) -> bool {
    let Some(DocValue::Array(statements)) = cmd.get(list) else {
        return false;
    };
    statements
        .iter()
        .any(|s| s.get("q").is_some_and(is_empty_doc) && many(s))
}

fn pipeline_writes(stages: &[DocValue]) -> bool {
    stages
        .iter()
        .any(|s| s.get("$out").is_some() || s.get("$merge").is_some())
}

impl MongoOp {
    /// The collection the operation works on.
    pub fn collection(&self) -> Option<&str> {
        use MongoOp::*;
        match self {
            Find { coll, .. }
            | Aggregate { coll, .. }
            | CountDocuments { coll, .. }
            | EstimatedDocumentCount { coll }
            | Distinct { coll, .. }
            | InsertOne { coll, .. }
            | InsertMany { coll, .. }
            | UpdateOne { coll, .. }
            | UpdateMany { coll, .. }
            | ReplaceOne { coll, .. }
            | DeleteOne { coll, .. }
            | DeleteMany { coll, .. }
            | FindOneAndUpdate { coll, .. }
            | CreateIndex { coll, .. }
            | DropIndex { coll, .. }
            | GetIndexes { coll }
            | Drop { coll } => Some(coll),
            RunCommand(_) | GetCollectionNames | ShowDbs | ShowCollections | Use(_) => None,
        }
    }

    /// The statement class, for read-only sessions and confirmations. `use` only switches
    /// the shell's database, so it reads.
    pub fn class(&self) -> StatementClass {
        use MongoOp::*;
        use StatementClass::{Ddl, Destructive, Other, Read, Write};
        match self {
            Find { .. }
            | CountDocuments { .. }
            | EstimatedDocumentCount { .. }
            | Distinct { .. }
            | GetIndexes { .. }
            | GetCollectionNames
            | ShowDbs
            | ShowCollections
            | Use(_) => Read,
            Aggregate { pipeline, .. } if pipeline_writes(pipeline) => Write,
            Aggregate { .. } => Read,
            UpdateMany { filter, .. } if is_empty_doc(filter) => {
                Destructive("updateMany with an empty filter changes every document".into())
            }
            DeleteMany { filter, .. } if is_empty_doc(filter) => {
                Destructive("deleteMany with an empty filter deletes every document".into())
            }
            InsertOne { .. }
            | InsertMany { .. }
            | UpdateOne { .. }
            | UpdateMany { .. }
            | ReplaceOne { .. }
            | DeleteOne { .. }
            | DeleteMany { .. }
            | FindOneAndUpdate { .. } => Write,
            CreateIndex { .. } => Ddl,
            DropIndex { .. } => Destructive("dropIndex drops an index".into()),
            Drop { coll } => Destructive(format!("drop() drops the collection {coll}")),
            RunCommand(cmd) => {
                let DocValue::Document(fields) = cmd else {
                    return Other;
                };
                let Some((name, _)) = fields.first() else {
                    return Other;
                };
                match name.as_str() {
                    "aggregate" => match cmd.get("pipeline") {
                        Some(DocValue::Array(stages)) if pipeline_writes(stages) => Write,
                        _ => Read,
                    },
                    n if READ_COMMANDS.contains(&n) => Read,
                    "drop" | "dropDatabase" | "dropIndexes" | "dropUser" | "dropRole" => {
                        Destructive(format!("{name} drops data"))
                    }
                    "create"
                    | "createIndexes"
                    | "collMod"
                    | "renameCollection"
                    | "createUser"
                    | "grantRolesToUser"
                    | "revokeRolesFromUser" => Ddl,
                    "delete"
                        if every_document(cmd, "deletes", |s| {
                            matches!(
                                s.get("limit"),
                                Some(DocValue::Int32(0) | DocValue::Int64(0))
                            ) || matches!(s.get("limit"), Some(DocValue::Double(l)) if *l == 0.0)
                        }) =>
                    {
                        Destructive("delete with an empty filter deletes every document".into())
                    }
                    "update"
                        if every_document(cmd, "updates", |s| {
                            s.get("multi") == Some(&DocValue::Bool(true))
                        }) =>
                    {
                        Destructive("update with an empty filter changes every document".into())
                    }
                    "insert" | "update" | "delete" | "findAndModify" => Write,
                    _ => Other,
                }
            }
        }
    }
}

/// Parses a script of shell statements. Each statement comes with its byte range.
pub fn parse_mongo_shell(text: &str) -> Result<Vec<(Range<usize>, MongoOp)>, ShellError> {
    split_mongo_shell(text)
        .into_iter()
        .map(|r| {
            let op = Parser::new(text, r.clone()).statement()?;
            Ok((r, op))
        })
        .collect()
}

/// Parses one value (a document, array or literal) filling all of `text`.
pub fn parse_shell_value(text: &str) -> Result<DocValue, ShellError> {
    let mut p = Parser::new(text, 0..text.len());
    let v = p.value(0)?;
    p.finish()?;
    Ok(v)
}

/// A chained method call: its name token, name and arguments with their offsets.
type Call<'s> = (Token, &'s str, Vec<(DocValue, usize)>);

struct Parser<'a> {
    text: &'a str,
    toks: Vec<Token>,
    i: usize,
    end: usize,
}

fn is_word(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Ident | TokenKind::Keyword | TokenKind::Function
    )
}

impl<'a> Parser<'a> {
    fn new(text: &'a str, range: Range<usize>) -> Self {
        let mut toks = Vec::new();
        QueryLanguage::MongoShell.lex(&text[range.clone()], LexState::Normal, &mut toks);
        for t in &mut toks {
            t.start += range.start;
            t.end += range.start;
        }
        toks.retain(|t| t.kind != TokenKind::Comment);
        Self {
            text,
            toks,
            i: 0,
            end: range.end,
        }
    }

    fn peek(&self) -> Option<Token> {
        self.toks.get(self.i).copied()
    }

    fn peek_at(&self, n: usize) -> Option<Token> {
        self.toks.get(self.i + n).copied()
    }

    fn tok_text(&self, t: Token) -> &'a str {
        &self.text[t.start..t.end]
    }

    fn at(&self, s: &str) -> bool {
        self.peek().is_some_and(|t| self.tok_text(t) == s)
    }

    fn at_n(&self, n: usize, s: &str) -> bool {
        self.peek_at(n).is_some_and(|t| self.tok_text(t) == s)
    }

    fn pos(&self) -> usize {
        self.peek().map_or(self.end, |t| t.start)
    }

    fn err<T>(&self, message: impl Into<String>) -> Result<T, ShellError> {
        Err(ShellError::new(message, self.pos()))
    }

    fn bump(&mut self) -> Option<Token> {
        let t = self.peek();
        self.i += 1;
        t
    }

    fn describe(&self) -> String {
        match self.peek() {
            Some(t) => format!("`{}`", self.tok_text(t)),
            None => "the end of the statement".into(),
        }
    }

    fn expect(&mut self, s: &str) -> Result<Token, ShellError> {
        match self.at(s).then(|| self.bump()).flatten() {
            Some(t) => Ok(t),
            None => self.err(format!("expected `{s}` but found {}", self.describe())),
        }
    }

    fn word(&mut self, what: &str) -> Result<(Token, &'a str), ShellError> {
        match self.peek() {
            Some(t) if is_word(t.kind) => {
                self.i += 1;
                Ok((t, self.tok_text(t)))
            }
            _ => self.err(format!("expected {what} but found {}", self.describe())),
        }
    }

    fn finish(&self) -> Result<(), ShellError> {
        match self.peek() {
            None => Ok(()),
            Some(_) => self.err(format!(
                "unexpected {}; separate statements with `;` or a new line",
                self.describe()
            )),
        }
    }

    fn unsupported<T>(&self, at: usize, construct: &str) -> Result<T, ShellError> {
        Err(ShellError::new(
            format!("{construct} is not supported: {RAW_MODE}"),
            at,
        ))
    }

    // ---- statements ----------------------------------------------------------------------

    fn statement(&mut self) -> Result<MongoOp, ShellError> {
        let (t, w) = match self.peek() {
            Some(t) if is_word(t.kind) => (t, self.tok_text(t)),
            Some(t) => {
                return self.unsupported(t.start, &format!("`{}`", self.tok_text(t)));
            }
            None => return self.err("empty statement"),
        };
        match w {
            "show" => {
                self.bump();
                let (wt, what) = self.word("`dbs` or `collections`")?;
                let op = match what {
                    "dbs" | "databases" => MongoOp::ShowDbs,
                    "collections" | "tables" => MongoOp::ShowCollections,
                    _ => return self.unsupported(wt.start, &format!("`show {what}`")),
                };
                self.finish()?;
                Ok(op)
            }
            "use" => {
                self.bump();
                let name = self.text[t.end..self.end].trim();
                let name = name
                    .strip_prefix(['"', '\''])
                    .and_then(|n| n.strip_suffix(['"', '\'']))
                    .unwrap_or(name);
                if name.is_empty() || name.contains(char::is_whitespace) {
                    return self.err("expected a database name after `use`");
                }
                Ok(MongoOp::Use(name.to_string()))
            }
            "db" => {
                self.bump();
                self.db_statement()
            }
            "function" => self.unsupported(t.start, "a function"),
            _ => self.unsupported(t.start, &format!("`{w}`")),
        }
    }

    fn db_statement(&mut self) -> Result<MongoOp, ShellError> {
        let coll = if self.at("[") {
            self.bump();
            let name = self.string_arg("a collection name")?;
            self.expect("]")?;
            name
        } else {
            self.expect(".")?;
            let (t, w) = self.word("a collection or a database method")?;
            if self.at("(") {
                match w {
                    "runCommand" => {
                        let args = self.args()?;
                        let [cmd] = self.arity::<1, 1>(t, w, args)?;
                        let cmd = self.doc_arg(cmd, "a command document")?;
                        self.finish()?;
                        return Ok(MongoOp::RunCommand(cmd));
                    }
                    "getCollectionNames" => {
                        let args = self.args()?;
                        self.arity::<0, 0>(t, w, args)?;
                        self.finish()?;
                        return Ok(MongoOp::GetCollectionNames);
                    }
                    "getCollection" => {
                        self.expect("(")?;
                        let name = self.string_arg("a collection name")?;
                        self.expect(")")?;
                        name
                    }
                    _ => return self.unsupported(t.start, &format!("`db.{w}()`")),
                }
            } else {
                // `db.system.users.find()`: dotted names until the word before `(`.
                let mut name = w.to_string();
                while self.at(".")
                    && self.peek_at(1).is_some_and(|t| is_word(t.kind))
                    && !self.at_n(2, "(")
                {
                    self.bump();
                    let (_, part) = self.word("a name")?;
                    name.push('.');
                    name.push_str(part);
                }
                name
            }
        };
        self.expect(".")?;
        let (mt, method) = self.word("a collection method")?;
        if !self.at("(") {
            return self.err(format!("expected `(` after `{method}`"));
        }
        // Unknown names fail before their arguments, which may hold anything (`function…`).
        if !COLLECTION_METHODS.contains(&method) {
            return self.unsupported(mt.start, &format!("`{method}()`"));
        }
        let args = self.args()?;
        let mut chain = Vec::new();
        while self.at(".") {
            self.bump();
            let (ct, name) = self.word("a cursor method")?;
            if !self.at("(") {
                return self.err(format!("expected `(` after `{name}`"));
            }
            if !CURSOR_METHODS.contains(&name) {
                return self.unsupported(ct.start, &format!("`.{name}()`"));
            }
            chain.push((ct, name, self.args()?));
        }
        self.finish()?;
        self.collection_op(coll, mt, method, args, chain)
    }

    fn collection_op(
        &self,
        coll: String,
        mt: Token,
        method: &str,
        args: Vec<(DocValue, usize)>,
        chain: Vec<Call<'_>>,
    ) -> Result<MongoOp, ShellError> {
        let no_chain = |chain: &[Call<'_>], allowed: &[&str]| {
            for (ct, name, _) in chain {
                if !allowed.contains(name) {
                    return Err(ShellError::new(
                        format!("`.{name}()` cannot follow {method}(): {RAW_MODE}"),
                        ct.start,
                    ));
                }
            }
            Ok(())
        };
        let op = match method {
            "find" | "findOne" => {
                let [filter, projection, options] = self.arity::<0, 3>(mt, method, args)?;
                if let Some((_, at)) = options {
                    return Err(ShellError::new(
                        format!("{method}() options are not supported: {RAW_MODE}"),
                        at,
                    ));
                }
                let mut filter = self.opt_doc(filter, "a filter document")?;
                let mut projection = self.opt_doc(projection, "a projection document")?;
                let (mut sort, mut limit, mut skip) = (None, None, None);
                if method == "findOne" {
                    no_chain(&chain, &["pretty"])?;
                    limit = Some(1);
                }
                for (ct, name, cargs) in chain {
                    match name {
                        "sort" => {
                            let [v] = self.arity::<1, 1>(ct, name, cargs)?;
                            sort = Some(self.doc_arg(v, "a sort document")?);
                        }
                        "projection" => {
                            let [v] = self.arity::<1, 1>(ct, name, cargs)?;
                            projection = Some(self.doc_arg(v, "a projection document")?);
                        }
                        "limit" => {
                            let [v] = self.arity::<1, 1>(ct, name, cargs)?;
                            limit = Some(self.int_arg(v)?);
                        }
                        "skip" => {
                            let [(v, at)] = self.arity::<1, 1>(ct, name, cargs)?;
                            let n = self.int_arg((v, at))?;
                            skip = Some(u64::try_from(n).map_err(|_| {
                                ShellError::new("skip() needs a number of 0 or more", at)
                            })?);
                        }
                        "pretty" | "toArray" => {
                            self.arity::<0, 0>(ct, name, cargs)?;
                        }
                        _ => return self.unsupported(ct.start, &format!("`.{name}()`")),
                    }
                }
                MongoOp::Find {
                    coll,
                    filter: filter.take().unwrap_or(DocValue::Document(Vec::new())),
                    projection: projection.take(),
                    sort,
                    limit,
                    skip,
                }
            }
            "aggregate" => {
                no_chain(&chain, &["toArray", "pretty"])?;
                let (pipeline, options) = match args.first() {
                    Some((DocValue::Array(_), _)) => {
                        let mut it = args.into_iter();
                        let Some((DocValue::Array(stages), _)) = it.next() else {
                            return self.err("aggregate() needs a pipeline array");
                        };
                        let options = it.next();
                        if let Some((_, at)) = it.next() {
                            return Err(ShellError::new("too many arguments to aggregate()", at));
                        }
                        (stages, self.opt_doc(options, "an options document")?)
                    }
                    // `aggregate({$match…}, {$group…})`: stages as arguments.
                    _ => (args.into_iter().map(|(v, _)| v).collect(), None),
                };
                MongoOp::Aggregate {
                    coll,
                    pipeline,
                    options,
                }
            }
            _ => {
                no_chain(&chain, &[])?;
                self.simple_op(coll, mt, method, args)?
            }
        };
        Ok(op)
    }

    fn simple_op(
        &self,
        coll: String,
        mt: Token,
        method: &str,
        args: Vec<(DocValue, usize)>,
    ) -> Result<MongoOp, ShellError> {
        let filter_doc = |v| self.doc_arg(v, "a filter document");
        Ok(match method {
            "countDocuments" => {
                let [filter, options] = self.arity::<0, 2>(mt, method, args)?;
                MongoOp::CountDocuments {
                    coll,
                    filter: self
                        .opt_doc(filter, "a filter document")?
                        .unwrap_or(DocValue::Document(Vec::new())),
                    options: self.opt_doc(options, "an options document")?,
                }
            }
            "estimatedDocumentCount" => {
                self.arity::<0, 0>(mt, method, args)?;
                MongoOp::EstimatedDocumentCount { coll }
            }
            "distinct" => {
                let [field, filter] = self.arity::<1, 2>(mt, method, args)?;
                let (field, at) = self.required(field)?;
                let DocValue::String(field) = field else {
                    return Err(ShellError::new("distinct() needs a field name string", at));
                };
                MongoOp::Distinct {
                    coll,
                    field,
                    filter: self.opt_doc(filter, "a filter document")?,
                }
            }
            "insertOne" => {
                let [doc] = self.arity::<1, 1>(mt, method, args)?;
                MongoOp::InsertOne {
                    coll,
                    doc: self.doc_arg(doc, "a document")?,
                }
            }
            "insertMany" => {
                let [docs, options] = self.arity::<1, 2>(mt, method, args)?;
                let (docs, at) = self.required(docs)?;
                let DocValue::Array(docs) = docs else {
                    return Err(ShellError::new(
                        "insertMany() needs an array of documents",
                        at,
                    ));
                };
                if docs.iter().any(|d| !matches!(d, DocValue::Document(_))) {
                    return Err(ShellError::new(
                        "insertMany() needs an array of documents",
                        at,
                    ));
                }
                MongoOp::InsertMany {
                    coll,
                    docs,
                    options: self.opt_doc(options, "an options document")?,
                }
            }
            "updateOne" | "updateMany" | "findOneAndUpdate" => {
                let [filter, update, options] = self.arity::<2, 3>(mt, method, args)?;
                let filter = filter_doc(self.required(filter)?)?;
                let (update, at) = self.required(update)?;
                if !matches!(update, DocValue::Document(_) | DocValue::Array(_)) {
                    return Err(ShellError::new(
                        format!("{method}() needs an update document or pipeline"),
                        at,
                    ));
                }
                let options = self.opt_doc(options, "an options document")?;
                match method {
                    "updateOne" => MongoOp::UpdateOne {
                        coll,
                        filter,
                        update,
                        options,
                    },
                    "updateMany" => MongoOp::UpdateMany {
                        coll,
                        filter,
                        update,
                        options,
                    },
                    _ => MongoOp::FindOneAndUpdate {
                        coll,
                        filter,
                        update,
                        options,
                    },
                }
            }
            "replaceOne" => {
                let [filter, replacement, options] = self.arity::<2, 3>(mt, method, args)?;
                MongoOp::ReplaceOne {
                    coll,
                    filter: filter_doc(self.required(filter)?)?,
                    replacement: self
                        .doc_arg(self.required(replacement)?, "a replacement document")?,
                    options: self.opt_doc(options, "an options document")?,
                }
            }
            "deleteOne" | "deleteMany" => {
                let [filter] = self.arity::<1, 1>(mt, method, args)?;
                let filter = filter_doc(filter)?;
                if method == "deleteOne" {
                    MongoOp::DeleteOne { coll, filter }
                } else {
                    MongoOp::DeleteMany { coll, filter }
                }
            }
            "createIndex" => {
                let [keys, options] = self.arity::<1, 2>(mt, method, args)?;
                MongoOp::CreateIndex {
                    coll,
                    keys: self.doc_arg(self.required(keys)?, "a key document")?,
                    options: self.opt_doc(options, "an options document")?,
                }
            }
            "dropIndex" => {
                let [(index, at)] = self.arity::<1, 1>(mt, method, args)?;
                if !matches!(index, DocValue::String(_) | DocValue::Document(_)) {
                    return Err(ShellError::new(
                        "dropIndex() needs an index name or key document",
                        at,
                    ));
                }
                MongoOp::DropIndex { coll, index }
            }
            "getIndexes" => {
                self.arity::<0, 0>(mt, method, args)?;
                MongoOp::GetIndexes { coll }
            }
            "drop" => {
                self.arity::<0, 0>(mt, method, args)?;
                MongoOp::Drop { coll }
            }
            _ => return self.unsupported(mt.start, &format!("`{method}()`")),
        })
    }

    /// Checks `args` holds `MIN..=MAX` values, padding the result with `None`. With
    /// `MIN == MAX` use [`Self::arity_exact`]-style destructuring via `[x]`.
    fn arity<const MIN: usize, const MAX: usize>(
        &self,
        t: Token,
        name: &str,
        args: Vec<(DocValue, usize)>,
    ) -> Result<<Self as Arity<MIN, MAX>>::Out, ShellError>
    where
        Self: Arity<MIN, MAX>,
    {
        if args.len() < MIN {
            return Err(ShellError::new(
                format!(
                    "{name}() needs {MIN} argument{}",
                    if MIN == 1 { "" } else { "s" }
                ),
                t.start,
            ));
        }
        if args.len() > MAX {
            return Err(ShellError::new(
                format!("too many arguments to {name}()"),
                args[MAX].1,
            ));
        }
        Ok(<Self as Arity<MIN, MAX>>::shape(args))
    }

    /// An argument that [`Self::arity`] guarantees is present; a missing one is a parse error.
    fn required(&self, arg: Option<(DocValue, usize)>) -> Result<(DocValue, usize), ShellError> {
        arg.ok_or_else(|| ShellError::new("missing argument", self.pos()))
    }

    fn doc_arg(&self, (v, at): (DocValue, usize), what: &str) -> Result<DocValue, ShellError> {
        match v {
            DocValue::Document(_) => Ok(v),
            _ => Err(ShellError::new(format!("expected {what}"), at)),
        }
    }

    fn opt_doc(
        &self,
        v: Option<(DocValue, usize)>,
        what: &str,
    ) -> Result<Option<DocValue>, ShellError> {
        v.map(|v| self.doc_arg(v, what)).transpose()
    }

    fn int_arg(&self, (v, at): (DocValue, usize)) -> Result<i64, ShellError> {
        match v {
            DocValue::Int32(i) => Ok(i64::from(i)),
            DocValue::Int64(i) => Ok(i),
            DocValue::Double(d) if d.fract() == 0.0 && d.abs() < 9e15 => Ok(d as i64),
            _ => Err(ShellError::new("expected a whole number", at)),
        }
    }

    fn string_arg(&mut self, what: &str) -> Result<String, ShellError> {
        let at = self.pos();
        match self.value(0)? {
            DocValue::String(s) => Ok(s),
            _ => Err(ShellError::new(format!("expected {what} string"), at)),
        }
    }

    /// `( value, … )` with an optional trailing comma; each value with its offset.
    fn args(&mut self) -> Result<Vec<(DocValue, usize)>, ShellError> {
        self.args_depth(0)
    }

    // ---- values --------------------------------------------------------------------------

    fn value(&mut self, depth: usize) -> Result<DocValue, ShellError> {
        if depth > MAX_DOC_DEPTH {
            return self.err(format!("values nest deeper than {MAX_DOC_DEPTH} levels"));
        }
        let Some(t) = self.peek() else {
            return self.err("expected a value");
        };
        let s = self.tok_text(t);
        match t.kind {
            TokenKind::Punct if s == "{" => self.object(depth),
            TokenKind::Punct if s == "[" => {
                self.bump();
                let mut items = Vec::new();
                while !self.at("]") {
                    items.push(self.value(depth + 1)?);
                    if !self.at("]") {
                        self.expect(",")?;
                    }
                }
                self.bump();
                Ok(DocValue::Array(items))
            }
            TokenKind::String | TokenKind::QuotedIdent if s.starts_with('/') => {
                self.bump();
                let Some(close) = s.rfind('/') else {
                    return self.err("expected a regular expression");
                };
                let mut flags: Vec<char> = s[close + 1..].chars().collect();
                flags.sort_unstable();
                Ok(DocValue::Regex {
                    pattern: s[1..close].to_string(),
                    options: flags.into_iter().collect(),
                })
            }
            TokenKind::String | TokenKind::QuotedIdent => {
                let v = self.string_token(t)?;
                self.bump();
                Ok(DocValue::String(v))
            }
            TokenKind::Number => {
                let v = parse_number(s)
                    .ok_or_else(|| ShellError::new(format!("invalid number `{s}`"), t.start))?;
                self.bump();
                Ok(v)
            }
            TokenKind::Operator if s == "-" || s == "+" => {
                self.bump();
                match self.peek() {
                    Some(n) if self.tok_text(n) == "Infinity" => {
                        self.bump();
                        Ok(DocValue::Double(if s == "-" {
                            f64::NEG_INFINITY
                        } else {
                            f64::INFINITY
                        }))
                    }
                    _ => self.err("expected a number"),
                }
            }
            k if is_word(k) => self.word_value(t, s, depth),
            _ => self.err(format!("expected a value but found `{s}`")),
        }
    }

    /// The value of a quoted string token; an unterminated string is an error at its start.
    fn string_token(&self, t: Token) -> Result<String, ShellError> {
        let s = self.tok_text(t);
        if !crate::lang::js_string_closed(s) {
            return Err(ShellError::new(
                "unterminated string: add the closing quote",
                t.start,
            ));
        }
        Ok(decode_string(s))
    }

    fn object(&mut self, depth: usize) -> Result<DocValue, ShellError> {
        let Some(open) = self.bump() else {
            return self.err("expected `{`");
        };
        let mut fields = Vec::new();
        while !self.at("}") {
            let Some(k) = self.peek() else {
                return self.err("expected `}`");
            };
            let ks = self.tok_text(k);
            let key = match k.kind {
                TokenKind::String | TokenKind::QuotedIdent if !ks.starts_with('/') => {
                    self.string_token(k)?
                }
                TokenKind::Number => ks.to_string(),
                kind if is_word(kind) => ks.to_string(),
                _ => return self.err(format!("expected a key but found `{ks}`")),
            };
            self.bump();
            self.expect(":")?;
            fields.push((key, self.value(depth + 1)?));
            if !self.at("}") {
                if self.peek().is_none() || !self.at(",") {
                    return self.err(format!(
                        "expected `,` or `}}` but found {}",
                        self.describe()
                    ));
                }
                self.bump();
            }
        }
        self.bump();
        decode_wrapper(fields).map_err(|e| ShellError::new(e.to_string(), open.start))
    }

    fn word_value(&mut self, t: Token, w: &str, depth: usize) -> Result<DocValue, ShellError> {
        if w == "function" {
            return self.unsupported(t.start, "a function");
        }
        let call = self.at_n(1, "(");
        if !call {
            self.bump();
            return match w {
                "true" => Ok(DocValue::Bool(true)),
                "false" => Ok(DocValue::Bool(false)),
                "null" => Ok(DocValue::Null),
                "undefined" => Ok(DocValue::Undefined),
                "Infinity" => Ok(DocValue::Double(f64::INFINITY)),
                "NaN" => Ok(DocValue::Double(f64::NAN)),
                "MinKey" => Ok(DocValue::MinKey),
                "MaxKey" => Ok(DocValue::MaxKey),
                "new" => {
                    let (ct, name) = self.word("a constructor")?;
                    if !self.at("(") {
                        return self.err(format!("expected `(` after `{name}`"));
                    }
                    self.helper(ct, name, true, depth)
                }
                _ => Err(ShellError::new(
                    format!(
                        "`{w}` is not supported: variables and expressions are JavaScript; {RAW_MODE}"
                    ),
                    t.start,
                )),
            };
        }
        self.bump();
        self.helper(t, w, false, depth)
    }

    /// A helper call such as `ObjectId("…")`, with the name already consumed.
    fn helper(
        &mut self,
        t: Token,
        name: &str,
        new: bool,
        depth: usize,
    ) -> Result<DocValue, ShellError> {
        let args_at = self.pos();
        let raw_first = self.peek_at(1).map(|t| self.tok_text(t));
        let args = self.args_depth(depth)?;
        let bad = |what: &str| Err(ShellError::new(format!("{name}() needs {what}"), t.start));
        let one_string = |args: &[(DocValue, usize)]| match args {
            [(DocValue::String(s), _)] => Some(s.clone()),
            _ => None,
        };
        let wrap = |r: Result<DocValue, DocParseError>, at: usize| {
            r.map_err(|e| ShellError::new(e.to_string(), at))
        };
        let first_at = args.first().map_or(args_at, |a| a.1);
        match name {
            "ObjectId" | "ObjectID" => match args.as_slice() {
                [] => Ok(DocValue::ObjectId(new_object_id())),
                [(DocValue::String(s), at)] => {
                    wrap(parse_object_id(s).map(DocValue::ObjectId), *at)
                }
                _ => bad("a 24-digit hex string"),
            },
            "ISODate" | "Date" => match args.as_slice() {
                [] => Ok(DocValue::DateTime(now_ms())),
                [(DocValue::String(s), at)] => wrap(parse_iso_date(s).map(DocValue::DateTime), *at),
                [(n @ (DocValue::Int32(_) | DocValue::Int64(_) | DocValue::Double(_)), at)]
                    if name == "Date" =>
                {
                    let ms = self.int_arg((n.clone(), *at))?;
                    Ok(DocValue::DateTime(ms))
                }
                _ if name == "Date" && args.len() > 1 => Err(ShellError::new(
                    format!(
                        "{}Date(year, month, …) is not supported: use ISODate(\"2024-01-31T00:00:00Z\")",
                        if new { "new " } else { "" }
                    ),
                    t.start,
                )),
                _ => bad("an ISO 8601 date string"),
            },
            "NumberLong" | "Long" => match args.as_slice() {
                [(DocValue::String(s), at)] => s
                    .trim()
                    .parse()
                    .map(DocValue::Int64)
                    .map_err(|_| ShellError::new(format!("invalid NumberLong \"{s}\""), *at)),
                [(v, at)] => Ok(DocValue::Int64(self.int_arg((v.clone(), *at))?)),
                _ => bad("a number or a numeric string"),
            },
            "NumberInt" | "Int32" => match args.as_slice() {
                [(DocValue::String(s), at)] => s
                    .trim()
                    .parse()
                    .map(DocValue::Int32)
                    .map_err(|_| ShellError::new(format!("invalid NumberInt \"{s}\""), *at)),
                [(v, at)] => {
                    let n = self.int_arg((v.clone(), *at))?;
                    i32::try_from(n)
                        .map(DocValue::Int32)
                        .map_err(|_| ShellError::new("NumberInt() is out of range", *at))
                }
                _ => bad("a number"),
            },
            "NumberDecimal" | "Decimal128" => {
                let s = match args.as_slice() {
                    [(DocValue::String(s), _)] => s.clone(),
                    // A number keeps the digits as typed.
                    [(DocValue::Int32(_) | DocValue::Int64(_) | DocValue::Double(_), _)] => {
                        raw_first.unwrap_or_default().to_string()
                    }
                    _ => return bad("a decimal string"),
                };
                wrap(
                    check_decimal(&s).map(|()| DocValue::Decimal128(s)),
                    first_at,
                )
            }
            "UUID" => match one_string(&args) {
                Some(s) => wrap(
                    parse_uuid(&s).map(|bytes| DocValue::Binary { subtype: 4, bytes }),
                    first_at,
                ),
                None => bad("a UUID string"),
            },
            "BinData" => match args.as_slice() {
                [(sub, sat), (DocValue::String(b64), bat)] => {
                    let sub = self.int_arg((sub.clone(), *sat))?;
                    let subtype = u8::try_from(sub)
                        .map_err(|_| ShellError::new("BinData() subtype must be 0–255", *sat))?;
                    wrap(
                        base64_decode(b64).map(|bytes| DocValue::Binary { subtype, bytes }),
                        *bat,
                    )
                }
                _ => bad("a subtype and a base64 string"),
            },
            "Timestamp" => match args.as_slice() {
                [] => Ok(DocValue::Timestamp { t: 0, i: 0 }),
                [(a, aat), (b, bat)] => {
                    let to_u32 = |v: &DocValue, at: usize| -> Result<u32, ShellError> {
                        let n = self.int_arg((v.clone(), at))?;
                        u32::try_from(n)
                            .map_err(|_| ShellError::new("out of range for Timestamp()", at))
                    };
                    Ok(DocValue::Timestamp {
                        t: to_u32(a, *aat)?,
                        i: to_u32(b, *bat)?,
                    })
                }
                [(d @ DocValue::Document(_), at)] => {
                    let get = |k: &str| -> Result<u32, ShellError> {
                        let v = d.get(k).cloned().unwrap_or(DocValue::Null);
                        let n = self.int_arg((v, *at))?;
                        u32::try_from(n)
                            .map_err(|_| ShellError::new("out of range for Timestamp()", *at))
                    };
                    Ok(DocValue::Timestamp {
                        t: get("t")?,
                        i: get("i")?,
                    })
                }
                _ => bad("seconds and an increment"),
            },
            "MinKey" if args.is_empty() => Ok(DocValue::MinKey),
            "MaxKey" if args.is_empty() => Ok(DocValue::MaxKey),
            "RegExp" => match args.as_slice() {
                [(DocValue::String(p), _)] => Ok(DocValue::Regex {
                    pattern: p.clone(),
                    options: String::new(),
                }),
                [(DocValue::String(p), _), (DocValue::String(o), _)] => {
                    let mut flags: Vec<char> = o.chars().collect();
                    flags.sort_unstable();
                    Ok(DocValue::Regex {
                        pattern: p.clone(),
                        options: flags.into_iter().collect(),
                    })
                }
                _ => bad("a pattern string and optional flags"),
            },
            _ => self.unsupported(t.start, &format!("`{name}()`")),
        }
    }

    fn args_depth(&mut self, depth: usize) -> Result<Vec<(DocValue, usize)>, ShellError> {
        self.expect("(")?;
        let mut out = Vec::new();
        while !self.at(")") {
            let at = self.pos();
            out.push((self.value(depth + 1)?, at));
            if !self.at(")") {
                self.expect(",")?;
            }
        }
        self.expect(")")?;
        Ok(out)
    }
}

/// Fixed-size argument lists: `[T; N]` for an exact count, `[Option<T>; MAX]` otherwise.
trait Arity<const MIN: usize, const MAX: usize> {
    type Out;
    fn shape(args: Vec<(DocValue, usize)>) -> Self::Out;
}

macro_rules! exact {
    ($n:literal) => {
        impl Arity<$n, $n> for Parser<'_> {
            type Out = [(DocValue, usize); $n];
            fn shape(args: Vec<(DocValue, usize)>) -> Self::Out {
                args.try_into()
                    .unwrap_or_else(|_| unreachable!("arity checked"))
            }
        }
    };
}
exact!(0);
exact!(1);

macro_rules! ranged {
    ($min:literal, $max:literal) => {
        impl Arity<$min, $max> for Parser<'_> {
            type Out = [Option<(DocValue, usize)>; $max];
            fn shape(args: Vec<(DocValue, usize)>) -> Self::Out {
                let mut it = args.into_iter();
                std::array::from_fn(|_| it.next())
            }
        }
    };
}
ranged!(0, 2);
ranged!(0, 3);
ranged!(1, 2);
ranged!(2, 3);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// A number token: hex and plain integers as `Int32` (or `Int64` when larger, `Double`
/// beyond that), anything with a fraction or exponent as `Double`; `None` for text that
/// is not a number (`0x`, `1.2.3`).
fn parse_number(s: &str) -> Option<DocValue> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        let v = i64::from_str_radix(hex, 16).ok()?;
        return Some(int_value(if neg { -v } else { v }));
    }
    if !s.contains(['.', 'e', 'E'])
        && let Ok(v) = s.parse::<i64>()
    {
        return Some(int_value(v));
    }
    let v: f64 = s.parse().ok()?;
    v.is_finite().then_some(DocValue::Double(v))
}

/// The value of a closed quoted JavaScript string token, escapes resolved.
fn decode_string(tok: &str) -> String {
    let inner = &tok[1..tok.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = chars.next() else { break };
        match e {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '0' => out.push('\0'),
            '\n' => {}
            'x' => {
                let h: String = chars.by_ref().take(2).collect();
                out.extend(u32::from_str_radix(&h, 16).ok().and_then(char::from_u32));
            }
            'u' => {
                let code = if chars.peek() == Some(&'{') {
                    chars.next();
                    let h: String = chars.by_ref().take_while(|&c| c != '}').collect();
                    u32::from_str_radix(&h, 16).ok()
                } else {
                    let h: String = chars.by_ref().take(4).collect();
                    u32::from_str_radix(&h, 16).ok()
                };
                match code {
                    // A surrogate pair: `😀`.
                    Some(hi @ 0xD800..=0xDBFF) => {
                        let mut look = chars.clone();
                        if look.next() == Some('\\') && look.next() == Some('u') {
                            let h: String = look.by_ref().take(4).collect();
                            if let Ok(lo @ 0xDC00..=0xDFFF) = u32::from_str_radix(&h, 16) {
                                chars = look;
                                let c = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                out.extend(char::from_u32(c));
                                continue;
                            }
                        }
                        out.push('\u{FFFD}');
                    }
                    Some(c) => out.push(char::from_u32(c).unwrap_or('\u{FFFD}')),
                    None => out.push('\u{FFFD}'),
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> MongoOp {
        let mut ops = parse_mongo_shell(text).unwrap();
        assert_eq!(ops.len(), 1, "{text}");
        ops.pop().unwrap().1
    }

    #[test]
    fn new_date_without_arguments_is_now() {
        let MongoOp::InsertOne { doc, .. } = one("db.c.insertOne({a: new Date(), b: ObjectId()})")
        else {
            panic!()
        };
        let Some(DocValue::DateTime(ms)) = doc.get("a") else {
            panic!()
        };
        assert!((now_ms() - ms).abs() < 60_000);
        assert!(matches!(doc.get("b"), Some(DocValue::ObjectId(_))));
    }

    #[test]
    fn strings_decode_escapes() {
        assert_eq!(
            decode_string(r#""a\n\tè\x41\u{1F600}😀\'""#),
            "a\n\tèA😀😀'"
        );
        assert_eq!(decode_string("'it\\'s'"), "it's");
    }

    #[test]
    fn classes() {
        use StatementClass::*;
        assert_eq!(one("db.c.find()").class(), Read);
        assert_eq!(one("db.c.insertOne({})").class(), Write);
        assert!(matches!(one("db.c.deleteMany({})").class(), Destructive(_)));
        assert_eq!(one("db.c.deleteMany({a: 1})").class(), Write);
        assert!(matches!(
            one("db.c.updateMany({}, {$set: {a: 1}})").class(),
            Destructive(_)
        ));
        assert!(matches!(one("db.c.drop()").class(), Destructive(_)));
        assert_eq!(one("db.c.createIndex({a: 1})").class(), Ddl);
        assert_eq!(one("db.c.aggregate([{$out: 'x'}])").class(), Write);
        assert_eq!(one("db.runCommand({ping: 1})").class(), Read);
        assert!(matches!(
            one("db.runCommand({drop: 'c'})").class(),
            Destructive(_)
        ));
        assert_eq!(one("db.runCommand({shutdown: 1})").class(), Other);
        // runCommand writes over every document are destructive, like deleteMany({}).
        assert!(matches!(
            one("db.runCommand({delete: 'c', deletes: [{q: {}, limit: 0}]})").class(),
            Destructive(_)
        ));
        assert!(matches!(
            one("db.runCommand({update: 'c', updates: [{q: {}, u: {$set: {a: 1}}, multi: true}]})")
                .class(),
            Destructive(_)
        ));
        assert_eq!(
            one("db.runCommand({delete: 'c', deletes: [{q: {a: 1}, limit: 0}]})").class(),
            Write
        );
        assert_eq!(
            one("db.runCommand({delete: 'c', deletes: [{q: {}, limit: 1}]})").class(),
            Write
        );
        assert_eq!(
            one("db.runCommand({update: 'c', updates: [{q: {}, u: {$set: {a: 1}}}]})").class(),
            Write
        );
        assert_eq!(one("use x").class(), Read);
        assert_eq!(one("db.c.find()").collection(), Some("c"));
    }

    #[test]
    fn shell_values() {
        assert_eq!(
            parse_shell_value("[1, 'a',]").unwrap(),
            DocValue::Array(vec![DocValue::Int32(1), DocValue::String("a".into())])
        );
        assert!(parse_shell_value("{a: 1} x").is_err());
        let mut deep = "[".repeat(600);
        deep.push_str(&"]".repeat(600));
        assert!(
            parse_shell_value(&deep)
                .unwrap_err()
                .message
                .contains("deeper")
        );
    }
}
