//! NoSQL tabs. Redis opens a key browser with a value view and a console; the document
//! databases (MongoDB, Elasticsearch) open their own tab kinds beside it.

mod doc_run;
mod doc_tab;
pub(crate) mod index_admin;
mod redis_tab;
mod reopen;

use sqlr_core::{DocTarget, DocValue, KvEdit, NodePath};

pub(crate) use doc_tab::{DocTab, doc_editor, doc_ui};
pub(crate) use redis_tab::{RedisTab, redis_ui};

/// A change or run a document tab (or an index dialog) asks the app to make: the app asks
/// the connection profile whether to confirm it first, then runs it through the engine.
///
/// `Debug` names the command and its connection, never statements or documents.
#[derive(Clone)]
pub(crate) enum DocCommand {
    /// Statements typed in the tab (⌘↵, ⇧⌘↵).
    Run {
        conn: String,
        statements: Vec<String>,
    },
    /// The last run read again after an edit (no confirmation: it already ran).
    Rerun {
        conn: String,
        statements: Vec<String>,
    },
    /// Replace the document `id` (insert when `None`).
    Save {
        conn: String,
        target: DocTarget,
        id: Option<DocValue>,
        doc: DocValue,
    },
    /// Delete documents, grouped by target (already confirmed by the tab).
    Delete {
        conn: String,
        groups: Vec<(DocTarget, Vec<DocValue>)>,
    },
    CreateIndex {
        conn: String,
        database: Option<String>,
        text: String,
        /// The explorer path to reload once done.
        refresh: NodePath,
    },
    /// Already confirmed by its dialog.
    DropIndex {
        conn: String,
        database: Option<String>,
        text: String,
        refresh: NodePath,
    },
}

impl DocCommand {
    pub(crate) fn conn(&self) -> &str {
        match self {
            DocCommand::Run { conn, .. }
            | DocCommand::Rerun { conn, .. }
            | DocCommand::Save { conn, .. }
            | DocCommand::Delete { conn, .. }
            | DocCommand::CreateIndex { conn, .. }
            | DocCommand::DropIndex { conn, .. } => conn,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            DocCommand::Run { .. } => "Run",
            DocCommand::Rerun { .. } => "Rerun",
            DocCommand::Save { .. } => "Save",
            DocCommand::Delete { .. } => "Delete",
            DocCommand::CreateIndex { .. } => "CreateIndex",
            DocCommand::DropIndex { .. } => "DropIndex",
        }
    }
}

impl std::fmt::Debug for DocCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(self.name())
            .field("conn", &self.conn())
            .finish_non_exhaustive()
    }
}

/// A change a Redis tab asks the app to make: the app asks the connection profile whether to
/// confirm it first, then runs it through the engine.
///
/// `Debug` shows what is being done to which key, never the values or the raw console line
/// (it may hold a password).
#[derive(Clone)]
pub(crate) enum RedisCommand {
    /// An edit of one key (`KvEdit::Delete` included).
    Edit {
        conn: String,
        db: u32,
        key: Vec<u8>,
        edit: KvEdit,
    },
    /// The keys picked in the key browser.
    Delete {
        conn: String,
        db: u32,
        keys: Vec<Vec<u8>>,
    },
    /// A console line typed in tab `tab`, which is on database `db`.
    Console {
        conn: String,
        tab: u64,
        db: u32,
        line: String,
    },
}

impl std::fmt::Debug for RedisCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RedisCommand::Edit { conn, db, key, .. } => f
                .debug_struct("Edit")
                .field("conn", conn)
                .field("db", db)
                .field("key", &sqlr_core::display_key(key))
                .finish_non_exhaustive(),
            RedisCommand::Delete { conn, db, keys } => f
                .debug_struct("Delete")
                .field("conn", conn)
                .field("db", db)
                .field("keys", &keys.len())
                .finish(),
            RedisCommand::Console {
                conn,
                tab,
                db,
                line,
            } => f
                .debug_struct("Console")
                .field("conn", conn)
                .field("tab", tab)
                .field("db", db)
                .field("line", &format_args!("<{} bytes>", line.len()))
                .finish(),
        }
    }
}

impl RedisCommand {
    pub(crate) fn conn(&self) -> &str {
        match self {
            RedisCommand::Edit { conn, .. }
            | RedisCommand::Delete { conn, .. }
            | RedisCommand::Console { conn, .. } => conn,
        }
    }

    /// The database the command runs on.
    pub(crate) fn db(&self) -> u32 {
        match self {
            RedisCommand::Edit { db, .. }
            | RedisCommand::Delete { db, .. }
            | RedisCommand::Console { db, .. } => *db,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_commands_debug_without_statements_or_documents() {
        let run = DocCommand::Run {
            conn: "m".into(),
            statements: vec!["db.runCommand({createUser: 'a', pwd: 'hunter2'})".into()],
        };
        let save = DocCommand::Save {
            conn: "m".into(),
            target: DocTarget {
                database: None,
                collection: "c".into(),
            },
            id: None,
            doc: DocValue::String("hunter2".into()),
        };
        for command in [run, save] {
            let text = format!("{:?}", crate::confirm::PendingKind::Doc(command));
            assert!(!text.contains("hunter2"), "{text}");
        }
    }

    #[test]
    fn debug_output_never_holds_a_console_line_or_a_value() {
        let console = RedisCommand::Console {
            conn: "c".into(),
            tab: 1,
            db: 0,
            line: "AUTH hunter2".into(),
        };
        let edit = RedisCommand::Edit {
            conn: "c".into(),
            db: 0,
            key: b"k".to_vec(),
            edit: KvEdit::SetString(b"hunter2".to_vec()),
        };
        for command in [console, edit] {
            let text = format!("{command:?}");
            assert!(!text.contains("hunter2"), "{text}");
        }
        let run = crate::confirm::PendingKind::Redis(RedisCommand::Console {
            conn: "c".into(),
            tab: 1,
            db: 0,
            line: "AUTH hunter2".into(),
        });
        assert!(!format!("{run:?}").contains("hunter2"));
    }
}
