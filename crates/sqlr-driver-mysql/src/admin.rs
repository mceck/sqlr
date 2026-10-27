//! Administration for MySQL and MariaDB: accounts (`user@host`) and roles from the `mysql`
//! schema, grants from `mysql.db`, `mysql.tables_priv` and `mysql.procs_priv`, sessions from
//! the process list, locks (MySQL: `performance_schema.data_locks`; MariaDB:
//! `information_schema.INNODB_LOCKS`), variables, database sizes and table statistics.
//! Catalog reads take no user input or pass it as parameters; session ids are parsed as
//! numbers before `KILL`; DDL quotes names through the dialect.
//!
//! MariaDB roles have no host: a role is a [`RoleDef`] without one, and its grantee name is
//! its bare name.

use mysql_async::Conn;
use mysql_async::prelude::Queryable;
use sqlr_core::{
    Admin, AdminSupport, Dialect, DriverError, ErrorKind, Grant, LockInfo, ObjectKind, ObjectRef,
    RoleDef, RoleField, ServerProperty, SessionInfo, TableRef, TableStats, grouped_grants,
    split_grantee,
};

use crate::catalog::{int, rows, string, text};
use crate::connect::MyConnection;
use crate::dialect::MyDialect;
use crate::map_error;

const TABLE_PRIVILEGES: [&str; 12] = [
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "CREATE",
    "DROP",
    "ALTER",
    "INDEX",
    "REFERENCES",
    "TRIGGER",
    "CREATE VIEW",
    "SHOW VIEW",
];

/// `mysql.db` columns and the privileges they hold, in `DB_SQL`'s order (after Host, Db,
/// User and Grant_priv).
const DB_PRIVILEGES: [&str; 18] = [
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "CREATE",
    "DROP",
    "REFERENCES",
    "INDEX",
    "ALTER",
    "CREATE TEMPORARY TABLES",
    "LOCK TABLES",
    "CREATE VIEW",
    "SHOW VIEW",
    "CREATE ROUTINE",
    "ALTER ROUTINE",
    "EXECUTE",
    "EVENT",
    "TRIGGER",
];

const DB_SQL: &str = "\
SELECT Host, Db, User, Grant_priv, Select_priv, Insert_priv, Update_priv, Delete_priv,
       Create_priv, Drop_priv, References_priv, Index_priv, Alter_priv,
       Create_tmp_table_priv, Lock_tables_priv, Create_view_priv, Show_view_priv,
       Create_routine_priv, Alter_routine_priv, Execute_priv, Event_priv, Trigger_priv
FROM mysql.db WHERE 1 = 1";

const TABLES_PRIV_SQL: &str = "\
SELECT Host, Db, User, Table_name, Table_priv FROM mysql.tables_priv WHERE 1 = 1";

const PROCS_PRIV_SQL: &str = "\
SELECT Host, Db, User, Routine_name, Routine_type, Proc_priv FROM mysql.procs_priv
WHERE 1 = 1";

/// `base` (a privilege table query ending in a `WHERE`) narrowed by `conditions`, each a
/// clause with `?` placeholders and its parameters, then ordered by `order`.
fn narrowed(base: &str, conditions: &[(&str, Vec<String>)], order: &str) -> (String, Vec<String>) {
    let mut sql = base.to_string();
    let mut params = Vec::new();
    for (clause, values) in conditions {
        sql.push_str(" AND ");
        sql.push_str(clause);
        params.extend(values.iter().cloned());
    }
    sql.push_str(" ORDER BY ");
    sql.push_str(order);
    (sql, params)
}

fn support() -> AdminSupport {
    let list = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    AdminSupport {
        roles: true,
        grants: true,
        sessions: true,
        locks: true,
        properties: true,
        statistics: true,
        privileges: vec![
            (ObjectKind::Database, list(&DB_PRIVILEGES)),
            (ObjectKind::Table, list(&TABLE_PRIVILEGES)),
            (ObjectKind::View, list(&TABLE_PRIVILEGES)),
            (ObjectKind::Function, list(&["EXECUTE", "ALTER ROUTINE"])),
            (ObjectKind::Procedure, list(&["EXECUTE", "ALTER ROUTINE"])),
        ],
        role_fields: vec![
            RoleField::Login,
            RoleField::ConnectionLimit,
            RoleField::MemberOf,
            RoleField::Password,
        ],
        hosts: true,
    }
}

/// `name@host` as a grantee; a MariaDB role (empty host) is its bare name.
fn grantee(name: &str, host: &str) -> String {
    if host.is_empty() {
        name.to_string()
    } else {
        format!("{name}@{host}")
    }
}

/// An account (`` `name`@`host` ``) or, without a host, a MariaDB role (`` `name` ``).
fn account(d: &MyDialect, name: &str, host: Option<&str>) -> String {
    match host {
        Some(host) => format!("{}@{}", d.quote_ident(name), d.quote_ident(host)),
        None => d.quote_ident(name),
    }
}

/// The account a grantee name stands for.
fn grantee_account(d: &MyDialect, name: &str) -> String {
    let (name, host) = split_grantee(name);
    account(d, name, host)
}

/// A privilege column's `'Y'`, or a JSON flag as MariaDB's `JSON_VALUE` reads it (`1`).
fn yes(value: Option<String>) -> bool {
    value.is_some_and(|v| v.eq_ignore_ascii_case("y") || v.eq_ignore_ascii_case("true") || v == "1")
}

const MYSQL_USERS_SQL: &str = "\
SELECT User, Host, account_locked, max_user_connections, Super_priv, Create_priv,
       Create_user_priv, Repl_slave_priv
FROM mysql.user ORDER BY User, Host";

const MARIADB_USERS_SQL: &str = "\
SELECT u.User, u.Host, JSON_VALUE(g.Priv, '$.account_locked'), u.max_user_connections,
       u.Super_priv, u.Create_priv, u.Create_user_priv, u.Repl_slave_priv, u.is_role
FROM mysql.user u
LEFT JOIN mysql.global_priv g ON g.Host = u.Host AND g.User = u.User
ORDER BY u.User, u.Host";

/// System accounts that only the server uses.
fn internal(name: &str) -> bool {
    name.starts_with("mysql.") || name.starts_with("mariadb.")
}

async fn roles(conn: &mut Conn, mariadb: bool) -> Result<Vec<RoleDef>, DriverError> {
    let users = rows(
        conn,
        if mariadb {
            MARIADB_USERS_SQL
        } else {
            MYSQL_USERS_SQL
        },
        &[],
    )
    .await?;
    // (member grantee, role grantee)
    let edges: Vec<(String, String)> = if mariadb {
        rows(
            conn,
            "SELECT User, Host, Role FROM mysql.roles_mapping",
            &[],
        )
        .await?
        .iter()
        .map(|r| (grantee(&string(r, 0), &string(r, 1)), string(r, 2)))
        .collect()
    } else {
        rows(
            conn,
            "SELECT TO_USER, TO_HOST, FROM_USER, FROM_HOST FROM mysql.role_edges",
            &[],
        )
        .await?
        .iter()
        .map(|r| {
            (
                grantee(&string(r, 0), &string(r, 1)),
                grantee(&string(r, 2), &string(r, 3)),
            )
        })
        .collect()
    };
    let mut out = Vec::new();
    for r in &users {
        let name = string(r, 0);
        let host = string(r, 1);
        if internal(&name) {
            continue;
        }
        let is_role = mariadb && yes(text(r, 8));
        let me = grantee(&name, &host);
        let mut member_of: Vec<String> = edges
            .iter()
            .filter(|(member, _)| *member == me)
            .map(|(_, role)| role.clone())
            .collect();
        member_of.sort();
        out.push(RoleDef {
            login: !is_role && !yes(text(r, 2)),
            connection_limit: int(r, 3)
                .filter(|n| *n > 0)
                .and_then(|n| i32::try_from(n).ok()),
            superuser: yes(text(r, 4)),
            create_db: yes(text(r, 5)),
            create_role: yes(text(r, 6)),
            replication: yes(text(r, 7)),
            inherit: true,
            member_of,
            host: (!host.is_empty()).then_some(host),
            name,
            ..Default::default()
        });
    }
    Ok(out)
}

/// `CREATE USER` (or a MariaDB `CREATE ROLE`) with its role grants, or the `RENAME USER`,
/// `ALTER USER`, `GRANT` and `REVOKE` statements that turn `before` into `after`.
fn role_ddl(d: &MyDialect, before: Option<&RoleDef>, after: &RoleDef) -> Vec<String> {
    let me = account(d, &after.name, after.host.as_deref());
    let mut out = Vec::new();
    let password = after
        .password
        .as_ref()
        .map(|p| format!("IDENTIFIED BY {}", d.comment_literal(p.expose())))
        // Removing the password: an empty one.
        .or_else(|| after.clear_password.then(|| "IDENTIFIED BY ''".to_string()));
    let member_before: &[String] = before.map_or(&[], |b| b.member_of.as_slice());
    match before {
        None if after.host.is_none() => out.push(format!("CREATE ROLE {me}")),
        None => {
            let mut s = format!("CREATE USER {me}");
            if let Some(p) = &password {
                s.push_str(&format!(" {p}"));
            }
            if let Some(n) = after.connection_limit {
                s.push_str(&format!(" WITH MAX_USER_CONNECTIONS {n}"));
            }
            s.push_str(if after.login {
                " ACCOUNT UNLOCK"
            } else {
                " ACCOUNT LOCK"
            });
            out.push(s);
        }
        Some(b) => {
            if b.name != after.name || b.host != after.host {
                out.push(format!(
                    "RENAME USER {} TO {me}",
                    account(d, &b.name, b.host.as_deref())
                ));
            }
            let mut parts = Vec::new();
            parts.extend(password);
            if b.connection_limit != after.connection_limit {
                let n = after.connection_limit.unwrap_or(0);
                parts.push(format!("WITH MAX_USER_CONNECTIONS {n}"));
            }
            if b.login != after.login && after.host.is_some() {
                parts.push(
                    if after.login {
                        "ACCOUNT UNLOCK"
                    } else {
                        "ACCOUNT LOCK"
                    }
                    .into(),
                );
            }
            if !parts.is_empty() {
                out.push(format!("ALTER USER {me} {}", parts.join(" ")));
            }
            for role in member_before
                .iter()
                .filter(|r| !after.member_of.contains(r))
            {
                out.push(format!("REVOKE {} FROM {me}", grantee_account(d, role)));
            }
        }
    }
    for role in after
        .member_of
        .iter()
        .filter(|r| !member_before.contains(r))
    {
        out.push(format!("GRANT {} TO {me}", grantee_account(d, role)));
    }
    out
}

/// A privilege of a `SET` column (`Select`, `Create View`, `Show view`) as a keyword.
fn keyword(privilege: &str) -> String {
    privilege.trim().to_ascii_uppercase()
}

fn object(kind: ObjectKind, db: String, name: String) -> ObjectRef {
    let database = kind == ObjectKind::Database;
    ObjectRef {
        kind,
        database: None,
        schema: (!database).then_some(db),
        name,
        parent: None,
        signature: None,
    }
}

/// Whether `found` is the object `wanted` asks for (tables and views alike: the privilege
/// tables do not tell them apart).
fn same_object(found: &ObjectRef, wanted: &ObjectRef) -> bool {
    let relation = |k: ObjectKind| matches!(k, ObjectKind::Table | ObjectKind::View);
    let kind = found.kind == wanted.kind || (relation(found.kind) && relation(wanted.kind));
    let schema = match wanted.kind {
        ObjectKind::Database | ObjectKind::Schema => true,
        _ => found.schema == wanted.schema,
    };
    kind && schema && found.name == wanted.name
}

async fn grants(
    conn: &mut Conn,
    who: Option<&str>,
    wanted: Option<&ObjectRef>,
) -> Result<Vec<Grant>, DriverError> {
    // The server narrows the privilege tables to the grantee and the object asked for.
    let mut by_grantee = Vec::new();
    if let Some(w) = who {
        let (name, host) = split_grantee(w);
        let account = vec![name.to_string(), host.unwrap_or("").to_string()];
        by_grantee.push(("User = ? AND Host = ?", account));
    }
    let db_of = |o: &ObjectRef| {
        o.schema
            .clone()
            .or_else(|| o.database.clone())
            .unwrap_or_default()
    };
    let read = |kind: &[ObjectKind]| wanted.is_none_or(|w| kind.contains(&w.kind));
    let mut out = Vec::new();
    let mut db_conditions = by_grantee.clone();
    if let Some(w) = wanted {
        db_conditions.push(("Db = ?", vec![w.name.clone()]));
    }
    let (db_sql, db_params) = narrowed(DB_SQL, &db_conditions, "Db, User, Host");
    let db_rows = if read(&[ObjectKind::Database, ObjectKind::Schema]) {
        let params: Vec<&str> = db_params.iter().map(String::as_str).collect();
        rows(conn, &db_sql, &params).await?
    } else {
        Vec::new()
    };
    for r in db_rows {
        let to = grantee(&string(&r, 2), &string(&r, 0));
        let on = object(ObjectKind::Database, string(&r, 1), string(&r, 1));
        let grantable = yes(text(&r, 3));
        for (i, privilege) in DB_PRIVILEGES.iter().enumerate() {
            if yes(text(&r, 4 + i)) {
                out.push(Grant {
                    grantee: to.clone(),
                    object: on.clone(),
                    privilege: privilege.to_string(),
                    grantable,
                });
            }
        }
    }
    let set_grants = |out: &mut Vec<Grant>, to: String, on: ObjectRef, set: String| {
        let privileges: Vec<&str> = set.split(',').filter(|p| !p.trim().is_empty()).collect();
        let grantable = privileges
            .iter()
            .any(|p| p.trim().eq_ignore_ascii_case("grant"));
        for p in privileges
            .iter()
            .filter(|p| !p.trim().eq_ignore_ascii_case("grant"))
        {
            out.push(Grant {
                grantee: to.clone(),
                object: on.clone(),
                privilege: keyword(p),
                grantable,
            });
        }
    };
    if read(&[ObjectKind::Table, ObjectKind::View]) {
        let mut conditions = by_grantee.clone();
        if let Some(w) = wanted {
            conditions.push(("Db = ? AND Table_name = ?", vec![db_of(w), w.name.clone()]));
        }
        let (sql, params) = narrowed(TABLES_PRIV_SQL, &conditions, "Db, Table_name, User, Host");
        let params: Vec<&str> = params.iter().map(String::as_str).collect();
        for r in rows(conn, &sql, &params).await? {
            let to = grantee(&string(&r, 2), &string(&r, 0));
            let on = object(ObjectKind::Table, string(&r, 1), string(&r, 3));
            set_grants(&mut out, to, on, string(&r, 4));
        }
    }
    let mut proc_conditions = by_grantee;
    if let Some(w) = wanted {
        proc_conditions.push((
            "Db = ? AND Routine_name = ?",
            vec![db_of(w), w.name.clone()],
        ));
    }
    let (proc_sql, proc_params) = narrowed(
        PROCS_PRIV_SQL,
        &proc_conditions,
        "Db, Routine_name, User, Host",
    );
    let proc_rows = if read(&[ObjectKind::Function, ObjectKind::Procedure]) {
        let params: Vec<&str> = proc_params.iter().map(String::as_str).collect();
        rows(conn, &proc_sql, &params).await?
    } else {
        Vec::new()
    };
    for r in proc_rows {
        let to = grantee(&string(&r, 2), &string(&r, 0));
        let kind = if string(&r, 4).eq_ignore_ascii_case("procedure") {
            ObjectKind::Procedure
        } else {
            ObjectKind::Function
        };
        let on = object(kind, string(&r, 1), string(&r, 3));
        set_grants(&mut out, to, on, string(&r, 5));
    }
    out.retain(|g| who.is_none_or(|w| g.grantee == w));
    if let Some(wanted) = wanted {
        out.retain(|g| same_object(&g.object, wanted));
        // Answer in the kind asked for (a view's grants read as a table's).
        for g in &mut out {
            g.object.kind = wanted.kind;
        }
    }
    Ok(out)
}

/// What `GRANT … ON` names `object` as; `None` for kinds nothing is granted on.
fn grant_target(d: &MyDialect, object: &ObjectRef) -> Option<String> {
    let db = object.schema.as_deref().or(object.database.as_deref());
    let qualified = || match db {
        Some(db) => format!("{}.{}", d.quote_ident(db), d.quote_ident(&object.name)),
        None => d.quote_ident(&object.name),
    };
    Some(match object.kind {
        ObjectKind::Database | ObjectKind::Schema => format!("{}.*", d.quote_ident(&object.name)),
        ObjectKind::Table | ObjectKind::View => qualified(),
        ObjectKind::Function => format!("FUNCTION {}", qualified()),
        ObjectKind::Procedure => format!("PROCEDURE {}", qualified()),
        _ => return None,
    })
}

/// `GRANT` and `REVOKE` statements. MySQL's grant option belongs to an object, not to one
/// privilege: revoking it for one privilege revokes it for every privilege on the object.
fn grant_ddl(d: &MyDialect, add: &[Grant], revoke: &[Grant]) -> Vec<String> {
    let mut out = Vec::new();
    let groups = grouped_grants(revoke);
    for (object, to, option, privileges) in &groups {
        let Some(target) = grant_target(d, object) else {
            continue;
        };
        let from = grantee_account(d, to);
        // Privileges and the grant option on the same object go in one statement.
        let with_privileges = groups
            .iter()
            .any(|(o, t, opt, _)| !opt && o == object && t == to);
        let statement = match (option, with_privileges) {
            (true, true) => continue,
            (true, false) => format!("REVOKE GRANT OPTION ON {target} FROM {from}"),
            (false, _) => {
                let option_too = groups
                    .iter()
                    .any(|(o, t, opt, _)| *opt && o == object && t == to);
                let option = if option_too { ", GRANT OPTION" } else { "" };
                format!(
                    "REVOKE {}{option} ON {target} FROM {from}",
                    privileges.join(", ")
                )
            }
        };
        if !out.contains(&statement) {
            out.push(statement);
        }
    }
    for (object, to, option, privileges) in grouped_grants(add) {
        let Some(target) = grant_target(d, object) else {
            continue;
        };
        let with = if option { " WITH GRANT OPTION" } else { "" };
        out.push(format!(
            "GRANT {} ON {target} TO {}{with}",
            privileges.join(", "),
            grantee_account(d, to)
        ));
    }
    out
}

const SESSIONS_SQL: &str = "\
SELECT ID, USER, HOST, DB, COMMAND, STATE, INFO,
       CAST(UNIX_TIMESTAMP(NOW(3)) * 1000 - TIME * 1000 AS SIGNED)
FROM information_schema.PROCESSLIST ORDER BY ID";

async fn sessions(conn: &mut Conn, own: &[u32]) -> Result<Vec<SessionInfo>, DriverError> {
    let mut out = Vec::new();
    for r in rows(conn, SESSIONS_SQL, &[]).await? {
        let command = string(&r, 4);
        if command == "Daemon" {
            continue;
        }
        let id = int(&r, 0).unwrap_or_default();
        let state = match command.as_str() {
            "Sleep" => "idle".to_string(),
            "Query" | "Execute" => "active".to_string(),
            other => other.to_ascii_lowercase(),
        };
        let active = state == "active";
        out.push(SessionInfo {
            id: id.to_string(),
            user: string(&r, 1),
            client: text(&r, 2).filter(|h| !h.is_empty()),
            database: text(&r, 3),
            wait: text(&r, 5).filter(|w| !w.is_empty()),
            query: text(&r, 6),
            started_ms: None,
            query_started_ms: int(&r, 7).filter(|_| active),
            own: own.iter().any(|o| i64::from(*o) == id),
            state,
        });
    }
    Ok(out)
}

const MYSQL_LOCKS_SQL: &str = "\
SELECT t.PROCESSLIST_ID, l.LOCK_TYPE, l.LOCK_MODE, l.OBJECT_SCHEMA, l.OBJECT_NAME,
       l.LOCK_STATUS,
       (SELECT GROUP_CONCAT(bt.PROCESSLIST_ID)
        FROM performance_schema.data_lock_waits w
        JOIN performance_schema.threads bt ON bt.THREAD_ID = w.BLOCKING_THREAD_ID
        WHERE w.REQUESTING_ENGINE_LOCK_ID = l.ENGINE_LOCK_ID AND w.ENGINE = l.ENGINE)
FROM performance_schema.data_locks l
JOIN performance_schema.threads t ON t.THREAD_ID = l.THREAD_ID
WHERE t.PROCESSLIST_ID IS NOT NULL AND t.PROCESSLIST_ID <> CONNECTION_ID()
ORDER BY t.PROCESSLIST_ID";

const MARIADB_LOCKS_SQL: &str = "\
SELECT r.trx_mysql_thread_id, l.lock_type, l.lock_mode, l.lock_table, NULL,
       NULL,
       (SELECT GROUP_CONCAT(b.trx_mysql_thread_id)
        FROM information_schema.INNODB_LOCK_WAITS w
        JOIN information_schema.INNODB_TRX b ON b.trx_id = w.blocking_trx_id
        WHERE w.requested_lock_id = l.lock_id)
FROM information_schema.INNODB_LOCKS l
JOIN information_schema.INNODB_TRX r ON r.trx_id = l.lock_trx_id
ORDER BY r.trx_mysql_thread_id";

async fn locks(conn: &mut Conn, mariadb: bool) -> Result<Vec<LockInfo>, DriverError> {
    let sql = if mariadb {
        MARIADB_LOCKS_SQL
    } else {
        MYSQL_LOCKS_SQL
    };
    Ok(rows(conn, sql, &[])
        .await?
        .iter()
        .map(|r| {
            let blocked_by: Vec<String> = text(r, 6)
                .map(|ids| ids.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            let object = match (text(r, 3), text(r, 4)) {
                (Some(schema), Some(name)) => Some(format!("{schema}.{name}")),
                (one, other) => one.or(other),
            };
            let granted = match text(r, 5) {
                Some(status) => status == "GRANTED",
                None => blocked_by.is_empty(),
            };
            LockInfo {
                session: string(r, 0),
                blocked_by: if granted { Vec::new() } else { blocked_by },
                mode: format!("{} {}", string(r, 1), string(r, 2))
                    .trim()
                    .to_string(),
                object,
                granted,
            }
        })
        .collect())
}

/// `id` as a process list id.
fn thread_id(id: &str) -> Result<u64, DriverError> {
    id.trim()
        .parse()
        .map_err(|_| DriverError::new(ErrorKind::Other, format!("\"{id}\" is not a session id")))
}

async fn properties(conn: &mut Conn) -> Result<Vec<ServerProperty>, DriverError> {
    Ok(rows(conn, "SHOW GLOBAL VARIABLES", &[])
        .await?
        .iter()
        .map(|r| {
            let name = string(r, 0);
            let category = name.split('_').next().unwrap_or(&name).to_string();
            ServerProperty {
                value: string(r, 1),
                category: Some(category),
                source: Some("global".into()),
                name,
                ..Default::default()
            }
        })
        .collect())
}

async fn database_sizes(conn: &mut Conn) -> Result<Vec<(String, i64)>, DriverError> {
    Ok(rows(
        conn,
        "SELECT s.SCHEMA_NAME, CAST(COALESCE(SUM(t.DATA_LENGTH + t.INDEX_LENGTH), 0) AS SIGNED) \
         FROM information_schema.SCHEMATA s \
         LEFT JOIN information_schema.TABLES t ON t.TABLE_SCHEMA = s.SCHEMA_NAME \
         GROUP BY s.SCHEMA_NAME ORDER BY 1",
        &[],
    )
    .await?
    .iter()
    .map(|r| (string(r, 0), int(r, 1).unwrap_or_default()))
    .collect())
}

const STATS_COLUMNS: &str = "\
SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_ROWS, DATA_LENGTH, INDEX_LENGTH, DATA_FREE, ENGINE,
       AUTO_INCREMENT, TABLE_COLLATION, UPDATE_TIME
FROM information_schema.TABLES
WHERE TABLE_TYPE = 'BASE TABLE'";

async fn table_stats(
    conn: &mut Conn,
    schema: Option<&str>,
) -> Result<Vec<TableStats>, DriverError> {
    let found = match schema {
        Some(schema) => {
            let sql = format!("{STATS_COLUMNS} AND TABLE_SCHEMA = ? ORDER BY 1, 2");
            rows(conn, &sql, &[schema]).await?
        }
        None => {
            let sql = format!(
                "{STATS_COLUMNS} AND TABLE_SCHEMA NOT IN \
                 ('mysql', 'information_schema', 'performance_schema', 'sys') ORDER BY 1, 2"
            );
            rows(conn, &sql, &[]).await?
        }
    };
    Ok(found
        .iter()
        .map(|r| {
            let (data, index) = (int(r, 3), int(r, 4));
            let mut extra = Vec::new();
            let fields = [
                (6, "Engine"),
                (5, "Free space (bytes)"),
                (7, "Auto increment"),
                (8, "Collation"),
                (9, "Last update"),
            ];
            for (i, label) in fields {
                if let Some(v) = text(r, i).filter(|v| !v.is_empty()) {
                    extra.push((label.to_string(), v));
                }
            }
            TableStats {
                table: TableRef {
                    database: None,
                    schema: Some(string(r, 0)),
                    name: string(r, 1),
                },
                rows_estimate: int(r, 2),
                total_bytes: data.zip(index).map(|(d, i)| d + i).or(data),
                table_bytes: data,
                index_bytes: index,
                extra,
            }
        })
        .collect())
}

#[async_trait::async_trait]
impl Admin for MyConnection {
    fn supports(&self) -> AdminSupport {
        support()
    }

    fn current_user(&self) -> Option<String> {
        self.opts.user().map(String::from)
    }

    fn session_ids(&self) -> Vec<String> {
        self.own_ids().iter().map(u32::to_string).collect()
    }

    async fn roles(&self) -> Result<Vec<RoleDef>, DriverError> {
        let mut meta = self.meta.lock().await;
        roles(&mut meta, self.dialect.mariadb).await
    }

    async fn grants(
        &self,
        grantee: Option<&str>,
        object: Option<&ObjectRef>,
    ) -> Result<Vec<Grant>, DriverError> {
        let mut meta = self.meta.lock().await;
        grants(&mut meta, grantee, object).await
    }

    fn role_ddl(&self, before: Option<&RoleDef>, after: &RoleDef) -> Vec<String> {
        role_ddl(&self.dialect, before, after)
    }

    fn drop_role_ddl(&self, role: &RoleDef) -> Vec<String> {
        let verb = if role.host.is_some() { "USER" } else { "ROLE" };
        vec![format!(
            "DROP {verb} {}",
            account(&self.dialect, &role.name, role.host.as_deref())
        )]
    }

    fn grant_ddl(&self, add: &[Grant], revoke: &[Grant]) -> Vec<String> {
        grant_ddl(&self.dialect, add, revoke)
    }

    async fn sessions(&self) -> Result<Vec<SessionInfo>, DriverError> {
        let mut meta = self.meta.lock().await;
        let own = self.own_ids();
        sessions(&mut meta, &own).await
    }

    async fn locks(&self) -> Result<Vec<LockInfo>, DriverError> {
        let mut meta = self.meta.lock().await;
        locks(&mut meta, self.dialect.mariadb).await
    }

    /// MySQL never gives a process list id to another connection while the server runs,
    /// so `started_ms` needs no check.
    async fn cancel_session(&self, id: &str, _started_ms: Option<i64>) -> Result<(), DriverError> {
        let id = thread_id(id)?;
        let mut meta = self.meta.lock().await;
        meta.query_drop(format!("KILL QUERY {id}"))
            .await
            .map_err(map_error)
    }

    async fn terminate_session(
        &self,
        id: &str,
        _started_ms: Option<i64>,
    ) -> Result<(), DriverError> {
        let id = thread_id(id)?;
        let mut meta = self.meta.lock().await;
        meta.query_drop(format!("KILL {id}"))
            .await
            .map_err(map_error)
    }

    async fn properties(&self) -> Result<Vec<ServerProperty>, DriverError> {
        let mut meta = self.meta.lock().await;
        properties(&mut meta).await
    }

    async fn database_sizes(&self) -> Result<Vec<(String, i64)>, DriverError> {
        let mut meta = self.meta.lock().await;
        database_sizes(&mut meta).await
    }

    async fn table_stats(&self, schema: Option<&str>) -> Result<Vec<TableStats>, DriverError> {
        let mut meta = self.meta.lock().await;
        table_stats(&mut meta, schema).await
    }
}

#[cfg(test)]
mod tests {
    use sqlr_core::Password;

    use super::*;

    #[test]
    fn accounts_are_quoted_and_roles_have_no_host() {
        let d = MyDialect::default();
        let user = RoleDef {
            name: "a`b".into(),
            host: Some("%".into()),
            login: true,
            connection_limit: Some(3),
            member_of: vec!["reader@%".into(), "plain".into()],
            password: Some(Password::new("it's \\ x")),
            ..Default::default()
        };
        assert_eq!(
            role_ddl(&d, None, &user),
            [
                "CREATE USER `a``b`@`%` IDENTIFIED BY 'it''s \\\\ x' \
                 WITH MAX_USER_CONNECTIONS 3 ACCOUNT UNLOCK",
                "GRANT `reader`@`%` TO `a``b`@`%`",
                "GRANT `plain` TO `a``b`@`%`",
            ]
        );
        let loaded = RoleDef {
            password: None,
            ..user
        };
        let after = RoleDef {
            name: "c".into(),
            login: false,
            connection_limit: None,
            member_of: vec!["plain".into()],
            ..loaded.clone()
        };
        assert_eq!(
            role_ddl(&d, Some(&loaded), &after),
            [
                "RENAME USER `a``b`@`%` TO `c`@`%`",
                "ALTER USER `c`@`%` WITH MAX_USER_CONNECTIONS 0 ACCOUNT LOCK",
                "REVOKE `reader`@`%` FROM `c`@`%`",
            ]
        );
        let role = RoleDef {
            name: "r".into(),
            ..Default::default()
        };
        assert_eq!(role_ddl(&d, None, &role), ["CREATE ROLE `r`"]);
    }

    #[test]
    fn passwords_follow_the_sessions_backslash_mode() {
        let user = RoleDef {
            name: "u".into(),
            host: Some("%".into()),
            login: true,
            password: Some(Password::new("a\\b'c")),
            ..Default::default()
        };
        let plain = MyDialect {
            no_backslash_escapes: true,
            ..MyDialect::default()
        };
        assert_eq!(
            role_ddl(&plain, None, &user),
            ["CREATE USER `u`@`%` IDENTIFIED BY 'a\\b''c' ACCOUNT UNLOCK"]
        );
        assert_eq!(
            role_ddl(&MyDialect::default(), None, &user),
            ["CREATE USER `u`@`%` IDENTIFIED BY 'a\\\\b''c' ACCOUNT UNLOCK"]
        );
    }

    #[test]
    fn grant_option_is_revoked_per_object() {
        let d = MyDialect::default();
        let table = ObjectRef {
            kind: ObjectKind::Table,
            database: None,
            schema: Some("shop".into()),
            name: "t".into(),
            parent: None,
            signature: None,
        };
        let g = |privilege: &str, grantable| Grant {
            grantee: "clerk@%".into(),
            object: table.clone(),
            privilege: privilege.into(),
            grantable,
        };
        assert_eq!(
            grant_ddl(&d, &[g("SELECT", false), g("SHOW VIEW", false)], &[]),
            ["GRANT SELECT, SHOW VIEW ON `shop`.`t` TO `clerk`@`%`"]
        );
        assert_eq!(
            grant_ddl(&d, &[], &[g("SELECT", true), g("UPDATE", true)]),
            ["REVOKE GRANT OPTION ON `shop`.`t` FROM `clerk`@`%`"]
        );
        // The last grantable privileges go, and the object's grant option with them.
        assert_eq!(
            grant_ddl(
                &d,
                &[],
                &[g("SELECT", false), g("UPDATE", false), g("UPDATE", true)]
            ),
            ["REVOKE SELECT, UPDATE, GRANT OPTION ON `shop`.`t` FROM `clerk`@`%`"]
        );
        let db = ObjectRef {
            kind: ObjectKind::Database,
            schema: None,
            name: "shop".into(),
            ..table.clone()
        };
        let insert = Grant {
            object: db,
            ..g("INSERT", false)
        };
        assert_eq!(
            grant_ddl(&d, &[insert], &[]),
            ["GRANT INSERT ON `shop`.* TO `clerk`@`%`"]
        );
    }
}
