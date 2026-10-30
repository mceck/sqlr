//! Statement classification for read-only mode, confirmations and smart commit (§7.9).
//!
//! The classifier works on lexer tokens, so keywords inside strings and comments never
//! count, and a `WHERE` only counts at the statement's own parenthesis depth.

use crate::syntax::{LexState, SqlSyntax, TokenKind};

/// A transaction-control verb of a statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxVerb {
    /// Start a transaction.
    Begin,
    /// Commit the transaction.
    Commit,
    /// Roll the transaction back.
    Rollback,
    /// Set a savepoint with this name.
    Savepoint(String),
    /// Roll back to the savepoint with this name.
    RollbackTo(String),
    /// Release the savepoint with this name.
    Release(String),
}

/// The kind of a statement, as decided by the classifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatementClass {
    /// SELECT, SHOW, EXPLAIN without ANALYZE, VALUES, TABLE, PRAGMA reads, WITH … SELECT.
    Read,
    /// INSERT, UPDATE/DELETE with WHERE, MERGE, REPLACE, COPY FROM, CALL, writable CTEs.
    Write,
    /// CREATE, ALTER, COMMENT, GRANT, REVOKE, RENAME.
    Ddl,
    /// DROP, TRUNCATE, DELETE/UPDATE without WHERE, ALTER … DROP; the reason names which.
    Destructive(String),
    /// A transaction-control statement.
    Transaction(TxVerb),
    /// SET, USE and anything unknown; treated like a write.
    Other,
}

impl StatementClass {
    /// Whether the statement may change data or schema (`Other` counts, to be safe).
    pub fn modifies(&self) -> bool {
        matches!(
            self,
            StatementClass::Write
                | StatementClass::Ddl
                | StatementClass::Destructive(_)
                | StatementClass::Other
        )
    }
}

/// A significant token with its parenthesis depth.
struct Tok<'a> {
    text: &'a str,
    kind: TokenKind,
    depth: u32,
}

impl Tok<'_> {
    fn is_word(&self) -> bool {
        matches!(
            self.kind,
            TokenKind::Keyword | TokenKind::Ident | TokenKind::Function
        )
    }

    fn is(&self, word: &str) -> bool {
        self.is_word() && self.text.eq_ignore_ascii_case(word)
    }

    fn is_any(&self, words: &[&str]) -> bool {
        words.iter().any(|w| self.is(w))
    }

    fn punct(&self, p: &str) -> bool {
        self.kind == TokenKind::Punct && self.text == p
    }
}

/// Words that start the statement an EXPLAIN or a WITH wraps.
const STATEMENT_VERBS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "VALUES", "TABLE", "REPLACE", "CREATE",
    "DECLARE", "EXECUTE",
];
const DML_VERBS: &[&str] = &["INSERT", "UPDATE", "DELETE", "MERGE"];
/// What `EXPLAIN` can wrap: the statement verbs and a `WITH` query.
const EXPLAIN_VERBS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "VALUES", "TABLE", "REPLACE", "CREATE",
    "DECLARE", "EXECUTE", "WITH",
];
/// SQLite pragmas that take arguments and only read (table-valued pragmas).
const READ_PRAGMAS_WITH_ARGS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "foreign_key_check",
    "integrity_check",
    "quick_check",
];
/// SQLite pragmas that act even without arguments.
const ACTION_PRAGMAS: &[&str] = &[
    "optimize",
    "shrink_memory",
    "incremental_vacuum",
    "wal_checkpoint",
];

impl SqlSyntax {
    /// Classifies the first statement of `sql`.
    pub fn classify(&self, sql: &str) -> StatementClass {
        let mut toks = Vec::new();
        let mut depth = 0u32;
        self.significant_tokens(sql, &mut toks, &mut depth);
        self.classify_tokens(&toks)
    }

    /// Classifies `sql` as a whole script rather than by its first statement.
    ///
    /// The UI usually splits a script before running it, but some paths (transfer sources,
    /// export re-runs) receive a raw string, and some dialects allow several statements in
    /// one batch without a `;` separator (T-SQL). This method classifies every
    /// semicolon-separated statement and then looks for a top-level write keyword that
    /// would start another statement. It is deliberately conservative: when in doubt it
    /// returns a modifying class, so read-only mode and confirmations do not miss a
    /// statement hidden after a harmless first one.
    pub fn classify_script(&self, sql: &str) -> StatementClass {
        let ranges = self.split(sql, LexState::Normal);
        let mut first_word = None;
        for range in &ranges {
            let text = &sql[range.clone()];
            let class = self.classify(text);
            if first_word.is_none() {
                first_word = self.first_word(text);
            }
            if class.modifies() {
                return class;
            }
        }
        // `EXPLAIN DELETE …` only plans the statement (without ANALYZE); its embedded
        // DELETE must not turn a plan into a write. `EXPLAIN ANALYZE` is already a write
        // above when its wrapped statement modifies. A write keyword after the planned
        // statement is a second statement in a semicolon-less batch, though.
        if first_word.is_some_and(|word| word.eq_ignore_ascii_case("EXPLAIN")) {
            if let Some(body) = self.explain_body(sql) {
                let planned = self.first_word(body).unwrap_or("");
                if matches!(
                    planned.to_ascii_uppercase().as_str(),
                    "SELECT" | "SHOW" | "VALUES" | "TABLE" | "DESCRIBE" | "DESC" | "WITH"
                ) {
                    return self.batch_write(body).unwrap_or(StatementClass::Read);
                }
            }
            return StatementClass::Read;
        }
        self.batch_write(sql).unwrap_or(StatementClass::Read)
    }

    /// The text of the statement an `EXPLAIN` wraps, from its first verb; `None` when no
    /// verb follows.
    fn explain_body<'a>(&self, sql: &'a str) -> Option<&'a str> {
        let mut tokens = Vec::new();
        self.lex(sql, LexState::Normal, &mut tokens);
        let mut seen_explain = false;
        let mut depth = 0u32;
        for t in &tokens {
            if t.kind == TokenKind::Comment {
                continue;
            }
            let text = &sql[t.start..t.end];
            if !seen_explain {
                seen_explain =
                    t.kind != TokenKind::Semicolon && text.eq_ignore_ascii_case("EXPLAIN");
                continue;
            }
            match text {
                "(" => {
                    depth += 1;
                    continue;
                }
                ")" => {
                    depth = depth.saturating_sub(1);
                    continue;
                }
                _ => {}
            }
            if depth == 0
                && matches!(
                    t.kind,
                    TokenKind::Keyword | TokenKind::Ident | TokenKind::Function
                )
                && EXPLAIN_VERBS.iter().any(|v| text.eq_ignore_ascii_case(v))
            {
                return Some(&sql[t.start..]);
            }
        }
        None
    }

    /// The first word token in `text`, skipping comments.
    fn first_word<'a>(&self, text: &'a str) -> Option<&'a str> {
        let mut tokens = Vec::new();
        self.lex(text, LexState::Normal, &mut tokens);
        tokens
            .iter()
            .find(|t| {
                matches!(
                    t.kind,
                    TokenKind::Keyword | TokenKind::Ident | TokenKind::Function
                )
            })
            .map(|t| &text[t.start..t.end])
    }

    /// A write keyword at parenthesis depth zero in a batch that has no semicolon before
    /// it. T-SQL allows `SELECT 1\nDELETE FROM t` and PL/SQL blocks such as
    /// `BEGIN DELETE FROM t; END;`; both must count as writes even though their first
    /// word is harmless. Returns the class to report, or `None` for a read-only batch.
    fn batch_write(&self, sql: &str) -> Option<StatementClass> {
        use StatementClass::*;
        let mut tokens = Vec::new();
        self.lex(sql, LexState::Normal, &mut tokens);
        let mut depth = 0u32;
        let mut seen_word = false;
        let mut last_words: Vec<String> = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            let text = &sql[t.start..t.end];
            if t.kind == TokenKind::Comment {
                continue;
            }
            if t.kind == TokenKind::Semicolon {
                // A new statement starts here; `FOR UPDATE` in the previous SELECT must
                // not suppress an UPDATE in this one, and a later `WITH` is a new
                // statement whose own classification already checked it.
                last_words.clear();
                seen_word = false;
                continue;
            }
            let open = t.kind == TokenKind::Punct && text == "(";
            let close = t.kind == TokenKind::Punct && text == ")";
            if close {
                depth = depth.saturating_sub(1);
            }
            if depth == 0
                && matches!(
                    t.kind,
                    TokenKind::Keyword | TokenKind::Ident | TokenKind::Function
                )
            {
                let upper = text.to_ascii_uppercase();
                // `WITH` needs a statement boundary; when it follows another word it
                // starts a second statement whose CTE may modify data. `WITH (NOLOCK)` is
                // a T-SQL table hint, not a CTE: its next token is `(`.
                let next_is_paren = tokens[i + 1..]
                    .iter()
                    .find(|t| t.kind != TokenKind::Comment)
                    .is_some_and(|t| t.kind == TokenKind::Punct && &sql[t.start..t.end] == "(");
                if seen_word && upper == "WITH" && !next_is_paren {
                    return Some(Destructive(
                        "WITH starts another statement in a batch".into(),
                    ));
                }
                // `FOR UPDATE` / `FOR NO KEY UPDATE` locks rows but does not change data.
                if upper == "UPDATE"
                    && last_words
                        .iter()
                        .rposition(|w| w == "FOR")
                        .is_some_and(|at| {
                            last_words[at + 1..]
                                .iter()
                                .all(|w| matches!(w.as_str(), "NO" | "KEY" | "OF"))
                        })
                {
                    // no write; fall through to remember this word
                } else {
                    match upper.as_str() {
                        "DROP" | "TRUNCATE" | "DELETE" | "KILL" | "SHUTDOWN" | "REVOKE"
                        | "DENY" | "WRITETEXT" | "UPDATETEXT" | "INSERT" | "UPDATE" | "MERGE"
                        | "REPLACE" | "UPSERT" | "CALL" | "EXEC" | "EXECUTE" | "LOAD" | "SET"
                        | "USE" | "PRAGMA" | "VACUUM" | "REINDEX" | "ANALYZE" | "CHECKPOINT"
                        | "RECONFIGURE" | "BACKUP" | "RESTORE" | "DBCC" | "BULK" | "ATTACH"
                        | "DETACH" | "CREATE" | "ALTER" | "COMMENT" | "GRANT" | "RENAME" => {
                            // A batch hides the statement from the per-statement
                            // confirmation; make it destructive so every profile asks.
                            return Some(Destructive(format!("{upper} in a batch of statements")));
                        }
                        _ => {}
                    }
                }
                seen_word = true;
                last_words.push(upper);
                if last_words.len() > 4 {
                    last_words.remove(0);
                }
            }
            if open {
                depth += 1;
            }
        }
        None
    }

    /// Whether the first statement of `sql` changes the schema or privileges (CREATE, ALTER,
    /// DROP, TRUNCATE, COMMENT, GRANT, REVOKE, RENAME): the statements MySQL commits an open
    /// transaction before. Decided on its first word, like [`SqlSyntax::classify`].
    pub fn is_schema_change(&self, sql: &str) -> bool {
        const WORDS: &[&str] = &[
            "CREATE", "ALTER", "DROP", "TRUNCATE", "COMMENT", "GRANT", "REVOKE", "RENAME",
        ];
        let mut toks = Vec::new();
        let mut depth = 0u32;
        self.significant_tokens(sql, &mut toks, &mut depth);
        toks.iter()
            .find(|t| t.is_word())
            .is_some_and(|t| t.is_any(WORDS))
    }

    /// Appends the tokens of the first statement in `sql`, without comments. In MySQL
    /// (`hash_comments`) an executable comment `/*! … */` runs as code, so its content
    /// counts. Returns `false` once a `;` ends the statement.
    fn significant_tokens<'a>(
        &self,
        sql: &'a str,
        toks: &mut Vec<Tok<'a>>,
        depth: &mut u32,
    ) -> bool {
        let mut raw = Vec::new();
        self.lex(sql, LexState::Normal, &mut raw);
        for t in raw {
            let text = &sql[t.start..t.end];
            match t.kind {
                TokenKind::Comment => {
                    if let Some(body) = self
                        .hash_comments
                        .then(|| text.strip_prefix("/*!"))
                        .flatten()
                        .and_then(|b| b.strip_suffix("*/"))
                    {
                        // `/*!40001 …` runs only on servers at least that version: skip it.
                        let code = body.trim_start_matches(|c: char| c.is_ascii_digit());
                        if !self.significant_tokens(code, toks, depth) {
                            return false;
                        }
                    }
                    continue;
                }
                TokenKind::Semicolon => return false,
                _ => {}
            }
            let open = t.kind == TokenKind::Punct && text == "(";
            let close = t.kind == TokenKind::Punct && text == ")";
            if close {
                *depth = depth.saturating_sub(1);
            }
            toks.push(Tok {
                text,
                kind: t.kind,
                depth: *depth,
            });
            if open {
                *depth += 1;
            }
        }
        true
    }

    fn classify_tokens(&self, toks: &[Tok<'_>]) -> StatementClass {
        use StatementClass::*;
        let Some(start) = toks.iter().position(Tok::is_word) else {
            return Other;
        };
        let toks = &toks[start..];
        let first = &toks[0];
        let depth = first.depth;
        let at_depth = |word: &str| toks.iter().any(|t| t.depth == depth && t.is(word));
        let next_word = toks.get(1).filter(|t| t.is_word());
        let upper = first.text.to_ascii_uppercase();
        // Ending other sessions' work, whatever the statement around it.
        for function in ["pg_terminate_backend", "pg_cancel_backend"] {
            if toks.iter().any(|t| t.is(function)) {
                return Destructive(function.to_string());
            }
        }
        match upper.as_str() {
            "SELECT" if at_depth("INTO") => Write,
            "SELECT" | "SHOW" | "VALUES" | "TABLE" | "DESCRIBE" | "DESC" => Read,
            "EXPLAIN" => self.classify_explain(toks),
            "PRAGMA" => classify_pragma(toks),
            "WITH" => self.classify_with(toks),
            "INSERT" | "REPLACE" | "MERGE" | "CALL" | "UPSERT" | "LOAD" => Write,
            // `COPY … TO PROGRAM` runs a shell command on the server.
            "COPY" if at_depth("PROGRAM") => Destructive("COPY TO PROGRAM".into()),
            "COPY" if at_depth("FROM") => Write,
            "COPY" => Read,
            "UPDATE" | "DELETE" if at_depth("WHERE") => Write,
            "UPDATE" | "DELETE" => Destructive(format!("{upper} without WHERE")),
            "DROP" => Destructive(match next_word {
                Some(w) => format!("DROP {}", w.text.to_ascii_uppercase()),
                None => "DROP".to_string(),
            }),
            "TRUNCATE" => Destructive("TRUNCATE".into()),
            "ALTER" if at_depth("DROP") => Destructive("ALTER … DROP".into()),
            "REVOKE" => Destructive("REVOKE".into()),
            "KILL" => Destructive("KILL".into()),
            "ALTER" | "CREATE" | "COMMENT" | "GRANT" | "RENAME" => Ddl,
            "BEGIN" => Transaction(TxVerb::Begin),
            "START" if next_word.is_some_and(|w| w.is("TRANSACTION")) => Transaction(TxVerb::Begin),
            "COMMIT" | "END" => Transaction(TxVerb::Commit),
            "ROLLBACK" | "ABORT" => match toks.iter().position(|t| t.is("TO")) {
                Some(to) => match self.name_after(&toks[to + 1..]) {
                    Some(name) => Transaction(TxVerb::RollbackTo(name)),
                    None => Other,
                },
                None => Transaction(TxVerb::Rollback),
            },
            "SAVEPOINT" => self
                .name_after(&toks[1..])
                .map_or(Other, |n| Transaction(TxVerb::Savepoint(n))),
            "RELEASE" => self
                .name_after(&toks[1..])
                .map_or(Other, |n| Transaction(TxVerb::Release(n))),
            _ => Other,
        }
    }

    /// The savepoint name in `toks`, skipping an optional `SAVEPOINT`.
    fn name_after(&self, toks: &[Tok<'_>]) -> Option<String> {
        let toks = match toks.first() {
            Some(t) if t.is("SAVEPOINT") => &toks[1..],
            _ => toks,
        };
        let name = toks.first()?;
        (name.is_word() || name.kind == TokenKind::QuotedIdent).then(|| self.unquote(name.text).0)
    }

    /// `EXPLAIN` reads unless it runs the statement (`ANALYZE`) and that statement writes.
    fn classify_explain(&self, toks: &[Tok<'_>]) -> StatementClass {
        let depth = toks[0].depth;
        let Some(inner) = toks
            .iter()
            .skip(1)
            .position(|t| t.depth == depth && t.is_any(EXPLAIN_VERBS))
            .map(|i| i + 1)
        else {
            return StatementClass::Read;
        };
        let analyze = toks[1..inner]
            .iter()
            .any(|t| t.is_any(&["ANALYZE", "ANALYSE"]));
        if !analyze {
            return StatementClass::Read;
        }
        match self.classify_tokens(&toks[inner..]) {
            StatementClass::Read => StatementClass::Read,
            _ => StatementClass::Write,
        }
    }

    /// `WITH`: the main statement's class, made a write by a data-modifying CTE.
    fn classify_with(&self, toks: &[Tok<'_>]) -> StatementClass {
        let depth = toks[0].depth;
        let Some(main) = toks
            .iter()
            .skip(1)
            .position(|t| t.depth == depth && t.is_any(STATEMENT_VERBS))
            .map(|i| i + 1)
        else {
            return StatementClass::Other;
        };
        let writable_cte = toks[..main]
            .windows(2)
            .any(|w| w[0].punct("(") && w[1].is_any(DML_VERBS));
        match self.classify_tokens(&toks[main..]) {
            StatementClass::Read if writable_cte => StatementClass::Write,
            class => class,
        }
    }
}

/// SQLite `PRAGMA [schema.]name [= value | (value)]`: a read without arguments (unless it
/// acts, like `optimize`) or for a table-valued read pragma; a setter otherwise.
fn classify_pragma(toks: &[Tok<'_>]) -> StatementClass {
    if toks.iter().any(|t| t.text == "=") {
        return StatementClass::Other;
    }
    let name = match (toks.get(1), toks.get(2)) {
        (_, Some(dot)) if dot.punct(".") => toks.get(3),
        (name, _) => name,
    };
    let Some(name) = name else {
        return StatementClass::Other;
    };
    let has_args = toks.iter().any(|t| t.punct("("));
    let read = if has_args {
        name.is_any(READ_PRAGMAS_WITH_ARGS)
    } else {
        !name.is_any(ACTION_PRAGMAS)
    };
    if read {
        StatementClass::Read
    } else {
        StatementClass::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::SqlSyntax;

    fn destructive(reason: &str) -> StatementClass {
        StatementClass::Destructive(reason.into())
    }

    #[test]
    fn administration_that_takes_away_is_destructive() {
        let s = SqlSyntax::ansi();
        let cases = [
            ("REVOKE SELECT ON t FROM u", "REVOKE"),
            ("revoke reader from app", "REVOKE"),
            ("KILL 42", "KILL"),
            ("kill query 42", "KILL"),
            ("SELECT pg_terminate_backend(42)", "pg_terminate_backend"),
            (
                "select pg_cancel_backend(pid) from pg_stat_activity",
                "pg_cancel_backend",
            ),
            ("DROP ROLE app", "DROP ROLE"),
            ("drop user 'a'@'%'", "DROP USER"),
        ];
        for (sql, reason) in cases {
            assert_eq!(s.classify(sql), destructive(reason), "{sql}");
        }
        assert_eq!(s.classify("GRANT SELECT ON t TO u"), StatementClass::Ddl);
        assert_eq!(
            s.classify("select 'pg_terminate_backend' from t"),
            StatementClass::Read
        );
    }

    #[test]
    fn classify_statements() {
        use StatementClass::*;
        let cases: Vec<(&str, StatementClass)> = vec![
            ("select 1", Read),
            ("  SELECT * FROM t WHERE a = 'drop table x'", Read),
            ("(select 1) union (select 2)", Read),
            ("show tables", Read),
            ("explain select * from t", Read),
            ("values (1), (2)", Read),
            ("table t", Read),
            ("describe t", Read),
            ("pragma table_info(t)", Read),
            ("with x as (select 1) select * from x", Read),
            ("select 'drop table' ", Read),
            ("-- delete from t\nselect 1", Read),
            ("/* drop table t */ select 1", Read),
            (
                "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x",
                Write,
            ),
            ("with x as (select 1) insert into t select * from x", Write),
            ("insert into t values (1)", Write),
            ("replace into t values (1)", Write),
            ("update t set a = 1 where id = 1", Write),
            ("delete from t where id=1", Write),
            (
                "merge into t using s on t.id = s.id when matched then delete",
                Write,
            ),
            ("call p(1)", Write),
            ("copy t from stdin", Write),
            (
                "copy t to program 'touch /tmp/pwned'",
                destructive("COPY TO PROGRAM"),
            ),
            (
                "copy (select 1) to program 'id'",
                destructive("COPY TO PROGRAM"),
            ),
            ("explain analyze delete from t", Write),
            ("explain (analyze, buffers) update t set a = 1", Write),
            ("select * into t2 from t", Write),
            ("delete from t", destructive("DELETE without WHERE")),
            (
                "delete from t where id in (select id from u where x)  -- ok",
                Write,
            ),
            (
                "delete from t using u returning (select 1 where true)",
                destructive("DELETE without WHERE"),
            ),
            (
                "update t set a=1 -- where id=1",
                destructive("UPDATE without WHERE"),
            ),
            ("DROP TABLE t", destructive("DROP TABLE")),
            ("drop index i", destructive("DROP INDEX")),
            ("truncate t", destructive("TRUNCATE")),
            ("truncate table t", destructive("TRUNCATE")),
            ("alter table t drop column c", destructive("ALTER … DROP")),
            ("alter table t add column c int", Ddl),
            ("create index i on t (a)", Ddl),
            ("CREATE TABLE t (id int)", Ddl),
            ("comment on table t is 'drop'", Ddl),
            ("grant select on t to bob", Ddl),
            ("revoke select on t from bob", destructive("REVOKE")),
            ("rename table a to b", Ddl),
            ("begin", Transaction(TxVerb::Begin)),
            ("BEGIN TRANSACTION", Transaction(TxVerb::Begin)),
            ("start transaction", Transaction(TxVerb::Begin)),
            ("start transaction read only", Transaction(TxVerb::Begin)),
            ("commit", Transaction(TxVerb::Commit)),
            ("end", Transaction(TxVerb::Commit)),
            ("rollback", Transaction(TxVerb::Rollback)),
            ("abort", Transaction(TxVerb::Rollback)),
            ("savepoint s1", Transaction(TxVerb::Savepoint("s1".into()))),
            (
                "rollback to savepoint s1",
                Transaction(TxVerb::RollbackTo("s1".into())),
            ),
            (
                "rollback to s1",
                Transaction(TxVerb::RollbackTo("s1".into())),
            ),
            (
                "release savepoint \"S 1\"",
                Transaction(TxVerb::Release("S 1".into())),
            ),
            (
                "EXPLAIN ANALYZE WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x",
                Write,
            ),
            ("explain analyze with x as (select 1) select * from x", Read),
            (
                "explain with x as (delete from t returning *) select * from x",
                Read,
            ),
            ("pragma foreign_keys", Read),
            ("pragma main.user_version", Read),
            ("pragma table_xinfo('t')", Read),
            ("pragma main.index_list(t)", Read),
            ("pragma query_only(0)", Other),
            ("pragma user_version(5)", Other),
            ("pragma optimize", Other),
            ("pragma wal_checkpoint", Other),
            ("set search_path = x", Other),
            ("use shop", Other),
            ("pragma foreign_keys = on", Other),
            ("vacuum", Other),
            ("", Other),
            ("-- only a comment", Other),
        ];
        let syntax = SqlSyntax::ansi();
        for (sql, expected) in cases {
            assert_eq!(syntax.classify(sql), expected, "{sql}");
        }
    }

    #[test]
    fn mysql_executable_comments_count_as_code() {
        let mysql = SqlSyntax {
            hash_comments: true,
            line_comment_needs_space: true,
            ident_quotes: &[(b'`', b'`')],
            double_quote_strings: true,
            backslash_escapes: true,
            ..(*SqlSyntax::ansi()).clone()
        };
        assert_eq!(
            mysql.classify("SELECT 1 /*! INTO OUTFILE '/tmp/x' */"),
            StatementClass::Write
        );
        assert_eq!(
            mysql.classify("/*!40000 DROP TABLE t */"),
            StatementClass::Destructive("DROP TABLE".into())
        );
        assert_eq!(
            mysql.classify("/* DROP TABLE t */ SELECT 1"),
            StatementClass::Read
        );
        assert_eq!(
            mysql.classify("SELECT /*!40001 SQL_NO_CACHE */ 1"),
            StatementClass::Read
        );
    }

    #[test]
    fn schema_changes_by_their_first_word() {
        let syntax = SqlSyntax::ansi();
        for sql in [
            "create table t (a int)",
            "  -- note\nDROP TABLE t",
            "alter table t drop column c",
            "truncate t",
            "grant select on t to bob",
            "revoke select on t from bob",
            "rename table a to b",
            "comment on table t is 'x'",
        ] {
            assert!(syntax.is_schema_change(sql), "{sql}");
        }
        for sql in [
            "select 'drop table t'",
            "/* create */ select 1",
            "delete from t",
            "update t set a = 1",
            "insert into t values ('alter')",
            "",
        ] {
            assert!(!syntax.is_schema_change(sql), "{sql}");
        }
    }

    #[test]
    fn modifies_everything_but_reads_and_transactions() {
        use StatementClass::*;
        assert!(!Read.modifies());
        assert!(!Transaction(TxVerb::Commit).modifies());
        for c in [Write, Ddl, destructive("x"), Other] {
            assert!(c.modifies(), "{c:?}");
        }
    }

    #[test]
    fn a_script_is_a_write_when_any_statement_is() {
        use StatementClass::*;
        let syntax = SqlSyntax::ansi();
        for sql in [
            "SELECT 1; DELETE FROM t",
            "SELECT 1\nDELETE FROM t",
            "BEGIN DELETE FROM t END",
            "SELECT 1\nINSERT INTO t VALUES (1)",
            "SELECT 1\nEXEC dangerous_procedure",
            "EXPLAIN SELECT 1\nDELETE FROM t",
            "BEGIN UPDATE t SET a = 1; END;",
            "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x",
        ] {
            let class = syntax.classify_script(sql);
            assert!(class.modifies(), "{sql} was {class:?}");
        }
        for sql in [
            "SELECT 1",
            "SELECT 1; SELECT 2",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM t FOR NO KEY UPDATE",
            "EXPLAIN DELETE FROM t",
            "EXPLAIN WITH x AS (SELECT 1) SELECT * FROM x",
            "SELECT 'DELETE FROM t' FROM t",
            "SELECT \"delete\" FROM t",
            "SELECT 1 -- DELETE FROM t",
            "SELECT 1 /* DELETE FROM t */",
            "SELECT * FROM t WITH (NOLOCK)",
            "SELECT 1; WITH x AS (SELECT 1) SELECT * FROM x",
            "SELECT 1\nBEGIN SELECT 2 END",
            "COPY t TO STDOUT",
        ] {
            assert_eq!(syntax.classify_script(sql), Read, "{sql}");
        }
    }
}
