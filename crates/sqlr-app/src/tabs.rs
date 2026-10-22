//! Query tabs: a `QueryEditor` above the tab's run (one statement or a script), inside the
//! dock area, and the app's run, run-script and stop actions.

use sqlr_ui::ToastKind;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Id, RichText, Stroke, WidgetText};
use sqlr_core::{ConnectionConfig, Dialect, DriverError, SqlSyntax};
use sqlr_engine::{Engine, QueryId, QueryStatus, ScriptId, ScriptOnError, SessionKey};
use sqlr_ui::theme::{self, Palette, Spacing, icons};
use sqlr_ui::{
    ChangesAction, EditorState, GridAction, GridInput, GridState, Marker, ResultBarAction,
    ResultBarView, RunRequest, data_grid, query_editor, result_bar, result_bar_frame,
};

use crate::app::{SqlrApp, tab_mut};
use crate::catalog_source::TabCatalog;
use crate::confirm::PendingKind;
use crate::messages::messages_view;
use crate::objects::ObjectTab;

/// What a tab last ran.
pub enum TabRun {
    /// One statement; `origin` is the buffer char offset of its first char (`None` for SQL
    /// that is not in the buffer, like a server-side sort or filter of it).
    Single { q: QueryId, origin: Option<usize> },
    /// A script; `origins[i]` is where statement `i` starts in the buffer.
    Script {
        id: ScriptId,
        origins: Vec<usize>,
        pane: ScriptPane,
    },
}

/// The part of a script's run shown below the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptPane {
    /// The script's `n`th rows result (by order of arrival).
    Result(usize),
    Output,
    Messages,
}

/// The part of a single statement's run shown below the editor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SinglePane {
    #[default]
    Result,
    Messages,
}

/// The editing state of one result: the table its rows come from, why it is read-only,
/// and the changeset being applied.
#[derive(Clone, Debug, Default)]
pub struct ResultEdit {
    pub table: Option<sqlr_core::TableRef>,
    /// Why the rows cannot be edited; `None` while editable or still being worked out.
    pub read_only: Option<String>,
    pub applying: Option<sqlr_engine::JobId>,
}

/// A server-side sort or filter of the statement the tab ran (§7.5).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerView {
    /// The statement as the user ran it.
    pub base: String,
    /// `(column index, column name, descending)`.
    pub order: Vec<(usize, String, bool)>,
    pub filter: String,
}

pub struct QueryTab {
    /// Unique for the app's lifetime; the dock and the widgets inside key on it.
    pub key: u64,
    pub title: String,
    pub conn: Option<String>,
    /// The server database this tab targets, when it is not the connection's own.
    pub database: Option<String>,
    pub editor: EditorState,
    pub run: Option<TabRun>,
    /// Grid state per result of the run.
    pub grids: HashMap<QueryId, GridState>,
    /// Editing state per result of the run.
    pub edits: HashMap<QueryId, ResultEdit>,
    /// The pane of a single statement's run.
    pub pane: SinglePane,
    /// The server-side sort and filter the current run applies.
    pub server: Option<ServerView>,
    /// A cell edited in the value panel.
    pub value_edit: Option<(QueryId, sqlr_ui::ValueEdit)>,
    /// Rows per fetch for this tab's results.
    pub page_size: usize,
    /// Error sources of the run already turned into markers: 1 once a failed single query
    /// was marked, else the script output lines read.
    marked: usize,
    /// Per statement of the run (0 for a single query), the 1-based line and column its
    /// server error pointed at in the buffer when it arrived. Later edits don't move it.
    error_at: HashMap<usize, (usize, usize)>,
    /// The Redis browser this tab shows in place of an editor.
    pub redis: Option<Box<crate::nosql::RedisTab>>,
    /// Set when the tab is an object editor instead of a query editor.
    pub object: Option<Box<ObjectTab>>,
    /// Set when the tab runs document statements (MongoDB, Elasticsearch).
    pub doc: Option<Box<crate::nosql::DocTab>>,
    /// Set when the tab is an administration panel.
    pub admin: Option<Box<crate::admin::AdminTab>>,
    /// Set when the tab shows an object's DDL instead of an editor.
    pub ddl: Option<Box<crate::ddl_tab::DdlTab>>,
}

impl QueryTab {
    pub fn new(
        key: u64,
        title: String,
        conn: Option<String>,
        sql: &str,
        syntax: Arc<SqlSyntax>,
        page_size: usize,
    ) -> Self {
        Self {
            key,
            title,
            conn,
            database: None,
            editor: EditorState::new(sql, syntax),
            run: None,
            grids: HashMap::new(),
            edits: HashMap::new(),
            pane: SinglePane::Result,
            server: None,
            value_edit: None,
            page_size,
            marked: 0,
            error_at: HashMap::new(),
            redis: None,
            object: None,
            doc: None,
            admin: None,
            ddl: None,
        }
    }

    /// The result the tab shows: the single query, or the selected script result.
    pub fn shown_query(&self, engine: &Engine) -> Option<QueryId> {
        match self.run.as_ref()? {
            TabRun::Single { q, .. } => Some(*q),
            TabRun::Script { id, pane, .. } => {
                let ScriptPane::Result(n) = pane else {
                    return None;
                };
                let script = engine.script(*id)?;
                let s = script.read();
                s.results.get(*n).map(|(_, q)| *q)
            }
        }
    }

    pub fn is_running(&self, engine: &Engine) -> bool {
        match &self.run {
            None => false,
            Some(TabRun::Single { q, .. }) => engine
                .result(*q)
                .is_some_and(|r| r.read().status == QueryStatus::Running),
            Some(TabRun::Script { id, .. }) => engine.script(*id).is_some_and(|s| !s.read().done),
        }
    }

    /// Interrupts the run: the running script statement, or the query.
    pub fn stop(&self, engine: &Engine) {
        match &self.run {
            None => {}
            Some(TabRun::Single { q, .. }) => engine.cancel(*q),
            Some(TabRun::Script { id, .. }) => engine.cancel_script(*id),
        }
    }

    /// Closes the previous run's results and forgets its grids and markers.
    pub fn close_run(&mut self, engine: &Engine) {
        match self.run.take() {
            None => {}
            Some(TabRun::Single { q, .. }) => engine.close_result(q),
            Some(TabRun::Script { id, .. }) => engine.close_script(id),
        }
        self.grids.clear();
        self.edits.clear();
        self.value_edit = None;
        self.marked = 0;
        self.error_at.clear();
        self.editor.set_markers(Vec::new());
    }

    /// The row edits staged in the run's grids and not applied yet: `close_run` drops them.
    pub fn grid_pending(&self) -> usize {
        self.grids
            .values()
            .filter_map(|g| g.edits.as_ref())
            .map(|e| e.pending())
            .sum()
    }

    /// The session this tab runs on, on `conn`.
    pub fn session(&self, engine: &Engine, conn: &str) -> SessionKey {
        engine.session(conn, Some(self.key))
    }

    /// Runs one statement on `conn` in place of the previous run.
    pub fn run_single(&mut self, engine: &Engine, conn: &str, target: RunRequest) {
        self.run_sql(engine, conn, target.sql, Some(target.origin));
    }

    /// Runs `sql` as the tab's single statement; `origin` locates it in the buffer.
    pub fn run_sql(
        &mut self,
        engine: &Engine,
        conn: &str,
        sql: String,
        origin: Option<usize>,
    ) -> QueryId {
        self.close_run(engine);
        let key = self.session(engine, conn);
        let q = engine.execute_in(&key, self.database.as_deref(), sql, self.page_size);
        self.run = Some(TabRun::Single { q, origin });
        q
    }

    /// Runs an object's Data page (`sql`, a read) in place of the previous run: it opens no
    /// Manual transaction, so the editor's own applies are not refused for it.
    pub fn run_page(&mut self, engine: &Engine, conn: &str, sql: String) -> QueryId {
        self.close_run(engine);
        let key = self.session(engine, conn);
        let q = engine.read_in(&key, self.database.as_deref(), sql, self.page_size);
        self.run = Some(TabRun::Single { q, origin: None });
        q
    }

    /// Runs `targets` as a script on `conn` in place of the previous run.
    pub fn run_script(
        &mut self,
        engine: &Engine,
        conn: &str,
        targets: Vec<RunRequest>,
        on_error: ScriptOnError,
    ) {
        self.close_run(engine);
        let origins = targets.iter().map(|t| t.origin).collect();
        let statements = targets.into_iter().map(|t| t.sql).collect();
        let id = engine.execute_script_in(
            &self.session(engine, conn),
            self.database.as_deref(),
            statements,
            self.page_size,
            on_error,
        );
        self.run = Some(TabRun::Script {
            id,
            origins,
            pane: ScriptPane::Result(0),
        });
    }

    /// Turns new server error positions of the run into editor markers.
    fn update_markers(&mut self, engine: &Engine) {
        let mut new = Vec::new();
        match &self.run {
            None => return,
            Some(TabRun::Single { q, origin }) => {
                if self.marked > 0 {
                    return;
                }
                let Some(origin) = *origin else {
                    return;
                };
                let Some(result) = engine.result(*q) else {
                    return;
                };
                let r = result.read();
                if let QueryStatus::Failed(e) = &r.status {
                    self.marked = 1;
                    if let Some(pos) = e.position {
                        let at = self.line_col(origin + pos);
                        self.error_at.insert(0, at);
                        new.push(Marker {
                            at: origin + pos,
                            message: e.message.clone(),
                        });
                    }
                }
            }
            Some(TabRun::Script { id, origins, .. }) => {
                let Some(script) = engine.script(*id) else {
                    return;
                };
                let s = script.read();
                for line in s.output.iter().skip(self.marked) {
                    if let Some(e) = &line.error
                        && let (Some(pos), Some(origin)) = (e.position, origins.get(line.statement))
                    {
                        let at = self.line_col(origin + pos);
                        self.error_at.insert(line.statement, at);
                        new.push(Marker {
                            at: origin + pos,
                            message: e.message.clone(),
                        });
                    }
                }
                self.marked = s.output.len();
                let results = s.results.clone();
                drop(s);
                // A rows result can also fail later, on a fetch: locate that error too.
                for (statement, q) in results {
                    if self.error_at.contains_key(&statement) {
                        continue;
                    }
                    let Some(result) = engine.result(q) else {
                        continue;
                    };
                    let r = result.read();
                    if let QueryStatus::Failed(e) = &r.status
                        && let (Some(pos), Some(origin)) = (e.position, origins.get(statement))
                    {
                        let at = self.line_col(origin + pos);
                        self.error_at.insert(statement, at);
                    }
                }
            }
        }
        if !new.is_empty() {
            let mut markers = self.editor.markers().to_vec();
            markers.extend(new);
            self.editor.set_markers(markers);
        }
    }

    /// The 1-based line and column of the server error of the run's statement `statement`
    /// (0 for a single query), once it has arrived.
    fn error_at(&self, statement: usize) -> Option<(usize, usize)> {
        self.error_at.get(&statement).copied()
    }

    /// 1-based line and column of buffer char offset `at`.
    fn line_col(&self, at: usize) -> (usize, usize) {
        let p = self.editor.model().buffer().pos_of_char(at);
        (p.line + 1, p.col + 1)
    }
}

/// Work the dock area asks for; applied once the dock is drawn (and no result guard is held).
pub enum TabCommand {
    FetchMore(QueryId),
    /// The changes bar of a result.
    Changes {
        tab: u64,
        q: QueryId,
        action: ChangesAction,
    },
    /// An action of a result's grid.
    Grid {
        tab: u64,
        q: QueryId,
        action: GridAction,
    },
    /// "Export…" in a result's status bar.
    Export {
        tab: u64,
        q: QueryId,
    },
    /// The tab's close button: the app asks first when its transaction is open.
    RequestClose(u64),
    /// The breadcrumb's connection or database: select the tab's connection in the explorer.
    RevealConnection(u64),
    Add,
    /// A change from a Redis tab.
    Redis {
        tab: u64,
        command: crate::nosql::RedisCommand,
    },
    /// An object tab's editor action.
    Object {
        key: u64,
        action: sqlr_ui::ObjectEditorAction,
    },
    /// A run or an edit from a document tab.
    Doc {
        tab: u64,
        command: crate::nosql::DocCommand,
    },
    /// An object tab's "Apply grants" (Permissions).
    ApplyGrants(u64),
    /// An object tab's Data page is stale but holds row edits: ask before dropping them.
    DiscardData(u64),
    /// A DDL tab's toolbar action.
    Ddl {
        key: u64,
        action: crate::ddl_tab::DdlAction,
    },
    /// An administration tab's command.
    Admin {
        key: u64,
        command: crate::admin::AdminCommand,
    },
}

pub struct Viewer<'a> {
    pub engine: &'a Engine,
    /// Dialects of the saved connections, by connection id.
    pub dialects: &'a HashMap<String, Arc<dyn Dialect>>,
    pub connections: &'a [ConnectionConfig],
    /// Connections whose driver edits results.
    pub editable: &'a HashSet<String>,
    /// Profile colors of the saved connections that have one, by connection id.
    pub tints: &'a HashMap<String, [u8; 3]>,
    /// Built-in and user connection profiles.
    pub profiles: &'a [sqlr_core::ConnectionProfile],
    /// The grids' display formats, per connection.
    pub formats: &'a crate::prefs::Formats,
    /// The keys of each leaf's active tab (their titles are underlined).
    pub active: HashSet<u64>,
    pub palette: Palette,
    /// The icon font of tab titles.
    pub icon_font: FontId,
    pub commands: Vec<TabCommand>,
}

/// The database a tab's statements run on, when it is not the connection's own database
/// (`own_database`, when the session names one).
pub(crate) fn target_database<'a>(
    tab_database: Option<&'a str>,
    own_database: Option<&str>,
) -> Option<&'a str> {
    tab_database.filter(|db| own_database != Some(*db))
}

impl egui_dock::TabViewer for Viewer<'_> {
    type Tab = QueryTab;

    fn id(&mut self, tab: &mut QueryTab) -> Id {
        Id::new(("query_tab", tab.key))
    }

    /// A stripe in the profile's color (when it has one), a database icon in the connection's
    /// color, then the title (underlined in the accent on the active tab; the tab's text
    /// color applies to it).
    fn title(&mut self, tab: &mut QueryTab) -> WidgetText {
        let p = self.palette;
        let conn = tab
            .conn
            .as_ref()
            .and_then(|id| self.connections.iter().find(|c| &c.id == id));
        let tint = tab.conn.as_ref().and_then(|id| self.tints.get(id)).copied();
        let icon_color = match conn {
            Some(c) => tint
                .or(c.color)
                .map_or(p.icon_table, |[r, g, b]| Color32::from_rgb(r, g, b)),
            None => p.text_disabled,
        };
        let font = FontId::proportional(13.0);
        // An object tab shows its kind's icon, its name and its pending changes.
        let (icon, icon_color, badge) = match tab.object.as_deref_mut() {
            Some(object) => {
                tab.title = object.title();
                let kind = object
                    .state
                    .as_ref()
                    .map_or(object.object.kind, |s| s.kind());
                let (icon, color) = sqlr_ui::kind_icon(kind);
                let pending = object.pending();
                (
                    icon,
                    color(&p),
                    (pending > 0).then(|| format!(" ● {pending}")),
                )
            }
            None => match tab.admin.as_deref() {
                Some(admin) => {
                    let icon = match admin.panel() {
                        sqlr_ui::AdminPanel::Users => icons::KEY,
                        sqlr_ui::AdminPanel::Sessions => icons::CONNECTION,
                        sqlr_ui::AdminPanel::Server => icons::SETTINGS,
                    };
                    (icon, icon_color, None)
                }
                None if tab.ddl.is_some() => (icons::SQL, icon_color, None),
                None => (icons::DATABASE, icon_color, None),
            },
        };
        let mut job = LayoutJob::default();
        if let Some([r, g, b]) = tint {
            job.append(
                "▍",
                0.0,
                TextFormat::simple(font.clone(), Color32::from_rgb(r, g, b)),
            );
        }
        job.append(
            icon,
            0.0,
            TextFormat::simple(self.icon_font.clone(), icon_color),
        );
        let underline = if self.active.contains(&tab.key) {
            Stroke::new(2.0, p.accent)
        } else {
            Stroke::NONE
        };
        job.append(
            &tab.title,
            Spacing::S + 2.0,
            TextFormat {
                underline,
                ..TextFormat::simple(font.clone(), Color32::PLACEHOLDER)
            },
        );
        if let Some(badge) = badge {
            job.append(&badge, 0.0, TextFormat::simple(font, p.warning));
        }
        job.into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut QueryTab) {
        // The grids below read the formats of this tab's connection.
        let formats = self.formats.for_conn(tab.conn.as_deref());
        sqlr_ui::set_data_formats(ui.ctx(), formats.clone());
        if let (Some(redis), Some(conn)) = (tab.redis.as_deref_mut(), tab.conn.as_deref()) {
            crate::nosql::redis_ui(ui, tab.key, conn, redis, self.engine, &mut self.commands);
            return;
        }
        if tab.object.is_some() {
            self.object_ui(ui, tab);
            return;
        }
        if let Some(doc) = tab.doc.as_deref_mut() {
            let commands = &mut self.commands;
            crate::nosql::doc_ui(ui, tab.key, &mut tab.editor, doc, self.engine, commands);
            return;
        }
        if let Some(ddl) = tab.ddl.as_deref_mut() {
            let syntax = self
                .dialects
                .get(&ddl.conn)
                .map_or_else(SqlSyntax::ansi, |d| d.syntax());
            if let Some(action) = ddl.show(ui, self.engine, &syntax) {
                self.commands.push(TabCommand::Ddl {
                    key: tab.key,
                    action,
                });
            }
            return;
        }
        if let Some(admin) = tab.admin.as_deref_mut() {
            let dialect = self.dialects.get(admin.conn()).map(|d| &**d);
            if let Some(command) = admin.show(ui, self.engine, dialect, tab.key) {
                self.commands.push(TabCommand::Admin {
                    key: tab.key,
                    command,
                });
            }
            return;
        }
        tab.update_markers(self.engine);
        let conn = tab.conn.as_deref();
        // The database the tab's statements run on, when it is not the connection's own.
        let own = conn.and_then(|c| self.engine.session_database(c));
        let database = target_database(tab.database.as_deref(), own.as_deref());
        let dialect = tab.conn.as_ref().and_then(|c| self.dialects.get(c));
        let catalog = TabCatalog::new(self.engine, conn, database, dialect.map(|d| &**d));
        if let Some(conn) = conn {
            catalog.prefetch(self.engine, conn, database);
        }
        egui::Panel::top(Id::new(("breadcrumb", tab.key)))
            .exact_size(Spacing::BREADCRUMB)
            .frame(
                egui::Frame::NONE
                    .fill(self.palette.bg_editor)
                    .inner_margin(egui::Margin::symmetric(Spacing::M as i8, 0)),
            )
            .show(ui, |ui| {
                let cfg = conn.and_then(|c| self.connections.iter().find(|x| x.id == c));
                let name = cfg.map(|c| c.name.as_str());
                let mode = cfg.map(|c| crate::app::profile_of(self.profiles, c).commit_mode);
                if crate::shell::tab_breadcrumb(ui, self.engine, tab, name, mode) {
                    self.commands.push(TabCommand::RevealConnection(tab.key));
                }
            });
        // The results get the larger share by default; the editor can still be dragged
        // taller.
        let editor_height = theme::editor_split_height(ui.available_height());
        egui::Panel::top(Id::new(("editor_split", tab.key)))
            .resizable(true)
            .default_size(editor_height)
            .min_size(60.0)
            .frame(egui::Frame::NONE.fill(self.palette.bg_editor))
            .show(ui, |ui| {
                let output = query_editor(ui, &mut tab.editor, &catalog);
                if let Some(conn) = conn {
                    for need in output.needs {
                        self.engine.load_catalog_in(conn, database, need);
                    }
                }
            });
        let mut cx = ViewCx {
            engine: self.engine,
            dialect: dialect.map(|d| &**d),
            editable: tab.conn.as_ref().is_some_and(|c| self.editable.contains(c)),
            commands: &mut self.commands,
        };
        run_view(ui, tab, &mut cx);
    }

    /// The app closes the tab itself (after asking, when its transaction is open).
    fn on_close(&mut self, tab: &mut QueryTab) -> egui_dock::tab_viewer::OnCloseResponse {
        self.commands.push(TabCommand::RequestClose(tab.key));
        egui_dock::tab_viewer::OnCloseResponse::Ignore
    }

    fn on_add(&mut self, _path: egui_dock::NodePath) {
        self.commands.push(TabCommand::Add);
    }

    /// The editor and the grid scroll themselves.
    fn scroll_bars(&self, _tab: &QueryTab) -> [bool; 2] {
        [false, false]
    }
}

impl Viewer<'_> {
    /// An object tab: the object editor, and below it, on the Data sub-tab, the tab's run
    /// (the object's first page) drawn like any result, with the same grid editing.
    fn object_ui(&mut self, ui: &mut egui::Ui, tab: &mut QueryTab) {
        let Some(object) = tab.object.as_deref_mut() else {
            return;
        };
        let conn = object.conn.clone();
        let dialect = self.dialects.get(&conn);
        let syntax = dialect.map_or_else(SqlSyntax::ansi, |d| d.syntax());
        let action = object.show(ui, self.engine, syntax);
        if object.take_apply_grants() {
            self.commands.push(TabCommand::ApplyGrants(tab.key));
        }
        let (stale, data) = (object.take_data_stale(), object.data_shown());
        if let Some(action) = action {
            self.commands.push(TabCommand::Object {
                key: tab.key,
                action,
            });
        }
        if stale {
            if tab.grid_pending() > 0 {
                // The app asks before the Data page's row edits are dropped.
                self.commands.push(TabCommand::DiscardData(tab.key));
            } else {
                tab.close_run(self.engine);
            }
        }
        if !data {
            return;
        }
        let mut cx = ViewCx {
            engine: self.engine,
            dialect: dialect.map(|d| &**d),
            editable: self.editable.contains(&conn),
            commands: &mut self.commands,
        };
        match &tab.run {
            Some(TabRun::Single { q, .. }) => {
                let q = *q;
                result_view(ui, tab, &mut cx, q, 0);
            }
            _ => hint(ui, "Loading…"),
        }
    }
}

fn run_view(ui: &mut egui::Ui, tab: &mut QueryTab, cx: &mut ViewCx<'_>) {
    match &tab.run {
        None => hint(
            ui,
            "Run the statement at the caret with ⌘↵, or the script with ⇧⌘↵",
        ),
        Some(TabRun::Single { q, .. }) => {
            let q = *q;
            single_view(ui, tab, cx, q);
        }
        Some(TabRun::Script { id, pane, .. }) => {
            let (id, pane) = (*id, *pane);
            script_view(ui, tab, cx, id, pane);
        }
    }
}

/// What a tab's run view needs besides the tab.
pub(crate) struct ViewCx<'a> {
    pub engine: &'a Engine,
    /// The tab connection's dialect.
    pub dialect: Option<&'a dyn Dialect>,
    /// The tab connection's driver edits results.
    pub editable: bool,
    pub commands: &'a mut Vec<TabCommand>,
}

/// The strip of panes above a run: `labels` as selectable tabs (returns the one clicked).
fn pane_strip(
    ui: &mut egui::Ui,
    labels: &[(String, bool)],
    extra: impl FnOnce(&mut egui::Ui),
) -> Option<usize> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        for (i, (label, selected)) in labels.iter().enumerate() {
            if ui.selectable_label(*selected, label).clicked() {
                picked = Some(i);
            }
        }
        extra(ui);
    });
    picked
}

fn single_view(ui: &mut egui::Ui, tab: &mut QueryTab, cx: &mut ViewCx<'_>, q: QueryId) {
    let labels = [
        ("Result 1".to_string(), tab.pane == SinglePane::Result),
        ("Messages".to_string(), tab.pane == SinglePane::Messages),
    ];
    let picked = pane_strip(ui, &labels, |_| {});
    match picked {
        Some(0) => tab.pane = SinglePane::Result,
        Some(1) => tab.pane = SinglePane::Messages,
        _ => {}
    }
    ui.separator();
    match tab.pane {
        SinglePane::Result => result_view(ui, tab, cx, q, 0),
        SinglePane::Messages => {
            let messages = cx
                .engine
                .result(q)
                .map(|r| r.read().messages.clone())
                .unwrap_or_default();
            messages_view(ui, ("messages", tab.key), &messages);
        }
    }
}

fn script_view(
    ui: &mut egui::Ui,
    tab: &mut QueryTab,
    cx: &mut ViewCx<'_>,
    id: ScriptId,
    pane: ScriptPane,
) {
    let engine = cx.engine;
    let Some(script) = engine.script(id) else {
        hint(ui, "Script closed");
        return;
    };
    // Copied out so no script guard is held while a result is drawn (that calls the engine).
    let (results, running) = {
        let s = script.read();
        let results: Vec<(usize, QueryId)> = s.results.clone();
        (results, s.running)
    };
    let muted = theme::palette(ui.ctx()).text_muted;
    let shown = match pane {
        ScriptPane::Result(n) if n < results.len() => ScriptPane::Result(n),
        ScriptPane::Messages => ScriptPane::Messages,
        _ => ScriptPane::Output,
    };
    let mut panes: Vec<ScriptPane> = (0..results.len()).map(ScriptPane::Result).collect();
    panes.push(ScriptPane::Output);
    panes.push(ScriptPane::Messages);
    let labels: Vec<(String, bool)> = panes
        .iter()
        .map(|p| {
            let label = match p {
                ScriptPane::Result(n) => format!("Result {}", n + 1),
                ScriptPane::Output => "Output".to_string(),
                ScriptPane::Messages => "Messages".to_string(),
            };
            (label, *p == shown)
        })
        .collect();
    let picked = pane_strip(ui, &labels, |ui| {
        if let Some(n) = running {
            ui.spinner();
            ui.label(RichText::new(format!("statement {}", n + 1)).color(muted));
        }
    })
    .map(|i| panes[i]);
    if let Some(p) = picked
        && let Some(TabRun::Script { pane, .. }) = &mut tab.run
    {
        *pane = p;
    }
    ui.separator();
    match picked.unwrap_or(shown) {
        ScriptPane::Result(n) if n < results.len() => {
            let (statement, q) = results[n];
            result_view(ui, tab, cx, q, statement);
        }
        ScriptPane::Messages => {
            let messages = script.read().messages.clone();
            messages_view(ui, ("messages", tab.key), &messages);
        }
        _ => {
            // Drawing the log calls no engine method, so the guard may be held.
            let s = script.read();
            let row_height = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical()
                .id_salt(("script_output", tab.key))
                .auto_shrink(false)
                .show_rows(ui, row_height, s.output.len(), |ui, range| {
                    for line in &s.output[range] {
                        if line.error.is_none() {
                            ui.label(&line.text);
                            continue;
                        }
                        let at = tab.error_at(line.statement);
                        let text = match at {
                            Some((l, c)) => format!("{} (at line {l}, column {c})", line.text),
                            None => line.text.clone(),
                        };
                        ui.colored_label(theme::palette(ui.ctx()).error, text);
                    }
                });
        }
    }
}

/// One result: its edit strip and grid, row count, error or progress. `statement` is its
/// index in the run (0 for a single query), which locates its error in the buffer.
fn result_view(
    ui: &mut egui::Ui,
    tab: &mut QueryTab,
    cx: &mut ViewCx<'_>,
    q: QueryId,
    statement: usize,
) {
    let engine = cx.engine;
    if engine.result(q).is_none() {
        hint(ui, "Result closed");
        return;
    }
    // Before the result guard is taken: this may ask the engine for a table description.
    crate::results::refresh_editability(tab, engine, q, cx.editable);
    let Some(result) = engine.result(q) else {
        hint(ui, "Result closed");
        return;
    };
    // The guard lives only for this frame's drawing; engine calls are deferred.
    let r = result.read();
    match &r.status {
        QueryStatus::Failed(e) => {
            let at = tab.error_at(statement);
            error_panel(ui, e, at);
        }
        status => {
            if let Some(n) = r.affected {
                ui.add_space(Spacing::M);
                ui.label(format!("{n} rows affected · {} ms", r.elapsed.as_millis()));
            } else if *status == QueryStatus::Running && r.rows.row_count() == 0 {
                ui.add_space(Spacing::M);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Running…");
                });
            } else if *status == QueryStatus::Cancelled && r.rows.columns().is_empty() {
                hint(ui, "Cancelled");
            } else {
                let more = *status == QueryStatus::Ready { more: true };
                let edit = tab.edits.get(&q).cloned().unwrap_or_default();
                let grid = tab.grids.entry(q).or_default();
                let applying = edit.applying.is_some();
                let view = ResultBarView {
                    pending: grid.edits.as_ref().map_or(0, |e| e.pending()),
                    applying,
                    editable: grid.edits.is_some(),
                    exportable: !r.rows.columns().is_empty(),
                    read_only: grid
                        .edits
                        .is_none()
                        .then_some(edit.read_only.as_deref())
                        .flatten(),
                };
                let bar = egui::Panel::bottom(egui::Id::new(("result_bar", tab.key, q)))
                    .exact_size(Spacing::STATUS)
                    .frame(result_bar_frame(ui.ctx()))
                    .show(ui, |ui| result_bar(ui, &view))
                    .inner;
                match bar {
                    Some(ResultBarAction::Changes(action)) => {
                        cx.commands.push(TabCommand::Changes {
                            tab: tab.key,
                            q,
                            action,
                        });
                    }
                    Some(ResultBarAction::AddRow) => grid.add_row(),
                    Some(ResultBarAction::Export) => {
                        cx.commands.push(TabCommand::Export { tab: tab.key, q });
                    }
                    None => {}
                }
                let table = edit.table.as_ref().map(|t| t.name.as_str());
                let input = GridInput {
                    rows: &r.rows,
                    more_available: more,
                    dialect: cx.dialect,
                    table,
                };
                let output = data_grid(ui, grid, &input);
                if output.need_more {
                    cx.commands.push(TabCommand::FetchMore(q));
                }
                for action in grid.take_actions() {
                    cx.commands.push(TabCommand::Grid {
                        tab: tab.key,
                        q,
                        action,
                    });
                }
            }
        }
    }
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.add_space(Spacing::M);
    ui.horizontal(|ui| {
        ui.add_space(Spacing::M);
        ui.label(RichText::new(text).color(theme::palette(ui.ctx()).text_muted));
    });
}

/// `at` is the 1-based line and column of the error position, in the editor's buffer.
fn error_panel(ui: &mut egui::Ui, e: &DriverError, at: Option<(usize, usize)>) {
    ui.add_space(Spacing::M);
    ui.colored_label(theme::palette(ui.ctx()).error, &e.message);
    if let Some(hint) = &e.hint {
        ui.label(format!("Hint: {hint}"));
    }
    if let Some((line, column)) = at {
        ui.label(format!("at line {line}, column {column}"));
    }
}

impl SqlrApp {
    /// Opens `sql` in a new tab on `conn` (and `database`) and runs it.
    pub(crate) fn open_and_run(
        &mut self,
        title: String,
        conn: &str,
        database: Option<String>,
        sql: &str,
    ) {
        let key = self.new_tab(Some(title), Some(conn.to_string()), sql);
        if let Some(tab) = self.tab(key) {
            tab.database = database;
        }
        self.run_tab(key);
    }

    pub(crate) fn run_active(&mut self) {
        if let Some(key) = self.active_key() {
            self.run_tab(key);
        }
    }

    pub(crate) fn run_script_active(&mut self) {
        let Some(key) = self.active_key() else {
            return;
        };
        if self
            .tab(key)
            .is_some_and(|t| t.redis.is_some() || t.ddl.is_some())
        {
            return;
        }
        if self.tab(key).is_some_and(|t| t.doc.is_some()) {
            self.run_doc_tab(key, true);
            return;
        }
        let Some(conn) = self.runnable_conn(key) else {
            return;
        };
        let targets: Vec<RunRequest> = self
            .tab(key)
            .map(|t| t.editor.script_targets())
            .unwrap_or_default()
            .into_iter()
            .filter_map(trimmed)
            .collect();
        if targets.is_empty() {
            return;
        }
        let kind = PendingKind::Script(targets);
        match self.needs_confirmation(key, &conn, &kind) {
            Some(pending) => self.confirm = Some(pending),
            None => self.start_kind(key, &conn, kind),
        }
    }

    /// Starts a confirmed run of `kind` on tab `key`.
    pub(crate) fn start_kind(&mut self, key: u64, conn: &str, kind: PendingKind) {
        match kind {
            PendingKind::Single(target) => self.start_single(key, conn, target),
            PendingKind::Script(targets) => self.start_script(key, conn, targets),
            PendingKind::Redis(command) => self.apply_redis(command),
            PendingKind::Doc(command) => self.apply_doc(key, command),
        }
    }

    /// Runs `targets` as a script on tab `key` (connecting first if needed).
    pub(crate) fn start_script(&mut self, key: u64, conn: &str, targets: Vec<RunRequest>) {
        if !self.start_run(conn) {
            self.resume_after_secrets(key, conn, PendingKind::Script(targets));
            return;
        }
        let stop = self
            .setting_for("general.stop_script_on_error", Some(conn))
            .as_bool();
        let on_error = if stop {
            ScriptOnError::Stop
        } else {
            ScriptOnError::Continue
        };
        if let Some(tab) = tab_mut(&mut self.dock, key) {
            tab.server = None;
            tab.run_script(&self.engine, conn, targets, on_error);
        }
    }

    /// Runs one statement on tab `key` (connecting first if needed).
    pub(crate) fn start_single(&mut self, key: u64, conn: &str, target: RunRequest) {
        if !self.start_run(conn) {
            self.resume_after_secrets(key, conn, PendingKind::Single(target));
            return;
        }
        if let Some(tab) = tab_mut(&mut self.dock, key) {
            tab.server = None;
            tab.run_single(&self.engine, conn, target);
        }
    }

    pub(crate) fn stop_active(&mut self) {
        if let Some(key) = self.active_key()
            && let Some(tab) = tab_mut(&mut self.dock, key)
        {
            tab.stop(&self.engine);
        }
    }

    pub(crate) fn run_tab(&mut self, key: u64) {
        // A Redis or DDL tab has no editor to run.
        if self
            .tab(key)
            .is_some_and(|t| t.redis.is_some() || t.ddl.is_some())
        {
            return;
        }
        if self.tab(key).is_some_and(|t| t.doc.is_some()) {
            self.run_doc_tab(key, false);
            return;
        }
        let Some(conn) = self.runnable_conn(key) else {
            return;
        };
        let Some(target) = self
            .tab(key)
            .and_then(|t| t.editor.run_target())
            .and_then(trimmed)
        else {
            return;
        };
        let kind = PendingKind::Single(target);
        match self.needs_confirmation(key, &conn, &kind) {
            Some(pending) => self.confirm = Some(pending),
            None => self.start_kind(key, &conn, kind),
        }
    }

    /// The tab's connection, or `None` (and a notice) when it has none.
    fn runnable_conn(&mut self, key: u64) -> Option<String> {
        let conn = self.tab(key)?.conn.clone();
        if conn.is_none() {
            self.notify_as(ToastKind::Info, "Choose a connection for this tab first");
        }
        conn
    }

    /// Connects the session if it is not live; `false` while the connection waits for
    /// secrets the user has to enter.
    fn start_run(&mut self, conn: &str) -> bool {
        self.is_live(conn) || self.connect(conn)
    }
}

/// `target` without surrounding whitespace (its origin moved past the leading part); `None`
/// when nothing is left.
fn trimmed(target: RunRequest) -> Option<RunRequest> {
    let rest = target.sql.trim_start();
    let lead = target.sql[..target.sql.len() - rest.len()].chars().count();
    let sql = rest.trim_end();
    (!sql.is_empty()).then(|| RunRequest {
        sql: sql.to_string(),
        origin: target.origin + lead,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_uses_buffer_coordinates() {
        let tab = QueryTab::new(1, "t".into(), None, "é\r\nü x", SqlSyntax::ansi(), 500);
        assert_eq!(tab.line_col(0), (1, 1));
        // Char 3 is the first char of line 2 ("\r\n" is two chars).
        assert_eq!(tab.line_col(3), (2, 1));
        assert_eq!(tab.line_col(5), (2, 3));
        assert_eq!(tab.line_col(99), (2, 4));
    }

    #[test]
    fn target_database_is_the_tabs_own_when_different() {
        // The tab stays on the connection's own database.
        assert_eq!(target_database(None, None), None);
        assert_eq!(target_database(None, Some("app")), None);
        // "New query" on the connection's own database.
        assert_eq!(target_database(Some("app"), Some("app")), None);
        // Another database on the same server.
        assert_eq!(target_database(Some("other"), Some("app")), Some("other"));
        // Not connected yet, so the own database is unknown.
        assert_eq!(target_database(Some("app"), None), Some("app"));
    }

    #[test]
    fn error_location_is_fixed_when_the_error_arrives() {
        use sqlr_core::{ConnectionConfig, ErrorKind, Secrets};
        use sqlr_engine::{DriverRegistry, SessionStatus};
        use sqlr_testkit::{FakeDriver, FakeScript};

        let mut error = DriverError::new(ErrorKind::Syntax, "syntax error at \"form\"");
        error.position = Some(9);
        let mut drivers = DriverRegistry::default();
        drivers.register(FakeDriver::new(FakeScript {
            query_error: Some(error),
            ..Default::default()
        }));
        let engine = Engine::new(drivers, Arc::new(|| {}));
        engine.connect(
            ConnectionConfig {
                id: "c".into(),
                name: "c".into(),
                driver: "fake".into(),
                group: None,
                color: None,
                values: Default::default(),
                profile: None,
                save_secrets: true,
            },
            Secrets::default(),
            sqlr_core::ConnectionProfile::development(),
        );
        let wait = |done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !done() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        wait(&|| {
            matches!(
                engine.session_status("c"),
                Some(SessionStatus::Connected(_))
            )
        });

        let text = "select 1;\nselect * form t;";
        let mut tab = QueryTab::new(
            1,
            "t".into(),
            Some("c".into()),
            text,
            SqlSyntax::ansi(),
            500,
        );
        let target = RunRequest {
            sql: "select * form t".into(),
            origin: 10,
        };
        for script in [false, true] {
            if script {
                tab.run_script(&engine, "c", vec![target.clone()], ScriptOnError::Stop);
                wait(&|| {
                    let TabRun::Script { id, .. } = tab.run.as_ref().unwrap() else {
                        unreachable!()
                    };
                    engine.script(*id).is_some_and(|s| s.read().done)
                });
            } else {
                tab.run_single(&engine, "c", target.clone());
                wait(&|| {
                    tab.shown_query(&engine)
                        .and_then(|q| engine.result(q))
                        .is_some_and(|r| matches!(r.read().status, QueryStatus::Failed(_)))
                });
            }
            tab.update_markers(&engine);
            // "form" is at line 2, column 10.
            assert_eq!(tab.error_at(0), Some((2, 10)), "script: {script}");
            // Lines typed above the statement after the error arrived don't move it.
            tab.editor
                .set_carets(vec![sqlr_ui::Caret::at(sqlr_ui::Pos::default())]);
            tab.editor.insert_text("-- note\n");
            assert_eq!(tab.error_at(0), Some((2, 10)), "script: {script}");
            tab.editor = EditorState::new(text, SqlSyntax::ansi());
        }
    }

    #[test]
    fn single_failure_marks_once() {
        use sqlr_engine::DriverRegistry;
        let engine = Engine::new(DriverRegistry::default(), Arc::new(|| {}));
        let mut tab = QueryTab::new(1, "t".into(), None, "select x", SqlSyntax::ansi(), 500);
        // No session: the query fails without a position, which marks nothing.
        tab.run_single(
            &engine,
            "missing",
            RunRequest {
                sql: "select x".into(),
                origin: 0,
            },
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !matches!(
            tab.shown_query(&engine)
                .and_then(|q| engine.result(q))
                .map(|r| r.read().status.clone()),
            Some(QueryStatus::Failed(_))
        ) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        tab.update_markers(&engine);
        assert_eq!(tab.marked, 1);
        assert!(tab.editor.markers().is_empty());
        tab.close_run(&engine);
        assert_eq!(tab.marked, 0);
        assert!(tab.run.is_none());
    }
}
