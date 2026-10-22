//! A whole schema in nine catalog queries, whatever the number of objects: the relations,
//! then their columns, constraints, indexes (two: tables and materialized views), triggers
//! and partitions, then the schema's sequences and routines. The single-object loaders'
//! row decoders and SQL are reused, so a snapshot reads like [`crate::introspect::load_object`].

use std::collections::HashMap;

use sqlr_core::{
    DriverError, ObjectDef, PartitionDef, SchemaSnapshot, SequenceDef, TableDef, TableRef, ViewDef,
};
use tokio_postgres::Client;

use crate::describe::{PgColumn, column_from_row, columns_bulk_sql};
use crate::introspect::{
    ROUTINES_SQL, TRIGGERS_SQL, column_def, constraint_from_row, constraints_bulk_sql,
    index_from_row, indexes_bulk_sql, routine_from_row, trigger_from_row,
};
use crate::map_error;

fn table_ref(schema: &str, name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some(schema.to_string()),
        name: name.to_string(),
    }
}

struct Relation {
    oid: u32,
    name: String,
    kind: String,
    persistence: String,
    partition_key: Option<String>,
    tablespace: Option<String>,
    reloptions: Option<String>,
    comment: Option<String>,
    view_query: Option<String>,
}

/// Reads the schema in a read-only transaction whose search path is `pg_catalog` alone, so
/// the server prints every name qualified (`public.t`, not `t`) whatever the session's path:
/// two schemas then compare by their contents, not by where the path points.
pub(crate) async fn load_schema(
    client: &Client,
    schema: &str,
) -> Result<SchemaSnapshot, DriverError> {
    client
        .batch_execute("BEGIN READ ONLY; SET LOCAL search_path = pg_catalog")
        .await
        .map_err(map_error)?;
    let read = read_schema(client, schema).await;
    let _ = client.batch_execute("ROLLBACK").await;
    read
}

async fn read_schema(client: &Client, schema: &str) -> Result<SchemaSnapshot, DriverError> {
    // 1. Every table, view and materialized view.
    let relations: Vec<Relation> = client
        .query(
            "SELECT c.oid, c.relname::text, c.relkind::text, c.relpersistence::text, \
                    pg_get_partkeydef(c.oid), ts.spcname::text, \
                    array_to_string(c.reloptions, ', '), obj_description(c.oid, 'pg_class'), \
                    CASE WHEN c.relkind IN ('v', 'm') THEN pg_get_viewdef(c.oid, true) END \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_tablespace ts ON ts.oid = c.reltablespace \
             WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm') \
             ORDER BY c.relname",
            &[&schema],
        )
        .await
        .map_err(map_error)?
        .iter()
        .map(|r| Relation {
            oid: r.get(0),
            name: r.get(1),
            kind: r.get(2),
            persistence: r.get(3),
            partition_key: r.get(4),
            tablespace: r.get(5),
            reloptions: r.get(6),
            comment: r.get(7),
            view_query: r.get(8),
        })
        .collect();
    let all: Vec<u32> = relations.iter().map(|r| r.oid).collect();
    let tables: Vec<u32> = relations
        .iter()
        .filter(|r| r.kind == "r" || r.kind == "p")
        .map(|r| r.oid)
        .collect();
    let matviews: Vec<u32> = relations
        .iter()
        .filter(|r| r.kind == "m")
        .map(|r| r.oid)
        .collect();

    // 2. Their columns.
    let mut columns: HashMap<u32, Vec<PgColumn>> = HashMap::new();
    for r in client
        .query(columns_bulk_sql().as_str(), &[&all])
        .await
        .map_err(map_error)?
    {
        columns
            .entry(r.get::<_, u32>(9))
            .or_default()
            .push(column_from_row(&r));
    }
    // 3. Constraints.
    let mut constraints: HashMap<u32, Vec<_>> = HashMap::new();
    for r in client
        .query(constraints_bulk_sql().as_str(), &[&tables])
        .await
        .map_err(map_error)?
    {
        constraints
            .entry(r.get::<_, u32>(13))
            .or_default()
            .push(constraint_from_row(&r));
    }
    // 4–5. Indexes: a table's leave out the ones a constraint owns, a materialized view's
    // keep them all.
    let mut indexes: HashMap<u32, Vec<_>> = HashMap::new();
    for (oids, with_constraints) in [(&tables, false), (&matviews, true)] {
        for r in client
            .query(
                indexes_bulk_sql().as_str(),
                &[oids, &None::<&str>, &with_constraints],
            )
            .await
            .map_err(map_error)?
        {
            indexes
                .entry(r.get::<_, u32>(10))
                .or_default()
                .push(index_from_row(&r));
        }
    }
    // 6. Triggers (of tables; a view's INSTEAD OF triggers are not part of the view).
    let names: HashMap<u32, &str> = relations.iter().map(|r| (r.oid, r.name.as_str())).collect();
    let mut triggers: HashMap<u32, Vec<_>> = HashMap::new();
    for r in client
        .query(TRIGGERS_SQL, &[&tables])
        .await
        .map_err(map_error)?
    {
        let oid: u32 = r.get(4);
        let table = table_ref(schema, names.get(&oid).copied().unwrap_or(""));
        triggers
            .entry(oid)
            .or_default()
            .push(trigger_from_row(&r, &table));
    }
    // 7. Partitions.
    let mut partitions: HashMap<u32, Vec<PartitionDef>> = HashMap::new();
    for r in client
        .query(
            "SELECT i.inhparent, n.nspname::text, c.relname::text, \
                    pg_get_expr(c.relpartbound, c.oid) \
             FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE i.inhparent = ANY($1) AND c.relispartition ORDER BY n.nspname, c.relname",
            &[&tables],
        )
        .await
        .map_err(map_error)?
    {
        partitions
            .entry(r.get::<_, u32>(0))
            .or_default()
            .push(PartitionDef {
                table: table_ref(r.get(1), r.get(2)),
                bound: r.get(3),
            });
    }

    let mut objects = Vec::new();
    for rel in &relations {
        let me = table_ref(schema, &rel.name);
        let cols = columns.remove(&rel.oid).unwrap_or_default();
        match rel.kind.as_str() {
            "r" | "p" => {
                let mut def = TableDef::new(me);
                def.columns = cols.into_iter().map(column_def).collect();
                def.constraints = constraints.remove(&rel.oid).unwrap_or_default();
                def.indexes = indexes.remove(&rel.oid).unwrap_or_default();
                def.triggers = triggers.remove(&rel.oid).unwrap_or_default();
                def.comment = rel.comment.clone();
                if rel.persistence == "u" {
                    def.options.push(("unlogged".into(), "true".into()));
                }
                if let Some(key) = &rel.partition_key {
                    def.options.push(("partition_by".into(), key.clone()));
                }
                if let Some(with) = &rel.reloptions {
                    def.options.push(("with".into(), with.clone()));
                }
                if let Some(space) = &rel.tablespace {
                    def.options.push(("tablespace".into(), space.clone()));
                }
                def.partitions = partitions.remove(&rel.oid).unwrap_or_default();
                objects.push(ObjectDef::Table(def));
            }
            _ => {
                let materialized = rel.kind == "m";
                let mut options = Vec::new();
                if let Some(with) = &rel.reloptions {
                    options.push(("with".to_string(), with.clone()));
                }
                if let Some(space) = &rel.tablespace {
                    options.push(("tablespace".to_string(), space.clone()));
                }
                objects.push(ObjectDef::View(ViewDef {
                    view: me,
                    query: rel
                        .view_query
                        .clone()
                        .unwrap_or_default()
                        .trim()
                        .trim_end_matches(';')
                        .to_string(),
                    materialized,
                    columns: cols.into_iter().map(|c| c.name).collect(),
                    comment: rel.comment.clone(),
                    options,
                    indexes: indexes.remove(&rel.oid).unwrap_or_default(),
                }));
            }
        }
    }
    // 8. Sequences.
    for r in client
        .query(
            "SELECT c.relname::text, format_type(s.seqtypid, NULL), s.seqincrement, s.seqmin, \
                    s.seqmax, s.seqstart, s.seqcache, s.seqcycle, \
                    obj_description(c.oid, 'pg_class') \
             FROM pg_sequence s JOIN pg_class c ON c.oid = s.seqrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 \
               AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = c.oid \
                   AND d.classid = 'pg_class'::regclass AND d.refclassid = 'pg_class'::regclass \
                   AND d.deptype IN ('a', 'i')) \
             ORDER BY c.relname",
            &[&schema],
        )
        .await
        .map_err(map_error)?
    {
        objects.push(ObjectDef::Sequence(SequenceDef {
            sequence: table_ref(schema, &r.get::<_, String>(0)),
            data_type: Some(r.get(1)),
            increment: Some(r.get(2)),
            min_value: Some(r.get(3)),
            max_value: Some(r.get(4)),
            start: Some(r.get(5)),
            cache: Some(r.get(6)),
            cycle: r.get(7),
            comment: r.get(8),
        }));
    }
    // 9. Routines.
    for r in client
        .query(ROUTINES_SQL, &[&schema])
        .await
        .map_err(map_error)?
    {
        let name: String = r.get(11);
        objects.push(ObjectDef::Routine(routine_from_row(&r, schema, &name)));
    }
    Ok(SchemaSnapshot {
        engine: "postgres".into(),
        schema: schema.to_string(),
        objects,
        dependencies: Vec::new(),
    })
}
