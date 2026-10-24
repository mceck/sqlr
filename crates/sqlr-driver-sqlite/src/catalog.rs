//! Table descriptions, object DDL and the autocomplete catalog.

use rusqlite::{Connection, OptionalExtension};
use sqlr_core::{
    CatalogColumn, CatalogRelation, Dialect, DriverError, ErrorKind, Node, NodeKind, RelationKind,
    SchemaCatalog, TableColumn, TableInfo, TableRef,
};

use crate::browse::FOLDERS;
use crate::dialect::SqliteDialect;
use crate::map_error;

fn schema_of(table: &TableRef) -> &str {
    table.schema.as_deref().unwrap_or("main")
}

pub(crate) fn describe(conn: &Connection, table: &TableRef) -> Result<TableInfo, DriverError> {
    let schema = schema_of(table);
    let mut stmt = conn
        .prepare(
            "SELECT name, type, \"notnull\", dflt_value, pk, hidden \
             FROM pragma_table_xinfo(?1, ?2) WHERE hidden <> 1 ORDER BY cid",
        )
        .map_err(map_error)?;
    let rows: Vec<(String, String, bool, Option<String>, i64, i64)> = stmt
        .query_map([&table.name, schema], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .map_err(map_error)?
        .collect::<Result<_, _>>()
        .map_err(map_error)?;
    if rows.is_empty() {
        return Err(DriverError::new(
            ErrorKind::Other,
            format!("table \"{}\".\"{}\" not found", schema, table.name),
        ));
    }
    let mut pk: Vec<(i64, String)> = rows
        .iter()
        .filter(|r| r.4 > 0)
        .map(|r| (r.4, r.0.clone()))
        .collect();
    pk.sort();
    let indexed_primary_key: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_index_list(?1, ?2) WHERE origin = 'pk')",
            [&table.name, schema],
            |r| r.get(0),
        )
        .map_err(map_error)?;
    let automatic_key = if pk.len() == 1 && !indexed_primary_key {
        Some(pk[0].1.clone())
    } else {
        None
    };
    let columns = rows
        .into_iter()
        .map(
            |(name, type_name, not_null, default, _, hidden)| TableColumn {
                automatic: automatic_key.as_ref() == Some(&name)
                    && type_name.eq_ignore_ascii_case("INTEGER"),
                name,
                type_name,
                nullable: !not_null,
                default,
                generated: hidden == 2 || hidden == 3,
            },
        )
        .collect();

    let mut unique_keys = Vec::new();
    let mut stmt = conn
        .prepare(
            "SELECT name FROM pragma_index_list(?1, ?2) \
             WHERE \"unique\" = 1 AND origin <> 'pk' AND partial = 0 ORDER BY seq",
        )
        .map_err(map_error)?;
    let indexes: Vec<String> = stmt
        .query_map([&table.name, schema], |r| r.get(0))
        .map_err(map_error)?
        .collect::<Result<_, _>>()
        .map_err(map_error)?;
    let mut info = conn
        .prepare("SELECT name FROM pragma_index_info(?1, ?2) ORDER BY seqno")
        .map_err(map_error)?;
    for index in indexes {
        let names: Vec<Option<String>> = info
            .query_map([&index, schema], |r| r.get(0))
            .map_err(map_error)?
            .collect::<Result<_, _>>()
            .map_err(map_error)?;
        // An expression column (name NULL) cannot identify rows from result values.
        if let Some(key) = names.into_iter().collect::<Option<Vec<String>>>() {
            unique_keys.push(key);
        }
    }
    Ok(TableInfo {
        table: TableRef {
            database: None,
            schema: Some(schema.to_string()),
            name: table.name.clone(),
        },
        columns,
        primary_key: pk.into_iter().map(|(_, n)| n).collect(),
        unique_keys,
    })
}

/// The `CREATE` statement of an object; a table brings its indexes and triggers.
pub(crate) fn ddl(conn: &Connection, node: &Node) -> Result<String, DriverError> {
    let unsupported = || {
        DriverError::new(
            ErrorKind::Unsupported,
            format!("no DDL for \"{}\"", node.name),
        )
    };
    let [schema, folder, name] = node.path.segments() else {
        return Err(unsupported());
    };
    let Some((_, ty, _)) = FOLDERS.iter().find(|(f, _, _)| f == folder) else {
        return Err(unsupported());
    };
    let q = SqliteDialect.quote_ident(schema);
    let sql: Option<Option<String>> = conn
        .query_row(
            &format!("SELECT sql FROM {q}.sqlite_schema WHERE type = ?1 AND name = ?2"),
            [ty, name.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_error)?;
    let Some(Some(sql)) = sql else {
        return Err(DriverError::new(
            ErrorKind::Other,
            format!("\"{name}\" not found"),
        ));
    };
    let mut parts = vec![format!("{sql};")];
    if node.kind == NodeKind::Table {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT sql FROM {q}.sqlite_schema WHERE tbl_name = ?1 \
                 AND type IN ('index', 'trigger') AND sql IS NOT NULL ORDER BY type, name"
            ))
            .map_err(map_error)?;
        let extra: Vec<String> = stmt
            .query_map([name], |r| r.get(0))
            .map_err(map_error)?
            .collect::<Result<_, _>>()
            .map_err(map_error)?;
        parts.extend(extra.into_iter().map(|s| format!("{s};")));
    }
    Ok(parts.join("\n\n"))
}

pub(crate) fn catalog(conn: &Connection, schema: &str) -> Result<SchemaCatalog, DriverError> {
    let sql = format!(
        "SELECT m.name, m.type, p.name, p.type FROM {}.sqlite_schema m \
         LEFT JOIN pragma_table_info(m.name, ?1) p \
         WHERE m.type IN ('table', 'view') AND m.name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
         ORDER BY m.name, p.cid",
        SqliteDialect.quote_ident(schema)
    );
    let mut stmt = conn.prepare(&sql).map_err(map_error)?;
    let rows: Vec<(String, String, Option<String>, Option<String>)> = stmt
        .query_map([schema], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(map_error)?
        .collect::<Result<_, _>>()
        .map_err(map_error)?;
    let mut relations: Vec<CatalogRelation> = Vec::new();
    for (name, ty, column, column_type) in rows {
        if relations.last().is_none_or(|r| r.name != name) {
            relations.push(CatalogRelation {
                name,
                kind: if ty == "view" {
                    RelationKind::View
                } else {
                    RelationKind::Table
                },
                columns: Vec::new(),
            });
        }
        if let (Some(name), Some(rel)) = (column, relations.last_mut()) {
            rel.columns.push(CatalogColumn {
                name,
                type_name: column_type.unwrap_or_default(),
            });
        }
    }
    Ok(SchemaCatalog {
        schema: schema.to_string(),
        relations,
    })
}
