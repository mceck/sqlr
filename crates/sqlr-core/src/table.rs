//! Table descriptions: columns and the keys that identify a row (spec §7.7).

/// A table or view. `database` is set when the server holds several databases the
/// connection can reach (MySQL databases, SQLite attached files use `schema`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TableRef {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TableColumn {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    /// The default expression as the server prints it.
    pub default: Option<String>,
    /// Computed by the server; never written.
    pub generated: bool,
    /// The server can generate this value when omitted (identity, sequence, rowid).
    #[serde(default)]
    pub automatic: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TableInfo {
    pub table: TableRef,
    pub columns: Vec<TableColumn>,
    /// Primary-key columns in key order; empty when the table has none.
    pub primary_key: Vec<String>,
    /// Unique keys other than the primary key, each in key order.
    pub unique_keys: Vec<Vec<String>>,
}

impl TableInfo {
    pub fn column(&self, name: &str) -> Option<&TableColumn> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// The columns that identify one row: the primary key, else the first unique key whose
    /// columns are all `NOT NULL`. `None` means rows cannot be edited safely.
    pub fn row_key(&self) -> Option<&[String]> {
        if !self.primary_key.is_empty() {
            return Some(&self.primary_key);
        }
        self.unique_keys
            .iter()
            .find(|key| {
                !key.is_empty()
                    && key
                        .iter()
                        .all(|name| self.column(name).is_some_and(|c| !c.nullable))
            })
            .map(Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, nullable: bool) -> TableColumn {
        TableColumn {
            automatic: false,
            name: name.into(),
            type_name: "text".into(),
            nullable,
            default: None,
            generated: false,
        }
    }

    fn info(primary_key: &[&str], unique_keys: &[&[&str]]) -> TableInfo {
        TableInfo {
            table: TableRef {
                database: None,
                schema: Some("public".into()),
                name: "t".into(),
            },
            columns: vec![col("id", false), col("email", true), col("code", false)],
            primary_key: primary_key.iter().map(|s| s.to_string()).collect(),
            unique_keys: unique_keys
                .iter()
                .map(|k| k.iter().map(|s| s.to_string()).collect())
                .collect(),
        }
    }

    #[test]
    fn row_key_prefers_primary_key() {
        let t = info(&["id"], &[&["code"]]);
        assert_eq!(t.row_key(), Some(&["id".to_string()][..]));
    }

    #[test]
    fn row_key_skips_nullable_unique() {
        assert_eq!(info(&[], &[&["email"]]).row_key(), None);
        let t = info(&[], &[&["email"], &["code"]]);
        assert_eq!(t.row_key(), Some(&["code".to_string()][..]));
        assert_eq!(info(&[], &[]).row_key(), None);
        // A key naming a column the table does not have is never usable.
        assert_eq!(info(&[], &[&["missing"]]).row_key(), None);
    }
}
