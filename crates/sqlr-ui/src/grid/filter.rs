//! The server `WHERE` field above the Result grid, with column completion.
//!
//! The list holds the Result's own columns: the server filter wraps the original statement as
//! a subquery, so only the Result's output names are valid there. A word is read with the
//! Dialect's lexer, which also keeps the list out of strings, quoted identifiers and comments.

use std::ops::Range;

use egui::text::{CCursor, CCursorRange};
use egui::{Id, Key, Modifiers, Rect, RichText, TextEdit};
use sqlr_core::{ColumnMeta, LexState, SqlSyntax, TokenKind};

use crate::grid::{GridAction, GridInput, GridState};
use crate::theme::{self, Spacing, icons};

/// Rows shown at once in the completion popup.
const VISIBLE_ROWS: usize = 8;
/// The popup never grows wider than this, even when the field does.
const MAX_WIDTH: f32 = 420.0;
const MIN_WIDTH: f32 = 240.0;

/// One offered column.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    /// The column name as the Result shows it.
    pub label: String,
    /// The identifier to insert, quoted for the Dialect when needed.
    pub insert: String,
    /// The column's type.
    pub detail: String,
}

/// The word at the caret and the columns that match it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Query {
    /// Byte range of the word in the field text.
    pub range: Range<usize>,
    pub prefix: String,
    pub items: Vec<Item>,
}

/// The WHERE field's completion session, kept across frames.
#[derive(Default)]
pub(crate) struct Completion {
    open: bool,
    /// Opened with Ctrl+Space: it stays open with an empty prefix.
    explicit: bool,
    selected: usize,
    /// The word the selection belongs to; a new word resets it.
    prefix: String,
    /// The first visible row.
    top: usize,
    scroll_to_selected: bool,
}

impl Completion {
    fn open(&mut self, explicit: bool) {
        if self.open {
            self.explicit |= explicit;
        } else {
            self.open = true;
            self.explicit = explicit;
            self.selected = 0;
            self.prefix.clear();
            self.top = 0;
            self.scroll_to_selected = true;
        }
    }

    fn close(&mut self) {
        self.open = false;
        self.explicit = false;
        self.selected = 0;
        self.prefix.clear();
        self.top = 0;
        self.scroll_to_selected = false;
    }

    fn move_selection(&mut self, delta: isize, len: usize) {
        if len == 0 {
            return;
        }
        self.selected = self.selected.saturating_add_signed(delta).min(len - 1);
        self.scroll_to_selected = true;
    }
}

/// What a frame of the popup produced.
#[derive(Default)]
struct PopupOutput {
    clicked: Option<usize>,
    clicked_elsewhere: bool,
}

/// The filter bar: the `WHERE` field, its column completion and the run on ↵.
pub(crate) fn bar(ui: &mut egui::Ui, state: &mut GridState, input: &GridInput<'_>) {
    let columns = input.rows.columns();
    let syntax = input
        .dialect
        .map_or_else(SqlSyntax::ansi, |dialect| dialect.syntax());
    let id = ui.make_persistent_id("sqlr_where_filter");
    let GridState {
        where_filter,
        actions,
        filter_completion: completion,
        ..
    } = state;

    // The focus this frame decides whether the completion shortcut belongs to the field.
    let focused = ui.ctx().memory(|memory| memory.has_focus(id));
    if !focused {
        completion.close();
    }

    let before = query(
        &syntax,
        columns,
        where_filter,
        caret_of(ui.ctx(), id, where_filter),
    );
    if completion.open && before.as_ref().is_none_or(|q| q.items.is_empty()) {
        completion.close();
    }

    let mut chosen = None;
    if completion.open {
        // Consumed before the single-line field sees them: it would otherwise move the caret
        // or surrender focus on ↵.
        let (up, down, enter, tab, escape) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Tab),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if escape {
            completion.close();
        } else if let Some(q) = &before {
            if up {
                completion.move_selection(-1, q.items.len());
            }
            if down {
                completion.move_selection(1, q.items.len());
            }
            if enter || tab {
                chosen = q.items.get(completion.selected).cloned();
            }
        }
    }

    // The Complete shortcut opens the list even with an empty word, or closes an open one.
    // Ctrl+Space is a chord of the Keymap, so a user rebinding it keeps working here.
    let complete = focused && {
        let keymap = crate::keymap::ui_keymap(ui.ctx());
        ui.input_mut(|i| keymap.consume(i, sqlr_core::ActionId::Complete))
    };
    if complete {
        if completion.open {
            completion.close();
        } else {
            completion.open(true);
        }
    }

    let field = egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(Spacing::M as i8, Spacing::S as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = Spacing::S;
                icons::paint(ui, icons::FILTER, theme::palette(ui.ctx()).text_muted);
                crate::text_menu::text_edit(
                    ui,
                    TextEdit::singleline(where_filter)
                        .id(id)
                        .hint_text("Filter rows (SQL WHERE), e.g. status = 'paid' ↵")
                        .desired_width(f32::INFINITY),
                )
            })
            .inner
        })
        .inner;

    let run = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    let had_focus = field.has_focus();
    if field.secondary_clicked() {
        completion.close();
    }

    if let Some(item) = chosen {
        apply(ui.ctx(), id, where_filter, &syntax, columns, &item);
        completion.close();
        field.request_focus();
    } else {
        let now = query(
            &syntax,
            columns,
            where_filter,
            caret_of(ui.ctx(), id, where_filter),
        );
        if let Some(q) = &now
            && q.prefix != completion.prefix
        {
            completion.selected = 0;
            completion.prefix = q.prefix.clone();
        }
        if completion.open {
            let close = now.as_ref().is_none_or(|q| q.items.is_empty())
                || now
                    .as_ref()
                    .is_some_and(|q| q.prefix.is_empty() && !completion.explicit);
            if close {
                completion.close();
            }
        } else if had_focus && field.changed() {
            // Typing (or pasting) opens on the word under the caret; an empty word, such as
            // the one after a trailing space, does not.
            if let Some(q) = &now
                && !q.prefix.is_empty()
            {
                completion.open(false);
                completion.prefix = q.prefix.clone();
            }
        }

        if completion.open
            && let Some(q) = &now
        {
            let out = popup(ui, completion, field.rect, &q.items, id.with("completion"));
            if let Some(i) = out.clicked
                && let Some(item) = q.items.get(i)
            {
                apply(ui.ctx(), id, where_filter, &syntax, columns, item);
                completion.close();
                field.request_focus();
            } else if out.clicked_elsewhere {
                completion.close();
            }
        }
    }

    if run {
        actions.push(GridAction::FilterOnServer(where_filter.trim().to_string()));
        completion.close();
    }
}

/// Draws the popup under `field`; returns a clicked item or whether the pointer clicked
/// elsewhere.
fn popup(
    ui: &mut egui::Ui,
    completion: &mut Completion,
    field: Rect,
    items: &[Item],
    id: Id,
) -> PopupOutput {
    let mut out = PopupOutput::default();
    if items.is_empty() {
        return out;
    }
    let palette = theme::palette(ui.ctx());
    let font = theme::monospace(ui.ctx());
    let width = field.width().clamp(MIN_WIDTH, MAX_WIDTH);
    let area = egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(field.left_bottom() + egui::vec2(0.0, 2.0))
        .constrain_to(ui.ctx().content_rect())
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(width);
                ui.set_max_width(MAX_WIDTH);
                let row_h = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
                if completion.scroll_to_selected {
                    if completion.selected < completion.top {
                        completion.top = completion.selected;
                    } else if completion.selected >= completion.top + VISIBLE_ROWS {
                        completion.top = completion.selected + 1 - VISIBLE_ROWS;
                    }
                }
                let mut area = egui::ScrollArea::vertical()
                    .id_salt(id.with("rows"))
                    .max_height(VISIBLE_ROWS as f32 * row_h)
                    .auto_shrink([false, true]);
                if std::mem::take(&mut completion.scroll_to_selected) {
                    area = area.vertical_scroll_offset(completion.top as f32 * row_h);
                }
                let shown = area.show_rows(ui, row_h, items.len(), |ui, rows| {
                    for i in rows {
                        let item = &items[i];
                        let text = (
                            RichText::new(format!("{:<4}", "col"))
                                .font(font.clone())
                                .color(palette.text_muted),
                            RichText::new(&item.label).font(font.clone()),
                            RichText::new(format!("  {}", item.detail))
                                .font(font.clone())
                                .color(palette.text_muted),
                        );
                        let row = ui.add(
                            egui::Button::selectable(i == completion.selected, text)
                                .min_size(egui::vec2(ui.available_width(), 0.0)),
                        );
                        if row.clicked() {
                            out.clicked = Some(i);
                        }
                    }
                });
                completion.top = (shown.state.offset.y / row_h).round() as usize;
            });
        });
    out.clicked_elsewhere = area.response.clicked_elsewhere();
    out
}

/// Replaces the word at the caret with `item` and puts the caret after it.
fn apply(
    ctx: &egui::Context,
    id: Id,
    text: &mut String,
    syntax: &SqlSyntax,
    columns: &[ColumnMeta],
    item: &Item,
) {
    let caret = caret_of(ctx, id, text);
    let Some(q) = query(syntax, columns, text, caret) else {
        return;
    };
    text.replace_range(q.range.clone(), &item.insert);
    let caret = text[..q.range.start].chars().count() + item.insert.chars().count();
    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) {
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(caret))));
        state.store(ctx, id);
    }
}

/// The caret's char index in the field's text, or the end when there is no text state yet.
fn caret_of(ctx: &egui::Context, id: Id, text: &str) -> usize {
    egui::text_edit::TextEditState::load(ctx, id)
        .and_then(|state| state.cursor.char_range())
        .map_or_else(
            || text.chars().count(),
            |range| range.primary.index.0.min(text.chars().count()),
        )
}

/// The word at char `caret` and the columns that match it; `None` inside a string, a quoted
/// identifier or a comment, where a column name is not a column.
pub(crate) fn query(
    syntax: &SqlSyntax,
    columns: &[ColumnMeta],
    text: &str,
    caret: usize,
) -> Option<Query> {
    let caret = caret.min(text.chars().count());
    let caret_byte = char_to_byte(text, caret);
    let before = &text[..caret_byte];
    let mut tokens = Vec::new();
    let end = syntax.lex(before, LexState::Normal, &mut tokens);
    if end.state != LexState::Normal {
        return None;
    }
    // A line comment runs to the caret; a closing quoted identifier is already complete.
    if let Some(last) = tokens.last()
        && last.end == before.len()
        && matches!(last.kind, TokenKind::Comment | TokenKind::QuotedIdent)
    {
        return None;
    }
    let (range, prefix) = match tokens.last() {
        Some(last)
            if last.end == before.len()
                && matches!(
                    last.kind,
                    TokenKind::Ident | TokenKind::Keyword | TokenKind::Function
                ) =>
        {
            (
                last.start..last.end,
                before[last.start..last.end].to_string(),
            )
        }
        _ => (caret_byte..caret_byte, String::new()),
    };
    Some(Query {
        range,
        items: matching(syntax, columns, &prefix),
        prefix,
    })
}

/// The byte offset of char `index`.
fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(byte, _)| byte)
}

/// The columns whose names match `prefix`: prefix matches first, then substring matches,
/// each in Result column order, without repeated names.
fn matching(syntax: &SqlSyntax, columns: &[ColumnMeta], prefix: &str) -> Vec<Item> {
    let mut seen: Vec<&str> = Vec::new();
    let mut starts = Vec::new();
    let mut contains = Vec::new();
    for column in columns {
        let name = column.name.as_str();
        if name.is_empty() || seen.contains(&name) {
            continue;
        }
        seen.push(name);
        let item = Item {
            label: name.to_string(),
            insert: syntax.quote_if_needed(name).into_owned(),
            detail: column.type_name.clone(),
        };
        match rank(name, prefix) {
            Some(0) => starts.push(item),
            Some(1) => contains.push(item),
            _ => {}
        }
    }
    starts.extend(contains);
    starts
}

/// `Some(0)` for a case-insensitive prefix match, `Some(1)` for a substring match.
fn rank(label: &str, prefix: &str) -> Option<u8> {
    if prefix.is_empty() {
        return Some(0);
    }
    if label.is_ascii() && prefix.is_ascii() {
        let (label, prefix) = (label.as_bytes(), prefix.as_bytes());
        if label.len() < prefix.len() {
            return None;
        }
        if label[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return Some(0);
        }
        return label
            .windows(prefix.len())
            .any(|window| window.eq_ignore_ascii_case(prefix))
            .then_some(1);
    }
    let label = label.to_lowercase();
    let prefix = prefix.to_lowercase();
    if label.starts_with(&prefix) {
        Some(0)
    } else {
        label.contains(&prefix).then_some(1)
    }
}

#[cfg(test)]
mod tests {
    use sqlr_core::{ColumnKind, DisplayHint};

    use super::*;

    fn column(name: &str, type_name: &str) -> ColumnMeta {
        ColumnMeta::new(name, type_name, ColumnKind::Text, DisplayHint::Plain)
    }

    fn columns() -> Vec<ColumnMeta> {
        vec![
            column("id", "int8"),
            column("name", "text"),
            column("order id", "int4"),
            column("Order", "int8"),
            column("name", "text"),
        ]
    }

    fn names(q: &Query) -> Vec<&str> {
        q.items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn the_word_at_the_caret_is_replaced() {
        let q = query(&SqlSyntax::ansi(), &columns(), "id > 1 AND na", 13).unwrap();
        assert_eq!(q.prefix, "na");
        assert_eq!(&"id > 1 AND na"[q.range.clone()], "na");
        assert_eq!(names(&q), ["name"]);

        let q = query(&SqlSyntax::ansi(), &columns(), "na", 0).unwrap();
        assert_eq!(q.prefix, "");
        assert_eq!(q.range, 0..0);
    }

    #[test]
    fn literals_quotes_and_comments_have_no_list() {
        let ansi = SqlSyntax::ansi();
        assert!(query(&ansi, &columns(), "name = 'na", 11).is_none());
        assert!(query(&ansi, &columns(), "name = \"na", 11).is_none());
        assert!(query(&ansi, &columns(), "name = 1 -- na", 14).is_none());
        assert!(query(&ansi, &columns(), "/* na", 5).is_none());
        assert!(query(&ansi, &columns(), "name = \"id\"", 11).is_none());
        // After a closed literal, a new word completes normally.
        let q = query(&ansi, &columns(), "name = 'x' AND na", 17).unwrap();
        assert_eq!(q.prefix, "na");
    }

    #[test]
    fn prefix_matches_come_first_then_substrings() {
        let q = query(&SqlSyntax::ansi(), &columns(), "i", 1).unwrap();
        // `id` starts with i; `order id` contains it; duplicate `name` entries are gone.
        assert_eq!(names(&q), ["id", "order id"]);
        let q = query(&SqlSyntax::ansi(), &columns(), "or", 2).unwrap();
        assert_eq!(names(&q), ["order id", "Order"]);
    }

    #[test]
    fn reserved_words_are_quoted() {
        let q = query(&SqlSyntax::ansi(), &[column("order", "int8")], "ord", 3).unwrap();
        assert_eq!(q.items[0].insert, "\"order\"");
    }

    #[test]
    fn names_are_quoted_for_the_dialect() {
        let ansi = SqlSyntax::ansi();
        let q = query(&ansi, &columns(), "ord", 3).unwrap();
        let item = q.items.iter().find(|i| i.label == "order id").unwrap();
        assert_eq!(item.insert, "\"order id\"");
        assert_eq!(item.detail, "int4");
        // Mixed case and reserved words are quoted too.
        let q = query(&ansi, &columns(), "Ord", 3).unwrap();
        let item = q.items.iter().find(|i| i.label == "Order").unwrap();
        assert_eq!(item.insert, "\"Order\"");
        let q = query(&ansi, &columns(), "na", 2).unwrap();
        assert_eq!(q.items[0].insert, "name");
    }
}
