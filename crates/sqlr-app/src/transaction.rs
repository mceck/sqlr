//! Transaction controls (§7.9): the toolbar's commit mode, read-only, Commit, Rollback and
//! savepoint controls, the status bar's transaction line, and the prompts that end open
//! transactions before a tab closes, a connection disconnects, the app quits, auto-commit
//! is switched on, or an idle transaction times out.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{ComboBox, RichText};
use sqlr_core::CommitMode;
use sqlr_engine::{SessionKey, Slot, TxPhase, TxState};
use sqlr_ui::format_when;
use sqlr_ui::theme::{self, Spacing, icons};

use crate::app::{SqlrApp, tab_mut};

/// How often the UI wakes while a transaction is open, to notice an idle timeout.
const IDLE_CHECK: Duration = Duration::from_secs(15);

/// How long a disconnect waits for its transactions to end before forcing it through; a
/// dropped connection or a VPN change can leave a commit or rollback waiting forever.
const FORCE_DISCONNECT: Duration = Duration::from_secs(5);

/// What happens once the listed transactions are committed or rolled back.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EndTarget {
    CloseTab(u64),
    Disconnect(String),
    /// Disconnect and connect again, reloading the connection.
    Reconnect(String),
    Quit,
    /// Switch the session to auto-commit.
    AutoCommit(SessionKey),
    /// An idle transaction: nothing else happens.
    Idle(SessionKey),
}

/// A prompt to commit or roll back open transactions, and then do `target`.
pub(crate) struct PendingEnd {
    target: EndTarget,
    sessions: Vec<SessionKey>,
    /// `Some(commit)` once the user chose; the target runs when every session is idle.
    chosen: Option<bool>,
    /// A disconnect or reconnect runs anyway at this time, even if a transaction is still
    /// waiting on an unreachable server.
    force_at: Option<Instant>,
}

impl EndTarget {
    /// Whether the target forces through when its transactions do not end in time.
    fn force_on_timeout(&self) -> bool {
        matches!(self, Self::Disconnect(_) | Self::Reconnect(_))
    }
}

/// The status bar's transaction line.
#[derive(Clone, Debug, PartialEq)]
pub struct TxLine {
    pub text: String,
    pub failed: bool,
}

impl TxLine {
    pub fn of(tx: &TxState) -> Option<Self> {
        match tx.phase {
            TxPhase::Idle => None,
            TxPhase::Failed => Some(Self {
                text: "Transaction failed — only Rollback is possible".into(),
                failed: true,
            }),
            TxPhase::Open => {
                let n = tx.statements;
                let noun = if n == 1 { "statement" } else { "statements" };
                let since = tx
                    .since_ms
                    .map(|ms| format!(" · since {}", hh_mm(ms)))
                    .unwrap_or_default();
                Some(Self {
                    text: format!("Transaction: {n} {noun}{since}"),
                    failed: false,
                })
            }
        }
    }
}

/// Local `HH:MM` of a Unix-milliseconds time.
fn hh_mm(ms: i64) -> String {
    let when = format_when(ms);
    when.get(11..16).unwrap_or(&when).to_string()
}

/// The words for a commit mode, in the toolbar's commit-mode menu.
pub(crate) fn commit_mode_status(mode: CommitMode) -> &'static str {
    match mode {
        CommitMode::Auto => "Auto-commit",
        CommitMode::Manual => "Manual commit",
        CommitMode::Smart => "Smart commit",
    }
}

/// One line on what a commit mode does, for its menu and tooltip.
pub(crate) fn mode_help(mode: CommitMode) -> &'static str {
    match mode {
        CommitMode::Auto => "Every statement is committed as soon as it runs",
        CommitMode::Manual => "Changes wait in a transaction until you commit or roll back",
        CommitMode::Smart => {
            "Reads run in auto-commit; the first change opens a transaction to commit"
        }
    }
}

impl SqlrApp {
    /// The active tab's session and its transaction state, when the tab has a connection.
    /// A Redis tab never opens a session of its own: its scans, edits and console lines run
    /// on the connection's explorer session, so that is the one its controls act on.
    pub(crate) fn active_session(&mut self) -> Option<(SessionKey, Option<TxState>)> {
        let key = self.active_key()?;
        let tab = tab_mut(&mut self.dock, key)?;
        let conn = tab.conn.clone()?;
        let session = if tab.redis.is_some() {
            SessionKey::meta(&conn)
        } else {
            self.engine.session(&conn, Some(key))
        };
        let tx = self.engine.transaction(&session);
        Some((session, tx))
    }

    /// Profile colors of the saved connections that have one, by connection id.
    pub(crate) fn profile_tints(&self) -> HashMap<String, [u8; 3]> {
        self.connections
            .iter()
            .filter_map(|c| Some((c.id.clone(), self.profile_for(c).color?)))
            .collect()
    }

    /// Commit mode, read-only, Commit, Rollback, Savepoint and "Rollback to ▾"; only
    /// read-only for drivers without transactions. `folded` when drawn inside the
    /// toolbar's overflow popup: the commit mode is then a row of segments, since a combo's
    /// own popup would close the overflow one.
    pub(crate) fn transaction_controls(&mut self, ui: &mut egui::Ui, folded: bool) {
        let session = self.active_session();
        let (key, tx) = match &session {
            Some((key, tx)) => (Some(key.clone()), tx.clone()),
            None => (None, None),
        };
        // Before the session opens, the controls show the profile's settings.
        let profile = key
            .as_ref()
            .and_then(|k| self.connection(&k.conn))
            .map(|c| self.profile_for(c));
        // Without a session (or a known driver) the controls stay, disabled.
        let transactions = key
            .as_ref()
            .and_then(|key| self.connection(&key.conn))
            .and_then(|cfg| self.engine.manifest(&cfg.driver))
            .is_none_or(|manifest| manifest.capabilities.transactions);
        let mode = tx
            .as_ref()
            .map(|t| t.mode)
            .or(profile.as_ref().map(|p| p.commit_mode))
            .unwrap_or_default();
        let read_only = tx
            .as_ref()
            .map(|t| t.read_only)
            .or(profile.as_ref().map(|p| p.read_only))
            .unwrap_or(false);
        let phase = tx.as_ref().map_or(TxPhase::Idle, |t| t.phase.clone());
        let connected = tx.is_some();
        let mut picked = None;
        let palette = theme::palette(ui.ctx());
        if transactions {
            ui.add_enabled_ui(connected, |ui| {
                if folded {
                    for m in [CommitMode::Auto, CommitMode::Manual, CommitMode::Smart] {
                        if theme::segment(ui, m == mode, commit_mode_status(m))
                            .on_hover_text(mode_help(m))
                            .clicked()
                        {
                            picked = Some(m);
                        }
                    }
                    return;
                }
                ComboBox::from_id_salt("commit_mode")
                    .selected_text(commit_mode_status(mode))
                    .width(116.0)
                    .show_ui(ui, |ui| {
                        ui.set_min_width(280.0);
                        for m in [CommitMode::Auto, CommitMode::Manual, CommitMode::Smart] {
                            let response = ui.selectable_label(m == mode, commit_mode_status(m));
                            ui.label(
                                RichText::new(mode_help(m))
                                    .small()
                                    .color(palette.text_muted),
                            );
                            ui.add_space(Spacing::S);
                            if response.clicked() {
                                picked = Some(m);
                            }
                        }
                    })
                    .response
                    .on_hover_text(format!("Commit mode: {}", mode_help(mode)))
                    .on_disabled_hover_text("Connect to change the commit mode");
            });
        }
        let toggled = icons::toolbar_button(
            ui,
            connected,
            icons::READ_ONLY,
            None,
            read_only,
            "Read-only",
        );
        let toggled = theme::tooltip(
            toggled,
            "Read-only session",
            None,
            Some("Refuse statements that modify data"),
        )
        .clicked();
        if !transactions {
            if let (Some(key), true) = (&key, toggled) {
                self.engine.set_read_only(key, !read_only);
            }
            return;
        }
        let open = phase != TxPhase::Idle;
        let commit = icons::toolbar_button(
            ui,
            open,
            icons::COMMIT,
            Some(palette.success),
            false,
            "Commit",
        );
        let commit = theme::tooltip_for(
            commit,
            "Commit",
            sqlr_core::ActionId::Commit,
            Some("Make the open transaction's changes permanent"),
        )
        .clicked();
        let rollback = icons::toolbar_button(ui, open, icons::ROLLBACK, None, false, "Rollback");
        let rollback = theme::tooltip_for(
            rollback,
            "Rollback",
            sqlr_core::ActionId::Rollback,
            Some("Discard the open transaction's changes"),
        )
        .clicked();
        let savepoints = tx
            .as_ref()
            .map(|t| t.savepoints.clone())
            .unwrap_or_default();
        let can_savepoint = phase == TxPhase::Open && mode != CommitMode::Auto;
        let savepoint = icons::toolbar_button(
            ui,
            can_savepoint,
            icons::SAVEPOINT,
            None,
            false,
            "Savepoint",
        );
        let savepoint = theme::tooltip(
            savepoint,
            "Savepoint",
            None,
            Some("Mark a point in the transaction to roll back to"),
        )
        .clicked();
        let mut rollback_to = None;
        let can_rollback_to = open && !savepoints.is_empty();
        // A flat toolbar button like its neighbours; its popup lists the savepoints.
        let menu = icons::toolbar_button(
            ui,
            can_rollback_to,
            icons::ROLLBACK_TO,
            None,
            false,
            "Rollback to savepoint",
        );
        egui::Popup::menu(&menu)
            .style(theme::menu_style(ui.ctx()))
            .show(|ui| {
                for name in &savepoints {
                    if theme::menu::item(ui, icons::ROLLBACK_TO, name, None).clicked() {
                        rollback_to = Some(name.clone());
                        ui.close();
                    }
                }
            });
        theme::tooltip(
            menu,
            "Roll back to a savepoint",
            None,
            Some("Undo the changes made after a savepoint, keeping the transaction open"),
        );
        let Some(key) = key else {
            return;
        };
        if let Some(m) = picked {
            if m == CommitMode::Auto && open {
                self.ask_end(EndTarget::AutoCommit(key.clone()), vec![key.clone()]);
            } else {
                self.engine.set_commit_mode(&key, m);
            }
        }
        if toggled {
            self.engine.set_read_only(&key, !read_only);
        }
        if commit {
            self.engine.commit(&key);
        }
        if rollback {
            self.engine.rollback(&key);
        }
        if savepoint {
            let name = (savepoints.len() + 1..)
                .map(|n| format!("sp{n}"))
                .find(|n| !savepoints.contains(n))
                .unwrap_or_default();
            self.engine.savepoint(&key, name);
        }
        if let Some(name) = rollback_to {
            self.engine.rollback_to(&key, name);
        }
    }

    /// Commits (or rolls back) the active tab's transaction.
    pub(crate) fn commit_active(&mut self, commit: bool) {
        if let Some((key, _)) = self.active_session() {
            if commit {
                self.engine.commit(&key);
            } else {
                self.engine.rollback(&key);
            }
        }
    }

    /// The active tab's transaction line for the status bar.
    pub(crate) fn tx_line(&mut self) -> Option<TxLine> {
        let (_, tx) = self.active_session()?;
        TxLine::of(&tx?)
    }

    /// Asks to commit or roll back `sessions` before `target`; without open transactions
    /// `target` happens at once.
    pub(crate) fn ask_end(&mut self, target: EndTarget, sessions: Vec<SessionKey>) {
        let sessions: Vec<SessionKey> = sessions
            .into_iter()
            .filter(|k| {
                self.engine
                    .transaction(k)
                    .is_some_and(|t| t.phase != TxPhase::Idle)
            })
            .collect();
        if sessions.is_empty() {
            self.finish_end(target);
            return;
        }
        self.ending = Some(PendingEnd {
            target,
            sessions,
            chosen: None,
            force_at: None,
        });
    }

    /// The open transactions of `conn`'s sessions.
    pub(crate) fn open_sessions_of(&self, conn: &str) -> Vec<SessionKey> {
        self.engine
            .open_transactions()
            .into_iter()
            .map(|(k, _)| k)
            .filter(|k| k.conn == conn)
            .collect()
    }

    /// A tab's close: asks first when its own session has an open transaction.
    pub(crate) fn request_close_tab(&mut self, key: u64) {
        // A Redis tab opens no session of its own.
        if tab_mut(&mut self.dock, key).is_some_and(|t| t.redis.is_some()) {
            self.close_tab_now(key);
            return;
        }
        if self.ask_discard_on_close(key) {
            return;
        }
        self.close_without_discard(key);
    }

    /// A tab's close once its pending edits are dropped: asks first when its own session
    /// has an open transaction.
    pub(crate) fn close_without_discard(&mut self, key: u64) {
        let Some(conn) = tab_mut(&mut self.dock, key).and_then(|t| t.conn.clone()) else {
            self.close_tab_now(key);
            return;
        };
        let session = self.engine.session(&conn, Some(key));
        if session.slot == Slot::Meta {
            // A shared session outlives the tab.
            self.close_tab_now(key);
            return;
        }
        self.ask_end(EndTarget::CloseTab(key), vec![session]);
    }

    pub(crate) fn request_disconnect(&mut self, conn: &str) {
        let sessions = self.open_sessions_of(conn);
        self.ask_end(EndTarget::Disconnect(conn.to_string()), sessions);
    }

    /// Disconnects and reconnects `conn`, reloading its config, secrets and explorer.
    pub(crate) fn request_reconnect(&mut self, conn: &str) {
        let sessions = self.open_sessions_of(conn);
        self.ask_end(EndTarget::Reconnect(conn.to_string()), sessions);
    }

    /// Closes the tab, its results and its session; Reopen closed tab can open it again.
    pub(crate) fn close_tab_now(&mut self, key: u64) {
        let Some(path) = self.dock.find_tab_from(|t| t.key == key) else {
            return;
        };
        self.remember_closed_tab(key);
        if let Some(tab) = tab_mut(&mut self.dock, key) {
            if let (Some(redis), Some(conn)) = (&tab.redis, &tab.conn) {
                self.engine.kv_close_console(conn, key);
                self.engine.kv_release(conn, redis.db());
            }
            tab.close_run(&self.engine);
            tab.close_plan(&self.engine);
            if let Some(er) = &mut tab.er {
                er.close(&self.engine);
            }
            if let Some(doc) = &mut tab.doc {
                doc.close(&self.engine);
            }
            if let Some(object) = &mut tab.object {
                object.close(&self.engine);
            }
            if let Some(admin) = &mut tab.admin {
                admin.close(&self.engine);
            }
            if let Some(conn) = tab.conn.clone() {
                // Its own session, whatever the per-tab setting is now (a no-op if none).
                let session = SessionKey {
                    conn,
                    slot: Slot::Tab(key),
                };
                self.engine.close_session(&session);
            }
        }
        let _ = self.dock.remove_tab(path);
    }

    fn finish_end(&mut self, target: EndTarget) {
        match target {
            EndTarget::CloseTab(key) => self.close_tab_now(key),
            EndTarget::Disconnect(conn) => {
                self.engine.disconnect(&conn);
                // A disconnected connection shows no children; collapse it so the next
                // click expands and reconnects instead of only closing it.
                self.tree.collapse_connection(&conn);
            }
            EndTarget::Reconnect(conn) => {
                self.reconnect(&conn);
            }
            EndTarget::Quit => {
                self.quit_confirmed = true;
                self.quit_now = true;
            }
            EndTarget::AutoCommit(key) => self.engine.set_commit_mode(&key, CommitMode::Auto),
            EndTarget::Idle(_) => {}
        }
    }

    /// Intercepts the window's close while transactions are open (§7.9).
    pub(crate) fn guard_quit(&mut self, ctx: &egui::Context) {
        if self.quit_now {
            self.quit_now = false;
            self.mcp.stop();
            self.terminal.kill();
            while let Some(mut pending) = self.pending_approvals.pop_front() {
                pending.answer(Err(sqlr_mcp::error::ToolError::denied()));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.quit_confirmed || !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        let open: Vec<SessionKey> = self
            .engine
            .open_transactions()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        if !open.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.ask_end(EndTarget::Quit, open);
        }
    }

    /// Prompts for idle transactions and draws the pending end prompt; runs its target
    /// once every session ended. `failed` is set when the engine reported a failure this
    /// frame (a commit that did not go through stops the pending target).
    pub(crate) fn end_prompts(&mut self, ctx: &egui::Context, failed: bool) {
        let open = self.engine.open_transactions();
        if !open.is_empty() {
            ctx.request_repaint_after(IDLE_CHECK);
        }
        if self.ending.is_none()
            && let Some((key, tx)) = open.iter().find(|(k, tx)| {
                tx.idle_timeout && self.kept_open.get(k) != Some(&tx.last_activity_ms)
            })
        {
            self.kept_open.insert(key.clone(), tx.last_activity_ms);
            self.ending = Some(PendingEnd {
                target: EndTarget::Idle(key.clone()),
                sessions: vec![key.clone()],
                chosen: None,
                force_at: None,
            });
        }
        let Some(pending) = &self.ending else {
            return;
        };
        let now = Instant::now();
        if pending.force_at.is_some_and(|at| now >= at) {
            // The server never answered the commit or rollback: disconnect anyway.
            if let Some(pending) = self.ending.take() {
                self.finish_end(pending.target);
            }
            return;
        }
        if let Some(at) = pending.force_at {
            ctx.request_repaint_after(at.saturating_duration_since(now));
        }
        if pending.chosen.is_some() {
            if failed {
                self.ending = None;
                return;
            }
            let done = pending.sessions.iter().all(|k| {
                self.engine
                    .transaction(k)
                    .is_none_or(|t| t.phase == TxPhase::Idle)
            });
            if done && let Some(pending) = self.ending.take() {
                self.finish_end(pending.target);
            }
            return;
        }
        let names: Vec<String> = pending
            .sessions
            .iter()
            .map(|k| {
                let name = self
                    .connection(&k.conn)
                    .map_or(k.conn.clone(), |c| c.name.clone());
                match &k.slot {
                    Slot::Meta => name,
                    Slot::Tab(t) => {
                        match self.dock.iter_all_tabs().find(|(_, tab)| tab.key == *t) {
                            Some((_, tab)) => format!("{name} ({})", tab.title),
                            None => name,
                        }
                    }
                    Slot::Agent(_) => format!("{name} (agent)"),
                }
            })
            .collect();
        let (prompt, keep_label) = match &pending.target {
            EndTarget::Idle(_) => {
                let minutes = self
                    .connection(&pending.sessions[0].conn)
                    .map(|c| self.profile_for(c))
                    .and_then(|p| p.idle_tx_timeout_min)
                    .unwrap_or(0);
                (
                    format!(
                        "The transaction on {} has been idle for {minutes} min.",
                        names.join(", ")
                    ),
                    "Keep open",
                )
            }
            EndTarget::AutoCommit(_) => (
                format!(
                    "Auto-commit ends the open transaction on {}.",
                    names.join(", ")
                ),
                "Cancel",
            ),
            _ => (
                format!("Open transactions on {}.", names.join(", ")),
                "Cancel",
            ),
        };
        let response =
            theme::Dialog::new("end_transactions", theme::icons::COMMIT, "Open transaction")
                .subtitle("Commit or roll back before continuing")
                .width(480.0)
                .show(ctx, |d| {
                    d.body(|ui| {
                        ui.label(&prompt);
                    });
                    d.footer(|f| {
                        if f.primary("Commit", None) {
                            return Some(true);
                        }
                        if f.button("Roll back", true) {
                            return Some(false);
                        }
                        f.cancel_as(keep_label);
                        None
                    })
                });
        let answer = match response.inner {
            Some(commit) => Some(Some(commit)),
            None if response.closed => Some(None),
            None => None,
        };
        match answer {
            None => {}
            Some(None) => self.ending = None,
            Some(Some(commit)) => self.answer_end(commit),
        }
    }

    /// Commits or rolls back the pending transactions. A disconnect or reconnect that the
    /// server does not let finish within [`FORCE_DISCONNECT`] is forced through anyway.
    fn answer_end(&mut self, commit: bool) {
        let Some(pending) = &mut self.ending else {
            return;
        };
        let sessions = pending.sessions.clone();
        let force = pending.target.force_on_timeout();
        for k in &sessions {
            if commit {
                self.engine.commit(k);
            } else {
                self.engine.rollback(k);
            }
        }
        pending.chosen = Some(commit);
        if force {
            pending.force_at = Some(Instant::now() + FORCE_DISCONNECT);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use sqlr_core::{ConnectionConfig, Secrets};
    use sqlr_engine::{DriverRegistry, Engine, QueryStatus, SessionStatus};
    use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
    use sqlr_testkit::{FakeDriver, FakeScript};

    use super::*;
    use crate::app::AppDeps;

    fn wait(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn closing_a_tab_closes_its_own_session_after_the_setting_changes() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeDriver::new(FakeScript::default());
        let mut drivers = DriverRegistry::default();
        drivers.register(fake.clone());
        let repo =
            ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
        let cfg = ConnectionConfig {
            id: "c1".into(),
            name: "db".into(),
            driver: "fake".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        repo.save(&cfg, &Secrets::default()).unwrap();
        let deps = AppDeps {
            check_updates: false,
            engine: Engine::new(drivers, Arc::new(|| {})),
            repo,
            settings: Settings::default(),
            config_dir: dir.path().to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
        };
        let mut app = SqlrApp::new(deps, &egui::Context::default());
        let key = app.new_tab(None, Some("c1".into()), "select 1");
        app.run_tab(key);
        wait("the tab's own connection", || fake.open_connections() == 2);
        app.settings.session_per_tab = false;
        app.apply_engine_settings();
        app.close_tab_now(key);
        wait("the tab's session to close", || {
            fake.open_connections() == 1
        });
    }

    /// An app with the saved connection "c1" on `fake`.
    fn app_with_fake(fake: Arc<FakeDriver>) -> (SqlrApp, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut drivers = DriverRegistry::default();
        drivers.register(fake);
        let repo =
            ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
        let cfg = ConnectionConfig {
            id: "c1".into(),
            name: "db".into(),
            driver: "fake".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        repo.save(&cfg, &Secrets::default()).unwrap();
        let deps = AppDeps {
            check_updates: false,
            engine: Engine::new(drivers, Arc::new(|| {})),
            repo,
            settings: Settings::default(),
            config_dir: dir.path().to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
        };
        (SqlrApp::new(deps, &egui::Context::default()), dir)
    }

    #[test]
    fn nontransactional_toolbar_keeps_read_only_without_transaction_controls() {
        use egui_kittest::kittest::Queryable;
        let (mut app, _dir) =
            app_with_fake(FakeDriver::without_transactions(FakeScript::default()));
        app.connect("c1");
        wait("connect", || app.is_live("c1"));
        app.new_tab(None, Some("c1".into()), "select 1");
        let mut harness = egui_kittest::Harness::builder()
            .with_size((900.0, 100.0))
            .build_ui_state(
                |ui, app: &mut SqlrApp| app.transaction_controls(ui, true),
                app,
            );
        sqlr_ui::theme::install_fonts(&harness.ctx, &[]);
        harness.run_steps(2);
        assert!(harness.query_by_label("Read-only").is_some());
        for label in ["Tx", "Manual", "Commit", "Rollback", "Savepoint"] {
            assert!(
                harness.query_by_label(label).is_none(),
                "unexpected {label} control"
            );
        }
    }

    #[test]
    fn a_disconnect_starts_the_force_timer_once_the_user_chooses() {
        let fake = FakeDriver::new(FakeScript::default());
        let (mut app, _dir) = app_with_fake(fake);
        app.ending = Some(PendingEnd {
            target: EndTarget::Disconnect("c1".into()),
            sessions: Vec::new(),
            chosen: None,
            force_at: None,
        });

        app.answer_end(false);

        let force_at = app.ending.as_ref().unwrap().force_at.unwrap();
        let left = force_at.saturating_duration_since(Instant::now());
        assert!(left <= FORCE_DISCONNECT);
        assert!(left > FORCE_DISCONNECT - Duration::from_secs(1), "{left:?}");
    }

    #[test]
    fn disconnect_is_forced_when_the_transaction_never_ends() {
        let fake = FakeDriver::new(FakeScript::default());
        let (mut app, _dir) = app_with_fake(fake.clone());
        app.connect("c1");
        wait("the connect", || app.is_live("c1"));
        let key = app.engine.session("c1", None);
        let begin = app.engine.execute_in(&key, None, "begin".into(), 100);
        wait("the begin", || {
            app.engine
                .result(begin)
                .is_some_and(|r| r.read().status != QueryStatus::Running)
        });
        assert_ne!(app.engine.transaction(&key).unwrap().phase, TxPhase::Idle);

        app.ending = Some(PendingEnd {
            target: EndTarget::Disconnect("c1".into()),
            sessions: vec![key],
            chosen: Some(false),
            force_at: Some(Instant::now() - Duration::from_secs(1)),
        });
        app.end_prompts(&egui::Context::default(), false);

        assert!(app.ending.is_none(), "the prompt went away");
        assert_eq!(
            app.engine.session_status("c1"),
            Some(SessionStatus::Disconnected)
        );
        wait("the forced disconnect", || fake.open_connections() == 0);
    }

    fn tx(phase: TxPhase, statements: u32) -> TxState {
        let depth = u32::from(phase != TxPhase::Idle);
        TxState {
            mode: CommitMode::Manual,
            read_only: false,
            phase,
            depth,
            statements,
            since_ms: None,
            savepoints: Vec::new(),
            last_activity_ms: 0,
            idle_timeout: false,
            wrote: false,
            started_by_savepoint: false,
        }
    }

    #[test]
    fn status_line_per_phase() {
        assert_eq!(TxLine::of(&tx(TxPhase::Idle, 0)), None);
        assert_eq!(
            TxLine::of(&tx(TxPhase::Open, 1)).unwrap().text,
            "Transaction: 1 statement"
        );
        assert_eq!(
            TxLine::of(&tx(TxPhase::Open, 3)).unwrap().text,
            "Transaction: 3 statements"
        );
        let failed = TxLine::of(&tx(TxPhase::Failed, 3)).unwrap();
        assert!(failed.failed);
        assert_eq!(
            failed.text,
            "Transaction failed — only Rollback is possible"
        );
        let since = TxLine::of(&TxState {
            since_ms: Some(0),
            ..tx(TxPhase::Open, 2)
        })
        .unwrap();
        assert!(since.text.starts_with("Transaction: 2 statements · since "));
        assert_eq!(
            since.text.len(),
            "Transaction: 2 statements · since 00:00".len()
        );
    }
}
