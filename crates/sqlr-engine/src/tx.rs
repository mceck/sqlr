//! Transaction state of a session (§7.9): commit modes over plain SQL from the dialect, kept
//! in sync with the statements the user types, and the run messages that report it.

use std::sync::Arc;

use sqlr_core::{
    CommitMode, Connection, Dialect, DriverError, ErrorKind, Execution, SqlSyntax, StatementClass,
    TxVerb,
};

use crate::engine::{Shared, apply_read_only};
use crate::library::now_ms;
use crate::session::{SessionIo, SessionKey};

#[derive(Clone, Debug, PartialEq)]
pub enum TxPhase {
    Idle,
    Open,
    /// A statement failed inside the transaction (PostgreSQL): only a rollback is possible.
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TxState {
    pub mode: CommitMode,
    pub read_only: bool,
    pub phase: TxPhase,
    /// Successful BEGIN levels still waiting for COMMIT (zero when idle).
    pub depth: u32,
    /// Statements run in the open transaction.
    pub statements: u32,
    /// When the open transaction started, in Unix milliseconds.
    pub since_ms: Option<i64>,
    /// Savepoints of the open transaction, oldest first.
    pub savepoints: Vec<String>,
    /// When a statement or transaction command last ran, in Unix milliseconds.
    pub last_activity_ms: i64,
    /// The open transaction has been idle longer than the profile's `idle_tx_timeout_min`.
    pub idle_timeout: bool,
    /// A statement that may modify (or a changeset) ran in the open transaction.
    pub wrote: bool,
    /// A top-level `SAVEPOINT` started the transaction (SQLite): releasing the last
    /// savepoint commits it.
    pub started_by_savepoint: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageLevel {
    Info,
    Notice,
    Warning,
    Error,
}

/// One line of a run's Messages log.
#[derive(Clone, Debug, PartialEq)]
pub struct RunMessage {
    pub at_ms: i64,
    pub level: MessageLevel,
    pub text: String,
}

impl RunMessage {
    pub fn new(level: MessageLevel, text: impl Into<String>) -> Self {
        Self {
            at_ms: crate::library::now_ms(),
            level,
            text: text.into(),
        }
    }
}

pub(crate) const READ_ONLY: &str = "The session is read-only";
pub(crate) const MYSQL_DDL_NOTE: &str = "MySQL committed the open transaction before DDL";

/// What the dialect says about transactions, for the state machine.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TxRules {
    /// The server commits an open transaction before DDL (MySQL).
    pub ddl_commits: bool,
    /// A failed statement aborts the open transaction (PostgreSQL).
    pub error_aborts: bool,
    /// BEGIN inside an open transaction adds a level that COMMIT must remove.
    pub nested_begin: bool,
    /// `SAVEPOINT` with an existing name replaces the old one (MySQL, MariaDB, Oracle).
    pub savepoints_replace: bool,
    /// A top-level `SAVEPOINT` starts a transaction by itself (SQLite).
    pub savepoint_opens: bool,
}

impl TxRules {
    pub(crate) fn from_dialect(dialect: &dyn Dialect) -> Self {
        Self {
            ddl_commits: dialect.ddl_commits_transaction(),
            error_aborts: dialect.error_aborts_transaction(),
            nested_begin: dialect.nested_begin_increments_depth(),
            savepoints_replace: dialect.savepoint_names_replace(),
            savepoint_opens: dialect.savepoint_starts_transaction(),
        }
    }
}

pub(crate) const FAILED_COMMIT_NOTE: &str = "The failed transaction was rolled back";

/// A statement as the transaction state machine sees it.
pub(crate) struct Stmt {
    pub class: StatementClass,
    /// COMMIT/ROLLBACK AND CHAIN immediately starts the next transaction.
    chain: bool,
    /// It changes the schema or privileges ([`SqlSyntax::is_schema_change`]): the statements
    /// a server with `ddl_commits` commits an open transaction before.
    pub schema_change: bool,
}

impl Stmt {
    pub(crate) fn of(syntax: &SqlSyntax, sql: &str) -> Self {
        let class = syntax.classify(sql);
        let mut tokens = Vec::new();
        syntax.lex(sql, sqlr_core::LexState::Normal, &mut tokens);
        let significant: Vec<_> = tokens
            .iter()
            .take_while(|t| t.kind != sqlr_core::TokenKind::Semicolon)
            .filter(|t| t.kind != sqlr_core::TokenKind::Comment)
            .collect();
        let chain = matches!(
            class,
            StatementClass::Transaction(TxVerb::Commit | TxVerb::Rollback)
        ) && significant.windows(2).any(|pair| {
            pair.iter().zip(["AND", "CHAIN"]).all(|(token, word)| {
                matches!(
                    token.kind,
                    sqlr_core::TokenKind::Keyword | sqlr_core::TokenKind::Ident
                ) && sql[token.start..token.end].eq_ignore_ascii_case(word)
            })
        });
        Self {
            class,
            chain,
            schema_change: syntax.is_schema_change(sql),
        }
    }
}

impl TxState {
    pub(crate) fn new(mode: CommitMode, read_only: bool, now_ms: i64) -> Self {
        Self {
            mode,
            read_only,
            phase: TxPhase::Idle,
            depth: 0,
            statements: 0,
            since_ms: None,
            savepoints: Vec::new(),
            last_activity_ms: now_ms,
            idle_timeout: false,
            wrote: false,
            started_by_savepoint: false,
        }
    }

    /// Whether a transaction is open (or failed and waiting for a rollback).
    pub fn is_open(&self) -> bool {
        self.phase != TxPhase::Idle
    }

    /// Whether the open transaction keeps a DDL apply on another session from running: it
    /// wrote or holds savepoints. One that only read (an editor's page in Manual mode) does
    /// not.
    pub fn blocks_ddl(&self) -> bool {
        self.is_open() && (self.wrote || !self.savepoints.is_empty())
    }

    /// Whether the engine must run `begin_sql()` before `stmt`.
    ///
    /// `BEGIN`, `COMMIT` and `ROLLBACK` never open a transaction. A typed `SAVEPOINT` does,
    /// in Manual and in Smart mode: a savepoint only means something inside a transaction,
    /// and the engine must track the one it lives in. `ROLLBACK TO` and `RELEASE` do not:
    /// with no transaction open they name no savepoint, and opening one only to fail would
    /// leave a failed transaction behind, so the server reports the error instead.
    pub(crate) fn needs_begin(&self, stmt: &Stmt, rules: TxRules) -> bool {
        if self.is_open() || (rules.ddl_commits && stmt.schema_change) {
            return false;
        }
        let class = &stmt.class;
        match self.mode {
            CommitMode::Auto => false,
            CommitMode::Manual => !matches!(
                class,
                StatementClass::Transaction(
                    TxVerb::Begin
                        | TxVerb::Commit
                        | TxVerb::Rollback
                        | TxVerb::RollbackTo(_)
                        | TxVerb::Release(_)
                )
            ),
            CommitMode::Smart => matches!(
                class,
                StatementClass::Write
                    | StatementClass::Ddl
                    | StatementClass::Destructive(_)
                    | StatementClass::Transaction(TxVerb::Savepoint(_))
            ),
        }
    }

    pub(crate) fn open(&mut self, now_ms: i64) {
        self.phase = TxPhase::Open;
        self.depth = 1;
        self.statements = 0;
        self.since_ms = Some(now_ms);
        self.savepoints.clear();
        self.last_activity_ms = now_ms;
        self.wrote = false;
        self.started_by_savepoint = false;
    }

    pub(crate) fn close(&mut self, now_ms: i64) {
        self.phase = TxPhase::Idle;
        self.depth = 0;
        self.statements = 0;
        self.since_ms = None;
        self.savepoints.clear();
        self.last_activity_ms = now_ms;
        self.wrote = false;
        self.started_by_savepoint = false;
    }

    /// A successful COMMIT ends only the innermost level on servers with nested BEGIN.
    fn committed(&mut self, now_ms: i64) {
        if self.depth > 1 {
            self.depth -= 1;
            self.last_activity_ms = now_ms;
        } else {
            self.close(now_ms);
        }
    }

    /// Records a savepoint. MySQL, MariaDB and Oracle replace a savepoint with the same
    /// name, so the older one disappears from the stack; PostgreSQL and SQLite keep both.
    pub(crate) fn savepoint(&mut self, name: String, replace: bool) {
        if replace && let Some(i) = self.savepoints.iter().rposition(|s| s == &name) {
            self.savepoints.remove(i);
        }
        self.savepoints.push(name);
    }

    /// Keeps only the savepoints up to and including `name` (the target of a rollback).
    /// Duplicate names resolve to the most recent one, as PostgreSQL and SQLite do.
    pub(crate) fn rolled_back_to(&mut self, name: &str) {
        if let Some(i) = self.savepoints.iter().rposition(|s| s == name) {
            self.savepoints.truncate(i + 1);
        }
        if self.phase == TxPhase::Failed {
            self.phase = TxPhase::Open;
        }
    }

    /// Follows `stmt` after it succeeded; returns a note for the Messages log.
    pub(crate) fn after_success(
        &mut self,
        stmt: &Stmt,
        rules: TxRules,
        now_ms: i64,
    ) -> Option<RunMessage> {
        self.last_activity_ms = now_ms;
        match &stmt.class {
            StatementClass::Transaction(TxVerb::Begin) => {
                if !self.is_open() {
                    self.open(now_ms);
                } else if rules.nested_begin {
                    self.depth += 1;
                }
                Some(RunMessage::new(MessageLevel::Info, "Transaction started"))
            }
            StatementClass::Transaction(TxVerb::Commit) => {
                if self.depth > 1 {
                    self.committed(now_ms);
                    return Some(RunMessage::new(
                        MessageLevel::Info,
                        "Nested COMMIT completed; the outer transaction remains open",
                    ));
                }
                let phase = self.phase.clone();
                self.committed(now_ms);
                if stmt.chain {
                    self.open(now_ms);
                }
                match phase {
                    TxPhase::Idle => None,
                    TxPhase::Open => {
                        Some(RunMessage::new(MessageLevel::Info, "Transaction committed"))
                    }
                    // PostgreSQL turns the COMMIT of an aborted transaction into a rollback.
                    TxPhase::Failed => {
                        Some(RunMessage::new(MessageLevel::Warning, FAILED_COMMIT_NOTE))
                    }
                }
            }
            StatementClass::Transaction(TxVerb::Rollback) => {
                let was_open = self.is_open();
                self.close(now_ms);
                if stmt.chain {
                    self.open(now_ms);
                }
                was_open.then(|| RunMessage::new(MessageLevel::Info, "Transaction rolled back"))
            }
            StatementClass::Transaction(TxVerb::Savepoint(name)) => {
                if !self.is_open() && rules.savepoint_opens {
                    self.open(now_ms);
                    self.started_by_savepoint = true;
                }
                if self.is_open() {
                    self.savepoint(name.clone(), rules.savepoints_replace);
                }
                None
            }
            StatementClass::Transaction(TxVerb::RollbackTo(name)) => {
                self.rolled_back_to(name);
                None
            }
            StatementClass::Transaction(TxVerb::Release(name)) => {
                // RELEASE drops the most recent savepoint with this name (an older
                // duplicate stays valid) and every savepoint after it.
                if let Some(i) = self.savepoints.iter().rposition(|s| s == name) {
                    self.savepoints.truncate(i);
                }
                // SQLite commits when the savepoint that started the transaction goes.
                if self.started_by_savepoint && self.savepoints.is_empty() {
                    self.close(now_ms);
                }
                None
            }
            _ if self.is_open() && rules.ddl_commits && stmt.schema_change => {
                self.close(now_ms);
                Some(RunMessage::new(MessageLevel::Warning, MYSQL_DDL_NOTE))
            }
            _ => {
                if self.is_open() {
                    self.statements += 1;
                    self.wrote |= stmt.class.modifies();
                }
                None
            }
        }
    }

    /// Follows `stmt` after it failed (not by a lost connection). A server that commits
    /// before DDL has done so even when the DDL then failed.
    pub(crate) fn after_failure(
        &mut self,
        stmt: &Stmt,
        rules: TxRules,
        now_ms: i64,
    ) -> Option<RunMessage> {
        self.last_activity_ms = now_ms;
        // A failed write may still hold the locks it took.
        if self.is_open() {
            self.wrote |= stmt.class.modifies();
        }
        if self.is_open() && rules.ddl_commits && stmt.schema_change {
            self.close(now_ms);
            return Some(RunMessage::new(MessageLevel::Warning, MYSQL_DDL_NOTE));
        }
        if self.phase == TxPhase::Open && rules.error_aborts {
            self.phase = TxPhase::Failed;
            return Some(RunMessage::new(
                MessageLevel::Warning,
                "The transaction failed: only Rollback is possible",
            ));
        }
        None
    }

    /// The connection went away: the server rolled the transaction back.
    pub(crate) fn lost(&mut self, reason: &str, now_ms: i64) -> Option<RunMessage> {
        let was_open = self.is_open();
        self.close(now_ms);
        was_open.then(|| {
            RunMessage::new(
                MessageLevel::Error,
                format!("The transaction was lost: {reason}"),
            )
        })
    }
}

/// A transaction command (or read-only switch) the engine runs on a session's queue.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TxOp {
    Commit,
    Rollback,
    Savepoint(String),
    RollbackTo(String),
    SetReadOnly(bool),
}

/// Runs `sql` for its effect; a rows result is closed at once.
pub(crate) async fn run_sql(conn: &Arc<dyn Connection>, sql: &str) -> Result<(), DriverError> {
    let rel = conn.relational().ok_or_else(|| {
        DriverError::new(ErrorKind::Unsupported, "this connection does not run SQL")
    })?;
    if let Execution::Rows(mut stream) = rel.execute(sql).await? {
        // Close errors are not actionable: the statement ran.
        let _ = stream.close().await;
    }
    Ok(())
}

impl Shared {
    /// Runs `op` on the session in its turn: after every earlier execute, with the
    /// session's open stream closed first (never concurrently with a fetch).
    pub(crate) async fn run_tx_op(
        self: Arc<Self>,
        key: SessionKey,
        io: Arc<SessionIo>,
        ticket: u64,
        op: TxOp,
    ) {
        io.connect_finished().await;
        let turn = io.wait_turn(ticket).await;
        let mut slot = io.stream.lock().await;
        drop(turn);
        let snapshot = {
            let mut inner = self.inner.lock();
            inner
                .session(&key, &io)
                .filter(|s| s.is_connected())
                .and_then(|s| {
                    let base = s.conn.clone()?;
                    let others: Vec<_> = s.databases.values().cloned().collect();
                    Some((base, s.tx_conn.clone(), others, s.tx.clone()))
                })
        };
        let Some((base, tx_conn, others, tx)) = snapshot else {
            // The connect failed before the switch could run. Remember the request so the
            // next reconnect starts read-only, instead of silently dropping it.
            if let TxOp::SetReadOnly(on) = &op {
                let mut inner = self.inner.lock();
                if let Some(s) = inner.session(&key, &io) {
                    s.tx.read_only = *on;
                    let mode = s.tx.mode;
                    inner.prefs.insert(key.clone(), (mode, *on));
                }
            }
            return;
        };
        let conn = tx_conn.unwrap_or_else(|| base.clone());
        // Read-only also applies to document connections (their driver's own switch); the
        // transaction verbs need a relational dialect.
        let dialect_sql = match (&op, conn.relational()) {
            (TxOp::SetReadOnly(_), _) => String::new(),
            (TxOp::Commit, Some(_)) => "COMMIT".to_string(),
            (TxOp::Rollback, Some(_)) => "ROLLBACK".to_string(),
            (TxOp::Savepoint(name), Some(rel)) => rel.dialect().savepoint_sql(name),
            (TxOp::RollbackTo(name), Some(rel)) => rel.dialect().rollback_to_sql(name),
            (_, None) => return,
        };
        // A command that changes nothing must not close the result still streaming.
        match &op {
            TxOp::Commit | TxOp::Rollback if !tx.is_open() => return,
            TxOp::Savepoint(_) if tx.phase != TxPhase::Open => return,
            TxOp::RollbackTo(_) if !tx.is_open() => return,
            _ => {}
        }
        if let Some((prev, mut stream)) = slot.take() {
            let _ = stream.close().await;
            self.mark_complete(prev);
        }
        let rules = conn.relational().map_or(TxRules::default(), |rel| {
            TxRules::from_dialect(rel.dialect())
        });
        let outcome = match &op {
            TxOp::SetReadOnly(on) => {
                let mut outcome = Ok(());
                for c in std::iter::once(&base).chain(&others) {
                    if let Err(e) = apply_read_only(c, *on).await {
                        outcome = Err(e);
                        break;
                    }
                }
                outcome
            }
            _ => run_sql(&conn, &dialect_sql).await,
        };
        match outcome {
            Ok(()) => {
                {
                    let mut inner = self.inner.lock();
                    let inner = &mut *inner;
                    if let Some(s) = inner.session(&key, &io) {
                        let now = now_ms();
                        s.tx.last_activity_ms = now;
                        match &op {
                            TxOp::Commit => {
                                s.tx.committed(now);
                                if !s.tx.is_open() {
                                    s.tx_conn = None;
                                }
                            }
                            TxOp::Rollback => {
                                s.tx.close(now);
                                s.tx_conn = None;
                            }
                            TxOp::Savepoint(name) => {
                                let replace = conn
                                    .relational()
                                    .is_some_and(|rel| rel.dialect().savepoint_names_replace());
                                s.tx.savepoint(name.clone(), replace);
                            }
                            TxOp::RollbackTo(name) => s.tx.rolled_back_to(name),
                            // The flag follows the server once it accepted the switch.
                            TxOp::SetReadOnly(on) => {
                                s.tx.read_only = *on;
                                let mode = s.tx.mode;
                                inner.prefs.insert(key.clone(), (mode, *on));
                            }
                        }
                    }
                }
                // PostgreSQL turns the COMMIT of an aborted transaction into a rollback.
                if op == TxOp::Commit && tx.phase == TxPhase::Failed {
                    self.notice(format!("{}: {FAILED_COMMIT_NOTE}", key.conn));
                }
            }
            Err(e) => {
                self.on_error(&key, &io, &e);
                // A failed transaction command may have ended the server transaction:
                // PostgreSQL rolls back on a failed COMMIT and aborts on a failed
                // SAVEPOINT/ROLLBACK TO. Keep the UI state in step with the server.
                {
                    let mut inner = self.inner.lock();
                    if let Some(s) = inner.session(&key, &io) {
                        let now = now_ms();
                        s.tx.last_activity_ms = now;
                        match &op {
                            TxOp::Commit if rules.error_aborts => {
                                s.tx.close(now);
                                s.tx_conn = None;
                            }
                            TxOp::Rollback => {
                                // The rollback itself failed: the server state is unknown.
                                s.tx.close(now);
                                s.tx_conn = None;
                            }
                            TxOp::Savepoint(_) | TxOp::RollbackTo(_) if rules.error_aborts => {
                                s.tx.phase = TxPhase::Failed;
                            }
                            _ => {}
                        }
                    }
                }
                let what = match &op {
                    TxOp::Commit => "Commit",
                    TxOp::Rollback => "Rollback",
                    TxOp::Savepoint(_) => "Savepoint",
                    TxOp::RollbackTo(_) => "Rollback to savepoint",
                    TxOp::SetReadOnly(_) => "Switching read-only mode",
                };
                self.notice(format!("{what} failed on {}: {}", key.conn, e.message));
            }
        }
        self.repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(sql: &str) -> Stmt {
        Stmt::of(&sqlr_core::SqlSyntax::ansi(), sql)
    }

    #[test]
    fn manual_begins_before_anything_but_transaction_verbs() {
        let tx = TxState::new(CommitMode::Manual, false, 0);
        let rules = TxRules::default();
        assert!(tx.needs_begin(&class("select 1"), rules));
        assert!(!tx.needs_begin(&class("begin"), rules));
        assert!(!tx.needs_begin(&class("commit"), rules));
        let mysql = TxRules {
            ddl_commits: true,
            ..rules
        };
        assert!(!tx.needs_begin(&class("create table t (a int)"), mysql));
        assert!(!tx.needs_begin(&class("drop table t"), mysql));
        assert!(tx.needs_begin(&class("delete from t"), mysql));
    }

    #[test]
    fn smart_begins_only_before_writes() {
        let tx = TxState::new(CommitMode::Smart, false, 0);
        let rules = TxRules::default();
        assert!(!tx.needs_begin(&class("select 1"), rules));
        assert!(!tx.needs_begin(&class("set x = 1"), rules));
        assert!(tx.needs_begin(&class("insert into t values (1)"), rules));
        assert!(tx.needs_begin(&class("drop table t"), rules));
        assert!(tx.needs_begin(&class("create table t (a int)"), rules));
        assert!(
            !TxState::new(CommitMode::Auto, false, 0).needs_begin(&class("drop table t"), rules)
        );
    }

    #[test]
    fn savepoints_follow_user_statements() {
        let mut tx = TxState::new(CommitMode::Auto, false, 0);
        let rules = TxRules::default();
        tx.after_success(&class("begin"), rules, 1);
        for sql in ["savepoint a", "savepoint b", "savepoint c"] {
            tx.after_success(&class(sql), rules, 2);
        }
        tx.after_success(&class("rollback to savepoint b"), rules, 3);
        assert_eq!(tx.savepoints, ["a", "b"]);
        tx.after_success(&class("release savepoint a"), rules, 4);
        assert!(tx.savepoints.is_empty());
        tx.after_success(&class("insert into t values (1)"), rules, 5);
        assert_eq!(tx.statements, 1);
        assert_eq!(tx.since_ms, Some(1));
        tx.after_success(&class("commit"), rules, 6);
        assert_eq!(tx.phase, TxPhase::Idle);
        assert_eq!(tx.last_activity_ms, 6);
    }

    #[test]
    fn a_reused_savepoint_name_replaces_the_older_one_on_mysql() {
        let mut tx = TxState::new(CommitMode::Auto, false, 0);
        let rules = TxRules {
            savepoints_replace: true,
            ..TxRules::default()
        };
        tx.after_success(&class("begin"), rules, 1);
        for sql in ["savepoint a", "savepoint b", "savepoint a"] {
            tx.after_success(&class(sql), rules, 2);
        }
        assert_eq!(tx.savepoints, ["b", "a"]);
    }

    #[test]
    fn postgres_keeps_savepoints_with_the_same_name() {
        let mut tx = TxState::new(CommitMode::Auto, false, 0);
        let rules = TxRules::default();
        tx.after_success(&class("begin"), rules, 1);
        for sql in ["savepoint a", "savepoint b", "savepoint a"] {
            tx.after_success(&class(sql), rules, 2);
        }
        assert_eq!(tx.savepoints, ["a", "b", "a"]);
    }

    #[test]
    fn duplicate_savepoints_resolve_to_the_most_recent_one() {
        let mut tx = TxState::new(CommitMode::Auto, false, 0);
        let rules = TxRules::default();
        tx.after_success(&class("begin"), rules, 1);
        for sql in ["savepoint a", "savepoint a", "savepoint b"] {
            tx.after_success(&class(sql), rules, 2);
        }
        tx.after_success(&class("rollback to savepoint a"), rules, 3);
        assert_eq!(tx.savepoints, ["a", "a"]);
        tx.after_success(&class("release savepoint a"), rules, 4);
        assert_eq!(tx.savepoints, ["a"]);
    }

    #[test]
    fn a_top_level_savepoint_opens_and_its_release_commits() {
        let mut tx = TxState::new(CommitMode::Auto, false, 0);
        let rules = TxRules {
            savepoint_opens: true,
            ..TxRules::default()
        };
        tx.after_success(&class("savepoint outermost"), rules, 1);
        assert_eq!(tx.phase, TxPhase::Open);
        assert!(tx.started_by_savepoint);
        tx.after_success(&class("savepoint inner"), rules, 2);
        tx.after_success(&class("release savepoint inner"), rules, 3);
        assert_eq!(tx.phase, TxPhase::Open);
        tx.after_success(&class("release savepoint outermost"), rules, 4);
        assert_eq!(tx.phase, TxPhase::Idle);
        assert!(tx.savepoints.is_empty());
    }

    #[test]
    fn releasing_the_last_savepoint_keeps_an_explicit_transaction_open() {
        let mut tx = TxState::new(CommitMode::Auto, false, 0);
        let rules = TxRules {
            savepoint_opens: true,
            ..TxRules::default()
        };
        tx.after_success(&class("begin"), rules, 1);
        tx.after_success(&class("savepoint a"), rules, 2);
        tx.after_success(&class("release savepoint a"), rules, 3);
        assert_eq!(tx.phase, TxPhase::Open);
    }

    #[test]
    fn a_top_level_savepoint_does_not_open_where_the_server_ignores_it() {
        let mut tx = TxState::new(CommitMode::Auto, false, 0);
        tx.after_success(&class("savepoint a"), TxRules::default(), 1);
        assert_eq!(tx.phase, TxPhase::Idle);
    }

    #[test]
    fn failure_aborts_only_where_the_server_does() {
        let mut pg = TxState::new(CommitMode::Manual, false, 0);
        pg.open(0);
        let aborts = TxRules {
            error_aborts: true,
            ..TxRules::default()
        };
        assert!(pg.after_failure(&class("select oops"), aborts, 1).is_some());
        assert_eq!(pg.phase, TxPhase::Failed);
        let mut mysql = TxState::new(CommitMode::Manual, false, 0);
        mysql.open(0);
        assert!(
            mysql
                .after_failure(&class("select oops"), TxRules::default(), 1)
                .is_none()
        );
        assert_eq!(mysql.phase, TxPhase::Open);
        let lost = mysql.lost("server closed", 2).unwrap();
        assert_eq!(lost.text, "The transaction was lost: server closed");
        assert_eq!(lost.level, MessageLevel::Error);
        assert_eq!(mysql.phase, TxPhase::Idle);
    }

    #[test]
    fn commit_of_a_failed_transaction_says_it_rolled_back() {
        let mut pg = TxState::new(CommitMode::Manual, false, 0);
        pg.open(0);
        pg.phase = TxPhase::Failed;
        let note = pg
            .after_success(&class("commit"), TxRules::default(), 1)
            .unwrap();
        assert_eq!(note.text, "The failed transaction was rolled back");
        assert_eq!(note.level, MessageLevel::Warning);
        assert_eq!(pg.phase, TxPhase::Idle);
    }

    #[test]
    fn chained_endings_start_a_fresh_transaction_unless_no_chain_is_requested() {
        for sql in ["COMMIT AND CHAIN", "ROLLBACK WORK AND /* note */ CHAIN"] {
            let mut tx = TxState::new(CommitMode::Auto, false, 0);
            tx.open(0);
            tx.wrote = true;
            tx.savepoints.push("old".into());
            tx.after_success(&class(sql), TxRules::default(), 10);
            assert_eq!(tx.phase, TxPhase::Open, "{sql}");
            assert_eq!(tx.since_ms, Some(10));
            assert!(!tx.wrote);
            assert!(tx.savepoints.is_empty());
        }
        for sql in ["COMMIT AND NO CHAIN", "ROLLBACK /* AND CHAIN */"] {
            let mut tx = TxState::new(CommitMode::Auto, false, 0);
            tx.open(0);
            tx.after_success(&class(sql), TxRules::default(), 10);
            assert_eq!(tx.phase, TxPhase::Idle, "{sql}");
        }
    }

    #[test]
    fn failing_ddl_still_ends_a_mysql_transaction() {
        let mysql = TxRules {
            ddl_commits: true,
            ..TxRules::default()
        };
        let mut tx = TxState::new(CommitMode::Manual, false, 0);
        tx.open(0);
        let note = tx
            .after_failure(&class("create table t (a int)"), mysql, 1)
            .unwrap();
        assert_eq!(note.text, MYSQL_DDL_NOTE);
        assert_eq!(tx.phase, TxPhase::Idle);
        // Without the implicit commit a failing DML statement leaves it open.
        tx.open(2);
        assert!(
            tx.after_failure(&class("delete from t"), mysql, 3)
                .is_none()
        );
        assert_eq!(tx.phase, TxPhase::Open);
    }

    #[test]
    fn truncate_and_grant_count_as_ddl() {
        let mysql = TxRules {
            ddl_commits: true,
            ..TxRules::default()
        };
        let tx = TxState::new(CommitMode::Manual, false, 0);
        assert!(!tx.needs_begin(&class("truncate t"), mysql));
        assert!(!tx.needs_begin(&class("grant select on t to bob"), mysql));
    }
}
