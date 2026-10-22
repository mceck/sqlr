//! HANA SQL: double-quoted identifiers, `?` placeholders, `LIMIT n`.

use sqlr_driver_sdk::{Dialect, Node};

/// `ident` as a quoted identifier.
pub(crate) fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

pub(crate) struct HanaDialect;

impl Dialect for HanaDialect {
    fn quote_ident(&self, ident: &str) -> String {
        quote(ident)
    }

    /// Relation nodes are `[schema, folder, name]`.
    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String> {
        if !node.kind.opens_data() {
            return None;
        }
        let segments = node.path.segments();
        let (schema, name) = (segments.first()?, segments.get(2)?);
        let mut sql = format!("SELECT * FROM {}.{}", quote(schema), quote(name));
        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        Some(sql)
    }

    fn default_schemas(&self) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_driver_sdk::{NodeKind, NodePath};

    fn node(kind: NodeKind, path: &[&str]) -> Node {
        Node {
            path: NodePath(path.iter().map(|s| s.to_string()).collect()),
            name: path.last().unwrap().to_string(),
            kind,
            has_children: false,
            detail: None,
        }
    }

    #[test]
    fn identifiers_and_select_all() {
        let d = HanaDialect;
        assert_eq!(d.quote_ident("a\"b"), "\"a\"\"b\"");
        let table = node(NodeKind::Table, &["MY SCHEMA", "tables", "T\"1"]);
        assert_eq!(
            d.select_all(&table, Some(10)).as_deref(),
            Some("SELECT * FROM \"MY SCHEMA\".\"T\"\"1\" LIMIT 10")
        );
        assert_eq!(
            d.select_all(&node(NodeKind::Folder, &["S", "tables"]), None),
            None
        );
    }

    #[test]
    fn dialect_describes_itself_as_data() {
        // The sidecar sends this over the wire; the client must rebuild the same SQL.
        let spec = sqlr_driver_sdk::sidecar_dialect_spec(&HanaDialect);
        assert_eq!(spec.select_segments, [0, 2]);
        assert_eq!(spec.limit, sqlr_driver_sdk::LimitStyle::Limit);
    }
}
