//! Native Snowflake driver using the public HTTPS SQL API, with no external runtime.
//!
//! Supply an OAuth token, programmatic access token, or externally generated key-pair
//! JWT through `Secrets["token"]`. Each request carries its own configured database,
//! schema, warehouse and role. Independent requests have no durable session: explicit
//! transactions and session control are rejected; result editing and object editors
//! are deliberately unavailable. Results download one partition at a time and retain
//! exact NUMBER/DECFLOAT values as text. Output parameters request readable temporal
//! values without losing fractional seconds.
//!
//! Protocol: <https://docs.snowflake.com/en/developer-guide/sql-api/reference>.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod catalog;
mod protocol;
#[cfg(test)]
mod tests;

use parking_lot::Mutex;
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde_json::{Value as Json, json};
use sqlr_core::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

/// Factory for the built-in Snowflake SQL API driver.
pub struct SnowflakeDriver {
    manifest: DriverManifest,
    #[cfg(test)]
    endpoint: Option<String>,
}
impl SnowflakeDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            manifest: manifest(),
            #[cfg(test)]
            endpoint: None,
        })
    }
    #[cfg(test)]
    fn for_test(endpoint: &str) -> Arc<Self> {
        Arc::new(Self {
            manifest: manifest(),
            endpoint: Some(endpoint.into()),
        })
    }
}
fn manifest() -> DriverManifest {
    let mut fields = Vec::new();
    for (key, label, required, secret) in [
        ("host", "Account host", true, false),
        ("database", "Database", false, false),
        ("schema", "Schema", false, false),
        ("warehouse", "Warehouse", false, false),
        ("role", "Role", false, false),
        ("auth_type", "Authentication", true, false),
        ("token", "Access token / JWT", true, true),
    ] {
        fields.push(FieldSpec {
            key: key.into(),
            label: label.into(),
            ty: if secret {
                FieldType::Password
            } else if key == "auth_type" {
                FieldType::Choice(vec![
                    ("OAUTH".into(), "OAuth token".into()),
                    (
                        "PROGRAMMATIC_ACCESS_TOKEN".into(),
                        "Programmatic access token".into(),
                    ),
                    ("KEYPAIR_JWT".into(), "Key-pair JWT".into()),
                ])
            } else {
                FieldType::Text
            },
            default: (key == "auth_type").then(|| "OAUTH".into()),
            required,
            secret,
            group: "Connection".into(),
            free_form: false,
        });
    }
    DriverManifest {
        id: "snowflake".into(),
        name: "Snowflake".into(),
        family: Family::Relational,
        capabilities: Capabilities {
            server_cancel: true,
            tabular_query: true,
            schemas: true,
            ..Default::default()
        },
        fields,
        dialect: "snowflake".into(),
        url_schemes: vec!["snowflake".into()],
        default_port: Some(443),
        editing: None,
    }
}
#[async_trait::async_trait]
impl Driver for SnowflakeDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }
    fn dialect(&self) -> Option<Arc<dyn Dialect>> {
        Some(Arc::new(SnowflakeDialect::default()))
    }
    fn dialect_for(&self, cfg: &ConnectionConfig) -> Option<Arc<dyn Dialect>> {
        Some(Arc::new(SnowflakeDialect::for_config(cfg)))
    }
    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let token = secrets
            .0
            .get("token")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                DriverError::new(ErrorKind::Auth, "Snowflake access token is required")
            })?;
        let host = cfg
            .values
            .get("host")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                DriverError::new(ErrorKind::Connection, "Snowflake account host is required")
            })?;
        // Host only: never accept a URL, user-info, paths, query strings or fragments.
        if host
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-')))
            || host.starts_with('.')
            || host.ends_with('.')
        {
            return Err(DriverError::new(
                ErrorKind::Connection,
                "Snowflake host must be a hostname without a URL scheme or path",
            ));
        }
        let endpoint = Url::parse(&format!("https://{host}/"))
            .map_err(|_| DriverError::new(ErrorKind::Connection, "Invalid Snowflake host"))?;
        #[cfg(test)]
        let endpoint = match &self.endpoint {
            Some(value) => Url::parse(value).unwrap(),
            None => endpoint,
        };
        let auth_type = cfg
            .value(&self.manifest, "auth_type")
            .unwrap_or("OAUTH")
            .to_ascii_uppercase();
        let auth_type = match auth_type.as_str() {
            "OAUTH" => HeaderValue::from_static("OAUTH"),
            "KEYPAIR_JWT" => HeaderValue::from_static("KEYPAIR_JWT"),
            "PROGRAMMATIC_ACCESS_TOKEN" => HeaderValue::from_static("PROGRAMMATIC_ACCESS_TOKEN"),
            _ => {
                return Err(DriverError::new(
                    ErrorKind::Auth,
                    "Unsupported Snowflake authentication type",
                ));
            }
        };
        let mut headers = HeaderMap::new();
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| DriverError::new(ErrorKind::Auth, "Invalid Snowflake access token"))?;
        authorization.set_sensitive(true);
        headers.insert(AUTHORIZATION, authorization);
        headers.insert("X-Snowflake-Authorization-Token-Type", auth_type);
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::builder()
            .default_headers(headers)
            .user_agent("sqlr/0.1")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| {
                DriverError::new(
                    ErrorKind::Connection,
                    "Could not initialize Snowflake HTTPS client",
                )
            })?;
        let context: serde_json::Map<String, Json> = ["database", "schema", "warehouse", "role"]
            .into_iter()
            .filter_map(|key| {
                cfg.values
                    .get(key)
                    .filter(|s| !s.is_empty())
                    .map(|v| (key.into(), json!(v)))
            })
            .collect();
        let dialect = SnowflakeDialect::for_config(cfg);
        Ok(Arc::new(SnowflakeConnection {
            api: Arc::new(Api {
                client,
                token: token.clone(),
                endpoint,
                context,
            }),
            active: Arc::new(Mutex::new(None)),
            dialect,
        }))
    }
}

/// ANSI quoted identifiers and Snowflake's positional `?` bindings.
pub struct SnowflakeDialect {
    default_schema: String,
}
impl SnowflakeDialect {
    fn for_config(cfg: &ConnectionConfig) -> Self {
        Self {
            default_schema: cfg
                .values
                .get("schema")
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| "PUBLIC".into()),
        }
    }
}
impl Default for SnowflakeDialect {
    fn default() -> Self {
        Self {
            default_schema: "PUBLIC".into(),
        }
    }
}
impl Dialect for SnowflakeDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
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
    fn default_schemas(&self) -> Vec<String> {
        vec![self.default_schema.clone()]
    }
    fn is_system_schema(&self, schema: &str) -> bool {
        schema == "INFORMATION_SCHEMA"
    }
}

struct Api {
    client: Client,
    /// Only for removing an echoed token from server messages; `Api` has no `Debug`.
    token: String,
    endpoint: Url,
    context: serde_json::Map<String, Json>,
}
type Active = Arc<Mutex<Option<Arc<Query>>>>;
struct Query {
    handle: Mutex<Option<String>>,
    cancelled: AtomicBool,
    api: Arc<Api>,
}
/// Clears only this query's slot, including when an execution future/stream is dropped.
struct QueryGuard {
    query: Arc<Query>,
    active: Active,
}
impl Drop for QueryGuard {
    fn drop(&mut self) {
        let mut active = self.active.lock();
        if active.as_ref().is_some_and(|q| Arc::ptr_eq(q, &self.query)) {
            *active = None;
        }
    }
}
impl Query {
    fn check(&self) -> Result<(), DriverError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(DriverError::new(
                ErrorKind::Cancelled,
                "Snowflake query cancelled",
            ))
        } else {
            Ok(())
        }
    }
    async fn cancel(&self) -> Result<(), DriverError> {
        self.cancelled.store(true, Ordering::Release);
        let handle = self.handle.lock().clone();
        if let Some(handle) = handle {
            self.api.cancel(&handle).await?;
        }
        Ok(())
    }
}
struct SnowflakeConnection {
    api: Arc<Api>,
    active: Active,
    dialect: SnowflakeDialect,
}
struct SnowflakeCancel {
    active: Active,
}
#[async_trait::async_trait]
impl CancelHandle for SnowflakeCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        let query = self.active.lock().clone();
        if let Some(query) = query {
            query.cancel().await?;
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl Connection for SnowflakeConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let rows = self.rows("SELECT CURRENT_VERSION()", &[]).await?;
        let version = catalog::text(&rows, 0, 0)?.to_owned();
        Ok(ServerInfo {
            product: "Snowflake".into(),
            version,
        })
    }
    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        self.browse_impl(path).await
    }
    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(SnowflakeCancel {
            active: self.active.clone(),
        })
    }
    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }
    fn database_name(&self) -> Option<&str> {
        self.api.context.get("database").and_then(Json::as_str)
    }
}
#[async_trait::async_trait]
impl RelationalConn for SnowflakeConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        self.execute_with(sql, &[]).await
    }
    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        let words = words(sql);
        if matches!(
            words.first().map(String::as_str),
            Some(
                "USE" | "SET" | "UNSET" | "BEGIN" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE"
            )
        ) || words.starts_with(&["START".into(), "TRANSACTION".into()])
            || words.starts_with(&["ALTER".into(), "SESSION".into()])
        {
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                "Snowflake SQL API requests have no durable session; configure database/schema/warehouse/role in connection settings. Explicit session and transaction controls are unsupported",
            ));
        }
        let query = Arc::new(Query {
            handle: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            api: self.api.clone(),
        });
        *self.active.lock() = Some(query.clone());
        let guard = QueryGuard {
            query: query.clone(),
            active: self.active.clone(),
        };
        let mut body = self.api.context.clone();
        body.insert("statement".into(), json!(sql));
        body.insert("parameters".into(),json!({"multi_statement_count":"1","date_output_format":"YYYY-MM-DD","time_output_format":"HH24:MI:SS.FF9","timestamp_ntz_output_format":"YYYY-MM-DD HH24:MI:SS.FF9","timestamp_ltz_output_format":"YYYY-MM-DD HH24:MI:SS.FF9 TZH:TZM","timestamp_tz_output_format":"YYYY-MM-DD HH24:MI:SS.FF9 TZH:TZM","binary_output_format":"HEX","timezone":"UTC"}));
        body.insert("bindings".into(), protocol::bindings(params)?);
        let (mut status, mut response) = self
            .api
            .request(
                reqwest::Method::POST,
                "api/v2/statements",
                &[
                    ("async", "true".into()),
                    ("requestId", uuid::Uuid::new_v4().to_string()),
                ],
                Some(&Json::Object(body)),
            )
            .await?;
        loop {
            if let Some(handle) = response.get("statementHandle").and_then(Json::as_str) {
                protocol::validate_handle(handle)?;
                *query.handle.lock() = Some(handle.into());
            }
            if query.cancelled.load(Ordering::Acquire) {
                let handle = query.handle.lock().clone();
                if let Some(handle) = handle {
                    let _ = self.api.cancel(&handle).await;
                }
                return query.check().map(|()| unreachable!());
            }
            if !matches!(status, 202 | 429) {
                break;
            }
            let handle = query.handle.lock().clone().ok_or_else(|| {
                if status == 429 {
                    DriverError::new(
                        ErrorKind::Timeout,
                        "Snowflake rate limit reached; retry the statement",
                    )
                } else {
                    protocol::invalid()
                }
            })?;
            tokio::time::sleep(Duration::from_millis(100)).await;
            query.check()?;
            (status, response) = self
                .api
                .request(
                    reqwest::Method::GET,
                    &format!("api/v2/statements/{handle}"),
                    &[],
                    None,
                )
                .await?;
        }
        let handle = query.handle.lock().clone().ok_or_else(protocol::invalid)?;
        // The SQL API reference does not promise statementTypeId; when available use
        // the server's DML family IDs from Snowflake's official Python connector:
        // https://github.com/snowflakedb/snowflake-connector-python/blob/main/src/snowflake/connector/cursor.py
        // Otherwise use the single submitted statement's verb (Snowflake has no RETURNING).
        let dml = if let Some(id) = response.get("statementTypeId") {
            let id = id
                .as_u64()
                .or_else(|| id.as_str().and_then(|s| s.parse().ok()))
                .ok_or_else(protocol::invalid)?;
            matches!(id, 0x3000 | 0x3100 | 0x3200 | 0x3300 | 0x3400 | 0x3500)
        } else {
            matches!(
                words.first().map(String::as_str),
                Some("INSERT" | "UPDATE" | "DELETE" | "MERGE")
            )
        };
        let mut stream = protocol::Stream::new(self.api.clone(), handle, &response, guard)?;
        if dml {
            let mut affected = 0u64;
            while let Some(chunk) = stream.next_chunk(128).await? {
                for r in 0..chunk.len() {
                    for c in 0..chunk.columns().len() {
                        // UPDATE reports a diagnostic subset of multi-joined rows;
                        // these are already included in the total rows updated.
                        if chunk.columns()[c]
                            .name
                            .eq_ignore_ascii_case("number of multi-joined rows updated")
                        {
                            continue;
                        }
                        let count = match chunk.cell(r, c) {
                            Cell::Text(s) => s.parse::<u64>().map_err(|_| protocol::invalid())?,
                            _ => return Err(protocol::invalid()),
                        };
                        affected = affected.checked_add(count).ok_or_else(protocol::invalid)?;
                    }
                }
            }
            stream.close().await?;
            Ok(Execution::Affected(affected))
        } else {
            Ok(Execution::Rows(Box::new(stream)))
        }
    }
    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        self.describe_impl(table).await
    }
    async fn catalog(&self, schema: &str) -> Result<SchemaCatalog, DriverError> {
        self.catalog_impl(schema).await
    }
    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }
}
fn words(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    SqlSyntax::ansi().lex(sql, LexState::Normal, &mut tokens);
    tokens
        .into_iter()
        .filter(|t| t.kind != TokenKind::Comment)
        .take(2)
        .map(|t| sql[t.start..t.end].to_ascii_uppercase())
        .collect()
}
