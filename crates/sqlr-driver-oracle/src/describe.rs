//! Table descriptions from Oracle's data dictionary.

use oracle_rs::Connection;
use sqlr_core::{DriverError, ErrorKind, TableInfo, TableRef};

use crate::browse::{column_defs, query};

const KEYS_SQL: &str = "\
SELECT ac.constraint_type, ac.constraint_name, acc.column_name, acc.position \
FROM all_constraints ac \
JOIN all_cons_columns acc ON acc.owner = ac.owner AND acc.constraint_name = ac.constraint_name \
WHERE ac.owner = :1 AND ac.table_name = :2 AND ac.constraint_type IN ('P', 'U') \
  AND ac.status = 'ENABLED' \
ORDER BY ac.constraint_type, ac.constraint_name, acc.position";

pub(crate) async fn describe(
    conn: &Connection,
    table: &TableRef,
) -> Result<TableInfo, DriverError> {
    let schema = match table.schema.as_deref() {
        Some(schema) if !schema.is_empty() => schema.to_string(),
        _ => query(
            conn,
            "SELECT SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA') FROM dual",
            &[],
        )
        .await?
        .rows
        .first()
        .and_then(|r| r.get(0))
        .and_then(oracle_rs::Value::as_str)
        .unwrap_or_default()
        .to_string(),
    };
    let columns = column_defs(conn, &schema, &table.name).await?;
    if columns.is_empty() {
        return Err(DriverError::new(
            ErrorKind::Other,
            format!("table {}.{} not found", schema, table.name),
        ));
    }
    let result = query(conn, KEYS_SQL, &[&schema, &table.name]).await?;
    let mut groups: Vec<(String, bool, Vec<String>)> = Vec::new();
    for row in &result.rows {
        let kind = row.get(0).and_then(oracle_rs::Value::as_str).unwrap_or("U");
        let Some(name) = row.get(1).and_then(oracle_rs::Value::as_str) else {
            continue;
        };
        let Some(column) = row.get(2).and_then(oracle_rs::Value::as_str) else {
            continue;
        };
        match groups.iter_mut().find(|(group, _, _)| group == name) {
            Some((_, _, columns)) => columns.push(column.to_string()),
            None => groups.push((name.to_string(), kind == "P", vec![column.to_string()])),
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
            schema: Some(schema),
            name: table.name.clone(),
        },
        columns,
        primary_key,
        unique_keys,
    })
}
