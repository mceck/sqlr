//! The explorer actions: "Select 100 rows", "Copy name" / "Copy qualified
//! name", "Copy connection URL", "New editor tab" and the failed connection's Retry.

use sqlr_core::{Node, NodePath, connection_url};
use sqlr_ui::ToastKind;

use crate::app::SqlrApp;

impl SqlrApp {
    /// Opens the first `limit` rows of a table or view and runs the query.
    pub(crate) fn select_rows(&mut self, conn: &str, node: &Node, limit: usize) {
        if self.is_nosql(conn) {
            // Key-value and document data open in their own tabs.
            self.open_data(conn, node);
            return;
        }
        let Some((database, sql)) = self.engine.open_data_target(conn, node, Some(limit)) else {
            self.notify(format!("Cannot open the data of {}", node.name));
            return;
        };
        let title = match &database {
            Some(db) => format!("{} ({db})", node.name),
            None => node.name.clone(),
        };
        self.open_and_run(title, conn, database, &sql);
    }

    /// Copies the node's name, or its schema-qualified name with each part quoted by the
    /// connection's dialect.
    pub(crate) fn copy_node_name(
        &mut self,
        ctx: &egui::Context,
        conn: &str,
        node: &Node,
        qualified: bool,
    ) {
        let text = if qualified {
            let (schema, _) = self.location(conn, Some(node));
            match self.dialects.get(conn) {
                Some(d) => match schema {
                    Some(s) => format!("{}.{}", d.quote_ident(&s), d.quote_ident(&node.name)),
                    None => d.quote_ident(&node.name),
                },
                None => match schema {
                    Some(s) => format!("{s}.{}", node.name),
                    None => node.name.clone(),
                },
            }
        } else {
            node.name.clone()
        };
        ctx.copy_text(text.clone());
        self.notify_as(ToastKind::Success, format!("Copied {text}"));
    }

    /// Copies the connection's URL; it never holds a secret.
    pub(crate) fn copy_connection_url(&mut self, ctx: &egui::Context, conn: &str) {
        let Some(cfg) = self.connection(conn).cloned() else {
            return;
        };
        let url = self
            .manifest(&cfg.driver)
            .and_then(|m| connection_url(&cfg, &m));
        match url {
            Some(url) => {
                ctx.copy_text(url);
                self.notify_as(ToastKind::Success, "Connection URL copied");
            }
            None => self.notify_as(
                ToastKind::Info,
                format!("{} has no connection URL", cfg.name),
            ),
        }
    }

    /// A new editor tab bound to `conn` (an info toast for connections that run no SQL).
    pub(crate) fn new_editor_tab_on(&mut self, conn: &str) {
        if self.is_nosql(conn) {
            let name = self.connection(conn).map_or(conn, |c| c.name.as_str());
            let text = format!(
                "{name} does not run SQL: open its databases or collections from the explorer"
            );
            self.notify_as(ToastKind::Info, text);
            return;
        }
        self.new_tab(None, Some(conn.to_string()), "");
    }

    /// Connects a failed connection again and shows its tree.
    pub(crate) fn retry_connection(&mut self, conn: &str) {
        self.tree.set_expanded(conn, &NodePath::root(), true);
        if self.reconnect(conn) {
            self.engine.browse(conn, NodePath::root());
        }
    }
}
