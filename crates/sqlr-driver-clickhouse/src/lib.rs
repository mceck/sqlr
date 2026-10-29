//! Bundled ClickHouse driver using the official pure Rust HTTP client.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod catalog;
mod stream;

use parking_lot::Mutex;
use sqlr_core::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tracing::instrument::WithSubscriber;

/// Factory for native ClickHouse connections.
pub struct ClickHouseDriver {
    manifest: DriverManifest,
}
impl ClickHouseDriver {
    pub fn new() -> Arc<Self> {
        let fields = [
            ("host", "Host", FieldType::Text, Some("localhost"), true),
            ("port", "Port", FieldType::Port, Some("8123"), true),
            (
                "database",
                "Database",
                FieldType::Text,
                Some("default"),
                false,
            ),
            ("user", "User", FieldType::Text, Some("default"), false),
            ("password", "Password", FieldType::Password, None, false),
            ("tls", "TLS", FieldType::Bool, Some("false"), false),
        ]
        .into_iter()
        .map(|(key, label, ty, default, required)| FieldSpec {
            key: key.into(),
            label: label.into(),
            ty,
            default: default.map(str::to_owned),
            required,
            secret: key == "password",
            group: "Connection".into(),
            free_form: false,
        })
        .collect();
        Arc::new(Self {
            manifest: DriverManifest {
                id: "clickhouse".into(),
                name: "ClickHouse".into(),
                family: Family::Relational,
                capabilities: Capabilities {
                    tabular_query: true,
                    server_cancel: true,
                    schemas: true,
                    ssh: true,
                    ..Default::default()
                },
                fields,
                dialect: "clickhouse".into(),
                url_schemes: vec!["clickhouse".into()],
                default_port: Some(8123),
                editing: None,
            },
        })
    }
}
#[async_trait::async_trait]
impl Driver for ClickHouseDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }
    fn dialect(&self) -> Option<Arc<dyn Dialect>> {
        Some(Arc::new(ClickHouseDialect {
            database: "default".into(),
        }))
    }
    fn dialect_for(&self, cfg: &ConnectionConfig) -> Option<Arc<dyn Dialect>> {
        Some(Arc::new(ClickHouseDialect {
            database: cfg
                .value(&self.manifest, "database")
                .filter(|s| !s.is_empty())
                .unwrap_or("default")
                .into(),
        }))
    }
    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let value = |key| cfg.value(&self.manifest, key).filter(|s| !s.is_empty());
        let host = value("host").ok_or_else(|| {
            DriverError::new(ErrorKind::Connection, "ClickHouse host is required")
        })?;
        let port: u16 = value("port")
            .unwrap_or("8123")
            .parse()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| DriverError::new(ErrorKind::Connection, "Invalid ClickHouse port"))?;
        // Accept a host or an IPv6 literal, never a URL containing credentials or a path.
        if host
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']')))
        {
            return Err(DriverError::new(
                ErrorKind::Connection,
                "ClickHouse host must be a hostname or IP address",
            ));
        }
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.into()
        };
        let scheme = if value("tls") == Some("true") {
            "https"
        } else {
            "http"
        };
        let database = value("database").unwrap_or("default").to_owned();
        let password = secrets.0.get("password").cloned().unwrap_or_default();
        let client = clickhouse::Client::default()
            .with_url(format!("{scheme}://{host}:{port}"))
            .with_database(&database)
            .with_user(value("user").unwrap_or("default"))
            .with_password(&password)
            .with_setting("output_format_json_quote_64bit_integers", "1")
            .with_setting("output_format_json_quote_decimals", "1")
            .with_setting("output_format_json_quote_denormals", "1")
            .with_product_info("sqlr", env!("CARGO_PKG_VERSION"));
        let conn = Arc::new(ClickHouseConnection {
            client,
            password: Arc::from(password),
            active: Arc::new(Mutex::new(None)),
            dialect: ClickHouseDialect { database },
            read_only: AtomicBool::new(value("read_only") == Some("true")),
        });
        tokio::time::timeout(Duration::from_secs(15), conn.ping())
            .await
            .map_err(|_| {
                DriverError::new(ErrorKind::Timeout, "ClickHouse connection timed out")
            })??;
        Ok(conn)
    }
}

struct ClickHouseDialect {
    database: String,
}
impl Dialect for ClickHouseDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("`{}`", ident.replace('\\', "\\\\").replace('`', "``"))
    }
    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String> {
        if !node.kind.opens_data() || node.path.0.len() != 2 {
            return None;
        }
        let mut sql = format!(
            "SELECT * FROM {}.{}",
            self.quote_ident(&node.path.0[0]),
            self.quote_ident(&node.name)
        );
        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        Some(sql)
    }
    fn syntax(&self) -> Arc<SqlSyntax> {
        let mut syntax = (*SqlSyntax::ansi()).clone();
        syntax.ident_quotes = &[(b'`', b'`'), (b'"', b'"')];
        syntax.backslash_escapes = true;
        syntax.hash_comments = true;
        Arc::new(syntax)
    }
    fn string_literal(&self, text: &str) -> String {
        format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
    }
    fn read_only_sql(&self, on: bool) -> Option<String> {
        Some(format!("SET readonly = {}", u8::from(on)))
    }
    fn is_system_schema(&self, schema: &str) -> bool {
        matches!(
            schema,
            "system" | "information_schema" | "INFORMATION_SCHEMA"
        )
    }
    fn database_is_schema(&self) -> bool {
        true
    }
    fn default_schemas(&self) -> Vec<String> {
        vec![self.database.clone()]
    }
}

struct ClickHouseConnection {
    client: clickhouse::Client,
    // Only used to redact echoed server errors. This type deliberately has no Debug.
    password: Arc<str>,
    active: Active,
    dialect: ClickHouseDialect,
    read_only: AtomicBool,
}
type Active = Arc<Mutex<Option<Arc<Running>>>>;
struct Running {
    id: String,
    cancelled: AtomicBool,
    notify: Notify,
    client: clickhouse::Client,
    password: Arc<str>,
}
impl Running {
    async fn kill(&self) -> Result<(), DriverError> {
        let sql = "KILL QUERY WHERE query_id = {sqlr_id:String} SYNC";
        tokio::time::timeout(
            Duration::from_secs(5),
            self.client
                .query(sql)
                .param("sqlr_id", &self.id)
                .execute()
                .with_subscriber(tracing::subscriber::NoSubscriber::default()),
        )
        .await
        .map_err(|_| DriverError::new(ErrorKind::Timeout, "ClickHouse cancellation timed out"))?
        .map_err(|e| map_error(&e, &self.password))
    }
}
struct QueryGuard {
    query: Arc<Running>,
    active: Active,
    finished: bool,
}
/// Registers only the request currently being polled. Paused result streams keep
/// their query identifiers but leave the connection available for catalog reads.
struct ActiveRead<'a>(&'a QueryGuard);
impl Drop for ActiveRead<'_> {
    fn drop(&mut self) {
        self.0.clear_active();
    }
}
impl QueryGuard {
    fn activate(&self) -> Result<ActiveRead<'_>, DriverError> {
        let mut active = self.active.lock();
        if active.is_some() {
            return Err(DriverError::new(
                ErrorKind::Other,
                "ClickHouse connection is already reading a response",
            ));
        }
        *active = Some(self.query.clone());
        Ok(ActiveRead(self))
    }
    fn clear_active(&self) {
        let mut active = self.active.lock();
        if active.as_ref().is_some_and(|q| Arc::ptr_eq(q, &self.query)) {
            *active = None;
        }
    }
}
impl Drop for QueryGuard {
    fn drop(&mut self) {
        let mut active = self.active.lock();
        if active.as_ref().is_some_and(|q| Arc::ptr_eq(q, &self.query)) {
            *active = None;
        }
        drop(active);
        if !self.finished
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let query = self.query.clone();
            runtime.spawn(async move {
                let _ = query.kill().await;
            });
        }
    }
}
struct ClickHouseCancel(Active);
#[async_trait::async_trait]
impl CancelHandle for ClickHouseCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        let query = self.0.lock().clone();
        let Some(query) = query else {
            return Ok(());
        };
        query.cancelled.store(true, Ordering::SeqCst);
        query.notify.notify_one();
        query.kill().await
    }
}
#[async_trait::async_trait]
impl Connection for ClickHouseConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let rows = self.rows("SELECT version()", &[]).await?;
        Ok(ServerInfo {
            product: "ClickHouse".into(),
            version: catalog::text(&rows, 0, 0)?.into(),
        })
    }
    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        self.browse_impl(path).await
    }
    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(ClickHouseCancel(self.active.clone()))
    }
    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }
    fn database_name(&self) -> Option<&str> {
        Some(&self.dialect.database)
    }
    async fn connect_database(&self, database: &str) -> Result<Arc<dyn Connection>, DriverError> {
        Ok(Arc::new(Self {
            client: self.client.clone().with_database(database),
            password: self.password.clone(),
            active: Arc::new(Mutex::new(None)),
            dialect: ClickHouseDialect {
                database: database.into(),
            },
            read_only: AtomicBool::new(self.read_only.load(Ordering::SeqCst)),
        }))
    }
}
#[async_trait::async_trait]
impl RelationalConn for ClickHouseConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        self.execute_with(sql, &[]).await
    }
    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        let syntax = self.dialect.syntax();
        let sql = trim_statement_in(&syntax, sql);
        let mut tokens = Vec::new();
        syntax.lex(&sql, LexState::Normal, &mut tokens);
        tokens.retain(|t| t.kind != TokenKind::Comment);
        let first = tokens
            .first()
            .map(|t| sql[t.start..t.end].to_ascii_uppercase())
            .unwrap_or_default();
        // Requests carry their own database and settings, so durable session statements
        // receive an explicit error rather than pretending their state was retained.
        if matches!(
            first.as_str(),
            "BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" | "USE" | "SET"
        ) {
            if params.is_empty()
                && tokens.len() == 4
                && first == "SET"
                && sql[tokens[1].start..tokens[1].end].eq_ignore_ascii_case("readonly")
                && &sql[tokens[2].start..tokens[2].end] == "="
            {
                match &sql[tokens[3].start..tokens[3].end] {
                    "0" => self.read_only.store(false, Ordering::SeqCst),
                    "1" => self.read_only.store(true, Ordering::SeqCst),
                    _ => {
                        return Err(DriverError::new(
                            ErrorKind::Unsupported,
                            "Expected readonly = 0 or 1",
                        ));
                    }
                }
                return Ok(Execution::Affected(0));
            }
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                "ClickHouse uses auto-commit requests; choose the database in the connection and query settings with SETTINGS",
            ));
        }
        let sql = bind_sql(&sql, &tokens, params)?;
        // The official client reserves '?' even inside literals; escape these after
        // replacing real placeholders with ClickHouse's typed server parameters.
        let mut query = self.client.query(&sql);
        for (i, value) in params.iter().enumerate() {
            let name = format!("sqlr_p{}", i + 1);
            query = match value {
                Value::Null => query.param(&name, Option::<String>::None),
                Value::Bool(v) => query.param(&name, v),
                Value::Int(v) => query.param(&name, v),
                Value::Float(v) => query.param(&name, v),
                Value::Text(v) => query.param(&name, v),
                Value::Bytes(_) => {
                    return Err(DriverError::new(
                        ErrorKind::Unsupported,
                        "ClickHouse binary parameters are not supported",
                    ));
                }
            };
        }
        if self.read_only.load(Ordering::SeqCst) {
            query = query.with_setting("readonly", "1");
        }
        let running = Arc::new(Running {
            id: uuid::Uuid::new_v4().to_string(),
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
            client: self.client.clone(),
            password: self.password.clone(),
        });
        let guard = QueryGuard {
            query: running.clone(),
            active: self.active.clone(),
            finished: false,
        };
        // Upstream debug diagnostics include server error bodies, which can echo values.
        // The engine records failures under sqlr's diagnostics.include_sql policy.
        let cursor =
            tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
                query
                    .with_setting("query_id", &running.id)
                    .fetch_bytes("JSONCompactEachRowWithNamesAndTypes")
            })
            .map_err(|e| map_error(&e, &self.password))?;
        stream::ClickHouseStream::open(cursor, guard).await
    }
    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }
    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        self.describe_impl(table).await
    }
    async fn catalog(&self, schema: &str) -> Result<SchemaCatalog, DriverError> {
        self.catalog_impl(schema).await
    }
}

fn bind_sql(sql: &str, tokens: &[Token], params: &[Value]) -> Result<String, DriverError> {
    if params.is_empty() {
        return Ok(sql.replace('?', "??"));
    }
    let mut bound = String::new();
    let mut end = 0;
    let mut index = 0;
    for token in tokens {
        if &sql[token.start..token.end] != "?"
            || matches!(
                token.kind,
                TokenKind::String | TokenKind::QuotedIdent | TokenKind::Comment
            )
        {
            continue;
        }
        let value = params
            .get(index)
            .ok_or_else(|| DriverError::new(ErrorKind::Other, "Too few ClickHouse parameters"))?;
        let ty = match value {
            Value::Null => "Nullable(String)",
            Value::Bool(_) => "Bool",
            Value::Int(_) => "Int64",
            Value::Float(_) => "Float64",
            Value::Text(_) | Value::Bytes(_) => "String",
        };
        bound.push_str(&sql[end..token.start].replace('?', "??"));
        index += 1;
        bound.push_str(&format!("{{sqlr_p{index}:{ty}}}"));
        end = token.end;
    }
    if index != params.len() {
        return Err(DriverError::new(
            ErrorKind::Other,
            "ClickHouse parameter count does not match placeholders",
        ));
    }
    bound.push_str(&sql[end..].replace('?', "??"));
    Ok(bound)
}

fn map_error(error: &clickhouse::error::Error, password: &str) -> DriverError {
    use clickhouse::error::Error;
    let message = error.to_string();
    let kind = match error {
        Error::Network(_) => ErrorKind::Connection,
        Error::TimedOut => ErrorKind::Timeout,
        _ if message.contains("AUTHENTICATION_FAILED") || message.contains("REQUIRED_PASSWORD") => {
            ErrorKind::Auth
        }
        _ if message.contains("QUERY_WAS_CANCELLED") => ErrorKind::Cancelled,
        _ if message.contains("SYNTAX_ERROR") => ErrorKind::Syntax,
        _ if message.contains("ACCESS_DENIED") || message.contains("READONLY") => {
            ErrorKind::Permission
        }
        _ => ErrorKind::Other,
    };
    let message = if password.is_empty() {
        message
    } else {
        message.replace(password, "[redacted]")
    };
    DriverError::new(kind, message)
}
fn invalid() -> DriverError {
    DriverError::new(ErrorKind::Protocol, "Invalid ClickHouse tabular response")
}
