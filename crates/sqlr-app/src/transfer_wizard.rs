//! The data transfer wizard (spec §8.5): a source (table, query or file), a target
//! (existing table, new table or file), the column mapping, the options, a review and the
//! run. The engine does the work (`Engine::transfer`); this module only collects the
//! choices, shows what the source holds and follows the job.

use std::path::{Path, PathBuf};

use egui::{ComboBox, DragValue, RichText, TextEdit};
use sqlr_core::{
    ColumnMapping, CsvOptions, Endpoint, Node, ObjectDef, TableColumn, TableRef, TextFormat,
    TransferOptions, TransferSpec, new_table_def,
};
use sqlr_engine::{Engine, JobId, JobOutcome, JobProgress, SessionKey, SessionStatus, SourceProbe};
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{WizardAction, WizardStep, mapping_table, wizard_footer, wizard_steps};

use crate::app::SqlrApp;
use crate::database_tools::human_bytes;
use crate::transfer_dialogs::{FileAsk, suggested_name};

/// A connection the wizard can read from or write to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConnChoice {
    pub id: String,
    pub name: String,
    pub driver: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceKind {
    Table,
    Query,
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetKind {
    Existing,
    New,
    File,
}

/// The file formats of the wizard, for reading and writing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileKind {
    Csv,
    Tsv,
    Json,
    JsonLines,
    Xlsx,
    Parquet,
    SqlInsert,
}

impl FileKind {
    const SOURCES: [FileKind; 6] = [
        FileKind::Csv,
        FileKind::Tsv,
        FileKind::Json,
        FileKind::JsonLines,
        FileKind::Xlsx,
        FileKind::Parquet,
    ];
    const TARGETS: [FileKind; 7] = [
        FileKind::Csv,
        FileKind::Tsv,
        FileKind::Json,
        FileKind::JsonLines,
        FileKind::Xlsx,
        FileKind::Parquet,
        FileKind::SqlInsert,
    ];

    fn label(self) -> &'static str {
        match self {
            FileKind::Csv => "CSV",
            FileKind::Tsv => "TSV",
            FileKind::Json => "JSON (array of objects)",
            FileKind::JsonLines => "JSON Lines",
            FileKind::Xlsx => "Excel (.xlsx)",
            FileKind::Parquet => "Parquet",
            FileKind::SqlInsert => "SQL INSERT",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            FileKind::Csv => "csv",
            FileKind::Tsv => "tsv",
            FileKind::Json => "json",
            FileKind::JsonLines => "jsonl",
            FileKind::Xlsx => "xlsx",
            FileKind::Parquet => "parquet",
            FileKind::SqlInsert => "sql",
        }
    }

    /// The format a file's extension suggests.
    pub fn of_path(path: &Path) -> Self {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "tsv" => FileKind::Tsv,
            "json" => FileKind::Json,
            "jsonl" | "ndjson" => FileKind::JsonLines,
            "xlsx" => FileKind::Xlsx,
            "parquet" => FileKind::Parquet,
            "sql" => FileKind::SqlInsert,
            _ => FileKind::Csv,
        }
    }
}

/// The schema a table of `driver` lives in unless the user says otherwise.
fn default_schema(driver: &str) -> &'static str {
    match driver {
        "postgres" => "public",
        "sqlite" => "main",
        _ => "",
    }
}

fn table_ref(schema: &str, name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some(schema.trim().to_string()).filter(|s| !s.is_empty()),
        name: name.trim().to_string(),
    }
}

/// What a native file dialog is being asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AskFor {
    Source,
    Target,
    Rejected,
}

/// What the wizard tells the app.
pub(crate) enum WizardEvent {
    None,
    Close,
}

pub(crate) struct TransferWizard {
    step: usize,
    source_kind: SourceKind,
    s_conn: String,
    s_schema: String,
    s_table: String,
    s_sql: String,
    s_path: String,
    s_file: FileKind,
    s_sheet: Option<String>,
    s_header: bool,
    s_encoding: String,
    target_kind: TargetKind,
    t_conn: String,
    t_schema: String,
    t_table: String,
    t_path: String,
    t_file: FileKind,
    ask: Option<(AskFor, FileAsk)>,
    probe_job: Option<JobId>,
    probe_for: String,
    observed_source: (String, f64),
    confirm_run: bool,
    /// What the source holds, for the inputs it was read with.
    probe: Option<(String, SourceProbe)>,
    preview_rows: Option<sqlr_core::RowBuffer>,
    preview_grid: sqlr_ui::GridState,
    preview_for: String,
    probe_error: Option<String>,
    mapping: Vec<ColumnMapping>,
    /// The existing target's columns were asked for.
    described: Option<(String, TableRef)>,
    options: TransferOptions,
    upsert: bool,
    upsert_text: String,
    rejected_path: String,
    /// The `CREATE TABLE` of a new target, and whether the user edited it.
    ddl: String,
    ddl_edited: bool,
    ddl_for: String,
    warnings: Vec<String>,
    run: Option<JobId>,
    progress: JobProgress,
    log: Vec<String>,
    outcome: Option<Result<String, String>>,
    /// Look into the source as soon as its connections are up (an edited task).
    autoprobe: bool,
    /// The mapping and the edited DDL come from a task: the first look keeps them.
    keep_mapping: bool,
}

impl TransferWizard {
    pub fn new(conns: &[ConnChoice]) -> Self {
        let first = conns.first();
        let driver = first.map_or("", |c| c.driver.as_str());
        let id = first.map(|c| c.id.clone()).unwrap_or_default();
        Self {
            step: 0,
            source_kind: SourceKind::Table,
            s_conn: id.clone(),
            s_schema: default_schema(driver).into(),
            s_table: String::new(),
            s_sql: String::new(),
            s_path: String::new(),
            s_file: FileKind::Csv,
            s_sheet: None,
            s_header: true,
            s_encoding: String::new(),
            target_kind: TargetKind::Existing,
            t_conn: conns.get(1).map_or(id, |c| c.id.clone()),
            t_schema: default_schema(conns.get(1).map_or(driver, |c| c.driver.as_str())).into(),
            t_table: String::new(),
            t_path: String::new(),
            t_file: FileKind::Csv,
            ask: None,
            probe_job: None,
            probe_for: String::new(),
            observed_source: (String::new(), 0.0),
            confirm_run: false,
            probe: None,
            preview_rows: None,
            preview_grid: sqlr_ui::GridState::preview(),
            preview_for: String::new(),
            probe_error: None,
            mapping: Vec::new(),
            described: None,
            options: TransferOptions::default(),
            upsert: false,
            upsert_text: String::new(),
            rejected_path: String::new(),
            ddl: String::new(),
            ddl_edited: false,
            ddl_for: String::new(),
            warnings: Vec::new(),
            run: None,
            progress: JobProgress::default(),
            log: Vec::new(),
            outcome: None,
            autoprobe: false,
            keep_mapping: false,
        }
    }

    /// A wizard whose source is `table` of `conn` (the explorer's "Transfer data…").
    pub fn for_table(conns: &[ConnChoice], conn: &str, table: TableRef) -> Self {
        let mut wizard = Self::new(conns);
        wizard.s_conn = conn.to_string();
        wizard.s_schema = table.schema.clone().unwrap_or_default();
        // Copying into a table of the same name is the usual wish.
        wizard.t_table = table.name.clone();
        wizard.s_table = table.name;
        // The target defaults to another connection when there is one.
        if let Some(other) = conns.iter().find(|c| c.id != conn) {
            wizard.t_conn = other.id.clone();
            wizard.t_schema = default_schema(&other.driver).into();
        } else {
            wizard.t_conn = conn.to_string();
        }
        wizard
    }

    fn driver_of<'a>(&self, conns: &'a [ConnChoice], id: &str) -> &'a str {
        conns
            .iter()
            .find(|c| c.id == id)
            .map_or("", |c| c.driver.as_str())
    }

    pub(crate) fn source_endpoint(&self) -> Option<Endpoint> {
        match self.source_kind {
            SourceKind::Table => {
                if self.s_conn.is_empty() || self.s_table.trim().is_empty() {
                    return None;
                }
                Some(Endpoint::Table {
                    conn: self.s_conn.clone(),
                    table: table_ref(&self.s_schema, &self.s_table),
                })
            }
            SourceKind::Query => {
                if self.s_conn.is_empty() || self.s_sql.trim().is_empty() {
                    return None;
                }
                Some(Endpoint::Query {
                    conn: self.s_conn.clone(),
                    database: None,
                    sql: self.s_sql.trim().to_string(),
                })
            }
            SourceKind::File => {
                let path = PathBuf::from(self.s_path.trim());
                if self.s_path.trim().is_empty() {
                    return None;
                }
                Some(match self.s_file {
                    FileKind::Xlsx => Endpoint::Workbook {
                        path,
                        sheet: self.s_sheet.clone(),
                        header: self.s_header,
                    },
                    FileKind::Parquet => Endpoint::Parquet { path },
                    FileKind::Csv => Endpoint::File {
                        path,
                        format: TextFormat::Csv(CsvOptions {
                            header: self.s_header,
                            ..CsvOptions::default()
                        }),
                    },
                    FileKind::Tsv => Endpoint::File {
                        path,
                        format: TextFormat::Tsv,
                    },
                    FileKind::Json => Endpoint::File {
                        path,
                        format: TextFormat::Json,
                    },
                    FileKind::JsonLines | FileKind::SqlInsert => Endpoint::File {
                        path,
                        format: TextFormat::JsonLines,
                    },
                })
            }
        }
    }

    pub(crate) fn target_endpoint(&self) -> Option<Endpoint> {
        match self.target_kind {
            TargetKind::Existing | TargetKind::New => {
                if self.t_conn.is_empty() || self.t_table.trim().is_empty() {
                    return None;
                }
                let table = table_ref(&self.t_schema, &self.t_table);
                Some(if self.target_kind == TargetKind::Existing {
                    Endpoint::Table {
                        conn: self.t_conn.clone(),
                        table,
                    }
                } else {
                    Endpoint::NewTable {
                        conn: self.t_conn.clone(),
                        table,
                        ddl: (self.ddl_edited && !self.ddl.trim().is_empty())
                            .then(|| self.ddl.clone()),
                    }
                })
            }
            TargetKind::File => {
                let path = PathBuf::from(self.t_path.trim());
                if self.t_path.trim().is_empty() {
                    return None;
                }
                Some(match self.t_file {
                    FileKind::Xlsx => Endpoint::Workbook {
                        path,
                        sheet: None,
                        header: true,
                    },
                    FileKind::Parquet => Endpoint::Parquet { path },
                    other => Endpoint::File {
                        format: match other {
                            FileKind::Tsv => TextFormat::Tsv,
                            FileKind::Json => TextFormat::Json,
                            FileKind::JsonLines => TextFormat::JsonLines,
                            FileKind::SqlInsert => TextFormat::SqlInsert {
                                table: path
                                    .file_stem()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "data".into()),
                            },
                            _ => TextFormat::Csv(CsvOptions::default()),
                        },
                        path,
                    },
                })
            }
        }
    }

    /// The transfer the choices describe; the reason when they do not yet.
    pub(crate) fn spec(&self) -> Result<TransferSpec, String> {
        let source = self.source_endpoint().ok_or("The source is incomplete")?;
        let target = self.target_endpoint().ok_or("The target is incomplete")?;
        let table_target = self.target_kind != TargetKind::File;
        if table_target && !self.mapping.iter().any(|m| m.target.is_some()) {
            return Err("No column is mapped".into());
        }
        let mut options = self.options.clone();
        options.upsert_key = (self.upsert && table_target).then(|| {
            self.upsert_text
                .split(',')
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
                .collect()
        });
        if options.upsert_key.as_ref().is_some_and(Vec::is_empty) {
            return Err("Name the upsert key columns".into());
        }
        options.rejected_file = (!self.rejected_path.trim().is_empty())
            .then(|| PathBuf::from(self.rejected_path.trim()));
        options.encoding = (self.source_kind == SourceKind::File
            && !self.s_encoding.trim().is_empty())
        .then(|| self.s_encoding.trim().to_string());
        Ok(TransferSpec {
            source,
            target,
            mapping: if table_target {
                self.mapping.clone()
            } else {
                Vec::new()
            },
            options,
        })
    }

    /// Identifies the source inputs, so a stale probe is dropped when they change.
    fn source_key(&self) -> String {
        format!(
            "{:?}|{}|{}|{}|{}|{}|{:?}|{:?}|{}|{}",
            self.source_kind,
            self.s_conn,
            self.s_schema,
            self.s_table,
            self.s_sql,
            self.s_path,
            self.s_file,
            self.s_sheet,
            self.s_header,
            self.s_encoding
        )
    }

    fn probe_is_current(&self) -> bool {
        self.probe
            .as_ref()
            .is_some_and(|(key, _)| *key == self.source_key())
    }

    fn start_probe(&mut self, engine: &Engine) {
        let Some(source) = self.source_endpoint() else {
            return;
        };
        let options = TransferOptions {
            encoding: (!self.s_encoding.trim().is_empty())
                .then(|| self.s_encoding.trim().to_string()),
            ..TransferOptions::default()
        };
        self.probe_error = None;
        self.probe_for = self.source_key();
        self.probe_job = Some(engine.probe_source(source, options));
    }

    fn table_target(&self) -> bool {
        self.target_kind != TargetKind::File
    }

    /// The columns of the existing target table, once loaded.
    fn target_columns(&self, engine: &Engine) -> Option<Vec<TableColumn>> {
        if self.target_kind != TargetKind::Existing {
            return Some(Vec::new());
        }
        let table = table_ref(&self.t_schema, &self.t_table);
        match engine.table_info(&self.t_conn, &table)? {
            sqlr_engine::LoadState::Ready(info) => Some(info.columns),
            _ => None,
        }
    }

    fn poll(&mut self, engine: &Engine) {
        if let Some(id) = self.probe_job {
            match engine.job(id) {
                None => self.probe_job = None,
                Some(state) => {
                    if let Some(outcome) = state.outcome {
                        engine.forget_job(id);
                        self.probe_job = None;
                        match outcome {
                            JobOutcome::Probed(probe) => {
                                self.probe = Some((self.probe_for.clone(), probe));
                                if !std::mem::take(&mut self.keep_mapping) {
                                    self.mapping.clear();
                                    self.ddl_edited = false;
                                }
                            }
                            JobOutcome::Failed(e) => self.probe_error = Some(e.message),
                            _ => {}
                        }
                    }
                }
            }
        }
        if let Some(id) = self.run {
            match engine.job(id) {
                None => {
                    self.run = None;
                    self.outcome = Some(Err("The job disappeared".into()));
                }
                Some(state) => {
                    self.progress = state.progress;
                    self.log = state.log.clone();
                    if let Some(outcome) = state.outcome {
                        engine.forget_job(id);
                        self.run = None;
                        self.outcome = Some(summarize(outcome));
                    }
                }
            }
        }
    }

    fn steps(&self, engine: &Engine) -> Vec<WizardStep> {
        let mut steps = vec![
            WizardStep::new("Source", self.source_missing()),
            WizardStep::new(
                "Target",
                self.target_missing().or_else(|| {
                    if self.target_kind != TargetKind::Existing {
                        return None;
                    }
                    match engine.table_info(&self.t_conn, &table_ref(&self.t_schema, &self.t_table))
                    {
                        Some(sqlr_engine::LoadState::Ready(_)) => None,
                        Some(sqlr_engine::LoadState::Failed(e)) => {
                            Some(format!("Target table is unavailable: {}", e.message))
                        }
                        _ => Some("Checking the target table…".into()),
                    }
                }),
            ),
        ];
        if self.table_target() {
            let missing = if self.target_columns(engine).is_none() {
                Some("Loading the target table's columns…".to_string())
            } else if !self.mapping.iter().any(|m| m.target.is_some()) {
                Some("Map at least one column".to_string())
            } else {
                None
            };
            steps.push(WizardStep::new("Mapping", missing));
        }
        steps.push(WizardStep::new(
            "Options",
            (self.options.batch == 0).then(|| "Rows per batch must be at least 1".to_string()),
        ));
        let blocked = if self.run.is_some() {
            Some("Transfer is running…".into())
        } else if self.outcome.is_some() {
            Some("The transfer has run".into())
        } else {
            self.spec().err()
        };
        steps.push(WizardStep::new("Review", blocked));
        steps
    }

    /// What the Source step still needs, if anything.
    fn source_missing(&self) -> Option<String> {
        if self.probe_is_current() {
            return None;
        }
        let missing = match self.source_kind {
            SourceKind::File if self.s_path.trim().is_empty() => "Choose a source file",
            SourceKind::Table | SourceKind::Query if self.s_conn.is_empty() => {
                "Choose a source connection"
            }
            SourceKind::Table if self.s_table.trim().is_empty() => "Enter the source table",
            SourceKind::Query if self.s_sql.trim().is_empty() => "Enter the source query",
            _ if self.probe_job.is_some() => "Loading the preview…",
            _ => "Reading the source preview…",
        };
        Some(missing.to_string())
    }

    /// What the Target step still needs, if anything.
    fn target_missing(&self) -> Option<String> {
        if self.target_endpoint().is_some() {
            return None;
        }
        let missing = match self.target_kind {
            TargetKind::File => "Choose a target file",
            _ if self.t_conn.is_empty() => "Choose a target connection",
            _ => "Enter the target table",
        };
        Some(missing.to_string())
    }

    /// Returns what the app should do.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        engine: &Engine,
        conns: &[ConnChoice],
        answer: Option<&PathBuf>,
    ) -> WizardEvent {
        self.poll(engine);
        self.poll_ask(ctx);
        let key = self.source_key();
        let now = ctx.input(|i| i.time);
        if self.observed_source.0 != key {
            self.observed_source = (key.clone(), now);
            self.probe_error = None;
        }
        if self.source_endpoint().is_some()
            && !self.probe_is_current()
            && self.probe_job.is_none()
            && self.probe_for != key
            && (self.autoprobe || now - self.observed_source.1 >= 0.35)
            && self
                .needed_connections()
                .iter()
                .all(|id| matches!(engine.session_status(id), Some(SessionStatus::Connected(_))))
        {
            self.autoprobe = false;
            self.start_probe(engine);
        }
        if self.source_endpoint().is_some() && !self.probe_is_current() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let steps = self.steps(engine);
        self.step = self.step.min(steps.len() - 1);
        let event = WizardEvent::None;
        let mut open = true;
        let response = theme::Dialog::new("transfer_wizard", theme::icons::ROWS, "Transfer data")
            .subtitle(
                "Choose a source and destination, review the mapping, then start the transfer",
            )
            .width(860.0)
            .min_body_height(280.0)
            .show(ctx, |d| {
                d.top(|ui| wizard_steps(ui, &steps, self.step));
                let title = steps[self.step].title.clone();
                d.body(|ui| match title.as_str() {
                    "Source" => self.source_ui(ui, engine, conns, answer),
                    "Target" => self.target_ui(ui, engine, conns, answer),
                    "Mapping" => self.mapping_ui(ui, engine),
                    "Options" => self.options_ui(ui, answer),
                    "Review" => {
                        self.review_ui(ui, engine, conns);
                        self.run_ui(ui);
                    }
                    _ => self.run_ui(ui),
                });
                d.footer(|f| {
                    if self.step + 1 < steps.len() {
                        return wizard_footer(f, &steps, self.step);
                    }
                    let mut action = None;
                    if let Some(job) = self.run {
                        if f.button("Stop transfer", true) {
                            engine.cancel_job(job);
                        }
                    } else if self.outcome.is_none() {
                        let blocked = steps[self.step].reason.as_deref();
                        let clicked =
                            if self.options.truncate_first || self.options.disable_constraints {
                                f.danger("Run transfer", blocked)
                            } else {
                                f.primary("Run transfer", blocked)
                            };
                        if clicked {
                            action = Some(WizardAction::Finish);
                        }
                        if f.button("Back", true) {
                            action = Some(WizardAction::Back);
                        }
                    }
                    if f.cancel_as(if self.outcome.is_some() {
                        "Close"
                    } else {
                        "Cancel"
                    }) {
                        action = Some(WizardAction::Cancel);
                    }
                    action
                })
            });
        let action = response.inner;
        if response.closed {
            open = false;
        }
        match action {
            Some(WizardAction::Back) => self.step = self.step.saturating_sub(1),
            Some(WizardAction::Next) => self.step += 1,
            Some(WizardAction::Cancel) => open = false,
            Some(WizardAction::Finish) => {
                if self.options.truncate_first || self.options.disable_constraints {
                    self.confirm_run = true;
                } else {
                    self.start(engine);
                }
            }
            None => {}
        }
        if self.confirm_run {
            let spec = crate::confirm::ConfirmSpec::new(
                egui::Id::new("confirm_transfer"),
                theme::icons::WARNING,
                "Run transfer with risky options",
                "Run transfer",
            );
            let answer = crate::confirm::confirm_modal(
                ctx,
                spec,
                |ui| {
                    ui.label(format!(
                        "Target: {}",
                        describe(self.target_endpoint().as_ref(), conns)
                    ));
                    if self.options.truncate_first {
                        ui.colored_label(
                            theme::palette(ui.ctx()).error,
                            "All existing target rows will be deleted.",
                        );
                    }
                    if self.options.disable_constraints {
                        ui.colored_label(
                            theme::palette(ui.ctx()).error,
                            "Foreign key checks will be disabled during the transfer.",
                        );
                    }
                },
                |_| {},
            );
            if let Some(confirmed) = answer {
                self.confirm_run = false;
                if confirmed {
                    self.start(engine);
                }
            }
        }
        if !open {
            if let Some(id) = self.run.take() {
                engine.cancel_job(id);
                engine.forget_job(id);
            }
            return WizardEvent::Close;
        }
        event
    }

    fn start(&mut self, engine: &Engine) {
        match self.spec() {
            Ok(spec) => {
                self.log.clear();
                self.outcome = None;
                self.run = Some(engine.transfer(spec));
            }
            Err(e) => self.outcome = Some(Err(e)),
        }
    }

    fn poll_ask(&mut self, ctx: &egui::Context) {
        let Some((what, ask)) = &mut self.ask else {
            return;
        };
        let what = *what;
        match ask.poll(ctx) {
            None => {}
            Some(None) => self.ask = None,
            Some(Some(path)) => {
                self.ask = None;
                let text = path.display().to_string();
                match what {
                    AskFor::Source => {
                        self.s_file = FileKind::of_path(&path);
                        self.s_sheet = None;
                        self.s_path = text;
                    }
                    AskFor::Target => {
                        self.t_file = FileKind::of_path(&path);
                        self.t_path = text;
                    }
                    AskFor::Rejected => self.rejected_path = text,
                }
            }
        }
    }

    /// How the connection `id` stands, for the label beside its picker.
    fn conn_state(ui: &mut egui::Ui, engine: &Engine, id: &str) {
        let p = theme::palette(ui.ctx());
        match engine.session_status(id) {
            Some(SessionStatus::Connected(_)) => {
                ui.label(RichText::new("connected").small().color(p.success));
            }
            Some(SessionStatus::Failed(e)) => {
                ui.label(RichText::new(e.message).small().color(p.error));
            }
            Some(SessionStatus::Connecting) | None if !id.is_empty() => {
                ui.label(RichText::new("connecting…").small().color(p.text_muted));
            }
            _ => {}
        }
    }

    /// The connections the current choices read from or write to.
    pub(crate) fn needed_connections(&self) -> Vec<String> {
        let mut ids = Vec::new();
        if self.source_kind != SourceKind::File && !self.s_conn.is_empty() {
            ids.push(self.s_conn.clone());
        }
        if self.target_kind != TargetKind::File
            && !self.t_conn.is_empty()
            && !ids.contains(&self.t_conn)
        {
            ids.push(self.t_conn.clone());
        }
        ids
    }

    fn conn_combo(ui: &mut egui::Ui, id: &str, conns: &[ConnChoice], current: &mut String) -> bool {
        let mut changed = false;
        let shown = conns
            .iter()
            .find(|c| c.id == *current)
            .map_or("choose a connection", |c| c.name.as_str());
        ComboBox::from_id_salt(id)
            .selected_text(shown)
            .width(220.0)
            .show_ui(ui, |ui| {
                for c in conns {
                    if ui.selectable_label(*current == c.id, &c.name).clicked() {
                        *current = c.id.clone();
                        changed = true;
                    }
                }
            });
        changed
    }

    fn source_ui(
        &mut self,
        ui: &mut egui::Ui,
        engine: &Engine,
        conns: &[ConnChoice],
        answer: Option<&PathBuf>,
    ) {
        ui.horizontal(|ui| {
            for (kind, label) in [
                (SourceKind::Table, "Table"),
                (SourceKind::Query, "Query"),
                (SourceKind::File, "File"),
            ] {
                ui.selectable_value(&mut self.source_kind, kind, label);
            }
        });
        ui.add_space(Spacing::M);
        match self.source_kind {
            SourceKind::Table | SourceKind::Query => {
                ui.horizontal(|ui| {
                    ui.label("Connection");
                    if Self::conn_combo(ui, "tw-source-conn", conns, &mut self.s_conn) {
                        self.s_schema = default_schema(self.driver_of(conns, &self.s_conn)).into();
                    }
                    Self::conn_state(ui, engine, &self.s_conn);
                });
                if self.source_kind == SourceKind::Table {
                    ui.horizontal(|ui| {
                        ui.label("Schema");
                        let schemas =
                            crate::catalog_picker::schema_names(engine, &self.s_conn, None);
                        crate::catalog_picker::name(
                            ui,
                            "source_schema",
                            &mut self.s_schema,
                            &schemas,
                            "Schema",
                            160.0,
                        );
                        ui.label("Table");
                        let tables = crate::catalog_picker::table_names(
                            engine,
                            &self.s_conn,
                            &self.s_schema,
                        );
                        crate::catalog_picker::name(
                            ui,
                            "source_table",
                            &mut self.s_table,
                            &tables,
                            "Table",
                            220.0,
                        );
                    });
                } else {
                    ui.label("Statement (it must only read)");
                    ui.add(
                        TextEdit::multiline(&mut self.s_sql)
                            .code_editor()
                            .desired_rows(5)
                            .desired_width(f32::INFINITY),
                    );
                }
            }
            SourceKind::File => {
                ui.horizontal(|ui| {
                    ui.label("File");
                    ui.add(TextEdit::singleline(&mut self.s_path).desired_width(420.0));
                    if ui.button("Browse…").clicked() {
                        self.ask = Some((
                            AskFor::Source,
                            FileAsk::open(
                                answer,
                                "Choose the source file",
                                &["csv", "tsv", "json", "jsonl", "ndjson", "xlsx", "parquet"],
                            ),
                        ));
                    }
                });
                ui.horizontal(|ui| {
                    ComboBox::from_id_salt("tw-source-format")
                        .selected_text(self.s_file.label())
                        .show_ui(ui, |ui| {
                            for kind in FileKind::SOURCES {
                                ui.selectable_value(&mut self.s_file, kind, kind.label());
                            }
                        });
                    if matches!(self.s_file, FileKind::Csv | FileKind::Xlsx) {
                        ui.checkbox(&mut self.s_header, "First row holds the column names");
                    }
                });
                if matches!(
                    self.s_file,
                    FileKind::Csv | FileKind::Tsv | FileKind::Json | FileKind::JsonLines
                ) {
                    ui.horizontal(|ui| {
                        ui.label("Encoding");
                        ui.add(
                            TextEdit::singleline(&mut self.s_encoding)
                                .hint_text("detect (UTF-8, or UTF-16 by its mark)")
                                .desired_width(260.0),
                        );
                    });
                }
                if self.s_file == FileKind::Xlsx
                    && let Some((_, probe)) =
                        self.probe.as_ref().filter(|_| self.probe_is_current())
                    && probe.sheets.len() > 1
                {
                    let sheets = probe.sheets.clone();
                    ui.horizontal(|ui| {
                        ui.label("Sheet");
                        let shown = self
                            .s_sheet
                            .clone()
                            .or_else(|| sheets.first().cloned())
                            .unwrap_or_default();
                        ComboBox::from_id_salt("tw-sheet")
                            .selected_text(shown)
                            .show_ui(ui, |ui| {
                                for sheet in &sheets {
                                    ui.selectable_value(
                                        &mut self.s_sheet,
                                        Some(sheet.clone()),
                                        sheet,
                                    );
                                }
                            });
                    });
                }
            }
        }
        ui.add_space(Spacing::M);
        let ready = self.source_endpoint().is_some();
        ui.horizontal(|ui| {
            let busy = self.probe_job.is_some();
            if ui
                .add_enabled(ready && !busy, egui::Button::new("Load preview"))
                .clicked()
            {
                self.start_probe(engine);
            }
            if busy {
                ui.spinner();
                ui.label("Reading source preview…");
            }
        });
        if let Some(e) = &self.probe_error {
            ui.colored_label(theme::palette(ui.ctx()).error, e);
        }
        if let Some((_, probe)) = self.probe.as_ref().filter(|_| self.probe_is_current()) {
            ui.add_space(Spacing::S);
            ui.label(
                RichText::new(format!(
                    "{} columns, first {} rows",
                    probe.columns.len(),
                    probe.rows.len()
                ))
                .color(theme::palette(ui.ctx()).text_muted),
            );
            if self.preview_rows.is_none() || self.preview_for != self.source_key() {
                let columns: std::sync::Arc<[sqlr_core::ColumnMeta]> = probe.columns.clone().into();
                let mut rows = sqlr_core::RowBuffer::new(columns.clone());
                let mut chunk = sqlr_core::ChunkBuilder::new(columns);
                for row in probe.rows.iter().take(20) {
                    let _ = chunk.push_row(
                        &row.iter()
                            .map(sqlr_core::Value::as_cell)
                            .collect::<Vec<_>>(),
                    );
                }
                rows.push_chunk(chunk.finish());
                self.preview_rows = Some(rows);
                self.preview_for = self.source_key();
            }
            if let Some(rows) = &self.preview_rows {
                let height = (52.0 + rows.row_count().min(6) as f32 * 26.0).min(220.0);
                ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                    sqlr_ui::result_grid(ui, &mut self.preview_grid, rows, false);
                });
            }
        }
    }

    fn target_ui(
        &mut self,
        ui: &mut egui::Ui,
        engine: &Engine,
        conns: &[ConnChoice],
        answer: Option<&PathBuf>,
    ) {
        ui.horizontal(|ui| {
            for (kind, label) in [
                (TargetKind::Existing, "Existing table"),
                (TargetKind::New, "New table"),
                (TargetKind::File, "File"),
            ] {
                if ui
                    .selectable_value(&mut self.target_kind, kind, label)
                    .changed()
                {
                    self.mapping.clear();
                    self.described = None;
                }
            }
        });
        ui.add_space(Spacing::M);
        match self.target_kind {
            TargetKind::Existing | TargetKind::New => {
                ui.horizontal(|ui| {
                    ui.label("Connection");
                    if Self::conn_combo(ui, "tw-target-conn", conns, &mut self.t_conn) {
                        self.t_schema = default_schema(self.driver_of(conns, &self.t_conn)).into();
                        self.mapping.clear();
                        self.described = None;
                    }
                    Self::conn_state(ui, engine, &self.t_conn);
                });
                ui.horizontal(|ui| {
                    ui.label("Schema");
                    let schemas = crate::catalog_picker::schema_names(engine, &self.t_conn, None);
                    crate::catalog_picker::name(
                        ui,
                        "target_schema",
                        &mut self.t_schema,
                        &schemas,
                        "Schema",
                        160.0,
                    );
                    ui.label("Table");
                    let tables =
                        crate::catalog_picker::table_names(engine, &self.t_conn, &self.t_schema);
                    if crate::catalog_picker::name(
                        ui,
                        "target_table",
                        &mut self.t_table,
                        &tables,
                        "Table",
                        220.0,
                    ) {
                        self.mapping.clear();
                        self.described = None;
                    }
                });
                if self.target_kind == TargetKind::Existing && !self.t_table.trim().is_empty() {
                    let table = table_ref(&self.t_schema, &self.t_table);
                    let key = (self.t_conn.clone(), table.clone());
                    if self.described.as_ref() != Some(&key) {
                        engine.describe(&SessionKey::meta(&self.t_conn), table.clone());
                        self.described = Some(key);
                    }
                    match engine.table_info(&self.t_conn, &table) {
                        Some(sqlr_engine::LoadState::Ready(info)) => {
                            ui.colored_label(
                                theme::palette(ui.ctx()).success,
                                format!("Table exists · {} columns", info.columns.len()),
                            );
                        }
                        Some(sqlr_engine::LoadState::Failed(e)) => {
                            ui.colored_label(
                                theme::palette(ui.ctx()).error,
                                format!("Target table not found or unavailable: {}", e.message),
                            );
                        }
                        _ => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Checking target table…");
                            });
                        }
                    }
                }
                if self.target_kind == TargetKind::New {
                    ui.label(
                        RichText::new(
                            "The table is created from the source's columns, with their types \
                             mapped to this connection's dialect.",
                        )
                        .small()
                        .color(theme::palette(ui.ctx()).text_muted),
                    );
                }
            }
            TargetKind::File => {
                ui.horizontal(|ui| {
                    ui.label("File");
                    ui.add(TextEdit::singleline(&mut self.t_path).desired_width(420.0));
                    if ui.button("Browse…").clicked() {
                        let name = suggested_name("transfer", self.t_file.extension());
                        self.ask = Some((
                            AskFor::Target,
                            FileAsk::save(answer, "Write the file", &name),
                        ));
                    }
                });
                ComboBox::from_id_salt("tw-target-format")
                    .selected_text(self.t_file.label())
                    .show_ui(ui, |ui| {
                        for kind in FileKind::TARGETS {
                            ui.selectable_value(&mut self.t_file, kind, kind.label());
                        }
                    });
            }
        }
    }

    fn mapping_ui(&mut self, ui: &mut egui::Ui, engine: &Engine) {
        let Some((_, probe)) = self.probe.as_ref().filter(|_| self.probe_is_current()) else {
            ui.label("Load the source's preview first.");
            return;
        };
        let source = probe.columns.clone();
        if self.target_kind == TargetKind::Existing {
            let table = table_ref(&self.t_schema, &self.t_table);
            let key = (self.t_conn.clone(), table.clone());
            if self.described.as_ref() != Some(&key) {
                engine.describe(&SessionKey::meta(&self.t_conn), table);
                self.described = Some(key);
            }
        }
        let Some(targets) = self.target_columns(engine) else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading the table's columns…");
            });
            if let Some(sqlr_engine::LoadState::Failed(e)) =
                engine.table_info(&self.t_conn, &table_ref(&self.t_schema, &self.t_table))
            {
                ui.colored_label(theme::palette(ui.ctx()).error, e.message);
            }
            return;
        };
        egui::ScrollArea::vertical().show(ui, |ui| {
            if mapping_table(ui, &source, &targets, &mut self.mapping) {
                self.ddl_edited = false;
            }
        });
    }

    fn options_ui(&mut self, ui: &mut egui::Ui, answer: Option<&PathBuf>) {
        let table_target = self.table_target();
        ui.horizontal(|ui| {
            ui.label("Rows per batch");
            ui.add(DragValue::new(&mut self.options.batch).range(1..=100_000));
        });
        if table_target {
            ui.checkbox(
                &mut self.options.commit_per_batch,
                "Commit after each batch (a cancel keeps the committed batches)",
            );
            ui.checkbox(
                &mut self.options.truncate_first,
                RichText::new("Delete the target's rows first")
                    .color(theme::palette(ui.ctx()).error),
            );
            if self.options.truncate_first {
                theme::hint(
                    ui,
                    "Removes every existing row before loading. You will confirm on Review.",
                );
            }
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.upsert, "Update rows that exist, by key");
                ui.add_enabled_ui(self.upsert, |ui| {
                    ComboBox::from_id_salt("upsert_keys")
                        .selected_text(if self.upsert_text.is_empty() {
                            "Choose key columns"
                        } else {
                            &self.upsert_text
                        })
                        .show_ui(ui, |ui| {
                            let mut keys: Vec<String> = self
                                .upsert_text
                                .split(',')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned)
                                .collect();
                            for name in self.mapping.iter().filter_map(|m| m.target.as_ref()) {
                                let mut included = keys.contains(name);
                                if ui.checkbox(&mut included, name).changed() {
                                    if included {
                                        keys.push(name.clone());
                                    } else {
                                        keys.retain(|k| k != name);
                                    }
                                }
                            }
                            self.upsert_text = keys.join(", ");
                        });
                });
            });
            ui.checkbox(
                &mut self.options.disable_constraints,
                RichText::new("Switch foreign keys off during the load")
                    .color(theme::palette(ui.ctx()).error),
            );
            ui.checkbox(
                &mut self.options.skip_bad_rows,
                "Skip bad rows and report them",
            );
            if self.options.skip_bad_rows {
                ui.horizontal(|ui| {
                    ui.label("Rejected rows file");
                    ui.add(TextEdit::singleline(&mut self.rejected_path).desired_width(320.0));
                    if ui.button("Browse…").clicked() {
                        self.ask = Some((
                            AskFor::Rejected,
                            FileAsk::save(answer, "Save the rejected rows", "rejected.csv"),
                        ));
                    }
                });
            }
        } else {
            ui.label("A file target has no further options.");
        }
    }

    /// The `CREATE TABLE` a new target gets, from the source's columns and the mapping.
    fn generate_ddl(&mut self, engine: &Engine, conns: &[ConnChoice]) {
        let Some((_, probe)) = &self.probe else {
            return;
        };
        let from = if self.source_kind == SourceKind::File {
            "file".to_string()
        } else {
            self.driver_of(conns, &self.s_conn).to_string()
        };
        let to = self.driver_of(conns, &self.t_conn).to_string();
        let columns: Vec<(String, String)> = self
            .mapping
            .iter()
            .filter_map(|m| {
                let name = m.target.clone()?;
                let source = probe.columns.get(m.source)?;
                Some((name, source.type_name.clone()))
            })
            .collect();
        let (mut def, warnings) = new_table_def(
            &table_ref(&self.t_schema, &self.t_table),
            &from,
            &to,
            &columns,
        );
        let forced = self.mapping.iter().filter(|m| m.target.is_some());
        for (column, m) in def.columns.iter_mut().zip(forced) {
            if let Some(ty) = &m.target_type {
                column.type_name = ty.clone();
            }
        }
        self.warnings = warnings;
        self.ddl = engine
            .create_ddl(&self.t_conn, &ObjectDef::Table(def))
            .unwrap_or_else(|e| format!("-- {}", e.message));
    }

    fn review_ui(&mut self, ui: &mut egui::Ui, engine: &Engine, conns: &[ConnChoice]) {
        let p = theme::palette(ui.ctx());
        // From → to, the one thing to check before starting.
        egui::Frame::new()
            .fill(p.bg_subtle)
            .stroke(egui::Stroke::new(1.0, p.border))
            .corner_radius(Spacing::RADIUS_M)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let endpoint = |ui: &mut egui::Ui, caption: &str, text: String| {
                    ui.label(RichText::new(caption).small().color(p.text_muted));
                    ui.label(RichText::new(text).strong());
                };
                endpoint(ui, "FROM", describe(self.source_endpoint().as_ref(), conns));
                ui.add_space(Spacing::XS);
                ui.label(
                    sqlr_ui::theme::icons::text(ui, sqlr_ui::theme::icons::MOVE_DOWN, 14.0)
                        .color(p.accent),
                );
                ui.add_space(Spacing::XS);
                endpoint(ui, "TO", describe(self.target_endpoint().as_ref(), conns));
            });
        ui.add_space(Spacing::M);
        egui::Grid::new("tw-review")
            .num_columns(2)
            .min_col_width(130.0)
            .spacing([Spacing::L, Spacing::S])
            .show(ui, |ui| {
                let muted = |ui: &mut egui::Ui, text: &str| {
                    ui.label(RichText::new(text).color(p.text_muted));
                };
                if self.table_target() {
                    let mapped = self.mapping.iter().filter(|m| m.target.is_some()).count();
                    muted(ui, "Columns");
                    ui.label(format!("{mapped} of {} mapped", self.mapping.len()));
                    ui.end_row();
                }
                if let Some((_, probe)) = &self.probe {
                    muted(ui, "Rows");
                    ui.label(format!(
                        "Every source row (previewed {} of them)",
                        probe.rows.len()
                    ));
                    ui.end_row();
                }
                muted(ui, "Batches");
                ui.label(format!(
                    "{} rows each, {}",
                    self.options.batch,
                    if self.options.commit_per_batch {
                        "committed one by one (done batches stay if it stops)"
                    } else {
                        "committed once at the end"
                    }
                ));
                ui.end_row();
                if self.upsert {
                    muted(ui, "Existing rows");
                    ui.label(format!("Updated by key: {}", self.upsert_text));
                    ui.end_row();
                }
                if self.options.skip_bad_rows {
                    muted(ui, "Bad rows");
                    ui.label("Skipped and reported");
                    ui.end_row();
                }
                if !self.rejected_path.is_empty() {
                    muted(ui, "Rejected rows file");
                    ui.label(&self.rejected_path);
                    ui.end_row();
                }
                if self.source_kind == SourceKind::File {
                    muted(ui, "Encoding");
                    ui.label(if self.s_encoding.is_empty() {
                        "Detected (UTF-8 by default)"
                    } else {
                        &self.s_encoding
                    });
                    ui.end_row();
                }
            });
        // The options that destroy or bypass something, impossible to miss.
        for (on, text) in [
            (
                self.options.truncate_first,
                "Every row already in the target is deleted before loading",
            ),
            (
                self.options.disable_constraints,
                "Foreign key checks are off while loading",
            ),
        ] {
            if on {
                ui.add_space(Spacing::S);
                ui.horizontal(|ui| {
                    sqlr_ui::theme::icons::paint(ui, sqlr_ui::theme::icons::WARNING, p.error);
                    ui.colored_label(p.error, text);
                });
            }
        }
        if self.target_kind == TargetKind::New {
            let key = format!(
                "{}|{}|{}|{:?}",
                self.t_conn, self.t_schema, self.t_table, self.mapping
            );
            if !self.ddl_edited && self.ddl_for != key {
                self.generate_ddl(engine, conns);
                self.ddl_for = key;
            }
            ui.add_space(Spacing::M);
            theme::section_title(ui, "New table");
            theme::hint(ui, "The statement that creates it; edit it if you need to.");
            for w in &self.warnings {
                ui.colored_label(p.warning, w);
            }
            let edit = ui.add(
                TextEdit::multiline(&mut self.ddl)
                    .code_editor()
                    .desired_rows(10)
                    .desired_width(f32::INFINITY),
            );
            if edit.changed() {
                self.ddl_edited = true;
            }
        }
    }

    fn run_ui(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        if self.run.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!(
                    "{} rows{}",
                    self.progress.rows,
                    if self.progress.bytes > 0 {
                        format!(" · {}", human_bytes(self.progress.bytes))
                    } else {
                        String::new()
                    }
                ));
            });
        }
        match &self.outcome {
            Some(Ok(message)) => {
                ui.colored_label(p.success, message);
            }
            Some(Err(message)) => {
                ui.colored_label(p.error, message);
            }
            None => {}
        }
        if !self.log.is_empty() {
            ui.add_space(Spacing::S);
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.log {
                        ui.monospace(line);
                    }
                });
        }
    }
}

impl SqlrApp {
    /// The connections the wizard offers: every saved one.
    pub(crate) fn wizard_connections(&self) -> Vec<ConnChoice> {
        self.connections
            .iter()
            .map(|c| ConnChoice {
                id: c.id.clone(),
                name: c.name.clone(),
                driver: c.driver.clone(),
            })
            .collect()
    }

    /// Opens the transfer wizard, with `table` as the source when given ("Transfer data…" on
    /// an explorer table).
    pub(crate) fn open_transfer_wizard(&mut self, table: Option<(&str, &Node)>) {
        let conns = self.wizard_connections();
        if conns.is_empty() {
            self.notify("Add a connection first".to_string());
            return;
        }
        self.transfer_wizard = Some(match table {
            Some((conn, node)) => match self.node_table(conn, node) {
                Some(table) => TransferWizard::for_table(&conns, conn, table),
                None => {
                    self.notify(format!("Cannot transfer the data of {}", node.name));
                    return;
                }
            },
            None => TransferWizard::new(&conns),
        });
    }

    /// The transfer wizard and the job it runs.
    pub(crate) fn transfer_wizard_ui(&mut self, ctx: &egui::Context) {
        let conns = self.wizard_connections();
        let Some(wizard) = &mut self.transfer_wizard else {
            return;
        };
        // A connection never connected in this run is connected for the wizard; one that
        // failed stays failed (its error shows beside the picker).
        let needed = wizard.needed_connections();
        for id in needed {
            if self.engine.session_status(&id).is_none() {
                self.connect(&id);
            }
        }
        let Some(wizard) = &mut self.transfer_wizard else {
            return;
        };
        match wizard.show(ctx, &self.engine, &conns, self.file_answer.as_ref()) {
            WizardEvent::None => {}
            WizardEvent::Close => self.transfer_wizard = None,
        }
    }
}

fn summarize(outcome: JobOutcome) -> Result<String, String> {
    match outcome {
        JobOutcome::Imported {
            read,
            written,
            rejected,
        } => Ok(format!(
            "Read {read} rows, wrote {written}, rejected {}",
            rejected.len()
        )),
        JobOutcome::Exported { rows, path } => {
            Ok(format!("Wrote {rows} rows to {}", path.display()))
        }
        JobOutcome::Failed(e) => Err(e.message),
        JobOutcome::Cancelled => Err("Cancelled".into()),
        _ => Ok("Done".into()),
    }
}

/// A one-line description of an endpoint.
pub(crate) fn describe(endpoint: Option<&Endpoint>, conns: &[ConnChoice]) -> String {
    let name = |id: &str| {
        conns
            .iter()
            .find(|c| c.id == id)
            .map_or(id.to_string(), |c| c.name.clone())
    };
    let table = |t: &TableRef| match &t.schema {
        Some(s) => format!("{s}.{}", t.name),
        None => t.name.clone(),
    };
    match endpoint {
        None => "(incomplete)".into(),
        Some(Endpoint::Table { conn, table: t }) => format!("table {} on {}", table(t), name(conn)),
        Some(Endpoint::NewTable { conn, table: t, .. }) => {
            format!("new table {} on {}", table(t), name(conn))
        }
        Some(Endpoint::Query { conn, .. }) => format!("a query on {}", name(conn)),
        Some(Endpoint::File { path, .. })
        | Some(Endpoint::Workbook { path, .. })
        | Some(Endpoint::Parquet { path }) => format!("file {}", path.display()),
        Some(_) => "(other)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use sqlr_core::{ColumnKind, ColumnMeta, DisplayHint, DriverError, ErrorKind, Value};
    use sqlr_engine::DriverRegistry;

    fn conns() -> Vec<ConnChoice> {
        vec![
            ConnChoice {
                id: "a".into(),
                name: "A".into(),
                driver: "sqlite".into(),
            },
            ConnChoice {
                id: "b".into(),
                name: "B".into(),
                driver: "postgres".into(),
            },
        ]
    }

    fn engine() -> Engine {
        Engine::new(DriverRegistry::default(), Arc::new(|| {}))
    }

    fn probe() -> SourceProbe {
        SourceProbe {
            columns: vec![
                ColumnMeta::new("id", "INTEGER", ColumnKind::Int, DisplayHint::Plain),
                ColumnMeta::new("name", "TEXT", ColumnKind::Text, DisplayHint::Plain),
            ],
            rows: vec![vec![Value::Int(1), Value::Text("ann".into())]],
            sheets: Vec::new(),
        }
    }

    fn frame(
        wizard: &mut TransferWizard,
        engine: &Engine,
        conns: &[ConnChoice],
        answer: Option<&PathBuf>,
    ) {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            wizard.show(ui.ctx(), engine, conns, answer);
        });
        // No renderer consumes the font atlas upload here.
        out.textures_delta.clear();
    }

    #[test]
    fn a_file_source_loads_its_preview_without_an_extra_click() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.csv");
        std::fs::write(&path, "id,name\n1,Ada\n").unwrap();
        let engine = Engine::new(
            sqlr_engine::DriverRegistry::default(),
            std::sync::Arc::new(|| {}),
        );
        let mut wizard = TransferWizard::new(&[]);
        wizard.source_kind = SourceKind::File;
        wizard.s_path = path.display().to_string();
        let mut h = egui_kittest::Harness::builder()
            .with_size((1100.0, 850.0))
            .with_step_dt(0.05)
            .build_ui_state(
                |ui, wizard: &mut TransferWizard| {
                    wizard.show(ui.ctx(), &engine, &[], None);
                },
                wizard,
            );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !h.state().probe_is_current() && std::time::Instant::now() < deadline {
            h.run_steps(5);
            std::thread::yield_now();
        }
        assert!(
            h.state().probe_is_current(),
            "the file preview should load automatically"
        );
    }

    #[test]
    fn a_table_to_table_spec_needs_a_mapping() {
        let mut wizard = TransferWizard::for_table(
            &conns(),
            "a",
            TableRef {
                database: None,
                schema: Some("main".into()),
                name: "src".into(),
            },
        );
        assert_eq!(
            wizard.t_table, "src",
            "the target table defaults to the source's name"
        );
        wizard.t_table = "dst".into();
        assert_eq!(
            wizard.t_conn, "b",
            "the target defaults to another connection"
        );
        assert_eq!(wizard.t_schema, "public");
        assert_eq!(wizard.spec().unwrap_err(), "No column is mapped");
        wizard.mapping = vec![ColumnMapping::new(0, Some("id".into()))];
        let spec = wizard.spec().unwrap();
        assert_eq!(
            spec.source,
            Endpoint::Table {
                conn: "a".into(),
                table: table_ref("main", "src")
            }
        );
        assert!(matches!(spec.target, Endpoint::Table { .. }));
    }

    #[test]
    fn options_flow_into_the_spec() {
        let mut wizard = TransferWizard::new(&conns());
        wizard.s_conn = "a".into();
        wizard.s_table = "src".into();
        wizard.t_table = "dst".into();
        wizard.mapping = vec![ColumnMapping::new(0, Some("id".into()))];
        wizard.upsert = true;
        wizard.upsert_text = "id, tenant".into();
        wizard.rejected_path = "/tmp/r.csv".into();
        let spec = wizard.spec().unwrap();
        assert_eq!(
            spec.options.upsert_key,
            Some(vec!["id".to_string(), "tenant".to_string()])
        );
        assert_eq!(
            spec.options.rejected_file,
            Some(PathBuf::from("/tmp/r.csv"))
        );
        wizard.upsert_text = " , ".into();
        assert_eq!(wizard.spec().unwrap_err(), "Name the upsert key columns");
    }

    #[test]
    fn a_file_target_has_no_mapping_and_formats_follow_extensions() {
        let mut wizard = TransferWizard::new(&conns());
        wizard.s_conn = "a".into();
        wizard.s_sql = "SELECT 1".into();
        wizard.source_kind = SourceKind::Query;
        wizard.target_kind = TargetKind::File;
        wizard.t_path = "/tmp/out.parquet".into();
        wizard.t_file = FileKind::of_path(Path::new("/tmp/out.parquet"));
        let spec = wizard.spec().unwrap();
        assert!(spec.mapping.is_empty());
        assert_eq!(
            spec.target,
            Endpoint::Parquet {
                path: PathBuf::from("/tmp/out.parquet")
            }
        );
        assert_eq!(FileKind::of_path(Path::new("x.XLSX")), FileKind::Xlsx);
        assert_eq!(
            FileKind::of_path(Path::new("x.ndjson")),
            FileKind::JsonLines
        );
        assert_eq!(FileKind::of_path(Path::new("x")), FileKind::Csv);
    }

    #[test]
    fn every_source_kind_becomes_an_endpoint() {
        let mut wizard = TransferWizard::new(&conns());
        wizard.s_conn = "a".into();
        wizard.s_table = "src".into();
        wizard.s_schema = "main".into();
        assert!(matches!(
            wizard.source_endpoint(),
            Some(Endpoint::Table { .. })
        ));

        wizard.source_kind = SourceKind::Query;
        wizard.s_sql = " SELECT 1 ".into();
        assert_eq!(
            wizard.source_endpoint(),
            Some(Endpoint::Query {
                conn: "a".into(),
                database: None,
                sql: "SELECT 1".into()
            })
        );
        wizard.s_sql.clear();
        assert!(wizard.source_endpoint().is_none());

        wizard.source_kind = SourceKind::File;
        wizard.s_path = "/tmp/in.csv".into();
        for (file, expected) in [
            (FileKind::Csv, "csv"),
            (FileKind::Tsv, "tsv"),
            (FileKind::Json, "json"),
            (FileKind::JsonLines, "jsonl"),
            (FileKind::SqlInsert, "jsonl"),
        ] {
            wizard.s_file = file;
            match wizard.source_endpoint().unwrap() {
                Endpoint::File { format, .. } => {
                    let text = format!("{format:?}").to_ascii_lowercase();
                    assert!(text.contains(expected), "{file:?}: {text}");
                }
                other => panic!("{file:?}: {other:?}"),
            }
        }
        wizard.s_file = FileKind::Xlsx;
        wizard.s_sheet = Some("Sheet2".into());
        assert!(matches!(
            wizard.source_endpoint(),
            Some(Endpoint::Workbook {
                sheet: Some(ref s),
                ..
            }) if s == "Sheet2"
        ));
        wizard.s_file = FileKind::Parquet;
        assert!(matches!(
            wizard.source_endpoint(),
            Some(Endpoint::Parquet { .. })
        ));
    }

    #[test]
    fn every_target_kind_becomes_an_endpoint() {
        let mut wizard = TransferWizard::new(&conns());
        wizard.t_conn = "b".into();
        wizard.t_schema = "public".into();
        wizard.t_table = "dst".into();
        assert!(matches!(
            wizard.target_endpoint(),
            Some(Endpoint::Table { .. })
        ));
        wizard.target_kind = TargetKind::New;
        assert!(matches!(
            wizard.target_endpoint(),
            Some(Endpoint::NewTable {
                ddl: None,
                ref table,
                ..
            }) if table.name == "dst"
        ));
        wizard.ddl_edited = true;
        wizard.ddl = "CREATE TABLE dst (id integer)".into();
        assert!(matches!(
            wizard.target_endpoint(),
            Some(Endpoint::NewTable { ddl: Some(_), .. })
        ));
        wizard.t_table.clear();
        assert!(wizard.target_endpoint().is_none());

        wizard.target_kind = TargetKind::File;
        wizard.t_path = "/tmp/out.sql".into();
        for file in FileKind::TARGETS {
            wizard.t_file = file;
            assert!(wizard.target_endpoint().is_some(), "{file:?}");
        }
        wizard.t_path.clear();
        assert!(wizard.target_endpoint().is_none());
    }

    #[test]
    fn every_step_draws_with_a_preview_and_a_mapping() {
        let engine = engine();
        let mut wizard = TransferWizard::new(&conns());
        wizard.s_conn = "a".into();
        wizard.s_table = "src".into();
        wizard.t_conn = "b".into();
        wizard.t_table = "dst".into();
        wizard.target_kind = TargetKind::New;
        wizard.mapping = vec![
            ColumnMapping::new(0, Some("id".into())),
            ColumnMapping::new(1, Some("name".into())),
        ];
        for file in FileKind::SOURCES {
            wizard.source_kind = SourceKind::File;
            wizard.s_path = "/tmp/in.csv".into();
            wizard.s_file = file;
            wizard.probe = Some((wizard.source_key(), probe()));
            frame(&mut wizard, &engine, &conns(), None);
        }
        wizard.source_kind = SourceKind::Table;
        wizard.probe = Some((wizard.source_key(), probe()));

        for step in 0..wizard.steps(&engine).len() {
            wizard.step = step;
            frame(&mut wizard, &engine, &conns(), None);
        }
        // The wizard clamps a stale step when steps disappear (a file target).
        wizard.target_kind = TargetKind::File;
        wizard.t_path = "/tmp/out.csv".into();
        wizard.step = 10;
        frame(&mut wizard, &engine, &conns(), None);
        assert!(wizard.step < wizard.steps(&engine).len());
    }

    #[test]
    fn options_cover_every_toggle() {
        let engine = engine();
        let mut wizard = TransferWizard::new(&conns());
        wizard.s_conn = "a".into();
        wizard.s_table = "src".into();
        wizard.t_conn = "b".into();
        wizard.t_table = "dst".into();
        wizard.target_kind = TargetKind::New;
        wizard.mapping = vec![ColumnMapping::new(0, Some("id".into()))];
        wizard.probe = Some((wizard.source_key(), probe()));
        wizard.step = 3;
        for (upsert, truncate, skip, constraints) in
            [(true, true, true, true), (false, false, false, false)]
        {
            wizard.upsert = upsert;
            wizard.upsert_text = "id".into();
            wizard.options.truncate_first = truncate;
            wizard.options.skip_bad_rows = skip;
            wizard.options.disable_constraints = constraints;
            frame(&mut wizard, &engine, &conns(), None);
        }
        wizard.target_kind = TargetKind::File;
        wizard.t_path = "/tmp/out.csv".into();
        frame(&mut wizard, &engine, &conns(), None);
    }

    #[test]
    fn file_answers_fill_the_source_target_and_rejected_paths() {
        let engine = engine();
        let mut wizard = TransferWizard::new(&conns());
        let path = PathBuf::from("/tmp/answer.json");
        wizard.ask = Some((
            AskFor::Source,
            FileAsk::open(Some(&path), "source", &["json"]),
        ));
        frame(&mut wizard, &engine, &conns(), Some(&path));
        assert_eq!(wizard.s_path, "/tmp/answer.json");
        assert_eq!(wizard.s_file, FileKind::Json);

        wizard.ask = Some((
            AskFor::Target,
            FileAsk::save(Some(&path), "target", "out.json"),
        ));
        frame(&mut wizard, &engine, &conns(), Some(&path));
        assert_eq!(wizard.t_path, "/tmp/answer.json");
        assert_eq!(wizard.t_file, FileKind::Json);

        wizard.ask = Some((
            AskFor::Rejected,
            FileAsk::save(Some(&path), "rejected", "r.csv"),
        ));
        frame(&mut wizard, &engine, &conns(), Some(&path));
        assert_eq!(wizard.rejected_path, "/tmp/answer.json");
    }

    #[test]
    fn review_generates_the_new_table_ddl() {
        let engine = engine();
        let mut wizard = TransferWizard::new(&conns());
        wizard.s_conn = "a".into();
        wizard.s_table = "src".into();
        wizard.t_conn = "b".into();
        wizard.t_table = "dst".into();
        wizard.target_kind = TargetKind::New;
        wizard.mapping = vec![
            ColumnMapping::new(0, Some("id".into())),
            ColumnMapping::new(1, Some("name".into())),
        ];
        wizard.probe = Some((wizard.source_key(), probe()));
        wizard.step = 4;
        frame(&mut wizard, &engine, &conns(), None);
        // With no registered dialect the DDL carries the error for the user.
        assert!(wizard.ddl.starts_with("-- "), "{}", wizard.ddl);
    }

    #[test]
    fn the_run_step_shows_progress_outcomes_and_logs() {
        let engine = engine();
        let mut wizard = TransferWizard::new(&conns());
        wizard.s_conn = "a".into();
        wizard.s_table = "src".into();
        wizard.t_conn = "b".into();
        wizard.t_table = "dst".into();
        wizard.mapping = vec![ColumnMapping::new(0, Some("id".into()))];
        wizard.probe = Some((wizard.source_key(), probe()));
        wizard.step = 5;
        wizard.progress = JobProgress {
            rows: 12,
            bytes: 2048,
        };
        wizard.log = vec!["batch 1".into()];
        frame(&mut wizard, &engine, &conns(), None);

        for outcome in [
            Ok("Read 1 rows, wrote 1, rejected 0".into()),
            Err("boom".into()),
        ] {
            wizard.outcome = Some(outcome);
            frame(&mut wizard, &engine, &conns(), None);
        }
        wizard.outcome = None;
        wizard.run = Some(engine.transfer(wizard.spec().unwrap()));
        frame(&mut wizard, &engine, &conns(), None);
        let _ = &wizard.progress;
    }

    #[test]
    fn an_invalid_spec_becomes_an_error_outcome() {
        let engine = engine();
        let mut wizard = TransferWizard::new(&conns());
        wizard.start(&engine);
        assert!(wizard.outcome.is_some_and(|o| o.is_err()));
    }

    #[test]
    fn outcomes_are_summarized() {
        let ok = |outcome| summarize(outcome).unwrap();
        assert_eq!(
            ok(JobOutcome::Imported {
                read: 3,
                written: 2,
                rejected: vec![(4, "bad".into())],
            }),
            "Read 3 rows, wrote 2, rejected 1"
        );
        assert_eq!(
            ok(JobOutcome::Exported {
                rows: 5,
                path: PathBuf::from("/tmp/out.csv"),
            }),
            "Wrote 5 rows to /tmp/out.csv"
        );
        assert_eq!(ok(JobOutcome::Done), "Done");
        assert_eq!(
            summarize(JobOutcome::Failed(DriverError::new(
                ErrorKind::Other,
                "boom"
            )))
            .unwrap_err(),
            "boom"
        );
        assert_eq!(summarize(JobOutcome::Cancelled).unwrap_err(), "Cancelled");
    }

    #[test]
    fn endpoints_are_described_for_the_review() {
        let conns = conns();
        assert_eq!(describe(None, &conns), "(incomplete)");
        assert_eq!(
            describe(
                Some(&Endpoint::Table {
                    conn: "a".into(),
                    table: table_ref("main", "src"),
                }),
                &conns
            ),
            "table main.src on A"
        );
        assert_eq!(
            describe(
                Some(&Endpoint::NewTable {
                    conn: "b".into(),
                    table: table_ref("", "dst"),
                    ddl: None,
                }),
                &conns
            ),
            "new table dst on B"
        );
        assert_eq!(
            describe(
                Some(&Endpoint::Query {
                    conn: "a".into(),
                    database: None,
                    sql: "SELECT 1".into(),
                }),
                &conns
            ),
            "a query on A"
        );
        for endpoint in [
            Endpoint::File {
                path: "/tmp/x.csv".into(),
                format: TextFormat::Csv(CsvOptions::default()),
            },
            Endpoint::Workbook {
                path: "/tmp/x.xlsx".into(),
                sheet: None,
                header: true,
            },
            Endpoint::Parquet {
                path: "/tmp/x.parquet".into(),
            },
        ] {
            assert!(describe(Some(&endpoint), &conns).starts_with("file /tmp/x."));
        }
        assert_eq!(
            describe(
                Some(&Endpoint::RedisKeys {
                    conn: "a".into(),
                    db: 0,
                    pattern: b"*".to_vec(),
                }),
                &conns
            ),
            "(other)"
        );
    }

    #[test]
    fn needed_connections_follow_the_endpoints() {
        let mut wizard = TransferWizard::new(&conns());
        wizard.s_conn = "a".into();
        wizard.t_conn = "b".into();
        assert_eq!(wizard.needed_connections(), ["a", "b"]);
        wizard.target_kind = TargetKind::File;
        assert_eq!(wizard.needed_connections(), ["a"]);
        wizard.source_kind = SourceKind::File;
        assert!(wizard.needed_connections().is_empty());
        let mut same = TransferWizard::new(&conns());
        same.s_conn = "a".into();
        same.t_conn = "a".into();
        assert_eq!(same.needed_connections(), ["a"]);
    }
}
