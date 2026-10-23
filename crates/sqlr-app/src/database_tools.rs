//! The explorer's native dump and restore dialogs (spec §8.5): the options,
//! the command's log, progress and cancellation.
//!
//! The engine runs the tools; this module only collects the options and a path.

use std::path::PathBuf;

use egui::{ComboBox, DragValue, RichText, TextEdit};
use sqlr_core::{DumpFormat, DumpOptions, NodePath, RestoreOptions, SavedTask, TaskSpec};
use sqlr_engine::{Engine, JobId, JobOutcome, JobProgress};
use sqlr_ui::theme::{self, Spacing};

use crate::app::SqlrApp;
use crate::transfer_dialogs::{FileAsk, suggested_name};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DumpKind {
    Postgres,
    Mysql,
    Sqlite,
}

impl DumpKind {
    fn of(driver: &str) -> Option<Self> {
        match driver {
            "postgres" => Some(DumpKind::Postgres),
            "mysql" => Some(DumpKind::Mysql),
            "sqlite" => Some(DumpKind::Sqlite),
            _ => None,
        }
    }

    fn formats(self) -> &'static [DumpFormat] {
        match self {
            DumpKind::Postgres => &[
                DumpFormat::Plain,
                DumpFormat::Custom,
                DumpFormat::Tar,
                DumpFormat::Directory,
            ],
            DumpKind::Mysql => &[DumpFormat::MysqlSql],
            DumpKind::Sqlite => &[DumpFormat::Plain, DumpFormat::SqliteBinary],
        }
    }

    fn format_label(self, format: DumpFormat) -> &'static str {
        match format {
            DumpFormat::Plain if self == DumpKind::Sqlite => "SQL script",
            DumpFormat::Plain => "Plain SQL",
            DumpFormat::Custom => "Custom archive",
            DumpFormat::Tar => "Tar archive",
            DumpFormat::Directory => "Directory (parallel)",
            DumpFormat::MysqlSql => "SQL script",
            DumpFormat::SqliteBinary => "Copy of the database file",
        }
    }
}

/// The state of a dump or restore dialog, including its running job.
struct ToolDialog {
    conn: String,
    conn_name: String,
    database: String,
    kind: DumpKind,
    path: String,
    ask: Option<FileAsk>,
    job: Option<JobId>,
    progress: JobProgress,
    log: Vec<String>,
    outcome: Option<Result<String, String>>,
}

impl ToolDialog {
    fn new(conn: &str, conn_name: &str, database: &str, kind: DumpKind) -> Self {
        Self {
            conn: conn.to_string(),
            conn_name: conn_name.to_string(),
            database: database.to_string(),
            kind,
            path: String::new(),
            ask: None,
            job: None,
            progress: JobProgress::default(),
            log: Vec::new(),
            outcome: None,
        }
    }

    /// Polls the running job: keeps its log and turns its outcome into a message.
    fn poll(&mut self, engine: &Engine) {
        let Some(id) = self.job else {
            return;
        };
        let Some(state) = engine.job(id) else {
            self.job = None;
            self.outcome = Some(Err("The job disappeared".into()));
            return;
        };
        self.progress = state.progress;
        self.log = state.log.clone();
        let Some(outcome) = state.outcome else {
            return;
        };
        engine.forget_job(id);
        self.job = None;
        self.outcome = Some(match outcome {
            JobOutcome::Dumped { path } => Ok(format!("Dumped to {}", path.display())),
            JobOutcome::Restored { path } => {
                // The archive may have created or replaced objects.
                engine.refresh(&self.conn, NodePath::root());
                Ok(format!("Restored from {}", path.display()))
            }
            JobOutcome::Failed(e) => Err(e.message),
            JobOutcome::Cancelled => Err("Cancelled".into()),
            _ => Ok("Done".into()),
        });
    }

    fn running(&self) -> bool {
        self.job.is_some()
    }

    fn close(&mut self, engine: &Engine) {
        if let Some(id) = self.job.take() {
            engine.cancel_job(id);
            engine.forget_job(id);
        }
    }

    fn progress_text(&self) -> String {
        if self.progress.bytes > 0 {
            format!("{} processed", human_bytes(self.progress.bytes))
        } else if self.progress.rows > 0 {
            format!("{} log lines", self.progress.rows)
        } else {
            "working…".into()
        }
    }

    fn log_ui(&self, ui: &mut egui::Ui) {
        if self.log.is_empty() {
            return;
        }
        ui.add_space(Spacing::S);
        ui.label(RichText::new("Log").strong());
        egui::ScrollArea::vertical()
            .max_height(190.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.log {
                    ui.monospace(line);
                }
            });
    }

    fn outcome_ui(&self, ui: &mut egui::Ui) {
        let Some(outcome) = &self.outcome else {
            return;
        };
        ui.add_space(Spacing::S);
        match outcome {
            Ok(message) => {
                ui.colored_label(theme::palette(ui.ctx()).success, message);
            }
            Err(message) => {
                ui.colored_label(theme::palette(ui.ctx()).error, message);
            }
        }
    }
}

/// The "Dump database…" dialog.
pub(crate) struct DumpDialog {
    state: ToolDialog,
    options: DumpOptions,
    /// "Save as task" was clicked.
    save_requested: bool,
    /// The saved task being edited: its id and name.
    editing: Option<(String, String)>,
}

impl DumpDialog {
    pub fn new(conn: &str, conn_name: &str, database: &str, driver: &str) -> Option<Self> {
        let kind = DumpKind::of(driver)?;
        let format = kind.formats()[0];
        Some(Self {
            state: ToolDialog::new(conn, conn_name, database, kind),
            options: DumpOptions {
                format,
                ..Default::default()
            },
            save_requested: false,
            editing: None,
        })
    }

    /// The dialog reopened on a saved dump task.
    pub fn from_task(app: &SqlrApp, task: &SavedTask) -> Option<Self> {
        let TaskSpec::Dump {
            conn,
            database,
            options,
            out,
        } = &task.spec
        else {
            return None;
        };
        let cfg = app.connection(conn)?;
        let mut dialog = Self::new(conn, &cfg.name, database, &cfg.driver)?;
        dialog.options = options.clone();
        dialog.state.path = out.display().to_string();
        dialog.editing = Some((task.id.clone(), task.name.clone()));
        Some(dialog)
    }

    /// The task to save, once "Save as task" was clicked.
    pub fn take_task(&mut self) -> Option<(TaskSpec, Option<(String, String)>)> {
        if !std::mem::take(&mut self.save_requested) {
            return None;
        }
        Some((
            TaskSpec::Dump {
                conn: self.state.conn.clone(),
                database: self.state.database.clone(),
                options: self.options.clone(),
                out: PathBuf::from(self.state.path.trim()),
            },
            self.editing.clone(),
        ))
    }

    /// Returns `false` once the dialog should close.
    pub fn show(&mut self, ctx: &egui::Context, engine: &Engine, answer: Option<&PathBuf>) -> bool {
        self.state.poll(engine);
        if let Some(ask) = &mut self.state.ask {
            match ask.poll(ctx) {
                None => {}
                Some(None) => self.state.ask = None,
                Some(Some(path)) => {
                    self.state.ask = None;
                    self.state.path = path.display().to_string();
                }
            }
        }
        let mut open = true;
        theme::tool_dialog(
            ctx,
            &mut open,
            "Dump database",
            theme::icons::EXPORT,
            "Back up a database to a file or archive",
            680.0,
            |ui| {
                self.body(ui, engine, answer);
            },
        );
        if !open {
            self.state.close(engine);
        }
        open
    }

    fn body(&mut self, ui: &mut egui::Ui, engine: &Engine, answer: Option<&PathBuf>) {
        ui.label(
            RichText::new(format!(
                "{} · {}",
                self.state.database, self.state.conn_name
            ))
            .font(theme::semibold(ui.ctx(), 13.0)),
        );
        ui.label(
            RichText::new(
                "Dumps the whole database with its native tool. The password is passed to \
                 the tool out of the command line.",
            )
            .small()
            .color(theme::palette(ui.ctx()).text_muted),
        );
        ui.add_space(Spacing::M);

        let mut format = self.options.format;
        ComboBox::from_label("Format")
            .selected_text(self.state.kind.format_label(format))
            .show_ui(ui, |ui| {
                for candidate in self.state.kind.formats() {
                    ui.selectable_value(
                        &mut format,
                        *candidate,
                        self.state.kind.format_label(*candidate),
                    );
                }
            });
        self.options.format = format;

        ui.horizontal(|ui| {
            ui.label("File");
            ui.add_enabled(
                !self.state.running(),
                TextEdit::singleline(&mut self.state.path)
                    .desired_width((ui.available_width() - 130.0).max(80.0))
                    .hint_text("Choose a backup file…"),
            );
            if ui
                .add_enabled(!self.state.running(), egui::Button::new("Browse…"))
                .clicked()
            {
                let name = suggested_name(&self.state.database, self.options.format.extension());
                self.state.ask = Some(FileAsk::save(answer, "Dump to", &name));
            }
        });
        if self.options.format == DumpFormat::Directory {
            ui.label(
                RichText::new(
                    "The directory is created by pg_dump; it must not exist or be empty.",
                )
                .small()
                .color(theme::palette(ui.ctx()).text_muted),
            );
        }

        ui.add_space(Spacing::L);
        theme::section_title(ui, "Backup options");
        ui.add_space(Spacing::S);
        match self.state.kind {
            DumpKind::Postgres => self.postgres_options(ui),
            DumpKind::Mysql => self.mysql_options(ui),
            DumpKind::Sqlite => self.sqlite_options(ui),
        }
        self.advanced(ui);
        self.state.log_ui(ui);
        self.state.outcome_ui(ui);

        ui.add_space(Spacing::L);
        theme::hairline(ui);
        ui.add_space(Spacing::M);
        ui.horizontal(|ui| {
            let valid = !self.state.path.trim().is_empty()
                && !(self.options.schema_only && self.options.data_only)
                && !self.state.running();
            if ui
                .add_enabled(valid, egui::Button::new("Save as task"))
                .clicked()
            {
                self.save_requested = true;
            }
            let button = theme::primary_button_state(ui.ctx(), valid, "Dump");
            if ui.add_enabled(valid, button).clicked() {
                let path = PathBuf::from(self.state.path.trim());
                let id = engine.run_dump(
                    &self.state.conn,
                    &self.state.database,
                    self.options.clone(),
                    path,
                );
                self.state.job = Some(id);
                self.state.log.clear();
                self.state.outcome = None;
            }
            if self.state.running() {
                if ui.button("Cancel").clicked()
                    && let Some(id) = self.state.job
                {
                    engine.cancel_job(id);
                }
                ui.spinner();
                ui.label(self.state.progress_text());
            }
        });
    }

    fn postgres_options(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.options.schema_only, "Schema only");
            ui.checkbox(&mut self.options.data_only, "Data only");
            ui.checkbox(&mut self.options.clean, "Add DROP statements");
            ui.checkbox(&mut self.options.if_exists, "Use IF EXISTS");
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.options.create, "Create the database");
            ui.checkbox(&mut self.options.no_owner, "No owner");
            ui.checkbox(&mut self.options.no_privileges, "No privileges");
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Encoding");
            let mut encoding = self.options.encoding.clone().unwrap_or_default();
            if ui
                .add(TextEdit::singleline(&mut encoding).desired_width(120.0))
                .changed()
            {
                self.options.encoding = (!encoding.trim().is_empty()).then_some(encoding);
            }
            if self.options.format == DumpFormat::Custom
                || self.options.format == DumpFormat::Directory
            {
                ui.label("Compression");
                let mut enabled = self.options.compression.is_some();
                ui.checkbox(&mut enabled, "");
                if enabled {
                    let mut level = self.options.compression.unwrap_or(6);
                    if ui.add(DragValue::new(&mut level).range(0..=9)).changed() {
                        self.options.compression = Some(level);
                    }
                } else {
                    self.options.compression = None;
                }
            }
            if self.options.format == DumpFormat::Directory {
                ui.label("Jobs");
                let mut jobs = self.options.jobs.unwrap_or(1);
                if ui.add(DragValue::new(&mut jobs).range(1..=64)).changed() {
                    self.options.jobs = Some(jobs);
                }
            }
        });
    }

    fn mysql_options(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.options.schema_only, "Schema only");
            ui.checkbox(&mut self.options.data_only, "Data only");
            ui.checkbox(&mut self.options.clean, "Add DROP statements");
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.options.create, "Include CREATE DATABASE");
            ui.label("Character set");
            let mut encoding = self.options.encoding.clone().unwrap_or_default();
            if ui
                .add(TextEdit::singleline(&mut encoding).desired_width(120.0))
                .changed()
            {
                self.options.encoding = (!encoding.trim().is_empty()).then_some(encoding);
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.options.mysql_routines, "Routines");
            ui.checkbox(&mut self.options.mysql_events, "Events");
            ui.checkbox(&mut self.options.mysql_triggers, "Triggers");
            ui.checkbox(
                &mut self.options.mysql_single_transaction,
                "Single transaction (InnoDB snapshot)",
            );
        });
    }

    fn sqlite_options(&mut self, ui: &mut egui::Ui) {
        if self.options.format == DumpFormat::SqliteBinary {
            ui.label(
                RichText::new("Writes a consistent copy of the database file with .backup.")
                    .small()
                    .color(theme::palette(ui.ctx()).text_muted),
            );
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.options.schema_only, "Schema only");
        });
    }

    fn advanced(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Advanced")
            .id_salt("dump_advanced")
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Executable");
                    ui.add(
                        TextEdit::singleline(&mut self.options.executable)
                            .desired_width(320.0)
                            .hint_text("auto-detect"),
                    )
                    .on_hover_text(
                        "Empty searches PATH, Homebrew, Postgres.app and /usr/local/bin",
                    );
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Extra arguments");
                    ui.add(
                        TextEdit::singleline(&mut self.options.extra_args)
                            .desired_width(420.0)
                            .hint_text("e.g. --no-tablespaces"),
                    )
                    .on_hover_text("Arguments are shown in the log: do not put passwords here.");
                });
            });
    }
}

/// The "Restore database…" dialog.
pub(crate) struct RestoreDialog {
    state: ToolDialog,
    options: RestoreOptions,
    /// "Save as task" was clicked.
    save_requested: bool,
    /// The saved task being edited: its id and name.
    editing: Option<(String, String)>,
}

impl RestoreDialog {
    pub fn new(conn: &str, conn_name: &str, database: &str, driver: &str) -> Option<Self> {
        let kind = DumpKind::of(driver)?;
        Some(Self {
            state: ToolDialog::new(conn, conn_name, database, kind),
            options: RestoreOptions::default(),
            save_requested: false,
            editing: None,
        })
    }

    /// The dialog reopened on a saved restore task.
    pub fn from_task(app: &SqlrApp, task: &SavedTask) -> Option<Self> {
        let TaskSpec::Restore {
            conn,
            database,
            options,
            input,
        } = &task.spec
        else {
            return None;
        };
        let cfg = app.connection(conn)?;
        let mut dialog = Self::new(conn, &cfg.name, database, &cfg.driver)?;
        dialog.options = options.clone();
        dialog.state.path = input.display().to_string();
        dialog.editing = Some((task.id.clone(), task.name.clone()));
        Some(dialog)
    }

    /// The task to save, once "Save as task" was clicked.
    pub fn take_task(&mut self) -> Option<(TaskSpec, Option<(String, String)>)> {
        if !std::mem::take(&mut self.save_requested) {
            return None;
        }
        Some((
            TaskSpec::Restore {
                conn: self.state.conn.clone(),
                database: self.state.database.clone(),
                options: self.options.clone(),
                input: PathBuf::from(self.state.path.trim()),
            },
            self.editing.clone(),
        ))
    }

    pub fn show(&mut self, ctx: &egui::Context, engine: &Engine, answer: Option<&PathBuf>) -> bool {
        self.state.poll(engine);
        if let Some(ask) = &mut self.state.ask {
            match ask.poll(ctx) {
                None => {}
                Some(None) => self.state.ask = None,
                Some(Some(path)) => {
                    self.state.ask = None;
                    self.state.path = path.display().to_string();
                }
            }
        }
        let mut open = true;
        theme::tool_dialog(
            ctx,
            &mut open,
            "Restore database",
            theme::icons::IMPORT,
            "Restore objects and data from an existing backup",
            680.0,
            |ui| {
                self.body(ui, engine, answer);
            },
        );
        if !open {
            self.state.close(engine);
        }
        open
    }

    fn body(&mut self, ui: &mut egui::Ui, engine: &Engine, answer: Option<&PathBuf>) {
        ui.label(
            RichText::new(format!(
                "{} · {}",
                self.state.database, self.state.conn_name
            ))
            .font(theme::semibold(ui.ctx(), 13.0)),
        );
        ui.label(
            RichText::new(
                "Restores an archive with the database's native tool. Restoring can replace \
                 existing objects.",
            )
            .small()
            .color(theme::palette(ui.ctx()).text_muted),
        );
        ui.add_space(Spacing::M);
        ui.horizontal(|ui| {
            ui.label("File");
            ui.add_enabled(
                !self.state.running(),
                TextEdit::singleline(&mut self.state.path)
                    .desired_width((ui.available_width() - 130.0).max(80.0))
                    .hint_text("Choose a backup file…"),
            );
            if ui
                .add_enabled(!self.state.running(), egui::Button::new("Browse…"))
                .clicked()
            {
                let extensions: &[&str] = match self.state.kind {
                    DumpKind::Postgres => &["sql", "dump", "tar", "backup"],
                    DumpKind::Mysql => &["sql"],
                    DumpKind::Sqlite => &["sql", "db", "sqlite", "sqlite3"],
                };
                self.state.ask = Some(FileAsk::open(answer, "Restore from", extensions));
            }
        });
        if self.state.kind == DumpKind::Sqlite {
            ui.label(
                RichText::new(
                    "A .sql file runs through sqlite3; a database file is copied over the \
                     connection's file.",
                )
                .small()
                .color(theme::palette(ui.ctx()).text_muted),
            );
        }
        ui.add_space(Spacing::L);
        theme::section_title(ui, "Restore options");
        ui.add_space(Spacing::S);
        if self.state.kind == DumpKind::Postgres {
            if self.is_plain_sql() {
                self.psql_options(ui);
            } else {
                self.postgres_options(ui);
            }
        }
        self.advanced(ui);
        self.state.log_ui(ui);
        self.state.outcome_ui(ui);
        ui.add_space(Spacing::L);
        theme::hairline(ui);
        ui.add_space(Spacing::M);
        ui.horizontal(|ui| {
            let valid = !self.state.path.trim().is_empty()
                && !(self.options.schema_only && self.options.data_only)
                && !self.state.running();
            let button = theme::primary_button_state(ui.ctx(), valid, "Restore");
            if ui.add_enabled(valid, button).clicked() {
                let path = PathBuf::from(self.state.path.trim());
                let id = engine.run_restore(
                    &self.state.conn,
                    &self.state.database,
                    self.options.clone(),
                    path,
                );
                self.state.job = Some(id);
                self.state.log.clear();
                self.state.outcome = None;
            }
            if self.state.running() {
                if ui.button("Cancel").clicked()
                    && let Some(id) = self.state.job
                {
                    engine.cancel_job(id);
                }
                ui.spinner();
                ui.label(self.state.progress_text());
            }
        });
    }

    fn is_plain_sql(&self) -> bool {
        let path = self.state.path.trim().to_ascii_lowercase();
        path.ends_with(".sql")
    }

    fn psql_options(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new(
                "A plain SQL file runs through psql: DROP/CREATE statements come from the \
                 file itself.",
            )
            .small()
            .color(theme::palette(ui.ctx()).text_muted),
        );
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(
                &mut self.options.single_transaction,
                "Single transaction (all or nothing)",
            );
            ui.checkbox(&mut self.options.exit_on_error, "Stop at the first error");
        });
    }

    fn postgres_options(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.options.clean, "Clean (drop before restore)");
            ui.checkbox(&mut self.options.if_exists, "Use IF EXISTS");
            ui.checkbox(&mut self.options.create, "Create the database");
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.options.schema_only, "Schema only");
            ui.checkbox(&mut self.options.data_only, "Data only");
            ui.checkbox(&mut self.options.no_owner, "No owner");
            ui.checkbox(&mut self.options.no_privileges, "No privileges");
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(
                &mut self.options.single_transaction,
                "Single transaction (all or nothing)",
            );
            ui.checkbox(&mut self.options.exit_on_error, "Stop at the first error");
            if !self.options.single_transaction && !self.options.create {
                ui.label("Jobs");
                let mut jobs = self.options.jobs.unwrap_or(1);
                if ui.add(DragValue::new(&mut jobs).range(1..=64)).changed() {
                    self.options.jobs = Some(jobs);
                }
            }
        });
        if self.options.create {
            ui.label(
                RichText::new(
                    "Create the database connects to postgres and lets the archive create \
                     its own database.",
                )
                .small()
                .color(theme::palette(ui.ctx()).text_muted),
            );
        }
    }

    fn advanced(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Advanced")
            .id_salt("restore_advanced")
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Executable");
                    ui.add(
                        TextEdit::singleline(&mut self.options.executable)
                            .desired_width(320.0)
                            .hint_text("auto-detect"),
                    );
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Extra arguments");
                    ui.add(
                        TextEdit::singleline(&mut self.options.extra_args)
                            .desired_width(420.0)
                            .hint_text("e.g. --no-tablespaces"),
                    )
                    .on_hover_text("Arguments are shown in the log: do not put passwords here.");
                });
            });
    }
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

impl SqlrApp {
    /// "Dump database…" on a database node.
    pub(crate) fn open_database_dump(&mut self, conn: &str, database: &str) {
        let Some((name, driver)) = self
            .connection(conn)
            .map(|c| (c.name.clone(), c.driver.clone()))
        else {
            return;
        };
        self.dump_dialog = DumpDialog::new(conn, &name, database, &driver);
    }

    /// "Restore database…" on a database node.
    pub(crate) fn open_database_restore(&mut self, conn: &str, database: &str) {
        let Some((name, driver)) = self
            .connection(conn)
            .map(|c| (c.name.clone(), c.driver.clone()))
        else {
            return;
        };
        self.restore_dialog = RestoreDialog::new(conn, &name, database, &driver);
    }

    /// The dump and restore dialogs and the jobs they run.
    pub(crate) fn database_tools_ui(&mut self, ctx: &egui::Context) {
        let engine = &self.engine;
        let answer = self.file_answer.as_ref();
        if let Some(dialog) = &mut self.dump_dialog
            && !dialog.show(ctx, engine, answer)
        {
            self.dump_dialog = None;
        }
        if let Some(dialog) = &mut self.restore_dialog
            && !dialog.show(ctx, engine, answer)
        {
            self.restore_dialog = None;
        }
        let tasks = [
            self.dump_dialog.as_mut().and_then(DumpDialog::take_task),
            self.restore_dialog
                .as_mut()
                .and_then(RestoreDialog::take_task),
        ];
        for (spec, editing) in tasks.into_iter().flatten() {
            self.save_task_dialog(spec, editing);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relational_drivers_offer_the_dialogs() {
        assert!(DumpDialog::new("c", "conn", "app", "postgres").is_some());
        assert!(DumpDialog::new("c", "conn", "app", "mysql").is_some());
        assert!(DumpDialog::new("c", "conn", "app", "sqlite").is_some());
        assert!(DumpDialog::new("c", "conn", "app", "mongodb").is_none());
        assert!(RestoreDialog::new("c", "conn", "app", "postgres").is_some());
        assert!(RestoreDialog::new("c", "conn", "app", "redis").is_none());
    }

    #[test]
    fn the_first_format_matches_the_driver() {
        let pg = DumpDialog::new("c", "conn", "app", "postgres").unwrap();
        assert_eq!(pg.options.format, DumpFormat::Plain);
        let mysql = DumpDialog::new("c", "conn", "app", "mysql").unwrap();
        assert_eq!(mysql.options.format, DumpFormat::MysqlSql);
        let sqlite = DumpDialog::new("c", "conn", "app", "sqlite").unwrap();
        assert_eq!(sqlite.options.format, DumpFormat::Plain);
    }
}
