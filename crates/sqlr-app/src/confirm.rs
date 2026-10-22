//! Production safeguards (§7.9): before a run, each statement is classified and the
//! connection profile decides whether to ask first.

use egui::{Id, RichText};
use sqlr_core::{ConnectionProfile, SqlSyntax, StatementClass};
use sqlr_ui::RunRequest;

use crate::nosql::RedisCommand;
use sqlr_ui::theme::{self, Spacing};

use crate::app::SqlrApp;

/// Statements listed in a confirmation.
const LISTED: usize = 10;
/// Chars of each listed statement's first line.
const LINE_CHARS: usize = 100;

/// What runs once the user confirms.
#[derive(Clone, Debug)]
pub(crate) enum PendingKind {
    Single(RunRequest),
    Script(Vec<RunRequest>),
    /// A change from a Redis tab: an edit, a key deletion or a console line.
    Redis(RedisCommand),
    /// A run or an edit of a document tab, or an index change.
    Doc(crate::nosql::DocCommand),
}

/// A run waiting for "Run" or "Cancel".
#[derive(Clone, Debug)]
pub(crate) struct PendingRun {
    pub tab: u64,
    pub conn: String,
    pub kind: PendingKind,
    pub prompt: String,
    /// The statements the prompt is about, one line each.
    pub statements: Vec<String>,
}

/// The confirmation `statements` need on `conn_name` under `profile`, if any: the prompt
/// and the statements it is about.
pub(crate) fn confirmation(
    profile: &ConnectionProfile,
    conn_name: &str,
    syntax: &SqlSyntax,
    statements: &[&str],
) -> Option<(String, Vec<String>)> {
    let classes: Vec<StatementClass> = statements.iter().map(|s| syntax.classify(s)).collect();
    confirmation_for(profile, conn_name, &classes, statements)
}

/// Like [`confirmation`], for statements already classified (by a driver that is not SQL).
pub(crate) fn confirmation_for(
    profile: &ConnectionProfile,
    conn_name: &str,
    classes: &[StatementClass],
    statements: &[&str],
) -> Option<(String, Vec<String>)> {
    if profile.confirm_destructive {
        let destructive: Vec<(usize, &String)> = classes
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match c {
                StatementClass::Destructive(reason) => Some((i, reason)),
                _ => None,
            })
            .collect();
        if let Some((_, reason)) = destructive.first() {
            let prompt = format!(
                "This will {reason} on {conn_name} ({} profile).",
                profile.name.to_lowercase()
            );
            let listed = destructive.iter().map(|(i, _)| statements[*i]).collect();
            return Some((prompt, lines(listed)));
        }
    }
    if profile.confirm_writes {
        let writes: Vec<&str> = classes
            .iter()
            .zip(statements)
            .filter(|(c, _)| c.modifies())
            .map(|(_, s)| *s)
            .collect();
        let n = writes.len();
        if n > 0 {
            let prompt = if n == 1 {
                format!("Run 1 statement that modifies data on {conn_name}?")
            } else {
                format!("Run {n} statements that modify data on {conn_name}?")
            };
            return Some((prompt, lines(writes)));
        }
    }
    None
}

/// A prompt above muted monospace lines (the statements a confirmation is about).
pub(crate) fn prompt_lines(ui: &mut egui::Ui, prompt: &str, lines: &[String]) {
    ui.set_max_width(560.0);
    ui.label(prompt);
    ui.add_space(Spacing::S);
    let p = theme::palette(ui.ctx());
    for line in lines {
        ui.label(RichText::new(line).monospace().color(p.text_muted));
    }
}

/// A confirmation modal: `body` (which sets the width), then a red `confirm` button, a
/// `dismiss` button and `extra` buttons after them. `Some(true)` once confirmed,
/// `Some(false)` once dismissed (the button, Escape or a click outside), `None` while open.
pub(crate) fn confirm_modal(
    ctx: &egui::Context,
    id: Id,
    confirm: &str,
    dismiss: &str,
    body: impl FnOnce(&mut egui::Ui),
    extra: impl FnOnce(&mut egui::Ui),
) -> Option<bool> {
    let mut answer = None;
    let response = egui::Modal::new(id).show(ctx, |ui| {
        body(ui);
        ui.add_space(Spacing::L);
        ui.horizontal(|ui| {
            let p = theme::palette(ui.ctx());
            let button =
                egui::Button::new(RichText::new(confirm).color(p.accent_text)).fill(p.error);
            if ui.add(button).clicked() {
                answer = Some(true);
            }
            if ui.button(dismiss).clicked() {
                answer = Some(false);
            }
            extra(ui);
        });
    });
    if answer.is_none() && response.should_close() {
        answer = Some(false);
    }
    answer
}

/// The first line of each statement, cut; at most `LISTED`, then how many more.
fn lines(statements: Vec<&str>) -> Vec<String> {
    let mut out: Vec<String> = statements
        .iter()
        .take(LISTED)
        .map(|s| sqlr_ui::first_line(s, LINE_CHARS).into_owned())
        .collect();
    if statements.len() > LISTED {
        out.push(format!("…and {} more", statements.len() - LISTED));
    }
    out
}

impl SqlrApp {
    /// The confirmation a run of `kind` on tab `tab` needs, if the profile asks for one.
    pub(crate) fn needs_confirmation(
        &mut self,
        tab: u64,
        conn: &str,
        kind: &PendingKind,
    ) -> Option<PendingRun> {
        let cfg = self.connection(conn)?.clone();
        let profile = self.profile_for(&cfg);
        let syntax = self.tab(tab)?.editor.syntax().clone();
        let sql: Vec<&str> = match kind {
            PendingKind::Single(t) => vec![t.sql.as_str()],
            PendingKind::Script(ts) => ts.iter().map(|t| t.sql.as_str()).collect(),
            // Redis commands are confirmed where they are issued (`run_redis_command`).
            PendingKind::Redis(_) => return None,
            // So are document commands (`run_doc_command`).
            PendingKind::Doc(_) => return None,
        };
        let (prompt, statements) = confirmation(&profile, &cfg.name, &syntax, &sql)?;
        Some(PendingRun {
            tab,
            conn: conn.to_string(),
            kind: kind.clone(),
            prompt,
            statements,
        })
    }

    /// Draws the pending confirmation; "Run" starts the run.
    pub(crate) fn confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.confirm else {
            return;
        };
        let answer = confirm_modal(
            ctx,
            Id::new("confirm_run"),
            "Run",
            "Cancel",
            |ui| prompt_lines(ui, &pending.prompt, &pending.statements),
            |_| {},
        );
        let Some(run) = answer else {
            return;
        };
        let Some(pending) = self.confirm.take() else {
            return;
        };
        if run {
            match pending.kind {
                PendingKind::Single(target) => {
                    self.start_single(pending.tab, &pending.conn, target)
                }
                PendingKind::Script(targets) => {
                    self.start_script(pending.tab, &pending.conn, targets)
                }
                PendingKind::Redis(command) => self.apply_redis(command),
                PendingKind::Doc(command) => self.apply_doc(pending.tab, command),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn production() -> ConnectionProfile {
        ConnectionProfile::builtins().remove(2)
    }

    #[test]
    fn destructive_first_then_writes() {
        let syntax = SqlSyntax::ansi();
        let (prompt, listed) =
            confirmation(&production(), "db", &syntax, &["select 1", "drop table t"]).unwrap();
        assert_eq!(prompt, "This will DROP TABLE on db (production profile).");
        assert_eq!(listed, ["drop table t"]);
        let (prompt, listed) = confirmation(
            &production(),
            "db",
            &syntax,
            &[
                "insert into t values (1)",
                "select 1",
                "update t set a = 1 where b",
            ],
        )
        .unwrap();
        assert_eq!(prompt, "Run 2 statements that modify data on db?");
        assert_eq!(listed.len(), 2);
        assert!(confirmation(&production(), "db", &syntax, &["select 1"]).is_none());
    }

    #[test]
    fn development_asks_only_before_destruction() {
        let dev = ConnectionProfile::development();
        let syntax = SqlSyntax::ansi();
        assert!(confirmation(&dev, "db", &syntax, &["insert into t values (1)"]).is_none());
        assert!(confirmation(&dev, "db", &syntax, &["truncate t"]).is_some());
        let quiet = ConnectionProfile {
            confirm_destructive: false,
            ..dev
        };
        assert!(confirmation(&quiet, "db", &syntax, &["truncate t"]).is_none());
    }

    #[test]
    fn long_scripts_are_summarized() {
        let many: Vec<String> = (0..12)
            .map(|i| format!("insert into t values ({i})"))
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let (_, listed) = confirmation(&production(), "db", &SqlSyntax::ansi(), &refs).unwrap();
        assert_eq!(listed.len(), LISTED + 1);
        assert_eq!(listed.last().unwrap(), "…and 2 more");
    }
}
