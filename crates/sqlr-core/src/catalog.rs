//! Cached schema metadata used by autocomplete and the command palette.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationKind {
    Table,
    View,
    MaterializedView,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogColumn {
    pub name: String,
    pub type_name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogRelation {
    pub name: String,
    pub kind: RelationKind,
    pub columns: Vec<CatalogColumn>,
}

/// All relations of one schema, sorted by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaCatalog {
    pub schema: String,
    pub relations: Vec<CatalogRelation>,
}

/// A catalog piece autocomplete is missing and wants loaded.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CatalogNeed {
    Schemas,
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
