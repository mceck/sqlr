//! Server administration: users and roles, grants, sessions, locks, server properties and
//! statistics (spec §8.4). Declared here so drivers implement one trait; every method has
//! a default that reports the feature as unsupported.

use std::fmt;

use crate::error::{DriverError, ErrorKind};
use crate::schema::{ObjectKind, ObjectRef};
use crate::table::TableRef;

/// What previews, reports and confirmations show in place of a password.
pub const PASSWORD_MASK: &str = "********";

/// A password typed into a form. `Debug` never prints it, and nothing serializes it: it
/// lives in memory until the statement that sets it runs.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The mask previews show ([`PASSWORD_MASK`]).
    pub fn masked() -> Self {
        Self(PASSWORD_MASK.to_string())
    }

    /// The password itself, for the statement that sets it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(\"********\")")
    }
}

/// A role or user. `host` is MySQL's `'user'@'host'` part.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleDef {
    pub name: String,
    pub login: bool,
    pub superuser: bool,
    pub create_db: bool,
    pub create_role: bool,
    pub inherit: bool,
    pub replication: bool,
    pub connection_limit: Option<i32>,
    pub valid_until: Option<String>,
    /// The roles this one is a member of, as their [`RoleDef::grantee`] names.
    pub member_of: Vec<String>,
    pub host: Option<String>,
    pub options: Vec<(String, String)>,
    /// A new password: set only to change it, never loaded from the server.
    pub password: Option<Password>,
    /// Remove the role's password (PostgreSQL `PASSWORD NULL`); never loaded.
    pub clear_password: bool,
}

impl RoleDef {
    /// How grants and memberships name this role: its name, or `name@host` for a MySQL
    /// account.
    pub fn grantee(&self) -> String {
        match &self.host {
            Some(host) => format!("{}@{host}", self.name),
            None => self.name.clone(),
        }
    }

    /// A role the server defines for itself (PostgreSQL's `pg_*`, flagged by the driver
    /// with the option `predefined`): roles can join it, but it is not managed.
    pub fn is_predefined(&self) -> bool {
        self.options
            .iter()
            .any(|(k, v)| k == "predefined" && v == "true")
    }

    /// The role with its new password (if any) replaced by [`PASSWORD_MASK`], for previews.
    pub fn masked(&self) -> RoleDef {
        RoleDef {
            password: self.password.as_ref().map(|_| Password::masked()),
            ..self.clone()
        }
    }
}

/// `grantee` split into a name and a host at its last `@` (MySQL accounts); no host when
/// it has none.
pub fn split_grantee(grantee: &str) -> (&str, Option<&str>) {
    match grantee.rsplit_once('@') {
        Some((name, host)) => (name, Some(host)),
        None => (grantee, None),
    }
}

/// One privilege of `grantee` on `object`. In [`Admin::grant_ddl`]'s `revoke` list,
/// `grantable` revokes only the right to grant it on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Grant {
    pub grantee: String,
    pub object: ObjectRef,
    pub privilege: String,
    pub grantable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub user: String,
    pub database: Option<String>,
    pub client: Option<String>,
    pub state: String,
    pub wait: Option<String>,
    pub query: Option<String>,
    pub started_ms: Option<i64>,
    pub query_started_ms: Option<i64>,
    /// One of this app's own sessions on the connection.
    pub own: bool,
}

impl SessionInfo {
    /// The session runs nothing (`idle`; MySQL's `Sleep`).
    pub fn is_idle(&self) -> bool {
        self.state == "idle"
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LockInfo {
    pub session: String,
    /// The sessions this lock waits for (empty for a granted lock).
    pub blocked_by: Vec<String>,
    pub mode: String,
    pub object: Option<String>,
    pub granted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerProperty {
    pub name: String,
    pub value: String,
    pub unit: Option<String>,
    pub category: Option<String>,
    pub description: Option<String>,
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableStats {
    pub table: TableRef,
    pub rows_estimate: Option<i64>,
    pub total_bytes: Option<i64>,
    pub table_bytes: Option<i64>,
    pub index_bytes: Option<i64>,
    /// Last vacuum/analyze, scans, fragmentation…
    pub extra: Vec<(String, String)>,
}

/// The role attributes a driver reads and writes; the role form shows only these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoleField {
    Login,
    Superuser,
    CreateDb,
    CreateRole,
    Inherit,
    Replication,
    ConnectionLimit,
    ValidUntil,
    MemberOf,
    Password,
}

/// What a driver's [`Admin`] implements; panels the driver does not support stay hidden.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdminSupport {
    pub roles: bool,
    pub grants: bool,
    pub sessions: bool,
    pub locks: bool,
    pub properties: bool,
    pub statistics: bool,
    /// The privileges that can be granted on each object kind.
    pub privileges: Vec<(ObjectKind, Vec<String>)>,
    /// The role attributes the role form edits.
    pub role_fields: Vec<RoleField>,
    /// Roles are `user@host` accounts (MySQL): the form asks for the host.
    pub hosts: bool,
}

impl AdminSupport {
    /// The privileges of `kind`; empty when none can be granted on it.
    pub fn privileges_of(&self, kind: ObjectKind) -> &[String] {
        self.privileges
            .iter()
            .find(|(k, _)| *k == kind)
            .map_or(&[], |(_, p)| p.as_slice())
    }
}

fn unsupported(what: &str) -> DriverError {
    DriverError::new(ErrorKind::Unsupported, format!("{what} are not supported"))
}

#[async_trait::async_trait]
pub trait Admin: Send + Sync {
    fn supports(&self) -> AdminSupport {
        AdminSupport::default()
    }
    /// The user this connection logged in as; dropping it asks twice.
    fn current_user(&self) -> Option<String> {
        None
    }
    /// The server's ids of this connection's own sessions (sessions it marks `own`).
    fn session_ids(&self) -> Vec<String> {
        Vec::new()
    }
    /// What stops a running snapshot early (a long statistics read), when the driver can.
    fn snapshot_cancel(&self) -> Option<std::sync::Arc<dyn crate::driver::CancelHandle>> {
        None
    }
    async fn roles(&self) -> Result<Vec<RoleDef>, DriverError> {
        Err(unsupported("roles"))
    }
    async fn grants(
        &self,
        _grantee: Option<&str>,
        _object: Option<&ObjectRef>,
    ) -> Result<Vec<Grant>, DriverError> {
        Err(unsupported("grants"))
    }
    /// Statements that create (`before` is `None`) or alter a role.
    fn role_ddl(&self, _before: Option<&RoleDef>, _after: &RoleDef) -> Vec<String> {
        Vec::new()
    }
    /// Statements that drop `role`.
    fn drop_role_ddl(&self, _role: &RoleDef) -> Vec<String> {
        Vec::new()
    }
    fn grant_ddl(&self, _add: &[Grant], _revoke: &[Grant]) -> Vec<String> {
        Vec::new()
    }
    async fn sessions(&self) -> Result<Vec<SessionInfo>, DriverError> {
        Err(unsupported("sessions"))
    }
    async fn locks(&self) -> Result<Vec<LockInfo>, DriverError> {
        Err(unsupported("locks"))
    }
    /// Cancels the running query of session `id` (`pg_cancel_backend`, `KILL QUERY`).
    /// With `started_ms` (its [`SessionInfo::started_ms`]), a session with that id that
    /// started at another time (a reused id) is left alone and the call fails.
    async fn cancel_session(&self, _id: &str, _started_ms: Option<i64>) -> Result<(), DriverError> {
        Err(unsupported("sessions"))
    }
    /// Ends session `id` (`pg_terminate_backend`, `KILL`); `started_ms` as for
    /// [`Admin::cancel_session`].
    async fn terminate_session(
        &self,
        _id: &str,
        _started_ms: Option<i64>,
    ) -> Result<(), DriverError> {
        Err(unsupported("sessions"))
    }
    async fn properties(&self) -> Result<Vec<ServerProperty>, DriverError> {
        Err(unsupported("server properties"))
    }
    async fn database_sizes(&self) -> Result<Vec<(String, i64)>, DriverError> {
        Err(unsupported("database sizes"))
    }
    async fn table_stats(&self, _schema: Option<&str>) -> Result<Vec<TableStats>, DriverError> {
        Err(unsupported("table statistics"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_never_show_in_debug() {
        let role = RoleDef {
            name: "app".into(),
            password: Some(Password::new("hunter2")),
            ..Default::default()
        };
        let printed = format!("{role:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert_eq!(role.password.as_ref().unwrap().expose(), "hunter2");
        assert_eq!(
            role.masked().password.unwrap().expose(),
            PASSWORD_MASK,
            "previews show the mask"
        );
        assert_eq!(RoleDef::default().masked().password, None);
    }

    #[test]
    fn grantees_name_mysql_accounts_with_their_host() {
        let mut role = RoleDef {
            name: "a@b".into(),
            ..Default::default()
        };
        assert_eq!(role.grantee(), "a@b");
        role.host = Some("%".into());
        assert_eq!(role.grantee(), "a@b@%");
        assert_eq!(split_grantee("a@b@%"), ("a@b", Some("%")));
        assert_eq!(split_grantee("plain"), ("plain", None));
    }
}
