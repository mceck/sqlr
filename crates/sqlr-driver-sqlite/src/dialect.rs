//! SQL dialect rules for SQLite.

use std::sync::Arc;

use sqlr_core::{DdlGen, Dialect, Node, SqlSyntax};

use crate::syntax::sqlite_syntax;

pub struct SqliteDialect;

impl Dialect for SqliteDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String> {
        if !node.kind.opens_data() {
            return None;
        }
        let [schema, _folder, name] = node.path.segments() else {
            return None;
        };
        let mut sql = format!(
            "SELECT * FROM {}.{}",
            self.quote_ident(schema),
            self.quote_ident(name)
        );
        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        Some(sql)
    }

    fn syntax(&self) -> Arc<SqlSyntax> {
        sqlite_syntax()
    }

    fn default_schemas(&self) -> Vec<String> {
        vec!["main".to_string()]
    }

    fn upsert_sql(&self, insert: &str, key: &[String], update: &[String]) -> Option<String> {
        let keys: Vec<String> = key.iter().map(|k| self.quote_ident(k)).collect();
        let action = if update.is_empty() {
            "DO NOTHING".to_string()
        } else {
            let sets: Vec<String> = update
                .iter()
                .map(|c| {
                    let c = self.quote_ident(c);
                    format!("{c} = excluded.{c}")
                })
                .collect();
            format!("DO UPDATE SET {}", sets.join(", "))
        };
        Some(format!(
            "{insert} ON CONFLICT ({}) {action}",
            keys.join(", ")
        ))
    }

    fn explain_spec(
        &self,
        sql: &str,
        analyze: bool,
    ) -> Result<sqlr_core::ExplainSpec, sqlr_core::DriverError> {
        if analyze {
            return Err(sqlr_core::DriverError::new(
                sqlr_core::ErrorKind::Unsupported,
                "SQLite has no EXPLAIN ANALYZE: use Explain for the query plan",
            ));
        }
        Ok(sqlr_core::ExplainSpec {
            before: Vec::new(),
            sql: format!("EXPLAIN QUERY PLAN {sql}"),
            after: Vec::new(),
            parser: sqlr_core::ExplainParser::SqliteRows,
        })
    }

    fn constraints_toggle_sql(&self) -> Option<(Vec<String>, Vec<String>)> {
        // The pragma is a no-op inside a transaction, so it runs before BEGIN.
        Some((
            vec!["PRAGMA foreign_keys = OFF".into()],
            vec!["PRAGMA foreign_keys = ON".into()],
        ))
    }

    fn read_only_sql(&self, on: bool) -> Option<String> {
        Some(format!("PRAGMA query_only = {}", u8::from(on)))
    }

    fn ddl_gen(&self) -> Option<&dyn DdlGen> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::{NodeKind, NodePath};

    #[test]
    fn upsert_and_constraint_toggles() {
        let d = SqliteDialect;
        let insert = "INSERT INTO \"t\" (\"a\", \"b\") VALUES (?, ?)";
        let key = ["a".to_string()];
        assert_eq!(
            d.upsert_sql(insert, &key, &["b".to_string()]).as_deref(),
            Some(
                "INSERT INTO \"t\" (\"a\", \"b\") VALUES (?, ?) ON CONFLICT (\"a\") DO UPDATE \
                 SET \"b\" = excluded.\"b\""
            )
        );
        let (off, on) = d.constraints_toggle_sql().unwrap();
        assert_eq!(off, ["PRAGMA foreign_keys = OFF"]);
        assert_eq!(on, ["PRAGMA foreign_keys = ON"]);
    }

    #[test]
    fn dialect_rules() {
        let d = SqliteDialect;
        assert_eq!(d.quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(d.placeholder(3), "?");
        assert_eq!(d.default_schemas(), ["main"]);
        let node = Node {
            path: NodePath(vec!["main".into(), "tables".into(), "My T".into()]),
            name: "My T".into(),
            kind: NodeKind::Table,
            has_children: true,
            detail: None,
        };
        assert_eq!(
            d.select_all(&node, Some(5)).as_deref(),
            Some("SELECT * FROM \"main\".\"My T\" LIMIT 5")
        );
        assert!(!d.ddl_commits_transaction());
    }
}
