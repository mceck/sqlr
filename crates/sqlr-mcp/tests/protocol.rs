use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sqlr_core::{
    AgentAccess, ConnectionConfig, ConnectionProfile, Driver, DriverManifest, Secrets, SettingValue,
};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_mcp::host::{
    AgentConnection, AgentHost, ApprovalRequest, ConnectionWrite, FolderOp, GridCell, KeymapEntry,
    OpenTab, RunTarget, SettingEntry, SettingLevel, ShowView, Shown, TabDetail, TabEdit,
    TabResultRef, TabSummary,
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
    /// Connections created through `write_connection`, beside `config`.
    created: Vec<ConnectionConfig>,
    approvals: Vec<ApprovalRequest>,
    /// The engine tab runs execute on, once set.
    engine: Option<Arc<Engine>>,
    tab_text: String,
    tab_selection: Option<(usize, usize)>,
    tab_last_run: Vec<u64>,
    tab_transaction_open: bool,
    tab_grid_selection: Vec<GridCell>,
    opened_tabs: Vec<OpenTab>,
    shown: Vec<ShowView>,
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
                created: Vec::new(),
                approvals: Vec::new(),
                engine: None,
                tab_text: "select 1".into(),
                tab_selection: None,
                tab_last_run: Vec::new(),
                tab_transaction_open: false,
                tab_grid_selection: Vec::new(),
                opened_tabs: Vec::new(),
                shown: Vec::new(),
            }),
        }
    }
}

#[async_trait]
impl AgentHost for TestHost {
    async fn connection(&self, id: &str) -> sqlr_mcp::error::ToolResult<AgentConnection> {
        let state = self.state.lock();
        let config = if state.config.id == id {
            state.config.clone()
        } else {
            state
                .created
                .iter()
                .find(|config| config.id == id)
                .cloned()
                .ok_or_else(|| sqlr_mcp::error::ToolError::not_found("unknown connection"))?
        };
        Ok(AgentConnection {
            config,
            secrets: state.secrets.clone(),
            profile: state.profile.clone(),
            manifest: state.manifest.clone(),
        })
    }

    async fn connections(&self) -> sqlr_mcp::error::ToolResult<Vec<ConnectionConfig>> {
        let state = self.state.lock();
        let mut all = vec![state.config.clone()];
        all.extend(state.created.iter().cloned());
        Ok(all)
    }
    async fn folders(&self) -> sqlr_mcp::error::ToolResult<Vec<String>> {
        Ok(Vec::new())
    }
    async fn profiles(&self) -> sqlr_mcp::error::ToolResult<Vec<ConnectionProfile>> {
        Ok(ConnectionProfile::builtins())
    }
    async fn save_profile(&self, profile: ConnectionProfile) -> sqlr_mcp::error::ToolResult<()> {
        self.state.lock().saved_profiles.push(profile);
        Ok(())
    }
    async fn delete_profile(&self, _id: &str) -> sqlr_mcp::error::ToolResult<()> {
        Ok(())
    }
    async fn drivers(&self) -> sqlr_mcp::error::ToolResult<Vec<DriverManifest>> {
        Ok(vec![self.state.lock().manifest.clone()])
    }
    async fn secret_presence(
        &self,
        _id: &str,
    ) -> sqlr_mcp::error::ToolResult<BTreeMap<String, bool>> {
        Ok(BTreeMap::new())
    }
    async fn settings(&self) -> sqlr_mcp::error::ToolResult<Vec<SettingEntry>> {
        Ok(self.state.lock().setting_entries.clone())
    }
    async fn set_setting(
        &self,
        id: &str,
        level: SettingLevel,
        level_id: Option<&str>,
        value: Option<SettingValue>,
    ) -> sqlr_mcp::error::ToolResult<()> {
        self.state.lock().setting_writes.push((
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
            .keymap_writes
            .push((action.into(), chords));
        Ok(())
    }
    async fn write_connection(&self, write: ConnectionWrite) -> sqlr_mcp::error::ToolResult<()> {
        let mut state = self.state.lock();
        if write.config.agent_access != AgentAccess::None
            && write.config.agent_access != state.config.agent_access
        {
            return Err(sqlr_mcp::error::ToolError::invalid(
                "exposure is human-only",
            ));
        }
        if write.config.id != state.config.id {
            state.created.retain(|config| config.id != write.config.id);
            state.created.push(write.config.clone());
        }
        state.writes.push(write);
        Ok(())
    }
    async fn folder(&self, op: FolderOp) -> sqlr_mcp::error::ToolResult<()> {
        self.state.lock().folders.push(op);
        Ok(())
    }
    async fn delete(&self, target: &str, id: &str) -> sqlr_mcp::error::ToolResult<()> {
        self.state.lock().deletions.push((target.into(), id.into()));
        Ok(())
    }
    async fn approve(&self, request: ApprovalRequest) -> sqlr_mcp::error::ToolResult<()> {
        let mut state = self.state.lock();
        state.approvals.push(request);
        if state.allow {
            Ok(())
        } else {
            Err(sqlr_mcp::error::ToolError::denied())
        }
    }
    async fn tabs(&self) -> sqlr_mcp::error::ToolResult<Vec<TabSummary>> {
        Ok(vec![TabSummary {
            id: 1,
            title: "Query".into(),
            connection_id: Some(self.state.lock().config.id.clone()),
            database: None,
            dirty: false,
            revision: 1,
            busy: false,
        }])
    }
    async fn tab(&self, id: u64) -> sqlr_mcp::error::ToolResult<TabDetail> {
        if id != 1 && !(100..200).contains(&id) {
            return Err(sqlr_mcp::error::ToolError::not_found("unknown tab"));
        }
        let state = self.state.lock();
        let busy = state.engine.as_ref().is_some_and(|engine| {
            state.tab_last_run.iter().any(|q| {
                engine
                    .result(sqlr_engine::QueryId(*q))
                    .is_some_and(|r| r.read().status == sqlr_engine::QueryStatus::Running)
            })
        });
        Ok(TabDetail {
            id,
            title: "Query".into(),
            connection_id: Some(state.config.id.clone()),
            database: None,
            dirty: false,
            revision: 1,
            busy,
            text: state.tab_text.clone(),
            caret: 0,
            selection: state.tab_selection,
            last_run: state.tab_last_run.clone(),
            transaction_open: state.tab_transaction_open,
            grid_selection: state.tab_grid_selection.clone(),
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
            .tab_edits
            .push((id, expected_revision, edit));
        self.tab(1).await
    }
    async fn tab_statements(
        &self,
        _id: u64,
        target: RunTarget,
    ) -> sqlr_mcp::error::ToolResult<Vec<String>> {
        let state = self.state.lock();
        let all: Vec<String> = state
            .tab_text
            .split(';')
            .map(str::trim)
            .filter(|sql| !sql.is_empty())
            .map(str::to_string)
            .collect();
        match target {
            RunTarget::Script => Ok(all),
            RunTarget::Statement(index) => all
                .get(index)
                .cloned()
                .map(|sql| vec![sql])
                .ok_or_else(|| sqlr_mcp::error::ToolError::invalid("no such statement")),
            RunTarget::Selection => match state.tab_selection {
                Some((start, end)) if start < end => Ok(vec![state.tab_text[start..end].into()]),
                _ => Err(sqlr_mcp::error::ToolError::invalid(
                    "The tab has no selection",
                )),
            },
            RunTarget::Caret => Ok(all.into_iter().take(1).collect()),
        }
    }
    async fn run_tab(
        &self,
        id: u64,
        target: RunTarget,
        max_rows: usize,
        statements: Vec<String>,
    ) -> sqlr_mcp::error::ToolResult<Vec<u64>> {
        if self.tab_statements(id, target).await? != statements {
            return Err(sqlr_mcp::error::ToolError::conflict(1));
        }
        let mut state = self.state.lock();
        state.tab_runs.push((id, target, max_rows));
        let Some(engine) = state.engine.clone() else {
            return Ok(Vec::new());
        };
        let conn = state.config.id.clone();
        let ids: Vec<u64> = statements
            .into_iter()
            .map(|sql| {
                engine
                    .execute_in(&sqlr_engine::SessionKey::meta(&conn), None, sql, max_rows)
                    .0
            })
            .collect();
        state.tab_last_run = ids.clone();
        Ok(ids)
    }
    async fn tab_result(&self, id: u64, query: u64) -> sqlr_mcp::error::ToolResult<TabResultRef> {
        let state = self.state.lock();
        if id != 1 || !state.tab_last_run.contains(&query) {
            return Err(sqlr_mcp::error::ToolError::not_found("no such tab result"));
        }
        Ok(TabResultRef {
            connection_id: Some(state.config.id.clone()),
            query,
        })
    }
    async fn open_tab(&self, open: OpenTab) -> sqlr_mcp::error::ToolResult<TabDetail> {
        let id = {
            let mut state = self.state.lock();
            state.opened_tabs.push(open);
            99 + state.opened_tabs.len() as u64
        };
        self.tab(id).await
    }
    async fn show(&self, view: ShowView, open_allowed: bool) -> sqlr_mcp::error::ToolResult<Shown> {
        let mut state = self.state.lock();
        // An ER diagram of the same tables focuses the tab it opened.
        if let ShowView::ErDiagram { tables, .. } = &view
            && let Some(index) = state.shown.iter().position(
                |shown| matches!(shown, ShowView::ErDiagram { tables: open, .. } if open == tables),
            )
        {
            state.shown.push(view);
            return Ok(Shown {
                tab_id: Some(151 + index as u64),
                opened: false,
            });
        }
        let opened = matches!(
            view,
            ShowView::Ddl { .. } | ShowView::TableData { .. } | ShowView::ErDiagram { .. }
        );
        if opened && !open_allowed {
            return Err(sqlr_mcp::error::ToolError::tab_limit());
        }
        let explained = match &view {
            ShowView::Explain { tab_id, .. } => Some(*tab_id),
            _ => None,
        };
        state.shown.push(view);
        Ok(Shown {
            tab_id: explained.or(opened.then_some(150 + state.shown.len() as u64)),
            opened,
        })
    }
    async fn toast(&self, text: &str) {
        self.state.lock().toasts.push(text.to_string());
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

    async fn initialize(&mut self) -> Value {
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
        event_json(&text)
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
async fn auth_headers_capabilities_and_instructions() {
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

    let initialized = client.initialize().await;
    assert_eq!(server.session_count(), 1);
    let instructions = initialized["result"]["instructions"].as_str().unwrap();
    for required in [
        "only while the app",
        "exposed",
        "Approval",
        "tighten",
        "write-only",
        "npx skills add mceck/sqlr",
        "Integrated terminal",
    ] {
        assert!(
            instructions.contains(required),
            "missing {required}: {instructions}"
        );
    }
    assert!(!instructions.contains("get_guide"), "{instructions}");
    let tools = client.rpc(2, "tools/list", json!({})).await;
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    let expected: Vec<&str> = sqlr_mcp::registry::TOOLS.iter().map(|t| t.name).collect();
    assert_eq!(names, expected);
    assert!(!names.contains(&"get_guide".to_string()));
    assert!(names.contains(&"run_query".to_string()));
    let unknown = client
        .rpc(4, "tools/call", json!({"name":"get_guide","arguments":{}}))
        .await;
    assert!(unknown.get("error").is_some(), "{unknown}");

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
    host.state.lock().config.agent_access = AgentAccess::Read;
    let mut reader = Client::new(port, &token);
    reader.initialize().await;
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
    host.state.lock().allow = false;
    let denied = client
        .tool(
            5,
            "run_query",
            json!({"connection_id":"c1","query":"drop table t"}),
        )
        .await;
    assert_eq!(denied["error"]["code"], "denied");
    host.state.lock().allow = true;

    // Exposure and safety are enforced before approval.
    host.state.lock().profile.read_only = true;
    let read_only = client
        .tool(
            6,
            "run_query",
            json!({"connection_id":"c1","query":"insert into t values (3, 'c')"}),
        )
        .await;
    assert_eq!(read_only["error"]["code"], "read_only");
    host.state.lock().profile.read_only = false;

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
    assert!(host.state.lock().writes.is_empty());
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
        let state = host.state.lock();
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
        host.state.lock().writes.last().unwrap().config.agent_access,
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
        let mut state = host.state.lock();
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
            let state = host.state.lock();
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
    let before = host.state.lock().setting_writes.len();
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
    assert_eq!(host.state.lock().setting_writes.len(), before);
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
    assert_eq!(host.state.lock().setting_writes.last().unwrap().3, None);
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
        let state = host.state.lock();
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
        let state = host.state.lock();
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
    assert_eq!(host.state.lock().keymap_writes.len(), 2);
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
        let state = host.state.lock();
        let saved = state.saved_profiles.last().unwrap();
        assert_eq!(saved.name, "Safe");
        assert_eq!(saved.color, Some([1, 2, 3]));
        assert_eq!(saved.commit_mode, sqlr_core::CommitMode::Manual);
        assert!(saved.confirm_writes && saved.confirm_destructive && saved.read_only);
        assert_eq!(saved.idle_tx_timeout_min, Some(2));
    }
    let before = host.state.lock().saved_profiles.len();
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
    assert_eq!(host.state.lock().saved_profiles.len(), before);
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
        let state = host.state.lock();
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
    {
        let mut state = host.state.lock();
        state.tab_text = "select 1; select 2".into();
        state.tab_selection = Some((0, 8));
    }
    for (index, target) in [json!("script"), json!("selection"), json!({"statement":1})]
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
        assert_eq!(result["tab_id"], 1, "{result}");
        assert_eq!(result["query_ids"], json!([]), "{result}");
    }
    {
        let state = host.state.lock();
        assert!(matches!(state.tab_runs[0], (1, RunTarget::Script, 12)));
        assert!(matches!(state.tab_runs[1], (1, RunTarget::Selection, 12)));
        assert!(matches!(
            state.tab_runs[2],
            (1, RunTarget::Statement(1), 12)
        ));
    }
    host.state.lock().tab_selection = None;
    for (index, target) in [json!("selection"), json!({"statement":2})]
        .into_iter()
        .enumerate()
    {
        let result = client
            .tool(
                250 + index as u64,
                "run_query_tab",
                json!({"tab_id":1,"target":target}),
            )
            .await;
        assert_eq!(result["error"]["code"], "invalid", "{result}");
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
    assert_eq!(host.state.lock().tab_edits.len(), 2);
    assert_eq!(host.state.lock().tab_runs.len(), 3);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn configuration_deletion_requires_approval_and_protects_used_profiles() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    host.state.lock().allow = false;
    let refused = client
        .tool(
            100,
            "config_delete",
            json!({"target":"connection","id":"c1"}),
        )
        .await;
    assert_eq!(refused["error"]["code"], "denied");
    assert!(host.state.lock().deletions.is_empty());
    host.state.lock().allow = true;
    for (index, (target, id)) in [
        ("connection", "c1"),
        ("folder", "work"),
        ("profile", "unused"),
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
        host.state.lock().deletions,
        vec![
            ("connection".into(), "c1".into()),
            ("folder".into(), "work".into()),
            ("profile".into(), "unused".into()),
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
    assert_eq!(result["error"]["code"], "not_exposed");
    for (index, (target, key)) in [
        ("folders", "folders"),
        ("settings", "settings"),
        ("keymap", "actions"),
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
    // Saved tasks are gone from the surface.
    let result = client
        .tool(501, "config_list", json!({"target":"tasks"}))
        .await;
    assert_eq!(result["error"]["code"], "invalid");
    let result = client
        .tool(502, "config_delete", json!({"target":"task","id":"t1"}))
        .await;
    assert_eq!(result["error"]["code"], "invalid");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn configuration_writes_reach_only_exposed_or_session_created_connections() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::None, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;

    // A non-exposed connection answers like an unknown one, and nothing reaches the host.
    for (index, id) in ["c1", "missing"].into_iter().enumerate() {
        let result = client
            .tool(
                100 + index as u64,
                "config_write",
                json!({"target":"connection","payload":{"id":id,"values":{"path":"/tmp/evil.db"}}}),
            )
            .await;
        assert_eq!(result["error"]["code"], "not_exposed", "{id}: {result}");
        let result = client
            .tool(
                110 + index as u64,
                "config_delete",
                json!({"target":"connection","id":id}),
            )
            .await;
        assert_eq!(result["error"]["code"], "not_exposed", "{id}: {result}");
        let result = client
            .tool(
                120 + index as u64,
                "config_write",
                json!({"target":"setting","payload":{
                    "id":"text","level":{"name":"connection","id":id},"value":"x"
                }}),
            )
            .await;
        assert_eq!(result["error"]["code"], "not_exposed", "{id}: {result}");
    }
    {
        let state = host.state.lock();
        assert!(state.writes.is_empty());
        assert!(state.deletions.is_empty());
        assert!(state.setting_writes.is_empty());
    }

    // The session that created a connection may finish it; another session may not.
    let created = client
        .tool(
            200,
            "config_write",
            json!({"target":"connection","payload":{"driver":"sqlite","name":"New"}}),
        )
        .await;
    let id = created["id"].as_str().unwrap().to_string();
    let patched = client
        .tool(
            201,
            "config_write",
            json!({"target":"connection","payload":{"id":id,"values":{"path":"/tmp/new.db"}}}),
        )
        .await;
    assert_eq!(patched["created"], false, "{patched}");
    let mut other = Client::new(port, &token);
    other.initialize().await;
    let refused = other
        .tool(
            202,
            "config_write",
            json!({"target":"connection","payload":{"id":id,"name":"Taken"}}),
        )
        .await;
    assert_eq!(refused["error"]["code"], "not_exposed", "{refused}");
    let refused = other
        .tool(203, "config_delete", json!({"target":"connection","id":id}))
        .await;
    assert_eq!(refused["error"]["code"], "not_exposed", "{refused}");
    let deleted = client
        .tool(204, "config_delete", json!({"target":"connection","id":id}))
        .await;
    assert_eq!(deleted["deleted"], true, "{deleted}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_values_name_only_exposed_connections() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::None, store).await;
    {
        let mut state = host.state.lock();
        let mut entry = setting_entry("text", json!({"type":"text"}));
        entry.values.insert("global".into(), json!("g"));
        entry.values.insert("profile:production".into(), json!("p"));
        entry.values.insert("connection:c1".into(), json!("hidden"));
        state.setting_entries.push(entry);
    }
    let mut client = Client::new(port, &token);
    client.initialize().await;
    let listed = client
        .tool(2, "config_list", json!({"target":"settings"}))
        .await;
    assert_eq!(
        listed["settings"][0]["values"],
        json!({"global":"g","profile:production":"p"})
    );
    assert!(!listed.to_string().contains("c1"), "{listed}");

    host.state.lock().config.agent_access = AgentAccess::Read;
    let listed = client
        .tool(3, "config_list", json!({"target":"settings"}))
        .await;
    assert_eq!(listed["settings"][0]["values"]["connection:c1"], "hidden");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn describe_object_ddl_and_explain_read_the_catalog() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::Write, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    for (index, sql) in [
        "create table customers (id integer primary key, email text not null)",
        "create table orders (id integer primary key, customer_id integer not null \
         references customers (id) on delete cascade, total real)",
        "create unique index orders_customer on orders (customer_id) where total > 0",
    ]
    .into_iter()
    .enumerate()
    {
        let created = client
            .tool(
                10 + index as u64,
                "run_query",
                json!({"connection_id":"c1","query":sql}),
            )
            .await;
        assert_eq!(created["status"], "ready", "{created}");
    }

    let described = client
        .tool(
            20,
            "describe_object",
            json!({"connection_id":"c1","name":"orders"}),
        )
        .await;
    assert_eq!(
        described["indexes"][0]["name"], "orders_customer",
        "{described}"
    );
    assert_eq!(described["indexes"][0]["columns"], json!(["customer_id"]));
    assert_eq!(described["indexes"][0]["unique"], true);
    assert!(
        described["indexes"][0]["predicate"]
            .as_str()
            .unwrap()
            .contains("total"),
        "{described}"
    );
    let fk = &described["foreign_keys"][0];
    assert_eq!(fk["columns"], json!(["customer_id"]), "{described}");
    assert_eq!(fk["referenced_table"]["name"], "customers");
    assert_eq!(fk["referenced_columns"], json!(["id"]));
    assert_eq!(fk["on_delete"], "CASCADE");
    assert_eq!(fk["on_update"], "NO ACTION");

    // get_ddl takes a browse path.
    let root = client
        .tool(30, "browse", json!({"connection_id":"c1"}))
        .await;
    let mut path: Option<Value> = None;
    let mut frontier: Vec<Value> = root["nodes"].as_array().unwrap().clone();
    let mut call = 31;
    while path.is_none() {
        let node = frontier.remove(0);
        if node["name"] == "orders" && node["kind"] == "table" {
            path = Some(node["path"].clone());
            break;
        }
        if node["has_children"] == true && node["kind"] != "table" {
            let children = client
                .tool(
                    call,
                    "browse",
                    json!({"connection_id":"c1","path":node["path"]}),
                )
                .await;
            call += 1;
            frontier.extend(children["nodes"].as_array().unwrap().iter().cloned());
        }
    }
    let ddl = client
        .tool(
            50,
            "get_ddl",
            json!({"connection_id":"c1","path":path.clone().unwrap()}),
        )
        .await;
    let text = ddl["ddl"].as_str().unwrap_or_else(|| panic!("{ddl}"));
    assert!(text.to_ascii_uppercase().contains("CREATE TABLE"), "{text}");
    assert!(text.contains("orders"), "{text}");
    let missing = client
        .tool(
            51,
            "get_ddl",
            json!({"connection_id":"c1","path":["main","tables","nope"]}),
        )
        .await;
    assert_eq!(missing["error"]["code"], "not_found", "{missing}");

    // Explain: a plan with the operator mix and the raw text.
    let plan = client
        .tool(
            60,
            "explain_query",
            json!({"connection_id":"c1","query":"select * from orders where customer_id = 1"}),
        )
        .await;
    assert!(plan.get("error").is_none(), "{plan}");
    assert!(!plan["raw"].as_str().unwrap().is_empty(), "{plan}");
    assert!(plan["operators"]["nodes"].as_u64().unwrap() >= 1, "{plan}");
    assert!(plan["warnings"].is_array(), "{plan}");
    let multi = client
        .tool(
            61,
            "explain_query",
            json!({"connection_id":"c1","query":"select 1; select 2"}),
        )
        .await;
    assert_eq!(multi["error"]["code"], "invalid", "{multi}");
    // SQLite has no EXPLAIN ANALYZE.
    let analyzed = client
        .tool(
            62,
            "explain_query",
            json!({"connection_id":"c1","query":"select * from orders","analyze":true}),
        )
        .await;
    assert_eq!(analyzed["error"]["code"], "unsupported", "{analyzed}");
    // Analyze on a write waits on an Approval first.
    host.state.lock().allow = false;
    let denied = client
        .tool(
            63,
            "explain_query",
            json!({"connection_id":"c1","query":"delete from orders","analyze":true}),
        )
        .await;
    assert_eq!(denied["error"]["code"], "denied", "{denied}");
    assert_eq!(host.state.lock().approvals.len(), 1);
    host.state.lock().allow = true;
    // A Read exposure refuses analyze on a write before any Approval.
    host.state.lock().config.agent_access = AgentAccess::Read;
    let mut reader = Client::new(port, &token);
    reader.initialize().await;
    let refused = reader
        .tool(
            64,
            "explain_query",
            json!({"connection_id":"c1","query":"delete from orders","analyze":true}),
        )
        .await;
    assert_eq!(refused["error"]["code"], "read_only", "{refused}");
    assert_eq!(host.state.lock().approvals.len(), 1);
    // Without analyze a write's plan is only read.
    let plain = reader
        .tool(
            65,
            "explain_query",
            json!({"connection_id":"c1","query":"delete from orders where id = 1"}),
        )
        .await;
    assert!(plain.get("error").is_none(), "{plain}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn search_history_filters_exposed_connections_before_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(sqlr_store::Db::open_in_memory().unwrap());
    let entry = |conn: &str, sql: &str, at: i64| sqlr_core::HistoryEntry {
        id: 0,
        conn_id: conn.into(),
        database: Some("main".into()),
        sql: sql.into(),
        started_at_ms: at,
        duration_ms: 3,
        outcome: sqlr_core::HistoryOutcome::Rows(1),
    };
    db.add_history(&entry("c1", "select 1 from visible", 1))
        .unwrap();
    db.add_history(&entry("c1", "select 2 from visible", 2))
        .unwrap();
    for at in 10..20 {
        db.add_history(&entry("hidden", "select 3 from visible", at))
            .unwrap();
    }
    let driver = SqliteDriver::new();
    let manifest = driver.manifest().clone();
    let mut drivers = DriverRegistry::default();
    drivers.register(driver);
    let engine = Arc::new(Engine::with_db(drivers, Arc::new(|| {}), db));
    let host = Arc::new(TestHost::new(
        sqlite_config(dir.path(), AgentAccess::Read),
        manifest,
    ));
    let state = Arc::new(sqlr_mcp::state::ServerState::new(
        engine,
        host.clone(),
        0,
        "t".into(),
    ));
    let found = direct_tool(
        &state,
        "search_history",
        json!({"query":"VISIBLE","limit":2}),
    )
    .await;
    let entries = found["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("{found}"));
    assert_eq!(entries.len(), 2, "{found}");
    assert_eq!(entries[0]["sql"], "select 2 from visible");
    assert_eq!(entries[0]["connection_id"], "c1");
    assert_eq!(entries[0]["connection_name"], "Test");
    assert_eq!(entries[0]["database"], "main");
    assert_eq!(entries[0]["duration_ms"], 3);
    assert_eq!(entries[0]["outcome"]["status"], "rows");
    assert!(!found.to_string().contains("hidden"), "{found}");
    let only = direct_tool(&state, "search_history", json!({"connection_id":"hidden"})).await;
    assert_eq!(only["error"]["code"], "not_exposed", "{only}");
    let too_many = direct_tool(&state, "search_history", json!({"limit":201})).await;
    assert_eq!(too_many["error"]["code"], "invalid", "{too_many}");
    host.state.lock().config.agent_access = AgentAccess::None;
    let none = direct_tool(&state, "search_history", json!({})).await;
    assert_eq!(none["entries"], json!([]), "{none}");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connection_reports_success_failures_and_missing_secrets() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, dir, port, token) = setup(AgentAccess::Read, store).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    let ok = client
        .tool(2, "test_connection", json!({"connection_id":"c1"}))
        .await;
    assert_eq!(ok["ok"], true, "{ok}");
    assert!(ok["server_version"].as_str().is_some(), "{ok}");
    // The test session is not the explorer's.
    assert!(server.session_count() == 1);

    host.state.lock().config.values.insert(
        "path".into(),
        dir.path().join("missing/dir/t.db").display().to_string(),
    );
    host.state
        .lock()
        .config
        .values
        .insert("create_if_missing".into(), "false".into());
    let failed = client
        .tool(3, "test_connection", json!({"connection_id":"c1"}))
        .await;
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "connection", "{failed}");
    assert!(failed["error"]["message"].as_str().is_some());

    host.state.lock().config.agent_access = AgentAccess::None;
    let hidden = client
        .tool(4, "test_connection", json!({"connection_id":"c1"}))
        .await;
    assert_eq!(hidden["error"]["code"], "not_exposed", "{hidden}");
    server.shutdown().await;

    // A driver that needs a password names the missing secret field.
    use sqlr_testkit::{FakeDriver, FakeScript};
    let state = driver_state(
        FakeDriver::new(FakeScript {
            password: Some("pw".into()),
            ..Default::default()
        }),
        AgentAccess::Read,
    );
    let missing = direct_tool(&state, "test_connection", json!({"connection_id":"c1"})).await;
    assert_eq!(missing["error"]["code"], "secret_required", "{missing}");
    assert!(
        missing["error"]["message"]
            .as_str()
            .unwrap()
            .contains("password"),
        "{missing}"
    );
}

/// A server whose test host runs tab statements on the server's own engine.
async fn setup_with_engine(
    access: AgentAccess,
) -> (McpServer, Arc<TestHost>, tempfile::TempDir, u16, String) {
    let dir = tempfile::tempdir().unwrap();
    let config = sqlite_config(dir.path(), access);
    let driver = SqliteDriver::new();
    let manifest = driver.manifest().clone();
    let mut drivers = DriverRegistry::default();
    drivers.register(driver);
    let engine = Arc::new(Engine::new(drivers, Arc::new(|| {})));
    let host = Arc::new(TestHost::new(config, manifest));
    host.state.lock().engine = Some(engine.clone());
    let port = free_port();
    let server = McpServer::start(StartOptions {
        port,
        data_dir: dir.path().join("data"),
        engine,
        host: host.clone(),
        token_store: Arc::new(MemoryMcpTokenStore::with_token("t")),
        app_version: env!("CARGO_PKG_VERSION").into(),
    })
    .await
    .unwrap();
    (server, host, dir, port, "t".into())
}

#[tokio::test(flavor = "multi_thread")]
async fn tab_runs_return_result_ids_the_tab_owns() {
    let (server, host, _dir, port, token) = setup_with_engine(AgentAccess::Write).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    host.state.lock().tab_text =
        "create table t (id integer, name text); insert into t values (1, 'a'), (2, 'b'); \
         select id, name from t order by id"
            .into();
    let run = client
        .tool(2, "run_query_tab", json!({"tab_id":1,"target":"script"}))
        .await;
    assert_eq!(run["tab_id"], 1, "{run}");
    assert_eq!(run["status"], "ready", "{run}");
    let ids: Vec<String> = run["query_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 3, "{run}");

    // The same envelope as run_query.
    let page = client
        .tool(
            3,
            "get_query_result",
            json!({"query_id": ids[2], "offset": 1, "limit": 5}),
        )
        .await;
    assert_eq!(page["kind"], "tabular", "{page}");
    assert_eq!(page["query_id"], ids[2]);
    assert_eq!(page["tabular"]["rows"], json!([[2, "b"]]), "{page}");
    let inserted = client
        .tool(4, "get_query_result", json!({"query_id": ids[1]}))
        .await;
    assert_eq!(inserted["tabular"]["affected"], 2, "{inserted}");

    // get_query_tab lists the last run, the transaction and the selection.
    host.state.lock().tab_grid_selection = vec![GridCell {
        column: "name".into(),
        row: 1,
        value: json!("b"),
    }];
    let tab = client.tool(5, "get_query_tab", json!({"tab_id":1})).await;
    assert_eq!(tab["last_run_query_ids"], json!(ids), "{tab}");
    assert_eq!(tab["transaction_open"], false);
    assert_eq!(
        tab["grid_selection"],
        json!([{"column":"name","row":1,"value":"b"}])
    );

    // The tab owns its results.
    let closed = client
        .tool(6, "close_result", json!({"query_id": ids[2]}))
        .await;
    assert_eq!(closed["error"]["code"], "invalid", "{closed}");
    let still = client
        .tool(7, "get_query_result", json!({"query_id": ids[2]}))
        .await;
    assert_eq!(still["tabular"]["rows"][0], json!([1, "a"]), "{still}");

    // A new run turns the previous ids into not_found.
    host.state.lock().tab_text = "select 3".into();
    let rerun = client
        .tool(8, "run_query_tab", json!({"tab_id":1,"target":"script"}))
        .await;
    assert_eq!(rerun["query_ids"].as_array().unwrap().len(), 1, "{rerun}");
    let gone = client
        .tool(9, "get_query_result", json!({"query_id": ids[2]}))
        .await;
    assert_eq!(gone["error"]["code"], "not_found", "{gone}");

    // Run ids, the transaction and the selection stay hidden on a non-exposed connection.
    host.state.lock().config.agent_access = AgentAccess::None;
    let hidden = client.tool(10, "get_query_tab", json!({"tab_id":1})).await;
    assert_eq!(hidden["text"], "select 3");
    for key in ["last_run_query_ids", "transaction_open", "grid_selection"] {
        assert!(hidden.get(key).is_none(), "{key}: {hidden}");
    }
    let refused = client
        .tool(
            11,
            "get_query_result",
            json!({"query_id": rerun["query_ids"][0]}),
        )
        .await;
    assert_eq!(refused["error"]["code"], "not_exposed", "{refused}");
    let refused = client
        .tool(12, "run_query_tab", json!({"tab_id":1,"target":"script"}))
        .await;
    assert_eq!(refused["error"]["code"], "not_exposed", "{refused}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tab_runs_that_need_confirmation_wait_on_an_approval_and_run_once() {
    let (server, host, _dir, port, token) = setup_with_engine(AgentAccess::Write).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    {
        let mut state = host.state.lock();
        state.profile.confirm_writes = true;
        state.tab_text = "create table t (id integer)".into();
        state.allow = false;
    }
    let denied = client
        .tool(2, "run_query_tab", json!({"tab_id":1,"target":"script"}))
        .await;
    assert_eq!(denied["error"]["code"], "denied", "{denied}");
    {
        let state = host.state.lock();
        assert!(state.tab_runs.is_empty(), "a denied run never starts");
        assert_eq!(state.approvals.len(), 1);
        assert_eq!(
            state.approvals[0].statement.as_deref(),
            Some("create table t (id integer)")
        );
    }
    host.state.lock().allow = true;
    let allowed = client
        .tool(3, "run_query_tab", json!({"tab_id":1,"target":"script"}))
        .await;
    assert_eq!(allowed["status"], "ready", "{allowed}");
    assert_eq!(host.state.lock().tab_runs.len(), 1, "it runs exactly once");
    assert!(!allowed.to_string().contains("approval_required"));

    // A Read exposure refuses a write before any Approval.
    host.state.lock().config.agent_access = AgentAccess::Read;
    let refused = client
        .tool(4, "run_query_tab", json!({"tab_id":1,"target":"script"}))
        .await;
    assert_eq!(refused["error"]["code"], "read_only", "{refused}");
    assert_eq!(host.state.lock().approvals.len(), 2);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn open_query_tab_and_show_in_app_validate_and_share_the_tab_limit() {
    let (server, host, _dir, port, token) = setup_with_engine(AgentAccess::Read).await;
    let mut client = Client::new(port, &token);
    client.initialize().await;
    client
        .tool(
            2,
            "run_query",
            json!({"connection_id":"c1","query":"select 1"}),
        )
        .await;
    for (index, args) in [
        json!({"connection_id":"c1","view":"nowhere"}),
        json!({"connection_id":"c1","view":"ddl"}),
        json!({"connection_id":"c1","view":"ddl","path":[]}),
        json!({"connection_id":"c1","view":"er_diagram","tables":[]}),
        json!({"connection_id":"c1","view":"explain"}),
        json!({"connection_id":"c1","view":"explain","tab_id":1,"query":"select 1"}),
        json!({"connection_id":"c1","view":"explain","query":"select 1; select 2"}),
    ]
    .into_iter()
    .enumerate()
    {
        let result = client
            .tool(10 + index as u64, "show_in_app", args.clone())
            .await;
        assert_eq!(result["error"]["code"], "invalid", "{args}: {result}");
    }
    let missing = client
        .tool(
            20,
            "show_in_app",
            json!({"connection_id":"c1","view":"ddl","path":["main","tables","nope"]}),
        )
        .await;
    assert_eq!(missing["error"]["code"], "not_found", "{missing}");
    let explorer = client
        .tool(
            21,
            "show_in_app",
            json!({"connection_id":"c1","view":"explorer"}),
        )
        .await;
    assert!(explorer.get("tab_id").is_none(), "{explorer}");
    assert!(matches!(
        host.state.lock().shown.last(),
        Some(ShowView::Explorer { path, .. }) if path.is_empty()
    ));
    let explained = client
        .tool(
            22,
            "show_in_app",
            json!({"connection_id":"c1","view":"explain","tab_id":1}),
        )
        .await;
    assert!(explained.get("error").is_none(), "{explained}");
    assert!(matches!(
        host.state.lock().shown.last(),
        Some(ShowView::Explain { tab_id: 1, statement, analyze: false }) if statement == "select 1"
    ));
    host.state.lock().tab_transaction_open = true;
    let busy = client
        .tool(
            23,
            "show_in_app",
            json!({"connection_id":"c1","view":"explain","tab_id":1}),
        )
        .await;
    assert_eq!(busy["error"]["code"], "busy", "{busy}");
    // The statement is checked before the open transaction.
    host.state.lock().tab_text = "delete from t".into();
    let refused = client
        .tool(
            24,
            "show_in_app",
            json!({"connection_id":"c1","view":"explain","tab_id":1,"analyze":true}),
        )
        .await;
    assert_eq!(refused["error"]["code"], "read_only", "{refused}");
    host.state.lock().tab_text = "select 1".into();

    // Five tabs per MCP session, across open_query_tab and the tab-opening views.
    let opened = client
        .tool(
            30,
            "open_query_tab",
            json!({"connection_id":"c1","text":"select 42","title":"Proposal","database":"main"}),
        )
        .await;
    assert_eq!(opened["tab_id"], 100, "{opened}");
    assert_eq!(opened["revision"], 1);
    {
        let state = host.state.lock();
        let tab = &state.opened_tabs[0];
        assert_eq!(tab.text, "select 42");
        assert_eq!(tab.title.as_deref(), Some("Proposal"));
        assert_eq!(tab.database.as_deref(), Some("main"));
    }
    let explain_tab = client
        .tool(
            31,
            "show_in_app",
            json!({"connection_id":"c1","view":"explain","query":"select 2"}),
        )
        .await;
    assert_eq!(explain_tab["tab_id"], 101, "{explain_tab}");
    let er = client
        .tool(
            32,
            "show_in_app",
            json!({"connection_id":"c1","view":"er_diagram","tables":["a","b"]}),
        )
        .await;
    assert!(er["tab_id"].is_u64(), "{er}");
    assert!(matches!(
        host.state.lock().shown.last(),
        Some(ShowView::ErDiagram { schema, tables, .. }) if schema == "main" && tables.len() == 2
    ));
    for call in 33..35 {
        let more = client
            .tool(
                call,
                "open_query_tab",
                json!({"connection_id":"c1","text":"select 1"}),
            )
            .await;
        assert!(more["tab_id"].is_u64(), "{more}");
    }
    let sixth = client
        .tool(
            40,
            "open_query_tab",
            json!({"connection_id":"c1","text":"select 1"}),
        )
        .await;
    assert_eq!(sixth["error"]["code"], "busy", "{sixth}");
    assert!(
        sixth["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reuse")
    );
    let sixth = client
        .tool(
            41,
            "show_in_app",
            json!({"connection_id":"c1","view":"explain","query":"select 1"}),
        )
        .await;
    assert_eq!(sixth["error"]["code"], "busy", "{sixth}");
    // At the cap, a call that opens no tab still works: Explain in an open tab, and a
    // view that focuses the tab it opened before.
    host.state.lock().tab_transaction_open = false;
    let explained = client
        .tool(
            50,
            "show_in_app",
            json!({"connection_id":"c1","view":"explain","tab_id":1}),
        )
        .await;
    assert_eq!(explained["tab_id"], 1, "{explained}");
    let focused = client
        .tool(
            51,
            "show_in_app",
            json!({"connection_id":"c1","view":"er_diagram","tables":["a","b"]}),
        )
        .await;
    assert_eq!(focused["tab_id"], er["tab_id"], "{focused}");
    let new_er = client
        .tool(
            52,
            "show_in_app",
            json!({"connection_id":"c1","view":"er_diagram","tables":["c"]}),
        )
        .await;
    assert_eq!(new_er["error"]["code"], "busy", "{new_er}");
    // The explorer opens no tab and stays available.
    let explorer = client
        .tool(
            42,
            "show_in_app",
            json!({"connection_id":"c1","view":"explorer"}),
        )
        .await;
    assert!(explorer.get("error").is_none(), "{explorer}");
    // Another MCP session has its own five.
    let mut other = Client::new(port, &token);
    other.initialize().await;
    let fresh = other
        .tool(
            43,
            "open_query_tab",
            json!({"connection_id":"c1","text":"select 1"}),
        )
        .await;
    assert!(fresh["tab_id"].is_u64(), "{fresh}");

    host.state.lock().config.agent_access = AgentAccess::None;
    let hidden = other
        .tool(
            44,
            "open_query_tab",
            json!({"connection_id":"c1","text":"select 1"}),
        )
        .await;
    assert_eq!(hidden["error"]["code"], "not_exposed", "{hidden}");
    let hidden = other
        .tool(
            45,
            "show_in_app",
            json!({"connection_id":"c1","view":"explorer"}),
        )
        .await;
    assert_eq!(hidden["error"]["code"], "not_exposed", "{hidden}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn describe_object_reports_a_failed_object_definition() {
    use sqlr_core::{TableInfo, TableRef};
    use sqlr_testkit::{FakeDriver, FakeScript};
    let orders = TableInfo {
        table: TableRef {
            database: None,
            schema: None,
            name: "orders".into(),
        },
        columns: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    };
    // A driver that reads object definitions but fails on this one: an error, not empty
    // lists.
    let state = driver_state(
        FakeDriver::new(FakeScript {
            tables: vec![orders.clone()],
            editing: Some(Default::default()),
            objects: Vec::new(),
            ..Default::default()
        }),
        AgentAccess::Read,
    );
    let failed = direct_tool(
        &state,
        "describe_object",
        json!({"connection_id":"c1","name":"orders"}),
    )
    .await;
    assert_eq!(failed["error"]["code"], "catalog", "{failed}");

    // A driver that reads no object definitions answers empty lists.
    let state = driver_state(
        FakeDriver::new(FakeScript {
            tables: vec![orders],
            ..Default::default()
        }),
        AgentAccess::Read,
    );
    let described = direct_tool(
        &state,
        "describe_object",
        json!({"connection_id":"c1","name":"orders"}),
    )
    .await;
    assert!(described.get("error").is_none(), "{described}");
    assert_eq!(described["indexes"], json!([]), "{described}");
    assert_eq!(described["foreign_keys"], json!([]), "{described}");
}

#[tokio::test(flavor = "multi_thread")]
async fn explain_query_checks_the_statement_before_an_open_transaction() {
    let store = Arc::new(MemoryMcpTokenStore::with_token("t"));
    let (server, host, _dir, port, token) = setup(AgentAccess::Read, store).await;
    // Production commits manually: the agent's first read opens a transaction.
    host.state.lock().profile = ConnectionProfile::builtins().remove(2);
    let mut client = Client::new(port, &token);
    client.initialize().await;
    let read = client
        .tool(
            2,
            "run_query",
            json!({"connection_id":"c1","query":"select 1"}),
        )
        .await;
    assert_eq!(read["status"], "ready", "{read}");
    let busy = client
        .tool(
            3,
            "explain_query",
            json!({"connection_id":"c1","query":"select 1"}),
        )
        .await;
    assert_eq!(busy["error"]["code"], "busy", "{busy}");
    let multi = client
        .tool(
            4,
            "explain_query",
            json!({"connection_id":"c1","query":"select 1; select 2"}),
        )
        .await;
    assert_eq!(multi["error"]["code"], "invalid", "{multi}");
    let write = client
        .tool(
            5,
            "explain_query",
            json!({"connection_id":"c1","query":"delete from t","analyze":true}),
        )
        .await;
    assert_eq!(write["error"]["code"], "read_only", "{write}");
    let analyze = client
        .tool(
            6,
            "explain_query",
            json!({"connection_id":"c1","query":"select 1","analyze":"yes"}),
        )
        .await;
    assert_eq!(analyze["error"]["code"], "invalid", "{analyze}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn document_mcp_queries_use_the_sidecar_connection() {
    let spec = sqlr_sidecar::SidecarSpec::new(
        "fakedoc",
        "1.0.0",
        sqlr_testkit::workspace_binary("examples/document_driver"),
    );
    let (driver, _) = sqlr_sidecar::SidecarDriver::probe(spec).await.unwrap();
    let state = driver_state(Arc::new(driver), AgentAccess::Read);
    let result = direct_tool(
        &state,
        "run_query",
        json!({
            "connection_id":"c1", "query":"db.c.find({})", "database":"db", "max_rows":1
        }),
    )
    .await;
    assert_eq!(result["kind"], "document", "{result}");
    assert_eq!(
        result["document"]["documents"][0]["_id"],
        json!("070707070707070707070707")
    );
    let id = result["query_id"].as_str().unwrap();
    let next = direct_tool(
        &state,
        "get_query_result",
        json!({"query_id":id,"offset":1,"limit":1}),
    )
    .await;
    assert_eq!(next["truncated"], false, "{next}");
    let denied = direct_tool(
        &state,
        "run_query",
        json!({"connection_id":"c1", "query":"db.c.insertOne({n:1})"}),
    )
    .await;
    assert_eq!(denied["error"]["code"], "read_only");
    assert_eq!(
        direct_tool(&state, "close_result", json!({"query_id":id})).await["closed"],
        true
    );
}
