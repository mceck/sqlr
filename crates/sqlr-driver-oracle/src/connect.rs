//! Connection setup for the Oracle driver (`oracle-rs`, no Oracle client libraries).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use oracle_rs::{Config, Connection as OraConnection};
use sqlr_core::{
    CancelHandle, Connection, ConnectionConfig, Dialect, Driver, DriverError, DriverManifest,
    ErrorKind, Execution, Introspect, LexState, Node, NodeKind, NodePath, ObjectDef, ObjectRef,
    RelationalConn, Secrets, ServerInfo, TableInfo, TableRef, TokenKind, Value,
};

use crate::dialect::OracleDialect;
use crate::syntax::oracle_syntax;
use crate::{browse, catalog, describe, execute, introspect, manifest, map_error};

pub struct OracleDriver {
    manifest: DriverManifest,
}

impl OracleDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            manifest: manifest::manifest(),
        })
    }
}

pub struct OracleConnection {
    /// The live connection; replaced when Oracle closes it (see [`Self::healed`]).
    conn: std::sync::Mutex<Arc<OraConnection>>,
    config: Config,
    service: String,
    dialect: OracleDialect,
    /// The session runs an explicit transaction: the engine opened it with `SET
    /// TRANSACTION` (Manual and Smart modes, applies, imports). Its statements must not
    /// commit on their own.
    explicit_transaction: AtomicBool,
}

/// What a statement does to Oracle's implicit transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TxEffect {
    /// It neither writes nor changes the transaction (queries, `SAVEPOINT`, `ROLLBACK TO`).
    Reads,
    /// `SET TRANSACTION`: the engine's `begin_sql`, which starts an explicit transaction.
    Opens,
    /// `COMMIT` or `ROLLBACK`: the transaction ends.
    Ends,
    /// It writes. On a server with autocommit off it leaves a transaction open.
    Writes,
}

/// The first three significant words of `sql`, upper-cased (comments skipped).
pub(crate) fn leading_words(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    oracle_syntax().lex(sql, LexState::Normal, &mut tokens);
    tokens
        .iter()
        .filter(|t| {
            matches!(
                t.kind,
                TokenKind::Keyword | TokenKind::Ident | TokenKind::Function
            )
        })
        .take(3)
        .map(|t| sql[t.start..t.end].to_ascii_uppercase())
        .collect()
}

fn tx_effect(sql: &str) -> TxEffect {
    let words = leading_words(sql);
    match words.as_slice() {
        [first, ..] if first == "COMMIT" => TxEffect::Ends,
        [first, rest @ ..] if first == "ROLLBACK" => {
            let rest = if rest.first().is_some_and(|word| word == "WORK") {
                &rest[1..]
            } else {
                rest
            };
            if rest.first().is_some_and(|word| word == "TO") {
                TxEffect::Reads
            } else {
                TxEffect::Ends
            }
        }
        [first, second, ..] if first == "SET" && second == "TRANSACTION" => TxEffect::Opens,
        _ if oracle_syntax().classify_script(sql).modifies() => TxEffect::Writes,
        _ => TxEffect::Reads,
    }
}

#[async_trait::async_trait]
impl Driver for OracleDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    fn dialect(&self) -> Option<Arc<dyn Dialect>> {
        Some(Arc::new(OracleDialect))
    }

    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        sqlr_core::validate_driver_properties(&self.manifest.fields, &cfg.values)?;
        let m = &self.manifest;
        let text = |key: &str| {
            cfg.value(m, key).ok_or_else(|| {
                DriverError::new(
                    ErrorKind::Other,
                    format!("missing connection field `{key}`"),
                )
            })
        };
        let host = text("host")?.trim().to_string();
        let port: u16 = text("port")?
            .parse()
            .map_err(|_| DriverError::new(ErrorKind::Other, "invalid port"))?;
        let service = text("service")?.trim().to_string();
        let user = text("user")?.trim().to_string();
        let password = secrets.0.get("password").cloned().unwrap_or_default();
        let by_sid = cfg.value(m, "service_type") == Some("sid");

        let mut config = if by_sid {
            Config::with_sid(host, port, service.clone(), user, password)
        } else {
            Config::new(host, port, service.clone(), user, password)
        };
        config.connect_timeout = Duration::from_secs(15);
        if cfg.value(m, "tls").is_some_and(|v| v == "true") {
            config = config.with_tls().map_err(map_error)?;
        }
        let conn = OraConnection::connect_with_config(config.clone())
            .await
            .map_err(map_error)?;
        Ok(Arc::new(OracleConnection {
            conn: std::sync::Mutex::new(Arc::new(conn)),
            config,
            service,
            dialect: OracleDialect,
            explicit_transaction: AtomicBool::new(false),
        }))
    }
}

impl OracleConnection {
    fn conn(&self) -> Arc<OraConnection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The connection to use: the current one, or a fresh one when Oracle has closed it (a
    /// failed statement can leave it unusable without saying so). Losing an explicit
    /// transaction is reported before the caller can run work in the replacement session.
    async fn live(&self) -> Result<Arc<OraConnection>, DriverError> {
        let current = self.conn();
        let usable =
            !current.is_closed() && current.state().await == oracle_rs::ConnectionState::Ready;
        if usable {
            return Ok(current);
        }
        match OraConnection::connect_with_config(self.config.clone()).await {
            Ok(fresh) => {
                let fresh = Arc::new(fresh);
                *self.conn.lock().unwrap_or_else(|e| e.into_inner()) = fresh.clone();
                // The old session's transaction did not survive the new connection.
                if self.explicit_transaction.swap(false, Ordering::SeqCst) {
                    return Err(DriverError::new(
                        ErrorKind::Connection,
                        "the connection was reopened: an open transaction was lost; the operation was not executed",
                    ));
                }
                Ok(fresh)
            }
            Err(_) => Ok(current),
        }
    }

    /// Runs `sql` with the session's autocommit rules. oracle-rs never commits by itself
    /// and the engine sends no COMMIT in Auto mode, so a statement outside an explicit
    /// transaction must commit its own work (spec §7.9: "every statement commits on its
    /// own"). Successful DDL commits on the server. A failing DDL can commit before its
    /// execution and close the session, which also ends the explicit transaction.
    async fn run_statement(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        let effect = tx_effect(sql);
        let conn = self.live().await?;
        let explicit = self.explicit_transaction.load(Ordering::SeqCst);
        let result = self
            .healed(execute::execute(conn.clone(), sql, params).await)
            .await;
        let committing_ddl = oracle_syntax().is_schema_change(sql);
        let failed_ddl = result.is_err() && committing_ddl;
        let failed_ddl_closed_session = failed_ddl
            && (conn.is_closed() || conn.state().await != oracle_rs::ConnectionState::Ready);
        match &result {
            Ok(_) => {
                if committing_ddl {
                    // Oracle commits before and after DDL whatever the session was doing.
                    self.explicit_transaction.store(false, Ordering::SeqCst);
                } else {
                    match effect {
                        TxEffect::Opens => self.explicit_transaction.store(true, Ordering::SeqCst),
                        TxEffect::Ends => self.explicit_transaction.store(false, Ordering::SeqCst),
                        TxEffect::Reads => {}
                        TxEffect::Writes if !explicit => {
                            conn.commit().await.map_err(map_error)?;
                        }
                        TxEffect::Writes => {}
                    }
                }
            }
            Err(e) if e.kind == ErrorKind::Connection => {
                self.explicit_transaction.store(false, Ordering::SeqCst)
            }
            Err(_) if failed_ddl_closed_session => {
                // oracle-rs can report failed DDL as a non-connection error even when
                // the session is closed. The prior transaction cannot continue there.
                self.explicit_transaction.store(false, Ordering::SeqCst);
            }
            // A failed statement outside an explicit transaction must not leave its
            // transaction (and locks) open, or the next `SET TRANSACTION` is refused.
            Err(_) if !explicit && effect == TxEffect::Writes => {
                let _ = conn.rollback().await;
            }
            Err(_) => {}
        }
        result
    }

    /// Oracle Free closes the connection on a failed statement, and the library reports that
    /// as a closed connection: the error's cause is lost and every later call fails. On such
    /// an error the connection is opened again, so the session carries on; session state (an
    /// open transaction, `ALTER SESSION` settings) does not survive, and the message says so.
    async fn healed<T>(&self, result: Result<T, DriverError>) -> Result<T, DriverError> {
        let mut error = match result {
            Err(e) if e.kind == ErrorKind::Connection => e,
            other => return other,
        };
        if let Ok(fresh) = OraConnection::connect_with_config(self.config.clone()).await {
            *self.conn.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(fresh);
            self.explicit_transaction.store(false, Ordering::SeqCst);
            error.message = format!(
                "{} (the connection was reopened: an open transaction was lost)",
                error.message
            );
        }
        Err(error)
    }
}

#[async_trait::async_trait]
impl Connection for OracleConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let conn = self.live().await?;
        self.healed(conn.ping().await.map_err(map_error)).await?;
        let info = conn.server_info().await;
        let version = if !info.version.is_empty() {
            info.version
        } else if !info.banner.is_empty() {
            info.banner
        } else {
            info.instance_name.unwrap_or_else(|| "Oracle".to_string())
        };
        Ok(ServerInfo {
            product: "Oracle".into(),
            version,
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        self.healed(browse::browse(&*self.live().await?, path).await)
            .await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(OracleCancel)
    }

    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }

    fn database_name(&self) -> Option<&str> {
        Some(&self.service)
    }
}

#[async_trait::async_trait]
impl RelationalConn for OracleConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        self.run_statement(sql, &[]).await
    }

    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        self.run_statement(sql, params).await
    }

    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        self.healed(describe::describe(&*self.live().await?, table).await)
            .await
    }

    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }

    async fn catalog(&self, schema: &str) -> Result<sqlr_core::SchemaCatalog, DriverError> {
        self.healed(catalog::catalog(&*self.live().await?, schema).await)
            .await
    }

    /// Paths are `[schema, folder, name]` and `[schema, "tables", table, "indexes", index]`.
    fn object_ref(&self, node: &Node) -> Option<ObjectRef> {
        let segs: Vec<&str> = node.path.segments().iter().map(String::as_str).collect();
        let object = |kind, schema: &str, name: &str, parent: Option<&str>| ObjectRef {
            kind,
            database: None,
            schema: Some(schema.to_string()),
            name: name.to_string(),
            parent: parent.map(String::from),
            signature: None,
        };
        match (node.kind, segs.as_slice()) {
            (NodeKind::Index, [schema, "tables", table, "indexes", index]) => Some(object(
                sqlr_core::ObjectKind::Index,
                schema,
                index,
                Some(table),
            )),
            (NodeKind::Table, [schema, "tables", name]) => {
                Some(object(sqlr_core::ObjectKind::Table, schema, name, None))
            }
            (NodeKind::View, [schema, "views", name]) => {
                Some(object(sqlr_core::ObjectKind::View, schema, name, None))
            }
            _ => None,
        }
    }

    fn introspect(&self) -> Option<&dyn Introspect> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl Introspect for OracleConnection {
    async fn load_object(&self, object: &ObjectRef) -> Result<ObjectDef, DriverError> {
        self.healed(introspect::load_object(&*self.live().await?, object).await)
            .await
    }

    async fn list_roles(&self) -> Result<Vec<String>, DriverError> {
        self.healed(introspect::roles(&*self.live().await?).await)
            .await
    }

    /// The connection's own database (pluggable database) only.
    async fn load_schema(
        &self,
        database: Option<&str>,
        schema: &str,
    ) -> Result<sqlr_core::SchemaSnapshot, DriverError> {
        if database.is_some_and(|d| d != self.service) {
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                "multiple databases are not supported",
            ));
        }
        self.healed(introspect::load_schema(&*self.live().await?, schema).await)
            .await
    }
}

struct OracleCancel;

#[async_trait::async_trait]
impl CancelHandle for OracleCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        Err(DriverError::new(
            ErrorKind::Unsupported,
            "Oracle cannot cancel a running statement without closing the connection",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_connection() -> OracleConnection {
        static LOCK: std::sync::OnceLock<sqlr_testkit::TestLock> = std::sync::OnceLock::new();
        let _ = LOCK.get_or_init(|| sqlr_testkit::TestLock::acquire("oracle"));
        let (host, port) = sqlr_testkit::service("ORACLE");
        let config = Config::new(host, port, "FREEPDB1", "sqlr_test", "sqlr_test");
        let conn = OraConnection::connect_with_config(config.clone())
            .await
            .unwrap();
        OracleConnection {
            conn: std::sync::Mutex::new(Arc::new(conn)),
            config,
            service: "FREEPDB1".into(),
            dialect: OracleDialect,
            explicit_transaction: AtomicBool::new(false),
        }
    }

    #[tokio::test]
    async fn a_lost_transaction_is_reported_before_executing_another_statement() {
        sqlr_testkit::skip_container_tests!();
        let conn = test_connection().await;
        let name = sqlr_testkit::unique_name("SQLR_LOST_TX").to_uppercase();
        conn.execute(&format!("CREATE TABLE {name} (id NUMBER(10))"))
            .await
            .unwrap();
        conn.execute("SET TRANSACTION READ WRITE").await.unwrap();
        conn.execute(&format!("INSERT INTO {name} VALUES (1)"))
            .await
            .unwrap();
        conn.conn().close().await.unwrap();
        let error = conn
            .execute(&format!("INSERT INTO {name} VALUES (2)"))
            .await
            .err();
        let Execution::Rows(mut rows) = conn
            .execute(&format!("SELECT COUNT(*) FROM {name}"))
            .await
            .unwrap()
        else {
            panic!("expected rows");
        };
        let chunk = rows.next_chunk(10).await.unwrap().unwrap();
        let count = Value::from_cell(chunk.cell(0, 0));
        rows.close().await.unwrap();
        conn.execute(&format!("DROP TABLE {name} PURGE"))
            .await
            .unwrap();
        let error = error.expect("a lost explicit transaction must not be silently replaced");
        assert_eq!(error.kind, ErrorKind::Connection);
        assert!(error.message.contains("transaction"), "{error:?}");
        assert_eq!(count.display().as_ref(), "0", "the new write must not run");
    }

    #[tokio::test]
    async fn ping_reports_a_lost_transaction_instead_of_masking_it() {
        sqlr_testkit::skip_container_tests!();
        let conn = test_connection().await;
        conn.execute("SET TRANSACTION READ WRITE").await.unwrap();
        conn.conn().close().await.unwrap();
        let error = conn
            .ping()
            .await
            .expect_err("ping must report the lost explicit transaction");
        assert_eq!(error.kind, ErrorKind::Connection);
        assert!(error.message.contains("transaction"), "{error:?}");
        assert!(conn.ping().await.is_ok());
    }

    #[test]
    fn statements_open_end_and_write_to_the_implicit_transaction() {
        // The engine's begin_sql opens the explicit transaction.
        assert_eq!(tx_effect("SET TRANSACTION READ WRITE"), TxEffect::Opens);
        assert_eq!(
            tx_effect("-- note\nset transaction read only"),
            TxEffect::Opens
        );
        assert_eq!(tx_effect("COMMIT"), TxEffect::Ends);
        assert_eq!(tx_effect("rollback"), TxEffect::Ends);
        assert_eq!(tx_effect("COMMIT WORK"), TxEffect::Ends);
        assert_eq!(tx_effect("COMMIT COMMENT 'audit'"), TxEffect::Ends);
        assert_eq!(tx_effect("ROLLBACK WORK"), TxEffect::Ends);
        assert_eq!(tx_effect("ROLLBACK TO SAVEPOINT s"), TxEffect::Reads);
        assert_eq!(tx_effect("ROLLBACK WORK TO SAVEPOINT s"), TxEffect::Reads);
        assert_eq!(tx_effect("SAVEPOINT s"), TxEffect::Reads);
        assert_eq!(tx_effect("SELECT 1 FROM dual"), TxEffect::Reads);
        assert_eq!(tx_effect("INSERT INTO t VALUES (1)"), TxEffect::Writes);
        assert_eq!(tx_effect("UPDATE t SET a = 1"), TxEffect::Writes);
        // A PL/SQL block is not a transaction begin, and its DML must be committed.
        assert_eq!(tx_effect("BEGIN NULL; END;"), TxEffect::Reads);
        assert_eq!(
            tx_effect("BEGIN INSERT INTO t VALUES (1); END;"),
            TxEffect::Writes
        );
        // DDL commits on the server; the driver still recognizes it as a write.
        assert_eq!(tx_effect("CREATE TABLE t (a NUMBER)"), TxEffect::Writes);
    }

    #[test]
    fn ddl_is_detected_as_an_implicit_commit() {
        assert!(oracle_syntax().is_schema_change("CREATE TABLE t (a NUMBER)"));
        assert!(oracle_syntax().is_schema_change("TRUNCATE TABLE t"));
        assert!(!oracle_syntax().is_schema_change("INSERT INTO t VALUES (1)"));
    }
}
