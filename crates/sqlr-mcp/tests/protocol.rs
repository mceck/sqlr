use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sqlr_core::{
    AgentAccess, ConnectionConfig, ConnectionProfile, Driver, DriverManifest, SavedTask, Secrets,
    SettingValue,
};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_mcp::host::{
    AgentConnection, AgentHost, ApprovalRequest, ConnectionWrite, FolderOp, KeymapEntry, RunTarget,
    SettingEntry, SettingLevel, TabDetail, TabEdit, TabSummary,
};
use sqlr_mcp::server::{McpServer, StartOptions};
use sqlr_store::{McpTokenStore, MemoryMcpTokenStore, MemorySecretStore};

struct HostState {
    config: ConnectionConfig,
    secrets: Secrets,
    profile: ConnectionProfile,
    manifest: DriverManifest,
    allow: bool,
    writes: Vec<ConnectionWrite>,
    toasts: Vec<String>,
}

struct TestHost {
    state: Mutex<HostState>,
}

impl TestHost {
    fn new(config: ConnectionConfig, manifest: DriverManifest) -> Self {
        Self {
            state: Mutex::new(HostState {
                config,
                secrets: Secrets::default(),
                profile: ConnectionProfile::development(),
                manifest,
                allow: true,
                writes: Vec::new(),
                toasts: Vec::new(),
            }),
        }
    }
}

#[async_trait]
impl AgentHost for TestHost {
    async fn connection(&self, id: &str) -> sqlr_mcp::error::ToolResult<AgentConnection> {
        let state = self.state.lock().unwrap();
        if state.config.id != id {
            return Err(sqlr_mcp::error::ToolError::not_found("unknown connection"));
        }
        Ok(AgentConnection {
            config: state.config.clone(),
            secrets: state.secrets.clone(),
            profile: state.profile.clone(),
            manifest: state.manifest.clone(),
        })
    }

    async fn connections(&self) -> sqlr_mcp::error::ToolResult<Vec<ConnectionConfig>> {
        Ok(vec![self.state.lock().unwrap().config.clone()])
    }
    async fn folders(&self) -> sqlr_mcp::error::ToolResult<Vec<String>> {
        Ok(Vec::new())
    }
    async fn profiles(&self) -> sqlr_mcp::error::ToolResult<Vec<ConnectionProfile>> {
        Ok(ConnectionProfile::builtins())
    }
    async fn save_profile(&self, _profile: ConnectionProfile) -> sqlr_mcp::error::ToolResult<()> {
        Ok(())
    }
    async fn delete_profile(&self, _id: &str) -> sqlr_mcp::error::ToolResult<()> {
        Ok(())
    }
    async fn drivers(&self) -> sqlr_mcp::error::ToolResult<Vec<DriverManifest>> {
        Ok(vec![self.state.lock().unwrap().manifest.clone()])
    }
    async fn secret_presence(
        &self,
        _id: &str,
    ) -> sqlr_mcp::error::ToolResult<BTreeMap<String, bool>> {
        Ok(BTreeMap::new())
    }
    async fn settings(&self) -> sqlr_mcp::error::ToolResult<Vec<SettingEntry>> {
        Ok(Vec::new())
    }
    async fn set_setting(
        &self,
        _id: &str,
        _level: SettingLevel,
        _level_id: Option<&str>,
        _value: Option<SettingValue>,
    ) -> sqlr_mcp::error::ToolResult<()> {
        Ok(())
    }
    async fn keymap(&self) -> sqlr_mcp::error::ToolResult<Vec<KeymapEntry>> {
        Ok(Vec::new())
    }
    async fn set_keymap(
        &self,
        _action: &str,
        _chords: Option<Vec<String>>,
    ) -> sqlr_mcp::error::ToolResult<()> {
        Ok(())
    }
    async fn tasks(&self) -> sqlr_mcp::error::ToolResult<Vec<SavedTask>> {
        Ok(Vec::new())
    }
    async fn save_task(&self, _task: SavedTask) -> sqlr_mcp::error::ToolResult<()> {
        Ok(())
    }
    async fn write_connection(&self, write: ConnectionWrite) -> sqlr_mcp::error::ToolResult<()> {
        let mut state = self.state.lock().unwrap();
        if write.config.agent_access != AgentAccess::None
            && write.config.agent_access != state.config.agent_access
        {
            return Err(sqlr_mcp::error::ToolError::invalid(
                "exposure is human-only",
            ));
        }
        state.writes.push(write);
        Ok(())
    }
    async fn folder(&self, _op: FolderOp) -> sqlr_mcp::error::ToolResult<()> {
        Ok(())
    }
    async fn delete(&self, _target: &str, _id: &str) -> sqlr_mcp::error::ToolResult<()> {
        Ok(())
    }
    async fn approve(&self, _request: ApprovalRequest) -> sqlr_mcp::error::ToolResult<()> {
        if self.state.lock().unwrap().allow {
            Ok(())
        } else {
            Err(sqlr_mcp::error::ToolError::denied())
        }
    }
    async fn tabs(&self) -> sqlr_mcp::error::ToolResult<Vec<TabSummary>> {
        Ok(vec![TabSummary {
            id: 1,
            title: "Query".into(),
            connection_id: Some(self.state.lock().unwrap().config.id.clone()),
            database: None,
            dirty: false,
            revision: 1,
            busy: false,
        }])
    }
    async fn tab(&self, id: u64) -> sqlr_mcp::error::ToolResult<TabDetail> {
        if id != 1 {
            return Err(sqlr_mcp::error::ToolError::not_found("unknown tab"));
        }
        Ok(TabDetail {
            id: 1,
            title: "Query".into(),
            connection_id: Some(self.state.lock().unwrap().config.id.clone()),
            database: None,
            dirty: false,
            revision: 1,
            busy: false,
            text: "select 1".into(),
            caret: 0,
            selection: None,
        })
    }
    async fn edit_tab(
        &self,
        _id: u64,
        _expected_revision: u64,
        _edit: TabEdit,
    ) -> sqlr_mcp::error::ToolResult<TabDetail> {
        self.tab(1).await
    }
    async fn run_tab(
        &self,
        _id: u64,
        _target: RunTarget,
        _max_rows: usize,
    ) -> sqlr_mcp::error::ToolResult<Value> {
        Ok(json!({"statements": []}))
    }
    async fn toast(&self, text: &str) {
        self.state.lock().unwrap().toasts.push(text.to_string());
    }
}

struct Client {
    http: reqwest::Client,
    url: String,
    token: String,
    session: Option<String>,
}

impl Client {
    fn new(port: u16, token: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            url: format!("http://127.0.0.1:{port}/mcp"),
            token: token.to_string(),
            session: None,
        }
    }

    async fn post(&self, body: Value, with_auth: bool) -> (StatusCode, String, Option<String>) {
        let mut request = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&body);
        if with_auth {
            request = request.header("Authorization", format!("Bearer {}", self.token));
        }
        if let Some(session) = &self.session {
            request = request.header("mcp-session-id", session);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let text = response.text().await.unwrap();
        (status, text, session)
    }

    async fn initialize(&mut self) {
        let (status, text, session) = self
            .post(
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "test-client", "version": "1.2"}
                    }
                }),
                true,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        self.session = session.or(self.session.take());
        let _ = event_json(&text);
    }

    async fn rpc(&mut self, id: u64, method: &str, params: Value) -> Value {
        let (status, text, _) = self
            .post(
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
                true,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        event_json(&text)
    }

    async fn tool(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        let result = self
            .rpc(
                id,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            )
            .await;
        let result = result
            .get("result")
            .cloned()
            .unwrap_or_else(|| result.clone());
        result
            .get("structuredContent")
            .cloned()
            .unwrap_or_else(|| json!({}))
    }
}

fn event_json(text: &str) -> Value {
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data:")
            && let Ok(value) = serde_json::from_str(data.trim())
        {
            return value;
        }
    }
    serde_json::from_str(text).unwrap_or_else(|_| json!({"body": text}))
}

fn free_port() -> u16 {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn sqlite_config(dir: &std::path::Path, access: AgentAccess) -> ConnectionConfig {
    ConnectionConfig {
        id: "c1".into(),
        name: "Test".into(),
        driver: "sqlite".into(),
        group: None,
        color: None,
        values: [
            ("path", dir.join("t.db").display().to_string()),
            ("create_if_missing", "true".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        profile: None,
        save_secrets: true,
        agent_access: access,
    }
}

async fn setup(
    access: AgentAccess,
    token_store: Arc<dyn McpTokenStore>,
) -> (McpServer, Arc<TestHost>, tempfile::TempDir, u16, String) {
    let dir = tempfile::tempdir().unwrap();
    let config = sqlite_config(dir.path(), access);
    let driver = SqliteDriver::new();
    let manifest = driver.manifest().clone();
    let mut drivers = DriverRegistry::default();
    drivers.register(driver);
    let engine = Arc::new(Engine::new(drivers, Arc::new(|| {})));
    engine.set_secret_store(Arc::new(MemorySecretStore::default()));
    let token = token_store
        .get()
        .unwrap()
        .unwrap_or_else(|| "test-token".into());
    let host = Arc::new(TestHost::new(config, manifest));
    let port = free_port();
    let data_dir = dir.path().join("data");
    let server = McpServer::start(StartOptions {
        port,
        data_dir,
        engine,
        host: host.clone(),
        token_store,
        app_version: "0.1.0".into(),
    })
    .await
    .unwrap();
    (server, host, dir, port, token)
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_headers_capabilities_and_guide() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("secret-token"));
    let (server, _host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    assert_eq!(server.session_count(), 0);

    let mut client = Client::new(port, &token);
    // Missing token is rejected.
    let (status, _, _) = client
        .post(json!({"jsonrpc":"2.0","id":1,"method":"ping"}), false)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Wrong token is rejected.
    let wrong = Client::new(port, "nope");
    let (status, _, _) = wrong
        .post(json!({"jsonrpc":"2.0","id":1,"method":"ping"}), true)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    client.initialize().await;
    assert_eq!(server.session_count(), 1);
    let tools = client.rpc(2, "tools/list", json!({})).await;
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 19);
    assert!(names.contains(&"get_guide".to_string()));
    assert!(names.contains(&"run_query".to_string()));

    let text = client
        .rpc(4, "tools/call", json!({"name":"get_guide","arguments":{}}))
        .await;
    let text = text["result"]["content"][0]["text"].as_str().unwrap();
    for name in &names {
        assert!(text.contains(name), "{name} missing from the guide");
    }

    // Discovery is private and has no token.
    let path = server.discovery_path().to_path_buf();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains(&token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    server.shutdown().await;
    assert!(!path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn exposure_read_write_and_query_contract() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, _host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;

    let listed = client
        .tool(2, "config_list", json!({"target":"connections"}))
        .await;
    assert_eq!(listed["connections"][0]["agent_access"], "read");
    assert!(listed["connections"][0].get("values").is_none());
    assert!(listed["connections"][0].get("secrets").is_none());

    let select = client
        .tool(
            3,
            "run_query",
            json!({"connection_id":"c1","query":"select 1 as one, 'x' as text"}),
        )
        .await;
    assert_eq!(select["kind"], "tabular");
    assert_eq!(select["status"], "ready");
    assert_eq!(select["tabular"]["rows"][0][0], 1);
    assert_eq!(select["tabular"]["rows"][0][1], "x");
    let query_id = select["query_id"].as_str().unwrap();

    let page = client
        .tool(
            4,
            "get_query_result",
            json!({"query_id": query_id, "offset": 0, "limit": 10}),
        )
        .await;
    assert_eq!(page["offset"], 0);
    assert_eq!(page["tabular"]["rows"][0][0], 1);
    let closed = client
        .tool(5, "close_result", json!({"query_id": query_id}))
        .await;
    assert_eq!(closed["closed"], true);
    let closed = client
        .tool(6, "close_result", json!({"query_id": query_id}))
        .await;
    assert_eq!(closed["closed"], false);

    // A read session refuses writes before reaching an approval.
    let write = client
        .tool(
            7,
            "run_query",
            json!({"connection_id":"c1","query":"create table t (id integer)"}),
        )
        .await;
    assert_eq!(write["error"]["code"], "read_only");

    // Multi-statement input is rejected with the count.
    let multi = client
        .tool(
            8,
            "run_query",
            json!({"connection_id":"c1","query":"select 1; select 2"}),
        )
        .await;
    assert_eq!(multi["error"]["code"], "invalid");
    assert!(multi["error"]["message"].as_str().unwrap().contains('2'));

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn describe_object_connects_without_an_agent_query_session() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::Write, store).await;
    let mut writer = Client::new(port, &token);
    writer.initialize().await;
    let created = writer
        .tool(
            2,
            "run_query",
            json!({"connection_id":"c1","query":"create table PuckDraft (id integer primary key, title text not null)"}),
        )
        .await;
    assert_eq!(created["status"], "ready", "{created}");

    // Metadata must work for Read exposure and before this client runs any query.
    host.state.lock().unwrap().config.agent_access = AgentAccess::Read;
    let mut reader = Client::new(port, &token);
    reader.initialize().await;
    reader
        .rpc(2, "tools/call", json!({"name":"get_guide","arguments":{}}))
        .await;
    for call_id in [3, 4] {
        let described = reader
            .tool(
                call_id,
                "describe_object",
                json!({"connection_id":"c1","name":"PuckDraft"}),
            )
            .await;
        assert!(described.get("error").is_none(), "{described}");
        assert_eq!(described["table"]["name"], "PuckDraft");
        assert_eq!(described["columns"][0]["name"], "id");
        assert_eq!(described["columns"][1]["name"], "title");
        assert_eq!(described["columns"][1]["nullable"], false);
        assert_eq!(described["primary_key"], json!(["id"]));
    }
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn close_result_releases_native_replies() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = SqliteDriver::new().manifest().clone();
    let host = Arc::new(TestHost::new(
        sqlite_config(dir.path(), AgentAccess::Read),
        manifest,
    ));
    let engine = Arc::new(Engine::new(DriverRegistry::default(), Arc::new(|| {})));
    let state = Arc::new(sqlr_mcp::state::ServerState::new(
        engine,
        host,
        0,
        "test-token".into(),
    ));
    let ctx = state.context("reader");
    let id = state
        .results
        .insert_reply("reader", &sqlr_core::RespValue::Int(42));
    let arguments = json!({"query_id":id});

    let closed = sqlr_mcp::tools::call(&state, &ctx, "close_result", arguments.as_object())
        .await
        .unwrap();
    assert_eq!(closed.structured_content.unwrap()["closed"], true);
    let page = sqlr_mcp::tools::call(&state, &ctx, "get_query_result", arguments.as_object())
        .await
        .unwrap();
    assert_eq!(
        page.structured_content.unwrap()["error"]["code"],
        "not_found"
    );
    let closed_again = sqlr_mcp::tools::call(&state, &ctx, "close_result", arguments.as_object())
        .await
        .unwrap();
    assert_eq!(closed_again.structured_content.unwrap()["closed"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn not_exposed_connections_are_invisible() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, _host, _dir, port, token) = setup(AgentAccess::None, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    let listed = client
        .tool(2, "config_list", json!({"target":"connections"}))
        .await;
    assert_eq!(listed["connections"].as_array().unwrap().len(), 0);
    let browse = client
        .tool(3, "browse", json!({"connection_id":"c1"}))
        .await;
    assert_eq!(browse["error"]["code"], "not_exposed");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn origin_and_host_validation() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, _host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    let client = reqwest::Client::new();
    let body = json!({
        "jsonrpc":"2.0","id":1,"method":"initialize",
        "params": {
            "protocolVersion":"2025-11-25",
            "capabilities":{},
            "clientInfo":{"name":"origin-test","version":"1"}
        }
    });
    let post = |origin: Option<&str>, host: Option<&str>| {
        let mut request = client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Authorization", format!("Bearer {token}"))
            .json(&body);
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        if let Some(host) = host {
            request = request.header("Host", host);
        }
        request.send()
    };
    assert_eq!(post(None, None).await.unwrap().status(), StatusCode::OK);
    assert_eq!(
        post(Some(&format!("http://127.0.0.1:{port}")), None)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        post(Some("null"), None).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(Some("https://evil.example"), None)
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(None, Some(&format!("evil.example:{port}")))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn write_exposure_approvals_and_token_rotation() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("old-token"));
    let (mut server, host, _dir, port, token) = setup(AgentAccess::Write, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;

    let created = client
        .tool(
            2,
            "run_query",
            json!({"connection_id":"c1","query":"create table t (id integer, name text)"}),
        )
        .await;
    assert_eq!(created["status"], "ready", "{created}");
    let inserted = client
        .tool(
            3,
            "run_query",
            json!({"connection_id":"c1","query":"insert into t values (1, 'a'), (2, 'b')"}),
        )
        .await;
    assert_eq!(inserted["status"], "ready", "{inserted}");
    let selected = client
        .tool(
            4,
            "run_query",
            json!({"connection_id":"c1","query":"select id, name from t order by id"}),
        )
        .await;
    assert_eq!(selected["tabular"]["rows"][1][1], "b");

    // A denied destructive statement is reported as denied.
    host.state.lock().unwrap().allow = false;
    let denied = client
        .tool(
            5,
            "run_query",
            json!({"connection_id":"c1","query":"drop table t"}),
        )
        .await;
    assert_eq!(denied["error"]["code"], "denied");
    host.state.lock().unwrap().allow = true;

    // Exposure and safety are enforced before approval.
    host.state.lock().unwrap().profile.read_only = true;
    let read_only = client
        .tool(
            6,
            "run_query",
            json!({"connection_id":"c1","query":"insert into t values (3, 'c')"}),
        )
        .await;
    assert_eq!(read_only["error"]["code"], "read_only");
    host.state.lock().unwrap().profile.read_only = false;

    // Configuration: exposure is human-only, values are write-only.
    let refused = client
        .tool(
            7,
            "config_write",
            json!({"target":"connection","payload":{"id":"c1","agent_access":"write"}}),
        )
        .await;
    assert_eq!(refused["error"]["code"], "invalid");
    let listed = client
        .tool(8, "config_list", json!({"target":"connections"}))
        .await;
    assert!(listed["connections"][0].get("values").is_none());
    assert!(listed["connections"][0].get("secrets").is_none());

    // Rotation invalidates the old token and drops sessions.
    server.rotate_token().await.unwrap();
    let old = Client::new(port, "old-token");
    let (status, _, _) = old
        .post(json!({"jsonrpc":"2.0","id":9,"method":"ping"}), true)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let mut fresh = Client::new(port, server.token());
    fresh.initialize().await;
    assert_eq!(server.session_count(), 1);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_port_reports_the_error_without_a_discovery_file() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = sqlite_config(dir.path(), AgentAccess::Read);
    let driver = SqliteDriver::new();
    let manifest = driver.manifest().clone();
    let mut drivers = DriverRegistry::default();
    drivers.register(driver);
    let engine = Arc::new(Engine::new(drivers, Arc::new(|| {})));
    let host = Arc::new(TestHost::new(config, manifest));
    let data_dir = dir.path().join("data");
    let error = McpServer::start(StartOptions {
        port,
        data_dir: data_dir.clone(),
        engine,
        host,
        token_store: store,
        app_version: "0.1.0".into(),
    })
    .await
    .err()
    .expect("the port is busy");
    assert!(matches!(
        error,
        sqlr_mcp::server::StartError::PortBusy { .. }
    ));
    assert!(!data_dir.join("mcp.json").exists());
    drop(listener);
}

#[tokio::test(flavor = "multi_thread")]
async fn session_cap_is_enforced() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, _host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    for _ in 0..8 {
        let mut client = Client::new(port, &token);
        client.initialize().await;
    }
    assert_eq!(server.session_count(), 8);
    let ninth = Client::new(port, &token);
    let (status, _, _) = ninth
        .post(
            json!({
                "jsonrpc":"2.0","id":1,"method":"initialize",
                "params": {
                    "protocolVersion":"2025-11-25",
                    "capabilities":{},
                    "clientInfo":{"name":"ninth","version":"1"}
                }
            }),
            true,
        )
        .await;
    assert!(!status.is_success(), "a ninth session is refused: {status}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn snippets_and_profiles_follow_the_rules() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;

    let written = client
        .tool(
            2,
            "write_snippet",
            json!({"name":"Top customers", "folder":"reports/sales", "sql":"select 1"}),
        )
        .await;
    let id = written["id"].as_str().unwrap().to_string();
    let listed = client.tool(3, "list_snippets", json!({})).await;
    assert_eq!(listed["snippets"][0]["name"], "Top customers");
    assert!(listed["snippets"][0].get("sql").is_none());
    let full = client.tool(4, "get_snippet", json!({"id": id})).await;
    assert_eq!(full["sql"], "select 1");
    let deleted = client.tool(5, "delete_snippet", json!({"id": id})).await;
    assert_eq!(deleted["deleted"], true);

    // A profile's safety flags can only be tightened.
    let refused = client
        .tool(
            6,
            "config_write",
            json!({
                "target":"profile",
                "payload":{"id":"production", "confirm_writes": false}
            }),
        )
        .await;
    assert_eq!(refused["error"]["code"], "invalid");
    assert!(
        host.state
            .lock()
            .unwrap()
            .toasts
            .iter()
            .any(|toast| toast.contains("snippet")),
        "writes produce a toast"
    );
    server.shutdown().await;
}
