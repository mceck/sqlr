//! Object definitions from `information_schema` (always parameterized). MySQL and MariaDB
//! differ in how they print defaults and which columns some views have; `mariadb` picks.

use mysql_async::{Conn, Row};
use sqlr_core::{
    ColumnDef, ConstraintDef, DatabaseDef, Dialect, DriverError, ErrorKind, FkAction, Identity,
    IndexColumn, IndexDef, ObjectDef, ObjectKind, ObjectRef, RoutineDef, TableDef, TableRef,
    TriggerDef, ViewDef,
};

use crate::catalog::{int, rows, string, text};
use crate::dialect::MyDialect;

fn not_found(what: &str, name: &str) -> DriverError {
    DriverError::new(ErrorKind::Other, format!("{what} `{name}` not found"))
}

pub(crate) fn table_ref(db: &str, name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some(db.to_string()),
        name: name.to_string(),
    }
}

/// `object`'s database: its schema, else the connection's `current` database.
fn database_of<'a>(
    object: &'a ObjectRef,
    current: Option<&'a str>,
) -> Result<&'a str, DriverError> {
    object
        .schema
        .as_deref()
        .or(object.database.as_deref())
        .or(current)
        .ok_or_else(|| DriverError::new(ErrorKind::Other, "no database selected"))
}

pub(crate) async fn load_object(
    conn: &mut Conn,
    object: &ObjectRef,
    current: Option<&str>,
    mariadb: bool,
) -> Result<ObjectDef, DriverError> {
    let name = object.name.as_str();
    if object.kind == ObjectKind::Database || object.kind == ObjectKind::Schema {
        return database(conn, name).await;
    }
    let db = database_of(object, current)?;
    match object.kind {
        ObjectKind::Table => Ok(ObjectDef::Table(table(conn, db, name, mariadb).await?)),
        ObjectKind::View => view(conn, db, name).await,
        ObjectKind::Index => {
            let parent = object.parent.as_deref().ok_or_else(|| {
                DriverError::new(ErrorKind::Other, "an index is named with its table")
            })?;
            let index = raw_indexes(conn, db, parent, mariadb)
                .await?
                .into_iter()
                .find(|i| i.def.name == name)
                .ok_or_else(|| not_found("index", name))?;
            Ok(ObjectDef::Index {
                table: table_ref(db, parent),
                index: index.def,
            })
        }
        ObjectKind::Function | ObjectKind::Procedure => routine(conn, db, name, object.kind).await,
        ObjectKind::Trigger => triggers(conn, db, None, Some(name))
            .await?
            .into_iter()
            .next()
            .map(ObjectDef::Trigger)
            .ok_or_else(|| not_found("trigger", name)),
        other => Err(DriverError::new(
            ErrorKind::Unsupported,
            format!(
                "{} objects are not supported",
                other.keyword().to_lowercase()
            ),
        )),
    }
}

pub(crate) async fn users(conn: &mut Conn) -> Result<Vec<String>, DriverError> {
    Ok(rows(
        conn,
        "SELECT DISTINCT GRANTEE FROM information_schema.USER_PRIVILEGES ORDER BY GRANTEE",
        &[],
    )
    .await?
    .iter()
    .map(|r| string(r, 0))
    .collect())
}

async fn database(conn: &mut Conn, name: &str) -> Result<ObjectDef, DriverError> {
    let row = rows(
        conn,
        "SELECT DEFAULT_CHARACTER_SET_NAME, DEFAULT_COLLATION_NAME \
         FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?",
        &[name],
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| not_found("database", name))?;
    Ok(ObjectDef::Database(DatabaseDef {
        name: name.to_string(),
        options: vec![
            ("charset".into(), string(&row, 0)),
            ("collation".into(), string(&row, 1)),
        ],
    }))
}

/// The character set a collation belongs to: its name up to the first `_`.
pub(crate) fn charset_of(collation: &str) -> &str {
    collation.split('_').next().unwrap_or(collation)
}

const NUMERIC_TYPES: [&str; 10] = [
    "tinyint",
    "smallint",
    "mediumint",
    "int",
    "bigint",
    "decimal",
    "float",
    "double",
    "year",
    "bit",
];

/// A column default as an SQL expression. MariaDB prints defaults as SQL already (with
/// `NULL` for none); MySQL prints literals bare and marks expressions `DEFAULT_GENERATED`.
fn default_expression(
    raw: Option<String>,
    extra: &str,
    data_type: &str,
    mariadb: bool,
) -> Option<String> {
    let raw = raw?;
    if mariadb {
        return (raw != "NULL").then_some(raw);
    }
    if extra.to_ascii_uppercase().contains("DEFAULT_GENERATED") {
        let upper = raw.to_ascii_uppercase();
        return Some(
            if upper.starts_with("CURRENT_TIMESTAMP") || upper.starts_with("NOW(") {
                raw
            } else {
                format!("({raw})")
            },
        );
    }
    if (data_type == "bit" && raw.starts_with("b'"))
        || NUMERIC_TYPES.contains(&data_type) && raw.parse::<f64>().is_ok()
    {
        return Some(raw);
    }
    Some(MyDialect::default().string_literal(&raw))
}

/// `on update CURRENT_TIMESTAMP` in `EXTRA`, as SQL.
fn on_update(extra: &str) -> Option<&str> {
    let at = extra.to_ascii_lowercase().find("on update ")?;
    Some(extra[at + "on update ".len()..].trim())
}

pub(crate) const COLUMNS_SQL: &str = "\
SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, EXTRA, COLUMN_COMMENT,
       COLLATION_NAME, GENERATION_EXPRESSION, DATA_TYPE
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
ORDER BY ORDINAL_POSITION";

async fn columns(
    conn: &mut Conn,
    db: &str,
    name: &str,
    table_collation: &str,
    mariadb: bool,
) -> Result<Vec<ColumnDef>, DriverError> {
    Ok(rows(conn, COLUMNS_SQL, &[db, name])
        .await?
        .iter()
        .map(|r| column_from_row(r, table_collation, mariadb))
        .collect())
}

/// A column from a row of [`COLUMNS_SQL`] (its first nine columns).
pub(crate) fn column_from_row(r: &Row, table_collation: &str, mariadb: bool) -> ColumnDef {
    let extra = string(r, 4);
    let upper = extra.to_ascii_uppercase();
    let generated = upper.contains("VIRTUAL GENERATED")
        || upper.contains("STORED GENERATED")
        || upper.contains("PERSISTENT GENERATED");
    let default = if generated {
        None
    } else {
        default_expression(text(r, 3), &extra, &string(r, 8), mariadb)
    };
    ColumnDef {
        name: string(r, 0),
        type_name: string(r, 1),
        nullable: string(r, 2) == "YES",
        default,
        generated: generated.then(|| string(r, 7)),
        generated_stored: upper.contains("STORED GENERATED")
            || upper.contains("PERSISTENT GENERATED"),
        identity: upper
            .contains("AUTO_INCREMENT")
            .then_some(Identity::AutoIncrement),
        comment: text(r, 5).filter(|c| !c.is_empty()),
        collation: text(r, 6).filter(|c| c != table_collation),
        on_update: on_update(&extra).map(String::from),
    }
}

/// An index as `STATISTICS` lists it, with what decides whether it reads as a unique
/// constraint.
pub(crate) struct RawIndex {
    pub(crate) def: IndexDef,
    /// Unique, B-tree, plain ascending columns without prefixes.
    pub(crate) plain_unique: bool,
}

async fn raw_indexes(
    conn: &mut Conn,
    db: &str,
    table: &str,
    mariadb: bool,
) -> Result<Vec<RawIndex>, DriverError> {
    let sql = format!(
        "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME, SUB_PART, COLLATION, INDEX_TYPE, \
                {} \
         FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
         ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX",
        index_expression(mariadb)
    );
    let mut out: Vec<RawIndex> = Vec::new();
    for r in rows(conn, &sql, &[db, table]).await? {
        push_index_row(&mut out, &r);
    }
    Ok(out)
}

/// The `STATISTICS` column holding an index part's expression (MariaDB has none).
pub(crate) fn index_expression(mariadb: bool) -> &'static str {
    if mariadb { "NULL" } else { "EXPRESSION" }
}

/// Adds a row of the `STATISTICS` query (its first seven columns, ordered by index and
/// position) to the indexes read so far.
pub(crate) fn push_index_row(out: &mut Vec<RawIndex>, r: &Row) {
    let name = string(r, 0);
    let index_type = string(r, 5);
    if out.last().is_none_or(|i| i.def.name != name) {
        let unique = int(r, 1) == Some(0);
        out.push(RawIndex {
            def: IndexDef {
                name,
                unique,
                method: (index_type != "BTREE").then(|| index_type.clone()),
                ..Default::default()
            },
            plain_unique: unique && index_type == "BTREE",
        });
    }
    let column = match text(r, 2) {
        Some(column) => IndexColumn {
            expr: column,
            expression: false,
            desc: string(r, 4) == "D",
            prefix: int(r, 3).and_then(|n| u32::try_from(n).ok()),
        },
        None => IndexColumn {
            expr: format!("({})", string(r, 6)),
            expression: true,
            desc: string(r, 4) == "D",
            prefix: None,
        },
    };
    let Some(index) = out.last_mut() else {
        return;
    };
    if column.desc || column.prefix.is_some() || column.is_expression() {
        index.plain_unique = false;
    }
    index.def.columns.push(column);
}

fn fk_action(rule: &str) -> FkAction {
    FkAction::parse(rule).unwrap_or_default()
}

pub(crate) const FOREIGN_KEYS_SQL: &str = "\
SELECT rc.CONSTRAINT_NAME, rc.UPDATE_RULE, rc.DELETE_RULE, kcu.COLUMN_NAME,
       kcu.REFERENCED_TABLE_SCHEMA, kcu.REFERENCED_TABLE_NAME, kcu.REFERENCED_COLUMN_NAME
FROM information_schema.REFERENTIAL_CONSTRAINTS rc
JOIN information_schema.KEY_COLUMN_USAGE kcu
  ON kcu.CONSTRAINT_SCHEMA = rc.CONSTRAINT_SCHEMA AND kcu.CONSTRAINT_NAME = rc.CONSTRAINT_NAME
 AND kcu.TABLE_NAME = rc.TABLE_NAME
WHERE rc.CONSTRAINT_SCHEMA = ? AND rc.TABLE_NAME = ?
ORDER BY rc.CONSTRAINT_NAME, kcu.ORDINAL_POSITION";

async fn foreign_keys(
    conn: &mut Conn,
    db: &str,
    table: &str,
) -> Result<Vec<ConstraintDef>, DriverError> {
    let mut out: Vec<ConstraintDef> = Vec::new();
    for r in rows(conn, FOREIGN_KEYS_SQL, &[db, table]).await? {
        push_fk_row(&mut out, &r);
    }
    Ok(out)
}

/// Adds a row of [`FOREIGN_KEYS_SQL`] (its first seven columns, ordered by constraint and
/// position) to the foreign keys read so far.
pub(crate) fn push_fk_row(out: &mut Vec<ConstraintDef>, r: &Row) {
    let name = string(r, 0);
    let same =
        matches!(out.last(), Some(ConstraintDef::ForeignKey { name: Some(n), .. }) if *n == name);
    if !same {
        out.push(ConstraintDef::ForeignKey {
            name: Some(name),
            columns: Vec::new(),
            target: table_ref(&string(r, 4), &string(r, 5)),
            target_columns: Vec::new(),
            on_update: fk_action(&string(r, 1)),
            on_delete: fk_action(&string(r, 2)),
            deferrable: false,
            initially_deferred: false,
        });
    }
    if let Some(ConstraintDef::ForeignKey {
        columns,
        target_columns,
        ..
    }) = out.last_mut()
    {
        columns.push(string(r, 3));
        target_columns.push(string(r, 6));
    }
}

async fn checks(
    conn: &mut Conn,
    db: &str,
    table: &str,
    mariadb: bool,
) -> Result<Vec<ConstraintDef>, DriverError> {
    // MariaDB names checks per table, MySQL per database.
    let same_table = if mariadb {
        " AND cc.TABLE_NAME = tc.TABLE_NAME"
    } else {
        ""
    };
    let sql = format!(
        "SELECT tc.CONSTRAINT_NAME, cc.CHECK_CLAUSE \
         FROM information_schema.TABLE_CONSTRAINTS tc \
         JOIN information_schema.CHECK_CONSTRAINTS cc \
           ON cc.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA \
          AND cc.CONSTRAINT_NAME = tc.CONSTRAINT_NAME{same_table} \
         WHERE tc.TABLE_SCHEMA = ? AND tc.TABLE_NAME = ? AND tc.CONSTRAINT_TYPE = 'CHECK' \
         ORDER BY tc.CONSTRAINT_NAME"
    );
    Ok(rows(conn, &sql, &[db, table])
        .await?
        .iter()
        .map(|r| ConstraintDef::Check {
            name: Some(string(r, 0)),
            expression: string(r, 1),
        })
        .collect())
}

/// Files a table's `STATISTICS` indexes: the primary key and plain unique indexes are
/// constraints, the rest are indexes.
pub(crate) fn sort_indexes(def: &mut TableDef, indexes: Vec<RawIndex>) {
    let mut uniques = Vec::new();
    for index in indexes {
        let columns = || index.def.columns.iter().map(|c| c.expr.clone()).collect();
        if index.def.name == "PRIMARY" {
            def.constraints.push(ConstraintDef::PrimaryKey {
                name: None,
                columns: columns(),
            });
        } else if index.plain_unique {
            uniques.push(ConstraintDef::Unique {
                name: Some(index.def.name.clone()),
                columns: columns(),
            });
        } else {
            def.indexes.push(index.def);
        }
    }
    def.constraints.extend(uniques);
}

pub(crate) async fn table(
    conn: &mut Conn,
    db: &str,
    name: &str,
    mariadb: bool,
) -> Result<TableDef, DriverError> {
    let row = rows(
        conn,
        "SELECT ENGINE, TABLE_COLLATION, TABLE_COMMENT FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND TABLE_TYPE = 'BASE TABLE'",
        &[db, name],
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| not_found("table", name))?;
    let collation = string(&row, 1);
    let mut def = TableDef::new(table_ref(db, name));
    def.columns = columns(conn, db, name, &collation, mariadb).await?;
    sort_indexes(&mut def, raw_indexes(conn, db, name, mariadb).await?);
    def.constraints.extend(foreign_keys(conn, db, name).await?);
    def.constraints
        .extend(checks(conn, db, name, mariadb).await?);
    def.triggers = triggers(conn, db, Some(name), None).await?;
    def.comment = text(&row, 2).filter(|c| !c.is_empty());
    if let Some(engine) = text(&row, 0) {
        def.options.push(("engine".into(), engine));
    }
    def.options
        .push(("charset".into(), charset_of(&collation).to_string()));
    def.options.push(("collation".into(), collation));
    Ok(def)
}

/// Triggers of `table`, or the one named `name`, in firing order.
async fn triggers(
    conn: &mut Conn,
    db: &str,
    table: Option<&str>,
    name: Option<&str>,
) -> Result<Vec<TriggerDef>, DriverError> {
    let (filter, value) = match (table, name) {
        (Some(t), _) => ("EVENT_OBJECT_TABLE", t),
        (None, Some(n)) => ("TRIGGER_NAME", n),
        (None, None) => return Ok(Vec::new()),
    };
    let sql = format!(
        "SELECT TRIGGER_NAME, EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION, \
                ACTION_STATEMENT \
         FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ? AND {filter} = ? \
         ORDER BY EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION, ACTION_ORDER"
    );
    Ok(rows(conn, &sql, &[db, value])
        .await?
        .iter()
        .map(|r| trigger_from_row(db, r))
        .collect())
}

/// A trigger from a row of the `TRIGGERS` query (its first five columns).
pub(crate) fn trigger_from_row(db: &str, r: &Row) -> TriggerDef {
    TriggerDef {
        name: string(r, 0),
        table: table_ref(db, &string(r, 1)),
        timing: string(r, 2),
        events: vec![string(r, 3)],
        for_each_row: true,
        condition: None,
        body: string(r, 4),
    }
}

async fn view(conn: &mut Conn, db: &str, name: &str) -> Result<ObjectDef, DriverError> {
    let row = rows(
        conn,
        "SELECT VIEW_DEFINITION, CHECK_OPTION, SECURITY_TYPE FROM information_schema.VIEWS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
        &[db, name],
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| not_found("view", name))?;
    let query = string(&row, 0);
    if query.is_empty() {
        return Err(DriverError::new(
            ErrorKind::Permission,
            format!("the definition of `{name}` is hidden: missing privileges"),
        ));
    }
    let mut options = vec![("security".to_string(), string(&row, 2))];
    let check = string(&row, 1);
    if check != "NONE" && !check.is_empty() {
        options.push(("check_option".into(), check));
    }
    let columns = rows(
        conn,
        "SELECT COLUMN_NAME FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
        &[db, name],
    )
    .await?
    .iter()
    .map(|r| string(r, 0))
    .collect();
    Ok(ObjectDef::View(ViewDef {
        view: table_ref(db, name),
        query,
        materialized: false,
        columns,
        comment: None,
        options,
        indexes: Vec::new(),
    }))
}

async fn routine(
    conn: &mut Conn,
    db: &str,
    name: &str,
    kind: ObjectKind,
) -> Result<ObjectDef, DriverError> {
    let routine_type = if kind == ObjectKind::Procedure {
        "PROCEDURE"
    } else {
        "FUNCTION"
    };
    let row = rows(
        conn,
        "SELECT DTD_IDENTIFIER, ROUTINE_DEFINITION, IS_DETERMINISTIC, SQL_DATA_ACCESS, \
                SECURITY_TYPE, ROUTINE_COMMENT \
         FROM information_schema.ROUTINES \
         WHERE ROUTINE_SCHEMA = ? AND ROUTINE_NAME = ? AND ROUTINE_TYPE = ?",
        &[db, name, routine_type],
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| not_found(&routine_type.to_lowercase(), name))?;
    let body = text(&row, 1).ok_or_else(|| {
        DriverError::new(
            ErrorKind::Permission,
            format!("the definition of `{name}` is hidden: missing privileges"),
        )
    })?;
    let d = MyDialect::default();
    let arguments: Vec<String> = rows(
        conn,
        "SELECT PARAMETER_MODE, PARAMETER_NAME, DTD_IDENTIFIER \
         FROM information_schema.PARAMETERS \
         WHERE SPECIFIC_SCHEMA = ? AND SPECIFIC_NAME = ? AND ROUTINE_TYPE = ? \
           AND ORDINAL_POSITION > 0 ORDER BY ORDINAL_POSITION",
        &[db, name, routine_type],
    )
    .await?
    .iter()
    .map(|r| {
        let param = format!("{} {}", d.quote_ident(&string(r, 1)), string(r, 2));
        match text(r, 0) {
            Some(mode) if kind == ObjectKind::Procedure => format!("{mode} {param}"),
            _ => param,
        }
    })
    .collect();
    let mut options = Vec::new();
    if string(&row, 2) == "YES" {
        options.push(("deterministic".to_string(), "true".to_string()));
    }
    options.push(("data_access".into(), string(&row, 3)));
    options.push(("security".into(), string(&row, 4)));
    if let Some(comment) = text(&row, 5).filter(|c| !c.is_empty()) {
        options.push(("comment".into(), comment));
    }
    Ok(ObjectDef::Routine(RoutineDef {
        schema: Some(db.to_string()),
        name: name.to_string(),
        kind,
        arguments: arguments.join(", "),
        identity_arguments: None,
        returns: (kind == ObjectKind::Function).then(|| string(&row, 0)),
        language: Some("SQL".into()),
        body,
        options,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_become_expressions() {
        let mysql = |raw: &str, extra: &str, ty: &str| {
            default_expression(Some(raw.into()), extra, ty, false)
        };
        assert_eq!(mysql("7", "", "int").as_deref(), Some("7"));
        assert_eq!(mysql("7", "", "varchar").as_deref(), Some("'7'"));
        assert_eq!(mysql("it's", "", "text").as_deref(), Some("'it''s'"));
        assert_eq!(
            mysql("CURRENT_TIMESTAMP", "DEFAULT_GENERATED", "datetime").as_deref(),
            Some("CURRENT_TIMESTAMP")
        );
        assert_eq!(
            mysql("rand()", "DEFAULT_GENERATED", "double").as_deref(),
            Some("(rand())")
        );
        assert_eq!(default_expression(None, "", "int", false), None);
        assert_eq!(
            default_expression(Some("NULL".into()), "", "int", true),
            None
        );
        assert_eq!(
            default_expression(Some("'x'".into()), "", "text", true).as_deref(),
            Some("'x'")
        );
        assert_eq!(
            on_update("DEFAULT_GENERATED on update CURRENT_TIMESTAMP"),
            Some("CURRENT_TIMESTAMP")
        );
        assert_eq!(charset_of("utf8mb4_0900_ai_ci"), "utf8mb4");
        assert_eq!(charset_of("binary"), "binary");
    }
}
