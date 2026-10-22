//! SQL text in object editors: highlighted read-only views (the DDL sub-tab, the preview
//! dialog) and multi-line code fields (view queries, routine and trigger bodies).

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, TextEdit};
use sqlr_core::{LexState, SqlSyntax, Token, script};

use super::{ObjectEditorContext, caption};
use crate::theme::{self, EditorColors, Spacing};

/// `text` laid out in the code font, colored by the lexer of `syntax`.
pub fn highlighted(ctx: &egui::Context, syntax: &SqlSyntax, text: &str) -> LayoutJob {
    let palette = theme::palette(ctx);
    let colors = EditorColors::for_palette(&palette);
    let font = theme::monospace(ctx);
    let mut tokens: Vec<Token> = Vec::new();
    syntax.lex(text, LexState::Normal, &mut tokens);
    let mut job = LayoutJob::default();
    let plain = |color: Color32| TextFormat::simple(font.clone(), color);
    let mut at = 0;
    for t in &tokens {
        if t.start > at {
            job.append(&text[at..t.start], 0.0, plain(colors.plain));
        }
        job.append(&text[t.start..t.end], 0.0, plain(colors.token(t.kind)));
        at = t.end;
    }
    if at < text.len() {
        job.append(&text[at..], 0.0, plain(colors.plain));
    }
    job
}

/// `text` highlighted, selectable and copyable but not editable, in a scroll area.
pub fn sql_view(ui: &mut egui::Ui, id: &str, text: &str, syntax: &SqlSyntax, max_height: f32) {
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let mut job = highlighted(ui.ctx(), syntax, buf.as_str());
        job.wrap.max_width = wrap;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let bg = theme::palette(ui.ctx()).bg_editor;
    egui::Frame::new()
        .fill(bg)
        .inner_margin(Spacing::M)
        .corner_radius(Spacing::RADIUS_S)
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .id_salt(("sql_view", id))
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let mut text = text;
                    crate::text_menu::text_edit(
                        ui,
                        TextEdit::multiline(&mut text)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .frame(egui::Frame::NONE)
                            .layouter(&mut layouter),
                    );
                });
        });
}

/// An editable multi-line code field, highlighted with `syntax`.
pub fn code_editor(
    ui: &mut egui::Ui,
    text: &mut String,
    syntax: &SqlSyntax,
    rows: usize,
    hint: &str,
) -> egui::Response {
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let mut job = highlighted(ui.ctx(), syntax, buf.as_str());
        job.wrap.max_width = wrap;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    crate::text_menu::text_edit(
        ui,
        TextEdit::multiline(text)
            .code_editor()
            .desired_rows(rows)
            .desired_width(f32::INFINITY)
            .hint_text(hint)
            .layouter(&mut layouter),
    )
}

/// The DDL sub-tab: the object's DDL as loaded, then the pending statements.
pub(super) fn ddl_tab(ui: &mut egui::Ui, ctx: &ObjectEditorContext) {
    let half = (ui.available_height() / 2.0 - 40.0).max(80.0);
    caption(ui, "Current DDL");
    match ctx.current_ddl {
        Some(ddl) => sql_view(ui, "current_ddl", ddl, &ctx.syntax, half),
        None => {
            caption(ui, "None: the object does not exist yet");
        }
    }
    ui.add_space(Spacing::M);
    caption(ui, "Pending DDL");
    match &ctx.pending_ddl {
        None => {
            caption(ui, "No pending changes");
        }
        Some(Ok([])) => {
            caption(ui, "No pending changes");
        }
        Some(Ok(statements)) => sql_view(ui, "pending_ddl", &script(statements), &ctx.syntax, half),
        Some(Err(e)) => {
            ui.colored_label(theme::palette(ui.ctx()).error, *e);
        }
    }
}
