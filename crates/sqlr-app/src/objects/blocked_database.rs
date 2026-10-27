//! Recovery for an explorer database drop: inspect its server sessions, terminate a
//! confirmed session, and retry the original drop. All server work uses engine Jobs.

use sqlr_core::SessionInfo;
use sqlr_engine::{
    AdminAction, AdminQuery, AdminSnapshot, ApplyOptions, Engine, JobId, JobOutcome, SessionKey,
};
use sqlr_ui::theme::{self, Spacing};

use super::ExplorerJob;
use crate::admin::{actions_blocked, take};
use crate::app::SqlrApp;

pub(super) struct DatabaseDrop {
    pub conn: String,
    pub database: String,
    pub statements: Vec<String>,
    pub options: ApplyOptions,
    pub what: String,
}

impl DatabaseDrop {
    pub fn start(&self, engine: &Engine) -> JobId {
        engine.apply_ddl(
            &SessionKey::meta(&self.conn),
            self.statements.clone(),
            self.options.clone(),
        )
    }
}

struct Terminate {
    session: SessionInfo,
    again: bool,
}

pub(super) struct DatabaseDropDialog {
    request: DatabaseDrop,
    error: String,
    sessions: Option<Vec<SessionInfo>>,
    snapshot: Option<JobId>,
    action: Option<JobId>,
    message: Option<String>,
    ask: Option<Terminate>,
}

impl DatabaseDropDialog {
    fn new(engine: &Engine, request: DatabaseDrop, error: String) -> Self {
        let snapshot = Some(engine.admin_snapshot(&request.conn, AdminQuery::Sessions));
        Self {
            request,
            error,
            sessions: None,
            snapshot,
            action: None,
            message: None,
            ask: None,
        }
    }

    fn refresh(&mut self, engine: &Engine) {
        self.sessions = None;
        self.snapshot = Some(engine.admin_snapshot(&self.request.conn, AdminQuery::Sessions));
    }

    fn poll(&mut self, engine: &Engine) {
        if let Some(outcome) = take(engine, &mut self.snapshot) {
            match outcome {
                JobOutcome::Admin(AdminSnapshot::Sessions(sessions)) => {
                    self.sessions = Some(
                        sessions
                            .into_iter()
                            .filter(|s| s.database.as_deref() == Some(&self.request.database))
                            .collect(),
                    );
                }
                JobOutcome::Failed(e) => self.message = Some(e.message),
                JobOutcome::Cancelled => self.message = Some("Session refresh cancelled".into()),
                _ => {}
            }
        }
        if let Some(outcome) = take(engine, &mut self.action) {
            self.message = match outcome {
                JobOutcome::Done => None,
                JobOutcome::Failed(e) => Some(e.message),
                JobOutcome::Cancelled => Some("Termination cancelled".into()),
                _ => None,
            };
            self.refresh(engine);
        }
    }

    fn close(&self, engine: &Engine) {
        for id in [self.snapshot, self.action].into_iter().flatten() {
            engine.cancel_job(id);
            engine.forget_job(id);
        }
    }

    fn confirm_termination(&mut self, ctx: &egui::Context, engine: &Engine) {
        let Some(ask) = &self.ask else {
            return;
        };
        let id = &ask.session.id;
        let prompt = if ask.again {
            format!("Session {id} belongs to sqlr. Terminate it anyway?")
        } else {
            format!("Terminate session {id}?")
        };
        let blocked = actions_blocked(engine, &self.request.conn, false);
        let response = theme::Dialog::new(
            ("drop_database_terminate", ask.again),
            theme::icons::WARNING,
            "Terminate session",
        )
        .danger()
        .width(560.0)
        .show(ctx, |d| {
            d.body(|ui| {
                ui.label(&prompt);
                ui.label(format!(
                    "User: {} · Database: {}",
                    ask.session.user, self.request.database
                ));
                ui.add_space(Spacing::S);
                ui.label("This interrupts active work and rolls back open transactions.");
            });
            d.footer(|f| {
                let yes = f.danger(
                    if ask.again {
                        "Terminate it anyway"
                    } else {
                        "Terminate"
                    },
                    blocked,
                );
                f.cancel();
                yes
            })
        });
        if !response.inner && !response.closed {
            return;
        }
        let yes = response.inner;
        let Some(mut ask) = self.ask.take() else {
            return;
        };
        if yes {
            if ask.session.own && !ask.again {
                ask.again = true;
                self.ask = Some(ask);
            } else {
                self.action = Some(engine.admin_action(
                    &self.request.conn,
                    AdminAction::TerminateSession {
                        id: ask.session.id,
                        started_ms: ask.session.started_ms,
                    },
                ));
            }
        }
    }
}

impl SqlrApp {
    pub(super) fn database_drop_failed(&mut self, request: DatabaseDrop, error: String) {
        if self
            .engine
            .admin_support(&request.conn)
            .is_some_and(|s| s.sessions)
        {
            if let Some(previous) = self.objects.blocked_database.take() {
                previous.close(&self.engine);
            }
            self.objects.blocked_database =
                Some(DatabaseDropDialog::new(&self.engine, request, error));
        } else {
            self.notify(error);
        }
    }

    pub(super) fn blocked_database_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.objects.blocked_database else {
            return;
        };
        dialog.poll(&self.engine);
        // Keep the recovery dialog state while a confirmation replaces it on screen.
        if dialog.ask.is_some() {
            dialog.confirm_termination(ctx, &self.engine);
            return;
        }
        let busy = dialog.snapshot.is_some() || dialog.action.is_some();
        let blocked = actions_blocked(&self.engine, &dialog.request.conn, false);
        let retry_blocked = actions_blocked(&self.engine, &dialog.request.conn, true).or(if busy {
            Some("Wait for the current operation")
        } else {
            None
        });
        let title = if dialog.sessions.as_ref().is_some_and(|s| !s.is_empty()) {
            "Database in use"
        } else {
            "Could not drop database"
        };
        let mut terminate = None;
        let mut refresh = false;
        let mut retry = false;
        let response = theme::Dialog::new("blocked_database", theme::icons::DATABASE, title)
            .subtitle(&dialog.request.database)
            .width(740.0)
            .show(ctx, |d| {
                d.body(|ui| {
                    ui.colored_label(theme::palette(ui.ctx()).error, &dialog.error);
                    if let Some(message) = &dialog.message {
                        ui.add_space(Spacing::S);
                        ui.colored_label(theme::palette(ui.ctx()).error, message);
                    }
                    ui.add_space(Spacing::M);
                    if dialog.action.is_some() {
                        ui.label("Terminating session…");
                    } else if dialog.snapshot.is_some() {
                        ui.label("Loading server sessions…");
                    } else if let Some(sessions) = &dialog.sessions {
                        if sessions.is_empty() {
                            ui.label("No visible sessions are connected to this database. You can retry the drop.");
                        } else {
                            ui.label("These server sessions are connected to the database:");
                            ui.add_space(Spacing::S);
                            egui::Grid::new("database_blockers")
                                .striped(true)
                                .num_columns(5)
                                .min_col_width(60.0)
                                .spacing(egui::vec2(Spacing::M, Spacing::S))
                                .show(ui, |ui| {
                                    for label in ["PID / ID", "User", "Client", "State", ""] {
                                        ui.strong(label);
                                    }
                                    ui.end_row();
                                    for session in sessions {
                                        ui.label(&session.id);
                                        ui.label(&session.user);
                                        ui.label(session.client.as_deref().unwrap_or("Unknown"));
                                        ui.label(&session.state);
                                        ui.vertical(|ui| {
                                            ui.set_min_width(160.0);
                                            if session.own {
                                                ui.label("sqlr session");
                                            }
                                            let button = ui.add_enabled(
                                                blocked.is_none(),
                                                egui::Button::new(format!("Terminate session {}", session.id))
                                                    .wrap_mode(egui::TextWrapMode::Extend),
                                            );
                                            let button = if let Some(reason) = blocked {
                                                button.on_disabled_hover_text(reason)
                                            } else {
                                                button
                                            };
                                            if button.clicked() {
                                                terminate = Some(session.clone());
                                            }
                                        });
                                        ui.end_row();
                                    }
                                });
                        }
                    }
                });
                d.footer(|f| {
                    refresh = f.secondary("Refresh", !busy);
                    retry = f.danger("Retry drop", retry_blocked);
                    f.cancel_as("Close");
                });
            });
        if let Some(session) = terminate {
            dialog.ask = Some(Terminate {
                session,
                again: false,
            });
        }
        if refresh {
            dialog.message = None;
            dialog.refresh(&self.engine);
        }
        if (response.closed || retry)
            && let Some(dialog) = self.objects.blocked_database.take()
        {
            dialog.close(&self.engine);
            if retry {
                let id = dialog.request.start(&self.engine);
                self.objects
                    .jobs
                    .push((id, ExplorerJob::DropDatabase(dialog.request)));
            }
        }
    }
}
