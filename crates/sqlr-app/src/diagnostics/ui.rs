//! The Diagnostics window, the startup banner and Report a Problem.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use egui::{Align2, Frame, Id, Margin, RichText, ScrollArea, vec2};
use sqlr_engine::{OperationState, SessionKey, SessionStatus, TxPhase};
use sqlr_ui::theme::{self, Spacing, icons};

use crate::app::SqlrApp;
use crate::diagnostics::model::StartupNotice;
use crate::diagnostics::{
    ApplicationInfo, BundleInputs, BundleOutline, ConnectionInfo, DriverInfo, Environment,
    Incident, IncidentSummary, InterfaceInfo, IssueDraft, LogLevel, OperationSnapshot, TabInfo,
};

/// The window's state, owned by the application.
#[derive(Default)]
pub struct DiagnosticsUi {
    pub open: bool,
    pub selected: Option<String>,
    pub selected_incident: Option<Incident>,
    pub outline: Option<BundleOutline>,
    pub preview: Option<String>,
    pub preview_open: bool,
    /// The GitHub issue prefilled from the previewed bundle.
    pub issue: Option<IssueDraft>,
    pub status: Option<String>,
    pub incidents: Vec<IncidentSummary>,
    pub(crate) loaded: bool,
    /// A bundle being built or saved on a worker thread.
    pub pending: Option<PendingBundle>,
    /// The previous session's notice, until Dismiss.
    pub notice: Option<StartupNotice>,
    pub report: Option<ReportProblemDialog>,
    store_pending: Vec<Receiver<StoreResult>>,
}

enum StoreResult {
    Incidents(Vec<IncidentSummary>),
    Selected(String, Option<Box<Incident>>),
    Acknowledged,
    Manual(Option<Box<Incident>>, bool, bool, BundleAction),
    Copied(Option<String>),
    Saved(Result<(), String>),
}

pub struct PendingBundle {
    pub receiver: Receiver<PendingBundleResult>,
    pub action: BundleAction,
}

pub enum PendingBundleResult {
    Built(String, BundleOutline),
    Saved(std::io::Result<()>),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BundleAction {
    Copy,
    Save,
    Preview,
    Issue,
}

/// Report a Problem: a free-text note and what to include.
pub struct ReportProblemDialog {
    pub note: String,
    pub include_sql: bool,
    pub include_titles: bool,
}

impl ReportProblemDialog {
    pub fn new(include_sql: bool) -> Self {
        Self {
            note: String::new(),
            include_sql,
            include_titles: true,
        }
    }
}

impl SqlrApp {
    /// Draws the startup banner, the Diagnostics window and Report a Problem, and collects
    /// finished bundle work.
    pub(crate) fn diagnostics_ui(&mut self, ctx: &egui::Context) {
        self.poll_diagnostics_store(ctx);
        self.poll_bundle(ctx);
        self.bundle_preview_ui(ctx);
        self.startup_banner_ui(ctx);
        if self.diagnostics_ui.open {
            if !self.diagnostics_ui.loaded {
                self.refresh_incidents();
                self.diagnostics_ui.loaded = true;
            }
            self.diagnostics_window_ui(ctx);
        }
        self.report_problem_ui(ctx);
    }

    fn store_work(
        &mut self,
        work: impl FnOnce(&crate::diagnostics::Diagnostics) -> StoreResult + Send + 'static,
    ) {
        let Some(diagnostics) = self.diagnostics.clone() else {
            return;
        };
        let worker = diagnostics.clone();
        let repaint = self.ui_ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        if diagnostics.background(move || {
            let result = work(&worker);
            let _ = tx.send(result);
            if let Some(ctx) = repaint {
                ctx.request_repaint();
            }
        }) {
            self.diagnostics_ui.store_pending.push(rx);
        } else {
            self.diagnostics_ui.status = Some("Diagnostics writer is unavailable".into());
        }
    }

    fn refresh_incidents(&mut self) {
        self.store_work(|diagnostics| StoreResult::Incidents(diagnostics.incidents()));
    }

    fn load_selected(&mut self, id: &str) {
        self.diagnostics_ui.selected = Some(id.to_string());
        self.diagnostics_ui.selected_incident = None;
        let id = id.to_string();
        self.store_work(move |diagnostics| {
            let incident = diagnostics.read_incident(&id).map(Box::new);
            StoreResult::Selected(id, incident)
        });
    }

    fn poll_diagnostics_store(&mut self, ctx: &egui::Context) {
        let mut waiting = Vec::new();
        for rx in std::mem::take(&mut self.diagnostics_ui.store_pending) {
            match rx.try_recv() {
                Ok(StoreResult::Incidents(incidents)) => self.diagnostics_ui.incidents = incidents,
                Ok(StoreResult::Selected(id, incident)) => {
                    if self.diagnostics_ui.selected.as_ref() == Some(&id) {
                        self.diagnostics_ui.selected_incident = incident.map(|i| *i);
                    }
                }
                Ok(StoreResult::Acknowledged) => self.refresh_incidents(),
                Ok(StoreResult::Manual(incident, sql, titles, action)) => {
                    if let Some(incident) = incident {
                        self.diagnostics_ui.selected = Some(incident.id.clone());
                        self.diagnostics_ui.selected_incident = Some(*incident.clone());
                        self.diagnostics_ui.open = true;
                        self.refresh_incidents();
                        self.start_bundle(action, ctx, sql, titles, Some(*incident));
                    } else {
                        self.notify("Could not record the report: diagnostics are not writable");
                    }
                }
                Ok(StoreResult::Copied(Some(json))) => {
                    ctx.copy_text(json);
                    self.diagnostics_ui.status = Some("Incident copied".into());
                }
                Ok(StoreResult::Copied(None)) => {
                    self.diagnostics_ui.status = Some("Incident is no longer available".into())
                }
                Ok(StoreResult::Saved(result)) => {
                    self.diagnostics_ui.status = Some(match result {
                        Ok(()) => "Diagnostics bundle saved".into(),
                        Err(error) => format!("Could not save the bundle: {error}"),
                    });
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => waiting.push(rx),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.diagnostics_ui.status = Some("Diagnostics worker stopped".into())
                }
            }
        }
        self.diagnostics_ui.store_pending.extend(waiting);
    }

    fn startup_banner_ui(&mut self, ctx: &egui::Context) {
        let Some(notice) = self.diagnostics_ui.notice.clone() else {
            return;
        };
        let palette = theme::palette(ctx);
        let mut open_report = false;
        let mut report_issue = false;
        let mut copy = false;
        let mut dismiss = false;
        egui::Area::new(Id::new("startup-notice"))
            .anchor(Align2::CENTER_TOP, [0.0, 10.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(palette.bg_raised)
                    .stroke(egui::Stroke::new(1.0, palette.border))
                    .corner_radius(egui::CornerRadius::same(Spacing::RADIUS_M))
                    .inner_margin(Margin::symmetric(Spacing::L as i8, Spacing::M as i8))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(40),
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = Spacing::M;
                            icons::paint(ui, icons::WARNING, palette.warning);
                            ui.label(
                                RichText::new(if notice.is_panic() {
                                    "sqlr crashed the last time it ran."
                                } else {
                                    "The previous session did not close cleanly."
                                })
                                .strong(),
                            );
                            if ui
                                .button("Open report")
                                .on_hover_text("Show the session summary and the recent incidents")
                                .clicked()
                            {
                                open_report = true;
                            }
                            if ui
                                .button("Report on GitHub")
                                .on_hover_text(
                                    "Review the full report, then open a prefilled issue",
                                )
                                .clicked()
                            {
                                report_issue = true;
                            }
                            if ui
                                .button("Copy")
                                .on_hover_text("Copy the incident report")
                                .clicked()
                            {
                                copy = true;
                            }
                            if ui.button("Dismiss").clicked() {
                                dismiss = true;
                            }
                        });
                    });
            });
        if open_report {
            self.diagnostics_ui.open = true;
            self.diagnostics_ui.loaded = false;
            self.load_selected(&notice.incident().id);
        }
        if copy {
            ctx.copy_text(notice.json().to_string());
            self.notify("Crash report copied");
        }
        if report_issue {
            let incident = serde_json::from_str(notice.json()).ok();
            self.start_bundle(BundleAction::Issue, ctx, true, true, incident);
        }
        if dismiss {
            let id = notice.incident().id.clone();
            self.store_work(move |diagnostics| {
                diagnostics.acknowledge(&id);
                StoreResult::Acknowledged
            });
            self.diagnostics_ui.notice = None;
            self.refresh_incidents();
        }
    }

    fn diagnostics_window_ui(&mut self, ctx: &egui::Context) {
        let mut open = self.diagnostics_ui.open;
        let mut close = false;
        let mut copy_bundle = false;
        let mut save_bundle = false;
        let mut issue_bundle = false;
        let mut copy_incident = false;
        let mut save_incident = false;
        let mut issue_incident = false;
        let mut open_logs = false;
        let mut new_report = false;
        let mut level_change: Option<LogLevel> = None;
        let mut sql_change: Option<bool> = None;

        egui::Window::new("Diagnostics")
            .open(&mut open)
            .title_bar(false)
            .collapsible(false)
            .resizable(true)
            .default_size(vec2(820.0, 620.0))
            .min_width(620.0)
            .frame(Frame::window(&ctx.global_style()).inner_margin(Margin::ZERO))
            .show(ctx, |ui| {
                if theme::dialog_header(
                    ui,
                    icons::WARNING,
                    "Diagnostics",
                    Some("Local reports and logs; nothing leaves this machine unless you share it"),
                ) {
                    close = true;
                }
                Frame::new()
                    .inner_margin(Margin::symmetric(Spacing::L as i8, Spacing::M as i8))
                    .show(ui, |ui| {
                        ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .max_height(ctx.content_rect().height() - 160.0)
                            .show(ui, |ui| {
                                self.diagnostics_body(
                                    ui,
                                    &mut copy_bundle,
                                    &mut save_bundle,
                                    &mut issue_bundle,
                                    &mut copy_incident,
                                    &mut save_incident,
                                    &mut issue_incident,
                                    &mut open_logs,
                                    &mut new_report,
                                    &mut level_change,
                                    &mut sql_change,
                                )
                            });
                    });
            });
        self.diagnostics_ui.open = open && !close;

        if let Some(level) = level_change {
            let result = self
                .diagnostics
                .as_ref()
                .map(|diagnostics| diagnostics.set_log_level(level))
                .unwrap_or_else(|| Err("logging is not initialized".into()));
            self.diagnostics_ui.status = Some(match result {
                Ok(()) => format!("Log level set to {} for this session", level.as_str()),
                Err(error) => error,
            });
        }
        if let Some(include) = sql_change {
            self.set_include_sql(include, ctx);
        }
        if open_logs {
            self.open_folder(&self.data_dir.join("logs"));
        }
        if new_report {
            let include = self.settings.include_sql_in_diagnostics;
            self.diagnostics_ui.report = Some(ReportProblemDialog::new(include));
        }
        if copy_bundle {
            self.start_bundle(BundleAction::Copy, ctx, true, true, None);
        }
        if save_bundle {
            self.start_bundle(BundleAction::Save, ctx, true, true, None);
        }
        if issue_bundle {
            self.start_bundle(BundleAction::Issue, ctx, true, true, None);
        }
        if copy_incident && let Some(id) = self.diagnostics_ui.selected.clone() {
            self.store_work(move |diagnostics| {
                StoreResult::Copied(diagnostics.read_incident_json(&id))
            });
        }
        if save_incident {
            self.start_bundle(
                BundleAction::Save,
                ctx,
                true,
                true,
                self.diagnostics_ui.selected_incident.clone(),
            );
        }
        if issue_incident {
            self.start_bundle(
                BundleAction::Issue,
                ctx,
                true,
                true,
                self.diagnostics_ui.selected_incident.clone(),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn diagnostics_body(
        &mut self,
        ui: &mut egui::Ui,
        copy_bundle: &mut bool,
        save_bundle: &mut bool,
        issue_bundle: &mut bool,
        copy_incident: &mut bool,
        save_incident: &mut bool,
        issue_incident: &mut bool,
        open_logs: &mut bool,
        new_report: &mut bool,
        level_change: &mut Option<LogLevel>,
        sql_change: &mut Option<bool>,
    ) {
        let palette = theme::palette(ui.ctx());
        ui.label(RichText::new("This session").strong());
        ui.add_space(Spacing::XS);
        if let Some(diagnostics) = &self.diagnostics {
            let session = diagnostics.session();
            let environment = diagnostics.environment();
            egui::Grid::new("diagnostics_session")
                .num_columns(2)
                .spacing([Spacing::L, Spacing::XS])
                .show(ui, |ui| {
                    ui.label("Version");
                    ui.label(format!(
                        "{} · {} · {}",
                        session.version,
                        environment.profile.as_deref().unwrap_or("unknown build"),
                        environment.target.as_deref().unwrap_or("unknown target")
                    ));
                    ui.end_row();
                    ui.label("Session");
                    ui.label(RichText::new(&session.id).monospace());
                    ui.end_row();
                    ui.label("Uptime");
                    ui.label(format_duration(session.uptime_s));
                    ui.end_row();
                    ui.label("System");
                    ui.label(format!(
                        "{} {} · {}",
                        environment.os,
                        environment.os_version.as_deref().unwrap_or(""),
                        environment.arch
                    ));
                    ui.end_row();
                    if let Some(dir) = &session.data_dir {
                        ui.label("Data directory");
                        ui.label(RichText::new(dir).monospace());
                        ui.end_row();
                    }
                    ui.label("Drivers");
                    ui.label(self.engine.manifests().len().to_string());
                    ui.end_row();
                    ui.label("Connections");
                    ui.label(self.connected_connection_count().to_string());
                    ui.end_row();
                    if session.other_instance {
                        ui.label("Note");
                        ui.label("Another sqlr instance uses this data directory.");
                        ui.end_row();
                    }
                });
        } else {
            ui.colored_label(
                palette.warning,
                "Diagnostics are not initialized in this session; only stderr is available.",
            );
        }
        ui.add_space(Spacing::M);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::S;
            if ui.button("Copy diagnostics").clicked() {
                *copy_bundle = true;
            }
            if ui.button("Save bundle…").clicked() {
                *save_bundle = true;
            }
            if ui
                .button("Open GitHub issue…")
                .on_hover_text("Review the full report, then open a prefilled issue")
                .clicked()
            {
                *issue_bundle = true;
            }
            if ui.button("Report a Problem…").clicked() {
                *new_report = true;
            }
            if let Some(status) = &self.diagnostics_ui.status {
                theme::hint(ui, status);
            }
        });
        if let Some(outline) = &self.diagnostics_ui.outline {
            theme::hint(
                ui,
                &format!(
                    "Last bundle: {} KiB · {} tabs · {} operations · {} log lines · {} incidents{}",
                    outline.bytes.div_ceil(1024),
                    outline.tabs,
                    outline.operations,
                    outline.log_lines,
                    outline.incidents,
                    if outline.truncated.is_empty() {
                        String::new()
                    } else {
                        format!(" · truncated: {}", outline.truncated.join(", "))
                    }
                ),
            );
        }
        if self.diagnostics_ui.preview.is_some() {
            ui.collapsing("Bundle preview", |ui| {
                if let Some(preview) = &self.diagnostics_ui.preview {
                    let mut preview = preview.clone();
                    ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut preview)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .interactive(false),
                        );
                    });
                }
            });
        }

        ui.add_space(Spacing::M);
        theme::hairline(ui);
        ui.label(RichText::new("Incidents").strong());
        ui.add_space(Spacing::XS);
        if self.diagnostics_ui.incidents.is_empty() {
            theme::hint(
                ui,
                "No incidents recorded in this session's data directory.",
            );
        } else {
            let incidents = self.diagnostics_ui.incidents.clone();
            ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                for incident in &incidents {
                    let selected = self.diagnostics_ui.selected.as_deref() == Some(&incident.id);
                    let label = format!(
                        "{}  {:<16}  {}{}",
                        format_when(incident.at_ms),
                        incident.kind.as_str(),
                        incident.message.lines().next().unwrap_or(""),
                        if incident.acknowledged {
                            "  (dismissed)"
                        } else {
                            ""
                        }
                    );
                    if ui.selectable_label(selected, label).clicked() {
                        self.load_selected(&incident.id);
                    }
                }
            });
        }

        if let Some(incident) = self.diagnostics_ui.selected_incident.clone() {
            ui.add_space(Spacing::M);
            theme::hairline(ui);
            ui.label(RichText::new("Selected incident").strong());
            egui::Grid::new("diagnostics_incident")
                .num_columns(2)
                .spacing([Spacing::L, Spacing::XS])
                .show(ui, |ui| {
                    ui.label("Kind");
                    ui.label(incident.kind.as_str());
                    ui.end_row();
                    ui.label("When");
                    ui.label(format_when(incident.at_ms));
                    ui.end_row();
                    ui.label("Message");
                    ui.label(&incident.message);
                    ui.end_row();
                    if let Some(thread) = &incident.thread {
                        ui.label("Thread");
                        ui.label(thread);
                        ui.end_row();
                    }
                    if let Some(file) = &incident.file {
                        ui.label("Location");
                        ui.label(format!(
                            "{file}{}",
                            incident
                                .line
                                .map(|line| format!(":{line}"))
                                .unwrap_or_default()
                        ));
                        ui.end_row();
                    }
                    if let Some(note) = &incident.note {
                        ui.label("Note");
                        ui.label(note);
                        ui.end_row();
                    }
                });
            if !incident.fields.is_empty() {
                ui.collapsing("Event fields", |ui| {
                    egui::Grid::new("diagnostics_fields")
                        .num_columns(2)
                        .spacing([Spacing::L, Spacing::XS])
                        .show(ui, |ui| {
                            for (key, value) in &incident.fields {
                                ui.label(RichText::new(key).monospace());
                                ui.label(RichText::new(value.to_string()).monospace());
                                ui.end_row();
                            }
                        });
                });
            }
            ui.collapsing("Environment", |ui| {
                ui.label(format!(
                    "{} {} · {}",
                    incident.environment.version,
                    incident.environment.os,
                    incident.environment.arch
                ));
            });
            ui.collapsing(
                format!("Log excerpt ({} lines)", incident.logs.len()),
                |ui| {
                    ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for line in &incident.logs {
                            ui.label(RichText::new(line).monospace().small());
                        }
                    });
                },
            );
            ui.horizontal(|ui| {
                if ui.button("Copy").clicked() {
                    *copy_incident = true;
                }
                if ui.button("Save…").clicked() {
                    *save_incident = true;
                }
                if ui
                    .button("Open GitHub issue…")
                    .on_hover_text("Review the full report, then open a prefilled issue")
                    .clicked()
                {
                    *issue_incident = true;
                }
            });
        }

        ui.add_space(Spacing::M);
        theme::hairline(ui);
        ui.label(RichText::new("Logs").strong());
        ui.add_space(Spacing::XS);
        let env_controlled = self
            .diagnostics
            .as_ref()
            .is_some_and(|diagnostics| diagnostics.log_level_env_controlled());
        let current = self
            .diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.log_level())
            .unwrap_or(LogLevel::Info);
        let mut chosen = current;
        ui.horizontal(|ui| {
            ui.label("Level for this session");
            ui.add_enabled_ui(!env_controlled, |ui| {
                egui::ComboBox::from_id_salt("diagnostics_log_level")
                    .selected_text(current.label())
                    .show_ui(ui, |ui| {
                        for level in LogLevel::ALL {
                            ui.selectable_value(&mut chosen, level, level.label());
                        }
                    });
            });
            if env_controlled {
                theme::hint(ui, "RUST_LOG controls the level for this launch");
            } else if chosen != current {
                *level_change = Some(chosen);
            }
            if ui.button("Open logs folder").clicked() {
                *open_logs = true;
            }
        });
        theme::hint(
            ui,
            "Log files rotate daily; the newest fourteen are kept. The level resets on the next launch.",
        );

        ui.add_space(Spacing::M);
        theme::hairline(ui);
        ui.label(RichText::new("Privacy").strong());
        ui.add_space(Spacing::XS);
        let mut include = self.settings.include_sql_in_diagnostics;
        if ui
            .checkbox(&mut include, "Include SQL in logs and reports")
            .changed()
        {
            *sql_change = Some(include);
        }
        theme::hint(
            ui,
            "SQL is recorded as typed, without literal redaction. Secrets are never recorded. Review a bundle before sharing it.",
        );
    }

    fn report_problem_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.diagnostics_ui.report.take() else {
            return;
        };
        let mut open = true;
        let mut close = false;
        let mut create_copy = false;
        let mut create_save = false;
        let mut create_issue = false;
        egui::Window::new("Report a Problem")
            .open(&mut open)
            .title_bar(false)
            .collapsible(false)
            .resizable(true)
            .default_size(vec2(620.0, 460.0))
            .frame(Frame::window(&ctx.global_style()).inner_margin(Margin::ZERO))
            .show(ctx, |ui| {
                if theme::dialog_header(
                    ui,
                    icons::WARNING,
                    "Report a Problem",
                    Some("Saved locally and listed with the other incidents"),
                ) {
                    close = true;
                }
                Frame::new()
                    .inner_margin(Margin::symmetric(Spacing::L as i8, Spacing::M as i8))
                    .show(ui, |ui| {
                        ui.label("What were you doing when the problem happened?");
                        ui.add(
                            egui::TextEdit::multiline(&mut dialog.note)
                                .desired_rows(6)
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(Spacing::S);
                        ui.add_enabled_ui(self.settings.include_sql_in_diagnostics, |ui| {
                            ui.checkbox(&mut dialog.include_sql, "Include SQL statements");
                        });
                        ui.checkbox(&mut dialog.include_titles, "Include open tab titles");
                        ui.add_space(Spacing::S);
                        theme::hint(
                            ui,
                            "Passwords, passphrases, tokens and vault values are never included. \
                             A GitHub issue opens only after you review the full report.",
                        );
                        ui.add_space(Spacing::M);
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add(theme::primary_button(ui.ctx(), "Create report and copy"))
                                .clicked()
                            {
                                create_copy = true;
                            }
                            if ui.button("Create report and save…").clicked() {
                                create_save = true;
                            }
                            if ui.button("Create report for GitHub…").clicked() {
                                create_issue = true;
                            }
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                        });
                    });
            });
        if open && !close && !create_copy && !create_save && !create_issue {
            self.diagnostics_ui.report = Some(dialog);
            return;
        }
        if !(create_copy || create_save || create_issue) {
            return;
        }
        let include_sql = dialog.include_sql;
        let include_titles = dialog.include_titles;
        let note = dialog.note.trim().to_string();
        let action = if create_copy {
            BundleAction::Copy
        } else if create_save {
            BundleAction::Save
        } else {
            BundleAction::Issue
        };
        self.store_work(move |diagnostics| {
            StoreResult::Manual(
                diagnostics
                    .record_manual(note, include_sql, include_titles)
                    .map(Box::new),
                include_sql,
                include_titles,
                action,
            )
        });
    }

    /// Builds the bundle on a worker thread; the interface thread only assembles snapshots.
    fn start_bundle(
        &mut self,
        action: BundleAction,
        _ctx: &egui::Context,
        include_sql: bool,
        include_titles: bool,
        incident: Option<Incident>,
    ) {
        let Some(diagnostics) = self.diagnostics.clone() else {
            self.notify("Diagnostics are not initialized in this session");
            return;
        };
        let incident = incident.or_else(|| self.diagnostics_ui.selected_incident.clone());
        let inputs = self.bundle_inputs(include_sql, include_titles, incident);
        let (tx, receiver) = std::sync::mpsc::channel();
        let repaint = _ctx.clone();
        let worker = diagnostics.clone();
        if !diagnostics.background(move || {
            let (text, outline) = worker.bundle(&inputs);
            let _ = tx.send(PendingBundleResult::Built(text, outline));
            repaint.request_repaint();
        }) {
            self.diagnostics_ui.status = Some("Diagnostics writer is unavailable".into());
            return;
        }
        self.diagnostics_ui.pending = Some(PendingBundle { receiver, action });
        self.diagnostics_ui.status = Some("Building the diagnostics bundle…".into());
    }

    fn poll_bundle(&mut self, _ctx: &egui::Context) {
        let Some(pending) = self.diagnostics_ui.pending.take() else {
            return;
        };
        let received = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.diagnostics_ui.pending = Some(pending);
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                PendingBundleResult::Failed("the bundle worker stopped".into())
            }
        };
        match received {
            PendingBundleResult::Built(text, outline) => {
                self.diagnostics_ui.outline = Some(outline);
                self.diagnostics_ui.issue = Some(crate::diagnostics::issue_draft(&text));
                self.diagnostics_ui.preview = Some(text);
                self.diagnostics_ui.preview_open = true;
                self.diagnostics_ui.status =
                    Some("Review the bundle before copying, saving or sharing it".into());
            }

            PendingBundleResult::Saved(Ok(())) => {
                self.diagnostics_ui.status = Some("Diagnostics bundle saved".into());
            }
            PendingBundleResult::Saved(Err(error)) => {
                self.diagnostics_ui.status = Some(format!("Could not save the bundle: {error}"));
            }
            PendingBundleResult::Failed(error) => {
                self.diagnostics_ui.status = Some(error);
            }
        }
    }

    /// Opens the prefilled issue. The app sends nothing: the browser shows the form and the
    /// person decides. A bundle too large for the URL is copied for one paste.
    fn open_issue(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.diagnostics_ui.issue.clone() else {
            self.diagnostics_ui.status = Some("Build the bundle before opening an issue".into());
            return;
        };
        if draft.paste_required
            && let Some(text) = &self.diagnostics_ui.preview
        {
            ctx.copy_text(text.clone());
        }
        ctx.open_url(egui::OpenUrl::new_tab(draft.url));
        self.diagnostics_ui.status = Some(if draft.paste_required {
            "GitHub issue opened; paste the full report that sqlr put on the clipboard".into()
        } else {
            "GitHub issue opened in the browser with the full report".into()
        });
        self.diagnostics_ui.preview_open = false;
    }

    fn choose_bundle_path(&mut self) -> Option<PathBuf> {
        if let Some(path) = self.file_answer.take() {
            return Some(path);
        }
        rfd::FileDialog::new()
            .set_title("Save diagnostics bundle")
            .set_file_name(format!(
                "sqlr-diagnostics-{}.json",
                jiff::Timestamp::now().strftime("%Y%m%d-%H%M%S")
            ))
            .save_file()
    }

    fn bundle_preview_ui(&mut self, ctx: &egui::Context) {
        if !self.diagnostics_ui.preview_open {
            return;
        }
        let mut open = true;
        let mut copy = false;
        let mut save = false;
        let mut issue = false;
        egui::Window::new("Review diagnostics bundle")
            .open(&mut open)
            .default_size(vec2(760.0, 560.0))
            .show(ctx, |ui| {
                ui.label(
                    "Review this local report before sharing it. Nothing is sent automatically.",
                );
                ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                    if let Some(text) = &self.diagnostics_ui.preview {
                        ui.add(
                            egui::TextEdit::multiline(&mut text.as_str())
                                .code_editor()
                                .desired_width(f32::INFINITY),
                        );
                    }
                });
                if let Some(draft) = &self.diagnostics_ui.issue {
                    theme::hint(
                        ui,
                        if draft.paste_required {
                            "The report is too large for a prefilled issue: sqlr copies it to the \
                             clipboard, and the issue asks you to paste it at the marker."
                        } else {
                            "The whole report fits in the issue body. Nothing is sent until you \
                             submit the issue on GitHub."
                        },
                    );
                }
                ui.horizontal(|ui| {
                    if ui
                        .add(theme::primary_button(
                            ctx,
                            "Open GitHub issue with this report",
                        ))
                        .on_hover_text("Open a prefilled public issue in your browser")
                        .clicked()
                    {
                        issue = true;
                    }
                    if ui.button("Copy reviewed bundle").clicked() {
                        copy = true;
                    }
                    if ui.button("Save reviewed bundle…").clicked() {
                        save = true;
                    }
                });
            });
        self.diagnostics_ui.preview_open = open;
        if issue {
            self.open_issue(ctx);
        }
        if copy && let Some(text) = &self.diagnostics_ui.preview {
            ctx.copy_text(text.clone());
            self.diagnostics_ui.status = Some("Diagnostics copied".into());
            self.diagnostics_ui.preview_open = false;
        }
        if save
            && let Some(path) = self.choose_bundle_path()
            && let Some(text) = self.diagnostics_ui.preview.clone()
        {
            self.store_work(move |diagnostics| {
                StoreResult::Saved(
                    diagnostics
                        .save_bundle(&path, &text)
                        .map_err(|e| e.to_string()),
                )
            });
            self.diagnostics_ui.preview_open = false;
        }
    }

    /// Cheap copies of the live application, with no secrets.
    fn bundle_inputs(
        &self,
        include_sql: bool,
        include_titles: bool,
        incident: Option<Incident>,
    ) -> BundleInputs {
        let environment = self
            .diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.environment())
            .unwrap_or_else(Environment::basic);
        let installed = self.engine.installed_drivers();
        let sidecars = self.engine.sidecar_states();
        let drivers = self
            .engine
            .manifests()
            .into_iter()
            .map(|manifest| {
                let sidecar = sidecars.iter().find(|state| state.id == manifest.id);
                DriverInfo {
                    id: manifest.id.clone(),
                    name: manifest.name.clone(),
                    version: sidecar
                        .map(|state| state.version.clone())
                        .or_else(|| {
                            installed
                                .iter()
                                .find(|driver| driver.id == manifest.id)
                                .map(|driver| driver.active.clone())
                        })
                        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
                    source: if installed.iter().any(|driver| driver.id == manifest.id) {
                        "installed".into()
                    } else {
                        "bundled".into()
                    },
                    sidecar_running: sidecar.map(|state| state.running),
                    sidecar_restarts: sidecar.map(|state| state.restarts),
                }
            })
            .collect();

        let mut settings: Vec<(String, String, serde_json::Value)> = sqlr_core::setting_defs()
            .iter()
            .map(|def| {
                (
                    "resolved".to_string(),
                    def.id.to_string(),
                    self.setting_for(def.id, None).to_json(),
                )
            })
            .collect();
        settings.extend(
            self.layered
                .stored_values()
                .into_iter()
                .map(|(level, id, value)| (level, id, value.to_json())),
        );

        let connections = self
            .connections
            .iter()
            .map(|connection| {
                let status = match self.engine.session_status(&connection.id) {
                    Some(SessionStatus::Connected(_)) => "connected",
                    Some(SessionStatus::Connecting) => "connecting",
                    Some(SessionStatus::Failed(_)) => "failed",
                    Some(SessionStatus::Disconnected) => "disconnected",
                    None => "not connected",
                };
                let transaction = self
                    .engine
                    .transaction(&SessionKey::meta(&connection.id))
                    .filter(|tx| tx.phase != TxPhase::Idle)
                    .map(|tx| {
                        format!(
                            "{:?} · {}{}",
                            tx.phase,
                            if tx.read_only {
                                "read-only"
                            } else {
                                "read-write"
                            },
                            tx.since_ms
                                .map(|since| format!(" · since {since}"))
                                .unwrap_or_default()
                        )
                    });
                let value = |key: &str| connection.values.get(key).cloned();
                ConnectionInfo {
                    id: connection.id.clone(),
                    name: connection.name.clone(),
                    driver: connection.driver.clone(),
                    host: value("host"),
                    port: value("port").and_then(|p| p.parse().ok()),
                    database: value("database"),
                    user: value("user").or_else(|| value("username")),
                    tls: value("tls_mode")
                        .or_else(|| value("sslmode"))
                        .or_else(|| value("tls")),
                    profile: connection.profile.clone(),
                    status: Some(status.to_string()),
                    transaction,
                }
            })
            .collect();

        let open_tabs = self
            .dock
            .iter_all_tabs()
            .map(|(_, tab)| TabInfo {
                title: if include_titles {
                    tab.title.clone()
                } else {
                    "<title omitted>".into()
                },
                connection: tab.conn.clone(),
                database: tab.database.clone(),
                kind: if tab.redis.is_some() {
                    "redis"
                } else if tab.object.is_some() {
                    "object"
                } else if tab.admin.is_some() {
                    "administration"
                } else if tab.ddl.is_some() {
                    "ddl"
                } else if tab.doc.is_some() {
                    "document"
                } else {
                    "query"
                }
                .into(),
            })
            .collect();
        let open_transactions = self
            .engine
            .open_transactions()
            .into_iter()
            .map(|(key, tx)| {
                format!(
                    "{} · {:?}{}",
                    key.conn,
                    tx.phase,
                    if tx.read_only { " · read-only" } else { "" }
                )
            })
            .collect();
        let mut panels = Vec::new();
        if self.show_value_panel {
            panels.push("value".to_string());
        }
        if self.show_history {
            panels.push("history".to_string());
        }
        if self.show_operations {
            panels.push("operations".to_string());
        }
        if self.terminal.is_open() {
            panels.push("terminal".to_string());
        }

        let operations = self
            .engine
            .operations()
            .into_iter()
            .map(|operation| {
                let (outcome, error) = match &operation.state {
                    OperationState::Running => ("running", None),
                    OperationState::Cancelling => ("cancelling", None),
                    OperationState::Done { ok } => {
                        (if *ok { "done" } else { "done with errors" }, None)
                    }
                    OperationState::Cancelled => ("cancelled", None),
                    OperationState::Failed(error) => ("failed", Some(error.clone())),
                };
                OperationSnapshot {
                    kind: format!("{:?}", operation.kind),
                    connection: operation.conn,
                    label: operation.label,
                    duration_ms: operation.elapsed.as_millis() as u64,
                    outcome: outcome.into(),
                    error,
                }
            })
            .collect();

        BundleInputs {
            application: ApplicationInfo {
                version: env!("CARGO_PKG_VERSION").into(),
                profile: environment.profile.clone().unwrap_or_default(),
                target: environment.target.clone().unwrap_or_default(),
                commit: environment.commit.clone(),
                graphics_backend: environment.graphics_backend.clone(),
            },
            drivers,
            settings,
            connections,
            interface: InterfaceInfo {
                open_tabs,
                titles_included: include_titles,
                open_transactions,
                panels,
            },
            operations,
            incident,
            include_sql: include_sql && self.settings.include_sql_in_diagnostics,
            include_titles,
            redact: Vec::new(),
        }
    }
}

fn format_when(at_ms: i64) -> String {
    sqlr_ui::format_when(at_ms)
}

fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}
