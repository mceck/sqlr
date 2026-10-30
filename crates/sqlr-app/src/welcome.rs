//! The welcome screen in the app: shown in the editor area while no tab is open. Its cards
//! open the new-connection dialog on a known database, its buttons the demo database and
//! the dialog, and its recent list connects a connection in a new editor tab.

use egui::Color32;
use sqlr_core::{Family, KnownStatus, known_database};
use sqlr_ui::{DriverCard, RecentItem, WelcomeAction, welcome};

use crate::app::SqlrApp;

/// Recent connections listed.
const RECENT_SHOWN: usize = 8;

/// A compact selection of native and installable databases for the two-row grid.
const CARDS: [&str; 8] = [
    "postgresql",
    "mysql",
    "sqlite",
    "mongodb",
    "redis",
    "sqlserver",
    "oracle",
    "clickhouse",
];

/// How long ago `at_ms` was, from `now_ms`: "just now", "5 min ago", "3 h ago",
/// "yesterday", "4 days ago", then the date.
pub(crate) fn relative_time(at_ms: i64, now_ms: i64) -> String {
    let minutes = (now_ms - at_ms).max(0) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes} min ago"),
        60..=1_439 => format!("{} h ago", minutes / 60),
        1_440..=2_879 => "yesterday".into(),
        2_880..=43_199 => format!("{} days ago", minutes / 1_440),
        // The UTC date: the local zone would be read from disk on the UI thread.
        _ => crate::demo::date(at_ms.div_euclid(86_400_000)),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl SqlrApp {
    /// Available drivers and databases whose driver can be installed from the card.
    fn welcome_cards(&self) -> Vec<DriverCard> {
        CARDS
            .iter()
            .filter_map(|id| {
                let database = known_database(id)?;
                let driver = database.driver()?;
                let installed = self.engine.drivers().get(driver).is_some();
                if !installed && !matches!(database.status, KnownStatus::Installable { .. }) {
                    return None;
                }
                let detail = match database.status {
                    KnownStatus::Installable { .. } if installed => "Installed driver",
                    KnownStatus::Installable { .. } => "Install driver",
                    KnownStatus::Compatible {
                        driver: "postgres", ..
                    } => "Via PostgreSQL",
                    KnownStatus::Compatible { .. } => "Via MySQL",
                    _ if database.id == "sqlite" => "Local file",
                    _ => match database.family {
                        Family::Relational => "Native · SQL",
                        Family::Document if database.id == "elasticsearch" => "Search engine",
                        Family::Document => "Documents",
                        Family::KeyValue => "Key-value store",
                    },
                };
                Some(DriverCard {
                    id: id.to_string(),
                    name: database.name.to_string(),
                    detail: detail.to_string(),
                    install: !installed,
                    icon: if matches!(database.status, KnownStatus::Compatible { .. }) {
                        database.id
                    } else {
                        driver
                    },
                })
            })
            .collect()
    }

    /// Saved connections by last use, most recent first.
    fn recent_items(&self) -> Vec<RecentItem> {
        let now = now_ms();
        self.engine
            .recent()
            .iter()
            .filter_map(|(id, at)| {
                let cfg = self.connection(id)?;
                let manifest = self.manifest(&cfg.driver);
                let driver = manifest
                    .as_ref()
                    .map_or_else(|| cfg.driver.clone(), |m| m.name.clone());
                let place = manifest.as_ref().and_then(|m| {
                    cfg.value(m, "path")
                        .and_then(|p| std::path::Path::new(p).file_name())
                        .map(|f| f.to_string_lossy().into_owned())
                        .or_else(|| cfg.value(m, "host").map(str::to_string))
                        .filter(|p| !p.is_empty())
                });
                let detail = match place {
                    Some(place) => format!("{driver} · {place}"),
                    None => driver,
                };
                Some(RecentItem {
                    id: id.clone(),
                    name: cfg.name.clone(),
                    detail,
                    driver: cfg.driver.clone(),
                    color: self
                        .connection_tint(id)
                        .map(|[r, g, b]| Color32::from_rgb(r, g, b)),
                    when: relative_time(*at, now),
                })
            })
            .take(RECENT_SHOWN)
            .collect()
    }

    /// Draws the welcome screen in the editor area.
    pub(crate) fn welcome_ui(&mut self, ui: &mut egui::Ui) {
        let palette = sqlr_ui::theme::palette(ui.ctx());
        let cards = self.welcome_cards();
        let recent = self.recent_items();
        let action = egui::Frame::NONE
            .fill(palette.bg_editor)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                welcome(ui, &cards, &recent)
            })
            .inner;
        match action {
            Some(WelcomeAction::NewConnection(Some(id))) => self.open_known_connection(&id),
            Some(WelcomeAction::NewConnection(None)) => self.open_new_connection(),
            Some(WelcomeAction::MoreDatabases) => {
                self.databases_dialog = Some(crate::databases_dialog::DatabasesDialog::catalog());
            }
            Some(WelcomeAction::ImportConnections) => self.open_client_import(),
            Some(WelcomeAction::OpenDemo) => self.open_demo(),
            Some(WelcomeAction::OpenRecent(id)) => self.open_recent(&id),
            None => {}
        }
    }

    /// Connects `id` and opens an editor tab on it (its browser, for a key-value store).
    fn open_recent(&mut self, id: &str) {
        if self.connection(id).is_none() {
            return;
        }
        self.tree.selected = Some((id.to_string(), sqlr_core::NodePath::root()));
        self.tree.selected_folder = None;
        let connected = self.is_live(id) || self.connect(id);
        if self.is_key_value(id) {
            if connected {
                self.pending_redis.insert(id.to_string());
            }
            return;
        }
        if self.is_document(id) {
            // A document connection edits its statements in a document tab, not a SQL one.
            let title = self
                .connection(id)
                .map_or_else(|| id.to_string(), |c| c.name.clone());
            self.open_doc_tab(title, id, None, "", false);
            return;
        }
        self.new_editor_tab_on(id);
    }

    /// `(id, name, driver)` of every saved connection.
    #[doc(hidden)]
    pub fn saved_connections(&self) -> Vec<(String, String, String)> {
        self.connections
            .iter()
            .map(|c| (c.id.clone(), c.name.clone(), c.driver.clone()))
            .collect()
    }

    /// The engine's recent connection ids, most recent first.
    #[doc(hidden)]
    pub fn recent_ids(&self) -> Vec<String> {
        self.engine
            .recent()
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlr_core::{ConnectionConfig, Secrets};
    use sqlr_engine::{DriverRegistry, Engine};
    use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
    use sqlr_testkit::FakeDocDriver;

    use super::*;
    use crate::app::AppDeps;

    /// An app with the saved document connection "m".
    fn document_app() -> (SqlrApp, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut drivers = DriverRegistry::default();
        drivers.register(FakeDocDriver::new(Default::default()));
        let repo =
            ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
        repo.save(
            &ConnectionConfig {
                id: "m".into(),
                name: "m-conn".into(),
                driver: "fakedoc".into(),
                group: None,
                color: None,
                values: Default::default(),
                profile: None,
                save_secrets: true,
                agent_access: Default::default(),
            },
            &Secrets::default(),
        )
        .unwrap();
        let deps = AppDeps {
            check_updates: false,
            engine: Engine::new(drivers, Arc::new(|| {})),
            repo,
            settings: Settings::default(),
            config_dir: dir.path().to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
        };
        (SqlrApp::new(deps, &egui::Context::default()), dir)
    }

    #[test]
    fn a_recent_document_connection_opens_a_document_tab() {
        let (mut app, _dir) = document_app();
        app.open_recent("m");
        let tab = app.active_tab().expect("a document tab");
        assert_eq!(tab.conn.as_deref(), Some("m"));
        assert!(tab.doc.is_some(), "the tab runs document statements");
    }

    #[test]
    fn relative_times() {
        let now = 10_000_000_000;
        let min = 60_000;
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(relative_time(now + 5_000, now), "just now");
        assert_eq!(relative_time(now - 5 * min, now), "5 min ago");
        assert_eq!(relative_time(now - 180 * min, now), "3 h ago");
        assert_eq!(relative_time(now - 30 * 60 * min, now), "yesterday");
        assert_eq!(relative_time(now - 4 * 1_440 * min, now), "4 days ago");
        assert_eq!(relative_time(0, now), "1970-01-01");
    }
}
