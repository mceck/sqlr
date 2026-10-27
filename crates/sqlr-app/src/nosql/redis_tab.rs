//! The Redis tab: the key browser on the left, the value view on the right and a console
//! sub-tab. The widgets only draw; scans, pages, edits and commands go through the engine,
//! and everything that changes data goes through the app so the connection profile can ask
//! first.

use std::collections::HashSet;

use egui::{Align, Layout, Margin, RichText};
use sqlr_core::{
    Family, KvEdit, KvNodePath, Node, StatementClass, display_key, redact_command,
    split_command_line,
};
use sqlr_engine::{Engine, SessionStatus};
use sqlr_ui::theme::{self, Spacing, icons};
use sqlr_ui::{
    ConsoleState, KeyBrowserAction, KeyBrowserState, KvViewAction, KvViewState, NewKeyDraft,
    console, key_browser, key_value_view, new_key_dialog,
};

use super::RedisCommand;
use crate::app::SqlrApp;
use crate::confirm::{PendingKind, PendingRun, confirmation_for};
use crate::tabs::TabCommand;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Keys,
    Console,
}

pub(crate) struct RedisTab {
    db: u32,
    mode: Mode,
    browser: KeyBrowserState,
    /// The pattern the keys are scanned with.
    pattern: String,
    /// The key the value view shows.
    selected: Option<Vec<u8>>,
    value: KvViewState,
    new_key: Option<NewKeyDraft>,
    console: ConsoleState,
    separator: String,
}

impl RedisTab {
    pub(crate) fn new(db: u32, separator: String) -> Self {
        Self {
            db,
            mode: Mode::Keys,
            browser: KeyBrowserState::default(),
            pattern: String::new(),
            selected: None,
            value: KvViewState::default(),
            new_key: None,
            console: ConsoleState::default(),
            separator,
        }
    }

    pub(crate) fn db(&self) -> u32 {
        self.db
    }

    /// Shows `key` in the value view (and highlights it in the browser).
    pub(crate) fn select(&mut self, key: Vec<u8>) {
        self.mode = Mode::Keys;
        self.browser.current = Some(key.clone());
        self.selected = Some(key);
    }

    /// Shows the console with `line` in its input, not run.
    pub(crate) fn console_with(&mut self, line: &str) {
        self.mode = Mode::Console;
        self.console.input = line.to_string();
    }

    /// The key the value view shows.
    pub(crate) fn selected(&self) -> Option<&[u8]> {
        self.selected.as_deref()
    }

    /// Saves the value view's unsaved edit (Apply), as its Save button would. Only the key
    /// browser shows the value view.
    pub(crate) fn save_edit(&mut self) {
        if self.mode == Mode::Keys && self.selected.is_some() {
            self.value.request_save();
        }
    }

    /// Drops the value view's unsaved edit (Discard).
    pub(crate) fn discard_edit(&mut self) {
        self.value.discard_edit();
    }

    /// Scans the keys again and reloads the shown value unless it holds an unsaved edit
    /// (Refresh).
    pub(crate) fn refresh(&self, engine: &Engine, conn: &str) {
        if !matches!(
            engine.session_status(conn),
            Some(SessionStatus::Connected(_))
        ) {
            return;
        }
        engine.kv_scan(conn, self.db, &self.pattern, false);
        if let Some(key) = &self.selected
            && !self.value.has_unsaved_edit()
        {
            engine.kv_open(conn, self.db, key);
        }
    }

    #[cfg(test)]
    pub(crate) fn console_input(&self) -> &str {
        &self.console.input
    }
}

/// Draws the tab for connection `conn`.
pub(crate) fn redis_ui(
    ui: &mut egui::Ui,
    tab: u64,
    conn: &str,
    r: &mut RedisTab,
    engine: &Engine,
    commands: &mut Vec<TabCommand>,
) {
    let p = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        for (mode, label) in [(Mode::Keys, "Keys"), (Mode::Console, "Console")] {
            if theme::dialog_tab(ui, r.mode == mode, label).clicked() {
                r.mode = mode;
            }
        }
        ui.label(
            RichText::new(format!("db{}", r.db))
                .small()
                .monospace()
                .color(p.text_muted),
        );
    });
    theme::hairline(ui);
    match engine.session_status(conn) {
        Some(SessionStatus::Connected(_)) => {}
        Some(SessionStatus::Connecting) => {
            ui.add_space(Spacing::L);
            theme::hint(ui, "Connecting…");
            return;
        }
        Some(SessionStatus::Failed(e)) => {
            ui.add_space(Spacing::L);
            ui.colored_label(p.error, e.message);
            return;
        }
        _ => {
            ui.add_space(Spacing::L);
            theme::hint(ui, "Not connected: connect it from the explorer.");
            return;
        }
    }
    match r.mode {
        Mode::Keys => keys_ui(ui, tab, conn, r, engine, commands),
        Mode::Console => console_ui(ui, tab, conn, r, engine, commands),
    }
}

fn keys_ui(
    ui: &mut egui::Ui,
    tab: u64,
    conn: &str,
    r: &mut RedisTab,
    engine: &Engine,
    commands: &mut Vec<TabCommand>,
) {
    let p = theme::palette(ui.ctx());
    // A new session (or a reconnect) has no scan yet: start it.
    let keys = match engine.kv_keys(conn, r.db, &r.pattern) {
        Some(keys) => keys,
        None => {
            engine.kv_scan(conn, r.db, &r.pattern, false);
            engine.kv_keys(conn, r.db, &r.pattern).unwrap_or_default()
        }
    };
    let mut browser_action = None;
    egui::Panel::left(egui::Id::new(("redis_keys", tab)))
        .resizable(true)
        .default_size(320.0)
        .min_size(220.0)
        .frame(egui::Frame::NONE.inner_margin(Margin::same(Spacing::M as i8)))
        .show(ui, |ui| {
            if let Some(e) = &keys.error {
                ui.colored_label(p.error, &e.message);
            } else if keys.loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    theme::hint(ui, "Scanning…");
                });
            }
            browser_action = key_browser(ui, &mut r.browser, &keys.keys, keys.more, &r.separator);
        });
    match browser_action {
        Some(KeyBrowserAction::Open(key)) => r.select(key),
        Some(KeyBrowserAction::LoadMore) => {
            engine.kv_scan(conn, r.db, &r.pattern, true);
        }
        Some(KeyBrowserAction::SetPattern(pattern)) => {
            r.pattern = pattern;
            engine.kv_scan(conn, r.db, &r.pattern, false);
        }
        Some(KeyBrowserAction::Refresh) => {
            engine.kv_scan(conn, r.db, &r.pattern, false);
        }
        Some(KeyBrowserAction::Delete(keys)) => commands.push(TabCommand::Redis {
            tab,
            command: RedisCommand::Delete {
                conn: conn.to_string(),
                db: r.db,
                keys,
            },
        }),
        Some(KeyBrowserAction::NewKey) => r.new_key = Some(NewKeyDraft::default()),
        None => {}
    }
    if let Some((key, edit)) = new_key_dialog(ui.ctx(), &mut r.new_key) {
        r.select(key.clone());
        commands.push(TabCommand::Redis {
            tab,
            command: RedisCommand::Edit {
                conn: conn.to_string(),
                db: r.db,
                key,
                edit,
            },
        });
    }
    egui::Frame::NONE
        .inner_margin(Margin::same(Spacing::M as i8))
        .show(ui, |ui| value_ui(ui, tab, conn, r, engine, commands));
}

fn value_ui(
    ui: &mut egui::Ui,
    tab: u64,
    conn: &str,
    r: &mut RedisTab,
    engine: &Engine,
    commands: &mut Vec<TabCommand>,
) {
    let p = theme::palette(ui.ctx());
    let Some(key) = r.selected.clone() else {
        theme::hint(ui, "Select a key to see its value.");
        return;
    };
    let Some(value) = engine.kv_value(conn, r.db, &key) else {
        engine.kv_open(conn, r.db, &key);
        theme::hint(ui, "Loading…");
        return;
    };
    if let Some(new) = value.renamed_to {
        // The engine already loads the new name.
        r.select(new);
        return;
    }
    if let Some(e) = &value.error {
        ui.horizontal(|ui| {
            ui.colored_label(p.error, &e.message);
            if ui.button("Retry").clicked() {
                engine.kv_open(conn, r.db, &key);
            }
        });
    }
    if value.missing {
        theme::hint(
            ui,
            "This key does not exist: it was deleted or has expired.",
        );
        return;
    }
    let Some(info) = &value.info else {
        if value.loading {
            ui.horizontal(|ui| {
                ui.spinner();
                theme::hint(ui, "Loading…");
            });
        }
        return;
    };
    if value.loading {
        ui.spinner();
    }
    match key_value_view(ui, &mut r.value, info, value.page.as_deref()) {
        Some(KvViewAction::LoadMore) => {
            engine.kv_load_more(conn, r.db, &key);
        }
        Some(KvViewAction::Refresh) => {
            engine.kv_open(conn, r.db, &key);
        }
        Some(KvViewAction::Edit(edit)) => commands.push(TabCommand::Redis {
            tab,
            command: RedisCommand::Edit {
                conn: conn.to_string(),
                db: r.db,
                key,
                edit,
            },
        }),
        None => {}
    }
}

fn console_ui(
    ui: &mut egui::Ui,
    tab: u64,
    conn: &str,
    r: &mut RedisTab,
    engine: &Engine,
    commands: &mut Vec<TabCommand>,
) {
    let snapshot = engine.kv_console(conn, tab);
    r.console.history = snapshot.history.clone();
    let p = theme::palette(ui.ctx());
    egui::Frame::NONE
        .inner_margin(Margin::symmetric(Spacing::L as i8, Spacing::M as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let chip = |ui: &mut egui::Ui, text: String, color: egui::Color32| {
                    egui::Frame::new()
                        .fill(p.bg_subtle)
                        .corner_radius(egui::CornerRadius::same(4))
                        .inner_margin(Margin::symmetric(6, 2))
                        .show(ui, |ui| {
                            ui.label(RichText::new(text).small().monospace().color(color));
                        });
                };
                chip(ui, format!("db{}", snapshot.db), p.text_muted);
                if let Some(n) = snapshot.queued {
                    chip(ui, format!("MULTI · {n} queued"), p.warning);
                }
                if snapshot.running {
                    ui.spinner();
                    theme::hint(ui, "Running…");
                    if icons::button(ui, true, icons::STOP, Some(p.error), "Cancel")
                        .on_hover_text("Cancel the running command")
                        .clicked()
                    {
                        engine.kv_cancel_console(conn);
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icons::button(
                        ui,
                        true,
                        icons::TRUNCATE,
                        Some(p.text_muted),
                        "Clear output",
                    )
                    .on_hover_text("Clear the console output")
                    .clicked()
                    {
                        engine.kv_clear_console(conn, tab);
                    }
                });
            });
            ui.add_space(Spacing::M);
            if let Some(line) = console(ui, &mut r.console, &snapshot.output) {
                commands.push(TabCommand::Redis {
                    tab,
                    command: RedisCommand::Console {
                        conn: conn.to_string(),
                        tab,
                        db: snapshot.db,
                        line,
                    },
                });
            }
        });
}

/// What an edit is, as a command line for the confirmation: `HSET key field`.
fn edit_line(key: &[u8], edit: &KvEdit) -> (String, StatementClass) {
    let name = display_key(key);
    let (verb, extra) = match edit {
        KvEdit::SetString(_) => ("SET", String::new()),
        KvEdit::HashSet(field, _) => ("HSET", display_key(field)),
        KvEdit::HashDel(field) => ("HDEL", display_key(field)),
        KvEdit::ListSet(index, _) => ("LSET", index.to_string()),
        KvEdit::ListPush { left: true, .. } => ("LPUSH", String::new()),
        KvEdit::ListPush { left: false, .. } => ("RPUSH", String::new()),
        KvEdit::ListRem { index, .. } => ("LREM", index.to_string()),
        KvEdit::Create(first) => return edit_line(key, first),
        KvEdit::SetAdd(member) => ("SADD", display_key(member)),
        KvEdit::SetRem(member) => ("SREM", display_key(member)),
        KvEdit::ZAdd(member, _) => ("ZADD", display_key(member)),
        KvEdit::ZRem(member) => ("ZREM", display_key(member)),
        KvEdit::XAdd(_) => ("XADD", String::new()),
        KvEdit::XDel(id) => ("XDEL", id.clone()),
        KvEdit::Expire(Some(ms)) => ("PEXPIRE", ms.to_string()),
        KvEdit::Expire(None) => ("PERSIST", String::new()),
        KvEdit::Rename(new) => ("RENAME", display_key(new)),
        KvEdit::Delete => ("DEL", String::new()),
    };
    let line = if extra.is_empty() {
        format!("{verb} {name}")
    } else {
        format!("{verb} {name} {extra}")
    };
    // Deleting a key was confirmed by the dialog that asked for it.
    (line, StatementClass::Write)
}

impl SqlrApp {
    /// Whether `conn` is a key-value (Redis) connection.
    pub(crate) fn is_key_value(&self, conn: &str) -> bool {
        self.connection(conn)
            .and_then(|c| self.manifest(&c.driver))
            .is_some_and(|m| m.family == Family::KeyValue)
    }

    /// The tab browsing database `db` of `conn`, if one is open.
    fn find_redis_tab(&mut self, conn: &str, db: u32) -> Option<u64> {
        self.dock
            .iter_all_tabs()
            .find(|(_, t)| {
                t.conn.as_deref() == Some(conn) && t.redis.as_ref().is_some_and(|r| r.db() == db)
            })
            .map(|(_, t)| t.key)
    }

    /// Opens the Redis browser on database `db` of `conn`, showing `key` when given. A tab
    /// that is already open for the database is focused instead.
    pub(crate) fn open_redis(&mut self, conn: &str, db: u32, key: Option<Vec<u8>>) {
        if let Some(tab_key) = self.find_redis_tab(conn, db) {
            self.focus_tab(tab_key);
            if let (Some(key), Some(r)) = (key, self.tab(tab_key).and_then(|t| t.redis.as_mut())) {
                r.select(key);
            }
            return;
        }
        let Some(cfg) = self.connection(conn).cloned() else {
            return;
        };
        let separator = self
            .manifest(&cfg.driver)
            .and_then(|m| cfg.value(&m, "separator").map(str::to_string))
            .unwrap_or_else(|| ":".into());
        let tab_key = self.new_tab(
            Some(format!("{} (db{db})", cfg.name)),
            Some(conn.into()),
            "",
        );
        // The console starts on the database the tab was opened on.
        self.engine.kv_set_console_db(conn, tab_key, db);
        let mut redis = RedisTab::new(db, separator);
        if let Some(key) = key {
            redis.select(key);
        }
        if let Some(tab) = self.tab(tab_key) {
            tab.redis = Some(Box::new(redis));
        }
    }

    /// Opens what a double-clicked explorer node of a Redis connection stands for.
    pub(crate) fn open_redis_node(&mut self, conn: &str, node: &Node) {
        let Some(db) = KvNodePath::db_of(&node.path) else {
            return;
        };
        self.open_redis(conn, db, KvNodePath::key_of(&node.path));
    }

    /// Opens the browser on the database an explorer node named (`db3`).
    pub(crate) fn open_redis_database(&mut self, conn: &str, database: &str) {
        if let Some(db) = database.strip_prefix("db").and_then(|n| n.parse().ok()) {
            self.open_redis(conn, db, None);
        }
    }

    /// The database `conn` is configured on (0 when it names none).
    fn configured_redis_db(&self, conn: &str) -> u32 {
        self.connection(conn)
            .and_then(|c| {
                let m = self.manifest(&c.driver)?;
                c.value(&m, "database")?.trim().parse().ok()
            })
            .unwrap_or(0)
    }

    /// Opens (or focuses) the browser on `conn`'s configured database and shows its console
    /// with `line` in the input, not run.
    pub(crate) fn open_redis_console(&mut self, conn: &str, line: &str) {
        let db = self.configured_redis_db(conn);
        self.open_redis(conn, db, None);
        if let Some(tab) = self.find_redis_tab(conn, db)
            && let Some(redis) = self.tab(tab).and_then(|t| t.redis.as_mut())
        {
            redis.console_with(line);
        }
    }

    /// Opens the browser of every Redis connection the user just connected, once its session
    /// is up.
    pub(crate) fn open_pending_redis(&mut self) {
        if self.pending_redis.is_empty() {
            return;
        }
        let waiting: Vec<String> = self.pending_redis.iter().cloned().collect();
        let mut done: HashSet<String> = HashSet::new();
        for id in waiting {
            match self.engine.session_status(&id) {
                Some(SessionStatus::Connecting) => {}
                Some(SessionStatus::Connected(_)) => {
                    let db = self.configured_redis_db(&id);
                    self.open_redis(&id, db, None);
                    done.insert(id);
                }
                _ => {
                    done.insert(id);
                }
            }
        }
        self.pending_redis.retain(|id| !done.contains(id));
    }

    /// Applies a change from a Redis tab, after the connection profile's confirmation (the
    /// profile asks before writes and destructive commands, like it does for SQL).
    pub(crate) fn run_redis_command(&mut self, tab: u64, command: RedisCommand) {
        let conn = command.conn().to_string();
        let Some(cfg) = self.connection(&conn).cloned() else {
            return;
        };
        let profile = self.profile_for(&cfg);
        let (classes, lines) = self.redis_classes(&command);
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        // The prompt names the database: a command in the console may run on any of them.
        let name = format!("{} db{}", cfg.name, command.db());
        match confirmation_for(&profile, &name, &classes, &refs) {
            Some((prompt, statements)) => {
                self.confirm = Some(PendingRun {
                    tab,
                    conn,
                    kind: PendingKind::Redis(command),
                    prompt,
                    statements,
                });
            }
            None => self.apply_redis(command),
        }
    }

    /// The class and display line of each statement `command` runs. Console lines show
    /// without secrets.
    fn redis_classes(&self, command: &RedisCommand) -> (Vec<StatementClass>, Vec<String>) {
        match command {
            RedisCommand::Edit { key, edit, .. } => {
                let (line, class) = edit_line(key, edit);
                (vec![class], vec![line])
            }
            // The browser's own dialog already asked about deleting: a profile that confirms
            // writes asks once more, one that only confirms destructive commands does not.
            RedisCommand::Delete { keys, .. } => {
                let line = match keys.as_slice() {
                    [key] => format!("DEL {}", display_key(key)),
                    keys => format!("DEL {} keys", keys.len()),
                };
                (vec![StatementClass::Write], vec![line])
            }
            RedisCommand::Console { conn, line, .. } => {
                let Ok(args) = split_command_line(line) else {
                    // The engine answers the syntax error.
                    return (Vec::new(), Vec::new());
                };
                match self.engine.kv_classify(conn, line) {
                    Some(class) => (vec![class], vec![redact_command(&args)]),
                    None => (Vec::new(), Vec::new()),
                }
            }
        }
    }

    pub(crate) fn apply_redis(&mut self, command: RedisCommand) {
        match command {
            RedisCommand::Edit {
                conn,
                db,
                key,
                edit,
            } => {
                self.engine.kv_edit(&conn, db, &key, edit);
            }
            RedisCommand::Delete { conn, db, keys } => {
                self.engine.kv_delete(&conn, db, keys);
            }
            RedisCommand::Console {
                conn, tab, line, ..
            } => {
                self.engine.kv_command(&conn, tab, &line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_lines_name_the_command_and_the_class() {
        let (line, class) = edit_line(b"user:1", &KvEdit::HashSet(b"age".to_vec(), b"3".to_vec()));
        assert_eq!(line, "HSET user:1 age");
        assert_eq!(class, StatementClass::Write);
        let (line, class) = edit_line(b"k", &KvEdit::Delete);
        assert_eq!(line, "DEL k");
        assert_eq!(class, StatementClass::Write, "the dialog already asked");
        let (line, _) = edit_line(
            b"k",
            &KvEdit::Create(Box::new(KvEdit::SetString(b"v".to_vec()))),
        );
        assert_eq!(line, "SET k");
        let (line, _) = edit_line(
            b"l",
            &KvEdit::ListRem {
                index: 3,
                value: b"x".to_vec(),
            },
        );
        assert_eq!(line, "LREM l 3");
        let (line, _) = edit_line(b"k", &KvEdit::Expire(None));
        assert_eq!(line, "PERSIST k");
        let (line, _) = edit_line(b"bin\xff", &KvEdit::SetString(vec![1]));
        assert_eq!(line, "SET bin\\xff");
    }
}
