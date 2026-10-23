//! The palettes: ⌘P over every known catalog's relations, ⌘K over app commands, snippets
//! and history. Items are rebuilt only when what they list changes.

use sqlr_ui::ToastKind;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use sqlr_core::{
    CatalogNeed, HistoryEntry, HistoryQuery, Node, NodeKind, NodePath, ObjectKind, RelationKind,
};
use sqlr_engine::{HistoryChannel, LoadState};
use sqlr_ui::{
    PaletteGroup, PaletteItem, PaletteMode, PaletteOutput, PaletteState, first_line, format_when,
    palette,
};

use crate::app::SqlrApp;

/// History matches listed in ⌘K.
const HISTORY_ITEMS: usize = 20;
/// Chars of a history entry's first line in its label.
const HISTORY_LABEL_CHARS: usize = 80;

/// What a palette item does.
#[derive(Clone, Debug)]
pub enum PaletteTarget {
    Object {
        conn: String,
        schema: String,
        name: String,
        kind: RelationKind,
    },
    /// Open the relation in the object editor.
    EditObject {
        conn: String,
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
    Settings,
    DriverManager,
    Quit,
    About,
    Run,
    RunScript,
    Stop,
    ToggleValuePanel,
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
    ShowTasks,
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
    /// "New index…" on the collection (or Indices folder) selected in the explorer.
    NewIndex(String, NodePath),
}

/// Kinds "New …" offers in ⌘K, in this order.
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

/// The explorer node of a catalog relation (`<schema>/<folder>/<name>`).
fn relation_node(schema: String, name: String, kind: RelationKind) -> Node {
    let (folder, _, node_kind) = relation_folder(kind);
    Node {
        path: NodePath(vec![schema, folder.to_string(), name.clone()]),
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
        if mode == PaletteMode::Objects {
            // Every connected session's default schemas, so they can be listed.
            for cfg in &self.connections {
                if !self.is_live(&cfg.id) {
                    continue;
                }
                let Some(dialect) = self.dialects.get(&cfg.id) else {
                    continue;
                };
                for schema in dialect.default_schemas() {
                    self.engine
                        .load_catalog(&cfg.id, CatalogNeed::Schema(schema));
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
            let (items, targets) = self.palette_items(mode, active_conn.as_deref());
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
            PaletteOutput::Chosen(i) => {
                // Back to the editor; a command that opens a tab focuses that one instead.
                if let Some(tab) = self.active_tab() {
                    tab.editor.request_focus();
                }
                if let Some(target) = self.palette.targets.get(i).cloned() {
                    self.palette_choose(target, ctx);
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

    fn palette_items(
        &self,
        mode: PaletteMode,
        active_conn: Option<&str>,
    ) -> (Vec<PaletteItem>, Vec<PaletteTarget>) {
        let mut items = Vec::new();
        let mut targets = Vec::new();
        let mut push = |label: String, detail: String, group, fuzzy, target| {
            items.push(PaletteItem {
                label,
                detail,
                group,
                fuzzy,
            });
            targets.push(target);
        };
        match mode {
            PaletteMode::Objects => {
                for (conn, catalog) in self.engine.all_catalogs() {
                    let Some(cfg) = self.connection(&conn) else {
                        continue;
                    };
                    // Drivers with object editors open relations in them too.
                    let editable = self.editing_for(&conn).is_some();
                    for state in catalog.schema.values() {
                        let LoadState::Ready(schema) = state else {
                            continue;
                        };
                        for r in &schema.relations {
                            let (_, kind, _) = relation_folder(r.kind);
                            push(
                                format!("{}.{}", schema.schema, r.name),
                                format!("{} · {kind}", cfg.name),
                                PaletteGroup::Object,
                                true,
                                PaletteTarget::Object {
                                    conn: conn.clone(),
                                    schema: schema.schema.clone(),
                                    name: r.name.clone(),
                                    kind: r.kind,
                                },
                            );
                            if editable {
                                push(
                                    format!("Edit {}.{}", schema.schema, r.name),
                                    format!("{} · {kind} · object editor", cfg.name),
                                    PaletteGroup::Object,
                                    true,
                                    PaletteTarget::EditObject {
                                        conn: conn.clone(),
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
                    let detail = c
                        .action_id()
                        .map(|a| {
                            self.keymap
                                .keymap
                                .chord_for(a)
                                .map(|c| c.label(cfg!(target_os = "macos")))
                                .unwrap_or_default()
                        })
                        .unwrap_or_else(|| detail.to_string());
                    (label.to_string(), detail, c)
                };
                let mut commands = vec![
                    command("New query tab", "⌘T", AppCommand::NewTab),
                    command("New connection", "", AppCommand::NewConnection),
                    command("Settings…", "", AppCommand::Settings),
                    command("Driver Manager…", "", AppCommand::DriverManager),
                    command("Run statement", "⌘↵", AppCommand::Run),
                    command("Run script", "⇧⌘↵", AppCommand::RunScript),
                    command("Stop", "", AppCommand::Stop),
                    command("Toggle value panel", "", AppCommand::ToggleValuePanel),
                    command("Toggle history panel", "", AppCommand::ToggleHistory),
                    command("Toggle soft wrap", "", AppCommand::ToggleSoftWrap),
                    command("Save as snippet…", "", AppCommand::SaveSnippet),
                    command("Commit", "", AppCommand::Commit),
                    command("Rollback", "", AppCommand::Rollback),
                    command("Connection profiles…", "", AppCommand::Profiles),
                    command("Export result…", "", AppCommand::ExportResult),
                    command("Transfer data…", "", AppCommand::TransferData),
                    command("Operations", "", AppCommand::ShowOperations),
                    command("Tasks", "", AppCommand::ShowTasks),
                    command("Compare schemas…", "", AppCommand::CompareSchemas),
                    command("Explain", "⌥⌘E", AppCommand::Explain),
                    command("Explain Analyze", "⌥⇧⌘E", AppCommand::ExplainAnalyze),
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
                        AppCommand::NewIndex(conn.clone(), node.path.clone()),
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
        (items, targets)
    }

    fn palette_choose(&mut self, target: PaletteTarget, ctx: &egui::Context) {
        match target {
            PaletteTarget::Object {
                conn,
                schema,
                name,
                kind,
            } => self.open_relation(&conn, schema, name, kind),
            PaletteTarget::EditObject {
                conn,
                schema,
                name,
                kind,
            } => {
                let node = relation_node(schema, name, kind);
                self.open_object(&conn, &node);
            }
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

    /// Opens a catalog relation's data in a new tab, connecting if needed.
    fn open_relation(&mut self, conn: &str, schema: String, name: String, kind: RelationKind) {
        let node = relation_node(schema, name, kind);
        let sql = self
            .dialects
            .get(conn)
            .and_then(|d| d.select_all(&node, None));
        match sql {
            Some(sql) => self.open_and_run(node.name.clone(), conn, None, &sql),
            None => self.notify(format!("Cannot open the data of {}", node.name)),
        }
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
            AppCommand::DriverManager => self.open_driver_manager(),
            AppCommand::TransferData => self.open_transfer_wizard(None),
            AppCommand::ShowOperations => self.show_operations = true,
            AppCommand::ShowTasks => self.show_tasks_pane(),
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
