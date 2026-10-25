//! The known databases: the catalog a new connection starts from (a searchable list grouped
//! Bundled · Compatible · Planned) and the read-only "Databases…" matrix of the settings.

use std::collections::HashSet;

use egui::{Id, RichText, Sense, WidgetInfo, WidgetType, vec2};
use sqlr_core::{Family, KnownDatabase, KnownStatus, known_database, known_databases};
use sqlr_ui::ToastKind;
use sqlr_ui::theme::{self, Spacing, icons};

use crate::app::SqlrApp;
use crate::dialogs::ConnDialog;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Pick a database for a new connection.
    Catalog,
    /// Every known database and how sqlr reaches it.
    Matrix,
}

pub struct DatabasesDialog {
    mode: Mode,
    filter: String,
}

/// What the dialog's user did.
enum Event {
    Picked(&'static str),
    Closed,
}

fn group_of(db: &KnownDatabase) -> &'static str {
    match db.status {
        KnownStatus::Bundled { .. } => "Bundled",
        KnownStatus::Compatible { .. } => "Compatible",
        KnownStatus::Installable { .. } => "From file",
        KnownStatus::Planned { .. } => "Planned",
    }
}

fn family_label(family: Family) -> &'static str {
    match family {
        Family::Relational => "Relational",
        Family::Document => "Document",
        Family::KeyValue => "Key-value",
    }
}

fn matches(db: &KnownDatabase, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    filter.is_empty()
        || db.name.to_lowercase().contains(&filter)
        || db.notes.to_lowercase().contains(&filter)
}

impl DatabasesDialog {
    pub fn catalog() -> Self {
        Self {
            mode: Mode::Catalog,
            filter: String::new(),
        }
    }

    pub fn matrix() -> Self {
        Self {
            mode: Mode::Matrix,
            filter: String::new(),
        }
    }

    /// `drivers`: the ids of the drivers this build has; a database whose driver is missing
    /// cannot be picked.
    fn show(&mut self, ctx: &egui::Context, drivers: &HashSet<String>) -> Option<Event> {
        let mut event = None;
        let title = match self.mode {
            Mode::Catalog => "New connection",
            Mode::Matrix => "Databases",
        };
        let response = egui::Modal::new(Id::new("databases_dialog")).show(ctx, |ui| {
            ui.set_width(match self.mode {
                Mode::Catalog => 480.0,
                Mode::Matrix => 720.0,
            });
            ui.label(RichText::new(title).font(theme::semibold(ui.ctx(), 15.0)));
            ui.add_space(Spacing::S);
            ui.horizontal(|ui| {
                icons::paint(ui, icons::SEARCH, theme::palette(ui.ctx()).text_muted);
                sqlr_ui::text_edit(
                    ui,
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("Search databases")
                        .desired_width(f32::INFINITY),
                );
            });
            ui.add_space(Spacing::S);
            egui::ScrollArea::vertical()
                .id_salt("databases_list")
                .max_height(420.0)
                .auto_shrink([false, true])
                .show(ui, |ui| match self.mode {
                    Mode::Catalog => {
                        if let Some(id) = catalog_list(ui, &self.filter, drivers) {
                            event = Some(Event::Picked(id));
                        }
                    }
                    Mode::Matrix => matrix(ui, &self.filter, drivers),
                });
            ui.add_space(Spacing::M);
            ui.horizontal(|ui| {
                if ui.button("Close").clicked() {
                    event = Some(Event::Closed);
                }
            });
        });
        if event.is_none() && response.should_close() {
            event = Some(Event::Closed);
        }
        event
    }
}

/// The grouped, filtered list; returns the id picked.
fn catalog_list(
    ui: &mut egui::Ui,
    filter: &str,
    drivers: &HashSet<String>,
) -> Option<&'static str> {
    let palette = theme::palette(ui.ctx());
    let mut picked = None;
    for group in ["Bundled", "Compatible", "From file", "Planned"] {
        let entries: Vec<&KnownDatabase> = known_databases()
            .iter()
            .filter(|db| group_of(db) == group && matches(db, filter))
            .collect();
        if entries.is_empty() {
            continue;
        }
        ui.add_space(Spacing::S);
        ui.label(
            RichText::new(group.to_uppercase())
                .font(theme::semibold(ui.ctx(), 10.5))
                .color(palette.text_muted),
        );
        for db in entries {
            let usable = db.driver().is_some_and(|d| drivers.contains(d));
            let installable = !usable && matches!(db.status, KnownStatus::Installable { .. });
            if catalog_row(ui, db, usable, installable) {
                picked = Some(db.id);
            }
        }
    }
    if picked.is_none() && !known_databases().iter().any(|db| matches(db, filter)) {
        theme::hint(ui, "No database matches");
    }
    picked
}

/// One entry: the database mark, the name and the note (disabled when it cannot be used).
fn catalog_row(ui: &mut egui::Ui, db: &KnownDatabase, usable: bool, installable: bool) -> bool {
    let palette = theme::palette(ui.ctx());
    let clickable = usable || installable;
    let sense = if clickable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let response = ui.interact(rect, Id::new(("known_database", db.id)), sense);
    let name = db.name;
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, clickable, name));
    if clickable && response.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(Spacing::RADIUS_M),
            palette.hover,
        );
    }
    let icon = egui::Rect::from_min_size(
        egui::pos2(rect.left() + Spacing::M, rect.center().y - 8.0),
        vec2(16.0, 16.0),
    );
    icons::paint_database_at(ui, icon, db.driver().unwrap_or("database"));
    let text = if clickable {
        palette.text
    } else {
        palette.text_disabled
    };
    let name_pos = egui::pos2(icon.right() + Spacing::M, rect.center().y);
    let galley =
        ui.painter()
            .layout_no_wrap(db.name.to_string(), egui::FontId::proportional(13.0), text);
    let name_width = galley.size().x;
    ui.painter().galley(
        egui::pos2(name_pos.x, name_pos.y - galley.size().y / 2.0),
        galley,
        text,
    );
    let note = if usable || db.driver().is_none() {
        db.notes
    } else if installable {
        "Click to choose its driver file"
    } else {
        "Its driver is not part of this build"
    };
    ui.painter().text(
        egui::pos2(name_pos.x + name_width + Spacing::M, name_pos.y),
        egui::Align2::LEFT_CENTER,
        note,
        egui::FontId::proportional(11.5),
        palette.text_muted,
    );
    clickable && response.clicked()
}

/// The read-only matrix: database, family, how sqlr reaches it, driver, port, notes.
fn matrix(ui: &mut egui::Ui, filter: &str, drivers: &HashSet<String>) {
    let palette = theme::palette(ui.ctx());
    egui::Grid::new("databases_matrix")
        .striped(true)
        .num_columns(5)
        .spacing(vec2(Spacing::L, Spacing::S))
        .show(ui, |ui| {
            for head in ["Database", "Family", "Support", "Driver", "Port"] {
                ui.label(RichText::new(head).font(theme::semibold(ui.ctx(), 12.0)));
            }
            ui.end_row();
            for db in known_databases().iter().filter(|db| matches(db, filter)) {
                ui.label(db.name);
                ui.label(RichText::new(family_label(db.family)).color(palette.text_muted));
                let (support, color) = match db.status {
                    KnownStatus::Bundled { driver } if drivers.contains(driver) => {
                        ("Bundled".to_string(), palette.success)
                    }
                    KnownStatus::Compatible { driver, .. } if drivers.contains(driver) => {
                        ("Compatible".to_string(), palette.success)
                    }
                    KnownStatus::Installable { driver } if drivers.contains(driver) => {
                        ("Installed driver".to_string(), palette.success)
                    }
                    KnownStatus::Installable { .. } => {
                        ("Needs driver file".to_string(), palette.accent)
                    }
                    KnownStatus::Planned { note } => {
                        (format!("Planned ({note})"), palette.text_muted)
                    }
                    _ => ("Driver missing".to_string(), palette.warning),
                };
                ui.label(RichText::new(support).color(color))
                    .on_hover_text(db.notes);
                ui.label(db.driver().unwrap_or("—"));
                ui.label(db.default_port.map_or("—".to_string(), |p| p.to_string()));
                ui.end_row();
            }
        });
}

impl SqlrApp {
    /// Opens the new-connection dialog for the known database `id`; with the dialog
    /// already open, it switches to a new data source there.
    pub(crate) fn open_known_connection(&mut self, id: &str) {
        let Some(known) = known_database(id) else {
            return;
        };
        let manifest = known.driver().and_then(|d| self.manifest(d));
        if manifest.is_none()
            && let KnownStatus::Installable { driver } = known.status
        {
            self.request_driver_install(driver, Some(known.id));
            return;
        }
        let Some(manifest) = manifest else {
            let text = match known.status {
                KnownStatus::Planned { note } => {
                    format!("{} is planned ({note})", known.name)
                }
                _ => format!("The driver for {} is not part of this build", known.name),
            };
            self.notify_as(ToastKind::Info, text);
            return;
        };
        match &mut self.conn_dialog {
            Some(dialog) => dialog.start_known(&self.engine, &manifest, known),
            None => self.conn_dialog = Some(ConnDialog::create_known(&manifest, known)),
        }
    }

    /// Draws the catalog or the matrix (every frame).
    pub(crate) fn databases_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.databases_dialog else {
            return;
        };
        let drivers: HashSet<String> = self
            .engine
            .drivers()
            .manifests()
            .into_iter()
            .map(|m| m.id)
            .collect();
        match dialog.show(ctx, &drivers) {
            None => {}
            Some(Event::Closed) => self.databases_dialog = None,
            Some(Event::Picked(id)) => {
                self.databases_dialog = None;
                self.open_known_connection(id);
            }
        }
    }

    /// The connection dialog's driver, field values and name, when it is open.
    #[doc(hidden)]
    pub fn conn_dialog_form(
        &self,
    ) -> Option<(String, std::collections::BTreeMap<String, String>, String)> {
        self.conn_dialog.as_ref().map(ConnDialog::form_values)
    }
}

#[cfg(test)]
mod tests {
    use sqlr_core::{Driver, KnownStatus, known_databases};

    /// The drivers the app ships (as `production_deps` registers them).
    fn shipped() -> Vec<sqlr_core::DriverManifest> {
        let drivers: Vec<std::sync::Arc<dyn Driver>> = vec![
            sqlr_driver_postgres::PostgresDriver::new(),
            sqlr_driver_mysql::MysqlDriver::new(),
            sqlr_driver_mssql::SqlServerDriver::new(),
            sqlr_driver_oracle::OracleDriver::new(),
            sqlr_driver_sqlite::SqliteDriver::new(),
            sqlr_driver_snowflake::SnowflakeDriver::new(),
            sqlr_driver_mongo::MongoDriver::new(),
            sqlr_driver_elastic::ElasticDriver::new(),
            sqlr_driver_redis::RedisDriver::new(),
        ];
        drivers.iter().map(|d| d.manifest().clone()).collect()
    }

    #[test]
    fn known_presets_match_manifests() {
        let manifests = shipped();
        for db in known_databases() {
            let Some(driver) = db.driver() else {
                continue;
            };
            if matches!(db.status, KnownStatus::Installable { .. }) {
                continue; // not shipped: its manifest comes with the installed driver
            }
            let manifest = manifests
                .iter()
                .find(|m| m.id == driver)
                .unwrap_or_else(|| panic!("{}: no shipped driver {driver}", db.id));
            assert_eq!(manifest.family, db.family, "{}", db.id);
            for (key, value) in db.preset() {
                let field = manifest
                    .fields
                    .iter()
                    .find(|f| f.key == *key)
                    .unwrap_or_else(|| panic!("{}: {driver} has no field {key}", db.id));
                assert!(!field.secret, "{}: a preset never fills a secret", db.id);
                if let sqlr_core::FieldType::Choice(options) = &field.ty {
                    assert!(
                        options.iter().any(|(v, _)| v == value),
                        "{}: {value}",
                        db.id
                    );
                }
            }
            // A bundled database's port is its driver's default port.
            if matches!(db.status, KnownStatus::Bundled { .. }) {
                assert_eq!(db.default_port, manifest.default_port, "{}", db.id);
            }
        }
    }
}
