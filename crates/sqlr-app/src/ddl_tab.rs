//! The DDL tab: the `CREATE` statement of an explorer object, read-only, with "Open in new
//! editor tab" and "Copy". The statement is written by the engine (`Engine::load_ddl`).

use egui::RichText;
use sqlr_core::{Node, SqlSyntax};
use sqlr_engine::{Engine, JobId, JobOutcome, SessionKey};
use sqlr_ui::theme::{self, Spacing, icons};
use sqlr_ui::{ToastKind, sql_view};

use crate::app::{SqlrApp, tab_mut};

/// Where the tab's statement is.
#[derive(Clone, Debug, PartialEq)]
pub enum DdlState {
    Loading,
    Ready(String),
    Failed(String),
}

pub struct DdlTab {
    pub conn: String,
    pub node: Node,
    pub state: DdlState,
    /// The engine job writing the statement, until it ends.
    job: Option<JobId>,
    formatted: bool,
}

/// A DDL tab's toolbar action.
#[derive(Clone, Debug, PartialEq)]
pub enum DdlAction {
    OpenInEditor(String),
    Copied,
    Reload,
}

/// Split a table definition at top-level column commas, preserving quoted text and nested
/// type/default expressions through the driver's lexer.
fn formatted_ddl(syntax: &SqlSyntax, text: &str) -> String {
    let text = sqlr_core::format_sql(syntax, text, &sqlr_core::FormatOptions::default());
    if !text
        .trim_start()
        .to_ascii_uppercase()
        .starts_with("CREATE TABLE")
    {
        return text;
    }
    let mut tokens = Vec::new();
    let end = syntax.lex(&text, sqlr_core::LexState::Normal, &mut tokens);
    if end.state != sqlr_core::LexState::Normal {
        return text;
    }
    let mut depth = 0;
    let mut at = 0;
    let mut out = String::new();
    for token in tokens
        .iter()
        .filter(|t| t.kind == sqlr_core::TokenKind::Punct)
    {
        match &text[token.start..token.end] {
            "(" => {
                if depth == 0 {
                    out.push_str(text[..token.start].trim_end());
                    out.push_str(" (\n    ");
                    at = token.end;
                }
                depth += 1;
            }
            "," if depth == 1 => {
                out.push_str(text[at..token.end].trim());
                out.push_str("\n    ");
                at = token.end;
            }
            ")" if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push_str(text[at..token.start].trim());
                    out.push_str("\n)");
                    out.push_str(&text[token.end..]);
                    return out;
                }
            }
            _ => {}
        }
    }
    text
}

impl DdlTab {
    pub fn open(engine: &Engine, conn: &str, node: Node) -> Self {
        let job = engine.load_ddl(&SessionKey::meta(conn), node.clone());
        Self {
            conn: conn.to_string(),
            node,
            state: DdlState::Loading,
            job: Some(job),
            formatted: false,
        }
    }

    pub fn title(&self) -> String {
        format!("DDL · {}", self.node.name)
    }

    /// Writes the statement again (the object may have changed).
    pub fn reload(&mut self, engine: &Engine) {
        if let Some(id) = self.job.take() {
            engine.forget_job(id);
        }
        *self = Self::open(engine, &self.conn, self.node.clone());
    }

    /// Takes the job's outcome once it ended.
    fn poll(&mut self, engine: &Engine) {
        let Some(id) = self.job else {
            return;
        };
        let state = engine.job(id);
        if state.as_ref().is_some_and(|s| s.outcome.is_none()) {
            return;
        }
        self.job = None;
        engine.forget_job(id);
        self.state = match state.and_then(|s| s.outcome) {
            Some(JobOutcome::Ddl(text)) => DdlState::Ready(text),
            Some(JobOutcome::Failed(e)) => DdlState::Failed(e.message),
            _ => DdlState::Failed("The DDL could not be written".into()),
        };
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        engine: &Engine,
        syntax: &SqlSyntax,
    ) -> Option<DdlAction> {
        self.poll(engine);
        if !self.formatted
            && let DdlState::Ready(text) = &mut self.state
        {
            *text = formatted_ddl(syntax, text);
            self.formatted = true;
        }
        let palette = theme::palette(ui.ctx());
        let mut action = None;
        egui::Frame::NONE
            .fill(palette.bg_editor)
            .inner_margin(Spacing::M)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = Spacing::S;
                    ui.label(RichText::new("Read-only").small().color(palette.text_muted));
                    let ready = match &self.state {
                        DdlState::Ready(text) => Some(text.clone()),
                        _ => None,
                    };
                    let enabled = ready.is_some();
                    if ui
                        .add_enabled(enabled, egui::Button::new("Open in new editor tab"))
                        .clicked()
                        && let Some(text) = &ready
                    {
                        action = Some(DdlAction::OpenInEditor(text.clone()));
                    }
                    if ui.add_enabled(enabled, egui::Button::new("Copy")).clicked()
                        && let Some(text) = ready
                    {
                        ui.ctx().copy_text(text);
                        action = Some(DdlAction::Copied);
                    }
                    if icons::icon_button(ui, icons::REFRESH, None, "Reload DDL").clicked() {
                        action = Some(DdlAction::Reload);
                    }
                });
                ui.add_space(Spacing::S);
                match &self.state {
                    DdlState::Loading => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("Writing the DDL…").color(palette.text_muted));
                        });
                    }
                    DdlState::Failed(message) => {
                        ui.colored_label(palette.error, message);
                    }
                    DdlState::Ready(text) => {
                        let height = ui.available_height();
                        let id = format!("ddl_{}_{:?}", self.conn, self.node.path);
                        sql_view(ui, &id, text, syntax, height);
                    }
                }
            });
        action
    }
}

impl SqlrApp {
    /// Opens (or focuses) the DDL tab of `node`.
    pub(crate) fn open_ddl(&mut self, conn: &str, node: &sqlr_core::Node) {
        if let Some(path) = self.dock.find_tab_from(|t| {
            t.ddl
                .as_ref()
                .is_some_and(|d| d.conn == conn && d.node.path == node.path)
        }) {
            let _ = self.dock.set_active_tab(path);
            return;
        }
        let ddl = DdlTab::open(&self.engine, conn, node.clone());
        let key = self.new_tab(Some(ddl.title()), Some(conn.to_string()), "");
        if let Some(tab) = tab_mut(&mut self.dock, key) {
            tab.ddl = Some(Box::new(ddl));
        }
    }

    /// Handles a DDL tab's toolbar action.
    pub(crate) fn ddl_action(&mut self, key: u64, action: DdlAction) {
        match action {
            DdlAction::OpenInEditor(text) => {
                let Some((conn, name)) = self
                    .tab(key)
                    .and_then(|t| t.ddl.as_deref())
                    .map(|d| (d.conn.clone(), d.node.name.clone()))
                else {
                    return;
                };
                self.new_tab(Some(format!("{name}.sql")), Some(conn), &text);
            }
            DdlAction::Copied => self.notify_as(ToastKind::Success, "DDL copied"),
            DdlAction::Reload => {
                let engine = &self.engine;
                if let Some(ddl) = tab_mut(&mut self.dock, key).and_then(|t| t.ddl.as_deref_mut()) {
                    ddl.reload(engine);
                }
            }
        }
    }

    /// The statement of the last DDL tab (in dock order), once written.
    #[doc(hidden)]
    pub fn ddl_text(&self) -> Option<String> {
        self.dock
            .iter_all_tabs()
            .filter_map(|(_, t)| t.ddl.as_deref())
            .last()
            .and_then(|d| match &d.state {
                DdlState::Ready(text) => Some(text.clone()),
                _ => None,
            })
    }
}
