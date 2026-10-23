//! T-SQL dialect rules for the editor, grids and generated statements.

use std::sync::Arc;

use sqlr_core::{
    Dialect, LexState, Node, NodeKind, SqlSyntax, Token, TokenKind, merge_sql, trim_statement_in,
    wrap_subquery,
};

use crate::syntax::tsql_syntax;

pub struct MsDialect;

/// `sql` ready to sit in a subquery: T-SQL rejects a top-level `ORDER BY` there unless the
/// query also has `TOP` or an `OFFSET`, so a bare one gets `OFFSET 0 ROWS`.
fn as_subquery(syntax: &SqlSyntax, sql: &str) -> String {
    let sql = trim_statement_in(syntax, sql);
    let mut tokens: Vec<Token> = Vec::new();
    syntax.lex(&sql, LexState::Normal, &mut tokens);
    let (mut depth, mut order_by, mut limited) = (0usize, false, false);
    let mut prev_order = false;
    for t in &tokens {
        match t.kind {
            TokenKind::Comment => continue,
            TokenKind::Punct => match &sql[t.start..t.end] {
                "(" => depth += 1,
                ")" => depth = depth.saturating_sub(1),
                _ => {}
            },
            TokenKind::Keyword | TokenKind::Ident | TokenKind::Function if depth == 0 => {
                let word = &sql[t.start..t.end];
                if word.eq_ignore_ascii_case("BY") && prev_order {
                    order_by = true;
                }
                if ["TOP", "OFFSET", "FETCH"]
                    .iter()
                    .any(|k| word.eq_ignore_ascii_case(k))
                {
                    limited = true;
                }
                prev_order = word.eq_ignore_ascii_case("ORDER");
                continue;
            }
            _ => {}
        }
        prev_order = false;
    }
    if order_by && !limited {
        format!("{sql}\nOFFSET 0 ROWS")
    } else {
        sql.into_owned()
    }
}

impl Dialect for MsDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("[{}]", ident.replace(']', "]]"))
    }

    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String> {
        if !node.kind.opens_data() {
            return None;
        }
        let (schema, name) = match node.path.segments() {
            [schema, _folder, name] => (schema.as_str(), name.as_str()),
            [schema, name] if node.kind != NodeKind::Schema => (schema.as_str(), name.as_str()),
            _ => return None,
        };
        let top = limit.map_or_else(String::new, |n| format!("TOP ({n}) "));
        Some(format!(
            "SELECT {top}* FROM {}.{}",
            self.quote_ident(schema),
            self.quote_ident(name)
        ))
    }

    fn syntax(&self) -> Arc<SqlSyntax> {
        tsql_syntax()
    }

    fn default_schemas(&self) -> Vec<String> {
        vec!["dbo".to_string()]
    }

    fn placeholder(&self, index: usize) -> String {
        format!("@P{index}")
    }

    fn begin_sql(&self) -> &'static str {
        "BEGIN TRANSACTION"
    }

    fn savepoint_sql(&self, name: &str) -> String {
        format!("SAVE TRANSACTION {}", self.quote_ident(name))
    }

    fn rollback_to_sql(&self, name: &str) -> String {
        format!("ROLLBACK TRANSACTION {}", self.quote_ident(name))
    }

    /// T-SQL has no `NULLS LAST`: an explicit sort key puts NULLs after the values.
    fn wrap_order(&self, sql: &str, order: &[(String, bool)]) -> String {
        let keys: Vec<String> = order
            .iter()
            .map(|(column, desc)| {
                let quoted = self.quote_ident(column);
                let dir = if *desc { " DESC" } else { "" };
                format!("CASE WHEN {quoted} IS NULL THEN 1 ELSE 0 END, {quoted}{dir}")
            })
            .collect();
        wrap_subquery(&self.syntax(), &as_subquery(&self.syntax(), sql), &keys)
    }

    fn wrap_where(&self, sql: &str, condition: &str) -> String {
        format!(
            "SELECT * FROM (\n{}\n) sqlr_q WHERE {}",
            as_subquery(&self.syntax(), sql),
            condition.trim()
        )
    }

    /// `N'…'` keeps characters outside the database's code page.
    fn string_literal(&self, text: &str) -> String {
        format!("N'{}'", text.replace('\'', "''"))
    }

    /// `SET SHOWPLAN_ALL ON` makes the server return the estimated plan instead of running
    /// the statement. The driver reads a rowset only for a query, so a data change has no
    /// plan to read, and there is no analyze.
    fn explain_spec(
        &self,
        sql: &str,
        analyze: bool,
    ) -> Result<sqlr_core::ExplainSpec, sqlr_core::DriverError> {
        let unsupported = |m: &str| {
            Err(sqlr_core::DriverError::new(
                sqlr_core::ErrorKind::Unsupported,
                m,
            ))
        };
        if analyze {
            return unsupported("SQL Server has no EXPLAIN ANALYZE here: use Explain");
        }
        if self.syntax().classify(sql) != sqlr_core::StatementClass::Read {
            return unsupported("Explain on SQL Server is offered for queries only");
        }
        Ok(sqlr_core::ExplainSpec {
            before: vec!["SET SHOWPLAN_ALL ON".into()],
            sql: sql.trim().trim_end_matches(';').to_string(),
            after: vec!["SET SHOWPLAN_ALL OFF".into()],
            parser: sqlr_core::ExplainParser::MssqlShowplan,
        })
    }

    fn bool_literal(&self, value: bool) -> &'static str {
        if value { "1" } else { "0" }
    }

    /// T-SQL has no `ON CONFLICT`: a `MERGE` over the row's values (it ends in `;`).
    fn upsert_row_sql(
        &self,
        table: &str,
        columns: &[String],
        placeholders: &[String],
        key: &[String],
        update: &[String],
    ) -> Option<String> {
        let names: Vec<String> = columns.iter().map(|c| self.quote_ident(c)).collect();
        let source = format!(
            "(VALUES ({})) sqlr_s ({})",
            placeholders.join(", "),
            names.join(", ")
        );
        Some(format!(
            "{};",
            merge_sql(self, table, &source, columns, key, update)
        ))
    }

    /// `sp_MSforeachtable` reaches every table; `WITH CHECK` makes the re-enabled
    /// constraints trusted again.
    fn constraints_toggle_sql(&self) -> Option<(Vec<String>, Vec<String>)> {
        Some((
            vec!["EXEC sp_MSforeachtable 'ALTER TABLE ? NOCHECK CONSTRAINT ALL'".into()],
            vec!["EXEC sp_MSforeachtable 'ALTER TABLE ? WITH CHECK CHECK CONSTRAINT ALL'".into()],
        ))
    }

    fn ddl_gen(&self) -> Option<&dyn sqlr_core::DdlGen> {
        Some(self)
    }

    fn bytes_literal(&self, bytes: &[u8]) -> String {
        format!("0x{}", &sqlr_core::hex_bytes(bytes)[2..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::{Dialect, NodePath};

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
    fn quotes_brackets_and_qualifies() {
        assert_eq!(MsDialect.quote_ident("a]b"), "[a]]b]");
        let n = node(&["dbo", "tables", "users"], NodeKind::Table);
        assert_eq!(
            MsDialect.select_all(&n, Some(10)).as_deref(),
            Some("SELECT TOP (10) * FROM [dbo].[users]")
        );
        assert_eq!(
            MsDialect.select_all(&n, None).as_deref(),
            Some("SELECT * FROM [dbo].[users]")
        );
        assert_eq!(
            MsDialect.select_all(&node(&["dbo"], NodeKind::Schema), None),
            None
        );
    }

    #[test]
    fn text_literals_are_unicode_and_booleans_are_bits() {
        assert_eq!(MsDialect.string_literal("it's"), "N'it''s'");
        assert_eq!(MsDialect.string_literal("città"), "N'città'");
        assert_eq!(MsDialect.bool_literal(true), "1");
        assert_eq!(MsDialect.bool_literal(false), "0");
    }

    #[test]
    fn an_ordered_query_can_be_wrapped() {
        let d = MsDialect;
        let filtered = d.wrap_where("SELECT * FROM t ORDER BY a;", "b = 1");
        assert_eq!(
            filtered,
            "SELECT * FROM (\nSELECT * FROM t ORDER BY a\nOFFSET 0 ROWS\n) sqlr_q WHERE b = 1"
        );
        let sorted = d.wrap_order("SELECT * FROM t ORDER BY a", &[("b".into(), false)]);
        assert!(sorted.contains("ORDER BY a\nOFFSET 0 ROWS\n)"), "{sorted}");
        // TOP already allows it; an ORDER BY in a nested query or a string is not top-level.
        for sql in [
            "SELECT TOP 5 * FROM t ORDER BY a",
            "SELECT * FROM t ORDER BY a OFFSET 5 ROWS",
            "SELECT * FROM (SELECT TOP 9 * FROM u ORDER BY a) x",
            "SELECT 'order by' FROM t",
            "SELECT * FROM t",
        ] {
            assert!(!d.wrap_where(sql, "1=1").contains("OFFSET 0"), "{sql}");
        }
    }

    #[test]
    fn explain_uses_showplan_for_queries_only() {
        let spec = MsDialect.explain_spec("SELECT * FROM t;", false).unwrap();
        assert_eq!(spec.before, ["SET SHOWPLAN_ALL ON"]);
        assert_eq!(spec.sql, "SELECT * FROM t");
        assert_eq!(spec.after, ["SET SHOWPLAN_ALL OFF"]);
        for (sql, analyze) in [("SELECT 1", true), ("DELETE FROM t", false)] {
            let err = MsDialect.explain_spec(sql, analyze).unwrap_err();
            assert_eq!(err.kind, sqlr_core::ErrorKind::Unsupported, "{sql}");
        }
    }

    #[test]
    fn upsert_is_a_merge() {
        let cols = ["id".to_string(), "v".to_string()];
        let ph = ["@P1".to_string(), "@P2".to_string()];
        assert_eq!(
            MsDialect
                .upsert_row_sql("[dbo].[t]", &cols, &ph, &["id".into()], &["v".into()])
                .as_deref(),
            Some(
                "MERGE INTO [dbo].[t] sqlr_t USING (VALUES (@P1, @P2)) sqlr_s ([id], [v]) \
                 ON (sqlr_t.[id] = sqlr_s.[id]) WHEN MATCHED THEN UPDATE SET \
                 sqlr_t.[v] = sqlr_s.[v] WHEN NOT MATCHED THEN INSERT ([id], [v]) \
                 VALUES (sqlr_s.[id], sqlr_s.[v]);"
            )
        );
    }

    #[test]
    fn constraints_are_toggled_on_every_table() {
        let (off, on) = MsDialect.constraints_toggle_sql().unwrap();
        assert_eq!(
            off,
            ["EXEC sp_MSforeachtable 'ALTER TABLE ? NOCHECK CONSTRAINT ALL'"]
        );
        assert_eq!(
            on,
            ["EXEC sp_MSforeachtable 'ALTER TABLE ? WITH CHECK CHECK CONSTRAINT ALL'"]
        );
    }

    #[test]
    fn placeholders_transactions_and_literals() {
        assert_eq!(MsDialect.placeholder(2), "@P2");
        assert_eq!(MsDialect.begin_sql(), "BEGIN TRANSACTION");
        assert_eq!(MsDialect.savepoint_sql("sp"), "SAVE TRANSACTION [sp]");
        assert_eq!(MsDialect.rollback_to_sql("sp"), "ROLLBACK TRANSACTION [sp]");
        assert_eq!(MsDialect.bytes_literal(&[0, 0xab]), "0x00ab");
        assert_eq!(
            MsDialect.wrap_order("SELECT * FROM t", &[("a".into(), true)]),
            "SELECT * FROM (\nSELECT * FROM t\n) sqlr_q ORDER BY CASE WHEN [a] IS NULL THEN 1 \
             ELSE 0 END, [a] DESC"
        );
    }
}
