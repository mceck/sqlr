//! The breadcrumb above a query editor: `connection › database › tab`, with the commit
//! mode and the run shortcut on the right.

use sqlr_core::CommitMode;
use sqlr_engine::Engine;
use sqlr_ui::theme::icons;
use sqlr_ui::{Crumb, breadcrumb};

use crate::app::{LeftPane, SqlrApp};
use crate::tabs::QueryTab;
use crate::transaction::{mode_name, session_commit_mode};

/// The breadcrumb's segments for `tab`: its connection (`name`), the database it runs on
/// when known, and its title.
pub(crate) fn crumbs(tab: &QueryTab, name: Option<&str>, database: Option<String>) -> Vec<Crumb> {
    let mut crumbs = vec![Crumb {
        label: name.unwrap_or("No connection").to_string(),
        icon: Some(icons::CONNECTION),
    }];
    if let Some(db) = database {
        crumbs.push(Crumb {
            label: db,
            icon: Some(icons::DATABASE),
        });
    }
    crumbs.push(Crumb {
        label: tab.title.clone(),
        icon: None,
    });
    crumbs
}

/// Draws the breadcrumb of a query tab; returns `true` when its connection or database
/// segment was clicked.
pub(crate) fn tab_breadcrumb(
    ui: &mut egui::Ui,
    engine: &Engine,
    tab: &QueryTab,
    name: Option<&str>,
    profile_mode: Option<CommitMode>,
) -> bool {
    let conn = tab.conn.as_deref();
    let database = tab
        .database
        .clone()
        .or_else(|| conn.and_then(|c| engine.session_database(c)));
    let mode = match conn {
        Some(c) => session_commit_mode(engine, &engine.session(c, Some(tab.key)), profile_mode),
        None => CommitMode::default(),
    };
    let run = sqlr_ui::keymap::ui_keymap(ui.ctx())
        .label(sqlr_core::ActionId::RunStatement)
        .unwrap_or_default();
    let right = format!("{} · {run} run", mode_name(mode));
    let crumbs = crumbs(tab, name, database);
    let last = crumbs.len() - 1;
    breadcrumb(ui, &crumbs, &right).is_some_and(|i| i < last && conn.is_some())
}

impl SqlrApp {
    /// The connection or database segment of tab `key`'s breadcrumb was clicked: the
    /// explorer shows and selects the tab's connection.
    pub(crate) fn reveal_tab_connection(&mut self, key: u64) {
        let Some(conn) = self.tab(key).and_then(|t| t.conn.clone()) else {
            return;
        };
        self.left = LeftPane::Connections;
        self.tree.selected = Some((conn, sqlr_core::NodePath::root()));
        self.tree.selected_folder = None;
    }
}
