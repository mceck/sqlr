//! The "Sessions and locks" tab. While it is drawn (shown and not paused), it asks the
//! engine for a sessions and a locks snapshot every interval: the timer is the tab's frame
//! clock, the reads are engine jobs. A hidden or closed tab is not drawn, so it stops.

use egui::{Id, RichText};
use sqlr_core::{AdminSupport, LockInfo, SessionInfo};
use sqlr_engine::{AdminQuery, AdminSnapshot, Engine, JobId, JobOutcome};
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{SessionsAction, SessionsState, locks_view, sessions_table};

use super::{AdminCommand, take};

pub struct SessionsTab {
    pub conn: String,
    support: AdminSupport,
    pub state: SessionsState,
    sessions: Vec<SessionInfo>,
    locks: Vec<LockInfo>,
    sessions_job: Option<JobId>,
    locks_job: Option<JobId>,
    /// A cancel or terminate the tab follows.
    pub action: Option<JobId>,
    /// When the last refresh started, on the frame clock.
    last: Option<f64>,
    /// `(text, is_error)`.
    message: Option<(String, bool)>,
}

impl SessionsTab {
    pub fn new(engine: &Engine, conn: &str) -> Self {
        Self {
            conn: conn.to_string(),
            support: engine.admin_support(conn).unwrap_or_default(),
            state: SessionsState::default(),
            sessions: Vec::new(),
            locks: Vec::new(),
            sessions_job: None,
            locks_job: None,
            action: None,
            last: None,
            message: None,
        }
    }

    pub fn jobs(&self) -> Vec<JobId> {
        [self.sessions_job, self.locks_job, self.action]
            .into_iter()
            .flatten()
            .collect()
    }

    /// The selected session's query, for the value panel.
    pub fn selected_query(&self) -> Option<&str> {
        let id = self.state.selected.as_ref()?;
        self.sessions.iter().find(|s| s.id == *id)?.query.as_deref()
    }

    /// When the session `id` started, as last listed.
    pub fn started_ms(&self, id: &str) -> Option<i64> {
        self.sessions.iter().find(|s| s.id == id)?.started_ms
    }

    /// Whether the session `id` is one of the app's own.
    pub fn is_own(&self, id: &str) -> bool {
        self.sessions.iter().any(|s| s.id == id && s.own)
    }

    fn refresh(&mut self, engine: &Engine, now: f64) {
        self.last = Some(now);
        self.read(engine);
    }

    /// Asks for a new snapshot now (Refresh), unless one is being read.
    pub fn reload(&mut self, engine: &Engine) {
        if self.sessions_job.is_none() && self.locks_job.is_none() {
            self.read(engine);
        }
    }

    fn read(&mut self, engine: &Engine) {
        if self.support.sessions && self.sessions_job.is_none() {
            self.sessions_job = Some(engine.admin_snapshot(&self.conn, AdminQuery::Sessions));
        }
        if self.support.locks && self.locks_job.is_none() {
            self.locks_job = Some(engine.admin_snapshot(&self.conn, AdminQuery::Locks));
        }
    }

    fn poll(&mut self, engine: &Engine, now: f64) {
        if let Some(outcome) = take(engine, &mut self.sessions_job) {
            match outcome {
                JobOutcome::Admin(AdminSnapshot::Sessions(s)) => self.sessions = s,
                JobOutcome::Failed(e) => self.message = Some((e.message, true)),
                _ => {}
            }
        }
        if let Some(outcome) = take(engine, &mut self.locks_job) {
            match outcome {
                JobOutcome::Admin(AdminSnapshot::Locks(l)) => self.locks = l,
                JobOutcome::Failed(e) => self.message = Some((e.message, true)),
                _ => {}
            }
        }
        if let Some(outcome) = take(engine, &mut self.action) {
            match outcome {
                JobOutcome::Done => self.message = Some(("Done".into(), false)),
                JobOutcome::Failed(e) => self.message = Some((e.message, true)),
                _ => {}
            }
            // What the action changed shows at once.
            self.refresh(engine, now);
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, engine: &Engine, key: u64) -> Option<AdminCommand> {
        let now = ui.input(|i| i.time);
        self.poll(engine, now);
        self.state.actions_blocked =
            super::actions_blocked(engine, &self.conn, false).map(String::from);
        let interval = f64::from(self.state.interval_s.clamp(1, 60));
        let idle = self.sessions_job.is_none() && self.locks_job.is_none();
        let due = self.last.is_none_or(|t| now - t >= interval);
        if !self.state.paused && idle && due {
            self.refresh(engine, now);
        }
        if !self.state.paused {
            let wait = self
                .last
                .map_or(interval, |t| interval - (now - t))
                .max(0.05);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(wait));
        }
        let mut command = None;
        let p = theme::palette(ui.ctx());
        if self.support.locks {
            egui::Panel::bottom(Id::new(("locks", key)))
                .resizable(true)
                .default_size(220.0)
                .min_size(80.0)
                .show(ui, |ui| {
                    ui.add_space(Spacing::S);
                    if let Some(session) = locks_view(ui, &self.locks) {
                        self.state.selected = Some(session);
                    }
                });
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.add_space(Spacing::S);
            if let Some((text, error)) = &self.message {
                let color = if *error { p.error } else { p.success };
                ui.label(RichText::new(text).color(color));
            }
            if !self.support.sessions {
                theme::hint(ui, "This connection does not list sessions");
                return;
            }
            match sessions_table(ui, &mut self.state, &self.sessions) {
                Some(SessionsAction::Refresh) => {
                    if self.sessions_job.is_none() && self.locks_job.is_none() {
                        self.refresh(engine, now);
                    }
                }
                Some(action) => {
                    let id = match &action {
                        SessionsAction::Cancel(id) | SessionsAction::Terminate(id) => id,
                        SessionsAction::Refresh => unreachable!(),
                    };
                    let own = self.is_own(id);
                    command = Some(AdminCommand::Session { action, own });
                }
                None => {}
            }
        });
        command
    }
}
