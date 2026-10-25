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
    folders: Vec<FolderOp>,
    saved_profiles: Vec<ConnectionProfile>,
    setting_entries: Vec<SettingEntry>,
    setting_writes: Vec<(String, SettingLevel, Option<String>, Option<SettingValue>)>,
    keymap_writes: Vec<(String, Option<Vec<String>>)>,
    tab_edits: Vec<(u64, u64, TabEdit)>,
    tab_runs: Vec<(u64, RunTarget, usize)>,
    deletions: Vec<(String, String)>,
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
                folders: Vec::new(),
                saved_profiles: Vec::new(),
                setting_entries: Vec::new(),
                setting_writes: Vec::new(),
                keymap_writes: Vec::new(),
                tab_edits: Vec::new(),
                tab_runs: Vec::new(),
                deletions: Vec::new(),
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
    async fn save_profile(&self, profile: ConnectionProfile) -> sqlr_mcp::error::ToolResult<()> {
        self.state.lock().unwrap().saved_profiles.push(profile);
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
        Ok(self.state.lock().unwrap().setting_entries.clone())
    }
    async fn set_setting(
        &self,
        id: &str,
        level: SettingLevel,
        level_id: Option<&str>,
        value: Option<SettingValue>,
    ) -> sqlr_mcp::error::ToolResult<()> {
        self.state.lock().unwrap().setting_writes.push((
            id.into(),
            level,
            level_id.map(str::to_owned),
            value,
        ));
        Ok(())
    }
    async fn keymap(&self) -> sqlr_mcp::error::ToolResult<Vec<KeymapEntry>> {
        Ok(Vec::new())
    }
    async fn set_keymap(
        &self,
        action: &str,
        chords: Option<Vec<String>>,
    ) -> sqlr_mcp::error::ToolResult<()> {
        self.state
            .lock()
            .unwrap()
            .keymap_writes
            .push((action.into(), chords));
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
    async fn folder(&self, op: FolderOp) -> sqlr_mcp::error::ToolResult<()> {
        self.state.lock().unwrap().folders.push(op);
        Ok(())
    }
    async fn delete(&self, target: &str, id: &str) -> sqlr_mcp::error::ToolResult<()> {
        self.state
            .lock()
            .unwrap()
            .deletions
            .push((target.into(), id.into()));
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
        id: u64,
        expected_revision: u64,
        edit: TabEdit,
    ) -> sqlr_mcp::error::ToolResult<TabDetail> {
        self.state
            .lock()
            .unwrap()
            .tab_edits
            .push((id, expected_revision, edit));
        self.tab(1).await
    }
    async fn run_tab(
        &self,
        id: u64,
        target: RunTarget,
        max_rows: usize,
    ) -> sqlr_mcp::error::ToolResult<Value> {
        self.state
            .lock()
            .unwrap()
            .tab_runs
            .push((id, target, max_rows));
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
        app_version: env!("CARGO_PKG_VERSION").into(),
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
        app_version: env!("CARGO_PKG_VERSION").into(),
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

#[tokio::test(flavor = "multi_thread")]
async fn connection_writes_validate_metadata_and_preserve_exposure() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    let invalid = [
        json!({"name":"new"}),
        json!({"driver":"missing","name":"new"}),
        json!({"driver":"sqlite"}),
        json!({"id":"c1","name":"  "}),
        json!({"id":"c1","driver":"other"}),
        json!({"id":"c1","group":42}),
        json!({"id":"c1","color":"red"}),
        json!({"id":"c1","color":[256,0,0]}),
        json!({"id":"c1","profile":42}),
        json!({"id":"c1","profile":"missing"}),
        json!({"id":"c1","save_secrets":"true"}),
        json!({"id":"c1","values":[]}),
        json!({"id":"c1","values":{"path":false}}),
        json!({"id":"c1","values":{"unknown":"value"}}),
        json!({"id":"c1","remove_values":"path"}),
        json!({"id":"c1","remove_values":[1]}),
        json!({"id":"c1","remove_values":["tls_verify"]}),
        json!({"id":"c1","secrets":[]}),
        json!({"id":"c1","secrets":{"path":"secret"}}),
        json!({"id":"c1","remove_secrets":false}),
        json!({"id":"c1","remove_secrets":[1]}),
        json!({"id":"c1","remove_secrets":["path"]}),
    ];
    for (index, payload) in invalid.into_iter().enumerate() {
        let result = client
            .tool(
                100 + index as u64,
                "config_write",
                json!({"target":"connection","payload":payload}),
            )
            .await;
        assert_eq!(result["error"]["code"], "invalid", "{payload}: {result}");
    }
    assert!(host.state.lock().unwrap().writes.is_empty());
    let result = client
        .tool(
            200,
            "config_write",
            json!({"target":"connection","payload":{
                "id":"c1","name":"Renamed","group":"work","color":[12,34,56],
                "profile":"production","save_secrets":false,"values":{"property.timeout":"10"},
                "remove_values":["path"]
            }}),
        )
        .await;
    assert_eq!(result, json!({"id":"c1","created":false}));
    {
        let state = host.state.lock().unwrap();
        let write = state.writes.last().unwrap();
        assert_eq!(write.config.name, "Renamed");
        assert_eq!(write.config.group.as_deref(), Some("work"));
        assert_eq!(write.config.color, Some([12, 34, 56]));
        assert_eq!(write.config.profile.as_deref(), Some("production"));
        assert_eq!(write.config.agent_access, AgentAccess::Read);
        assert_eq!(write.config.values.get("property.timeout").unwrap(), "10");
        assert!(!write.config.values.contains_key("path"));
        assert_eq!(write.save_secrets, Some(false));
    }
    let result = client
        .tool(
            201,
            "config_write",
            json!({"target":"connection","payload":{
                "driver":"sqlite","name":"New","group":null,"color":null,"profile":null
            }}),
        )
        .await;
    assert_eq!(result["created"], true);
    assert_eq!(
        host.state
            .lock()
            .unwrap()
            .writes
            .last()
            .unwrap()
            .config
            .agent_access,
        AgentAccess::None
    );
    server.shutdown().await;
}

fn setting_entry(id: &str, kind: Value) -> SettingEntry {
    SettingEntry {
        id: id.into(),
        section: "test".into(),
        label: id.into(),
        description: String::new(),
        kind,
        default: Value::Null,
        allowed_levels: vec!["global".into(), "profile".into(), "connection".into()],
        safety: "normal".into(),
        agent_readable: true,
        agent_writable: true,
        values: BTreeMap::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn setting_writes_parse_types_levels_and_reject_unsafe_changes() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    let cases = [
        (
            "boolean",
            json!({"type":"bool"}),
            json!(true),
            SettingValue::Bool(true),
            json!("true"),
        ),
        (
            "integer",
            json!({"type":"int","min":1,"max":3}),
            json!(2),
            SettingValue::Int(2),
            json!(4),
        ),
        (
            "float",
            json!({"type":"float","min":0.0,"max":2.0}),
            json!(1.5),
            SettingValue::Float(1.5),
            json!(3.0),
        ),
        (
            "text",
            json!({"type":"text"}),
            json!("hello"),
            SettingValue::Text("hello".into()),
            json!(false),
        ),
        (
            "path",
            json!({"type":"path"}),
            json!("/tmp"),
            SettingValue::Path("/tmp".into()),
            json!(12),
        ),
        (
            "choice",
            json!({"type":"choice","options":[{"id":"a","label":"A"}]}),
            json!("a"),
            SettingValue::Choice("a".into()),
            json!("b"),
        ),
        (
            "color",
            json!({"type":"color"}),
            json!([1, 2, 3]),
            SettingValue::Color([1, 2, 3]),
            json!([1, 2, 300]),
        ),
    ];
    {
        let mut state = host.state.lock().unwrap();
        state.setting_entries = cases
            .iter()
            .map(|(id, kind, ..)| setting_entry(id, kind.clone()))
            .collect();
        let mut safety = setting_entry("safe", json!({"type":"bool"}));
        safety.safety = "tighten_only".into();
        safety.values.insert("global".into(), json!(true));
        state.setting_entries.push(safety);
        let mut restricted = setting_entry("global-only", json!({"type":"bool"}));
        restricted.allowed_levels = vec!["global".into()];
        state.setting_entries.push(restricted);
        state
            .setting_entries
            .push(setting_entry("terminal.shell", json!({"type":"text"})));
    }
    let mut client = Client::new(port, &token);
    client.initialize().await;
    for (index, (id, _, value, expected, bad)) in cases.into_iter().enumerate() {
        let level = match index % 3 {
            0 => json!({"name":"global"}),
            1 => json!({"name":"profile","id":"production"}),
            _ => json!({"name":"connection","id":"c1"}),
        };
        let result = client
            .tool(
                100 + index as u64,
                "config_write",
                json!({"target":"setting","payload":{
                    "id":id,"level":level,"value":value
                }}),
            )
            .await;
        assert!(result.get("error").is_none(), "{result}");
        {
            let state = host.state.lock().unwrap();
            let write = state.setting_writes.last().unwrap();
            assert_eq!(write.0, id);
            assert_eq!(write.3, Some(expected));
            assert_eq!(
                write.1,
                [
                    SettingLevel::Global,
                    SettingLevel::Profile,
                    SettingLevel::Connection
                ][index % 3]
            );
            assert_eq!(
                write.2.as_deref(),
                [None, Some("production"), Some("c1")][index % 3]
            );
        }
        let result = client
            .tool(
                200 + index as u64,
                "config_write",
                json!({"target":"setting","payload":{
                    "id":id,"level":level,"value":bad
                }}),
            )
            .await;
        assert_eq!(result["error"]["code"], "invalid", "{result}");
    }
    let before = host.state.lock().unwrap().setting_writes.len();
    for (index, payload) in [
        json!({"id":"safe","level":{"name":"global"},"value":false}),
        json!({"id":"safe","level":{"name":"global"},"reset":true}),
        json!({"id":"terminal.shell","level":{"name":"global"},"value":"sh"}),
        json!({"id":"global-only","level":{"name":"connection","id":"c1"},"value":true}),
        json!({"id":"text","level":false,"value":"x"}),
        json!({"id":"text","level":{"name":"profile"},"value":"x"}),
        json!({"id":"text","level":{"name":"connection"},"value":"x"}),
        json!({"id":"text","level":{"name":"missing"},"value":"x"}),
        json!({"id":"text","value":"x"}),
        json!({"id":"missing","level":{"name":"global"},"value":true}),
        json!({"id":"text","level":{"name":"global"}}),
    ]
    .into_iter()
    .enumerate()
    {
        let result = client
            .tool(
                300 + index as u64,
                "config_write",
                json!({"target":"setting","payload":payload}),
            )
            .await;
        assert_eq!(result["error"]["code"], "invalid", "{payload}: {result}");
    }
    assert_eq!(host.state.lock().unwrap().setting_writes.len(), before);
    let result = client
        .tool(
            400,
            "config_write",
            json!({"target":"setting","payload":{
                "id":"text","level":{"name":"global"},"reset":true
            }}),
        )
        .await;
    assert_eq!(result["reset"], true);
    assert_eq!(
        host.state.lock().unwrap().setting_writes.last().unwrap().3,
        None
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn folder_operations_and_keymap_writes_reach_the_host_after_validation() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    for (index, payload) in [
        json!({"action":"create","name":"work"}),
        json!({"action":"rename","from":"work","to":"prod"}),
        json!({"action":"move","name":"prod","target":{"at":"end"}}),
        json!({"action":"move","name":"prod","target":{"at":"before","id":"c1"}}),
        json!({"action":"move","name":"prod","target":{"at":"after","id":"c1"}}),
        json!({"action":"move","name":"prod","target":{"at":"into","id":"parent"}}),
    ]
    .into_iter()
    .enumerate()
    {
        let result = client
            .tool(
                100 + index as u64,
                "config_write",
                json!({"target":"folder","payload":payload}),
            )
            .await;
        assert_eq!(result["action"], payload["action"]);
    }
    {
        let state = host.state.lock().unwrap();
        let actual: Vec<Value> = state
            .folders
            .iter()
            .map(|op| serde_json::to_value(op).unwrap())
            .collect();
        assert_eq!(
            actual,
            vec![
                json!({"Create":{"name":"work"}}),
                json!({"Rename":{"from":"work","to":"prod"}}),
                json!({"Move":{"name":"prod","target":{"at":"end"}}}),
                json!({"Move":{"name":"prod","target":{"at":"before","id":"c1"}}}),
                json!({"Move":{"name":"prod","target":{"at":"after","id":"c1"}}}),
                json!({"Move":{"name":"prod","target":{"at":"into","id":"parent"}}}),
            ]
        );
    }
    for (index, payload) in [
        json!({"action":"delete"}),
        json!({"action":"move","name":"prod","target":{"at":"before"}}),
        json!({"action":"move","name":"prod","target":{"at":"invalid"}}),
    ]
    .into_iter()
    .enumerate()
    {
        let result = client
            .tool(
                200 + index as u64,
                "config_write",
                json!({"target":"folder","payload":payload}),
            )
            .await;
        assert_eq!(result["error"]["code"], "invalid");
    }
    let action = sqlr_core::ActionId::NewTab.id();
    let result = client
        .tool(
            300,
            "config_write",
            json!({"target":"keymap","payload":{"action":action,"chords":["Cmd+Shift+T"]}}),
        )
        .await;
    assert_eq!(result["chords"], json!(["Cmd+Shift+T"]));
    let result = client
        .tool(
            301,
            "config_write",
            json!({"target":"keymap","payload":{"action":action,"reset":true}}),
        )
        .await;
    assert_eq!(result["reset"], true);
    {
        let state = host.state.lock().unwrap();
        assert_eq!(
            state.keymap_writes,
            vec![
                (action.into(), Some(vec!["Cmd+Shift+T".into()])),
                (action.into(), None)
            ]
        );
    }
    for (index, payload) in [
        json!({"action":"missing","reset":true}),
        json!({"action":action,"chords":"Cmd+T"}),
        json!({"action":action,"chords":[1]}),
        json!({"action":action,"chords":["not-a-key"]}),
    ]
    .into_iter()
    .enumerate()
    {
        let result = client
            .tool(
                400 + index as u64,
                "config_write",
                json!({"target":"keymap","payload":payload}),
            )
            .await;
        assert_eq!(result["error"]["code"], "invalid", "{payload}: {result}");
    }
    assert_eq!(host.state.lock().unwrap().keymap_writes.len(), 2);
    server.shutdown().await;
}

fn driver_state(driver: Arc<dyn Driver>, access: AgentAccess) -> Arc<sqlr_mcp::state::ServerState> {
    let manifest = driver.manifest().clone();
    let config = ConnectionConfig {
        id: "c1".into(),
        name: "Test".into(),
        driver: manifest.id.clone(),
        group: None,
        color: None,
        values: BTreeMap::new(),
        profile: None,
        save_secrets: true,
        agent_access: access,
    };
    let host = Arc::new(TestHost::new(config, manifest));
    let mut drivers = DriverRegistry::default();
    drivers.register(driver);
    let engine = Arc::new(Engine::new(drivers, Arc::new(|| {})));
    Arc::new(sqlr_mcp::state::ServerState::new(
        engine,
        host,
        0,
        "test".into(),
    ))
}

async fn direct_tool(
    state: &Arc<sqlr_mcp::state::ServerState>,
    name: &str,
    arguments: Value,
) -> Value {
    sqlr_mcp::tools::call(state, &state.context("test"), name, arguments.as_object())
        .await
        .unwrap()
        .structured_content
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn document_queries_page_close_and_report_native_errors() {
    use sqlr_core::{DocValue, DriverError, ErrorKind};
    use sqlr_testkit::{FakeDocDriver, FakeDocScript};
    let docs: Vec<DocValue> = (1..=5)
        .map(|id| {
            DocValue::Document(vec![
                ("_id".into(), DocValue::Int32(id)),
                ("name".into(), DocValue::String(format!("row {id}"))),
            ])
        })
        .collect();
    let driver = FakeDocDriver::new(FakeDocScript {
        docs,
        ..Default::default()
    });
    let state = driver_state(driver.clone(), AgentAccess::Read);
    let result = direct_tool(
        &state,
        "run_query",
        json!({"connection_id":"c1","query":"db.users.find({})","database":"demo","max_rows":2}),
    )
    .await;
    assert_eq!(result["kind"], "document");
    assert_eq!(result["status"], "ready");
    assert_eq!(
        result["document"]["documents"],
        json!([{"_id":1,"name":"row 1"},{"_id":2,"name":"row 2"}])
    );
    assert_eq!(result["truncated"], true);
    assert_eq!(
        driver.runs(),
        vec![(Some("demo".into()), "db.users.find({})".into())]
    );
    let id = result["query_id"].as_str().unwrap();
    let page = direct_tool(
        &state,
        "get_query_result",
        json!({"query_id":id,"offset":2,"limit":3}),
    )
    .await;
    assert_eq!(
        page["document"]["documents"],
        json!([{"_id":3,"name":"row 3"},{"_id":4,"name":"row 4"}])
    );
    assert_eq!(page["truncated"], true);
    let last = direct_tool(
        &state,
        "get_query_result",
        json!({"query_id":id,"offset":4,"limit":3}),
    )
    .await;
    assert_eq!(
        last["document"]["documents"],
        json!([{"_id":5,"name":"row 5"}])
    );
    assert_eq!(last["truncated"], false);
    let cancelled = direct_tool(&state, "cancel_query", json!({"query_id":id})).await;
    assert_eq!(cancelled["query_id"], id);
    let closed = direct_tool(&state, "close_result", json!({"query_id":id})).await;
    assert_eq!(closed["closed"], true);
    assert_eq!(
        direct_tool(&state, "get_query_result", json!({"query_id":id})).await["error"]["code"],
        "not_found"
    );
    assert_eq!(
        direct_tool(
            &state,
            "run_query",
            json!({"connection_id":"c1","query":"db.users.deleteMany({})"})
        )
        .await["error"]["code"],
        "read_only"
    );
    assert_eq!(
        direct_tool(
            &state,
            "describe_object",
            json!({"connection_id":"c1","name":"users"})
        )
        .await["error"]["code"],
        "unsupported"
    );
    let state = driver_state(
        FakeDocDriver::new(FakeDocScript {
            run_error: Some(DriverError::new(
                ErrorKind::Syntax,
                "invalid document query",
            )),
            ..Default::default()
        }),
        AgentAccess::Read,
    );
    let result = direct_tool(
        &state,
        "run_query",
        json!({"connection_id":"c1","query":"db.users.find({})"}),
    )
    .await;
    assert_eq!(result["status"], "failed");
    assert_eq!(result["error"]["code"], "syntax");
    assert_eq!(result["error"]["message"], "invalid document query");
}

#[tokio::test(flavor = "multi_thread")]
async fn key_value_queries_return_replies_and_reject_blocking_commands() {
    use sqlr_testkit::{FakeKvDriver, FakeKvScript};
    let driver = FakeKvDriver::new(FakeKvScript::default());
    let state = driver_state(driver.clone(), AgentAccess::Write);
    let set = direct_tool(
        &state,
        "run_query",
        json!({"connection_id":"c1","query":"SET greeting hello"}),
    )
    .await;
    assert_eq!(set["reply"]["value"], "OK");
    let get = direct_tool(
        &state,
        "run_query",
        json!({"connection_id":"c1","query":"GET greeting"}),
    )
    .await;
    assert_eq!(get["kind"], "reply");
    assert_eq!(get["status"], "ready");
    assert_eq!(get["reply"]["value"], "hello");
    let id = get["query_id"].as_str().unwrap();
    let page = direct_tool(&state, "get_query_result", json!({"query_id":id})).await;
    assert_eq!(page["reply"]["value"], "hello");
    assert_eq!(
        direct_tool(&state, "close_result", json!({"query_id":id})).await["closed"],
        true
    );
    for query in [
        "MULTI",
        "EXEC",
        "DISCARD",
        "MONITOR",
        "SUBSCRIBE events",
        "PSUBSCRIBE *",
        "BLPOP queue 0",
    ] {
        assert_eq!(
            direct_tool(
                &state,
                "run_query",
                json!({"connection_id":"c1","query":query})
            )
            .await["error"]["code"],
            "unsupported",
            "{query}"
        );
    }
    assert_eq!(
        driver.commands(),
        vec![
            (
                0,
                vec![b"SET".to_vec(), b"greeting".to_vec(), b"hello".to_vec()]
            ),
            (0, vec![b"GET".to_vec(), b"greeting".to_vec()]),
        ]
    );
    let state = driver_state(
        FakeKvDriver::new(FakeKvScript::default()),
        AgentAccess::Read,
    );
    assert_eq!(
        direct_tool(
            &state,
            "run_query",
            json!({"connection_id":"c1","query":"SET key value"})
        )
        .await["error"]["code"],
        "read_only"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn profiles_validate_safety_flags_and_commit_modes_before_saving() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    let valid = json!({"name":"Safe","color":[1,2,3],"commit_mode":"manual","confirm_writes":true,
        "confirm_destructive":true,"read_only":true,"idle_tx_timeout_min":2});
    let result = client
        .tool(
            100,
            "config_write",
            json!({"target":"profile","payload":valid}),
        )
        .await;
    assert!(result["id"].as_str().unwrap().starts_with("profile-"));
    {
        let state = host.state.lock().unwrap();
        let saved = state.saved_profiles.last().unwrap();
        assert_eq!(saved.name, "Safe");
        assert_eq!(saved.color, Some([1, 2, 3]));
        assert_eq!(saved.commit_mode, sqlr_core::CommitMode::Manual);
        assert!(saved.confirm_writes && saved.confirm_destructive && saved.read_only);
        assert_eq!(saved.idle_tx_timeout_min, Some(2));
    }
    let before = host.state.lock().unwrap().saved_profiles.len();
    for (index, payload) in [
        json!({}),
        json!({"name":" "}),
        json!({"id":"missing"}),
        json!({"name":"P","color":"red"}),
        json!({"name":"P","color":[0,0,256]}),
        json!({"name":"P","commit_mode":"unknown"}),
        json!({"name":"P","read_only":"yes"}),
        json!({"name":"P","idle_tx_timeout_min":-1}),
        json!({"id":"production","confirm_writes":false}),
        json!({"id":"production","confirm_destructive":false}),
        json!({"id":"production","commit_mode":"auto"}),
    ]
    .into_iter()
    .enumerate()
    {
        let result = client
            .tool(
                200 + index as u64,
                "config_write",
                json!({"target":"profile","payload":payload}),
            )
            .await;
        assert_eq!(result["error"]["code"], "invalid", "{payload}: {result}");
    }
    assert_eq!(host.state.lock().unwrap().saved_profiles.len(), before);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn editor_tools_validate_and_forward_ranges_and_run_targets() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    let listed = client.tool(100, "list_query_tabs", json!({})).await;
    assert_eq!(listed["tabs"][0]["id"], 1);
    assert_eq!(listed["tabs"][0]["connection_id"], "c1");
    let tab = client.tool(101, "get_query_tab", json!({"tab_id":1})).await;
    assert_eq!(tab["text"], "select 1");
    assert_eq!(tab["revision"], 1);
    let edited = client
        .tool(
            102,
            "edit_query_tab",
            json!({"tab_id":1,"expected_revision":7,"edit":{"replace":"select 2"}}),
        )
        .await;
    assert_eq!(edited["tab_id"], 1);
    let edited = client.tool(103,"edit_query_tab",json!({"tab_id":1,"expected_revision":8,"edit":{"edits":[{"start":7,"end":8,"text":"3"}]}})).await;
    assert_eq!(edited["revision"], 1);
    {
        let state = host.state.lock().unwrap();
        assert!(matches!(&state.tab_edits[0],(1,7,TabEdit::Replace(text)) if text == "select 2"));
        let (id, revision, TabEdit::Edits(ranges)) = &state.tab_edits[1] else {
            panic!("range edit")
        };
        assert_eq!((*id, *revision), (1, 8));
        assert_eq!(
            (ranges[0].start, ranges[0].end, ranges[0].text.as_str()),
            (7, 8, "3")
        );
    }
    for (index, target) in [json!("script"), json!("selection"), json!({"statement":2})]
        .into_iter()
        .enumerate()
    {
        let result = client
            .tool(
                200 + index as u64,
                "run_query_tab",
                json!({"tab_id":1,"target":target,"max_rows":12}),
            )
            .await;
        assert_eq!(result, json!({"statements":[]}));
    }
    {
        let state = host.state.lock().unwrap();
        assert!(matches!(state.tab_runs[0], (1, RunTarget::Script, 12)));
        assert!(matches!(state.tab_runs[1], (1, RunTarget::Selection, 12)));
        assert!(matches!(
            state.tab_runs[2],
            (1, RunTarget::Statement(2), 12)
        ));
    }
    let invalid_edits = [
        json!({}),
        json!({"edits":[1]}),
        json!({"edits":[{"end":1,"text":"x"}]}),
        json!({"edits":[{"start":0,"text":"x"}]}),
        json!({"edits":[{"start":0,"end":1}]}),
        json!({"edits":[{"start":2,"end":1,"text":"x"}]}),
    ];
    for (index, edit) in invalid_edits.into_iter().enumerate() {
        let result = client
            .tool(
                300 + index as u64,
                "edit_query_tab",
                json!({"tab_id":1,"expected_revision":1,"edit":edit}),
            )
            .await;
        assert_eq!(result["error"]["code"], "invalid");
    }
    for (index, target) in [json!("unknown"), json!({"statement":-1}), json!(false)]
        .into_iter()
        .enumerate()
    {
        let result = client
            .tool(
                400 + index as u64,
                "run_query_tab",
                json!({"tab_id":1,"target":target}),
            )
            .await;
        assert_eq!(result["error"]["code"], "invalid");
    }
    assert_eq!(host.state.lock().unwrap().tab_edits.len(), 2);
    assert_eq!(host.state.lock().unwrap().tab_runs.len(), 3);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn configuration_deletion_requires_approval_and_protects_used_profiles() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    host.state.lock().unwrap().allow = false;
    let refused = client
        .tool(
            100,
            "config_delete",
            json!({"target":"connection","id":"c1"}),
        )
        .await;
    assert_eq!(refused["error"]["code"], "denied");
    assert!(host.state.lock().unwrap().deletions.is_empty());
    host.state.lock().unwrap().allow = true;
    for (index, (target, id)) in [
        ("connection", "c1"),
        ("folder", "work"),
        ("profile", "unused"),
        ("task", "task-1"),
    ]
    .into_iter()
    .enumerate()
    {
        let result = client
            .tool(
                200 + index as u64,
                "config_delete",
                json!({"target":target,"id":id}),
            )
            .await;
        assert_eq!(result, json!({"target":target,"id":id,"deleted":true}));
    }
    assert_eq!(
        host.state.lock().unwrap().deletions,
        vec![
            ("connection".into(), "c1".into()),
            ("folder".into(), "work".into()),
            ("profile".into(), "unused".into()),
            ("task".into(), "task-1".into()),
        ]
    );
    let result = client
        .tool(
            300,
            "config_delete",
            json!({"target":"profile","id":"development"}),
        )
        .await;
    assert_eq!(result["error"]["code"], "invalid");
    let result = client
        .tool(
            301,
            "config_delete",
            json!({"target":"connection","id":"missing"}),
        )
        .await;
    assert_eq!(result["error"]["code"], "not_found");
    for (index, (target, key)) in [
        ("folders", "folders"),
        ("settings", "settings"),
        ("keymap", "actions"),
        ("tasks", "tasks"),
    ]
    .into_iter()
    .enumerate()
    {
        let result = client
            .tool(400 + index as u64, "config_list", json!({"target":target}))
            .await;
        assert_eq!(result[key], json!([]));
    }
    let result = client
        .tool(
            500,
            "config_list",
            json!({"target":"drivers","driver_id":"missing"}),
        )
        .await;
    assert_eq!(result["drivers"], json!([]));
    server.shutdown().await;
}
