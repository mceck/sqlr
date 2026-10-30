//! Visual explain plans and ER diagrams in the app (spec §8.6): a plan pane on the tab's
//! own session, and an ER diagram window for a schema or a selection of its tables. Both
//! only read.

use std::collections::BTreeMap;

use sqlr_core::{ErColors, ErLayout, ErModel, StatementClass, er_layout, er_to_svg};
use sqlr_engine::{Engine, JobId, JobOutcome};
use sqlr_ui::ToastKind;
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{ColumnsShown, ErAction, ErView, er_canvas, er_measure};

use crate::app::{SqlrApp, tab_mut};
use crate::transfer_dialogs::FileAsk;

impl SqlrApp {
    /// ⌥⌘E / ⌥⇧⌘E: the plan of the statement under the caret (or the selection), in the
    /// active tab's Plan pane.
    pub(crate) fn explain_active(&mut self, analyze: bool) {
        let Some(key) = self.active_key() else {
            return;
        };
        let (unsupported, conn, target) = match self.tab(key) {
            None => return,
            Some(tab) => (
                tab.redis.is_some()
                    || tab.doc.is_some()
                    || tab.object.is_some()
                    || tab.admin.is_some()
                    || tab.ddl.is_some(),
                tab.conn.clone(),
                tab.editor.run_target(),
            ),
        };
        if unsupported {
            self.notify_as(ToastKind::Info, "Open a SQL tab to explain a statement");
            return;
        }
        let Some(conn) = conn else {
            self.notify_as(ToastKind::Info, "Choose a connection for this tab first");
            return;
        };
        if !self.explain_supported(&conn) {
            self.notify_as(
                ToastKind::Info,
                "This database does not provide query plans",
            );
            return;
        }
        let Some(target) = target else {
            self.notify_as(ToastKind::Info, "Put the caret in a statement to explain");
            return;
        };
        let writes = self
            .dialects
            .get(&conn)
            .is_none_or(|d| d.syntax().classify(&target.sql) != StatementClass::Read);
        if let Some(tab) = tab_mut(&mut self.dock, key) {
            tab.start_explain(&self.engine, &conn, target.sql, analyze, writes);
        }
    }

    /// Whether the connection's driver declares query plans.
    fn explain_supported(&self, conn: &str) -> bool {
        self.connections
            .iter()
            .find(|c| c.id == conn)
            .is_some_and(|c| {
                self.manifest(&c.driver)
                    .is_some_and(|m| m.capabilities.explain)
            })
    }
}

/// The ER diagram window.
pub(crate) struct ErDialog {
    conn: String,
    database: Option<String>,
    schema: String,
    tables: Option<Vec<String>>,
    model_job: Option<JobId>,
    layout_job: Option<JobId>,
    model: Option<ErModel>,
    layout: ErLayout,
    /// The saved positions by table name, waiting for the model.
    saved: Option<BTreeMap<String, (f32, f32)>>,
    saved_loaded: bool,
    view: ErView,
    error: Option<String>,
    ask: Option<(bool, FileAsk)>,
    export_jobs: Vec<JobId>,
}

impl ErDialog {
    /// What reopening the diagram needs once its tab closed.
    pub(crate) fn closed_tab(&self) -> crate::closed_tabs::ClosedTab {
        crate::closed_tabs::ClosedTab::Er {
            conn: self.conn.clone(),
            database: self.database.clone(),
            schema: self.schema.clone(),
            tables: self.tables.clone(),
        }
    }

    fn new(
        conn: String,
        database: Option<String>,
        schema: String,
        tables: Option<Vec<String>>,
    ) -> Self {
        Self {
            conn,
            database,
            schema,
            tables,
            model_job: None,
            layout_job: None,
            model: None,
            layout: ErLayout::default(),
            saved: None,
            saved_loaded: false,
            view: ErView::default(),
            error: None,
            ask: None,
            export_jobs: Vec::new(),
        }
    }

    /// Where this diagram's layout is saved: connection, database, schema and the table set.
    fn key(&self) -> String {
        let mut names = self.tables.clone().unwrap_or_default();
        names.sort();
        let hash = names
            .iter()
            .flat_map(|n| n.bytes().chain(std::iter::once(0)))
            .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
            });
        format!(
            "{}|{}|{}|{hash:016x}",
            self.conn,
            self.database.clone().unwrap_or_default(),
            self.schema
        )
    }

    fn relayout(&mut self) {
        if let Some(model) = &self.model {
            let mode = self.view.columns;
            self.layout = er_layout(model, &|t| er_measure(t, mode));
            self.view.fit = true;
        }
    }

    fn apply_saved(&mut self) {
        let (Some(model), Some(saved)) = (&self.model, &self.saved) else {
            return;
        };
        let mode = self.view.columns;
        self.layout = er_layout(model, &|t| er_measure(t, mode));
        let mut hits = 0;
        for (i, t) in model.tables.iter().enumerate() {
            if let Some(pos) = saved.get(&t.table.name) {
                self.layout.positions[i] = *pos;
                hits += 1;
            }
        }
        if hits == 0 {
            self.view.fit = true;
        }
    }

    fn save(&self, engine: &Engine) {
        let Some(model) = &self.model else {
            return;
        };
        let map: BTreeMap<&str, [f32; 2]> = model
            .tables
            .iter()
            .zip(&self.layout.positions)
            .map(|(t, p)| (t.table.name.as_str(), [p.0, p.1]))
            .collect();
        if let Ok(json) = serde_json::to_string(&map) {
            engine.save_er_layout(&self.key(), &json);
        }
    }

    #[cfg(test)]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        engine: &Engine,
        answer: Option<&std::path::PathBuf>,
    ) -> bool {
        egui::Area::new(egui::Id::new("er_test_view")).show(ctx, |ui| {
            self.show_ui(ui, engine, answer);
        });
        true
    }

    pub(crate) fn close(&mut self, engine: &Engine) {
        for job in [self.model_job.take(), self.layout_job.take()]
            .into_iter()
            .flatten()
        {
            engine.cancel_job(job);
            engine.forget_job(job);
        }
    }

    pub fn show_ui(
        &mut self,
        ui: &mut egui::Ui,
        engine: &Engine,
        answer: Option<&std::path::PathBuf>,
    ) {
        let ctx = ui.ctx().clone();
        if self.model_job.is_none() && self.model.is_none() && self.error.is_none() {
            self.model_job = Some(engine.er_model(
                &self.conn,
                self.database.clone(),
                self.schema.clone(),
                self.tables.clone(),
            ));
            self.layout_job = Some(engine.load_er_layout(&self.key()));
        }
        if let Some(id) = self.layout_job
            && let Some(outcome) = engine.job(id).and_then(|s| s.outcome)
        {
            engine.forget_job(id);
            self.layout_job = None;
            if let JobOutcome::ErLayout(Some(json)) = outcome {
                self.saved = serde_json::from_str::<BTreeMap<String, [f32; 2]>>(&json)
                    .ok()
                    .map(|m| m.into_iter().map(|(k, v)| (k, (v[0], v[1]))).collect());
            }
            self.saved_loaded = true;
        }
        if let Some(id) = self.model_job
            && let Some(outcome) = engine.job(id).and_then(|s| s.outcome)
        {
            engine.forget_job(id);
            self.model_job = None;
            match outcome {
                JobOutcome::ErModel(model) => {
                    self.model = Some(model);
                    self.relayout();
                }
                JobOutcome::Failed(e) => self.error = Some(e.message),
                _ => {}
            }
        }
        if self.model.is_some() && self.saved_loaded && self.saved.is_some() {
            self.apply_saved();
            self.saved = None;
        }
        if let Some((svg, ask)) = &mut self.ask
            && let Some(done) = ask.poll(&ctx)
        {
            let svg = *svg;
            self.ask = None;
            if let (Some(path), Some(model), true) = (done, &self.model, svg) {
                let text = er_to_svg(model, &self.layout, &ErColors::default());
                self.export_jobs.push(engine.save_text(path, text));
            }
        }
        let action = egui::Frame::NONE
            .inner_margin(Spacing::M)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                if let Some(e) = &self.error {
                    ui.colored_label(theme::palette(ui.ctx()).error, e);
                    None
                } else if let Some(model) = &self.model {
                    er_canvas(ui, model, &mut self.layout, &mut self.view)
                } else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading tables and relationships…");
                    });
                    None
                }
            })
            .inner;
        match action {
            Some(ErAction::Moved(..)) => self.save(engine),
            Some(ErAction::Relayout) => {
                self.relayout();
                self.save(engine);
            }
            Some(ErAction::ExportSvg) => {
                self.ask = Some((
                    true,
                    FileAsk::save(
                        answer,
                        "Export the diagram",
                        &format!("{}.svg", self.schema),
                    ),
                ));
            }
            Some(ErAction::ExportPng) => {
                // A PNG needs the GPU screenshot of the canvas; the SVG is exact and opens
                // everywhere.
                self.error = None;
            }
            Some(ErAction::OpenTable(_)) | None => {}
        }
        let _ = ColumnsShown::All;
    }
}

impl SqlrApp {
    /// Where the ER diagram tab of these tables is, when one is open.
    pub(crate) fn er_diagram_tab(
        &self,
        conn: &str,
        database: Option<&str>,
        schema: &str,
        tables: Option<&[String]>,
    ) -> Option<egui_dock::TabPath> {
        self.dock.find_tab_from(|t| {
            t.er.as_ref().is_some_and(|er| {
                er.conn == conn
                    && er.database.as_deref() == database
                    && er.schema == schema
                    && er.tables.as_deref() == tables
            })
        })
    }

    /// "ER diagram" on a schema or database node: the whole schema, or `tables` of it.
    pub(crate) fn open_er_diagram(
        &mut self,
        conn: &str,
        database: Option<String>,
        schema: String,
        tables: Option<Vec<String>>,
    ) {
        if self.engine.session_status(conn).is_none() {
            self.connect(conn);
        }
        if let Some(path) =
            self.er_diagram_tab(conn, database.as_deref(), &schema, tables.as_deref())
        {
            self.focus_path(path);
            return;
        }
        let title = format!("ER · {schema}");
        let diagram = ErDialog::new(conn.to_string(), database, schema, tables);
        let key = self.new_tab(Some(title), Some(conn.into()), "");
        if let Some(tab) = crate::app::tab_mut(&mut self.dock, key) {
            tab.er = Some(Box::new(diagram));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use sqlr_engine::DriverRegistry;

    fn engine() -> Engine {
        Engine::new(DriverRegistry::default(), Arc::new(|| {}))
    }

    fn frame(engine: &Engine, show: impl FnMut(&egui::Context)) {
        let ctx = egui::Context::default();
        let mut once = Some(show);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let show = once.as_mut().expect("one frame");
            show(ui.ctx());
        });
        // No renderer consumes the font atlas upload here.
        out.textures_delta.clear();
        let _ = engine;
    }

    #[test]
    fn er_opens_as_a_document_tab() {
        let (mut app, _dir) = crate::palette::tests::bare_app();
        app.open_er_diagram("missing", None, "main".into(), None);
        assert_eq!(app.tab_titles(), vec!["ER · main"]);
    }

    #[test]
    fn an_er_layout_key_covers_the_table_set() {
        let all = ErDialog::new("c".into(), None, "s".into(), None);
        let one = ErDialog::new("c".into(), None, "s".into(), Some(vec!["a".into()]));
        let two = ErDialog::new(
            "c".into(),
            Some("db".into()),
            "s".into(),
            Some(vec!["b".into(), "a".into()]),
        );
        assert_ne!(all.key(), one.key());
        assert_eq!(one.key(), one.key());
        assert!(two.key().starts_with("c|db|s|"));
        // The table names are sorted before hashing.
        let reversed = ErDialog::new(
            "c".into(),
            Some("db".into()),
            "s".into(),
            Some(vec!["a".into(), "b".into()]),
        );
        assert_eq!(two.key(), reversed.key());
    }

    #[test]
    fn saved_positions_are_applied_and_otherwise_fit() {
        let mut dialog = ErDialog::new("c".into(), None, "s".into(), None);
        dialog.model = Some(ErModel::default());
        dialog.relayout();
        assert!(dialog.view.fit);

        let mut saved = BTreeMap::new();
        saved.insert("missing".to_string(), (10.0, 20.0));
        dialog.saved = Some(saved);
        dialog.apply_saved();
        assert!(dialog.view.fit);

        dialog.saved = Some(BTreeMap::new());
        dialog.apply_saved();
        assert!(dialog.view.fit);
        dialog.save(&engine());
    }

    #[test]
    fn the_er_dialog_draws_its_model_and_error() {
        let engine = engine();
        let mut dialog = ErDialog::new("c".into(), None, "main".into(), Some(vec!["a".into()]));
        dialog.saved_loaded = true;
        dialog.model = Some(ErModel::default());
        dialog.relayout();
        frame(&engine, |ctx| {
            assert!(dialog.show(ctx, &engine, None));
        });

        dialog.error = Some("no schema".into());
        dialog.model = None;
        frame(&engine, |ctx| {
            dialog.show(ctx, &engine, None);
        });

        // A dialog without a model starts the jobs and shows its spinner.
        let mut waiting = ErDialog::new("c".into(), None, "main".into(), None);
        frame(&engine, |ctx| {
            waiting.show(ctx, &engine, None);
        });
        assert!(waiting.model_job.is_some() || waiting.error.is_some());
    }

    #[test]
    fn exporting_svg_from_the_dialog_saves_a_file() {
        let engine = engine();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagram.svg");
        let mut dialog = ErDialog::new("c".into(), None, "main".into(), None);
        dialog.saved_loaded = true;
        dialog.model = Some(ErModel::default());
        dialog.relayout();
        dialog.ask = Some((
            true,
            FileAsk::save(Some(&path), "Export the diagram", "diagram.svg"),
        ));
        frame(&engine, |ctx| {
            dialog.show(ctx, &engine, Some(&path));
        });
        assert!(dialog.ask.is_none());
        assert_eq!(dialog.export_jobs.len(), 1);
    }
}
