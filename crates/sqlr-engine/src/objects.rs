//! Object editor engine operations: loading object definitions, writing DDL with
//! the connected dialect (no I/O), and applying DDL on a session.
//!
//! An apply follows the [`DdlGen`] placement rule: each maximal run of
//! [`Placement::Transaction`] statements runs in one transaction when the driver declares
//! transactional DDL (otherwise statement by statement); [`Placement::Alone`] statements run
//! by themselves; after a failure the rest is skipped, except [`Placement::Cleanup`]
//! statements. It runs in the session's execute order, holding its stream, and is refused on
//! a read-only session and inside the user's open transaction. Once something was applied
//! the connection's catalog is forgotten and the explorer paths given are reloaded.

use std::sync::Arc;

use sqlr_core::{
    Connection, DdlGen, DriverError, ErrorKind, Node, NodePath, ObjectDef, ObjectRef,
    PASSWORD_MASK, Password, Placement, SchemaChange, SqlTemplate, TableDef,
};

use crate::engine::{Command, Engine, Shared};
use crate::job::{JobControl, JobId, JobKind, JobOutcome};
use crate::session::{LoadState, SessionIo, SessionKey};
use crate::tx::{READ_ONLY, run_sql};

/// What became of one statement of an apply.
#[derive(Clone, Debug, PartialEq)]
pub enum DdlStepOutcome {
    Applied,
    /// It ran, then its transaction was rolled back.
    RolledBack,
    Failed(DriverError),
    /// Not run: an earlier statement failed.
    Skipped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DdlReport {
    pub statements: Vec<(String, DdlStepOutcome)>,
    /// A transaction was rolled back after a failure.
    pub rolled_back: bool,
    /// The apply ran transaction statements inside transactions.
    pub transactional: bool,
    /// Transaction commands that failed (`BEGIN`, `COMMIT`), each with its error; the
    /// statements of that transaction are `Skipped` (BEGIN) or `RolledBack` (COMMIT).
    pub control: Vec<(String, DriverError)>,
}

impl DdlReport {
    /// Every statement was applied.
    pub fn ok(&self) -> bool {
        self.control.is_empty()
            && self
                .statements
                .iter()
                .all(|(_, o)| *o == DdlStepOutcome::Applied)
    }

    /// The first failure: a statement's, else a transaction command's.
    pub fn failure(&self) -> Option<&DriverError> {
        self.statements
            .iter()
            .find_map(|(_, o)| match o {
                DdlStepOutcome::Failed(e) => Some(e),
                _ => None,
            })
            .or_else(|| self.control.first().map(|(_, e)| e))
    }

    /// Statements that stayed applied.
    pub fn applied(&self) -> usize {
        self.statements
            .iter()
            .filter(|(_, o)| *o == DdlStepOutcome::Applied)
            .count()
    }

    /// One or two sentences for the user.
    pub fn summary(&self) -> String {
        let n = self.statements.len();
        let Some((at, e)) = self
            .statements
            .iter()
            .enumerate()
            .find_map(|(i, (_, o))| match o {
                DdlStepOutcome::Failed(e) => Some((i, e)),
                _ => None,
            })
        else {
            if let Some((verb, e)) = self.control.first() {
                let mut out = format!("{verb} failed: {}.", e.message);
                if self.rolled_back {
                    out.push_str(" The transaction was rolled back.");
                }
                return out;
            }
            return match n {
                1 => "1 statement applied".to_string(),
                n => format!("{n} statements applied"),
            };
        };
        let mut out = format!("Statement {} of {n} failed: {}.", at + 1, e.message);
        if self.rolled_back {
            out.push_str(" The transaction was rolled back.");
        }
        if !self.transactional {
            let before = self.statements[..at]
                .iter()
                .filter(|(_, o)| *o == DdlStepOutcome::Applied)
                .count();
            if before > 0 {
                let (noun, verb) = if before == 1 {
                    ("statement", "was")
                } else {
                    ("statements", "were")
                };
                out.push_str(&format!(
                    " The DDL is not transactional: {before} {noun} before the failure {verb} applied."
                ));
            }
        }
        out
    }
}

/// How [`Engine::apply_ddl`] runs its statements.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ApplyOptions {
    /// Run transaction statements inside transactions (the driver's `transactional_ddl`).
    pub transactional: bool,
    /// The server database to run on, when it is not the session's own.
    pub database: Option<String>,
    /// Explorer paths to reload once something was applied.
    pub refresh: Vec<NodePath>,
    /// Set when the statements hold secrets (a role's password): what the report shows.
    pub masking: Option<Masking>,
}

/// Statements whose text holds secrets. The server gets the statements as given; the
/// report (and so the job's outcome) holds `shown` instead, and every secret is replaced
/// by [`PASSWORD_MASK`] in its error messages.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Masking {
    /// The statements as previews show them, one per statement run.
    pub shown: Vec<String>,
    pub secrets: Vec<Password>,
}

impl Masking {
    /// `text` without any of the secrets, as typed or as a dialect escaped them inside a
    /// string literal (quotes doubled, backslashes doubled, or both: SQL, MySQL's
    /// `comment_literal`, PostgreSQL's `E'…'`).
    fn scrub(&self, text: &str) -> String {
        let mut forms: Vec<String> = Vec::new();
        for s in self
            .secrets
            .iter()
            .map(Password::expose)
            .filter(|s| !s.is_empty())
        {
            let quotes = s.replace('\'', "''");
            let backslashes = s.replace('\\', "\\\\");
            let both = backslashes.replace('\'', "''");
            for form in [both, quotes, backslashes, s.to_string()] {
                if !forms.contains(&form) {
                    forms.push(form);
                }
            }
        }
        // A longer form first, so a shorter one cannot leave part of it behind.
        forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
        forms.iter().fold(text.to_string(), |t, form| {
            t.replace(form.as_str(), PASSWORD_MASK)
        })
    }

    fn scrub_error(&self, e: &mut DriverError) {
        e.message = self.scrub(&e.message);
        if let Some(hint) = &e.hint {
            e.hint = Some(self.scrub(hint));
        }
    }

    /// `report` as the user may see it.
    fn mask(&self, report: &mut DdlReport) {
        for (i, (text, outcome)) in report.statements.iter_mut().enumerate() {
            *text = match self.shown.get(i) {
                Some(shown) => shown.clone(),
                None => self.scrub(text),
            };
            if let DdlStepOutcome::Failed(e) = outcome {
                self.scrub_error(e);
            }
        }
        for (_, e) in &mut report.control {
            self.scrub_error(e);
        }
    }
}

fn not_connected() -> DriverError {
    DriverError::new(ErrorKind::Connection, "not connected")
}

fn unsupported(message: &str) -> DriverError {
    DriverError::new(ErrorKind::Unsupported, message)
}

impl Engine {
    /// Loads the definition of `object` on the connection's explorer session, as a
    /// [`JobKind::LoadObject`] job: [`JobOutcome::Object`] or [`JobOutcome::Failed`].
    pub fn load_object(&self, conn: &str, object: ObjectRef) -> JobId {
        let shared = &self.shared;
        let job = shared.jobs.create(JobKind::LoadObject);
        let id = job.id;
        match shared.live_io(conn) {
            None => shared.jobs.finish(id, JobOutcome::Failed(not_connected())),
            Some(io) => {
                let task = shared.clone().run_load(job, conn.to_string(), io, object);
                shared.send(Command::job(id, Some(conn.to_string()), task));
            }
        }
        shared.repaint();
        id
    }

    /// Writes the `CREATE` statement of the object `node` stands for
    /// ([`sqlr_core::RelationalConn::ddl`]) on the session `key`, as a [`JobKind::LoadDdl`]
    /// job: [`JobOutcome::Ddl`], or [`JobOutcome::Failed`] (not connected, no object, no DDL
    /// support).
    pub fn load_ddl(&self, key: &SessionKey, node: Node) -> JobId {
        let shared = &self.shared;
        let job = shared.jobs.create(JobKind::LoadDdl);
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
                let task = shared.clone().run_load_ddl(job, key.clone(), io, node);
                shared.send(Command::job(id, Some(key.conn.clone()), task));
            }
        }
        shared.repaint();
        id
    }

    /// Runs DDL `statements` (as a [`DdlGen`] wrote them) on the session `key`, in its execute
    /// order, as a [`JobKind::ApplyDdl`] job: [`JobOutcome::DdlApplied`] with the report,
    /// [`JobOutcome::Failed`] when it is refused (no live session, read-only, an open
    /// transaction), or [`JobOutcome::Cancelled`] when cancelled before it started. A cancel
    /// while it runs interrupts the running statement, which then fails.
    pub fn apply_ddl(
        &self,
        key: &SessionKey,
        statements: Vec<String>,
        options: ApplyOptions,
    ) -> JobId {
        let shared = &self.shared;
        let job = shared.jobs.create(JobKind::ApplyDdl);
        let id = job.id;
        {
            let inner = shared.inner.lock();
            let io = inner
                .sessions
                .get(key)
                .filter(|s| s.is_live())
                .map(|s| s.io.clone());
            match io {
                None => shared.jobs.finish(id, JobOutcome::Failed(not_connected())),
                Some(io) => {
                    let ticket = io.issue_ticket();
                    let task = shared.clone().run_ddl_apply(
                        job,
                        key.clone(),
                        io,
                        ticket,
                        statements,
                        options,
                    );
                    shared.send(Command::job(id, Some(key.conn.clone()), task));
                }
            }
        }
        shared.repaint();
        id
    }

    /// The statements that apply `changes`, from the connected session's dialect (no I/O).
    pub fn preview_ddl(
        &self,
        conn: &str,
        changes: &[SchemaChange],
    ) -> Result<Vec<String>, DriverError> {
        self.with_ddl_gen(conn, |g| g.ddl(changes))
    }

    /// The statements that turn table `before` into `after` (no I/O).
    pub fn table_ddl(
        &self,
        conn: &str,
        before: &TableDef,
        after: &TableDef,
    ) -> Result<Vec<String>, DriverError> {
        self.with_ddl_gen(conn, |g| g.table_ddl(before, after))
    }

    /// The script that creates `def` (no I/O).
    pub fn create_ddl(&self, conn: &str, def: &ObjectDef) -> Result<String, DriverError> {
        self.with_ddl_gen(conn, |g| g.create_ddl(def))
    }

    /// A "Generate SQL" template for `def` in the connected session's dialect (no I/O).
    pub fn generate_sql(
        &self,
        conn: &str,
        template: SqlTemplate,
        def: &ObjectDef,
    ) -> Result<String, DriverError> {
        let conn = self.meta_conn(conn)?;
        let rel = conn
            .relational()
            .ok_or_else(|| unsupported("this connection does not run SQL"))?;
        sqlr_core::generate_sql(template, def, rel.dialect())
    }

    /// The object an explorer node stands for, once the connection is connected.
    pub fn object_ref(&self, conn: &str, node: &Node) -> Option<ObjectRef> {
        self.meta_conn(conn).ok()?.relational()?.object_ref(node)
    }

    /// The server database objects under `path` belong to, when it is not the connection's
    /// own.
    pub fn database_for_path(&self, conn: &str, path: &NodePath) -> Option<String> {
        self.meta_conn(conn).ok()?.database_for_path(path)
    }

    pub(crate) fn meta_conn(&self, conn: &str) -> Result<Arc<dyn Connection>, DriverError> {
        let inner = self.shared.inner.lock();
        inner
            .sessions
            .get(&SessionKey::meta(conn))
            .filter(|s| s.is_connected())
            .and_then(|s| s.conn.clone())
            .ok_or_else(not_connected)
    }

    fn with_ddl_gen<T>(
        &self,
        conn: &str,
        f: impl FnOnce(&dyn DdlGen) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        let conn = self.meta_conn(conn)?;
        let generator = conn
            .relational()
            .and_then(|rel| rel.dialect().ddl_gen())
            .ok_or_else(|| unsupported("object editing is not supported"))?;
        f(generator)
    }
}

impl Shared {
    async fn run_load(
        self: Arc<Self>,
        job: JobControl,
        conn_id: String,
        io: Arc<SessionIo>,
        object: ObjectRef,
    ) {
        let key = SessionKey::meta(&conn_id);
        let outcome = match self.connected(&key, &io).await {
            None => Err(not_connected()),
            Some(conn) => match conn.relational().and_then(|rel| rel.introspect()) {
                None => Err(unsupported("object definitions are not supported")),
                Some(introspect) => introspect.load_object(&object).await,
            },
        };
        if let Err(e) = &outcome {
            self.on_error(&key, &io, e);
        }
        let outcome = match outcome {
            Ok(def) => JobOutcome::Object(def),
            Err(e) => JobOutcome::Failed(e),
        };
        self.jobs.finish(job.id, outcome);
        self.repaint();
    }

    async fn run_load_ddl(
        self: Arc<Self>,
        job: JobControl,
        key: SessionKey,
        io: Arc<SessionIo>,
        node: Node,
    ) {
        let outcome = match self.connected(&key, &io).await {
            None => Err(not_connected()),
            Some(conn) => match conn.relational() {
                None => Err(unsupported("DDL is not supported")),
                Some(rel) => rel.ddl(&node).await,
            },
        };
        if let Err(e) = &outcome {
            self.on_error(&key, &io, e);
        }
        let outcome = match outcome {
            Ok(ddl) => JobOutcome::Ddl(ddl),
            Err(e) => JobOutcome::Failed(e),
        };
        self.jobs.finish(job.id, outcome);
        self.repaint();
    }

    async fn run_ddl_apply(
        self: Arc<Self>,
        job: JobControl,
        key: SessionKey,
        io: Arc<SessionIo>,
        ticket: u64,
        statements: Vec<String>,
        options: ApplyOptions,
    ) {
        io.connect_finished().await;
        let turn = io.wait_turn(ticket).await;
        // Held for the whole apply: nothing else runs on the session meanwhile.
        let mut slot = io.stream.lock().await;
        drop(turn);
        if let Some((prev, mut stream)) = slot.take() {
            let _ = stream.close().await;
            self.mark_complete(prev);
        }
        let outcome = if job.is_cancelled() {
            Err(DriverError::new(
                ErrorKind::Cancelled,
                "The DDL was not applied",
            ))
        } else {
            self.apply_locked(&job, &key, &io, statements, &options)
                .await
        };
        drop(slot);
        let outcome = match (outcome, &options.masking) {
            (Ok(mut report), Some(masking)) => {
                masking.mask(&mut report);
                Ok(report)
            }
            (Err(mut e), Some(masking)) => {
                masking.scrub_error(&mut e);
                Err(e)
            }
            (outcome, None) => outcome,
        };
        let outcome = match outcome {
            Ok(report) => {
                if report
                    .statements
                    .iter()
                    .any(|(_, o)| *o == DdlStepOutcome::Applied)
                {
                    self.invalidate_catalog(&key.conn);
                    for path in options.refresh {
                        self.refresh_explorer(&key.conn, path);
                    }
                }
                JobOutcome::DdlApplied(report)
            }
            Err(e) if e.kind == ErrorKind::Cancelled => JobOutcome::Cancelled,
            Err(e) => JobOutcome::Failed(e),
        };
        self.jobs.finish(job.id, outcome);
        self.repaint();
    }

    /// The apply itself, with the session's stream held.
    async fn apply_locked(
        self: &Arc<Self>,
        job: &JobControl,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        statements: Vec<String>,
        options: &ApplyOptions,
    ) -> Result<DdlReport, DriverError> {
        let (base, existing) = {
            let mut inner = self.inner.lock();
            // Another session's open transaction may hold locks the DDL would wait on; one
            // that only read is ignored. The apply's own session never runs DDL inside one.
            let mut open: Vec<&crate::session::Slot> = inner
                .sessions
                .iter()
                .filter(|(k, s)| {
                    k.conn == key.conn
                        && if *k == key {
                            s.tx.is_open()
                        } else {
                            s.tx.blocks_ddl()
                        }
                })
                .map(|(k, _)| &k.slot)
                .collect();
            open.sort_by_key(|slot| match slot {
                crate::session::Slot::Meta => 0,
                crate::session::Slot::Tab(t) => t + 1,
                crate::session::Slot::Agent(_) => 1,
            });
            if let Some(slot) = open.first() {
                let place = match slot {
                    crate::session::Slot::Meta => "the explorer session".to_string(),
                    crate::session::Slot::Tab(t) => format!("tab {t}"),
                    crate::session::Slot::Agent(_) => "an agent session".to_string(),
                };
                return Err(DriverError::new(
                    ErrorKind::Other,
                    format!("A transaction is open in {place}: commit or roll back first"),
                ));
            }
            let session = inner
                .session(key, io)
                .filter(|s| s.is_connected())
                .ok_or_else(not_connected)?;
            if session.tx.read_only {
                return Err(DriverError::new(ErrorKind::Permission, READ_ONLY));
            }
            let base = session.conn.clone().ok_or_else(not_connected)?;
            let existing = options
                .database
                .as_ref()
                .and_then(|db| session.databases.get(db))
                .cloned();
            (base, existing)
        };
        let conn = match &options.database {
            Some(db) if base.database_name() != Some(db.as_str()) => match existing {
                Some(conn) => conn,
                None => {
                    let conn = base.connect_database(db).await?;
                    if let Some(s) = self.inner.lock().session(key, io) {
                        s.databases.insert(db.clone(), conn.clone());
                    }
                    conn
                }
            },
            _ => base,
        };
        // A cancel interrupts the statement that runs.
        job.set_cancel(Some(conn.cancel_handle()));
        let placements: Vec<Placement> = {
            let generator = conn.relational().and_then(|rel| rel.dialect().ddl_gen());
            statements
                .iter()
                .map(|s| generator.map_or(Placement::Transaction, |g| g.placement(s)))
                .collect()
        };
        let begin = conn
            .relational()
            .map_or("BEGIN", |rel| rel.dialect().begin_sql());
        let mut outcomes: Vec<DdlStepOutcome> = vec![DdlStepOutcome::Skipped; statements.len()];
        let mut failed = false;
        let mut rolled_back = false;
        let mut control = Vec::new();
        let mut i = 0;
        while i < statements.len() {
            let placement = placements[i];
            if placement == Placement::Cleanup || (!failed && placement == Placement::Alone) {
                outcomes[i] = self.step(key, io, &conn, &statements[i]).await;
                failed |= matches!(outcomes[i], DdlStepOutcome::Failed(_));
                i += 1;
                continue;
            }
            if failed {
                i += 1;
                continue;
            }
            if !options.transactional {
                outcomes[i] = self.step(key, io, &conn, &statements[i]).await;
                failed |= matches!(outcomes[i], DdlStepOutcome::Failed(_));
                i += 1;
                continue;
            }
            // One transaction over the run of transaction statements starting here.
            let end = (i..statements.len())
                .find(|&j| placements[j] != Placement::Transaction)
                .unwrap_or(statements.len());
            if let Err(e) = run_sql(&conn, begin).await {
                self.on_error(key, io, &e);
                control.push((begin.to_string(), e));
                failed = true;
                i = end;
                continue;
            }
            for j in i..end {
                outcomes[j] = self.step(key, io, &conn, &statements[j]).await;
                if matches!(outcomes[j], DdlStepOutcome::Failed(_)) {
                    failed = true;
                    if let Err(e) = run_sql(&conn, "ROLLBACK").await {
                        self.on_error(key, io, &e);
                    }
                    rolled_back = true;
                    for outcome in &mut outcomes[i..j] {
                        *outcome = DdlStepOutcome::RolledBack;
                    }
                    break;
                }
            }
            if !failed && let Err(e) = run_sql(&conn, "COMMIT").await {
                self.on_error(key, io, &e);
                control.push(("COMMIT".to_string(), e));
                // A rejected COMMIT can leave the transaction open (for example,
                // deferred foreign keys in SQLite). Undo it before cleanup or reuse.
                if let Err(undo) = run_sql(&conn, "ROLLBACK").await {
                    self.on_error(key, io, &undo);
                    control.push(("ROLLBACK".to_string(), undo));
                }
                for outcome in &mut outcomes[i..end] {
                    *outcome = DdlStepOutcome::RolledBack;
                }
                failed = true;
                rolled_back = true;
            }
            i = end;
        }
        Ok(DdlReport {
            statements: statements.into_iter().zip(outcomes).collect(),
            rolled_back,
            transactional: options.transactional,
            control,
        })
    }

    /// Runs one statement; a lost connection fails the session.
    async fn step(
        self: &Arc<Self>,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        conn: &Arc<dyn Connection>,
        sql: &str,
    ) -> DdlStepOutcome {
        match run_sql(conn, sql).await {
            Ok(()) => DdlStepOutcome::Applied,
            Err(e) => {
                self.on_error(key, io, &e);
                DdlStepOutcome::Failed(e)
            }
        }
    }

    /// Drops the cached explorer subtree at `path` and loads `path` again when it was
    /// loaded; an expanded descendant is browsed again when the explorer shows it.
    fn refresh_explorer(&self, conn_id: &str, path: NodePath) {
        let io = self.live_io(conn_id);
        let mut inner = self.inner.lock();
        let Some(cache) = inner.explorer.get_mut(conn_id) else {
            return;
        };
        let was_loaded = cache.contains_key(&path);
        cache.retain(|p, _| !p.segments().starts_with(path.segments()));
        if let (true, Some(io)) = (was_loaded, io) {
            cache.insert(path.clone(), LoadState::Loading);
            self.send(Command::Browse {
                conn_id: conn_id.to_string(),
                path,
                io,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(outcomes: Vec<DdlStepOutcome>, rolled_back: bool) -> DdlReport {
        DdlReport {
            statements: outcomes
                .into_iter()
                .enumerate()
                .map(|(i, o)| (format!("s{i}"), o))
                .collect(),
            rolled_back,
            transactional: true,
            control: Vec::new(),
        }
    }

    #[test]
    fn summaries() {
        let ok = report(vec![DdlStepOutcome::Applied; 2], false);
        assert!(ok.ok());
        assert_eq!(ok.summary(), "2 statements applied");
        let e = DriverError::new(ErrorKind::Syntax, "bad");
        let failed = report(
            vec![
                DdlStepOutcome::RolledBack,
                DdlStepOutcome::Failed(e),
                DdlStepOutcome::Skipped,
            ],
            true,
        );
        assert_eq!(
            failed.summary(),
            "Statement 2 of 3 failed: bad. The transaction was rolled back."
        );
        assert_eq!(failed.applied(), 0);
    }
}
