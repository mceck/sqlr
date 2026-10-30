//! Autocomplete's catalog and the table description editing needs.

use hdbconnect_async::Connection as HdbConnection;
use sqlr_driver_sdk::{
    CatalogColumn, CatalogRelation, DriverError, RelationKind, SchemaCatalog, TableColumn,
    TableInfo, TableRef,
};

use crate::browse::rows;

pub(crate) async fn catalog(
    conn: &HdbConnection,
    schema: &str,
) -> Result<SchemaCatalog, DriverError> {
    let mut relations: Vec<CatalogRelation> = Vec::new();
    for (kind, sql) in [
        (
            RelationKind::Table,
            "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE_NAME FROM SYS.TABLE_COLUMNS \
             WHERE SCHEMA_NAME = ? ORDER BY TABLE_NAME, POSITION",
        ),
        (
            RelationKind::View,
            "SELECT VIEW_NAME, COLUMN_NAME, DATA_TYPE_NAME FROM SYS.VIEW_COLUMNS \
             WHERE SCHEMA_NAME = ? ORDER BY VIEW_NAME, POSITION",
        ),
    ] {
        for r in rows(conn, sql, &[schema]).await? {
            let column = CatalogColumn {
                name: r[1].clone(),
                type_name: r[2].clone(),
            };
            match relations.last_mut() {
                Some(last) if last.name == r[0] && last.kind == kind => last.columns.push(column),
                _ => relations.push(CatalogRelation {
                    name: r[0].clone(),
                    kind,
                    columns: vec![column],
                }),
            }
        }
    }
    relations.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(SchemaCatalog {
        schema: schema.to_string(),
        relations,
    })
}

/// Whether a HANA `GENERATION_TYPE` makes a column `(generated, automatic)`.
///
/// `ALWAYS AS` (computed and row-start/end columns) and `ALWAYS AS IDENTITY` refuse
/// written values; `BY DEFAULT AS IDENTITY` accepts them. Both identity kinds supply a
/// value when it is omitted, so both are automatic.
fn generation_flags(generation_type: &str) -> (bool, bool) {
    let generation_type = generation_type.trim();
    (
        generation_type.starts_with("ALWAYS AS"),
        generation_type.ends_with("AS IDENTITY"),
    )
}

pub(crate) async fn describe(
    conn: &HdbConnection,
    table: &TableRef,
) -> Result<TableInfo, DriverError> {
    let schema = match &table.schema {
        Some(s) => s.clone(),
        None => rows(conn, "SELECT CURRENT_SCHEMA FROM DUMMY", &[]).await?[0][0].clone(),
    };
    let mut columns = Vec::new();
    for sql in [
        "SELECT COLUMN_NAME, DATA_TYPE_NAME, IS_NULLABLE, DEFAULT_VALUE, GENERATION_TYPE \
         FROM SYS.TABLE_COLUMNS WHERE SCHEMA_NAME = ? AND TABLE_NAME = ? ORDER BY POSITION",
        "SELECT COLUMN_NAME, DATA_TYPE_NAME, IS_NULLABLE, DEFAULT_VALUE, '' \
         FROM SYS.VIEW_COLUMNS WHERE SCHEMA_NAME = ? AND VIEW_NAME = ? ORDER BY POSITION",
    ] {
        let found = rows(conn, sql, &[&schema, &table.name]).await?;
        if !found.is_empty() {
            columns = found
                .into_iter()
                .map(|r| {
                    let (generated, automatic) = generation_flags(&r[4]);
                    TableColumn {
                        automatic,
                        name: r[0].clone(),
                        type_name: r[1].clone(),
                        nullable: r[2] == "TRUE",
                        default: Some(r[3].clone()).filter(|d| !d.is_empty()),
                        generated,
                    }
                })
                .collect();
            break;
        }
    }
    let keys = rows(
        conn,
        "SELECT CONSTRAINT_NAME, COLUMN_NAME, IS_PRIMARY_KEY, IS_UNIQUE_KEY FROM SYS.CONSTRAINTS \
         WHERE SCHEMA_NAME = ? AND TABLE_NAME = ? AND (IS_PRIMARY_KEY = 'TRUE' OR IS_UNIQUE_KEY = 'TRUE') \
         ORDER BY CONSTRAINT_NAME, POSITION",
        &[&schema, &table.name],
    )
    .await?;
    let mut primary_key = Vec::new();
    let mut unique_keys: Vec<Vec<String>> = Vec::new();
    let mut last_unique = String::new();
    for r in keys {
        if r[2] == "TRUE" {
            primary_key.push(r[1].clone());
        } else {
            if r[0] != last_unique {
                unique_keys.push(Vec::new());
                last_unique = r[0].clone();
            }
            unique_keys
                .last_mut()
                .expect("pushed above")
                .push(r[1].clone());
        }
    }
    Ok(TableInfo {
        table: TableRef {
            database: None,
            schema: Some(schema),
            name: table.name.clone(),
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
    fn identity_columns_are_automatic_and_only_always_generated() {
        // SYS.TABLE_COLUMNS.GENERATION_TYPE: NULL (an empty cell here) for plain columns.
        assert_eq!(generation_flags(""), (false, false));
        assert_eq!(generation_flags("NONE"), (false, false));
        assert_eq!(generation_flags("ALWAYS AS"), (true, false));
        assert_eq!(generation_flags("ALWAYS AS ROW START"), (true, false));
        assert_eq!(generation_flags("ALWAYS AS ROW END"), (true, false));
        // An ALWAYS identity refuses written values; a BY DEFAULT one accepts them but
        // still supplies a value when it is omitted.
        assert_eq!(generation_flags("ALWAYS AS IDENTITY"), (true, true));
        assert_eq!(generation_flags("BY DEFAULT AS IDENTITY"), (false, true));
    }
}
