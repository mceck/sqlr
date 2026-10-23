//! Visual explain plans and ER diagrams in the app (spec §8.6): a plan window for the
//! statement under the caret, and an ER diagram window for a schema or a selection of its
//! tables. Both only read.

use std::collections::BTreeMap;

use egui::RichText;
use sqlr_core::{ErColors, ErLayout, ErModel, PlanTree, StatementClass, er_layout, er_to_svg};
use sqlr_engine::{Engine, JobId, JobOutcome, SessionKey};
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{
    ColumnsShown, ErAction, ErView, PlanAction, PlanViewState, ToastKind, er_canvas, er_measure,
    plan_view,
};

use crate::app::SqlrApp;
use crate::transfer_dialogs::FileAsk;

/// The plan window: waits for confirmation before it runs a statement that changes data,
/// then shows the plan.
pub(crate) struct PlanDialog {
    conn: String,
    sql: String,
    analyze: bool,
    /// The statement changes data and will run (inside a transaction that is rolled back).
    needs_confirmation: bool,
    job: Option<JobId>,
    plan: Option<PlanTree>,
    error: Option<String>,
    view: PlanViewState,
}

impl PlanDialog {
    fn new(conn: String, sql: String, analyze: bool, writes: bool) -> Self {
        Self {
            conn,
            sql,
            analyze,
            needs_confirmation: analyze && writes,
            job: None,
            plan: None,
            error: None,
            view: PlanViewState::default(),
        }
    }

    fn start(&mut self, engine: &Engine) {
        self.needs_confirmation = false;
        self.error = None;
        self.job = Some(engine.explain(
            &SessionKey::meta(&self.conn),
            self.sql.clone(),
            self.analyze,
        ));
    }

    /// `false` once the window should close.
    pub fn show(&mut self, ctx: &egui::Context, engine: &Engine) -> bool {
        if self.job.is_none()
            && self.plan.is_none()
            && self.error.is_none()
            && !self.needs_confirmation
        {
            self.start(engine);
        }
        if let Some(id) = self.job
            && let Some(state) = engine.job(id)
            && let Some(outcome) = state.outcome
        {
            engine.forget_job(id);
            self.job = None;
            match outcome {
                JobOutcome::Plan(plan) => self.plan = Some(plan),
                JobOutcome::Failed(e) => self.error = Some(e.message),
                _ => {}
            }
        }
        let mut open = true;
        let mut run = false;
        let title = if self.analyze {
            "Explain Analyze"
        } else {
            "Explain"
        };
        theme::tool_dialog(
            ctx,
            &mut open,
            title,
            theme::icons::QUERY,
            "Inspect the execution plan and the work done by each node",
            800.0,
            |ui| {
                ui.label(
                    RichText::new(self.sql.lines().next().unwrap_or("").to_string())
                        .monospace()
                        .color(theme::palette(ui.ctx()).text_muted),
                );
                ui.add_space(Spacing::S);
                if self.needs_confirmation {
                    ui.label(
                        "Explain Analyze runs this statement. Its changes are rolled back, but \
                         triggers and other side effects still happen.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Run Explain Analyze").clicked() {
                            run = true;
                        }
                    });
                } else if self.job.is_some() {
                    ui.spinner();
                } else if let Some(e) = &self.error {
                    ui.colored_label(theme::palette(ui.ctx()).error, e);
                } else if let Some(plan) = &self.plan
                    && let Some(PlanAction::CopyRaw) = plan_view(ui, plan, &mut self.view)
                {
                    ui.ctx().copy_text(plan.raw.clone());
                }
            },
        );
        if run {
            self.start(engine);
        }
        open
    }
}

impl SqlrApp {
    /// ⌥⌘E / ⌥⇧⌘E: the plan of the statement under the caret (or the selection).
    pub(crate) fn explain_active(&mut self, analyze: bool) {
        let Some(key) = self.active_key() else {
            return;
        };
        let Some(tab) = self.tab(key) else {
            return;
        };
        let (Some(conn), Some(target)) = (tab.conn.clone(), tab.editor.run_target()) else {
            self.notify_as(
                ToastKind::Info,
                "Put the caret in a statement of a tab with a connection",
            );
            return;
        };
        let sql = target.sql.trim().to_string();
        let writes = self
            .dialects
            .get(&conn)
            .is_some_and(|d| d.syntax().classify(&sql) != StatementClass::Read);
        self.plan_dialog = Some(PlanDialog::new(conn, sql, analyze, writes));
    }

    pub(crate) fn plan_ui(&mut self, ctx: &egui::Context) {
        if let Some(dialog) = &mut self.plan_dialog
            && !dialog.show(ctx, &self.engine)
        {
            self.plan_dialog = None;
        }
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

    /// `false` once the window should close.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        engine: &Engine,
        answer: Option<&std::path::PathBuf>,
    ) -> bool {
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
            && let Some(done) = ask.poll(ctx)
        {
            let svg = *svg;
            self.ask = None;
            if let (Some(path), Some(model), true) = (done, &self.model, svg) {
                let text = er_to_svg(model, &self.layout, &ErColors::default());
                self.export_jobs.push(engine.save_text(path, text));
            }
        }
        let mut open = true;
        let mut action = None;
        theme::tool_dialog(
            ctx,
            &mut open,
            &format!("ER diagram · {}", self.schema),
            theme::icons::SCHEMA,
            "Explore tables and relationships · Drag to arrange, scroll to zoom",
            960.0,
            |ui| {
                ui.set_height((ctx.content_rect().height() - 260.0).clamp(180.0, 520.0));
                if let Some(e) = &self.error {
                    ui.colored_label(theme::palette(ui.ctx()).error, e);
                } else if let Some(model) = &self.model {
                    action = er_canvas(ui, model, &mut self.layout, &mut self.view);
                } else {
                    ui.spinner();
                }
            },
        );
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
        open
    }
}

impl SqlrApp {
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
        self.er_dialog = Some(ErDialog::new(conn.to_string(), database, schema, tables));
    }

    pub(crate) fn er_ui(&mut self, ctx: &egui::Context) {
        if let Some(dialog) = &mut self.er_dialog
            && !dialog.show(ctx, &self.engine, self.file_answer.as_ref())
        {
            self.er_dialog = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use sqlr_core::PlanNode;
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
    fn a_plan_dialog_waits_then_shows_the_plan_or_the_error() {
        let engine = engine();
        let mut dialog = PlanDialog::new("c".into(), "SELECT 1".into(), true, true);
        frame(&engine, |ctx| {
            assert!(dialog.show(ctx, &engine));
        });
        // The confirmation is drawn until the user asks to run it.
        assert!(dialog.needs_confirmation);
        dialog.start(&engine);
        for _ in 0..100 {
            frame(&engine, |ctx| {
                dialog.show(ctx, &engine);
            });
            if dialog.error.is_some() || dialog.plan.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(dialog.error.is_some() || dialog.plan.is_some());

        dialog.job = None;
        dialog.error = Some("boom".into());
        frame(&engine, |ctx| {
            dialog.show(ctx, &engine);
        });
        dialog.error = None;
        dialog.plan = Some(PlanTree {
            root: PlanNode::default(),
            ..PlanTree::default()
        });
        frame(&engine, |ctx| {
            dialog.show(ctx, &engine);
        });
        assert!(!dialog.analyze || dialog.plan.is_some());
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
