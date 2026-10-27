//! The "Server properties" tab: properties, database sizes and table statistics, each
//! loaded when its pane is first shown and again on Refresh.

use egui::RichText;
use sqlr_core::{AdminSupport, Dialect, ServerProperty, TableStats};
use sqlr_engine::{AdminQuery, AdminSnapshot, Engine, JobId, JobOutcome};
use sqlr_ui::theme::{self, Spacing, icons};
use sqlr_ui::{
    PropertiesState, StatsState, database_sizes_view, properties_view, table_stats_view,
};

use super::take;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    Properties,
    Sizes,
    Statistics,
}

impl Pane {
    fn label(self) -> &'static str {
        match self {
            Pane::Properties => "Properties",
            Pane::Sizes => "Database sizes",
            Pane::Statistics => "Table statistics",
        }
    }
}

/// A snapshot as loaded, or why it failed.
type Loaded<T> = Option<Result<T, LoadError>>;

/// Why a snapshot is not shown.
#[derive(Debug, thiserror::Error)]
enum LoadError {
    /// The server refused the request.
    #[error(transparent)]
    Failed(#[from] sqlr_core::DriverError),
    /// The job ended with something other than a snapshot.
    #[error("Nothing was loaded")]
    Nothing,
}

pub struct ServerTab {
    pub conn: String,
    support: AdminSupport,
    pane: Pane,
    properties: Loaded<Vec<ServerProperty>>,
    sizes: Loaded<Vec<(String, i64)>>,
    stats: Loaded<Vec<TableStats>>,
    /// The schema whose statistics are shown (`None`: every schema).
    schema: Option<String>,
    /// The schema `stats` were loaded for.
    stats_schema: Option<Option<String>>,
    job: Option<(Pane, JobId)>,
    properties_state: PropertiesState,
    stats_state: StatsState,
}

impl ServerTab {
    pub fn new(engine: &Engine, conn: &str, dialect: Option<&dyn Dialect>) -> Self {
        let support = engine.admin_support(conn).unwrap_or_default();
        let pane = if support.properties {
            Pane::Properties
        } else {
            Pane::Sizes
        };
        Self {
            conn: conn.to_string(),
            support,
            pane,
            properties: None,
            sizes: None,
            stats: None,
            schema: dialect.and_then(|d| d.default_schemas().into_iter().next()),
            stats_schema: None,
            job: None,
            properties_state: PropertiesState::default(),
            stats_state: StatsState::default(),
        }
    }

    pub fn jobs(&self) -> Vec<JobId> {
        self.job.iter().map(|(_, id)| *id).collect()
    }

    fn panes(&self) -> Vec<Pane> {
        let mut out = Vec::new();
        if self.support.properties {
            out.push(Pane::Properties);
        }
        if self.support.statistics {
            out.extend([Pane::Sizes, Pane::Statistics]);
        }
        out
    }

    fn load(&mut self, engine: &Engine, pane: Pane) {
        if self.job.is_some() {
            return;
        }
        let query = match pane {
            Pane::Properties => AdminQuery::Properties,
            Pane::Sizes => AdminQuery::DatabaseSizes,
            Pane::Statistics => {
                self.stats_schema = Some(self.schema.clone());
                AdminQuery::TableStats {
                    schema: self.schema.clone(),
                }
            }
        };
        self.job = Some((pane, engine.admin_snapshot(&self.conn, query)));
    }

    fn poll(&mut self, engine: &Engine) {
        let Some((pane, id)) = self.job else {
            return;
        };
        let mut slot = Some(id);
        let Some(outcome) = take(engine, &mut slot) else {
            return;
        };
        self.job = None;
        match (pane, outcome) {
            (_, JobOutcome::Admin(AdminSnapshot::Properties(p))) => self.properties = Some(Ok(p)),
            (_, JobOutcome::Admin(AdminSnapshot::DatabaseSizes(s))) => self.sizes = Some(Ok(s)),
            (_, JobOutcome::Admin(AdminSnapshot::TableStats(s))) => self.stats = Some(Ok(s)),
            (pane, other) => {
                let e = match other {
                    JobOutcome::Failed(e) => LoadError::Failed(e),
                    _ => LoadError::Nothing,
                };
                match pane {
                    Pane::Properties => self.properties = Some(Err(e)),
                    Pane::Sizes => self.sizes = Some(Err(e)),
                    Pane::Statistics => self.stats = Some(Err(e)),
                }
            }
        }
    }

    fn schemas(&self, engine: &Engine, dialect: Option<&dyn Dialect>) -> Vec<String> {
        let mut out: Vec<String> = dialect.map(|d| d.default_schemas()).unwrap_or_default();
        if let Some(catalog) = engine.catalog(&self.conn) {
            for s in catalog.schema.keys() {
                if !out.contains(s) {
                    out.push(s.clone());
                }
            }
        }
        out
    }

    pub fn show(&mut self, ui: &mut egui::Ui, engine: &Engine, dialect: Option<&dyn Dialect>) {
        self.poll(engine);
        let loaded = match self.pane {
            Pane::Properties => self.properties.is_some(),
            Pane::Sizes => self.sizes.is_some(),
            Pane::Statistics => self.stats_schema.as_ref() == Some(&self.schema),
        };
        if !loaded {
            self.load(engine, self.pane);
        }
        ui.add_space(Spacing::S);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::S;
            for pane in self.panes() {
                if theme::segment(ui, self.pane == pane, pane.label()).clicked() {
                    self.pane = pane;
                }
            }
            ui.separator();
            if self.pane == Pane::Statistics {
                let text = self.schema.clone().unwrap_or_else(|| "All schemas".into());
                egui::ComboBox::from_id_salt(("stats_schema", &self.conn))
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.schema, None, "All schemas");
                        for s in self.schemas(engine, dialect) {
                            let label = s.clone();
                            ui.selectable_value(&mut self.schema, Some(s), label);
                        }
                    });
            }
            if icons::button(ui, self.job.is_none(), icons::REFRESH, None, "Refresh").clicked() {
                self.load(engine, self.pane);
            }
            if self.job.is_some() {
                ui.spinner();
            }
        });
        ui.add_space(Spacing::S);
        let error = theme::palette(ui.ctx()).error;
        let show_error = |ui: &mut egui::Ui, e: &LoadError| {
            ui.label(RichText::new(e.to_string()).color(error));
        };
        match self.pane {
            Pane::Properties => match &self.properties {
                Some(Ok(p)) => properties_view(ui, &mut self.properties_state, p),
                Some(Err(e)) => show_error(ui, e),
                None => {}
            },
            Pane::Sizes => match &self.sizes {
                Some(Ok(s)) => database_sizes_view(ui, s),
                Some(Err(e)) => show_error(ui, e),
                None => {}
            },
            Pane::Statistics => match &self.stats {
                Some(Ok(s)) => table_stats_view(ui, &mut self.stats_state, s),
                Some(Err(e)) => show_error(ui, e),
                None => {}
            },
        }
    }
}

#[cfg(test)]
mod load_error_tests {
    use super::*;

    #[test]
    fn load_errors_keep_their_messages() {
        assert_eq!(LoadError::Nothing.to_string(), "Nothing was loaded");
        let failed = LoadError::from(sqlr_core::DriverError::new(
            sqlr_core::ErrorKind::Other,
            "denied",
        ));
        assert_eq!(failed.to_string(), "denied");
    }
}
