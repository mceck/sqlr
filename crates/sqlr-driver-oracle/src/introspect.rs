//! Object definitions from Oracle's `ALL_*` dictionary views: a whole schema in five
//! queries, whatever the number of objects. Every query is parameterized.

use std::collections::HashMap;

use oracle_rs::{Connection, Row};
use sqlr_core::{
    ColumnDef, ConstraintDef, DatabaseDef, DriverError, ErrorKind, FkAction, Identity, IndexColumn,
    IndexDef, ObjectDef, ObjectKind, ObjectRef, SchemaDef, SchemaSnapshot, TableDef, TableRef,
    ViewDef,
};

use crate::browse::{int, query, type_name};

const OBJECTS_SQL: &str = "\
SELECT o.object_name, o.object_type, v.text_vc \
FROM all_objects o \
LEFT JOIN all_views v ON v.owner = o.owner AND v.view_name = o.object_name \
WHERE o.owner = :1 AND o.object_type IN ('TABLE', 'VIEW') \
  AND NOT EXISTS (SELECT 1 FROM all_mviews m WHERE m.owner = o.owner AND m.mview_name = o.object_name) \
ORDER BY o.object_name";

/// Whether the dictionary has `ALL_TAB_COLS.DATA_DEFAULT_VC` (the default as text: the
/// older `DATA_DEFAULT` is a LONG, which comes back empty).
const HAS_DEFAULT_VC_SQL: &str = "\
SELECT COUNT(*) FROM all_tab_cols \
WHERE owner = 'SYS' AND table_name = 'ALL_TAB_COLS' AND column_name = 'DATA_DEFAULT_VC'";

fn columns_sql(default_vc: bool) -> String {
    format!(
        "SELECT c.table_name, c.column_name, c.data_type, c.char_length, c.data_length, \
                c.data_precision, c.data_scale, c.nullable, c.identity_column, c.virtual_column, \
                {}, \
                (SELECT i.generation_type FROM all_tab_identity_cols i \
                 WHERE i.owner = c.owner AND i.table_name = c.table_name \
                   AND i.column_name = c.column_name) \
         FROM all_tab_cols c \
         WHERE c.owner = :1 AND c.hidden_column = 'NO' \
         ORDER BY c.table_name, c.column_id",
        if default_vc {
            "c.data_default_vc"
        } else {
            "CAST(NULL AS VARCHAR2(1))"
        }
    )
}

const CONSTRAINTS_SQL: &str = "\
SELECT c.table_name, c.constraint_name, c.constraint_type, cc.column_name, \
       rc.owner, rc.table_name, rcc.column_name, c.delete_rule, c.search_condition_vc \
FROM all_constraints c \
LEFT JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name \
LEFT JOIN all_constraints rc ON rc.owner = c.r_owner AND rc.constraint_name = c.r_constraint_name \
LEFT JOIN all_cons_columns rcc ON rcc.owner = rc.owner \
      AND rcc.constraint_name = rc.constraint_name AND rcc.position = cc.position \
WHERE c.owner = :1 AND c.constraint_type IN ('P', 'U', 'R', 'C') \
ORDER BY c.table_name, c.constraint_name, cc.position";

/// A function-based index (a descending column makes one) indexes a hidden virtual column
/// named `SYS_NC…$`, whose default is the expression.
fn indexes_sql(default_vc: bool) -> String {
    let (select, join) = if default_vc {
        (
            "hc.data_default_vc",
            "LEFT JOIN all_tab_cols hc ON hc.owner = i.table_owner \
              AND hc.table_name = i.table_name AND hc.column_name = ic.column_name \
              AND hc.hidden_column = 'YES' AND hc.virtual_column = 'YES'",
        )
    } else {
        ("CAST(NULL AS VARCHAR2(1))", "")
    };
    format!(
        "SELECT i.table_name, i.index_name, i.uniqueness, i.index_type, ic.column_name, \
                ic.descend, {select} \
         FROM all_indexes i \
         JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name \
         {join} \
         WHERE i.owner = :1 AND i.table_owner = i.owner AND i.index_type NOT IN ('LOB', 'IOT - TOP') \
           AND NOT EXISTS (SELECT 1 FROM all_constraints k WHERE k.owner = i.owner \
                           AND k.index_name = i.index_name AND k.constraint_type IN ('P', 'U')) \
         ORDER BY i.table_name, i.index_name, ic.column_position"
    )
}

/// The key part of an index: a plain column name, or the expression in parentheses.
fn index_column(name: &str, expression: Option<&str>) -> IndexColumn {
    let Some(expression) = expression.map(str::trim).filter(|e| !e.is_empty()) else {
        return IndexColumn::column(name);
    };
    // `"TOTAL"` is the column itself.
    match expression
        .strip_prefix('"')
        .and_then(|e| e.strip_suffix('"'))
        .filter(|e| !e.contains('"'))
    {
        Some(column) => IndexColumn::column(column),
        None => IndexColumn {
            expr: format!("({expression})"),
            ..IndexColumn::default()
        },
    }
}

/// An index's type as the generator names it: a plain B-tree (even a function-based one,
/// as a descending column makes it) has none.
fn index_method(index_type: &str) -> Option<String> {
    match index_type
        .strip_prefix("FUNCTION-BASED ")
        .unwrap_or(index_type)
    {
        "NORMAL" | "" => None,
        other => Some(other.to_string()),
    }
}

fn text(row: &Row, i: usize) -> String {
    row.get(i)
        .and_then(oracle_rs::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn opt(row: &Row, i: usize) -> Option<String> {
    row.get(i)
        .and_then(oracle_rs::Value::as_str)
        .map(str::to_string)
}

/// Oracle's `DELETE_RULE`: `CASCADE`, `SET NULL` or `NO ACTION`.
fn action(rule: &str) -> FkAction {
    FkAction::parse(rule).unwrap_or_default()
}

fn table_ref(schema: &str, name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some(schema.to_string()),
        name: name.to_string(),
    }
}

pub(crate) async fn load_schema(
    conn: &Connection,
    schema: &str,
) -> Result<SchemaSnapshot, DriverError> {
    let objects = query(conn, OBJECTS_SQL, &[schema]).await?;
    let has_default_vc = query(conn, HAS_DEFAULT_VC_SQL, &[])
        .await?
        .rows
        .first()
        .and_then(|r| int(r, 0))
        == Some(1);
    let mut columns: HashMap<String, Vec<ColumnDef>> = HashMap::new();
    for r in &query(conn, &columns_sql(has_default_vc), &[schema])
        .await?
        .rows
    {
        // `DEFAULT NULL` is no default.
        let expression = opt(r, 10)
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty() && !e.eq_ignore_ascii_case("NULL"));
        let identity = match opt(r, 11).as_deref() {
            Some("ALWAYS") => Some(Identity::Always),
            Some(_) => Some(Identity::ByDefault),
            None => None,
        };
        let virtual_column = text(r, 9) == "YES";
        columns.entry(text(r, 0)).or_default().push(ColumnDef {
            name: text(r, 1),
            type_name: type_name(&text(r, 2), int(r, 3), int(r, 4), int(r, 5), int(r, 6)),
            nullable: text(r, 7) == "Y",
            // An identity's default is its sequence.
            default: if identity.is_some() || virtual_column {
                None
            } else {
                expression.clone()
            },
            generated: if virtual_column { expression } else { None },
            identity,
            ..ColumnDef::default()
        });
    }
    // Constraints and indexes arrive one row per column: consecutive rows of one
    // constraint (or index) make one entry.
    let mut constraints: HashMap<String, Vec<ConstraintDef>> = HashMap::new();
    let mut last: Option<(String, String)> = None;
    for r in &query(conn, CONSTRAINTS_SQL, &[schema]).await?.rows {
        let (table, name, kind) = (text(r, 0), text(r, 1), text(r, 2));
        let list = constraints.entry(table.clone()).or_default();
        let fresh = last.as_ref() != Some(&(table.clone(), name.clone()));
        if fresh {
            let named = Some(name.clone());
            match kind.as_str() {
                "P" => list.push(ConstraintDef::PrimaryKey {
                    name: named,
                    columns: Vec::new(),
                }),
                "U" => list.push(ConstraintDef::Unique {
                    name: named,
                    columns: Vec::new(),
                }),
                "R" => list.push(ConstraintDef::ForeignKey {
                    name: named,
                    columns: Vec::new(),
                    target: table_ref(&text(r, 4), &text(r, 5)),
                    target_columns: Vec::new(),
                    on_delete: action(&text(r, 7)),
                    on_update: FkAction::NoAction,
                    deferrable: false,
                    initially_deferred: false,
                }),
                _ => {
                    let expression = text(r, 8);
                    // The NOT NULL of a column is the column's own `nullable`.
                    if !expression.to_ascii_uppercase().ends_with("IS NOT NULL") {
                        list.push(ConstraintDef::Check {
                            name: named,
                            expression,
                        });
                    }
                }
            }
        }
        match list.last_mut() {
            Some(
                ConstraintDef::PrimaryKey { columns, .. } | ConstraintDef::Unique { columns, .. },
            ) if kind == "P" || kind == "U" => columns.push(text(r, 3)),
            Some(ConstraintDef::ForeignKey {
                columns,
                target_columns,
                ..
            }) if kind == "R" => {
                columns.push(text(r, 3));
                target_columns.push(text(r, 6));
            }
            _ => {}
        }
        last = Some((table, name));
    }
    let mut indexes: HashMap<String, Vec<IndexDef>> = HashMap::new();
    let mut last: Option<(String, String)> = None;
    for r in &query(conn, &indexes_sql(has_default_vc), &[schema])
        .await?
        .rows
    {
        let (table, name) = (text(r, 0), text(r, 1));
        let list = indexes.entry(table.clone()).or_default();
        if last.as_ref() != Some(&(table.clone(), name.clone())) {
            list.push(IndexDef {
                name: name.clone(),
                unique: text(r, 2) == "UNIQUE",
                method: index_method(&text(r, 3)),
                ..IndexDef::default()
            });
        }
        if let Some(index) = list.last_mut() {
            index.columns.push(IndexColumn {
                desc: text(r, 5) == "DESC",
                ..index_column(&text(r, 4), opt(r, 6).as_deref())
            });
        }
        last = Some((table, name));
    }

    let mut defs = Vec::new();
    for r in &objects.rows {
        let name = text(r, 0);
        let cols = columns.remove(&name).unwrap_or_default();
        if text(r, 1) == "VIEW" {
            defs.push(ObjectDef::View(ViewDef {
                view: table_ref(schema, &name),
                query: text(r, 2),
                materialized: false,
                columns: cols.into_iter().map(|c| c.name).collect(),
                comment: None,
                options: Vec::new(),
                indexes: Vec::new(),
            }));
            continue;
        }
        let mut def = TableDef::new(table_ref(schema, &name));
        def.columns = cols;
        def.constraints = constraints.remove(&name).unwrap_or_default();
        def.indexes = indexes.remove(&name).unwrap_or_default();
        defs.push(ObjectDef::Table(def));
    }
    Ok(SchemaSnapshot {
        engine: "oracle".into(),
        schema: schema.to_string(),
        objects: defs,
        dependencies: Vec::new(),
    })
}

pub(crate) async fn load_object(
    conn: &Connection,
    object: &ObjectRef,
) -> Result<ObjectDef, DriverError> {
    let missing = |what: &str| {
        DriverError::new(
            ErrorKind::Other,
            format!("{what} `{}` not found", object.name),
        )
    };
    match object.kind {
        ObjectKind::Database => Ok(ObjectDef::Database(DatabaseDef {
            name: object.name.clone(),
            options: Vec::new(),
        })),
        ObjectKind::Schema => Ok(ObjectDef::Schema(SchemaDef {
            name: object.name.clone(),
            owner: None,
            comment: None,
        })),
        ObjectKind::Table | ObjectKind::View | ObjectKind::Index => {
            let schema = object
                .schema
                .as_deref()
                .ok_or_else(|| DriverError::new(ErrorKind::Other, "no schema given"))?;
            let snapshot = load_schema(conn, schema).await?;
            let wanted = object.parent.as_deref().unwrap_or(&object.name);
            let found = snapshot.objects.into_iter().find(|o| match o {
                ObjectDef::Table(t) => t.table.name == wanted,
                ObjectDef::View(v) => v.view.name == wanted,
                _ => false,
            });
            match (object.kind, found) {
                (ObjectKind::Table, Some(def @ ObjectDef::Table(_)))
                | (ObjectKind::View, Some(def @ ObjectDef::View(_))) => Ok(def),
                (ObjectKind::Index, Some(ObjectDef::Table(t))) => t
                    .indexes
                    .into_iter()
                    .find(|i| i.name == object.name)
                    .map(|index| ObjectDef::Index {
                        table: t.table,
                        index,
                    })
                    .ok_or_else(|| missing("index")),
                _ => Err(missing(&object.kind.keyword().to_lowercase())),
            }
        }
        other => Err(DriverError::new(
            ErrorKind::Unsupported,
            format!(
                "{} objects are not supported",
                other.keyword().to_lowercase()
            ),
        )),
    }
}

pub(crate) async fn roles(conn: &Connection) -> Result<Vec<String>, DriverError> {
    let result = query(
        conn,
        "SELECT username FROM all_users WHERE oracle_maintained = 'N' ORDER BY username",
        &[],
    )
    .await?;
    Ok(result.rows.iter().map(|r| text(r, 0)).collect())
}
