//! Administration for PostgreSQL: roles from `pg_roles`, grants from the catalogs' ACLs
//! (owners' implicit privileges included), sessions from `pg_stat_activity`, locks from
//! `pg_locks` with `pg_blocking_pids`, settings, database sizes and table statistics. Every
//! query runs on the explorer client and is parameterized; DDL quotes identifiers through
//! the dialect.

use sqlr_core::{
    Admin, AdminSupport, Dialect, DriverError, ErrorKind, Grant, LockInfo, ObjectKind, ObjectRef,
    RoleDef, RoleField, ServerProperty, SessionInfo, TableRef, TableStats,
};
use tokio_postgres::Client;

use crate::connect::PgConnection;
use crate::dialect::PgDialect;
use crate::map_error;

const TABLE_PRIVILEGES: [&str; 7] = [
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "TRUNCATE",
    "REFERENCES",
    "TRIGGER",
];

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
            (ObjectKind::Table, list(&TABLE_PRIVILEGES)),
            (ObjectKind::View, list(&TABLE_PRIVILEGES)),
            (ObjectKind::MaterializedView, list(&["SELECT"])),
            (ObjectKind::Sequence, list(&["USAGE", "SELECT", "UPDATE"])),
            (ObjectKind::Function, list(&["EXECUTE"])),
            (ObjectKind::Procedure, list(&["EXECUTE"])),
            (ObjectKind::Schema, list(&["USAGE", "CREATE"])),
            (
                ObjectKind::Database,
                list(&["CONNECT", "CREATE", "TEMPORARY"]),
            ),
        ],
        role_fields: vec![
            RoleField::Login,
            RoleField::Superuser,
            RoleField::CreateDb,
            RoleField::CreateRole,
            RoleField::Inherit,
            RoleField::Replication,
            RoleField::ConnectionLimit,
            RoleField::ValidUntil,
            RoleField::MemberOf,
            RoleField::Password,
        ],
        hosts: false,
    }
}

const ROLES_SQL: &str = "\
SELECT r.rolname::text, r.rolcanlogin, r.rolsuper, r.rolcreatedb, r.rolcreaterole,
       r.rolinherit, r.rolreplication, r.rolconnlimit,
       CASE WHEN r.rolvaliduntil IS NULL OR r.rolvaliduntil = 'infinity' THEN NULL
            ELSE r.rolvaliduntil::text END,
       ARRAY(SELECT b.rolname::text FROM pg_auth_members m
             JOIN pg_roles b ON b.oid = m.roleid
             WHERE m.member = r.oid ORDER BY 1),
       r.rolname ~ '^pg_'
FROM pg_roles r
ORDER BY 1";

async fn roles(client: &Client) -> Result<Vec<RoleDef>, DriverError> {
    let rows = client.query(ROLES_SQL, &[]).await.map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| RoleDef {
            name: r.get(0),
            login: r.get(1),
            superuser: r.get(2),
            create_db: r.get(3),
            create_role: r.get(4),
            inherit: r.get(5),
            replication: r.get(6),
            connection_limit: Some(r.get::<_, i32>(7)).filter(|n| *n >= 0),
            valid_until: r.get(8),
            member_of: r.get(9),
            // Predefined roles: joinable, not managed.
            options: if r.get(10) {
                vec![("predefined".into(), "true".into())]
            } else {
                Vec::new()
            },
            ..Default::default()
        })
        .collect())
}

fn flag(on: bool, word: &str) -> String {
    if on {
        word.to_string()
    } else {
        format!("NO{word}")
    }
}

/// `CREATE ROLE` (with its memberships) or the `ALTER ROLE`, `GRANT` and `REVOKE`
/// statements that turn `before` into `after`.
fn role_ddl(d: &PgDialect, before: Option<&RoleDef>, after: &RoleDef) -> Vec<String> {
    let q = |name: &str| d.quote_ident(name);
    let name = q(&after.name);
    let mut out = Vec::new();
    let mut attributes = Vec::new();
    let bools = [
        (after.login, before.map(|b| b.login), "LOGIN"),
        (after.superuser, before.map(|b| b.superuser), "SUPERUSER"),
        (after.create_db, before.map(|b| b.create_db), "CREATEDB"),
        (
            after.create_role,
            before.map(|b| b.create_role),
            "CREATEROLE",
        ),
        (after.inherit, before.map(|b| b.inherit), "INHERIT"),
        (
            after.replication,
            before.map(|b| b.replication),
            "REPLICATION",
        ),
    ];
    for (on, was, word) in bools {
        if was != Some(on) {
            attributes.push(flag(on, word));
        }
    }
    if before.is_none_or(|b| b.connection_limit != after.connection_limit)
        && (before.is_some() || after.connection_limit.is_some())
    {
        let limit = after.connection_limit.unwrap_or(-1);
        attributes.push(format!("CONNECTION LIMIT {limit}"));
    }
    if let Some(password) = &after.password {
        attributes.push(format!("PASSWORD {}", d.string_literal(password.expose())));
    } else if after.clear_password {
        attributes.push("PASSWORD NULL".into());
    }
    if before.is_none_or(|b| b.valid_until != after.valid_until) {
        match &after.valid_until {
            Some(until) => attributes.push(format!("VALID UNTIL {}", d.string_literal(until))),
            None if before.is_some() => attributes.push("VALID UNTIL 'infinity'".into()),
            None => {}
        }
    }
    let member_before: &[String] = before.map_or(&[], |b| b.member_of.as_slice());
    match before {
        None => out.push(format!("CREATE ROLE {name} WITH {}", attributes.join(" "))),
        Some(b) => {
            if b.name != after.name {
                out.push(format!("ALTER ROLE {} RENAME TO {name}", q(&b.name)));
            }
            if !attributes.is_empty() {
                out.push(format!("ALTER ROLE {name} WITH {}", attributes.join(" ")));
            }
            for role in member_before
                .iter()
                .filter(|r| !after.member_of.contains(r))
            {
                out.push(format!("REVOKE {} FROM {name}", q(role)));
            }
        }
    }
    for role in after
        .member_of
        .iter()
        .filter(|r| !member_before.contains(r))
    {
        out.push(format!("GRANT {} TO {name}", q(role)));
    }
    out
}

const GRANTS_SQL: &str = "\
WITH objects AS (
  SELECT CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'matview'
                        WHEN 'S' THEN 'sequence' ELSE 'table' END AS kind,
         n.nspname::text AS schema, c.relname::text AS name, NULL::text AS signature,
         COALESCE(c.relacl, acldefault(CASE WHEN c.relkind = 'S' THEN 's'::\"char\"
                                            ELSE 'r'::\"char\" END, c.relowner)) AS acl
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
  WHERE c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
    AND n.nspname NOT IN ('pg_catalog', 'information_schema')
    AND n.nspname NOT LIKE 'pg\\_%'
  UNION ALL
  SELECT CASE p.prokind WHEN 'p' THEN 'procedure' ELSE 'function' END,
         n.nspname::text, p.proname::text, pg_get_function_identity_arguments(p.oid),
         COALESCE(p.proacl, acldefault('f'::\"char\", p.proowner))
  FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
  WHERE p.prokind IN ('f', 'p')
    AND n.nspname NOT IN ('pg_catalog', 'information_schema')
    AND n.nspname NOT LIKE 'pg\\_%'
  UNION ALL
  SELECT 'schema', NULL, n.nspname::text, NULL,
         COALESCE(n.nspacl, acldefault('n'::\"char\", n.nspowner))
  FROM pg_namespace n
  WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
    AND n.nspname NOT LIKE 'pg\\_%'
  UNION ALL
  SELECT 'database', NULL, d.datname::text, NULL,
         COALESCE(d.datacl, acldefault('d'::\"char\", d.datdba))
  FROM pg_database d
  WHERE NOT d.datistemplate
)
SELECT o.kind, o.schema, o.name, o.signature, COALESCE(g.rolname::text, 'PUBLIC'),
       a.privilege_type, a.is_grantable
FROM objects o
CROSS JOIN LATERAL aclexplode(o.acl) a
LEFT JOIN pg_roles g ON g.oid = a.grantee
WHERE ($1::text IS NULL OR COALESCE(g.rolname::text, 'PUBLIC') = $1)
  AND ($2::text IS NULL OR o.kind = $2)
  AND ($3::text IS NULL OR o.schema = $3)
  AND ($4::text IS NULL OR o.name = $4)
  AND ($5::text IS NULL OR o.signature = $5)
ORDER BY o.schema NULLS FIRST, o.name, o.kind, 5, 6";

/// The `kind` column of [`GRANTS_SQL`] for an object kind; `None` for kinds nothing is
/// granted on.
fn kind_word(kind: ObjectKind) -> Option<&'static str> {
    Some(match kind {
        ObjectKind::Table => "table",
        ObjectKind::View => "view",
        ObjectKind::MaterializedView => "matview",
        ObjectKind::Sequence => "sequence",
        ObjectKind::Function => "function",
        ObjectKind::Procedure => "procedure",
        ObjectKind::Schema => "schema",
        ObjectKind::Database => "database",
        ObjectKind::Index | ObjectKind::Trigger | ObjectKind::Role => return None,
    })
}

fn kind_of(word: &str) -> ObjectKind {
    match word {
        "view" => ObjectKind::View,
        "matview" => ObjectKind::MaterializedView,
        "sequence" => ObjectKind::Sequence,
        "function" => ObjectKind::Function,
        "procedure" => ObjectKind::Procedure,
        "schema" => ObjectKind::Schema,
        "database" => ObjectKind::Database,
        _ => ObjectKind::Table,
    }
}

async fn grants(
    client: &Client,
    grantee: Option<&str>,
    object: Option<&ObjectRef>,
) -> Result<Vec<Grant>, DriverError> {
    let kind = match object {
        Some(o) => match kind_word(o.kind) {
            Some(word) => Some(word),
            None => return Ok(Vec::new()),
        },
        None => None,
    };
    let schema = object.and_then(|o| o.schema.as_deref());
    let name = object.map(|o| o.name.as_str());
    let signature = object.and_then(|o| o.signature.as_deref());
    let rows = client
        .query(GRANTS_SQL, &[&grantee, &kind, &schema, &name, &signature])
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| Grant {
            object: ObjectRef {
                kind: kind_of(r.get(0)),
                database: None,
                schema: r.get(1),
                name: r.get(2),
                parent: None,
                signature: r.get(3),
            },
            grantee: r.get(4),
            privilege: r.get(5),
            grantable: r.get(6),
        })
        .collect())
}

/// What `GRANT … ON` names `object` as; `None` for kinds nothing is granted on.
fn grant_target(d: &PgDialect, object: &ObjectRef) -> Option<String> {
    let qualified = || d.qualified(&object.table_ref());
    let routine = |word: &str| {
        let args = object.signature.as_deref().unwrap_or("");
        format!("{word} {}({args})", qualified())
    };
    Some(match object.kind {
        ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => {
            format!("TABLE {}", qualified())
        }
        ObjectKind::Sequence => format!("SEQUENCE {}", qualified()),
        ObjectKind::Function => routine("FUNCTION"),
        ObjectKind::Procedure => routine("PROCEDURE"),
        ObjectKind::Schema => format!("SCHEMA {}", d.quote_ident(&object.name)),
        ObjectKind::Database => format!("DATABASE {}", d.quote_ident(&object.name)),
        ObjectKind::Index | ObjectKind::Trigger | ObjectKind::Role => return None,
    })
}

/// A privilege keyword: uppercase words only, so nothing else reaches the statement.
fn privilege_word(privilege: &str) -> bool {
    !privilege.is_empty()
        && privilege
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == ' ')
}

/// `grants` grouped by object, grantee and grant option, in order of first appearance.
fn grouped(grants: &[Grant]) -> Vec<(&ObjectRef, &str, bool, Vec<&str>)> {
    let mut out: Vec<(&ObjectRef, &str, bool, Vec<&str>)> = Vec::new();
    for g in grants.iter().filter(|g| privilege_word(&g.privilege)) {
        match out
            .iter_mut()
            .find(|(o, e, a, _)| **o == g.object && *e == g.grantee && *a == g.grantable)
        {
            Some((_, _, _, privileges)) => privileges.push(&g.privilege),
            None => out.push((&g.object, &g.grantee, g.grantable, vec![&g.privilege])),
        }
    }
    out
}

fn grant_ddl(d: &PgDialect, add: &[Grant], revoke: &[Grant]) -> Vec<String> {
    let grantee = |name: &str| {
        if name == "PUBLIC" {
            "PUBLIC".to_string()
        } else {
            d.quote_ident(name)
        }
    };
    // Revoking a privilege revokes its grant option too: no separate option revoke.
    let revoke: Vec<Grant> = revoke
        .iter()
        .filter(|g| {
            !g.grantable
                || !revoke.iter().any(|r| {
                    !r.grantable
                        && r.object == g.object
                        && r.grantee == g.grantee
                        && r.privilege == g.privilege
                })
        })
        .cloned()
        .collect();
    let mut out = Vec::new();
    for (object, to, option, privileges) in grouped(&revoke) {
        let Some(target) = grant_target(d, object) else {
            continue;
        };
        let what = if option { "GRANT OPTION FOR " } else { "" };
        out.push(format!(
            "REVOKE {what}{} ON {target} FROM {}",
            privileges.join(", "),
            grantee(to)
        ));
    }
    for (object, to, option, privileges) in grouped(add) {
        let Some(target) = grant_target(d, object) else {
            continue;
        };
        let with = if option { " WITH GRANT OPTION" } else { "" };
        out.push(format!(
            "GRANT {} ON {target} TO {}{with}",
            privileges.join(", "),
            grantee(to)
        ));
    }
    out
}

const SESSIONS_SQL: &str = "\
SELECT a.pid, COALESCE(a.usename::text, ''), a.datname::text,
       NULLIF(concat_ws(' @ ', NULLIF(a.application_name, ''),
                        host(a.client_addr) || COALESCE(':' || a.client_port, '')), ''),
       COALESCE(a.state, a.backend_type),
       CASE WHEN a.wait_event IS NOT NULL THEN a.wait_event_type || ': ' || a.wait_event END,
       a.query,
       (extract(epoch FROM a.backend_start) * 1000)::int8,
       (extract(epoch FROM a.query_start) * 1000)::int8
FROM pg_stat_activity a
WHERE a.backend_type = 'client backend'
ORDER BY a.pid";

async fn sessions(client: &Client, own: &[i32]) -> Result<Vec<SessionInfo>, DriverError> {
    let rows = client.query(SESSIONS_SQL, &[]).await.map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            let pid: i32 = r.get(0);
            SessionInfo {
                id: pid.to_string(),
                user: r.get(1),
                database: r.get(2),
                client: r.get(3),
                state: r.get(4),
                wait: r.get(5),
                query: r.get::<_, Option<String>>(6).filter(|q| !q.is_empty()),
                started_ms: r.get(7),
                query_started_ms: r.get(8),
                own: own.contains(&pid),
            }
        })
        .collect())
}

const LOCKS_SQL: &str = "\
SELECT l.pid::text,
       CASE WHEN l.granted THEN ARRAY[]::text[] ELSE pg_blocking_pids(l.pid)::text[] END,
       l.mode,
       CASE WHEN l.relation IS NOT NULL THEN l.relation::regclass::text
            WHEN l.transactionid IS NOT NULL THEN 'transaction ' || l.transactionid::text
            WHEN l.virtualxid IS NOT NULL THEN 'virtual transaction ' || l.virtualxid
            ELSE l.locktype END,
       l.granted
FROM pg_locks l
WHERE l.pid IS NOT NULL AND l.pid <> pg_backend_pid()
ORDER BY l.pid, l.granted DESC";

async fn locks(client: &Client) -> Result<Vec<LockInfo>, DriverError> {
    let rows = client.query(LOCKS_SQL, &[]).await.map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| LockInfo {
            session: r.get(0),
            blocked_by: r.get(1),
            mode: r.get(2),
            object: r.get(3),
            granted: r.get(4),
        })
        .collect())
}

/// `id` as a server process id.
fn pid(id: &str) -> Result<i32, DriverError> {
    id.trim()
        .parse()
        .map_err(|_| DriverError::new(ErrorKind::Other, format!("\"{id}\" is not a session id")))
}

/// Signals session `id` through `function` (`pg_cancel_backend` or `pg_terminate_backend`,
/// a constant). With `started_ms`, only the session that started then: a pid the server
/// gave to a newer session since is left alone.
async fn signal(
    client: &Client,
    function: &str,
    id: &str,
    started_ms: Option<i64>,
) -> Result<(), DriverError> {
    let pid = pid(id)?;
    let sql = format!(
        "SELECT {function}(a.pid) FROM pg_stat_activity a \
         WHERE a.pid = $1 \
           AND ($2::int8 IS NULL OR (extract(epoch FROM a.backend_start) * 1000)::int8 = $2)"
    );
    let row = client
        .query_opt(&sql, &[&pid, &started_ms])
        .await
        .map_err(map_error)?;
    match row.map(|r| r.get::<_, bool>(0)) {
        Some(true) => Ok(()),
        Some(false) => Err(DriverError::new(
            ErrorKind::Other,
            format!("session {pid} could not be signalled"),
        )),
        None => Err(DriverError::new(
            ErrorKind::Other,
            format!("session {pid} has ended (its id may belong to another session now)"),
        )),
    }
}

async fn properties(client: &Client) -> Result<Vec<ServerProperty>, DriverError> {
    let rows = client
        .query(
            "SELECT name, setting, unit, category, short_desc, source \
             FROM pg_settings ORDER BY category, name",
            &[],
        )
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| ServerProperty {
            name: r.get(0),
            value: r.get::<_, Option<String>>(1).unwrap_or_default(),
            unit: r.get(2),
            category: r.get(3),
            description: r.get(4),
            source: r.get(5),
        })
        .collect())
}

async fn database_sizes(client: &Client) -> Result<Vec<(String, i64)>, DriverError> {
    let rows = client
        .query(
            "SELECT datname::text, pg_database_size(oid) FROM pg_database \
             WHERE NOT datistemplate AND datallowconn \
               AND has_database_privilege(oid, 'CONNECT') \
             ORDER BY 1",
            &[],
        )
        .await
        .map_err(map_error)?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
}

const STATS_SQL: &str = "\
SELECT n.nspname::text, c.relname::text,
       CASE WHEN c.reltuples >= 0 THEN c.reltuples::int8 ELSE s.n_live_tup END,
       pg_total_relation_size(c.oid), pg_table_size(c.oid), pg_indexes_size(c.oid),
       s.seq_scan, s.idx_scan, s.n_live_tup, s.n_dead_tup,
       GREATEST(s.last_vacuum, s.last_autovacuum)::text,
       GREATEST(s.last_analyze, s.last_autoanalyze)::text
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
LEFT JOIN pg_stat_all_tables s ON s.relid = c.oid
WHERE c.relkind IN ('r', 'p', 'm')
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND n.nspname NOT LIKE 'pg\\_%'
  AND ($1::text IS NULL OR n.nspname = $1)
ORDER BY 1, 2";

async fn table_stats(
    client: &Client,
    schema: Option<&str>,
) -> Result<Vec<TableStats>, DriverError> {
    let rows = client
        .query(STATS_SQL, &[&schema])
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            let mut extra = Vec::new();
            let numbers = [
                (6, "Sequential scans"),
                (7, "Index scans"),
                (8, "Live rows"),
                (9, "Dead rows"),
            ];
            for (i, label) in numbers {
                if let Some(n) = r.get::<_, Option<i64>>(i) {
                    extra.push((label.to_string(), n.to_string()));
                }
            }
            for (i, label) in [(10, "Last vacuum"), (11, "Last analyze")] {
                if let Some(when) = r.get::<_, Option<String>>(i) {
                    extra.push((label.to_string(), when));
                }
            }
            TableStats {
                table: TableRef {
                    database: None,
                    schema: Some(r.get(0)),
                    name: r.get(1),
                },
                rows_estimate: r.get(2),
                total_bytes: r.get(3),
                table_bytes: r.get(4),
                index_bytes: r.get(5),
                extra,
            }
        })
        .collect())
}

#[async_trait::async_trait]
impl Admin for PgConnection {
    fn supports(&self) -> AdminSupport {
        support()
    }

    fn current_user(&self) -> Option<String> {
        self.config.get_user().map(String::from)
    }

    fn session_ids(&self) -> Vec<String> {
        self.own_pids().iter().map(i32::to_string).collect()
    }

    async fn roles(&self) -> Result<Vec<RoleDef>, DriverError> {
        roles(&self.meta).await
    }

    async fn grants(
        &self,
        grantee: Option<&str>,
        object: Option<&ObjectRef>,
    ) -> Result<Vec<Grant>, DriverError> {
        grants(&self.meta, grantee, object).await
    }

    fn role_ddl(&self, before: Option<&RoleDef>, after: &RoleDef) -> Vec<String> {
        role_ddl(&self.dialect, before, after)
    }

    fn drop_role_ddl(&self, role: &RoleDef) -> Vec<String> {
        vec![format!(
            "DROP ROLE {}",
            self.dialect.quote_ident(&role.name)
        )]
    }

    fn grant_ddl(&self, add: &[Grant], revoke: &[Grant]) -> Vec<String> {
        grant_ddl(&self.dialect, add, revoke)
    }

    async fn sessions(&self) -> Result<Vec<SessionInfo>, DriverError> {
        sessions(&self.meta, &self.own_pids()).await
    }

    async fn locks(&self) -> Result<Vec<LockInfo>, DriverError> {
        locks(&self.meta).await
    }

    async fn cancel_session(&self, id: &str, started_ms: Option<i64>) -> Result<(), DriverError> {
        signal(&self.meta, "pg_cancel_backend", id, started_ms).await
    }

    async fn terminate_session(
        &self,
        id: &str,
        started_ms: Option<i64>,
    ) -> Result<(), DriverError> {
        signal(&self.meta, "pg_terminate_backend", id, started_ms).await
    }

    async fn properties(&self) -> Result<Vec<ServerProperty>, DriverError> {
        properties(&self.meta).await
    }

    async fn database_sizes(&self) -> Result<Vec<(String, i64)>, DriverError> {
        database_sizes(&self.meta).await
    }

    async fn table_stats(&self, schema: Option<&str>) -> Result<Vec<TableStats>, DriverError> {
        table_stats(&self.meta, schema).await
    }
}

#[cfg(test)]
mod tests {
    use sqlr_core::Password;

    use super::*;

    fn table(name: &str) -> ObjectRef {
        ObjectRef {
            kind: ObjectKind::Table,
            database: None,
            schema: Some("s".into()),
            name: name.into(),
            parent: None,
            signature: None,
        }
    }

    #[test]
    fn role_ddl_writes_only_what_changed() {
        let d = PgDialect;
        let new = RoleDef {
            name: "a\"b".into(),
            login: true,
            inherit: true,
            member_of: vec!["r".into()],
            password: Some(Password::new("p'w")),
            ..Default::default()
        };
        assert_eq!(
            role_ddl(&d, None, &new),
            [
                "CREATE ROLE \"a\"\"b\" WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT \
                 NOREPLICATION PASSWORD 'p''w'",
                "GRANT \"r\" TO \"a\"\"b\""
            ]
        );
        let before = RoleDef {
            password: None,
            ..new.clone()
        };
        let after = RoleDef {
            name: "c".into(),
            superuser: true,
            connection_limit: Some(2),
            member_of: vec!["s".into()],
            ..before.clone()
        };
        assert_eq!(
            role_ddl(&d, Some(&before), &after),
            [
                "ALTER ROLE \"a\"\"b\" RENAME TO \"c\"",
                "ALTER ROLE \"c\" WITH SUPERUSER CONNECTION LIMIT 2",
                "REVOKE \"r\" FROM \"c\"",
                "GRANT \"s\" TO \"c\""
            ]
        );
        assert!(role_ddl(&d, Some(&before), &before).is_empty());
        let cleared = RoleDef {
            clear_password: true,
            ..before.clone()
        };
        assert_eq!(
            role_ddl(&d, Some(&before), &cleared),
            ["ALTER ROLE \"a\"\"b\" WITH PASSWORD NULL"]
        );
    }

    #[test]
    fn grants_are_grouped_and_revokes_come_first() {
        let d = PgDialect;
        let g = |privilege: &str, grantable, object: ObjectRef| Grant {
            grantee: "clerk".into(),
            object,
            privilege: privilege.into(),
            grantable,
        };
        let add = [
            g("SELECT", false, table("t")),
            g("INSERT", false, table("t")),
            g("UPDATE", true, table("t")),
            g("bad; DROP", false, table("t")),
        ];
        let revoke = [g("DELETE", true, table("u"))];
        assert_eq!(
            grant_ddl(&d, &add, &revoke),
            [
                "REVOKE GRANT OPTION FOR DELETE ON TABLE \"s\".\"u\" FROM \"clerk\"",
                "GRANT SELECT, INSERT ON TABLE \"s\".\"t\" TO \"clerk\"",
                "GRANT UPDATE ON TABLE \"s\".\"t\" TO \"clerk\" WITH GRANT OPTION",
            ]
        );
        // Revoking a privilege revokes its grant option too: one statement.
        assert_eq!(
            grant_ddl(
                &d,
                &[],
                &[
                    g("SELECT", false, table("t")),
                    g("SELECT", true, table("t"))
                ]
            ),
            ["REVOKE SELECT ON TABLE \"s\".\"t\" FROM \"clerk\""]
        );
        let public = Grant {
            grantee: "PUBLIC".into(),
            ..g(
                "USAGE",
                false,
                ObjectRef {
                    kind: ObjectKind::Schema,
                    schema: None,
                    ..table("s")
                },
            )
        };
        assert_eq!(
            grant_ddl(&d, &[public], &[]),
            ["GRANT USAGE ON SCHEMA \"s\" TO PUBLIC"]
        );
    }
}
