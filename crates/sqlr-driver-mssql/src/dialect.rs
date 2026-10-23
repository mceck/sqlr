//! T-SQL dialect rules for the editor, grids and generated statements.

use std::sync::Arc;

use sqlr_core::{Dialect, Node, NodeKind, SqlSyntax, wrap_subquery};

use crate::syntax::tsql_syntax;

pub struct MsDialect;

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
        wrap_subquery(&self.syntax(), sql, &keys)
    }

    /// `N'…'` keeps characters outside the database's code page.
    fn string_literal(&self, text: &str) -> String {
        format!("N'{}'", text.replace('\'', "''"))
    }

    fn bool_literal(&self, value: bool) -> &'static str {
        if value { "1" } else { "0" }
    }

    /// `sp_MSforeachtable` reaches every table; `WITH CHECK` makes the re-enabled
    /// constraints trusted again.
    fn constraints_toggle_sql(&self) -> Option<(Vec<String>, Vec<String>)> {
        Some((
            vec!["EXEC sp_MSforeachtable 'ALTER TABLE ? NOCHECK CONSTRAINT ALL'".into()],
            vec!["EXEC sp_MSforeachtable 'ALTER TABLE ? WITH CHECK CHECK CONSTRAINT ALL'".into()],
        ))
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
