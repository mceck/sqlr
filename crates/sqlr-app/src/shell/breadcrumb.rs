//! The breadcrumb above a query editor: `connection › database › tab`, with the commit
//! mode and the run shortcut on the right.

use sqlr_core::{CommitMode, NodeKind, NodePath};
use sqlr_engine::{Engine, LoadState, TxPhase};
use sqlr_ui::theme::icons;
use sqlr_ui::{Crumb, breadcrumb_with_response};

use crate::app::{LeftPane, SqlrApp};
use crate::tabs::{QueryTab, TabCommand};
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

/// Draws the breadcrumb, including the query database selector.
pub(crate) fn tab_breadcrumb(
    ui: &mut egui::Ui,
    engine: &Engine,
    tab: &QueryTab,
    name: Option<&str>,
    profile_mode: Option<CommitMode>,
) -> Option<TabCommand> {
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
    let has_database = database.is_some();
    let crumbs = crumbs(tab, name, database.clone());
    let mut action = None;
    let clicked = breadcrumb_with_response(ui, &crumbs, &right, |_, i, response| {
        let Some(conn) = conn.filter(|_| has_database && i == 1) else {
            return;
        };
        egui::Popup::menu(response)
            .style(sqlr_ui::theme::menu_style(&response.ctx))
            .show(|ui| {
                engine.browse(conn, NodePath::root());
                let blocked = tab.is_running(engine)
                    || engine
                        .transaction(&engine.session(conn, Some(tab.key)))
                        .is_some_and(|tx| tx.phase != TxPhase::Idle);
                if blocked {
                    ui.label("Finish the query or transaction before changing database.");
                }
                match engine.children(conn, &NodePath::root()) {
                    Some(LoadState::Ready(nodes)) => {
                        let mut databases: Vec<_> = nodes
                            .iter()
                            .filter(|n| matches!(n.kind, NodeKind::Database | NodeKind::Schema))
                            .filter_map(|n| engine.database_for_path(conn, &n.path))
                            .collect();
                        if let Some(current) = &database {
                            databases.push(current.clone());
                        }
                        databases.sort();
                        databases.dedup();
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                for db in databases {
                                    let current = database.as_deref() == Some(db.as_str());
                                    if sqlr_ui::theme::menu::MenuItem::new(&db)
                                        .checked(current)
                                        .enabled(!blocked)
                                        .ui(ui)
                                        .clicked()
                                    {
                                        action = Some(TabCommand::ChangeDatabase {
                                            tab: tab.key,
                                            database: db,
                                        });
                                        ui.close();
                                    }
                                }
                            });
                    }
                    Some(LoadState::Failed(error)) => {
                        ui.label(&error.message);
                    }
                    _ => {
                        ui.spinner();
                        ui.label("Loading databases…");
                    }
                }
            });
    });
    if clicked == Some(0) && conn.is_some() {
        action = Some(TabCommand::RevealConnection(tab.key));
    }
    action
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
