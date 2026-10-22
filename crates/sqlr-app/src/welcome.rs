//! The welcome screen in the app: shown in the editor area while no tab is open. Its cards
//! open the new-connection dialog on a known database, its buttons the demo database and
//! the dialog, and its recent list connects a connection in a new editor tab.

use egui::Color32;
use sqlr_core::known_database;
use sqlr_ui::{DriverCard, RecentItem, WelcomeAction, welcome};

use crate::app::SqlrApp;

/// Recent connections listed.
const RECENT_SHOWN: usize = 8;

/// The welcome cards: `(known database id, card name, detail)`.
const CARDS: [(&str, &str, &str); 3] = [
    ("postgresql", "PostgreSQL", "TCP server"),
    ("mysql", "MySQL / MariaDB", "TCP server"),
    ("sqlite", "SQLite", "File"),
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
    /// The welcome screen's cards for the drivers this build has.
    fn welcome_cards(&self) -> Vec<DriverCard> {
        CARDS
            .iter()
            .filter_map(|(id, name, detail)| {
                let driver = known_database(id)?.driver()?;
                self.engine.drivers().get(driver)?;
                Some(DriverCard {
                    id: id.to_string(),
                    name: name.to_string(),
                    detail: detail.to_string(),
                    icon: driver,
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
    use super::relative_time;

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
