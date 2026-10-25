//! The local library in the app: opening `sqlr.db` (or an in-memory stand-in), the history
//! panel, and the snippets panel with its dialogs.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlr_core::{HistoryEntry, Snippet};
use sqlr_engine::{HistoryChannel, LoadState};
use sqlr_store::Db;
use sqlr_ui::{
    DraftResult, HistoryAction, HistoryPanelState, SnippetAction, SnippetDraft, SnippetsState,
    ToastKind, history_panel, snippet_dialog, snippets_panel, suggest_snippet_name, theme,
};

use crate::app::SqlrApp;
use crate::dialogs::{Confirm, DeleteAsk, confirm_delete};

/// Opens `sqlr.db` at `path`. When that fails (corrupt file, unwritable directory, no data
/// directory at all), history and snippets live in memory for the session and the returned
/// notice says so.
pub fn open_db_or_memory(path: Option<&Path>) -> (Arc<Db>, Option<String>) {
    let error = match path {
        None => "no data directory for this user".to_string(),
        Some(path) => match Db::open(path) {
            Ok(db) => return (Arc::new(db), None),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "opening sqlr.db failed");
                e.to_string()
            }
        },
    };
    let db = Db::open_in_memory().expect("open an in-memory database");
    let notice = format!("History and snippets will not be saved: {error}");
    (Arc::new(db), Some(notice))
}

/// The history and snippets panels' state.
#[derive(Default)]
pub struct LibraryUi {
    history: HistoryPanelState,
    /// The panel's last loaded entries, kept on screen while a newer search runs.
    history_entries: Option<Arc<Vec<HistoryEntry>>>,
    history_error: Option<String>,
    snippets: SnippetsState,
    /// The snippet being created or edited.
    draft: Option<SnippetDraft>,
    /// The snippet id awaiting delete confirmation.
    delete_pending: Option<String>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The snippet a saved draft becomes: a new id when it has none, a trimmed name, and a folder
/// without empty or padded segments.
fn snippet_from(draft: SnippetDraft) -> Snippet {
    let folder: Vec<&str> = draft
        .folder
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    Snippet {
        id: draft.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: draft.name.trim().to_string(),
        folder: folder.join("/"),
        conn_id: draft.conn_id,
        sql: draft.sql,
        updated_at_ms: now_ms(),
    }
}

impl SqlrApp {
    pub(crate) fn history_ui(&mut self, ui: &mut egui::Ui) {
        theme::section_title(ui, "Query history");
        theme::hairline(ui);
        ui.add_space(theme::Spacing::S);
        if let Some(view) = self.engine.history(HistoryChannel::Panel) {
            match view.entries {
                LoadState::Ready(entries) => {
                    self.library.history_entries = Some(entries);
                    self.library.history_error = None;
                }
                LoadState::Failed(e) => self.library.history_error = Some(e.message),
                LoadState::Loading => {}
            }
        }
        if let Some(error) = &self.library.history_error {
            ui.colored_label(theme::palette(ui.ctx()).error, error);
        }
        let conns = self.conn_names();
        let entries = self.library.history_entries.clone();
        let actions = history_panel(
            ui,
            &mut self.library.history,
            &conns,
            entries.as_deref().map(Vec::as_slice),
        );
        let ctx = ui.ctx().clone();
        for action in actions {
            match action {
                HistoryAction::Search(q) => self.engine.search_history(HistoryChannel::Panel, q),
                HistoryAction::Open(e) => self.open_history_entry(&e),
                HistoryAction::Insert(e) => self.insert_into_active(&e.sql),
                HistoryAction::Copy(e) => ctx.copy_text(e.sql),
                HistoryAction::Clear { conn_id } => self.clear_history_pending = Some(conn_id),
            }
        }
    }

    /// A new tab with the entry's text on its connection (if still saved) and on the server
    /// database it ran on, not run: a document or Redis tab for a NoSQL connection. A
    /// connection that is no longer saved leaves the tab unbound, with a toast.
    pub(crate) fn open_history_entry(&mut self, e: &HistoryEntry) {
        let conn = self.connection(&e.conn_id).map(|c| c.id.clone());
        if conn.is_none() {
            self.notify_as(
                ToastKind::Info,
                format!(
                    "Connection {} is no longer saved: the query opened without one",
                    e.conn_id
                ),
            );
        }
        let before = self.next_tab;
        self.open_text(None, conn.clone(), &e.sql);
        let opened = self.next_tab > before;
        if conn.is_some()
            && opened
            && let Some(tab) = self.tab(self.next_tab)
            && tab.doc.is_none()
            && tab.redis.is_none()
        {
            tab.database = e.database.clone();
        }
    }

    /// Inserts `text` at the active editor's carets.
    pub(crate) fn insert_into_active(&mut self, text: &str) {
        if let Some(tab) = self.active_tab() {
            tab.editor.insert_text(text);
            tab.editor.request_focus();
        }
    }

    pub(crate) fn snippets_ui(&mut self, ui: &mut egui::Ui) {
        let snippets = self.engine.snippets();
        let active_conn = self.active_tab().and_then(|t| t.conn.clone());
        let actions = snippets_panel(
            ui,
            &mut self.library.snippets,
            &snippets,
            active_conn.as_deref(),
        );
        for action in actions {
            match action {
                SnippetAction::Insert(sql) => self.insert_into_active(&sql),
                SnippetAction::OpenInNewTab(s) => {
                    let conn = s
                        .conn_id
                        .as_deref()
                        .and_then(|id| self.connection(id))
                        .map(|c| c.id.clone());
                    self.open_text(Some(s.name), conn, &s.sql);
                }
                SnippetAction::Edit(draft) => self.library.draft = Some(draft),
                SnippetAction::Delete(id) => self.library.delete_pending = Some(id),
            }
        }
    }

    /// Opens the snippet dialog on the active editor's selection, or its whole text.
    pub(crate) fn save_as_snippet(&mut self) {
        let Some(key) = self.active_key() else {
            return;
        };
        self.save_as_snippet_for(key);
    }

    /// [`save_as_snippet`](Self::save_as_snippet) for one tab, whose editor asked for it.
    /// Nothing happens when the editor is empty.
    pub(crate) fn save_as_snippet_for(&mut self, key: u64) {
        let Some(tab) = self.tab(key) else {
            return;
        };
        let selected = tab.editor.model().selected_text();
        let sql = if selected.trim().is_empty() {
            tab.editor.text()
        } else {
            selected
        };
        if sql.trim().is_empty() {
            return;
        }
        self.library.draft = Some(SnippetDraft {
            name: suggest_snippet_name(&sql),
            sql,
            ..SnippetDraft::default()
        });
    }

    pub(crate) fn library_dialogs(&mut self, ctx: &egui::Context) {
        let conns = self.conn_names();
        if let Some(draft) = &mut self.library.draft {
            match snippet_dialog(ctx, draft, &conns) {
                DraftResult::Open => {}
                DraftResult::Cancel => self.library.draft = None,
                DraftResult::Save => {
                    if let Some(draft) = self.library.draft.take() {
                        self.engine.save_snippet(snippet_from(draft));
                    }
                }
            }
        }
        if let Some(id) = self.library.delete_pending.clone() {
            let snippets = self.engine.snippets();
            let name = snippets
                .iter()
                .find(|s| s.id == id)
                .map_or(id.as_str(), |s| s.name.as_str());
            let prompt = format!("Delete the snippet \"{name}\"?");
            let ask = DeleteAsk {
                title: "Delete snippet",
                action: "Delete",
                prompt: &prompt,
                detail: None,
            };
            match confirm_delete(ctx, ask) {
                Confirm::Pending => {}
                Confirm::No => self.library.delete_pending = None,
                Confirm::Yes => {
                    self.library.delete_pending = None;
                    self.engine.delete_snippet(&id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlr_core::HistoryOutcome;

    use super::*;

    fn entry() -> HistoryEntry {
        HistoryEntry {
            id: 0,
            conn_id: "c1".into(),
            database: None,
            sql: "select 1".into(),
            started_at_ms: 1,
            duration_ms: 2,
            outcome: HistoryOutcome::Rows(1),
        }
    }

    #[test]
    fn db_falls_back_to_memory_with_notice() {
        let dir = tempfile::tempdir().unwrap();
        let garbage = dir.path().join("garbage.db");
        std::fs::write(&garbage, vec![0xABu8; 4096]).unwrap();
        let (db, notice) = open_db_or_memory(Some(&garbage));
        db.add_history(&entry()).unwrap();
        let notice = notice.expect("a notice");
        assert!(notice.contains("will not be saved"), "{notice}");

        let (db, notice) = open_db_or_memory(Some(&dir.path().join("sqlr.db")));
        db.add_history(&entry()).unwrap();
        assert_eq!(notice, None);

        let (db, notice) = open_db_or_memory(None);
        db.add_history(&entry()).unwrap();
        assert!(notice.unwrap().contains("will not be saved"));
    }

    #[test]
    fn unwritable_directory_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the data directory should be: creating it fails with an io error.
        let blocker = dir.path().join("data");
        std::fs::write(&blocker, b"x").unwrap();
        let (db, notice) = open_db_or_memory(Some(&blocker.join("sqlr.db")));
        db.add_history(&entry()).unwrap();
        assert!(notice.unwrap().contains("will not be saved"));
    }

    #[test]
    fn saved_draft_gets_an_id_and_a_clean_folder() {
        let s = snippet_from(SnippetDraft {
            id: None,
            name: " mine ".into(),
            folder: "/a/ b //".into(),
            conn_id: Some("c1".into()),
            sql: "select 1".into(),
        });
        assert!(!s.id.is_empty());
        assert_eq!(s.name, "mine");
        assert_eq!(s.folder, "a/b");
        assert!(s.updated_at_ms > 0);
        let kept = snippet_from(SnippetDraft {
            id: Some("x".into()),
            name: "n".into(),
            ..SnippetDraft::default()
        });
        assert_eq!(kept.id, "x");
    }
}
