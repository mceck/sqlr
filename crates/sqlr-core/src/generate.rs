//! "Generate SQL" templates for an object (explorer menu and object editor toolbar).
//! Values are `/* <column> */ NULL`, so a template runs as is and shows what goes where.

use crate::driver::Dialect;
use crate::error::{DriverError, ErrorKind};
use crate::schema::{ObjectDef, ObjectKind, TableDef};
use crate::schema_change::SchemaChange;
use crate::table::TableRef;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SqlTemplate {
    Select,
    Insert,
    Update,
    Delete,
    Create,
    Drop,
}

/// `name` inside a block comment; a `*/` in it would end the comment early.
fn comment(name: &str) -> String {
    format!("/* {} */", name.replace("*/", "* /"))
}

fn value(name: &str) -> String {
    format!("{} NULL", comment(name))
}

fn unsupported(template: SqlTemplate, kind: ObjectKind) -> DriverError {
    DriverError::new(
        ErrorKind::Unsupported,
        format!(
            "no {template:?} template for a {}",
            kind.keyword().to_lowercase()
        ),
    )
}

/// A relation's name and columns: `(name, columns, writable columns, row key)`.
struct Relation<'a> {
    name: &'a TableRef,
    columns: Vec<String>,
    writable: Vec<String>,
    key: Option<Vec<String>>,
}

fn table_relation(t: &TableDef) -> Relation<'_> {
    Relation {
        name: &t.table,
        columns: t.columns.iter().map(|c| c.name.clone()).collect(),
        writable: t
            .columns
            .iter()
            .filter(|c| c.generated.is_none())
            .filter(|c| c.identity != Some(crate::schema::Identity::Always))
            .map(|c| c.name.clone())
            .collect(),
        key: t.row_key(),
    }
}

/// The `WHERE` of an UPDATE or DELETE: the row key, or a condition that matches nothing.
fn key_condition(rel: &Relation, d: &dyn Dialect) -> String {
    match &rel.key {
        Some(key) => key
            .iter()
            .map(|k| format!("{} = {}", d.quote_ident(k), value(k)))
            .collect::<Vec<_>>()
            .join(" AND "),
        None => "1 = 0 /* no key: write the condition */".to_string(),
    }
}

/// The statement that drops `def`, with `IF EXISTS` (indexes drop without it: MySQL
/// has none).
fn drop_sql(def: &ObjectDef, d: &dyn Dialect) -> Result<String, DriverError> {
    let generator = d
        .ddl_gen()
        .ok_or_else(|| unsupported(SqlTemplate::Drop, def.kind()))?;
    let object = def.object_ref();
    let kind = object.kind;
    let statements = generator.ddl(&[SchemaChange::Drop {
        object,
        cascade: false,
    }])?;
    let mut sql = statements.join(";\n");
    let prefix = format!("DROP {} ", kind.keyword());
    if kind != ObjectKind::Index
        && sql.starts_with(&prefix)
        && !sql[prefix.len()..].starts_with("IF EXISTS")
    {
        sql.insert_str(prefix.len(), "IF EXISTS ");
    }
    Ok(format!("{sql};"))
}

pub fn generate_sql(
    template: SqlTemplate,
    def: &ObjectDef,
    dialect: &dyn Dialect,
) -> Result<String, DriverError> {
    let d = dialect;
    match template {
        SqlTemplate::Create => {
            let generator = d
                .ddl_gen()
                .ok_or_else(|| unsupported(template, def.kind()))?;
            return generator.create_ddl(def);
        }
        SqlTemplate::Drop => return drop_sql(def, d),
        _ => {}
    }
    let rel = match def {
        ObjectDef::Table(t) => table_relation(t),
        ObjectDef::View(v) if !v.materialized || template == SqlTemplate::Select => Relation {
            name: &v.view,
            columns: v.columns.clone(),
            writable: v.columns.clone(),
            key: None,
        },
        _ => return Err(unsupported(template, def.kind())),
    };
    let name = d.qualified(rel.name);
    let quoted = |cols: &[String]| {
        cols.iter()
            .map(|c| d.quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Ok(match template {
        SqlTemplate::Select => {
            let cols = if rel.columns.is_empty() {
                "*".to_string()
            } else {
                quoted(&rel.columns)
            };
            format!("SELECT {cols}\nFROM {name};")
        }
        SqlTemplate::Insert if rel.writable.is_empty() => {
            format!("{};", d.insert_defaults_sql(&name))
        }
        SqlTemplate::Insert => {
            let values: Vec<String> = rel.writable.iter().map(|c| value(c)).collect();
            format!(
                "INSERT INTO {name} ({})\nVALUES ({});",
                quoted(&rel.writable),
                values.join(", ")
            )
        }
        SqlTemplate::Update => {
            let key = rel.key.clone().unwrap_or_default();
            let mut set: Vec<String> = rel
                .writable
                .iter()
                .filter(|c| !key.contains(c))
                .map(|c| format!("{} = {}", d.quote_ident(c), value(c)))
                .collect();
            if set.is_empty() {
                // Every column is part of the key: a no-op assignment keeps it valid.
                set = key
                    .iter()
                    .map(|k| format!("{0} = {0}", d.quote_ident(k)))
                    .collect();
            }
            format!(
                "UPDATE {name}\nSET {}\nWHERE {};",
                set.join(", "),
                key_condition(&rel, d)
            )
        }
        SqlTemplate::Delete => format!("DELETE FROM {name}\nWHERE {};", key_condition(&rel, d)),
        SqlTemplate::Create | SqlTemplate::Drop => unreachable!("handled above"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::DdlGen;
    use crate::node::Node;
    use crate::schema::{ColumnDef, ConstraintDef, Identity, ViewDef};

    struct Ansi;

    impl Dialect for Ansi {
        fn quote_ident(&self, ident: &str) -> String {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
        fn select_all(&self, _node: &Node, _limit: Option<usize>) -> Option<String> {
            None
        }
        fn ddl_gen(&self) -> Option<&dyn DdlGen> {
            Some(self)
        }
    }

    impl DdlGen for Ansi {
        fn ddl(&self, changes: &[SchemaChange]) -> Result<Vec<String>, DriverError> {
            Ok(changes
                .iter()
                .map(|c| match c {
                    SchemaChange::Drop { object, .. } => format!(
                        "DROP {} {}",
                        object.kind.keyword(),
                        self.qualified(&object.table_ref())
                    ),
                    other => format!("{other:?}"),
                })
                .collect())
        }
        fn create_ddl(&self, def: &ObjectDef) -> Result<String, DriverError> {
            Ok(format!("CREATE {};", def.kind().keyword()))
        }
    }

    fn col(name: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            type_name: "text".into(),
            nullable,
            ..Default::default()
        }
    }

    fn users() -> TableDef {
        let mut t = TableDef::new(TableRef {
            database: None,
            schema: Some("public".into()),
            name: "users".into(),
        });
        t.columns = vec![
            ColumnDef {
                identity: Some(Identity::ByDefault),
                ..col("id", false)
            },
            col("name", false),
            col("e*/mail", true),
        ];
        t.constraints.push(ConstraintDef::PrimaryKey {
            name: None,
            columns: vec!["id".into()],
        });
        t
    }

    #[test]
    fn generate_templates() {
        let def = ObjectDef::Table(users());
        let sql = |t| generate_sql(t, &def, &Ansi).unwrap();
        assert_eq!(
            sql(SqlTemplate::Select),
            "SELECT \"id\", \"name\", \"e*/mail\"\nFROM \"public\".\"users\";"
        );
        assert_eq!(
            sql(SqlTemplate::Insert),
            "INSERT INTO \"public\".\"users\" (\"id\", \"name\", \"e*/mail\")\n\
             VALUES (/* id */ NULL, /* name */ NULL, /* e* /mail */ NULL);"
        );
        assert_eq!(
            sql(SqlTemplate::Update),
            "UPDATE \"public\".\"users\"\n\
             SET \"name\" = /* name */ NULL, \"e*/mail\" = /* e* /mail */ NULL\n\
             WHERE \"id\" = /* id */ NULL;"
        );
        assert_eq!(
            sql(SqlTemplate::Delete),
            "DELETE FROM \"public\".\"users\"\nWHERE \"id\" = /* id */ NULL;"
        );
        assert_eq!(sql(SqlTemplate::Create), "CREATE TABLE;");
        assert_eq!(
            sql(SqlTemplate::Drop),
            "DROP TABLE IF EXISTS \"public\".\"users\";"
        );
    }

    #[test]
    fn update_template_when_every_column_is_key() {
        let mut t = users();
        t.columns.truncate(1);
        let def = ObjectDef::Table(t);
        assert_eq!(
            generate_sql(SqlTemplate::Update, &def, &Ansi).unwrap(),
            "UPDATE \"public\".\"users\"\nSET \"id\" = \"id\"\nWHERE \"id\" = /* id */ NULL;"
        );
    }

    #[test]
    fn insert_template_with_no_writable_column_inserts_defaults() {
        // A table whose only column is an identity or generated one has no column an
        // INSERT may name: `INSERT INTO t () VALUES ()` is not valid everywhere.
        let mut t = users();
        t.columns.truncate(1);
        t.columns[0].identity = Some(Identity::Always);
        t.constraints.clear();
        assert_eq!(
            generate_sql(SqlTemplate::Insert, &ObjectDef::Table(t), &Ansi).unwrap(),
            "INSERT INTO \"public\".\"users\" DEFAULT VALUES;"
        );
    }

    #[test]
    fn templates_skip_generated_columns_and_need_a_key() {
        let mut t = users();
        t.columns[0].identity = Some(Identity::Always);
        t.columns.push(ColumnDef {
            generated: Some("upper(name)".into()),
            ..col("shout", true)
        });
        t.constraints.clear();
        let def = ObjectDef::Table(t);
        assert_eq!(
            generate_sql(SqlTemplate::Insert, &def, &Ansi).unwrap(),
            "INSERT INTO \"public\".\"users\" (\"name\", \"e*/mail\")\n\
             VALUES (/* name */ NULL, /* e* /mail */ NULL);"
        );
        assert_eq!(
            generate_sql(SqlTemplate::Delete, &def, &Ansi).unwrap(),
            "DELETE FROM \"public\".\"users\"\nWHERE 1 = 0 /* no key: write the condition */;"
        );
        let view = ObjectDef::View(ViewDef {
            view: TableRef {
                database: None,
                schema: None,
                name: "v".into(),
            },
            query: "SELECT 1 AS a".into(),
            materialized: true,
            columns: vec!["a".into()],
            comment: None,
            options: Vec::new(),
            indexes: Vec::new(),
        });
        assert_eq!(
            generate_sql(SqlTemplate::Select, &view, &Ansi).unwrap(),
            "SELECT \"a\"\nFROM \"v\";"
        );
        let err = generate_sql(SqlTemplate::Insert, &view, &Ansi).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported);
    }
}
