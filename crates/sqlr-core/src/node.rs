//! Explorer tree nodes.

/// The position of a node in the explorer tree: the names of its ancestors from the root, then its own.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct NodePath(pub Vec<String>);

impl NodePath {
    /// The empty path, which is the connection itself.
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// The path of the child named `segment`.
    pub fn child(&self, segment: &str) -> Self {
        let mut v = self.0.clone();
        v.push(segment.to_string());
        Self(v)
    }

    /// The names from the root.
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// Whether this is the root.
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The node's own name, `None` at the root.
    pub fn last(&self) -> Option<&str> {
        self.0.last().map(String::as_str)
    }
}

/// What an explorer node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum NodeKind {
    /// A database of the server.
    Database,
    /// A schema.
    Schema,
    /// A folder grouping child nodes.
    Folder,
    /// A table.
    Table,
    /// A view.
    View,
    /// A materialized view.
    MaterializedView,
    /// A column.
    Column,
    /// An index.
    Index,
    /// A function.
    Function,
    /// A stored procedure.
    Procedure,
    /// A sequence.
    Sequence,
    /// A trigger.
    Trigger,
}

impl NodeKind {
    /// True for kinds that can be opened as a data grid.
    pub fn opens_data(self) -> bool {
        matches!(
            self,
            NodeKind::Table | NodeKind::View | NodeKind::MaterializedView
        )
    }
}

/// An entry of the explorer tree.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Node {
    /// Where it sits in the tree.
    pub path: NodePath,
    /// Name shown for it.
    pub name: String,
    /// What it is.
    pub kind: NodeKind,
    /// Whether it can be expanded.
    pub has_children: bool,
    /// Extra text shown beside the name.
    pub detail: Option<String>,
}

impl Node {
    /// A folder the explorer hides while it holds no objects: every folder but `tables`,
    /// which always shows (and is never loaded ahead of its opening).
    pub fn hides_when_empty(&self) -> bool {
        self.kind == NodeKind::Folder && !self.name.eq_ignore_ascii_case("tables")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_and_last() {
        let p = NodePath::root().child("public").child("tables");
        assert_eq!(p.segments(), ["public", "tables"]);
        assert_eq!(p.last(), Some("tables"));
        assert!(!p.is_root());
    }

    #[test]
    fn opens_data_only_for_relations() {
        assert!(NodeKind::Table.opens_data());
        assert!(NodeKind::View.opens_data());
        assert!(NodeKind::MaterializedView.opens_data());
        assert!(!NodeKind::Column.opens_data());
        assert!(!NodeKind::Database.opens_data());
    }
}
