//! Oracle SQL dialect rules for the editor, grids and generated statements.

use std::sync::Arc;

use sqlr_core::{Dialect, Node, SqlSyntax, hex_bytes, merge_sql};

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

    /// `BEGIN … END` is an anonymous PL/SQL block, not the start of a transaction.
    fn begin_is_plsql(&self) -> bool {
        true
    }

    fn ddl_commits_transaction(&self) -> bool {
        true
    }

    /// Oracle has no `RELEASE SAVEPOINT`: a savepoint ends with its transaction.
    fn release_savepoint_sql(&self, _name: &str) -> Option<String> {
        None
    }

    /// Oracle has no `ON CONFLICT`: a `MERGE` over a one-row `SELECT … FROM dual`.
    fn upsert_row_sql(
        &self,
        table: &str,
        columns: &[String],
        placeholders: &[String],
        key: &[String],
        update: &[String],
    ) -> Option<String> {
        let selected: Vec<String> = placeholders
            .iter()
            .zip(columns)
            .map(|(p, c)| format!("{p} {}", self.quote_ident(c)))
            .collect();
        let source = format!("(SELECT {} FROM dual) sqlr_s", selected.join(", "));
        Some(merge_sql(self, table, &source, columns, key, update))
    }

    /// `EXPLAIN PLAN` fills the session's plan table; `DBMS_XPLAN` prints it. Oracle's
    /// estimates only: running the statement for actual figures is not offered.
    fn explain_spec(
        &self,
        sql: &str,
        analyze: bool,
    ) -> Result<sqlr_core::ExplainSpec, sqlr_core::DriverError> {
        if analyze {
            return Err(sqlr_core::DriverError::new(
                sqlr_core::ErrorKind::Unsupported,
                "Oracle has no EXPLAIN ANALYZE here: use Explain for the estimated plan",
            ));
        }
        let sql = sql.trim().trim_end_matches(';').trim_end();
        Ok(sqlr_core::ExplainSpec {
            before: vec![format!("EXPLAIN PLAN SET STATEMENT_ID = 'sqlr' FOR {sql}")],
            sql: "SELECT PLAN_TABLE_OUTPUT FROM TABLE(DBMS_XPLAN.DISPLAY(NULL, 'sqlr', 'TYPICAL'))"
                .into(),
            after: vec!["DELETE FROM PLAN_TABLE WHERE STATEMENT_ID = 'sqlr'".into()],
            parser: sqlr_core::ExplainParser::OracleText,
        })
    }

    fn ddl_gen(&self) -> Option<&dyn sqlr_core::DdlGen> {
        Some(self)
    }

    fn bool_literal(&self, value: bool) -> &'static str {
        if value { "1" } else { "0" }
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
    fn upsert_is_a_merge_without_a_trailing_semicolon() {
        let cols = ["ID".to_string(), "V".to_string()];
        let ph = [":1".to_string(), ":2".to_string()];
        assert_eq!(
            OracleDialect
                .upsert_row_sql("\"S\".\"T\"", &cols, &ph, &["ID".into()], &[])
                .as_deref(),
            Some(
                "MERGE INTO \"S\".\"T\" sqlr_t USING (SELECT :1 \"ID\", :2 \"V\" FROM dual) sqlr_s \
                 ON (sqlr_t.\"ID\" = sqlr_s.\"ID\") WHEN NOT MATCHED THEN INSERT (\"ID\", \"V\") \
                 VALUES (sqlr_s.\"ID\", sqlr_s.\"V\")"
            )
        );
    }

    #[test]
    fn explain_fills_the_plan_table_and_displays_it() {
        let spec = OracleDialect
            .explain_spec("SELECT * FROM emp;", false)
            .unwrap();
        assert_eq!(
            spec.before,
            ["EXPLAIN PLAN SET STATEMENT_ID = 'sqlr' FOR SELECT * FROM emp"]
        );
        assert!(spec.sql.contains("DBMS_XPLAN.DISPLAY(NULL, 'sqlr'"));
        assert_eq!(spec.parser, sqlr_core::ExplainParser::OracleText);
        let err = OracleDialect
            .explain_spec("SELECT 1 FROM dual", true)
            .unwrap_err();
        assert_eq!(err.kind, sqlr_core::ErrorKind::Unsupported);
    }

    #[test]
    fn savepoints_are_rolled_back_to_but_never_released() {
        assert_eq!(
            OracleDialect.rollback_to_sql("sp"),
            "ROLLBACK TO SAVEPOINT \"sp\""
        );
        assert_eq!(OracleDialect.release_savepoint_sql("sp"), None);
    }

    #[test]
    fn begin_is_a_plsql_block_not_a_transaction() {
        assert!(OracleDialect.begin_is_plsql());
        assert_eq!(OracleDialect.begin_sql(), "SET TRANSACTION READ WRITE");
    }

    #[test]
    fn booleans_are_numbers() {
        // Oracle before 23ai has no TRUE/FALSE literals.
        assert_eq!(OracleDialect.bool_literal(true), "1");
        assert_eq!(OracleDialect.bool_literal(false), "0");
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
