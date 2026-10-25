//! Catalog values are always bound; only display SQL uses quoted identifiers.
use crate::map_error;
use duckdb::Connection;
use sqlr_driver_sdk::{
    CatalogColumn, CatalogRelation, DriverError, ErrorKind, Node, NodeKind, NodePath, RelationKind,
    SchemaCatalog, TableColumn, TableInfo, TableRef,
};

fn node(
    parent: &NodePath,
    name: String,
    kind: NodeKind,
    has_children: bool,
    detail: Option<String>,
) -> Node {
    Node {
        path: parent.child(&name),
        name,
        kind,
        has_children,
        detail,
    }
}
pub(crate) fn browse(conn: &Connection, path: &NodePath) -> Result<Vec<Node>, DriverError> {
    let segments: Vec<_> = path.segments().iter().map(String::as_str).collect();
    match segments.as_slice() {
        [] => {
            let mut stmt = conn.prepare("SELECT schema_name FROM information_schema.schemata WHERE catalog_name=current_database() AND schema_name NOT IN ('information_schema','pg_catalog') ORDER BY schema_name").map_err(map_error)?;
            stmt.query_map([], |row| row.get::<_, String>(0))
                .map_err(map_error)?
                .map(|r| {
                    r.map(|name| node(path, name, NodeKind::Schema, true, None))
                        .map_err(map_error)
                })
                .collect()
        }
        [_] => Ok(["tables", "views"]
            .iter()
            .map(|name| node(path, (*name).into(), NodeKind::Folder, true, None))
            .collect()),
        [schema, folder @ ("tables" | "views")] => {
            let cat = catalog(conn, schema)?;
            Ok(cat
                .relations
                .into_iter()
                .filter(|r| (*folder == "views") == (r.kind == RelationKind::View))
                .map(|r| {
                    node(
                        path,
                        r.name,
                        if r.kind == RelationKind::View {
                            NodeKind::View
                        } else {
                            NodeKind::Table
                        },
                        true,
                        None,
                    )
                })
                .collect())
        }
        [schema, "tables" | "views", name] => {
            let table = TableRef {
                database: None,
                schema: Some((*schema).into()),
                name: (*name).into(),
            };
            Ok(describe(conn, &table)?
                .columns
                .into_iter()
                .map(|c| node(path, c.name, NodeKind::Column, false, Some(c.type_name)))
                .collect())
        }
        _ => Err(DriverError::new(
            ErrorKind::Unsupported,
            "this DuckDB explorer path is unsupported",
        )),
    }
}
pub(crate) fn describe(conn: &Connection, table: &TableRef) -> Result<TableInfo, DriverError> {
    let schema = table.schema.as_deref().unwrap_or("main");
    let database = table.database.as_deref();
    let mut stmt = conn.prepare("SELECT column_name, data_type, is_nullable, column_default, is_generated FROM duckdb_columns() WHERE database_name=COALESCE(?,current_database()) AND schema_name=? AND table_name=? ORDER BY column_index").map_err(map_error)?;
    let columns = stmt
        .query_map(duckdb::params![database, schema, &table.name], |row| {
            Ok(TableColumn {
                name: row.get(0)?,
                type_name: row.get(1)?,
                nullable: row.get(2)?,
                default: row.get(3)?,
                generated: row.get(4)?,
                automatic: false,
            })
        })
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;
    if columns.is_empty() {
        return Err(DriverError::new(
            ErrorKind::Other,
            "DuckDB table or view does not exist",
        ));
    }
    let mut stmt = conn.prepare("SELECT constraint_type, constraint_index, col.column_name FROM duckdb_constraints(), UNNEST(constraint_column_names) WITH ORDINALITY AS col(column_name, position) WHERE database_name=COALESCE(?,current_database()) AND schema_name=? AND table_name=? AND constraint_type IN ('PRIMARY KEY','UNIQUE') ORDER BY constraint_index,col.position").map_err(map_error)?;
    let keys = stmt
        .query_map(duckdb::params![database, schema, &table.name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;
    let mut info = TableInfo {
        table: table.clone(),
        columns,
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    };
    let mut previous = None;
    for (kind, index, column) in keys {
        if kind == "PRIMARY KEY" {
            info.primary_key.push(column);
        } else {
            if previous != Some(index) {
                info.unique_keys.push(Vec::new());
                previous = Some(index);
            }
            info.unique_keys
                .last_mut()
                .expect("key group was initialized")
                .push(column);
        }
    }
    Ok(info)
}
pub(crate) fn catalog(conn: &Connection, schema: &str) -> Result<SchemaCatalog, DriverError> {
    let mut stmt = conn.prepare("SELECT t.table_name,t.table_type,c.column_name,c.data_type FROM information_schema.tables t LEFT JOIN information_schema.columns c ON c.table_catalog=t.table_catalog AND c.table_schema=t.table_schema AND c.table_name=t.table_name WHERE t.table_catalog=current_database() AND t.table_schema=? ORDER BY t.table_name,c.ordinal_position").map_err(map_error)?;
    let rows = stmt
        .query_map([schema], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;
    let mut relations: Vec<CatalogRelation> = Vec::new();
    for (name, kind, column, ty) in rows {
        if relations.last().is_none_or(|r| r.name != name) {
            relations.push(CatalogRelation {
                name,
                kind: if kind == "VIEW" {
                    RelationKind::View
                } else {
                    RelationKind::Table
                },
                columns: Vec::new(),
            });
        }
        if let Some(name) = column {
            relations
                .last_mut()
                .expect("relation was initialized")
                .columns
                .push(CatalogColumn {
                    name,
                    type_name: ty.unwrap_or_default(),
                });
        }
    }
    Ok(SchemaCatalog {
        schema: schema.into(),
        relations,
    })
}
