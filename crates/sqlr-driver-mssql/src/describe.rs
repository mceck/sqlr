//! Table descriptions from SQL Server's catalog views.

use sqlr_core::{DriverError, ErrorKind, TableInfo, TableRef};

use crate::browse::{MsClient, column_defs};
use crate::map_error;

const KEYS_SQL: &str = "\
SELECT i.name, c.name, i.is_primary_key \
FROM sys.indexes i \
JOIN sys.objects o ON o.object_id = i.object_id \
JOIN sys.schemas s ON s.schema_id = o.schema_id \
JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
WHERE s.name = @P1 AND o.name = @P2 \
  AND i.is_unique = 1 AND i.has_filter = 0 AND ic.is_included_column = 0 \
ORDER BY i.is_primary_key DESC, i.name, ic.key_ordinal";

pub(crate) async fn describe(
    client: &mut MsClient,
    table: &TableRef,
) -> Result<TableInfo, DriverError> {
    let schema = table.schema.as_deref().unwrap_or("dbo");
    let columns = column_defs(client, schema, &table.name).await?;
    if columns.is_empty() {
        return Err(DriverError::new(
            ErrorKind::Other,
            format!("table {schema}.{} not found", table.name),
        ));
    }
    let rows = client
        .query(KEYS_SQL, &[&schema, &table.name])
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)?;
    let mut groups: Vec<(String, bool, Vec<String>)> = Vec::new();
    for row in rows.iter() {
        let index = row.get::<&str, _>(0).unwrap_or_default().to_string();
        let column = row.get::<&str, _>(1).unwrap_or_default().to_string();
        let primary = row.get::<bool, _>(2).unwrap_or(false);
        match groups.iter_mut().find(|(name, _, _)| name == &index) {
            Some((_, _, columns)) => columns.push(column),
            None => groups.push((index, primary, vec![column])),
        }
    }
    let mut primary_key = Vec::new();
    let mut unique_keys = Vec::new();
    for (_, primary, columns) in groups {
        if primary {
            primary_key = columns;
        } else {
            unique_keys.push(columns);
        }
    }
    Ok(TableInfo {
        table: TableRef {
            database: table.database.clone(),
            schema: Some(schema.to_string()),
            name: table.name.clone(),
        },
        columns,
        primary_key,
        unique_keys,
    })
}
