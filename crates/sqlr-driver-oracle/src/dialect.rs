//! Oracle SQL dialect rules for the editor, grids and generated statements.

use std::sync::Arc;

use sqlr_core::{Dialect, Node, SqlSyntax, hex_bytes};

use crate::syntax::oracle_syntax;

pub struct OracleDialect;

impl Dialect for OracleDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String> {
        if !node.kind.opens_data() {
            return None;
        }
        let (schema, name) = match node.path.segments() {
            [schema, _folder, name] => (schema.as_str(), name.as_str()),
            _ => return None,
        };
        let mut sql = format!(
            "SELECT * FROM {}.{}",
            self.quote_ident(schema),
            self.quote_ident(name)
        );
        if let Some(n) = limit {
            sql.push_str(&format!(" FETCH FIRST {n} ROWS ONLY"));
        }
        Some(sql)
    }

    fn syntax(&self) -> Arc<SqlSyntax> {
        oracle_syntax()
    }

    fn placeholder(&self, index: usize) -> String {
        format!(":{index}")
    }

    /// Oracle starts a transaction with `SET TRANSACTION`; DML also starts one implicitly.
    fn begin_sql(&self) -> &'static str {
        "SET TRANSACTION READ WRITE"
    }

    fn ddl_commits_transaction(&self) -> bool {
        true
    }

    /// `HEXTORAW('…')` is a RAW literal whatever the session's NLS settings are.
    fn bytes_literal(&self, bytes: &[u8]) -> String {
        format!("HEXTORAW('{}')", &hex_bytes(bytes)[2..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::{Dialect, NodeKind, NodePath};

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
    fn quotes_qualifies_and_limits() {
        assert_eq!(OracleDialect.quote_ident("a\"b"), "\"a\"\"b\"");
        let n = node(&["SCOTT", "tables", "EMP"], NodeKind::Table);
        assert_eq!(
            OracleDialect.select_all(&n, Some(10)).as_deref(),
            Some("SELECT * FROM \"SCOTT\".\"EMP\" FETCH FIRST 10 ROWS ONLY")
        );
        assert_eq!(
            OracleDialect.select_all(&n, None).as_deref(),
            Some("SELECT * FROM \"SCOTT\".\"EMP\"")
        );
    }

    #[test]
    fn placeholders_and_literals() {
        assert_eq!(OracleDialect.placeholder(2), ":2");
        assert_eq!(OracleDialect.begin_sql(), "SET TRANSACTION READ WRITE");
        assert!(OracleDialect.ddl_commits_transaction());
        assert_eq!(OracleDialect.bytes_literal(&[0, 0xab]), "HEXTORAW('00ab')");
        assert_eq!(OracleDialect.string_literal("it's"), "'it''s'");
    }
}
