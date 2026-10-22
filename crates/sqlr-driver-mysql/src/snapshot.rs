//! A whole database in ten `information_schema` queries, whatever the number of objects:
//! tables and views, columns, indexes, foreign keys, checks, triggers, view definitions,
//! routines and their parameters. The single-object loaders' row decoders are reused, and
//! their SQL is rewritten to read every table of the schema at once.

use std::collections::HashMap;

use mysql_async::Conn;
use sqlr_core::{
    ConstraintDef, Dialect, DriverError, ErrorKind, ObjectDef, ObjectKind, RoutineDef,
    SchemaSnapshot, TableDef, TriggerDef, ViewDef,
};

use crate::catalog::{rows, string, text};
use crate::dialect::MyDialect;
use crate::introspect::{
    COLUMNS_SQL, FOREIGN_KEYS_SQL, RawIndex, charset_of, column_from_row, index_expression,
    push_fk_row, push_index_row, sort_indexes, table_ref, trigger_from_row,
};

fn columns_sql() -> String {
    COLUMNS_SQL
        .replace(
            "WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?\nORDER BY ORDINAL_POSITION",
            "WHERE TABLE_SCHEMA = ?\nORDER BY TABLE_NAME, ORDINAL_POSITION",
        )
        .replace(
            "GENERATION_EXPRESSION, DATA_TYPE\n",
            "GENERATION_EXPRESSION, DATA_TYPE, TABLE_NAME\n",
        )
}

fn foreign_keys_sql() -> String {
    FOREIGN_KEYS_SQL
        .replace(
            "WHERE rc.CONSTRAINT_SCHEMA = ? AND rc.TABLE_NAME = ?\nORDER BY rc.CONSTRAINT_NAME, kcu.ORDINAL_POSITION",
            "WHERE rc.CONSTRAINT_SCHEMA = ?\nORDER BY rc.TABLE_NAME, rc.CONSTRAINT_NAME, kcu.ORDINAL_POSITION",
        )
        .replace(
            "kcu.REFERENCED_COLUMN_NAME\n",
            "kcu.REFERENCED_COLUMN_NAME, rc.TABLE_NAME\n",
        )
}

pub(crate) async fn load_schema(
    conn: &mut Conn,
    db: &str,
    mariadb: bool,
) -> Result<SchemaSnapshot, DriverError> {
    // 1. Tables and views.
    let relations = rows(
        conn,
        "SELECT TABLE_NAME, TABLE_TYPE, TABLE_COLLATION, TABLE_COMMENT, ENGINE \
         FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
        &[db],
    )
    .await?;
    // 2. Columns of all of them.
    let mut columns: HashMap<String, Vec<mysql_async::Row>> = HashMap::new();
    for r in rows(conn, &columns_sql(), &[db]).await? {
        columns.entry(string(&r, 9)).or_default().push(r);
    }
    // 3. Indexes.
    let sql = format!(
        "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME, SUB_PART, COLLATION, INDEX_TYPE, {}, \
                TABLE_NAME \
         FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? \
         ORDER BY TABLE_NAME, INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX",
        index_expression(mariadb)
    );
    let mut indexes: HashMap<String, Vec<RawIndex>> = HashMap::new();
    for r in rows(conn, &sql, &[db]).await? {
        push_index_row(indexes.entry(string(&r, 7)).or_default(), &r);
    }
    // 4. Foreign keys.
    let mut foreign_keys: HashMap<String, Vec<ConstraintDef>> = HashMap::new();
    for r in rows(conn, &foreign_keys_sql(), &[db]).await? {
        push_fk_row(foreign_keys.entry(string(&r, 7)).or_default(), &r);
    }
    // 5. Checks (MariaDB names them per table, MySQL per database).
    let same_table = if mariadb {
        " AND cc.TABLE_NAME = tc.TABLE_NAME"
    } else {
        ""
    };
    let sql = format!(
        "SELECT tc.CONSTRAINT_NAME, cc.CHECK_CLAUSE, tc.TABLE_NAME \
         FROM information_schema.TABLE_CONSTRAINTS tc \
         JOIN information_schema.CHECK_CONSTRAINTS cc \
           ON cc.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA \
          AND cc.CONSTRAINT_NAME = tc.CONSTRAINT_NAME{same_table} \
         WHERE tc.TABLE_SCHEMA = ? AND tc.CONSTRAINT_TYPE = 'CHECK' \
         ORDER BY tc.TABLE_NAME, tc.CONSTRAINT_NAME"
    );
    let mut checks: HashMap<String, Vec<ConstraintDef>> = HashMap::new();
    for r in rows(conn, &sql, &[db]).await? {
        checks
            .entry(string(&r, 2))
            .or_default()
            .push(ConstraintDef::Check {
                name: Some(string(&r, 0)),
                expression: string(&r, 1),
            });
    }
    // 6. Triggers, in firing order.
    let mut triggers: HashMap<String, Vec<TriggerDef>> = HashMap::new();
    for r in rows(
        conn,
        "SELECT TRIGGER_NAME, EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION, \
                ACTION_STATEMENT \
         FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ? \
         ORDER BY EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION, ACTION_ORDER",
        &[db],
    )
    .await?
    {
        triggers
            .entry(string(&r, 1))
            .or_default()
            .push(trigger_from_row(db, &r));
    }
    // 7. View definitions.
    let mut views: HashMap<String, (String, String, String)> = HashMap::new();
    for r in rows(
        conn,
        "SELECT TABLE_NAME, VIEW_DEFINITION, CHECK_OPTION, SECURITY_TYPE \
         FROM information_schema.VIEWS WHERE TABLE_SCHEMA = ?",
        &[db],
    )
    .await?
    {
        views.insert(string(&r, 0), (string(&r, 1), string(&r, 2), string(&r, 3)));
    }

    let mut objects = Vec::new();
    for rel in &relations {
        let name = string(rel, 0);
        match string(rel, 1).as_str() {
            "BASE TABLE" => {
                let collation = string(rel, 2);
                let mut def = TableDef::new(table_ref(db, &name));
                def.columns = columns
                    .get(&name)
                    .map(|rows| {
                        rows.iter()
                            .map(|r| column_from_row(r, &collation, mariadb))
                            .collect()
                    })
                    .unwrap_or_default();
                sort_indexes(&mut def, indexes.remove(&name).unwrap_or_default());
                def.constraints
                    .extend(foreign_keys.remove(&name).unwrap_or_default());
                def.constraints
                    .extend(checks.remove(&name).unwrap_or_default());
                def.triggers = triggers.remove(&name).unwrap_or_default();
                def.comment = text(rel, 3).filter(|c| !c.is_empty());
                if let Some(engine) = text(rel, 4) {
                    def.options.push(("engine".into(), engine));
                }
                def.options
                    .push(("charset".into(), charset_of(&collation).to_string()));
                def.options.push(("collation".into(), collation));
                objects.push(ObjectDef::Table(def));
            }
            "VIEW" => {
                let (query, check, security) = views.remove(&name).unwrap_or_default();
                if query.is_empty() {
                    return Err(DriverError::new(
                        ErrorKind::Permission,
                        format!("the definition of `{name}` is hidden: missing privileges"),
                    ));
                }
                let mut options = vec![("security".to_string(), security)];
                if check != "NONE" && !check.is_empty() {
                    options.push(("check_option".into(), check));
                }
                objects.push(ObjectDef::View(ViewDef {
                    view: table_ref(db, &name),
                    query,
                    materialized: false,
                    columns: columns
                        .get(&name)
                        .map(|rows| rows.iter().map(|r| string(r, 0)).collect())
                        .unwrap_or_default(),
                    comment: None,
                    options,
                    indexes: Vec::new(),
                }));
            }
            _ => {}
        }
    }

    // 8–9. Routines and their parameters.
    let d = MyDialect::default();
    let mut parameters: HashMap<(String, String), Vec<String>> = HashMap::new();
    for r in rows(
        conn,
        "SELECT SPECIFIC_NAME, ROUTINE_TYPE, PARAMETER_MODE, PARAMETER_NAME, DTD_IDENTIFIER \
         FROM information_schema.PARAMETERS \
         WHERE SPECIFIC_SCHEMA = ? AND ORDINAL_POSITION > 0 \
         ORDER BY SPECIFIC_NAME, ROUTINE_TYPE, ORDINAL_POSITION",
        &[db],
    )
    .await?
    {
        let procedure = string(&r, 1) == "PROCEDURE";
        let param = format!("{} {}", d.quote_ident(&string(&r, 3)), string(&r, 4));
        let param = match text(&r, 2) {
            Some(mode) if procedure => format!("{mode} {param}"),
            _ => param,
        };
        parameters
            .entry((string(&r, 0), string(&r, 1)))
            .or_default()
            .push(param);
    }
    for r in rows(
        conn,
        "SELECT ROUTINE_NAME, ROUTINE_TYPE, DTD_IDENTIFIER, ROUTINE_DEFINITION, \
                IS_DETERMINISTIC, SQL_DATA_ACCESS, SECURITY_TYPE, ROUTINE_COMMENT \
         FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? \
         ORDER BY ROUTINE_NAME, ROUTINE_TYPE",
        &[db],
    )
    .await?
    {
        let name = string(&r, 0);
        let routine_type = string(&r, 1);
        let kind = if routine_type == "PROCEDURE" {
            ObjectKind::Procedure
        } else {
            ObjectKind::Function
        };
        let body = text(&r, 3).ok_or_else(|| {
            DriverError::new(
                ErrorKind::Permission,
                format!("the definition of `{name}` is hidden: missing privileges"),
            )
        })?;
        let mut options = Vec::new();
        if string(&r, 4) == "YES" {
            options.push(("deterministic".to_string(), "true".to_string()));
        }
        options.push(("data_access".into(), string(&r, 5)));
        options.push(("security".into(), string(&r, 6)));
        if let Some(comment) = text(&r, 7).filter(|c| !c.is_empty()) {
            options.push(("comment".into(), comment));
        }
        let arguments = parameters
            .remove(&(name.clone(), routine_type))
            .unwrap_or_default();
        objects.push(ObjectDef::Routine(RoutineDef {
            schema: Some(db.to_string()),
            name,
            kind,
            arguments: arguments.join(", "),
            identity_arguments: None,
            returns: (kind == ObjectKind::Function).then(|| string(&r, 2)),
            language: Some("SQL".into()),
            body,
            options,
        }));
    }
    Ok(SchemaSnapshot {
        engine: "mysql".into(),
        schema: db.to_string(),
        objects,
        dependencies: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_queries_are_derived_from_the_single_object_ones() {
        let columns = columns_sql();
        assert!(
            columns.contains("WHERE TABLE_SCHEMA = ?\nORDER BY TABLE_NAME, ORDINAL_POSITION"),
            "{columns}"
        );
        assert!(columns.contains("DATA_TYPE, TABLE_NAME\nFROM"), "{columns}");
        let fks = foreign_keys_sql();
        assert!(
            fks.contains("WHERE rc.CONSTRAINT_SCHEMA = ?\nORDER BY rc.TABLE_NAME"),
            "{fks}"
        );
        assert!(
            fks.contains("kcu.REFERENCED_COLUMN_NAME, rc.TABLE_NAME\nFROM"),
            "{fks}"
        );
    }
}
