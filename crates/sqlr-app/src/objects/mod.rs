//! Object editors in the app: object tabs, the explorer's object
//! actions (open, New ▸, Generate SQL ▸, Copy DDL, rename, truncate, drop), the create,
//! drop and preview dialogs, and the confirmations the connection profile asks for.

mod blocked_database;
pub(crate) mod create;
pub(crate) mod menus;
pub(crate) mod tab;

pub use tab::{ObjectTab, ObjectTabError};

use egui::Id;
use sqlr_core::{
    CatalogNeed, Node, NodeKind, NodePath, ObjectEditing, ObjectKind, ObjectRef, RelationKind,
    SchemaChange, SqlSyntax, SqlTemplate, script,
};
use sqlr_engine::{ApplyOptions, JobId, JobOutcome, LoadState, SessionKey};
use sqlr_ui::ToastKind;
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{ObjectEditorAction, TreeAction, kind_label, sql_view, template_label};

use crate::app::{SqlrApp, tab_mut};
use crate::confirm::{ConfirmSpec, confirm_modal, confirmation, prompt_lines};
use blocked_database::{DatabaseDrop, DatabaseDropDialog};
use create::CreateDialog;

/// Where an apply's outcome goes.
#[derive(Clone, Debug)]
pub(crate) enum AfterApply {
    /// The object tab with this key follows it.
    Tab(u64),
    /// The status bar says this once it succeeded.
    Explorer(String),
    /// A database drop retains its target so a failure can show its server sessions.
    DropDatabase { database: String, what: String },
    /// The users and roles tab with this key follows it.
    Admin(u64),
    /// The object tab with this key follows it as a change of the object's grants.
    Grants(u64),
}

/// An explorer action waiting for its job.
enum ExplorerJob {
    /// Open the template in a new editor tab.
    Generate {
        conn: String,
        template: SqlTemplate,
        title: String,
        database: Option<String>,
    },
    CopyDdl {
        conn: String,
    },
    Applied {
        what: String,
    },
    DropDatabase(DatabaseDrop),
}

/// DDL waiting for the profile's confirmation.
struct ConfirmDdl {
    conn: String,
    prompt: String,
    lines: Vec<String>,
    statements: Vec<String>,
    options: ApplyOptions,
    then: AfterApply,
}

/// Drop or truncate: always asked first, with the statements shown.
struct DestructiveDialog {
    conn: String,
    /// The dialog's title ("Drop table orders").
    heading: String,
    /// The question in its body.
    title: String,
    statements: Vec<String>,
    /// The statements with `CASCADE`, when the dialect has it, and whether it is checked.
    cascade: Option<(Vec<String>, bool)>,
    options: ApplyOptions,
    done: String,
    cascade_label: &'static str,
    /// The confirming button's label.
    verb: &'static str,
    drop_database: Option<String>,
}

/// The texts of a drop or truncate confirmation.
struct Destructive {
    heading: String,
    title: String,
    cascade_label: &'static str,
    done: String,
    verb: &'static str,
}

#[derive(Default)]
pub(crate) struct ObjectsUi {
    create: Option<CreateDialog>,
    destructive: Option<DestructiveDialog>,
    confirm: Option<ConfirmDdl>,
    /// The object tab whose pending DDL is previewed, and the statements.
    preview: Option<(u64, Result<Vec<String>, ObjectTabError>)>,
    jobs: Vec<(JobId, ExplorerJob)>,
    blocked_database: Option<DatabaseDropDialog>,
    /// An object or users tab whose pending edits would be lost, and what then happens.
    pub(crate) discard: Option<(u64, Discard)>,
}

/// What discarding a tab's pending edits is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Discard {
    Refresh,
    Close,
    /// An object tab's Data page is to be read again, dropping its row edits.
    Data,
    /// An action of the users and roles tab.
    Users(crate::admin::UsersDiscard),
}

/// `path` without its last segment.
fn parent(path: &NodePath) -> NodePath {
    let s = path.segments();
    NodePath(s[..s.len().saturating_sub(1)].to_vec())
}

impl SqlrApp {
    /// The driver's object editing support for `conn`.
    pub(crate) fn editing_for(&self, conn: &str) -> Option<ObjectEditing> {
        let cfg = self.connection(conn)?;
        self.engine.manifest(&cfg.driver)?.editing
    }

    /// Object editing support of every saved connection whose driver has it.
    pub(crate) fn editing_by_conn(&self) -> std::collections::HashMap<String, ObjectEditing> {
        self.connections
            .iter()
            .filter_map(|c| Some((c.id.clone(), self.engine.manifest(&c.driver)?.editing?)))
            .collect()
    }

    fn syntax_of(&self, conn: &str) -> std::sync::Arc<SqlSyntax> {
        self.dialects
            .get(conn)
            .map_or_else(SqlSyntax::ansi, |d| d.syntax())
    }

    /// The object tab with `key`.
    fn object_tab(&mut self, key: u64) -> Option<&mut ObjectTab> {
        tab_mut(&mut self.dock, key).and_then(|t| t.object.as_deref_mut())
    }

    /// The kind of the explorer node at `path`, from the loaded tree.
    fn node_kind_at(&self, conn: &str, path: &NodePath) -> Option<NodeKind> {
        match self.engine.children(conn, &parent(path))? {
            LoadState::Ready(nodes) => nodes.iter().find(|n| n.path == *path).map(|n| n.kind),
            _ => None,
        }
    }

    /// The schema a node is in (its nearest schema node), and the table it belongs to (a
    /// table node, or its "indexes"/"triggers" folder).
    pub(crate) fn location(
        &self,
        conn: &str,
        node: Option<&Node>,
    ) -> (Option<String>, Option<String>) {
        let Some(node) = node else {
            let schema = self
                .dialects
                .get(conn)
                .and_then(|d| d.default_schemas().into_iter().next());
            return (schema, None);
        };
        let segments = node.path.segments();
        let mut schema = None;
        for k in (1..=segments.len()).rev() {
            let path = NodePath(segments[..k].to_vec());
            let kind = if path == node.path {
                Some(node.kind)
            } else {
                self.node_kind_at(conn, &path)
            };
            if kind == Some(NodeKind::Schema) {
                schema = Some(segments[k - 1].clone());
                break;
            }
        }
        if schema.is_none() {
            schema = self
                .dialects
                .get(conn)
                .and_then(|d| d.default_schemas().into_iter().next());
        }
        let table = match node.kind {
            NodeKind::Table => Some(node.name.clone()),
            NodeKind::Folder if segments.len() >= 2 => {
                let up = parent(&node.path);
                (self.node_kind_at(conn, &up) == Some(NodeKind::Table))
                    .then(|| segments[segments.len() - 2].clone())
            }
            _ => None,
        };
        (schema, table)
    }

    /// The table names of `schema` in the connection's catalog (asked for when missing).
    fn catalog_tables(&self, conn: &str, schema: Option<&str>) -> Vec<String> {
        let Some(schema) = schema else {
            return Vec::new();
        };
        let catalog = self.engine.catalog(conn);
        match catalog.as_ref().and_then(|c| c.schema.get(schema)) {
            Some(LoadState::Ready(c)) => c
                .relations
                .iter()
                .filter(|r| r.kind == RelationKind::Table)
                .map(|r| r.name.clone())
                .collect(),
            _ => {
                self.engine
                    .load_catalog(conn, CatalogNeed::Schema(schema.to_string()));
                Vec::new()
            }
        }
    }

    /// Handles the explorer's object actions.
    pub(crate) fn object_tree_action(&mut self, action: TreeAction) {
        match action {
            TreeAction::OpenObject { conn, node } => self.open_object(&conn, &node),
            TreeAction::NewObject { conn, node, kind } => self.new_object(&conn, node, kind),
            TreeAction::DropObject { conn, node } => self.drop_object(&conn, &node),
            TreeAction::Truncate { conn, node } => self.truncate(&conn, &node),
            TreeAction::RenameObject { conn, node, to } => self.rename_object(&conn, &node, &to),
            TreeAction::GenerateSql {
                conn,
                node,
                template,
            } => {
                if let Some(object) = self.node_object(&conn, &node) {
                    let title = format!("{} {}", template_label(template), node.name);
                    let database = object.database.clone();
                    let id = self.engine.load_object(&conn, object);
                    self.objects.jobs.push((
                        id,
                        ExplorerJob::Generate {
                            conn,
                            template,
                            title,
                            database,
                        },
                    ));
                }
            }
            TreeAction::CopyDdl { conn, node } => {
                if let Some(object) = self.node_object(&conn, &node) {
                    let id = self.engine.load_object(&conn, object);
                    self.objects.jobs.push((id, ExplorerJob::CopyDdl { conn }));
                }
            }
            _ => {}
        }
    }

    /// The object `node` stands for, or a notice.
    fn node_object(&mut self, conn: &str, node: &Node) -> Option<ObjectRef> {
        let object = self.engine.object_ref(conn, node);
        if object.is_none() {
            self.notify(format!("{} is not an object sqlr can edit", node.name));
        }
        object
    }

    /// Opens (or focuses) the object editor of `node`.
    pub(crate) fn open_object(&mut self, conn: &str, node: &Node) {
        let Some(object) = self.node_object(conn, node) else {
            return;
        };
        let Some(editing) = self.editing_for(conn) else {
            return;
        };
        if let Some(path) = self.dock.find_tab_from(|t| {
            t.object
                .as_ref()
                .is_some_and(|o| o.conn == conn && o.object == object)
        }) {
            let _ = self.dock.set_active_tab(path);
            return;
        }
        let tab = ObjectTab::open(
            &self.engine,
            conn,
            object,
            editing,
            vec![parent(&node.path)],
        );
        self.push_object_tab(conn, tab);
    }

    fn push_object_tab(&mut self, conn: &str, object: ObjectTab) {
        let key = self.new_tab(Some(object.title()), Some(conn.to_string()), "");
        if let Some(tab) = tab_mut(&mut self.dock, key) {
            tab.database = object.database();
            tab.object = Some(Box::new(object));
        }
    }

    pub(crate) fn new_object(&mut self, conn: &str, node: Option<Node>, kind: ObjectKind) {
        if kind == ObjectKind::Role {
            self.notify_as(
                ToastKind::Info,
                "Roles are created in the administration panel",
            );
            return;
        }
        let Some(cfg) = self.connection(conn).cloned() else {
            return;
        };
        let dialect = self
            .engine
            .manifest(&cfg.driver)
            .map(|m| m.dialect)
            .unwrap_or_default();
        let (schema, table) = match kind {
            // Databases and schemas are not inside a schema.
            ObjectKind::Database | ObjectKind::Schema => (None, None),
            _ => self.location(conn, node.as_ref()),
        };
        let database = node
            .as_ref()
            .and_then(|n| self.engine.database_for_path(conn, &n.path));
        let tables = if matches!(kind, ObjectKind::Index | ObjectKind::Trigger) {
            self.catalog_tables(conn, schema.as_deref())
        } else {
            Vec::new()
        };
        let refresh = vec![node.map_or_else(NodePath::root, |n| n.path)];
        self.objects.create = Some(CreateDialog::new(
            conn, kind, schema, database, table, tables, refresh, &dialect,
        ));
    }

    fn drop_object(&mut self, conn: &str, node: &Node) {
        let Some(object) = self.node_object(conn, node) else {
            return;
        };
        let what = kind_label(object.kind).to_lowercase();
        let change = |cascade| SchemaChange::Drop {
            object: object.clone(),
            cascade,
        };
        let dialog = Destructive {
            heading: format!("Drop {what} {}", node.name),
            title: format!("Drop {what} {}?", node.name),
            cascade_label: "Cascade: also drop the objects that depend on it",
            done: format!("Dropped {what} {}", node.name),
            verb: "Drop",
        };
        self.ask_destructive(conn, node, &object, change, dialog);
    }

    fn truncate(&mut self, conn: &str, node: &Node) {
        let Some(object) = self.node_object(conn, node) else {
            return;
        };
        let table = object.table_ref();
        let change = |cascade| SchemaChange::Truncate {
            table: table.clone(),
            cascade,
        };
        let dialog = Destructive {
            heading: format!("Truncate {}", node.name),
            title: format!("Delete every row of {}?", node.name),
            cascade_label: "Cascade: also empty the tables that reference it",
            done: format!("Truncated {}", node.name),
            verb: "Truncate",
        };
        self.ask_destructive(conn, node, &object, change, dialog);
    }

    /// Opens the confirmation of a drop or truncate: the dialect's statements, and a
    /// CASCADE checkbox when its statements with `cascade` differ.
    fn ask_destructive(
        &mut self,
        conn: &str,
        node: &Node,
        object: &ObjectRef,
        change: impl Fn(bool) -> SchemaChange,
        dialog: Destructive,
    ) {
        let plain = match self.engine.preview_ddl(conn, &[change(false)]) {
            Ok(s) => s,
            Err(e) => {
                self.notify(e.message);
                return;
            }
        };
        let cascade = self
            .engine
            .preview_ddl(conn, &[change(true)])
            .ok()
            .filter(|c| *c != plain)
            .map(|c| (c, false));
        self.objects.destructive = Some(DestructiveDialog {
            conn: conn.to_string(),
            heading: dialog.heading,
            title: dialog.title,
            statements: plain,
            cascade,
            cascade_label: dialog.cascade_label,
            options: self.explorer_options(conn, object, node),
            done: dialog.done,
            verb: dialog.verb,
            drop_database: (dialog.verb == "Drop" && object.kind == ObjectKind::Database)
                .then(|| object.name.clone()),
        });
    }

    fn rename_object(&mut self, conn: &str, node: &Node, to: &str) {
        let Some(object) = self.node_object(conn, node) else {
            return;
        };
        let change = SchemaChange::Rename {
            object: object.clone(),
            to: to.to_string(),
        };
        match self.engine.preview_ddl(conn, &[change]) {
            Ok(statements) => {
                let options = self.explorer_options(conn, &object, node);
                let done = format!("Renamed {} to {to}", node.name);
                self.request_ddl(conn, statements, options, AfterApply::Explorer(done));
            }
            Err(e) => self.notify(e.message),
        }
    }

    /// How an explorer action on `node` applies: the driver's transaction support, the
    /// object's database, and a reload of the node's parent.
    fn explorer_options(&self, conn: &str, object: &ObjectRef, node: &Node) -> ApplyOptions {
        ApplyOptions {
            transactional: self.editing_for(conn).is_some_and(|e| e.transactional_ddl),
            database: object.database.clone(),
            refresh: vec![parent(&node.path)],
            masking: None,
        }
    }

    /// Applies `statements` after the profile's confirmation, when it asks for one (a
    /// `REVOKE` or `DROP ROLE` is destructive). The confirmation lists the statements as
    /// the user may see them: passwords masked.
    pub(crate) fn request_ddl(
        &mut self,
        conn: &str,
        statements: Vec<String>,
        options: ApplyOptions,
        then: AfterApply,
    ) {
        let Some(cfg) = self.connection(conn).cloned() else {
            return;
        };
        let profile = self.profile_for(&cfg);
        let syntax = self.syntax_of(conn);
        let shown = options.masking.as_ref().map_or(&statements, |m| &m.shown);
        let sql: Vec<&str> = shown.iter().map(String::as_str).collect();
        match confirmation(&profile, &cfg.name, &syntax, &sql) {
            Some((prompt, lines)) => {
                self.objects.confirm = Some(ConfirmDdl {
                    conn: conn.to_string(),
                    prompt,
                    lines,
                    statements,
                    options,
                    then,
                })
            }
            None => self.start_ddl(conn, statements, options, then),
        }
    }

    fn start_ddl(
        &mut self,
        conn: &str,
        statements: Vec<String>,
        options: ApplyOptions,
        then: AfterApply,
    ) {
        match then {
            AfterApply::Tab(key) => {
                let engine = &self.engine;
                if let Some(tab) =
                    tab_mut(&mut self.dock, key).and_then(|t| t.object.as_deref_mut())
                {
                    tab.start_apply(engine, statements, options);
                }
            }
            AfterApply::Explorer(what) => {
                let id = self
                    .engine
                    .apply_ddl(&SessionKey::meta(conn), statements, options);
                self.objects.jobs.push((id, ExplorerJob::Applied { what }));
            }
            AfterApply::DropDatabase { database, what } => {
                let request = DatabaseDrop {
                    conn: conn.to_string(),
                    database,
                    statements,
                    options,
                    what,
                };
                let id = request.start(&self.engine);
                self.objects
                    .jobs
                    .push((id, ExplorerJob::DropDatabase(request)));
            }
            AfterApply::Admin(key) => self.start_admin_apply(key, statements, options),
            AfterApply::Grants(key) => {
                let engine = &self.engine;
                if let Some(tab) =
                    tab_mut(&mut self.dock, key).and_then(|t| t.object.as_deref_mut())
                {
                    tab.start_grant_apply(engine, statements, options);
                }
            }
        }
    }

    /// Applies the object tab's pending changes (Apply, ⌘S).
    pub(crate) fn apply_object_tab(&mut self, key: u64) {
        let engine = &self.engine;
        let Some(tab) = tab_mut(&mut self.dock, key).and_then(|t| t.object.as_deref_mut()) else {
            return;
        };
        let Some(state) = &mut tab.state else {
            return;
        };
        if tab.apply.is_some() || state.pending() == 0 {
            return;
        }
        if state
            .problems(&tab.editing.column_types)
            .iter()
            .any(|p| p.error)
        {
            tab.message = Some(("Fix the problems listed first".into(), true));
            return;
        }
        let conn = tab.conn.clone();
        match tab.statements(engine) {
            Ok(statements) => {
                let options = tab.apply_options();
                self.request_ddl(&conn, statements, options, AfterApply::Tab(key));
            }
            Err(e) => tab.message = Some((e.to_string(), true)),
        }
    }

    /// Whether the active tab is an object tab.
    pub(crate) fn active_is_object(&mut self) -> bool {
        self.active_tab().is_some_and(|t| t.object.is_some())
    }

    /// Handles an object tab's editor action.
    pub(crate) fn object_action(&mut self, key: u64, action: ObjectEditorAction) {
        let engine = &self.engine;
        let dialects = &self.dialects;
        let Some(host) = tab_mut(&mut self.dock, key) else {
            return;
        };
        let rows_pending = host.grid_pending();
        let Some(tab) = host.object.as_deref_mut() else {
            return;
        };
        match action {
            ObjectEditorAction::Preview => {
                let statements = tab.statements(engine);
                self.objects.preview = Some((key, statements));
            }
            ObjectEditorAction::Apply => self.apply_object_tab(key),
            ObjectEditorAction::Revert => tab.message = None,
            ObjectEditorAction::Refresh if tab.pending() + rows_pending > 0 => {
                self.objects.discard = Some((key, Discard::Refresh));
            }
            ObjectEditorAction::Refresh => tab.reload(engine),
            ObjectEditorAction::OpenData => {
                let (conn, database, title) = (tab.conn.clone(), tab.database(), tab.title());
                match tab.select_sql(engine) {
                    Some(sql) => self.open_and_run(title, &conn, database, &sql),
                    None => self.notify("Cannot open this object's data"),
                }
            }
            ObjectEditorAction::GenerateSql(template) => {
                let before = tab.state.as_ref().and_then(|s| s.before.clone());
                let (conn, database, name) = (tab.conn.clone(), tab.database(), tab.title());
                if let Some(def) = before {
                    match engine.generate_sql(&conn, template, &def) {
                        Ok(sql) => {
                            let title = format!("{} {name}", template_label(template));
                            self.open_sql_tab(title, &conn, database, &sql);
                        }
                        Err(e) => self.notify(e.message),
                    }
                }
            }
            ObjectEditorAction::PickTable => tab.load_tables(engine, dialects.get(&tab.conn)),
            ObjectEditorAction::LoadData => self.load_object_data(key),
            ObjectEditorAction::LoadPermissions => tab.load_permissions(engine),
            ObjectEditorAction::LoadStats => tab.load_stats(engine),
        }
    }

    /// Applies the object tab's grant changes (its Permissions' "Apply grants").
    pub(crate) fn apply_object_grants(&mut self, key: u64) {
        let engine = &self.engine;
        let Some(tab) = tab_mut(&mut self.dock, key).and_then(|t| t.object.as_deref()) else {
            return;
        };
        let conn = tab.conn.clone();
        let transactional = tab.editing.transactional_ddl;
        match tab.grant_statements(engine) {
            Ok(statements) if statements.is_empty() => {}
            Ok(statements) => {
                let options = ApplyOptions {
                    transactional,
                    ..Default::default()
                };
                self.request_ddl(&conn, statements, options, AfterApply::Grants(key));
            }
            Err(e) => self.notify(e.to_string()),
        }
    }

    /// Runs the object's first page as the hosting tab's run: the Data sub-tab shows it
    /// with the result tabs' grid, edit buffer, changes bar and changeset apply.
    fn load_object_data(&mut self, key: u64) {
        let engine = &self.engine;
        let Some(host) = tab_mut(&mut self.dock, key) else {
            return;
        };
        if host.grid_pending() > 0 {
            // A new page would drop the row edits of this one.
            self.objects.discard = Some((key, Discard::Data));
            return;
        }
        let Some(object) = host.object.as_deref() else {
            return;
        };
        let (conn, sql) = (object.conn.clone(), object.select_sql(engine));
        match sql {
            Some(sql) => {
                host.run_page(engine, &conn, sql);
            }
            None => self.notify("Cannot open this object's data"),
        }
    }

    /// Opens `sql` in a new editor tab on `conn` without running it.
    fn open_sql_tab(&mut self, title: String, conn: &str, database: Option<String>, sql: &str) {
        let key = self.new_tab(Some(title), Some(conn.to_string()), sql);
        if let Some(tab) = tab_mut(&mut self.dock, key) {
            tab.database = database;
        }
    }

    /// Follows explorer jobs and draws the object dialogs (every frame).
    pub(crate) fn objects_ui(&mut self, ctx: &egui::Context) {
        self.poll_object_jobs(ctx);
        self.blocked_database_ui(ctx);
        self.create_ui(ctx);
        self.destructive_ui(ctx);
        self.confirm_ddl_ui(ctx);
        self.preview_ui(ctx);
        self.discard_ui(ctx);
    }

    /// Asks before the stale Data page of object tab `key` drops its row edits.
    pub(crate) fn ask_discard_data(&mut self, key: u64) {
        if self.objects.discard.is_none() {
            self.objects.discard = Some((key, Discard::Data));
        }
    }

    /// Whether closing tab `key` must first ask to discard its pending edits; asks if so.
    pub(crate) fn ask_discard_on_close(&mut self, key: u64) -> bool {
        let pending = match tab_mut(&mut self.dock, key) {
            Some(t) => {
                let rows = if t.object.is_some() {
                    t.grid_pending()
                } else {
                    0
                };
                rows + t.object.as_deref_mut().map_or(0, |o| o.pending())
                    + t.admin.as_deref().map_or(0, |a| a.pending())
            }
            None => 0,
        };
        if pending > 0 {
            self.objects.discard = Some((key, Discard::Close));
        }
        pending > 0
    }

    /// What the discard question calls the edits of tab `key`.
    fn discard_name(&mut self, key: u64) -> Option<String> {
        let tab = tab_mut(&mut self.dock, key)?;
        if let Some(object) = tab.object.as_deref() {
            return Some(object.title());
        }
        match tab.admin.as_deref()? {
            crate::admin::AdminTab::Users(users) => Some(users.edited_name()),
            _ => None,
        }
    }

    fn discard_ui(&mut self, ctx: &egui::Context) {
        let Some((key, then)) = self.objects.discard.clone() else {
            return;
        };
        let Some(name) = self.discard_name(key) else {
            self.objects.discard = None;
            return;
        };
        let answer = confirm_modal(
            ctx,
            ConfirmSpec::new(
                Id::new("discard_object_edits"),
                theme::icons::UNDO,
                "Discard changes",
                "Discard",
            ),
            |ui| {
                ui.label(format!("Discard the pending changes of {name}?"));
            },
            |_| {},
        );
        let Some(discard) = answer else {
            return;
        };
        self.objects.discard = None;
        if !discard {
            return;
        }
        match then {
            Discard::Refresh => {
                let engine = &self.engine;
                if let Some(host) = tab_mut(&mut self.dock, key) {
                    host.close_run(engine);
                    if let Some(tab) = host.object.as_deref_mut() {
                        tab.reload(engine);
                    }
                }
            }
            Discard::Data => {
                if let Some(host) = tab_mut(&mut self.dock, key) {
                    host.close_run(&self.engine);
                }
                self.load_object_data(key);
            }
            // The Data sub-tab runs on the tab's own session: its transaction is still
            // to end.
            Discard::Close => self.close_without_discard(key),
            Discard::Users(action) => {
                let engine = &self.engine;
                if let Some(crate::admin::AdminTab::Users(users)) =
                    tab_mut(&mut self.dock, key).and_then(|t| t.admin.as_deref_mut())
                {
                    users.perform(engine, action);
                }
            }
        }
    }

    fn poll_object_jobs(&mut self, ctx: &egui::Context) {
        let jobs = std::mem::take(&mut self.objects.jobs);
        for (id, job) in jobs {
            let state = self.engine.job(id);
            if state.as_ref().is_some_and(|s| s.outcome.is_none()) {
                self.objects.jobs.push((id, job));
                continue;
            }
            self.engine.forget_job(id);
            match (state.and_then(|s| s.outcome), job) {
                (
                    Some(JobOutcome::Object(def)),
                    ExplorerJob::Generate {
                        conn,
                        template,
                        title,
                        database,
                    },
                ) => match self.engine.generate_sql(&conn, template, &def) {
                    Ok(sql) => self.open_sql_tab(title, &conn, database, &sql),
                    Err(e) => self.notify(e.message),
                },
                (Some(JobOutcome::Object(def)), ExplorerJob::CopyDdl { conn }) => {
                    match self.engine.create_ddl(&conn, &def) {
                        Ok(ddl) => {
                            ctx.copy_text(ddl);
                            self.notify_as(ToastKind::Success, "DDL copied");
                        }
                        Err(e) => self.notify(e.message),
                    }
                }
                (Some(JobOutcome::DdlApplied(report)), ExplorerJob::Applied { what }) => {
                    if report.ok() {
                        self.notify_as(ToastKind::Success, what);
                    } else {
                        self.notify(report.summary());
                    }
                }
                (Some(JobOutcome::DdlApplied(report)), ExplorerJob::DropDatabase(request)) => {
                    if report.ok() {
                        self.notify_as(ToastKind::Success, request.what);
                    } else {
                        self.database_drop_failed(request, report.summary());
                    }
                }
                (Some(JobOutcome::Failed(e)), ExplorerJob::DropDatabase(request)) => {
                    self.database_drop_failed(request, e.message);
                }
                (Some(JobOutcome::Failed(e)), _) => self.notify(e.message),
                _ => {}
            }
        }
    }

    fn create_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.objects.create else {
            return;
        };
        if dialog.needs_tables() {
            let (conn, schema) = (dialog.conn.clone(), dialog.schema.clone());
            let tables = self.catalog_tables(&conn, schema.as_deref());
            if let Some(dialog) = &mut self.objects.create {
                dialog.tables = tables;
            }
        }
        let Some(dialog) = &mut self.objects.create else {
            return;
        };
        let Some(create) = dialog.show(ctx) else {
            return;
        };
        let Some(dialog) = self.objects.create.take() else {
            return;
        };
        if !create {
            return;
        }
        let Some(editing) = self.editing_for(&dialog.conn) else {
            return;
        };
        let Some(def) = dialog.definition() else {
            return;
        };
        let tab = ObjectTab::create(
            &dialog.conn,
            def,
            dialog.database.clone(),
            editing,
            dialog.refresh.clone(),
        );
        self.push_object_tab(&dialog.conn, tab);
    }

    fn destructive_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.objects.destructive else {
            return;
        };
        let syntax = self
            .dialects
            .get(&dialog.conn)
            .map_or_else(SqlSyntax::ansi, |d| d.syntax());
        let verb = dialog.verb;
        let heading = dialog.heading.clone();
        let icon = if verb == "Drop" {
            theme::icons::DROP
        } else {
            theme::icons::TRUNCATE
        };
        let answer = confirm_modal(
            ctx,
            ConfirmSpec::new(Id::new("destructive_object"), icon, &heading, verb).width(560.0),
            |ui| {
                ui.label(&dialog.title);
                ui.add_space(Spacing::S);
                if let Some((_, on)) = &mut dialog.cascade {
                    ui.checkbox(on, dialog.cascade_label);
                }
                let statements = match &dialog.cascade {
                    Some((cascade, true)) => cascade,
                    _ => &dialog.statements,
                };
                sql_view(ui, "destructive_sql", &script(statements), &syntax, 160.0);
            },
            |_| {},
        );
        let Some(run) = answer else {
            return;
        };
        let Some(dialog) = self.objects.destructive.take() else {
            return;
        };
        if run {
            let statements = match dialog.cascade {
                Some((cascade, true)) => cascade,
                _ => dialog.statements,
            };
            let then = match dialog.drop_database {
                Some(database) => AfterApply::DropDatabase {
                    database,
                    what: dialog.done,
                },
                None => AfterApply::Explorer(dialog.done),
            };
            self.start_ddl(&dialog.conn, statements, dialog.options, then);
        }
    }

    fn confirm_ddl_ui(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.objects.confirm else {
            return;
        };
        let mut show_all = false;
        let answer = confirm_modal(
            ctx,
            ConfirmSpec::new(
                Id::new("confirm_ddl"),
                theme::icons::SQL,
                "Apply DDL",
                "Apply",
            )
            .width(560.0),
            |ui| prompt_lines(ui, &pending.prompt, &pending.lines),
            |f| {
                // The lines above are cut: the whole script is one click away.
                if matches!(pending.then, AfterApply::Tab(_)) && f.secondary("Show full DDL", true)
                {
                    show_all = true;
                }
            },
        );
        if show_all
            && let Some(ConfirmDdl {
                then: AfterApply::Tab(key),
                statements,
                ..
            }) = self.objects.confirm.take()
        {
            // The preview's Apply asks again.
            self.objects.preview = Some((key, Ok(statements)));
            return;
        }
        let Some(run) = answer else {
            return;
        };
        let Some(pending) = self.objects.confirm.take() else {
            return;
        };
        if run {
            self.start_ddl(
                &pending.conn,
                pending.statements,
                pending.options,
                pending.then,
            );
        }
    }

    fn preview_ui(&mut self, ctx: &egui::Context) {
        let Some(key) = self.objects.preview.as_ref().map(|(k, _)| *k) else {
            return;
        };
        let syntax = self
            .object_tab(key)
            .map(|t| t.conn.clone())
            .map_or_else(SqlSyntax::ansi, |c| self.syntax_of(&c));
        let Some((_, statements)) = &self.objects.preview else {
            return;
        };
        let mut apply = false;
        let text = statements.as_ref().ok().map(|s| script(s));
        let response = theme::Dialog::new("preview_ddl", theme::icons::SQL, "Pending DDL")
            .subtitle("The statements Apply runs")
            .width(680.0)
            .show(ctx, |d| {
                d.body(|ui| match (&text, statements) {
                    (Some(text), _) => sql_view(ui, "preview_sql", text, &syntax, 360.0),
                    (None, Err(e)) => {
                        ui.colored_label(theme::palette(ui.ctx()).error, e.to_string());
                    }
                    (None, Ok(_)) => {}
                });
                d.footer(|f| {
                    if let Some(text) = &text {
                        if f.secondary("Copy", true) {
                            f.left().ctx().copy_text(text.clone());
                        }
                        apply = f.primary("Apply", None);
                    }
                    f.cancel_as("Close");
                });
            });
        if apply || response.closed {
            self.objects.preview = None;
        }
        if apply {
            self.apply_object_tab(key);
        }
    }
}

impl CreateDialog {
    fn needs_tables(&self) -> bool {
        matches!(self.kind, ObjectKind::Index | ObjectKind::Trigger) && self.tables.is_empty()
    }
}
