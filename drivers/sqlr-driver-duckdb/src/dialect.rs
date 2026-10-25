use sqlr_driver_sdk::{Dialect, Node};

pub struct DuckdbDialect;

impl Dialect for DuckdbDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }
    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String> {
        if !node.kind.opens_data() {
            return None;
        }
        let [schema, _, name] = node.path.segments() else {
            return None;
        };
        let mut sql = format!(
            "SELECT * FROM {}.{}",
            self.quote_ident(schema),
            self.quote_ident(name)
        );
        if let Some(limit) = limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        Some(sql)
    }
    fn default_schemas(&self) -> Vec<String> {
        vec!["main".into()]
    }
    fn error_aborts_transaction(&self) -> bool {
        true
    }
}
