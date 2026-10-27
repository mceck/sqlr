//! Index administration of document databases from the explorer: "New ▸ Index" on a
//! MongoDB collection (keys and directions, unique, TTL, partial filter) or on
//! Elasticsearch's Indices folder (an index with settings and mappings JSON), and "Drop…"
//! on a MongoDB index or an Elasticsearch index. The statements are the connection's own
//! (`createIndex`, `PUT /index`, …), run as jobs; creating follows the profile's
//! confirmation, dropping always asks.

use egui::{ComboBox, TextEdit, WidgetInfo, WidgetType};
use sqlr_core::{
    DocTarget, IndexDirection, MongoIndexSpec, Node, NodeKind, NodePath, ObjectKind, QueryLanguage,
    es_create_index, es_drop_index, mongo_create_index, mongo_drop_index,
};
use sqlr_engine::{Engine, JobId, JobOutcome};
use sqlr_ui::NodeMenu;
use sqlr_ui::ToastKind;
use sqlr_ui::theme::{self, Spacing};

use super::DocCommand;
use crate::app::SqlrApp;

/// The explorer menu entries of a document connection's node: index creation and drops.
pub(crate) fn node_menu(engine: &Engine, conn: &str, node: &Node) -> NodeMenu {
    // A read-only explorer session changes no indexes (as for the admin panels).
    let read_only = engine
        .transaction(&sqlr_engine::SessionKey::meta(conn))
        .is_none_or(|t| t.read_only);
    match engine.doc_language(conn) {
        Some(language) => menu_for(language, node, read_only),
        None => NodeMenu::default(),
    }
}

/// The index entries of `node` for a connection speaking `language`; none while the
/// session is read-only.
fn menu_for(language: QueryLanguage, node: &Node, read_only: bool) -> NodeMenu {
    if read_only {
        return NodeMenu::default();
    }
    let segs = node.path.segments();
    let (new, drop) = match language {
        QueryLanguage::MongoShell => (
            // Collections only: not views, not system collections.
            node.kind == NodeKind::Table
                && segs.len() == 3
                && segs.get(1).map(String::as_str) == Some("collections"),
            node.kind == NodeKind::Index,
        ),
        QueryLanguage::EsConsole => (
            node.kind == NodeKind::Folder && segs == ["indices"],
            node.kind == NodeKind::Table && segs.first().map(String::as_str) == Some("indices"),
        ),
        _ => (false, false),
    };
    NodeMenu {
        new: if new {
            vec![ObjectKind::Index]
        } else {
            Vec::new()
        },
        drop,
        ..Default::default()
    }
}

enum Form {
    Mongo {
        target: DocTarget,
        keys: Vec<(String, IndexDirection)>,
        name: String,
        unique: bool,
        ttl: Option<i64>,
        partial: String,
    },
    Es {
        name: String,
        settings: String,
        mappings: String,
    },
}

struct CreateDialog {
    conn: String,
    form: Form,
    refresh: NodePath,
    error: Option<String>,
}

struct DropDialog {
    title: String,
    command: DocCommand,
}

#[derive(Default)]
pub(crate) struct IndexAdmin {
    create: Option<CreateDialog>,
    drop: Option<DropDialog>,
    /// Running statements: job, connection, explorer path to reload, what it does.
    jobs: Vec<(JobId, String, NodePath, String)>,
}

fn labeled_field(ui: &mut egui::Ui, text: &mut String, label: &str, width: f32) {
    let response = ui.add(TextEdit::singleline(text).desired_width(width));
    response.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, label));
}

fn json_field(ui: &mut egui::Ui, text: &mut String, label: &str) {
    ui.label(label);
    let response = ui.add(
        TextEdit::multiline(text)
            .code_editor()
            .desired_rows(4)
            .desired_width(f32::INFINITY),
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, label));
}

impl CreateDialog {
    /// The statement to run, or why the form is not complete.
    fn statement(&self) -> Result<(Option<String>, String), sqlr_core::DocAdminError> {
        match &self.form {
            Form::Mongo {
                target,
                keys,
                name,
                unique,
                ttl,
                partial,
            } => {
                let spec = MongoIndexSpec {
                    keys: keys.clone(),
                    name: name.clone(),
                    unique: *unique,
                    ttl_seconds: *ttl,
                    partial_filter: partial.clone(),
                };
                Ok((target.database.clone(), mongo_create_index(target, &spec)?))
            }
            Form::Es {
                name,
                settings,
                mappings,
            } => Ok((None, es_create_index(name.trim(), settings, mappings)?)),
        }
    }

    /// Draws the form; `Some(true)` on "Create", `Some(false)` on "Cancel".
    fn ui(&mut self, ui: &mut egui::Ui) {
        match &mut self.form {
            Form::Mongo {
                keys,
                name,
                unique,
                ttl,
                partial,
                ..
            } => {
                theme::section_title(ui, "Indexed fields");
                theme::hint(
                    ui,
                    "Add fields in index order and choose ascending or descending for each.",
                );
                ui.add_space(Spacing::M);
                let mut remove = None;
                for (i, (field, direction)) in keys.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        labeled_field(ui, field, &format!("Key {}", i + 1), 200.0);
                        ComboBox::from_id_salt(("index_dir", i))
                            .selected_text(direction.label())
                            .show_ui(ui, |ui| {
                                for d in IndexDirection::ALL {
                                    ui.selectable_value(direction, d, d.label());
                                }
                            });
                        if i > 0 && ui.button("Remove").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    keys.remove(i);
                }
                if ui.button("Add key").clicked() {
                    keys.push((String::new(), IndexDirection::Asc));
                }
                ui.add_space(Spacing::S);
                ui.horizontal(|ui| {
                    ui.label("Name");
                    labeled_field(ui, name, "Index name", 200.0);
                });
                ui.checkbox(unique, "Unique");
                let mut expires = ttl.is_some();
                ui.horizontal(|ui| {
                    ui.checkbox(&mut expires, "Expire documents after (seconds)");
                    if expires {
                        let mut secs = ttl.unwrap_or(3600);
                        ui.add(egui::DragValue::new(&mut secs).range(0..=i64::MAX));
                        *ttl = Some(secs);
                    } else {
                        *ttl = None;
                    }
                });
                json_field(ui, partial, "Partial filter (a document; optional)");
            }
            Form::Es {
                name,
                settings,
                mappings,
            } => {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    labeled_field(ui, name, "Index name", 240.0);
                });
                json_field(ui, settings, "Settings (JSON; optional)");
                json_field(ui, mappings, "Mappings (JSON; optional)");
            }
        }
        if let Some(e) = &self.error {
            ui.colored_label(theme::palette(ui.ctx()).error, e);
        }
    }
}

impl SqlrApp {
    /// "New ▸ Index" on `node` of a document connection.
    pub(crate) fn open_index_dialog(&mut self, conn: &str, node: &Node) {
        let form = match self.engine.doc_language(conn) {
            Some(QueryLanguage::EsConsole) => Form::Es {
                name: String::new(),
                settings: String::new(),
                mappings: String::new(),
            },
            Some(QueryLanguage::MongoShell) => {
                let Some((target, _)) = self.engine.open_doc_target(conn, &node.path, 0) else {
                    return;
                };
                Form::Mongo {
                    target,
                    keys: vec![(String::new(), IndexDirection::Asc)],
                    name: String::new(),
                    unique: false,
                    ttl: None,
                    partial: String::new(),
                }
            }
            _ => return,
        };
        self.index_admin.create = Some(CreateDialog {
            conn: conn.to_string(),
            form,
            refresh: node.path.clone(),
            error: None,
        });
    }

    /// "Drop…" on an index node: asks first, always.
    pub(crate) fn ask_drop_index(&mut self, conn: &str, node: &Node) {
        let parent = NodePath::root();
        let refresh = node.path.segments()[..node.path.segments().len().saturating_sub(1)]
            .iter()
            .fold(parent, |p, s| p.child(s));
        let (title, database, text) = match self.engine.doc_language(conn) {
            Some(QueryLanguage::MongoShell) => {
                let Some((target, _)) = self.engine.open_doc_target(conn, &node.path, 0) else {
                    return;
                };
                (
                    format!("Drop index {} of {}?", node.name, target.collection),
                    target.database.clone(),
                    mongo_drop_index(&target, &node.name),
                )
            }
            Some(QueryLanguage::EsConsole) => match es_drop_index(&node.name) {
                Ok(text) => (
                    format!("Delete index {} and all its documents?", node.name),
                    None,
                    text,
                ),
                Err(e) => {
                    self.notify(e.to_string());
                    return;
                }
            },
            _ => return,
        };
        self.index_admin.drop = Some(DropDialog {
            title,
            command: DocCommand::DropIndex {
                conn: conn.to_string(),
                database,
                text,
                refresh,
            },
        });
    }

    pub(crate) fn track_admin_job(&mut self, job: JobId, conn: String, refresh: NodePath) {
        self.index_admin
            .jobs
            .push((job, conn, refresh, "Index change".into()));
    }

    /// The index dialogs, and the statements they started.
    pub(crate) fn index_admin_ui(&mut self, ctx: &egui::Context) {
        let mut finished = Vec::new();
        let engine = &self.engine;
        self.index_admin.jobs.retain(|(id, conn, path, what)| {
            match engine.job(*id).and_then(|j| j.outcome) {
                Some(outcome) => {
                    finished.push((*id, conn.clone(), path.clone(), what.clone(), outcome));
                    false
                }
                None => engine.job(*id).is_some(),
            }
        });
        for (id, conn, path, what, outcome) in finished {
            self.engine.forget_job(id);
            let (kind, text) = match outcome {
                JobOutcome::Done => {
                    self.engine.refresh(&conn, path);
                    (ToastKind::Success, format!("{what}: done"))
                }
                JobOutcome::Failed(e) => {
                    (ToastKind::Error, format!("{what} failed: {}", e.message))
                }
                _ => (ToastKind::Info, format!("{what}: cancelled")),
            };
            self.notify_as(kind, text);
        }
        if let Some(dialog) = &mut self.index_admin.create {
            let title = match &dialog.form {
                Form::Mongo { target, .. } => format!("New index on {}", target.collection),
                Form::Es { .. } => "New index".to_string(),
            };
            let response = theme::Dialog::new("create_index", theme::icons::KEY, &title)
                .subtitle("Choose the indexed fields and review optional index settings")
                .width(580.0)
                .show(ctx, |d| {
                    d.body(|ui| dialog.ui(ui));
                    d.footer(|f| {
                        let create = f.primary("Create", None);
                        f.cancel();
                        create
                    })
                });
            let answer = if response.inner {
                Some(true)
            } else if response.closed {
                Some(false)
            } else {
                None
            };
            match answer {
                Some(false) => self.index_admin.create = None,
                Some(true) => match dialog.statement() {
                    Err(e) => dialog.error = Some(e.to_string()),
                    Ok((database, text)) => {
                        let command = DocCommand::CreateIndex {
                            conn: dialog.conn.clone(),
                            database,
                            text,
                            refresh: dialog.refresh.clone(),
                        };
                        self.index_admin.create = None;
                        self.run_doc_command(0, command);
                    }
                },
                None => {}
            }
        }
        if let Some(dialog) = &self.index_admin.drop {
            let spec = crate::confirm::ConfirmSpec::new(
                egui::Id::new("drop_index"),
                theme::icons::DROP,
                "Drop index",
                "Drop",
            );
            let answer = crate::confirm::confirm_modal(
                ctx,
                spec,
                |ui| {
                    ui.label(&dialog.title);
                },
                |_| {},
            );
            if let Some(yes) = answer
                && let Some(dialog) = self.index_admin.drop.take()
                && yes
            {
                self.apply_doc(0, dialog.command);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(segs: &[&str], kind: NodeKind) -> Node {
        Node {
            path: segs.iter().fold(NodePath::root(), |p, s| p.child(s)),
            name: segs.last().unwrap().to_string(),
            kind,
            has_children: false,
            detail: None,
        }
    }

    #[test]
    fn new_index_only_on_collections() {
        let mongo =
            |segs: &[&str], kind| menu_for(QueryLanguage::MongoShell, &node(segs, kind), false);
        assert_eq!(
            mongo(&["app", "collections", "users"], NodeKind::Table).new,
            [ObjectKind::Index]
        );
        assert!(mongo(&["app", "views", "v"], NodeKind::View).new.is_empty());
        assert!(
            mongo(&["app", "system", "system.views"], NodeKind::Table)
                .new
                .is_empty(),
            "system collections are not administered"
        );
        assert!(mongo(&["app", "collections", "users", "email_1"], NodeKind::Index).drop);
        let es = |segs: &[&str], kind| menu_for(QueryLanguage::EsConsole, &node(segs, kind), false);
        assert_eq!(es(&["indices"], NodeKind::Folder).new, [ObjectKind::Index]);
        assert!(es(&["indices", "logs"], NodeKind::Table).drop);
        assert!(!es(&["aliases", "logs"], NodeKind::View).drop);
        assert!(!es(&["data_streams", "d"], NodeKind::Table).drop);
    }

    #[test]
    fn read_only_sessions_offer_no_index_changes() {
        let users = node(&["app", "collections", "users"], NodeKind::Table);
        let index = node(&["app", "collections", "users", "email_1"], NodeKind::Index);
        assert_eq!(
            menu_for(QueryLanguage::MongoShell, &users, true),
            NodeMenu::default()
        );
        assert!(!menu_for(QueryLanguage::MongoShell, &index, true).drop);
    }
}
