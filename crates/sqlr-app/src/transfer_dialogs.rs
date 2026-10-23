//! The export and import dialogs (§8.3), the native file dialogs they open (polled without
//! blocking a frame), and the background jobs the status bar follows.

use sqlr_ui::ToastKind;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use egui::{ComboBox, DragValue, RichText, TextEdit};
use sqlr_core::{
    ChunkBuilder, ColumnKind, ColumnMeta, CsvOptions, DisplayHint, DocTarget, Endpoint, KvNodePath,
    Node, RowBuffer, TableRef, TextFormat, TransferSpec, Value, glob_escape,
};
use sqlr_engine::{ExportSource, ImportOptions, JobId, JobOutcome, LoadState, QueryId, SessionKey};
use sqlr_ui::text_edit;
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{GridState, result_grid};

use crate::app::{SqlrApp, tab_mut};

/// Rejected rows listed in the import summary.
const LISTED_REJECTED: usize = 100;

type FileFuture = Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>;

/// A native file dialog, or the answer a test gave in advance.
pub(crate) enum FileAsk {
    Native(FileFuture),
    Ready(Option<PathBuf>),
}

impl FileAsk {
    pub fn save(answer: Option<&PathBuf>, title: &str, name: &str) -> Self {
        match answer {
            Some(path) => FileAsk::Ready(Some(path.clone())),
            None => FileAsk::Native(Box::pin(
                rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .set_file_name(name)
                    .save_file(),
            )),
        }
    }

    pub fn open(answer: Option<&PathBuf>, title: &str, extensions: &[&str]) -> Self {
        match answer {
            Some(path) => FileAsk::Ready(Some(path.clone())),
            None => FileAsk::Native(Box::pin(
                rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .add_filter("Data files", extensions)
                    .pick_file(),
            )),
        }
    }

    /// `Some(path)` once chosen, `Some(None)` when dismissed, `None` while still open. The
    /// dialog's future is polled without a waker, so while it is open the UI wakes up every
    /// 100 ms to look again.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Option<PathBuf>> {
        match self {
            FileAsk::Ready(answer) => Some(answer.take()),
            FileAsk::Native(future) => {
                let mut cx = Context::from_waker(Waker::noop());
                match future.as_mut().poll(&mut cx) {
                    Poll::Ready(file) => Some(file.map(|f| f.path().to_path_buf())),
                    Poll::Pending => {
                        ctx.request_repaint_after(std::time::Duration::from_millis(100));
                        None
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExportFormat {
    Csv,
    Tsv,
    Json,
    JsonLines,
    SqlInsert,
    Markdown,
}

impl ExportFormat {
    const ALL: [ExportFormat; 6] = [
        ExportFormat::Csv,
        ExportFormat::Tsv,
        ExportFormat::Json,
        ExportFormat::JsonLines,
        ExportFormat::SqlInsert,
        ExportFormat::Markdown,
    ];

    fn label(self) -> &'static str {
        match self {
            ExportFormat::Csv => "CSV",
            ExportFormat::Tsv => "TSV",
            ExportFormat::Json => "JSON (array of objects)",
            ExportFormat::JsonLines => "JSON Lines",
            ExportFormat::SqlInsert => "SQL INSERT",
            ExportFormat::Markdown => "Markdown",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Tsv => "tsv",
            ExportFormat::Json => "json",
            ExportFormat::JsonLines => "jsonl",
            ExportFormat::SqlInsert => "sql",
            ExportFormat::Markdown => "md",
        }
    }
}

/// CSV options as the dialogs edit them.
#[derive(Clone, Debug)]
struct CsvFields {
    delimiter: String,
    quote: String,
    header: bool,
    null_text: String,
    crlf: bool,
}

impl Default for CsvFields {
    fn default() -> Self {
        Self {
            delimiter: ",".into(),
            quote: "\"".into(),
            header: true,
            null_text: String::new(),
            crlf: false,
        }
    }
}

impl CsvFields {
    fn options(&self) -> CsvOptions {
        let first = |s: &str, default: char| s.chars().next().unwrap_or(default);
        CsvOptions {
            delimiter: match self.delimiter.as_str() {
                "\\t" => '\t',
                d => first(d, ','),
            },
            quote: first(&self.quote, '"'),
            header: self.header,
            null_text: self.null_text.clone(),
            crlf: self.crlf,
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, write: bool) {
        egui::Grid::new(ui.id().with("csv_options"))
            .num_columns(2)
            .spacing([Spacing::M, Spacing::S])
            .show(ui, |ui| {
                ui.label("Delimiter");
                text_edit(
                    ui,
                    TextEdit::singleline(&mut self.delimiter).desired_width(40.0),
                )
                .on_hover_text("One character; \\t for a tab");
                ui.end_row();
                ui.label("Quote");
                text_edit(
                    ui,
                    TextEdit::singleline(&mut self.quote).desired_width(40.0),
                );
                ui.end_row();
                ui.label("NULL as");
                text_edit(
                    ui,
                    TextEdit::singleline(&mut self.null_text).desired_width(80.0),
                )
                .on_hover_text("Unquoted fields with this text are NULL (default: empty)");
                ui.end_row();
            });
        ui.checkbox(&mut self.header, "Header row");
        if write {
            ui.checkbox(&mut self.crlf, "Windows line endings (CRLF)");
        }
    }
}

/// `name` reduced to letters, digits, `_` and `-`.
fn file_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() {
        "result".into()
    } else {
        stem
    }
}

/// `<name>_<yyyymmdd_hhmm>.<ext>`, in local time.
pub(crate) fn suggested_name(name: &str, ext: &str) -> String {
    let now = jiff::Zoned::now().strftime("%Y%m%d_%H%M").to_string();
    format!("{}_{now}.{ext}", file_stem(name))
}

/// Where an export reads its rows.
#[derive(Clone, Debug)]
pub(crate) struct Rerun {
    pub key: SessionKey,
    pub database: Option<String>,
    pub sql: String,
}

pub(crate) struct ExportDialog {
    /// Names the file and the SQL `INSERT` target.
    name: String,
    /// A loaded result and its row count.
    loaded: Option<(QueryId, usize)>,
    rerun: Option<Rerun>,
    /// A collection or Redis keys, exported through a transfer.
    endpoint: Option<Endpoint>,
    whole: bool,
    format: ExportFormat,
    csv: CsvFields,
    table: String,
    ask: Option<FileAsk>,
}

/// What the export dialog asks for this frame.
pub(crate) enum ExportEvent {
    None,
    Close,
    Start {
        source: ExportSource,
        format: TextFormat,
        path: PathBuf,
    },
    /// A collection or Redis keys to the file.
    Transfer {
        source: Endpoint,
        format: TextFormat,
        path: PathBuf,
    },
}

impl ExportDialog {
    /// `rerun` must only be given for a statement that only reads; `whole` picks it by
    /// default (when the loaded rows are not all of them).
    pub fn new(
        name: &str,
        loaded: Option<(QueryId, usize)>,
        rerun: Option<Rerun>,
        whole: bool,
    ) -> Self {
        Self {
            name: name.to_string(),
            whole: whole && rerun.is_some(),
            loaded,
            rerun,
            endpoint: None,
            format: ExportFormat::Csv,
            csv: CsvFields::default(),
            table: name.to_string(),
            ask: None,
        }
    }

    /// Exports `source` (a collection or Redis keys) through a transfer.
    pub fn for_endpoint(name: &str, source: Endpoint) -> Self {
        Self {
            endpoint: Some(source),
            ..Self::new(name, None, None, false)
        }
    }

    fn text_format(&self) -> TextFormat {
        match self.format {
            ExportFormat::Csv => TextFormat::Csv(self.csv.options()),
            ExportFormat::Tsv => TextFormat::Tsv,
            ExportFormat::Json => TextFormat::Json,
            ExportFormat::JsonLines => TextFormat::JsonLines,
            ExportFormat::SqlInsert => TextFormat::SqlInsert {
                table: self.table.clone(),
            },
            ExportFormat::Markdown => TextFormat::Markdown,
        }
    }

    fn source(&self) -> Option<ExportSource> {
        match (&self.rerun, self.loaded) {
            (Some(r), _) if self.whole || self.loaded.is_none() => Some(ExportSource::Rerun {
                key: r.key.clone(),
                database: r.database.clone(),
                sql: r.sql.clone(),
            }),
            (_, Some((q, _))) => Some(ExportSource::Loaded(q)),
            _ => None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, answer: Option<&PathBuf>) -> ExportEvent {
        if let Some(ask) = &mut self.ask {
            match ask.poll(ctx) {
                None => {}
                Some(None) => self.ask = None,
                Some(Some(path)) => {
                    self.ask = None;
                    if let Some(source) = self.endpoint.clone() {
                        return ExportEvent::Transfer {
                            source,
                            format: self.text_format(),
                            path,
                        };
                    }
                    if let Some(source) = self.source() {
                        return ExportEvent::Start {
                            source,
                            format: self.text_format(),
                            path,
                        };
                    }
                }
            }
        }
        let mut event = ExportEvent::None;
        let mut open = true;
        theme::tool_dialog(
            ctx,
            &mut open,
            "Export data",
            theme::icons::EXPORT,
            "Choose which rows to export and how to format them",
            520.0,
            |ui| {
                ui.label(RichText::new(&self.name).font(theme::semibold(ui.ctx(), 13.0)));
                ui.add_space(Spacing::S);
                if let Some((_, rows)) = self.loaded {
                    ui.radio_value(&mut self.whole, false, format!("Loaded rows ({rows})"));
                }
                match &self.endpoint {
                    Some(Endpoint::RedisKeys { pattern, .. }) => {
                        ui.label(format!(
                            "Keys matching {}: name, type, TTL, value as JSON",
                            sqlr_core::display_key(pattern)
                        ));
                    }
                    Some(_) => {
                        ui.label(
                            "Every document: JSON keeps them whole, other formats flatten them \
                             (nested values as JSON text)",
                        );
                    }
                    None => {}
                }
                if self.rerun.is_some() {
                    ui.radio_value(&mut self.whole, true, "Whole result (re-run)");
                    if self.whole {
                        ui.label(
                            RichText::new(
                                "Runs the statement again on a separate session: rows this tab \
                                 has not committed and its session settings are not seen.",
                            )
                            .color(theme::palette(ui.ctx()).text_muted),
                        );
                    }
                }
                ui.add_space(Spacing::L);
                theme::section_title(ui, "File format");
                ui.add_space(Spacing::M);
                ComboBox::from_label("Format")
                    .selected_text(self.format.label())
                    .show_ui(ui, |ui| {
                        for f in ExportFormat::ALL {
                            ui.selectable_value(&mut self.format, f, f.label());
                        }
                    });
                match self.format {
                    ExportFormat::Csv => self.csv.ui(ui, true),
                    ExportFormat::SqlInsert => {
                        ui.horizontal(|ui| {
                            ui.label("Table");
                            ui.text_edit_singleline(&mut self.table);
                        });
                    }
                    _ => {}
                }
                ui.add_space(Spacing::L);
                theme::hairline(ui);
                ui.add_space(Spacing::M);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let waiting = self.ask.is_some();
                    if ui
                        .add_enabled(
                            !waiting,
                            theme::primary_button_state(ui.ctx(), !waiting, "Export"),
                        )
                        .clicked()
                    {
                        let name = suggested_name(&self.name, self.format.extension());
                        self.ask = Some(FileAsk::save(answer, "Export to", &name));
                    }
                    if ui.button("Cancel").clicked() {
                        event = ExportEvent::Close;
                    }
                    if waiting {
                        ui.spinner();
                    }
                });
            },
        );
        if !open {
            event = ExportEvent::Close;
        }
        event
    }
}

/// A background job the app follows until it ends.
pub(crate) struct TrackedJob {
    pub id: JobId,
    pub what: JobWhat,
}

pub(crate) enum JobWhat {
    Export,
    Copy { cells: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImportFormat {
    Csv,
    Tsv,
    Json,
    JsonLines,
}

impl ImportFormat {
    const ALL: [ImportFormat; 4] = [
        ImportFormat::Csv,
        ImportFormat::Tsv,
        ImportFormat::Json,
        ImportFormat::JsonLines,
    ];

    fn label(self) -> &'static str {
        match self {
            ImportFormat::Csv => "CSV",
            ImportFormat::Tsv => "TSV",
            ImportFormat::Json => "JSON (array of objects)",
            ImportFormat::JsonLines => "JSON Lines",
        }
    }

    /// By the file's extension; CSV when unknown.
    fn of(path: &Path) -> Self {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "tsv" | "tab" => ImportFormat::Tsv,
            "json" => ImportFormat::Json,
            "jsonl" | "ndjson" => ImportFormat::JsonLines,
            _ => ImportFormat::Csv,
        }
    }
}

/// The preview of a file: its column names and first rows as a grid.
struct Preview {
    columns: Vec<String>,
    rows: RowBuffer,
    grid: GridState,
}

impl Preview {
    fn new(columns: Vec<String>, rows: Vec<Vec<Value>>) -> Self {
        let metas: Arc<[ColumnMeta]> = columns
            .iter()
            .map(|c| ColumnMeta::new(c, "", ColumnKind::Mixed, DisplayHint::Plain))
            .collect();
        let mut buffer = RowBuffer::new(metas.clone());
        let mut b = ChunkBuilder::new(metas);
        for row in &rows {
            let mut cells: Vec<_> = row.iter().map(Value::as_cell).collect();
            cells.resize(columns.len(), sqlr_core::Cell::Null);
            // Mixed columns take any cell.
            let _ = b.push_row(&cells);
        }
        buffer.push_chunk(b.finish());
        Self {
            columns,
            rows: buffer,
            grid: GridState::default(),
        }
    }
}

/// Rows read, rows written and the rejected rows (line, reason) of a finished import.
type Summary = (u64, u64, Vec<(u64, String)>);

pub(crate) struct ImportDialog {
    conn: String,
    key: SessionKey,
    target: TableRef,
    /// The collection imported into instead of a table (fields named like the file's
    /// columns).
    collection: Option<DocTarget>,
    /// The profile asks before destructive statements (truncating first).
    confirm_destructive: bool,
    /// The profile asks before writes (a collection import has no transaction to undo).
    confirm_writes: bool,
    path: Option<PathBuf>,
    format: ImportFormat,
    csv: CsvFields,
    ask: Option<FileAsk>,
    preview_job: Option<JobId>,
    preview: Option<Preview>,
    /// Per source column: the target column, `None` to skip it.
    mapping: Vec<Option<String>>,
    options: ImportOptions,
    /// The question asked before the import runs, while it waits for the answer.
    confirming: Option<String>,
    run: Option<JobId>,
    summary: Option<Summary>,
    save_rejected: Option<FileAsk>,
    /// The job writing the rejected rows, and what it said once done.
    save_job: Option<JobId>,
    saved: Option<Result<String, String>>,
    error: Option<String>,
}

impl ImportDialog {
    /// An import into `target` on `conn`, asking first as the profile's `confirm_destructive`
    /// and `confirm_writes` say.
    pub fn new(
        conn: &str,
        target: TableRef,
        confirm_destructive: bool,
        confirm_writes: bool,
    ) -> Self {
        Self {
            conn: conn.to_string(),
            key: SessionKey::meta(conn),
            target,
            collection: None,
            confirm_destructive,
            confirm_writes,
            path: None,
            format: ImportFormat::Csv,
            csv: CsvFields::default(),
            ask: None,
            preview_job: None,
            preview: None,
            mapping: Vec::new(),
            options: ImportOptions::default(),
            confirming: None,
            run: None,
            summary: None,
            save_rejected: None,
            save_job: None,
            saved: None,
            error: None,
        }
    }

    /// Imports into a collection (or index): each row becomes a flat document.
    pub fn for_collection(conn: &str, target: DocTarget, confirm_writes: bool) -> Self {
        let table = TableRef {
            database: target.database.clone(),
            schema: None,
            name: target.collection.clone(),
        };
        Self {
            collection: Some(target),
            ..Self::new(conn, table, false, confirm_writes)
        }
    }

    /// What to ask before the import runs, as the connection's profile wants: deleting every
    /// row first under `confirm_destructive`, writing (into a table or a collection) under
    /// `confirm_writes`. The row count is not known before the file is read.
    fn run_prompt(&self) -> Option<String> {
        if self.options.truncate_first && self.confirm_destructive {
            return Some(format!(
                "Delete every row of {} before importing?",
                self.target.name
            ));
        }
        self.confirm_writes.then(|| {
            format!(
                "Insert the file's rows into {}? The profile confirms writes.",
                self.target.name
            )
        })
    }

    fn text_format(&self) -> TextFormat {
        match self.format {
            ImportFormat::Csv => TextFormat::Csv(self.csv.options()),
            ImportFormat::Tsv => TextFormat::Tsv,
            ImportFormat::Json => TextFormat::Json,
            ImportFormat::JsonLines => TextFormat::JsonLines,
        }
    }

    fn start_preview(&mut self, engine: &sqlr_engine::Engine) {
        if let Some(path) = &self.path {
            self.preview = None;
            self.error = None;
            self.preview_job = Some(engine.preview_file(path.clone(), self.text_format()));
        }
    }

    /// The table's columns, once described.
    fn target_columns(&self, engine: &sqlr_engine::Engine) -> Option<Vec<String>> {
        // A collection has no columns: fields take the file's names.
        if self.collection.is_some() {
            return self.preview.as_ref().map(|p| p.columns.clone());
        }
        match engine.table_info(&self.conn, &self.target)? {
            LoadState::Ready(info) => Some(info.columns.iter().map(|c| c.name.clone()).collect()),
            _ => None,
        }
    }

    /// Runs the dialog for this frame; `false` once it is closed.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        engine: &sqlr_engine::Engine,
        answer: Option<&PathBuf>,
    ) -> bool {
        self.poll(ctx, engine);
        let targets = self.target_columns(engine);
        let mut open = true;
        let mut close = false;
        theme::tool_dialog(
            ctx,
            &mut open,
            &format!("Import into {}", self.target.name),
            theme::icons::IMPORT,
            "Preview a file, map its columns and review import options",
            700.0,
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("import-options")
                    .max_height((ctx.content_rect().height() - 340.0).max(140.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let shown = self
                                .path
                                .as_ref()
                                .map_or("No file chosen".to_string(), |p| p.display().to_string());
                            ui.label(shown);
                            if ui
                                .add_enabled(self.run.is_none(), egui::Button::new("Choose file…"))
                                .clicked()
                            {
                                self.ask = Some(FileAsk::open(
                                    answer,
                                    "Import from",
                                    &["csv", "tsv", "json", "jsonl", "ndjson", "txt"],
                                ));
                            }
                        });
                        let before = (self.format, self.csv.options());
                        ComboBox::from_label("Format")
                            .selected_text(self.format.label())
                            .show_ui(ui, |ui| {
                                for f in ImportFormat::ALL {
                                    ui.selectable_value(&mut self.format, f, f.label());
                                }
                            });
                        if self.format == ImportFormat::Csv {
                            egui::CollapsingHeader::new("CSV options")
                                .id_salt("import-csv-options")
                                .show(ui, |ui| self.csv.ui(ui, false));
                        }
                        if before != (self.format, self.csv.options()) {
                            self.start_preview(engine);
                        }
                        if let Some(e) = &self.error {
                            ui.colored_label(theme::palette(ui.ctx()).error, e);
                        }
                        if self.preview_job.is_some() {
                            ui.spinner();
                        }
                        if let Some(preview) = &mut self.preview {
                            ui.separator();
                            ui.label(format!("First {} rows", preview.rows.row_count()));
                            ui.allocate_ui(egui::vec2(ui.available_width(), 180.0), |ui| {
                                result_grid(ui, &mut preview.grid, &preview.rows, false);
                            });
                            ui.separator();
                            mapping_ui(ui, &preview.columns, &mut self.mapping, targets.as_deref());
                        }
                        ui.add_space(Spacing::M);
                        theme::hairline(ui);
                        ui.add_space(Spacing::M);
                        theme::section_title(ui, "Import options");
                        ui.add_space(Spacing::S);
                        options_ui(ui, &mut self.options, self.collection.is_none());
                    });
                ui.add_space(Spacing::M);
                theme::hairline(ui);
                ui.add_space(Spacing::M);
                self.run_ui(ui, engine, answer, &mut close);
            },
        );
        open && !close
    }

    fn run_ui(
        &mut self,
        ui: &mut egui::Ui,
        engine: &sqlr_engine::Engine,
        answer: Option<&PathBuf>,
        close: &mut bool,
    ) {
        let palette = theme::palette(ui.ctx());
        if let Some(job) = self.run {
            ui.horizontal(|ui| {
                ui.spinner();
                let rows = engine.job(job).map_or(0, |j| j.progress.rows);
                ui.label(format!("Importing… {rows} rows"));
                if ui.button("Cancel import").clicked() {
                    engine.cancel_job(job);
                }
            });
            return;
        }
        if let Some((read, written, rejected)) = &self.summary {
            ui.label(format!(
                "Read {read} rows, wrote {written}, rejected {}",
                rejected.len()
            ));
            for (line, reason) in rejected.iter().take(LISTED_REJECTED) {
                ui.label(RichText::new(format!("line {line}: {reason}")).color(palette.error));
            }
            if rejected.len() > LISTED_REJECTED {
                ui.label(format!("… {} more", rejected.len() - LISTED_REJECTED));
            }
            match &self.saved {
                Some(Ok(done)) => {
                    ui.colored_label(palette.success, done);
                }
                Some(Err(e)) => {
                    ui.colored_label(palette.error, e);
                }
                None if self.save_job.is_some() => {
                    ui.spinner();
                }
                None => {}
            }
            ui.horizontal(|ui| {
                if !rejected.is_empty() && ui.button("Save rejected rows…").clicked() {
                    let name = suggested_name(&format!("{}_rejected", self.target.name), "csv");
                    self.save_rejected = Some(FileAsk::save(answer, "Save rejected rows", &name));
                }
                if ui.button("Close").clicked() {
                    *close = true;
                }
            });
            return;
        }
        if let Some(prompt) = &self.confirming {
            ui.colored_label(palette.warning, prompt);
            let go = if self.options.truncate_first {
                "Delete and import"
            } else {
                "Import"
            };
            ui.horizontal(|ui| {
                if ui.button(go).clicked() {
                    self.confirming = None;
                    self.start(engine);
                }
                if ui.button("Back").clicked() {
                    self.confirming = None;
                }
            });
            return;
        }
        let ready = self.preview.is_some() && self.mapping.iter().any(Option::is_some);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    ready,
                    theme::primary_button_state(ui.ctx(), ready, "Run import"),
                )
                .clicked()
            {
                match self.run_prompt() {
                    Some(prompt) => self.confirming = Some(prompt),
                    None => self.start(engine),
                }
            }
            if ui.button("Cancel").clicked() {
                *close = true;
            }
        });
    }

    fn start(&mut self, engine: &sqlr_engine::Engine) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let mapping: Vec<(usize, Option<String>)> = self
            .mapping
            .iter()
            .enumerate()
            .map(|(i, t)| (i, t.clone()))
            .collect();
        self.error = None;
        if let Some(target) = &self.collection {
            self.run = Some(engine.transfer(TransferSpec {
                source: Endpoint::File {
                    path,
                    format: self.text_format(),
                },
                target: Endpoint::Collection {
                    conn: self.conn.clone(),
                    target: target.clone(),
                    query: None,
                },
                mapping: mapping.into_iter().map(Into::into).collect(),
                options: self.options.clone(),
            }));
            return;
        }
        self.run = Some(engine.import(
            &self.key,
            path,
            self.text_format(),
            self.target.clone(),
            mapping,
            self.options.clone(),
        ));
    }

    /// Picks up file dialogs and finished jobs.
    fn poll(&mut self, ctx: &egui::Context, engine: &sqlr_engine::Engine) {
        if let Some(ask) = &mut self.ask
            && let Some(answer) = ask.poll(ctx)
        {
            self.ask = None;
            if let Some(path) = answer {
                self.format = ImportFormat::of(&path);
                self.path = Some(path);
                self.summary = None;
                self.start_preview(engine);
            }
        }
        if let Some(ask) = &mut self.save_rejected
            && let Some(answer) = ask.poll(ctx)
        {
            self.save_rejected = None;
            if let (Some(path), Some((_, _, rejected))) = (answer, &self.summary) {
                let mut text = String::from("line,reason\n");
                for (line, reason) in rejected {
                    text.push_str(&format!("{line},\"{}\"\n", reason.replace('"', "\"\"")));
                }
                self.saved = None;
                self.save_job = Some(engine.save_text(path, text));
            }
        }
        if let Some(job) = self.save_job
            && let Some(outcome) = engine.job(job).and_then(|j| j.outcome)
        {
            self.save_job = None;
            engine.forget_job(job);
            self.saved = Some(match outcome {
                JobOutcome::Exported { path, .. } => {
                    Ok(format!("Saved the rejected rows to {}", path.display()))
                }
                JobOutcome::Failed(e) => {
                    Err(format!("Saving the rejected rows failed: {}", e.message))
                }
                _ => Err("Saving the rejected rows was cancelled".into()),
            });
        }
        if let Some(job) = self.preview_job
            && let Some(outcome) = engine.job(job).and_then(|j| j.outcome)
        {
            self.preview_job = None;
            engine.forget_job(job);
            match outcome {
                JobOutcome::Preview { columns, rows } => {
                    let targets = match self.collection {
                        Some(_) => columns.clone(),
                        None => self.target_columns(engine).unwrap_or_default(),
                    };
                    self.mapping = columns
                        .iter()
                        .map(|c| targets.iter().find(|t| t.eq_ignore_ascii_case(c)).cloned())
                        .collect();
                    self.preview = Some(Preview::new(columns, rows));
                }
                JobOutcome::Failed(e) => self.error = Some(e.message),
                _ => {}
            }
        }
        if let Some(job) = self.run
            && let Some(outcome) = engine.job(job).and_then(|j| j.outcome)
        {
            self.run = None;
            engine.forget_job(job);
            match outcome {
                JobOutcome::Imported {
                    read,
                    written,
                    rejected,
                } => self.summary = Some((read, written, rejected)),
                JobOutcome::Failed(e) => self.error = Some(e.message),
                JobOutcome::Cancelled => self.error = Some("The import was cancelled".into()),
                _ => {}
            }
        }
    }
}

fn mapping_ui(
    ui: &mut egui::Ui,
    sources: &[String],
    mapping: &mut [Option<String>],
    targets: Option<&[String]>,
) {
    let Some(targets) = targets else {
        ui.spinner();
        return;
    };
    egui::Grid::new("import_mapping")
        .num_columns(2)
        .spacing([Spacing::L, Spacing::S])
        .show(ui, |ui| {
            ui.label(RichText::new("File column").strong());
            ui.label(RichText::new("Table column").strong());
            ui.end_row();
            for (i, source) in sources.iter().enumerate() {
                ui.label(source);
                if let Some(slot) = mapping.get_mut(i) {
                    let shown = slot.as_deref().unwrap_or("skip").to_string();
                    ComboBox::from_id_salt(("import_map", i))
                        .selected_text(shown)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(slot, None, "skip");
                            for t in targets {
                                ui.selectable_value(slot, Some(t.clone()), t);
                            }
                        });
                }
                ui.end_row();
            }
        });
}

/// `table`: the target is a table (a collection has no transaction and is not emptied).
fn options_ui(ui: &mut egui::Ui, options: &mut ImportOptions, table: bool) {
    ui.horizontal(|ui| {
        ui.label("Batch size");
        ui.add(DragValue::new(&mut options.batch).range(1..=100_000));
    });
    if table {
        ui.checkbox(&mut options.commit_per_batch, "Commit after each batch");
    }
    ui.checkbox(&mut options.skip_bad_rows, "Skip bad rows and report them");
    if table {
        ui.checkbox(&mut options.truncate_first, "Delete the table's rows first");
    }
}

impl SqlrApp {
    /// The table a table or view node of `conn` stands for, with the database it lives in
    /// when it is not the connection's own.
    pub(crate) fn node_table(&self, conn: &str, node: &Node) -> Option<TableRef> {
        let (database, _) = self.engine.open_data_target(conn, node, None)?;
        let segments = node.path.segments();
        let schema = segments
            .len()
            .checked_sub(3)
            .and_then(|i| segments.get(i))
            .cloned();
        Some(TableRef {
            database,
            schema,
            name: node.name.clone(),
        })
    }

    /// "Export data…" on an explorer table or view: re-runs its `SELECT *`. On a document
    /// connection: every document of the collection or index; on Redis: the keys of the
    /// database, the prefix group or the key.
    pub(crate) fn open_node_export(&mut self, conn: &str, node: &Node) {
        if self.is_document(conn) {
            let Some((target, _)) = self.engine.open_doc_target(conn, &node.path, 0) else {
                self.notify(format!("Cannot export the documents of {}", node.name));
                return;
            };
            let source = Endpoint::Collection {
                conn: conn.to_string(),
                target,
                query: None,
            };
            self.export_dialog = Some(ExportDialog::for_endpoint(&node.name, source));
            return;
        }
        if self.is_key_value(conn) {
            let Some(db) = KvNodePath::db_of(&node.path) else {
                return;
            };
            let pattern = match (
                KvNodePath::key_of(&node.path),
                KvNodePath::prefix_of(&node.path),
            ) {
                (Some(key), _) => glob_escape(&key),
                (None, Some(prefix)) => {
                    let mut p = glob_escape(&prefix);
                    p.push(b'*');
                    p
                }
                (None, None) => b"*".to_vec(),
            };
            let source = Endpoint::RedisKeys {
                conn: conn.to_string(),
                db,
                pattern,
            };
            self.export_dialog = Some(ExportDialog::for_endpoint(&node.name, source));
            return;
        }
        let Some((database, sql)) = self.engine.open_data_target(conn, node, None) else {
            self.notify(format!("Cannot export the data of {}", node.name));
            return;
        };
        let rerun = Rerun {
            key: SessionKey::meta(conn),
            database,
            sql,
        };
        self.export_dialog = Some(ExportDialog::new(&node.name, None, Some(rerun), true));
    }

    /// "Export…" at the right of a result, in its status bar: its loaded rows, or the whole
    /// result re-run on the connection's explorer session.
    pub(crate) fn open_result_export(&mut self, tab: u64, q: QueryId) {
        let Some(t) = tab_mut(&mut self.dock, tab) else {
            return;
        };
        let Some(conn) = t.conn.clone() else {
            return;
        };
        let name = t
            .edits
            .get(&q)
            .and_then(|e| e.table.as_ref())
            .map_or(t.title.clone(), |table| table.name.clone());
        let database = t.database.clone();
        let Some((sql, rows, more)) = self.engine.result(q).map(|r| {
            let r = r.read();
            let more = r.status == sqlr_engine::QueryStatus::Ready { more: true };
            (r.sql.clone(), r.rows.row_count(), more)
        }) else {
            return;
        };
        // Only a statement that only reads may run again (the engine refuses others too).
        let reads = self
            .dialects
            .get(&conn)
            .is_some_and(|d| d.syntax().classify(&sql) == sqlr_core::StatementClass::Read);
        let rerun = reads.then(|| Rerun {
            key: SessionKey::meta(&conn),
            database,
            sql,
        });
        self.export_dialog = Some(ExportDialog::new(&name, Some((q, rows)), rerun, more));
    }

    /// "Import data…" on an explorer table (or a collection or index).
    pub(crate) fn open_node_import(&mut self, conn: &str, node: &Node) {
        if self.is_document(conn) {
            match self.engine.open_doc_target(conn, &node.path, 0) {
                Some((target, _)) => {
                    let confirm = self
                        .connection(conn)
                        .is_some_and(|c| self.profile_for(c).confirm_writes);
                    self.import_dialog = Some(ImportDialog::for_collection(conn, target, confirm));
                }
                None => self.notify(format!("Cannot import into {}", node.name)),
            }
            return;
        }
        let Some(target) = self.node_table(conn, node) else {
            self.notify(format!("Cannot import into {}", node.name));
            return;
        };
        let (destructive, writes) = self.connection(conn).map_or((false, false), |c| {
            let profile = self.profile_for(c);
            (profile.confirm_destructive, profile.confirm_writes)
        });
        self.engine
            .describe(&SessionKey::meta(conn), target.clone());
        self.import_dialog = Some(ImportDialog::new(conn, target, destructive, writes));
    }

    /// The export and import dialogs, and the jobs they started.
    pub(crate) fn transfer_ui(&mut self, ctx: &egui::Context) {
        if let Some(dialog) = &mut self.export_dialog {
            match dialog.show(ctx, self.file_answer.as_ref()) {
                ExportEvent::None => {}
                ExportEvent::Close => self.export_dialog = None,
                ExportEvent::Transfer {
                    source,
                    format,
                    path,
                } => {
                    let target = Endpoint::File { path, format };
                    let id = self.engine.transfer(TransferSpec {
                        source,
                        target,
                        mapping: Vec::new(),
                        options: ImportOptions::default(),
                    });
                    self.jobs.push(TrackedJob {
                        id,
                        what: JobWhat::Export,
                    });
                    self.export_dialog = None;
                }
                ExportEvent::Start {
                    source,
                    format,
                    path,
                } => {
                    let id = self.engine.export(source, format, path);
                    self.jobs.push(TrackedJob {
                        id,
                        what: JobWhat::Export,
                    });
                    self.export_dialog = None;
                }
            }
        }
        if let Some(dialog) = &mut self.import_dialog
            && !dialog.show(ctx, &self.engine, self.file_answer.as_ref())
        {
            self.import_dialog = None;
        }
        self.poll_jobs(ctx);
    }

    /// Ends the jobs that finished: notices, and the clipboard for copies.
    fn poll_jobs(&mut self, ctx: &egui::Context) {
        let engine = &self.engine;
        let mut finished = Vec::new();
        self.jobs
            .retain(|job| match engine.job(job.id).and_then(|j| j.outcome) {
                Some(outcome) => {
                    finished.push((job.id, outcome, job_what_copy(&job.what)));
                    false
                }
                None => engine.job(job.id).is_some(),
            });
        for (id, outcome, what) in finished {
            self.engine.forget_job(id);
            let (kind, text) = match (outcome, what) {
                (JobOutcome::Exported { rows, path }, None) => (
                    ToastKind::Success,
                    format!("Exported {rows} rows to {}", path.display()),
                ),
                (JobOutcome::Copied(text), Some(cells)) => {
                    ctx.copy_text(text);
                    (ToastKind::Success, format!("Copied {cells} cells"))
                }
                (JobOutcome::Failed(e), _) => (ToastKind::Error, format!("Failed: {}", e.message)),
                (JobOutcome::Cancelled, _) => (ToastKind::Info, "Cancelled".to_string()),
                _ => continue,
            };
            self.notify_as(kind, text);
        }
    }

    /// The running export (or copy) for the status bar: its line and job.
    pub(crate) fn job_line(&self) -> Option<(String, JobId)> {
        let job = self.jobs.first()?;
        let state = self.engine.job(job.id)?;
        let text = match job.what {
            JobWhat::Export => format!("Exporting… {} rows", state.progress.rows),
            JobWhat::Copy { cells } => format!("Copying {cells} cells…"),
        };
        Some((text, job.id))
    }
}

/// The cell count of a copy job, `None` for other jobs.
fn job_what_copy(what: &JobWhat) -> Option<usize> {
    match what {
        JobWhat::Copy { cells } => Some(*cells),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_collection_import_asks_when_the_profile_confirms_writes() {
        let target = DocTarget {
            database: Some("app".into()),
            collection: "users".into(),
        };
        let asks = ImportDialog::for_collection("m", target.clone(), true);
        assert_eq!(
            asks.run_prompt().as_deref(),
            Some("Insert the file's rows into users? The profile confirms writes.")
        );
        assert_eq!(
            ImportDialog::for_collection("m", target, false).run_prompt(),
            None
        );
    }

    #[test]
    fn suggested_names_are_safe_and_dated() {
        let name = suggested_name("my table/2", "csv");
        assert!(name.starts_with("my_table_2_"), "{name}");
        assert!(name.ends_with(".csv"));
        assert_eq!(name.len(), "my_table_2_".len() + "yyyymmdd_hhmm".len() + 4);
        assert_eq!(file_stem(""), "result");
        assert_eq!(
            ImportFormat::of(Path::new("a.JSONL")),
            ImportFormat::JsonLines
        );
        assert_eq!(ImportFormat::of(Path::new("a")), ImportFormat::Csv);
    }
}

#[cfg(test)]
mod file_ask_tests {
    use super::*;

    #[test]
    fn a_pending_native_dialog_keeps_the_ui_polling() {
        let ctx = egui::Context::default();
        let delays = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = delays.clone();
        ctx.set_request_repaint_callback(move |info| seen.lock().unwrap().push(info.delay));
        let mut ask = FileAsk::Native(Box::pin(std::future::pending()));
        assert!(ask.poll(&ctx).is_none(), "still open");
        let delays = delays.lock().unwrap();
        assert!(
            delays
                .iter()
                .any(|d| *d <= std::time::Duration::from_millis(100)),
            "{delays:?}"
        );
    }
}
