//! The `QueryEditor` widget: a virtualized viewport over [`EditorModel`], highlighted from
//! the [`SyntaxLayer`] through cached line galleys, with multi-caret input, IME, find and
//! replace, error markers and the completion popup.
//!
//! Only the visible lines (plus 2 of overscan) are laid out and painted. A line longer than
//! [`LONG_LINE_BYTES`] is laid out over its visible window ± 256 chars only, and positions in
//! it assume a monospace advance (proportional glyphs, such as CJK or
//! emoji, drift there; documented, not fixed).

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use egui::output::IMEOutput;
use egui::text::CCursor;
use egui::{
    Event, EventFilter, FontId, Galley, IMEPurpose, ImeEvent, Key, Modifiers, PointerButton, Pos2,
    Rect, Response, ScrollArea, Sense, Shape, Stroke, Ui, WidgetInfo, WidgetType, pos2, vec2,
};
use sqlr_core::{CatalogNeed, QueryLanguage, SqlSyntax, Token};

use crate::editor::buffer::{Change, Pos};
use crate::editor::complete::CompletionSource;
use crate::editor::find::matches_in_line;
use crate::editor::find_bar::{FindAction, FindBar};
use crate::editor::input::{Action, key_action};
use crate::editor::layout::{LAYOUT_CACHE_CAPACITY, LineLayoutCache};
use crate::editor::model::{Caret, EditCommand, EditorModel, Motion};
use crate::editor::popup::CompletionPopup;
use crate::editor::scope::ScopeWorker;
use crate::editor::syntax::{LEX_BUDGET, LONG_LINE_BYTES, SyntaxLayer};
use crate::editor::wrap::WrapIndex;
use crate::theme::{self, EditorColors, Palette};

/// Chars laid out on each side of the visible part of a long line.
const WINDOW_PAD_CHARS: usize = 256;
/// A long line's window starts at a multiple of this, so small scrolls hit the cache.
const WINDOW_QUANTUM: usize = 128;
/// Lines painted below the viewport.
const OVERSCAN_LINES: usize = 2;
/// Chars scanned back from a caret for the word a completion replaces.
const MAX_PREFIX_CHARS: usize = 256;
/// Bytes around the caret the tint scans for the caret's statement; a larger statement is
/// left untinted rather than copied every frame.
const TINT_STATEMENT_BYTES: usize = 64 * 1024;

/// SQL to run, and where it starts in the buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRequest {
    pub sql: String,
    /// Buffer char offset of `sql`'s first char.
    pub origin: usize,
}

/// An error position to underline, as a buffer char offset.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub at: usize,
    pub message: String,
}

/// What the last frame did. The counters are per frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    pub visible_lines: usize,
    pub lexed_lines: u64,
    pub galley_hits: u64,
    pub galley_misses: u64,
    pub painted_markers: usize,
    /// The time of a synchronous statement-scope parse in this frame.
    pub scope_parse: Option<Duration>,
}

pub struct EditorOutput {
    pub response: Response,
    /// The text changed in this frame.
    pub changed: bool,
    /// Catalog pieces completion wants loaded (each at most once per popup session).
    pub needs: Vec<CatalogNeed>,
    /// "Save as snippet…" was chosen from the context menu.
    pub save_snippet: bool,
}

/// A laid-out visible line of the last frame.
struct LineGeom {
    line: usize,
    galley: Arc<Galley>,
    /// The first char the galley holds (non-zero only for a long line's window).
    c0: usize,
    long: bool,
    /// The absolute visual row this line starts at (its buffer line number when not wrapped).
    first_row: usize,
}

struct Geometry {
    /// Screen position of the top left of line 0's text.
    text_origin: Pos2,
    row_height: f32,
    char_width: f32,
    /// The visible part of the widget, on screen.
    view: Rect,
    /// Visible lines, contiguous from `lines[0].line`.
    lines: Vec<LineGeom>,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            text_origin: Pos2::ZERO,
            row_height: 0.0,
            char_width: 0.0,
            view: Rect::NOTHING,
            lines: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompletionOptions {
    pub auto_open: bool,
    pub delay_ms: u64,
    pub insert_on_tab: bool,
    pub keyword_case: sqlr_core::Case,
}
impl Default for CompletionOptions {
    fn default() -> Self {
        Self {
            auto_open: true,
            delay_ms: 0,
            insert_on_tab: true,
            keyword_case: sqlr_core::Case::Preserve,
        }
    }
}

pub struct EditorState {
    model: EditorModel,
    syntax: SyntaxLayer,
    cache: LineLayoutCache,
    popup: CompletionPopup,
    find: FindBar,
    markers: Vec<Marker>,
    /// Per marker, the char range of its token (as last seen), which edits shift or drop.
    marker_tokens: Vec<Range<usize>>,
    preedit: String,
    /// Created on the first frame, so it can request repaints.
    worker: Option<ScopeWorker>,
    focus_requested: bool,
    focused: bool,
    /// The (revision, carets) last seen and when they last changed, to restart the blink.
    blink_key: (u64, Vec<Caret>),
    blink_start: f64,
    geom: Geometry,
    max_width: f32,
    dragging: bool,
    scroll_to_caret: bool,
    marker_spans: Vec<Range<usize>>,
    stats: FrameStats,
    scratch: Vec<Token>,
    /// Whether Ctrl+Space and typed prefixes open the completion popup.
    completion: bool,
    /// Whether the context menu offers "Save as snippet…".
    snippet_menu: bool,
    completion_options: CompletionOptions,
    completion_pending: Option<(f64, bool)>,
    format_options: sqlr_core::FormatOptions,
    /// Input time of the last frame (seconds), for undo grouping outside a frame.
    now: f64,
    /// This frame's theme.
    palette: Palette,
    colors: EditorColors,
    /// Soft wrap on: long logical lines break into several visual rows.
    soft_wrap: bool,
    /// The gutter with line numbers.
    line_numbers: bool,
    /// A subtle background behind the statement holding the primary caret.
    statement_tint: bool,
    /// Lines of that statement, as of the last frame.
    tint: Option<Range<usize>>,
    /// Visual rows per line at the current width, when wrapping.
    wrap: WrapIndex,
    /// Edits since the last frame, for the wrap index to splice in.
    wrap_changes: Vec<Change>,
}

impl EditorState {
    pub fn new(text: &str, syntax: Arc<SqlSyntax>) -> Self {
        crate::editor::scope::warm_up();
        let mut model = EditorModel::new(text);
        model.set_line_comment(syntax.line_comment);
        let layer = SyntaxLayer::new(syntax, model.buffer());
        Self {
            model,
            syntax: layer,
            cache: LineLayoutCache::new(LAYOUT_CACHE_CAPACITY),
            popup: CompletionPopup::default(),
            find: FindBar::default(),
            markers: Vec::new(),
            marker_tokens: Vec::new(),
            preedit: String::new(),
            worker: None,
            focus_requested: false,
            focused: false,
            blink_key: (0, Vec::new()),
            blink_start: 0.0,
            geom: Geometry::default(),
            max_width: 0.0,
            dragging: false,
            scroll_to_caret: false,
            marker_spans: Vec::new(),
            stats: FrameStats::default(),
            scratch: Vec::new(),
            completion: true,
            snippet_menu: false,
            completion_options: Default::default(),
            completion_pending: None,
            format_options: Default::default(),
            now: 0.0,
            palette: Palette::dark(),
            colors: EditorColors::dark(),
            soft_wrap: false,
            line_numbers: true,
            statement_tint: true,
            tint: None,
            wrap: WrapIndex::default(),
            wrap_changes: Vec::new(),
        }
    }

    /// Enables or disables the completion popup. The JSON value editor turns it off: its
    /// grammar has no names to complete.
    pub fn set_completion_options(&mut self, options: CompletionOptions) {
        self.completion_options = options;
    }

    pub fn set_completion(&mut self, on: bool) {
        self.completion = on;
        if !on {
            self.popup.close();
        }
    }

    /// Whether the context menu offers "Save as snippet…".
    pub fn set_snippet_menu(&mut self, on: bool) {
        self.snippet_menu = on;
    }

    /// Wraps long logical lines at the editor's width instead of scrolling horizontally.
    pub fn set_soft_wrap(&mut self, on: bool) {
        if self.soft_wrap != on {
            self.soft_wrap = on;
            // Rows are per width, so a toggle always rebuilds the index.
            self.wrap = WrapIndex::default();
            self.wrap_changes.clear();
        }
    }

    pub fn soft_wrap(&self) -> bool {
        self.soft_wrap
    }

    /// Shows the line numbers in the gutter.
    pub fn set_line_numbers(&mut self, on: bool) {
        self.line_numbers = on;
    }

    pub fn line_numbers(&self) -> bool {
        self.line_numbers
    }

    /// Tints the statement holding the primary caret.
    pub fn set_statement_tint(&mut self, on: bool) {
        self.statement_tint = on;
        if !on {
            self.tint = None;
        }
    }

    /// The lines of the statement holding the primary caret, as of the last frame.
    pub fn tint_lines(&self) -> Option<Range<usize>> {
        self.tint.clone()
    }

    /// The visual rows `line` takes at the current width (one when not wrapping).
    pub fn wrapped_rows(&self, line: usize) -> Option<usize> {
        if line >= self.model.buffer().len_lines() {
            return None;
        }
        if !self.soft_wrap {
            return Some(1);
        }
        Some(self.wrap.rows(line).unwrap_or(1))
    }

    pub fn text(&self) -> String {
        self.model.buffer().rope().to_string()
    }

    pub fn model(&self) -> &EditorModel {
        &self.model
    }

    /// The text's revision; it bumps on every change.
    pub fn revision(&self) -> u64 {
        self.model.revision()
    }

    pub fn char_len(&self) -> usize {
        self.model.buffer().len_chars()
    }

    /// The primary caret as a character offset.
    pub fn caret_char(&self) -> usize {
        self.model.buffer().char_index(self.model.primary().head)
    }

    /// The primary selection as `(start, end)` character offsets, end exclusive, when it is
    /// not empty.
    pub fn selection_chars(&self) -> Option<(usize, usize)> {
        let caret = self.model.primary();
        if caret.is_empty() {
            return None;
        }
        let a = self.model.buffer().char_index(caret.anchor);
        let b = self.model.buffer().char_index(caret.head);
        Some((a.min(b), a.max(b)))
    }

    /// Moves the primary caret to a character offset and scrolls to it.
    pub fn set_caret_char(&mut self, offset: usize) {
        let offset = offset.min(self.char_len());
        self.model
            .set_carets(vec![Caret::at(self.model.buffer().pos_of_char(offset))]);
        self.scroll_to_caret = true;
    }

    /// Replaces the whole buffer as one undo group.
    pub fn replace_all(&mut self, text: &str) -> Vec<Change> {
        let len = self.char_len();
        let changes = self.model.replace_range(0..len, text, self.now);
        self.after_external_edit(&changes);
        changes
    }

    /// Applies non-overlapping range edits as one undo group. Out-of-range or overlapping
    /// edits are clamped by the model.
    pub fn replace_ranges(&mut self, edits: &[(std::ops::Range<usize>, String)]) -> Vec<Change> {
        let changes = self.model.replace_ranges(edits, self.now);
        self.after_external_edit(&changes);
        changes
    }

    /// Applies the model-side effects of an edit made outside the widget (agent edits).
    pub fn after_external_edit(&mut self, changes: &[Change]) {
        self.after_changes(changes);
        self.scroll_to_caret = true;
        self.popup.close();
    }

    /// Highlights the first changed range for the next frame.
    pub fn highlight_agent_edit(&mut self, at: usize) {
        self.set_markers(vec![Marker {
            at,
            message: "Agent edit".into(),
        }]);
        self.scroll_to_caret = true;
    }

    /// The syntax the editor lexes and completes with.
    pub fn syntax(&self) -> &Arc<SqlSyntax> {
        self.syntax.syntax()
    }

    pub fn set_carets(&mut self, carets: Vec<Caret>) {
        self.model.set_carets(carets);
        self.popup.close();
    }

    /// Scrolls the primary caret into view in the next frame.
    pub fn reveal_caret(&mut self) {
        self.scroll_to_caret = true;
    }

    /// Resets the syntax layer (keeping the language) and sets the model's line comment.
    pub fn set_syntax(&mut self, syntax: Arc<SqlSyntax>) {
        self.reset_layer(syntax, self.syntax.language());
    }

    /// Switches the editor to `language` (highlighting, statements, line comment).
    pub fn set_language(&mut self, language: QueryLanguage) {
        self.reset_layer(self.syntax.syntax().clone(), language);
    }

    fn reset_layer(&mut self, syntax: Arc<SqlSyntax>, language: QueryLanguage) {
        self.model.set_line_comment(match language {
            QueryLanguage::Sql => syntax.line_comment,
            other => other.line_comment(),
        });
        self.syntax = SyntaxLayer::with_language(syntax, language, self.model.buffer());
        self.popup.invalidate();
    }

    /// The language the editor lexes.
    pub fn language(&self) -> QueryLanguage {
        self.syntax.language()
    }

    /// The primary selection (trimmed) if non-empty, else the statement at the primary caret.
    pub fn run_target(&mut self) -> Option<RunRequest> {
        let buf = self.model.buffer();
        let rope = buf.rope();
        let c = self.model.primary();
        if !c.is_empty() {
            let (a, b) = c.range();
            let start = buf.char_index(a);
            let text = rope.slice(start..buf.char_index(b)).to_string();
            let lead = text.len() - text.trim_start().len();
            let sql = text.trim();
            if !sql.is_empty() {
                return Some(RunRequest {
                    sql: sql.to_string(),
                    origin: start + text[..lead].chars().count(),
                });
            }
        }
        let range = self.syntax.statement_at(buf, buf.char_index(c.head))?;
        Some(self.request(range))
    }

    /// The statements in the primary selection, else in the whole text.
    /// Every statement of the whole buffer, whatever the selection.
    pub fn buffer_targets(&mut self) -> Vec<RunRequest> {
        let buf = self.model.buffer();
        let ranges = self.syntax.statements(buf, 0..buf.len_chars());
        ranges.into_iter().map(|r| self.request(r)).collect()
    }

    pub fn script_targets(&mut self) -> Vec<RunRequest> {
        let buf = self.model.buffer();
        let c = self.model.primary();
        let range = if c.is_empty() {
            0..buf.len_chars()
        } else {
            let (a, b) = c.range();
            buf.char_index(a)..buf.char_index(b)
        };
        let ranges = self.syntax.statements(buf, range);
        ranges.into_iter().map(|r| self.request(r)).collect()
    }

    fn request(&self, chars: Range<usize>) -> RunRequest {
        RunRequest {
            sql: self.model.buffer().rope().slice(chars.clone()).to_string(),
            origin: chars.start,
        }
    }

    pub fn set_markers(&mut self, markers: Vec<Marker>) {
        self.marker_tokens = markers.iter().map(|m| m.at..m.at + 1).collect();
        self.markers = markers;
        self.marker_spans.clear();
    }

    pub fn markers(&self) -> &[Marker] {
        &self.markers
    }

    /// Inserts `text` at the carets (replacing selections) as one undo group.
    pub fn set_format_options(&mut self, options: sqlr_core::FormatOptions) {
        self.model.set_indent(match options.indent {
            sqlr_core::Indent::Spaces(n) => " ".repeat(n.clamp(1, 16)),
            sqlr_core::Indent::Tabs => "\t".into(),
        });
        self.format_options = options;
    }

    pub fn apply_edit(&mut self, command: EditCommand) {
        self.edit(command, self.now);
    }

    /// Formats the selection, or the whole buffer, as one undo group.
    pub fn format_sql(&mut self, options: &sqlr_core::FormatOptions) {
        if self.language() != QueryLanguage::Sql {
            return;
        }
        let caret = self.model.carets()[0];
        let buf = self.model.buffer();
        let a = buf.char_index(caret.anchor);
        let b = buf.char_index(caret.head);
        let range = if a == b {
            0..buf.len_chars()
        } else {
            a.min(b)..a.max(b)
        };
        let text = self.text();
        let byte_a = text
            .char_indices()
            .nth(range.start)
            .map_or(text.len(), |(i, _)| i);
        let byte_b = text
            .char_indices()
            .nth(range.end)
            .map_or(text.len(), |(i, _)| i);
        let formatted = sqlr_core::format_sql(self.syntax.syntax(), &text[byte_a..byte_b], options);
        let changes = self.model.replace_range(range, &formatted, self.now);
        self.after_changes(&changes);
        self.popup.close();
        self.scroll_to_caret = true;
    }

    pub fn insert_text(&mut self, text: &str) {
        let changes = self.model.insert_at_carets(text, self.now);
        self.after_changes(&changes);
        self.popup.close();
        self.scroll_to_caret = true;
    }

    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    /// Whether the editor had keyboard focus in the last frame.
    pub fn has_focus(&self) -> bool {
        self.focused
    }

    pub fn completion_open(&self) -> bool {
        self.popup.is_open()
    }

    /// Whether Esc has something to dismiss here: the completion popup, the find bar, a
    /// selection or extra carets.
    pub fn escape_would_clear(&self) -> bool {
        let carets = self.model.carets();
        self.popup.is_open()
            || self.find.open
            || carets.len() > 1
            || carets.iter().any(|c| c.head != c.anchor)
    }

    pub fn completion_labels(&self) -> Vec<String> {
        self.popup.labels()
    }

    /// The caret position of `p` in the last frame (vertically centered in its row), if
    /// its line was visible.
    pub fn screen_pos(&self, p: Pos) -> Option<Pos2> {
        let p = self.model.buffer().clamp(p);
        let g = self.line_geom(p.line)?;
        let row = row_of_col(g, p.col);
        let y = self.geom.text_origin.y
            + (g.first_row + row) as f32 * self.geom.row_height
            + self.geom.row_height * 0.5;
        Some(pos2(self.x_in(g, p.col), y))
    }

    /// The char ranges underlined in the last frame.
    pub fn last_marker_spans(&self) -> Vec<Range<usize>> {
        self.marker_spans.clone()
    }

    /// Parses requested from the scope worker, and the bytes of text they covered.
    pub fn scope_requests(&self) -> (u64, u64) {
        self.worker
            .as_ref()
            .map_or((0, 0), ScopeWorker::request_stats)
    }

    /// Bytes copied out of the buffer and split into statements on the UI thread so far.
    pub fn split_bytes(&self) -> u64 {
        self.syntax.split_bytes()
    }

    pub fn frame_stats(&self) -> FrameStats {
        self.stats
    }

    fn line_geom(&self, line: usize) -> Option<&LineGeom> {
        let first = self.geom.lines.first()?.line;
        self.geom.lines.get(line.checked_sub(first)?)
    }

    /// The caret rectangle of `col` on `line` from the last frame's layout. A line that was
    /// not laid out this frame (off screen) falls back to the wrap index and a monospace x,
    /// so revealing a far caret still scrolls to the right spot.
    fn caret_rect(&self, line: usize, col: usize) -> Rect {
        let rh = self.geom.row_height;
        let (first_row, row, x) = match self.line_geom(line) {
            Some(g) if !g.long => (g.first_row, row_of_col(g, col), self.x_in(g, col)),
            _ => (
                if self.soft_wrap {
                    self.wrap.row_start(line)
                } else {
                    line
                },
                0,
                self.x_of(line, col),
            ),
        };
        Rect::from_min_size(
            pos2(x, self.geom.text_origin.y + (first_row + row) as f32 * rh),
            vec2(1.5, rh),
        )
    }

    /// Moves the primary caret one wrapped row up or down; true if it moved.
    fn move_visual_row(&mut self, down: bool, select: bool) -> bool {
        let rh = self.geom.row_height;
        if rh <= 0.0 {
            return false;
        }
        let primary = self.model.primary();
        let head = primary.head;
        let Some(g) = self.line_geom(head.line) else {
            return false;
        };
        if g.long {
            return false;
        }
        let rows = g.galley.rows.len();
        let row = row_of_col(g, head.col);
        let x = g.galley.pos_from_cursor(CCursor::new(head.col)).min.x;
        let n = self.model.buffer().len_lines();
        let (line, row) = if down {
            if row + 1 < rows {
                (head.line, row + 1)
            } else if head.line + 1 < n {
                (head.line + 1, 0)
            } else {
                return false;
            }
        } else if row > 0 {
            (head.line, row - 1)
        } else if head.line > 0 {
            let Some(prev) = self.line_geom(head.line - 1) else {
                return false;
            };
            if prev.long {
                return false;
            }
            (head.line - 1, prev.galley.rows.len() - 1)
        } else {
            return false;
        };
        let Some(g) = self.line_geom(line) else {
            return false;
        };
        let col = g
            .galley
            .cursor_from_pos(vec2(x, (row as f32 + 0.5) * rh))
            .index
            .0;
        let target = self.model.buffer().clamp(Pos { line, col });
        let anchor = if select { primary.anchor } else { target };
        self.model.set_carets(vec![Caret {
            anchor,
            head: target,
        }]);
        self.scroll_to_caret = true;
        true
    }

    /// Screen x of char column `col` in a laid-out line.
    fn x_in(&self, g: &LineGeom, col: usize) -> f32 {
        let origin = self.geom.text_origin.x;
        if g.long {
            return origin + col as f32 * self.geom.char_width;
        }
        origin + g.galley.pos_from_cursor(CCursor::new(col)).min.x
    }

    /// Screen x of `col` on `line`, laid out or not.
    fn x_of(&self, line: usize, col: usize) -> f32 {
        match self.line_geom(line) {
            Some(g) => self.x_in(g, col),
            None => self.geom.text_origin.x + col as f32 * self.geom.char_width,
        }
    }

    /// The buffer position under screen point `p`, from the last frame's layout.
    fn pos_at(&self, p: Pos2) -> Pos {
        let buf = self.model.buffer();
        let rh = self.geom.row_height.max(1.0);
        let row = ((p.y - self.geom.text_origin.y) / rh).floor().max(0.0) as usize;
        let (line, row) = if self.soft_wrap {
            self.wrap.line_of_row(row)
        } else {
            (row.min(buf.len_lines() - 1), 0)
        };
        let dx = (p.x - self.geom.text_origin.x).max(0.0);
        let col = match self.line_geom(line) {
            Some(g) if !g.long => {
                g.galley
                    .cursor_from_pos(vec2(dx, (row as f32 + 0.5) * rh))
                    .index
                    .0
            }
            _ => (dx / self.geom.char_width.max(1.0)).round() as usize,
        };
        buf.clamp(Pos { line, col })
    }

    /// Feeds model changes to the syntax layer and the markers.
    fn after_changes(&mut self, changes: &[Change]) {
        if changes.is_empty() {
            return;
        }
        self.syntax.apply_all(self.model.buffer(), changes);
        if self.soft_wrap {
            self.wrap_changes.extend_from_slice(changes);
        }
        for c in changes {
            shift_markers(&mut self.markers, &mut self.marker_tokens, c);
        }
    }

    /// Applies `cmd`; true if the text changed.
    fn edit(&mut self, cmd: EditCommand, now: f64) -> bool {
        let changes = self.model.apply(cmd, now);
        self.after_changes(&changes);
        self.scroll_to_caret = true;
        !changes.is_empty()
    }

    fn accept_completion(&mut self, index: Option<usize>, now: f64) -> bool {
        let item = match index {
            Some(i) => self.popup.item(i),
            None => self.popup.selected_item(),
        };
        self.popup.close();
        let Some(item) = item else { return false };
        let buf = self.model.buffer();
        let carets: Vec<Caret> = self
            .model
            .carets()
            .iter()
            .map(|c| {
                let n = word_before(buf, c.head);
                Caret {
                    anchor: Pos {
                        col: c.head.col - n,
                        ..c.head
                    },
                    head: c.head,
                }
            })
            .collect();
        self.model.set_carets(carets);
        let insert = if item.kind == super::complete::ItemKind::Keyword {
            match self.completion_options.keyword_case {
                sqlr_core::Case::Upper => item.insert.to_ascii_uppercase(),
                sqlr_core::Case::Lower => item.insert.to_ascii_lowercase(),
                sqlr_core::Case::Preserve => item.insert.clone(),
            }
        } else {
            item.insert.clone()
        };
        let changes = self.model.insert_at_carets(&insert, now);
        self.after_changes(&changes);
        for _ in 0..item.caret_back {
            self.model.apply(
                EditCommand::Move {
                    motion: Motion::Left,
                    select: false,
                },
                now,
            );
        }
        self.scroll_to_caret = true;
        !changes.is_empty()
    }

    fn run_find(&mut self, action: FindAction, now: f64) -> bool {
        let needle = self.find.needle.clone();
        let opts = self.find.opts;
        let buf = self.model.buffer();
        let c = self.model.primary();
        let (lo, hi) = c.range();
        let (lo, hi) = (buf.char_index(lo), buf.char_index(hi));
        let select = |m: &mut EditorModel, r: Range<usize>| {
            let b = m.buffer();
            let (a, h) = (b.pos_of_char(r.start), b.pos_of_char(r.end));
            m.set_carets(vec![Caret { anchor: a, head: h }]);
        };
        let mut changed = false;
        match action {
            FindAction::Next => {
                if let Some(r) = self.model.find(&needle, opts, hi, false) {
                    select(&mut self.model, r);
                }
            }
            FindAction::Previous => {
                if let Some(r) = self.model.find(&needle, opts, lo, true) {
                    select(&mut self.model, r);
                }
            }
            FindAction::Replace => {
                if lo < hi && self.model.find(&needle, opts, lo, false) == Some(lo..hi) {
                    let with = self.find.replacement.clone();
                    let changes = self.model.replace_range(lo..hi, &with, now);
                    self.after_changes(&changes);
                    changed = true;
                }
                let from = self.model.buffer().char_index(self.model.primary().head);
                if let Some(r) = self.model.find(&needle, opts, from, false) {
                    select(&mut self.model, r);
                }
            }
            FindAction::ReplaceAll => {
                let with = self.find.replacement.clone();
                let (n, changes) = self.model.replace_all(&needle, opts, &with, now);
                self.after_changes(&changes);
                changed = n > 0;
            }
            FindAction::Close => {
                self.find.close();
                self.focus_requested = true;
            }
        }
        self.popup.close();
        self.scroll_to_caret = true;
        changed
    }
}

/// Shifts the markers past `c`, and drops those whose token it overlaps.
fn shift_markers(markers: &mut Vec<Marker>, tokens: &mut Vec<Range<usize>>, c: &Change) {
    let (s, e) = (c.start_char, c.start_char + c.removed_chars);
    let delta = c.inserted_chars as isize - c.removed_chars as isize;
    let shift = |x: usize| (x as isize + delta) as usize;
    let mut i = 0;
    while i < markers.len() {
        let t = tokens[i].clone();
        if e <= t.start {
            markers[i].at = shift(markers[i].at);
            tokens[i] = shift(t.start)..shift(t.end);
        } else if s < t.end {
            markers.remove(i);
            tokens.remove(i);
            continue;
        }
        i += 1;
    }
}

/// The visual row `col` sits on in a laid-out line (0 unless the line wraps).
fn row_of_col(g: &LineGeom, col: usize) -> usize {
    if g.galley.rows.len() <= 1 {
        return 0;
    }
    let mut at = 0;
    for (i, row) in g.galley.rows.iter().enumerate() {
        at += row.char_count_including_newline().0;
        if col < at {
            return i;
        }
    }
    g.galley.rows.len() - 1
}

/// Lines of the statement holding the primary caret.
fn statement_lines(state: &mut EditorState) -> Option<Range<usize>> {
    if !state.statement_tint {
        return None;
    }
    let head = state.model.primary().head;
    // The syntax layer lexes to the caret line without a budget: on a fresh 10 MB document
    // that would be the whole file in one frame. The tint waits until lexing has got there.
    if head.line >= state.syntax.valid_lines() {
        return None;
    }
    let at = state.model.buffer().char_index(head);
    // Bounded: on a huge statement `statement_at` would copy and split it every frame.
    let range = {
        let buf = state.model.buffer();
        state
            .syntax
            .statement_within(buf, at, TINT_STATEMENT_BYTES)
            .ok()?
    }?;
    let buf = state.model.buffer();
    // A lone statement needs no tint: it would color the whole text.
    if range.start == 0 && range.end >= buf.len_chars().saturating_sub(1) {
        return None;
    }
    let first = buf.pos_of_char(range.start).line;
    let end = range.end.saturating_sub(1).max(range.start);
    let last = buf.pos_of_char(end).line;
    Some(first..last + 1)
}

/// Identifier chars right before `p` on its line (at most [`MAX_PREFIX_CHARS`]).
fn word_before(buf: &crate::editor::buffer::TextBuffer, p: Pos) -> usize {
    let at = buf.char_index(p);
    buf.rope()
        .chars_at(at)
        .reversed()
        .take(p.col.min(MAX_PREFIX_CHARS))
        .take_while(|&c| c.is_alphanumeric() || c == '_' || c == '$')
        .count()
}

/// Chars scanned on each side of a double-click for its word.
const MAX_WORD_CHARS: usize = 4096;

/// The word (alphanumeric and `_`) at or just before `p`, selected; `None` off a word.
fn word_selection(buf: &crate::editor::buffer::TextBuffer, p: Pos) -> Option<Caret> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let at = buf.char_index(p);
    let rope = buf.rope();
    let line_len = buf.line_len_chars(p.line);
    let right = rope
        .chars_at(at)
        .take((line_len - p.col).min(MAX_WORD_CHARS))
        .take_while(|&c| is_word(c))
        .count();
    let left = rope
        .chars_at(at)
        .reversed()
        .take(p.col.min(MAX_WORD_CHARS))
        .take_while(|&c| is_word(c))
        .count();
    (left + right > 0).then(|| Caret {
        anchor: Pos {
            col: p.col - left,
            ..p
        },
        head: Pos {
            col: p.col + right,
            ..p
        },
    })
}

/// Line `line` selected, with its line break when it has one.
fn line_selection(buf: &crate::editor::buffer::TextBuffer, line: usize) -> Caret {
    let head = if line + 1 < buf.len_lines() {
        Pos {
            line: line + 1,
            col: 0,
        }
    } else {
        Pos {
            line,
            col: buf.line_len_chars(line),
        }
    };
    Caret {
        anchor: Pos { line, col: 0 },
        head,
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Draws the editor and handles its input.
pub fn query_editor(
    ui: &mut Ui,
    state: &mut EditorState,
    source: &dyn CompletionSource,
) -> EditorOutput {
    let ctx = ui.ctx().clone();
    let font = theme::monospace(&ctx);
    state.palette = theme::palette(&ctx);
    state.colors = EditorColors::for_palette(&state.palette);
    // Galley rows are stacked at pixel-rounded heights; the editor's row geometry must agree
    // or hit-testing drifts one row every few dozen wrapped rows.
    let ppp = ctx.pixels_per_point();
    let rh = (ctx.fonts_mut(|f| f.row_height(&font)) * ppp).round() / ppp;
    let cw = ctx.fonts_mut(|f| f.glyph_width(&font, 'M'));
    let mac = ctx.os() == egui::os::OperatingSystem::Mac;
    let now = ui.input(|i| i.time);
    state.now = now;
    state.cache.begin_frame();
    let (hits0, misses0, lexed0) = (
        state.cache.hits(),
        state.cache.misses(),
        state.syntax.lexed_lines(),
    );
    let mut stats = FrameStats::default();
    let id = ui.make_persistent_id("sqlr_query_editor");
    let outer = ui.available_rect_before_wrap();
    let mut changed = false;
    let mut needs = Vec::new();

    // The find bar's own keys and buttons.
    for action in state.find.show(&ctx, id.with("find"), outer) {
        changed |= state.run_find(action, now);
    }

    let digits = state.model.buffer().len_lines().to_string().len();
    let gutter_w = if state.line_numbers {
        (digits + 2) as f32 * cw + 8.0
    } else {
        0.0
    };
    // From the outer rect, not the scroll viewport: scrollbars come and go, and the row
    // index must not be rebuilt every time the viewport loses a few points.
    let wrap_width = (outer.width() - gutter_w - 8.0).max(40.0).floor();

    let area = ScrollArea::both()
        .id_salt(id.with("scroll"))
        .auto_shrink([false, false]);
    let response = area
        .show_viewport(ui, |ui, viewport| {
            let n = state.model.buffer().len_lines();
            let wrap_width = wrap_width.max(40.0);
            if state.soft_wrap {
                let changes = std::mem::take(&mut state.wrap_changes);
                let buf = state.model.buffer();
                ctx.fonts_mut(|f| state.wrap.update(f, &font, buf, wrap_width, &changes));
            }
            let total_rows = if state.soft_wrap {
                state.wrap.total()
            } else {
                n
            };
            // With wrap on the content is never wider than the viewport: a content width
            // equal to it makes the horizontal scrollbar toggle and the wrap width follow,
            // rebuilding the row index every frame.
            let width = if state.soft_wrap {
                // Long lines are never wrapped, so they keep their horizontal scroll.
                (viewport.width() - 1.0).max(gutter_w + state.max_width + 4.0 * cw)
            } else {
                (gutter_w + state.max_width + 4.0 * cw).max(viewport.width())
            };
            let content = vec2(width, (total_rows as f32 * rh).max(viewport.height()));
            let (rect, _) = ui.allocate_exact_size(content, Sense::hover());
            let response = ui.interact(rect, id, Sense::click_and_drag());
            let label = match state.syntax.language() {
                QueryLanguage::Json => "JSON editor",
                _ => "SQL editor",
            };
            response.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, label));
            let view = viewport.translate(rect.min.to_vec2());
            let page = ((viewport.height() / rh) as usize).max(1);

            if std::mem::take(&mut state.focus_requested) {
                response.request_focus();
            }
            let mut focused = response.has_focus();
            let mut typed = String::new();
            let events = ui.input(|i| i.events.clone());
            for event in &events {
                match event {
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers,
                    } if view.contains(*pos)
                        && response.contains_pointer()
                        && ctx.layer_id_at(*pos).is_none_or(|l| l == ui.layer_id()) =>
                    {
                        response.request_focus();
                        focused = true;
                        let p = state.pos_at(*pos);
                        if modifiers.alt {
                            state.model.apply(EditCommand::AddCaret(p), now);
                        } else if modifiers.shift {
                            let anchor = state.model.primary().anchor;
                            state.model.set_carets(vec![Caret { anchor, head: p }]);
                        } else {
                            state.model.set_carets(vec![Caret::at(p)]);
                        }
                        state.dragging = !modifiers.alt;
                        state.popup.close();
                    }
                    Event::PointerButton {
                        button: PointerButton::Primary,
                        pressed: false,
                        ..
                    } => state.dragging = false,
                    Event::PointerMoved(pos) if state.dragging => {
                        let head = state.pos_at(*pos);
                        let anchor = state.model.primary().anchor;
                        if head != state.model.primary().head {
                            state.model.set_carets(vec![Caret { anchor, head }]);
                            state.scroll_to_caret = true;
                        }
                    }
                    _ if focused => {
                        changed |= handle_focused(state, &ctx, event, mac, page, now, &mut typed);
                    }
                    _ => {}
                }
            }
            // Double-click selects a word, triple-click a line (egui counts the clicks).
            let triple = response.triple_clicked_by(PointerButton::Primary);
            if (triple || response.double_clicked_by(PointerButton::Primary))
                && !ui.input(|i| i.modifiers.alt || i.modifiers.shift)
                && let Some(pos) = response.interact_pointer_pos()
            {
                let p = state.pos_at(pos);
                let buf = state.model.buffer();
                let sel = if triple {
                    Some(line_selection(buf, p.line))
                } else {
                    word_selection(buf, p)
                };
                if let Some(sel) = sel {
                    state.model.set_carets(vec![sel]);
                }
                state.dragging = false;
                state.popup.close();
            }
            if focused {
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(
                        id,
                        EventFilter {
                            tab: true,
                            horizontal_arrows: true,
                            vertical_arrows: true,
                            escape: true,
                        },
                    )
                });
            }
            if changed {
                state.popup.reset_selection();
            }

            // Completion: close on a non-identifier char, open on the Decisions' triggers.
            if state.completion
                && state.completion_options.auto_open
                && !typed.is_empty()
                && focused
            {
                if state.popup.is_open() && !typed.chars().all(|c| is_ident_char(c) || c == '.') {
                    state.popup.close();
                }
                let last = typed.chars().next_back();
                if !state.popup.is_open()
                    && let Some(c) = last
                    && (is_ident_char(c) || c == '.')
                    && state
                        .popup
                        .analysis(&state.model, &mut state.syntax)
                        .opens_on_typing(c == '.')
                {
                    state.completion_pending = Some((now, c == '.'));
                }
            }
            if let Some((started, _)) = state.completion_pending {
                let delay = state.completion_options.delay_ms as f64 / 1000.0;
                if focused && now - started >= delay {
                    state.popup.open(false);
                    state.completion_pending = None;
                } else if focused {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                        (delay - (now - started)).max(0.0),
                    ));
                } else {
                    state.completion_pending = None;
                }
            }
            let over_popup = state.popup.contains(ui.input(|i| i.pointer.interact_pos()));
            if !focused && !over_popup {
                state.popup.close();
            }
            if state.completion {
                let worker = state
                    .worker
                    .get_or_insert_with(|| ScopeWorker::new(Some(ctx.clone())));
                needs = state.popup.update(
                    &state.model,
                    &mut state.syntax,
                    worker,
                    source,
                    &mut stats.scope_parse,
                );
            }

            // Lines to draw.
            let n = state.model.buffer().len_lines();
            let (first, last) = if state.soft_wrap {
                let rows = state.wrap.total().max(1);
                let first_row = ((viewport.min.y / rh).floor().max(0.0) as usize).min(rows - 1);
                let last_row = (((viewport.max.y / rh).ceil().max(0.0) as usize) + OVERSCAN_LINES)
                    .min(rows - 1);
                let first = state.wrap.line_of_row(first_row).0;
                let last = state.wrap.line_of_row(last_row).0 + 1;
                (first, last.min(n))
            } else {
                let first = ((viewport.min.y / rh).floor().max(0.0) as usize).min(n - 1);
                let last =
                    (((viewport.max.y / rh).ceil().max(0.0) as usize) + OVERSCAN_LINES).min(n);
                (first, last)
            };
            let last = last.max(first + 1).min(n);
            let started = std::time::Instant::now();
            let buf = state.model.buffer();
            state.syntax.ensure(buf, last - 1, Some(LEX_BUDGET));
            if state.syntax.valid_lines() < n {
                let spare = LEX_BUDGET.saturating_sub(started.elapsed());
                if !spare.is_zero() && state.syntax.valid_lines() >= last {
                    state.syntax.ensure(buf, n - 1, Some(spare));
                }
                if state.syntax.valid_lines() < n {
                    ctx.request_repaint();
                }
            }

            state.geom.text_origin = rect.min + vec2(gutter_w, 0.0);
            state.geom.row_height = rh;
            state.geom.char_width = cw;
            state.geom.view = view;
            layout_lines(
                state,
                &ctx,
                &font,
                first..last,
                viewport.min.x,
                viewport.width(),
                gutter_w,
                state.soft_wrap.then_some(wrap_width),
            );
            stats.visible_lines = state.geom.lines.len();
            state.tint = statement_lines(state);

            let painter = ui.painter_at(view);
            painter.rect_filled(view, 0.0, state.palette.bg_editor);
            paint_statement_tint(state, &painter);
            paint_find_matches(state, &painter);
            paint_selections(state, &painter);
            for g in &state.geom.lines {
                let y = state.geom.text_origin.y + g.first_row as f32 * rh;
                // A long line's window starts at its first char's monospace position.
                let x = state.geom.text_origin.x + if g.long { g.c0 as f32 * cw } else { 0.0 };
                painter.galley(pos2(x, y), g.galley.clone(), state.colors.plain);
            }
            stats.painted_markers = paint_markers(state, ui, &painter, id);
            let key = (state.model.revision(), state.model.carets().to_vec());
            if key != state.blink_key {
                state.blink_key = key;
                state.blink_start = now;
            }
            let blink = ui.style().visuals.text_cursor.clone();
            let lit = if focused && blink.blink {
                let (on, off) = (blink.on_duration as f64, blink.off_duration as f64);
                let phase = (now - state.blink_start) % (on + off);
                let next = if phase < on {
                    on - phase
                } else {
                    on + off - phase
                };
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(next.max(0.01)));
                phase < on
            } else {
                true
            };
            let caret_rect = paint_carets(state, &ctx, &painter, &font, focused, lit);
            if state.scroll_to_caret
                && focused
                && let Some(r) = caret_rect
            {
                ui.scroll_to_rect(r.expand2(vec2(cw * 4.0, rh)), None);
            }
            state.scroll_to_caret = false;

            // The gutter is painted last, so it stays put over horizontally scrolled text.
            if state.line_numbers {
                let gutter = Rect::from_min_size(view.min, vec2(gutter_w - 4.0, view.height()));
                painter.rect_filled(gutter, 0.0, state.palette.bg_editor);
                let colors = state.colors;
                for g in &state.geom.lines {
                    let num = (g.line + 1).to_string();
                    let galley = state
                        .cache
                        .get(&ctx, &num, None, 0, None, None, &font, &colors);
                    let y = state.geom.text_origin.y + g.first_row as f32 * rh;
                    let x = view.min.x + gutter_w - 8.0 - galley.size().x;
                    painter.galley(pos2(x, y), galley, colors.gutter);
                }
            }
            for span in &state.marker_spans {
                let line = state.model.buffer().pos_of_char(span.start).line;
                let y = state.geom.text_origin.y + (line as f32 + 0.5) * rh;
                painter.circle_filled(pos2(view.min.x + 5.0, y), 3.0, state.palette.error);
            }

            if (focused || over_popup) && state.popup.is_open() {
                let c = state.model.primary().head;
                let back = word_before(state.model.buffer(), c);
                let x = state.x_of(c.line, c.col - back);
                let y = state.caret_rect(c.line, c.col - back).bottom();
                if let Some(i) = state
                    .popup
                    .show(&ctx, id.with("completion"), pos2(x, y), &font)
                {
                    changed |= state.accept_completion(Some(i), now);
                    response.request_focus();
                }
            }
            state.focused = focused || response.has_focus();
            response
        })
        .inner;

    stats.galley_hits = state.cache.hits() - hits0;
    stats.galley_misses = state.cache.misses() - misses0;
    stats.lexed_lines = state.syntax.lexed_lines() - lexed0;
    state.stats = stats;
    let snippet_menu = state.snippet_menu;
    let mut save_snippet = false;
    crate::text_menu::text_edit_menu_with(
        &response,
        crate::text_menu::TextMenuMode::Editable,
        |ui| {
            if !snippet_menu {
                return;
            }
            ui.separator();
            use crate::theme::icons as i;
            use sqlr_core::ActionId as A;
            let item = crate::text_menu::shortcut_item;
            item(ui, &response, i::SEARCH, "Find…", A::Find);
            item(ui, &response, i::FORMAT, "Format SQL", A::FormatSql);
            item(
                ui,
                &response,
                i::QUERY,
                "Toggle line comment",
                A::ToggleComment,
            );
            ui.separator();
            let shortcut =
                crate::keymap::ui_keymap(ui.ctx()).label(sqlr_core::ActionId::SaveSnippet);
            if crate::theme::icons::menu_item_shortcut(
                ui,
                crate::theme::icons::SNIPPET,
                "Save as snippet…",
                shortcut,
            )
            .clicked()
            {
                save_snippet = true;
                ui.close();
            }
        },
    );
    EditorOutput {
        response,
        changed,
        needs,
        save_snippet,
    }
}

/// Handles a keyboard, text, clipboard or IME event for the focused editor; true if the
/// text changed. Typed text is appended to `typed`.
fn handle_focused(
    state: &mut EditorState,
    ctx: &egui::Context,
    event: &Event,
    mac: bool,
    page: usize,
    now: f64,
    typed: &mut String,
) -> bool {
    match event {
        Event::Text(t) if !t.is_empty() => {
            typed.push_str(t);
            state.edit(EditCommand::Type(t.clone()), now)
        }
        Event::Paste(t) => {
            state.popup.close();
            state.edit(EditCommand::Paste(t.clone()), now)
        }
        Event::Copy | Event::Cut => {
            let text = state.model.selected_text();
            if text.is_empty() {
                return false;
            }
            ctx.copy_text(text);
            if matches!(event, Event::Cut) {
                state.popup.close();
                return state.edit(EditCommand::Type(String::new()), now);
            }
            false
        }
        Event::Ime(ImeEvent::Preedit { text, .. }) => {
            state.preedit = text.clone();
            false
        }
        Event::Ime(ImeEvent::Commit(t)) => {
            state.preedit.clear();
            if t.is_empty() {
                return false;
            }
            typed.push_str(t);
            state.edit(EditCommand::Type(t.clone()), now)
        }
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => handle_key(state, ctx, *key, *modifiers, mac, page, now),
        _ => false,
    }
}

fn handle_key(
    state: &mut EditorState,
    ctx: &egui::Context,
    key: Key,
    m: Modifiers,
    mac: bool,
    page: usize,
    now: f64,
) -> bool {
    state.completion_pending = None;
    if state.popup.is_open() && !m.command && !m.alt {
        match key {
            Key::ArrowUp => return popup_move(state, -1),
            Key::ArrowDown => return popup_move(state, 1),
            Key::PageUp => return popup_move(state, -10),
            Key::PageDown => return popup_move(state, 10),
            Key::Enter if !m.shift => return state.accept_completion(None, now),
            Key::Tab if !m.shift && state.completion_options.insert_on_tab => {
                return state.accept_completion(None, now);
            }
            Key::Escape => {
                state.popup.close();
                return false;
            }
            _ => {}
        }
    }
    if state.soft_wrap
        && !m.command
        && !m.alt
        && matches!(key, Key::ArrowUp | Key::ArrowDown)
        && state.move_visual_row(key == Key::ArrowDown, m.shift)
    {
        return false;
    }
    let Some(action) =
        crate::editor::input::mapped_action(crate::keymap::ui_keymap(ctx).lookup(key, m))
            .or_else(|| key_action(key, m, mac, page))
    else {
        return false;
    };
    match action {
        Action::Copy | Action::Cut => {
            let text = state.model.selected_text();
            if !text.is_empty() {
                ctx.copy_text(text);
            }
            if matches!(action, Action::Cut) {
                state.edit(EditCommand::Type(String::new()), now)
            } else {
                false
            }
        }
        Action::Paste => {
            crate::keymap::request_paste(ctx);
            false
        }
        Action::FormatSql => {
            let options = state.format_options.clone();
            let before = state.text();
            state.format_sql(&options);
            state.text() != before
        }
        Action::Edit(cmd) => {
            let keeps_popup = matches!(
                cmd,
                EditCommand::Backspace | EditCommand::Delete | EditCommand::DeleteWordBack
            );
            if !keeps_popup {
                state.popup.close();
            }
            state.edit(cmd, now)
        }
        Action::ToggleCompletion => {
            if state.completion {
                if state.popup.is_open() {
                    state.popup.close();
                } else {
                    state.popup.open(true);
                }
            }
            false
        }
        Action::OpenFind { replace } => {
            let selection = state.model.selected_text();
            state.find.open(replace, &selection);
            state.popup.close();
            false
        }
        Action::FindNext => state.run_find(FindAction::Next, now),
        Action::FindPrevious => state.run_find(FindAction::Previous, now),
        Action::Escape => {
            if state.popup.is_open() {
                state.popup.close();
            } else if state.find.open {
                state.find.close();
            } else if state.model.carets().len() > 1 {
                state.model.apply(EditCommand::CollapseCarets, now);
            } else {
                // Last: the selection collapses to the caret.
                let head = state.model.primary().head;
                state
                    .model
                    .set_carets(vec![crate::editor::model::Caret::at(head)]);
            }
            false
        }
    }
}

fn popup_move(state: &mut EditorState, delta: isize) -> bool {
    state.popup.move_selection(delta);
    false
}

/// Lays out the visible lines into `state.geom.lines`.
#[expect(
    clippy::too_many_arguments,
    reason = "the editor state, egui context, font, line range, horizontal view offset and width, gutter width and wrap width are independent per-frame inputs borrowed from the caller; a parameter struct would be built per frame only to be unpacked here"
)]
fn layout_lines(
    state: &mut EditorState,
    ctx: &egui::Context,
    font: &FontId,
    lines: Range<usize>,
    view_x: f32,
    view_w: f32,
    gutter_w: f32,
    wrap: Option<f32>,
) {
    let cw = state.geom.char_width.max(1.0);
    let vis_c0 = ((view_x - gutter_w).max(0.0) / cw) as usize;
    let vis_cols = (view_w / cw) as usize + 1;
    let mut out = Vec::with_capacity(lines.len());
    let mut corrected = false;
    let buf = state.model.buffer();
    for line in lines {
        let start = state.syntax.line_state(line);
        let len_bytes = buf.line_len_bytes(line);
        let first_row = if wrap.is_some() {
            state.wrap.row_start(line)
        } else {
            line
        };
        if len_bytes <= LONG_LINE_BYTES {
            let text = buf.line(line);
            let has = state
                .syntax
                .tokens(buf, line, 0..len_bytes, &mut state.scratch);
            let tokens = has.then_some(state.scratch.as_slice());
            let galley = state
                .cache
                .get(ctx, &text, start, 0, wrap, tokens, font, &state.colors);
            if wrap.is_some() {
                corrected |= state.wrap.set_rows(line, galley.rows.len());
            } else {
                state.max_width = state.max_width.max(galley.size().x);
            }
            out.push(LineGeom {
                line,
                galley,
                c0: 0,
                long: false,
                first_row,
            });
            continue;
        }
        let rope_line = buf.rope().line(line);
        let len_chars = buf.line_len_chars(line);
        let c0 = (vis_c0.saturating_sub(WINDOW_PAD_CHARS) / WINDOW_QUANTUM * WINDOW_QUANTUM)
            .min(len_chars);
        let c1 = ((vis_c0 + vis_cols + WINDOW_PAD_CHARS).div_ceil(WINDOW_QUANTUM) * WINDOW_QUANTUM)
            .min(len_chars);
        let (b0, b1) = (rope_line.char_to_byte(c0), rope_line.char_to_byte(c1));
        let text = rope_line.slice(c0..c1).to_string();
        let has = state.syntax.tokens(buf, line, b0..b1, &mut state.scratch);
        for t in &mut state.scratch {
            t.start = t.start.saturating_sub(b0);
            t.end = t.end.saturating_sub(b0).min(b1 - b0);
        }
        let tokens = has.then_some(state.scratch.as_slice());
        let galley = state
            .cache
            .get(ctx, &text, start, c0, None, tokens, font, &state.colors);
        if wrap.is_some() {
            corrected |= state.wrap.set_rows(line, 1);
        }
        // Long lines never wrap: they scroll sideways whether or not wrap is on.
        state.max_width = state.max_width.max(len_chars as f32 * cw);
        out.push(LineGeom {
            line,
            galley,
            c0,
            long: true,
            first_row,
        });
    }
    state.geom.lines = out;
    if corrected {
        // The corrected rows move the lines below; draw once more with the new offsets.
        ctx.request_repaint();
    }
}

/// Char column of byte `b` in a laid-out line's galley text.
fn col_of_byte(g: &LineGeom, b: usize) -> usize {
    g.c0 + g.galley.text()[..b].chars().count()
}

fn paint_find_matches(state: &EditorState, painter: &egui::Painter) {
    if !state.find.open || state.find.needle.is_empty() {
        return;
    }
    let rh = state.geom.row_height;
    for g in &state.geom.lines {
        let text = g.galley.text();
        for (ms, me) in matches_in_line(text, &state.find.needle, state.find.opts, 0, false) {
            let (ca, cb) = (col_of_byte(g, ms), col_of_byte(g, me));
            let xa = state.x_in(g, ca);
            let xb = state.x_in(g, cb);
            let ra = row_of_col(g, ca);
            let rb = row_of_col(g, cb);
            let y = state.geom.text_origin.y + (g.first_row + ra) as f32 * rh;
            if ra == rb {
                painter.rect_filled(
                    Rect::from_min_max(pos2(xa, y), pos2(xb, y + rh)),
                    0.0,
                    state.colors.find_match,
                );
            } else {
                // The match crosses a soft break: paint the tail of one row and the head of
                // the next.
                painter.rect_filled(
                    Rect::from_min_max(pos2(xa, y), pos2(state.geom.view.max.x, y + rh)),
                    0.0,
                    state.colors.find_match,
                );
                let y = state.geom.text_origin.y + (g.first_row + rb) as f32 * rh;
                painter.rect_filled(
                    Rect::from_min_max(pos2(state.geom.text_origin.x, y), pos2(xb, y + rh)),
                    0.0,
                    state.colors.find_match,
                );
            }
        }
    }
}

/// A subtle background behind the statement holding the primary caret.
fn paint_statement_tint(state: &EditorState, painter: &egui::Painter) {
    let Some(tint) = &state.tint else {
        return;
    };
    let rh = state.geom.row_height;
    let view = state.geom.view;
    for g in &state.geom.lines {
        if !tint.contains(&g.line) {
            continue;
        }
        let y = state.geom.text_origin.y + g.first_row as f32 * rh;
        painter.rect_filled(
            Rect::from_min_size(pos2(view.min.x, y), vec2(view.width(), rh)),
            0.0,
            state.palette.statement_tint,
        );
    }
}

fn paint_selections(state: &EditorState, painter: &egui::Painter) {
    let rh = state.geom.row_height;
    let (Some(first), Some(last)) = (state.geom.lines.first(), state.geom.lines.last()) else {
        return;
    };
    let (first, last) = (first.line, last.line);
    let buf = state.model.buffer();
    for c in state.model.carets() {
        if c.is_empty() {
            continue;
        }
        let (a, b) = c.range();
        for l in a.line.max(first)..=b.line.min(last) {
            let ca = if l == a.line { a.col } else { 0 };
            let cb = if l == b.line {
                b.col
            } else {
                buf.line_len_chars(l)
            };
            let Some(g) = state.line_geom(l) else {
                continue;
            };
            if g.long {
                // The window assumes a monospace advance; the old single-row rectangle holds.
                let xa = state.x_of(l, ca);
                let mut xb = state.x_of(l, cb);
                if l != b.line {
                    xb += state.geom.char_width * 0.5;
                }
                let y = state.geom.text_origin.y + g.first_row as f32 * rh;
                painter.rect_filled(
                    Rect::from_min_max(pos2(xa, y), pos2(xb, y + rh)),
                    0.0,
                    state.palette.selection,
                );
                continue;
            }
            let (ra, rb) = (row_of_col(g, ca), row_of_col(g, cb));
            let (xa, xb) = (state.x_in(g, ca), state.x_in(g, cb));
            let right = state.geom.view.max.x;
            let left = state.geom.text_origin.x;
            for row in ra..=rb {
                let x0 = if row == ra { xa } else { left };
                let x1 = if row == rb {
                    if l != b.line {
                        xb + state.geom.char_width * 0.5
                    } else {
                        xb
                    }
                } else {
                    right
                };
                if x1 <= x0 && ra == rb {
                    continue;
                }
                let y = state.geom.text_origin.y + (g.first_row + row) as f32 * rh;
                painter.rect_filled(
                    Rect::from_min_max(pos2(x0, y), pos2(x1, y + rh)),
                    0.0,
                    state.palette.selection,
                );
            }
        }
    }
}

/// Underlines the markers on visible lines; returns how many were painted.
fn paint_markers(state: &mut EditorState, ui: &Ui, painter: &egui::Painter, id: egui::Id) -> usize {
    state.marker_spans.clear();
    let (Some(first), Some(last)) = (state.geom.lines.first(), state.geom.lines.last()) else {
        return 0;
    };
    let (first, last) = (first.line, last.line);
    let rh = state.geom.row_height;
    let mut painted = 0;
    for i in 0..state.markers.len() {
        let at = state.markers[i].at;
        let buf = state.model.buffer();
        let len = buf.len_chars();
        let p = buf.pos_of_char(at);
        if p.line < first || p.line > last {
            continue;
        }
        let span = marker_span(state, at.min(len), p);
        state.marker_tokens[i] = span.clone();
        let buf = state.model.buffer();
        let (a, b) = (buf.pos_of_char(span.start), span.end - span.start);
        let xa = state.x_of(a.line, a.col);
        let xb = if span.start >= len {
            xa + state.geom.char_width
        } else {
            state.x_of(a.line, a.col + b)
        };
        let (row, first_row) = state
            .line_geom(a.line)
            .map_or((0, a.line), |g| (row_of_col(g, a.col), g.first_row));
        let y = state.geom.text_origin.y + (first_row + row + 1) as f32 * rh - 2.0;
        painter.add(wavy(xa, xb, y, state.palette.error));
        let hover = Rect::from_min_max(pos2(xa, y - rh + 2.0), pos2(xb.max(xa + 2.0), y + 2.0));
        let message = state.markers[i].message.clone();
        ui.interact(hover, id.with(("marker", i)), Sense::hover())
            .on_hover_text(message);
        state.marker_spans.push(span);
        painted += 1;
    }
    painted
}

/// The char range of the token containing char `at` (at `p`), or one char at `at`.
fn marker_span(state: &mut EditorState, at: usize, p: Pos) -> Range<usize> {
    let buf = state.model.buffer();
    let rope = buf.rope();
    let line_start = rope.line_to_char(p.line);
    let line_byte = rope.line_to_byte(p.line);
    let b = rope.char_to_byte(at) - line_byte;
    if state
        .syntax
        .tokens(buf, p.line, b..b + 1, &mut state.scratch)
        && let Some(t) = state.scratch.iter().find(|t| t.start <= b && b < t.end)
    {
        let s = rope.byte_to_char(line_byte + t.start);
        let e = rope.byte_to_char(line_byte + t.end);
        debug_assert!(s >= line_start);
        return s..e;
    }
    at..at + 1
}

/// A zigzag underline from `x0` to `x1` at `y`.
fn wavy(x0: f32, x1: f32, y: f32, color: egui::Color32) -> Shape {
    const STEP: f32 = 2.0;
    let mut points = Vec::new();
    let mut x = x0;
    let mut up = false;
    while x < x1 {
        points.push(pos2(x, if up { y - 1.5 } else { y + 0.5 }));
        x += STEP;
        up = !up;
    }
    points.push(pos2(x1, if up { y - 1.5 } else { y + 0.5 }));
    Shape::line(points, Stroke::new(1.0, color))
}

/// Paints the carets and the IME preedit; returns the primary caret's rect.
fn paint_carets(
    state: &EditorState,
    ctx: &egui::Context,
    painter: &egui::Painter,
    font: &FontId,
    focused: bool,
    lit: bool,
) -> Option<Rect> {
    let rh = state.geom.row_height;
    let primary = state.model.primary();
    let mut primary_rect = None;
    for c in state.model.carets() {
        let h = c.head;
        let caret = state.caret_rect(h.line, h.col);
        if *c == primary {
            primary_rect = Some(caret);
        }
        if state.line_geom(h.line).is_none() {
            continue;
        }
        let (x, y) = (caret.min.x, caret.min.y);
        if focused && lit {
            painter.rect_filled(caret, 0.0, state.colors.caret);
        }
        if focused && *c == primary && !state.preedit.is_empty() {
            let text = state.colors.plain;
            let g = ctx.fonts_mut(|f| f.layout_no_wrap(state.preedit.clone(), font.clone(), text));
            let w = g.size().x;
            painter.rect_filled(
                Rect::from_min_size(pos2(x, y), vec2(w, rh)),
                0.0,
                state.palette.bg_editor,
            );
            painter.galley(pos2(x, y), g, text);
            painter.line_segment(
                [pos2(x, y + rh - 1.0), pos2(x + w, y + rh - 1.0)],
                Stroke::new(1.0, text),
            );
        }
    }
    if focused && let Some(caret) = primary_rect {
        ctx.output_mut(|o| {
            o.ime = Some(IMEOutput {
                purpose: IMEPurpose::Normal,
                rect: state.geom.view,
                cursor_rect: caret,
                should_interrupt_composition: false,
            })
        });
    }
    primary_rect
}
