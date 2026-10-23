//! Schema catalog for autocomplete.

use sqlr_core::{CatalogColumn, CatalogRelation, DriverError, RelationKind, SchemaCatalog};

use crate::browse::{MsClient, type_name};
use crate::map_error;

const CATALOG_SQL: &str = "\
SELECT o.name, o.type, c.name, ty.name, c.max_length, c.precision, c.scale \
FROM sys.objects o \
JOIN sys.schemas s ON s.schema_id = o.schema_id \
LEFT JOIN sys.columns c ON c.object_id = o.object_id \
LEFT JOIN sys.types ty ON ty.user_type_id = c.user_type_id \
WHERE s.name = @P1 AND o.type IN ('U', 'V') AND o.is_ms_shipped = 0 \
ORDER BY o.name, c.column_id";

pub(crate) async fn catalog(
    client: &mut MsClient,
    schema: &str,
) -> Result<SchemaCatalog, DriverError> {
    let rows = client
        .query(CATALOG_SQL, &[&schema])
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)?;
    let mut relations: Vec<CatalogRelation> = Vec::new();
    for row in rows.iter() {
        let name = row.get::<&str, _>(0).unwrap_or_default();
        if relations.last().is_none_or(|r| r.name != name) {
            // `sys.objects.type` is `char(2)`: a view comes back as `V `.
            let kind = row.get::<&str, _>(1).unwrap_or_default().trim();
            relations.push(CatalogRelation {
                name: name.to_string(),
                kind: if kind == "V" {
                    RelationKind::View
                } else {
                    RelationKind::Table
                },
                columns: Vec::new(),
            });
        }
        let Some(column) = row.get::<&str, _>(2) else {
            continue;
        };
        let base = row.get::<&str, _>(3).unwrap_or("sql_variant");
        let type_name = type_name(
            base,
            row.get::<i16, _>(4).unwrap_or(0),
            row.get::<u8, _>(5).unwrap_or(0),
            row.get::<u8, _>(6).unwrap_or(0),
        );
        if let Some(relation) = relations.last_mut() {
            relation.columns.push(CatalogColumn {
                name: column.to_string(),
                type_name,
            });
        }
    }
    Ok(SchemaCatalog {
        schema: schema.to_string(),
        relations,
    })
}
