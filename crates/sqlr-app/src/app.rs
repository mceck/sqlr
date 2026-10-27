//! Layout A: menu bar, toolbar, explorer and snippets, query tabs, value panel, history panel
//! and status bar.

use sqlr_ui::ToastKind;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use egui::Margin;
use egui_dock::{DockArea, DockState};
use sqlr_core::{
    ConnectionConfig, ConnectionProfile, Dialect, DriverManifest, Node, Secrets, SqlSyntax,
};
use sqlr_driver_elastic::ElasticDriver;
use sqlr_driver_mongo::MongoDriver;
use sqlr_driver_mssql::SqlServerDriver;
use sqlr_driver_mysql::MysqlDriver;
use sqlr_driver_oracle::OracleDriver;
use sqlr_driver_postgres::PostgresDriver;
use sqlr_driver_redis::RedisDriver;
use sqlr_driver_snowflake::SnowflakeDriver;
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{DriverRegistry, Engine, Repaint, SessionStatus};
use sqlr_store::{ConnectionRepo, KeyringStore, LayeredSettings, MoveTarget, Settings};
use sqlr_ui::theme::{self, Spacing, icons};
use sqlr_ui::{TreeAction, TreeDrop, TreeState, schema_tree};

use crate::confirm::PendingRun;
use crate::dialogs::{ConnDialog, SecretPrompt};
use crate::fonts;
use crate::help::ShortcutsDialog;
use crate::library::{LibraryUi, open_db_or_memory};
#[cfg(target_os = "macos")]
use crate::native_menu::NativeMenu;
use crate::palette::AppPalette;
use crate::prefs::{Formats, PrefsWindow};
use crate::profiles_dialog::ProfilesDialog;
use crate::shell::status::{
    ConnInfo, ResultInfo, StatusAction, StatusBarState, StatusView, status_bar,
};
use crate::store_worker::{Listing, StoreOp, StoreReply};
use crate::tabs::{QueryTab, TabCommand, Viewer};
use crate::transaction::PendingEnd;
use crate::tree_snapshot::TreeSnapshot;

/// The store's sibling position for a tree drop target.
fn move_target(target: TreeDrop) -> MoveTarget {
    match target {
        TreeDrop::Before(id) => MoveTarget::Before(id),
        TreeDrop::After(id) => MoveTarget::After(id),
        TreeDrop::End => MoveTarget::End,
        TreeDrop::Into(folder) => MoveTarget::Into(folder),
    }
}

pub struct AppDeps {
    pub engine: Engine,
    pub repo: ConnectionRepo,
    pub settings: Settings,
    pub config_dir: PathBuf,
    /// Where the demo database is created; `None` uses `config_dir`.
    pub data_dir: Option<PathBuf>,
    /// Shown as a toast on start (e.g. the local database could not be opened).
    pub startup_notice: Option<String>,
    /// Why history and snippets live in memory only, when they do: a warning that stays in
    /// the status bar for the whole session.
    pub storage_warning: Option<String>,
    /// Where the MCP bearer token lives; tests pass an in-memory store.
    pub mcp_token_store: Option<Arc<dyn sqlr_store::McpTokenStore>>,
    /// The session's diagnostics, when the binary initialized them.
    pub diagnostics: Option<Arc<crate::diagnostics::Diagnostics>>,
    /// Why diagnostics are limited to stderr, when they are.
    pub diagnostics_warning: Option<String>,
    /// Whether the app may check GitHub for a newer release at startup (spec §12). Tests
    /// pass `false`, so no test ever reaches the network.
    pub check_updates: bool,
}

/// The real dependencies: keychain secrets, the user's config directory, `sqlr.db` (or an
/// in-memory stand-in) and every driver.
pub fn production_deps(repaint: Repaint) -> anyhow::Result<AppDeps> {
    production_deps_in(repaint, None, None)
}

/// [`production_deps`] with the config and data directories given on the command line in
/// place of the user's defaults.
pub fn production_deps_in(
    repaint: Repaint,
    config_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<AppDeps> {
    production_deps_with(repaint, config_dir, data_dir, None, None)
}

/// [`production_deps_in`] with the session's diagnostics and the warning about a log
/// directory that cannot be written.
pub fn production_deps_with(
    repaint: Repaint,
    config_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    diagnostics: Option<Arc<crate::diagnostics::Diagnostics>>,
    diagnostics_warning: Option<String>,
) -> anyhow::Result<AppDeps> {
    let config_dir = match config_dir {
        Some(dir) => dir,
        None => sqlr_store::default_config_dir().context("no config directory for this user")?,
    };
    let data_dir = data_dir.or_else(sqlr_store::default_data_dir);
    let secrets = Arc::new(sqlr_store::VaultSecretStore::new(
        &config_dir,
        Arc::new(KeyringStore),
    ));
    let repo = ConnectionRepo::open(&config_dir, Box::new(secrets.clone()))
        .context("open the connection store")?;
    let settings = Settings::load(&config_dir).unwrap_or_else(|e| {
        tracing::error!(incident = "startup", error = %e, "settings unreadable, using defaults");
        Settings::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(PostgresDriver::new());
    drivers.register(MysqlDriver::new());
    drivers.register(SqlServerDriver::new());
    drivers.register(OracleDriver::new());
    drivers.register(SqliteDriver::new());
    drivers.register(SnowflakeDriver::new());
    drivers.register(MongoDriver::new());
    drivers.register(ElasticDriver::new());
    drivers.register(RedisDriver::new());
    let db_path = data_dir.as_ref().map(|d| d.join("sqlr.db"));
    let (db, startup_notice) = open_db_or_memory(db_path.as_deref());
    let engine = Engine::with_db(drivers, repaint, db);
    if let Some(data) = &data_dir {
        engine.configure_drivers(data, settings.sidecar_idle_minutes);
    }
    engine.set_secret_store(secrets.clone());
    if let Some(data) = &data_dir {
        engine.set_known_hosts(data.join("known_hosts"));
    }
    Ok(AppDeps {
        engine,
        repo,
        settings,
        config_dir,
        data_dir,
        storage_warning: startup_notice.clone(),
        startup_notice,
        mcp_token_store: Some(Arc::new(sqlr_store::VaultMcpTokenStore::new(
            secrets,
            Arc::new(sqlr_store::KeyringMcpTokenStore),
        ))),
        diagnostics,
        diagnostics_warning,
        // The shipped binary checks for updates; nothing else does.
        check_updates: true,
    })
}

/// The left panel's two views.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeftPane {
    Connections,
    Snippets,
}

pub struct SqlrApp {
    pub(crate) engine: Arc<Engine>,
    /// `connections.toml` and the keychain, off the UI thread.
    pub(crate) store: crate::store_worker::StoreWorker,
    /// Shared by editor, engine and MCP; cloning it never performs I/O.
    pub(crate) secret_store: Arc<dyn sqlr_store::SecretStore>,
    /// Connections whose saved edit reconnects them once the new listing arrives.
    pub(crate) reconnect_on_listing: Vec<String>,
    pub(crate) settings: Settings,
    pub(crate) keymap: sqlr_store::StoredKeymap,
    /// Every level of the settings; `settings` is its global resolution.
    pub(crate) layered: LayeredSettings,
    /// A settings save running off the UI thread; its error is shown when it lands.
    settings_save: Option<std::sync::mpsc::Receiver<Result<(), sqlr_store::StoreError>>>,
    /// A save was asked for while another one was running.
    settings_dirty: bool,
    /// The display formats of the grids, per connection.
    pub(crate) formats: Formats,
    pub(crate) config_dir: PathBuf,
    /// See [`AppDeps::data_dir`].
    pub(crate) data_dir: PathBuf,
    pub(crate) connections: Vec<ConnectionConfig>,
    /// Folder names in display order (`connections.toml`).
    pub(crate) folders: Vec<String>,
    /// Built-in and user connection profiles (`profiles.toml`).
    pub(crate) profiles: Vec<ConnectionProfile>,
    /// The engine's profiles revision `profiles` was taken from.
    pub(crate) profiles_seen: u64,
    /// The drivers' dialects of the saved connections, by connection id.
    pub(crate) dialects: HashMap<String, Arc<dyn Dialect>>,
    pub(crate) tree: TreeState,
    pub(crate) dock: DockState<QueryTab>,
    pub(crate) next_tab: u64,
    pub(crate) show_value_panel: bool,
    pub(crate) show_history: bool,
    pub(crate) show_operations: bool,
    pub(crate) cancel_all_operations: bool,
    pub(crate) left: LeftPane,
    pub(crate) library: LibraryUi,
    pub(crate) palette: AppPalette,
    pub(crate) conn_dialog: Option<ConnDialog>,
    pub(crate) prefs: Option<PrefsWindow>,
    /// Help > Keyboard Shortcuts.
    pub(crate) shortcuts: ShortcutsDialog,
    /// The macOS menu bar; `None` on other platforms and in tests, where the in-window
    /// menus stay.
    #[cfg(target_os = "macos")]
    pub(crate) native_menu: Option<NativeMenu>,
    /// The connection id awaiting delete confirmation.
    pub(crate) delete_pending: Option<String>,
    /// A history clear awaiting confirmation: `Some(None)` clears every connection's.
    pub(crate) clear_history_pending: Option<Option<String>>,
    /// The folder name awaiting delete confirmation.
    pub(crate) folder_delete_pending: Option<String>,
    pub(crate) status_bar: StatusBarState,
    /// Notices and problems, shown as toasts at the bottom right.
    pub(crate) toasts: sqlr_ui::Toasts,
    /// The UI scale last given to egui; the setting is applied when it differs.
    pub(crate) applied_ui_scale: Option<f32>,
    /// Opens a folder with the OS file manager. Tests replace it.
    pub(crate) folder_opener: Arc<dyn Fn(&Path) + Send + Sync>,
    /// See [`AppDeps::storage_warning`].
    pub(crate) storage_warning: Option<String>,
    /// The session's diagnostics; `None` in tests that do not opt in.
    pub(crate) diagnostics: Option<Arc<crate::diagnostics::Diagnostics>>,
    /// The Diagnostics window, the startup banner and Report a Problem.
    pub(crate) diagnostics_ui: crate::diagnostics::DiagnosticsUi,
    /// The graphics backend eframe reported, once the first frame drew.
    pub(crate) graphics_backend: Option<String>,
    /// A run waiting for the profile's confirmation.
    pub(crate) confirm: Option<PendingRun>,
    /// Open transactions to commit or roll back before something else happens.
    pub(crate) ending: Option<PendingEnd>,
    /// Idle transactions the user chose to keep open, by their last activity.
    pub(crate) kept_open: HashMap<sqlr_engine::SessionKey, i64>,
    /// The user ended every transaction: the next close request quits.
    pub(crate) quit_confirmed: bool,
    /// Send the close command on the next frame.
    pub(crate) quit_now: bool,
    /// Redis connections the user just connected: their browser opens once they are up.
    pub(crate) pending_redis: std::collections::HashSet<String>,
    pub(crate) profiles_dialog: Option<ProfilesDialog>,
    pub(crate) client_migration: Option<crate::client_migration::MigrationDialog>,
    /// Asks for the secrets of a connection that does not save them.
    pub(crate) secret_prompt: Option<SecretPrompt>,
    /// Secrets entered for connections that do not save them, kept for the app's lifetime.
    pub(crate) entered_secrets: HashMap<String, Secrets>,
    /// Object editors' dialogs and explorer jobs.
    pub(crate) objects: crate::objects::ObjectsUi,
    /// Administration confirmations.
    pub(crate) admin: crate::admin::AdminUi,
    /// The Preview dialog of a result's pending changes.
    pub(crate) changes_dialog: Option<crate::results::ChangesDialog>,
    /// A large copy waiting for confirmation.
    pub(crate) copy_pending: Option<crate::results::PendingCopy>,
    pub(crate) export_dialog: Option<crate::transfer_dialogs::ExportDialog>,
    pub(crate) import_dialog: Option<crate::transfer_dialogs::ImportDialog>,
    /// The data transfer wizard (spec §8.5), when open.
    pub(crate) transfer_wizard: Option<crate::transfer_wizard::TransferWizard>,
    /// The schema compare dialog, when open.
    pub(crate) compare_dialog: Option<crate::compare_dialog::CompareDialog>,
    /// The ER diagram window, when open.
    /// The test-data generator dialog, when open.
    pub(crate) datagen_dialog: Option<crate::datagen_dialog::DatagenDialog>,
    /// Native dump and restore dialogs (spec §8.5).
    pub(crate) dump_dialog: Option<crate::database_tools::DumpDialog>,
    pub(crate) restore_dialog: Option<crate::database_tools::RestoreDialog>,
    /// Export and copy jobs the status bar follows.
    pub(crate) jobs: Vec<crate::transfer_dialogs::TrackedJob>,
    /// Index dialogs of document connections and the statements they started.
    pub(crate) index_admin: crate::nosql::index_admin::IndexAdmin,
    /// The answer of every file dialog, given by a test instead of a native dialog.
    pub(crate) file_answer: Option<PathBuf>,
    /// The known-databases catalog or matrix, when open.
    pub(crate) databases_dialog: Option<crate::databases_dialog::DatabasesDialog>,
    pub(crate) drivers_ui: crate::drivers::DriversUi,
    /// The demo database being prepared.
    pub(crate) demo: Option<crate::demo::DemoSetup>,
    /// The MCP server, its status and its start/stop bookkeeping.
    pub(crate) mcp: crate::agents::AgentsUi,
    pub(crate) mcp_token_store: Arc<dyn sqlr_store::McpTokenStore>,
    pub(crate) host_jobs: std::sync::mpsc::Receiver<crate::agents::HostTask>,
    pub(crate) host_tx: std::sync::mpsc::Sender<crate::agents::HostTask>,
    /// Agent approvals waiting for the person, oldest first.
    pub(crate) pending_approvals: std::collections::VecDeque<crate::agents::PendingApproval>,
    /// Focus the Deny button when the next approval draws.
    pub(crate) approval_focus_pending: bool,
    /// Connection writes waiting for the store's answer, by connection id.
    pub(crate) pending_connection_writes:
        HashMap<String, Vec<crate::agents::PendingConnectionWrite>>,
    /// The integrated terminal (session-only visibility and height).
    pub(crate) terminal: sqlr_terminal::TerminalPanel,
    /// Persistent terminal directory and background MCP configuration writer.
    pub(crate) terminal_workspace: crate::terminal_workspace::TerminalWorkspace,
    /// The context of the current frame, for work queued outside `ui` (settings saves).
    pub(crate) ui_ctx: Option<egui::Context>,
    /// The update prompt's state (spec §12); `Idle` when there is nothing to offer.
    pub(crate) update: crate::updates::UpdateFlow,
    /// The update workers' messages, polled each frame.
    pub(crate) update_rx: Option<std::sync::mpsc::Receiver<crate::updates::Msg>>,
    /// Set while a download runs; storing `true` stops it.
    pub(crate) update_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// What the check remembers: the skipped version and when it last ran.
    pub(crate) update_state: crate::updates::State,
    /// A running update-state save; one at a time.
    pub(crate) update_state_save:
        Option<std::sync::mpsc::Receiver<Result<(), crate::updates::UpdateError>>>,
    /// A state save asked for while one ran; saved when it ends.
    pub(crate) update_state_dirty: bool,
    /// What a running install replaces, kept for **Restart now**.
    pub(crate) update_target: Option<crate::updates::Target>,
}

impl SqlrApp {
    pub fn new(deps: AppDeps, ctx: &egui::Context) -> Self {
        fonts::install(ctx);
        // The value panel previews PNG/JPEG/GIF/WebP bytes.
        egui_extras::install_image_loaders(ctx);
        let AppDeps {
            engine,
            repo,
            settings,
            config_dir,
            data_dir,
            startup_notice,
            storage_warning,
            mcp_token_store,
            diagnostics,
            diagnostics_warning,
            check_updates,
        } = deps;
        let engine = Arc::new(engine);
        // The only UI-thread I/O outside the store thread: `connections.toml`, the layered
        // settings, the keymap and the small update state, once, before the first frame.
        // Profiles are read by the engine.
        let listing = crate::store_worker::Listing::read(&repo);
        // The overrides come from the files; the global level is what `deps` carries.
        let (mut layered, layered_notice) = LayeredSettings::load(&config_dir);
        settings.apply_to(&mut layered);
        let secret_store = repo.secret_store();
        let wake = ctx.clone();
        let store = crate::store_worker::StoreWorker::spawn(repo, move || wake.request_repaint());
        let (host_tx, host_rx) = std::sync::mpsc::channel();
        let mut diagnostics_ui = crate::diagnostics::DiagnosticsUi::default();
        if let Some(diagnostics) = &diagnostics {
            diagnostics_ui.notice = diagnostics.take_startup_notice();
        }
        let storage_warning = match (storage_warning, diagnostics_warning) {
            (Some(first), Some(second)) => Some(format!("{first}; {second}")),
            (first, second) => first.or(second),
        };
        // Read before `config_dir` moves into the app, once per launch.
        let update_state = crate::updates::State::load(&config_dir);
        let mut app = Self {
            engine,
            store,
            secret_store,
            reconnect_on_listing: Vec::new(),
            settings,
            keymap: sqlr_store::StoredKeymap::load(&config_dir),
            layered,
            settings_save: None,
            settings_dirty: false,
            formats: Formats::default(),
            data_dir: data_dir.unwrap_or_else(|| config_dir.clone()),
            config_dir,
            connections: Vec::new(),
            folders: Vec::new(),
            profiles: ConnectionProfile::builtins(),
            profiles_seen: 0,
            dialects: HashMap::new(),
            tree: TreeState::default(),
            dock: DockState::new(Vec::new()),
            next_tab: 0,
            show_value_panel: false,
            show_history: false,
            show_operations: false,
            cancel_all_operations: false,
            left: LeftPane::Connections,
            library: LibraryUi::default(),
            palette: AppPalette::default(),
            conn_dialog: None,
            prefs: None,
            shortcuts: ShortcutsDialog::default(),
            #[cfg(target_os = "macos")]
            native_menu: None,
            delete_pending: None,
            clear_history_pending: None,
            folder_delete_pending: None,
            status_bar: StatusBarState::default(),
            toasts: sqlr_ui::Toasts::default(),
            applied_ui_scale: None,
            folder_opener: default_folder_opener(),
            storage_warning,
            diagnostics,
            diagnostics_ui,
            graphics_backend: None,
            confirm: None,
            ending: None,
            kept_open: HashMap::new(),
            quit_confirmed: false,
            quit_now: false,
            pending_redis: std::collections::HashSet::new(),
            profiles_dialog: None,
            client_migration: None,
            secret_prompt: None,
            entered_secrets: HashMap::new(),
            objects: Default::default(),
            admin: Default::default(),
            changes_dialog: None,
            copy_pending: None,
            export_dialog: None,
            import_dialog: None,
            transfer_wizard: None,
            compare_dialog: None,
            datagen_dialog: None,
            dump_dialog: None,
            restore_dialog: None,
            index_admin: Default::default(),
            jobs: Vec::new(),
            file_answer: None,
            databases_dialog: None,
            drivers_ui: Default::default(),
            demo: None,
            mcp: Default::default(),
            mcp_token_store: mcp_token_store
                .unwrap_or_else(|| Arc::new(sqlr_store::KeyringMcpTokenStore)),
            host_jobs: host_rx,
            host_tx,
            pending_approvals: Default::default(),
            approval_focus_pending: false,
            pending_connection_writes: HashMap::new(),
            terminal: Default::default(),
            terminal_workspace: Default::default(),
            ui_ctx: None,
            update: crate::updates::UpdateFlow::Idle,
            update_rx: None,
            update_cancel: None,
            update_state,
            update_state_save: None,
            update_state_dirty: false,
            update_target: None,
        };
        if let Some(notice) = startup_notice {
            app.notify(notice);
        }
        if let Some(corrupt) = layered_notice.corrupt {
            app.notify(corrupt);
        }
        app.apply_engine_settings();
        tracing::info!(
            target: "sqlr_app::session",
            version = env!("CARGO_PKG_VERSION"),
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
            data_dir = %app.data_dir.display(),
            drivers = app.engine.manifests().len(),
            "session started"
        );
        // The daily check starts once everything it may touch exists; its answer comes
        // back through `poll_updates`.
        if check_updates {
            app.start_update_check(ctx);
        }
        app.reload_profiles();
        app.apply_listing(listing);
        app
    }

    /// Polls background work without drawing, including while the window is hidden.
    pub fn logic(&mut self, ctx: &egui::Context) {
        self.sync_profiles();
        self.poll_store();
        self.agents_tick(ctx);
        self.poll_settings_save(ctx);
        self.poll_updates(ctx);
        self.poll_update_state_save(ctx);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        sqlr_ui::keymap::normalize_platform_clipboard(ui.ctx(), &self.keymap.keymap);
        // Sampled before anything else sees the key: what is open now gets Esc first.
        let esc_stop = self.esc_may_stop(ui.ctx());
        // Cheap when unchanged; follows the OS while the setting is System.
        let accent = self.layered.get("appearance.accent", None).0;
        let palette = theme::palette_for(ui.ctx(), self.settings.theme);
        let palette = match accent {
            sqlr_core::SettingValue::Color(rgb) => palette.with_accent(rgb),
            _ => palette,
        };
        theme::apply(ui.ctx(), &palette, self.settings.mono_font_size);
        self.apply_ui_scale(ui.ctx());
        self.apply_editor_settings();
        // Menu bar commands first: they may open a dialog or toggle a panel this frame.
        #[cfg(target_os = "macos")]
        self.native_menu_ui(ui.ctx());
        sqlr_ui::keymap::set_ui_keymap(ui.ctx(), self.keymap.keymap.clone());
        let terminal_focused = self.terminal.focused();
        for action in crate::actions::dispatch_actions(ui.ctx(), &self.keymap.keymap, |action| {
            self.prefs.is_none()
                && self.palette.state.mode.is_none()
                && action != sqlr_core::ActionId::Stop
                && (!terminal_focused
                    || matches!(
                        action,
                        sqlr_core::ActionId::ToggleTerminal
                            | sqlr_core::ActionId::Settings
                            | sqlr_core::ActionId::Quit
                            | sqlr_core::ActionId::Shortcuts
                            | sqlr_core::ActionId::Commands
                            | sqlr_core::ActionId::ShowOperations
                    ))
        }) {
            self.dispatch_action(action, ui.ctx());
        }
        // Also support embedders that drive the UI without an eframe logic hook.
        self.logic(ui.ctx());
        self.approval_ui(ui.ctx());
        self.open_pending_redis();
        self.forget_failed_secrets();
        let failures = self.engine.take_notices();
        let failed = !failures.is_empty();
        for notice in failures {
            self.notify(notice);
        }
        self.poll_applies();
        self.guard_quit(ui.ctx());
        self.end_prompts(ui.ctx(), failed);
        self.confirm_ui(ui.ctx());
        // Before the editors, so the palette's keys reach it first.
        self.palette_ui(ui.ctx());

        let bar = |style: &egui::Style| {
            egui::Frame::side_top_panel(style).inner_margin(Margin::symmetric(Spacing::M as i8, 0))
        };
        // One chrome bar: app mark and menus, the tab's
        // connection, the execution controls, then the panel toggles on the right.
        egui::Panel::top("toolbar")
            .exact_size(Spacing::TOOLBAR)
            .frame(bar(ui.style()).fill(theme::palette(ui.ctx()).bg_toolbar))
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status_bar")
            .exact_size(Spacing::STATUS)
            .frame(bar(ui.style()))
            .show(ui, |ui| self.status(ui));
        // The sidebar runs the full height; the bottom panels dock under the editors only.
        let side = |style: &egui::Style| {
            egui::Frame::side_top_panel(style).inner_margin(Margin::same(Spacing::M as i8))
        };
        let sidebar = (ui.available_width() * 0.2).clamp(200.0, 270.0);
        egui::Panel::left("connections")
            .resizable(true)
            .default_size(sidebar)
            .min_size(160.0)
            .frame(side(ui.style()))
            .show(ui, |ui| self.left_panel(ui));
        self.operations_ui(ui);
        if self.terminal.is_open() {
            egui::Panel::bottom("terminal")
                .resizable(true)
                .default_size(300.0)
                .min_size(120.0)
                .frame(
                    egui::Frame::side_top_panel(ui.style())
                        .inner_margin(Margin::same(Spacing::M as i8)),
                )
                .show(ui, |ui| self.terminal_ui(ui));
        }
        if self.show_history {
            egui::Panel::bottom("history")
                .resizable(true)
                .default_size(220.0)
                .min_size(100.0)
                .frame(
                    egui::Frame::side_top_panel(ui.style())
                        .inner_margin(Margin::same(Spacing::M as i8)),
                )
                .show(ui, |ui| self.history_ui(ui));
        }
        // Document results own their inspector; Redis has its own value editor.
        if self.show_value_panel && !self.active_is_nosql_tab() {
            egui::Panel::right("value")
                .resizable(true)
                .default_size(280.0)
                .min_size(160.0)
                .frame(side(ui.style()))
                .show(ui, |ui| self.value(ui));
        }
        egui::CentralPanel::no_frame().show(ui, |ui| self.tabs(ui));
        self.dialogs(ui.ctx());
        self.databases_ui(ui.ctx());
        self.drivers_frame(ui.ctx());
        self.demo_ui();
        self.library_dialogs(ui.ctx());
        self.objects_ui(ui.ctx());
        self.admin_ui(ui.ctx());
        self.changes_dialog_ui(ui.ctx());
        self.copy_confirm_ui(ui.ctx());
        self.transfer_ui(ui.ctx());
        self.client_migration_ui(ui.ctx());
        self.transfer_wizard_ui(ui.ctx());
        self.compare_ui(ui.ctx());
        self.datagen_ui(ui.ctx());
        self.database_tools_ui(ui.ctx());
        self.index_admin_ui(ui.ctx());
        if let Some(sqlr_ui::shortcuts::ShortcutsAction::OpenKeymap) =
            self.shortcuts
                .show(ui.ctx(), &self.keymap.keymap, &self.keymap.preset)
        {
            self.open_preferences_section("Keymap");
        }
        self.diagnostics_ui(ui.ctx());
        self.update_ui(ui.ctx());
        self.toasts.show(ui.ctx());
        self.esc_stop(ui.ctx(), esc_stop);
    }

    /// Records the renderer eframe chose, once, for the diagnostics environment.
    pub fn observe_graphics(&mut self, backend: &str) {
        if self.graphics_backend.is_some() {
            return;
        }
        self.graphics_backend = Some(backend.to_string());
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.set_graphics_backend(backend);
        }
    }

    /// The active tab's editor text.
    #[doc(hidden)]
    /// Number of saved connections whose explorer session is connected.
    pub fn connected_connection_count(&self) -> usize {
        self.connections
            .iter()
            .filter(|c| {
                matches!(
                    self.engine.session_status(&c.id),
                    Some(SessionStatus::Connected(_))
                )
            })
            .count()
    }

    /// The session's diagnostics, for tests and embedding.
    #[doc(hidden)]
    pub fn diagnostics_for_tests(&self) -> Option<&Arc<crate::diagnostics::Diagnostics>> {
        self.diagnostics.as_ref()
    }

    /// The Diagnostics window's state, for tests.
    #[doc(hidden)]
    pub fn diagnostics_ui_for_tests(&self) -> &crate::diagnostics::DiagnosticsUi {
        &self.diagnostics_ui
    }

    pub fn active_text(&mut self) -> Option<String> {
        self.active_tab().map(|t| t.editor.text())
    }

    /// The explorer's selected connection and node.
    #[doc(hidden)]
    pub fn explorer_selection(&self) -> Option<(String, sqlr_core::NodePath)> {
        self.tree.selected.clone()
    }

    /// Whether the command palette is open.
    #[doc(hidden)]
    pub fn palette_open(&self) -> bool {
        self.palette.state.mode.is_some()
    }

    /// The live toasts' texts, oldest first.
    #[doc(hidden)]
    pub fn toast_texts(&self) -> Vec<String> {
        self.toasts.texts()
    }

    /// Every tab's title, in dock order (the dock's tab bar is not in the accessibility tree).
    #[doc(hidden)]
    pub fn tab_titles(&self) -> Vec<String> {
        self.dock
            .iter_all_tabs()
            .map(|(_, t)| t.title.clone())
            .collect()
    }

    /// Every tab's connection id, in dock order.
    #[doc(hidden)]
    pub fn tab_conns(&self) -> Vec<Option<String>> {
        self.dock
            .iter_all_tabs()
            .map(|(_, t)| t.conn.clone())
            .collect()
    }

    /// Flips soft wrap and saves the setting off the UI thread (the editors pick it up next
    /// frame).
    pub(crate) fn toggle_soft_wrap(&mut self, ctx: &egui::Context) {
        self.settings.soft_wrap = !self.settings.soft_wrap;
        self.settings.apply_to(&mut self.layered);
        self.save_settings(ctx);
    }

    /// Applies the one diagnostics privacy setting to logs, incidents, bundles and the
    /// engine, and persists it.
    pub(crate) fn set_include_sql(&mut self, include: bool, ctx: &egui::Context) {
        if self.settings.include_sql_in_diagnostics == include {
            return;
        }
        self.settings.include_sql_in_diagnostics = include;
        self.settings.apply_to(&mut self.layered);
        self.apply_engine_settings();
        self.save_settings(ctx);
    }

    /// Writes the layered settings on a worker thread. One save runs at a time: a request
    /// made meanwhile is saved once it ends, so an older state cannot land last.
    pub(crate) fn save_settings(&mut self, ctx: &egui::Context) {
        if self.settings_save.is_some() {
            self.settings_dirty = true;
            return;
        }
        let live = self.layered.clone();
        let dir = self.config_dir.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        self.settings_save = Some(receiver);
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _ = sender.send(live.save(&dir));
            repaint.request_repaint();
        });
    }

    /// Shows the error of a finished settings save, then starts the one that was asked for
    /// meanwhile.
    fn poll_settings_save(&mut self, ctx: &egui::Context) {
        let Some(receiver) = &self.settings_save else {
            return;
        };
        let failure = match receiver.try_recv() {
            Ok(result) => result.err().map(|error| error.to_string()),
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some("the save thread stopped".to_string())
            }
        };
        self.settings_save = None;
        if let Some(error) = failure {
            self.toasts.push(
                ToastKind::Error,
                format!("Could not save settings: {error}"),
            );
        }
        if std::mem::take(&mut self.settings_dirty) {
            self.save_settings(ctx);
        }
    }

    /// Whether soft wrap is on (the View menu toggle and the Settings checkbox).
    #[doc(hidden)]
    pub fn soft_wrap_enabled(&self) -> bool {
        self.settings.soft_wrap
    }

    /// Connects `id` like the tree's Connect does.
    #[doc(hidden)]
    pub fn connect_for_tests(&mut self, id: &str) {
        self.connect(id);
    }

    /// Whether the app knows the SQL dialect of connection `id`.
    #[doc(hidden)]
    pub fn has_dialect(&self, id: &str) -> bool {
        self.dialects.contains_key(id)
    }

    /// Opens the new-connection form for a known database (what picking it in the catalog
    /// does).
    #[doc(hidden)]
    pub fn open_known_connection_for_tests(&mut self, id: &str) {
        self.open_known_connection(id);
    }

    /// Opens the read-only databases matrix (the settings page's "Databases…").
    #[doc(hidden)]
    pub fn open_databases_matrix_for_tests(&mut self) {
        self.databases_dialog = Some(crate::databases_dialog::DatabasesDialog::matrix());
    }

    /// Every tab's server database, in dock order.
    #[doc(hidden)]
    pub fn tab_databases(&self) -> Vec<Option<String>> {
        self.dock
            .iter_all_tabs()
            .map(|(_, t)| t.database.clone())
            .collect()
    }

    /// Every tab's tint (its connection's profile color), in dock order.
    #[doc(hidden)]
    pub fn tab_tints(&self) -> Vec<Option<[u8; 3]>> {
        let tints = self.profile_tints();
        self.dock
            .iter_all_tabs()
            .map(|(_, t)| t.conn.as_ref().and_then(|c| tints.get(c)).copied())
            .collect()
    }

    /// Opens a new query tab, as the toolbar's New query does.
    #[doc(hidden)]
    pub fn new_query_tab_for_tests(&mut self) -> u64 {
        self.new_tab(None, None, "")
    }

    // ----- panels -----

    /// Moves File, Edit, View, Query and Help into the macOS menu bar. Called by the
    /// binary only: tests and other platforms keep the in-window menus.
    #[cfg(target_os = "macos")]
    pub fn install_native_menu(&mut self, ctx: &egui::Context) {
        match NativeMenu::install(
            ctx,
            self.show_value_panel,
            self.show_history,
            self.terminal.is_open(),
            self.settings.soft_wrap,
        ) {
            Ok(menu) => self.native_menu = Some(menu),
            Err(error) => {
                tracing::warn!(%error, "native menu unavailable; keeping the in-window menus");
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn native_menu_ui(&mut self, ctx: &egui::Context) {
        let commands = self
            .native_menu
            .as_ref()
            .map(NativeMenu::drain)
            .unwrap_or_default();
        for command in commands {
            // While Preferences is open the keys are its own (the keymap editor records
            // them): an accelerator goes back to being its key press.
            // Clipboard commands stay events: text fields take those, not keys.
            use crate::palette::AppCommand as C;
            let clipboard = matches!(command, C::Copy | C::Cut | C::Paste);
            let command = match command.action_id() {
                Some(action) if self.prefs.is_some() && !clipboard => C::Key(action),
                _ => command,
            };
            self.run_command(command, ctx);
        }
        if let Some(menu) = &self.native_menu {
            menu.sync_keymap(&self.keymap.keymap);
            menu.sync(
                self.show_value_panel,
                self.show_history,
                self.terminal.is_open(),
                self.settings.soft_wrap,
            );
        }
    }

    fn status(&mut self, ui: &mut egui::Ui) {
        let (page_size, result) = match self.active_key() {
            Some(key) => {
                let engine = &self.engine;
                match tab_mut(&mut self.dock, key) {
                    Some(t) => (Some(t.page_size), t.shown_query(engine)),
                    None => (None, None),
                }
            }
            None => (None, None),
        };
        let info = result.and_then(|q| ResultInfo::read(&self.engine, q));
        let conn = self
            .active_tab()
            .and_then(|t| t.conn.clone())
            .and_then(|id| {
                let cfg = self.connection(&id)?;
                let name = cfg.name.clone();
                let driver = self
                    .engine
                    .drivers()
                    .get(&cfg.driver)
                    .map_or_else(|| cfg.driver.clone(), |d| d.manifest().name.clone());
                let status = self.engine.session_status(&id);
                Some(ConnInfo {
                    name,
                    driver,
                    status,
                })
            });
        let result_controls = self.active_tab().is_some_and(|t| {
            t.ddl.is_none() && t.er.is_none() && t.pane != crate::tabs::SinglePane::Plan
        });
        let transaction = result_controls.then(|| self.tx_line()).flatten();
        let selection = result.and_then(|q| {
            let key = self.active_key()?;
            let engine = &self.engine;
            let grid = tab_mut(&mut self.dock, key)?.grids.get(&q)?;
            let result = engine.result(q)?;
            let rows = result.read();
            grid.selection_summary(&rows.rows)
        });
        let caret = self
            .active_tab()
            .filter(|t| {
                t.redis.is_none()
                    && t.object.is_none()
                    && t.admin.is_none()
                    && t.ddl.is_none()
                    && t.er.is_none()
            })
            .map(|t| {
                let head = t.editor.model().primary().head;
                (head.line + 1, head.col + 1)
            });
        let job = self.job_line();
        let view = StatusView {
            conn,
            transaction,
            caret,
            page_size: page_size.filter(|_| result_controls),
            info: info.as_ref().filter(|_| result_controls),
            selection: selection.as_deref().filter(|_| result_controls),
            storage_warning: self.storage_warning.as_deref(),
            job: job.as_ref().map(|(text, _)| text.as_str()),
            terminal_open: self.terminal.is_open(),
            mcp: self
                .settings
                .agents_enabled
                .then(|| crate::shell::status::McpView {
                    running: self.mcp.status.is_running(),
                    sessions: self.mcp.sessions(),
                    detail: format!(
                        "{}: {} active session(s){}",
                        self.mcp.status.label(),
                        self.mcp.sessions(),
                        self.mcp
                            .url()
                            .map_or(String::new(), |url| format!(" · {url}"))
                    ),
                }),
        };
        let action = status_bar(ui, &mut self.status_bar, &view);
        match action {
            Some(StatusAction::SetPageSize(n)) => {
                if let Some(tab) = self.active_tab() {
                    tab.page_size = n;
                }
                if let Some(q) = result {
                    self.engine.set_page_size(q, n);
                }
            }
            Some(StatusAction::FetchAll) => {
                if let Some(q) = result {
                    self.engine.fetch_all(q);
                }
            }
            Some(StatusAction::CancelJob) => {
                if let Some((_, id)) = job {
                    self.engine.cancel_job(id);
                }
            }
            Some(StatusAction::OpenAgents) => {
                self.open_preferences_section("Agents");
            }
            Some(StatusAction::OpenCommands) => {
                self.open_palette(sqlr_ui::PaletteMode::Commands);
            }
            Some(StatusAction::ToggleTerminal) => self.terminal.toggle(),
            None => {}
        }
    }

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::S;
            for (pane, label) in [
                (LeftPane::Connections, "Connections"),
                (LeftPane::Snippets, "Snippets"),
            ] {
                if theme::dialog_tab(ui, self.left == pane, label).clicked() {
                    self.left = pane;
                }
            }
        });
        theme::hairline(ui);
        ui.add_space(Spacing::S);
        match self.left {
            LeftPane::Connections => {
                let tints = self.profile_tints();
                let editing = self.editing_by_conn();
                let snapshot = TreeSnapshot::build(
                    &self.engine,
                    &self.connections,
                    &self.folders,
                    &self.tree,
                    &tints,
                    &editing,
                    self.settings.show_empty_folders,
                );
                self.tree.show_empty_folders = self.settings.show_empty_folders;
                let actions = schema_tree(ui, &mut self.tree, &snapshot);
                for action in actions {
                    self.tree_action(ui.ctx(), action);
                }
            }
            LeftPane::Snippets => self.snippets_ui(ui),
        }
    }

    fn value(&mut self, ui: &mut egui::Ui) {
        theme::section_title(ui, "Value");
        theme::hairline(ui);
        ui.add_space(Spacing::S);
        self.value_ui(ui);
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        if self.dock.iter_all_tabs().next().is_none() {
            self.welcome_ui(ui);
            return;
        }
        let active = self
            .dock
            .iter_leaves()
            .filter_map(|(_, leaf)| leaf.tabs.get(leaf.active.0).map(|t| t.key))
            .collect();
        let tints = self.profile_tints();
        let drivers = self.engine.drivers();
        let editable: std::collections::HashSet<String> = self
            .connections
            .iter()
            .filter(|c| {
                drivers
                    .get(&c.driver)
                    .is_some_and(|d| d.manifest().capabilities.editable_results)
            })
            .map(|c| c.id.clone())
            .collect();
        let explain: std::collections::HashSet<String> = self
            .connections
            .iter()
            .filter(|c| {
                drivers
                    .get(&c.driver)
                    .is_some_and(|d| d.manifest().capabilities.explain)
            })
            .map(|c| c.id.clone())
            .collect();
        let mut viewer = Viewer {
            engine: &self.engine,
            dialects: &self.dialects,
            connections: &self.connections,
            editable: &editable,
            explain: &explain,
            tints: &tints,
            formats: &self.formats,
            show_value_panel: self.show_value_panel,
            include_sql: self.settings.include_sql_in_diagnostics,
            active,
            palette: theme::palette(ui.ctx()),
            icon_font: theme::fonts::icons(ui.ctx(), icons::SIZE),
            commands: Vec::new(),
        };
        DockArea::new(&mut self.dock)
            .style(theme::dock_style(ui.ctx()))
            .show_add_buttons(true)
            .show_leaf_collapse_buttons(false)
            .show_inside(ui, &mut viewer);
        // Grids outside the tabs (previews, dialogs) use the global formats.
        sqlr_ui::set_data_formats(ui.ctx(), self.formats.global().clone());
        // Applied with no result guard held: some of these lock the result.
        for command in std::mem::take(&mut viewer.commands) {
            match command {
                TabCommand::FetchMore(q) => self.engine.fetch_more(q),
                TabCommand::OpenResultTable { tab, q } => self.open_result_table(tab, q),
                TabCommand::SaveSnippet(key) => self.save_as_snippet_for(key),
                TabCommand::RequestClose(key) => self.request_close_tab(key),
                TabCommand::RevealConnection(key) => self.reveal_tab_connection(key),
                TabCommand::ChangeDatabase { tab, database } => {
                    if let Some(tab) = tab_mut(&mut self.dock, tab) {
                        tab.database = Some(database);
                    }
                }
                TabCommand::Add => {
                    self.new_tab(None, None, "");
                }
                TabCommand::Redis { tab, command } => self.run_redis_command(tab, command),
                TabCommand::Object { key, action } => self.object_action(key, action),
                TabCommand::Doc { tab, command } => self.run_doc_command(tab, command),
                TabCommand::ApplyGrants(key) => self.apply_object_grants(key),
                TabCommand::DiscardData(key) => self.ask_discard_data(key),
                TabCommand::Admin { key, command } => self.admin_command(key, command),
                TabCommand::Ddl { key, action } => self.ddl_action(key, action),
                TabCommand::Changes { tab, q, action } => self.changes_action(tab, q, action),
                TabCommand::Grid { tab, q, action } => self.grid_action(tab, q, action),
                TabCommand::Export { tab, q } => self.open_result_export(tab, q),
            }
        }
    }

    // ----- actions -----

    fn tree_action(&mut self, ctx: &egui::Context, action: TreeAction) {
        match action {
            TreeAction::Retry(id) => self.retry_connection(&id),
            TreeAction::InstallDriver(driver) => self.request_driver_install(&driver, None),
            TreeAction::NewEditorTab(id) => self.new_editor_tab_on(&id),
            TreeAction::CopyUrl(id) => self.copy_connection_url(ctx, &id),
            TreeAction::SelectRows { conn, node, limit } => self.select_rows(&conn, &node, limit),
            TreeAction::OpenDdl { conn, node } => self.open_ddl(&conn, &node),
            TreeAction::CopyName {
                conn,
                node,
                qualified,
            } => self.copy_node_name(ctx, &conn, &node, qualified),
            TreeAction::NewConnection => self.open_new_connection(),
            TreeAction::OpenDemo => self.open_demo(),
            TreeAction::ImportConnections => self.open_client_import(),
            TreeAction::NewConnectionIn(folder) => self.open_new_connection_in(Some(&folder)),
            TreeAction::CreateFolder(name) => self.create_folder(&name),
            TreeAction::RenameFolder { from, to } => self.rename_folder(&from, &to),
            TreeAction::DeleteFolder(name) => self.folder_delete_pending = Some(name),
            TreeAction::MoveConnection {
                conn,
                folder,
                target,
            } => self.move_connection(&conn, folder.as_deref(), target),
            TreeAction::MoveFolder { folder, target } => self.move_folder(&folder, target),
            TreeAction::Connect(id) => {
                if self.connect(&id) && self.is_key_value(&id) {
                    self.pending_redis.insert(id);
                }
            }
            TreeAction::Disconnect(id) => self.request_disconnect(&id),
            TreeAction::Reconnect(id) => self.request_reconnect(&id),
            TreeAction::Edit(id) => self.open_edit_connection(&id),
            TreeAction::Duplicate(id) => self.duplicate_connection(&id),
            TreeAction::Delete(id) => self.delete_pending = Some(id),
            TreeAction::SetAgentAccess { conn, access } => self.set_agent_access(&conn, access),
            TreeAction::Expand { conn, path } => self.engine.browse(&conn, path),
            TreeAction::Refresh { conn, path } => self.engine.refresh(&conn, path),
            TreeAction::OpenData { conn, node } => self.open_data(&conn, &node),
            TreeAction::NewQuery { conn, database } => self.new_query_on(&conn, &database),
            TreeAction::ExportData { conn, node } => self.open_node_export(&conn, &node),
            TreeAction::ImportData { conn, node } => self.open_node_import(&conn, &node),
            TreeAction::GenerateData { conn, node } => self.open_datagen(&conn, &node),
            TreeAction::ErDiagram { conn, node } => {
                let (database, schema) = self.schema_target(&conn, &node);
                self.open_er_diagram(&conn, database, schema, None);
            }
            TreeAction::CompareSchema { conn, node } => {
                let (database, schema) = self.schema_target(&conn, &node);
                self.open_compare(Some(&conn), database, Some(schema));
            }
            TreeAction::TransferData { conn, node } => {
                self.open_transfer_wizard(Some((&conn, &node)))
            }
            TreeAction::DumpDatabase { conn, database } => {
                self.open_database_dump(&conn, &database)
            }
            TreeAction::RestoreDatabase { conn, database } => {
                self.open_database_restore(&conn, &database)
            }
            // Index administration of document databases.
            TreeAction::NewObject {
                conn,
                node: Some(node),
                kind: sqlr_core::ObjectKind::Index,
            } if self.is_document(&conn) => self.open_index_dialog(&conn, &node),
            TreeAction::DropObject { conn, node } if self.is_document(&conn) => {
                self.ask_drop_index(&conn, &node)
            }
            TreeAction::OpenAdmin { conn, panel } => self.open_admin(&conn, panel),
            other => self.object_tree_action(other),
        }
    }

    /// The database and schema a tree node stands for. A database node of a server that keeps
    /// schemas inside databases (Postgres) means that database's default schema.
    fn schema_target(&self, conn: &str, node: &sqlr_core::Node) -> (Option<String>, String) {
        let database = self.engine.database_for_path(conn, &node.path);
        match node.kind {
            sqlr_core::NodeKind::Schema => (database, node.name.clone()),
            sqlr_core::NodeKind::Database => match self.dialects.get(conn) {
                Some(d) if !d.database_is_schema() => {
                    let schema = d
                        .default_schemas()
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| "public".to_string());
                    (database, schema)
                }
                _ => (None, node.name.clone()),
            },
            _ => (None, node.name.clone()),
        }
    }

    pub(crate) fn connection(&self, id: &str) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.id == id)
    }

    /// A connection's tint: its own color, else its profile's.
    pub(crate) fn connection_tint(&self, id: &str) -> Option<[u8; 3]> {
        let c = self.connection(id)?;
        c.color.or_else(|| self.profile_for(c).color)
    }

    /// `(id, name)` of every saved connection.
    pub(crate) fn conn_names(&self) -> Vec<(String, String)> {
        self.connections
            .iter()
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect()
    }

    /// The driver's manifest with the engine's SSH fields.
    pub(crate) fn manifest(&self, driver: &str) -> Option<DriverManifest> {
        self.engine.manifest(driver)
    }

    /// The settings the engine follows: sessions per tab, the connect timeout and the row
    /// cap (globally, then what each connection overrides), and the export formats.
    pub(crate) fn apply_engine_settings(&mut self) {
        self.engine
            .set_session_per_tab(self.settings.session_per_tab);
        self.engine
            .set_connect_timeout(self.settings.connect_timeout_s);
        self.engine.set_row_cap(self.settings.row_cap);
        self.engine
            .set_include_sql(self.settings.include_sql_in_diagnostics);
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.set_include_sql(self.settings.include_sql_in_diagnostics);
        }
        self.engine.set_result_memory_limits(
            self.settings.result_memory_mib.saturating_mul(1 << 20),
            self.settings.total_memory_mib.saturating_mul(1 << 20),
        );
        self.formats = Formats::build(&self.layered, &self.connections);
        let global = self.formats.global();
        self.engine
            .set_export_formats(global.use_for_export.then(|| global.clone()));
        for cfg in &self.connections {
            self.engine.set_connection_settings(
                &cfg.id,
                crate::prefs::engine_overrides(&self.layered, cfg),
            );
        }
    }

    /// The value of a setting for a connection (the global one for none).
    pub(crate) fn setting_for(&self, id: &str, conn: Option<&str>) -> sqlr_core::SettingValue {
        let key = conn
            .and_then(|c| self.connection(c))
            .map(crate::prefs::conn_key);
        self.layered.get(id, key).0
    }

    /// Opens the preferences window at `scope`.
    pub(crate) fn open_preferences(&mut self, scope: sqlr_core::Level) {
        if self.prefs.is_none() {
            self.prefs = Some(PrefsWindow::new(&self.layered, scope));
        }
    }

    pub(crate) fn open_preferences_section(&mut self, section: &'static str) {
        self.prefs = Some(PrefsWindow::new_for_section(
            &self.layered,
            sqlr_core::Level::Global,
            section,
        ));
    }

    /// The JSON text of the value of setting `id` for a connection (global for none).
    #[doc(hidden)]
    pub fn setting_text(&self, id: &str, conn: Option<&str>) -> String {
        self.setting_for(id, conn).to_json().to_string()
    }

    /// Opens the preferences window editing the overrides of connection `id`.
    #[doc(hidden)]
    pub fn open_preferences_for_connection(&mut self, id: &str) {
        self.open_preferences(sqlr_core::Level::Connection(id.to_string()));
    }

    /// Gives every open tab the editor settings (every frame; the setters are idempotent).
    pub(crate) fn apply_editor_settings(&mut self) {
        let format_options =
            sqlr_core::FormatOptions::from_values(|id| self.layered.get(id, None).0);
        let get = |id| self.layered.get(id, None).0;
        let completion_options = sqlr_ui::CompletionOptions {
            auto_open: get("editor.completion_auto_open").as_bool(),
            delay_ms: get("editor.completion_delay_ms").as_i64() as u64,
            insert_on_tab: get("editor.completion_insert_on_tab").as_bool(),
            keyword_case: match get("editor.completion_keyword_case").as_str() {
                "upper" => sqlr_core::Case::Upper,
                "lower" => sqlr_core::Case::Lower,
                _ => sqlr_core::Case::Preserve,
            },
        };
        let line_numbers = self.settings.line_numbers;
        let soft_wrap = self.settings.soft_wrap;
        for (_, tab) in self.dock.iter_all_tabs_mut() {
            tab.editor.set_format_options(format_options.clone());
            tab.editor
                .set_completion_options(completion_options.clone());
            tab.editor.set_line_numbers(line_numbers);
            tab.editor.set_soft_wrap(soft_wrap);
        }
    }

    /// Opens `path` in the OS file manager, through [`SqlrApp::folder_opener`].
    pub(crate) fn open_folder(&self, path: &Path) {
        (self.folder_opener)(path);
    }

    /// Replaces the folder opener (tests record where it was sent).
    pub fn set_folder_opener(&mut self, f: impl Fn(&Path) + Send + Sync + 'static) {
        self.folder_opener = Arc::new(f);
    }

    /// Asks the engine to read `profiles.toml` again; [`SqlrApp::sync_profiles`] picks the
    /// result up.
    pub(crate) fn reload_profiles(&mut self) {
        self.engine.load_profiles(self.config_dir.clone());
    }

    /// Adopts the profiles the engine last read, when they changed (every frame; cheap).
    pub(crate) fn sync_profiles(&mut self) {
        let revision = self.engine.profiles_revision();
        if revision == self.profiles_seen {
            return;
        }
        self.profiles_seen = revision;
        if let Some(list) = self.engine.profiles() {
            self.profiles = list.as_ref().clone();
        }
    }

    /// The connection's profile; an unknown or missing one is `development`.
    pub(crate) fn profile_for(&self, cfg: &ConnectionConfig) -> ConnectionProfile {
        crate::prefs::tightened(profile_of(&self.profiles, cfg), &self.layered, cfg)
    }

    /// Reads the saved connections and folders again (on the store thread).
    pub(crate) fn reload_connections(&mut self) {
        self.store.send(StoreOp::List);
    }

    /// Adopts the saved connections and folders as the store read them.
    pub(crate) fn apply_listing(&mut self, listing: Listing) {
        match listing.connections {
            Ok(list) => self.connections = list,
            Err(e) => {
                tracing::warn!(error = %e, "reading saved connections failed");
                self.notify(format!("Could not read connections: {e}"));
            }
        }
        match listing.folders {
            Ok(folders) => self.folders = folders,
            Err(e) => {
                tracing::warn!(error = %e, "reading connection folders failed");
                self.notify(format!("Could not read connection folders: {e}"));
            }
        }
        let drivers = self.engine.drivers();
        self.dialects = self
            .connections
            .iter()
            .filter_map(|c| Some((c.id.clone(), drivers.get(&c.driver)?.dialect_for(c)?)))
            .collect();
        for id in std::mem::take(&mut self.reconnect_on_listing) {
            if self.is_live(&id) {
                self.request_reconnect(&id);
            }
        }
        // An edit may have changed a connection's driver: its tabs follow the new syntax.
        let syntaxes: HashMap<String, Arc<SqlSyntax>> = self
            .connections
            .iter()
            .map(|c| (c.id.clone(), self.syntax_for(Some(&c.id))))
            .collect();
        for (_, tab) in self.dock.iter_all_tabs_mut() {
            if let Some(syntax) = tab.conn.as_ref().and_then(|c| syntaxes.get(c))
                && !Arc::ptr_eq(tab.editor.syntax(), syntax)
            {
                tab.editor.set_syntax(syntax.clone());
            }
        }
    }

    /// Adds a folder; it is selected once the store saved it. The inline row already closed
    /// itself.
    pub(crate) fn create_folder(&mut self, name: &str) {
        self.store
            .send(StoreOp::CreateFolder(name.trim().to_string()));
    }

    /// Renames a folder and keeps the explorer state with it.
    pub(crate) fn rename_folder(&mut self, from: &str, to: &str) {
        self.store.send(StoreOp::RenameFolder {
            from: from.to_string(),
            to: to.trim().to_string(),
        });
    }

    /// Deletes a folder; its connections move to the top level.
    pub(crate) fn delete_folder(&mut self, name: &str) {
        self.store.send(StoreOp::DeleteFolder(name.to_string()));
    }

    pub(crate) fn move_connection(&mut self, conn: &str, folder: Option<&str>, target: TreeDrop) {
        self.store.send(StoreOp::MoveConnection {
            conn: conn.to_string(),
            folder: folder.map(str::to_string),
            target: move_target(target),
        });
    }

    pub(crate) fn move_folder(&mut self, folder: &str, target: TreeDrop) {
        self.store.send(StoreOp::MoveFolder {
            folder: folder.to_string(),
            target: move_target(target),
        });
    }

    /// Follows the store thread's answers (every frame).
    pub(crate) fn poll_store(&mut self) {
        while let Some(reply) = self.store.try_recv() {
            self.store_reply(reply);
        }
    }

    fn store_reply(&mut self, reply: StoreReply) {
        match reply {
            StoreReply::ClientMigration {
                id,
                result,
                listing,
            } => {
                if let Some(listing) = listing {
                    self.apply_listing(listing);
                }
                if let Some(dialog) = &mut self.client_migration {
                    dialog.receive(&id, result, &self.connections, self.engine.drivers());
                }
            }
            StoreReply::Listed(listing) => self.apply_listing(listing),
            StoreReply::FolderCreated {
                name,
                result,
                listing,
            } => {
                self.apply_listing(listing);
                match result {
                    Ok(()) => self.tree.folder_created(&name),
                    Err(e) => {
                        tracing::warn!(folder = name, error = %e, "creating the folder failed");
                        self.notify(format!("Could not create the folder: {e}"));
                        self.tree.folder_create_failed(&name);
                    }
                }
            }
            StoreReply::FolderRenamed {
                from,
                to,
                result,
                listing,
            } => {
                match result {
                    Ok(()) => self.tree.folders_renamed(&from, &to),
                    Err(e) => {
                        tracing::warn!(folder = from, error = %e, "renaming the folder failed");
                        self.notify(format!("Could not rename the folder: {e}"));
                        self.tree.folder_rename_failed(&from, &to);
                    }
                }
                self.apply_listing(listing);
            }
            StoreReply::FolderDeleted {
                name,
                result,
                listing,
            } => {
                match result {
                    Ok(()) => self.tree.folder_deleted(&name),
                    Err(e) => {
                        tracing::warn!(folder = name, error = %e, "deleting the folder failed");
                        self.notify(format!("Could not delete the folder: {e}"));
                    }
                }
                self.apply_listing(listing);
            }
            StoreReply::ConnectionMoved {
                conn,
                folder,
                result,
                listing,
            } => {
                match result {
                    Ok(()) => {
                        if let Some(folder) = &folder {
                            self.tree.set_folder_expanded(folder, true);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(conn, folder = ?folder, error = %e, "moving the connection failed");
                        self.notify(format!("Could not move the connection: {e}"));
                    }
                }
                self.apply_listing(listing);
            }
            StoreReply::FolderMoved {
                folder,
                to,
                result,
                listing,
            } => {
                match result {
                    Ok(()) => self.tree.folders_renamed(&folder, &to),
                    Err(e) => {
                        tracing::warn!(folder, error = %e, "moving the folder failed");
                        self.notify(format!("Could not move the folder: {e}"));
                    }
                }
                self.apply_listing(listing);
            }
            StoreReply::Saved {
                id,
                editing,
                result,
                listing,
            } => {
                self.apply_listing(listing);
                let outcome = result.as_ref().map(|_| ()).map_err(ToString::to_string);
                self.connection_save_ended(&id, editing, result);
                if let Some(acks) = self.pending_connection_writes.remove(&id) {
                    for (ack, name) in acks {
                        let result = match &outcome {
                            Ok(()) => {
                                self.notify(format!("Agent updated connection {name}"));
                                Ok(())
                            }
                            Err(error) => Err(sqlr_mcp::error::ToolError::invalid(error.clone())),
                        };
                        let _ = ack.send(result);
                    }
                }
            }
            StoreReply::Deleted {
                id,
                name,
                result,
                listing,
            } => {
                match result {
                    Ok(()) => self.engine.forget_recent(&id),
                    Err(e) => {
                        tracing::warn!(conn = %id, error = %e, "deleting the connection failed");
                        self.notify(format!("Could not delete {name}: {e}"));
                    }
                }
                self.apply_listing(listing);
            }
            StoreReply::Duplicated {
                name,
                copy,
                listing,
            } => {
                self.apply_listing(listing);
                self.connection_duplicated(&name, copy);
            }
            StoreReply::Secrets { id, result } => {
                if let Some(dialog) = &mut self.conn_dialog {
                    dialog.secrets_arrived(&id, result);
                }
            }
        }
    }

    /// Entered secrets of a connection whose connect failed are forgotten, so the next
    /// connect asks again (a mistyped password is not retried forever).
    pub(crate) fn forget_failed_secrets(&mut self) {
        let engine = &self.engine;
        self.entered_secrets
            .retain(|id, _| !matches!(engine.session_status(id), Some(SessionStatus::Failed(_))));
    }

    /// Connects the connection's explorer session; the engine reads saved secrets from the
    /// keychain. A connection that does not save its secrets asks for them first (once per
    /// app run, or again after a failed connect): then this returns `false` and the connect
    /// happens when the user answers.
    pub(crate) fn connect(&mut self, id: &str) -> bool {
        let Some(mut cfg) = self.connection(id).cloned() else {
            return true;
        };
        // A driver that is not installed (a connection file from another machine) offers
        // its install instead of failing.
        if self.engine.drivers_loaded() && self.engine.drivers().get(&cfg.driver).is_none() {
            self.request_driver_install(&cfg.driver.clone(), None);
            return false;
        }
        // An unknown driver needs no secrets: the engine reports it as failed.
        let secrets = match self.manifest(&cfg.driver) {
            Some(manifest) if !cfg.save_secrets => match self.entered_secrets.get(id) {
                Some(entered) => entered.clone(),
                None => {
                    self.secret_prompt = Some(SecretPrompt::new(&cfg, &manifest));
                    return false;
                }
            },
            _ => Secrets::default(),
        };
        let profile = self.profile_for(&cfg);
        if matches!(cfg.driver.as_str(), "postgres" | "mysql") {
            let (value, level) = self.layered.get(
                &format!("drivers.{}.properties", cfg.driver),
                Some(crate::prefs::conn_key(&cfg)),
            );
            match serde_json::from_str::<std::collections::BTreeMap<String, String>>(value.as_str())
            {
                Ok(properties) => {
                    for (key, value) in properties {
                        let key = format!("property.{key}");
                        if cfg.values.get(&key).is_none_or(|v| v.is_empty())
                            || matches!(level, sqlr_core::Level::Connection(_))
                        {
                            cfg.values.insert(key, value);
                        }
                    }
                }
                Err(error) => {
                    self.notify(format!("Invalid driver properties: {error}"));
                    return false;
                }
            }
        }
        for field in sqlr_core::driver_property_fields(&cfg.driver) {
            let name = field.key.trim_start_matches("property.");
            let setting = format!("drivers.{}.{name}", cfg.driver);
            let (value, level) = self
                .layered
                .get(&setting, Some(crate::prefs::conn_key(&cfg)));
            if !value.as_str().is_empty()
                && (cfg.values.get(&field.key).is_none_or(|v| v.is_empty())
                    || matches!(level, sqlr_core::Level::Connection(_)))
            {
                cfg.values.insert(field.key, value.as_str().to_string());
            }
        }
        self.engine.connect(cfg, secrets, profile);
        true
    }

    pub(crate) fn is_live(&self, id: &str) -> bool {
        matches!(
            self.engine.session_status(id),
            Some(SessionStatus::Connecting | SessionStatus::Connected(_))
        )
    }

    /// Closes every session of `id` and connects it again from its saved config, so edits
    /// and new secrets take effect. Returns `false` when the connection asks for its
    /// secrets first; it reconnects when the user answers.
    pub(crate) fn reconnect(&mut self, id: &str) -> bool {
        self.engine.disconnect(id);
        self.connect(id)
    }

    /// Adopts a saved connection edit. A live connection reconnects at once; with open
    /// transactions, [`SqlrApp::request_reconnect`] asks first.
    pub(crate) fn connection_saved(&mut self, id: &str) {
        // An edited connection asks for its unsaved secrets again.
        self.entered_secrets.remove(id);
        // It reconnects with the config as saved, once the store listed it.
        self.reconnect_on_listing.push(id.to_string());
        self.reload_connections();
    }

    fn new_query_on(&mut self, conn: &str, database: &str) {
        if self.is_key_value(conn) {
            self.open_redis_database(conn, database);
            return;
        }
        if self.is_document(conn) {
            self.new_doc_query(conn, database);
            return;
        }
        let key = self.new_tab(
            Some(format!("Query ({database})")),
            Some(conn.to_string()),
            "",
        );
        if let Some(tab) = self.tab(key) {
            tab.database = Some(database.to_string());
        }
    }

    pub(crate) fn open_data(&mut self, conn: &str, node: &Node) {
        if self.is_key_value(conn) {
            self.open_redis_node(conn, node);
            return;
        }
        if self.is_document(conn) {
            self.open_doc_node(conn, node);
            return;
        }
        let Some((database, sql)) = self.engine.open_data_target(conn, node, None) else {
            self.notify(format!("Cannot open the data of {}", node.name));
            return;
        };
        let title = match &database {
            Some(db) => format!("{} ({db})", node.name),
            None => node.name.clone(),
        };
        self.open_and_run(title, conn, database, &sql);
    }

    // ----- tabs -----

    /// The editor syntax for a tab on `conn`.
    fn syntax_for(&self, conn: Option<&str>) -> Arc<SqlSyntax> {
        conn.and_then(|c| self.dialects.get(c))
            .map_or_else(SqlSyntax::ansi, |d| d.syntax())
    }

    /// Adds a focused tab and returns its key.
    pub(crate) fn new_tab(
        &mut self,
        title: Option<String>,
        conn: Option<String>,
        sql: &str,
    ) -> u64 {
        self.next_tab += 1;
        let key = self.next_tab;
        let title = title.unwrap_or_else(|| format!("Query {key}"));
        let syntax = self.syntax_for(conn.as_deref());
        let page_size = self.setting_for("grid.page_size", conn.as_deref()).as_i64() as usize;
        let mut tab = QueryTab::new(key, title, conn, sql, syntax, page_size);
        tab.editor.request_focus();
        self.dock.push_to_focused_leaf(tab);
        self.focus_tab(key);
        key
    }

    /// Makes `key`'s tab the active tab of its leaf and focuses that leaf.
    pub(crate) fn focus_tab(&mut self, key: u64) {
        if let Some(path) = self.dock.find_tab_from(|t| t.key == key) {
            let _ = self.dock.set_active_tab(path);
            self.dock.set_focused_node_and_surface(path.node_path());
        }
    }

    /// Points the tab at another connection (its database no longer applies) and switches
    /// the editor to that driver's syntax.
    pub(crate) fn set_tab_conn(&mut self, key: u64, conn: Option<String>) {
        let syntax = self.syntax_for(conn.as_deref());
        let Some(old) = self.tab(key).map(|t| t.conn.clone()) else {
            return;
        };
        if old == conn {
            return;
        }
        // A Redis browser belongs to the connection it was opened on.
        if self.tab(key).is_some_and(|t| t.redis.is_some()) {
            self.notify_as(
                ToastKind::Info,
                "A Redis tab stays on its connection: open another one",
            );
            return;
        }
        if self.tab(key).is_some_and(|t| t.doc.is_some()) {
            self.notify_as(
                ToastKind::Info,
                "A document tab stays on its connection: open another one",
            );
            return;
        }
        // A SQL tab cannot show a document or key-value connection: those open their own tabs.
        if let Some(target) = conn.as_deref().filter(|c| self.is_nosql(c)) {
            let name = self.connection(target).map_or(target, |c| c.name.as_str());
            self.notify_as(
                ToastKind::Info,
                format!("{name} does not run SQL: open it from the explorer"),
            );
            return;
        }
        // The tab's session on its old connection is no longer used.
        if let Some(old) = old {
            let session = self.engine.session(&old, Some(key));
            if session.slot != sqlr_engine::Slot::Meta
                && self
                    .engine
                    .transaction(&session)
                    .is_some_and(|t| t.phase != sqlr_engine::TxPhase::Idle)
            {
                self.notify_as(
                    ToastKind::Info,
                    "Commit or roll back this tab's transaction before switching",
                );
                return;
            }
            self.engine.close_session(&session);
        }
        let Some(tab) = tab_mut(&mut self.dock, key) else {
            return;
        };
        tab.conn = conn;
        if let Some(db) = tab.database.take()
            && let Some(base) = tab.title.strip_suffix(&format!(" ({db})"))
        {
            tab.title = base.to_string();
        }
        tab.editor.set_syntax(syntax);
    }

    /// The focused leaf's active tab, else the first tab.
    pub(crate) fn active_key(&mut self) -> Option<u64> {
        if let Some((_, tab)) = self.dock.find_active_focused() {
            return Some(tab.key);
        }
        self.dock.iter_all_tabs().next().map(|(_, t)| t.key)
    }

    pub(crate) fn active_tab(&mut self) -> Option<&mut QueryTab> {
        let key = self.active_key()?;
        self.tab(key)
    }

    pub(crate) fn tab(&mut self, key: u64) -> Option<&mut QueryTab> {
        tab_mut(&mut self.dock, key)
    }
}

/// `cfg`'s profile among `profiles`; an unknown or missing one is `development`.
pub(crate) fn profile_of(
    profiles: &[ConnectionProfile],
    cfg: &ConnectionConfig,
) -> ConnectionProfile {
    let id = cfg
        .profile
        .as_deref()
        .unwrap_or(ConnectionProfile::DEVELOPMENT);
    profiles
        .iter()
        .find(|p| p.id == id)
        .or_else(|| {
            profiles
                .iter()
                .find(|p| p.id == ConnectionProfile::DEVELOPMENT)
        })
        .cloned()
        .unwrap_or_else(ConnectionProfile::development)
}

/// The tab with `key`; a free function so callers can hold other fields of the app.
pub(crate) fn tab_mut(dock: &mut DockState<QueryTab>, key: u64) -> Option<&mut QueryTab> {
    dock.iter_all_tabs_mut()
        .find(|(_, t)| t.key == key)
        .map(|(_, t)| t)
}

/// Opens a folder with the OS file manager: `open` on macOS, `explorer` on Windows and
/// `xdg-open` elsewhere. The command is spawned, never waited for: the UI thread goes on.
fn default_folder_opener() -> Arc<dyn Fn(&Path) + Send + Sync> {
    Arc::new(|path| {
        if let Err(e) = folder_command(path).spawn() {
            tracing::warn!(error = %e, "could not open the folder");
        }
    })
}

#[cfg(target_os = "macos")]
fn folder_command(path: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("open");
    cmd.arg(path);
    cmd
}

#[cfg(target_os = "windows")]
fn folder_command(path: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("explorer");
    cmd.arg(path);
    cmd
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn folder_command(path: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(path);
    cmd
}

#[cfg(test)]
mod tests {
    use sqlr_store::{MemorySecretStore, ProfileRepo};
    use sqlr_testkit::{FakeDriver, FakeScript};

    use super::*;

    #[test]
    fn production_registers_snowflake_but_not_duckdb() {
        let dir = tempfile::tempdir().unwrap();
        let deps = production_deps_in(
            Arc::new(|| {}),
            Some(dir.path().join("config")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        let manifest = deps
            .engine
            .manifest("snowflake")
            .expect("built-in Snowflake");
        assert_eq!(manifest.family, sqlr_core::Family::Relational);
        // DuckDB is a sidecar driver installed from a file, not part of the app.
        assert!(deps.engine.manifest("duckdb").is_none());
    }

    #[test]
    fn snowflake_completion_uses_the_saved_schema_before_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let deps = production_deps_in(
            Arc::new(|| {}),
            Some(dir.path().join("config")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        let cfg = ConnectionConfig {
            id: "snow".into(),
            name: "Warehouse".into(),
            driver: "snowflake".into(),
            group: None,
            color: None,
            values: [("schema".into(), "ANALYTICS".into())].into(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        let mut app = SqlrApp::new(deps, &egui::Context::default());
        app.apply_listing(Listing {
            connections: Ok(vec![cfg]),
            folders: Ok(vec![]),
        });
        assert_eq!(app.dialects["snow"].default_schemas(), vec!["ANALYTICS"]);
    }

    /// Another handle on the saved connections in `dir` (secrets in memory).
    fn saved_repo(dir: &tempfile::TempDir) -> ConnectionRepo {
        ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap()
    }

    /// Reads the saved connections again and waits for the store thread's answer.
    fn settle(app: &mut SqlrApp) {
        app.reload_connections();
        wait("the store", || {
            app.poll_store();
            !app.store.busy()
        });
    }

    #[test]
    fn driver_change_reapplies_tab_syntax() {
        let dir = tempfile::tempdir().unwrap();
        let mut drivers = DriverRegistry::default();
        drivers.register(FakeDriver::new(FakeScript::default()));
        drivers.register(PostgresDriver::new());
        let repo =
            ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
        let mut cfg = ConnectionConfig {
            id: "c1".into(),
            name: "db".into(),
            driver: "fake".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        repo.save(&cfg, &Secrets::default()).unwrap();
        let deps = AppDeps {
            check_updates: false,
            engine: Engine::new(drivers, Arc::new(|| {})),
            repo,
            settings: Settings::default(),
            config_dir: dir.path().to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
        };
        let mut app = SqlrApp::new(deps, &egui::Context::default());
        let on_c1 = app.new_tab(None, Some("c1".into()), "");
        let other = app.new_tab(None, None, "");
        assert!(!app.tab(on_c1).unwrap().editor.syntax().dollar_quotes);

        cfg.driver = "postgres".into();
        saved_repo(&dir).save(&cfg, &Secrets::default()).unwrap();
        settle(&mut app);
        assert!(
            app.tab(on_c1).unwrap().editor.syntax().dollar_quotes,
            "the tab on the edited connection uses the new driver's syntax"
        );
        assert!(!app.tab(other).unwrap().editor.syntax().dollar_quotes);
    }

    #[test]
    fn connection_color_wins_over_its_profile_color() {
        let dir = tempfile::tempdir().unwrap();
        let mut drivers = DriverRegistry::default();
        drivers.register(FakeDriver::new(FakeScript::default()));
        let repo =
            ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
        let mut cfg = ConnectionConfig {
            id: "c1".into(),
            name: "db".into(),
            driver: "fake".into(),
            group: None,
            color: Some([0x12, 0x34, 0x56]),
            values: Default::default(),
            profile: Some(ConnectionProfile::TEST.into()),
            save_secrets: true,
            agent_access: Default::default(),
        };
        repo.save(&cfg, &Secrets::default()).unwrap();
        let deps = AppDeps {
            check_updates: false,
            engine: Engine::new(drivers, Arc::new(|| {})),
            repo,
            settings: Settings::default(),
            config_dir: dir.path().to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
        };
        let mut app = SqlrApp::new(deps, &egui::Context::default());

        assert_eq!(app.connection_tint("c1"), Some([0x12, 0x34, 0x56]));

        cfg.color = None;
        saved_repo(&dir).save(&cfg, &Secrets::default()).unwrap();
        settle(&mut app);
        assert_eq!(
            app.connection_tint("c1"),
            Some([0xE0, 0xA0, 0x30]),
            "without its own color, the profile's is the fallback"
        );
    }

    /// A keychain that records the thread of every read.
    #[derive(Default)]
    struct Recording {
        inner: MemorySecretStore,
        reads: parking_lot::Mutex<Vec<String>>,
        writes: parking_lot::Mutex<Vec<String>>,
    }

    impl sqlr_store::SecretStore for Recording {
        fn get(&self, conn: &str, field: &str) -> Result<Option<String>, sqlr_store::StoreError> {
            let thread = std::thread::current().name().unwrap_or("?").to_string();
            self.reads.lock().push(thread);
            self.inner.get(conn, field)
        }
        fn set(&self, conn: &str, field: &str, value: &str) -> Result<(), sqlr_store::StoreError> {
            let thread = std::thread::current().name().unwrap_or("?").to_string();
            self.writes.lock().push(thread);
            self.inner.set(conn, field, value)
        }
        fn delete(&self, conn: &str, field: &str) -> Result<(), sqlr_store::StoreError> {
            let thread = std::thread::current().name().unwrap_or("?").to_string();
            self.writes.lock().push(thread);
            self.inner.delete(conn, field)
        }
    }

    /// An app on the fake driver whose saved connection "c1" (password `hunter2`) keeps its
    /// secrets in `keychain`.
    fn app_with_keychain(dir: &tempfile::TempDir, keychain: Arc<Recording>) -> SqlrApp {
        let mut drivers = DriverRegistry::default();
        drivers.register(FakeDriver::new(FakeScript::default()));
        let repo = ConnectionRepo::open(dir.path(), Box::new(keychain)).unwrap();
        let cfg = ConnectionConfig {
            id: "c1".into(),
            name: "db".into(),
            driver: "fake".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        let pw = Secrets([("password".to_string(), "hunter2".to_string())].into());
        repo.save(&cfg, &pw).unwrap();
        let deps = AppDeps {
            check_updates: false,
            engine: Engine::new(drivers, Arc::new(|| {})),
            repo,
            settings: Settings::default(),
            config_dir: dir.path().to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
        };
        SqlrApp::new(deps, &egui::Context::default())
    }

    #[test]
    fn connection_store_io_runs_off_the_ui_thread() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(Recording::default());
        let mut app = app_with_keychain(&dir, keychain.clone());
        let here = std::thread::current().name().unwrap_or("?").to_string();
        keychain.reads.lock().clear();
        keychain.writes.lock().clear();

        // Each call only queues work: the keychain is used on the store thread (checked by
        // thread name below), and the listing changes only when the app polls the answers.
        // Editing reads the secrets there.
        app.open_edit_connection("c1");
        assert!(app.conn_dialog.is_some());
        // Duplicating reads and writes the secrets there.
        app.duplicate_connection("c1");
        // Folder changes write `connections.toml` there too.
        app.create_folder("Team");
        assert!(!app.folders.iter().any(|f| f == "Team"), "listed in place");
        // Deleting removes the secrets there.
        app.conn_dialog = None;
        app.delete_connection("c1");

        wait("the store", || {
            app.poll_store();
            !app.store.busy()
        });
        assert!(app.folders.iter().any(|f| f == "Team"));
        assert!(app.connection("c1").is_none());
        assert!(app.connections.iter().any(|c| c.name == "db copy"));
        let reads = keychain.reads.lock().clone();
        let writes = keychain.writes.lock().clone();
        assert!(!reads.is_empty() && !writes.is_empty());
        assert!(
            reads.iter().chain(&writes).all(|t| *t != here),
            "{reads:?} {writes:?}"
        );
    }

    #[test]
    fn connection_editor_and_engine_share_one_vault_unlock() {
        use sqlr_store::SecretStore as _;

        let dir = tempfile::tempdir().unwrap();
        let keyring = Arc::new(Recording::default());
        let seed = sqlr_store::VaultSecretStore::new(dir.path(), keyring.clone());
        seed.set("c1", "password", "first-password").unwrap();
        seed.set("c2", "password", "second-password").unwrap();
        drop(seed);
        keyring.reads.lock().clear();
        let secrets = Arc::new(sqlr_store::VaultSecretStore::new(
            dir.path(),
            keyring.clone(),
        ));
        let repo = ConnectionRepo::open(dir.path(), Box::new(secrets.clone())).unwrap();
        for id in ["c1", "c2"] {
            repo.save(
                &ConnectionConfig {
                    id: id.into(),
                    name: id.into(),
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
        }
        let fake = FakeDriver::new(Default::default());
        let mut drivers = DriverRegistry::default();
        drivers.register(fake.clone());
        let engine = Engine::new(drivers, Arc::new(|| {}));
        engine.set_secret_store(secrets);
        let mut app = SqlrApp::new(
            AppDeps {
                check_updates: false,
                engine,
                repo,
                settings: Settings::default(),
                config_dir: dir.path().into(),
                data_dir: None,
                startup_notice: None,
                storage_warning: None,
                mcp_token_store: None,
                diagnostics: None,
                diagnostics_warning: None,
            },
            &egui::Context::default(),
        );
        assert!(keyring.reads.lock().is_empty(), "startup stays lazy");
        app.open_edit_connection("c1");
        wait("editor secrets", || {
            app.poll_store();
            !app.store.busy()
        });
        app.connect("c1");
        app.connect("c2");
        wait("both connections", || fake.connects().len() == 2);
        let reads = keyring.reads.lock();
        assert_eq!(
            reads.len(),
            1,
            "the editor and both connections share one key"
        );
        let here = std::thread::current().name().unwrap_or("?").to_string();
        assert_ne!(reads[0], here);
        let passwords: std::collections::BTreeSet<_> = fake
            .connects()
            .iter()
            .map(|(_, secrets)| secrets.0["password"].clone())
            .collect();
        assert_eq!(
            passwords,
            ["first-password".into(), "second-password".into()].into()
        );
    }

    #[test]
    fn soft_wrap_saves_the_settings_off_the_ui_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with_keychain(&dir, Arc::new(Recording::default()));
        let ctx = egui::Context::default();
        assert!(!app.soft_wrap_enabled());
        app.toggle_soft_wrap(&ctx);
        assert!(app.soft_wrap_enabled());
        assert!(app.settings_save.is_some(), "the save is queued, not done");
        wait("the settings save", || {
            app.poll_settings_save(&ctx);
            app.settings_save.is_none()
        });
        let (saved, _) = LayeredSettings::load(dir.path());
        assert_eq!(
            saved.get("editor.soft_wrap", None).0,
            sqlr_core::SettingValue::Bool(true)
        );
        // The UI reads the new value at once, before the save has landed.
        assert!(app.soft_wrap_enabled());
    }

    fn wait(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn connect_and_profiles_do_no_io_on_the_ui_thread() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeDriver::new(FakeScript::default());
        let mut drivers = DriverRegistry::default();
        drivers.register(fake.clone());
        let keychain = Arc::new(Recording::default());
        let repo = ConnectionRepo::open(dir.path(), Box::new(keychain.clone())).unwrap();
        let cfg = ConnectionConfig {
            id: "c1".into(),
            name: "db".into(),
            driver: "fake".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: Some("mine".into()),
            save_secrets: true,
            agent_access: Default::default(),
        };
        let pw = Secrets([("password".to_string(), "hunter2".to_string())].into());
        repo.save(&cfg, &pw).unwrap();
        let mine = ConnectionProfile {
            id: "mine".into(),
            name: "Mine".into(),
            color: Some([1, 2, 3]),
            ..ConnectionProfile::development()
        };
        ProfileRepo::open(dir.path()).save(&mine).unwrap();
        let engine = Engine::new(drivers, Arc::new(|| {}));
        engine.set_secret_store(keychain.clone());
        let deps = AppDeps {
            check_updates: false,
            engine,
            repo,
            settings: Settings::default(),
            config_dir: dir.path().to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
        };
        let mut app = SqlrApp::new(deps, &egui::Context::default());
        // The profiles arrive from the engine.
        wait("the profiles", || {
            app.sync_profiles();
            app.profiles.iter().any(|p| p.id == "mine")
        });
        assert_eq!(app.profile_for(&cfg).color, Some([1, 2, 3]));

        app.connect("c1");
        wait("the connect", || !fake.connects().is_empty());
        assert_eq!(fake.connects()[0].1.0["password"], "hunter2");
        let here = std::thread::current().name().unwrap_or("?").to_string();
        let reads = keychain.reads.lock().clone();
        assert!(
            !reads.is_empty() && reads.iter().all(|t| *t != here),
            "{reads:?}"
        );
    }

    #[test]
    fn saving_a_live_connection_reconnects_it_with_the_new_config() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeDriver::new(FakeScript::default());
        let mut drivers = DriverRegistry::default();
        drivers.register(fake.clone());
        let repo =
            ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
        let cfg = ConnectionConfig {
            id: "c1".into(),
            name: "old".into(),
            driver: "fake".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        repo.save(&cfg, &Secrets::default()).unwrap();
        let deps = AppDeps {
            check_updates: false,
            engine: Engine::new(drivers, Arc::new(|| {})),
            repo,
            settings: Settings::default(),
            config_dir: dir.path().to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
        };
        let mut app = SqlrApp::new(deps, &egui::Context::default());
        app.connect("c1");
        wait("the first connect", || fake.connects().len() == 1);

        let mut edited = cfg;
        edited.name = "renamed".into();
        saved_repo(&dir).save(&edited, &Secrets::default()).unwrap();
        app.connection_saved("c1");

        wait("the reconnect", || {
            app.poll_store();
            fake.connects().len() == 2
        });
        assert_eq!(fake.connects()[1].0.name, "renamed");
        assert_eq!(app.connection("c1").unwrap().name, "renamed");
    }
}
