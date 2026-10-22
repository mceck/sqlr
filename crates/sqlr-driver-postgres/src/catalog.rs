//! Schema catalog for autocomplete.

use sqlr_core::{CatalogColumn, CatalogRelation, DriverError, RelationKind, SchemaCatalog};
use tokio_postgres::Client;

use crate::map_error;

const CATALOG_SQL: &str = "\
SELECT c.relname, c.relkind::text, a.attname, format_type(a.atttypid, a.atttypmod)
FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
LEFT JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
WHERE n.nspname = $1 AND c.relkind IN ('r','p','v','m')
ORDER BY c.relname, a.attnum";

pub(crate) async fn catalog(meta: &Client, schema: &str) -> Result<SchemaCatalog, DriverError> {
    let rows = meta
        .query(CATALOG_SQL, &[&schema])
        .await
        .map_err(map_error)?;
    let mut relations: Vec<CatalogRelation> = Vec::new();
    for row in rows {
        let name: String = row.get(0);
        if relations.last().is_none_or(|r| r.name != name) {
            let kind: String = row.get(1);
            relations.push(CatalogRelation {
                name,
                kind: match kind.as_str() {
                    "v" => RelationKind::View,
                    "m" => RelationKind::MaterializedView,
                    _ => RelationKind::Table,
                },
                columns: Vec::new(),
            });
        }
        let column: Option<String> = row.get(2);
        if let (Some(name), Some(rel)) = (column, relations.last_mut()) {
            rel.columns.push(CatalogColumn {
                name,
                type_name: row.get(3),
            });
        }
    }
    Ok(SchemaCatalog {
        schema: schema.to_string(),
        relations,
    })
}
