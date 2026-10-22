//! The edit model: a [`TextBuffer`], sorted multi-carets and grouped undo, driven by
//! [`EditCommand`]s. Editing commands read only the chars around each caret, never whole
//! lines, so a keystroke costs the same on a 1 MB line as on a short one.

use std::ops::Range;

use crate::editor::buffer::{Change, Pos, TextBuffer};
use crate::editor::undo::{GroupKind, Record, UndoEdit, UndoStack};

/// The indent unit.
pub const INDENT: &str = "    ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caret {
    pub head: Pos,
    pub anchor: Pos,
}

impl Caret {
    pub fn at(p: Pos) -> Self {
        Self { head: p, anchor: p }
    }

    /// The selection's start and end, in document order.
    pub fn range(&self) -> (Pos, Pos) {
        (self.head.min(self.anchor), self.head.max(self.anchor))
    }

    pub fn is_empty(&self) -> bool {
        self.head == self.anchor
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
    PageUp(usize),
    PageDown(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum EditCommand {
    Type(String),
    Paste(String),
    Newline,
    Tab,
    BackTab,
    Backspace,
    Delete,
    DeleteWordBack,
    Move { motion: Motion, select: bool },
    SelectAll,
    AddCaret(Pos),
    AddCaretAbove,
    AddCaretBelow,
    CollapseCarets,
    Undo,
    Redo,
    ToggleComment,
    DuplicateLine,
}

pub struct EditorModel {
    buffer: TextBuffer,
    /// Sorted, non-overlapping, at least one; the primary caret is the last.
    carets: Vec<Caret>,
    /// Per caret (in `carets` order), the column Up, Down, PageUp and PageDown aim for: set
    /// by the first vertical move, kept by the next ones. Empty after any other motion, edit
    /// or caret placement.
    goals: Vec<Option<usize>>,
    undo: UndoStack,
    line_comment: String,
    indent: String,
}

/// One caret's planned edit: replace `range` with `text`, then put the caret's anchor and
/// head these many chars after the range start.
struct Plan {
    range: Range<usize>,
    text: String,
    anchor: usize,
    head: usize,
}

impl Plan {
    /// Replace `range` with `text`, caret after it.
    fn put(range: Range<usize>, text: String) -> Self {
        let n = text.chars().count();
        Self {
            range,
            text,
            anchor: n,
            head: n,
        }
    }

    /// No edit; the caret moves to `at + by`.
    fn step(at: usize, by: usize) -> Self {
        Self {
            range: at..at,
            text: String::new(),
            anchor: by,
            head: by,
        }
    }
}

/// The result of [`EditorModel::replace_many`].
pub(super) struct Applied {
    pub changes: Vec<Change>,
    pub edits: Vec<UndoEdit>,
    /// The replaced ranges (old chars) with their inserted char counts, ascending.
    spans: Vec<(usize, usize, usize)>,
    /// Per input replacement, where its new text starts.
    new_starts: Vec<usize>,
}

impl Applied {
    /// Where old char `p` ends up: after an insertion at `p`, at the start of a replacement
    /// that swallowed it.
    fn map(&self, p: usize) -> usize {
        let mut delta = 0isize;
        for &(s, e, ins) in &self.spans {
            if e <= p {
                delta += ins as isize - (e - s) as isize;
            } else if s < p {
                return (s as isize + delta) as usize;
            } else {
                break;
            }
        }
        (p as isize + delta) as usize
    }
}

impl EditorModel {
    pub fn new(text: &str) -> Self {
        Self {
            buffer: TextBuffer::from_str(text),
            carets: vec![Caret::at(Pos::default())],
            goals: Vec::new(),
            undo: UndoStack::default(),
            line_comment: "--".to_string(),
            indent: INDENT.into(),
        }
    }

    pub fn set_indent(&mut self, indent: String) {
        self.indent = indent;
    }

    pub fn buffer(&self) -> &TextBuffer {
        &self.buffer
    }

    pub fn carets(&self) -> &[Caret] {
        &self.carets
    }

    pub fn primary(&self) -> Caret {
        *self.carets.last().expect("at least one caret")
    }

    /// Replaces the carets (clamped, sorted, merged); closes the undo group.
    pub fn set_carets(&mut self, carets: Vec<Caret>) {
        self.carets = carets;
        self.normalize();
        self.undo.close();
    }

    pub fn set_line_comment(&mut self, token: &str) {
        self.line_comment = token.to_string();
    }

    /// Bumps on every text change.
    pub fn revision(&self) -> u64 {
        self.buffer.version()
    }

    /// Non-empty selections, joined with "\n".
    pub fn selected_text(&self) -> String {
        let rope = self.buffer.rope();
        let parts: Vec<String> = self
            .carets
            .iter()
            .filter(|c| !c.is_empty())
            .map(|c| rope.slice(self.char_range(c)).to_string())
            .collect();
        parts.join("\n")
    }

    /// Applies `cmd` at input time `now` (seconds); returns the buffer changes in the order
    /// they were made.
    pub fn apply(&mut self, cmd: EditCommand, now: f64) -> Vec<Change> {
        use EditCommand as C;
        match cmd {
            C::Type(text) => {
                let single = single_char(&text);
                self.run(GroupKind::Type, now, Some(&text), |b, c| {
                    type_plan(b, c, &text, single)
                })
            }
            C::Paste(text) => self.run(GroupKind::Other, now, None, |b, c| {
                Plan::put(char_range(b, c), text.clone())
            }),
            C::Newline => {
                let indent = self.indent.clone();
                self.run(GroupKind::Other, now, None, |b, c| {
                    newline_plan(b, c, &indent)
                })
            }
            C::Tab => self.tab(now),
            C::BackTab => self.back_tab(now),
            C::Backspace => self.run(GroupKind::Backspace, now, None, |b, c| {
                delete_plan(b, c, |b, p| {
                    let at = b.char_index(p);
                    match (prev_in_line(b, p), next_in_line(b, p)) {
                        (Some(l), Some(r)) if is_empty_pair(l, r) => at - 1..at + 1,
                        _ => b.char_index(prev_pos(b, p))..at,
                    }
                })
            }),
            C::Delete => self.run(GroupKind::Other, now, None, |b, c| {
                delete_plan(b, c, |b, p| b.char_index(p)..b.char_index(next_pos(b, p)))
            }),
            C::DeleteWordBack => self.run(GroupKind::Other, now, None, |b, c| {
                delete_plan(b, c, |b, p| b.char_index(word_left(b, p))..b.char_index(p))
            }),
            C::Move { motion, select } => {
                self.move_carets(&motion, select);
                Vec::new()
            }
            C::SelectAll => {
                let last = self.buffer.len_lines() - 1;
                let end = Pos {
                    line: last,
                    col: self.buffer.line_len_chars(last),
                };
                self.set_carets(vec![Caret {
                    head: end,
                    anchor: Pos::default(),
                }]);
                Vec::new()
            }
            C::AddCaret(p) => {
                let mut carets = self.carets.clone();
                carets.push(Caret::at(p));
                self.set_carets(carets);
                Vec::new()
            }
            C::AddCaretAbove | C::AddCaretBelow => {
                // Keep growing from the outermost caret, at the primary's column.
                let col = self.primary().head.col;
                let line = if cmd == C::AddCaretAbove {
                    self.carets[0].head.line.checked_sub(1)
                } else {
                    Some(self.primary().head.line + 1).filter(|&l| l < self.buffer.len_lines())
                };
                let mut carets = self.carets.clone();
                if let Some(line) = line {
                    carets.push(Caret::at(Pos { line, col }));
                }
                self.set_carets(carets);
                Vec::new()
            }
            C::CollapseCarets => {
                self.set_carets(vec![self.primary()]);
                Vec::new()
            }
            C::Undo | C::Redo => {
                let done = if cmd == C::Undo {
                    self.undo.undo(&mut self.buffer)
                } else {
                    self.undo.redo(&mut self.buffer)
                };
                let Some((changes, carets)) = done else {
                    return Vec::new();
                };
                self.carets = carets;
                self.normalize();
                changes
            }
            C::DuplicateLine => self.run(GroupKind::Other, now, None, |buf, caret| {
                let range = char_range(buf, caret);
                if range.is_empty() {
                    let line = caret.head.line;
                    let end = buf.rope().line_to_char(line) + buf.line_len_chars(line);
                    Plan::put(end..end, format!("\n{}", buf.line(line)))
                } else {
                    let text = buf.rope().slice(range.clone()).to_string();
                    Plan::put(range.end..range.end, text)
                }
            }),
            C::ToggleComment => self.toggle_comment(now),
        }
    }

    /// Replaces `chars` with `text` as one undo group; a single caret lands after the insert.
    pub fn replace_range(&mut self, chars: Range<usize>, text: &str, now: f64) -> Vec<Change> {
        let snap = |c: usize| self.buffer.char_index(self.buffer.pos_of_char(c));
        let (a, b) = (snap(chars.start), snap(chars.end));
        let (a, b) = (a.min(b), a.max(b));
        let before = self.carets.clone();
        let applied = self.replace_many(vec![(a..b, text)]);
        let end = applied.new_starts[0] + text.chars().count();
        self.carets = vec![Caret::at(self.buffer.pos_of_char(end))];
        self.goals.clear();
        self.record(GroupKind::Other, before, applied.edits, now, None);
        applied.changes
    }

    /// Inserts `text` at every caret (replacing selections) as one undo group, without pairing.
    pub fn insert_at_carets(&mut self, text: &str, now: f64) -> Vec<Change> {
        self.run(GroupKind::Other, now, None, |b, c| {
            Plan::put(char_range(b, c), text.to_string())
        })
    }

    fn char_range(&self, c: &Caret) -> Range<usize> {
        char_range(&self.buffer, c)
    }

    /// Plans one edit per caret with `f`, applies them, places the carets and records undo.
    fn run(
        &mut self,
        kind: GroupKind,
        now: f64,
        typed: Option<&str>,
        mut f: impl FnMut(&TextBuffer, &Caret) -> Plan,
    ) -> Vec<Change> {
        let before = self.carets.clone();
        let plans: Vec<Plan> = self.carets.iter().map(|c| f(&self.buffer, c)).collect();
        let applied = self.replace_many(
            plans
                .iter()
                .map(|p| (p.range.clone(), p.text.as_str()))
                .collect(),
        );
        self.carets = plans
            .iter()
            .zip(&applied.new_starts)
            .map(|(p, &start)| Caret {
                head: self.buffer.pos_of_char(start + p.head),
                anchor: self.buffer.pos_of_char(start + p.anchor),
            })
            .collect();
        self.normalize();
        self.record(kind, before, applied.edits, now, typed);
        applied.changes
    }

    /// Applies replacements (old chars) and maps every caret through them.
    pub(super) fn replace_mapped(
        &mut self,
        reps: Vec<(Range<usize>, &str)>,
        now: f64,
    ) -> Vec<Change> {
        let before = self.carets.clone();
        let old: Vec<(usize, usize)> = before
            .iter()
            .map(|c| {
                (
                    self.buffer.char_index(c.anchor),
                    self.buffer.char_index(c.head),
                )
            })
            .collect();
        let applied = self.replace_many(reps);
        self.carets = old
            .into_iter()
            .map(|(a, h)| Caret {
                anchor: self.buffer.pos_of_char(applied.map(a)),
                head: self.buffer.pos_of_char(applied.map(h)),
            })
            .collect();
        self.normalize();
        self.record(GroupKind::Other, before, applied.edits, now, None);
        applied.changes
    }

    fn record(
        &mut self,
        kind: GroupKind,
        carets_before: Vec<Caret>,
        edits: Vec<UndoEdit>,
        now: f64,
        typed: Option<&str>,
    ) {
        let (first_char, last_char) = match (kind, typed) {
            (GroupKind::Type, Some(t)) => (t.chars().next(), t.chars().next_back()),
            (GroupKind::Backspace, _) => edits.first().map_or((None, None), |e| {
                (e.removed.chars().next_back(), e.removed.chars().next())
            }),
            _ => (None, None),
        };
        self.undo.record(Record {
            kind,
            edits,
            carets_before,
            carets_after: self.carets.clone(),
            now,
            first_char,
            last_char,
        });
    }

    /// Replaces each `(range, text)` (old chars; overlaps are clipped) from the end of the
    /// document backwards, so earlier offsets stay valid.
    pub(super) fn replace_many(&mut self, reps: Vec<(Range<usize>, &str)>) -> Applied {
        let len = self.buffer.len_chars();
        let mut order: Vec<usize> = (0..reps.len()).collect();
        order.sort_by_key(|&i| (reps[i].0.start, reps[i].0.end));
        let mut ranges = vec![0..0; reps.len()];
        let mut inserted = vec![0; reps.len()];
        let mut new_starts = vec![0; reps.len()];
        let mut spans = Vec::with_capacity(reps.len());
        let (mut prev_end, mut delta) = (0, 0isize);
        for &i in &order {
            let r = &reps[i].0;
            let s = r.start.max(prev_end).min(len);
            let e = r.end.min(len).max(s);
            inserted[i] = reps[i].1.chars().count();
            new_starts[i] = (s as isize + delta) as usize;
            delta += inserted[i] as isize - (e - s) as isize;
            spans.push((s, e, inserted[i]));
            ranges[i] = s..e;
            prev_end = e;
        }
        let mut changes = Vec::new();
        let mut edits = Vec::new();
        for &i in order.iter().rev() {
            let (range, text) = (ranges[i].clone(), reps[i].1);
            if range.is_empty() && text.is_empty() {
                continue;
            }
            let removed = self.buffer.rope().slice(range.clone()).to_string();
            changes.push(self.buffer.replace(range.clone(), text));
            edits.push(UndoEdit {
                at: range.start,
                removed,
                inserted: text.to_string(),
            });
        }
        Applied {
            changes,
            edits,
            spans,
            new_starts,
        }
    }

    /// Clamps, sorts and merges overlapping carets, and drops the goal columns.
    fn normalize(&mut self) {
        self.normalize_with_goals(Vec::new());
    }

    /// Like [`Self::normalize`], with `goals` (one per caret, in `carets` order) kept for the
    /// carets they belong to; a merged caret keeps the first one's.
    fn normalize_with_goals(&mut self, goals: Vec<Option<usize>>) {
        let buf = &self.buffer;
        let mut carets: Vec<(Caret, Option<usize>)> = self
            .carets
            .drain(..)
            .zip(goals.into_iter().chain(std::iter::repeat(None)))
            .map(|(mut c, goal)| {
                c.head = buf.clamp(c.head);
                c.anchor = buf.clamp(c.anchor);
                (c, goal)
            })
            .collect();
        carets.sort_by_key(|(c, _)| c.range());
        let mut out: Vec<Caret> = Vec::with_capacity(carets.len());
        let mut out_goals: Vec<Option<usize>> = Vec::with_capacity(carets.len());
        for (c, goal) in carets {
            if let Some(last) = out.last_mut() {
                let ((ls, le), (s, e)) = (last.range(), c.range());
                if s < le || (s == le && (ls == le || s == e)) {
                    let hi = le.max(e);
                    *last = if c.head < c.anchor {
                        Caret {
                            head: ls,
                            anchor: hi,
                        }
                    } else {
                        Caret {
                            head: hi,
                            anchor: ls,
                        }
                    };
                    continue;
                }
            }
            out.push(c);
            out_goals.push(goal);
        }
        if out.is_empty() {
            out.push(Caret::at(Pos::default()));
        }
        self.carets = out;
        self.goals = if out_goals.iter().any(Option::is_some) {
            out_goals
        } else {
            Vec::new()
        };
    }

    fn move_carets(&mut self, motion: &Motion, select: bool) {
        let buf = &self.buffer;
        let is_vertical = matches!(
            motion,
            Motion::Up | Motion::Down | Motion::PageUp(_) | Motion::PageDown(_)
        );
        let old_goals = std::mem::take(&mut self.goals);
        let mut goals = Vec::new();
        for (i, c) in self.carets.iter_mut().enumerate() {
            let collapsing = !select && !c.is_empty();
            let (lo, hi) = c.range();
            let h = c.head;
            let goal = old_goals.get(i).copied().flatten().unwrap_or(h.col);
            if is_vertical {
                goals.push(Some(goal));
            }
            let vertical = |line: usize| buf.clamp(Pos { line, col: goal });
            let head = match motion {
                Motion::Left if collapsing => lo,
                Motion::Right if collapsing => hi,
                Motion::Left => prev_pos(buf, h),
                Motion::Right => next_pos(buf, h),
                Motion::Up => vertical(h.line.saturating_sub(1)),
                Motion::Down => vertical(h.line + 1),
                Motion::PageUp(n) => vertical(h.line.saturating_sub(*n)),
                Motion::PageDown(n) => vertical(h.line + n),
                Motion::WordLeft => word_left(buf, h),
                Motion::WordRight => word_right(buf, h),
                Motion::LineStart => Pos { col: 0, ..h },
                Motion::LineEnd => Pos {
                    col: buf.line_len_chars(h.line),
                    ..h
                },
                Motion::DocStart => Pos::default(),
                Motion::DocEnd => buf.clamp(Pos {
                    line: usize::MAX,
                    col: usize::MAX,
                }),
            };
            c.head = head;
            if !select {
                c.anchor = head;
            }
        }
        self.normalize_with_goals(goals);
        self.undo.close();
    }

    /// Lines touched by any caret, ascending; a selection ending at col 0 skips that line.
    fn touched_lines(&self) -> Vec<usize> {
        let mut lines: Vec<usize> = Vec::new();
        for c in &self.carets {
            let (s, e) = c.range();
            let last = if e.line > s.line && e.col == 0 {
                e.line - 1
            } else {
                e.line
            };
            let from = lines.last().map_or(s.line, |&l| s.line.max(l + 1));
            lines.extend(from..=last);
        }
        lines
    }

    fn line_start(&self, line: usize) -> usize {
        self.buffer.rope().line_to_char(line)
    }

    /// Leading spaces and tabs of `line`, in chars.
    fn indent_len(&self, line: usize) -> usize {
        let slice = self.buffer.rope().line(line);
        slice.chars().take_while(|&c| c == ' ' || c == '\t').count()
    }

    fn tab(&mut self, now: f64) -> Vec<Change> {
        let indent = self.indent.clone();
        let cols = indent.len().max(1);
        if self.carets.iter().any(|c| c.head.line != c.anchor.line) {
            let reps = self
                .touched_lines()
                .into_iter()
                .map(|l| {
                    let s = self.line_start(l);
                    (s..s, indent.as_str())
                })
                .collect();
            return self.replace_mapped(reps, now);
        }
        self.run(GroupKind::Other, now, None, |b, c| {
            let col = c.range().0.col;
            Plan::put(
                char_range(b, c),
                if indent == "\t" {
                    "\t".into()
                } else {
                    " ".repeat(cols - col % cols)
                },
            )
        })
    }

    fn back_tab(&mut self, now: f64) -> Vec<Change> {
        let reps = self
            .touched_lines()
            .into_iter()
            .filter_map(|l| {
                let line = self.buffer.rope().line(l);
                let n = if line.chars().next() == Some('\t') {
                    1
                } else {
                    let spaces = line.chars().take_while(|&c| c == ' ');
                    spaces.take(self.indent.len().max(1)).count()
                };
                let s = self.line_start(l);
                (n > 0).then_some((s..s + n, ""))
            })
            .collect();
        self.replace_mapped(reps, now)
    }

    fn toggle_comment(&mut self, now: f64) -> Vec<Change> {
        let token = self.line_comment.clone();
        if token.is_empty() {
            self.undo.close();
            return Vec::new();
        }
        let token_len = token.chars().count();
        // (line, indent) of the non-blank touched lines, and whether each is commented.
        let mut lines = Vec::new();
        let mut all_commented = true;
        for l in self.touched_lines() {
            let indent = self.indent_len(l);
            if indent == self.buffer.line_len_chars(l) {
                continue;
            }
            let slice = self.buffer.rope().line(l);
            all_commented &= slice.chars().skip(indent).take(token_len).eq(token.chars());
            lines.push((l, indent));
        }
        if lines.is_empty() {
            self.undo.close();
            return Vec::new();
        }
        let with_space = format!("{token} ");
        let reps = if all_commented {
            lines
                .iter()
                .map(|&(l, indent)| {
                    let s = self.line_start(l) + indent;
                    let space = self.buffer.rope().get_char(s + token_len) == Some(' ');
                    (s..s + token_len + usize::from(space), "")
                })
                .collect()
        } else {
            let min = lines.iter().map(|&(_, i)| i).min().unwrap_or(0);
            lines
                .iter()
                .map(|&(l, _)| {
                    let s = self.line_start(l) + min;
                    (s..s, with_space.as_str())
                })
                .collect()
        };
        self.replace_mapped(reps, now)
    }
}

fn char_range(buf: &TextBuffer, c: &Caret) -> Range<usize> {
    let (a, b) = c.range();
    buf.char_index(a)..buf.char_index(b)
}

fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn closing_of(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '\'' | '"' => Some(open),
        _ => None,
    }
}

fn is_empty_pair(l: char, r: char) -> bool {
    matches!((l, r), ('(', ')') | ('[', ']') | ('\'', '\'') | ('"', '"'))
}

/// The char right after `p` on its line.
fn next_in_line(buf: &TextBuffer, p: Pos) -> Option<char> {
    (p.col < buf.line_len_chars(p.line)).then(|| buf.rope().char(buf.char_index(p)))
}

/// The char right before `p` on its line.
fn prev_in_line(buf: &TextBuffer, p: Pos) -> Option<char> {
    (p.col > 0).then(|| buf.rope().char(buf.char_index(p) - 1))
}

fn prev_pos(buf: &TextBuffer, p: Pos) -> Pos {
    if p.col > 0 {
        Pos {
            col: p.col - 1,
            ..p
        }
    } else if p.line > 0 {
        Pos {
            line: p.line - 1,
            col: buf.line_len_chars(p.line - 1),
        }
    } else {
        p
    }
}

fn next_pos(buf: &TextBuffer, p: Pos) -> Pos {
    if p.col < buf.line_len_chars(p.line) {
        Pos {
            col: p.col + 1,
            ..p
        }
    } else if p.line + 1 < buf.len_lines() {
        Pos {
            line: p.line + 1,
            col: 0,
        }
    } else {
        p
    }
}

/// Start of the word (or punctuation run) before `p`, skipping whitespace; the previous line's
/// end from col 0.
fn word_left(buf: &TextBuffer, p: Pos) -> Pos {
    if p.col == 0 {
        return prev_pos(buf, p);
    }
    let mut chars = buf.rope().chars_at(buf.char_index(p));
    let mut col = p.col;
    let mut class = None;
    while col > 0 {
        let Some(c) = chars.prev() else { break };
        if !step_word(c, &mut class) {
            break;
        }
        col -= 1;
    }
    Pos { col, ..p }
}

/// End of the word (or punctuation run) after `p`, skipping whitespace; the next line's start
/// from the line end.
fn word_right(buf: &TextBuffer, p: Pos) -> Pos {
    let len = buf.line_len_chars(p.line);
    if p.col == len {
        return next_pos(buf, p);
    }
    let mut chars = buf.rope().chars_at(buf.char_index(p));
    let mut col = p.col;
    let mut class = None;
    while col < len {
        let Some(c) = chars.next() else { break };
        if !step_word(c, &mut class) {
            break;
        }
        col += 1;
    }
    Pos { col, ..p }
}

/// One step of a word motion: whitespace before the run is skipped; the run is the chars
/// of the first non-whitespace char's class (word or punctuation). False stops the motion.
fn step_word(c: char, class: &mut Option<bool>) -> bool {
    if c.is_whitespace() {
        return class.is_none();
    }
    let k = is_word_char(c);
    *class.get_or_insert(k) == k
}

fn type_plan(buf: &TextBuffer, c: &Caret, text: &str, single: Option<char>) -> Plan {
    let range = char_range(buf, c);
    if let Some(ch) = single {
        let close = closing_of(ch);
        if !c.is_empty() {
            if let Some(close) = close {
                let inner = buf.rope().slice(range.clone()).to_string();
                let n = range.len();
                let (anchor, head) = if c.head < c.anchor {
                    (1 + n, 1)
                } else {
                    (1, 1 + n)
                };
                return Plan {
                    range,
                    text: format!("{ch}{inner}{close}"),
                    anchor,
                    head,
                };
            }
        } else {
            let (next, prev) = (next_in_line(buf, c.head), prev_in_line(buf, c.head));
            if matches!(ch, ')' | ']' | '\'' | '"') && next == Some(ch) {
                return Plan::step(range.start, 1);
            }
            let quote_ok =
                !matches!(ch, '\'' | '"') || !prev.is_some_and(|p| is_word_char(p) || p == ch);
            let next_ok = next.is_none_or(|n| n.is_whitespace() || n == ')' || n == ']');
            if let Some(close) = close
                && quote_ok
                && next_ok
            {
                return Plan {
                    range,
                    text: format!("{ch}{close}"),
                    anchor: 1,
                    head: 1,
                };
            }
        }
    }
    Plan::put(range, text.to_string())
}

/// Deletes the selection, or the range `around(caret)` when there is none.
fn delete_plan(
    buf: &TextBuffer,
    c: &Caret,
    around: impl Fn(&TextBuffer, Pos) -> Range<usize>,
) -> Plan {
    let range = if c.is_empty() {
        around(buf, c.head)
    } else {
        char_range(buf, c)
    };
    Plan::put(range, String::new())
}

fn newline_plan(buf: &TextBuffer, c: &Caret, indent_unit: &str) -> Plan {
    let (start, end) = c.range();
    let indent: String = buf
        .rope()
        .line(start.line)
        .chars()
        .take(start.col)
        .take_while(|&ch| ch == ' ' || ch == '\t')
        .collect();
    // The last non-space char before the caret on its line.
    let mut chars = buf.rope().chars_at(buf.char_index(start));
    let opens = (0..start.col)
        .map_while(|_| chars.prev())
        .find(|&ch| ch != ' ' && ch != '\t')
        == Some('(');
    let mut text = format!("\n{indent}");
    if opens {
        text.push_str(indent_unit);
    }
    let caret = text.chars().count();
    if opens && next_in_line(buf, end) == Some(')') {
        text.push('\n');
        text.push_str(&indent);
    }
    Plan {
        range: char_range(buf, c),
        text,
        anchor: caret,
        head: caret,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    fn text(m: &EditorModel) -> String {
        m.buffer().rope().to_string()
    }

    fn ty(s: &str) -> EditCommand {
        EditCommand::Type(s.into())
    }

    fn mv(motion: Motion) -> EditCommand {
        EditCommand::Move {
            motion,
            select: false,
        }
    }

    fn sel(anchor: Pos, head: Pos) -> Caret {
        Caret { head, anchor }
    }

    fn model_at(t: &str, at: Pos) -> EditorModel {
        let mut m = EditorModel::new(t);
        m.set_carets(vec![Caret::at(at)]);
        m
    }

    #[test]
    fn multibyte_and_crlf_editing() {
        let mut m = EditorModel::new("è日🚀\r\nx");
        for _ in 0..3 {
            m.apply(mv(Motion::Right), 0.0);
        }
        m.apply(ty("!"), 0.0);
        assert_eq!(m.buffer().line(0), "è日🚀!");
        assert_eq!(text(&m), "è日🚀!\r\nx");
        assert_eq!(m.primary().head, p(0, 4));
        m.apply(mv(Motion::Right), 0.0);
        assert_eq!(m.primary().head, p(1, 0));
        m.apply(mv(Motion::Left), 0.0);
        assert_eq!(m.primary().head, p(0, 4));
        // Backspace at a line start removes the whole "\r\n".
        m.set_carets(vec![Caret::at(p(1, 0))]);
        m.apply(EditCommand::Backspace, 0.0);
        assert_eq!(text(&m), "è日🚀!x");
        assert_eq!(m.primary().head, p(0, 4));
    }

    #[test]
    fn multi_caret_type() {
        let mut m = EditorModel::new("a\nb\nc");
        m.set_carets(vec![
            Caret::at(p(0, 0)),
            Caret::at(p(1, 0)),
            Caret::at(p(2, 0)),
        ]);
        m.apply(ty("-- "), 0.0);
        assert_eq!(text(&m), "-- a\n-- b\n-- c");
        let heads: Vec<Pos> = m.carets().iter().map(|c| c.head).collect();
        assert_eq!(heads, [p(0, 3), p(1, 3), p(2, 3)]);
    }

    #[test]
    fn overlapping_carets_merge() {
        let mut m = EditorModel::new("abc");
        m.set_carets(vec![
            Caret::at(p(0, 1)),
            sel(p(0, 0), p(0, 2)),
            Caret::at(p(0, 1)),
        ]);
        assert_eq!(m.carets(), [sel(p(0, 0), p(0, 2))]);
        m.set_carets(vec![Caret::at(p(0, 1)), Caret::at(p(0, 2))]);
        m.apply(EditCommand::Backspace, 0.0);
        assert_eq!(text(&m), "c");
        assert_eq!(m.carets(), [Caret::at(p(0, 0))]);
    }

    #[test]
    fn pairs_and_overtype() {
        let mut m = EditorModel::new("");
        m.apply(ty("("), 0.0);
        assert_eq!(text(&m), "()");
        assert_eq!(m.primary().head, p(0, 1));
        m.apply(ty(")"), 0.1);
        assert_eq!(text(&m), "()");
        assert_eq!(m.primary().head, p(0, 2));

        let mut m = model_at("it", p(0, 2));
        m.apply(ty("'"), 0.0);
        assert_eq!(text(&m), "it'");

        let mut m = model_at("x = ", p(0, 4));
        m.apply(ty("'"), 0.0);
        assert_eq!(text(&m), "x = ''");
        assert_eq!(m.primary().head, p(0, 5));
        m.apply(ty("'"), 0.1);
        assert_eq!(text(&m), "x = ''");
        assert_eq!(m.primary().head, p(0, 6));

        // No pair before a word char.
        let mut m = model_at("ab", p(0, 0));
        m.apply(ty("["), 0.0);
        assert_eq!(text(&m), "[ab");
    }

    #[test]
    fn wrap_selection_in_quotes() {
        let mut m = EditorModel::new("abc");
        m.set_carets(vec![sel(p(0, 0), p(0, 3))]);
        m.apply(ty("'"), 0.0);
        assert_eq!(text(&m), "'abc'");
        assert_eq!(m.selected_text(), "abc");
        m.apply(ty("("), 0.0);
        assert_eq!(text(&m), "'(abc)'");
    }

    #[test]
    fn backspace_deletes_empty_pair() {
        let mut m = EditorModel::new("");
        m.apply(ty("("), 0.0);
        m.apply(EditCommand::Backspace, 0.1);
        assert_eq!(text(&m), "");
        let mut m = model_at("x ''", p(0, 3));
        m.apply(EditCommand::Backspace, 0.0);
        assert_eq!(text(&m), "x ");
        let mut m = model_at("(a)", p(0, 2));
        m.apply(EditCommand::Backspace, 0.0);
        assert_eq!(text(&m), "()");
    }

    #[test]
    fn newline_auto_indents() {
        let mut m = model_at("    select (", p(0, 12));
        m.apply(EditCommand::Newline, 0.0);
        assert_eq!(m.buffer().line(1), " ".repeat(8));
        assert_eq!(m.primary().head, p(1, 8));

        let mut m = model_at("()", p(0, 1));
        m.apply(EditCommand::Newline, 0.0);
        assert_eq!(text(&m), "(\n    \n)");
        assert_eq!(m.primary().head, p(1, 4));

        let mut m = model_at("  ab", p(0, 3));
        m.apply(EditCommand::Newline, 0.0);
        assert_eq!(text(&m), "  a\n  b");
    }

    #[test]
    fn tab_and_backtab_block() {
        let mut m = EditorModel::new("a\nb");
        m.set_carets(vec![sel(p(0, 0), p(1, 1))]);
        m.apply(EditCommand::Tab, 0.0);
        assert_eq!(text(&m), "    a\n    b");
        m.apply(EditCommand::BackTab, 0.0);
        assert_eq!(text(&m), "a\nb");
        assert_eq!(m.carets(), [sel(p(0, 0), p(1, 1))]);

        let mut m = model_at("ab", p(0, 1));
        m.apply(EditCommand::Tab, 0.0);
        assert_eq!(text(&m), "a   b");
        assert_eq!(m.primary().head, p(0, 4));

        let mut m = model_at("\t  x", p(0, 3));
        m.apply(EditCommand::BackTab, 0.0);
        assert_eq!(text(&m), "  x");
    }

    #[test]
    fn toggle_comment_round_trip() {
        let original = "  a\n\n    b";
        let mut m = EditorModel::new(original);
        m.set_carets(vec![sel(p(0, 0), p(2, 5))]);
        m.apply(EditCommand::ToggleComment, 0.0);
        assert_eq!(text(&m), "  -- a\n\n  --   b");
        m.apply(EditCommand::ToggleComment, 0.0);
        assert_eq!(text(&m), original);

        let mut m = model_at("x", p(0, 0));
        m.set_line_comment("#");
        m.apply(EditCommand::ToggleComment, 0.0);
        assert_eq!(text(&m), "# x");
    }

    #[test]
    fn word_motions() {
        let mut m = model_at("select foo.bar", p(0, 0));
        let mut heads = Vec::new();
        for _ in 0..4 {
            m.apply(mv(Motion::WordRight), 0.0);
            heads.push(m.primary().head.col);
        }
        assert_eq!(heads, [6, 10, 11, 14]);
        heads.clear();
        for _ in 0..4 {
            m.apply(mv(Motion::WordLeft), 0.0);
            heads.push(m.primary().head.col);
        }
        assert_eq!(heads, [11, 10, 7, 0]);

        // Word motions cross line ends, and a selection extends.
        let mut m = model_at("ab\ncd", p(0, 2));
        m.apply(mv(Motion::WordRight), 0.0);
        assert_eq!(m.primary().head, p(1, 0));
        m.apply(
            EditCommand::Move {
                motion: Motion::WordRight,
                select: true,
            },
            0.0,
        );
        assert_eq!(m.selected_text(), "cd");

        let mut m = model_at("ab cd", p(0, 5));
        m.apply(EditCommand::DeleteWordBack, 0.0);
        assert_eq!(text(&m), "ab ");
    }

    #[test]
    fn vertical_and_document_motions() {
        let mut m = model_at("abcd\nx\nefgh", p(0, 3));
        m.apply(mv(Motion::Down), 0.0);
        assert_eq!(m.primary().head, p(1, 1));
        m.apply(mv(Motion::DocEnd), 0.0);
        assert_eq!(m.primary().head, p(2, 4));
        m.apply(mv(Motion::PageUp(10)), 0.0);
        assert_eq!(m.primary().head, p(0, 4));
        m.apply(mv(Motion::LineStart), 0.0);
        assert_eq!(m.primary().head, p(0, 0));
        m.apply(EditCommand::SelectAll, 0.0);
        assert_eq!(m.selected_text(), "abcd\nx\nefgh");
        m.apply(mv(Motion::Left), 0.0);
        assert_eq!(m.carets(), [Caret::at(p(0, 0))]);
    }

    #[test]
    fn vertical_moves_keep_a_goal_column() {
        let lines = "select * from users where\n\nab\nselect id, name from users";
        let heads = |m: &EditorModel| m.carets().iter().map(|c| c.head).collect::<Vec<_>>();
        // Through a blank and a short line, the caret comes back to column 20.
        let mut m = model_at(lines, p(0, 20));
        for (motion, want) in [
            (Motion::Down, p(1, 0)),
            (Motion::Down, p(2, 2)),
            (Motion::Down, p(3, 20)),
            (Motion::Up, p(2, 2)),
            (Motion::PageUp(2), p(0, 20)),
            (Motion::PageDown(3), p(3, 20)),
        ] {
            m.apply(mv(motion.clone()), 0.0);
            assert_eq!(m.primary().head, want, "after {motion:?}");
        }
        // Selecting keeps it too.
        m.apply(
            EditCommand::Move {
                motion: Motion::Up,
                select: true,
            },
            0.0,
        );
        m.apply(
            EditCommand::Move {
                motion: Motion::Up,
                select: true,
            },
            0.0,
        );
        m.apply(mv(Motion::PageUp(1)), 0.0);
        assert_eq!(m.primary().head, p(0, 20));

        // A horizontal motion sets a new goal.
        let mut m = model_at(lines, p(0, 20));
        m.apply(mv(Motion::Down), 0.0);
        m.apply(mv(Motion::Down), 0.0);
        m.apply(mv(Motion::Left), 0.0);
        m.apply(mv(Motion::Down), 0.0);
        assert_eq!(m.primary().head, p(3, 1));

        // So does an edit, and so does placing the caret.
        let mut m = model_at(lines, p(0, 20));
        m.apply(mv(Motion::Down), 0.0);
        m.apply(ty("x"), 0.0);
        m.apply(mv(Motion::Down), 0.0);
        m.apply(mv(Motion::Down), 0.0);
        assert_eq!(m.primary().head, p(3, 1));
        let mut m = model_at(lines, p(0, 20));
        m.apply(mv(Motion::Down), 0.0);
        m.set_carets(vec![Caret::at(p(2, 1))]);
        m.apply(mv(Motion::Down), 0.0);
        assert_eq!(m.primary().head, p(3, 1));

        // Each caret has its own goal.
        let mut m =
            EditorModel::new("abcdefghijklmnopqrstuvwxyz\nabcdefgh\nabcdefghijklmnopqrstuvwxyz");
        m.set_carets(vec![Caret::at(p(0, 4)), Caret::at(p(0, 20))]);
        m.apply(mv(Motion::Down), 0.0);
        assert_eq!(heads(&m), [p(1, 4), p(1, 8)]);
        m.apply(mv(Motion::Down), 0.0);
        assert_eq!(heads(&m), [p(2, 4), p(2, 20)]);
    }

    #[test]
    fn add_caret_below_and_collapse() {
        let mut m = model_at("abc\nde\nfgh", p(0, 2));
        m.apply(EditCommand::AddCaretBelow, 0.0);
        m.apply(EditCommand::AddCaretBelow, 0.0);
        let heads: Vec<Pos> = m.carets().iter().map(|c| c.head).collect();
        assert_eq!(heads, [p(0, 2), p(1, 2), p(2, 2)]);
        m.apply(ty("x"), 0.0);
        assert_eq!(text(&m), "abxc\ndex\nfgxh");
        m.apply(EditCommand::CollapseCarets, 0.0);
        assert_eq!(m.carets(), [Caret::at(p(2, 3))]);
        m.apply(EditCommand::AddCaretAbove, 0.0);
        m.apply(EditCommand::AddCaretAbove, 0.0);
        let heads: Vec<Pos> = m.carets().iter().map(|c| c.head).collect();
        assert_eq!(heads, [p(0, 3), p(1, 3), p(2, 3)]);
        m.apply(EditCommand::AddCaret(p(0, 0)), 0.0);
        assert_eq!(m.carets().len(), 4);
        assert_eq!(m.primary().head, p(2, 3));
    }

    #[test]
    fn replace_range_and_insert_at_carets() {
        let mut m = EditorModel::new("select fr");
        m.replace_range(7..9, "from", 0.0);
        assert_eq!(text(&m), "select from");
        assert_eq!(m.carets(), [Caret::at(p(0, 11))]);
        m.set_carets(vec![Caret::at(p(0, 0)), Caret::at(p(0, 6))]);
        let before = m.revision();
        m.insert_at_carets("(", 0.0);
        assert_eq!(text(&m), "(select( from");
        assert!(m.revision() > before);
        m.apply(EditCommand::Undo, 0.0);
        assert_eq!(text(&m), "select from");
    }
}
