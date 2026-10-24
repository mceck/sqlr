//! Table descriptions from `pg_catalog`. All queries are parameterized.

use sqlr_core::{Dialect, DriverError, ErrorKind, TableColumn, TableInfo, TableRef};
use tokio_postgres::Client;

use crate::dialect::PgDialect;
use crate::map_error;

/// A column as `pg_attribute` and `pg_attrdef` describe it.
pub(crate) struct PgColumn {
    pub(crate) name: String,
    /// `format_type`, with the type modifier (`numeric(10,2)`).
    pub(crate) type_name: String,
    pub(crate) not_null: bool,
    /// The default expression, or the generation expression when `generated` is set.
    pub(crate) expression: Option<String>,
    /// `a` (ALWAYS) or `d` (BY DEFAULT) for identity columns, empty otherwise.
    pub(crate) identity: String,
    /// `s` (stored) or `v` (virtual) for generated columns, empty otherwise.
    pub(crate) generated: String,
    pub(crate) comment: Option<String>,
    /// `(schema, name)` of a collation other than the type's.
    pub(crate) collation: Option<(String, String)>,
}

const COLUMNS_SQL: &str = "\
SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull,
       pg_get_expr(d.adbin, d.adrelid), a.attidentity::text, a.attgenerated::text,
       col_description(a.attrelid, a.attnum), cn.nspname::text, co.collname::text
FROM pg_attribute a
JOIN pg_type t ON t.oid = a.atttypid
LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
LEFT JOIN pg_collation co ON co.oid = a.attcollation AND a.attcollation <> t.typcollation
LEFT JOIN pg_namespace cn ON cn.oid = co.collnamespace
WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped
ORDER BY a.attnum";

/// A column from a row of [`COLUMNS_SQL`] (the first nine columns).
pub(crate) fn column_from_row(r: &tokio_postgres::Row) -> PgColumn {
    let schema: Option<String> = r.get(7);
    let name: Option<String> = r.get(8);
    PgColumn {
        name: r.get(0),
        type_name: r.get(1),
        not_null: r.get(2),
        expression: r.get(3),
        identity: r.get(4),
        generated: r.get(5),
        comment: r.get(6),
        collation: schema.zip(name),
    }
}

/// [`COLUMNS_SQL`] for many relations at once: `$1` is an `oid[]` and each row ends with
/// its relation's oid.
pub(crate) fn columns_bulk_sql() -> String {
    COLUMNS_SQL
        .replace("a.attrelid = $1", "a.attrelid = ANY($1)")
        .replace(
            "cn.nspname::text, co.collname::text",
            "cn.nspname::text, co.collname::text, a.attrelid",
        )
        .replace("ORDER BY a.attnum", "ORDER BY a.attrelid, a.attnum")
}

pub(crate) async fn columns(client: &Client, oid: u32) -> Result<Vec<PgColumn>, DriverError> {
    let rows = client
        .query(COLUMNS_SQL, &[&oid])
        .await
        .map_err(map_error)?;
    Ok(rows.iter().map(column_from_row).collect())
}

/// Unique, valid, non-partial indexes on plain columns, primary key first, then by name.
/// Their key columns (not `INCLUDE`d ones) identify a row.
const KEYS_SQL: &str = "\
SELECT i.indisprimary, array_agg(a.attname::text ORDER BY k.ord)
FROM pg_index i
JOIN pg_class ic ON ic.oid = i.indexrelid
CROSS JOIN LATERAL unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord)
JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
WHERE i.indrelid = $1 AND i.indisunique AND i.indisvalid AND i.indpred IS NULL
  AND k.ord <= i.indnkeyatts AND NOT (0 = ANY (i.indkey::int2[]))
GROUP BY i.indexrelid, i.indisprimary, ic.relname
ORDER BY i.indisprimary DESC, ic.relname";

pub(crate) async fn describe(
    client: &Client,
    dialect: &PgDialect,
    table: &TableRef,
) -> Result<TableInfo, DriverError> {
    // `to_regclass` resolves an unqualified name through the search path.
    let name = dialect.qualified(table);
    let row = client
        .query_opt(
            "SELECT c.oid, n.nspname::text, c.relname::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.oid = to_regclass($1)",
            &[&name],
        )
        .await
        .map_err(map_error)?
        .ok_or_else(|| DriverError::new(ErrorKind::Other, format!("table {name} not found")))?;
    let oid: u32 = row.get(0);

    let columns = columns(client, oid)
        .await?
        .into_iter()
        .map(|c| {
            let generated = !c.generated.is_empty();
            TableColumn {
                automatic: !c.identity.is_empty()
                    || c.expression
                        .as_deref()
                        .is_some_and(|d| d.contains("nextval(")),
                name: c.name,
                type_name: c.type_name,
                nullable: !c.not_null,
                default: c.expression.filter(|_| !generated),
                // `GENERATED ALWAYS AS IDENTITY` refuses written values too.
                generated: generated || c.identity == "a",
            }
        })
        .collect();

    let mut primary_key = Vec::new();
    let mut unique_keys = Vec::new();
    for key in client.query(KEYS_SQL, &[&oid]).await.map_err(map_error)? {
        let columns: Vec<String> = key.get(1);
        if key.get(0) {
            primary_key = columns;
        } else {
            unique_keys.push(columns);
        }
    }

    Ok(TableInfo {
        table: TableRef {
            database: None,
            schema: Some(row.get(1)),
            name: row.get(2),
        },
        columns,
        primary_key,
        unique_keys,
    })
}
