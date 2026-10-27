//! Apply pending changes (⌘S), Discard pending changes and Refresh (F5, ⌘R) on the active
//! tab. Each resolves what the tab shows and calls the entry point of that surface's own
//! button, so the connection profile's confirmations and every refusal stay the same.

use sqlr_core::StatementClass;
use sqlr_engine::QueryId;
use sqlr_ui::ChangesAction;

use crate::admin::{AdminCommand, AdminTab, UsersDiscard};
use crate::app::{SqlrApp, tab_mut};
use crate::nosql::DocCommand;
use crate::objects::Discard;
use crate::tabs::{SinglePane, TabRun};

/// What the active tab's pending changes are.
enum Target {
    /// A result's changeset: an editor tab's shown result, or an object editor's Data page.
    Result(QueryId),
    /// An object editor's grants (its Permissions page).
    Grants,
    /// An object editor's Schema change list (its other pages).
    Schema,
    /// The Users and roles editor.
    Users,
    Redis,
    Document,
}

impl SqlrApp {
    /// What the pending changes of tab `key` are, by what it shows; `None` for a tab
    /// without any (a DDL viewer, an ER diagram, the Sessions and Server panels).
    fn pending_target(&mut self, key: u64) -> Option<Target> {
        let engine = &self.engine;
        let tab = tab_mut(&mut self.dock, key)?;
        if let Some(object) = tab.object.as_deref() {
            return Some(if object.data_shown() {
                Target::Result(shown_result(tab, engine)?)
            } else if object.permissions_shown() {
                Target::Grants
            } else {
                Target::Schema
            });
        }
        if let Some(admin) = tab.admin.as_deref() {
            return matches!(admin, AdminTab::Users(_)).then_some(Target::Users);
        }
        if tab.redis.is_some() {
            return Some(Target::Redis);
        }
        if tab.doc.is_some() {
            return Some(Target::Document);
        }
        if !tab.is_sql_editor() {
            return None;
        }
        shown_result(tab, engine).map(Target::Result)
    }

    /// Applies the active tab's pending changes (⌘S), as its Apply or Save button would.
    pub(crate) fn apply_active(&mut self) {
        let Some(key) = self.active_key() else {
            return;
        };
        match self.pending_target(key) {
            Some(Target::Result(q)) => self.apply_result(key, q),
            Some(Target::Grants) => {
                if self
                    .tab(key)
                    .and_then(|t| t.object.as_deref())
                    .is_some_and(|o| o.grants_applicable())
                {
                    self.apply_object_grants(key);
                }
            }
            Some(Target::Schema) => self.apply_object_tab(key),
            Some(Target::Users) => {
                let engine = &self.engine;
                let ready = tab_mut(&mut self.dock, key)
                    .and_then(|t| t.admin.as_deref())
                    .is_some_and(|a| match a {
                        AdminTab::Users(u) => {
                            u.editable_now()
                                && crate::admin::actions_blocked(engine, &u.conn, true).is_none()
                        }
                        _ => false,
                    });
                if ready {
                    self.admin_command(key, AdminCommand::ApplyRole);
                }
            }
            Some(Target::Redis) => {
                if let Some(redis) = self.tab(key).and_then(|t| t.redis.as_deref_mut()) {
                    redis.save_edit();
                }
            }
            Some(Target::Document) => {
                if let Some(doc) = self.tab(key).and_then(|t| t.doc.as_deref_mut()) {
                    doc.save_json();
                }
            }
            None => {}
        }
    }

    /// Applies result `q`'s changeset, committing its open cell editor first; an invalid
    /// value keeps the editor open and applies nothing.
    fn apply_result(&mut self, key: u64, q: QueryId) {
        let engine = &self.engine;
        let Some(tab) = tab_mut(&mut self.dock, key) else {
            return;
        };
        if tab.edits.get(&q).is_some_and(|e| e.applying.is_some()) {
            return;
        }
        let Some(grid) = tab.grids.get_mut(&q) else {
            return;
        };
        if grid.editing() {
            let Some(result) = engine.result(q) else {
                return;
            };
            let committed = grid.commit_editor(&result.read().rows);
            if !committed {
                return;
            }
        }
        if grid.edits.as_ref().is_none_or(|e| e.pending() == 0) {
            return;
        }
        self.changes_action(key, q, ChangesAction::Apply);
    }

    /// Drops the active tab's pending changes, as its Discard, Revert or Cancel button
    /// would. It asks nothing.
    pub(crate) fn discard_active(&mut self) {
        let Some(key) = self.active_key() else {
            return;
        };
        let target = self.pending_target(key);
        let Some(tab) = self.tab(key) else {
            return;
        };
        match target {
            Some(Target::Result(q)) => {
                let Some(grid) = tab.grids.get_mut(&q) else {
                    return;
                };
                let pending = grid.edits.as_ref().is_some_and(|e| e.pending() > 0);
                grid.cancel_editor();
                if pending && tab.edits.get(&q).is_none_or(|e| e.applying.is_none()) {
                    self.changes_action(key, q, ChangesAction::Discard);
                }
            }
            Some(Target::Grants) => {
                if let Some(object) = tab.object.as_deref_mut() {
                    object.revert_grants();
                }
            }
            Some(Target::Schema) => {
                if let Some(object) = tab.object.as_deref_mut() {
                    object.revert();
                }
            }
            Some(Target::Users) => {
                if let Some(AdminTab::Users(users)) = tab.admin.as_deref_mut() {
                    users.revert();
                }
            }
            Some(Target::Redis) => {
                if let Some(redis) = tab.redis.as_deref_mut() {
                    redis.discard_edit();
                }
            }
            Some(Target::Document) => {
                if let Some(doc) = tab.doc.as_deref_mut() {
                    doc.cancel_json();
                }
            }
            None => {}
        }
    }

    /// Reloads what the active tab shows (F5, ⌘R), asking first where that would drop
    /// pending edits. It does nothing while the tab's query runs.
    pub(crate) fn refresh_active(&mut self) {
        let Some(key) = self.active_key() else {
            return;
        };
        let engine = &self.engine;
        let Some(tab) = tab_mut(&mut self.dock, key) else {
            return;
        };
        if let Some(object) = tab.object.as_deref() {
            // A new object has nothing to load yet.
            if object.load.is_none() && object.apply.is_none() && !object.is_new() {
                self.object_action(key, sqlr_ui::ObjectEditorAction::Refresh);
            }
            return;
        }
        if let Some(admin) = tab.admin.as_deref_mut() {
            match admin {
                AdminTab::Users(users) if users.pending() > 0 => {
                    self.admin_command(key, AdminCommand::Discard(UsersDiscard::Reload));
                }
                AdminTab::Users(users) => users.perform(engine, UsersDiscard::Reload),
                AdminTab::Sessions(sessions) => sessions.reload(engine),
                AdminTab::Server(server) => server.reload(engine),
            }
            return;
        }
        if let Some(ddl) = tab.ddl.as_deref() {
            if ddl.state != crate::ddl_tab::DdlState::Loading {
                self.ddl_action(key, crate::ddl_tab::DdlAction::Reload);
            }
            return;
        }
        if let (Some(redis), Some(conn)) = (tab.redis.as_deref(), tab.conn.as_deref()) {
            redis.refresh(engine, conn);
            return;
        }
        if let Some(doc) = tab.doc.as_deref() {
            let statements = doc.rerun();
            if doc.json_editing() || doc.is_running(engine) || statements.is_empty() {
                return;
            }
            let conn = doc.conn.clone();
            self.run_doc_command(key, DocCommand::Rerun { conn, statements });
            return;
        }
        if !tab.is_sql_editor() || tab.is_running(engine) {
            return;
        }
        let Some(q) = shown_result(tab, engine) else {
            return;
        };
        // Refresh only reads again: a statement that writes is never repeated by it.
        let syntax = tab.editor.syntax();
        let reads = engine
            .result(q)
            .is_some_and(|r| syntax.classify_script(&r.read().sql) == StatementClass::Read);
        let applying = tab.edits.get(&q).is_some_and(|e| e.applying.is_some());
        if !reads || applying {
            return;
        }
        let pending = tab
            .grids
            .get(&q)
            .and_then(|g| g.edits.as_ref())
            .is_some_and(|e| e.pending() > 0);
        if pending {
            if self.objects.discard.is_none() {
                self.objects.discard = Some((key, Discard::Rerun(q)));
            }
            return;
        }
        self.rerun_result(key, q);
    }

    /// Focuses the tab at `index` in Next tab order; a position past the last does
    /// nothing. `None` focuses the last tab.
    pub(crate) fn go_to_tab(&mut self, index: Option<usize>) {
        let keys: Vec<u64> = self.dock.iter_all_tabs().map(|(_, t)| t.key).collect();
        let key = match index {
            Some(i) => keys.get(i),
            None => keys.last(),
        };
        if let Some(&key) = key {
            self.focus_tab(key);
        }
    }
}

/// The result a tab shows in its Result pane, if any.
fn shown_result(tab: &crate::tabs::QueryTab, engine: &sqlr_engine::Engine) -> Option<QueryId> {
    if matches!(tab.run, Some(TabRun::Single { .. })) && tab.pane != SinglePane::Result {
        return None;
    }
    tab.shown_query(engine)
}
