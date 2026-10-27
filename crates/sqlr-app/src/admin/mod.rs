//! Administration in the app: the "Users and roles", "Sessions and
//! locks" and "Server properties" tabs of a connection, opened from the explorer's
//! connection menu and ⌘⇧P, and the confirmations their actions need.
//!
//! - Role and grant changes are DDL: they go through the profile's confirmation (a revoke
//!   is destructive) and [`sqlr_engine::Engine::apply_ddl`], with passwords masked.
//! - Dropping a role always asks, twice when it is the connected user.
//! - Cancelling a query or terminating a session always asks, twice for one of the app's
//!   own sessions ("this connection").

mod server_tab;
mod sessions_tab;
mod users_tab;

pub use server_tab::ServerTab;
pub use sessions_tab::SessionsTab;
pub use users_tab::{UsersDiscard, UsersTab};

use egui::Id;
use sqlr_core::{ColumnKind, ColumnMeta, Dialect, DisplayHint, RoleDef, SqlSyntax, script};
use sqlr_engine::{AdminAction, ApplyOptions, Engine, JobId, JobOutcome};
use sqlr_ui::ToastKind;
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{AdminPanel, SessionsAction, sql_view};

use crate::app::{SqlrApp, tab_mut};
use crate::objects::AfterApply;

/// An administration tab's content. It always lives boxed in its dock tab, so the
/// variants' sizes do not matter.
#[expect(
    clippy::large_enum_variant,
    reason = "one tab is alive per administration pane and is accessed on every frame; boxing the larger variants would add an indirection to each access to save a few hundred bytes of one value"
)]
pub enum AdminTab {
    Users(UsersTab),
    Sessions(SessionsTab),
    Server(ServerTab),
}

/// What an administration tab asks the app for.
#[derive(Clone, Debug)]
pub enum AdminCommand {
    /// Apply the users tab's pending changes.
    ApplyRole,
    /// Drop this role (asked first).
    DropRole(RoleDef),
    /// Cancel or terminate a session (asked first); `own` when it is the app's.
    Session { action: SessionsAction, own: bool },
    /// Do this in the users tab once the user agrees to drop its pending edits.
    Discard(UsersDiscard),
}

/// The job's outcome once it ended; the job is then forgotten and `slot` cleared.
pub(crate) fn take(engine: &Engine, slot: &mut Option<JobId>) -> Option<JobOutcome> {
    let id = (*slot)?;
    let outcome = engine.job(id)?.outcome?;
    engine.forget_job(id);
    *slot = None;
    Some(outcome)
}

impl AdminTab {
    pub fn conn(&self) -> &str {
        match self {
            AdminTab::Users(t) => &t.conn,
            AdminTab::Sessions(t) => &t.conn,
            AdminTab::Server(t) => &t.conn,
        }
    }

    pub fn panel(&self) -> AdminPanel {
        match self {
            AdminTab::Users(_) => AdminPanel::Users,
            AdminTab::Sessions(_) => AdminPanel::Sessions,
            AdminTab::Server(_) => AdminPanel::Server,
        }
    }

    /// Stops the tab's running reads (a long statistics count) and forgets its jobs.
    pub fn close(&mut self, engine: &Engine) {
        let jobs = match self {
            AdminTab::Users(t) => t.jobs(),
            AdminTab::Sessions(t) => t.jobs(),
            AdminTab::Server(t) => t.jobs(),
        };
        for id in jobs {
            engine.cancel_job(id);
            engine.forget_job(id);
        }
    }

    /// Edits that closing the tab would drop.
    pub fn pending(&self) -> usize {
        match self {
            AdminTab::Users(t) => t.pending(),
            _ => 0,
        }
    }

    /// The selected session's query, for the value panel.
    pub fn selected_query(&self) -> Option<&str> {
        match self {
            AdminTab::Sessions(t) => t.selected_query(),
            _ => None,
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        engine: &Engine,
        dialect: Option<&dyn Dialect>,
        key: u64,
    ) -> Option<AdminCommand> {
        match self {
            AdminTab::Users(t) => t.show(ui, engine, dialect, key),
            AdminTab::Sessions(t) => t.show(ui, engine, key),
            AdminTab::Server(t) => {
                t.show(ui, engine, dialect);
                None
            }
        }
    }
}

/// Why administration actions on `conn` are off, as the object tab's Apply says: a
/// read-only explorer session, no connection, or (for DDL, `ddl`) a transaction an apply
/// refuses: any open on the explorer session, which applies run on, or one that wrote on
/// another session of the connection.
pub(crate) fn actions_blocked(engine: &Engine, conn: &str, ddl: bool) -> Option<&'static str> {
    let meta = sqlr_engine::SessionKey::meta(conn);
    match engine.transaction(&meta) {
        None => Some("Not connected"),
        Some(t) if t.read_only => Some("The session is read-only"),
        _ if ddl
            && engine
                .open_transactions()
                .iter()
                .any(|(k, t)| k.conn == conn && (*k == meta || t.blocks_ddl())) =>
        {
            Some("A transaction is open on this connection: commit or roll back first")
        }
        _ => None,
    }
}

/// The query column the value panel shows a session's query under.
pub(crate) fn query_meta() -> ColumnMeta {
    ColumnMeta::new("query", "text", ColumnKind::Text, DisplayHint::Plain)
}

/// A confirmation an administration action waits for.
struct Ask {
    key: u64,
    conn: String,
    what: Asked,
    /// The second question (the connected user, the app's own session) is shown.
    again: bool,
}

enum Asked {
    Drop {
        role: RoleDef,
        statements: Vec<String>,
        /// The connection logged in as this role.
        current: bool,
    },
    Session {
        action: SessionsAction,
        own: bool,
    },
}

#[derive(Default)]
pub(crate) struct AdminUi {
    ask: Option<Ask>,
}

impl SqlrApp {
    /// Opens (or focuses) an administration panel of `conn`, connected or connecting.
    pub(crate) fn open_admin(&mut self, conn: &str, panel: AdminPanel) {
        if let Some(path) = self.dock.find_tab_from(|t| {
            t.admin
                .as_ref()
                .is_some_and(|a| a.conn() == conn && a.panel() == panel)
        }) {
            let _ = self.dock.set_active_tab(path);
            return;
        }
        if self.engine.admin_support(conn).is_none() {
            self.notify_as(
                ToastKind::Info,
                "Connect first: administration needs a live connection",
            );
            return;
        }
        let dialect = self.dialects.get(conn).cloned();
        let tab = match panel {
            AdminPanel::Users => AdminTab::Users(UsersTab::new(&self.engine, conn)),
            AdminPanel::Sessions => AdminTab::Sessions(SessionsTab::new(&self.engine, conn)),
            AdminPanel::Server => {
                AdminTab::Server(ServerTab::new(&self.engine, conn, dialect.as_deref()))
            }
        };
        let name = self.connection(conn).map_or(conn, |c| c.name.as_str());
        let title = format!("{} ({name})", panel.label());
        let key = self.new_tab(Some(title), Some(conn.to_string()), "");
        if let Some(t) = tab_mut(&mut self.dock, key) {
            t.admin = Some(Box::new(tab));
        }
    }

    /// Starts applying DDL in the users tab `key` (after the confirmations).
    pub(crate) fn start_admin_apply(
        &mut self,
        key: u64,
        statements: Vec<String>,
        options: ApplyOptions,
    ) {
        let engine = &self.engine;
        if let Some(AdminTab::Users(t)) =
            tab_mut(&mut self.dock, key).and_then(|t| t.admin.as_deref_mut())
        {
            t.start_apply(engine, statements, options);
        }
    }

    /// Handles an administration tab's command.
    pub(crate) fn admin_command(&mut self, key: u64, command: AdminCommand) {
        let Some(conn) = tab_mut(&mut self.dock, key)
            .and_then(|t| t.admin.as_deref())
            .map(|a| a.conn().to_string())
        else {
            return;
        };
        match command {
            AdminCommand::ApplyRole => {
                let transactional = self.editing_for(&conn).is_some_and(|e| e.transactional_ddl);
                let engine = &self.engine;
                let Some(tab) =
                    tab_mut(&mut self.dock, key).and_then(|t| match t.admin.as_deref_mut() {
                        Some(AdminTab::Users(u)) => Some(u),
                        _ => None,
                    })
                else {
                    return;
                };
                match tab.changes(engine, transactional) {
                    Ok(Some(c)) => {
                        let then = AfterApply::Admin(key);
                        self.request_ddl(&conn, c.statements, c.options, then);
                    }
                    Ok(None) => {}
                    Err(e) => self.notify(e),
                }
            }
            AdminCommand::DropRole(role) => match self.engine.drop_role_ddl(&conn, &role) {
                Ok(statements) => {
                    let current = self.engine.admin_current_user(&conn).as_deref()
                        == Some(role.name.as_str());
                    self.admin.ask = Some(Ask {
                        key,
                        conn,
                        what: Asked::Drop {
                            role,
                            statements,
                            current,
                        },
                        again: false,
                    });
                }
                Err(e) => self.notify(e.message),
            },
            AdminCommand::Discard(action) => {
                self.objects.discard = Some((key, crate::objects::Discard::Users(action)));
            }
            AdminCommand::Session { action, own } => {
                self.admin.ask = Some(Ask {
                    key,
                    conn,
                    what: Asked::Session { action, own },
                    again: false,
                });
            }
        }
    }

    /// Draws the pending administration confirmation (every frame).
    pub(crate) fn admin_ui(&mut self, ctx: &egui::Context) {
        let Some(ask) = &self.admin.ask else {
            return;
        };
        let syntax = self
            .dialects
            .get(&ask.conn)
            .map_or_else(SqlSyntax::ansi, |d| d.syntax());
        let (title, confirm): (String, &str) = match (&ask.what, ask.again) {
            (Asked::Drop { role, .. }, false) => {
                (format!("Drop role {}?", role.grantee()), "Drop role")
            }
            (Asked::Drop { role, .. }, true) => (
                format!("You are connected as {}. Drop it anyway?", role.name),
                "Drop it anyway",
            ),
            (Asked::Session { action, .. }, again) => {
                let (id, cancel) = match action {
                    SessionsAction::Cancel(id) => (id, true),
                    SessionsAction::Terminate(id) => (id, false),
                    SessionsAction::Refresh => return,
                };
                match (cancel, again) {
                    (true, false) => (
                        format!("Cancel the running query of session {id}?"),
                        "Cancel the query",
                    ),
                    (false, false) => (format!("Terminate session {id}?"), "Terminate the session"),
                    (true, true) => (
                        format!("Session {id} is this connection. Cancel its query anyway?"),
                        "Cancel it anyway",
                    ),
                    (false, true) => (
                        format!("Session {id} is this connection. Terminate it anyway?"),
                        "Terminate it anyway",
                    ),
                }
            }
        };
        let dismiss = match ask.what {
            Asked::Drop { .. } => "Cancel",
            Asked::Session { .. } => "Close",
        };
        let heading = match &ask.what {
            Asked::Drop { role, .. } => format!("Drop role {}", role.name),
            Asked::Session {
                action: SessionsAction::Cancel(_),
                ..
            } => "Cancel query".to_string(),
            Asked::Session { .. } => "Terminate session".to_string(),
        };
        let spec = crate::confirm::ConfirmSpec::new(
            Id::new(("admin_confirm", ask.again)),
            theme::icons::WARNING,
            &heading,
            confirm,
        )
        .dismiss(dismiss)
        .width(560.0);
        let answer = crate::confirm::confirm_modal(
            ctx,
            spec,
            |ui| {
                ui.label(&title);
                ui.add_space(Spacing::S);
                if let Asked::Drop { statements, .. } = &ask.what {
                    sql_view(ui, "drop_role_sql", &script(statements), &syntax, 120.0);
                }
            },
            |_| {},
        );
        let Some(yes) = answer else {
            return;
        };
        let Some(mut ask) = self.admin.ask.take() else {
            return;
        };
        if !yes {
            return;
        }
        let needs_again = match &ask.what {
            Asked::Drop { current, .. } => *current,
            Asked::Session { own, .. } => *own,
        };
        if needs_again && !ask.again {
            ask.again = true;
            self.admin.ask = Some(ask);
            return;
        }
        match ask.what {
            Asked::Drop { statements, .. } => {
                let transactional = self
                    .editing_for(&ask.conn)
                    .is_some_and(|e| e.transactional_ddl);
                let options = ApplyOptions {
                    transactional,
                    ..Default::default()
                };
                self.start_admin_apply(ask.key, statements, options);
            }
            Asked::Session { action, .. } => {
                // The session's start, as listed: a reused id is not acted on.
                let mut started = |id: &str| match tab_mut(&mut self.dock, ask.key)
                    .and_then(|t| t.admin.as_deref())
                {
                    Some(AdminTab::Sessions(t)) => t.started_ms(id),
                    _ => None,
                };
                let action = match action {
                    SessionsAction::Cancel(id) => AdminAction::CancelSession {
                        started_ms: started(&id),
                        id,
                    },
                    SessionsAction::Terminate(id) => AdminAction::TerminateSession {
                        started_ms: started(&id),
                        id,
                    },
                    SessionsAction::Refresh => return,
                };
                let id = self.engine.admin_action(&ask.conn, action);
                if let Some(AdminTab::Sessions(t)) =
                    tab_mut(&mut self.dock, ask.key).and_then(|t| t.admin.as_deref_mut())
                {
                    if let Some(old) = t.action.replace(id) {
                        // The previous action stops before it is forgotten (a no-op once
                        // it ended).
                        self.engine.cancel_job(old);
                        self.engine.forget_job(old);
                    }
                } else {
                    self.engine.forget_job(id);
                }
            }
        }
    }
}
