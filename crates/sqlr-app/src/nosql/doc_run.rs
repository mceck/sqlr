//! Document tabs in the app: opening them from the explorer, running their statements and
//! applying their edits, each after the connection profile's confirmation (writes and
//! destructive statements, classified by the driver).

use sqlr_core::{Family, Node, StatementClass};
use sqlr_engine::SessionKey;

use super::{DocCommand, DocTab, doc_editor};
use crate::app::SqlrApp;
use crate::app::tab_mut;
use crate::confirm::{PendingKind, PendingRun, confirmation_for};

impl SqlrApp {
    /// Whether `conn`'s driver is a document database.
    pub(crate) fn is_document(&self, conn: &str) -> bool {
        self.connection(conn)
            .and_then(|c| self.manifest(&c.driver))
            .is_some_and(|m| m.family == Family::Document)
    }

    /// Opens a document tab on `conn` (and `database`) with `text`; runs it when asked.
    pub(crate) fn open_doc_tab(
        &mut self,
        title: String,
        conn: &str,
        database: Option<String>,
        text: &str,
        run: bool,
    ) -> u64 {
        let language = self
            .connection(conn)
            .and_then(|c| self.manifest(&c.driver))
            .map_or(sqlr_core::QueryLanguage::MongoShell, |m| m.language());
        let page = self.settings.page_size;
        let key = self.new_tab(Some(title), Some(conn.to_string()), text);
        if let Some(tab) = self.tab(key) {
            doc_editor(&mut tab.editor, language);
            tab.database = database;
            tab.doc = Some(Box::new(DocTab::new(conn.to_string(), language, page)));
        }
        if run {
            self.run_doc_tab(key, true);
        }
        key
    }

    /// "Open data" on a collection, view or index: its documents, a page at a time.
    pub(crate) fn open_doc_node(&mut self, conn: &str, node: &Node) {
        let page = self.settings.page_size;
        let Some((target, text)) = self.engine.open_doc_target(conn, &node.path, page) else {
            self.notify(format!("Cannot open the documents of {}", node.name));
            return;
        };
        let title = match &target.database {
            Some(db) => format!("{} ({db})", target.collection),
            None => target.collection.clone(),
        };
        self.open_doc_tab(title, conn, target.database, &text, true);
    }

    /// "New query" on a database of a document connection.
    pub(crate) fn new_doc_query(&mut self, conn: &str, database: &str) {
        self.open_doc_tab(
            format!("Query ({database})"),
            conn,
            Some(database.to_string()),
            "",
            false,
        );
    }

    /// Runs the statement at the caret (or, with `script`, every statement) of document tab
    /// `key`.
    pub(crate) fn run_doc_tab(&mut self, key: u64, script: bool) {
        let Some(tab) = self.tab(key) else {
            return;
        };
        let Some(conn) = tab.conn.clone() else {
            return;
        };
        let targets = if script {
            tab.editor.script_targets()
        } else {
            tab.editor.run_target().into_iter().collect()
        };
        let statements: Vec<String> = targets
            .into_iter()
            .map(|t| t.sql.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if statements.is_empty() {
            return;
        }
        self.run_doc_command(key, DocCommand::Run { conn, statements });
    }

    /// Asks the profile's confirmation for `command` when it needs one, else applies it.
    pub(crate) fn run_doc_command(&mut self, tab: u64, command: DocCommand) {
        let conn = command.conn().to_string();
        let Some(cfg) = self.connection(&conn).cloned() else {
            return;
        };
        // An edit reads the result again only when the run only read: a write never runs
        // twice.
        if let DocCommand::Rerun { statements, .. } = &command
            && statements
                .iter()
                .any(|s| self.engine.doc_classify(&conn, s) != StatementClass::Read)
        {
            return;
        }
        let profile = self.profile_for(&cfg);
        let (classes, lines): (Vec<StatementClass>, Vec<String>) = match &command {
            DocCommand::Run { statements, .. } => (
                statements
                    .iter()
                    .map(|s| self.engine.doc_classify(&conn, s))
                    .collect(),
                statements.clone(),
            ),
            DocCommand::Save { target, id, .. } => (
                vec![StatementClass::Write],
                vec![match id {
                    Some(id) => format!("replace {} {}", target.collection, id.to_relaxed_json()),
                    None => format!("insert into {}", target.collection),
                }],
            ),
            DocCommand::CreateIndex { text, .. } => (
                vec![self.engine.doc_classify(&conn, text)],
                vec![text.clone()],
            ),
            // Asked by their own dialogs; a re-read only reads what already ran.
            DocCommand::Delete { .. } | DocCommand::DropIndex { .. } | DocCommand::Rerun { .. } => {
                (Vec::new(), Vec::new())
            }
        };
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        match confirmation_for(&profile, &cfg.name, &classes, &refs) {
            Some((prompt, statements)) => {
                self.confirm = Some(PendingRun {
                    tab,
                    conn,
                    kind: PendingKind::Doc(command),
                    prompt,
                    statements,
                });
            }
            None => self.apply_doc(tab, command),
        }
    }

    /// Applies a confirmed document command.
    pub(crate) fn apply_doc(&mut self, tab: u64, command: DocCommand) {
        match command {
            DocCommand::Run { .. } | DocCommand::Rerun { .. } => {
                let rerun = matches!(command, DocCommand::Rerun { .. });
                let (DocCommand::Run { conn, statements } | DocCommand::Rerun { conn, statements }) =
                    command
                else {
                    unreachable!()
                };
                if !self.is_live(&conn) && !self.connect(&conn) {
                    self.resume_after_secrets(
                        tab,
                        &conn,
                        PendingKind::Doc(DocCommand::Run {
                            conn: conn.clone(),
                            statements,
                        }),
                    );
                    return;
                }
                let session = self.engine.session(&conn, Some(tab));
                let engine = &self.engine;
                let Some(t) = tab_mut(&mut self.dock, tab) else {
                    return;
                };
                let database = t.database.clone();
                let Some(d) = t.doc.as_deref_mut() else {
                    return;
                };
                let results = statements
                    .iter()
                    .map(|s| engine.run_doc(&session, database.as_deref(), s.clone(), d.page_size))
                    .collect();
                d.set_run(engine, statements, results, rerun);
            }
            DocCommand::Save {
                conn,
                target,
                id,
                doc,
            } => {
                let what = match &id {
                    Some(_) => "Save",
                    None => "Insert",
                };
                let session = self.engine.session(&conn, Some(tab));
                let job = self.engine.save_doc(&session, target, id, doc);
                self.track_doc_job(tab, job, what);
            }
            DocCommand::Delete { conn, groups } => {
                let session = self.engine.session(&conn, Some(tab));
                for (target, ids) in groups {
                    let job = self.engine.delete_docs(&session, target, ids);
                    self.track_doc_job(tab, job, "Delete");
                }
            }
            DocCommand::CreateIndex {
                conn,
                database,
                text,
                refresh,
            }
            | DocCommand::DropIndex {
                conn,
                database,
                text,
                refresh,
            } => {
                let job = self
                    .engine
                    .doc_admin(&SessionKey::meta(&conn), database, text);
                self.track_admin_job(job, conn, refresh);
            }
        }
    }

    /// Adds `job` to the tab's pending action (a save or a delete): the result is read
    /// again once all the action's jobs worked.
    fn track_doc_job(&mut self, tab: u64, job: sqlr_engine::JobId, what: &str) {
        if let Some(d) = tab_mut(&mut self.dock, tab).and_then(|t| t.doc.as_deref_mut()) {
            let (action, result) = d.pending_action.unwrap_or((0, None));
            d.jobs.push(super::doc_tab::DocJob {
                id: job,
                what: what.to_string(),
                action,
                result,
            });
        }
    }
}
