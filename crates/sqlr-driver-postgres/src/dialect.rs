//! SQL dialect rules for PostgreSQL.

use std::sync::Arc;

use sqlr_core::{DdlGen, Dialect, LexState, Node, SqlSyntax, TokenKind, hex_bytes};

use crate::syntax::pg_syntax;

pub struct PgDialect;

impl Dialect for PgDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String> {
        if !node.kind.opens_data() {
            return None;
        }
        // Node paths are `[schema, folder, name]`, or `[database, schema, folder, name]`.
        let (schema, name) = match node.path.segments() {
            [schema, _folder, name] => (schema, name),
            [_database, schema, _folder, name] => (schema, name),
            _ => return None,
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
        pg_syntax()
    }

    fn default_schemas(&self) -> Vec<String> {
        vec!["public".to_string()]
    }

    fn is_system_schema(&self, schema: &str) -> bool {
        matches!(schema, "pg_catalog" | "information_schema")
    }

    fn placeholder(&self, index: usize) -> String {
        format!("${index}")
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
                    format!("{c} = EXCLUDED.{c}")
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
        if self.has_parameters(sql) {
            return Err(sqlr_core::DriverError::new(
                sqlr_core::ErrorKind::Unsupported,
                "Explain needs a statement without parameters ($1…): write the values in",
            ));
        }
        let reads = self.syntax().classify(sql) == sqlr_core::StatementClass::Read;
        let options = if analyze {
            "ANALYZE, BUFFERS, FORMAT JSON"
        } else {
            "FORMAT JSON"
        };
        // A data change is only run inside a transaction that is rolled back.
        let wrap = analyze && !reads;
        Ok(sqlr_core::ExplainSpec {
            before: if wrap {
                vec!["BEGIN".into()]
            } else {
                Vec::new()
            },
            sql: format!("EXPLAIN ({options}) {sql}"),
            after: if wrap {
                vec!["ROLLBACK".into()]
            } else {
                Vec::new()
            },
            parser: sqlr_core::ExplainParser::PgJson,
        })
    }

    fn constraints_toggle_sql(&self) -> Option<(Vec<String>, Vec<String>)> {
        // Needs superuser; the server says so when the role lacks it.
        Some((
            vec!["SET session_replication_role = replica".into()],
            vec!["SET session_replication_role = DEFAULT".into()],
        ))
    }

    fn read_only_sql(&self, on: bool) -> Option<String> {
        let mode = if on { "READ ONLY" } else { "READ WRITE" };
        Some(format!("SET SESSION CHARACTERISTICS AS TRANSACTION {mode}"))
    }

    fn error_aborts_transaction(&self) -> bool {
        true
    }

    /// Text holding a backslash uses an escape string (`E'…'`, backslashes doubled), which
    /// reads the same whatever `standard_conforming_strings` is; other text is quoted.
    fn string_literal(&self, text: &str) -> String {
        let quoted = text.replace('\'', "''");
        if text.contains('\\') {
            format!("E'{}'", quoted.replace('\\', "\\\\"))
        } else {
            format!("'{quoted}'")
        }
    }

    /// `E'\\x…'::bytea`: the escape string keeps the hex form's backslash whatever
    /// `standard_conforming_strings` is.
    fn bytes_literal(&self, bytes: &[u8]) -> String {
        format!("E'\\\\x{}'::bytea", &hex_bytes(bytes)[2..])
    }

    fn ddl_gen(&self) -> Option<&dyn DdlGen> {
        Some(self)
    }
}

impl PgDialect {
    /// Whether `sql` binds a numbered parameter (`$1`), lexed so a `$1` inside a string
    /// literal or a comment does not count.
    fn has_parameters(&self, sql: &str) -> bool {
        let mut tokens = Vec::new();
        self.syntax().lex(sql, LexState::Normal, &mut tokens);
        tokens
            .iter()
            .any(|t| t.kind == TokenKind::Param && sql[t.start..t.end].starts_with('$'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::{Dialect, Driver, Node, NodeKind, NodePath};

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
    fn dialect_exposes_syntax_and_defaults() {
        assert_eq!(PgDialect.default_schemas(), ["public"]);
        assert!(crate::PostgresDriver::new().dialect().is_some());
        assert!(PgDialect.syntax().dollar_quotes);
    }

    #[test]
    fn placeholders_literals_and_read_only() {
        assert_eq!(PgDialect.placeholder(3), "$3");
        assert_eq!(PgDialect.bytes_literal(&[0, 0xab]), "E'\\\\x00ab'::bytea");
        assert_eq!(PgDialect.string_literal("it's"), "'it''s'");
        assert_eq!(PgDialect.string_literal("a\\b'"), "E'a\\\\b'''");
        assert_eq!(
            PgDialect.read_only_sql(true).as_deref(),
            Some("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
        );
        assert_eq!(
            PgDialect.read_only_sql(false).as_deref(),
            Some("SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE")
        );
    }

    #[test]
    fn upsert_and_constraint_toggles() {
        let insert = "INSERT INTO \"t\" (\"a\", \"b\") VALUES ($1, $2)";
        let key = ["a".to_string()];
        assert_eq!(
            PgDialect
                .upsert_sql(insert, &key, &["b".to_string()])
                .as_deref(),
            Some(
                "INSERT INTO \"t\" (\"a\", \"b\") VALUES ($1, $2) ON CONFLICT (\"a\") DO UPDATE \
                 SET \"b\" = EXCLUDED.\"b\""
            )
        );
        assert_eq!(
            PgDialect.upsert_sql(insert, &key, &[]).as_deref(),
            Some("INSERT INTO \"t\" (\"a\", \"b\") VALUES ($1, $2) ON CONFLICT (\"a\") DO NOTHING")
        );
        let (off, on) = PgDialect.constraints_toggle_sql().unwrap();
        assert_eq!(off, ["SET session_replication_role = replica"]);
        assert_eq!(on, ["SET session_replication_role = DEFAULT"]);
    }

    #[test]
    fn quote_ident_plain() {
        assert_eq!(PgDialect.quote_ident("users"), "\"users\"");
    }

    #[test]
    fn quote_ident_mixed_case_space_quote() {
        assert_eq!(PgDialect.quote_ident("My \"T\""), "\"My \"\"T\"\"\"");
    }

    #[test]
    fn explain_refuses_parameters_but_not_literals_that_look_like_them() {
        for sql in [
            "SELECT 1",
            "SELECT '$1' AS price",
            "SELECT $$ a $1 b $$ AS note",
            "SELECT data ? 'key' FROM t",
            "SELECT 1 -- $1 in a comment",
        ] {
            assert!(PgDialect.explain_spec(sql, false).is_ok(), "{sql}");
        }
        for sql in [
            "SELECT $1::int",
            "SELECT $12::int",
            "INSERT INTO t VALUES ($1)",
        ] {
            let err = PgDialect.explain_spec(sql, false).unwrap_err();
            assert_eq!(err.kind, sqlr_core::ErrorKind::Unsupported, "{sql}");
        }
    }

    #[test]
    fn select_all_table_with_limit() {
        let n = node(&["public", "tables", "users"], NodeKind::Table);
        assert_eq!(
            PgDialect.select_all(&n, Some(500)).as_deref(),
            Some("SELECT * FROM \"public\".\"users\" LIMIT 500")
        );
        assert_eq!(
            PgDialect.select_all(&n, None).as_deref(),
            Some("SELECT * FROM \"public\".\"users\"")
        );
    }

    #[test]
    fn select_all_in_another_database() {
        let n = node(&["other", "public", "tables", "users"], NodeKind::Table);
        assert_eq!(
            PgDialect.select_all(&n, None).as_deref(),
            Some("SELECT * FROM \"public\".\"users\"")
        );
    }

    #[test]
    fn select_all_none_for_column() {
        let n = node(
            &["public", "tables", "users", "columns", "id"],
            NodeKind::Column,
        );
        assert_eq!(PgDialect.select_all(&n, Some(5)), None);
    }
}
