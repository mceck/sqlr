//! The completion popup: when it opens, what it lists and how it is drawn.
//!
//! The caret context and the statement scope are computed once per (text revision, primary
//! caret) and cached, so an open popup costs one [`candidates`] call per frame. The context
//! reads at most [`CONTEXT_CLIP_BYTES`] before the caret. The scope needs the statement: the
//! UI thread splits the caret's region (the lines between `;` lines) only when it is at most
//! [`STRUCTURE_SYNC_LIMIT`] bytes. A larger region, or a caret line over that size, goes to
//! the [`ScopeWorker`] as a window around the caret, which the worker splits and parses. The
//! previous scope stays in use until the worker's result arrives.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::time::Instant;

use egui::{Area, Button, FontId, Frame, Id, Order, Pos2, RichText, ScrollArea};
use sqlr_core::{CatalogNeed, LexState, Token, TokenKind};

use crate::editor::buffer::TextBuffer;
use crate::editor::complete::{
    CONTEXT_CLIP_BYTES, CaretContext, CompletionItem, CompletionSource, ContextKind, ItemKind,
    analyze_context, candidates, relation_alias,
};
use crate::editor::model::EditorModel;
use crate::editor::scope::{STRUCTURE_SYNC_LIMIT, ScopeWorker, StatementScope, statement_scope};
use crate::editor::syntax::SyntaxLayer;
use crate::theme;

/// Bytes on each side of the caret that the worker gets of a huge statement.
pub(crate) const SCOPE_WINDOW_BYTES: usize = 32 * 1024;

/// The part of the bytes `span` (a statement, or a line holding one) to parse for a caret at
/// byte `caret`: at most [`SCOPE_WINDOW_BYTES`] each side of it, starting at a point where
/// lexing resumes with a known state (returned), so the worker can split the window and
/// parse the caret's statement. A window cuts a huge statement, so its scope can miss
/// relations outside it; that only narrows the candidates.
pub(crate) fn scope_window(
    buf: &TextBuffer,
    layer: &SyntaxLayer,
    span: Range<usize>,
    caret: usize,
) -> (Range<usize>, LexState) {
    let rope = buf.rope();
    let snap = |b: usize| rope.char_to_byte(rope.byte_to_char(b));
    let from = snap(caret.saturating_sub(SCOPE_WINDOW_BYTES).max(span.start));
    let end = snap((caret + SCOPE_WINDOW_BYTES).min(span.end));
    let (start, state) =
        if from == span.start && span.start == rope.line_to_byte(rope.byte_to_line(from)) {
            (from, layer.line_state(rope.byte_to_line(from)))
        } else {
            match layer.resume_point(buf, from) {
                Some((s, st)) => (s, Some(st)),
                None => (from, None),
            }
        };
    (start..end.max(start), state.unwrap_or(LexState::Normal))
}

/// Rows shown at once.
const VISIBLE_ROWS: usize = 10;

/// The caret context at one (revision, caret).
#[derive(Clone, Debug)]
pub(crate) struct Analysis {
    key: (u64, usize),
    pub(crate) ctx: CaretContext,
    alias_allowed: bool,
}

impl Analysis {
    /// The prefix length the popup needs before it opens on typing.
    pub(crate) fn opens_on_typing(&self, typed_dot: bool) -> bool {
        let n = self.ctx.prefix.chars().count();
        match self.ctx.kind {
            // `.` opens only on a member: after a number or an operator it is no qualifier.
            ContextKind::Member { .. } => typed_dot || n >= 1,
            _ if typed_dot => false,
            ContextKind::Off => false,
            ContextKind::General => n >= 2,
            ContextKind::Relation { .. } => n >= 1,
        }
    }
}

#[derive(Default)]
pub(crate) struct CompletionPopup {
    open: bool,
    /// The session was opened with Ctrl+Space: it survives an empty prefix.
    explicit: bool,
    selected: usize,
    /// The first visible row.
    top: usize,
    scroll_to_selected: bool,
    items: Vec<CompletionItem>,
    loading: bool,
    /// Needs already emitted in this popup session.
    emitted: Vec<CatalogNeed>,
    analysis: Option<Analysis>,
    /// Where the popup was drawn in the last frame.
    rect: Option<egui::Rect>,
    scope: StatementScope,
    scope_key: Option<u64>,
}

impl CompletionPopup {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Opens a session; `explicit` when the user asked for it (Ctrl+Space).
    pub(crate) fn open(&mut self, explicit: bool) {
        if self.open {
            self.explicit |= explicit;
        } else {
            self.open = true;
            self.explicit = explicit;
            self.selected = 0;
            self.top = 0;
            self.items.clear();
            self.loading = false;
            self.emitted.clear();
        }
    }

    /// Whether `pos` is over the popup drawn in the last frame. Clicking a row moves focus
    /// to it, so the editor keeps the popup open while the pointer is there.
    pub(crate) fn contains(&self, pos: Option<Pos2>) -> bool {
        self.open && pos.is_some_and(|p| self.rect.is_some_and(|r| r.contains(p)))
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.rect = None;
        self.items.clear();
        self.loading = false;
    }

    /// Drops the cached analysis (the syntax changed).
    pub(crate) fn invalidate(&mut self) {
        self.analysis = None;
        self.scope_key = None;
        self.scope = StatementScope::default();
    }

    pub(crate) fn reset_selection(&mut self) {
        self.selected = 0;
        self.scroll_to_selected = true;
    }

    pub(crate) fn labels(&self) -> Vec<String> {
        self.items.iter().map(|i| i.label.clone()).collect()
    }

    pub(crate) fn selected_item(&self) -> Option<CompletionItem> {
        self.items.get(self.selected).cloned()
    }

    pub(crate) fn item(&self, i: usize) -> Option<CompletionItem> {
        self.items.get(i).cloned()
    }

    pub(crate) fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
        self.scroll_to_selected = true;
    }

    /// The caret context at the primary caret, cached per (revision, caret).
    pub(crate) fn analysis(&mut self, model: &EditorModel, layer: &mut SyntaxLayer) -> &Analysis {
        let buf = model.buffer();
        let caret = buf.char_index(model.primary().head);
        let key = (model.revision(), caret);
        if self.analysis.as_ref().is_none_or(|a| a.key != key) {
            let (ctx, alias_allowed) = context_at(model, layer, caret);
            self.analysis = Some(Analysis {
                key,
                ctx,
                alias_allowed,
            });
        }
        self.analysis.as_ref().expect("set above")
    }

    /// Refreshes the items of an open popup; returns the needs not yet emitted in this
    /// session. `scope_parse` gets the time of a synchronous scope parse.
    pub(crate) fn update(
        &mut self,
        model: &EditorModel,
        layer: &mut SyntaxLayer,
        worker: &ScopeWorker,
        source: &dyn CompletionSource,
        scope_parse: &mut Option<std::time::Duration>,
    ) -> Vec<CatalogNeed> {
        if !self.open {
            return Vec::new();
        }
        let a = self.analysis(model, layer).clone();
        let emptied =
            !self.explicit && a.ctx.prefix.is_empty() && a.ctx.kind == ContextKind::General;
        if a.ctx.kind == ContextKind::Off || emptied {
            // An implicit popup closes once its prefix is gone (e.g. backspaced away).
            self.close();
            return Vec::new();
        }
        let key = {
            let mut h = DefaultHasher::new();
            a.key.hash(&mut h);
            h.finish()
        };
        if self.scope_key != Some(key) {
            self.scope_key = Some(key);
            self.request_scope(model, layer, worker, key, a.key.1, scope_parse);
        }
        if let Some((k, scope)) = worker.poll()
            && Some(k) == self.scope_key
        {
            self.scope = scope;
        }
        let found = candidates(&a.ctx, &self.scope, layer.syntax(), source);
        self.items = found.items;
        if a.alias_allowed {
            for item in &mut self.items {
                if matches!(item.kind, ItemKind::Table | ItemKind::View) {
                    let alias = relation_alias(&item.label, &self.scope, layer.syntax());
                    item.insert.push_str(&format!(" AS {alias}"));
                    item.detail.push_str(&format!(" AS {alias}"));
                }
            }
        }
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        self.loading = self.items.is_empty() && !found.needs.is_empty();
        if self.items.is_empty() && found.needs.is_empty() {
            self.close();
        }
        let fresh: Vec<CatalogNeed> = found
            .needs
            .into_iter()
            .filter(|n| !self.emitted.contains(n))
            .collect();
        self.emitted.extend(fresh.iter().cloned());
        fresh
    }

    /// Computes the scope at char `caret`, or asks the worker for it.
    fn request_scope(
        &mut self,
        model: &EditorModel,
        layer: &mut SyntaxLayer,
        worker: &ScopeWorker,
        key: u64,
        caret: usize,
        scope_parse: &mut Option<std::time::Duration>,
    ) {
        let buf = model.buffer();
        let rope = buf.rope();
        let syntax = layer.syntax().clone();
        let caret_byte = rope.char_to_byte(caret);
        let line = rope.char_to_line(caret);
        if buf.line_len_bytes(line) > STRUCTURE_SYNC_LIMIT {
            // A huge line: the worker gets a window around the caret and splits it
            // (statements spanning lines are cut at its start, which only narrows the scope).
            // Only the rope handle is cloned here.
            let start = rope.line_to_byte(line);
            let span = start..start + buf.line_len_bytes(line);
            let (range, state) = scope_window(buf, layer, span, caret_byte);
            worker.request_rope(key, syntax, rope.clone(), range, caret_byte, Some(state));
            return;
        }
        match layer.statement_within(buf, caret, STRUCTURE_SYNC_LIMIT) {
            Ok(None) => self.scope = StatementScope::default(),
            Ok(Some(r)) => {
                // Inside a region of at most the limit, so it is parsed here.
                let (sb, eb) = (rope.char_to_byte(r.start), rope.char_to_byte(r.end));
                let text = rope.slice(r).to_string();
                let t = Instant::now();
                self.scope = statement_scope(&syntax, &text, caret_byte.clamp(sb, eb) - sb);
                *scope_parse = Some(t.elapsed());
            }
            Err(region) => {
                // A huge region, such as one statement over many lines: as for a huge line,
                // the worker gets a window around the caret and splits it.
                let (range, state) = scope_window(buf, layer, region, caret_byte);
                worker.request_rope(key, syntax, rope.clone(), range, caret_byte, Some(state));
            }
        }
    }

    /// Draws the popup with its top left at `pos`; returns the index of a clicked item.
    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        pos: Pos2,
        font: &FontId,
    ) -> Option<usize> {
        if !self.open {
            return None;
        }
        let mut clicked = None;
        let palette = theme::palette(ctx);
        let badge_color = theme::EditorColors::for_palette(&palette).keyword;
        let shown = Area::new(id)
            .order(Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(280.0);
                    if self.loading {
                        ui.label("Loading…");
                        return;
                    }
                    let row_h = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
                    if self.scroll_to_selected {
                        if self.selected < self.top {
                            self.top = self.selected;
                        } else if self.selected >= self.top + VISIBLE_ROWS {
                            self.top = self.selected + 1 - VISIBLE_ROWS;
                        }
                    }
                    let mut area = ScrollArea::vertical()
                        .id_salt(id.with("rows"))
                        .max_height(VISIBLE_ROWS as f32 * row_h)
                        .auto_shrink([false, true]);
                    if std::mem::take(&mut self.scroll_to_selected) {
                        area = area.vertical_scroll_offset(self.top as f32 * row_h);
                    }
                    let out = area.show_rows(ui, row_h, self.items.len(), |ui, rows| {
                        for i in rows {
                            let item = &self.items[i];
                            let text = (
                                RichText::new(format!("{:<5}", badge(item.kind)))
                                    .font(font.clone())
                                    .color(badge_color),
                                RichText::new(&item.label).font(font.clone()),
                                RichText::new(format!("  {}", item.detail))
                                    .font(font.clone())
                                    .color(palette.text_muted),
                            );
                            let row = ui.add(
                                Button::selectable(i == self.selected, text)
                                    .min_size(egui::vec2(ui.available_width(), 0.0)),
                            );
                            if row.clicked() {
                                clicked = Some(i);
                            }
                        }
                    });
                    self.top = (out.state.offset.y / row_h).round() as usize;
                });
            });
        self.rect = Some(shown.response.rect);
        clicked
    }
}

fn badge(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Column => "col",
        ItemKind::Cte => "cte",
        ItemKind::Table => "table",
        ItemKind::View => "view",
        ItemKind::Schema => "sch",
        ItemKind::Function => "fn",
        ItemKind::Keyword => "kw",
    }
}

/// The caret context at char `caret`: the text before it, clipped to [`CONTEXT_CLIP_BYTES`]
/// at a line start (or, in a long line, at a token start), lexed from the state known there.
/// It reads only the clipped text, never a whole long line.
fn context_at(model: &EditorModel, layer: &SyntaxLayer, caret: usize) -> (CaretContext, bool) {
    let buf = model.buffer();
    let rope = buf.rope();
    let caret_byte = rope.char_to_byte(caret);
    let lo = caret_byte.saturating_sub(CONTEXT_CLIP_BYTES);
    let caret_line = rope.char_to_line(caret);
    let off = CaretContext {
        prefix: String::new(),
        kind: ContextKind::Off,
    };
    let (from, state) = {
        let l0 = rope.byte_to_line(lo);
        // The clip line's start state; unknown past the lexing frontier, where nothing is
        // offered rather than guessed.
        let known = |line: usize| layer.line_state(line);
        if rope.line_to_byte(l0) == lo {
            let Some(st) = known(l0) else {
                return (off, false);
            };
            (lo, st)
        } else if l0 < caret_line {
            let Some(st) = known(l0 + 1) else {
                return (off, false);
            };
            (rope.line_to_byte(l0 + 1), st)
        } else {
            // The caret's own line is long: cut at a token start inside the window.
            let line_start = rope.line_to_byte(caret_line);
            let (w0, w1) = (lo - line_start, caret_byte - line_start);
            let mut tokens: Vec<Token> = Vec::new();
            if !layer.tokens(buf, caret_line, w0..w1, &mut tokens) {
                return (off, false);
            }
            if let Some(t) = tokens.first()
                && t.start < w0
                && t.end >= w1
                && matches!(
                    t.kind,
                    TokenKind::String | TokenKind::Comment | TokenKind::QuotedIdent
                )
            {
                return (off, false);
            }
            let cut = tokens
                .iter()
                .find(|t| t.start >= w0)
                .map_or(w1, |t| t.start);
            (line_start + cut, LexState::Normal)
        }
    };
    let before: String = rope.byte_slice(from..caret_byte).into();
    let (ctx, mut alias_allowed) = analyze_context(layer.syntax(), &before, state);
    if alias_allowed {
        // Bounded lookahead preserves a user-written alias and table/column suffixes.
        let end = (caret + CONTEXT_CLIP_BYTES).min(rope.len_chars());
        let after = rope.slice(caret..end).to_string();
        let mut tokens = Vec::new();
        layer.syntax().lex(&after, LexState::Normal, &mut tokens);
        if let Some(t) = tokens.iter().find(|t| t.kind != TokenKind::Comment) {
            alias_allowed = !matches!(t.kind, TokenKind::Ident | TokenKind::QuotedIdent)
                && !after[t.start..t.end].eq_ignore_ascii_case("AS")
                && !matches!(&after[t.start..t.end], "." | "(");
        } else if end < rope.len_chars() {
            alias_allowed = false;
        }
    }
    (ctx, alias_allowed)
}

#[cfg(test)]
mod tests {
    use sqlr_core::SqlSyntax;

    use super::*;
    use crate::editor::buffer::Pos;
    use crate::editor::model::Caret;
    use crate::editor::syntax::CHECKPOINT_BYTES;

    fn setup(text: &str, caret: Pos) -> (EditorModel, SyntaxLayer) {
        let mut model = EditorModel::new(text);
        model.set_carets(vec![Caret::at(caret)]);
        let layer = SyntaxLayer::new(SqlSyntax::ansi(), model.buffer());
        (model, layer)
    }

    #[test]
    fn unknown_lex_state_is_off() {
        // The clip (4 KB before the caret) starts on a line past the frontier.
        let text = "select 1;\n".repeat(1000) + "select * from us";
        let (model, mut layer) = setup(
            &text,
            Pos {
                line: 1000,
                col: 16,
            },
        );
        let caret = model.buffer().char_index(model.primary().head);
        assert_eq!(context_at(&model, &layer, caret).0.kind, ContextKind::Off);
        layer.ensure(model.buffer(), 1000, None);
        assert_eq!(
            context_at(&model, &layer, caret).0,
            CaretContext {
                prefix: "us".into(),
                kind: ContextKind::Relation { schema: None }
            }
        );

        // A long caret line whose tokens aren't available yet.
        let long = "select 1;\n".to_string() + &"x + ".repeat(2000) + "1 from us";
        let col = long.lines().nth(1).map_or(0, str::len);
        let (model, mut layer) = setup(&long, Pos { line: 1, col });
        let caret = model.buffer().char_index(model.primary().head);
        assert_eq!(context_at(&model, &layer, caret).0.kind, ContextKind::Off);
        layer.ensure(model.buffer(), 1, None);
        assert_eq!(
            context_at(&model, &layer, caret).0.kind,
            ContextKind::Relation { schema: None }
        );
    }

    /// A single 1 MB statement on one line, with the caret `col` bytes in.
    fn huge_line(col: usize) -> (EditorModel, SyntaxLayer) {
        let mut text = String::from("INSERT INTO events (id, note) VALUES ");
        while text.len() < 1_000_000 {
            text.push_str("(1, 'plain text'),");
        }
        text.push_str("(2, 'x');");
        let (model, mut layer) = setup(&text, Pos { line: 0, col });
        layer.ensure(model.buffer(), 0, None);
        (model, layer)
    }

    #[test]
    fn a_huge_statement_hands_the_worker_a_window_around_the_caret() {
        let (model, mut layer) = huge_line(500_000);
        let worker = ScopeWorker::new(None);
        let mut popup = CompletionPopup::default();
        let caret = model.buffer().char_index(model.primary().head);
        popup.request_scope(&model, &mut layer, &worker, 1, caret, &mut None);
        let (requests, bytes) = worker.request_stats();
        assert_eq!(requests, 1);
        let bound = 2 * SCOPE_WINDOW_BYTES + 2 * CHECKPOINT_BYTES;
        assert!(
            bytes as usize <= bound,
            "{bytes} bytes handed to the worker"
        );
        assert!(bytes > 0);
    }

    #[test]
    fn a_huge_multi_line_statement_is_not_copied_on_the_ui_thread() {
        // One 2 MB `INSERT … VALUES` with a tuple per line and a single trailing `;`: every
        // caret line is short, but the statement is not.
        let mut text = String::from("select 1;\nINSERT INTO events (id, note) VALUES\n");
        while text.len() < 2_000_000 {
            text.push_str("(1, 'plain text'),\n");
        }
        text.push_str("(2, 'x');\nselect 2;\n");
        let line = text.lines().count() / 2;
        let (model, mut layer) = setup(&text, Pos { line, col: 18 });
        layer.ensure(model.buffer(), line, None);
        let worker = ScopeWorker::new(None);
        let mut popup = CompletionPopup::default();
        let caret = model.buffer().char_index(model.primary().head);
        popup.request_scope(&model, &mut layer, &worker, 1, caret, &mut None);
        assert!(
            layer.split_bytes() as usize <= STRUCTURE_SYNC_LIMIT,
            "{} bytes copied and split on the UI thread",
            layer.split_bytes()
        );
        // Line states past the caret were lexed only near it.
        assert!(
            layer.valid_lines() < line + STRUCTURE_SYNC_LIMIT / 10,
            "lexed up to line {} of {}",
            layer.valid_lines(),
            model.buffer().len_lines()
        );
        let (requests, bytes) = worker.request_stats();
        assert_eq!(requests, 1);
        assert!(bytes as usize <= 2 * SCOPE_WINDOW_BYTES + 2 * CHECKPOINT_BYTES);
    }

    #[test]
    fn a_window_keeps_the_caret_and_resumes_in_the_right_state() {
        // The caret sits in a string literal far into the line.
        let (model, layer) = huge_line(500_000);
        let buf = model.buffer();
        let caret = 500_000;
        let (range, state) = scope_window(buf, &layer, 0..buf.rope().len_bytes(), caret);
        assert!(range.start <= caret && caret <= range.end);
        assert!(range.len() <= 2 * SCOPE_WINDOW_BYTES + 2 * CHECKPOINT_BYTES);
        // The text before the window starts holds balanced literals, so lexing resumes Normal.
        assert_eq!(state, LexState::Normal);
        // A small statement is not clipped.
        let (model, layer) = setup("select 1 from t", Pos { line: 0, col: 3 });
        let n = model.buffer().rope().len_bytes();
        assert_eq!(scope_window(model.buffer(), &layer, 0..n, 3).0, 0..n);
    }
}
