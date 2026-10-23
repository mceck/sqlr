//! Explorer tree backed by SQL Server's catalog views. Every query is parameterized.

use sqlr_core::{DriverError, ErrorKind, Node, NodeKind, NodePath, TableColumn};
use tiberius::Client;
use tokio::net::TcpStream;
use tokio_util::compat::Compat;

use crate::map_error;

pub(crate) type MsClient = Client<Compat<TcpStream>>;

const FOLDERS: [&str; 5] = ["tables", "views", "functions", "procedures", "sequences"];

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

fn unsupported(path: &NodePath) -> DriverError {
    DriverError::new(
        ErrorKind::Unsupported,
        format!("cannot browse {:?}", path.segments()),
    )
}

/// A T-SQL type with the length or precision its column declares.
pub(crate) fn type_name(base: &str, max_length: i16, precision: u8, scale: u8) -> String {
    match base {
        "varchar" | "char" | "varbinary" | "binary" => {
            if max_length < 0 {
                format!("{base}(max)")
            } else {
                format!("{base}({max_length})")
            }
        }
        "nvarchar" | "nchar" => {
            if max_length < 0 {
                format!("{base}(max)")
            } else {
                format!("{base}({})", max_length / 2)
            }
        }
        "decimal" | "numeric" => format!("{base}({precision},{scale})"),
        // The fractional-second digits are part of the type: `datetime2(3)` reads
        // milliseconds, and a rebuild must not silently widen it to the default 7.
        "datetime2" | "datetimeoffset" | "time" if scale != 7 => format!("{base}({scale})"),
        _ => base.to_string(),
    }
}

pub(crate) async fn browse(
    client: &mut MsClient,
    path: &NodePath,
) -> Result<Vec<Node>, DriverError> {
    let segs: Vec<&str> = path.segments().iter().map(String::as_str).collect();
    match segs.as_slice() {
        [] => schemas(client, path).await,
        [_] => Ok(FOLDERS
            .iter()
            .map(|f| node(path, (*f).to_string(), NodeKind::Folder, true, None))
            .collect()),
        [s, "tables"] => tables(client, path, s).await,
        [s, "views"] => views(client, path, s).await,
        [s, "functions"] => routines(client, path, s, &["FN", "IF", "TF", "FS", "FT"], false).await,
        [s, "procedures"] => routines(client, path, s, &["P", "PC"], true).await,
        [s, "sequences"] => sequences(client, path, s).await,
        [_, "tables", _] => Ok(["columns", "indexes", "triggers"]
            .iter()
            .map(|f| node(path, (*f).to_string(), NodeKind::Folder, true, None))
            .collect()),
        [s, "tables", t, "columns"] => columns(client, path, s, t).await,
        [s, "tables", t, "indexes"] => indexes(client, path, s, t).await,
        [s, "tables", t, "triggers"] => triggers(client, path, s, t).await,
        [s, "views", v] => columns(client, path, s, v).await,
        _ => Err(unsupported(path)),
    }
}

async fn schemas(client: &mut MsClient, path: &NodePath) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT s.name FROM sys.schemas s \
             WHERE s.name NOT IN ('sys', 'INFORMATION_SCHEMA', 'guest') \
               AND (s.name = 'dbo' OR EXISTS (SELECT 1 FROM sys.objects o \
                    WHERE o.schema_id = s.schema_id AND o.is_ms_shipped = 0)) \
             ORDER BY CASE s.name WHEN 'dbo' THEN 0 ELSE 1 END, s.name",
            &[],
        )
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            node(
                path,
                r.get::<&str, _>(0).unwrap_or_default().to_string(),
                NodeKind::Schema,
                true,
                None,
            )
        })
        .collect())
}

async fn tables(
    client: &mut MsClient,
    path: &NodePath,
    schema: &str,
) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT t.name, SUM(CAST(p.rows AS bigint)) \
             FROM sys.tables t JOIN sys.schemas s ON s.schema_id = t.schema_id \
             LEFT JOIN sys.partitions p ON p.object_id = t.object_id AND p.index_id IN (0, 1) \
             WHERE s.name = @P1 AND t.is_ms_shipped = 0 \
             GROUP BY t.name ORDER BY t.name",
            &[&schema],
        )
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            let name = r.get::<&str, _>(0).unwrap_or_default().to_string();
            let count = r.get::<i64, _>(1).unwrap_or(0);
            node(
                path,
                name,
                NodeKind::Table,
                true,
                Some(format!("{count} rows")),
            )
        })
        .collect())
}

async fn views(
    client: &mut MsClient,
    path: &NodePath,
    schema: &str,
) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT v.name FROM sys.views v JOIN sys.schemas s ON s.schema_id = v.schema_id \
             WHERE s.name = @P1 AND v.is_ms_shipped = 0 ORDER BY v.name",
            &[&schema],
        )
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            node(
                path,
                r.get::<&str, _>(0).unwrap_or_default().to_string(),
                NodeKind::View,
                true,
                None,
            )
        })
        .collect())
}

async fn routines(
    client: &mut MsClient,
    path: &NodePath,
    schema: &str,
    types: &[&str],
    procedure: bool,
) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT o.name FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
             WHERE s.name = @P1 AND o.is_ms_shipped = 0 \
               AND o.type IN (SELECT value FROM STRING_SPLIT(@P2, ',')) ORDER BY o.name",
            &[&schema, &types.join(",")],
        )
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)?;
    let kind = if procedure {
        NodeKind::Procedure
    } else {
        NodeKind::Function
    };
    Ok(rows
        .iter()
        .map(|r| {
            node(
                path,
                r.get::<&str, _>(0).unwrap_or_default().to_string(),
                kind,
                false,
                None,
            )
        })
        .collect())
}

async fn sequences(
    client: &mut MsClient,
    path: &NodePath,
    schema: &str,
) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT q.name FROM sys.sequences q JOIN sys.schemas s ON s.schema_id = q.schema_id \
             WHERE s.name = @P1 ORDER BY q.name",
            &[&schema],
        )
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            node(
                path,
                r.get::<&str, _>(0).unwrap_or_default().to_string(),
                NodeKind::Sequence,
                false,
                None,
            )
        })
        .collect())
}

const COLUMNS_SQL: &str = "\
SELECT c.name, ty.name, c.max_length, c.precision, c.scale, c.is_nullable, c.is_identity, \
       c.is_computed, OBJECT_DEFINITION(c.default_object_id) \
FROM sys.columns c \
JOIN sys.types ty ON ty.user_type_id = c.user_type_id \
JOIN sys.objects o ON o.object_id = c.object_id \
JOIN sys.schemas s ON s.schema_id = o.schema_id \
WHERE s.name = @P1 AND o.name = @P2 \
ORDER BY c.column_id";

/// Columns of one table or view, for browsing and table descriptions.
pub(crate) async fn column_defs(
    client: &mut MsClient,
    schema: &str,
    name: &str,
) -> Result<Vec<TableColumn>, DriverError> {
    let rows = client
        .query(COLUMNS_SQL, &[&schema, &name])
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            let base = r.get::<&str, _>(1).unwrap_or("sql_variant");
            let generated = r.get::<bool, _>(7).unwrap_or(false);
            TableColumn {
                name: r.get::<&str, _>(0).unwrap_or_default().to_string(),
                type_name: type_name(
                    base,
                    r.get::<i16, _>(2).unwrap_or(0),
                    r.get::<u8, _>(3).unwrap_or(0),
                    r.get::<u8, _>(4).unwrap_or(0),
                ),
                nullable: r.get::<bool, _>(5).unwrap_or(false),
                default: r.get::<&str, _>(8).map(String::from),
                // Identity columns refuse written values too.
                generated: generated || r.get::<bool, _>(6).unwrap_or(false),
            }
        })
        .collect())
}

async fn columns(
    client: &mut MsClient,
    path: &NodePath,
    schema: &str,
    relation: &str,
) -> Result<Vec<Node>, DriverError> {
    Ok(column_defs(client, schema, relation)
        .await?
        .into_iter()
        .map(|c| {
            let detail = if c.nullable {
                c.type_name
            } else {
                format!("{} NOT NULL", c.type_name)
            };
            node(path, c.name, NodeKind::Column, false, Some(detail))
        })
        .collect())
}

async fn indexes(
    client: &mut MsClient,
    path: &NodePath,
    schema: &str,
    table: &str,
) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT i.name, i.type_desc, i.is_unique, i.is_primary_key \
             FROM sys.indexes i JOIN sys.objects o ON o.object_id = i.object_id \
             JOIN sys.schemas s ON s.schema_id = o.schema_id \
             WHERE s.name = @P1 AND o.name = @P2 AND i.name IS NOT NULL \
             ORDER BY i.is_primary_key DESC, i.name",
            &[&schema, &table],
        )
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            let name = r.get::<&str, _>(0).unwrap_or_default().to_string();
            let kind = r.get::<&str, _>(1).unwrap_or_default();
            let unique = if r.get::<bool, _>(2).unwrap_or(false) {
                " UNIQUE"
            } else {
                ""
            };
            let primary = if r.get::<bool, _>(3).unwrap_or(false) {
                " PRIMARY KEY"
            } else {
                ""
            };
            node(
                path,
                name,
                NodeKind::Index,
                false,
                Some(format!("{kind}{unique}{primary}")),
            )
        })
        .collect())
}

async fn triggers(
    client: &mut MsClient,
    path: &NodePath,
    schema: &str,
    table: &str,
) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT tr.name, tr.is_disabled FROM sys.triggers tr \
             JOIN sys.objects o ON o.object_id = tr.parent_id \
             JOIN sys.schemas s ON s.schema_id = o.schema_id \
             WHERE s.name = @P1 AND o.name = @P2 ORDER BY tr.name",
            &[&schema, &table],
        )
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            let detail = if r.get::<bool, _>(1).unwrap_or(false) {
                Some("disabled".to_string())
            } else {
                None
            };
            node(
                path,
                r.get::<&str, _>(0).unwrap_or_default().to_string(),
                NodeKind::Trigger,
                false,
                detail,
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::type_name;

    #[test]
    fn types_keep_the_lengths_and_scales_that_change_them() {
        assert_eq!(type_name("varchar", 20, 0, 0), "varchar(20)");
        assert_eq!(type_name("varchar", -1, 0, 0), "varchar(max)");
        assert_eq!(type_name("nvarchar", 240, 0, 0), "nvarchar(120)");
        assert_eq!(type_name("decimal", 9, 12, 3), "decimal(12,3)");
        // The default scale needs no argument; any other one is part of the type.
        assert_eq!(type_name("datetime2", 8, 27, 7), "datetime2");
        assert_eq!(type_name("datetime2", 8, 27, 3), "datetime2(3)");
        assert_eq!(type_name("datetimeoffset", 10, 34, 2), "datetimeoffset(2)");
        assert_eq!(type_name("time", 5, 16, 0), "time(0)");
        assert_eq!(type_name("int", 4, 10, 0), "int");
    }
}
