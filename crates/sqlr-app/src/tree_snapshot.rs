//! The explorer's per-frame snapshot of the engine's tree caches.

use std::collections::{HashMap, HashSet};

use sqlr_core::{
    ConnectionConfig, DatabaseTools, DriverManifest, Node, NodePath, ObjectEditing, ObjectKind,
};
use sqlr_engine::{Engine, LoadState, SessionStatus};
use sqlr_ui::{Children, ConnBadge, NodeMenu, SchemaSource, TreeConn, TreeState};

/// The per-frame owned copy of what the explorer shows (ruling R3).
///
/// `schema_tree` borrows children from its source while the engine hands out owned values,
/// so each frame copies the loaded children of every path the tree can display: expanded
/// paths, or every loaded path while the filter is active. An expanded path whose children
/// are not cached (dropped by a refresh, a reconnect or a disconnect) is browsed again while
/// its session is live.
pub struct TreeSnapshot<'a> {
    engine: &'a Engine,
    /// Object editing support per connection menus.
    editing: &'a HashMap<String, ObjectEditing>,
    conns: Vec<SnapConn>,
    /// Folder names in display order.
    folders: Vec<String>,
    /// By connection, then path: `children()` is two hash lookups, no allocation.
    children: HashMap<String, HashMap<NodePath, LoadState<Vec<Node>>>>,
}

struct SnapConn {
    id: String,
    name: String,
    driver: String,
    /// The driver's name, or a SQLite connection's file name.
    detail: Option<String>,
    /// The database a connection-level dump uses.
    database: Option<String>,
    group: Option<String>,
    color: Option<[u8; 3]>,
    tint: Option<[u8; 3]>,
    open_transaction: bool,
    agent_access: sqlr_core::AgentAccess,
    badge: ConnBadge,
}

impl<'a> TreeSnapshot<'a> {
    /// `tints` holds the profile color of each connection that has one; `editing` the
    /// object editing support of each connection's driver.
    pub fn build(
        engine: &'a Engine,
        connections: &[ConnectionConfig],
        folders: &[String],
        tree: &TreeState,
        tints: &HashMap<String, [u8; 3]>,
        editing: &'a HashMap<String, ObjectEditing>,
    ) -> Self {
        let open: HashSet<String> = engine
            .open_transactions()
            .into_iter()
            .map(|(key, _)| key.conn)
            .collect();
        let filtering = !tree.filter.is_empty();
        let root = NodePath::root();
        let mut children = HashMap::new();
        let conns = connections
            .iter()
            .map(|cfg| {
                let missing =
                    engine.drivers_loaded() && engine.drivers().get(&cfg.driver).is_none();
                let badge = match engine.session_status(&cfg.id) {
                    _ if missing => ConnBadge::MissingDriver,
                    None | Some(SessionStatus::Disconnected) => ConnBadge::Disconnected,
                    Some(SessionStatus::Connecting) => ConnBadge::Connecting,
                    Some(SessionStatus::Connected(_)) => ConnBadge::Connected,
                    Some(SessionStatus::Failed(e)) => ConnBadge::Failed(e.message),
                };
                let live = matches!(badge, ConnBadge::Connecting | ConnBadge::Connected);
                if tree.is_expanded(&cfg.id, &root) {
                    let mut loaded = HashMap::new();
                    let mut stack = vec![root.clone()];
                    while let Some(path) = stack.pop() {
                        let mut state = engine.children(&cfg.id, &path);
                        if state.is_none() && live && tree.is_expanded(&cfg.id, &path) {
                            // Returns at once; the children show as loading from now on.
                            engine.browse(&cfg.id, path.clone());
                            state = engine.children(&cfg.id, &path);
                        }
                        let Some(state) = state else {
                            continue;
                        };
                        if let LoadState::Ready(nodes) = &state {
                            stack.extend(
                                nodes
                                    .iter()
                                    .filter(|n| {
                                        n.has_children
                                            && (filtering || tree.is_expanded(&cfg.id, &n.path))
                                    })
                                    .map(|n| n.path.clone()),
                            );
                        }
                        loaded.insert(path, state);
                    }
                    children.insert(cfg.id.clone(), loaded);
                }
                let manifest = engine.manifest(&cfg.driver);
                let detail = conn_detail(cfg, manifest.as_ref());
                let database = if cfg.driver == "sqlite" {
                    Some("main".to_string())
                } else {
                    manifest.as_ref().and_then(|m| {
                        cfg.value(m, "database")
                            .map(str::trim)
                            .filter(|d| !d.is_empty())
                            .map(str::to_string)
                    })
                };
                SnapConn {
                    id: cfg.id.clone(),
                    name: cfg.name.clone(),
                    driver: cfg.driver.clone(),
                    detail,
                    database,
                    group: cfg.group.clone(),
                    color: cfg.color,
                    tint: tints.get(&cfg.id).copied(),
                    open_transaction: open.contains(&cfg.id),
                    agent_access: cfg.agent_access,
                    badge,
                }
            })
            .collect();
        Self {
            engine,
            editing,
            conns,
            folders: folders.to_vec(),
            children,
        }
    }

    fn connected(&self, conn: &str) -> bool {
        self.conns
            .iter()
            .any(|c| c.id == conn && c.badge == ConnBadge::Connected)
    }
}

/// What a connection row shows after its name: a SQLite connection's file name, else its
/// driver's name.
fn conn_detail(cfg: &ConnectionConfig, manifest: Option<&DriverManifest>) -> Option<String> {
    let manifest = manifest?;
    if let Some(path) = cfg.value(manifest, "path").filter(|p| !p.trim().is_empty()) {
        let name = std::path::Path::new(path.trim())
            .file_name()
            .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
        return Some(name);
    }
    Some(manifest.name.clone())
}

impl SchemaSource for TreeSnapshot<'_> {
    fn child_count(&self, conn: &str, path: &NodePath) -> Option<usize> {
        match self.children.get(conn).and_then(|m| m.get(path)) {
            Some(LoadState::Ready(nodes)) => Some(nodes.len()),
            // A collapsed folder is not copied each frame: the engine counts its children.
            _ => self.engine.child_count(conn, path),
        }
    }

    fn ddl(&self, conn: &str, node: &Node) -> bool {
        self.editing.contains_key(conn)
            && self.connected(conn)
            && self.engine.object_ref(conn, node).is_some()
    }

    fn node_menu(&self, conn: &str, node: &Node) -> NodeMenu {
        crate::objects::menus::node_menu(self.engine, self.editing.get(conn), conn, node)
    }

    fn conn_new(&self, conn: &str) -> Vec<ObjectKind> {
        match self.editing.get(conn) {
            Some(editing)
                if self.connected(conn) && !crate::objects::menus::read_only(self.engine, conn) =>
            {
                crate::objects::menus::conn_new(editing)
            }
            _ => Vec::new(),
        }
    }

    fn key_value(&self, conn: &str) -> bool {
        self.conns
            .iter()
            .find(|c| c.id == conn)
            .and_then(|c| self.engine.manifest(&c.driver))
            .is_some_and(|m| m.family == sqlr_core::Family::KeyValue)
    }

    fn database_tools(&self, conn: &str) -> DatabaseTools {
        if !self.connected(conn) {
            return DatabaseTools::default();
        }
        self.conns
            .iter()
            .find(|c| c.id == conn)
            .and_then(|c| self.engine.manifest(&c.driver))
            .map(|m| DatabaseTools {
                dump: m.capabilities.dump,
                restore: m.capabilities.dump,
            })
            .unwrap_or_default()
    }

    fn connection_database(&self, conn: &str) -> Option<String> {
        self.conns
            .iter()
            .find(|c| c.id == conn)
            .and_then(|c| c.database.clone())
    }

    fn conn_admin(&self, conn: &str) -> Vec<sqlr_ui::AdminPanel> {
        if !self.connected(conn) {
            return Vec::new();
        }
        self.engine
            .admin_support(conn)
            .map(|s| sqlr_ui::AdminPanel::for_support(&s))
            .unwrap_or_default()
    }

    fn connections(&self) -> Vec<TreeConn<'_>> {
        self.conns
            .iter()
            .map(|c| TreeConn {
                id: &c.id,
                name: &c.name,
                driver: &c.driver,
                detail: c.detail.as_deref(),
                group: c.group.as_deref(),
                color: c.color,
                badge: c.badge.clone(),
                tint: c.tint,
                open_transaction: c.open_transaction,
                agent_access: c.agent_access,
            })
            .collect()
    }

    fn folders(&self) -> Vec<&str> {
        self.folders.iter().map(String::as_str).collect()
    }

    fn children(&self, conn: &str, path: &NodePath) -> Children<'_> {
        match self.children.get(conn).and_then(|m| m.get(path)) {
            None => Children::NotLoaded,
            Some(LoadState::Loading) => Children::Loading,
            Some(LoadState::Ready(nodes)) => Children::Ready(nodes),
            Some(LoadState::Failed(e)) => Children::Failed(&e.message),
        }
    }
}
