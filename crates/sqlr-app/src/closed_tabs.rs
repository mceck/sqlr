//! Reopen closed tab (⇧⌘T): what closing a tab remembers to open it again. The list lives
//! in memory for the session only and keeps the most recent [`CAPACITY`] tabs.

use sqlr_core::{Node, NodePath, ObjectRef};
use sqlr_ui::{AdminPanel, ObjectSubTab, ToastKind};

use crate::app::SqlrApp;
use crate::objects::ObjectTab;
use crate::tabs::QueryTab;

/// Closed tabs kept for Reopen closed tab.
pub(crate) const CAPACITY: usize = 20;

/// What reopening a closed tab needs: what identifies it, never its results, pending edits
/// or a secret.
pub(crate) enum ClosedTab {
    /// A SQL editor tab, or a text tab without a connection.
    Editor {
        title: String,
        conn: Option<String>,
        database: Option<String>,
        text: String,
    },
    Object {
        conn: String,
        object: ObjectRef,
        /// The explorer paths its applies reload.
        refresh: Vec<NodePath>,
        page: Option<ObjectSubTab>,
    },
    Ddl {
        conn: String,
        node: Node,
    },
    Redis {
        conn: String,
        db: u32,
        key: Option<Vec<u8>>,
    },
    Doc {
        title: String,
        conn: String,
        database: Option<String>,
        text: String,
    },
    Admin {
        conn: String,
        panel: AdminPanel,
    },
    Er {
        conn: String,
        database: Option<String>,
        schema: String,
        tables: Option<Vec<String>>,
    },
}

impl ClosedTab {
    /// What reopening `tab` needs; `None` for an object editor of an object not created
    /// yet, whose definition exists only as its pending edits.
    pub(crate) fn of(tab: &QueryTab) -> Option<Self> {
        if let (Some(redis), Some(conn)) = (&tab.redis, &tab.conn) {
            return Some(Self::Redis {
                conn: conn.clone(),
                db: redis.db(),
                key: redis.selected().map(<[u8]>::to_vec),
            });
        }
        if let Some(object) = &tab.object {
            return Self::object(object);
        }
        if let Some(ddl) = &tab.ddl {
            return Some(Self::Ddl {
                conn: ddl.conn.clone(),
                node: ddl.node.clone(),
            });
        }
        if let Some(doc) = &tab.doc {
            return Some(Self::Doc {
                title: tab.title.clone(),
                conn: doc.conn.clone(),
                database: tab.database.clone(),
                text: tab.editor.text(),
            });
        }
        if let Some(admin) = &tab.admin {
            return Some(Self::Admin {
                conn: admin.conn().to_string(),
                panel: admin.panel(),
            });
        }
        if let Some(er) = &tab.er {
            return Some(er.closed_tab());
        }
        Some(Self::Editor {
            title: tab.title.clone(),
            conn: tab.conn.clone(),
            database: tab.database.clone(),
            text: tab.editor.text(),
        })
    }

    fn object(object: &ObjectTab) -> Option<Self> {
        if object.is_new() {
            return None;
        }
        Some(Self::Object {
            conn: object.conn.clone(),
            object: object.object.clone(),
            refresh: object.refresh.clone(),
            page: object.page(),
        })
    }

    /// The connection the tab was on.
    fn conn(&self) -> Option<&str> {
        match self {
            Self::Editor { conn, .. } => conn.as_deref(),
            Self::Object { conn, .. }
            | Self::Ddl { conn, .. }
            | Self::Redis { conn, .. }
            | Self::Doc { conn, .. }
            | Self::Admin { conn, .. }
            | Self::Er { conn, .. } => Some(conn),
        }
    }
}

impl SqlrApp {
    /// Remembers tab `key` for Reopen closed tab (as it closes).
    pub(crate) fn remember_closed_tab(&mut self, key: u64) {
        let Some(closed) = self.tab(key).and_then(|t| ClosedTab::of(t)) else {
            return;
        };
        self.closed_tabs.push(closed);
        if self.closed_tabs.len() > CAPACITY {
            self.closed_tabs.remove(0);
        }
    }

    /// Reopens the most recently closed tab through the function that opens that kind of
    /// tab. Nothing runs; a tab whose connection was deleted is dropped with a toast.
    pub(crate) fn reopen_closed_tab(&mut self) {
        let Some(closed) = self.closed_tabs.pop() else {
            return;
        };
        if let Some(conn) = closed.conn()
            && self.connection(conn).is_none()
        {
            self.notify_as(
                ToastKind::Info,
                "The connection of the closed tab no longer exists",
            );
            return;
        }
        match closed {
            ClosedTab::Editor {
                title,
                conn,
                database,
                text,
            } => {
                let key = self.new_tab(Some(title), conn, &text);
                if let Some(tab) = self.tab(key) {
                    tab.database = database;
                }
            }
            ClosedTab::Object {
                conn,
                object,
                refresh,
                page,
            } => self.reopen_object(&conn, object, refresh, page),
            ClosedTab::Ddl { conn, node } => self.open_ddl(&conn, &node),
            ClosedTab::Redis { conn, db, key } => self.open_redis(&conn, db, key),
            ClosedTab::Doc {
                title,
                conn,
                database,
                text,
            } => {
                self.open_doc_tab(title, &conn, database, &text, false);
            }
            ClosedTab::Admin { conn, panel } => self.open_admin(&conn, panel),
            ClosedTab::Er {
                conn,
                database,
                schema,
                tables,
            } => self.open_er_diagram(&conn, database, schema, tables),
        }
    }
}
