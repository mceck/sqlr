//! Object definitions from SQL Server's catalog views: a whole schema in six queries,
//! whatever the number of objects. Every query is parameterized.

use std::collections::HashMap;

use sqlr_core::{
    ColumnDef, ConstraintDef, DatabaseDef, DriverError, ErrorKind, FkAction, Identity, IndexColumn,
    IndexDef, ObjectDef, ObjectKind, ObjectRef, SchemaDef, SchemaSnapshot, TableDef, TableRef,
    ViewDef,
};
use tiberius::Row;

use crate::browse::{MsClient, type_name};
use crate::map_error;

const OBJECTS_SQL: &str = "\
SELECT o.name, RTRIM(o.type), m.definition \
FROM sys.objects o \
JOIN sys.schemas s ON s.schema_id = o.schema_id \
LEFT JOIN sys.sql_modules m ON m.object_id = o.object_id \
WHERE s.name = @P1 AND o.type IN ('U', 'V') AND o.is_ms_shipped = 0 \
ORDER BY o.name";

const COLUMNS_SQL: &str = "\
SELECT o.name, c.name, ty.name, c.max_length, c.precision, c.scale, c.is_nullable, \
       c.is_identity, dc.definition, cc.definition, cc.is_persisted, c.collation_name \
FROM sys.columns c \
JOIN sys.objects o ON o.object_id = c.object_id \
JOIN sys.schemas s ON s.schema_id = o.schema_id \
JOIN sys.types ty ON ty.user_type_id = c.user_type_id \
LEFT JOIN sys.default_constraints dc ON dc.object_id = c.default_object_id \
LEFT JOIN sys.computed_columns cc ON cc.object_id = c.object_id AND cc.column_id = c.column_id \
WHERE s.name = @P1 AND o.type IN ('U', 'V') AND o.is_ms_shipped = 0 \
ORDER BY o.name, c.column_id";

const INDEXES_SQL: &str = "\
SELECT o.name, i.name, i.is_unique, i.is_primary_key, i.is_unique_constraint, i.type_desc, \
       i.filter_definition, c.name, ic.is_descending_key, ic.is_included_column \
FROM sys.indexes i \
JOIN sys.objects o ON o.object_id = i.object_id \
JOIN sys.schemas s ON s.schema_id = o.schema_id \
JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
WHERE s.name = @P1 AND o.type = 'U' AND o.is_ms_shipped = 0 \
  AND i.type > 0 AND i.is_hypothetical = 0 \
ORDER BY o.name, i.index_id, ic.is_included_column, ic.key_ordinal, ic.index_column_id";

const FOREIGN_KEYS_SQL: &str = "\
SELECT po.name, fk.name, rs.name, ro.name, pc.name, rc.name, \
       fk.delete_referential_action, fk.update_referential_action \
FROM sys.foreign_keys fk \
JOIN sys.objects po ON po.object_id = fk.parent_object_id \
JOIN sys.schemas ps ON ps.schema_id = po.schema_id \
JOIN sys.objects ro ON ro.object_id = fk.referenced_object_id \
JOIN sys.schemas rs ON rs.schema_id = ro.schema_id \
JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
JOIN sys.columns rc \
  ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
WHERE ps.name = @P1 \
ORDER BY po.name, fk.name, fkc.constraint_column_id";

const CHECKS_SQL: &str = "\
SELECT o.name, k.name, k.definition \
FROM sys.check_constraints k \
JOIN sys.objects o ON o.object_id = k.parent_object_id \
JOIN sys.schemas s ON s.schema_id = o.schema_id \
WHERE s.name = @P1 \
ORDER BY o.name, k.name";

fn text(row: &Row, i: usize) -> String {
    row.get::<&str, _>(i).unwrap_or_default().to_string()
}

fn opt(row: &Row, i: usize) -> Option<String> {
    row.get::<&str, _>(i).map(str::to_string)
}

fn flag(row: &Row, i: usize) -> bool {
    row.get::<bool, _>(i).unwrap_or(false)
}

/// SQL Server's referential action codes: 0 none, 1 cascade, 2 set null, 3 set default.
fn action(code: u8) -> FkAction {
    match code {
        1 => FkAction::Cascade,
        2 => FkAction::SetNull,
        3 => FkAction::SetDefault,
        _ => FkAction::NoAction,
    }
}

async fn query(client: &mut MsClient, sql: &str, schema: &str) -> Result<Vec<Row>, DriverError> {
    client
        .query(sql, &[&schema])
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)
}

fn table_ref(schema: &str, name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some(schema.to_string()),
        name: name.to_string(),
    }
}

pub(crate) async fn load_schema(
    client: &mut MsClient,
    schema: &str,
) -> Result<SchemaSnapshot, DriverError> {
    let objects = query(client, OBJECTS_SQL, schema).await?;
    let mut columns: HashMap<String, Vec<ColumnDef>> = HashMap::new();
    for r in query(client, COLUMNS_SQL, schema).await? {
        let (computed, persisted) = (opt(&r, 9), flag(&r, 10));
        columns.entry(text(&r, 0)).or_default().push(ColumnDef {
            name: text(&r, 1),
            type_name: type_name(
                r.get::<&str, _>(2).unwrap_or("sql_variant"),
                r.get::<i16, _>(3).unwrap_or(0),
                r.get::<u8, _>(4).unwrap_or(0),
                r.get::<u8, _>(5).unwrap_or(0),
            ),
            nullable: flag(&r, 6),
            identity: flag(&r, 7).then_some(Identity::AutoIncrement),
            default: opt(&r, 8),
            generated_stored: computed.is_some() && persisted,
            generated: computed,
            collation: opt(&r, 11),
            ..ColumnDef::default()
        });
    }
    // Indexes arrive one row per column: consecutive rows of one index make one entry.
    let mut constraints: HashMap<String, Vec<ConstraintDef>> = HashMap::new();
    let mut indexes: HashMap<String, Vec<IndexDef>> = HashMap::new();
    let mut last: Option<(String, String)> = None;
    for r in query(client, INDEXES_SQL, schema).await? {
        let (table, name) = (text(&r, 0), text(&r, 1));
        let column = text(&r, 7);
        let included = flag(&r, 9);
        let fresh = last.as_ref() != Some(&(table.clone(), name.clone()));
        let primary = flag(&r, 3);
        let unique_constraint = flag(&r, 4);
        if primary || unique_constraint {
            let list = constraints.entry(table.clone()).or_default();
            if fresh {
                let name = Some(name.clone());
                list.push(if primary {
                    ConstraintDef::PrimaryKey {
                        name,
                        columns: Vec::new(),
                    }
                } else {
                    ConstraintDef::Unique {
                        name,
                        columns: Vec::new(),
                    }
                });
            }
            if let Some(
                ConstraintDef::PrimaryKey { columns, .. } | ConstraintDef::Unique { columns, .. },
            ) = list.last_mut()
            {
                columns.push(column);
            }
        } else {
            let list = indexes.entry(table.clone()).or_default();
            if fresh {
                list.push(IndexDef {
                    name: name.clone(),
                    unique: flag(&r, 2),
                    method: opt(&r, 5),
                    predicate: opt(&r, 6),
                    ..IndexDef::default()
                });
            }
            if let Some(index) = list.last_mut() {
                if included {
                    index.include.push(column);
                } else {
                    index.columns.push(IndexColumn {
                        desc: flag(&r, 8),
                        ..IndexColumn::column(&column)
                    });
                }
            }
        }
        last = Some((table, name));
    }
    let mut foreign_keys: HashMap<String, Vec<ConstraintDef>> = HashMap::new();
    let mut last: Option<(String, String)> = None;
    for r in query(client, FOREIGN_KEYS_SQL, schema).await? {
        let (table, name) = (text(&r, 0), text(&r, 1));
        let list = foreign_keys.entry(table.clone()).or_default();
        if last.as_ref() != Some(&(table.clone(), name.clone())) {
            list.push(ConstraintDef::ForeignKey {
                name: Some(name.clone()),
                columns: Vec::new(),
                target: table_ref(&text(&r, 2), &text(&r, 3)),
                target_columns: Vec::new(),
                on_delete: action(r.get::<u8, _>(6).unwrap_or(0)),
                on_update: action(r.get::<u8, _>(7).unwrap_or(0)),
                deferrable: false,
                initially_deferred: false,
            });
        }
        if let Some(ConstraintDef::ForeignKey {
            columns,
            target_columns,
            ..
        }) = list.last_mut()
        {
            columns.push(text(&r, 4));
            target_columns.push(text(&r, 5));
        }
        last = Some((table, name));
    }
    let mut checks: HashMap<String, Vec<ConstraintDef>> = HashMap::new();
    for r in query(client, CHECKS_SQL, schema).await? {
        checks
            .entry(text(&r, 0))
            .or_default()
            .push(ConstraintDef::Check {
                name: Some(text(&r, 1)),
                expression: text(&r, 2),
            });
    }

    let mut defs = Vec::new();
    for r in &objects {
        let name = text(r, 0);
        let cols = columns.remove(&name).unwrap_or_default();
        if text(r, 1) == "V" {
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
        def.constraints
            .extend(foreign_keys.remove(&name).unwrap_or_default());
        def.constraints
            .extend(checks.remove(&name).unwrap_or_default());
        def.indexes = indexes.remove(&name).unwrap_or_default();
        defs.push(ObjectDef::Table(def));
    }
    Ok(SchemaSnapshot {
        engine: "sqlserver".into(),
        schema: schema.to_string(),
        objects: defs,
        dependencies: Vec::new(),
    })
}

pub(crate) async fn load_object(
    client: &mut MsClient,
    database: &str,
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
            let schema = object.schema.as_deref().unwrap_or("dbo");
            let snapshot = load_schema(client, schema).await?;
            let _ = database;
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

pub(crate) async fn roles(client: &mut MsClient) -> Result<Vec<String>, DriverError> {
    let rows = client
        .query(
            "SELECT name FROM sys.database_principals \
             WHERE type IN ('S', 'U', 'G', 'R') AND is_fixed_role = 0 \
               AND name NOT IN ('dbo', 'guest', 'public') ORDER BY name",
            &[],
        )
        .await
        .map_err(map_error)?
        .into_first_result()
        .await
        .map_err(map_error)?;
    Ok(rows.iter().map(|r| text(r, 0)).collect())
}
