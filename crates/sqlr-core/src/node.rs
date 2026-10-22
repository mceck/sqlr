//! Explorer tree nodes.

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
    pub fn root() -> Self {
        Self(Vec::new())
    }

    pub fn child(&self, segment: &str) -> Self {
        let mut v = self.0.clone();
        v.push(segment.to_string());
        Self(v)
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn last(&self) -> Option<&str> {
        self.0.last().map(String::as_str)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum NodeKind {
    Database,
    Schema,
    Folder,
    Table,
    View,
    MaterializedView,
    Column,
    Index,
    Function,
    Procedure,
    Sequence,
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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Node {
    pub path: NodePath,
    pub name: String,
    pub kind: NodeKind,
    pub has_children: bool,
    pub detail: Option<String>,
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
