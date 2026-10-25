//! Text that comes back from the library (a history entry, a snippet) opens in the tab kind
//! its connection needs, and a SQL tab is only ever pointed at a connection that runs SQL.

use crate::app::SqlrApp;

impl SqlrApp {
    /// Whether `conn` runs neither SQL nor anything a SQL tab can show: a document or
    /// key-value connection, which has its own tab kind.
    pub(crate) fn is_nosql(&self, conn: &str) -> bool {
        self.is_document(conn) || self.is_key_value(conn)
    }

    /// Opens `text` on `conn` without running it: a document tab for a document connection,
    /// the Redis console (with the line in its input) for a key-value one, else a SQL tab.
    pub(crate) fn open_text(&mut self, title: Option<String>, conn: Option<String>, text: &str) {
        match conn {
            Some(c) if self.is_document(&c) => {
                let title = title
                    .or_else(|| self.connection(&c).map(|cfg| cfg.name.clone()))
                    .unwrap_or_else(|| c.clone());
                self.open_doc_tab(title, &c, None, text, false);
            }
            Some(c) if self.is_key_value(&c) => self.open_redis_console(&c, text),
            conn => {
                self.new_tab(title, conn, text);
            }
        }
    }

    /// Whether the active tab is a document or Redis tab.
    pub(crate) fn active_is_nosql_tab(&mut self) -> bool {
        self.active_tab()
            .is_some_and(|t| t.doc.is_some() || t.redis.is_some())
    }

    /// The connections the picker of a tab offers, as `(id, name)`: a SQL tab (or no tab,
    /// `nosql_tab` false) lists only the connections that run SQL.
    pub(crate) fn picker_conn_names(&self, nosql_tab: bool) -> Vec<(String, String)> {
        self.conn_names()
            .into_iter()
            .filter(|(id, _)| nosql_tab || !self.is_nosql(id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlr_core::{ConnectionConfig, Secrets};
    use sqlr_engine::{DriverRegistry, Engine};
    use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
    use sqlr_testkit::{FakeDocDriver, FakeDriver, FakeKvDriver};

    use super::*;
    use crate::app::AppDeps;

    /// An app with a SQL ("pg"), a document ("m") and a key-value ("r") connection.
    fn app() -> (SqlrApp, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut drivers = DriverRegistry::default();
        drivers.register(FakeDriver::new(Default::default()));
        drivers.register(FakeDocDriver::new(Default::default()));
        drivers.register(FakeKvDriver::new(Default::default()));
        let repo =
            ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
        for (id, driver) in [("pg", "fake"), ("m", "fakedoc"), ("r", "fakekv")] {
            let cfg = ConnectionConfig {
                id: id.into(),
                name: format!("{id}-conn"),
                driver: driver.into(),
                group: None,
                color: None,
                values: Default::default(),
                profile: None,
                save_secrets: true,
                agent_access: Default::default(),
            };
            repo.save(&cfg, &Secrets::default()).unwrap();
        }
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

    fn entry(conn: &str, text: &str) -> sqlr_core::HistoryEntry {
        sqlr_core::HistoryEntry {
            id: 1,
            conn_id: conn.into(),
            database: None,
            sql: text.into(),
            started_at_ms: 1,
            duration_ms: 1,
            outcome: sqlr_core::HistoryOutcome::Rows(1),
        }
    }

    #[test]
    fn a_document_history_entry_reopens_in_a_document_tab() {
        let (mut app, _dir) = app();
        app.open_history_entry(&entry("m", "db.people.find({})"));
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.conn.as_deref(), Some("m"));
        assert!(tab.doc.is_some(), "a SQL tab on a document connection");
        assert_eq!(tab.editor.text(), "db.people.find({})");
    }

    #[test]
    fn a_redis_history_entry_reopens_in_its_console() {
        let (mut app, _dir) = app();
        app.open_history_entry(&entry("r", "GET user:1"));
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.conn.as_deref(), Some("r"));
        let redis = tab.redis.as_ref().expect("a SQL tab on a Redis connection");
        assert_eq!(redis.console_input(), "GET user:1");
    }

    #[test]
    fn a_sql_history_entry_still_reopens_in_a_sql_tab() {
        let (mut app, _dir) = app();
        app.open_history_entry(&entry("pg", "select 1"));
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.conn.as_deref(), Some("pg"));
        assert!(tab.doc.is_none() && tab.redis.is_none());
    }

    #[test]
    fn a_sql_tab_is_never_pointed_at_a_nosql_connection() {
        let (mut app, _dir) = app();
        let key = app.new_tab(None, Some("pg".into()), "select 1");
        let nosql_tab = app.active_is_nosql_tab();
        let names: Vec<String> = app
            .picker_conn_names(nosql_tab)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(names, ["pg"]);
        for conn in ["m", "r"] {
            app.set_tab_conn(key, Some(conn.into()));
            assert_eq!(app.tab(key).unwrap().conn.as_deref(), Some("pg"), "{conn}");
            assert!(
                app.toasts
                    .texts()
                    .iter()
                    .any(|t| t.contains("does not run SQL")),
                "{conn}: no notice"
            );
        }
    }

    #[test]
    fn the_chip_of_a_sql_tab_lists_only_sql_connections() {
        use egui_kittest::kittest::Queryable;
        let (mut app, _dir) = app();
        app.new_tab(None, Some("pg".into()), "select 1");
        let mut h = egui_kittest::Harness::builder()
            .with_size((1400.0, 900.0))
            .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
        h.run_steps(2);
        let in_explorer =
            |h: &egui_kittest::Harness<'_, SqlrApp>, label: &str| h.get_all_by_label(label).count();
        let before = (in_explorer(&h, "m-conn"), in_explorer(&h, "r-conn"));
        let chip = h
            .get_all_by_label("pg-conn")
            .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
            .unwrap()
            .rect();
        h.get_all_by_label("pg-conn")
            .find(|n| n.rect() == chip)
            .unwrap()
            .click();
        h.run_steps(2);
        h.get_by_label("CONNECTIONS");
        // The popup adds no row for the document or the key-value connection.
        assert_eq!(
            (in_explorer(&h, "m-conn"), in_explorer(&h, "r-conn")),
            before
        );
    }
}
