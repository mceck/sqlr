//! Driver traits implemented by database backends.

use std::borrow::Cow;
use std::sync::Arc;

use crate::admin::Admin;
use crate::catalog::SchemaCatalog;
use crate::chunk::{ColumnMeta, ResultChunk};
use crate::config::{ConnectionConfig, Secrets};
use crate::error::{DriverError, ErrorKind};
use crate::manifest::DriverManifest;
use crate::node::{Node, NodePath};
use crate::schema::{ObjectDef, ObjectRef, TableDef};
use crate::schema_change::{SchemaChange, diff_table};
use crate::syntax::{LexState, SqlSyntax, TokenKind};
use crate::table::{TableInfo, TableRef};
use crate::value::Value;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerInfo {
    pub product: String,
    pub version: String,
}

#[async_trait::async_trait]
pub trait Driver: Send + Sync {
    fn manifest(&self) -> &DriverManifest;
    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError>;
    /// Dialect data (syntax, default schemas) for editors and autocomplete.
    fn dialect(&self) -> Option<Arc<dyn Dialect>> {
        None
    }
}

#[async_trait::async_trait]
pub trait Connection: Send + Sync {
    async fn ping(&self) -> Result<ServerInfo, DriverError>;
    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError>;
    fn cancel_handle(&self) -> Arc<dyn CancelHandle>;
    fn relational(&self) -> Option<&dyn RelationalConn> {
        None
    }
    /// The document family (MongoDB, Elasticsearch).
    fn document(&self) -> Option<&dyn crate::document::DocumentConn> {
        None
    }
    /// The key-value family (Redis).
    fn key_value(&self) -> Option<&dyn crate::kv::KeyValueConn> {
        None
    }
    /// The database this connection is attached to, when the driver can browse several.
    fn database_name(&self) -> Option<&str> {
        None
    }
    /// The path whose children are this connection's schemas: the root, or the current
    /// database when the explorer lists databases first.
    fn schema_path(&self) -> NodePath {
        NodePath::root()
    }
    /// The database whose objects `path` belongs to, when it is not this connection's own.
    fn database_for_path(&self, _path: &NodePath) -> Option<String> {
        None
    }
    /// Opens a connection to another database on the same server.
    async fn connect_database(&self, _database: &str) -> Result<Arc<dyn Connection>, DriverError> {
        Err(DriverError::new(
            ErrorKind::Unsupported,
            "multiple databases are not supported",
        ))
    }
}

#[async_trait::async_trait]
pub trait CancelHandle: Send + Sync {
    async fn cancel(&self) -> Result<(), DriverError>;
}

pub enum Execution {
    Rows(Box<dyn RowStream>),
    Affected(u64),
}

#[async_trait::async_trait]
pub trait RelationalConn: Send + Sync {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError>;
    /// Runs `sql` with `params` bound to its placeholders ([`Dialect::placeholder`]).
    async fn execute_with(&self, _sql: &str, _params: &[Value]) -> Result<Execution, DriverError> {
        Err(DriverError::new(
            ErrorKind::Unsupported,
            "bound parameters are not supported",
        ))
    }
    /// Columns and keys of `table`.
    async fn describe(&self, _table: &TableRef) -> Result<TableInfo, DriverError> {
        Err(DriverError::new(
            ErrorKind::Unsupported,
            "table descriptions are not supported",
        ))
    }
    /// The plan of `sql` (one statement): the dialect's explain statements run on this
    /// connection. With `analyze` the statement runs; a data change is rolled back.
    async fn explain(
        &self,
        sql: &str,
        analyze: bool,
    ) -> Result<crate::plan::PlanTree, DriverError> {
        let dialect = self.dialect();
        let statement = crate::plan::explain_check(&dialect.syntax(), sql)?;
        let spec = dialect.explain_spec(&statement, analyze)?;
        let run = |sql: String| async move {
            let mut rows: Vec<Vec<Value>> = Vec::new();
            if let Execution::Rows(mut stream) = self.execute(&sql).await? {
                while let Some(chunk) = stream.next_chunk(500).await? {
                    for r in 0..chunk.len() {
                        rows.push(
                            (0..chunk.columns().len())
                                .map(|c| Value::from_cell(chunk.cell(r, c)))
                                .collect(),
                        );
                    }
                }
                stream.close().await?;
            }
            Ok::<_, DriverError>(rows)
        };
        for sql in &spec.before {
            run(sql.clone()).await?;
        }
        let rows = run(spec.sql.clone()).await;
        // Whatever the explain did, the transaction around it ends.
        for sql in &spec.after {
            let _ = run(sql.clone()).await;
        }
        spec.parser.parse(&rows?)
    }
    /// The SQL that creates the object `node` stands for: its definition loaded through
    /// [`Introspect`] and written by the dialect's [`DdlGen`].
    async fn ddl(&self, node: &Node) -> Result<String, DriverError> {
        let (Some(introspect), Some(generator)) = (self.introspect(), self.dialect().ddl_gen())
        else {
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                "DDL is not supported",
            ));
        };
        let object = self.object_ref(node).ok_or_else(|| {
            DriverError::new(
                ErrorKind::Unsupported,
                format!("no DDL for {:?}", node.path.segments()),
            )
        })?;
        let def = introspect.load_object(&object).await?;
        generator.create_ddl(&def)
    }
    /// The object an explorer node stands for; `None` for nodes that are no object
    /// (folders, columns).
    fn object_ref(&self, _node: &Node) -> Option<ObjectRef> {
        None
    }
    /// Object definitions, for object editors.
    fn introspect(&self) -> Option<&dyn Introspect> {
        None
    }
    /// Users, roles, sessions, locks, properties and statistics.
    fn admin(&self) -> Option<&dyn Admin> {
        None
    }
    /// Server notices and warnings received since the last call.
    async fn take_notices(&self) -> Vec<String> {
        Vec::new()
    }
    /// Confirms, in the connection's open transaction, that `table` (named by a column
    /// origin of this connection's results) is still the table the origin was read from;
    /// an error when it was renamed or dropped since. Drivers whose origins cannot go stale
    /// keep the default.
    async fn check_origin(&self, _table: &TableRef) -> Result<(), DriverError> {
        Ok(())
    }
    fn dialect(&self) -> &dyn Dialect;
    /// Tables, views and columns of `schema`, for autocomplete.
    async fn catalog(&self, _schema: &str) -> Result<SchemaCatalog, DriverError> {
        Err(DriverError::new(ErrorKind::Unsupported, "no catalog"))
    }
}

#[async_trait::async_trait]
pub trait RowStream: Send {
    fn columns(&self) -> Arc<[ColumnMeta]>;
    /// Next chunk of at most `max_rows` rows; `Ok(None)` once the result is exhausted.
    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError>;
    /// Releases server resources (cursor, transaction). Idempotent.
    async fn close(&mut self) -> Result<(), DriverError>;
}

pub trait Dialect: Send + Sync {
    fn quote_ident(&self, ident: &str) -> String;
    /// `SELECT * FROM <object>` (+ ` LIMIT n`) for nodes whose kind opens data; `None` otherwise.
    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String>;
    fn syntax(&self) -> Arc<SqlSyntax> {
        SqlSyntax::ansi()
    }
    /// Schemas searched when a name is unqualified, in order.
    fn default_schemas(&self) -> Vec<String> {
        Vec::new()
    }
    /// Whether the server keeps databases and schemas at the same level (MySQL), so a
    /// database name is also a schema name.
    fn database_is_schema(&self) -> bool {
        false
    }
    /// The placeholder for the `index`-th bound parameter (1-based).
    fn placeholder(&self, _index: usize) -> String {
        "?".to_string()
    }
    /// `table` quoted and qualified by its schema when it has one.
    fn qualified(&self, table: &TableRef) -> String {
        match &table.schema {
            Some(schema) => format!(
                "{}.{}",
                self.quote_ident(schema),
                self.quote_ident(&table.name)
            ),
            None => self.quote_ident(&table.name),
        }
    }
    fn begin_sql(&self) -> &'static str {
        "BEGIN"
    }
    fn savepoint_sql(&self, name: &str) -> String {
        format!("SAVEPOINT {}", self.quote_ident(name))
    }
    fn rollback_to_sql(&self, name: &str) -> String {
        format!("ROLLBACK TO SAVEPOINT {}", self.quote_ident(name))
    }
    /// The statement that turns the session's read-only mode on or off; `None` when the
    /// server has no such setting.
    fn read_only_sql(&self, _on: bool) -> Option<String> {
        None
    }
    /// `sql` re-run sorted by `order` (`(column, descending)`), NULLs last.
    fn wrap_order(&self, sql: &str, order: &[(String, bool)]) -> String {
        let keys: Vec<String> = order
            .iter()
            .map(|(column, desc)| {
                let dir = if *desc { " DESC" } else { "" };
                format!("{}{dir} NULLS LAST", self.quote_ident(column))
            })
            .collect();
        wrap_subquery(&self.syntax(), sql, &keys)
    }
    /// `sql` re-run filtered by the user's `condition`.
    fn wrap_where(&self, sql: &str, condition: &str) -> String {
        format!(
            "SELECT * FROM (\n{}\n) sqlr_q WHERE {}",
            trim_statement_in(&self.syntax(), sql),
            condition.trim()
        )
    }
    /// The server commits an open transaction before running DDL (MySQL).
    fn ddl_commits_transaction(&self) -> bool {
        false
    }
    /// A failed statement aborts the open transaction until it is rolled back (PostgreSQL).
    fn error_aborts_transaction(&self) -> bool {
        false
    }
    /// `text` as a string literal.
    fn string_literal(&self, text: &str) -> String {
        format!("'{}'", text.replace('\'', "''"))
    }
    /// `value` as a boolean literal (servers without `TRUE`/`FALSE` write a bit).
    fn bool_literal(&self, value: bool) -> &'static str {
        if value { "TRUE" } else { "FALSE" }
    }
    /// `bytes` as a binary literal.
    fn bytes_literal(&self, bytes: &[u8]) -> String {
        let hex = crate::chunk::hex_bytes(bytes);
        format!("X'{}'", &hex[2..])
    }
    /// An `INSERT` of one row of defaults into `table` (already quoted and qualified).
    fn insert_defaults_sql(&self, table: &str) -> String {
        format!("INSERT INTO {table} DEFAULT VALUES")
    }

    /// `insert` (a plain `INSERT INTO … VALUES (…)` this dialect wrote) turned into an
    /// insert-or-update by the `key` columns, updating the `update` columns on a conflict
    /// (none: keep the existing row). `None` when the dialect cannot.
    fn upsert_sql(&self, _insert: &str, _key: &[String], _update: &[String]) -> Option<String> {
        None
    }
    /// A one-row insert-or-update of `table` (already quoted and qualified): `columns` are
    /// the raw names, `placeholders` the matching bound parameters. The default writes the
    /// plain insert and hands it to [`Dialect::upsert_sql`]; dialects that cannot extend an
    /// `INSERT` (`MERGE`) override it.
    fn upsert_row_sql(
        &self,
        table: &str,
        columns: &[String],
        placeholders: &[String],
        key: &[String],
        update: &[String],
    ) -> Option<String> {
        let names: Vec<String> = columns.iter().map(|c| self.quote_ident(c)).collect();
        let insert = format!(
            "INSERT INTO {table} ({}) VALUES ({})",
            names.join(", "),
            placeholders.join(", ")
        );
        self.upsert_sql(&insert, key, update)
    }
    /// The statements that switch foreign keys (and unique checks, where the server has
    /// them) off for a bulk load, and the ones that switch them back on. They run outside
    /// the load's transaction. `None` when the dialect cannot.
    fn constraints_toggle_sql(&self) -> Option<(Vec<String>, Vec<String>)> {
        None
    }

    /// Statements that put the session in the state a bulk load assumes (UTC for a server
    /// that reads datetime literals in its session zone), and the ones that put it back.
    /// They run outside the load's transaction. `None` when nothing is needed.
    fn load_session_sql(&self) -> Option<(Vec<String>, Vec<String>)> {
        None
    }

    /// The statements that explain `sql` (one statement), with `analyze` the server runs it:
    /// a data change is run inside a transaction that is rolled back.
    fn explain_spec(
        &self,
        _sql: &str,
        _analyze: bool,
    ) -> Result<crate::plan::ExplainSpec, DriverError> {
        Err(DriverError::new(
            ErrorKind::Unsupported,
            "this database has no query plans",
        ))
    }

    /// DDL generation for schema changes; `None` when object editing is not supported.
    fn ddl_gen(&self) -> Option<&dyn DdlGen> {
        None
    }
}

/// A one-row `MERGE` of `table`: `source` is the row as a derived table whose columns are
/// `columns`; `update` columns are set on a match (none: a match keeps the row).
pub fn merge_sql(
    dialect: &dyn Dialect,
    table: &str,
    source: &str,
    columns: &[String],
    key: &[String],
    update: &[String],
) -> String {
    let q = |c: &String| dialect.quote_ident(c);
    let on: Vec<String> = key
        .iter()
        .map(|k| format!("sqlr_t.{0} = sqlr_s.{0}", q(k)))
        .collect();
    let mut sql = format!(
        "MERGE INTO {table} sqlr_t USING {source} ON ({})",
        on.join(" AND ")
    );
    if !update.is_empty() {
        let sets: Vec<String> = update
            .iter()
            .map(|c| format!("sqlr_t.{0} = sqlr_s.{0}", q(c)))
            .collect();
        sql.push_str(&format!(
            " WHEN MATCHED THEN UPDATE SET {}",
            sets.join(", ")
        ));
    }
    let names: Vec<String> = columns.iter().map(q).collect();
    let values: Vec<String> = columns.iter().map(|c| format!("sqlr_s.{}", q(c))).collect();
    sql.push_str(&format!(
        " WHEN NOT MATCHED THEN INSERT ({}) VALUES ({})",
        names.join(", "),
        values.join(", ")
    ));
    sql
}

/// Loads object definitions from the server's catalogs.
#[async_trait::async_trait]
pub trait Introspect: Send + Sync {
    async fn load_object(&self, object: &ObjectRef) -> Result<ObjectDef, DriverError>;
    async fn list_roles(&self) -> Result<Vec<String>, DriverError>;
    /// Every table (with its columns, constraints, indexes and triggers), view, sequence
    /// and routine of `schema` on `database` (the connection's own when `None`), read in a
    /// fixed number of catalog queries whatever the object count: schema compare reads
    /// thousands of objects.
    async fn load_schema(
        &self,
        _database: Option<&str>,
        _schema: &str,
    ) -> Result<crate::compare::SchemaSnapshot, DriverError> {
        Err(DriverError::new(
            ErrorKind::Unsupported,
            "schema compare is not supported by this driver",
        ))
    }
}

/// Writes DDL. Statements carry no trailing semicolon; identifiers are quoted through
/// [`Dialect::quote_ident`], expression fields are written as is.
///
/// Applying a statement list follows one rule, set by [`DdlGen::placement`]: statements
/// run in order; each maximal run of [`Placement::Transaction`] statements runs in one
/// transaction (where the driver declares transactional DDL); a [`Placement::Alone`]
/// statement runs by itself outside any transaction; after a failure the remaining
/// statements are skipped, except [`Placement::Cleanup`] ones, which run outside any
/// transaction whatever happened before them.
pub trait DdlGen: Send + Sync {
    /// The statements that apply `changes`, in order.
    fn ddl(&self, changes: &[SchemaChange]) -> Result<Vec<String>, DriverError>;
    /// The script that creates `def` (statements ending in `;`, separated by blank lines).
    fn create_ddl(&self, def: &ObjectDef) -> Result<String, DriverError>;
    /// The statements that turn table `before` into `after`. Drivers whose `ALTER TABLE`
    /// cannot express a change rebuild the table instead (SQLite).
    fn table_ddl(&self, before: &TableDef, after: &TableDef) -> Result<Vec<String>, DriverError> {
        self.ddl(&diff_table(before, after))
    }
    /// Where `statement`, one this generator wrote, must run.
    fn placement(&self, _statement: &str) -> Placement {
        Placement::Transaction
    }
}

/// Where an engine runs a statement a [`DdlGen`] wrote (see [`DdlGen`] for the rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Placement {
    /// In the apply's transaction.
    Transaction,
    /// By itself, outside any transaction (PostgreSQL `CREATE DATABASE`,
    /// `CREATE INDEX CONCURRENTLY`; SQLite `PRAGMA foreign_keys = OFF`).
    Alone,
    /// Outside any transaction, and run even after a failure: it restores session state
    /// (SQLite `PRAGMA foreign_keys = ON`).
    Cleanup,
}

/// `statements` as one script: each ends in `;`, blank lines between them. A statement
/// that ends in a line comment gets its `;` on a line of its own, outside the comment.
pub fn script(statements: &[String]) -> String {
    let syntax = SqlSyntax::ansi();
    statements
        .iter()
        .map(|s| {
            let s = trim_statement_in(&syntax, s);
            if ends_in_line_comment(&syntax, &s) {
                format!("{s}\n;")
            } else {
                format!("{s};")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Whether the last token of `sql` is a comment that runs to the end of the line.
fn ends_in_line_comment(syntax: &SqlSyntax, sql: &str) -> bool {
    let mut tokens = Vec::new();
    syntax.lex(sql, LexState::Normal, &mut tokens);
    tokens.last().is_some_and(|t| {
        t.kind == TokenKind::Comment && t.end == sql.len() && !sql[t.start..t.end].ends_with("*/")
    })
}

/// `sql` without surrounding whitespace and trailing semicolons, lexed as ANSI SQL (see
/// [`trim_statement_in`]).
pub fn trim_statement(sql: &str) -> Cow<'_, str> {
    trim_statement_in(&SqlSyntax::ansi(), sql)
}

/// `sql` without surrounding whitespace and without the top-level semicolons that follow
/// its last token, including those before a trailing comment (`select 1; -- note` becomes
/// `select 1 -- note`). Semicolons in strings, quoted identifiers and block comments stay;
/// a trailing line comment loses the semicolons at its end (`select 1 -- note;`).
pub fn trim_statement_in<'a>(syntax: &SqlSyntax, sql: &'a str) -> Cow<'a, str> {
    let sql = sql.trim();
    let mut tokens = Vec::new();
    syntax.lex(sql, LexState::Normal, &mut tokens);
    let last = tokens
        .iter()
        .rposition(|t| !matches!(t.kind, TokenKind::Semicolon | TokenKind::Comment));
    let trailing: Vec<_> = tokens[last.map_or(0, |i| i + 1)..]
        .iter()
        .filter(|t| t.kind == TokenKind::Semicolon)
        .collect();
    let mut out = String::with_capacity(sql.len());
    let mut at = 0;
    for t in trailing {
        out.push_str(&sql[at..t.start]);
        at = t.end;
    }
    out.push_str(&sql[at..]);
    // Removing a semicolon can leave whitespace at the end (`select 1;\n ;`).
    let kept = out.trim_end().len();
    out.truncate(kept);
    if ends_in_line_comment(syntax, &out) {
        let kept = out
            .trim_end_matches(|c: char| c == ';' || c.is_whitespace())
            .len();
        out.truncate(kept);
    }
    if out == sql {
        Cow::Borrowed(sql)
    } else {
        Cow::Owned(out)
    }
}

/// `SELECT * FROM (<sql>) sqlr_q [ORDER BY <keys>]`, `sql` lexed with `syntax`. The
/// newlines keep a trailing line comment in `sql` from swallowing the closing parenthesis.
pub fn wrap_subquery(syntax: &SqlSyntax, sql: &str, order_keys: &[String]) -> String {
    let mut out = format!(
        "SELECT * FROM (\n{}\n) sqlr_q",
        trim_statement_in(syntax, sql)
    );
    if !order_keys.is_empty() {
        out.push_str(" ORDER BY ");
        out.push_str(&order_keys.join(", "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ansi;

    impl Dialect for Ansi {
        fn quote_ident(&self, ident: &str) -> String {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
        fn select_all(&self, _node: &Node, _limit: Option<usize>) -> Option<String> {
            None
        }
    }

    #[test]
    fn default_wrappers_quote_identifiers() {
        let order = [("My Col".to_string(), true), ("b".to_string(), false)];
        assert_eq!(
            Ansi.wrap_order("select 1 -- note;\n ;", &order),
            "SELECT * FROM (\nselect 1 -- note\n) sqlr_q ORDER BY \"My Col\" DESC NULLS LAST, \"b\" NULLS LAST"
        );
        assert_eq!(
            Ansi.wrap_where("select * from t;", " a > 1 "),
            "SELECT * FROM (\nselect * from t\n) sqlr_q WHERE a > 1"
        );
        let t = TableRef {
            database: None,
            schema: Some("s".into()),
            name: "a\"b".into(),
        };
        assert_eq!(Ansi.qualified(&t), "\"s\".\"a\"\"b\"");
        assert_eq!(Ansi.savepoint_sql("sp"), "SAVEPOINT \"sp\"");
        assert_eq!(Ansi.rollback_to_sql("sp"), "ROLLBACK TO SAVEPOINT \"sp\"");
        assert_eq!(Ansi.string_literal("it's"), "'it''s'");
        assert_eq!(Ansi.bytes_literal(&[0, 255]), "X'00ff'");
    }

    #[test]
    fn script_terminates_a_statement_that_ends_in_a_line_comment() {
        let sql = script(&["SELECT 1 -- one".to_string(), "SELECT 2".to_string()]);
        let ranges = crate::syntax::SqlSyntax::ansi().split(&sql, crate::syntax::LexState::Normal);
        assert_eq!(ranges.len(), 2, "{sql}");
    }

    #[test]
    fn wrapping_drops_a_semicolon_before_a_trailing_comment() {
        assert_eq!(trim_statement("select 1; -- note"), "select 1 -- note");
        let order = [("a".to_string(), true)];
        for sql in [
            Ansi.wrap_order("select 1; -- note", &order),
            Ansi.wrap_where("select 1; -- note", "a > 0"),
        ] {
            assert!(!sql.contains(';'), "{sql}");
        }
    }

    #[test]
    fn trimming_keeps_semicolons_inside_literals_and_block_comments() {
        assert_eq!(trim_statement("select ';' ; "), "select ';'");
        assert_eq!(trim_statement("select \"a;\";;"), "select \"a;\"");
        assert_eq!(
            trim_statement("select 1 /* a; */ ; /* b */"),
            "select 1 /* a; */  /* b */"
        );
        assert_eq!(trim_statement("select 1"), "select 1");
        assert!(matches!(
            trim_statement(" select 1 "),
            Cow::Borrowed("select 1")
        ));
    }

    #[test]
    fn script_ends_every_statement_outside_its_comments() {
        let statements = [
            "SELECT 1 /* one */".to_string(),
            "SELECT 2 -- two;".to_string(),
            "SELECT 3;".to_string(),
        ];
        assert_eq!(
            script(&statements),
            "SELECT 1 /* one */;\n\nSELECT 2 -- two\n;\n\nSELECT 3;"
        );
    }
}
