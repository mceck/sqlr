//! SQLite syntax data: ANSI plus SQLite's keywords, `[ident]` and `` `ident` `` quoting.

use std::sync::{Arc, LazyLock};

use sqlr_core::SqlSyntax;

/// SQLite keywords (https://sqlite.org/lang_keywords.html), uppercase.
pub const SQLITE_KEYWORDS: &[&str] = &[
    "ABORT",
    "ACTION",
    "ADD",
    "AFTER",
    "ALL",
    "ALTER",
    "ALWAYS",
    "ANALYZE",
    "AND",
    "AS",
    "ASC",
    "ATTACH",
    "AUTOINCREMENT",
    "BEFORE",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASCADE",
    "CASE",
    "CAST",
    "CHECK",
    "COLLATE",
    "COLUMN",
    "COMMIT",
    "CONFLICT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "CURRENT",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "DATABASE",
    "DEFAULT",
    "DEFERRABLE",
    "DEFERRED",
    "DELETE",
    "DESC",
    "DETACH",
    "DISTINCT",
    "DO",
    "DROP",
    "EACH",
    "ELSE",
    "END",
    "ESCAPE",
    "EXCEPT",
    "EXCLUDE",
    "EXCLUSIVE",
    "EXISTS",
    "EXPLAIN",
    "FAIL",
    "FILTER",
    "FIRST",
    "FOLLOWING",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "GENERATED",
    "GLOB",
    "GROUP",
    "GROUPS",
    "HAVING",
    "IF",
    "IGNORE",
    "IMMEDIATE",
    "IN",
    "INDEX",
    "INDEXED",
    "INITIALLY",
    "INNER",
    "INSERT",
    "INSTEAD",
    "INTERSECT",
    "INTO",
    "IS",
    "ISNULL",
    "JOIN",
    "KEY",
    "LAST",
    "LEFT",
    "LIKE",
    "LIMIT",
    "MATCH",
    "MATERIALIZED",
    "NATURAL",
    "NO",
    "NOT",
    "NOTHING",
    "NOTNULL",
    "NULL",
    "NULLS",
    "OF",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OTHERS",
    "OUTER",
    "OVER",
    "PARTITION",
    "PLAN",
    "PRAGMA",
    "PRECEDING",
    "PRIMARY",
    "QUERY",
    "RAISE",
    "RANGE",
    "RECURSIVE",
    "REFERENCES",
    "REGEXP",
    "REINDEX",
    "RELEASE",
    "RENAME",
    "REPLACE",
    "RESTRICT",
    "RETURNING",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWS",
    "SAVEPOINT",
    "SELECT",
    "SET",
    "TABLE",
    "TEMP",
    "TEMPORARY",
    "THEN",
    "TIES",
    "TO",
    "TRANSACTION",
    "TRIGGER",
    "UNBOUNDED",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VACUUM",
    "VALUES",
    "VIEW",
    "VIRTUAL",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
    "WITHOUT",
    // Not reserved by SQLite but common in its DDL.
    "FALSE",
    "ROWID",
    "STRICT",
    "TRUE",
];

const SQLITE_FUNCTIONS: &[&str] = &[
    "abs",
    "avg",
    "changes",
    "char",
    "coalesce",
    "count",
    "date",
    "datetime",
    "glob",
    "group_concat",
    "hex",
    "ifnull",
    "iif",
    "instr",
    "json",
    "json_array",
    "json_extract",
    "json_group_array",
    "json_object",
    "julianday",
    "last_insert_rowid",
    "length",
    "like",
    "lower",
    "ltrim",
    "max",
    "min",
    "nullif",
    "printf",
    "quote",
    "random",
    "randomblob",
    "replace",
    "round",
    "rtrim",
    "sqlite_version",
    "strftime",
    "substr",
    "sum",
    "time",
    "total",
    "total_changes",
    "trim",
    "typeof",
    "unicode",
    "unixepoch",
    "upper",
    "zeroblob",
];

static SQLITE: LazyLock<Arc<SqlSyntax>> = LazyLock::new(|| {
    let sorted = |list: &[&'static str]| {
        let mut v = list.to_vec();
        v.sort_unstable();
        v.dedup();
        v
    };
    let ansi = SqlSyntax::ansi();
    let mut keywords = ansi.keywords.clone();
    keywords.extend_from_slice(SQLITE_KEYWORDS);
    Arc::new(SqlSyntax {
        keywords: sorted(&keywords),
        reserved: sorted(SQLITE_KEYWORDS),
        functions: sorted(SQLITE_FUNCTIONS),
        ident_quotes: &[(b'"', b'"'), (b'[', b']'), (b'`', b'`')],
        ..(*ansi).clone()
    })
});

/// The shared SQLite syntax.
pub fn sqlite_syntax() -> Arc<SqlSyntax> {
    SQLITE.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::{LexState, StatementClass};

    #[test]
    fn sqlite_syntax_rules() {
        let s = sqlite_syntax();
        assert!(s.is_keyword("pragma") && s.is_keyword("VACUUM") && s.is_keyword("attach"));
        assert!(s.is_function("json_extract"));
        assert!(!s.dollar_quotes);
        assert_eq!(s.split("select [a;b]; select 2", LexState::Normal).len(), 2);
        assert_eq!(s.classify("pragma table_info(t)"), StatementClass::Read);
        assert_eq!(s.quote_if_needed("Order"), "\"Order\"");
    }
}
