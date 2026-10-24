//! Administration engine operations: snapshots of roles, grants, sessions,
//! locks, properties and statistics, and the cancel and terminate actions, as general jobs
//! on the connection's explorer session; the dialect's role and grant DDL (no I/O).
//!
//! Every driver call is async I/O, so a snapshot never holds one of the runtime's two
//! workers. Role and grant changes are DDL: they are applied with [`Engine::apply_ddl`].

use std::collections::HashSet;
use std::sync::Arc;

use sqlr_core::{
    Admin, AdminSupport, DriverError, ErrorKind, Grant, LockInfo, ObjectRef, RoleDef,
    ServerProperty, SessionInfo, TableStats,
};

use crate::engine::{Command, Engine, Shared, not_connected};
use crate::job::{JobControl, JobId, JobKind, JobOutcome};
use crate::session::{SessionIo, SessionKey};
use crate::tx::READ_ONLY;

/// What an administration snapshot reads.
#[derive(Clone, Debug, PartialEq)]
pub enum AdminQuery {
    Roles,
    /// Grants, of one grantee and/or on one object when given.
    Grants {
        grantee: Option<String>,
        object: Option<ObjectRef>,
    },
    Sessions,
    Locks,
    Properties,
    DatabaseSizes,
    /// Statistics of the tables of `schema` (every schema without one).
    TableStats {
        schema: Option<String>,
    },
}

/// What a snapshot read, by [`AdminQuery`].
#[derive(Clone, Debug, PartialEq)]
pub enum AdminSnapshot {
    Roles(Vec<RoleDef>),
    Grants(Vec<Grant>),
    /// Sessions; `own` marks every session of this app on the connection.
    Sessions(Vec<SessionInfo>),
    Locks(Vec<LockInfo>),
    Properties(Vec<ServerProperty>),
    DatabaseSizes(Vec<(String, i64)>),
    TableStats(Vec<TableStats>),
}

/// An action on a server session, by its id and (when known) when it started, so an id
/// the server gave to a newer session since is not acted on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminAction {
    /// Cancels the session's running statement.
    CancelSession { id: String, started_ms: Option<i64> },
    /// Ends the session.
    TerminateSession { id: String, started_ms: Option<i64> },
}

fn unsupported() -> DriverError {
    DriverError::new(
        ErrorKind::Unsupported,
        "administration is not supported by this connection",
    )
}

impl Engine {
    /// What the connected driver administers; `None` until connected, or without support.
    pub fn admin_support(&self, conn: &str) -> Option<AdminSupport> {
        self.with_admin(conn, |a| a.supports()).ok()
    }

    /// The user the connection logged in as, once connected.
    pub fn admin_current_user(&self, conn: &str) -> Option<String> {
        self.with_admin(conn, |a| a.current_user()).ok().flatten()
    }

    /// Statements that create (`before` is `None`) or alter a role, from the connected
    /// dialect (no I/O). A password in `after` is written as is: preview `after.masked()`.
    pub fn role_ddl(
        &self,
        conn: &str,
        before: Option<&RoleDef>,
        after: &RoleDef,
    ) -> Result<Vec<String>, DriverError> {
        self.with_admin(conn, |a| a.role_ddl(before, after))
    }

    /// Statements that drop `role` (no I/O).
    pub fn drop_role_ddl(&self, conn: &str, role: &RoleDef) -> Result<Vec<String>, DriverError> {
        self.with_admin(conn, |a| a.drop_role_ddl(role))
    }

    /// `GRANT` and `REVOKE` statements (no I/O); see [`Admin::grant_ddl`].
    pub fn grant_ddl(
        &self,
        conn: &str,
        add: &[Grant],
        revoke: &[Grant],
    ) -> Result<Vec<String>, DriverError> {
        self.with_admin(conn, |a| a.grant_ddl(add, revoke))
    }

    /// Reads `query` on the connection's explorer session as a [`JobKind::AdminSnapshot`]
    /// job: [`JobOutcome::Admin`] or [`JobOutcome::Failed`].
    pub fn admin_snapshot(&self, conn: &str, query: AdminQuery) -> JobId {
        let shared = &self.shared;
        let job = shared.jobs.create(JobKind::AdminSnapshot);
        let id = job.id;
        match shared.live_io(conn) {
            None => shared.jobs.finish(id, JobOutcome::Failed(not_connected())),
            Some(io) => {
                let task = shared
                    .clone()
                    .run_snapshot(job, conn.to_string(), io, query);
                shared.send(Command::job(id, Some(conn.to_string()), task));
            }
        }
        shared.repaint();
        id
    }

    /// Cancels or terminates a server session as a [`JobKind::AdminAction`] job:
    /// [`JobOutcome::Done`] or [`JobOutcome::Failed`]. Refused (`Permission`) while the
    /// explorer session is read-only.
    pub fn admin_action(&self, conn: &str, action: AdminAction) -> JobId {
        let shared = &self.shared;
        let job = shared.jobs.create(JobKind::AdminAction);
        let id = job.id;
        let read_only = {
            let inner = shared.inner.lock();
            inner
                .sessions
                .get(&SessionKey::meta(conn))
                .is_some_and(|s| s.tx.read_only)
        };
        match shared.live_io(conn) {
            _ if read_only => {
                let e = DriverError::new(ErrorKind::Permission, READ_ONLY);
                shared.jobs.finish(id, JobOutcome::Failed(e));
            }
            None => shared.jobs.finish(id, JobOutcome::Failed(not_connected())),
            Some(io) => {
                let task = shared.clone().run_action(job, conn.to_string(), io, action);
                shared.send(Command::job(id, Some(conn.to_string()), task));
            }
        }
        shared.repaint();
        id
    }

    fn with_admin<T>(&self, conn: &str, f: impl FnOnce(&dyn Admin) -> T) -> Result<T, DriverError> {
        let conn = self.meta_conn(conn)?;
        let admin = conn
            .relational()
            .and_then(|rel| rel.admin())
            .ok_or_else(unsupported)?;
        Ok(f(admin))
    }
}

impl Shared {
    async fn run_snapshot(
        self: Arc<Self>,
        job: JobControl,
        conn_id: String,
        io: Arc<SessionIo>,
        query: AdminQuery,
    ) {
        let key = SessionKey::meta(&conn_id);
        let outcome = match self.connected(&key, &io).await {
            None => Err(not_connected()),
            Some(conn) => match conn.relational().and_then(|rel| rel.admin()) {
                None => Err(unsupported()),
                Some(admin) => {
                    // A cancel (the tab closed) stops a long read where the driver can.
                    job.set_cancel(admin.snapshot_cancel());
                    let outcome = snapshot(admin, query).await;
                    job.set_cancel(None);
                    outcome
                }
            },
        };
        let outcome = match outcome {
            Ok(AdminSnapshot::Sessions(mut sessions)) => {
                let own = self.own_session_ids(&conn_id);
                for s in &mut sessions {
                    s.own |= own.contains(&s.id);
                }
                JobOutcome::Admin(AdminSnapshot::Sessions(sessions))
            }
            Ok(snapshot) => JobOutcome::Admin(snapshot),
            Err(e) if e.kind == ErrorKind::Cancelled => JobOutcome::Cancelled,
            Err(e) => {
                self.on_error(&key, &io, &e);
                JobOutcome::Failed(e)
            }
        };
        self.jobs.finish(job.id, outcome);
        self.repaint();
    }

    async fn run_action(
        self: Arc<Self>,
        job: JobControl,
        conn_id: String,
        io: Arc<SessionIo>,
        action: AdminAction,
    ) {
        let key = SessionKey::meta(&conn_id);
        let outcome = match self.connected(&key, &io).await {
            None => Err(not_connected()),
            Some(conn) => match conn.relational().and_then(|rel| rel.admin()) {
                None => Err(unsupported()),
                Some(admin) => match &action {
                    AdminAction::CancelSession { id, started_ms } => {
                        admin.cancel_session(id, *started_ms).await
                    }
                    AdminAction::TerminateSession { id, started_ms } => {
                        admin.terminate_session(id, *started_ms).await
                    }
                },
            },
        };
        let outcome = match outcome {
            Ok(()) => JobOutcome::Done,
            Err(e) => {
                self.on_error(&key, &io, &e);
                JobOutcome::Failed(e)
            }
        };
        self.jobs.finish(job.id, outcome);
        self.repaint();
    }

    /// The server's ids of every session this app holds on the connection: its explorer
    /// and tab sessions, and their connections to other databases.
    fn own_session_ids(&self, conn_id: &str) -> HashSet<String> {
        let inner = self.inner.lock();
        inner
            .sessions
            .iter()
            .filter(|(k, _)| k.conn == conn_id)
            .flat_map(|(_, s)| s.conn.iter().chain(s.databases.values()))
            .filter_map(|c| c.relational()?.admin().map(|a| a.session_ids()))
            .flatten()
            .collect()
    }
}

async fn snapshot(admin: &dyn Admin, query: AdminQuery) -> Result<AdminSnapshot, DriverError> {
    Ok(match query {
        AdminQuery::Roles => AdminSnapshot::Roles(admin.roles().await?),
        AdminQuery::Grants { grantee, object } => {
            AdminSnapshot::Grants(admin.grants(grantee.as_deref(), object.as_ref()).await?)
        }
        AdminQuery::Sessions => AdminSnapshot::Sessions(admin.sessions().await?),
        AdminQuery::Locks => AdminSnapshot::Locks(admin.locks().await?),
        AdminQuery::Properties => AdminSnapshot::Properties(admin.properties().await?),
        AdminQuery::DatabaseSizes => AdminSnapshot::DatabaseSizes(admin.database_sizes().await?),
        AdminQuery::TableStats { schema } => {
            AdminSnapshot::TableStats(admin.table_stats(schema.as_deref()).await?)
        }
    })
}
