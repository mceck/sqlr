//! Query tabs: a `QueryEditor` above the tab's run (one statement or a script), inside the
//! dock area, and the app's run, run-script and stop actions.

use sqlr_ui::ToastKind;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Id, RichText, Stroke, WidgetText};
use sqlr_core::{ConnectionConfig, Dialect, DriverError, PlanTree, SqlSyntax, StatementClass};
use sqlr_engine::{
    Engine, JobId, JobOutcome, QueryId, QueryStatus, ScriptId, ScriptOnError, SessionKey,
};
use sqlr_ui::theme::{self, Palette, Spacing, icons};
use sqlr_ui::{
    ChangesAction, EditorState, GridAction, GridInput, GridState, Marker, PlanChrome,
    PlanViewState, ResultBarAction, ResultBarView, RunRequest, data_grid, plan_view, query_editor,
    result_bar, result_bar_frame,
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
    Plan,
}

/// The part of a single statement's run shown below the editor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SinglePane {
    #[default]
    Result,
    Messages,
    Plan,
}

/// An Analyze of a statement that writes, waiting for the user's confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanRequest {
    pub sql: String,
    pub analyze: bool,
}

/// The Plan pane of a tab: the last plan, the job computing a new one, and the confirmation
/// an Analyze of a write needs.
#[derive(Default)]
pub struct PlanPane {
    pub job: Option<JobId>,
    pub tree: Option<PlanTree>,
    pub error: Option<String>,
    /// The statement the current plan (or the last attempt) is about.
    pub sql: String,
    /// The last plan was computed with ANALYZE.
    pub analyzed: bool,
    pub pending: Option<PlanRequest>,
    pub view: PlanViewState,
    pub started: Option<Instant>,
}

impl PlanPane {
    /// Whether the pane has anything to show (or to wait for).
    pub fn has_content(&self) -> bool {
        self.job.is_some() || self.tree.is_some() || self.error.is_some() || self.pending.is_some()
    }

    /// Whether the statement at the caret differs from the one the plan is about.
    pub fn stale(&self, current: Option<&str>) -> bool {
        self.tree.is_some() && current.is_none_or(|sql| sql.trim() != self.sql.trim())
    }

    /// Starts explaining `sql`, keeping the previous plan visible until the new one arrives.
    pub fn start(
        &mut self,
        engine: &Engine,
        key: &SessionKey,
        database: Option<&str>,
        sql: String,
        analyze: bool,
    ) {
        self.release(engine);
        self.error = None;
        self.sql = sql.clone();
        self.analyzed = analyze;
        self.pending = None;
        self.started = Some(Instant::now());
        self.view.selected = None;
        self.view.collapsed.clear();
        self.view.scroll_to = None;
        self.job = Some(engine.explain_in(key, database, sql, analyze));
    }

    /// Collects the job's outcome, once it has one.
    pub fn poll(&mut self, engine: &Engine) {
        let Some(job) = self.job else {
            return;
        };
        let Some(outcome) = engine.job(job).and_then(|state| state.outcome) else {
            return;
        };
        engine.forget_job(job);
        self.job = None;
        self.started = None;
        match outcome {
            JobOutcome::Plan(plan) => {
                self.tree = Some(plan);
                self.error = None;
            }
            JobOutcome::Failed(e) => self.error = Some(e.message),
            _ => {}
        }
    }

    /// Asks the server to stop the running explain.
    pub fn cancel(&self, engine: &Engine) {
        if let Some(job) = self.job {
            engine.cancel_job(job);
        }
    }

    /// Stops and forgets the job; the plan itself stays.
    pub fn release(&mut self, engine: &Engine) {
        if let Some(job) = self.job.take() {
            engine.cancel_job(job);
            engine.forget_job(job);
        }
        self.started = None;
    }

    /// The elapsed time of a running explain.
    pub fn elapsed_ms(&self) -> Option<u128> {
        self.started.map(|started| started.elapsed().as_millis())
    }
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
    /// The Plan pane: the last plan and the job computing one.
    pub plan: PlanPane,
    /// The server-side sort and filter the current run applies.
    pub server: Option<ServerView>,
    /// A cell edited in the value panel.
    pub value_edit: Option<(QueryId, sqlr_ui::ValueEdit)>,
    /// Rows per fetch for this tab's results.
    pub page_size: usize,
    /// The buffer changed since the tab opened; agent edits and typed edits set it.
    pub dirty: bool,
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
        let mut editor = EditorState::new(sql, syntax);
        editor.set_snippet_menu(true);
        Self {
            key,
            title,
            conn,
            database: None,
            editor,
            run: None,
            grids: HashMap::new(),
            edits: HashMap::new(),
            pane: SinglePane::Result,
            plan: PlanPane::default(),
            server: None,
            value_edit: None,
            page_size,
            dirty: false,
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

    /// Collects a finished explain job, whether its pane is shown or not.
    pub fn poll_plan(&mut self, engine: &Engine) {
        self.plan.poll(engine);
    }

    /// Explains `sql` in this tab's session and shows the Plan pane. An Analyze of a
    /// statement that writes waits for the confirmation banner instead of starting.
    pub fn start_explain(
        &mut self,
        engine: &Engine,
        conn: &str,
        sql: String,
        analyze: bool,
        writes: bool,
    ) {
        self.pane = SinglePane::Plan;
        if let Some(TabRun::Script { pane, .. }) = &mut self.run {
            *pane = ScriptPane::Plan;
        }
        if analyze && writes {
            self.plan.pending = Some(PlanRequest { sql, analyze });
            return;
        }
        let key = self.session(engine, conn);
        self.plan
            .start(engine, &key, self.database.as_deref(), sql, analyze);
    }

    /// Runs the Analyze the confirmation banner was waiting for.
    pub fn confirm_plan(&mut self, engine: &Engine, conn: &str) {
        let Some(request) = self.plan.pending.take() else {
            return;
        };
        let key = self.session(engine, conn);
        self.plan.start(
            engine,
            &key,
            self.database.as_deref(),
            request.sql,
            request.analyze,
        );
    }

    /// Explains the current target again, with the same Analyze setting as the last plan.
    pub fn recompute_plan(
        &mut self,
        engine: &Engine,
        conn: &str,
        dialect: Option<&dyn Dialect>,
        analyze: bool,
    ) -> bool {
        let Some(target) = self.editor.run_target() else {
            return false;
        };
        let writes =
            dialect.is_none_or(|d| d.syntax().classify(&target.sql) != StatementClass::Read);
        self.start_explain(engine, conn, target.sql, analyze, writes);
        true
    }

    /// Stops and forgets a plan job when the tab closes.
    pub fn close_plan(&mut self, engine: &Engine) {
        self.plan.release(engine);
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
        self.pane = SinglePane::Result;
        let key = self.session(engine, conn);
        let q = engine.execute_in(&key, self.database.as_deref(), sql, self.page_size);
        self.run = Some(TabRun::Single { q, origin });
        q
    }

    /// Runs an object's Data page (`sql`, a read) in place of the previous run: it opens no
    /// Manual transaction, so the editor's own applies are not refused for it.
    pub fn run_page(&mut self, engine: &Engine, conn: &str, sql: String) -> QueryId {
        self.close_run(engine);
        self.pane = SinglePane::Result;
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
        self.pane = SinglePane::Result;
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
    /// "Save as snippet…" from the tab's editor context menu.
    SaveSnippet(u64),
    /// The breadcrumb's connection or database: select the tab's connection in the explorer.
    RevealConnection(u64),
    /// Selects the database used by this query tab.
    ChangeDatabase {
        tab: u64,
        database: String,
    },
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
    /// Connections whose driver produces query plans.
    pub explain: &'a HashSet<String>,
    /// Profile colors of the saved connections that have one, by connection id.
    pub tints: &'a HashMap<String, [u8; 3]>,
    /// Built-in and user connection profiles.
    pub profiles: &'a [sqlr_core::ConnectionProfile],
    /// The grids' display formats, per connection.
    pub formats: &'a crate::prefs::Formats,
    pub show_value_panel: bool,
    /// Failed statements are included in the error panel's copied details.
    pub include_sql: bool,
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
        tab.poll_plan(self.engine);
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
            crate::nosql::doc_ui(
                ui,
                tab.key,
                &mut tab.editor,
                doc,
                self.engine,
                commands,
                self.show_value_panel,
            );
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
                let mode = cfg.map(|c| {
                    crate::transaction::supported_commit_mode(
                        self.engine,
                        &c.driver,
                        crate::app::profile_of(self.profiles, c).commit_mode,
                    )
                });
                if let Some(command) =
                    crate::shell::tab_breadcrumb(ui, self.engine, tab, name, mode)
                {
                    self.commands.push(command);
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
                tab.dirty |= output.changed;
                if output.save_snippet {
                    self.commands.push(TabCommand::SaveSnippet(tab.key));
                }
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
            explain: tab.conn.as_ref().is_some_and(|c| self.explain.contains(c)),
            connections: self.connections,
            include_sql: self.include_sql,
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
            explain: false,
            connections: self.connections,
            include_sql: self.include_sql,
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
        None => single_view(ui, tab, cx, None),
        Some(TabRun::Single { q, .. }) => {
            let q = *q;
            single_view(ui, tab, cx, Some(q));
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
    /// The tab connection's driver produces query plans.
    pub explain: bool,
    /// Saved connections, for the names the error details carry.
    pub connections: &'a [ConnectionConfig],
    /// Whether failed statements are copied with their details.
    pub include_sql: bool,
    pub commands: &'a mut Vec<TabCommand>,
}

/// The strip of panes above a run: `labels` as selectable tabs (returns the one clicked).
/// A label is `(text, selected, enabled)`.
fn pane_strip(
    ui: &mut egui::Ui,
    labels: &[(String, bool, bool)],
    extra: impl FnOnce(&mut egui::Ui),
) -> Option<usize> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        for (i, (label, selected, enabled)) in labels.iter().enumerate() {
            let response = ui
                .add_enabled_ui(*enabled, |ui| ui.selectable_label(*selected, label))
                .inner;
            if response.clicked() {
                picked = Some(i);
            }
        }
        extra(ui);
    });
    picked
}

/// The Explain split button at the right of a run's strip: `Explain` on a click, the
/// chevron for `Explain Analyze`.
fn explain_controls(ui: &mut egui::Ui, tab: &mut QueryTab, cx: &mut ViewCx<'_>) {
    let p = theme::palette(ui.ctx());
    let target = tab.editor.run_target();
    let conn = tab.conn.clone();
    let enabled = cx.explain && conn.is_some() && target.is_some();
    let tip = if !cx.explain {
        "This database does not provide query plans".to_string()
    } else if conn.is_none() {
        "Open a connection to explain this statement".to_string()
    } else if let Some(target) = &target {
        format!(
            "{}\n{}",
            sqlr_ui::keymap::tooltip(
                ui.ctx(),
                "Explain the statement at the caret",
                sqlr_core::ActionId::Explain
            ),
            target.sql.lines().next().unwrap_or_default()
        )
    } else {
        "Put the caret in a statement to explain".to_string()
    };
    let mut requested: Option<bool> = None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let arrow = icons::text(ui, icons::CARET_DOWN, 12.0).color(if enabled {
            p.text
        } else {
            p.text_disabled
        });
        let chevron = ui
            .add_enabled(
                enabled,
                egui::Button::new(arrow)
                    .fill(p.bg_raised)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .corner_radius(egui::CornerRadius {
                        nw: 0,
                        sw: 0,
                        ne: Spacing::RADIUS_S,
                        se: Spacing::RADIUS_S,
                    })
                    .min_size(egui::vec2(32.0, 28.0)),
            )
            .on_hover_text(if enabled {
                "Explain options"
            } else {
                tip.as_str()
            });
        chevron.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, "Explain options")
        });
        if enabled {
            egui::Popup::menu(&chevron)
                .style(theme::menu_style(ui.ctx()))
                .show(|ui| {
                    if icons::menu_item(ui, icons::PLAN, "Explain").clicked() {
                        requested = Some(false);
                        ui.close();
                    }
                    if icons::menu_item(ui, icons::PLAN, "Explain Analyze").clicked() {
                        requested = Some(true);
                        ui.close();
                    }
                });
        }
        let glyph = icons::text(ui, icons::PLAN, icons::SIZE).color(if enabled {
            p.text_muted
        } else {
            p.text_disabled
        });
        let label = RichText::new("Explain").color(if enabled { p.text } else { p.text_disabled });
        let primary = egui::Button::new((glyph, label))
            .fill(p.bg_raised)
            .stroke(egui::Stroke::new(1.0, p.border))
            .min_size(egui::vec2(100.0, 28.0))
            .corner_radius(egui::CornerRadius {
                nw: Spacing::RADIUS_S,
                sw: Spacing::RADIUS_S,
                ne: 0,
                se: 0,
            });
        let response = ui.add_enabled(enabled, primary).on_hover_text(tip.as_str());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, "Explain")
        });
        if response.clicked() {
            requested = Some(false);
        }
    });
    if let (Some(analyze), Some(target), Some(conn)) = (requested, target, conn) {
        let writes = cx
            .dialect
            .is_none_or(|d| d.syntax().classify(&target.sql) != StatementClass::Read);
        tab.start_explain(cx.engine, &conn, target.sql, analyze, writes);
    }
}

fn single_view(ui: &mut egui::Ui, tab: &mut QueryTab, cx: &mut ViewCx<'_>, q: Option<QueryId>) {
    let has_plan = tab.plan.has_content();
    let run = q.is_some();
    let mut labels = vec![
        (
            "Result 1".to_string(),
            tab.pane == SinglePane::Result && run,
            run,
        ),
        (
            "Messages".to_string(),
            tab.pane == SinglePane::Messages && run,
            run,
        ),
    ];
    if has_plan {
        labels.push(("Plan".to_string(), tab.pane == SinglePane::Plan, true));
    }
    let picked = pane_strip(ui, &labels, |ui| explain_controls(ui, tab, cx));
    match picked {
        Some(0) if run => tab.pane = SinglePane::Result,
        Some(1) if run => tab.pane = SinglePane::Messages,
        Some(2) if has_plan => tab.pane = SinglePane::Plan,
        _ => {}
    }
    ui.separator();
    match tab.pane {
        SinglePane::Plan => plan_pane(ui, tab, cx),
        _ => match q {
            None => {
                let text = crate::tabs::run_hint(ui.ctx());
                hint(ui, &text)
            }
            Some(q) if tab.pane == SinglePane::Messages => {
                let messages = cx
                    .engine
                    .result(q)
                    .map(|r| r.read().messages.clone())
                    .unwrap_or_default();
                messages_view(ui, ("messages", tab.key), &messages);
            }
            Some(q) => result_view(ui, tab, cx, q, 0),
        },
    }
}

/// The Plan pane: the confirmation an Analyze of a write needs, the running job, an error,
/// then the plan itself.
fn plan_pane(ui: &mut egui::Ui, tab: &mut QueryTab, cx: &mut ViewCx<'_>) {
    let p = theme::palette(ui.ctx());
    let engine = cx.engine;
    let conn = tab.conn.clone();
    let current = tab
        .plan
        .tree
        .is_some()
        .then(|| tab.editor.run_target().map(|target| target.sql))
        .flatten();
    let mut confirm = false;
    let mut discard = false;
    let mut stop = false;
    let mut retry = false;
    let mut recompute = false;

    if let Some(request) = &tab.plan.pending {
        egui::Frame::new()
            .fill(p.warning.gamma_multiply(0.12))
            .corner_radius(egui::CornerRadius::same(Spacing::RADIUS_M))
            .inner_margin(egui::Margin::same(Spacing::M as i8))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = Spacing::S;
                    icons::paint(ui, icons::WARNING, p.warning);
                    ui.label(
                        RichText::new(format!(
                            "Explain Analyze runs this statement: {}",
                            request.sql.lines().next().unwrap_or_default()
                        ))
                        .color(p.warning),
                    );
                });
                theme::hint(
                    ui,
                    "Its changes are rolled back, but triggers and other side effects still happen.",
                );
                ui.horizontal(|ui| {
                    if ui
                        .add(theme::primary_button(ui.ctx(), "Run Explain Analyze"))
                        .clicked()
                    {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        discard = true;
                    }
                });
            });
    }

    if tab.plan.job.is_some() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::S;
            ui.spinner();
            let elapsed = tab.plan.elapsed_ms().unwrap_or_default();
            ui.label(format!("Explaining… {elapsed} ms"));
            if icons::button(ui, true, icons::STOP, Some(p.error), "Stop").clicked() {
                stop = true;
            }
        });
    }
    if let Some(error) = &tab.plan.error {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::S;
            icons::paint(ui, icons::FAILED, p.error);
            ui.label(RichText::new(error).color(p.error));
            if ui.button("Retry").clicked() {
                retry = true;
            }
        });
    }

    let stale = tab.plan.stale(current.as_deref());
    let running = tab.plan.job.is_some() || tab.plan.pending.is_some();
    let sql = tab.plan.sql.clone();
    let analyzed = tab.plan.analyzed;
    if let Some(tree) = &tab.plan.tree {
        if let Some(action) = plan_view(
            ui,
            tree,
            &mut tab.plan.view,
            PlanChrome {
                sql: &sql,
                stale,
                running,
            },
        ) {
            match action {
                sqlr_ui::PlanAction::CopyRaw => ui.ctx().copy_text(tree.raw.clone()),
                sqlr_ui::PlanAction::Recompute => recompute = true,
            }
        }
    } else if tab.plan.job.is_none() && tab.plan.error.is_none() && tab.plan.pending.is_none() {
        theme::hint(
            ui,
            "The plan of the statement at the caret appears here. Press Explain.",
        );
    }

    if confirm && let Some(conn) = &conn {
        tab.confirm_plan(engine, conn);
    }
    if discard {
        tab.plan.pending = None;
    }
    if stop {
        tab.plan.cancel(engine);
    }
    if retry && let Some(conn) = &conn {
        let writes = cx
            .dialect
            .is_none_or(|d| d.syntax().classify(&sql) != StatementClass::Read);
        tab.start_explain(engine, conn, sql, analyzed, writes);
    }
    if recompute && let Some(conn) = &conn {
        tab.recompute_plan(engine, conn, cx.dialect, analyzed);
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
        ScriptPane::Plan => ScriptPane::Plan,
        _ => ScriptPane::Output,
    };
    let mut panes: Vec<ScriptPane> = (0..results.len()).map(ScriptPane::Result).collect();
    panes.push(ScriptPane::Output);
    panes.push(ScriptPane::Messages);
    if tab.plan.has_content() {
        panes.push(ScriptPane::Plan);
    }
    let labels: Vec<(String, bool, bool)> = panes
        .iter()
        .map(|p| {
            let label = match p {
                ScriptPane::Result(n) => format!("Result {}", n + 1),
                ScriptPane::Output => "Output".to_string(),
                ScriptPane::Messages => "Messages".to_string(),
                ScriptPane::Plan => "Plan".to_string(),
            };
            (label, *p == shown, true)
        })
        .collect();
    let picked = pane_strip(ui, &labels, |ui| {
        if let Some(n) = running {
            ui.spinner();
            ui.label(RichText::new(format!("statement {}", n + 1)).color(muted));
        }
        explain_controls(ui, tab, cx);
    })
    .map(|i| panes[i]);
    if let Some(p) = picked
        && let Some(TabRun::Script { pane, .. }) = &mut tab.run
    {
        if p == ScriptPane::Plan {
            tab.pane = SinglePane::Plan;
        }
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
        ScriptPane::Plan => plan_pane(ui, tab, cx),
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
            let connection = tab.conn.as_deref().map(|id| {
                cx.connections
                    .iter()
                    .find(|connection| connection.id == id)
                    .map_or_else(
                        || id.to_string(),
                        |connection| format!("{} ({id})", connection.name),
                    )
            });
            let details = ErrorDetails {
                error: e,
                at,
                connection,
                database: r.database.as_deref(),
                sql: cx.include_sql.then_some(r.sql.as_str()),
                elapsed_ms: r.elapsed.as_millis() as u64,
            };
            error_panel(ui, &details);
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

/// "Run the statement at the caret with ⌘↵, or the script with ⇧⌘↵", from the keymap.
pub(crate) fn run_hint(ctx: &egui::Context) -> String {
    use sqlr_core::ActionId;
    let keys = sqlr_ui::keymap::ui_keymap(ctx);
    match (
        keys.label(ActionId::RunStatement),
        keys.label(ActionId::RunScript),
    ) {
        (Some(run), Some(script)) => {
            format!("Run the statement at the caret with {run}, or the script with {script}")
        }
        (Some(run), None) => format!("Run the statement at the caret with {run}"),
        _ => "Run the statement at the caret with the Run button".to_string(),
    }
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.add_space(Spacing::M);
    ui.horizontal(|ui| {
        ui.add_space(Spacing::M);
        ui.label(RichText::new(text).color(theme::palette(ui.ctx()).text_muted));
    });
}

/// The error with everything needed to copy a useful report.
struct ErrorDetails<'a> {
    error: &'a DriverError,
    at: Option<(usize, usize)>,
    connection: Option<String>,
    database: Option<&'a str>,
    sql: Option<&'a str>,
    elapsed_ms: u64,
}

impl ErrorDetails<'_> {
    /// The text **Copy details** puts on the clipboard.
    fn text(&self) -> String {
        let mut out = format!("Error: {}\n", self.error.message);
        if let Some(hint) = &self.error.hint {
            out.push_str(&format!("Hint: {hint}\n"));
        }
        if let Some(connection) = &self.connection {
            out.push_str(&format!("Connection: {connection}\n"));
        }
        if let Some(database) = self.database {
            out.push_str(&format!("Database: {database}\n"));
        }
        if let Some((line, column)) = self.at {
            out.push_str(&format!("At: line {line}, column {column}\n"));
        }
        if let Some(sql) = self.sql {
            out.push_str(&format!(
                "Statement: {}\n",
                crate::logging::truncate_sql(sql).0
            ));
        }
        out.push_str(&format!("Elapsed: {} ms", self.elapsed_ms));
        out
    }
}

/// `details.at` is the 1-based line and column of the error position, in the editor's buffer.
fn error_panel(ui: &mut egui::Ui, details: &ErrorDetails<'_>) {
    ui.add_space(Spacing::M);
    ui.colored_label(theme::palette(ui.ctx()).error, &details.error.message);
    if let Some(hint) = &details.error.hint {
        ui.label(format!("Hint: {hint}"));
    }
    if let Some((line, column)) = details.at {
        ui.label(format!("at line {line}, column {column}"));
    }
    if ui.button("Copy details").clicked() {
        ui.ctx().copy_text(details.text());
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

impl SqlrApp {
    /// Runs a tab for an MCP agent: the whole buffer as a script, one statement by index,
    /// or the selection. Exposure and profile read-only are enforced here as a second line
    /// of defence; a confirmation opens the normal in-app dialog and asks the agent to retry.
    pub(crate) fn run_tab_for_agent(
        &mut self,
        key: u64,
        target: sqlr_mcp::host::RunTarget,
        max_rows: usize,
    ) -> sqlr_mcp::error::ToolResult<serde_json::Value> {
        use sqlr_mcp::error::ToolError;
        let Some((busy, conn)) = self
            .dock
            .iter_all_tabs()
            .find(|(_, tab)| tab.key == key)
            .map(|(_, tab)| (tab.is_running(&self.engine), tab.conn.clone()))
        else {
            return Err(ToolError::not_found("Unknown tab"));
        };
        if busy {
            return Err(ToolError::busy());
        }
        let Some(conn) = conn else {
            return Err(ToolError::invalid("The tab has no connection"));
        };
        let Some(cfg) = self.connection(&conn).cloned() else {
            return Err(ToolError::not_found("Unknown connection"));
        };
        if cfg.agent_access == sqlr_core::AgentAccess::None {
            return Err(ToolError::not_exposed(&conn));
        }
        let profile = self.profile_for(&cfg);
        let Some(tab) = self.tab(key) else {
            return Err(ToolError::not_found("Unknown tab"));
        };
        tab.page_size = max_rows.clamp(1, 1000);
        let syntax = tab.editor.syntax().clone();
        let (kind, statements) = match target {
            sqlr_mcp::host::RunTarget::Script => {
                let targets: Vec<RunRequest> = tab.editor.script_targets();
                if targets.is_empty() {
                    return Err(ToolError::invalid("The tab is empty"));
                }
                let statements = targets.iter().map(|t| t.sql.clone()).collect();
                (PendingKind::Script(targets), statements)
            }
            sqlr_mcp::host::RunTarget::Statement(index) => {
                let targets = tab.editor.script_targets();
                let Some(target) = targets.get(index).cloned() else {
                    return Err(ToolError::invalid(format!(
                        "Statement {index} does not exist; the tab has {}",
                        targets.len()
                    )));
                };
                let sql = target.sql.clone();
                (PendingKind::Single(target), vec![sql])
            }
            sqlr_mcp::host::RunTarget::Selection => {
                let Some(target) = tab.editor.run_target() else {
                    return Err(ToolError::invalid("The tab has no selection"));
                };
                if target.sql.trim().is_empty() {
                    return Err(ToolError::invalid("The selection is empty"));
                }
                let sql = target.sql.clone();
                (PendingKind::Single(target), vec![sql])
            }
        };
        if cfg.agent_access == sqlr_core::AgentAccess::Read || profile.read_only {
            for sql in &statements {
                if syntax.classify(sql).modifies() {
                    return Err(ToolError::read_only("A statement in this run"));
                }
            }
        }
        let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
        if let Some((prompt, listed)) =
            crate::confirm::confirmation(&profile, &cfg.name, &syntax, &refs)
        {
            self.confirm = Some(crate::confirm::PendingRun {
                tab: key,
                conn: conn.clone(),
                kind,
                prompt,
                statements: listed,
            });
            return Err(ToolError::new(
                "approval_required",
                "The in-app approval dialog is open; answer it and call run_query_tab again",
            ));
        }
        let count = statements.len();
        self.start_kind(key, &conn, kind);
        Ok(serde_json::json!({
            "tab_id": key,
            "statements": count,
            "status": "running",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_engine::DriverRegistry;

    #[test]
    fn copy_error_details_reaches_the_clipboard_with_context() {
        use egui_kittest::kittest::Queryable;
        let mut error = DriverError::new(sqlr_core::ErrorKind::Syntax, "syntax error");
        error.hint = Some("check FROM".into());
        let mut h = egui_kittest::Harness::builder().build_ui(move |ui| {
            error_panel(
                ui,
                &ErrorDetails {
                    error: &error,
                    at: Some((2, 3)),
                    connection: Some("local".into()),
                    database: Some("app"),
                    sql: Some("select bad"),
                    elapsed_ms: 17,
                },
            );
        });
        h.run_steps(2);
        h.get_by_label("Copy details").click();
        let mut copied = None;
        for _ in 0..5 {
            h.step();
            for command in &h.output().platform_output.commands {
                if let egui::OutputCommand::CopyText(text) = command {
                    copied = Some(text.clone());
                }
            }
        }
        let text = copied.expect("the clipboard command");
        for expected in [
            "syntax error",
            "check FROM",
            "local",
            "app",
            "select bad",
            "17 ms",
            "line 2, column 3",
        ] {
            assert!(text.contains(expected), "{text}");
        }
    }

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
                agent_access: Default::default(),
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

    #[test]
    fn an_analyze_of_a_write_waits_for_the_confirmation() {
        let engine = Engine::new(DriverRegistry::default(), Arc::new(|| {}));
        let mut tab = QueryTab::new(
            1,
            "t".into(),
            Some("c".into()),
            "UPDATE t SET a = 1",
            SqlSyntax::ansi(),
            500,
        );
        tab.start_explain(&engine, "c", "UPDATE t SET a = 1".into(), true, true);
        assert_eq!(tab.pane, SinglePane::Plan);
        assert!(tab.plan.job.is_none());
        assert_eq!(
            tab.plan.pending,
            Some(PlanRequest {
                sql: "UPDATE t SET a = 1".into(),
                analyze: true,
            })
        );
        assert!(tab.plan.has_content());
        // Cancelling the confirmation leaves no job behind.
        tab.plan.pending = None;
        assert!(!tab.plan.has_content());
    }

    #[test]
    fn a_plan_is_stale_when_the_statement_at_the_caret_changes() {
        let mut tab = QueryTab::new(1, "t".into(), None, "SELECT 1", SqlSyntax::ansi(), 500);
        assert!(!tab.plan.stale(Some("SELECT 1")));
        tab.plan.tree = Some(PlanTree::default());
        tab.plan.sql = "SELECT 1".into();
        assert!(!tab.plan.stale(Some("SELECT 1")));
        assert!(tab.plan.stale(Some("SELECT 2")));
        assert!(tab.plan.stale(None));
    }

    fn sqlite_app(dir: &std::path::Path) -> SqlrApp {
        use crate::AppDeps;
        use sqlr_core::{ConnectionConfig, ConnectionProfile, Secrets};
        use sqlr_engine::SessionStatus;
        use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};

        let file = dir.join("plan.db");
        rusqlite::Connection::open(&file)
            .unwrap()
            .execute_batch(
                "CREATE TABLE orders (id INTEGER PRIMARY KEY, total REAL);
                 CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT);
                 INSERT INTO orders VALUES (1, 10.0), (2, 20.0);
                 INSERT INTO customers VALUES (1, 'Ada');",
            )
            .unwrap();
        let mut drivers = DriverRegistry::default();
        drivers.register(sqlr_driver_sqlite::SqliteDriver::new());
        let engine = Engine::new(drivers, Arc::new(|| {}));
        let repo = ConnectionRepo::open(dir, Box::new(MemorySecretStore::default())).unwrap();
        let cfg = ConnectionConfig {
            id: "db".into(),
            name: "Plan DB".into(),
            driver: "sqlite".into(),
            group: None,
            color: None,
            values: [("path".into(), file.display().to_string())].into(),
            profile: None,
            save_secrets: false,
            agent_access: Default::default(),
        };
        repo.save(&cfg, &Secrets::default()).unwrap();
        engine.connect(cfg, Secrets::default(), ConnectionProfile::development());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !matches!(
            engine.session_status("db"),
            Some(SessionStatus::Connected(_))
        ) {
            assert!(
                std::time::Instant::now() < deadline,
                "SQLite did not connect"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        SqlrApp::new(
            AppDeps {
                check_updates: false,
                engine,
                repo,
                settings: Settings {
                    theme: sqlr_ui::theme::ThemePref::Dark,
                    ..Settings::default()
                },
                config_dir: dir.into(),
                data_dir: Some(dir.into()),
                startup_notice: None,
                storage_warning: None,
                mcp_token_store: None,
                diagnostics: None,
                diagnostics_warning: None,
            },
            &egui::Context::default(),
        )
    }

    fn app_harness(app: SqlrApp) -> egui_kittest::Harness<'static, SqlrApp> {
        let mut h = egui_kittest::Harness::builder()
            .with_size((1400.0, 900.0))
            .with_step_dt(0.01)
            .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
        sqlr_ui::theme::install_fonts(&h.ctx, &[]);
        h.run_steps(4);
        h
    }

    fn step_until(
        h: &mut egui_kittest::Harness<'static, SqlrApp>,
        what: &str,
        done: impl Fn(&egui_kittest::Harness<'static, SqlrApp>) -> bool,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done(h) {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn the_plan_pane_explains_before_a_run() {
        use egui_kittest::kittest::Queryable;

        let dir = tempfile::tempdir().unwrap();
        let mut app = sqlite_app(dir.path());
        app.new_tab(
            Some("plan".into()),
            Some("db".into()),
            "SELECT * FROM orders",
        );
        let mut h = app_harness(app);
        // The strip exists before the first run, with Explain enabled.
        h.get_by_label("Result 1");
        h.get_by_label("Messages");
        h.get_by_label("Explain").click();
        h.run_steps(2);
        step_until(&mut h, "the plan", |h| {
            h.query_by_label("Select SCAN orders").is_some()
        });
        h.get_by_label("Plan");
        h.get_by_label("Estimated");
        h.get_by_label("Select SCAN orders");
        // The inspector defaults to the root and names the table.
        h.get_by_label("SCAN orders");
    }

    #[test]
    fn a_changed_statement_recomputes_the_plan() {
        use egui_kittest::kittest::Queryable;

        let dir = tempfile::tempdir().unwrap();
        let mut app = sqlite_app(dir.path());
        let key = app.new_tab(
            Some("plan".into()),
            Some("db".into()),
            "SELECT * FROM orders",
        );
        let mut h = app_harness(app);
        h.get_by_label("Explain").click();
        step_until(&mut h, "the first plan", |h| {
            h.query_by_label("Select SCAN orders").is_some()
        });
        // The statement at the caret changes: the chip offers a recompute.
        h.state_mut()
            .tab(key)
            .unwrap()
            .editor
            .replace_all("SELECT * FROM customers");
        h.run_steps(2);
        h.get_by_label("Text changed");
        h.get_by_label("Recompute").click();
        h.run_steps(2);
        step_until(&mut h, "the new plan", |h| {
            h.query_by_label("Select SCAN customers").is_some()
        });
        h.get_by_label("Select SCAN customers");
    }

    #[test]
    fn global_actions_switch_tabs_and_open_the_requested_tools() {
        use sqlr_core::ActionId as A;
        let dir = tempfile::tempdir().unwrap();
        let mut app = sqlite_app(dir.path());
        let ctx = egui::Context::default();
        app.dock = egui_dock::DockState::new(vec![]);
        app.new_tab(Some("first".into()), Some("db".into()), "SELECT 1");
        let first = app.active_key().unwrap();
        app.dispatch_action(A::NewTab, &ctx);
        let second = app.active_key().unwrap();
        assert_ne!(first, second);
        app.dispatch_action(A::PreviousTab, &ctx);
        assert_eq!(app.active_key(), Some(first));
        app.dispatch_action(A::NextTab, &ctx);
        assert_eq!(app.active_key(), Some(second));
        app.dispatch_action(A::NextTab, &ctx);
        assert_eq!(app.active_key(), Some(first));
        let history = app.show_history;
        let values = app.show_value_panel;
        app.dispatch_action(A::ToggleHistory, &ctx);
        app.dispatch_action(A::ToggleValuePanel, &ctx);
        assert_eq!(app.show_history, !history);
        assert_eq!(app.show_value_panel, !values);
        app.dispatch_action(A::ShowOperations, &ctx);
        assert!(app.show_operations);
        app.dispatch_action(A::ShowTasks, &ctx);
        assert!(matches!(app.left, crate::app::LeftPane::Tasks));
        app.dispatch_action(A::Settings, &ctx);
        assert!(app.prefs.is_some());
        app.dispatch_action(A::Profiles, &ctx);
        assert!(app.profiles_dialog.is_some());
        app.dispatch_action(A::NewConnection, &ctx);
        assert!(app.conn_dialog.is_some());
        app.dispatch_action(A::TransferData, &ctx);
        assert!(app.transfer_wizard.is_some());
        let mut other = app.connections[0].clone();
        other.id = "second-db".into();
        app.connections.push(other);
        app.dispatch_action(A::CompareSchemas, &ctx);
        assert!(app.compare_dialog.is_some());
        app.dispatch_action(A::CloseTab, &ctx);
        assert_eq!(app.dock.iter_all_tabs().count(), 1);
    }

    #[test]
    fn explain_uses_the_tabs_selected_postgres_database() {
        sqlr_testkit::skip_container_tests!();
        use sqlr_core::{ConnectionConfig, ConnectionProfile, Secrets};
        use sqlr_engine::SessionStatus;
        let (host, port) = sqlr_testkit::service("POSTGRES");
        let mut drivers = DriverRegistry::default();
        drivers.register(sqlr_driver_postgres::PostgresDriver::new());
        let engine = Engine::new(drivers, Arc::new(|| {}));
        engine.connect(
            ConnectionConfig {
                id: "pg".into(),
                name: "pg".into(),
                driver: "postgres".into(),
                group: None,
                color: None,
                profile: None,
                save_secrets: false,
                agent_access: Default::default(),
                values: [
                    ("host".into(), host),
                    ("port".into(), port.to_string()),
                    ("database".into(), "postgres".into()),
                    ("user".into(), "postgres".into()),
                    ("show_all_databases".into(), "true".into()),
                ]
                .into(),
            },
            Secrets([("password".into(), "postgres".into())].into()),
            ConnectionProfile::development(),
        );
        let wait = |done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + std::time::Duration::from_secs(10);
            while !done() {
                assert!(Instant::now() < deadline, "database operation timed out");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        wait(&|| {
            matches!(
                engine.session_status("pg"),
                Some(SessionStatus::Connected(_))
            )
        });
        let run = |key: &SessionKey, database: Option<&str>, sql: String| {
            let q = engine.execute_in(key, database, sql.clone(), 100);
            wait(&|| {
                engine
                    .result(q)
                    .is_some_and(|r| r.read().status != QueryStatus::Running)
            });
            assert!(
                matches!(
                    engine.result(q).unwrap().read().status,
                    QueryStatus::Ready { .. }
                ),
                "{sql}"
            );
            q
        };
        let database = sqlr_testkit::unique_name("explain_db");
        let meta = SessionKey::meta("pg");
        run(&meta, None, format!("CREATE DATABASE \"{database}\""));
        run(
            &meta,
            Some(&database),
            "CREATE TABLE public.\"PuckDraft\" (id integer)".into(),
        );
        let mut tab = QueryTab::new(
            901,
            "Plan".into(),
            Some("pg".into()),
            "SELECT * FROM public.\"PuckDraft\"",
            SqlSyntax::ansi(),
            100,
        );
        tab.database = Some(database.clone());
        // Explain must work before the tab's first normal query, including Retry and Analyze.
        for analyze in [false, true] {
            tab.start_explain(
                &engine,
                "pg",
                "SELECT * FROM public.\"PuckDraft\"".into(),
                analyze,
                false,
            );
            wait(&|| {
                engine
                    .job(tab.plan.job.unwrap())
                    .is_some_and(|j| j.outcome.is_some())
            });
            tab.poll_plan(&engine);
            assert!(tab.plan.error.is_none(), "{:?}", tab.plan.error);
            assert!(tab.plan.tree.is_some());
            assert_eq!(tab.plan.analyzed, analyze);
        }
        tab.start_explain(
            &engine,
            "pg",
            "INSERT INTO public.\"PuckDraft\" VALUES (1)".into(),
            true,
            true,
        );
        assert!(tab.plan.pending.is_some());
        tab.confirm_plan(&engine, "pg");
        wait(&|| {
            engine
                .job(tab.plan.job.unwrap())
                .is_some_and(|j| j.outcome.is_some())
        });
        tab.poll_plan(&engine);
        assert!(tab.plan.error.is_none(), "{:?}", tab.plan.error);
        let q = run(
            &meta,
            Some(&database),
            "SELECT count(*) FROM public.\"PuckDraft\"".into(),
        );
        assert_eq!(
            engine.result(q).unwrap().read().rows.cell(0, 0),
            Some(sqlr_core::Cell::Int(0))
        );
        run(
            &meta,
            None,
            format!("DROP DATABASE \"{database}\" WITH (FORCE)"),
        );
    }

    #[test]
    fn analyze_of_a_write_confirms_then_reports_the_server_error() {
        use egui_kittest::kittest::Queryable;

        let dir = tempfile::tempdir().unwrap();
        let mut app = sqlite_app(dir.path());
        app.new_tab(
            Some("plan".into()),
            Some("db".into()),
            "UPDATE orders SET total = 1",
        );
        let mut h = app_harness(app);
        // The chevron menu asks for Analyze; SQLite has none, and the engine says so.
        let options = h.get_by_label("Explain options").rect();
        assert!(
            options.width() >= 30.0,
            "the options menu needs a generous click target"
        );
        assert!(options.height() >= 28.0);
        h.get_by_label("Explain options").click();
        h.run_steps(2);
        h.get_by_label("Explain Analyze").click();
        h.run_steps(2);
        h.get_by_label("Run Explain Analyze");
        h.get_by_label("Run Explain Analyze").click();
        h.run_steps(2);
        step_until(&mut h, "the analyze error", |h| {
            h.query_by_label_contains("SQLite has no EXPLAIN ANALYZE")
                .is_some()
        });
        h.get_by_label("Retry");
    }
}
