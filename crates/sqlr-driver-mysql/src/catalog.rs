//! Metadata from `information_schema` (always parameterized): the autocomplete catalog and
//! table descriptions.

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Params, Row, Value};
use sqlr_core::{
    CatalogColumn, CatalogRelation, DriverError, ErrorKind, RelationKind, SchemaCatalog,
    TableColumn, TableInfo, TableRef,
};

use crate::map_error;

/// Runs a parameterized metadata query.
pub(crate) async fn rows(
    conn: &mut Conn,
    sql: &str,
    params: &[&str],
) -> Result<Vec<Row>, DriverError> {
    let params = if params.is_empty() {
        Params::Empty
    } else {
        Params::Positional(params.iter().map(|p| Value::from(*p)).collect())
    };
    conn.exec(sql, params).await.map_err(map_error)
}

/// Column `i` as text; `None` for NULL.
pub(crate) fn text(row: &Row, i: usize) -> Option<String> {
    match row.as_ref(i)? {
        Value::NULL => None,
        Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        Value::Int(v) => Some(v.to_string()),
        Value::UInt(v) => Some(v.to_string()),
        other => Some(other.as_sql(true)),
    }
}

/// Column `i` as text, empty for NULL.
pub(crate) fn string(row: &Row, i: usize) -> String {
    text(row, i).unwrap_or_default()
}

/// Column `i` as an integer; `None` for NULL or non-numbers.
pub(crate) fn int(row: &Row, i: usize) -> Option<i64> {
    match row.as_ref(i)? {
        Value::Int(v) => Some(*v),
        Value::UInt(v) => i64::try_from(*v).ok(),
        Value::Bytes(b) => std::str::from_utf8(b).ok()?.parse().ok(),
        _ => None,
    }
}

const CATALOG_SQL: &str = "\
SELECT t.TABLE_NAME, t.TABLE_TYPE, c.COLUMN_NAME, c.COLUMN_TYPE
FROM information_schema.TABLES t
LEFT JOIN information_schema.COLUMNS c
  ON c.TABLE_SCHEMA = t.TABLE_SCHEMA AND c.TABLE_NAME = t.TABLE_NAME
WHERE t.TABLE_SCHEMA = ?
ORDER BY t.TABLE_NAME, c.ORDINAL_POSITION";

pub(crate) async fn catalog(conn: &mut Conn, schema: &str) -> Result<SchemaCatalog, DriverError> {
    let mut relations: Vec<CatalogRelation> = Vec::new();
    for row in rows(conn, CATALOG_SQL, &[schema]).await? {
        let name = string(&row, 0);
        if relations.last().is_none_or(|r| r.name != name) {
            let kind = match string(&row, 1).as_str() {
                "VIEW" | "SYSTEM VIEW" => RelationKind::View,
                _ => RelationKind::Table,
            };
            relations.push(CatalogRelation {
                name,
                kind,
                columns: Vec::new(),
            });
        }
        if let (Some(column), Some(rel)) = (text(&row, 2), relations.last_mut()) {
            rel.columns.push(CatalogColumn {
                name: column,
                type_name: string(&row, 3),
            });
        }
    }
    Ok(SchemaCatalog {
        schema: schema.to_string(),
        relations,
    })
}

const COLUMNS_SQL: &str = "\
SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, EXTRA
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
ORDER BY ORDINAL_POSITION";

/// Unique indexes, primary key first, columns in key order. Functional key parts have no
/// column name.
const KEYS_SQL: &str = "\
SELECT INDEX_NAME, COLUMN_NAME
FROM information_schema.STATISTICS
WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND NON_UNIQUE = 0
ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX";

/// Whether `EXTRA` marks a generated column. MySQL also writes `DEFAULT_GENERATED` for
/// expression defaults, which are ordinary columns.
fn is_generated(extra: &str) -> bool {
    let extra = extra.to_ascii_uppercase();
    [
        "VIRTUAL GENERATED",
        "STORED GENERATED",
        "PERSISTENT GENERATED",
    ]
    .iter()
    .any(|k| extra.contains(k))
}

pub(crate) async fn describe(
    conn: &mut Conn,
    database: &str,
    table: &str,
) -> Result<TableInfo, DriverError> {
    let columns: Vec<TableColumn> = rows(conn, COLUMNS_SQL, &[database, table])
        .await?
        .iter()
        .map(|row| TableColumn {
            automatic: string(row, 4)
                .to_ascii_lowercase()
                .contains("auto_increment"),
            name: string(row, 0),
            type_name: string(row, 1),
            nullable: string(row, 2) == "YES",
            // MariaDB prints a missing default as the bare word NULL.
            default: text(row, 3).filter(|d| d != "NULL"),
            generated: is_generated(&string(row, 4)),
        })
        .collect();
    if columns.is_empty() {
        return Err(DriverError::new(
            ErrorKind::Other,
            format!("table `{database}`.`{table}` does not exist"),
        ));
    }

    let mut keys: Vec<(String, Vec<Option<String>>)> = Vec::new();
    for row in rows(conn, KEYS_SQL, &[database, table]).await? {
        let index = string(&row, 0);
        if keys.last().is_none_or(|(name, _)| *name != index) {
            keys.push((index, Vec::new()));
        }
        if let Some((_, cols)) = keys.last_mut() {
            cols.push(text(&row, 1));
        }
    }
    let mut primary_key = Vec::new();
    let mut unique_keys = Vec::new();
    for (index, cols) in keys {
        // A key with an expression part cannot identify rows by column values.
        let Some(cols) = cols.into_iter().collect::<Option<Vec<String>>>() else {
            continue;
        };
        if index == "PRIMARY" {
            primary_key = cols;
        } else {
            unique_keys.push(cols);
        }
    }
    Ok(TableInfo {
        table: TableRef {
            database: None,
            schema: Some(database.to_string()),
            name: table.to_string(),
        },
        columns,
        primary_key,
        unique_keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_columns_by_extra() {
        assert!(is_generated("VIRTUAL GENERATED"));
        assert!(is_generated("stored generated"));
        assert!(is_generated("PERSISTENT GENERATED"));
        assert!(!is_generated("DEFAULT_GENERATED"));
        assert!(!is_generated(
            "DEFAULT_GENERATED on update CURRENT_TIMESTAMP"
        ));
        assert!(!is_generated("auto_increment"));
    }
}
