//! Cached schema metadata used by autocomplete and the command palette.

use serde::{Deserialize, Serialize};

use crate::NodeKind;

/// What a catalog relation is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationKind {
    /// A base table.
    Table,
    /// A view.
    View,
    /// A materialized view.
    MaterializedView,
}

impl RelationKind {
    /// The relation kind of an Explorer node; `None` for nodes that are no relation.
    pub fn of_node(kind: NodeKind) -> Option<Self> {
        match kind {
            NodeKind::Table => Some(Self::Table),
            NodeKind::View => Some(Self::View),
            NodeKind::MaterializedView => Some(Self::MaterializedView),
            _ => None,
        }
    }
}

/// A relation whose data the user opened lately, listed first by the object Palette even
/// while its Connection is closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentObject {
    /// The Connection it belongs to.
    pub conn_id: String,
    /// The server database it lives in, when it is not the Connection's own.
    pub database: Option<String>,
    /// Its schema.
    pub schema: String,
    /// Its name.
    pub name: String,
    /// Whether it is a table, a view or a materialized view.
    pub kind: RelationKind,
    /// When it was last opened, in Unix milliseconds.
    pub used_at_ms: i64,
}

/// A column of a cached relation, as autocomplete sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogColumn {
    /// Column name.
    pub name: String,
    /// The column's type name as the driver reports it.
    pub type_name: String,
}

/// A table or view of a cached schema, with its columns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogRelation {
    /// Relation name.
    pub name: String,
    /// Whether it is a table, a view or a materialized view.
    pub kind: RelationKind,
    /// Its columns, in ordinal order.
    pub columns: Vec<CatalogColumn>,
}

/// All relations of one schema, sorted by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaCatalog {
    /// Schema name.
    pub schema: String,
    /// The schema's relations.
    pub relations: Vec<CatalogRelation>,
}

/// A catalog piece autocomplete is missing and wants loaded.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CatalogNeed {
    /// The list of schemas of the connection.
    Schemas,
    /// The relations of the named schema.
    Schema(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_catalog_serde_round_trip() {
        let cat = SchemaCatalog {
            schema: "public".into(),
            relations: vec![CatalogRelation {
                name: "v_users".into(),
                kind: RelationKind::View,
                columns: vec![
                    CatalogColumn {
                        name: "id".into(),
                        type_name: "int4".into(),
                    },
                    CatalogColumn {
                        name: "name".into(),
                        type_name: "text".into(),
                    },
                ],
            }],
        };
        let json = serde_json::to_string(&cat).unwrap();
        assert_eq!(serde_json::from_str::<SchemaCatalog>(&json).unwrap(), cat);
    }
}
