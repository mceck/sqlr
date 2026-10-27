//! The tool implementations.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Map, Value, json};
use sqlr_core::{
    ActionId, AgentAccess, CatalogNeed, Chord, ColumnMeta, CommitMode, ConnectionConfig,
    ConnectionProfile, DriverManifest, ErrorKind, Family, Node, NodeKind, NodePath, SettingValue,
    Snippet, StatementClass, TableRef,
};
use sqlr_engine::{Engine, LoadState, QueryId, QueryStatus, SessionKey, SessionStatus, Slot};

use crate::error::{ToolError, ToolResult};
use crate::guide;
use crate::host::{
    ApprovalRequest, ConnectionWrite, EditRange, FolderOp, MoveTarget, Risk, RunTarget,
    SettingLevel, TabEdit,
};
use crate::policy;
use crate::results;
use crate::state::{ServerState, ToolContext};

struct Args(Map<String, Value>);

impl Args {
    fn new(args: Option<&Map<String, Value>>) -> Self {
        Self(args.cloned().unwrap_or_default())
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    fn str(&self, key: &str) -> ToolResult<&str> {
        self.get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::invalid(format!("missing string argument \"{key}\"")))
    }

    fn opt_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    fn object(&self, key: &str) -> ToolResult<&Map<String, Value>> {
        self.get(key)
            .and_then(Value::as_object)
            .ok_or_else(|| ToolError::invalid(format!("missing object argument \"{key}\"")))
    }

    fn opt_u64(&self, key: &str) -> ToolResult<Option<u64>> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value.as_u64().map(Some).ok_or_else(|| {
                ToolError::invalid(format!("\"{key}\" must be a non-negative integer"))
            }),
        }
    }

    fn u64(&self, key: &str) -> ToolResult<u64> {
        self.opt_u64(key)?
            .ok_or_else(|| ToolError::invalid(format!("missing integer argument \"{key}\"")))
    }

    fn opt_i64(&self, key: &str) -> ToolResult<Option<i64>> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_i64()
                .map(Some)
                .ok_or_else(|| ToolError::invalid(format!("\"{key}\" must be an integer"))),
        }
    }

    fn string_array(&self, key: &str) -> ToolResult<Vec<String>> {
        let array = self
            .get(key)
            .and_then(Value::as_array)
            .ok_or_else(|| ToolError::invalid(format!("missing array argument \"{key}\"")))?;
        array
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| ToolError::invalid(format!("\"{key}\" must contain strings")))
            })
            .collect()
    }
}

pub async fn call(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    name: &str,
    arguments: Option<&Map<String, Value>>,
) -> Result<CallToolResult, rmcp::ErrorData> {
    let args = Args::new(arguments);
    let outcome = match name {
        "get_guide" => Ok(get_guide(&args)),
        "config_list" => config_list(state, &args).await,
        "config_write" => config_write(state, ctx, &args).await,
        "config_delete" => config_delete(state, ctx, &args).await,
        "browse" => browse(state, &args).await,
        "describe_object" => describe_object(state, &args).await,
        "search_objects" => search_objects(state, &args).await,
        "run_query" => run_query(state, ctx, &args).await,
        "get_query_result" => get_query_result(state, ctx, &args).await,
        "cancel_query" => cancel_query(state, ctx, &args).await,
        "close_result" => close_result(state, ctx, &args).await,
        "list_query_tabs" => list_query_tabs(state).await,
        "get_query_tab" => get_query_tab(state, &args).await,
        "edit_query_tab" => edit_query_tab(state, &args).await,
        "run_query_tab" => run_query_tab(state, &args).await,
        "list_snippets" => Ok(list_snippets(state)),
        "get_snippet" => get_snippet(state, &args),
        "write_snippet" => write_snippet(state, &args).await,
        "delete_snippet" => delete_snippet(state, ctx, &args).await,
        _ => {
            return Err(rmcp::ErrorData::new(
                rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                format!("unknown tool \"{name}\""),
                None,
            ));
        }
    };
    Ok(match outcome {
        Ok(value) => value,
        Err(error) => error.to_result(),
    })
}

fn guide_result(topic: Option<&str>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(guide::guide(topic))])
}

fn get_guide(args: &Args) -> CallToolResult {
    guide_result(args.opt_str("topic"))
}

// ----- connections and catalog -----

async fn exposed_connection(
    state: &Arc<ServerState>,
    id: &str,
) -> ToolResult<crate::host::AgentConnection> {
    let connection = state.host.connection(id).await?;
    if connection.config.agent_access == AgentAccess::None {
        return Err(ToolError::not_exposed(id));
    }
    Ok(connection)
}

fn missing_secrets(connection: &crate::host::AgentConnection) -> Vec<String> {
    connection
        .manifest
        .fields
        .iter()
        .filter(|field| field.secret && field.required)
        .filter(|field| {
            connection
                .secrets
                .0
                .get(&field.key)
                .is_none_or(|value| value.is_empty())
        })
        .map(|field| field.key.clone())
        .collect()
}

fn connect_error(
    connection: &crate::host::AgentConnection,
    error: &sqlr_core::DriverError,
) -> ToolError {
    match error.kind {
        ErrorKind::Auth => {
            let fields: Vec<String> = connection
                .manifest
                .fields
                .iter()
                .filter(|field| field.secret)
                .map(|field| field.key.clone())
                .collect();
            ToolError::secret_required(&connection.config.id, &fields)
        }
        _ => ToolError::new(
            match error.kind {
                ErrorKind::Connection => "connection",
                ErrorKind::Timeout => "timeout",
                ErrorKind::Unsupported => "unsupported",
                ErrorKind::Permission => "permission",
                _ => "error",
            },
            redact(&error.message),
        ),
    }
}

/// Replaces credentials in free-form error text before it reaches a log or the model.
fn redact(text: &str) -> String {
    let mut out = text.to_string();
    // userinfo in URLs
    if let Some(scheme) = out.find("://") {
        let rest = &out[scheme + 3..];
        if let Some(at) = rest.find('@') {
            let authority = &rest[..at];
            if authority.contains(':') {
                let user = authority.split(':').next().unwrap_or_default();
                out.replace_range(scheme + 3..scheme + 3 + at, &format!("{user}:***"));
            }
        }
    }
    for key in ["password=", "passwd=", "token=", "secret="] {
        let lower = out.to_ascii_lowercase();
        if let Some(mut start) = lower.find(key) {
            start += key.len();
            let end = out[start..]
                .find(|c: char| c.is_whitespace() || c == '&' || c == ';' || c == ',')
                .map_or(out.len(), |i| start + i);
            out.replace_range(start..end, "***");
        }
    }
    out
}

async fn ensure_connected(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    connection: &crate::host::AgentConnection,
) -> ToolResult<SessionKey> {
    let id = connection.config.id.clone();
    if !connection.config.save_secrets {
        let missing = missing_secrets(connection);
        if !missing.is_empty() {
            return Err(ToolError::secret_required(&id, &missing));
        }
    }
    let needs_connect = !matches!(
        state.engine.session_status(&id),
        Some(SessionStatus::Connected(_) | SessionStatus::Connecting)
    );
    if needs_connect {
        state.engine.connect(
            connection.config.clone(),
            connection.secrets.clone(),
            connection.profile.clone(),
        );
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match state.engine.session_status(&id) {
            Some(SessionStatus::Connected(_)) => break,
            Some(SessionStatus::Failed(error)) => return Err(connect_error(connection, &error)),
            Some(SessionStatus::Disconnected) => {
                return Err(ToolError::new(
                    "connection",
                    format!("The connection \"{id}\" is closed"),
                ));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(ToolError::new(
                "timeout",
                format!("Connecting to \"{id}\" did not finish in time"),
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The explorer reflects the connection at once; the agent gets its own session.
    let key = state.engine.agent_session(&id, &ctx.session_id);
    state
        .engine
        .prepare_agent_session(&key, connection.config.agent_access == AgentAccess::Read);
    Ok(key)
}

fn node_kind(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Database => "database",
        NodeKind::Schema => "schema",
        NodeKind::Folder => "folder",
        NodeKind::Table => "table",
        NodeKind::View => "view",
        NodeKind::MaterializedView => "materialized_view",
        NodeKind::Column => "column",
        NodeKind::Index => "index",
        NodeKind::Function => "function",
        NodeKind::Procedure => "procedure",
        NodeKind::Sequence => "sequence",
        NodeKind::Trigger => "trigger",
    }
}

fn node_json(node: &Node) -> Value {
    json!({
        "name": node.name,
        "kind": node_kind(node.kind),
        "path": node.path.segments(),
        "has_children": node.has_children,
        "detail": node.detail,
    })
}

async fn browse(state: &Arc<ServerState>, args: &Args) -> ToolResult<CallToolResult> {
    let id = args.str("connection_id")?;
    let path: Vec<String> = args
        .get("path")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let connection = exposed_connection(state, id).await?;
    ensure_connected(
        state,
        &ToolContext {
            session_id: "catalog".into(),
            client_name: String::new(),
            client_version: String::new(),
        },
        &connection,
    )
    .await?;
    let path = NodePath(path);
    state.engine.browse(id, path.clone());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match state.engine.children(id, &path) {
            Some(LoadState::Ready(nodes)) => {
                let from_cache = state
                    .engine
                    .catalog(id)
                    .is_some_and(|catalog| catalog.from_cache);
                let value = json!({
                    "connection_id": id,
                    "path": path.segments(),
                    "from_cache": from_cache,
                    "nodes": nodes.iter().map(node_json).collect::<Vec<_>>(),
                });
                return Ok(CallToolResult::structured(value));
            }
            Some(LoadState::Failed(error)) => {
                return Err(ToolError::new("catalog", redact(&error.message)));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(ToolError::new(
                "timeout",
                "The catalog did not load in time",
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn describe_object(state: &Arc<ServerState>, args: &Args) -> ToolResult<CallToolResult> {
    let id = args.str("connection_id")?;
    let name = args.str("name")?;
    let connection = exposed_connection(state, id).await?;
    if connection.manifest.family != Family::Relational {
        return Err(ToolError::unsupported(
            "describe_object is relational; use browse for this family",
        ));
    }
    ensure_connected(
        state,
        &ToolContext {
            session_id: "describe".into(),
            client_name: String::new(),
            client_version: String::new(),
        },
        &connection,
    )
    .await?;
    let table = TableRef {
        database: args.opt_str("database").map(str::to_string),
        schema: args.opt_str("schema").map(str::to_string),
        name: name.to_string(),
    };
    // Catalog reads use the live explorer session. Agent sessions open lazily on
    // their first query; prepare_agent_session alone does not connect one.
    state.engine.describe(&SessionKey::meta(id), table.clone());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match state.engine.table_info(id, &table) {
            Some(LoadState::Ready(info)) => {
                let value = json!({
                    "connection_id": id,
                    "table": {
                        "database": info.table.database,
                        "schema": info.table.schema,
                        "name": info.table.name,
                    },
                    "columns": info.columns.iter().map(|column| json!({
                        "name": column.name,
                        "type_name": column.type_name,
                        "nullable": column.nullable,
                        "default": column.default,
                        "generated": column.generated,
                    })).collect::<Vec<_>>(),
                    "primary_key": info.primary_key,
                    "unique_keys": info.unique_keys,
                    "indexes": [],
                    "foreign_keys": [],
                });
                return Ok(CallToolResult::structured(value));
            }
            Some(LoadState::Failed(error)) => {
                return Err(ToolError::new("catalog", redact(&error.message)));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(ToolError::new(
                "timeout",
                "The table description did not load in time",
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn search_objects(state: &Arc<ServerState>, args: &Args) -> ToolResult<CallToolResult> {
    let query = args.str("query")?.to_ascii_lowercase();
    let limit = args.opt_u64("limit")?.unwrap_or(50).min(200) as usize;
    let only = args.opt_str("connection_id");
    let connections = state.host.connections().await?;
    let mut out = Vec::new();
    let mut from_cache = true;
    for config in connections {
        if config.agent_access == AgentAccess::None {
            continue;
        }
        if only.is_some_and(|id| id != config.id) {
            continue;
        }
        let connection = exposed_connection(state, &config.id).await?;
        if connection.manifest.family != Family::Relational {
            continue;
        }
        if ensure_connected(
            state,
            &ToolContext {
                session_id: "search".into(),
                client_name: String::new(),
                client_version: String::new(),
            },
            &connection,
        )
        .await
        .is_err()
        {
            continue;
        }
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let catalog = state.engine.catalog(&config.id);
            match catalog.as_ref().and_then(|c| c.schemas.as_ref()) {
                Some(LoadState::Ready(_)) => break,
                Some(LoadState::Failed(_)) => break,
                _ => {}
            }
            if Instant::now() >= deadline {
                break;
            }
            state.engine.load_catalog(&config.id, CatalogNeed::Schemas);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let Some(catalog) = state.engine.catalog(&config.id) else {
            continue;
        };
        from_cache &= catalog.from_cache;
        let schemas = match &catalog.schemas {
            Some(LoadState::Ready(schemas)) => schemas.clone(),
            _ => continue,
        };
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let catalog = state.engine.catalog(&config.id);
            let Some(catalog) = catalog else {
                break;
            };
            let all_ready = schemas.iter().all(|schema| {
                matches!(
                    catalog.schema.get(schema),
                    Some(LoadState::Ready(_)) | Some(LoadState::Failed(_))
                )
            });
            if all_ready {
                break;
            }
            for schema in &schemas {
                state
                    .engine
                    .load_catalog(&config.id, CatalogNeed::Schema(schema.clone()));
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let Some(catalog) = state.engine.catalog(&config.id) else {
            continue;
        };
        for (schema, load) in &catalog.schema {
            let LoadState::Ready(schema_catalog) = load else {
                continue;
            };
            for relation in &schema_catalog.relations {
                if relation.name.to_ascii_lowercase().contains(&query) {
                    out.push(json!({
                        "connection_id": config.id,
                        "schema": schema,
                        "name": relation.name,
                        "kind": format!("{:?}", relation.kind).to_ascii_lowercase(),
                        "path": [schema, "tables", &relation.name],
                    }));
                    if out.len() >= limit {
                        return Ok(CallToolResult::structured(json!({
                            "from_cache": from_cache,
                            "objects": out,
                        })));
                    }
                }
            }
        }
    }
    Ok(CallToolResult::structured(json!({
        "from_cache": from_cache,
        "objects": out,
    })))
}

// ----- queries -----

static NEXT_KV_CONSOLE: AtomicU64 = AtomicU64::new(1);

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn new_id(prefix: &str) -> String {
    let mut bytes = [0u8; 12];
    if getrandom::fill(&mut bytes).is_err() {
        return format!("{prefix}-{}-{}", std::process::id(), now_ms());
    }
    let mut out = String::with_capacity(prefix.len() + 25);
    out.push_str(prefix);
    out.push('-');
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn status_name(status: &QueryStatus) -> &'static str {
    match status {
        QueryStatus::Running => "running",
        QueryStatus::Ready { .. } => "ready",
        QueryStatus::Cancelled => "cancelled",
        QueryStatus::Failed(_) => "failed",
    }
}

async fn approve(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    connection: Option<&crate::host::AgentConnection>,
    database: Option<&str>,
    class: &StatementClass,
    statement: &str,
) -> ToolResult<()> {
    let Some(connection) = connection else {
        return Ok(());
    };
    let risk = match class {
        StatementClass::Destructive(_) => Risk::Destructive,
        _ => Risk::Write,
    };
    state
        .host
        .approve(ApprovalRequest {
            session_id: ctx.session_id.clone(),
            client_name: ctx.client_name.clone(),
            client_version: ctx.client_version.clone(),
            risk,
            connection: Some(connection.config.name.clone()),
            profile: Some(connection.profile.name.clone()),
            database: database.map(str::to_string),
            exposure: Some(connection.config.agent_access.as_str().to_string()),
            statement: Some(statement.to_string()),
            target: None,
            consequences: None,
        })
        .await
}

fn check_read_only(
    connection: &crate::host::AgentConnection,
    class: &StatementClass,
) -> ToolResult<()> {
    if class.modifies() {
        if connection.config.agent_access == AgentAccess::Read {
            return Err(ToolError::read_only("This statement"));
        }
        if connection.profile.read_only {
            return Err(ToolError::read_only("This statement"));
        }
    }
    Ok(())
}

fn check_approval(connection: &crate::host::AgentConnection, class: &StatementClass) -> bool {
    policy::approval_needed(&connection.profile, class)
}

async fn run_query(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    args: &Args,
) -> ToolResult<CallToolResult> {
    let id = args.str("connection_id")?;
    let query = args.str("query")?;
    if query.trim().is_empty() {
        return Err(ToolError::invalid("query must not be empty"));
    }
    let database = args.opt_str("database");
    let max_rows = results::default_rows(args.opt_i64("max_rows")?)?;
    let timeout_s = args.opt_i64("timeout_s")?.unwrap_or(30);
    if !(1..=120).contains(&timeout_s) {
        return Err(ToolError::invalid("timeout_s must be between 1 and 120"));
    }
    let deadline = Duration::from_secs(timeout_s as u64);
    let connection = exposed_connection(state, id).await?;
    let key = ensure_connected(state, ctx, &connection).await?;
    match connection.manifest.family {
        Family::Relational => {
            run_sql_query(
                state,
                ctx,
                &connection,
                &key,
                database,
                query,
                max_rows,
                deadline,
            )
            .await
        }
        Family::Document => {
            run_document_query(
                state,
                ctx,
                &connection,
                &key,
                database,
                query,
                max_rows,
                deadline,
            )
            .await
        }
        Family::KeyValue => {
            run_key_value_query(state, ctx, &connection, database, query, max_rows, deadline).await
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the server state, tool context, connection, session key, database, text, row cap and deadline are independent values resolved by the caller before dispatch; a parameter struct would only be built to be unpacked at the top of this function"
)]
async fn run_sql_query(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    connection: &crate::host::AgentConnection,
    key: &SessionKey,
    database: Option<&str>,
    text: &str,
    max_rows: usize,
    deadline: Duration,
) -> ToolResult<CallToolResult> {
    let driver = state.engine.drivers().get(&connection.config.driver);
    let dialect = driver.and_then(|d| d.dialect());
    let Some(dialect) = dialect else {
        return Err(ToolError::unsupported("This driver has no SQL dialect"));
    };
    let syntax = dialect.syntax();
    let statements = policy::sql_statements(&syntax, text);
    if statements.len() > 1 {
        return Err(ToolError::invalid(format!(
            "run_query takes exactly one statement; {count} were given. Use an editor tab for scripts.",
            count = statements.len()
        )));
    }
    let Some(statement) = statements.first() else {
        return Err(ToolError::invalid("query contains no statement"));
    };
    let class = syntax.classify(statement);
    check_read_only(connection, &class)?;
    if check_approval(connection, &class) {
        approve(state, ctx, Some(connection), database, &class, statement).await?;
    }
    let q = state
        .engine
        .execute_in(key, database, statement.clone(), max_rows);
    let query_id = state
        .results
        .insert(&ctx.session_id, results::ResultKind::Tabular, q, None);
    wait_query(state.engine.as_ref(), q, deadline).await;
    let (status, elapsed, error, page) =
        results::tabular_page(state.engine.as_ref(), q, 0, max_rows)?;
    let value = results::outcome_json(
        "tabular",
        &status,
        Some(&query_id),
        elapsed,
        page.truncated,
        error,
        Some(page),
        None,
        None,
    );
    Ok(CallToolResult::structured(value))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the server state, tool context, connection, session key, database, text, row cap and deadline are independent values resolved by the caller before dispatch; a parameter struct would only be built to be unpacked at the top of this function"
)]
async fn run_document_query(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    connection: &crate::host::AgentConnection,
    key: &SessionKey,
    database: Option<&str>,
    text: &str,
    max_rows: usize,
    deadline: Duration,
) -> ToolResult<CallToolResult> {
    let class = state.engine.doc_classify(&connection.config.id, text);
    check_read_only(connection, &class)?;
    if check_approval(connection, &class) {
        approve(state, ctx, Some(connection), database, &class, text).await?;
    }
    let q = state
        .engine
        .run_doc(key, database, text.to_string(), max_rows);
    let query_id = state
        .results
        .insert(&ctx.session_id, results::ResultKind::Document, q, None);
    wait_docs(state.engine.as_ref(), q, deadline).await;
    let (status, elapsed, error, page) =
        results::document_page(state.engine.as_ref(), q, 0, max_rows)?;
    let value = results::outcome_json(
        "document",
        &status,
        Some(&query_id),
        elapsed,
        page.truncated,
        error,
        None,
        Some(page),
        None,
    );
    Ok(CallToolResult::structured(value))
}

async fn run_key_value_query(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    connection: &crate::host::AgentConnection,
    database: Option<&str>,
    text: &str,
    _max_rows: usize,
    deadline: Duration,
) -> ToolResult<CallToolResult> {
    let upper = text.trim_start().to_ascii_uppercase();
    let first = upper.split_whitespace().next().unwrap_or("");
    if matches!(
        first,
        "MULTI" | "EXEC" | "DISCARD" | "MONITOR" | "SUBSCRIBE" | "PSUBSCRIBE"
    ) || first.starts_with("BL")
    {
        return Err(ToolError::unsupported(
            "MULTI blocks, blocking commands, MONITOR and SUBSCRIBE are not available through run_query",
        ));
    }
    let Some(class) = state.engine.kv_classify(&connection.config.id, text) else {
        return Err(ToolError::invalid("The command does not parse"));
    };
    check_read_only(connection, &class)?;
    if check_approval(connection, &class) && class.modifies() {
        approve(state, ctx, Some(connection), database, &class, text).await?;
    }
    let tab = 1_000_000 + NEXT_KV_CONSOLE.fetch_add(1, Ordering::Relaxed);
    let Some(job) = state.engine.kv_command(&connection.config.id, tab, text) else {
        return Err(ToolError::invalid("The command does not parse"));
    };
    let _ = job;
    let start = Instant::now();
    loop {
        let console = state.engine.kv_console(&connection.config.id, tab);
        if !console.running
            && !console.output.is_empty()
            && let Some(entry) = console.output.last()
        {
            let reply = results::resp_json(&entry.1);
            let id = state.results.insert_reply(&ctx.session_id, &entry.1);
            let value = results::outcome_json(
                "reply",
                "ready",
                Some(&id),
                start.elapsed().as_millis(),
                false,
                None,
                None,
                None,
                Some(reply),
            );
            state.engine.kv_close_console(&connection.config.id, tab);
            return Ok(CallToolResult::structured(value));
        }
        if start.elapsed() >= deadline {
            let value = results::outcome_json(
                "reply",
                "running",
                None,
                start.elapsed().as_millis(),
                false,
                None,
                None,
                None,
                Some(Value::Null),
            );
            return Ok(CallToolResult::structured(value));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_query(engine: &Engine, q: QueryId, deadline: Duration) {
    let end = Instant::now() + deadline;
    loop {
        let status = engine.result(q).map(|r| r.read().status.clone());
        if status.as_ref() != Some(&QueryStatus::Running) {
            return;
        }
        if Instant::now() >= end {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_docs(engine: &Engine, q: QueryId, deadline: Duration) {
    let end = Instant::now() + deadline;
    loop {
        let status = engine.doc_result(q).map(|r| r.read().status.clone());
        if status.as_ref() != Some(&QueryStatus::Running) {
            return;
        }
        if Instant::now() >= end {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn get_query_result(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    args: &Args,
) -> ToolResult<CallToolResult> {
    let id = args.str("query_id")?;
    let offset = args.opt_u64("offset")?.unwrap_or(0) as usize;
    let limit = results::default_rows(args.opt_i64("limit")?)?;
    state.results.touch(id);
    if let Some(reply) = state.results.reply(id) {
        let value = results::outcome_json(
            "reply",
            "ready",
            Some(id),
            0,
            false,
            None,
            None,
            None,
            Some(reply),
        );
        return Ok(CallToolResult::structured(value));
    }
    let Some((q, kind)) = state.results.get(id) else {
        return Err(ToolError::not_found("The result expired or was closed"));
    };
    let value = match kind {
        results::ResultKind::Tabular => {
            let (status, elapsed, error, page) =
                results::tabular_page(state.engine.as_ref(), q, offset, limit)?;
            results::outcome_json(
                "tabular",
                &status,
                Some(id),
                elapsed,
                page.truncated,
                error,
                Some(page),
                None,
                None,
            )
        }
        results::ResultKind::Document => {
            let (status, elapsed, error, page) =
                results::document_page(state.engine.as_ref(), q, offset, limit)?;
            results::outcome_json(
                "document",
                &status,
                Some(id),
                elapsed,
                page.truncated,
                error,
                None,
                Some(page),
                None,
            )
        }
        results::ResultKind::Reply => {
            return Err(ToolError::not_found("A reply has no pages"));
        }
    };
    let mut value = value;
    if let Some(object) = value.as_object_mut() {
        object.insert("offset".into(), json!(offset));
    }
    let _ = ctx;
    Ok(CallToolResult::structured(value))
}

async fn cancel_query(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    args: &Args,
) -> ToolResult<CallToolResult> {
    let id = args.str("query_id")?;
    state.results.touch(id);
    let Some((q, kind)) = state.results.get(id) else {
        return Err(ToolError::not_found("The query id is unknown"));
    };
    match kind {
        results::ResultKind::Document => state.engine.cancel_docs(q),
        _ => state.engine.cancel(q),
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let status = match kind {
        results::ResultKind::Document => state.engine.doc_result(q).map_or_else(
            || "closed".into(),
            |result| status_name(&result.read().status).to_string(),
        ),
        _ => state.engine.result(q).map_or_else(
            || "closed".into(),
            |result| status_name(&result.read().status).to_string(),
        ),
    };
    let _ = ctx;
    Ok(CallToolResult::structured(json!({
        "query_id": id,
        "status": status,
    })))
}

async fn close_result(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    args: &Args,
) -> ToolResult<CallToolResult> {
    let id = args.str("query_id")?;
    let Some((q, kind)) = state.results.remove(id) else {
        return Ok(CallToolResult::structured(
            json!({"query_id": id, "closed": false}),
        ));
    };
    if let Some(q) = q {
        results::close_engine_result(state.engine.as_ref(), q, kind);
    }
    let _ = ctx;
    Ok(CallToolResult::structured(
        json!({"query_id": id, "closed": true}),
    ))
}

// ----- config -----

async fn config_list(state: &Arc<ServerState>, args: &Args) -> ToolResult<CallToolResult> {
    let target = args.str("target")?;
    let value = match target {
        "connections" => {
            let mut out = Vec::new();
            for config in state.host.connections().await? {
                if config.agent_access == AgentAccess::None {
                    continue;
                }
                let secret_fields = state.host.secret_presence(&config.id).await?;
                out.push(json!({
                    "id": config.id,
                    "name": config.name,
                    "driver": config.driver,
                    "group": config.group,
                    "profile": config.profile.clone().unwrap_or_else(|| ConnectionProfile::DEVELOPMENT.to_string()),
                    "agent_access": config.agent_access.as_str(),
                    "secret_fields": secret_fields,
                }));
            }
            json!({"target": "connections", "connections": out})
        }
        "folders" => json!({"target": "folders", "folders": state.host.folders().await?}),
        "profiles" => json!({"target": "profiles", "profiles": state.host.profiles().await?}),
        "settings" => json!({"target": "settings", "settings": state.host.settings().await?}),
        "keymap" => json!({"target": "keymap", "actions": state.host.keymap().await?}),
        "tasks" => json!({"target": "tasks", "tasks": state.host.tasks().await?}),
        "drivers" => {
            let only = args.opt_str("driver_id");
            let manifests: Vec<Value> = state
                .host
                .drivers()
                .await?
                .iter()
                .filter(|manifest| only.is_none_or(|id| manifest.id == id))
                .map(policy::manifest_json)
                .collect();
            json!({"target": "drivers", "drivers": manifests})
        }
        other => {
            return Err(ToolError::invalid(format!(
                "Unknown config_list target \"{other}\""
            )));
        }
    };
    Ok(CallToolResult::structured(value))
}

async fn config_write(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    args: &Args,
) -> ToolResult<CallToolResult> {
    let target = args.str("target")?;
    let payload = args.object("payload")?;
    let payload = Args(payload.clone());
    let value = match target {
        "connection" => write_connection(state, &payload).await?,
        "folder" => write_folder(state, &payload).await?,
        "profile" => write_profile(state, &payload).await?,
        "setting" => write_setting(state, &payload).await?,
        "keymap" => write_keymap(state, &payload).await?,
        other => {
            return Err(ToolError::invalid(format!(
                "Unknown config_write target \"{other}\""
            )));
        }
    };
    let _ = ctx;
    Ok(CallToolResult::structured(value))
}

async fn write_connection(state: &Arc<ServerState>, payload: &Args) -> ToolResult<Value> {
    if payload.get("agent_access").is_some() {
        return Err(ToolError::invalid(
            "agent_access is human-only and cannot be written by an agent",
        ));
    }
    let drivers = state.host.drivers().await?;
    let existing = match payload.opt_str("id") {
        Some(id) => Some(state.host.connection(id).await?),
        None => None,
    };
    let driver_id = payload
        .opt_str("driver")
        .map(str::to_string)
        .or_else(|| existing.as_ref().map(|c| c.config.driver.clone()))
        .ok_or_else(|| ToolError::invalid("create requires a driver"))?;
    if let Some(existing) = &existing
        && payload
            .opt_str("driver")
            .is_some_and(|driver| driver != existing.config.driver)
    {
        return Err(ToolError::invalid(
            "A connection's driver cannot be changed",
        ));
    }
    let manifest = drivers
        .iter()
        .find(|manifest| manifest.id == driver_id)
        .ok_or_else(|| ToolError::invalid(format!("Unknown driver \"{driver_id}\"")))?
        .clone();
    let mut config = existing.as_ref().map_or_else(
        || ConnectionConfig {
            id: new_id("agent"),
            name: String::new(),
            driver: driver_id.clone(),
            group: None,
            color: None,
            values: BTreeMap::new(),
            profile: None,
            save_secrets: true,
            agent_access: AgentAccess::None,
        },
        |c| c.config.clone(),
    );
    if let Some(name) = payload.opt_str("name") {
        if name.trim().is_empty() {
            return Err(ToolError::invalid("A connection needs a name"));
        }
        config.name = name.to_string();
    }
    if existing.is_none() && config.name.is_empty() {
        return Err(ToolError::invalid("create requires a name"));
    }
    if let Some(group) = payload.get("group") {
        config.group = match group {
            Value::Null => None,
            Value::String(value) => Some(value.clone()),
            _ => return Err(ToolError::invalid("group must be a string or null")),
        };
    }
    if let Some(color) = payload.get("color") {
        config.color = match color {
            Value::Null => None,
            value => {
                let array = value
                    .as_array()
                    .ok_or_else(|| ToolError::invalid("color must be [r, g, b] or null"))?;
                let bytes: Vec<u8> = array
                    .iter()
                    .filter_map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
                    .collect();
                Some(
                    <[u8; 3]>::try_from(bytes)
                        .map_err(|_| ToolError::invalid("color must be [r, g, b]"))?,
                )
            }
        };
    }
    if let Some(profile_id) = payload.get("profile") {
        let profile_id = match profile_id {
            Value::Null => None,
            Value::String(value) => Some(value.as_str()),
            _ => return Err(ToolError::invalid("profile must be a string or null")),
        };
        if let Some(profile_id) = profile_id {
            let profiles = state.host.profiles().await?;
            let target = profiles
                .iter()
                .find(|p| p.id == profile_id)
                .ok_or_else(|| ToolError::invalid(format!("Unknown profile \"{profile_id}\"")))?;
            if let Some(existing) = &existing {
                let current_id = existing
                    .config
                    .profile
                    .clone()
                    .unwrap_or_else(|| ConnectionProfile::DEVELOPMENT.to_string());
                let current = profiles
                    .iter()
                    .find(|p| p.id == current_id)
                    .cloned()
                    .unwrap_or_else(ConnectionProfile::development);
                policy::profile_is_at_least_as_strict(&current, target).map_err(|reason| {
                    ToolError::invalid(format!("The profile cannot be loosened: {reason}"))
                })?;
            }
            config.profile = Some(profile_id.to_string());
        } else {
            config.profile = None;
        }
    }
    if let Some(save_secrets) = payload.get("save_secrets") {
        config.save_secrets = save_secrets
            .as_bool()
            .ok_or_else(|| ToolError::invalid("save_secrets must be a boolean"))?;
    }
    let mut secrets = BTreeMap::new();
    if let Some(values) = payload.get("values") {
        let values = values
            .as_object()
            .ok_or_else(|| ToolError::invalid("values must be an object"))?;
        for (key, value) in values {
            let value = value
                .as_str()
                .ok_or_else(|| ToolError::invalid(format!("value {key} must be a string")))?;
            validate_value_key(&manifest, key)?;
            let old = config.values.get(key).map(String::as_str);
            policy::transport_security_ok(key, old, value)?;
            config.values.insert(key.clone(), value.to_string());
        }
    }
    if let Some(remove) = payload.get("remove_values") {
        let keys = remove
            .as_array()
            .ok_or_else(|| ToolError::invalid("remove_values must be an array"))?;
        for key in keys {
            let key = key
                .as_str()
                .ok_or_else(|| ToolError::invalid("remove_values must contain strings"))?;
            if policy::is_transport_security_field(key) {
                return Err(ToolError::invalid(format!(
                    "{key} weakens transport security and cannot be removed by an agent"
                )));
            }
            config.values.remove(key);
        }
    }
    if let Some(values) = payload.get("secrets") {
        let values = values
            .as_object()
            .ok_or_else(|| ToolError::invalid("secrets must be an object"))?;
        for (key, value) in values {
            if !manifest.fields.iter().any(|f| f.secret && &f.key == key) {
                return Err(ToolError::invalid(format!(
                    "\"{key}\" is not a secret field of this driver"
                )));
            }
            let value = value
                .as_str()
                .ok_or_else(|| ToolError::invalid(format!("secret {key} must be a string")))?;
            secrets.insert(key.clone(), value.to_string());
        }
    }
    let mut remove_secrets = Vec::new();
    if let Some(remove) = payload.get("remove_secrets") {
        let keys = remove
            .as_array()
            .ok_or_else(|| ToolError::invalid("remove_secrets must be an array"))?;
        for key in keys {
            let key = key
                .as_str()
                .ok_or_else(|| ToolError::invalid("remove_secrets must contain strings"))?;
            if !manifest.fields.iter().any(|f| f.secret && f.key == key) {
                return Err(ToolError::invalid(format!(
                    "\"{key}\" is not a secret field of this driver"
                )));
            }
            remove_secrets.push(key.to_string());
        }
    }
    let created = existing.is_none();
    let id = config.id.clone();
    state
        .host
        .write_connection(ConnectionWrite {
            config,
            secrets,
            remove_secrets,
            save_secrets: payload.get("save_secrets").and_then(Value::as_bool),
        })
        .await?;
    Ok(json!({
        "id": id,
        "created": created,
    }))
}

fn validate_value_key(manifest: &DriverManifest, key: &str) -> ToolResult<()> {
    if key.starts_with("property.") || key == sqlr_core::TLS_SERVER_NAME {
        return Ok(());
    }
    if manifest
        .fields
        .iter()
        .any(|field| field.key == key && !field.secret)
    {
        Ok(())
    } else {
        Err(ToolError::invalid(format!(
            "\"{key}\" is not a value field of driver \"{manifest_id}\"",
            manifest_id = manifest.id
        )))
    }
}

async fn write_folder(state: &Arc<ServerState>, payload: &Args) -> ToolResult<Value> {
    let action = payload.str("action")?;
    match action {
        "create" => {
            let name = payload.str("name")?;
            state
                .host
                .folder(FolderOp::Create {
                    name: name.to_string(),
                })
                .await?;
            Ok(json!({"action": "create", "name": name}))
        }
        "rename" => {
            let from = payload.str("from")?;
            let to = payload.str("to")?;
            state
                .host
                .folder(FolderOp::Rename {
                    from: from.to_string(),
                    to: to.to_string(),
                })
                .await?;
            Ok(json!({"action": "rename", "from": from, "to": to}))
        }
        "move" => {
            let name = payload.str("name")?;
            let target = payload.object("target")?;
            let target = match target.get("at").and_then(Value::as_str) {
                Some("end") => MoveTarget::End,
                Some(at @ ("before" | "after" | "into")) => {
                    let id = target
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| ToolError::invalid("move target needs an id"))?;
                    match at {
                        "before" => MoveTarget::Before { id: id.into() },
                        "after" => MoveTarget::After { id: id.into() },
                        _ => MoveTarget::Into { id: id.into() },
                    }
                }
                _ => {
                    return Err(ToolError::invalid(
                        "target.at must be end, before, after or into",
                    ));
                }
            };
            state
                .host
                .folder(FolderOp::Move {
                    name: name.to_string(),
                    target,
                })
                .await?;
            Ok(json!({"action": "move", "name": name}))
        }
        other => Err(ToolError::invalid(format!(
            "Unknown folder action \"{other}\""
        ))),
    }
}

async fn write_profile(state: &Arc<ServerState>, payload: &Args) -> ToolResult<Value> {
    let profiles = state.host.profiles().await?;
    let existing = payload
        .opt_str("id")
        .and_then(|id| profiles.iter().find(|p| p.id == id).cloned());
    if payload.opt_str("id").is_some() && existing.is_none() {
        return Err(ToolError::invalid("Unknown profile"));
    }
    let mut profile = existing.clone().unwrap_or_else(|| ConnectionProfile {
        id: new_id("profile"),
        name: String::new(),
        color: None,
        commit_mode: CommitMode::Auto,
        confirm_writes: false,
        confirm_destructive: true,
        read_only: false,
        idle_tx_timeout_min: None,
    });
    if let Some(name) = payload.opt_str("name") {
        if name.trim().is_empty() {
            return Err(ToolError::invalid("A profile needs a name"));
        }
        profile.name = name.to_string();
    }
    if existing.is_none() && profile.name.is_empty() {
        return Err(ToolError::invalid("create requires a name"));
    }
    if let Some(color) = payload.get("color") {
        profile.color = match color {
            Value::Null => None,
            value => {
                let array = value
                    .as_array()
                    .ok_or_else(|| ToolError::invalid("color must be [r, g, b] or null"))?;
                let bytes: Vec<u8> = array
                    .iter()
                    .filter_map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
                    .collect();
                Some(
                    <[u8; 3]>::try_from(bytes)
                        .map_err(|_| ToolError::invalid("color must be [r, g, b]"))?,
                )
            }
        };
    }
    if let Some(mode) = payload.opt_str("commit_mode") {
        profile.commit_mode = match mode {
            "auto" => CommitMode::Auto,
            "manual" => CommitMode::Manual,
            "smart" => CommitMode::Smart,
            _ => {
                return Err(ToolError::invalid(
                    "commit_mode must be auto, manual or smart",
                ));
            }
        };
    }
    for (key, field) in [
        ("confirm_writes", &mut profile.confirm_writes),
        ("confirm_destructive", &mut profile.confirm_destructive),
        ("read_only", &mut profile.read_only),
    ] {
        if let Some(value) = payload.get(key) {
            let value = value
                .as_bool()
                .ok_or_else(|| ToolError::invalid(format!("{key} must be a boolean")))?;
            if existing.as_ref().is_some_and(|old| match key {
                "confirm_writes" => old.confirm_writes,
                "confirm_destructive" => old.confirm_destructive,
                _ => old.read_only,
            }) && !value
            {
                return Err(ToolError::invalid(format!(
                    "{key} is a safety flag and cannot be disabled by an agent"
                )));
            }
            *field = value;
        }
    }
    if let Some(timeout) = payload.get("idle_tx_timeout_min") {
        profile.idle_tx_timeout_min = match timeout {
            Value::Null => None,
            value => Some(
                value
                    .as_u64()
                    .filter(|n| *n <= u32::MAX as u64)
                    .ok_or_else(|| {
                        ToolError::invalid(
                            "idle_tx_timeout_min must be a non-negative integer or null",
                        )
                    })? as u32,
            ),
        };
        if let Some(old) = existing.as_ref().and_then(|p| p.idle_tx_timeout_min) {
            match profile.idle_tx_timeout_min {
                Some(new) if new <= old => {}
                _ => {
                    return Err(ToolError::invalid(
                        "idle_tx_timeout_min is a safety value and can only decrease",
                    ));
                }
            }
        }
    }
    if let Some(old) = &existing {
        let rank = |mode| match mode {
            CommitMode::Manual => 0,
            CommitMode::Auto => 2,
            CommitMode::Smart => 1,
        };
        if rank(profile.commit_mode) > rank(old.commit_mode) {
            return Err(ToolError::invalid(
                "commit_mode can only become stricter (auto → smart → manual)",
            ));
        }
    }
    let id = profile.id.clone();
    state.host.save_profile(profile).await?;
    Ok(json!({"id": id}))
}

fn parse_level(value: &Value) -> ToolResult<(SettingLevel, Option<String>)> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::invalid("level must be an object"))?;
    match object.get("name").and_then(Value::as_str) {
        Some("global") => Ok((SettingLevel::Global, None)),
        Some("profile") => Ok((
            SettingLevel::Profile,
            Some(
                object
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::invalid("profile level needs an id"))?
                    .to_string(),
            ),
        )),
        Some("connection") => Ok((
            SettingLevel::Connection,
            Some(
                object
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::invalid("connection level needs an id"))?
                    .to_string(),
            ),
        )),
        _ => Err(ToolError::invalid(
            "level.name must be global, profile or connection",
        )),
    }
}

async fn write_setting(state: &Arc<ServerState>, payload: &Args) -> ToolResult<Value> {
    let id = payload.str("id")?;
    let (level, level_id) = parse_level(
        payload
            .get("level")
            .ok_or_else(|| ToolError::invalid("missing level"))?,
    )?;
    let settings = state.host.settings().await?;
    let entry = settings
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| ToolError::invalid(format!("Unknown setting \"{id}\"")))?;
    if !entry.agent_writable || matches!(id, "terminal.shell") || id.starts_with("agents.") {
        return Err(ToolError::invalid(format!(
            "\"{id}\" is human-only and cannot be written by an agent"
        )));
    }
    let level_key = match level {
        SettingLevel::Global => "global".to_string(),
        SettingLevel::Profile => format!("profile:{}", level_id.as_deref().unwrap_or_default()),
        SettingLevel::Connection => {
            format!("connection:{}", level_id.as_deref().unwrap_or_default())
        }
    };
    let level_category = match level {
        SettingLevel::Global => "global",
        SettingLevel::Profile => "profile",
        SettingLevel::Connection => "connection",
    };
    if !entry
        .allowed_levels
        .iter()
        .any(|allowed| allowed == level_category)
    {
        return Err(ToolError::invalid(format!(
            "\"{id}\" cannot be set at this level"
        )));
    }
    let reset = payload
        .get("reset")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let current_true = entry
        .values
        .values()
        .any(|value| value.as_bool() == Some(true));
    if reset {
        if entry.safety == "tighten_only" && current_true {
            return Err(ToolError::invalid(format!(
                "Resetting \"{id}\" would loosen a tightened safety setting"
            )));
        }
        state
            .host
            .set_setting(id, level, level_id.as_deref(), None)
            .await?;
        return Ok(json!({"id": id, "reset": true, "level": level_key}));
    }
    let value = payload
        .get("value")
        .ok_or_else(|| ToolError::invalid("setting writes need value or reset"))?;
    let parsed = parse_setting_json(id, &entry.kind, value)?;
    if entry.safety == "tighten_only" && !parsed.as_bool() && current_true {
        return Err(ToolError::invalid(format!(
            "\"{id}\" is a safety setting and can only be tightened"
        )));
    }
    state
        .host
        .set_setting(id, level, level_id.as_deref(), Some(parsed))
        .await?;
    Ok(json!({"id": id, "level": level_key}))
}

fn parse_setting_json(id: &str, kind: &Value, value: &Value) -> ToolResult<SettingValue> {
    let ty = kind
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::internal("setting kind has no type"))?;
    match ty {
        "bool" => value
            .as_bool()
            .map(SettingValue::Bool)
            .ok_or_else(|| ToolError::invalid(format!("{id} expects a boolean"))),
        "int" => {
            let n = value
                .as_i64()
                .ok_or_else(|| ToolError::invalid(format!("{id} expects an integer")))?;
            let min = kind.get("min").and_then(Value::as_i64).unwrap_or(i64::MIN);
            let max = kind.get("max").and_then(Value::as_i64).unwrap_or(i64::MAX);
            if (min..=max).contains(&n) {
                Ok(SettingValue::Int(n))
            } else {
                Err(ToolError::invalid(format!(
                    "{id} must be between {min} and {max}"
                )))
            }
        }
        "float" => {
            let n = value
                .as_f64()
                .ok_or_else(|| ToolError::invalid(format!("{id} expects a number")))?;
            let min = kind.get("min").and_then(Value::as_f64).unwrap_or(f64::MIN);
            let max = kind.get("max").and_then(Value::as_f64).unwrap_or(f64::MAX);
            if n.is_finite() && (min..=max).contains(&n) {
                Ok(SettingValue::Float(n))
            } else {
                Err(ToolError::invalid(format!(
                    "{id} must be between {min} and {max}"
                )))
            }
        }
        "text" => value
            .as_str()
            .map(|s| SettingValue::Text(s.to_string()))
            .ok_or_else(|| ToolError::invalid(format!("{id} expects text"))),
        "path" => value
            .as_str()
            .map(|s| SettingValue::Path(s.to_string()))
            .ok_or_else(|| ToolError::invalid(format!("{id} expects a path"))),
        "choice" => {
            let text = value
                .as_str()
                .ok_or_else(|| ToolError::invalid(format!("{id} expects a choice id")))?;
            let options = kind
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if options
                .iter()
                .any(|option| option.get("id").and_then(Value::as_str) == Some(text))
            {
                Ok(SettingValue::Choice(text.to_string()))
            } else {
                Err(ToolError::invalid(format!(
                    "{text:?} is not an option of {id}"
                )))
            }
        }
        "color" => {
            let array = value
                .as_array()
                .ok_or_else(|| ToolError::invalid(format!("{id} expects [r, g, b]")))?;
            let bytes: Vec<u8> = array
                .iter()
                .filter_map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
                .collect();
            <[u8; 3]>::try_from(bytes)
                .map(SettingValue::Color)
                .map_err(|_| ToolError::invalid(format!("{id} expects [r, g, b]")))
        }
        other => Err(ToolError::internal(format!(
            "setting kind \"{other}\" is not writable through MCP"
        ))),
    }
}

async fn write_keymap(state: &Arc<ServerState>, payload: &Args) -> ToolResult<Value> {
    let action = payload.str("action")?;
    let action_id = ActionId::from_id(action)
        .ok_or_else(|| ToolError::invalid(format!("Unknown action \"{action}\"")))?;
    let reset = payload
        .get("reset")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if reset {
        state.host.set_keymap(action, None).await?;
        return Ok(json!({"action": action, "reset": true}));
    }
    let chords = payload.string_array("chords")?;
    for chord in &chords {
        Chord::parse(chord).ok_or_else(|| {
            ToolError::invalid(format!("\"{chord}\" is not a chord like Cmd+Shift+P"))
        })?;
    }
    state
        .host
        .set_keymap(action_id.id(), Some(chords.clone()))
        .await?;
    Ok(json!({"action": action, "chords": chords}))
}

async fn config_delete(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    args: &Args,
) -> ToolResult<CallToolResult> {
    let target = args.str("target")?;
    let id = args.str("id")?;
    let connections = state.host.connections().await?;
    let (target_label, consequences) = match target {
        "connection" => {
            let config = connections
                .iter()
                .find(|config| config.id == id)
                .ok_or_else(|| ToolError::not_found("Unknown connection"))?;
            (
                format!("Connection \"{}\"", config.name),
                "The connection is disconnected and its saved secrets are removed.".to_string(),
            )
        }
        "folder" => {
            let prefix = format!("{id}/");
            let connections_inside = connections
                .iter()
                .filter(|config| {
                    config
                        .group
                        .as_deref()
                        .is_some_and(|group| group == id || group.starts_with(&prefix))
                })
                .count();
            let folders = state.host.folders().await?;
            let subfolders = folders
                .iter()
                .filter(|folder| folder.as_str() != id && folder.starts_with(&prefix))
                .count();
            (
                format!("Folder \"{id}\" ({connections_inside} connection(s), {subfolders} subfolder(s))"),
                "The folder is removed; its connections and subfolders move to the parent. No connection is deleted."
                    .to_string(),
            )
        }
        "profile" => {
            if connections.iter().any(|config| {
                config
                    .profile
                    .as_deref()
                    .unwrap_or(ConnectionProfile::DEVELOPMENT)
                    == id
            }) {
                return Err(ToolError::invalid(
                    "The profile is in use by a connection and cannot be deleted",
                ));
            }
            (
                format!("Profile \"{id}\""),
                "The profile definition is removed.".to_string(),
            )
        }
        "task" => (
            format!("Task \"{id}\""),
            "The task definition is removed; it is never run.".to_string(),
        ),
        other => {
            return Err(ToolError::invalid(format!(
                "Unknown config_delete target \"{other}\""
            )));
        }
    };
    state
        .host
        .approve(ApprovalRequest {
            session_id: ctx.session_id.clone(),
            client_name: ctx.client_name.clone(),
            client_version: ctx.client_version.clone(),
            risk: Risk::ConfigurationDelete,
            connection: None,
            profile: None,
            database: None,
            exposure: None,
            statement: None,
            target: Some(target_label.clone()),
            consequences: Some(consequences),
        })
        .await?;
    state.host.delete(target, id).await?;
    Ok(CallToolResult::structured(json!({
        "target": target,
        "id": id,
        "deleted": true,
    })))
}

// ----- editor -----

async fn list_query_tabs(state: &Arc<ServerState>) -> ToolResult<CallToolResult> {
    let tabs = state.host.tabs().await?;
    Ok(CallToolResult::structured(json!({
        "tabs": tabs,
    })))
}

async fn get_query_tab(state: &Arc<ServerState>, args: &Args) -> ToolResult<CallToolResult> {
    let id = args.u64("tab_id")?;
    let tab = state.host.tab(id).await?;
    Ok(CallToolResult::structured(json!({
        "tab_id": tab.id,
        "title": tab.title,
        "connection_id": tab.connection_id,
        "database": tab.database,
        "dirty": tab.dirty,
        "revision": tab.revision,
        "busy": tab.busy,
        "text": tab.text,
        "caret": tab.caret,
        "selection": tab.selection.map(|(start, end)| json!({"start": start, "end": end})),
    })))
}

async fn edit_query_tab(state: &Arc<ServerState>, args: &Args) -> ToolResult<CallToolResult> {
    let id = args.u64("tab_id")?;
    let expected = args.u64("expected_revision")?;
    let edit = args.object("edit")?;
    let edit = if let Some(replace) = edit.get("replace").and_then(Value::as_str) {
        TabEdit::Replace(replace.to_string())
    } else if let Some(edits) = edit.get("edits").and_then(Value::as_array) {
        let mut ranges = Vec::new();
        for range in edits {
            let range = range
                .as_object()
                .ok_or_else(|| ToolError::invalid("each edit must be an object"))?;
            let start = range
                .get("start")
                .and_then(Value::as_u64)
                .ok_or_else(|| ToolError::invalid("edit.start must be an integer"))?
                as usize;
            let end = range
                .get("end")
                .and_then(Value::as_u64)
                .ok_or_else(|| ToolError::invalid("edit.end must be an integer"))?
                as usize;
            let text = range
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::invalid("edit.text must be a string"))?;
            if start > end {
                return Err(ToolError::invalid("edit ranges need start <= end"));
            }
            ranges.push(EditRange {
                start,
                end,
                text: text.to_string(),
            });
        }
        TabEdit::Edits(ranges)
    } else {
        return Err(ToolError::invalid("edit needs replace or edits"));
    };
    let tab = state.host.edit_tab(id, expected, edit).await?;
    Ok(CallToolResult::structured(json!({
        "tab_id": tab.id,
        "revision": tab.revision,
        "dirty": tab.dirty,
    })))
}

async fn run_query_tab(state: &Arc<ServerState>, args: &Args) -> ToolResult<CallToolResult> {
    let id = args.u64("tab_id")?;
    let target = match args.get("target") {
        Some(Value::String(value)) if value == "script" => RunTarget::Script,
        Some(Value::String(value)) if value == "selection" => RunTarget::Selection,
        Some(Value::Object(object)) => {
            let index = object
                .get("statement")
                .and_then(Value::as_u64)
                .ok_or_else(|| ToolError::invalid("target.statement must be an index"))?;
            RunTarget::Statement(index as usize)
        }
        Some(Value::String(value)) => {
            return Err(ToolError::invalid(format!(
                "Unknown run target \"{value}\""
            )));
        }
        _ => {
            return Err(ToolError::invalid(
                "target must be script, selection or {statement: n}",
            ));
        }
    };
    let max_rows = results::default_rows(args.opt_i64("max_rows")?)?;
    let tab = state.host.tab(id).await?;
    if tab.busy {
        return Err(ToolError::busy());
    }
    let Some(connection_id) = tab.connection_id.clone() else {
        return Err(ToolError::invalid("The tab has no connection"));
    };
    let connection = exposed_connection(state, &connection_id).await?;
    let _ = ensure_connected(
        state,
        &ToolContext {
            session_id: format!("tab-{id}"),
            client_name: String::new(),
            client_version: String::new(),
        },
        &connection,
    )
    .await?;
    let value = state.host.run_tab(id, target, max_rows).await?;
    Ok(CallToolResult::structured(value))
}

// ----- snippets -----

fn snippet_metadata(snippet: &Snippet) -> Value {
    json!({
        "id": snippet.id,
        "name": snippet.name,
        "folder": snippet.folder,
        "conn_id": snippet.conn_id,
        "updated_at_ms": snippet.updated_at_ms,
    })
}

fn list_snippets(state: &Arc<ServerState>) -> CallToolResult {
    let snippets = state.engine.snippets();
    CallToolResult::structured(json!({
        "snippets": snippets.iter().map(snippet_metadata).collect::<Vec<_>>(),
    }))
}

fn get_snippet(state: &Arc<ServerState>, args: &Args) -> ToolResult<CallToolResult> {
    let id = args.str("id")?;
    let snippets = state.engine.snippets();
    let snippet = snippets
        .iter()
        .find(|snippet| snippet.id == id)
        .ok_or_else(|| ToolError::not_found("Unknown snippet"))?;
    Ok(CallToolResult::structured(json!({
        "id": snippet.id,
        "name": snippet.name,
        "folder": snippet.folder,
        "conn_id": snippet.conn_id,
        "sql": snippet.sql,
        "updated_at_ms": snippet.updated_at_ms,
    })))
}

async fn write_snippet(state: &Arc<ServerState>, args: &Args) -> ToolResult<CallToolResult> {
    let name = args.str("name")?;
    let sql = args.str("sql")?;
    if name.trim().is_empty() {
        return Err(ToolError::invalid("A snippet needs a name"));
    }
    let snippets = state.engine.snippets();
    let id = args
        .opt_str("id")
        .map(str::to_string)
        .or_else(|| {
            snippets
                .iter()
                .find(|snippet| snippet.name == name)
                .map(|snippet| snippet.id.clone())
        })
        .unwrap_or_else(|| new_id("snippet"));
    let folder = args
        .opt_str("folder")
        .unwrap_or("")
        .trim_matches('/')
        .to_string();
    let conn_id = args.opt_str("conn_id").map(str::to_string);
    let snippet = Snippet {
        id: id.clone(),
        name: name.to_string(),
        folder,
        conn_id,
        sql: sql.to_string(),
        updated_at_ms: now_ms(),
    };
    state.engine.save_snippet(snippet);
    state
        .host
        .toast(&format!("Agent saved snippet \"{name}\""))
        .await;
    Ok(CallToolResult::structured(json!({"id": id})))
}

async fn delete_snippet(
    state: &Arc<ServerState>,
    ctx: &ToolContext,
    args: &Args,
) -> ToolResult<CallToolResult> {
    let id = args.str("id")?;
    let snippets = state.engine.snippets();
    let snippet = snippets
        .iter()
        .find(|snippet| snippet.id == id)
        .cloned()
        .ok_or_else(|| ToolError::not_found("Unknown snippet"))?;
    let folder = if snippet.folder.is_empty() {
        "the top level".to_string()
    } else {
        format!("folder \"{}\"", snippet.folder)
    };
    state
        .host
        .approve(ApprovalRequest {
            session_id: ctx.session_id.clone(),
            client_name: ctx.client_name.clone(),
            client_version: ctx.client_version.clone(),
            risk: Risk::SnippetDelete,
            connection: None,
            profile: None,
            database: None,
            exposure: None,
            statement: None,
            target: Some(format!("Snippet \"{}\" in {folder}", snippet.name)),
            consequences: Some("The snippet is removed from the library.".to_string()),
        })
        .await?;
    state.engine.delete_snippet(id);
    Ok(CallToolResult::structured(
        json!({"id": id, "deleted": true}),
    ))
}

// Keep imports used by the public contract in one place without leaking implementation
// details into tool results.
fn _type_check(_: &ColumnMeta, _: &Arc<()>, _: Slot) {}
