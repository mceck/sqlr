//! Pending edits of a result's rows and the DML that applies them (spec §7.7).
//!
//! Rows are identified only by their key (a primary key, or a unique key over `NOT NULL`
//! columns), compared with `=`. Values always travel as bound parameters; the preview
//! renders them as literals for display only.

use std::collections::HashMap;

use crate::driver::Dialect;
use crate::table::TableRef;
use crate::value::Value;

/// Why a Changeset cannot be turned into statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChangesetError {
    /// An update or delete is pending but the Changeset has no key columns.
    #[error("The changeset names no key columns")]
    NoKeyColumns,
    /// A row key has a different number of values than the Changeset has key columns.
    #[error("A row key has {values} values for {columns} key columns")]
    KeyLength {
        /// The number of values in the row key.
        values: usize,
        /// The number of key columns.
        columns: usize,
    },
}

/// One pending edit. `key` holds the row's key values as read, in `key_columns` order, so
/// an edit of a key column still finds the row by its original key.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Set one column of the row to a new value.
    Update {
        /// The row's key values as read.
        key: Vec<Value>,
        /// The edited column.
        column: String,
        /// Its new value.
        value: Value,
    },
    /// Columns left out take their default.
    Insert {
        /// The columns to set, with their values.
        values: Vec<(String, Value)>,
    },
    /// Remove the row.
    Delete {
        /// The row's key values as read.
        key: Vec<Value>,
    },
}

/// The pending edits of one table.
#[derive(Clone, Debug, PartialEq)]
pub struct Changeset {
    /// The table the edits apply to.
    pub table: TableRef,
    /// The columns that identify a row, in key order.
    pub key_columns: Vec<String>,
    /// The edits, in the order they were made.
    pub changes: Vec<Change>,
}

/// One statement of a changeset, with its parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct DmlStatement {
    /// The SQL text, with placeholders for `params`.
    pub sql: String,
    /// The bound values, in placeholder order.
    pub params: Vec<Value>,
    /// The statement must affect exactly one row, or the changeset is rolled back.
    pub expect_one_row: bool,
    /// Names the row in messages: `row id=42`, `new row 1`.
    pub row_label: String,
}

/// `value` as an SQL literal of `dialect`, for display (previews, filters typed into the
/// WHERE field). Generated DML binds values instead.
pub fn sql_literal(dialect: &dyn Dialect, value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::Bool(b) => dialect.bool_literal(*b).into(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) if f.is_finite() => f.to_string(),
        Value::Float(f) => dialect.string_literal(if f.is_nan() {
            "NaN"
        } else if *f > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }),
        Value::Text(t) => dialect.string_literal(t),
        Value::Bytes(b) => dialect.bytes_literal(b),
    }
}

/// How a statement writes its values: as placeholders that collect parameters, or as
/// literals.
enum Values<'d> {
    Bound(&'d dyn Dialect, Vec<Value>),
    Literal(&'d dyn Dialect),
}

impl Values<'_> {
    fn push(&mut self, value: &Value) -> String {
        match self {
            Values::Bound(dialect, params) => {
                params.push(value.clone());
                dialect.placeholder(params.len())
            }
            Values::Literal(dialect) => sql_literal(*dialect, value),
        }
    }

    fn into_params(self) -> Vec<Value> {
        match self {
            Values::Bound(_, params) => params,
            Values::Literal(_) => Vec::new(),
        }
    }
}

/// One row's merged updates: the columns in first-edit order, the last value of each.
struct RowUpdate<'a> {
    key: &'a [Value],
    columns: Vec<(&'a str, &'a Value)>,
}

/// What a statement is about, before its SQL is written.
enum Planned<'a> {
    Update(RowUpdate<'a>),
    Insert(&'a [(String, Value)], usize),
    Delete(&'a [Value]),
}

impl Changeset {
    /// The statements, in change order; the updates of one row are merged into one
    /// `UPDATE` at the place of that row's first update.
    ///
    /// An error when the changeset names no key columns, or a row key whose length is not
    /// theirs.
    pub fn statements(&self, dialect: &dyn Dialect) -> Result<Vec<DmlStatement>, ChangesetError> {
        self.statements_with_default_column(dialect, self.key_columns.first().map(String::as_str))
    }

    /// As [`Self::statements`], using a known writable column for default-only inserts.
    /// The column is a raw name, independently of the row key.
    pub fn statements_with_default_column(
        &self,
        dialect: &dyn Dialect,
        default_column: Option<&str>,
    ) -> Result<Vec<DmlStatement>, ChangesetError> {
        self.check_keys()?;
        Ok(self
            .plan()
            .iter()
            .map(|p| {
                let mut values = Values::Bound(dialect, Vec::new());
                let sql = self.write(dialect, p, &mut values, default_column);
                DmlStatement {
                    sql,
                    params: values.into_params(),
                    expect_one_row: true,
                    row_label: self.label(p),
                }
            })
            .collect())
    }

    /// The statements with their values as literals, each ended by `;`, one per line. For
    /// display only.
    pub fn preview(&self, dialect: &dyn Dialect) -> Result<String, ChangesetError> {
        self.check_keys()?;
        let mut out = String::new();
        for p in self.plan() {
            let mut values = Values::Literal(dialect);
            out.push_str(&self.write(
                dialect,
                &p,
                &mut values,
                self.key_columns.first().map(String::as_str),
            ));
            out.push_str(";\n");
        }
        Ok(out)
    }

    /// Every update and delete names its row by a full key.
    fn check_keys(&self) -> Result<(), ChangesetError> {
        let keyed = self
            .changes
            .iter()
            .any(|c| !matches!(c, Change::Insert { .. }));
        if keyed && self.key_columns.is_empty() {
            return Err(ChangesetError::NoKeyColumns);
        }
        for change in &self.changes {
            if let Change::Update { key, .. } | Change::Delete { key } = change
                && key.len() != self.key_columns.len()
            {
                return Err(ChangesetError::KeyLength {
                    values: key.len(),
                    columns: self.key_columns.len(),
                });
            }
        }
        Ok(())
    }

    fn plan(&self) -> Vec<Planned<'_>> {
        let mut planned: Vec<Planned<'_>> = Vec::new();
        // Index into `planned` of each row's merged update, by its key.
        let mut rows: HashMap<String, usize> = HashMap::new();
        let mut inserts = 0;
        for change in &self.changes {
            match change {
                Change::Update { key, column, value } => {
                    let id = format!("{key:?}");
                    match rows.get(&id) {
                        Some(&i) => {
                            if let Planned::Update(row) = &mut planned[i] {
                                match row.columns.iter_mut().find(|(c, _)| c == column) {
                                    Some(slot) => slot.1 = value,
                                    None => row.columns.push((column, value)),
                                }
                            }
                        }
                        None => {
                            rows.insert(id, planned.len());
                            planned.push(Planned::Update(RowUpdate {
                                key,
                                columns: vec![(column, value)],
                            }));
                        }
                    }
                }
                Change::Insert { values } => {
                    inserts += 1;
                    planned.push(Planned::Insert(values, inserts));
                }
                Change::Delete { key } => planned.push(Planned::Delete(key)),
            }
        }
        planned
    }

    fn write(
        &self,
        dialect: &dyn Dialect,
        planned: &Planned<'_>,
        values: &mut Values<'_>,
        default_column: Option<&str>,
    ) -> String {
        let table = dialect.qualified(&self.table);
        match planned {
            Planned::Update(row) => {
                let set: Vec<String> = row
                    .columns
                    .iter()
                    .map(|(c, v)| format!("{} = {}", dialect.quote_ident(c), values.push(v)))
                    .collect();
                let filter = self.key_filter(dialect, row.key, values);
                format!("UPDATE {table} SET {} WHERE {filter}", set.join(", "))
            }
            Planned::Insert([], _) => match default_column {
                Some(column) => dialect.insert_defaults_with_column_sql(&table, column),
                None => dialect.insert_defaults_sql(&table),
            },
            Planned::Insert(columns, _) => {
                let names: Vec<String> = columns
                    .iter()
                    .map(|(c, _)| dialect.quote_ident(c))
                    .collect();
                let placeholders: Vec<String> =
                    columns.iter().map(|(_, v)| values.push(v)).collect();
                format!(
                    "INSERT INTO {table} ({}) VALUES ({})",
                    names.join(", "),
                    placeholders.join(", ")
                )
            }
            Planned::Delete(key) => {
                let filter = self.key_filter(dialect, key, values);
                format!("DELETE FROM {table} WHERE {filter}")
            }
        }
    }

    /// `k1 = ? AND k2 = ?` over the key columns. Key columns are `NOT NULL`, so `=` holds.
    fn key_filter(&self, dialect: &dyn Dialect, key: &[Value], values: &mut Values<'_>) -> String {
        self.key_columns
            .iter()
            .zip(key)
            .map(|(c, v)| format!("{} = {}", dialect.quote_ident(c), values.push(v)))
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    fn label(&self, planned: &Planned<'_>) -> String {
        match planned {
            Planned::Update(RowUpdate { key, .. }) | Planned::Delete(key) => {
                let parts: Vec<String> = self
                    .key_columns
                    .iter()
                    .zip(key.iter())
                    .map(|(c, v)| format!("{c}={}", v.display()))
                    .collect();
                format!("row {}", parts.join(", "))
            }
            Planned::Insert(_, n) => format!("new row {n}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Node;

    /// `$n` placeholders and double-quoted identifiers.
    struct PgLike;
    /// `?` placeholders, backtick identifiers and `()` default rows.
    struct MyLike;

    impl Dialect for PgLike {
        fn quote_ident(&self, ident: &str) -> String {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
        fn select_all(&self, _node: &Node, _limit: Option<usize>) -> Option<String> {
            None
        }
        fn placeholder(&self, index: usize) -> String {
            format!("${index}")
        }
        fn bytes_literal(&self, bytes: &[u8]) -> String {
            format!("'\\x{}'", &crate::chunk::hex_bytes(bytes)[2..])
        }
    }

    impl Dialect for MyLike {
        fn quote_ident(&self, ident: &str) -> String {
            format!("`{}`", ident.replace('`', "``"))
        }
        fn select_all(&self, _node: &Node, _limit: Option<usize>) -> Option<String> {
            None
        }
        fn insert_defaults_sql(&self, table: &str) -> String {
            format!("INSERT INTO {table} () VALUES ()")
        }
    }

    fn table(schema: &str, name: &str) -> TableRef {
        TableRef {
            database: None,
            schema: Some(schema.into()),
            name: name.into(),
        }
    }

    fn changeset() -> Changeset {
        Changeset {
            table: table("public", "orders"),
            key_columns: vec!["id".into()],
            changes: vec![
                Change::Update {
                    key: vec![Value::Int(1)],
                    column: "total".into(),
                    value: Value::Float(9.5),
                },
                Change::Delete {
                    key: vec![Value::Int(3)],
                },
                Change::Update {
                    key: vec![Value::Int(1)],
                    column: "note".into(),
                    value: Value::Null,
                },
                // The last value of a column wins.
                Change::Update {
                    key: vec![Value::Int(1)],
                    column: "total".into(),
                    value: Value::Float(10.0),
                },
                Change::Insert {
                    values: vec![
                        ("note".into(), Value::Text("hi".into())),
                        ("total".into(), Value::Int(2)),
                    ],
                },
                Change::Insert { values: vec![] },
            ],
        }
    }

    #[test]
    fn changeset_update_insert_delete_sql() {
        let pg = changeset().statements(&PgLike).unwrap();
        assert_eq!(pg.len(), 4, "{pg:#?}");
        assert_eq!(
            pg[0].sql,
            "UPDATE \"public\".\"orders\" SET \"total\" = $1, \"note\" = $2 WHERE \"id\" = $3"
        );
        assert_eq!(
            pg[0].params,
            vec![Value::Float(10.0), Value::Null, Value::Int(1)]
        );
        assert_eq!(pg[0].row_label, "row id=1");
        assert!(pg[0].expect_one_row);
        assert_eq!(
            pg[1].sql,
            "DELETE FROM \"public\".\"orders\" WHERE \"id\" = $1"
        );
        assert_eq!(pg[1].params, vec![Value::Int(3)]);
        assert_eq!(pg[1].row_label, "row id=3");
        // Untouched columns are left out of the INSERT, so they take their defaults.
        assert_eq!(
            pg[2].sql,
            "INSERT INTO \"public\".\"orders\" (\"note\", \"total\") VALUES ($1, $2)"
        );
        assert_eq!(pg[2].params, vec![Value::Text("hi".into()), Value::Int(2)]);
        assert_eq!(pg[2].row_label, "new row 1");
        assert_eq!(
            pg[3].sql,
            "INSERT INTO \"public\".\"orders\" DEFAULT VALUES"
        );
        assert!(pg[3].params.is_empty());
        assert_eq!(pg[3].row_label, "new row 2");

        let my = changeset().statements(&MyLike).unwrap();
        assert_eq!(
            my[0].sql,
            "UPDATE `public`.`orders` SET `total` = ?, `note` = ? WHERE `id` = ?"
        );
        assert_eq!(my[1].sql, "DELETE FROM `public`.`orders` WHERE `id` = ?");
        assert_eq!(
            my[2].sql,
            "INSERT INTO `public`.`orders` (`note`, `total`) VALUES (?, ?)"
        );
        assert_eq!(my[3].sql, "INSERT INTO `public`.`orders` () VALUES ()");
    }

    #[test]
    fn composite_keys_and_key_column_edits() {
        let set = Changeset {
            table: TableRef {
                database: None,
                schema: None,
                name: "t".into(),
            },
            key_columns: vec!["a".into(), "b".into()],
            changes: vec![
                Change::Update {
                    key: vec![Value::Int(1), Value::Text("x".into())],
                    column: "a".into(),
                    value: Value::Int(7),
                },
                Change::Update {
                    key: vec![Value::Int(1), Value::Text("y".into())],
                    column: "a".into(),
                    value: Value::Int(8),
                },
            ],
        };
        let s = set.statements(&PgLike).unwrap();
        assert_eq!(s.len(), 2, "different keys are different rows");
        assert_eq!(
            s[0].sql,
            "UPDATE \"t\" SET \"a\" = $1 WHERE \"a\" = $2 AND \"b\" = $3"
        );
        assert_eq!(
            s[0].params,
            vec![Value::Int(7), Value::Int(1), Value::Text("x".into())]
        );
        assert_eq!(s[0].row_label, "row a=1, b=x");
    }

    #[test]
    fn changeset_on_awkward_table_name() {
        let set = Changeset {
            table: table("My Schema", "order\"s"),
            key_columns: vec!["Key Col".into()],
            changes: vec![
                Change::Update {
                    key: vec![Value::Int(1)],
                    column: "select".into(),
                    value: Value::Int(2),
                },
                Change::Delete {
                    key: vec![Value::Int(2)],
                },
            ],
        };
        let s = set.statements(&PgLike).unwrap();
        assert_eq!(
            s[0].sql,
            "UPDATE \"My Schema\".\"order\"\"s\" SET \"select\" = $1 WHERE \"Key Col\" = $2"
        );
        assert_eq!(
            s[1].sql,
            "DELETE FROM \"My Schema\".\"order\"\"s\" WHERE \"Key Col\" = $1"
        );
        let my = Changeset {
            table: table("My Schema", "order`s"),
            ..set
        }
        .statements(&MyLike)
        .unwrap();
        assert_eq!(
            my[0].sql,
            "UPDATE `My Schema`.`order``s` SET `select` = ? WHERE `Key Col` = ?"
        );
    }

    #[test]
    fn preview_renders_literals() {
        let set = Changeset {
            table: table("public", "people"),
            key_columns: vec!["id".into()],
            changes: vec![
                Change::Update {
                    key: vec![Value::Int(1)],
                    column: "name".into(),
                    value: Value::Text("O'Brien".into()),
                },
                Change::Update {
                    key: vec![Value::Int(1)],
                    column: "photo".into(),
                    value: Value::Bytes(vec![1, 2]),
                },
                Change::Insert {
                    values: vec![
                        ("name".into(), Value::Null),
                        ("ok".into(), Value::Bool(true)),
                    ],
                },
                Change::Delete {
                    key: vec![Value::Int(9)],
                },
            ],
        };
        assert_eq!(
            set.preview(&PgLike).unwrap(),
            "UPDATE \"public\".\"people\" SET \"name\" = 'O''Brien', \"photo\" = '\\x0102' \
             WHERE \"id\" = 1;\n\
             INSERT INTO \"public\".\"people\" (\"name\", \"ok\") VALUES (NULL, TRUE);\n\
             DELETE FROM \"public\".\"people\" WHERE \"id\" = 9;\n"
        );
        assert!(
            set.preview(&MyLike)
                .unwrap()
                .starts_with("UPDATE `public`.`people` SET `name` = 'O''Brien', `photo` = X'0102'")
        );
        assert_eq!(sql_literal(&PgLike, &Value::Float(f64::NAN)), "'NaN'");
        assert_eq!(sql_literal(&PgLike, &Value::Float(-0.5)), "-0.5");
    }

    #[test]
    fn malformed_keys_are_errors() {
        let mut set = changeset();
        set.key_columns.clear();
        assert_eq!(
            set.statements(&PgLike).unwrap_err().to_string(),
            "The changeset names no key columns"
        );
        let mut set = changeset();
        set.changes.push(Change::Delete {
            key: vec![Value::Int(1), Value::Int(2)],
        });
        assert_eq!(
            set.preview(&PgLike).unwrap_err().to_string(),
            "A row key has 2 values for 1 key columns"
        );
    }
}
