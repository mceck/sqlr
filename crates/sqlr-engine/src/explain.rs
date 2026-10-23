//! Explain jobs (spec §8.6): the plan of one statement, as a tree. The statement is explained
//! on the session's own connection, in its turn; with Analyze a data change runs inside a
//! transaction that is rolled back, so it never commits.

use std::sync::Arc;

use sqlr_core::{DriverError, ErrorKind, StatementClass};

use crate::engine::{Command, Engine, Shared, not_connected};
use crate::job::{JobControl, JobId, JobKind, JobOutcome};
use crate::session::{SessionIo, SessionKey};
use crate::transfer::take_turn;

impl Engine {
    /// Explains `sql` on session `key` as a job ([`JobOutcome::Plan`]); `analyze` runs it.
    pub fn explain(&self, key: &SessionKey, sql: String, analyze: bool) -> JobId {
        let job = self.shared.jobs.create(JobKind::Explain);
        let id = job.id;
        let io = {
            let inner = self.shared.inner.lock();
            inner
                .sessions
                .get(key)
                .filter(|s| s.is_live())
                .map(|s| s.io.clone())
        };
        match io {
            None => self
                .shared
                .jobs
                .finish(id, JobOutcome::Failed(not_connected())),
            Some(io) => {
                let ticket = io.issue_ticket();
                let task =
                    self.shared
                        .clone()
                        .run_explain(job, key.clone(), io, ticket, sql, analyze);
                self.shared.send(Command::Spawn(Box::pin(task)));
            }
        }
        self.shared.repaint();
        id
    }
}

impl Shared {
    async fn run_explain(
        self: Arc<Self>,
        job: JobControl,
        key: SessionKey,
        io: Arc<SessionIo>,
        ticket: u64,
        sql: String,
        analyze: bool,
    ) {
        let _slot = take_turn(&self, &io, ticket).await;
        let outcome = self.explain_on(&key, &io, &sql, analyze).await;
        let outcome = match outcome {
            Ok(plan) => JobOutcome::Plan(plan),
            Err(e) => JobOutcome::Failed(e),
        };
        self.jobs.finish(job.id, outcome);
        self.repaint();
    }

    async fn explain_on(
        &self,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        sql: &str,
        analyze: bool,
    ) -> Result<sqlr_core::PlanTree, DriverError> {
        let conn = self.connected(key, io).await.ok_or_else(not_connected)?;
        let rel = conn.relational().ok_or_else(|| {
            DriverError::new(ErrorKind::Unsupported, "this connection has no query plans")
        })?;
        if analyze {
            let open = {
                let mut inner = self.inner.lock();
                inner.session(key, io).is_some_and(|s| s.tx.is_open())
            };
            if open && rel.dialect().syntax().classify_script(sql) != StatementClass::Read {
                return Err(DriverError::new(
                    ErrorKind::Other,
                    "A transaction is open on this session: commit or roll back first, or use \
                     Explain without Analyze",
                ));
            }
        }
        rel.explain(sql, analyze).await
    }
}
