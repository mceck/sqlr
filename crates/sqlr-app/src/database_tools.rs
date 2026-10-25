//! The explorer's native dump and restore dialogs (spec §8.5): the options,
//! the command's log, progress and cancellation.
//!
//! The engine runs the tools; this module only collects the options and a path.

use std::path::PathBuf;

use egui::{ComboBox, DragValue, RichText, TextEdit, vec2};
use sqlr_core::{DumpFormat, DumpOptions, NodePath, RestoreOptions};
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
            DumpFormat::Plain => "SQL script",
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

    /// Why the run button is disabled: `missing_file` when no file is chosen.
    fn blocked(&self, missing_file: &'static str, both: bool) -> Option<&'static str> {
        if self.path.trim().is_empty() {
            Some(missing_file)
        } else if both {
            Some("Choose Schema only or Data only, not both")
        } else {
            None
        }
    }

    /// The shared footer: progress and a stop button while
    /// the job runs, otherwise Cancel and the `verb` primary. `true` when it was clicked.
    fn footer(
        &mut self,
        f: &mut theme::DialogFooter,
        engine: &Engine,
        verb: &str,
        blocked: Option<&str>,
        danger: bool,
    ) -> bool {
        if let Some(id) = self.job {
            f.left().spinner();
            f.left().label(self.progress_text());
            if f.button(&format!("Stop {}", verb.to_lowercase()), true) {
                engine.cancel_job(id);
            }
            return false;
        }
        let run = if danger {
            f.danger(verb, blocked)
        } else {
            f.primary(verb, blocked)
        };
        f.cancel_as(if self.outcome.is_some() {
            "Close"
        } else {
            "Cancel"
        });
        run
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

fn contents(ui: &mut egui::Ui, schema: &mut bool, data: &mut bool) {
    let mut choice = if *schema {
        1
    } else if *data {
        2
    } else {
        0
    };
    for (value, label) in [
        (0, "Everything"),
        (1, "Schema only (no rows)"),
        (2, "Data only (no definitions)"),
    ] {
        if ui.radio_value(&mut choice, value, label).changed() {
            *schema = choice == 1;
            *data = choice == 2;
        }
    }
}

/// One group of options: its name in a left column, its controls stacked on the right.
fn option_group(ui: &mut egui::Ui, name: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(Spacing::S);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(110.0, ui.spacing().interact_size.y),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(110.0);
                ui.label(RichText::new(name).color(theme::palette(ui.ctx()).text_muted));
            },
        );
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = Spacing::XS;
            add(ui);
        });
    });
}

/// An option that only applies when the one above it is on: indented and enabled with it.
fn dependent(ui: &mut egui::Ui, enabled: bool, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(22.0);
        ui.add_enabled_ui(enabled, add);
    });
}

/// A text field for the encoding, empty meaning the server's.
fn encoding_field(ui: &mut egui::Ui, encoding: &mut Option<String>) {
    let mut text = encoding.clone().unwrap_or_default();
    if ui
        .add(
            TextEdit::singleline(&mut text)
                .hint_text("Server default (UTF-8)")
                .desired_width(200.0),
        )
        .changed()
    {
        *encoding = (!text.trim().is_empty()).then_some(text);
    }
}

fn confirm_tool(ctx: &egui::Context, state: &ToolDialog, restore: bool) -> Option<bool> {
    let title = if restore {
        "Confirm restore"
    } else {
        "Confirm backup with DROP statements"
    };
    let primary = if restore {
        "Restore now"
    } else {
        "Create backup"
    };
    let spec = crate::confirm::ConfirmSpec::new(
        egui::Id::new(("confirm_database_tool", restore)),
        theme::icons::WARNING,
        title,
        primary,
    );
    crate::confirm::confirm_modal(
        ctx,
        spec,
        |ui| {
            ui.label(format!(
                "Database: {} on {}",
                state.database, state.conn_name
            ));
            ui.label(format!("File: {}", state.path));
            ui.colored_label(
                theme::palette(ui.ctx()).error,
                if restore && state.kind == DumpKind::Sqlite {
                    "The current database file will be replaced. Existing data will be lost."
                } else if restore {
                    "Existing database objects will be dropped before restoring the backup."
                } else {
                    "This backup includes DROP statements that delete objects when restored."
                },
            );
        },
        |_| {},
    )
}

/// The "Dump database…" dialog.
pub(crate) struct DumpDialog {
    state: ToolDialog,
    options: DumpOptions,
    confirming: bool,
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
            confirming: false,
        })
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
        let title = format!("Dump {}", self.state.database);
        let subtitle = format!(
            "Back up {} on {} to a file or archive",
            self.state.database, self.state.conn_name
        );
        let response = theme::Dialog::new("dump_dialog", theme::icons::EXPORT, &title)
            .subtitle(&subtitle)
            .width(680.0)
            .show(ctx, |d| {
                d.body(|ui| self.body(ui, answer));
                let blocked = self.state.blocked(
                    "Choose where to save the backup",
                    self.options.schema_only && self.options.data_only,
                );
                d.footer(|f| {
                    if self
                        .state
                        .footer(f, engine, "Dump", blocked, self.options.clean)
                    {
                        if self.options.clean {
                            self.confirming = true;
                        } else {
                            self.start(engine);
                        }
                    }
                });
            });
        if self.confirming {
            let answer = confirm_tool(ctx, &self.state, false);
            if let Some(confirmed) = answer {
                self.confirming = false;
                if confirmed {
                    self.start(engine);
                }
            }
        }
        if response.closed {
            self.state.close(engine);
        }
        !response.closed
    }

    fn start(&mut self, engine: &Engine) {
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

    fn body(&mut self, ui: &mut egui::Ui, answer: Option<&PathBuf>) {
        ui.label(
            RichText::new(
                if self.state.kind == DumpKind::Sqlite { "Back up the SQLite database to a SQL script or a consistent copy of its file." } else { "Back up the database with its native tool. Passwords are never included in command-line arguments." },
            )
            .small()
            .color(theme::palette(ui.ctx()).text_muted),
        );
        ui.add_space(Spacing::M);

        let mut format = self.options.format;
        let kind = self.state.kind;
        option_group(ui, "Format", |ui| {
            ComboBox::from_id_salt("dump_format")
                .selected_text(kind.format_label(format))
                .width(200.0)
                .show_ui(ui, |ui| {
                    for candidate in kind.formats() {
                        ui.selectable_value(&mut format, *candidate, kind.format_label(*candidate));
                    }
                });
        });
        self.options.format = format;

        let running = self.state.running();
        let mut browse = false;
        option_group(ui, "Save to", |ui| {
            ui.horizontal(|ui| {
                ui.add_enabled(
                    !running,
                    TextEdit::singleline(&mut self.state.path)
                        .desired_width((ui.available_width() - 90.0).max(80.0))
                        .hint_text("Choose where to save the backup…"),
                );
                browse = ui
                    .add_enabled(!running, egui::Button::new("Browse…"))
                    .clicked();
            });
        });
        if browse {
            let name = suggested_name(&self.state.database, self.options.format.extension());
            self.state.ask = Some(FileAsk::save(answer, "Dump to", &name));
        }
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
    }

    fn postgres_options(&mut self, ui: &mut egui::Ui) {
        let error = theme::palette(ui.ctx()).error;
        option_group(ui, "Contents", |ui| {
            contents(
                ui,
                &mut self.options.schema_only,
                &mut self.options.data_only,
            );
        });
        option_group(ui, "Statements", |ui| {
            ui.checkbox(
                &mut self.options.clean,
                RichText::new("Add DROP statements").color(error),
            )
            .on_hover_text("Restoring the backup drops each object before creating it again");
            let clean = self.options.clean;
            dependent(ui, clean, |ui| {
                ui.checkbox(&mut self.options.if_exists, "Use IF EXISTS")
                    .on_hover_text("The DROP statements skip objects that do not exist");
            });
            ui.checkbox(&mut self.options.create, "Include CREATE DATABASE");
        });
        option_group(ui, "Ownership", |ui| {
            ui.checkbox(&mut self.options.no_owner, "Leave out owners")
                .on_hover_text("No ALTER … OWNER: restored objects belong to whoever restores");
            ui.checkbox(&mut self.options.no_privileges, "Leave out privileges")
                .on_hover_text("No GRANT or REVOKE statements");
        });
        option_group(ui, "Encoding", |ui| {
            encoding_field(ui, &mut self.options.encoding);
        });
        let compressed = self.options.format == DumpFormat::Custom
            || self.options.format == DumpFormat::Directory;
        if compressed {
            option_group(ui, "Compression", |ui| {
                ui.horizontal(|ui| {
                    let mut enabled = self.options.compression.is_some();
                    ui.checkbox(&mut enabled, "Compress, level");
                    if enabled {
                        let mut level = self.options.compression.unwrap_or(6);
                        if ui.add(DragValue::new(&mut level).range(0..=9)).changed() {
                            self.options.compression = Some(level);
                        }
                    } else {
                        self.options.compression = None;
                    }
                });
            });
        }
        if self.options.format == DumpFormat::Directory {
            option_group(ui, "Parallel jobs", |ui| {
                let mut jobs = self.options.jobs.unwrap_or(1);
                if ui.add(DragValue::new(&mut jobs).range(1..=64)).changed() {
                    self.options.jobs = Some(jobs);
                }
            });
        }
    }

    fn mysql_options(&mut self, ui: &mut egui::Ui) {
        let error = theme::palette(ui.ctx()).error;
        option_group(ui, "Contents", |ui| {
            contents(
                ui,
                &mut self.options.schema_only,
                &mut self.options.data_only,
            );
        });
        option_group(ui, "Statements", |ui| {
            ui.checkbox(
                &mut self.options.clean,
                RichText::new("Add DROP statements").color(error),
            )
            .on_hover_text("Restoring the backup drops each table before creating it again");
            ui.checkbox(&mut self.options.create, "Include CREATE DATABASE");
        });
        option_group(ui, "Objects", |ui| {
            ui.checkbox(&mut self.options.mysql_routines, "Routines");
            ui.checkbox(&mut self.options.mysql_events, "Events");
            ui.checkbox(&mut self.options.mysql_triggers, "Triggers");
        });
        option_group(ui, "Consistency", |ui| {
            ui.checkbox(
                &mut self.options.mysql_single_transaction,
                "Consistent snapshot",
            )
            .on_hover_text(
                "Reads every transactional (InnoDB) table as of one moment, without locking",
            );
        });
        option_group(ui, "Encoding", |ui| {
            encoding_field(ui, &mut self.options.encoding);
        });
    }

    fn sqlite_options(&mut self, ui: &mut egui::Ui) {
        if self.options.format == DumpFormat::SqliteBinary {
            theme::hint(
                ui,
                "Writes a consistent copy of the database file, even while it is in use.",
            );
            return;
        }
        option_group(ui, "Contents", |ui| {
            ui.checkbox(&mut self.options.schema_only, "Schema only (no rows)");
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
    confirming: bool,
}

impl RestoreDialog {
    pub fn new(conn: &str, conn_name: &str, database: &str, driver: &str) -> Option<Self> {
        let kind = DumpKind::of(driver)?;
        Some(Self {
            state: ToolDialog::new(conn, conn_name, database, kind),
            options: RestoreOptions::default(),
            confirming: false,
        })
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
        let title = format!("Restore {}", self.state.database);
        let subtitle = format!(
            "Restore objects and data into {} on {} from a backup",
            self.state.database, self.state.conn_name
        );
        let response = theme::Dialog::new("restore_dialog", theme::icons::IMPORT, &title)
            .subtitle(&subtitle)
            .width(680.0)
            .show(ctx, |d| {
                d.body(|ui| self.body(ui, answer));
                let blocked = self.state.blocked(
                    "Choose a backup file",
                    self.options.schema_only && self.options.data_only,
                );
                d.footer(|f| {
                    if self.state.footer(
                        f,
                        engine,
                        "Restore",
                        blocked,
                        self.options.clean || self.state.kind == DumpKind::Sqlite,
                    ) {
                        if self.options.clean || self.state.kind == DumpKind::Sqlite {
                            self.confirming = true;
                        } else {
                            self.start(engine);
                        }
                    }
                });
            });
        if self.confirming {
            let answer = confirm_tool(ctx, &self.state, true);
            if let Some(confirmed) = answer {
                self.confirming = false;
                if confirmed {
                    self.start(engine);
                }
            }
        }
        if response.closed {
            self.state.close(engine);
        }
        !response.closed
    }

    fn start(&mut self, engine: &Engine) {
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

    fn body(&mut self, ui: &mut egui::Ui, answer: Option<&PathBuf>) {
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
        let formats = match self.state.kind {
            DumpKind::Postgres => {
                "Accepts .sql, .dump, .tar and directory archives; format is detected automatically."
            }
            DumpKind::Mysql => "Accepts SQL scripts (.sql).",
            DumpKind::Sqlite => {
                "Accepts SQL scripts (.sql) and SQLite database files (.db, .sqlite, .sqlite3)."
            }
        };
        theme::hint(ui, formats);
        if self.state.kind == DumpKind::Sqlite {
            ui.colored_label(theme::palette(ui.ctx()).error, "Restoring a database file replaces the entire connected database. You will confirm before it runs.");
        }
        ui.add_space(Spacing::L);
        if self.state.kind == DumpKind::Postgres {
            theme::section_title(ui, "Restore options");
            ui.add_space(Spacing::S);
            if self.is_plain_sql() {
                self.psql_options(ui);
            } else {
                self.postgres_options(ui);
            }
        }
        self.advanced(ui);
        self.state.log_ui(ui);
        self.state.outcome_ui(ui);
    }

    fn is_plain_sql(&self) -> bool {
        let path = self.state.path.trim().to_ascii_lowercase();
        path.ends_with(".sql")
    }

    fn psql_options(&mut self, ui: &mut egui::Ui) {
        theme::hint(
            ui,
            "A plain SQL file runs through psql: DROP and CREATE statements come from the \
             file itself.",
        );
        option_group(ui, "Execution", |ui| {
            ui.checkbox(
                &mut self.options.single_transaction,
                "Single transaction (all or nothing)",
            );
            ui.checkbox(&mut self.options.exit_on_error, "Stop at the first error");
        });
    }

    fn postgres_options(&mut self, ui: &mut egui::Ui) {
        let error = theme::palette(ui.ctx()).error;
        option_group(ui, "Contents", |ui| {
            contents(
                ui,
                &mut self.options.schema_only,
                &mut self.options.data_only,
            );
        });
        option_group(ui, "Existing objects", |ui| {
            ui.checkbox(
                &mut self.options.clean,
                RichText::new("Drop them before restoring").color(error),
            )
            .on_hover_text("Objects in the backup replace the ones of the same name");
            let clean = self.options.clean;
            dependent(ui, clean, |ui| {
                ui.checkbox(&mut self.options.if_exists, "Use IF EXISTS")
                    .on_hover_text("The drops skip objects that do not exist");
            });
            ui.checkbox(&mut self.options.create, "Create the database");
            if self.options.create {
                theme::hint(
                    ui,
                    "Connects to postgres and lets the backup create its own database.",
                );
            }
        });
        option_group(ui, "Ownership", |ui| {
            ui.checkbox(&mut self.options.no_owner, "Leave out owners");
            ui.checkbox(&mut self.options.no_privileges, "Leave out privileges");
        });
        option_group(ui, "Execution", |ui| {
            ui.checkbox(
                &mut self.options.single_transaction,
                "Single transaction (all or nothing)",
            );
            ui.checkbox(&mut self.options.exit_on_error, "Stop at the first error");
            if !self.options.single_transaction && !self.options.create {
                ui.horizontal(|ui| {
                    ui.label("Parallel jobs");
                    let mut jobs = self.options.jobs.unwrap_or(1);
                    if ui.add(DragValue::new(&mut jobs).range(1..=64)).changed() {
                        self.options.jobs = Some(jobs);
                    }
                });
            }
        });
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
    fn a_clean_restore_waits_for_confirmation_before_starting() {
        use egui_kittest::kittest::Queryable;
        let engine = Engine::new(
            sqlr_engine::DriverRegistry::default(),
            std::sync::Arc::new(|| {}),
        );
        let mut dialog = RestoreDialog::new("c", "Production", "orders", "postgres").unwrap();
        dialog.state.path = "/tmp/orders.dump".into();
        dialog.options.clean = true;
        let mut h = egui_kittest::Harness::builder()
            .with_size((1100.0, 900.0))
            .build_ui_state(
                |ui, dialog: &mut RestoreDialog| {
                    dialog.show(ui.ctx(), &engine, None);
                },
                dialog,
            );
        h.run_steps(3);
        h.get_by_label("Restore").click();
        h.run_steps(2);
        assert!(
            h.state().state.job.is_none(),
            "no restore starts before confirmation"
        );
        h.get_by_label("Confirm restore");
    }

    #[test]
    fn database_tools_offer_execution_without_task_saving() {
        use egui_kittest::kittest::Queryable;

        let engine = engine();
        let mut dump = DumpDialog::new("c", "conn", "app", "postgres").unwrap();
        dump.state.path = "/tmp/out.sql".into();
        let mut h = egui_kittest::Harness::builder()
            .with_size((900.0, 700.0))
            .build_ui_state(
                |ui, dialog: &mut DumpDialog| {
                    dialog.show(ui.ctx(), &engine, None);
                },
                dump,
            );
        h.run_steps(2);
        assert!(h.query_by_label("Save as task").is_none());
        h.get_by_label("Dump");

        let mut restore = RestoreDialog::new("c", "conn", "app", "postgres").unwrap();
        restore.state.path = "/tmp/in.sql".into();
        let mut h = egui_kittest::Harness::builder()
            .with_size((900.0, 700.0))
            .build_ui_state(
                |ui, dialog: &mut RestoreDialog| {
                    dialog.show(ui.ctx(), &engine, None);
                },
                restore,
            );
        h.run_steps(2);
        assert!(h.query_by_label("Save as task").is_none());
        h.get_by_label("Restore");
    }

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

    #[test]
    fn every_format_has_a_label_for_its_driver() {
        for kind in [DumpKind::Postgres, DumpKind::Mysql, DumpKind::Sqlite] {
            for format in kind.formats() {
                let label = kind.format_label(*format);
                assert!(!label.is_empty());
            }
        }
        assert_eq!(
            DumpKind::Sqlite.format_label(DumpFormat::Plain),
            "SQL script"
        );
        assert_eq!(
            DumpKind::Sqlite.format_label(DumpFormat::SqliteBinary),
            "Copy of the database file"
        );
        assert_eq!(
            DumpKind::Postgres.format_label(DumpFormat::Directory),
            "Directory (parallel)"
        );
    }

    #[test]
    fn every_dump_dialog_draws_its_options_and_log() {
        let engine = engine();
        for (driver, formats) in [
            (
                "postgres",
                &[
                    DumpFormat::Plain,
                    DumpFormat::Custom,
                    DumpFormat::Tar,
                    DumpFormat::Directory,
                ][..],
            ),
            ("mysql", &[DumpFormat::MysqlSql][..]),
            ("sqlite", &[DumpFormat::Plain, DumpFormat::SqliteBinary][..]),
        ] {
            let mut dialog = DumpDialog::new("c", "conn", "app", driver).unwrap();
            dialog.state.path = "/tmp/backup".into();
            dialog.state.log = vec!["writing rows".into(), "done".into()];
            dialog.state.progress = JobProgress {
                bytes: 2048,
                rows: 3,
            };
            for format in formats {
                dialog.options.format = *format;
                frame(&engine, |ctx| {
                    assert!(dialog.show(ctx, &engine, None));
                });
            }
            dialog.state.progress = JobProgress { bytes: 0, rows: 7 };
            dialog.state.outcome = Some(Err("boom".into()));
            frame(&engine, |ctx| {
                dialog.show(ctx, &engine, None);
            });
            dialog.state.outcome = Some(Ok("Dumped to /tmp/backup".into()));
            frame(&engine, |ctx| {
                dialog.show(ctx, &engine, None);
            });
        }
    }

    #[test]
    fn every_restore_dialog_draws_its_options() {
        let engine = engine();
        let mut pg = RestoreDialog::new("c", "conn", "app", "postgres").unwrap();
        for path in ["backup.sql", "backup.dump", "backup.tar"] {
            pg.state.path = path.into();
            frame(&engine, |ctx| {
                pg.show(ctx, &engine, None);
            });
        }
        for flag in [true, false] {
            pg.options.single_transaction = flag;
            pg.options.create = !flag;
            frame(&engine, |ctx| {
                pg.show(ctx, &engine, None);
            });
        }
        for driver in ["mysql", "sqlite"] {
            let mut dialog = RestoreDialog::new("c", "conn", "app", driver).unwrap();
            dialog.state.path = "backup.sql".into();
            frame(&engine, |ctx| {
                dialog.show(ctx, &engine, None);
            });
        }
    }

    #[test]
    fn progress_prefers_bytes_then_lines() {
        let mut state = ToolDialog::new("c", "conn", "app", DumpKind::Sqlite);
        assert_eq!(state.progress_text(), "working…");
        state.progress = JobProgress { bytes: 0, rows: 4 };
        assert_eq!(state.progress_text(), "4 log lines");
        state.progress = JobProgress {
            bytes: 1536,
            rows: 4,
        };
        assert_eq!(state.progress_text(), "1.5 KB processed");
    }

    #[test]
    fn bytes_are_human_readable() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
