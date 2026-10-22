//! Applying a changeset (§7.7, Decisions "Apply"): on the tab's session, in its own
//! transaction or inside the one that is open, refusing the whole changeset when a row was
//! changed or deleted by someone else.

use std::sync::Arc;

use sqlr_core::{Changeset, Connection, DriverError, ErrorKind, Execution, TableRef};

use crate::engine::{Shared, same_connection};
use crate::job::{ApplyReport, JobControl, JobOutcome};
use crate::library::now_ms;
use crate::session::{LoadState, SessionIo, SessionKey};
use crate::tx::{READ_ONLY, TxPhase, run_sql};

/// The savepoint that makes a changeset atomic inside the user's open transaction.
const SAVEPOINT: &str = "sqlr_apply";

/// Where the changeset's statements run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Auto-commit: `BEGIN` … `COMMIT`, `ROLLBACK` on any failure.
    Own,
    /// Manual or Smart with no transaction open: `BEGIN`, and the transaction stays open
    /// for the user to commit.
    Opens,
    /// A transaction is open: the changeset joins it behind a savepoint, so a failure undoes
    /// only the changeset.
    Joins,
}

impl Shared {
    /// Runs the changeset in the session's turn, after closing its open stream.
    pub(crate) async fn run_apply(
        self: Arc<Self>,
        job: JobControl,
        key: SessionKey,
        io: Arc<SessionIo>,
        ticket: u64,
        changeset: Changeset,
    ) {
        io.connect_finished().await;
        let turn = io.wait_turn(ticket).await;
        let mut slot = io.stream.lock().await;
        drop(turn);
        if let Some((prev, mut stream)) = slot.take() {
            let _ = stream.close().await;
            self.mark_complete(prev);
        }
        let outcome = match self.apply(&job, &key, &io, &changeset).await {
            Ok(report) => JobOutcome::Applied(report),
            Err(e) if e.kind == ErrorKind::Cancelled => JobOutcome::Cancelled,
            Err(e) => JobOutcome::Failed(e),
        };
        self.jobs.finish(job.id, outcome);
        self.repaint();
    }

    async fn apply(
        self: &Arc<Self>,
        job: &JobControl,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        changeset: &Changeset,
    ) -> Result<ApplyReport, DriverError> {
        let cancelled = || DriverError::new(ErrorKind::Cancelled, "The changes were not applied");
        if job.is_cancelled() {
            return Err(cancelled());
        }
        let conn = self
            .session_connection(key, io, changeset.table.database.as_deref())
            .await?;
        let (tx, tx_conn) = {
            let mut inner = self.inner.lock();
            let s = inner
                .session(key, io)
                .ok_or_else(crate::engine::not_connected)?;
            (s.tx.clone(), s.tx_conn.clone())
        };
        if tx.read_only {
            return Err(DriverError::new(ErrorKind::Permission, READ_ONLY));
        }
        let mode = match tx.phase {
            TxPhase::Failed => {
                return Err(DriverError::new(
                    ErrorKind::Other,
                    "The transaction failed: roll it back before applying changes",
                ));
            }
            TxPhase::Open => {
                if let Some(other) = tx_conn.filter(|c| !same_connection(c, &conn)) {
                    let db = other
                        .database_name()
                        .unwrap_or("another database")
                        .to_string();
                    return Err(DriverError::new(
                        ErrorKind::Other,
                        format!("A transaction is open on {db}; commit or roll back first"),
                    ));
                }
                Mode::Joins
            }
            TxPhase::Idle if tx.mode == sqlr_core::CommitMode::Auto => Mode::Own,
            TxPhase::Idle => Mode::Opens,
        };
        let rel = conn.relational().ok_or_else(|| {
            DriverError::new(ErrorKind::Unsupported, "this connection does not run SQL")
        })?;
        let dialect = rel.dialect();
        let start = match mode {
            Mode::Own | Mode::Opens => dialect.begin_sql().to_string(),
            Mode::Joins => dialect.savepoint_sql(SAVEPOINT),
        };
        if let Err(e) = run_sql(&conn, &start).await {
            self.on_error(key, io, &e);
            return Err(e);
        }
        if mode == Mode::Opens {
            let mut inner = self.inner.lock();
            if let Some(s) = inner.session(key, io) {
                s.tx.open(now_ms());
                s.tx_conn = Some(conn.clone());
            }
        }
        job.set_cancel(Some(conn.cancel_handle()));
        let local = TableRef {
            database: None,
            ..changeset.table.clone()
        };
        if let Err(e) = rel.check_origin(&local).await {
            self.undo(key, io, &conn, mode, &e).await;
            return Err(e);
        }
        let statements = match changeset.statements(dialect) {
            Ok(statements) => statements,
            Err(e) => {
                let e = DriverError::new(ErrorKind::Other, e);
                self.undo(key, io, &conn, mode, &e).await;
                return Err(e);
            }
        };
        for stmt in &statements {
            if job.is_cancelled() {
                job.set_cancel(None);
                let e = cancelled();
                self.undo(key, io, &conn, mode, &e).await;
                return Err(e);
            }
            let failure = match rel.execute_with(&stmt.sql, &stmt.params).await {
                Ok(Execution::Affected(1)) => None,
                Ok(Execution::Affected(n)) if !stmt.expect_one_row => {
                    let _ = n;
                    None
                }
                Ok(Execution::Affected(n)) => Some(DriverError::new(
                    ErrorKind::Other,
                    format!(
                        "{} was changed or deleted by someone else: the statement matched {n} \
                         rows instead of 1. Nothing was applied; refresh the data and try again",
                        stmt.row_label
                    ),
                )),
                Ok(Execution::Rows(mut rows)) => {
                    let _ = rows.close().await;
                    Some(DriverError::new(
                        ErrorKind::Protocol,
                        "the server returned rows instead of a row count",
                    ))
                }
                Err(e) => Some(e),
            };
            if let Some(e) = failure {
                job.set_cancel(None);
                self.undo(key, io, &conn, mode, &e).await;
                if job.is_cancelled() || e.kind == ErrorKind::Cancelled {
                    return Err(cancelled());
                }
                return Ok(ApplyReport {
                    applied: 0,
                    failed: Some((stmt.row_label.clone(), e)),
                });
            }
        }
        job.set_cancel(None);
        let finish = match mode {
            Mode::Own => Some("COMMIT".to_string()),
            Mode::Joins => Some(format!(
                "RELEASE SAVEPOINT {}",
                dialect.quote_ident(SAVEPOINT)
            )),
            Mode::Opens => None,
        };
        if let Some(sql) = finish
            && let Err(e) = run_sql(&conn, &sql).await
        {
            self.undo(key, io, &conn, mode, &e).await;
            return Err(e);
        }
        if mode != Mode::Own {
            let mut inner = self.inner.lock();
            if let Some(s) = inner.session(key, io) {
                s.tx.statements += statements.len() as u32;
                s.tx.wrote = true;
                s.tx.last_activity_ms = now_ms();
            }
        }
        Ok(ApplyReport {
            applied: statements.len(),
            failed: None,
        })
    }

    /// Undoes what the changeset did after `cause`: the transaction it opened, or its
    /// savepoint in the user's transaction. Nothing to undo when the connection is gone.
    async fn undo(
        self: &Arc<Self>,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        conn: &Arc<dyn Connection>,
        mode: Mode,
        cause: &DriverError,
    ) {
        if cause.kind == ErrorKind::Connection {
            self.on_error(key, io, cause);
            return;
        }
        let Some(rel) = conn.relational() else {
            return;
        };
        let outcome = match mode {
            Mode::Own | Mode::Opens => run_sql(conn, "ROLLBACK").await,
            Mode::Joins => {
                let back = run_sql(conn, &rel.dialect().rollback_to_sql(SAVEPOINT)).await;
                match back {
                    Ok(()) => {
                        let release =
                            format!("RELEASE SAVEPOINT {}", rel.dialect().quote_ident(SAVEPOINT));
                        run_sql(conn, &release).await
                    }
                    Err(e) => Err(e),
                }
            }
        };
        let mut inner = self.inner.lock();
        let Some(s) = inner.session(key, io) else {
            return;
        };
        match (&outcome, mode) {
            (Ok(()), Mode::Opens) => {
                s.tx.close(now_ms());
                s.tx_conn = None;
            }
            (Ok(()), _) => {}
            (Err(_), Mode::Joins) if rel.dialect().error_aborts_transaction() => {
                s.tx.phase = TxPhase::Failed;
            }
            (Err(_), _) => {}
        }
        drop(inner);
        if let Err(e) = outcome {
            self.on_error(key, io, &e);
            self.notice(format!(
                "Undoing the changes failed on {}: {}",
                key.conn, e.message
            ));
        }
    }

    /// Describes `table` on the session's connection (the server database `table` names,
    /// when it is not the session's own) into the table cache.
    pub(crate) async fn run_describe(
        self: Arc<Self>,
        key: SessionKey,
        io: Arc<SessionIo>,
        table: TableRef,
    ) {
        io.connect_finished().await;
        let state = match self
            .session_connection(&key, &io, table.database.as_deref())
            .await
        {
            Err(e) => LoadState::Failed(e),
            Ok(conn) => match conn.relational() {
                None => LoadState::Failed(DriverError::new(
                    ErrorKind::Unsupported,
                    "this connection has no tables",
                )),
                Some(rel) => match rel.describe(&table).await {
                    Ok(info) => LoadState::Ready(info),
                    Err(e) => LoadState::Failed(e),
                },
            },
        };
        {
            let mut inner = self.inner.lock();
            let entry = (key.conn.clone(), table);
            if inner.tables.get(&entry) == Some(&LoadState::Loading) {
                inner.tables.insert(entry, state);
            }
        }
        self.repaint();
    }
}
