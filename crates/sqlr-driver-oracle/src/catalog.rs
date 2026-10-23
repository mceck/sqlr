//! Schema catalog for autocomplete.

use oracle_rs::Connection;
use sqlr_core::{CatalogColumn, CatalogRelation, DriverError, RelationKind, SchemaCatalog};

use crate::browse::{query, type_name};

const CATALOG_SQL: &str = "\
SELECT c.table_name, \
       CASE WHEN v.view_name IS NULL THEN 'TABLE' ELSE 'VIEW' END, \
       c.column_name, c.data_type, c.char_length, c.data_length, c.data_precision, c.data_scale \
FROM all_tab_columns c \
LEFT JOIN all_views v ON v.owner = c.owner AND v.view_name = c.table_name \
WHERE c.owner = :1 \
ORDER BY c.table_name, c.column_id";

pub(crate) async fn catalog(conn: &Connection, schema: &str) -> Result<SchemaCatalog, DriverError> {
    let result = query(conn, CATALOG_SQL, &[schema]).await?;
    let mut relations: Vec<CatalogRelation> = Vec::new();
    for row in &result.rows {
        let Some(name) = row.get(0).and_then(oracle_rs::Value::as_str) else {
            continue;
        };
        if relations.last().is_none_or(|r| r.name != name) {
            let kind = row
                .get(1)
                .and_then(oracle_rs::Value::as_str)
                .unwrap_or("TABLE");
            relations.push(CatalogRelation {
                name: name.to_string(),
                kind: if kind == "VIEW" {
                    RelationKind::View
                } else {
                    RelationKind::Table
                },
                columns: Vec::new(),
            });
        }
        let Some(column) = row.get(2).and_then(oracle_rs::Value::as_str) else {
            continue;
        };
        let data_type = row
            .get(3)
            .and_then(oracle_rs::Value::as_str)
            .unwrap_or("UNKNOWN");
        if let Some(relation) = relations.last_mut() {
            relation.columns.push(CatalogColumn {
                name: column.to_string(),
                type_name: type_name(
                    data_type,
                    row.get_i64(4),
                    row.get_i64(5),
                    row.get_i64(6),
                    row.get_i64(7),
                ),
            });
        }
    }
    Ok(SchemaCatalog {
        schema: schema.to_string(),
        relations,
    })
}
