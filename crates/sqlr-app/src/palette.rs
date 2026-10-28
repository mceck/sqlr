//! The palettes: ⌘P over the recently opened relations and every relation of the live
//! connections, ⌘⇧P over app commands, snippets and history. Items are rebuilt only when
//! what they list changes.

use sqlr_ui::ToastKind;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use std::collections::HashSet;

use sqlr_core::{
    CatalogNeed, HistoryEntry, HistoryQuery, Node, NodeKind, NodePath, ObjectKind, RecentObject,
    RelationKind,
};
use sqlr_engine::{ConnCatalog, HistoryChannel, LoadState};
use sqlr_ui::{
    PaletteGroup, PaletteItem, PaletteMode, PaletteOutput, PaletteState, first_line, format_when,
    palette,
};

use crate::app::SqlrApp;

/// History matches listed in ⌘⇧P.
const HISTORY_ITEMS: usize = 20;
/// Chars of a history entry's first line in its label.
const HISTORY_LABEL_CHARS: usize = 80;

/// What a palette item does.
#[derive(Clone, Debug)]
pub enum PaletteTarget {
    Object {
        conn: String,
        /// The server database, when it is not the connection's own.
        database: Option<String>,
        schema: String,
        name: String,
        kind: RelationKind,
    },
    /// Open a collection or index (document connections) in a document tab.
    Documents {
        conn: String,
        node: Node,
    },
    Command(AppCommand),
    /// Insert this SQL into the active editor.
    Snippet(String),
    History(HistoryEntry),
}

#[derive(Clone, Debug)]
pub enum AppCommand {
    NewTab,
    CloseTab,
    NewConnection,
    ImportConnections,
    ExportConnections,
    Settings,
    DriverManager,
    Quit,
    About,
    Run,
    RunScript,
    Stop,
    ToggleValuePanel,
    ToggleTerminal,
    ToggleHistory,
    ToggleSoftWrap,
    SaveSnippet,
    Connect(String),
    Disconnect(String),
    Commit,
    Rollback,
    Profiles,
    /// "New <kind>…" on a connection.
    NewObject(String, ObjectKind),
    /// Open the selected explorer node in the object editor.
    EditSelected(String, NodePath),
    /// Open an administration panel of a connection.
    OpenAdmin(String, sqlr_ui::AdminPanel),
    ExportResult,
    TransferData,
    ShowOperations,
    CompareSchemas,
    Explain,
    ExplainAnalyze,
    /// Editing commands drive the focused widget with a synthetic egui event.
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    /// Opens the Help > Keyboard Shortcuts window.
    Shortcuts,
    /// Opens the Diagnostics window.
    Diagnostics,
    /// Asks GitHub for a newer release, from Help.
    CheckUpdates,
    /// Opens Report a Problem.
    ReportProblem,
    /// "New index…" on the collection (or Indices folder) selected in the explorer.
    NewIndex(String, NodePath),
    /// A query tab on the active tab's connection, else the explorer's selected one.
    NewTabOnConnection,
    /// Opens ⌘P.
    OpenObject,
    /// Opens ⌘⇧P.
    Commands,
    /// Applies the active tab's pending changes.
    Apply,
    /// Drops the active tab's pending changes.
    Discard,
    /// Reloads what the active tab shows.
    Refresh,
    /// Reopens the most recently closed tab.
    ReopenClosedTab,
    /// Presses the action's current shortcut for the focused widget: the editor, grid and
    /// explorer actions that have no command of their own (Format SQL, Find, Rename…).
    Key(sqlr_core::ActionId),
}

/// Kinds "New …" offers in ⌘⇧P, in this order.
const PALETTE_NEW: [ObjectKind; 6] = [
    ObjectKind::Table,
    ObjectKind::View,
    ObjectKind::MaterializedView,
    ObjectKind::Sequence,
    ObjectKind::Function,
    ObjectKind::Procedure,
];

#[derive(Default)]
pub struct AppPalette {
    pub(crate) state: PaletteState,
    items: Vec<PaletteItem>,
    /// Parallel to `items`.
    targets: Vec<PaletteTarget>,
    /// The revision `items` were built for.
    built: Option<u64>,
}

/// The title of a tab showing a relation's data: its name, with its database when it is
/// not the connection's own.
pub(crate) fn data_tab_title(name: &str, database: Option<&str>) -> String {
    match database {
        Some(db) => format!("{name} ({db})"),
        None => name.to_string(),
    }
}

/// A relation's Palette label: `schema.name`, or `database.schema.name` in another database.
fn relation_label(database: Option<&str>, schema: &str, name: &str) -> String {
    match database {
        Some(db) => format!("{db}.{schema}.{name}"),
        None => format!("{schema}.{name}"),
    }
}

fn relation_folder(kind: RelationKind) -> (&'static str, &'static str, NodeKind) {
    match kind {
        RelationKind::Table => ("tables", "table", NodeKind::Table),
        RelationKind::View => ("views", "view", NodeKind::View),
        RelationKind::MaterializedView => (
            "materialized views",
            "materialized view",
            NodeKind::MaterializedView,
        ),
    }
}

/// The explorer node of a catalog relation (`<schema>/<folder>/<name>`, under
/// `<database>/` when it is in another database).
fn relation_node(
    database: Option<String>,
    schema: String,
    name: String,
    kind: RelationKind,
) -> Node {
    let (folder, _, node_kind) = relation_folder(kind);
    let mut path: Vec<String> = database.into_iter().collect();
    path.extend([schema, folder.to_string(), name.clone()]);
    Node {
        path: NodePath(path),
        name,
        kind: node_kind,
        has_children: true,
        detail: None,
    }
}

impl SqlrApp {
    /// The collections and indices of the live document connections that the explorer has
    /// loaded: `(conn, label, node)`.
    fn doc_palette_nodes(&self) -> Vec<(String, String, Node)> {
        let mut out = Vec::new();
        for cfg in &self.connections {
            if !self.is_live(&cfg.id) || !self.is_document(&cfg.id) {
                continue;
            }
            let mut stack = vec![(NodePath::root(), 0)];
            while let Some((path, depth)) = stack.pop() {
                let Some(LoadState::Ready(nodes)) = self.engine.children(&cfg.id, &path) else {
                    continue;
                };
                for node in nodes {
                    if matches!(node.kind, NodeKind::Table | NodeKind::View)
                        && let Some((target, _)) =
                            self.engine.open_doc_target(&cfg.id, &node.path, 0)
                    {
                        let label = match &target.database {
                            Some(db) => format!("{db}.{}", target.collection),
                            None => target.collection.clone(),
                        };
                        out.push((cfg.id.clone(), label, node));
                    } else if depth < 2 {
                        stack.push((node.path.clone(), depth + 1));
                    }
                }
            }
        }
        out
    }

    pub(crate) fn open_palette(&mut self, mode: PaletteMode) {
        self.palette.state.open(mode);
        self.palette.built = None;
    }

    /// The other server databases of a live connection whose explorer lists databases
    /// (PostgreSQL with "Show all databases"), once its root is loaded.
    fn palette_databases(&self, conn: &str) -> Vec<String> {
        match self.engine.children(conn, &NodePath::root()) {
            Some(LoadState::Ready(nodes)) => nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Database)
                .filter_map(|n| self.engine.database_for_path(conn, &n.path))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The catalogs ⌘P lists for a live relational connection: its own, then each other
    /// database's, with that database's name.
    fn palette_catalogs(&self, conn: &str) -> Vec<(Option<String>, Arc<ConnCatalog>)> {
        let own = self.engine.catalog(conn).map(|c| (None, c));
        let others = self.palette_databases(conn).into_iter().filter_map(|db| {
            let catalog = self.engine.catalog_in(conn, Some(&db))?;
            Some((Some(db), catalog))
        });
        own.into_iter().chain(others).collect()
    }

    /// Asks for what ⌘P lists and is missing, on every live relational connection: the
    /// explorer root (for its databases), each database's schema list, and the catalog of
    /// every schema that is not a system one. Loads already done or running are no-ops.
    fn palette_load_catalogs(&self) {
        for cfg in &self.connections {
            let conn = cfg.id.as_str();
            if !self.is_live(conn) || self.is_nosql(conn) {
                continue;
            }
            let Some(dialect) = self.dialects.get(conn) else {
                continue;
            };
            // Asked once: a failed root stays failed, as in the Explorer.
            if self.engine.children(conn, &NodePath::root()).is_none() {
                self.engine.browse(conn, NodePath::root());
            }
            let databases =
                std::iter::once(None).chain(self.palette_databases(conn).into_iter().map(Some));
            for database in databases {
                let database = database.as_deref();
                self.engine
                    .load_catalog_in(conn, database, CatalogNeed::Schemas);
                let Some(catalog) = self.engine.catalog_in(conn, database) else {
                    continue;
                };
                let Some(LoadState::Ready(schemas)) = &catalog.schemas else {
                    continue;
                };
                for schema in schemas.iter().filter(|s| !dialect.is_system_schema(s)) {
                    if !catalog.schema.contains_key(schema) {
                        self.engine.load_catalog_in(
                            conn,
                            database,
                            CatalogNeed::Schema(schema.clone()),
                        );
                    }
                }
            }
        }
    }

    pub(crate) fn palette_ui(&mut self, ctx: &egui::Context) {
        let Some(mode) = self.palette.state.mode else {
            return;
        };
        let active_conn = self.active_tab().and_then(|t| t.conn.clone());
        let revision = self.palette_revision(mode, active_conn.as_deref());
        if self.palette.built != Some(revision) {
            if mode == PaletteMode::Objects {
                self.palette_load_catalogs();
            }
            let mac = ctx.os() == egui::os::OperatingSystem::Mac;
            let (items, targets) = self.palette_items(mode, active_conn.as_deref(), mac);
            self.palette.items = items;
            self.palette.targets = targets;
            self.palette.built = Some(revision);
        }
        let output = palette(ctx, &mut self.palette.state, &self.palette.items, revision);
        match output {
            PaletteOutput::QueryChanged if mode == PaletteMode::Commands => {
                let text = self.palette.state.query.trim().to_string();
                if !text.is_empty() {
                    let q = HistoryQuery {
                        text,
                        conn_id: None,
                        limit: HISTORY_ITEMS,
                    };
                    self.engine.search_history(HistoryChannel::Palette, q);
                }
            }
            PaletteOutput::Chosen(i) | PaletteOutput::EditChosen(i) => {
                // Back to the editor; a command that opens a tab focuses that one instead.
                if let Some(tab) = self.active_tab() {
                    tab.editor.request_focus();
                }
                if let Some(target) = self.palette.targets.get(i).cloned() {
                    if matches!(output, PaletteOutput::EditChosen(_))
                        && let PaletteTarget::Object {
                            conn,
                            database,
                            schema,
                            name,
                            kind,
                        } = &target
                        && self.editing_for(conn).is_some()
                    {
                        let node =
                            relation_node(database.clone(), schema.clone(), name.clone(), *kind);
                        self.open_object(conn, &node);
                    } else {
                        self.palette_choose(target, ctx);
                    }
                }
            }
            PaletteOutput::Closed => {
                if let Some(tab) = self.active_tab() {
                    tab.editor.request_focus();
                }
            }
            _ => {}
        }
    }

    /// Changes whenever the listed items would.
    fn palette_revision(&self, mode: PaletteMode, active_conn: Option<&str>) -> u64 {
        let mut h = DefaultHasher::new();
        (mode == PaletteMode::Objects).hash(&mut h);
        format!("{:?}", self.keymap.keymap).hash(&mut h);
        (active_conn, &self.tree.selected).hash(&mut h);
        self.engine.catalog_revision().hash(&mut h);
        if mode == PaletteMode::Objects {
            self.doc_palette_nodes().len().hash(&mut h);
            for o in self.engine.recent_objects().iter() {
                (&o.conn_id, &o.database, &o.schema, &o.name, o.used_at_ms).hash(&mut h);
            }
            for cfg in &self.connections {
                self.engine
                    .child_count(&cfg.id, &NodePath::root())
                    .hash(&mut h);
            }
        }
        for cfg in &self.connections {
            let admin = self.engine.admin_support(&cfg.id).is_some();
            (&cfg.id, &cfg.name, self.is_live(&cfg.id), admin).hash(&mut h);
        }
        if mode == PaletteMode::Commands {
            let snippets = self.engine.snippets();
            (Arc::as_ptr(&snippets) as usize, snippets.len()).hash(&mut h);
            if let Some(view) = self.engine.history(HistoryChannel::Palette)
                && let LoadState::Ready(entries) = &view.entries
            {
                (
                    Arc::as_ptr(entries) as usize,
                    entries.len(),
                    &view.query.text,
                )
                    .hash(&mut h);
            }
            self.palette.state.query.trim().is_empty().hash(&mut h);
        }
        h.finish()
    }

    /// The items of `mode`; shortcut hints in the macOS notation when `mac`.
    pub(crate) fn palette_items(
        &self,
        mode: PaletteMode,
        active_conn: Option<&str>,
        mac: bool,
    ) -> (Vec<PaletteItem>, Vec<PaletteTarget>) {
        let mut items: Vec<PaletteItem> = Vec::new();
        let mut targets = Vec::new();
        // Icon and shortcut of each command, in order; set once the items are built.
        let mut command_meta = Vec::new();
        let mut push = |label: String, detail: String, group, fuzzy, target| {
            items.push(PaletteItem {
                label,
                detail,
                group,
                fuzzy,
                icon: None,
                shortcut: None,
            });
            targets.push(target);
        };
        match mode {
            PaletteMode::Objects => {
                // The recently opened relations first, even of closed connections.
                let mut listed = HashSet::new();
                for recent in self.engine.recent_objects().iter() {
                    let Some(cfg) = self.connection(&recent.conn_id) else {
                        continue;
                    };
                    let RecentObject {
                        conn_id,
                        database,
                        schema,
                        name,
                        kind,
                        ..
                    } = recent.clone();
                    let (_, kind_label, _) = relation_folder(kind);
                    push(
                        relation_label(database.as_deref(), &schema, &name),
                        format!("{} · {kind_label}", cfg.name),
                        PaletteGroup::Recent,
                        true,
                        PaletteTarget::Object {
                            conn: conn_id.clone(),
                            database: database.clone(),
                            schema: schema.clone(),
                            name: name.clone(),
                            kind,
                        },
                    );
                    listed.insert((conn_id, database, schema, name));
                }
                // Then every relation of every live connection, sorted below.
                for cfg in &self.connections {
                    if !self.is_live(&cfg.id) {
                        continue;
                    }
                    let dialect = self.dialects.get(&cfg.id);
                    for (database, catalog) in self.palette_catalogs(&cfg.id) {
                        for state in catalog.schema.values() {
                            let LoadState::Ready(schema) = state else {
                                continue;
                            };
                            if dialect.is_some_and(|d| d.is_system_schema(&schema.schema)) {
                                continue;
                            }
                            for r in &schema.relations {
                                let key = (
                                    cfg.id.clone(),
                                    database.clone(),
                                    schema.schema.clone(),
                                    r.name.clone(),
                                );
                                if listed.contains(&key) {
                                    continue;
                                }
                                let (_, kind, _) = relation_folder(r.kind);
                                push(
                                    relation_label(database.as_deref(), &schema.schema, &r.name),
                                    format!("{} · {kind}", cfg.name),
                                    PaletteGroup::Object,
                                    true,
                                    PaletteTarget::Object {
                                        conn: cfg.id.clone(),
                                        database: database.clone(),
                                        schema: schema.schema.clone(),
                                        name: r.name.clone(),
                                        kind: r.kind,
                                    },
                                );
                            }
                        }
                    }
                }
                for (conn, label, node) in self.doc_palette_nodes() {
                    let name = self.connection(&conn).map_or("", |c| c.name.as_str());
                    let kind = if node.kind == NodeKind::View {
                        "view"
                    } else {
                        "collection"
                    };
                    push(
                        label,
                        format!("{name} · {kind}"),
                        PaletteGroup::Object,
                        true,
                        PaletteTarget::Documents { conn, node },
                    );
                }
            }
            PaletteMode::Commands => {
                let command = |label: &str, detail: &str, c: AppCommand| {
                    (label.to_string(), detail.to_string(), c)
                };
                let mut commands = vec![
                    command("New query tab", "", AppCommand::NewTab),
                    command(
                        "New query tab on the current connection",
                        "",
                        AppCommand::NewTabOnConnection,
                    ),
                    command("Go to a table or view…", "", AppCommand::OpenObject),
                    command("New connection", "", AppCommand::NewConnection),
                    command("Import connections…", "", AppCommand::ImportConnections),
                    command("Export connections…", "", AppCommand::ExportConnections),
                    command("Settings…", "", AppCommand::Settings),
                    command("Keyboard shortcuts", "", AppCommand::Shortcuts),
                    command("Check for updates…", "", AppCommand::CheckUpdates),
                    command("Driver Manager…", "", AppCommand::DriverManager),
                    command("Run statement", "", AppCommand::Run),
                    command("Run script", "", AppCommand::RunScript),
                    command("Stop", "", AppCommand::Stop),
                    command("Explain", "", AppCommand::Explain),
                    command("Explain analyze", "", AppCommand::ExplainAnalyze),
                    command("Commit", "", AppCommand::Commit),
                    command("Rollback", "", AppCommand::Rollback),
                    command("Apply pending changes", "", AppCommand::Apply),
                    command("Discard pending changes", "", AppCommand::Discard),
                    command("Refresh", "", AppCommand::Refresh),
                    command("Reopen closed tab", "", AppCommand::ReopenClosedTab),
                    command("Toggle value panel", "", AppCommand::ToggleValuePanel),
                    command("Toggle terminal", "", AppCommand::ToggleTerminal),
                    command("Toggle history panel", "", AppCommand::ToggleHistory),
                    command("Toggle soft wrap", "", AppCommand::ToggleSoftWrap),
                    command("Save as snippet…", "", AppCommand::SaveSnippet),
                    command("Connection profiles…", "", AppCommand::Profiles),
                    command("Export result…", "", AppCommand::ExportResult),
                    command("Transfer data…", "", AppCommand::TransferData),
                    command("Operations", "", AppCommand::ShowOperations),
                    command("Compare schemas…", "", AppCommand::CompareSchemas),
                ];
                // Object editors: create on the active tab's connection, edit the selection.
                if let Some(conn) = active_conn.filter(|c| self.is_live(c))
                    && let Some(editing) = self.editing_for(conn)
                {
                    for kind in PALETTE_NEW.iter().filter(|k| editing.create.contains(k)) {
                        let label = format!("New {}…", sqlr_ui::kind_label(*kind).to_lowercase());
                        let name = self.connection(conn).map_or("", |c| c.name.as_str());
                        commands.push(command(
                            &label,
                            name,
                            AppCommand::NewObject(conn.to_string(), *kind),
                        ));
                    }
                }
                if let Some((conn, path)) = &self.tree.selected
                    && !path.is_root()
                    && self.editing_for(conn).is_some()
                {
                    let name = path.last().unwrap_or_default();
                    commands.push(command(
                        "Edit object…",
                        name,
                        AppCommand::EditSelected(conn.clone(), path.clone()),
                    ));
                }
                // Document connections: a new index on the selected collection.
                if let Some(node) = self.selected_node()
                    && let Some((conn, _)) = &self.tree.selected
                    && crate::nosql::index_admin::node_menu(&self.engine, conn, &node)
                        .new
                        .contains(&ObjectKind::Index)
                {
                    commands.push(command(
                        &format!("New index on {}…", node.name),
                        "",
                        AppCommand::NewIndex(conn.clone(), node.path),
                    ));
                }
                // Administration panels of every connected connection.
                for cfg in &self.connections {
                    let Some(support) = self.engine.admin_support(&cfg.id) else {
                        continue;
                    };
                    for panel in sqlr_ui::AdminPanel::for_support(&support) {
                        commands.push(command(
                            &format!("{}: {}", panel.label(), cfg.name),
                            "",
                            AppCommand::OpenAdmin(cfg.id.clone(), panel),
                        ));
                    }
                }
                for cfg in &self.connections {
                    commands.push(if self.is_live(&cfg.id) {
                        command(
                            &format!("Disconnect: {}", cfg.name),
                            "",
                            AppCommand::Disconnect(cfg.id.clone()),
                        )
                    } else {
                        command(
                            &format!("Connect: {}", cfg.name),
                            "",
                            AppCommand::Connect(cfg.id.clone()),
                        )
                    });
                }
                for (label, detail, c) in commands {
                    if matches!(
                        c,
                        AppCommand::Run | AppCommand::RunScript | AppCommand::Stop
                    ) && !active_conn.is_some_and(|conn| self.is_live(conn))
                    {
                        continue;
                    }
                    let shortcut = c.action_id().and_then(|a| self.keymap.keymap.label(a, mac));
                    command_meta.push((c.icon(), shortcut));
                    push(
                        label,
                        detail,
                        PaletteGroup::Command,
                        true,
                        PaletteTarget::Command(c),
                    );
                }
                for s in self.engine.snippets().iter() {
                    push(
                        format!("Snippet: {}", s.name),
                        s.folder.clone(),
                        PaletteGroup::Snippet,
                        true,
                        PaletteTarget::Snippet(s.sql.clone()),
                    );
                }
                let query = self.palette.state.query.trim();
                if !query.is_empty()
                    && let Some(view) = self.engine.history(HistoryChannel::Palette)
                    && let LoadState::Ready(entries) = &view.entries
                {
                    for e in entries.iter() {
                        let conn = self
                            .connection(&e.conn_id)
                            .map_or(e.conn_id.as_str(), |c| c.name.as_str());
                        push(
                            format!("History: {}", first_line(&e.sql, HISTORY_LABEL_CHARS)),
                            format!("{conn} · {}", format_when(e.started_at_ms)),
                            PaletteGroup::History,
                            false,
                            PaletteTarget::History(e.clone()),
                        );
                    }
                }
            }
        }
        let commands = items
            .iter_mut()
            .filter(|item| item.group == PaletteGroup::Command);
        for (item, (icon, shortcut)) in commands.zip(command_meta) {
            item.icon = Some(icon);
            item.shortcut = shortcut;
        }
        if mode == PaletteMode::Objects {
            // Recent objects keep their order (most recent first); the rest sort by
            // connection, then name.
            let mut pairs: Vec<_> = items.into_iter().zip(targets).collect();
            let recent = pairs
                .iter()
                .take_while(|p| p.0.group == PaletteGroup::Recent)
                .count();
            pairs[recent..].sort_by(|a, b| {
                a.0.detail
                    .cmp(&b.0.detail)
                    .then_with(|| a.0.label.cmp(&b.0.label))
            });
            return pairs.into_iter().unzip();
        }
        (items, targets)
    }

    fn palette_choose(&mut self, target: PaletteTarget, ctx: &egui::Context) {
        match target {
            PaletteTarget::Object {
                conn,
                database,
                schema,
                name,
                kind,
            } => self.open_relation(&conn, database, schema, name, kind),
            PaletteTarget::Documents { conn, node } => self.open_doc_node(&conn, &node),
            PaletteTarget::Command(c) => self.run_command(c, ctx),
            PaletteTarget::Snippet(sql) => self.insert_into_active(&sql),
            PaletteTarget::History(e) => self.open_history_entry(&e),
        }
    }

    /// The node selected in the explorer, if its parent is loaded.
    fn selected_node(&self) -> Option<Node> {
        let (conn, path) = self.tree.selected.as_ref()?;
        let parent = NodePath(path.segments()[..path.segments().len().checked_sub(1)?].to_vec());
        match self.engine.children(conn, &parent) {
            Some(LoadState::Ready(nodes)) => nodes.into_iter().find(|n| n.path == *path),
            _ => None,
        }
    }

    /// Opens a catalog relation's data in a new tab, connecting if needed, and remembers it
    /// as recently opened.
    fn open_relation(
        &mut self,
        conn: &str,
        database: Option<String>,
        schema: String,
        name: String,
        kind: RelationKind,
    ) {
        let node = relation_node(database.clone(), schema.clone(), name, kind);
        let sql = self
            .dialects
            .get(conn)
            .and_then(|d| d.select_all(&node, None));
        let Some(sql) = sql else {
            self.notify(format!("Cannot open the data of {}", node.name));
            return;
        };
        self.remember_relation(conn, database.clone(), schema, node.name.clone(), kind);
        let title = data_tab_title(&node.name, database.as_deref());
        self.open_and_run(title, conn, database, &sql);
    }

    fn remember_relation(
        &self,
        conn: &str,
        database: Option<String>,
        schema: String,
        name: String,
        kind: RelationKind,
    ) {
        self.engine.touch_recent_object(RecentObject {
            conn_id: conn.to_string(),
            database,
            schema,
            name,
            kind,
            used_at_ms: 0,
        });
    }

    /// Remembers an explorer node whose data was just opened as a recently opened relation
    /// (tables, views and materialized views of relational connections). `database` is the
    /// one the data is read from.
    pub(crate) fn remember_opened(&self, conn: &str, database: Option<String>, node: &Node) {
        let Some(kind) = RelationKind::of_node(node.kind) else {
            return;
        };
        let Some(dialect) = self.dialects.get(conn) else {
            return;
        };
        let (Some(schema), _) = self.location(conn, Some(node)) else {
            return;
        };
        // Where databases are schemas (MySQL), the schema already names the database.
        let database = database.filter(|_| !dialect.database_is_schema());
        self.remember_relation(conn, database, schema, node.name.clone(), kind);
    }

    pub(crate) fn run_command(&mut self, command: AppCommand, ctx: &egui::Context) {
        match command {
            AppCommand::NewTab => {
                self.new_tab(None, None, "");
            }
            AppCommand::CloseTab => {
                if let Some(key) = self.active_key() {
                    self.request_close_tab(key);
                }
            }
            AppCommand::NewConnection => self.open_new_connection(),
            AppCommand::ImportConnections => self.open_client_import(),
            AppCommand::ExportConnections => self.open_client_export(),
            AppCommand::DriverManager => self.open_driver_manager(),
            AppCommand::TransferData => self.open_transfer_wizard(None),
            AppCommand::ShowOperations => self.show_operations = true,
            AppCommand::CompareSchemas => self.open_compare(None, None, None),
            AppCommand::Explain => self.explain_active(false),
            AppCommand::ExplainAnalyze => self.explain_active(true),
            AppCommand::Settings => self.open_preferences(sqlr_core::Level::Global),
            AppCommand::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            AppCommand::About => {
                #[cfg(target_os = "macos")]
                crate::native_menu::show_about();
            }
            AppCommand::Run => self.run_active(),
            AppCommand::RunScript => self.run_script_active(),
            AppCommand::Stop => self.stop_active(),
            AppCommand::ToggleValuePanel => self.show_value_panel = !self.show_value_panel,
            AppCommand::ToggleTerminal => self.terminal.toggle(),
            AppCommand::ToggleHistory => self.show_history = !self.show_history,
            AppCommand::ToggleSoftWrap => self.toggle_soft_wrap(ctx),
            AppCommand::SaveSnippet => self.save_as_snippet(),
            AppCommand::Connect(id) => {
                self.connect(&id);
            }
            AppCommand::Disconnect(id) => self.request_disconnect(&id),
            AppCommand::Commit => self.commit_active(true),
            AppCommand::Rollback => self.commit_active(false),
            AppCommand::Profiles => self.open_profiles(),
            AppCommand::NewObject(conn, kind) => self.new_object(&conn, None, kind),
            AppCommand::EditSelected(conn, path) => {
                let parent = NodePath(path.segments()[..path.segments().len() - 1].to_vec());
                let node = match self.engine.children(&conn, &parent) {
                    Some(LoadState::Ready(nodes)) => nodes.into_iter().find(|n| n.path == path),
                    _ => None,
                };
                match node {
                    Some(node) => self.open_object(&conn, &node),
                    None => {
                        self.notify_as(ToastKind::Info, "Select an object in the explorer first")
                    }
                }
            }
            AppCommand::OpenAdmin(conn, panel) => self.open_admin(&conn, panel),
            AppCommand::ExportResult => {
                let active = self.active_key();
                let engine = &self.engine;
                let target = active.and_then(|key| {
                    let tab = crate::app::tab_mut(&mut self.dock, key)?;
                    Some((key, tab.shown_query(engine)?))
                });
                match target {
                    Some((tab, q)) => self.open_result_export(tab, q),
                    None => self.notify_as(ToastKind::Info, "Run a query first"),
                }
            }
            AppCommand::Undo => send_key(ctx, egui::Key::Z, egui::Modifiers::COMMAND),
            AppCommand::Redo => send_key(
                ctx,
                egui::Key::Z,
                egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
            ),
            AppCommand::Cut => send_event(ctx, egui::Event::Cut),
            AppCommand::Copy => send_event(ctx, egui::Event::Copy),
            AppCommand::Paste => {
                // The clipboard is owned by the integration, so ask it for the paste; the
                // resulting `Event::Paste` arrives with the next frame's input.
                sqlr_ui::keymap::request_paste(ctx);
                ctx.request_repaint();
            }
            AppCommand::Delete => send_key(ctx, egui::Key::Delete, egui::Modifiers::NONE),
            AppCommand::SelectAll => send_key(ctx, egui::Key::A, egui::Modifiers::COMMAND),
            AppCommand::Shortcuts => self.shortcuts.open = true,
            AppCommand::Diagnostics => {
                self.diagnostics_ui.open = true;
                self.diagnostics_ui.loaded = false;
            }
            AppCommand::CheckUpdates => self.check_updates_now(ctx),
            AppCommand::ReportProblem => {
                self.diagnostics_ui.report = Some(crate::diagnostics::ReportProblemDialog::new(
                    self.settings.include_sql_in_diagnostics,
                ));
            }
            AppCommand::NewTabOnConnection => self.new_editor_tab(),
            AppCommand::OpenObject => self.open_palette(PaletteMode::Objects),
            AppCommand::Commands => self.open_palette(PaletteMode::Commands),
            AppCommand::Apply => self.apply_active(),
            AppCommand::Discard => self.discard_active(),
            AppCommand::Refresh => self.refresh_active(),
            AppCommand::ReopenClosedTab => self.reopen_closed_tab(),
            AppCommand::Key(action) => {
                let mac = ctx.os() == egui::os::OperatingSystem::Mac;
                if let Some(shortcut) = self
                    .keymap
                    .keymap
                    .chord_for(action)
                    .and_then(|c| sqlr_ui::keymap::chord_shortcut(&c, mac))
                {
                    send_key(ctx, shortcut.logical_key, shortcut.modifiers);
                }
            }
            AppCommand::NewIndex(conn, path) => {
                let parent =
                    NodePath(path.segments()[..path.segments().len().saturating_sub(1)].to_vec());
                let node = match self.engine.children(&conn, &parent) {
                    Some(LoadState::Ready(nodes)) => nodes.into_iter().find(|n| n.path == path),
                    _ => None,
                };
                if let Some(node) = node {
                    self.open_index_dialog(&conn, &node);
                }
            }
        }
    }
}

/// Feeds a synthetic key press to the focused widget, so a menu command goes through the
/// widget's own handling (the editor's undo stack, multi-caret selection, dialogs' text
/// fields) instead of duplicating it.
fn send_key(ctx: &egui::Context, key: egui::Key, modifiers: egui::Modifiers) {
    send_event(
        ctx,
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        },
    );
}

fn send_event(ctx: &egui::Context, event: egui::Event) {
    ctx.input_mut(|i| i.events.push(event));
    ctx.request_repaint();
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use sqlr_core::{ActionId, Chord, Keymap};
    use sqlr_engine::{DriverRegistry, Engine};
    use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
    use sqlr_ui::{PaletteMode, theme::icons};

    use crate::{AppDeps, SqlrApp};

    /// An app with no connections and no drivers.
    pub(crate) fn bare_app() -> (SqlrApp, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let repo =
            ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
        let deps = AppDeps {
            check_updates: false,
            engine: Engine::new(DriverRegistry::default(), Arc::new(|| {})),
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
        (SqlrApp::new(deps, &egui::Context::default()), dir)
    }

    #[test]
    fn the_palette_offers_pending_changes_refresh_and_reopen_with_their_chords() {
        let (app, _dir) = bare_app();
        let (items, _) = app.palette_items(PaletteMode::Commands, None, true);
        let shortcut = |label: &str| {
            items
                .iter()
                .find(|i| i.label == label)
                .unwrap_or_else(|| panic!("{label} is listed"))
                .shortcut
                .clone()
        };
        assert_eq!(shortcut("Apply pending changes").as_deref(), Some("⌘S"));
        assert_eq!(
            shortcut("Discard pending changes"),
            None,
            "no default chord"
        );
        assert_eq!(shortcut("Refresh").as_deref(), Some("F5"));
        assert_eq!(shortcut("Reopen closed tab").as_deref(), Some("⇧⌘T"));
    }

    #[test]
    fn connectionless_palette_does_not_offer_query_execution() {
        let (app, _dir) = bare_app();
        let (items, _) = app.palette_items(PaletteMode::Commands, None, true);
        assert!(
            items
                .iter()
                .all(|i| !matches!(i.label.as_str(), "Run statement" | "Run script" | "Stop"))
        );
    }

    fn hint(app: &SqlrApp, label: &str, mac: bool) -> Option<String> {
        let (items, _) = app.palette_items(PaletteMode::Commands, None, mac);
        let item = items.iter().find(|i| i.label == label).expect(label);
        item.shortcut.clone()
    }

    #[test]
    fn command_hints_come_from_the_keymap_in_platform_notation() {
        let (mut app, _dir) = bare_app();
        assert_eq!(hint(&app, "Explain", true).as_deref(), Some("⌥⌘E"));
        assert_eq!(hint(&app, "Explain", false).as_deref(), Some("Ctrl+Alt+E"));
        assert_eq!(
            app.keymap.keymap.label(ActionId::Stop, true).as_deref(),
            Some("Esc")
        );
        assert_eq!(hint(&app, "Toggle terminal", true).as_deref(), Some("⌃`"));
        assert_eq!(
            hint(&app, "Toggle terminal", false).as_deref(),
            Some("Ctrl+`")
        );
        // A user change shows at once; an unbound command has no hint.
        app.keymap
            .keymap
            .set_chords(ActionId::Explain, vec![Chord::parse("F7").unwrap()]);
        app.keymap.keymap.unbind(ActionId::Commit);
        assert_eq!(hint(&app, "Explain", true).as_deref(), Some("F7"));
        assert_eq!(hint(&app, "Commit", true), None);
        // Labels never carry a key of their own.
        let (items, _) = app.palette_items(PaletteMode::Commands, None, true);
        assert!(items.iter().all(|i| !i.label.contains("Ctrl")));
        // The VS Code preset's keys.
        app.keymap.keymap = Keymap::preset("vscode").unwrap();
        assert_eq!(hint(&app, "Operations", true).as_deref(), Some("⇧⌘U"));
    }

    #[test]
    fn commands_have_icons_by_category() {
        let (app, _dir) = bare_app();
        let (items, _) = app.palette_items(PaletteMode::Commands, None, true);
        let icon = |label: &str| items.iter().find(|i| i.label == label).unwrap().icon;
        assert_eq!(super::AppCommand::Run.icon(), icons::RUN);
        assert_eq!(icon("Commit"), Some(icons::COMMIT));
        assert_eq!(icon("Settings…"), Some(icons::SETTINGS));
        let distinct: std::collections::HashSet<_> = items.iter().map(|i| i.icon).collect();
        assert!(distinct.len() > 15, "{} icons", distinct.len());
    }
}
