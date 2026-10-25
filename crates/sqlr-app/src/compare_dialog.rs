//! Schema compare (spec §8.6): pick two schemas of one kind of database, see the tree of
//! differences with a side-by-side DDL diff, exclude objects and open the migration script in
//! an editor tab bound to the target. Compare never writes to either database.

use std::collections::HashSet;

use egui::{ComboBox, RichText, TextEdit};
use sqlr_core::{DiffKind, ObjectDiff, ObjectRef};
use sqlr_engine::{Engine, JobId, JobOutcome, SessionStatus};
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{diff_tree, text_diff};

use crate::app::SqlrApp;
use crate::transfer_wizard::ConnChoice;

/// What the dialog asks of the app.
pub(crate) enum CompareEvent {
    None,
    Close,
    /// Open `text` in an editor tab on `conn`.
    OpenScript {
        conn: String,
        title: String,
        text: String,
    },
}

pub(crate) struct CompareDialog {
    s_conn: String,
    s_database: String,
    s_schema: String,
    t_conn: String,
    t_database: String,
    t_schema: String,
    job: Option<JobId>,
    diffs: Option<Vec<ObjectDiff>>,
    error: Option<String>,
    excluded: HashSet<ObjectRef>,
    selected: Option<usize>,
}

impl CompareDialog {
    /// A dialog with the source schema filled in (the explorer's "Compare with…").
    pub fn new(
        conns: &[ConnChoice],
        conn: Option<&str>,
        database: Option<String>,
        schema: Option<String>,
    ) -> Self {
        let first = conn
            .map(str::to_string)
            .or_else(|| conns.first().map(|c| c.id.clone()))
            .unwrap_or_default();
        let other = conns
            .iter()
            .find(|c| c.id != first)
            .map_or(first.clone(), |c| c.id.clone());
        Self {
            s_conn: first,
            s_database: database.clone().unwrap_or_default(),
            // The target's schema usually has the same name.
            t_schema: schema.clone().unwrap_or_default(),
            s_schema: schema.unwrap_or_default(),
            t_conn: other,
            t_database: String::new(),
            job: None,
            diffs: None,
            error: None,
            excluded: HashSet::new(),
            selected: None,
        }
    }

    /// The connections the dialog reads.
    pub fn needed_connections(&self) -> Vec<String> {
        let mut ids = vec![self.s_conn.clone()];
        if self.t_conn != self.s_conn {
            ids.push(self.t_conn.clone());
        }
        ids.into_iter().filter(|i| !i.is_empty()).collect()
    }

    fn side(conn: &str, database: &str, schema: &str) -> (String, Option<String>, String) {
        (
            conn.to_string(),
            Some(database.trim().to_string()).filter(|d| !d.is_empty()),
            schema.trim().to_string(),
        )
    }

    fn poll(&mut self, engine: &Engine) {
        let Some(id) = self.job else {
            return;
        };
        let Some(state) = engine.job(id) else {
            self.job = None;
            return;
        };
        let Some(outcome) = state.outcome else {
            return;
        };
        engine.forget_job(id);
        self.job = None;
        match outcome {
            JobOutcome::Compared(diffs) => {
                self.excluded.clear();
                self.selected = (!diffs.is_empty()).then_some(0);
                self.diffs = Some(diffs);
            }
            JobOutcome::Failed(e) => self.error = Some(e.message),
            _ => {}
        }
    }

    fn conn_combo(ui: &mut egui::Ui, id: &str, conns: &[ConnChoice], current: &mut String) {
        let shown = conns
            .iter()
            .find(|c| c.id == *current)
            .map_or("choose a connection", |c| c.name.as_str());
        ComboBox::from_id_salt(id)
            .selected_text(shown)
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for c in conns {
                    if ui.selectable_label(*current == c.id, &c.name).clicked() {
                        *current = c.id.clone();
                    }
                }
            });
    }

    fn status(ui: &mut egui::Ui, engine: &Engine, id: &str) {
        let p = theme::palette(ui.ctx());
        match engine.session_status(id) {
            Some(SessionStatus::Connected(_)) => {
                ui.label(RichText::new("connected").small().color(p.success));
            }
            Some(SessionStatus::Failed(e)) => {
                ui.label(RichText::new(e.message).small().color(p.error));
            }
            _ if !id.is_empty() => {
                ui.label(RichText::new("connecting…").small().color(p.text_muted));
            }
            _ => {}
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        engine: &Engine,
        conns: &[ConnChoice],
    ) -> CompareEvent {
        self.poll(engine);
        let mut event = CompareEvent::None;
        let response =
            theme::Dialog::new("compare_dialog", theme::icons::SCHEMA, "Compare schemas")
                .subtitle("Compare a reference schema with the schema you want to update")
                .width(900.0)
                .show(ctx, |d| {
                    d.body(|ui| {
                        self.header(ui, engine, conns);
                        if let Some(e) = &self.error {
                            ui.colored_label(theme::palette(ui.ctx()).error, e);
                        }
                        if self.diffs.is_some() {
                            ui.add_space(Spacing::M);
                            event = self.results(ui, engine);
                        }
                    });
                    let blocked = self.compare_blocked();
                    d.footer(|f| {
                        if f.primary("Compare", blocked) {
                            self.error = None;
                            self.diffs = None;
                            self.job = Some(engine.compare_schemas(
                                Self::side(&self.s_conn, &self.s_database, &self.s_schema),
                                Self::side(&self.t_conn, &self.t_database, &self.t_schema),
                            ));
                        }
                        f.cancel_as("Close");
                        if self.job.is_some() {
                            f.right().spinner();
                        }
                    });
                });
        if response.closed {
            return CompareEvent::Close;
        }
        event
    }

    /// Why Compare is disabled, if it is.
    fn compare_blocked(&self) -> Option<&'static str> {
        if self.job.is_some() {
            Some("Comparing…")
        } else if self.s_conn.is_empty() {
            Some("Choose the source connection")
        } else if self.s_schema.trim().is_empty() {
            Some("Enter the source schema")
        } else if self.t_conn.is_empty() {
            Some("Choose the target connection")
        } else if self.t_schema.trim().is_empty() {
            Some("Enter the target schema")
        } else {
            None
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, engine: &Engine, conns: &[ConnChoice]) {
        let muted = theme::palette(ui.ctx()).text_muted;
        ui.columns(2, |columns| {
            for (ui, (title, hint, id, conn, database, schema)) in columns.iter_mut().zip([
                (
                    "Source",
                    "Reference schema",
                    "cmp-source",
                    &mut self.s_conn,
                    &mut self.s_database,
                    &mut self.s_schema,
                ),
                (
                    "Target",
                    "Schema to update",
                    "cmp-target",
                    &mut self.t_conn,
                    &mut self.t_database,
                    &mut self.t_schema,
                ),
            ]) {
                egui::Frame::new()
                    .fill(theme::palette(ui.ctx()).bg_subtle)
                    .corner_radius(Spacing::RADIUS_M)
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        theme::section_title(ui, title);
                        theme::hint(ui, hint);
                        ui.add_space(Spacing::M);
                        theme::field_label(ui, "Connection");
                        Self::conn_combo(ui, id, conns, conn);
                        Self::status(ui, engine, conn);
                        let label = ui.label(
                            egui::RichText::new(format!("{title} database (optional)"))
                                .small()
                                .color(muted),
                        );
                        ui.add(
                            TextEdit::singleline(database)
                                .hint_text("Connection default")
                                .desired_width(f32::INFINITY),
                        )
                        .labelled_by(label.id);
                        let label = ui.label(format!("{title} schema"));
                        ui.add(
                            TextEdit::singleline(schema)
                                .hint_text("Schema name")
                                .desired_width(f32::INFINITY),
                        )
                        .labelled_by(label.id);
                    });
            }
        });
        ui.add_space(Spacing::M);
        ui.label(
            RichText::new("Compare only reads: the migration script opens in an editor.")
                .small()
                .color(muted),
        );
    }

    fn results(&mut self, ui: &mut egui::Ui, engine: &Engine) -> CompareEvent {
        let mut event = CompareEvent::None;
        let Some(diffs) = &self.diffs else {
            return event;
        };
        if diffs.is_empty() {
            ui.label("The schemas are the same.");
            return event;
        }
        let counts = |kind: DiffKind| diffs.iter().filter(|d| d.kind == kind).count();
        ui.label(format!(
            "{} to create, {} to change, {} to drop",
            counts(DiffKind::Added),
            counts(DiffKind::Changed),
            counts(DiffKind::Removed)
        ));
        ui.horizontal(|ui| {
            let script = ui.button("Open migration script");
            if script.clicked() {
                let header = format!(
                    "-- Migration of {} to match {}",
                    self.t_schema.trim(),
                    self.s_schema.trim()
                );
                match engine.migration_script(&self.t_conn, diffs, &self.excluded, &header) {
                    Ok(text) => {
                        event = CompareEvent::OpenScript {
                            conn: self.t_conn.clone(),
                            title: format!("Migrate {}.sql", self.t_schema.trim()),
                            text,
                        };
                    }
                    Err(e) => self.error = Some(e.message),
                }
            }
        });
        ui.add_space(Spacing::S);
        // A scrolled body has no height limit: size the panes from the window.
        let height = (ui.ctx().content_rect().height() * 0.45).clamp(240.0, 480.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui(egui::vec2(300.0, height), |ui| {
                ui.set_width(300.0);
                diff_tree(ui, diffs, &mut self.excluded, &mut self.selected);
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_height(height);
                match self.selected.and_then(|i| diffs.get(i)) {
                    Some(d) => {
                        ui.label(RichText::new(&d.object.name).strong());
                        ui.label(
                            RichText::new("Target now (left) → source (right)")
                                .small()
                                .color(theme::palette(ui.ctx()).text_muted),
                        );
                        text_diff(
                            ui,
                            d.target_ddl.as_deref().unwrap_or(""),
                            d.source_ddl.as_deref().unwrap_or(""),
                        );
                    }
                    None => {
                        ui.label("Select an object to see its definitions.");
                    }
                }
            });
        });
        event
    }
}

impl SqlrApp {
    /// "Compare with…" on a schema or database node, or ⌘⇧P "Compare schemas…".
    pub(crate) fn open_compare(
        &mut self,
        conn: Option<&str>,
        database: Option<String>,
        schema: Option<String>,
    ) {
        let conns = self.wizard_connections();
        if conns.len() < 2 && conn.is_none() {
            self.notify("Compare needs a connection to start from".to_string());
            return;
        }
        self.compare_dialog = Some(CompareDialog::new(&conns, conn, database, schema));
    }

    /// The compare dialog; connects what it needs.
    pub(crate) fn compare_ui(&mut self, ctx: &egui::Context) {
        let conns = self.wizard_connections();
        let Some(dialog) = &self.compare_dialog else {
            return;
        };
        for id in dialog.needed_connections() {
            if self.engine.session_status(&id).is_none() {
                self.connect(&id);
            }
        }
        let Some(dialog) = &mut self.compare_dialog else {
            return;
        };
        match dialog.show(ctx, &self.engine, &conns) {
            CompareEvent::None => {}
            CompareEvent::Close => self.compare_dialog = None,
            CompareEvent::OpenScript { conn, title, text } => {
                self.new_tab(Some(title), Some(conn), &text);
            }
        }
    }
}
