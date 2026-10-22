//! Schema tree: connections with lazily loaded, filterable node subtrees.
//!
//! The widget only reads through [`SchemaSource`] and only emits [`TreeAction`]s; it does no
//! I/O. Rows are flattened first and painted with `show_rows`, so cost follows the number of
//! visible rows, not the size of the schema.

use std::collections::{HashMap, HashSet};

use egui::{
    Align2, Button, Color32, DragAndDrop, Id, Key, Margin, Rect, RichText, Sense, Stroke, TextEdit,
    WidgetInfo, WidgetType, vec2,
};
use sqlr_core::{
    DatabaseTools, Node, NodeKind, NodePath, ObjectKind, SqlTemplate, is_database_node,
};

use crate::admin::AdminPanel;
use crate::theme::{self, Palette, Spacing, icons};

const INDENT: f32 = Spacing::XL;
const ARROW_WIDTH: f32 = 16.0;
/// Room for a live connection's Reconnect and Disconnect buttons.
const ROW_BUTTONS_WIDTH: f32 = 52.0;
/// The widest a connection row's detail (driver or file name) gets.
const DETAIL_MAX: f32 = 90.0;

#[derive(Clone, Debug, PartialEq)]
pub enum ConnBadge {
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
    /// The connection's driver is not installed (a connection file from another machine):
    /// the row offers to install it.
    MissingDriver,
}

pub struct TreeConn<'a> {
    pub id: &'a str,
    pub name: &'a str,
    /// The driver id; selects the database icon shown before the name.
    pub driver: &'a str,
    /// Shown muted after the name: the driver's name, or a SQLite connection's file name.
    pub detail: Option<&'a str>,
    pub group: Option<&'a str>,
    /// The connection's own color: its root row is tinted with it.
    pub color: Option<[u8; 3]>,
    pub badge: ConnBadge,
    /// The connection profile's color: the row falls back to it when the connection has no
    /// color of its own.
    pub tint: Option<[u8; 3]>,
    /// A session of the connection has an open transaction (a `●` mark).
    pub open_transaction: bool,
}

pub enum Children<'a> {
    NotLoaded,
    Loading,
    Ready(&'a [Node]),
    Failed(&'a str),
}

/// The object entries of a node's context menu; the default offers none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeMenu {
    /// "Open editor".
    pub open: bool,
    /// "New ▸" kinds created under this node.
    pub new: Vec<ObjectKind>,
    pub drop: bool,
    pub rename: bool,
    pub truncate: bool,
    /// "Generate SQL ▸" templates; with `Create`, also "Copy DDL".
    pub generate: Vec<SqlTemplate>,
}

pub trait SchemaSource {
    fn connections(&self) -> Vec<TreeConn<'_>>;
    /// Folder names in display order; a connection's `group` names its folder.
    fn folders(&self) -> Vec<&str> {
        Vec::new()
    }
    fn children(&self, conn: &str, path: &NodePath) -> Children<'_>;
    /// How many children of `path` are loaded (`None` until they are). Folder rows show it,
    /// open or closed; it never starts a load.
    fn child_count(&self, conn: &str, path: &NodePath) -> Option<usize> {
        match self.children(conn, path) {
            Children::Ready(nodes) => Some(nodes.len()),
            _ => None,
        }
    }
    /// Whether "Open DDL" can show the `CREATE` statement of `node`'s object.
    fn ddl(&self, _conn: &str, _node: &Node) -> bool {
        false
    }
    /// Object entries for `node`'s context menu.
    fn node_menu(&self, _conn: &str, _node: &Node) -> NodeMenu {
        NodeMenu::default()
    }
    /// Kinds "New ▸" creates at the connection's root (databases, schemas).
    fn conn_new(&self, _conn: &str) -> Vec<ObjectKind> {
        Vec::new()
    }
    /// Administration panels the connection's context menu opens.
    fn conn_admin(&self, _conn: &str) -> Vec<AdminPanel> {
        Vec::new()
    }
    /// Whether the connection is a key-value store, as its driver's manifest says: its
    /// databases, prefix groups and keys export, and nothing imports.
    fn key_value(&self, _conn: &str) -> bool {
        false
    }
    /// Whether the connection's databases dump and restore with native tools.
    fn database_tools(&self, _conn: &str) -> DatabaseTools {
        DatabaseTools::default()
    }
    /// The database a connection-level dump uses: its configured database, or SQLite's
    /// `main`. `None` hides the entries at the connection's root.
    fn connection_database(&self, _conn: &str) -> Option<String> {
        None
    }
}

/// The menu label of an object kind.
pub fn kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Database => "Database",
        ObjectKind::Schema => "Schema",
        ObjectKind::Table => "Table",
        ObjectKind::View => "View",
        ObjectKind::MaterializedView => "Materialized view",
        ObjectKind::Index => "Index",
        ObjectKind::Sequence => "Sequence",
        ObjectKind::Function => "Function",
        ObjectKind::Procedure => "Procedure",
        ObjectKind::Trigger => "Trigger",
        ObjectKind::Role => "Role",
    }
}

#[derive(Default)]
pub struct TreeState {
    /// Case-insensitive substring filter over loaded node names.
    pub filter: String,
    pub selected: Option<(String, NodePath)>,
    /// The folder whose row is selected (for F2 and the context menu).
    pub selected_folder: Option<String>,
    /// The node being renamed inline (F2): connection, path and the text typed so far.
    pub renaming: Option<(String, NodePath, String)>,
    /// The folder being renamed inline: its current name and the text typed so far.
    pub renaming_folder: Option<(String, String)>,
    /// The new folder name being typed in the inline row, when it is open.
    pub new_folder: Option<String>,
    /// The folder the inline row creates in (`None`: the top level).
    pub new_folder_parent: Option<String>,
    /// Folders the user expanded; every folder starts closed.
    expanded_folders: HashSet<String>,
    expanded: HashMap<String, HashSet<NodePath>>,
}

impl TreeState {
    pub fn is_expanded(&self, conn: &str, path: &NodePath) -> bool {
        self.expanded.get(conn).is_some_and(|s| s.contains(path))
    }

    pub fn set_expanded(&mut self, conn: &str, path: &NodePath, open: bool) {
        if open {
            self.expanded
                .entry(conn.to_string())
                .or_default()
                .insert(path.clone());
        } else if let Some(set) = self.expanded.get_mut(conn) {
            set.remove(path);
        }
    }

    /// Forgets every expanded path under `conn`, closing its whole subtree.
    pub fn collapse_connection(&mut self, conn: &str) {
        self.expanded.remove(conn);
    }

    /// Folders start closed; only opening one is remembered.
    pub fn is_folder_expanded(&self, folder: &str) -> bool {
        self.expanded_folders.contains(folder)
    }

    pub fn set_folder_expanded(&mut self, folder: &str, open: bool) {
        if open {
            self.expand_folder(folder);
        } else {
            self.expanded_folders.remove(folder);
        }
    }

    /// Opens `folder` and every ancestor, so a nested folder becomes visible.
    pub fn expand_folder(&mut self, folder: &str) {
        let mut prefix = String::new();
        for segment in folder.split('/').filter(|s| !s.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            self.expanded_folders.insert(prefix.clone());
        }
    }

    /// Keeps the expanded folders, the selection and any open inline row with a folder after
    /// it (and its subfolders) was renamed.
    pub fn folders_renamed(&mut self, from: &str, to: &str) {
        let map = |path: &str| rewrite_folder_path(path, from, to);
        self.expanded_folders = self.expanded_folders.iter().map(|p| map(p)).collect();
        if let Some(folder) = &mut self.selected_folder {
            *folder = map(folder);
        }
        if let Some((folder, _)) = &mut self.renaming_folder {
            *folder = map(folder);
        }
        if let Some(parent) = &mut self.new_folder_parent {
            *parent = map(parent);
        }
    }

    /// Drops everything the tree remembered about a deleted folder's subtree.
    pub fn folder_deleted(&mut self, path: &str) {
        let prefix = format!("{path}/");
        let inside = |folder: &str| folder == path || folder.starts_with(&prefix);
        self.expanded_folders.retain(|folder| !inside(folder));
        if self.selected_folder.as_deref().is_some_and(inside) {
            self.selected_folder = None;
        }
        if self
            .renaming_folder
            .as_ref()
            .is_some_and(|(folder, _)| inside(folder))
        {
            self.renaming_folder = None;
        }
        if self.new_folder_parent.as_deref().is_some_and(inside) {
            self.new_folder_parent = None;
        }
    }

    /// Selects and reveals a folder the store just created.
    pub fn folder_created(&mut self, path: &str) {
        self.selected_folder = Some(path.to_string());
        self.selected = None;
        self.expand_folder(path);
    }

    /// Puts the rename field back with its leaf name after the store refused the rename.
    pub fn folder_rename_failed(&mut self, from: &str, to: &str) {
        self.renaming_folder = Some((from.to_string(), folder_leaf(to).to_string()));
    }

    /// Reopens the inline row, in the right folder, after the store refused to create it.
    pub fn folder_create_failed(&mut self, path: &str) {
        self.new_folder = Some(folder_leaf(path).to_string());
        self.new_folder_parent = folder_parent(path).map(str::to_string);
        if let Some(parent) = self.new_folder_parent.clone() {
            self.expand_folder(&parent);
        }
    }

    /// Opens the inline row that names a new folder: inside the selected folder, or at the
    /// top level when the selection is not a folder.
    pub fn start_new_folder(&mut self) {
        let parent = self.selected_folder.clone();
        self.start_new_folder_in(parent.as_deref());
    }

    /// Opens the inline row inside `parent` (`None`: the top level), revealing it.
    pub fn start_new_folder_in(&mut self, parent: Option<&str>) {
        self.renaming_folder = None;
        self.new_folder = Some(String::new());
        self.new_folder_parent = parent.map(str::to_string);
        if let Some(parent) = parent {
            self.expand_folder(parent);
            self.selected_folder = Some(parent.to_string());
            self.selected = None;
        }
    }

    fn is_selected(&self, conn: &str, path: &NodePath) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|(c, p)| c == conn && p == path)
    }
}

/// The last segment of a folder path.
fn folder_leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The folder that contains `path`, or the top level.
fn folder_parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

/// `parent/name`, or `name` at the top level.
fn join_folder(parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(parent) if !parent.is_empty() => format!("{parent}/{name}"),
        _ => name.to_string(),
    }
}

/// `path` with the `from` prefix replaced by `to`; other paths are unchanged.
fn rewrite_folder_path(path: &str, from: &str, to: &str) -> String {
    if path == from {
        return to.to_string();
    }
    match path.strip_prefix(&format!("{from}/")) {
        Some(tail) => format!("{to}/{tail}"),
        None => path.to_string(),
    }
}

/// Where a dragged row lands among its new siblings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeDrop {
    Before(String),
    After(String),
    End,
    /// The end of the contents of a folder.
    Into(String),
}

/// What a row drag carries. Only one kind of item is dragged at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TreeDrag {
    Connection(String),
    Folder(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum TreeAction {
    NewConnection,
    /// Open the new-connection dialog with this folder prefilled.
    NewConnectionIn(String),
    /// The user typed the name of a new folder.
    CreateFolder(String),
    RenameFolder {
        from: String,
        to: String,
    },
    DeleteFolder(String),
    /// Move a connection to `folder` (`None`: the top level).
    MoveConnection {
        conn: String,
        folder: Option<String>,
        target: TreeDrop,
    },
    /// Reorder a folder.
    MoveFolder {
        folder: String,
        target: TreeDrop,
    },
    Connect(String),
    /// Install the driver (by driver id) a connection needs.
    InstallDriver(String),
    /// Connect again after a failed connect (the inline "Retry").
    Retry(String),
    /// Open an editor tab on the connection.
    NewEditorTab(String),
    /// Copy the connection's URL, without its password.
    CopyUrl(String),
    Disconnect(String),
    Reconnect(String),
    Edit(String),
    /// Copy a saved connection and its stored secrets under a new name.
    Duplicate(String),
    Delete(String),
    Expand {
        conn: String,
        path: NodePath,
    },
    OpenData {
        conn: String,
        node: Node,
    },
    /// Open the first `limit` rows of a table or view.
    SelectRows {
        conn: String,
        node: Node,
        limit: usize,
    },
    /// Open a tab showing the `CREATE` statement of the node's object.
    OpenDdl {
        conn: String,
        node: Node,
    },
    /// Copy the node's name, or its schema-qualified, quoted name.
    CopyName {
        conn: String,
        node: Node,
        qualified: bool,
    },
    NewQuery {
        conn: String,
        database: String,
    },
    Refresh {
        conn: String,
        path: NodePath,
    },
    /// Open the object editor of `node`.
    OpenObject {
        conn: String,
        node: Node,
    },
    /// Create an object of `kind` under `node` (`None`: the connection's root).
    NewObject {
        conn: String,
        node: Option<Node>,
        kind: ObjectKind,
    },
    DropObject {
        conn: String,
        node: Node,
    },
    RenameObject {
        conn: String,
        node: Node,
        to: String,
    },
    Truncate {
        conn: String,
        node: Node,
    },
    GenerateSql {
        conn: String,
        node: Node,
        template: SqlTemplate,
    },
    CopyDdl {
        conn: String,
        node: Node,
    },
    /// "Export data…" on a table or view.
    ExportData {
        conn: String,
        node: Node,
    },
    /// "Import data…" on a table.
    ImportData {
        conn: String,
        node: Node,
    },
    /// "Generate data…" on a table.
    GenerateData {
        conn: String,
        node: Node,
    },
    /// "ER diagram" on a schema or database node.
    ErDiagram {
        conn: String,
        node: Node,
    },
    /// "Compare with…" on a schema or database node: the compare dialog with it as the source.
    CompareSchema {
        conn: String,
        node: Node,
    },
    /// "Transfer data…" on a table: the transfer wizard with it as the source.
    TransferData {
        conn: String,
        node: Node,
    },
    /// "Dump database…" on a database node (SQLite: its `main` schema).
    DumpDatabase {
        conn: String,
        database: String,
    },
    /// "Restore database…" on a database node.
    RestoreDatabase {
        conn: String,
        database: String,
    },
    /// Open an administration panel of the connection.
    OpenAdmin {
        conn: String,
        panel: AdminPanel,
    },
}

enum Row<'a> {
    Folder {
        /// The full `/`-joined path.
        path: String,
        /// The leaf name shown on the row.
        name: String,
        depth: usize,
        open: bool,
    },
    /// The inline row that names a new folder.
    NewFolder {
        depth: usize,
    },
    Conn {
        conn: &'a TreeConn<'a>,
        depth: usize,
        open: bool,
    },
    /// Under a connection whose connect failed: the error's first line, Retry and Edit.
    ConnFailed {
        conn: &'a TreeConn<'a>,
        message: &'a str,
        depth: usize,
    },
    /// Under a connection whose driver is not installed: the message and Install.
    ConnMissingDriver {
        conn: &'a TreeConn<'a>,
        depth: usize,
    },
    Node {
        conn: &'a str,
        /// The connection's driver id.
        driver: &'a str,
        node: &'a Node,
        depth: usize,
        open: bool,
    },
    Loading {
        depth: usize,
    },
    Failed {
        conn: &'a str,
        path: &'a NodePath,
        message: &'a str,
        depth: usize,
    },
    /// The invisible target under the last row that moves an item to the end.
    DropEnd,
}

/// Flattens the visible part of the tree. `filter` is lowercase; empty means no filtering.
struct Flattener<'a, 's> {
    source: &'a dyn SchemaSource,
    state: &'s TreeState,
    filter: &'s str,
}

impl<'a> Flattener<'a, '_> {
    fn name_matches(&self, name: &str) -> bool {
        self.filter.is_empty() || name.to_lowercase().contains(self.filter)
    }

    /// Any loaded descendant of `node` matches.
    fn descendant_matches(&self, conn: &str, node: &Node) -> bool {
        if !node.has_children {
            return false;
        }
        match self.source.children(conn, &node.path) {
            Children::Ready(children) => children
                .iter()
                .any(|c| self.name_matches(&c.name) || self.descendant_matches(conn, c)),
            _ => false,
        }
    }

    fn children(
        &self,
        conn: &'a str,
        driver: &'a str,
        path: &'a NodePath,
        depth: usize,
        badge_connecting: bool,
        rows: &mut Vec<Row<'a>>,
    ) {
        match self.source.children(conn, path) {
            Children::Ready(nodes) => {
                for node in nodes {
                    let below = !self.filter.is_empty() && self.descendant_matches(conn, node);
                    if !(self.filter.is_empty() || below || self.name_matches(&node.name)) {
                        continue;
                    }
                    let open =
                        node.has_children && (self.state.is_expanded(conn, &node.path) || below);
                    rows.push(Row::Node {
                        conn,
                        driver,
                        node,
                        depth,
                        open,
                    });
                    if open {
                        self.children(conn, driver, &node.path, depth + 1, false, rows);
                    }
                }
            }
            Children::Loading => rows.push(Row::Loading { depth }),
            Children::NotLoaded if badge_connecting => rows.push(Row::Loading { depth }),
            Children::NotLoaded => {}
            Children::Failed(message) => rows.push(Row::Failed {
                conn,
                path,
                message,
                depth,
            }),
        }
    }
}

fn flatten<'a>(
    source: &'a dyn SchemaSource,
    state: &TreeState,
    conns: &'a [TreeConn<'a>],
    folders: &'a [&'a str],
    filter: &str,
    dragging: bool,
) -> Vec<Row<'a>> {
    // Every folder path, in display order: explicit folders first, then groups only
    // connections name (old connections.toml files).
    let mut paths = Vec::new();
    for name in folders {
        add_folder_path(&mut paths, name);
    }
    let groups: Vec<(usize, Option<String>)> = conns
        .iter()
        .enumerate()
        .map(|(i, conn)| (i, conn.group.and_then(clean_folder_path)))
        .collect();
    for (_, group) in &groups {
        if let Some(group) = group {
            add_folder_path(&mut paths, group);
        }
    }

    // Children by parent path, in first-appearance order.
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut known = HashSet::new();
    for path in &paths {
        if !known.insert(path.clone()) {
            continue;
        }
        let parent = folder_parent(path).unwrap_or("").to_string();
        children.entry(parent).or_default().push(path.clone());
    }
    let mut members: HashMap<&str, Vec<&'a TreeConn>> = HashMap::new();
    for (i, group) in &groups {
        if let Some(group) = group {
            members.entry(group).or_default().push(&conns[*i]);
        }
    }

    let mut rows = Vec::new();
    // Ungrouped connections first, then folders in the user's order.
    for conn in conns
        .iter()
        .filter(|c| c.group.and_then(clean_folder_path).is_none())
    {
        push_conn(source, state, filter, conn, 0, &mut rows);
    }
    if state.new_folder.is_some() && state.new_folder_parent.is_none() {
        rows.push(Row::NewFolder { depth: 0 });
    }
    if let Some(top) = children.get("") {
        for path in top {
            push_folder(
                source, state, filter, path, 0, &children, &members, &mut rows,
            );
        }
    }
    if dragging {
        rows.push(Row::DropEnd);
    }
    rows
}

/// A folder path with trimmed, non-empty `/`-separated segments.
fn clean_folder_path(name: &str) -> Option<String> {
    let segments: Vec<&str> = name
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    (!segments.is_empty()).then(|| segments.join("/"))
}

/// Adds `name` and each missing ancestor, keeping the first appearance's order.
fn add_folder_path(out: &mut Vec<String>, name: &str) {
    let Some(name) = clean_folder_path(name) else {
        return;
    };
    let mut prefix = String::new();
    for segment in name.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        if !out.contains(&prefix) {
            out.push(prefix.clone());
        }
    }
}

fn push_conn<'a>(
    source: &'a dyn SchemaSource,
    state: &TreeState,
    filter: &str,
    conn: &'a TreeConn<'a>,
    depth: usize,
    rows: &mut Vec<Row<'a>>,
) {
    let root = &ROOT;
    let open = state.is_expanded(conn.id, root);
    rows.push(Row::Conn { conn, depth, open });
    if let ConnBadge::Failed(message) = &conn.badge {
        // In place of its tree, whose loads failed the same way.
        rows.push(Row::ConnFailed {
            conn,
            message,
            depth: depth + 1,
        });
        return;
    }
    if conn.badge == ConnBadge::MissingDriver {
        rows.push(Row::ConnMissingDriver {
            conn,
            depth: depth + 1,
        });
        return;
    }
    if open {
        Flattener {
            source,
            state,
            filter,
        }
        .children(
            conn.id,
            conn.driver,
            root,
            depth + 1,
            conn.badge == ConnBadge::Connecting,
            rows,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn push_folder<'a>(
    source: &'a dyn SchemaSource,
    state: &TreeState,
    filter: &str,
    path: &str,
    depth: usize,
    children: &HashMap<String, Vec<String>>,
    members: &HashMap<&str, Vec<&'a TreeConn<'a>>>,
    rows: &mut Vec<Row<'a>>,
) {
    let open = state.is_folder_expanded(path);
    rows.push(Row::Folder {
        path: path.to_string(),
        name: folder_leaf(path).to_string(),
        depth,
        open,
    });
    if !open {
        return;
    }
    if state.new_folder.as_ref().is_some() && state.new_folder_parent.as_deref() == Some(path) {
        rows.push(Row::NewFolder { depth: depth + 1 });
    }
    if let Some(kids) = children.get(path) {
        for kid in kids {
            push_folder(
                source,
                state,
                filter,
                kid,
                depth + 1,
                children,
                members,
                rows,
            );
        }
    }
    if let Some(conns) = members.get(path) {
        for conn in conns {
            push_conn(source, state, filter, conn, depth + 1, rows);
        }
    }
}

/// A shared empty path, so rows can borrow the root without allocating.
static ROOT: NodePath = NodePath(Vec::new());

pub fn schema_tree(
    ui: &mut egui::Ui,
    state: &mut TreeState,
    source: &dyn SchemaSource,
) -> Vec<TreeAction> {
    let mut actions = Vec::new();

    let palette = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        if icons::icon_button(ui, icons::ADD, Some(palette.success), "New connection").clicked() {
            actions.push(TreeAction::NewConnection);
        }
        if icons::icon_button(
            ui,
            icons::FOLDER_PLUS,
            Some(palette.icon_schema),
            "New folder",
        )
        .clicked()
        {
            state.start_new_folder();
        }
        // Refreshes the selected node, or the selected connection's whole tree.
        if icons::icon_button(ui, icons::REFRESH, None, "Refresh selected").clicked()
            && let Some((conn, path)) = &state.selected
        {
            actions.push(TreeAction::Refresh {
                conn: conn.clone(),
                path: path.clone(),
            });
        }
        icons::paint(ui, icons::SEARCH, palette.text_muted);
        let clear = !state.filter.is_empty();
        let width = if clear {
            (ui.available_width() - 26.0).max(40.0)
        } else {
            ui.available_width()
        };
        let filter = crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut state.filter)
                .hint_text("Filter objects")
                .desired_width(width),
        );
        if clear && icons::icon_button(ui, icons::CLOSE, None, "Clear filter").clicked() {
            state.filter.clear();
            filter.request_focus();
        }
    });
    ui.add_space(Spacing::S);

    let conns = source.connections();
    let folders = source.folders();
    let filter = state.filter.to_lowercase();
    let dragging = DragAndDrop::has_payload_of_type::<TreeDrag>(ui.ctx());
    let rows = flatten(source, state, &conns, &folders, &filter, dragging);

    // F2 renames the selected folder or node inline, when nothing else has the keyboard.
    let keymap = crate::keymap::ui_keymap(ui.ctx());
    let f2 = state.renaming.is_none()
        && state.renaming_folder.is_none()
        && state.new_folder.is_none()
        && ui.memory(|m| m.focused().is_none())
        && ui.input_mut(|i| keymap.consume(i, sqlr_core::ActionId::RenameSelection));
    if f2 {
        if let Some(folder) = state.selected_folder.clone() {
            state.renaming_folder = Some((folder.clone(), folder_leaf(&folder).to_string()));
        } else if let Some((conn, path)) = state.selected.clone() {
            let node = rows.iter().find_map(|r| match r {
                Row::Node { conn: c, node, .. } if *c == conn && node.path == path => Some(*node),
                _ => None,
            });
            if let Some(node) = node
                && source.node_menu(&conn, node).rename
            {
                state.renaming = Some((conn, path, node.name.clone()));
            }
        }
    }

    // Rows are separated by a micro gap so adjacent connections and nodes stay distinct.
    ui.spacing_mut().item_spacing.y = Spacing::XS;
    egui::ScrollArea::vertical()
        .id_salt("schema_tree_scroll")
        .auto_shrink(false)
        // A selectable label's hover background is 1 px taller than its row; without the
        // margin, the first row's background is clipped at the top of the scroll area.
        .content_margin(Margin::symmetric(0, 1))
        .show_rows(ui, Spacing::ROW_TREE, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for row in &rows[range] {
                render_row(ui, row, state, source, &palette, &mut actions);
            }
        });
    actions
}

fn render_row(
    ui: &mut egui::Ui,
    row: &Row<'_>,
    state: &mut TreeState,
    source: &dyn SchemaSource,
    palette: &Palette,
    actions: &mut Vec<TreeAction>,
) {
    match row {
        Row::Folder {
            path,
            name,
            depth,
            open,
        } => {
            folder_row(ui, path, name, *depth, *open, state, palette, actions);
        }
        Row::NewFolder { depth } => new_folder_row(ui, *depth, state, palette, actions),
        Row::Conn { conn, depth, open } => {
            conn_row(ui, conn, *depth, *open, state, source, palette, actions);
        }
        Row::Node {
            conn,
            driver,
            node,
            depth,
            open,
        } => node_row(
            ui, conn, driver, node, *depth, *open, state, source, palette, actions,
        ),
        Row::ConnFailed {
            conn,
            message,
            depth,
        } => {
            row_ui(ui, |ui| {
                ui.add_space(*depth as f32 * INDENT + ARROW_WIDTH);
                let text = format!(
                    "Could not connect: {}",
                    crate::text::first_line(message, 80)
                );
                // Retry and Edit stay visible: the message gives way.
                let budget = (ui.available_width() - 110.0).max(40.0);
                ui.scope(|ui| {
                    ui.set_max_width(budget);
                    ui.add(egui::Label::new(RichText::new(text).color(palette.error)).truncate())
                        .on_hover_text(*message);
                });
                if ui.small_button("Retry").clicked() {
                    actions.push(TreeAction::Retry(conn.id.to_string()));
                }
                if ui.small_button("Edit").clicked() {
                    actions.push(TreeAction::Edit(conn.id.to_string()));
                }
            });
        }
        Row::ConnMissingDriver { conn, depth } => {
            row_ui(ui, |ui| {
                ui.add_space(*depth as f32 * INDENT + ARROW_WIDTH);
                let text = format!("Driver {} is not installed", conn.driver);
                let budget = (ui.available_width() - 110.0).max(40.0);
                ui.scope(|ui| {
                    ui.set_max_width(budget);
                    ui.add(egui::Label::new(RichText::new(text).color(palette.warning)).truncate());
                });
                if ui.small_button("Install").clicked() {
                    actions.push(TreeAction::InstallDriver(conn.driver.to_string()));
                }
                if ui.small_button("Edit").clicked() {
                    actions.push(TreeAction::Edit(conn.id.to_string()));
                }
            });
        }
        Row::Loading { depth } => {
            row_ui(ui, |ui| {
                ui.add_space(*depth as f32 * INDENT + ARROW_WIDTH);
                ui.spinner();
                ui.label(RichText::new("Loading…").color(palette.text_muted));
            });
        }
        Row::Failed {
            conn,
            path,
            message,
            depth,
        } => {
            row_ui(ui, |ui| {
                ui.add_space(*depth as f32 * INDENT + ARROW_WIDTH);
                ui.colored_label(palette.error, *message);
                if ui.small_button("Retry").clicked() {
                    actions.push(TreeAction::Refresh {
                        conn: conn.to_string(),
                        path: (*path).clone(),
                    });
                }
            });
        }
        Row::DropEnd => drop_end_row(ui, palette, actions),
    }
}

/// One row: left to right, vertically centered, `ROW_TREE` high.
fn row_ui(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), Spacing::ROW_TREE),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_height(Spacing::ROW_TREE);
            add(ui);
        },
    );
}

/// [`row_ui`] with a stable id, returning the whole row as a drop target.
fn row_ui_hit(ui: &mut egui::Ui, id: Id, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let inner = ui.allocate_ui_with_layout(
        vec2(ui.available_width(), Spacing::ROW_TREE),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_height(Spacing::ROW_TREE);
            add(ui);
        },
    );
    ui.interact(inner.response.rect, id, Sense::hover())
}

/// The expand/collapse arrow; `None` for a leaf (space is kept so names align).
fn arrow(ui: &mut egui::Ui, name: &str, expandable: bool, open: bool, color: Color32) -> bool {
    let size = vec2(ARROW_WIDTH, Spacing::ROW_TREE);
    if !expandable {
        ui.allocate_exact_size(size, Sense::hover());
        return false;
    }
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let verb = if open { "Collapse" } else { "Expand" };
    response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{verb} {name}")));
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        if open {
            icons::CARET_DOWN
        } else {
            icons::CARET_RIGHT
        },
        icons::font(ui, 12.0),
        color,
    );
    response.clicked()
}

/// A folder row: arrow, folder mark, name. Drop target for connections and folders; drag
/// source for reordering and nesting folders.
#[allow(clippy::too_many_arguments)]
fn folder_row(
    ui: &mut egui::Ui,
    path: &str,
    name: &str,
    depth: usize,
    open: bool,
    state: &mut TreeState,
    palette: &Palette,
    actions: &mut Vec<TreeAction>,
) {
    let row_rect = Rect::from_min_size(
        ui.cursor().min,
        vec2(ui.available_width(), Spacing::ROW_TREE),
    );
    let payload = DragAndDrop::payload::<TreeDrag>(ui.ctx());
    let pointer = ui.ctx().pointer_hover_pos();
    let hovered = payload.is_some() && pointer.is_some_and(|p| row_rect.contains(p));
    if hovered && matches!(payload.as_deref(), Some(TreeDrag::Connection(_))) {
        ui.painter().rect_filled(row_rect, 4.0, palette.accent_soft);
    }

    let renaming = state
        .renaming_folder
        .as_ref()
        .is_some_and(|(folder, _)| folder == path);
    let mut toggle = false;
    let hit = row_ui_hit(ui, Id::new(("tree_folder", path)), |ui| {
        ui.add_space(depth as f32 * INDENT);
        toggle |= arrow(ui, name, true, open, palette.text_muted);
        icons::paint(ui, icons::FOLDER, palette.icon_schema);
        if renaming {
            rename_folder_field(ui, path, state, actions);
            return;
        }
        ui.spacing_mut().item_spacing.x = Spacing::XS;
        ui.spacing_mut().button_padding.x = Spacing::XS;
        let selected = state.selected_folder.as_deref() == Some(path);
        let response = ui.add(Button::selectable(selected, name).sense(Sense::click_and_drag()));
        let response = if path != name {
            response.on_hover_text(path)
        } else {
            response
        };
        response.dnd_set_drag_payload(TreeDrag::Folder(path.to_string()));
        if response.clicked() {
            state.selected_folder = Some(path.to_string());
            state.selected = None;
        }
        toggle |= response.double_clicked();
        theme::context_menu(&response, |ui| {
            if icons::menu_item(ui, icons::CONNECTION, "New connection").clicked() {
                actions.push(TreeAction::NewConnectionIn(path.to_string()));
                ui.close();
            }
            if icons::menu_item(ui, icons::FOLDER_PLUS, "New folder").clicked() {
                state.start_new_folder_in(Some(path));
                ui.close();
            }
            if icons::menu_item(ui, icons::EDIT, "Rename").clicked() {
                state.selected_folder = Some(path.to_string());
                state.selected = None;
                state.renaming_folder = Some((path.to_string(), name.to_string()));
                ui.close();
            }
            if icons::menu_item(ui, icons::DELETE, "Delete folder").clicked() {
                actions.push(TreeAction::DeleteFolder(path.to_string()));
                ui.close();
            }
        });
    });
    if toggle {
        state.set_folder_expanded(path, !open);
    }
    drop_on_folder(ui, &hit, path, state, palette, actions);
}

/// The inline rename field for a folder, showing its leaf name; true when it was drawn.
fn rename_folder_field(
    ui: &mut egui::Ui,
    path: &str,
    state: &mut TreeState,
    actions: &mut Vec<TreeAction>,
) -> bool {
    let Some((folder, text)) = &mut state.renaming_folder else {
        return false;
    };
    if folder != path {
        return false;
    }
    let response = crate::text_menu::text_edit(
        ui,
        TextEdit::singleline(text)
            .desired_width(180.0)
            .hint_text("folder name"),
    );
    if !response.has_focus() && !response.lost_focus() {
        response.request_focus();
    }
    let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
    if response.lost_focus() || escape {
        let name = text.trim().to_string();
        let to = join_folder(folder_parent(path), &name);
        if enter && !escape && !name.is_empty() && to != path {
            actions.push(TreeAction::RenameFolder {
                from: path.to_string(),
                to,
            });
        }
        state.renaming_folder = None;
    }
    true
}

/// The inline row that names a new folder (opened by the header's button or a folder's menu).
fn new_folder_row(
    ui: &mut egui::Ui,
    depth: usize,
    state: &mut TreeState,
    palette: &Palette,
    actions: &mut Vec<TreeAction>,
) {
    row_ui(ui, |ui| {
        ui.add_space(depth as f32 * INDENT + ARROW_WIDTH);
        icons::paint(ui, icons::FOLDER, palette.icon_schema);
        let Some(text) = &mut state.new_folder else {
            return;
        };
        let response = crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(text)
                .desired_width(180.0)
                .hint_text("Folder name"),
        );
        if !response.has_focus() && !response.lost_focus() {
            response.request_focus();
        }
        let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if response.lost_focus() || escape {
            let name = text.trim().to_string();
            if enter && !escape && !name.is_empty() {
                let path = join_folder(state.new_folder_parent.as_deref(), &name);
                actions.push(TreeAction::CreateFolder(path));
            }
            state.new_folder = None;
            state.new_folder_parent = None;
        }
    });
}

/// The trailing drop target that moves a dragged row to the end of its list.
fn drop_end_row(ui: &mut egui::Ui, palette: &Palette, actions: &mut Vec<TreeAction>) {
    let payload = DragAndDrop::payload::<TreeDrag>(ui.ctx());
    let hint = match payload.as_deref() {
        Some(TreeDrag::Connection(_)) => "Drop here to move to the top level",
        Some(TreeDrag::Folder(_)) => "Drop here to move to the end",
        None => "",
    };
    let hit = row_ui_hit(ui, Id::new("tree_drop_end"), |ui| {
        ui.label(RichText::new(hint).small().color(palette.text_muted));
    });
    if let Some(pointer) = ui.ctx().pointer_hover_pos()
        && hit.rect.contains(pointer)
    {
        ui.painter().hline(
            hit.rect.x_range(),
            hit.rect.top(),
            Stroke::new(2.0, palette.accent),
        );
    }
    if let Some(payload) = hit.dnd_release_payload::<TreeDrag>() {
        match payload.as_ref() {
            TreeDrag::Connection(conn) => actions.push(TreeAction::MoveConnection {
                conn: conn.clone(),
                folder: None,
                target: TreeDrop::End,
            }),
            TreeDrag::Folder(folder) => actions.push(TreeAction::MoveFolder {
                folder: folder.clone(),
                target: TreeDrop::End,
            }),
        }
    }
}

/// Accepts a connection dropped onto a connection row: insert before or after it, in that
/// connection's folder.
fn drop_on_connection(
    ui: &egui::Ui,
    hit: &egui::Response,
    conn: &TreeConn<'_>,
    state: &mut TreeState,
    palette: &Palette,
    actions: &mut Vec<TreeAction>,
) {
    let Some(payload) = hit.dnd_hover_payload::<TreeDrag>() else {
        return;
    };
    let TreeDrag::Connection(dragged) = payload.as_ref() else {
        return;
    };
    if dragged == conn.id {
        return;
    }
    let pointer = ui.ctx().pointer_hover_pos().unwrap_or(hit.rect.center());
    let before = pointer.y < hit.rect.center().y;
    ui.painter().hline(
        hit.rect.x_range(),
        if before {
            hit.rect.top()
        } else {
            hit.rect.bottom()
        },
        Stroke::new(2.0, palette.accent),
    );
    if let Some(payload) = hit.dnd_release_payload::<TreeDrag>() {
        let TreeDrag::Connection(dragged) = payload.as_ref() else {
            return;
        };
        actions.push(TreeAction::MoveConnection {
            conn: dragged.clone(),
            folder: conn.group.map(str::to_string),
            target: if before {
                TreeDrop::Before(conn.id.to_string())
            } else {
                TreeDrop::After(conn.id.to_string())
            },
        });
        if let Some(folder) = conn.group {
            state.set_folder_expanded(folder, true);
        }
    }
}

/// Accepts a connection dropped onto a folder (moving it there) or a folder dropped onto a
/// folder (reordering, or nesting it when dropped in the middle of the row).
fn drop_on_folder(
    ui: &egui::Ui,
    hit: &egui::Response,
    path: &str,
    state: &mut TreeState,
    palette: &Palette,
    actions: &mut Vec<TreeAction>,
) {
    let Some(payload) = hit.dnd_hover_payload::<TreeDrag>() else {
        return;
    };
    match payload.as_ref() {
        TreeDrag::Connection(_) => {
            if let Some(payload) = hit.dnd_release_payload::<TreeDrag>() {
                let TreeDrag::Connection(dragged) = payload.as_ref() else {
                    return;
                };
                state.set_folder_expanded(path, true);
                actions.push(TreeAction::MoveConnection {
                    conn: dragged.clone(),
                    folder: Some(path.to_string()),
                    target: TreeDrop::End,
                });
            }
        }
        TreeDrag::Folder(dragged) => {
            if dragged == path || path.starts_with(&format!("{dragged}/")) {
                return;
            }
            let pointer = ui.ctx().pointer_hover_pos().unwrap_or(hit.rect.center());
            let edge = hit.rect.height() * 0.3;
            let before = pointer.y < hit.rect.top() + edge;
            let after = pointer.y > hit.rect.bottom() - edge;
            let target = if before {
                ui.painter().hline(
                    hit.rect.x_range(),
                    hit.rect.top(),
                    Stroke::new(2.0, palette.accent),
                );
                TreeDrop::Before(path.to_string())
            } else if after {
                ui.painter().hline(
                    hit.rect.x_range(),
                    hit.rect.bottom(),
                    Stroke::new(2.0, palette.accent),
                );
                TreeDrop::After(path.to_string())
            } else {
                ui.painter().rect_filled(hit.rect, 4.0, palette.accent_soft);
                TreeDrop::Into(path.to_string())
            };
            if let Some(payload) = hit.dnd_release_payload::<TreeDrag>() {
                let TreeDrag::Folder(dragged) = payload.as_ref() else {
                    return;
                };
                if matches!(target, TreeDrop::Into(_)) {
                    state.set_folder_expanded(path, true);
                }
                actions.push(TreeAction::MoveFolder {
                    folder: dragged.clone(),
                    target,
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn conn_row(
    ui: &mut egui::Ui,
    conn: &TreeConn<'_>,
    depth: usize,
    open: bool,
    state: &mut TreeState,
    source: &dyn SchemaSource,
    palette: &Palette,
    actions: &mut Vec<TreeAction>,
) {
    let root = NodePath::root();
    let mut toggle = false;
    let hit = row_ui_hit(ui, Id::new(("tree_conn", conn.id)), |ui| {
        ui.add_space(depth as f32 * INDENT);
        if let Some([r, g, b]) = conn.color.or(conn.tint) {
            let rect = ui.max_rect();
            let fill = Color32::from_rgba_unmultiplied(r, g, b, 40);
            ui.painter().rect_filled(rect, 4.0, fill);
        }
        toggle |= arrow(ui, conn.name, true, open, palette.text_muted);
        let selected = state.is_selected(conn.id, &root);
        // The name gives way to the badge, the detail and the row buttons: a long one is
        // cut with an ellipsis and shown whole on hover.
        let live = matches!(conn.badge, ConnBadge::Connected | ConnBadge::Connecting);
        let (text, color) = match &conn.badge {
            ConnBadge::Disconnected => ("disconnected", palette.text_muted),
            ConnBadge::Connecting => ("connecting…", palette.warning),
            ConnBadge::Connected => ("connected", palette.success),
            ConnBadge::Failed(_) => ("failed", palette.error),
            ConnBadge::MissingDriver => ("driver missing", palette.warning),
        };
        let badge_text = RichText::new(text).small().color(color);
        let badge_width = egui::WidgetText::from(badge_text.clone())
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Body,
            )
            .size()
            .x;
        let trailing = badge_width
            + ui.spacing().item_spacing.x
            + if live { ROW_BUTTONS_WIDTH } else { 0.0 }
            + if conn.open_transaction { 16.0 } else { 0.0 };
        let budget = (ui.available_width() - trailing).max(60.0);
        let name = ui.horizontal(|ui| {
            ui.set_max_width(budget);
            // Keep the database mark close to the name; the row's other gaps stay wider.
            ui.spacing_mut().item_spacing.x = Spacing::XS;
            ui.spacing_mut().button_padding.x = Spacing::XS;
            icons::paint_database(ui, conn.driver);
            ui.add(
                Button::selectable(selected, conn.name)
                    .truncate()
                    .sense(Sense::click_and_drag()),
            )
        });
        let response = if name.response.rect.width() >= budget - 1.0 {
            name.inner.on_hover_text(conn.name)
        } else {
            name.inner
        };
        // The detail takes what is left, if that is enough to read.
        let room = (ui.available_width() - trailing).min(DETAIL_MAX);
        if let Some(detail) = conn.detail.filter(|_| room >= 28.0) {
            ui.scope(|ui| {
                ui.set_max_width(room);
                ui.add(
                    egui::Label::new(RichText::new(detail).small().color(palette.text_muted))
                        .truncate(),
                );
            });
        }
        response.dnd_set_drag_payload(TreeDrag::Connection(conn.id.to_string()));
        if response.clicked() {
            state.selected = Some((conn.id.to_string(), root.clone()));
            state.selected_folder = None;
        }
        toggle |= response.double_clicked();
        let badge = ui.label(badge_text);
        if let ConnBadge::Failed(message) = &conn.badge {
            badge.on_hover_text(message);
        }
        if conn.open_transaction {
            ui.label(RichText::new("●").color(palette.warning))
                .on_hover_text("Open transaction");
        }
        // The row's actions stay anchored to the right edge, away from the name.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if matches!(conn.badge, ConnBadge::Connected | ConnBadge::Connecting) {
                if icons::row_button(ui, icons::STOP, "Disconnect connection").clicked() {
                    actions.push(TreeAction::Disconnect(conn.id.to_string()));
                }
                if icons::row_button(ui, icons::REFRESH, "Reconnect").clicked() {
                    actions.push(TreeAction::Reconnect(conn.id.to_string()));
                }
            }
        });
        theme::context_menu(&response, |ui| {
            let connected = matches!(conn.badge, ConnBadge::Connected | ConnBadge::Connecting);
            if icons::menu_item(ui, icons::QUERY, "New editor tab").clicked() {
                actions.push(TreeAction::NewEditorTab(conn.id.to_string()));
                ui.close();
            }
            if connected {
                if icons::menu_item(ui, icons::STOP, "Disconnect").clicked() {
                    actions.push(TreeAction::Disconnect(conn.id.to_string()));
                    ui.close();
                }
            } else if icons::menu_item(ui, icons::CONNECTION, "Connect").clicked() {
                actions.push(TreeAction::Connect(conn.id.to_string()));
                ui.close();
            }
            if connected && icons::menu_item(ui, icons::REFRESH, "Refresh").clicked() {
                actions.push(TreeAction::Refresh {
                    conn: conn.id.to_string(),
                    path: NodePath::root(),
                });
                ui.close();
            }
            let kinds = if connected {
                source.conn_new(conn.id)
            } else {
                Vec::new()
            };
            if !kinds.is_empty() {
                icons::menu_button(ui, icons::ADD, "New", |ui| {
                    for kind in kinds {
                        if icons::menu_item(ui, icons::for_object(kind), kind_label(kind)).clicked()
                        {
                            actions.push(TreeAction::NewObject {
                                conn: conn.id.to_string(),
                                node: None,
                                kind,
                            });
                            ui.close();
                        }
                    }
                });
            }
            let tools = if connected {
                source.database_tools(conn.id)
            } else {
                DatabaseTools::default()
            };
            let database = connected
                .then(|| source.connection_database(conn.id))
                .flatten();
            if tools.dump
                && let Some(database) = &database
            {
                if icons::menu_item(ui, icons::EXPORT, "Dump database…").clicked() {
                    actions.push(TreeAction::DumpDatabase {
                        conn: conn.id.to_string(),
                        database: database.clone(),
                    });
                    ui.close();
                }
                if tools.restore
                    && icons::menu_item(ui, icons::IMPORT, "Restore database…").clicked()
                {
                    actions.push(TreeAction::RestoreDatabase {
                        conn: conn.id.to_string(),
                        database: database.clone(),
                    });
                    ui.close();
                }
            }
            let panels = if connected {
                source.conn_admin(conn.id)
            } else {
                Vec::new()
            };
            if !panels.is_empty() {
                ui.separator();
                for panel in panels {
                    let icon = match panel {
                        AdminPanel::Users => icons::KEY,
                        AdminPanel::Sessions => icons::CONNECTION,
                        AdminPanel::Server => icons::SETTINGS,
                    };
                    if icons::menu_item(ui, icon, panel.label()).clicked() {
                        actions.push(TreeAction::OpenAdmin {
                            conn: conn.id.to_string(),
                            panel,
                        });
                        ui.close();
                    }
                }
                ui.separator();
            }
            let folders = source.folders();
            if !folders.is_empty() {
                icons::menu_button(ui, icons::FOLDER, "Move to folder", |ui| {
                    if conn.group.is_some()
                        && icons::menu_item(ui, icons::FOLDER, "Top level").clicked()
                    {
                        actions.push(TreeAction::MoveConnection {
                            conn: conn.id.to_string(),
                            folder: None,
                            target: TreeDrop::End,
                        });
                        ui.close();
                    }
                    for folder in folders {
                        if conn.group == Some(folder) {
                            continue;
                        }
                        if icons::menu_item(ui, icons::FOLDER, folder).clicked() {
                            actions.push(TreeAction::MoveConnection {
                                conn: conn.id.to_string(),
                                folder: Some(folder.to_string()),
                                target: TreeDrop::End,
                            });
                            ui.close();
                        }
                    }
                });
            }
            if icons::menu_item(ui, icons::EDIT, "Edit").clicked() {
                actions.push(TreeAction::Edit(conn.id.to_string()));
                ui.close();
            }
            if icons::menu_item(ui, icons::DUPLICATE, "Duplicate").clicked() {
                actions.push(TreeAction::Duplicate(conn.id.to_string()));
                ui.close();
            }
            if icons::menu_item(ui, icons::COPY, "Copy connection URL")
                .on_hover_text("Without its password")
                .clicked()
            {
                actions.push(TreeAction::CopyUrl(conn.id.to_string()));
                ui.close();
            }
            if icons::menu_item(ui, icons::DELETE, "Delete").clicked() {
                actions.push(TreeAction::Delete(conn.id.to_string()));
                ui.close();
            }
        });
    });
    drop_on_connection(ui, &hit, conn, state, palette, actions);
    if !toggle {
        return;
    }
    state.set_expanded(conn.id, &root, !open);
    if open {
        return;
    }
    let expand = TreeAction::Expand {
        conn: conn.id.to_string(),
        path: root.clone(),
    };
    match conn.badge {
        ConnBadge::MissingDriver => {}
        ConnBadge::Disconnected | ConnBadge::Failed(_) => {
            actions.push(TreeAction::Connect(conn.id.to_string()));
            actions.push(expand);
        }
        _ => {
            if matches!(source.children(conn.id, &root), Children::NotLoaded) {
                actions.push(expand);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn node_row(
    ui: &mut egui::Ui,
    conn: &str,
    driver: &str,
    node: &Node,
    depth: usize,
    open: bool,
    state: &mut TreeState,
    source: &dyn SchemaSource,
    palette: &Palette,
    actions: &mut Vec<TreeAction>,
) {
    let mut toggle = false;
    row_ui(ui, |ui| {
        ui.add_space(depth as f32 * INDENT);
        toggle |= arrow(ui, &node.name, node.has_children, open, palette.text_muted);
        let (icon, color) = icons::for_node(node.kind);
        let renaming = state
            .renaming
            .as_ref()
            .is_some_and(|(c, p, _)| c == conn && *p == node.path);
        if renaming {
            icons::paint(ui, icon, color(palette));
            rename_field(ui, conn, node, state, actions);
            return;
        }
        let selected = state.is_selected(conn, &node.path);
        let response = ui
            .horizontal(|ui| {
                // Match the connection rows: the icon sits right next to its name.
                ui.spacing_mut().item_spacing.x = Spacing::XS;
                ui.spacing_mut().button_padding.x = Spacing::XS;
                icons::paint(ui, icon, color(palette));
                ui.selectable_label(selected, &node.name)
            })
            .inner;
        if let Some(detail) = &node.detail {
            ui.label(RichText::new(detail).small().color(palette.text_muted));
        }
        // A folder shows how many children it holds once they are loaded.
        if node.kind == NodeKind::Folder
            && let Some(count) = source.child_count(conn, &node.path)
        {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(Spacing::S);
                ui.label(
                    RichText::new(count.to_string())
                        .small()
                        .color(palette.text_muted),
                );
            });
        }
        if response.clicked() {
            state.selected = Some((conn.to_string(), node.path.clone()));
            state.selected_folder = None;
        }
        if response.double_clicked() {
            if node.kind.opens_data() {
                actions.push(TreeAction::OpenData {
                    conn: conn.to_string(),
                    node: node.clone(),
                });
            } else {
                toggle |= node.has_children;
            }
        }
        theme::context_menu(&response, |ui| {
            // Key-value stores (the driver's manifest says so, spec §5.3) export keys (a
            // database, a prefix group or a key) but take no imports.
            let redis = source.key_value(conn);
            if node.kind.opens_data()
                && !redis
                && icons::menu_item(ui, icons::ROWS, "Select 100 rows").clicked()
            {
                actions.push(TreeAction::SelectRows {
                    conn: conn.to_string(),
                    node: node.clone(),
                    limit: 100,
                });
                ui.close();
            }
            if node.kind.opens_data() && icons::menu_item(ui, icons::ROWS, "Open data").clicked() {
                actions.push(TreeAction::OpenData {
                    conn: conn.to_string(),
                    node: node.clone(),
                });
                ui.close();
            }
            if source.ddl(conn, node) && icons::menu_item(ui, icons::SQL, "Open DDL").clicked() {
                actions.push(TreeAction::OpenDdl {
                    conn: conn.to_string(),
                    node: node.clone(),
                });
                ui.close();
            }
            let exports = if redis {
                matches!(
                    node.kind,
                    NodeKind::Database | NodeKind::Folder | NodeKind::Table
                )
            } else {
                node.kind.opens_data()
            };
            if exports && icons::menu_item(ui, icons::EXPORT, "Export data…").clicked() {
                actions.push(TreeAction::ExportData {
                    conn: conn.to_string(),
                    node: node.clone(),
                });
                ui.close();
            }
            if !redis
                && node.kind == NodeKind::Table
                && icons::menu_item(ui, icons::IMPORT, "Import data…").clicked()
            {
                actions.push(TreeAction::ImportData {
                    conn: conn.to_string(),
                    node: node.clone(),
                });
                ui.close();
            }
            if !redis
                && node.kind == NodeKind::Table
                && icons::menu_item(ui, icons::IMPORT, "Generate data…").clicked()
            {
                actions.push(TreeAction::GenerateData {
                    conn: conn.to_string(),
                    node: node.clone(),
                });
                ui.close();
            }
            if !redis
                && node.kind == NodeKind::Table
                && icons::menu_item(ui, icons::EXPORT, "Transfer data…").clicked()
            {
                actions.push(TreeAction::TransferData {
                    conn: conn.to_string(),
                    node: node.clone(),
                });
                ui.close();
            }
            let tools = source.database_tools(conn);
            if tools.dump
                && matches!(node.kind, NodeKind::Schema | NodeKind::Database)
                && icons::menu_item(ui, icons::TABLE, "ER diagram").clicked()
            {
                actions.push(TreeAction::ErDiagram {
                    conn: conn.to_string(),
                    node: node.clone(),
                });
                ui.close();
            }
            if tools.dump
                && matches!(node.kind, NodeKind::Schema | NodeKind::Database)
                && icons::menu_item(ui, icons::EXPORT, "Compare with…").clicked()
            {
                actions.push(TreeAction::CompareSchema {
                    conn: conn.to_string(),
                    node: node.clone(),
                });
                ui.close();
            }
            if is_database_node(driver, node) && tools.dump {
                if icons::menu_item(ui, icons::EXPORT, "Dump database…").clicked() {
                    actions.push(TreeAction::DumpDatabase {
                        conn: conn.to_string(),
                        database: node.name.clone(),
                    });
                    ui.close();
                }
                if tools.restore
                    && icons::menu_item(ui, icons::IMPORT, "Restore database…").clicked()
                {
                    actions.push(TreeAction::RestoreDatabase {
                        conn: conn.to_string(),
                        database: node.name.clone(),
                    });
                    ui.close();
                }
            }
            // A key-value database has no queries: it opens the key browser.
            let (open_label, open_icon) = if redis {
                ("Open key browser", icons::KEY)
            } else {
                ("New query", icons::QUERY)
            };
            if node.kind == NodeKind::Database
                && icons::menu_item(ui, open_icon, open_label).clicked()
            {
                actions.push(TreeAction::NewQuery {
                    conn: conn.to_string(),
                    database: node.name.clone(),
                });
                ui.close();
            }
            object_menu(ui, conn, node, state, source, actions);
            if icons::menu_item(ui, icons::REFRESH, "Refresh").clicked() {
                actions.push(TreeAction::Refresh {
                    conn: conn.to_string(),
                    path: node.path.clone(),
                });
                ui.close();
            }
            if icons::menu_item(ui, icons::COPY, "Copy name").clicked() {
                actions.push(TreeAction::CopyName {
                    conn: conn.to_string(),
                    node: node.clone(),
                    qualified: false,
                });
                ui.close();
            }
            if !redis
                && node.kind.opens_data()
                && icons::menu_item(ui, icons::COPY, "Copy qualified name").clicked()
            {
                actions.push(TreeAction::CopyName {
                    conn: conn.to_string(),
                    node: node.clone(),
                    qualified: true,
                });
                ui.close();
            }
        });
    });
    if !toggle {
        return;
    }
    state.set_expanded(conn, &node.path, !open);
    if !open && matches!(source.children(conn, &node.path), Children::NotLoaded) {
        actions.push(TreeAction::Expand {
            conn: conn.to_string(),
            path: node.path.clone(),
        });
    }
}

/// The inline rename field, when `node` is being renamed; true when it was drawn.
fn rename_field(
    ui: &mut egui::Ui,
    conn: &str,
    node: &Node,
    state: &mut TreeState,
    actions: &mut Vec<TreeAction>,
) -> bool {
    let Some((c, path, text)) = &mut state.renaming else {
        return false;
    };
    if c != conn || *path != node.path {
        return false;
    }
    let response = crate::text_menu::text_edit(
        ui,
        TextEdit::singleline(text)
            .desired_width(180.0)
            .hint_text("new name"),
    );
    if !response.has_focus() && !response.lost_focus() {
        response.request_focus();
    }
    let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
    if response.lost_focus() || escape {
        let to = text.trim().to_string();
        if enter && !escape && !to.is_empty() && to != node.name {
            actions.push(TreeAction::RenameObject {
                conn: conn.to_string(),
                node: node.clone(),
                to,
            });
        }
        state.renaming = None;
    }
    true
}

/// The object entries of a node's context menu, as the source allows them.
fn object_menu(
    ui: &mut egui::Ui,
    conn: &str,
    node: &Node,
    state: &mut TreeState,
    source: &dyn SchemaSource,
    actions: &mut Vec<TreeAction>,
) {
    let menu = source.node_menu(conn, node);
    if menu == NodeMenu::default() {
        return;
    }
    ui.separator();
    let action = |a: TreeAction, actions: &mut Vec<TreeAction>, ui: &mut egui::Ui| {
        actions.push(a);
        ui.close();
    };
    let (c, n) = (conn.to_string(), node.clone());
    if menu.open && icons::menu_item(ui, icons::EDIT, "Open editor").clicked() {
        action(
            TreeAction::OpenObject {
                conn: c.clone(),
                node: n.clone(),
            },
            actions,
            ui,
        );
    }
    if !menu.new.is_empty() {
        icons::menu_button(ui, icons::ADD, "New", |ui| {
            for kind in &menu.new {
                if icons::menu_item(ui, icons::for_object(*kind), kind_label(*kind)).clicked() {
                    action(
                        TreeAction::NewObject {
                            conn: c.clone(),
                            node: Some(n.clone()),
                            kind: *kind,
                        },
                        actions,
                        ui,
                    );
                }
            }
        });
    }
    if !menu.generate.is_empty() {
        icons::menu_button(ui, icons::QUERY, "Generate SQL", |ui| {
            for template in &menu.generate {
                if icons::menu_item(
                    ui,
                    icons::for_template(*template),
                    crate::objects::template_label(*template),
                )
                .clicked()
                {
                    action(
                        TreeAction::GenerateSql {
                            conn: c.clone(),
                            node: n.clone(),
                            template: *template,
                        },
                        actions,
                        ui,
                    );
                }
            }
        });
        if menu.generate.contains(&SqlTemplate::Create)
            && icons::menu_item(ui, icons::COPY, "Copy DDL").clicked()
        {
            action(
                TreeAction::CopyDdl {
                    conn: c.clone(),
                    node: n.clone(),
                },
                actions,
                ui,
            );
        }
    }
    if menu.rename
        && icons::menu_item(ui, icons::EDIT, "Rename")
            .on_hover_text("F2")
            .clicked()
    {
        state.selected = Some((c.clone(), n.path.clone()));
        state.selected_folder = None;
        state.renaming = Some((c.clone(), n.path.clone(), n.name.clone()));
        ui.close();
    }
    if menu.truncate && icons::menu_item(ui, icons::TRUNCATE, "Truncate…").clicked() {
        action(
            TreeAction::Truncate {
                conn: c.clone(),
                node: n.clone(),
            },
            actions,
            ui,
        );
    }
    if menu.drop && icons::menu_item(ui, icons::DELETE, "Drop…").clicked() {
        action(TreeAction::DropObject { conn: c, node: n }, actions, ui);
    }
}
