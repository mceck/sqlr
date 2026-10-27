//! MCP Streamable HTTP lifecycle: bind, authenticate, publish discovery, manage sessions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, HOST, ORIGIN, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::any_service;
use futures_util::Stream;
use rmcp::ErrorData as McpError;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, InitializeRequestParams,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::transport::WorkerTransport;
use rmcp::transport::streamable_http_server::session::ServerSseMessage;
use rmcp::transport::streamable_http_server::session::SessionId;
use rmcp::transport::streamable_http_server::session::SessionManager;
use rmcp::transport::streamable_http_server::session::local::{
    LocalSessionManager, LocalSessionManagerError,
};
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{RoleServer, ServerHandler};
use serde_json::json;
use sqlr_engine::Engine;
use sqlr_store::McpTokenStore;
use tokio_util::sync::CancellationToken;

use crate::discovery::{self, Discovery};
use crate::error::ToolError;
use crate::host::AgentHost;
use crate::registry;
use crate::results::ResultKind;
use crate::state::ServerState;
use crate::token::generate_token;

pub const MAX_SESSIONS: usize = 8;
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const INSTRUCTIONS: &str = "Before calling any other tool in each new MCP session, you must call \
get_guide with {} and read its overview. Use get_guide(topic=tools) for argument and configuration \
payload reference, or topic=recipes for JSON examples. Start database work with \
config_list(target=connections), then browse returned IDs and paths; sqlr connects automatically. \
Only connections the person exposed as Read or Write are reachable, and writes still follow \
profile restrictions and approvals. Use run_query for one statement/native command and directly \
pageable MCP data; use run_query_tab for SQL already in an editor tab. Poll/page run_query results \
by query_id instead of rerunning statements, and close_result when done. Connection values and \
credentials are write-only. The integrated terminal is human-only and has no MCP tool.";

pub struct StartOptions {
    pub port: u16,
    pub data_dir: PathBuf,
    /// The application connects on the agent's behalf; the MCP layer connects lazily.
    pub engine: Arc<Engine>,
    pub host: Arc<dyn AgentHost>,
    pub token_store: Arc<dyn McpTokenStore>,
    pub app_version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("port {port} is already in use")]
    PortBusy { port: u16 },
    #[error("the keychain is unavailable: {0}")]
    Keychain(String),
    #[error("the data directory is unavailable: {0}")]
    DataDir(String),
    #[error("could not start the MCP server: {0}")]
    Other(String),
}

#[derive(Clone)]
struct AuthState {
    state: Arc<ServerState>,
}

/// The local session manager with the eight-session cap and cleanup on close.
pub struct CappedSessionManager {
    inner: Arc<LocalSessionManager>,
    state: Arc<ServerState>,
    limit: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum CappedSessionError {
    #[error("too many MCP sessions (at most {0})")]
    TooMany(usize),
    #[error("session: {0}")]
    Session(#[from] LocalSessionManagerError),
}

impl CappedSessionManager {
    pub fn new(state: Arc<ServerState>, limit: usize) -> Self {
        Self {
            inner: Arc::new(LocalSessionManager::default()),
            state,
            limit,
        }
    }

    async fn cleanup(&self, id: &str) {
        self.state.remove_session(id);
        let engine = self.state.engine.clone();
        let results = self.state.results.clone();
        for (query, kind) in results.sweep_owner(id) {
            crate::results::close_engine_result(engine.as_ref(), query, kind);
        }
        engine.close_agent_sessions(id);
    }
}

impl SessionManager for CappedSessionManager {
    type Error = CappedSessionError;
    type Transport = WorkerTransport<
        rmcp::transport::streamable_http_server::session::local::LocalSessionWorker,
    >;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        if self.state.session_count() >= self.limit {
            return Err(CappedSessionError::TooMany(self.limit));
        }
        let (id, transport) = self.inner.create_session().await?;
        self.state.register_session(&id, "", "");
        Ok((id, transport))
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        message: rmcp::model::ClientJsonRpcMessage,
    ) -> Result<rmcp::model::ServerJsonRpcMessage, Self::Error> {
        Ok(self.inner.initialize_session(id, message).await?)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        Ok(self.inner.has_session(id).await?)
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        let result = self.inner.close_session(id).await;
        self.cleanup(id).await;
        result.map_err(Into::into)
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        message: rmcp::model::ClientJsonRpcMessage,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        Ok(self.inner.create_stream(id, message).await?)
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: rmcp::model::ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        Ok(self.inner.accept_message(id, message).await?)
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        Ok(self.inner.create_standalone_stream(id).await?)
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        Ok(self.inner.resume(id, last_event_id).await?)
    }
}

#[derive(Clone)]
struct McpService {
    state: Arc<ServerState>,
}

impl ServerHandler for McpService {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("sqlr", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        context.peer.set_peer_info(request.clone());
        Ok(self
            .negotiate_initialize(&request)?
            .with_instructions(INSTRUCTIONS))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(registry::tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let parts = context.extensions.get::<axum::http::request::Parts>();
        let session_id = parts
            .and_then(|parts| parts.headers.get("mcp-session-id"))
            .and_then(|value| value.to_str().ok())
            .unwrap_or("stateless")
            .to_string();
        let client = context.client_info();
        let (name, version) = client
            .as_ref()
            .map(|info| (info.name.clone(), info.version.clone()))
            .unwrap_or_default();
        if self.state.sessions.lock().contains_key(&session_id) {
            self.state.touch_session(&session_id);
            if let Some(session) = self.state.sessions.lock().get_mut(&session_id)
                && !name.is_empty()
            {
                session.client_name = name.clone();
                session.client_version = version.clone();
            }
        } else {
            self.state.register_session(&session_id, &name, &version);
        }
        let tool_ctx = self.state.context(&session_id);
        let conn = request
            .arguments
            .as_ref()
            .and_then(|args| args.get("connection").or_else(|| args.get("conn")))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let span = tracing::info_span!("mcp_tool", tool = %request.name, conn = conn.as_deref());
        let started = std::time::Instant::now();
        self.state.begin_call(&session_id);
        let outcome = crate::tools::call(
            &self.state,
            &tool_ctx,
            request.name.as_ref(),
            request.arguments.as_ref(),
        );
        let outcome = tracing::Instrument::instrument(outcome, span).await;
        self.state.end_call(&session_id);
        let ok = outcome.is_ok();
        let error_code = outcome.as_ref().err().map(|error| error.code);
        tracing::info!(
            target: "sqlr_mcp",
            client = %tool_ctx.client_label(),
            session = %session_id,
            tool = %request.name,
            conn = conn.as_deref(),
            ok,
            error = ?error_code,
            ms = started.elapsed().as_millis() as u64,
            "agent tool call"
        );
        Ok(outcome?.into())
    }
}

async fn authenticate(State(auth): State<AuthState>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let port = auth.state.port;
    let host_ok = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| {
            host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
        });
    if !host_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(origin) = headers.get(ORIGIN) {
        let allowed = origin.to_str().ok().is_some_and(|origin| {
            origin == format!("http://127.0.0.1:{port}")
                || origin == format!("http://localhost:{port}")
        });
        if !allowed {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    if !bearer.is_some_and(|token| auth.state.check_token(token)) {
        let mut response = StatusCode::UNAUTHORIZED.into_response();
        response
            .headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    next.run(request).await
}

/// A running server: the listener task, its cancellation handle and the discovery path.
pub struct McpServer {
    state: Arc<ServerState>,
    manager: Arc<CappedSessionManager>,
    token_store: Arc<dyn McpTokenStore>,
    cancellation: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    discovery_path: PathBuf,
    data_dir: PathBuf,
    token: String,
}

impl McpServer {
    pub async fn start(options: StartOptions) -> Result<Self, StartError> {
        let token = match options.token_store.get() {
            Ok(Some(token)) => token,
            Ok(None) => {
                let token = generate_token().map_err(|e| StartError::Keychain(e.message))?;
                options
                    .token_store
                    .set(&token)
                    .map_err(|e| StartError::Keychain(e.to_string()))?;
                token
            }
            Err(e) => return Err(StartError::Keychain(e.to_string())),
        };
        std::fs::create_dir_all(&options.data_dir)
            .map_err(|e| StartError::DataDir(e.to_string()))?;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", options.port))
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AddrInUse {
                    StartError::PortBusy { port: options.port }
                } else {
                    StartError::Other(e.to_string())
                }
            })?;
        let state = Arc::new(ServerState::new(
            options.engine,
            options.host,
            options.port,
            token.clone(),
        ));
        let manager = Arc::new(CappedSessionManager::new(state.clone(), MAX_SESSIONS));
        let cancellation = CancellationToken::new();
        let mut config = StreamableHttpServerConfig::default();
        config.legacy_session_mode = true;
        config.allowed_hosts = vec![
            format!("127.0.0.1:{}", options.port),
            format!("localhost:{}", options.port),
        ];
        config.allowed_origins = vec![
            format!("http://127.0.0.1:{}", options.port),
            format!("http://localhost:{}", options.port),
        ];
        config.cancellation_token = cancellation.child_token();
        let service_state = state.clone();
        let service = StreamableHttpService::new(
            move || {
                Ok(McpService {
                    state: service_state.clone(),
                })
            },
            manager.clone(),
            config,
        );
        let router = Router::new()
            .route_service("/mcp", any_service(service))
            .layer(middleware::from_fn_with_state(
                AuthState {
                    state: state.clone(),
                },
                authenticate,
            ));
        let shutdown = cancellation.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { shutdown.cancelled().await })
                .await;
        });
        let discovery = Discovery {
            url: state.url.clone(),
            pid: std::process::id(),
            protocol_version: rmcp::model::ProtocolVersion::LATEST_WITH_INITIALIZE
                .as_str()
                .to_string(),
            app_version: options.app_version,
            started_at: jiff::Timestamp::now().to_string(),
        };
        let discovery_path = match discovery::write(&options.data_dir, &discovery) {
            Ok(path) => path,
            Err(error) => {
                cancellation.cancel();
                return Err(StartError::DataDir(error.message));
            }
        };
        let sweeper_state = state.clone();
        let sweeper_manager = manager.clone();
        let sweeper_cancel = cancellation.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sweeper_cancel.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                }
                for id in sweeper_state.idle_sessions(IDLE_TIMEOUT) {
                    let sid: SessionId = Arc::from(id.as_str());
                    let _ = sweeper_manager.close_session(&sid).await;
                }
                for (query, kind) in sweeper_state.results.sweep() {
                    crate::results::close_engine_result(sweeper_state.engine.as_ref(), query, kind);
                }
            }
        });
        Ok(Self {
            state,
            manager,
            token_store: options.token_store,
            cancellation,
            task: Some(task),
            discovery_path,
            data_dir: options.data_dir,
            token,
        })
    }

    pub fn url(&self) -> &str {
        &self.state.url
    }

    pub fn port(&self) -> u16 {
        self.state.port
    }

    pub fn session_count(&self) -> usize {
        self.state.session_count()
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// The JSON a client can paste; it contains the token.
    pub fn client_config(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "mcpServers": {
                "sqlr": {
                    "type": "streamable-http",
                    "url": self.state.url,
                    "headers": {"Authorization": format!("Bearer {}", self.token)}
                }
            }
        }))
        .unwrap_or_default()
    }

    /// Replaces the keychain token and drops every session. The listener stays bound.
    pub async fn rotate_token(&mut self) -> Result<(), String> {
        let token = generate_token().map_err(|e| e.message)?;
        self.token_store.set(&token).map_err(|e| e.to_string())?;
        self.state.rotate_token(token.clone());
        self.token = token;
        self.drop_all_sessions().await;
        Ok(())
    }

    async fn drop_all_sessions(&self) {
        for id in self.state.session_ids() {
            let sid: SessionId = Arc::from(id.as_str());
            let _ = self.manager.close_session(&sid).await;
        }
    }

    /// Stops accepting requests, drops sessions and removes the discovery file.
    pub async fn shutdown(mut self) {
        self.cancellation.cancel();
        self.drop_all_sessions().await;
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
        discovery::remove(&self.data_dir);
    }

    pub fn discovery_path(&self) -> &Path {
        &self.discovery_path
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.cancellation.cancel();
        discovery::remove(&self.data_dir);
    }
}

/// Closes a result of the kind stored for `id` (used by the session cleanup).
pub fn close_result(engine: &Engine, id: sqlr_engine::QueryId, kind: ResultKind) {
    crate::results::close_engine_result(engine, id, kind);
}

/// A tool error for a refused approval, reused by scripted hosts.
pub fn denied() -> ToolError {
    ToolError::denied()
}
