//! "Generate data…" (spec §8.6): fill a table with generated rows. The generators are
//! chosen per column from its name and type (foreign keys pick existing keys), previewed on
//! 20 rows and written through the transfer pipeline, with progress and a cancel.

use std::collections::BTreeMap;

use egui::{DragValue, RichText};
use sqlr_core::{
    ColumnGen, ConstraintDef, DataGenSpec, DatagenError, Endpoint, Generator, ObjectDef,
    ObjectKind, ObjectRef, RowGenerator, TableColumn, TableDef, TableRef, TransferOptions,
    TransferSpec, Value, default_generator,
};
use sqlr_engine::{Engine, JobId, JobOutcome, JobProgress};
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{ToastKind, datagen_table};

use crate::app::SqlrApp;
use crate::job_failure::JobFailure;

pub(crate) enum DatagenEvent {
    None,
    Close,
}

pub(crate) struct DatagenDialog {
    conn: String,
    table: TableRef,
    load: Option<JobId>,
    columns: Vec<ColumnGen>,
    types: Vec<(String, String)>,
    rows: u64,
    seed: u64,
    preview: Option<Result<Vec<Vec<Value>>, DatagenError>>,
    run: Option<JobId>,
    progress: JobProgress,
    outcome: Option<Result<String, JobFailure>>,
    error: Option<String>,
    confirm_insert: bool,
}

/// The generators a table's columns start with.
fn initial_columns(def: &TableDef) -> Vec<ColumnGen> {
    let unique_columns: Vec<&String> = def
        .constraints
        .iter()
        .filter_map(|c| match c {
            ConstraintDef::PrimaryKey { columns, .. } | ConstraintDef::Unique { columns, .. }
                if columns.len() == 1 =>
            {
                columns.first()
            }
            _ => None,
        })
        .collect();
    def.columns
        .iter()
        .map(|c| {
            let fk = def.constraints.iter().find(|k| {
                matches!(k, ConstraintDef::ForeignKey { columns, .. } if columns.contains(&c.name))
            });
            let column = TableColumn {
                automatic: c.identity.is_some(),
                name: c.name.clone(),
                type_name: c.type_name.clone(),
                nullable: c.nullable,
                default: c.default.clone(),
                generated: c.generated.is_some(),
            };
            ColumnGen {
                column: c.name.clone(),
                generator: default_generator(&column, fk),
                null_ratio: 0.0,
                unique: unique_columns.contains(&&c.name),
            }
        })
        .collect()
}

impl DatagenDialog {
    fn new(engine: &Engine, conn: &str, table: TableRef) -> Self {
        let object = ObjectRef::relation(ObjectKind::Table, &table);
        Self {
            conn: conn.to_string(),
            load: Some(engine.load_object(conn, object)),
            table,
            columns: Vec::new(),
            types: Vec::new(),
            rows: 100,
            seed: 42,
            preview: None,
            run: None,
            progress: JobProgress::default(),
            outcome: None,
            error: None,
            confirm_insert: false,
        }
    }

    fn spec(&self, rows: u64) -> DataGenSpec {
        DataGenSpec {
            table: self.table.clone(),
            rows,
            seed: self.seed,
            columns: self.columns.clone(),
        }
    }

    fn transfer(&self) -> TransferSpec {
        TransferSpec {
            source: Endpoint::Generator(self.spec(self.rows)),
            target: Endpoint::Table {
                conn: self.conn.clone(),
                table: self.table.clone(),
            },
            mapping: Vec::new(),
            options: TransferOptions {
                batch: 1000,
                ..TransferOptions::default()
            },
        }
    }

    /// 20 rows, with made-up keys standing in for a referenced table's.
    fn make_preview(&mut self) {
        let mut pools = BTreeMap::new();
        for c in &self.columns {
            if let Generator::ForeignKey { table, column } = &c.generator {
                pools.insert(
                    RowGenerator::pool_key(table, column),
                    (1..=5).map(Value::Int).collect(),
                );
            }
        }
        let spec = self.spec(20);
        let keep: Vec<usize> = (0..spec.columns.len())
            .filter(|&i| spec.columns[i].generator != Generator::Default)
            .collect();
        self.preview = Some(RowGenerator::new(&spec, pools).next_batch(20).map(|rows| {
            rows.into_iter()
                .map(|r| keep.iter().map(|&i| r[i].clone()).collect())
                .collect()
        }));
    }

    pub fn show(&mut self, ctx: &egui::Context, engine: &Engine) -> DatagenEvent {
        if let Some(id) = self.load
            && let Some(outcome) = engine.job(id).and_then(|s| s.outcome)
        {
            engine.forget_job(id);
            self.load = None;
            match outcome {
                JobOutcome::Object(ObjectDef::Table(def)) => {
                    self.columns = initial_columns(&def);
                    self.types = def
                        .columns
                        .iter()
                        .map(|c| (c.name.clone(), c.type_name.clone()))
                        .collect();
                }
                JobOutcome::Failed(e) => self.error = Some(e.message),
                _ => {}
            }
        }
        if let Some(id) = self.run
            && let Some(state) = engine.job(id)
        {
            self.progress = state.progress;
            if let Some(outcome) = state.outcome {
                engine.forget_job(id);
                self.run = None;
                self.outcome = Some(match outcome {
                    JobOutcome::Imported {
                        written, rejected, ..
                    } => Ok(format!("Wrote {written} rows, rejected {}", rejected.len())),
                    JobOutcome::Failed(e) => Err(e.into()),
                    JobOutcome::Cancelled => Err(JobFailure::Cancelled),
                    _ => Ok("Done".into()),
                });
            }
        }
        let event = DatagenEvent::None;
        let title = format!("Generate data for {}", self.table.name);
        let response = theme::Dialog::new("datagen_dialog", theme::icons::CREATE, &title)
            .subtitle("Configure generators, preview sample rows, then write to the table")
            .width(900.0)
            .show(ctx, |d| {
            d.body(|ui| {
                let p = theme::palette(ui.ctx());
                if let Some(e) = &self.error {
                    ui.colored_label(p.error, e.to_string());
                    return;
                }
                if self.load.is_some() {
                    ui.spinner();
                    return;
                }
                ui.horizontal(|ui| {
                    ui.label("Rows");
                    ui.add(
                        DragValue::new(&mut self.rows)
                            .range(1..=100_000_000)
                            .speed(100.0),
                    );
                    ui.label("Seed");
                    ui.add(DragValue::new(&mut self.seed));
                    ui.label(
                        RichText::new("The same seed gives the same rows.")
                            .small()
                            .color(p.text_muted),
                    );
                });
                ui.add_space(Spacing::L);
                theme::section_title(ui, "Column generators");
                theme::hint(
                    ui,
                    "Uncheck a column to let the database fill it. NULL % controls how often values are left empty.",
                );
                ui.add_space(Spacing::M);
                egui::ScrollArea::both().max_height(260.0).show(ui, |ui| {
                    if datagen_table(ui, &mut self.columns, &self.types) {
                        self.preview = None;
                    }
                });
                match &self.outcome {
                    Some(Ok(m)) => {
                        ui.colored_label(p.success, m);
                    }
                    Some(Err(m)) => {
                        ui.colored_label(p.error, m.to_string());
                    }
                    None => {}
                }
                match &self.preview {
                    Some(Ok(rows)) => {
                        egui::ScrollArea::both()
                            .id_salt("datagen-preview")
                            .max_height(180.0)
                            .show(ui, |ui| {
                                egui::Grid::new("datagen-preview-grid").striped(true).show(
                                    ui,
                                    |ui| {
                                        for row in rows {
                                            for v in row {
                                                let text: String =
                                                    v.display().chars().take(30).collect();
                                                ui.label(text);
                                            }
                                            ui.end_row();
                                        }
                                    },
                                );
                            });
                    }
                    Some(Err(e)) => {
                        ui.colored_label(p.error, e.to_string());
                    }
                    None => {}
                }
            });
            let ready = self.error.is_none() && self.load.is_none();
            d.footer(|f| {
                if !ready {
                    f.cancel_as("Close");
                    return;
                }
                if f.secondary("Preview", true) {
                    self.make_preview();
                }
                if let Some(id) = self.run {
                    f.left().spinner();
                    f.left().label(format!("{} rows", self.progress.rows));
                    if f.button("Stop", true) {
                        engine.cancel_job(id);
                    }
                    return;
                }
                if f.primary(&format!("Insert {} rows", self.rows), None) { self.confirm_insert = true; }
                f.cancel_as(if self.outcome.is_some() {
                    "Close"
                } else {
                    "Cancel"
                });
            });
            });
        if self.confirm_insert {
            let spec = crate::confirm::ConfirmSpec::new(
                egui::Id::new("confirm_generated_data"),
                theme::icons::ROWS,
                "Insert generated rows",
                "Insert rows",
            );
            if let Some(yes) = crate::confirm::confirm_modal(
                ctx,
                spec,
                |ui| {
                    ui.label(format!(
                        "Insert {} generated rows into {}?",
                        self.rows, self.table.name
                    ));
                },
                |_| {},
            ) {
                self.confirm_insert = false;
                if yes {
                    self.outcome = None;
                    self.run = Some(engine.transfer(self.transfer()));
                }
            }
        }
        if response.closed {
            if let Some(id) = self.run.take() {
                engine.cancel_job(id);
                engine.forget_job(id);
            }
            return DatagenEvent::Close;
        }
        event
    }
}

impl SqlrApp {
    /// "Generate data…" on an explorer table.
    pub(crate) fn open_datagen(&mut self, conn: &str, node: &sqlr_core::Node) {
        let Some(table) = self.node_table(conn, node) else {
            self.notify_as(
                ToastKind::Info,
                format!("Cannot generate data for {}", node.name),
            );
            return;
        };
        self.datagen_dialog = Some(DatagenDialog::new(&self.engine, conn, table));
    }

    pub(crate) fn datagen_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.datagen_dialog else {
            return;
        };
        match dialog.show(ctx, &self.engine) {
            DatagenEvent::None => {}
            DatagenEvent::Close => self.datagen_dialog = None,
        }
    }
}
