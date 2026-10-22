//! The explorer's object menu entries: what a node offers, decided by the driver's
//! `ObjectEditing` and the object the node stands for (spec §5.3: only what works shows).

use sqlr_core::{Node, NodeKind, ObjectEditing, ObjectKind, SqlTemplate};
use sqlr_engine::Engine;
use sqlr_ui::{NodeMenu, templates_for};

/// Kinds "New ▸" creates under `node`, among those the driver creates.
pub(crate) fn new_kinds(node: &Node, editing: &ObjectEditing) -> Vec<ObjectKind> {
    use ObjectKind::*;
    let wanted: Vec<ObjectKind> = match node.kind {
        NodeKind::Schema => vec![Table, View, MaterializedView, Sequence, Function, Procedure],
        NodeKind::Database => vec![Schema],
        NodeKind::Table => vec![Index, Trigger],
        NodeKind::Folder => match node.name.as_str() {
            "tables" => vec![Table],
            "views" => vec![View],
            "materialized views" => vec![MaterializedView],
            "sequences" => vec![Sequence],
            "functions" => vec![Function, Procedure],
            "procedures" => vec![Procedure],
            "indexes" => vec![Index],
            "triggers" => vec![Trigger],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    wanted
        .into_iter()
        .filter(|k| editing.create.contains(k))
        .collect()
}

/// Whether `conn`'s explorer session is read-only: its menus offer no DDL (as the
/// administration actions, [`crate::admin::actions_blocked`]).
pub(crate) fn read_only(engine: &Engine, conn: &str) -> bool {
    engine
        .transaction(&sqlr_engine::SessionKey::meta(conn))
        .is_some_and(|t| t.read_only)
}

/// Kinds "New ▸" creates at the connection's root.
pub(crate) fn conn_new(editing: &ObjectEditing) -> Vec<ObjectKind> {
    [ObjectKind::Database, ObjectKind::Schema]
        .into_iter()
        .filter(|k| editing.create.contains(k))
        .collect()
}

/// The object entries of `node`'s context menu on `conn`.
pub(crate) fn node_menu(
    engine: &Engine,
    editing: Option<&ObjectEditing>,
    conn: &str,
    node: &Node,
) -> NodeMenu {
    let Some(editing) = editing else {
        // Document databases offer index administration only.
        return crate::nosql::index_admin::node_menu(engine, conn, node);
    };
    let writable = !read_only(engine, conn);
    let new = if writable {
        new_kinds(node, editing)
    } else {
        Vec::new()
    };
    let Some(object) = engine.object_ref(conn, node) else {
        return NodeMenu {
            new,
            ..Default::default()
        };
    };
    let kind = object.kind;
    let generate = if kind == ObjectKind::Role {
        Vec::new()
    } else {
        templates_for(kind)
            .into_iter()
            .filter(|t| *t != SqlTemplate::Drop || editing.drop.contains(&kind))
            .collect()
    };
    NodeMenu {
        open: kind != ObjectKind::Role,
        new,
        drop: writable && editing.drop.contains(&kind),
        rename: writable && editing.rename.contains(&kind),
        truncate: writable && kind == ObjectKind::Table,
        generate,
    }
}
