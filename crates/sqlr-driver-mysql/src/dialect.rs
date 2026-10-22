//! SQL dialect rules for MySQL and MariaDB.

use std::sync::Arc;

use sqlr_core::{DdlGen, Dialect, Node, SqlSyntax, TableRef, wrap_subquery};

use crate::syntax::mysql_syntax;

/// MySQL rules. `database` is the connection's default database, searched for unqualified
/// names; `mariadb` is set when the server is MariaDB, whose DDL differs in places.
#[derive(Clone, Debug, Default)]
pub struct MyDialect {
    pub database: Option<String>,
    pub mariadb: bool,
    /// The session's `sql_mode` has `NO_BACKSLASH_ESCAPES` (read at connect): a backslash
    /// in a string literal is a plain character.
    pub no_backslash_escapes: bool,
}

impl Dialect for MyDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("`{}`", ident.replace('`', "``"))
    }

    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String> {
        if !node.kind.opens_data() {
            return None;
        }
        // Node paths are `[database, folder, name]`.
        let [database, _folder, name] = node.path.segments() else {
            return None;
        };
        let mut sql = format!(
            "SELECT * FROM {}.{}",
            self.quote_ident(database),
            self.quote_ident(name)
        );
        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        Some(sql)
    }

    fn syntax(&self) -> Arc<SqlSyntax> {
        mysql_syntax()
    }

    fn default_schemas(&self) -> Vec<String> {
        self.database.iter().cloned().collect()
    }

    /// MySQL databases are sqlr's schemas.
    fn database_is_schema(&self) -> bool {
        true
    }

    /// MySQL databases are sqlr's schema level: `schema` names the database (`database`
    /// is accepted as a fallback).
    fn qualified(&self, table: &TableRef) -> String {
        match table.schema.as_ref().or(table.database.as_ref()) {
            Some(db) => format!("{}.{}", self.quote_ident(db), self.quote_ident(&table.name)),
            None => self.quote_ident(&table.name),
        }
    }

    fn begin_sql(&self) -> &'static str {
        "START TRANSACTION"
    }

    fn upsert_sql(&self, insert: &str, key: &[String], update: &[String]) -> Option<String> {
        // MySQL picks the conflicting key itself; it needs one assignment, so with nothing
        // to update a no-op on the first key column keeps the existing row.
        let sets: Vec<String> = if update.is_empty() {
            key.iter()
                .take(1)
                .map(|c| {
                    let c = self.quote_ident(c);
                    format!("{c} = {c}")
                })
                .collect()
        } else {
            update
                .iter()
                .map(|c| {
                    let c = self.quote_ident(c);
                    format!("{c} = VALUES({c})")
                })
                .collect()
        };
        if sets.is_empty() {
            return None;
        }
        Some(format!(
            "{insert} ON DUPLICATE KEY UPDATE {}",
            sets.join(", ")
        ))
    }

    fn explain_spec(
        &self,
        sql: &str,
        analyze: bool,
    ) -> Result<sqlr_core::ExplainSpec, sqlr_core::DriverError> {
        let reads = self.syntax().classify(sql) == sqlr_core::StatementClass::Read;
        if analyze && !reads {
            return Err(sqlr_core::DriverError::new(
                sqlr_core::ErrorKind::Unsupported,
                "Explain Analyze runs the statement: here it is only offered for queries; \
                 use Explain for a data change",
            ));
        }
        // MySQL prints a tree (with costs and, analyzed, times); MariaDB only has JSON.
        let (sql, parser) = match (self.mariadb, analyze) {
            (false, false) => (
                format!("EXPLAIN FORMAT=TREE {sql}"),
                sqlr_core::ExplainParser::MysqlTree,
            ),
            (false, true) => (
                format!("EXPLAIN ANALYZE {sql}"),
                sqlr_core::ExplainParser::MysqlTree,
            ),
            (true, false) => (
                format!("EXPLAIN FORMAT=JSON {sql}"),
                sqlr_core::ExplainParser::MysqlJson,
            ),
            (true, true) => (
                format!("ANALYZE FORMAT=JSON {sql}"),
                sqlr_core::ExplainParser::MysqlJson,
            ),
        };
        Ok(sqlr_core::ExplainSpec {
            before: Vec::new(),
            sql,
            after: Vec::new(),
            parser,
        })
    }

    fn load_session_sql(&self) -> Option<(Vec<String>, Vec<String>)> {
        // Datetime literals are read in the session's zone; a load folds its values into UTC.
        Some((
            vec![
                "SET @sqlr_time_zone = @@session.time_zone".into(),
                "SET time_zone = '+00:00'".into(),
            ],
            vec!["SET time_zone = @sqlr_time_zone".into()],
        ))
    }

    fn constraints_toggle_sql(&self) -> Option<(Vec<String>, Vec<String>)> {
        Some((
            vec!["SET FOREIGN_KEY_CHECKS = 0, UNIQUE_CHECKS = 0".into()],
            vec!["SET FOREIGN_KEY_CHECKS = 1, UNIQUE_CHECKS = 1".into()],
        ))
    }

    fn read_only_sql(&self, on: bool) -> Option<String> {
        Some(
            if on {
                "SET SESSION TRANSACTION READ ONLY"
            } else {
                "SET SESSION TRANSACTION READ WRITE"
            }
            .to_string(),
        )
    }

    /// MySQL has no `NULLS LAST`: `<col> IS NULL` sorts NULLs after the other values.
    fn wrap_order(&self, sql: &str, order: &[(String, bool)]) -> String {
        let keys: Vec<String> = order
            .iter()
            .map(|(column, desc)| {
                let col = self.quote_ident(column);
                let dir = if *desc { " DESC" } else { "" };
                format!("{col} IS NULL, {col}{dir}")
            })
            .collect();
        wrap_subquery(&self.syntax(), sql, &keys)
    }

    fn ddl_commits_transaction(&self) -> bool {
        true
    }

    /// A backslash is an escape character in MySQL string literals unless the session runs
    /// with `NO_BACKSLASH_ESCAPES`. Text holding one is written as a hex literal with a
    /// `utf8mb4` introducer, which reads the same in both modes; other text is quoted.
    fn string_literal(&self, text: &str) -> String {
        if text.contains('\\') {
            let hex = sqlr_core::hex_bytes(text.as_bytes());
            format!("_utf8mb4 X'{}'", &hex[2..])
        } else {
            format!("'{}'", text.replace('\'', "''"))
        }
    }

    fn bytes_literal(&self, bytes: &[u8]) -> String {
        let hex = sqlr_core::hex_bytes(bytes);
        format!("X'{}'", &hex[2..])
    }

    /// MySQL has no `DEFAULT VALUES`.
    fn insert_defaults_sql(&self, table: &str) -> String {
        format!("INSERT INTO {table} () VALUES ()")
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
        let insert = "INSERT INTO `t` (`a`, `b`) VALUES (?, ?)";
        let key = ["a".to_string()];
        assert_eq!(
            MyDialect::default()
                .upsert_sql(insert, &key, &["b".to_string()])
                .as_deref(),
            Some(
                "INSERT INTO `t` (`a`, `b`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `b` = VALUES(`b`)"
            )
        );
        // MySQL needs one assignment: a no-op on the key keeps existing rows as they are.
        assert_eq!(
            MyDialect::default()
                .upsert_sql(insert, &key, &[])
                .as_deref(),
            Some("INSERT INTO `t` (`a`, `b`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `a` = `a`")
        );
        let (set, reset) = MyDialect::default().load_session_sql().unwrap();
        assert_eq!(
            set.last().map(String::as_str),
            Some("SET time_zone = '+00:00'")
        );
        assert_eq!(reset, ["SET time_zone = @sqlr_time_zone"]);
        let (off, on) = MyDialect::default().constraints_toggle_sql().unwrap();
        assert_eq!(off, ["SET FOREIGN_KEY_CHECKS = 0, UNIQUE_CHECKS = 0"]);
        assert_eq!(on, ["SET FOREIGN_KEY_CHECKS = 1, UNIQUE_CHECKS = 1"]);
    }

    fn node(path: &[&str], kind: NodeKind) -> Node {
        Node {
            path: NodePath(path.iter().map(|s| s.to_string()).collect()),
            name: path.last().unwrap().to_string(),
            kind,
            has_children: false,
            detail: None,
        }
    }

    #[test]
    fn quote_ident_doubles_backticks() {
        let d = MyDialect::default();
        assert_eq!(d.quote_ident("users"), "`users`");
        assert_eq!(d.quote_ident("a`b \"c\""), "`a``b \"c\"`");
    }

    #[test]
    fn select_all_qualifies_by_database() {
        let d = MyDialect::default();
        let n = node(&["shop", "tables", "order`s"], NodeKind::Table);
        assert_eq!(
            d.select_all(&n, Some(200)).as_deref(),
            Some("SELECT * FROM `shop`.`order``s` LIMIT 200")
        );
        let v = node(&["shop", "views", "v"], NodeKind::View);
        assert_eq!(
            d.select_all(&v, None).as_deref(),
            Some("SELECT * FROM `shop`.`v`")
        );
        let c = node(&["shop", "tables", "t", "columns", "id"], NodeKind::Column);
        assert_eq!(d.select_all(&c, None), None);
    }

    #[test]
    fn transaction_and_wrappers() {
        let d = MyDialect {
            database: Some("shop".into()),
            ..MyDialect::default()
        };
        assert_eq!(d.default_schemas(), ["shop"]);
        assert!(d.database_is_schema());
        assert_eq!(d.placeholder(3), "?");
        assert_eq!(d.begin_sql(), "START TRANSACTION");
        assert_eq!(
            d.insert_defaults_sql("`shop`.`t`"),
            "INSERT INTO `shop`.`t` () VALUES ()"
        );
        assert!(d.ddl_commits_transaction());
        assert_eq!(
            d.read_only_sql(true).as_deref(),
            Some("SET SESSION TRANSACTION READ ONLY")
        );
        assert_eq!(d.savepoint_sql("s`1"), "SAVEPOINT `s``1`");
        assert_eq!(d.rollback_to_sql("s1"), "ROLLBACK TO SAVEPOINT `s1`");
        assert_eq!(
            d.wrap_order("select * from t # note\n;", &[("My Col".into(), true)]),
            "SELECT * FROM (\nselect * from t # note\n) sqlr_q ORDER BY `My Col` IS NULL, `My Col` DESC"
        );
        let t = TableRef {
            database: None,
            schema: Some("shop".into()),
            name: "o`s".into(),
        };
        assert_eq!(d.qualified(&t), "`shop`.`o``s`");
        let t = TableRef {
            database: None,
            schema: None,
            name: "o".into(),
        };
        assert_eq!(d.qualified(&t), "`o`");
    }

    #[test]
    fn literals_escape_backslashes() {
        let d = MyDialect::default();
        assert_eq!(d.string_literal("it's"), "'it''s'");
        assert_eq!(d.string_literal("a\\b"), "_utf8mb4 X'615c62'");
        assert_eq!(d.bytes_literal(&[1, 0xab]), "X'01ab'");
    }
}
