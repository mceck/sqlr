//! The MCP server's application side: the `AgentHost` bridge, its lifecycle and approvals.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use sqlr_core::{ConnectionProfile, DriverManifest, Level, SavedTask, SettingValue};
use sqlr_engine::Engine;
use sqlr_mcp::error::{ToolError, ToolResult};
use sqlr_mcp::host::{
    AgentConnection, AgentHost, ApprovalRequest, ConnectionWrite, FolderOp, KeymapEntry, Risk,
    RunTarget, SettingEntry, SettingLevel, TabDetail, TabEdit, TabSummary,
};
use sqlr_mcp::server::{McpServer, StartError, StartOptions};
use sqlr_store::McpTokenStore;
use tokio::sync::oneshot;

use crate::app::SqlrApp;

pub type HostTask = Box<dyn FnOnce(&mut SqlrApp) + Send>;
pub type PendingConnectionWrite = (oneshot::Sender<ToolResult<()>>, String);

/// The `AgentHost` the MCP runtime calls. Each call is enqueued for the UI thread and
/// answered through a one-shot, so the UI never performs I/O for the agent.
#[derive(Clone)]
pub struct AppHostClient {
    tx: mpsc::Sender<HostTask>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl AppHostClient {
    pub fn new(tx: mpsc::Sender<HostTask>, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { tx, repaint }
    }

    async fn request<R, F>(&self, f: F) -> ToolResult<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut SqlrApp) -> ToolResult<R> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(Box::new(move |app| {
                let _ = tx.send(f(app));
            }))
            .is_err()
        {
            return Err(ToolError::internal("the application is shutting down"));
        }
        (self.repaint)();
        rx.await
            .map_err(|_| ToolError::internal("the application is shutting down"))?
    }
}

#[async_trait]
impl AgentHost for AppHostClient {
    async fn connection(&self, id: &str) -> ToolResult<AgentConnection> {
        let id = id.to_string();
        self.request(move |app| app.agent_connection(&id)).await
    }

    async fn connections(&self) -> ToolResult<Vec<sqlr_core::ConnectionConfig>> {
        self.request(|app| Ok(app.agent_connections())).await
    }

    async fn folders(&self) -> ToolResult<Vec<String>> {
        self.request(|app| Ok(app.folders.clone())).await
    }

    async fn profiles(&self) -> ToolResult<Vec<ConnectionProfile>> {
        self.request(|app| Ok(app.profiles.clone())).await
    }

    async fn save_profile(&self, profile: ConnectionProfile) -> ToolResult<()> {
        self.request(move |app| {
            app.engine.save_profile(app.config_dir.clone(), profile);
            app.reload_profiles();
            Ok(())
        })
        .await
    }

    async fn delete_profile(&self, id: &str) -> ToolResult<()> {
        let id = id.to_string();
        self.request(move |app| {
            app.engine
                .delete_profile(app.config_dir.clone(), id.clone());
            app.layered.forget(&Level::Profile(id));
            app.reload_profiles();
            Ok(())
        })
        .await
    }

    async fn drivers(&self) -> ToolResult<Vec<DriverManifest>> {
        self.request(|app| Ok(app.engine.manifests())).await
    }

    async fn secret_presence(&self, id: &str) -> ToolResult<BTreeMap<String, bool>> {
        let id = id.to_string();
        let (connection, secrets) = self
            .request(move |app| Ok((app.agent_connection(&id)?, app.secret_store.clone())))
            .await?;
        let id = connection.config.id.clone();
        let fields: Vec<String> = connection
            .manifest
            .fields
            .iter()
            .filter(|field| field.secret)
            .map(|field| field.key.clone())
            .collect();
        let save = connection.config.save_secrets;
        let entered = connection.secrets.clone();
        if !save {
            return Ok(fields
                .into_iter()
                .map(|key| {
                    let stored = entered.0.get(&key).is_some_and(|value| !value.is_empty());
                    (key, stored)
                })
                .collect());
        }
        tokio::task::spawn_blocking(move || {
            fields
                .into_iter()
                .map(|key| {
                    let stored = secrets.get(&id, &key).ok().flatten().is_some();
                    (key, stored)
                })
                .collect::<BTreeMap<_, _>>()
        })
        .await
        .map_err(|e| ToolError::internal(format!("reading secret presence failed: {e}")))
    }

    async fn settings(&self) -> ToolResult<Vec<SettingEntry>> {
        self.request(|app| Ok(app.agent_settings())).await
    }

    async fn set_setting(
        &self,
        id: &str,
        level: SettingLevel,
        level_id: Option<&str>,
        value: Option<SettingValue>,
    ) -> ToolResult<()> {
        let id = id.to_string();
        let level_id = level_id.map(str::to_string);
        self.request(move |app| {
            let level = match level {
                SettingLevel::Global => Level::Global,
                SettingLevel::Profile => Level::Profile(level_id.unwrap_or_default()),
                SettingLevel::Connection => Level::Connection(level_id.unwrap_or_default()),
            };
            app.layered
                .set(&id, level, value)
                .map_err(ToolError::invalid)?;
            app.reload_live_settings();
            app.save_settings_now();
            app.notify(format!("Agent updated setting {id}"));
            Ok(())
        })
        .await
    }

    async fn keymap(&self) -> ToolResult<Vec<KeymapEntry>> {
        self.request(|app| Ok(app.agent_keymap())).await
    }

    async fn set_keymap(&self, action: &str, chords: Option<Vec<String>>) -> ToolResult<()> {
        let action = action.to_string();
        self.request(move |app| {
            let action_id = sqlr_core::ActionId::from_id(&action)
                .ok_or_else(|| ToolError::invalid("unknown action"))?;
            match chords {
                Some(chords) => {
                    let parsed: Vec<sqlr_core::Chord> = chords
                        .iter()
                        .filter_map(|chord| sqlr_core::Chord::parse(chord))
                        .collect();
                    app.keymap.keymap.set_chords(action_id, parsed);
                }
                None => app.keymap.keymap.unbind(action_id),
            }
            app.keymap
                .save(&app.config_dir)
                .map_err(|e| ToolError::internal(e.to_string()))?;
            app.notify(format!("Agent updated shortcut {action}"));
            Ok(())
        })
        .await
    }

    async fn tasks(&self) -> ToolResult<Vec<SavedTask>> {
        self.request(|app| Ok(app.engine.tasks().as_ref().clone()))
            .await
    }

    async fn save_task(&self, task: SavedTask) -> ToolResult<()> {
        self.request(move |app| {
            app.engine.save_task(task);
            Ok(())
        })
        .await
    }

    async fn write_connection(&self, write: ConnectionWrite) -> ToolResult<()> {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(Box::new(move |app| {
                if let Err(error) = app.agent_write_connection(write, tx) {
                    // The ack sender moved into the app; report a failure that never reached it.
                    tracing::warn!(%error.message, "agent connection write was refused");
                }
            }))
            .is_err()
        {
            return Err(ToolError::internal("the application is shutting down"));
        }
        (self.repaint)();
        rx.await
            .map_err(|_| ToolError::internal("the application is shutting down"))?
    }

    async fn folder(&self, op: FolderOp) -> ToolResult<()> {
        self.request(move |app| app.agent_folder(op)).await
    }

    async fn delete(&self, target: &str, id: &str) -> ToolResult<()> {
        let target = target.to_string();
        let id = id.to_string();
        self.request(move |app| app.agent_delete(&target, &id))
            .await
    }

    async fn approve(&self, request: ApprovalRequest) -> ToolResult<()> {
        let (tx, rx) = oneshot::channel();
        self.request(move |app| {
            app.pending_approvals.push_back(PendingApproval {
                request,
                reply: Some(tx),
                deadline: Instant::now() + Duration::from_secs(60),
            });
            app.approval_focus_pending = true;
            Ok(())
        })
        .await?;
        rx.await.unwrap_or_else(|_| Err(ToolError::denied()))
    }

    async fn tabs(&self) -> ToolResult<Vec<TabSummary>> {
        self.request(|app| Ok(app.agent_tabs())).await
    }

    async fn tab(&self, id: u64) -> ToolResult<TabDetail> {
        self.request(move |app| app.agent_tab(id)).await
    }

    async fn edit_tab(
        &self,
        id: u64,
        expected_revision: u64,
        edit: TabEdit,
    ) -> ToolResult<TabDetail> {
        self.request(move |app| app.agent_edit_tab(id, expected_revision, edit))
            .await
    }

    async fn run_tab(&self, id: u64, target: RunTarget, max_rows: usize) -> ToolResult<Value> {
        self.request(move |app| app.agent_run_tab(id, target, max_rows))
            .await
    }

    async fn toast(&self, text: &str) {
        let text = text.to_string();
        let _ = self
            .request(move |app| {
                app.notify(text);
                Ok(())
            })
            .await;
    }
}

pub struct PendingApproval {
    pub request: ApprovalRequest,
    pub reply: Option<oneshot::Sender<ToolResult<()>>>,
    pub deadline: Instant,
}

impl PendingApproval {
    pub fn answer(&mut self, result: ToolResult<()>) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(result);
        }
    }
}

/// A running server and the runtime that owns its tasks.
pub struct RunningMcp {
    pub server: McpServer,
    pub runtime: Option<tokio::runtime::Runtime>,
}

impl Drop for RunningMcp {
    fn drop(&mut self) {
        // `McpServer::drop` cancels the listener and removes the discovery file; the runtime
        // is shut down without waiting for a stuck client.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum McpStatus {
    #[default]
    Stopped,
    Starting,
    Running,
    PortBusy,
    KeychainError,
    Error,
}

impl McpStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::PortBusy => "Port busy",
            Self::KeychainError => "Keychain error",
            Self::Error => "Error",
        }
    }

    pub fn is_running(&self) -> bool {
        *self == Self::Running
    }
}

/// What Settings → Agents shows of the running server.
#[derive(Clone, Debug, Default)]
pub struct McpPrefs {
    pub status: McpStatus,
    pub url: Option<String>,
    pub sessions: usize,
    pub error: Option<String>,
    pub token_present: bool,
}

type StartResult = Result<RunningMcp, (McpStatus, String)>;

pub struct AgentsUi {
    pub status: McpStatus,
    pub running: Option<RunningMcp>,
    pub last_error: Option<String>,
    start: Option<mpsc::Receiver<StartResult>>,
    seen_enabled: bool,
    seen_port: u16,
    failed_port: Option<u16>,
}

impl Default for AgentsUi {
    fn default() -> Self {
        Self {
            status: McpStatus::Stopped,
            running: None,
            last_error: None,
            start: None,
            seen_enabled: false,
            seen_port: 0,
            failed_port: None,
        }
    }
}

impl AgentsUi {
    pub fn url(&self) -> Option<String> {
        self.running.as_ref().map(|r| r.server.url().to_string())
    }

    pub fn sessions(&self) -> usize {
        self.running
            .as_ref()
            .map_or(0, |r| r.server.session_count())
    }

    pub fn token(&self) -> Option<String> {
        self.running.as_ref().map(|r| r.server.token().to_string())
    }

    pub fn client_config(&self) -> Option<String> {
        self.running.as_ref().map(|r| r.server.client_config())
    }

    /// Starts or stops the server to follow the settings. Called after every frame that
    /// changes them.
    #[allow(clippy::too_many_arguments)]
    pub fn sync(
        &mut self,
        enabled: bool,
        port: u16,
        engine: Arc<Engine>,
        host: Arc<dyn AgentHost>,
        data_dir: std::path::PathBuf,
        token_store: Arc<dyn McpTokenStore>,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) {
        if let Some(receiver) = &self.start
            && let Ok(result) = receiver.try_recv()
        {
            self.start = None;
            match result {
                Ok(running) => {
                    self.status = McpStatus::Running;
                    self.last_error = None;
                    self.running = Some(running);
                }
                Err((status, error)) => {
                    tracing::warn!(
                        target: "sqlr_mcp",
                        ?status,
                        error = %error,
                        "MCP server did not start"
                    );
                    self.status = status;
                    self.last_error = Some(error);
                    self.failed_port = Some(self.seen_port);
                }
            }
        }
        if !enabled {
            self.running = None;
            self.start = None;
            self.status = McpStatus::Stopped;
            self.seen_enabled = false;
            self.seen_port = port;
            self.failed_port = None;
            return;
        }
        let port_changed = self.seen_port != 0 && self.seen_port != port;
        self.seen_enabled = true;
        self.seen_port = port;
        if self.start.is_some() {
            return;
        }
        if port_changed {
            self.running = None;
            self.status = McpStatus::Stopped;
            self.failed_port = None;
        }
        if self.failed_port == Some(port) {
            return;
        }
        if self.running.is_some() {
            self.status = McpStatus::Running;
            return;
        }
        self.status = McpStatus::Starting;
        let (tx, rx) = mpsc::channel();
        self.start = Some(rx);
        std::thread::Builder::new()
            .name("sqlr-mcp-start".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("sqlr-mcp")
                    .enable_all()
                    .build();
                let result = match runtime {
                    Ok(runtime) => {
                        let options = StartOptions {
                            port,
                            data_dir,
                            engine,
                            host,
                            token_store,
                            app_version: env!("CARGO_PKG_VERSION").to_string(),
                        };
                        match runtime.block_on(McpServer::start(options)) {
                            Ok(server) => Ok(RunningMcp {
                                server,
                                runtime: Some(runtime),
                            }),
                            Err(error) => Err(start_error(error)),
                        }
                    }
                    Err(error) => Err((McpStatus::Error, error.to_string())),
                };
                let _ = tx.send(result);
                repaint();
            })
            .ok();
    }

    /// Stops the server; pending approvals are denied by the app when it is dropped.
    pub fn stop(&mut self) {
        self.running = None;
        self.start = None;
        self.status = McpStatus::Stopped;
    }

    pub fn rotate(&mut self) -> Result<(), String> {
        let Some(mut running) = self.running.take() else {
            return Err("The MCP server is not running".into());
        };
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("sqlr-mcp-rotate".into())
            .spawn(move || {
                let result = running
                    .runtime
                    .as_ref()
                    .expect("a running runtime")
                    .block_on(running.server.rotate_token());
                let _ = tx.send((running, result));
            })
            .map_err(|e| e.to_string())?;
        let (running, result) = rx.recv().map_err(|e| e.to_string())?;
        self.running = Some(running);
        result.map_err(|e| e.to_string())
    }
}

fn start_error(error: StartError) -> (McpStatus, String) {
    match error {
        StartError::PortBusy { port } => (McpStatus::PortBusy, format!("Port {port} is in use")),
        StartError::Keychain(error) => (McpStatus::KeychainError, error),
        StartError::DataDir(error) => (McpStatus::Error, error),
        StartError::Other(error) => (McpStatus::Error, error),
    }
}

impl SqlrApp {
    fn reload_live_settings(&mut self) {
        self.settings = sqlr_store::Settings::resolve(&self.layered, None);
        self.apply_engine_settings();
    }

    fn save_settings_now(&mut self) {
        if let Some(ctx) = self.ui_ctx.clone() {
            self.save_settings(&ctx);
        }
    }

    pub(crate) fn agent_connection(&self, id: &str) -> ToolResult<AgentConnection> {
        let config = self
            .connections
            .iter()
            .find(|config| config.id == id)
            .cloned()
            .ok_or_else(|| ToolError::not_found("Unknown connection"))?;
        let manifest = self
            .engine
            .manifest(&config.driver)
            .ok_or_else(|| ToolError::invalid("The driver is not installed"))?;
        let profile = self.profile_for(&config);
        let secrets = self.entered_secrets.get(id).cloned().unwrap_or_default();
        Ok(AgentConnection {
            config,
            secrets,
            profile,
            manifest,
        })
    }

    fn agent_connections(&self) -> Vec<sqlr_core::ConnectionConfig> {
        self.connections.clone()
    }

    fn agent_settings(&self) -> Vec<SettingEntry> {
        let stored = self.layered.stored_values();
        let mut values_by_id: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
        for (level, id, value) in stored {
            values_by_id
                .entry(id)
                .or_default()
                .insert(level, value.to_json());
        }
        sqlr_core::setting_defs()
            .iter()
            .filter(|def| def.agent_readable)
            .map(|def| {
                let mut values = values_by_id.remove(def.id).unwrap_or_default();
                if !values.contains_key("global") {
                    values.insert("global".into(), def.default.to_json());
                }
                SettingEntry {
                    id: def.id.to_string(),
                    section: def.section.to_string(),
                    label: def.label.to_string(),
                    description: def.description.to_string(),
                    kind: sqlr_mcp::policy::setting_kind_json(def.kind),
                    default: def.default.to_json(),
                    allowed_levels: match def.levels {
                        sqlr_core::Levels::GlobalOnly => vec!["global".into()],
                        sqlr_core::Levels::All => {
                            vec!["global".into(), "profile".into(), "connection".into()]
                        }
                    },
                    safety: match def.safety {
                        sqlr_core::Safety::None => "none",
                        sqlr_core::Safety::TightenOnly => "tighten_only",
                    }
                    .into(),
                    agent_readable: def.agent_readable,
                    agent_writable: def.agent_writable,
                    values,
                }
            })
            .collect()
    }

    fn agent_keymap(&self) -> Vec<KeymapEntry> {
        let parts = self.keymap.keymap.to_parts(&self.keymap.preset);
        let differs: HashMap<&str, Vec<String>> = parts
            .iter()
            .map(|(id, chords)| (*id, chords.clone()))
            .collect();
        sqlr_core::ActionId::ALL
            .iter()
            .map(|action| KeymapEntry {
                action: action.id().to_string(),
                label: action.label().to_string(),
                group: action.group().to_string(),
                chords: self
                    .keymap
                    .keymap
                    .chords_for(*action)
                    .iter()
                    .map(|chord| chord.to_text())
                    .collect(),
                differs_from_preset: differs.contains_key(action.id()),
            })
            .collect()
    }

    fn agent_write_connection(
        &mut self,
        write: ConnectionWrite,
        ack: oneshot::Sender<ToolResult<()>>,
    ) -> ToolResult<()> {
        let id = write.config.id.clone();
        // Defence in depth: exposure is changed only from the dialog or the explorer.
        match self.connection(&id) {
            Some(existing) if write.config.agent_access != existing.agent_access => {
                return Err(ToolError::invalid(
                    "agent_access is human-only and cannot be written by an agent",
                ));
            }
            None if write.config.agent_access != sqlr_core::AgentAccess::None => {
                return Err(ToolError::invalid(
                    "an agent-created connection is never exposed",
                ));
            }
            _ => {}
        }
        tracing::info!(
            target: "sqlr_mcp",
            connection = %id,
            fields = ?write.secrets.keys().collect::<Vec<_>>(),
            removed_fields = ?write.remove_secrets,
            "agent connection write"
        );
        let mut secrets = sqlr_core::Secrets::default();
        for (key, value) in write.secrets {
            secrets.0.insert(key, value);
        }
        for key in write.remove_secrets {
            secrets.0.insert(key, String::new());
        }
        if !write.config.save_secrets {
            let entered = self.entered_secrets.entry(id.clone()).or_default();
            for (key, value) in &secrets.0 {
                if value.is_empty() {
                    entered.0.remove(key);
                } else {
                    entered.0.insert(key.clone(), value.clone());
                }
            }
        }
        let name = write.config.name.clone();
        self.store.send(crate::store_worker::StoreOp::Save {
            cfg: write.config,
            secrets,
            editing: true,
        });
        self.pending_connection_writes
            .entry(id)
            .or_default()
            .push((ack, name));
        Ok(())
    }

    fn agent_folder(&mut self, op: FolderOp) -> ToolResult<()> {
        use crate::store_worker::StoreOp;
        let target = match op {
            FolderOp::Create { name } => StoreOp::CreateFolder(name),
            FolderOp::Rename { from, to } => StoreOp::RenameFolder { from, to },
            FolderOp::Move { name, target } => {
                let target = match target {
                    sqlr_mcp::host::MoveTarget::End => sqlr_store::MoveTarget::End,
                    sqlr_mcp::host::MoveTarget::Before { id } => sqlr_store::MoveTarget::Before(id),
                    sqlr_mcp::host::MoveTarget::After { id } => sqlr_store::MoveTarget::After(id),
                    sqlr_mcp::host::MoveTarget::Into { id } => sqlr_store::MoveTarget::Into(id),
                };
                StoreOp::MoveFolder {
                    folder: name,
                    target,
                }
            }
        };
        self.store.send(target);
        self.notify("Agent updated a folder".to_string());
        Ok(())
    }

    fn agent_delete(&mut self, target: &str, id: &str) -> ToolResult<()> {
        use crate::store_worker::StoreOp;
        match target {
            "connection" => {
                let Some(cfg) = self.connection(id).cloned() else {
                    return Err(ToolError::not_found("Unknown connection"));
                };
                let Some(manifest) = self.engine.manifest(&cfg.driver) else {
                    return Err(ToolError::invalid("The driver is not installed"));
                };
                self.store.send(StoreOp::Delete { cfg, manifest });
                self.connections.retain(|config| config.id != id);
                self.engine.disconnect(id);
                self.engine.invalidate_catalog(id);
                self.notify(format!("Agent deleted connection {id}"));
                Ok(())
            }
            "folder" => {
                self.store.send(StoreOp::DeleteFolder(id.to_string()));
                self.notify(format!("Agent deleted folder {id}"));
                Ok(())
            }
            "profile" => {
                self.engine
                    .delete_profile(self.config_dir.clone(), id.to_string());
                self.layered.forget(&Level::Profile(id.to_string()));
                self.reload_profiles();
                self.notify(format!("Agent deleted profile {id}"));
                Ok(())
            }
            "task" => {
                self.engine.delete_task(id);
                self.notify(format!("Agent deleted task {id}"));
                Ok(())
            }
            _ => Err(ToolError::invalid("Unknown delete target")),
        }
    }

    fn agent_tabs(&self) -> Vec<TabSummary> {
        self.dock
            .iter_all_tabs()
            .filter(|(_, tab)| {
                tab.redis.is_none()
                    && tab.object.is_none()
                    && tab.doc.is_none()
                    && tab.admin.is_none()
                    && tab.ddl.is_none()
            })
            .map(|(_, tab)| TabSummary {
                id: tab.key,
                title: tab.title.clone(),
                connection_id: tab.conn.clone(),
                database: tab.database.clone(),
                dirty: tab.dirty,
                revision: tab.editor.revision(),
                busy: tab.is_running(&self.engine),
            })
            .collect()
    }

    fn agent_tab(&self, id: u64) -> ToolResult<TabDetail> {
        let tab = self
            .dock
            .iter_all_tabs()
            .find(|(_, tab)| tab.key == id)
            .map(|(_, tab)| tab)
            .ok_or_else(|| ToolError::not_found("Unknown tab"))?;
        if tab.redis.is_some()
            || tab.object.is_some()
            || tab.doc.is_some()
            || tab.admin.is_some()
            || tab.ddl.is_some()
        {
            return Err(ToolError::invalid("Only SQL query tabs are available"));
        }
        Ok(TabDetail {
            id: tab.key,
            title: tab.title.clone(),
            connection_id: tab.conn.clone(),
            database: tab.database.clone(),
            dirty: tab.dirty,
            revision: tab.editor.revision(),
            busy: tab.is_running(&self.engine),
            text: tab.editor.text(),
            caret: tab.editor.caret_char(),
            selection: tab.editor.selection_chars(),
        })
    }

    fn agent_edit_tab(
        &mut self,
        id: u64,
        expected_revision: u64,
        edit: TabEdit,
    ) -> ToolResult<TabDetail> {
        let engine = &self.engine;
        let Some(tab) = crate::app::tab_mut(&mut self.dock, id) else {
            return Err(ToolError::not_found("Unknown tab"));
        };
        let current = tab.editor.revision();
        if current != expected_revision {
            return Err(ToolError::conflict(current));
        }
        tab.dirty = true;
        let changes = match edit {
            TabEdit::Replace(text) => tab.editor.replace_all(&text),
            TabEdit::Edits(edits) => tab.editor.replace_ranges(
                edits
                    .into_iter()
                    .map(|edit| (edit.start..edit.end, edit.text))
                    .collect(),
            ),
        };
        if let Some(first) = changes.first() {
            tab.editor.highlight_agent_edit(first.start_char);
        }
        Ok(TabDetail {
            id: tab.key,
            title: tab.title.clone(),
            connection_id: tab.conn.clone(),
            database: tab.database.clone(),
            dirty: tab.dirty,
            revision: tab.editor.revision(),
            busy: tab.is_running(engine),
            text: tab.editor.text(),
            caret: tab.editor.caret_char(),
            selection: tab.editor.selection_chars(),
        })
    }

    fn agent_run_tab(&mut self, id: u64, target: RunTarget, max_rows: usize) -> ToolResult<Value> {
        self.run_tab_for_agent(id, target, max_rows)
    }
}

impl SqlrApp {
    /// Handles the agent requests queued for the UI thread, then syncs the server lifecycle.
    #[doc(hidden)]
    pub fn mcp_stopped_for_tests(&self) -> bool {
        self.mcp.status == McpStatus::Stopped
    }

    #[doc(hidden)]
    pub fn mcp_running_for_tests(&self) -> bool {
        self.mcp.status.is_running()
    }

    #[doc(hidden)]
    pub fn agents_enabled_for_tests(&self) -> bool {
        self.settings.agents_enabled
    }

    /// Turns the MCP server on or off from the toolbar toggle and saves the setting in the
    /// background (the server follows it on the next tick).
    pub(crate) fn set_agents_enabled(&mut self, enabled: bool, ctx: &egui::Context) {
        if self.settings.agents_enabled == enabled {
            return;
        }
        self.settings.agents_enabled = enabled;
        if let Err(error) = self.layered.set(
            "agents.enabled",
            Level::Global,
            Some(SettingValue::Bool(enabled)),
        ) {
            self.notify(format!("Could not change agent access: {error}"));
            return;
        }
        self.save_settings(ctx);
    }

    #[doc(hidden)]
    pub fn set_mcp_enabled_for_tests(&mut self, enabled: bool) {
        self.settings.agents_enabled = enabled;
    }

    #[doc(hidden)]
    pub fn queue_approval_for_tests(&mut self) -> tokio::sync::oneshot::Receiver<ToolResult<()>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending_approvals.push_back(PendingApproval {
            request: ApprovalRequest {
                session_id: "test-session".into(),
                client_name: "test-agent".into(),
                client_version: "1.0".into(),
                risk: Risk::Destructive,
                connection: Some("Production".into()),
                profile: Some("Production".into()),
                database: Some("app".into()),
                exposure: Some("write".into()),
                statement: Some("drop table users".into()),
                target: None,
                consequences: None,
            },
            reply: Some(tx),
            deadline: Instant::now() + Duration::from_secs(60),
        });
        self.approval_focus_pending = true;
        rx
    }

    #[doc(hidden)]
    pub fn open_agents_settings_for_tests(&mut self) {
        self.open_preferences_section("Agents");
    }

    #[doc(hidden)]
    pub fn terminal_shell_for_tests(&self) -> String {
        self.terminal.config().shell.clone()
    }

    #[doc(hidden)]
    pub fn terminal_visible_for_tests(&self) -> bool {
        self.terminal.is_open()
    }

    #[doc(hidden)]
    pub fn terminal_focused_for_tests(&self) -> bool {
        self.terminal.focused()
    }

    #[doc(hidden)]
    pub fn terminal_rect_for_tests(&self) -> egui::Rect {
        self.terminal.canvas_rect()
    }

    #[doc(hidden)]
    pub fn terminal_text_for_tests(&self) -> String {
        let Some(session) = self.terminal.session() else {
            return String::new();
        };
        let snapshot = session.snapshot(0);
        snapshot.text_between((0, 0), (snapshot.rows - 1, snapshot.cols - 1))
    }

    pub(crate) fn agents_tick(&mut self, ctx: &egui::Context) {
        self.ui_ctx = Some(ctx.clone());
        while let Ok(task) = self.host_jobs.try_recv() {
            task(self);
        }
        let now = Instant::now();
        self.pending_approvals.retain_mut(|pending| {
            if pending
                .reply
                .as_ref()
                .is_some_and(oneshot::Sender::is_closed)
            {
                return false;
            }
            if now >= pending.deadline {
                tracing::info!(
                    target: "sqlr_mcp",
                    client = %pending.request.client_name,
                    session = %pending.request.session_id,
                    risk = pending.request.risk.label(),
                    allowed = false,
                    reason = "timeout",
                    "agent approval decided"
                );
                pending.answer(Err(ToolError::timeout()));
                return false;
            }
            true
        });
        if !self.settings.agents_enabled && self.mcp.status != McpStatus::Stopped {
            while let Some(mut pending) = self.pending_approvals.pop_front() {
                pending.answer(Err(ToolError::denied()));
            }
        }
        let repaint: Arc<dyn Fn() + Send + Sync> = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let host = Arc::new(AppHostClient::new(self.host_tx.clone(), repaint.clone()));
        self.mcp.sync(
            self.settings.agents_enabled,
            self.settings.agents_port,
            self.engine.clone(),
            host,
            self.data_dir.clone(),
            self.mcp_token_store.clone(),
            repaint,
        );
        self.terminal_workspace.sync(self.mcp.client_config());
        if self.settings.agents_enabled || self.terminal.is_open() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    pub(crate) fn approval_ui(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_approvals.front() else {
            return;
        };
        let expired = Instant::now() >= pending.deadline;
        let request = pending.request.clone();
        let remaining = pending
            .deadline
            .saturating_duration_since(Instant::now())
            .as_secs();
        let mut decision: Option<ToolResult<()>> = if expired {
            Some(Err(ToolError::timeout()))
        } else {
            None
        };
        let p = sqlr_ui::theme::palette(ctx);
        let modal = egui::Modal::new(egui::Id::new("agent_approval")).show(ctx, |ui| {
            ui.set_width(560.0_f32.min(ctx.content_rect().width() - 64.0));
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Agent request").strong().size(18.0));
                ui.add_space(8.0);
                let risk = egui::RichText::new(request.risk.label())
                    .small()
                    .color(p.accent_text);
                ui.label(risk);
            });
            ui.add_space(6.0);
            ui.label(format!(
                "{} (MCP session {})",
                request.client_name, request.session_id
            ));
            let mut target = String::new();
            if let Some(connection) = &request.connection {
                target.push_str(connection);
            }
            if let Some(profile) = &request.profile {
                target.push_str(&format!(" · profile {profile}"));
            }
            if let Some(database) = &request.database {
                target.push_str(&format!(" · database {database}"));
            }
            if let Some(exposure) = &request.exposure {
                target.push_str(&format!(" · agents: {exposure}"));
            }
            if !target.is_empty() {
                ui.label(egui::RichText::new(target).small().weak());
            }
            ui.add_space(8.0);
            if let Some(statement) = &request.statement {
                egui::ScrollArea::vertical()
                    .id_salt("agent_statement")
                    .max_height(260.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(statement).monospace().size(13.0))
                                .wrap(),
                        );
                    });
            } else {
                if let Some(target) = &request.target {
                    ui.label(egui::RichText::new(target).monospace());
                }
                if let Some(consequences) = &request.consequences {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(consequences).small().weak());
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "Denied automatically in {remaining}s if you do not answer."
                    ))
                    .small()
                    .weak(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let allow =
                        egui::Button::new(egui::RichText::new("Allow").color(p.accent_text))
                            .fill(p.accent);
                    if ui.add(allow).clicked() {
                        decision = Some(Ok(()));
                    }
                    let deny = ui.add(egui::Button::new("Deny"));
                    if deny.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        decision = Some(Err(ToolError::denied()));
                    }
                    if deny.clicked() {
                        decision = Some(Err(ToolError::denied()));
                    }
                    // The safe action is focused when the dialog opens; Escape denies.
                    if self.approval_focus_pending {
                        self.approval_focus_pending = false;
                        deny.request_focus();
                    }
                });
            });
        });
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if escape && decision.is_none() {
            decision = Some(Err(ToolError::denied()));
        }
        if modal.should_close() && decision.is_none() {
            decision = Some(Err(ToolError::denied()));
        }
        if let Some(result) = decision
            && let Some(mut pending) = self.pending_approvals.pop_front()
        {
            tracing::info!(
                target: "sqlr_mcp",
                client = %pending.request.client_name,
                session = %pending.request.session_id,
                risk = pending.request.risk.label(),
                allowed = result.is_ok(),
                "agent approval decided"
            );
            pending.answer(result);
        }
    }

    pub(crate) fn terminal_ui(&mut self, ui: &mut egui::Ui) {
        if self.settings.terminal_cwd.trim().is_empty()
            && self.mcp.status == McpStatus::Starting
            && !self.terminal.started()
        {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Starting MCP server…");
            });
            return;
        }
        let cwd = if self.settings.terminal_cwd.trim().is_empty() {
            let config = self.mcp.client_config();
            self.terminal_workspace.start(
                &self.config_dir,
                &self.data_dir,
                config.clone(),
                ui.ctx(),
            );
            self.terminal_workspace.sync(config);
            if let Some(error) = self.terminal_workspace.error() {
                ui.colored_label(sqlr_ui::theme::palette(ui.ctx()).error, error);
                if !self.terminal.started() && ui.button("Retry").clicked() {
                    self.terminal_workspace = Default::default();
                    ui.ctx().request_repaint();
                }
                if !self.terminal.started() {
                    return;
                }
            }
            if !self.terminal_workspace.ready() && !self.terminal.started() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Preparing terminal workspace…");
                });
                return;
            }
            let Some(path) = self.terminal_workspace.path() else {
                return;
            };
            path.to_string_lossy().into_owned()
        } else {
            self.settings.terminal_cwd.clone()
        };
        let palette = sqlr_ui::theme::palette(ui.ctx());
        let ansi = [
            palette.bg_editor,
            palette.error,
            palette.success,
            palette.warning,
            palette.accent,
            palette.text_muted,
            palette.text_muted,
            palette.text,
            palette.text_disabled,
            palette.error,
            palette.success,
            palette.warning,
            palette.accent,
            palette.text_muted,
            palette.text_muted,
            palette.text,
        ];
        let theme = sqlr_terminal::TerminalTheme {
            background: palette.bg_editor,
            foreground: palette.text,
            cursor: palette.accent,
            selection: palette.selection,
            ansi,
            font_size: self.settings.terminal_font_size,
        };
        self.terminal.set_config(sqlr_terminal::SessionConfig {
            shell: self.settings.terminal_shell.clone(),
            cwd,
            scrollback: self.settings.terminal_scrollback,
            ..Default::default()
        });
        let repaint: Arc<dyn Fn() + Send + Sync> = {
            let ctx = ui.ctx().clone();
            Arc::new(move || ctx.request_repaint())
        };
        let actions = self
            .terminal
            .show(ui, &theme, self.mcp.status.is_running(), repaint);
        for action in actions {
            match action {
                sqlr_terminal::TerminalAction::Restart => {
                    let repaint: Arc<dyn Fn() + Send + Sync> = {
                        let ctx = ui.ctx().clone();
                        Arc::new(move || ctx.request_repaint())
                    };
                    self.terminal.restart(repaint);
                }
                sqlr_terminal::TerminalAction::Clear => self.terminal.clear(),
                sqlr_terminal::TerminalAction::Kill => self.terminal.kill(),
                sqlr_terminal::TerminalAction::CopyMcpConfig => {
                    if let Some(config) = self.mcp.client_config() {
                        ui.ctx().copy_text(config);
                        self.notify("MCP client configuration copied (it contains the token)");
                    }
                }
                sqlr_terminal::TerminalAction::OpenSettings => {
                    self.prefs = Some(crate::prefs::PrefsWindow::new_for_section(
                        &self.layered,
                        sqlr_core::Level::Global,
                        "Terminal",
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppDeps;
    use sqlr_engine::DriverRegistry;
    use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};

    fn app_for_agent_tests(dir: &std::path::Path) -> SqlrApp {
        let mut drivers = DriverRegistry::default();
        drivers.register(sqlr_testkit::FakeDriver::new(Default::default()));
        let repo = ConnectionRepo::open(dir, Box::new(MemorySecretStore::default())).unwrap();
        repo.save(
            &sqlr_core::ConnectionConfig {
                id: "c1".into(),
                name: "Test".into(),
                driver: "fake".into(),
                group: None,
                color: None,
                profile: None,
                values: Default::default(),
                save_secrets: false,
                agent_access: sqlr_core::AgentAccess::Read,
            },
            &Default::default(),
        )
        .unwrap();
        SqlrApp::new(
            AppDeps {
                engine: Engine::new(drivers, Arc::new(|| {})),
                repo,
                settings: Settings::default(),
                config_dir: dir.into(),
                data_dir: None,
                startup_notice: None,
                storage_warning: None,
                mcp_token_store: None,
            },
            &egui::Context::default(),
        )
    }

    #[test]
    fn agent_edits_update_real_tabs_and_reject_stale_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_for_agent_tests(dir.path());
        app.new_tab(Some("Agent query".into()), Some("c1".into()), "SELECT 1");
        let id = app.active_key().unwrap();
        let tab = app.agent_tab(id).unwrap();
        assert_eq!(tab.text, "SELECT 1");
        assert!(
            app.agent_tabs()
                .iter()
                .any(|tab| tab.id == id && tab.connection_id.as_deref() == Some("c1"))
        );
        let edited = app
            .agent_edit_tab(id, tab.revision, TabEdit::Replace("SELECT 22".into()))
            .unwrap();
        assert_eq!(edited.text, "SELECT 22");
        assert!(edited.dirty);
        assert!(edited.revision > tab.revision);
        assert_eq!(
            app.agent_edit_tab(id, tab.revision, TabEdit::Replace("lost update".into()))
                .unwrap_err()
                .code,
            "conflict"
        );
        let edited = app
            .agent_edit_tab(
                id,
                edited.revision,
                TabEdit::Edits(vec![sqlr_mcp::host::EditRange {
                    start: 7,
                    end: 9,
                    text: "3".into(),
                }]),
            )
            .unwrap();
        assert_eq!(edited.text, "SELECT 3");
        assert_eq!(app.agent_tab(id).unwrap().text, "SELECT 3");
        assert_eq!(app.agent_tab(u64::MAX).unwrap_err().code, "not_found");
        assert_eq!(
            app.agent_edit_tab(u64::MAX, 0, TabEdit::Replace(String::new()))
                .unwrap_err()
                .code,
            "not_found"
        );
    }

    #[test]
    fn agent_metadata_and_folder_changes_match_the_live_application() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_for_agent_tests(dir.path());
        let settings = app.agent_settings();
        let wrap = settings
            .iter()
            .find(|entry| entry.id == "editor.soft_wrap")
            .unwrap();
        assert_eq!(wrap.kind, serde_json::json!({"type":"bool"}));
        assert_eq!(
            wrap.values.get("global"),
            Some(&serde_json::json!(app.settings.soft_wrap))
        );
        let keymap = app.agent_keymap();
        let new_tab = keymap
            .iter()
            .find(|entry| entry.action == sqlr_core::ActionId::NewTab.id())
            .unwrap();
        assert!(!new_tab.chords.is_empty());
        assert!(!new_tab.differs_from_preset);
        assert_eq!(app.agent_connections()[0].id, "c1");
        assert_eq!(
            app.agent_connection("missing").err().unwrap().code,
            "not_found"
        );
        app.agent_folder(FolderOp::Create {
            name: "work".into(),
        })
        .unwrap();
        app.agent_folder(FolderOp::Rename {
            from: "work".into(),
            to: "renamed".into(),
        })
        .unwrap();
        app.agent_folder(FolderOp::Create {
            name: "parent".into(),
        })
        .unwrap();
        for target in [
            sqlr_mcp::host::MoveTarget::End,
            sqlr_mcp::host::MoveTarget::Before {
                id: "parent".into(),
            },
            sqlr_mcp::host::MoveTarget::After {
                id: "parent".into(),
            },
            sqlr_mcp::host::MoveTarget::Into {
                id: "parent".into(),
            },
        ] {
            app.agent_folder(FolderOp::Move {
                name: "renamed".into(),
                target,
            })
            .unwrap();
        }
        app.agent_delete("folder", "parent/renamed").unwrap();
        app.agent_delete("task", "missing").unwrap();
        assert_eq!(
            app.agent_delete("connection", "missing").unwrap_err().code,
            "not_found"
        );
        assert_eq!(
            app.agent_delete("unsupported", "c1").unwrap_err().code,
            "invalid"
        );
        app.agent_delete("connection", "c1").unwrap();
        assert!(app.agent_connections().is_empty());
        // The same worker persists the folder operations in order.
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.logic(&ctx);
            let repo =
                ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
            if repo.list().unwrap().is_empty() {
                assert_eq!(repo.folders().unwrap(), vec!["parent".to_string()]);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn secret_presence_reads_the_applications_injected_store() {
        use sqlr_core::{ConnectionConfig, Secrets};
        use sqlr_store::SecretStore as _;

        let dir = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut drivers = DriverRegistry::default();
        drivers.register(sqlr_testkit::FakeDriver::new(Default::default()));
        let secrets = Arc::new(MemorySecretStore::default());
        secrets.set("c1", "password", "saved-password").unwrap();
        let repo = ConnectionRepo::open(dir.path(), Box::new(secrets)).unwrap();
        repo.save(
            &ConnectionConfig {
                id: "c1".into(),
                name: "fake".into(),
                driver: "fake".into(),
                group: None,
                color: None,
                values: Default::default(),
                profile: None,
                save_secrets: true,
                agent_access: Default::default(),
            },
            &Secrets::default(),
        )
        .unwrap();
        let mut app = SqlrApp::new(
            AppDeps {
                engine: Engine::new(drivers, Arc::new(|| {})),
                repo,
                settings: Settings::default(),
                config_dir: dir.path().into(),
                data_dir: None,
                startup_notice: None,
                storage_warning: None,
                mcp_token_store: None,
            },
            &ctx,
        );
        let host = AppHostClient::new(app.host_tx.clone(), Arc::new(|| {}));
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            tx.send(runtime.block_on(host.secret_presence("c1")))
                .unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let presence = loop {
            app.agents_tick(&ctx);
            match rx.try_recv() {
                Ok(result) => break result.unwrap(),
                Err(mpsc::TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "secret presence timed out");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("presence worker failed: {error}"),
            }
        };
        worker.join().unwrap();
        assert_eq!(presence.get("password"), Some(&true));
    }

    #[test]
    fn hidden_logic_expires_queued_approvals_and_discards_disconnected_requests() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut app = SqlrApp::new(
            AppDeps {
                engine: Engine::new(DriverRegistry::default(), Arc::new(|| {})),
                repo: ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default()))
                    .unwrap(),
                settings: Settings::default(),
                config_dir: dir.path().into(),
                data_dir: None,
                startup_notice: None,
                storage_warning: None,
                mcp_token_store: None,
            },
            &ctx,
        );
        let mut waiting = app.queue_approval_for_tests();
        let mut expired = app.queue_approval_for_tests();
        app.pending_approvals.back_mut().unwrap().deadline =
            Instant::now() - Duration::from_secs(1);
        drop(app.queue_approval_for_tests());

        let _ = ctx.run_logic(&egui::RawInput::default(), |ctx| app.logic(ctx));
        assert_eq!(
            expired
                .try_recv()
                .expect("an expired approval must be answered while hidden")
                .unwrap_err()
                .code,
            "timeout"
        );
        assert!(matches!(
            waiting.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(app.pending_approvals.len(), 1);
    }
}
