//! The edit model: a [`TextBuffer`], sorted multi-carets and grouped undo, driven by
//! [`EditCommand`]s. Editing commands read only the chars around each caret, never whole
//! lines, so a keystroke costs the same on a 1 MB line as on a short one.

use std::ops::Range;

use crate::editor::buffer::{Change, Pos, TextBuffer};
use crate::editor::find::FindOptions;
use crate::editor::undo::{GroupKind, Record, UndoEdit, UndoStack};

/// The indent unit.
pub const INDENT: &str = "    ";
/// Select all occurrences stops at this many carets.
pub const MAX_OCCURRENCE_CARETS: usize = 10_000;
/// The caret history keeps this many caret sets; the oldest are dropped.
pub const CARET_HISTORY_LIMIT: usize = 100;

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
    Move {
        motion: Motion,
        select: bool,
    },
    SelectAll,
    AddCaret(Pos),
    AddCaretAbove,
    AddCaretBelow,
    CollapseCarets,
    /// Selects the word around an empty primary caret, else adds the next match of the
    /// primary selection (found with these options) as the new primary selection.
    AddNextOccurrence(FindOptions),
    /// Selects every match of the word or selection, as [`Self::AddNextOccurrence`] finds them.
    SelectAllOccurrences(FindOptions),
    /// Restores the carets from before the latest caret-adding command.
    UndoCursor,
    AddCaretsToLineEnds,
    Undo,
    Redo,
    ToggleComment,
    MoveLineUp,
    MoveLineDown,
    CopyLineUp,
    CopyLineDown,
    DeleteLine,
    SelectLine,
    IndentLine,
    OutdentLine,
}

pub struct EditorModel {
    buffer: TextBuffer,
    /// Sorted, non-overlapping, at least one.
    carets: Vec<Caret>,
    /// Index in `carets` of the primary caret: the last caret given to `set_carets`, or the
    /// one a caret-adding command made primary (a next occurrence may wrap above the
    /// others). Edits and motions keep it on the same caret.
    primary: usize,
    /// Caret sets (with their primary) replaced by caret-adding commands, newest last.
    caret_history: Vec<(Vec<Caret>, usize)>,
    /// The word the last occurrence command started from: while it is still the primary
    /// selection, further occurrences match it as a whole word with the same case.
    occurrence_word: Option<String>,
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
            primary: 0,
            caret_history: Vec::new(),
            occurrence_word: None,
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
        self.carets[self.primary]
    }

    /// Replaces the carets (clamped, sorted, merged), the last one given primary; closes the
    /// undo group. Placing a single caret starts the caret history again.
    pub fn set_carets(&mut self, carets: Vec<Caret>) {
        self.primary = carets.len().saturating_sub(1);
        self.carets = carets;
        self.normalize();
        self.undo.close();
        self.occurrence_word = None;
        if self.carets.len() == 1 {
            self.caret_history.clear();
        }
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
        if !matches!(
            cmd,
            C::AddNextOccurrence(_) | C::SelectAllOccurrences(_) | C::UndoCursor
        ) {
            self.occurrence_word = None;
        }
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
                self.add_carets(carets, self.primary);
                Vec::new()
            }
            C::AddCaretAbove | C::AddCaretBelow => {
                // Keep growing from the outermost caret, at the primary's column. A caret
                // added below becomes primary.
                let col = self.primary().head.col;
                let mut carets = self.carets.clone();
                let mut primary = self.primary;
                if cmd == C::AddCaretAbove {
                    if let Some(line) = carets[0].head.line.checked_sub(1) {
                        carets.push(Caret::at(Pos { line, col }));
                    }
                } else {
                    let line = carets[carets.len() - 1].head.line + 1;
                    if line < self.buffer.len_lines() {
                        primary = carets.len();
                        carets.push(Caret::at(Pos { line, col }));
                    }
                }
                self.add_carets(carets, primary);
                Vec::new()
            }
            C::AddNextOccurrence(opts) => {
                self.add_next_occurrence(opts);
                Vec::new()
            }
            C::SelectAllOccurrences(opts) => {
                self.select_all_occurrences(opts);
                Vec::new()
            }
            C::UndoCursor => {
                if let Some((carets, primary)) = self.caret_history.pop() {
                    self.carets = carets;
                    self.primary = primary;
                    self.normalize();
                }
                self.undo.close();
                Vec::new()
            }
            C::AddCaretsToLineEnds => {
                let carets: Vec<Caret> = self
                    .touched_lines()
                    .into_iter()
                    .map(|line| {
                        Caret::at(Pos {
                            line,
                            col: self.buffer.line_len_chars(line),
                        })
                    })
                    .collect();
                let primary = carets.len() - 1;
                self.add_carets(carets, primary);
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
                self.primary = carets.len().saturating_sub(1);
                self.carets = carets;
                self.normalize();
                self.caret_history.clear();
                changes
            }
            C::ToggleComment => self.toggle_comment(now),
            C::MoveLineUp => self.move_lines(true, now),
            C::MoveLineDown => self.move_lines(false, now),
            C::CopyLineUp => self.copy_lines(true, now),
            C::CopyLineDown => self.copy_lines(false, now),
            C::DeleteLine => self.delete_lines(now),
            C::SelectLine => {
                self.select_lines();
                Vec::new()
            }
            C::IndentLine => self.indent_lines(now),
            C::OutdentLine => self.back_tab(now),
        }
    }

    /// Replaces `chars` with `text` as one undo group; a single caret lands after the insert.
    pub fn replace_range(&mut self, chars: Range<usize>, text: &str, now: f64) -> Vec<Change> {
        let snap = |c: usize| self.buffer.char_index(self.buffer.pos_of_char(c));
        let (a, b) = (snap(chars.start), snap(chars.end));
        let (a, b) = (a.min(b), a.max(b));
        let before = self.carets.clone();
        let applied = self.replace_many(&[(a..b, text)]);
        let end = applied.new_starts[0] + text.chars().count();
        self.carets = vec![Caret::at(self.buffer.pos_of_char(end))];
        self.primary = 0;
        self.goals.clear();
        self.record(GroupKind::Other, before, applied.edits, now, None);
        applied.changes
    }

    /// Applies several non-overlapping replacements as one undo group. The primary caret
    /// lands after the last replacement in document order.
    pub fn replace_ranges(&mut self, ranges: &[(Range<usize>, String)], now: f64) -> Vec<Change> {
        if ranges.is_empty() {
            return Vec::new();
        }
        let before = self.carets.clone();
        let inserted: Vec<usize> = ranges
            .iter()
            .map(|(_, text)| text.chars().count())
            .collect();
        let reps: Vec<(Range<usize>, &str)> = ranges
            .iter()
            .map(|(range, text)| (range.clone(), text.as_str()))
            .collect();
        let applied = self.replace_many(&reps);
        let end = applied
            .new_starts
            .iter()
            .zip(inserted)
            .map(|(start, len)| start + len)
            .max()
            .unwrap_or(0);
        self.carets = vec![Caret::at(self.buffer.pos_of_char(end))];
        self.primary = 0;
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
        let reps: Vec<(Range<usize>, &str)> = plans
            .iter()
            .map(|p| (p.range.clone(), p.text.as_str()))
            .collect();
        let applied = self.replace_many(&reps);
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
        reps: &[(Range<usize>, &str)],
        now: f64,
    ) -> Vec<Change> {
        let before = self.carets.clone();
        #[expect(
            clippy::needless_collect,
            reason = "the caret char indexes must be read before `replace_many` rewrites the buffer"
        )]
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
        if !edits.is_empty() {
            self.caret_history.clear();
            self.occurrence_word = None;
        }
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
    pub(super) fn replace_many(&mut self, reps: &[(Range<usize>, &str)]) -> Applied {
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
    /// carets they belong to; a merged caret keeps the first one's. `primary` indexes the
    /// unsorted carets and follows its caret, or the caret it merges into.
    fn normalize_with_goals(&mut self, goals: Vec<Option<usize>>) {
        let buf = &self.buffer;
        let primary = self.primary.min(self.carets.len().saturating_sub(1));
        let mut carets: Vec<(Caret, Option<usize>, bool)> = self
            .carets
            .drain(..)
            .zip(goals.into_iter().chain(std::iter::repeat(None)))
            .enumerate()
            .map(|(i, (mut c, goal))| {
                c.head = buf.clamp(c.head);
                c.anchor = buf.clamp(c.anchor);
                (c, goal, i == primary)
            })
            .collect();
        carets.sort_by_key(|(c, _, _)| c.range());
        let mut out: Vec<Caret> = Vec::with_capacity(carets.len());
        let mut out_goals: Vec<Option<usize>> = Vec::with_capacity(carets.len());
        self.primary = 0;
        for (c, goal, is_primary) in carets {
            if is_primary {
                // Merged into the last kept caret, or pushed right after it.
                self.primary = out.len().saturating_sub(1);
            }
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
            if is_primary {
                self.primary = out.len();
            }
            out.push(c);
            out_goals.push(goal);
        }
        if out.is_empty() {
            out.push(Caret::at(Pos::default()));
            self.primary = 0;
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
        if self.carets.iter().any(|c| c.head.line != c.anchor.line) {
            return self.indent_lines(now);
        }
        let indent = self.indent.clone();
        let cols = indent.len().max(1);
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

    /// Inserts one indent unit at the start of every touched line.
    fn indent_lines(&mut self, now: f64) -> Vec<Change> {
        let indent = self.indent.clone();
        let reps: Vec<(Range<usize>, &str)> = self
            .touched_lines()
            .into_iter()
            .map(|l| {
                let s = self.line_start(l);
                (s..s, indent.as_str())
            })
            .collect();
        self.replace_mapped(&reps, now)
    }

    /// Replaces the carets with `carets` (`primary` indexes them); when that changes them,
    /// the replaced set goes onto the caret history.
    fn add_carets(&mut self, carets: Vec<Caret>, primary: usize) {
        let before = (self.carets.clone(), self.primary);
        self.carets = carets;
        self.primary = primary;
        self.normalize();
        self.undo.close();
        if self.carets != before.0 || self.primary != before.1 {
            if self.caret_history.len() == CARET_HISTORY_LIMIT {
                self.caret_history.remove(0);
            }
            self.caret_history.push(before);
        }
    }

    /// The word (word chars only) around `p`, selected from its start to its end.
    fn word_at(&self, p: Pos) -> Option<Caret> {
        let buf = &self.buffer;
        let at = buf.char_index(p);
        let mut left = buf.rope().chars_at(at);
        let mut start = p.col;
        while start > 0 && left.prev().is_some_and(is_word_char) {
            start -= 1;
        }
        let mut right = buf.rope().chars_at(at);
        let len = buf.line_len_chars(p.line);
        let mut end = p.col;
        while end < len && right.next().is_some_and(is_word_char) {
            end += 1;
        }
        (start < end).then_some(Caret {
            anchor: Pos { col: start, ..p },
            head: Pos { col: end, ..p },
        })
    }

    fn text_of(&self, c: &Caret) -> String {
        self.buffer.rope().slice(self.char_range(c)).to_string()
    }

    /// What the occurrence commands look for: the primary selection, or with an empty
    /// primary the word around it (`None` on no word). A word is matched whole with the same
    /// case, and so is a selection that is still the word an occurrence command started
    /// from; any other selection follows the find bar's `opts`.
    fn occurrence_query(&mut self, opts: FindOptions) -> Option<(Caret, String, FindOptions)> {
        let word_opts = FindOptions {
            case_sensitive: true,
            whole_word: true,
        };
        let primary = self.primary();
        if primary.is_empty() {
            let word = self.word_at(primary.head)?;
            let needle = self.text_of(&word);
            self.occurrence_word = Some(needle.clone());
            return Some((word, needle, word_opts));
        }
        let needle = self.text_of(&primary);
        let opts = if self.occurrence_word.as_deref() == Some(needle.as_str()) {
            word_opts
        } else {
            opts
        };
        Some((primary, needle, opts))
    }

    fn add_next_occurrence(&mut self, opts: FindOptions) {
        self.undo.close();
        let primary = self.primary();
        let Some((held, needle, opts)) = self.occurrence_query(opts) else {
            return;
        };
        if primary.is_empty() {
            // The first press only selects the word.
            let mut carets = self.carets.clone();
            carets[self.primary] = held;
            self.add_carets(carets, self.primary);
            return;
        }
        let selected: Vec<Range<usize>> = self
            .carets
            .iter()
            .filter(|c| !c.is_empty())
            .map(|c| self.char_range(c))
            .collect();
        let mut from = self.char_range(&primary).end;
        let mut seen = std::collections::HashSet::new();
        // Find wraps around, so the search ends when a match comes back.
        while let Some(r) = self.find(&needle, opts, from, false) {
            if !seen.insert(r.start) {
                return;
            }
            if selected.iter().any(|s| s.start < r.end && r.start < s.end) {
                from = r.start + 1;
                continue;
            }
            let buf = &self.buffer;
            let mut carets = self.carets.clone();
            carets.push(Caret {
                anchor: buf.pos_of_char(r.start),
                head: buf.pos_of_char(r.end),
            });
            let primary = carets.len() - 1;
            self.add_carets(carets, primary);
            return;
        }
    }

    fn select_all_occurrences(&mut self, opts: FindOptions) {
        self.undo.close();
        let Some((held, needle, opts)) = self.occurrence_query(opts) else {
            return;
        };
        let range = self.char_range(&held);
        let found = self.find_all(&needle, opts, MAX_OCCURRENCE_CARETS);
        if found.is_empty() {
            return;
        }
        let primary = found
            .iter()
            .position(|r| r.start <= range.start && range.end <= r.end)
            .unwrap_or(found.len() - 1);
        let buf = &self.buffer;
        let carets = found
            .into_iter()
            .map(|r| Caret {
                anchor: buf.pos_of_char(r.start),
                head: buf.pos_of_char(r.end),
            })
            .collect();
        self.add_carets(carets, primary);
    }

    /// The touched lines as runs of consecutive lines `(first, last)`, ascending.
    fn line_blocks(&self) -> Vec<(usize, usize)> {
        let mut blocks: Vec<(usize, usize)> = Vec::new();
        for l in self.touched_lines() {
            match blocks.last_mut() {
                Some((_, last)) if *last + 1 == l => *last = l,
                _ => blocks.push((l, l)),
            }
        }
        blocks
    }

    /// The document's line break: the first line's terminator, "\n" without one.
    fn line_break(&self) -> &'static str {
        let buf = &self.buffer;
        if buf.len_lines() > 1 && self.line_start(1) - buf.line_len_chars(0) == 2 {
            "\r\n"
        } else {
            "\n"
        }
    }

    /// Char index of the end of `line`'s text, before its terminator.
    fn line_end(&self, line: usize) -> usize {
        self.line_start(line) + self.buffer.line_len_chars(line)
    }

    /// Lines `first..=last` joined with `eol`, without a final terminator.
    fn lines_text(&self, first: usize, last: usize, eol: &str) -> String {
        let lines: Vec<_> = (first..=last).map(|l| self.buffer.line(l)).collect();
        lines.join(eol)
    }

    /// Applies `reps` (old chars, disjoint) as one undo group, then places `carets`, given
    /// in `self.carets` order in new positions so the primary stays the same caret.
    fn line_edit(
        &mut self,
        reps: &[(Range<usize>, String)],
        carets: Vec<Caret>,
        now: f64,
    ) -> Vec<Change> {
        let before = self.carets.clone();
        let reps: Vec<(Range<usize>, &str)> = reps
            .iter()
            .map(|(range, text)| (range.clone(), text.as_str()))
            .collect();
        let applied = self.replace_many(&reps);
        self.carets = carets;
        self.normalize();
        self.record(GroupKind::Other, before, applied.edits, now, None);
        applied.changes
    }

    /// Each caret with the index of the line block holding its start.
    fn caret_blocks(&self, blocks: &[(usize, usize)]) -> Vec<(Caret, usize)> {
        self.carets
            .iter()
            .map(|c| {
                let line = c.range().0.line;
                (*c, blocks.partition_point(|&(_, last)| last < line))
            })
            .collect()
    }

    /// Swaps every line block with the line above or below it; nothing at the document edge.
    fn move_lines(&mut self, up: bool, now: f64) -> Vec<Change> {
        let blocks = self.line_blocks();
        let last_line = self.buffer.len_lines() - 1;
        let at_edge = if up {
            blocks.first().is_some_and(|&(first, _)| first == 0)
        } else {
            blocks.last().is_some_and(|&(_, last)| last == last_line)
        };
        if at_edge {
            self.undo.close();
            return Vec::new();
        }
        let eol = self.line_break();
        let reps: Vec<(Range<usize>, String)> = blocks
            .iter()
            .map(|&(a, b)| {
                let block = self.lines_text(a, b, eol);
                if up {
                    let above = self.buffer.line(a - 1);
                    (
                        self.line_start(a - 1)..self.line_end(b),
                        format!("{block}{eol}{above}"),
                    )
                } else {
                    let below = self.buffer.line(b + 1);
                    (
                        self.line_start(a)..self.line_end(b + 1),
                        format!("{below}{eol}{block}"),
                    )
                }
            })
            .collect();
        let carets = self
            .carets
            .iter()
            .map(|c| shift_lines(c, if up { -1 } else { 1 }))
            .collect();
        self.line_edit(&reps, carets, now)
    }

    /// Copies every line block above or below itself; the selections go to the lower copy
    /// when copying down and stay on the upper one when copying up.
    fn copy_lines(&mut self, up: bool, now: f64) -> Vec<Change> {
        let blocks = self.line_blocks();
        let eol = self.line_break();
        let mut reps = Vec::with_capacity(blocks.len());
        // Per block, the lines copies insert above it.
        let mut inserted_above = Vec::with_capacity(blocks.len());
        let mut inserted = 0;
        for &(a, b) in &blocks {
            inserted_above.push(inserted);
            let block = self.lines_text(a, b, eol);
            reps.push(if up {
                let s = self.line_start(a);
                (s..s, format!("{block}{eol}"))
            } else {
                let e = self.line_end(b);
                (e..e, format!("{eol}{block}"))
            });
            inserted += b - a + 1;
        }
        let carets = self
            .caret_blocks(&blocks)
            .into_iter()
            .map(|(c, k)| {
                let (a, b) = blocks[k];
                let own = if up { 0 } else { b - a + 1 };
                shift_lines(&c, (inserted_above[k] + own) as isize)
            })
            .collect();
        self.line_edit(&reps, carets, now)
    }

    /// Removes every touched line with its line break; each caret lands at its column on
    /// the following line, or on the previous one past the end.
    fn delete_lines(&mut self, now: f64) -> Vec<Change> {
        let blocks = self.line_blocks();
        let (n, len) = (self.buffer.len_lines(), self.buffer.len_chars());
        let mut reps = Vec::with_capacity(blocks.len());
        let mut landing = Vec::with_capacity(blocks.len());
        let mut removed = 0;
        for &(a, b) in &blocks {
            let (range, line) = if b + 1 < n {
                (self.line_start(a)..self.line_start(b + 1), a - removed)
            } else if a > 0 {
                (self.line_end(a - 1)..len, a - 1 - removed)
            } else {
                (0..len, 0)
            };
            reps.push((range, String::new()));
            landing.push(line);
            removed += b - a + 1;
        }
        let carets = self
            .caret_blocks(&blocks)
            .into_iter()
            .map(|(c, k)| {
                Caret::at(Pos {
                    line: landing[k],
                    col: c.head.col,
                })
            })
            .collect();
        self.line_edit(&reps, carets, now)
    }

    /// Selects every caret's touched lines whole; a selection of whole lines grows by one.
    fn select_lines(&mut self) {
        let n = self.buffer.len_lines();
        let line_start = |line: usize| {
            if line < n {
                Pos { line, col: 0 }
            } else {
                Pos {
                    line: n - 1,
                    col: self.buffer.line_len_chars(n - 1),
                }
            }
        };
        let carets: Vec<Caret> = self
            .carets
            .iter()
            .map(|c| {
                let (s, e) = c.range();
                let last = if e.line > s.line && e.col == 0 {
                    e.line - 1
                } else {
                    e.line
                };
                let start = Pos {
                    line: s.line,
                    col: 0,
                };
                let mut end = line_start(last + 1);
                if (s, e) == (start, end) {
                    end = line_start(last + 2);
                }
                Caret {
                    anchor: start,
                    head: end,
                }
            })
            .collect();
        self.carets = carets;
        self.normalize();
        self.undo.close();
    }

    fn back_tab(&mut self, now: f64) -> Vec<Change> {
        let reps: Vec<(Range<usize>, &str)> = self
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
        self.replace_mapped(&reps, now)
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
        let reps: Vec<(Range<usize>, &str)> = if all_commented {
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
        self.replace_mapped(&reps, now)
    }
}

/// `c` moved `by` lines, columns unchanged.
fn shift_lines(c: &Caret, by: isize) -> Caret {
    let shift = |p: Pos| Pos {
        line: p.line.saturating_add_signed(by),
        ..p
    };
    Caret {
        head: shift(c.head),
        anchor: shift(c.anchor),
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

    fn next(opts: FindOptions) -> EditCommand {
        EditCommand::AddNextOccurrence(opts)
    }

    fn all(opts: FindOptions) -> EditCommand {
        EditCommand::SelectAllOccurrences(opts)
    }

    fn model_with(t: &str, carets: Vec<Caret>) -> EditorModel {
        let mut m = EditorModel::new(t);
        m.set_carets(carets);
        m
    }

    /// Applies `cmd`, checks the text and carets it leaves, then that one undo restores both.
    fn check_line_command(
        t: &str,
        carets: &[Caret],
        cmd: EditCommand,
        want_text: &str,
        want_carets: &[Caret],
    ) {
        let label = format!("{cmd:?} on {t:?}");
        let mut m = model_with(t, carets.to_vec());
        m.apply(cmd, 0.0);
        assert_eq!(text(&m), want_text, "{label}");
        assert_eq!(m.carets(), want_carets, "{label}");
        m.apply(EditCommand::Undo, 1.0);
        assert_eq!(text(&m), t, "undo of {label}");
        assert_eq!(m.carets(), carets, "undo of {label}");
    }

    #[test]
    fn next_occurrence_starts_from_the_word_then_matches_whole_words_with_case() {
        let t = "id, orders_id, ID, id\nid";
        let mut m = model_at(t, p(0, 1));
        m.apply(next(FindOptions::default()), 0.0);
        assert_eq!(
            m.carets(),
            [sel(p(0, 0), p(0, 2))],
            "the word, no new caret"
        );
        m.apply(next(FindOptions::default()), 0.0);
        assert_eq!(m.carets(), [sel(p(0, 0), p(0, 2)), sel(p(0, 19), p(0, 21))]);
        assert_eq!(m.primary(), sel(p(0, 19), p(0, 21)));
        m.apply(next(FindOptions::default()), 0.0);
        assert_eq!(m.primary(), sel(p(1, 0), p(1, 2)));
        assert_eq!(m.carets().len(), 3);
        // Every occurrence is selected: nothing changes.
        let before = m.carets().to_vec();
        m.apply(next(FindOptions::default()), 0.0);
        assert_eq!(m.carets(), before);
        assert_eq!(m.primary(), sel(p(1, 0), p(1, 2)));
        assert_eq!(text(&m), t);

        // A caret right after a word picks that word; a caret on no word does nothing.
        let mut m = model_at("a + bc", p(0, 6));
        m.apply(next(FindOptions::default()), 0.0);
        assert_eq!(m.carets(), [sel(p(0, 4), p(0, 6))]);
        let mut m = model_at("a + bc", p(0, 2));
        m.apply(next(FindOptions::default()), 0.0);
        assert_eq!(m.carets(), [Caret::at(p(0, 2))]);
    }

    #[test]
    fn next_occurrence_wraps_and_makes_the_match_primary() {
        let mut m = model_at("id x\nid\nid", p(1, 1));
        m.apply(next(FindOptions::default()), 0.0);
        m.apply(next(FindOptions::default()), 0.0);
        assert_eq!(m.primary(), sel(p(2, 0), p(2, 2)));
        m.apply(next(FindOptions::default()), 0.0);
        // Wrapped to the top: primary although it is first in document order.
        assert_eq!(m.primary(), sel(p(0, 0), p(0, 2)));
        assert_eq!(m.carets().len(), 3);
        // The primary survives an edit on every caret.
        m.apply(ty("k"), 1.0);
        assert_eq!(text(&m), "k x\nk\nk");
        assert_eq!(m.primary(), Caret::at(p(0, 1)));
    }

    #[test]
    fn next_occurrence_from_a_selection_follows_the_find_options() {
        let t = "Abc abc xabc";
        let mut m = model_with(t, vec![sel(p(0, 4), p(0, 7))]);
        m.apply(next(FindOptions::default()), 0.0);
        assert_eq!(m.primary(), sel(p(0, 9), p(0, 12)));
        m.apply(next(FindOptions::default()), 0.0);
        assert_eq!(m.primary(), sel(p(0, 0), p(0, 3)));
        assert_eq!(m.carets().len(), 3);

        let strict = FindOptions {
            case_sensitive: true,
            whole_word: true,
        };
        let mut m = model_with(t, vec![sel(p(0, 4), p(0, 7))]);
        m.apply(next(strict), 0.0);
        assert_eq!(m.carets(), [sel(p(0, 4), p(0, 7))]);

        // Case-insensitive but whole word: "Abc", not "xabc".
        let ww = FindOptions {
            whole_word: true,
            ..FindOptions::default()
        };
        let mut m = model_with(t, vec![sel(p(0, 4), p(0, 7))]);
        m.apply(next(ww), 0.0);
        assert_eq!(m.primary(), sel(p(0, 0), p(0, 3)));
    }

    #[test]
    fn next_occurrence_of_a_multi_line_selection_does_nothing() {
        let mut m = model_with("ab\nab\nab\nab", vec![sel(p(0, 0), p(1, 2))]);
        m.apply(next(FindOptions::default()), 0.0);
        assert_eq!(m.carets(), [sel(p(0, 0), p(1, 2))]);
        m.apply(all(FindOptions::default()), 0.0);
        assert_eq!(m.carets(), [sel(p(0, 0), p(1, 2))]);
    }

    #[test]
    fn select_all_occurrences_keeps_the_primary_match() {
        let t = "id x id\nID id orders_id";
        let mut m = model_at(t, p(0, 6));
        m.apply(all(FindOptions::default()), 0.0);
        assert_eq!(
            m.carets(),
            [
                sel(p(0, 0), p(0, 2)),
                sel(p(0, 5), p(0, 7)),
                sel(p(1, 3), p(1, 5)),
            ]
        );
        assert_eq!(m.primary(), sel(p(0, 5), p(0, 7)));
        assert_eq!(text(&m), t);
        m.apply(EditCommand::UndoCursor, 0.0);
        assert_eq!(m.carets(), [Caret::at(p(0, 6))]);

        // From a selection, with the find options: case-insensitive, substrings too.
        let mut m = model_with(t, vec![sel(p(1, 0), p(1, 2))]);
        m.apply(all(FindOptions::default()), 0.0);
        assert_eq!(m.carets().len(), 5);
        assert_eq!(m.primary(), sel(p(1, 0), p(1, 2)));
    }

    #[test]
    fn select_all_occurrences_stops_at_the_cap() {
        let t = "a\n".repeat(MAX_OCCURRENCE_CARETS + 50);
        let mut m = model_at(&t, p(0, 0));
        m.apply(all(FindOptions::default()), 0.0);
        assert_eq!(m.carets().len(), MAX_OCCURRENCE_CARETS);
        assert_eq!(m.primary(), sel(p(0, 0), p(0, 1)));
    }

    #[test]
    fn undo_cursor_restores_caret_sets_and_leaves_the_text_alone() {
        let mut m = model_at("abc\nabc\nabc", p(0, 1));
        m.apply(ty("x"), 0.0);
        m.apply(EditCommand::AddCaretBelow, 0.0);
        m.apply(EditCommand::AddCaretBelow, 0.0);
        assert_eq!(m.carets().len(), 3);
        m.apply(EditCommand::UndoCursor, 0.0);
        assert_eq!(m.carets(), [Caret::at(p(0, 2)), Caret::at(p(1, 2))]);
        m.apply(EditCommand::UndoCursor, 0.0);
        assert_eq!(m.carets(), [Caret::at(p(0, 2))]);
        // The history is empty: nothing happens, and the typed text stays.
        m.apply(EditCommand::UndoCursor, 0.0);
        assert_eq!(m.carets(), [Caret::at(p(0, 2))]);
        assert_eq!(text(&m), "axbc\nabc\nabc");
        // The text undo stack was not touched.
        m.apply(EditCommand::Undo, 1.0);
        assert_eq!(text(&m), "abc\nabc\nabc");

        // A text edit starts the history again.
        let mut m = model_at("ab\nab", p(0, 0));
        m.apply(EditCommand::AddCaret(p(1, 0)), 0.0);
        m.apply(ty("z"), 0.0);
        m.apply(EditCommand::UndoCursor, 0.0);
        assert_eq!(m.carets(), [Caret::at(p(0, 1)), Caret::at(p(1, 1))]);

        // So does placing a single caret.
        let mut m = model_at("ab\nab", p(0, 0));
        m.apply(EditCommand::AddCaret(p(1, 0)), 0.0);
        m.set_carets(vec![Caret::at(p(1, 1))]);
        m.apply(EditCommand::UndoCursor, 0.0);
        assert_eq!(m.carets(), [Caret::at(p(1, 1))]);

        // Next occurrence and line ends push too; ⌘U restores the primary as well.
        let mut m = model_at("id\nid\nid", p(2, 1));
        m.apply(next(FindOptions::default()), 0.0);
        m.apply(next(FindOptions::default()), 0.0);
        m.apply(EditCommand::AddCaretsToLineEnds, 0.0);
        assert_eq!(m.carets().len(), 2);
        m.apply(EditCommand::UndoCursor, 0.0);
        assert_eq!(m.primary(), sel(p(0, 0), p(0, 2)));
        m.apply(EditCommand::UndoCursor, 0.0);
        assert_eq!(m.carets(), [sel(p(2, 0), p(2, 2))]);
        m.apply(EditCommand::UndoCursor, 0.0);
        assert_eq!(m.carets(), [Caret::at(p(2, 1))]);
    }

    #[test]
    fn caret_history_keeps_the_latest_hundred() {
        let t = "x\n".repeat(200);
        let mut m = model_at(&t, p(0, 0));
        for _ in 0..150 {
            m.apply(EditCommand::AddCaretBelow, 0.0);
        }
        assert_eq!(m.carets().len(), 151);
        for _ in 0..120 {
            m.apply(EditCommand::UndoCursor, 0.0);
        }
        assert_eq!(m.carets().len(), 51);
    }

    #[test]
    fn move_line_up_and_down() {
        check_line_command(
            "a\nb\nc",
            &[Caret::at(p(1, 1))],
            EditCommand::MoveLineUp,
            "b\na\nc",
            &[Caret::at(p(0, 1))],
        );
        check_line_command(
            "a\nb\nc",
            &[Caret::at(p(1, 1))],
            EditCommand::MoveLineDown,
            "a\nc\nb",
            &[Caret::at(p(2, 1))],
        );
        // A selection ending at column 0 leaves that line out, and stays on the moved text.
        check_line_command(
            "a\nb\nc\nd",
            &[sel(p(0, 0), p(2, 0))],
            EditCommand::MoveLineDown,
            "c\na\nb\nd",
            &[sel(p(1, 0), p(3, 0))],
        );
        // Several carets each move their block.
        check_line_command(
            "1\n2\n3\n4\n5",
            &[Caret::at(p(0, 1)), Caret::at(p(2, 0))],
            EditCommand::MoveLineDown,
            "2\n1\n4\n3\n5",
            &[Caret::at(p(1, 1)), Caret::at(p(3, 0))],
        );
        // CRLF documents keep their line breaks.
        check_line_command(
            "a\r\nb\r\nc",
            &[Caret::at(p(2, 1))],
            EditCommand::MoveLineUp,
            "a\r\nc\r\nb",
            &[Caret::at(p(1, 1))],
        );
        // At the edges nothing happens, and no undo group is recorded.
        for (cmd, at) in [
            (EditCommand::MoveLineUp, p(0, 0)),
            (EditCommand::MoveLineDown, p(2, 0)),
        ] {
            let mut m = model_at("a\nb\nc", p(1, 0));
            m.apply(ty("x"), 0.0);
            m.set_carets(vec![Caret::at(at)]);
            m.apply(cmd, 1.0);
            assert_eq!(text(&m), "a\nxb\nc");
            m.apply(EditCommand::Undo, 2.0);
            assert_eq!(text(&m), "a\nb\nc");
        }
    }

    #[test]
    fn copy_line_down_and_up() {
        check_line_command(
            "x\ny",
            &[sel(p(0, 0), p(0, 1))],
            EditCommand::CopyLineDown,
            "x\nx\ny",
            &[sel(p(1, 0), p(1, 1))],
        );
        check_line_command(
            "x\ny",
            &[sel(p(0, 0), p(0, 1))],
            EditCommand::CopyLineUp,
            "x\nx\ny",
            &[sel(p(0, 0), p(0, 1))],
        );
        check_line_command(
            "a\nb\nc",
            &[Caret::at(p(0, 0)), Caret::at(p(2, 1))],
            EditCommand::CopyLineDown,
            "a\na\nb\nc\nc",
            &[Caret::at(p(1, 0)), Caret::at(p(4, 1))],
        );
        check_line_command(
            "a\nb\nc\nd",
            &[sel(p(0, 1), p(1, 1)), Caret::at(p(3, 1))],
            EditCommand::CopyLineUp,
            "a\nb\na\nb\nc\nd\nd",
            &[sel(p(0, 1), p(1, 1)), Caret::at(p(5, 1))],
        );
        // Adjacent lines of two carets copy as one block.
        check_line_command(
            "a\nb",
            &[Caret::at(p(0, 0)), Caret::at(p(1, 1))],
            EditCommand::CopyLineDown,
            "a\nb\na\nb",
            &[Caret::at(p(2, 0)), Caret::at(p(3, 1))],
        );
        // The last line of a CRLF document gets a CRLF break.
        check_line_command(
            "a\r\nb",
            &[Caret::at(p(1, 0))],
            EditCommand::CopyLineDown,
            "a\r\nb\r\nb",
            &[Caret::at(p(2, 0))],
        );
        check_line_command(
            "a\r\nb",
            &[Caret::at(p(0, 1))],
            EditCommand::CopyLineUp,
            "a\r\na\r\nb",
            &[Caret::at(p(0, 1))],
        );
    }

    #[test]
    fn delete_line() {
        check_line_command(
            "a\nbcd\ne",
            &[Caret::at(p(1, 3))],
            EditCommand::DeleteLine,
            "a\ne",
            &[Caret::at(p(1, 1))],
        );
        // The last line: the caret lands on the line before.
        check_line_command(
            "a\nb",
            &[Caret::at(p(1, 1))],
            EditCommand::DeleteLine,
            "a",
            &[Caret::at(p(0, 1))],
        );
        check_line_command(
            "only",
            &[Caret::at(p(0, 2))],
            EditCommand::DeleteLine,
            "",
            &[Caret::at(p(0, 0))],
        );
        check_line_command(
            "a\r\nb\r\nc\r\nd",
            &[Caret::at(p(0, 1)), Caret::at(p(2, 0))],
            EditCommand::DeleteLine,
            "b\r\nd",
            &[Caret::at(p(0, 1)), Caret::at(p(1, 0))],
        );
        // A selection ending at column 0 keeps that line.
        check_line_command(
            "a\nb\nc",
            &[sel(p(0, 1), p(1, 0))],
            EditCommand::DeleteLine,
            "b\nc",
            &[Caret::at(p(0, 0))],
        );
    }

    #[test]
    fn select_line_extends_whole_line_selections() {
        let mut m = model_at("ab\ncd\nef", p(0, 1));
        let mut steps = Vec::new();
        for _ in 0..4 {
            m.apply(EditCommand::SelectLine, 0.0);
            steps.push(m.primary());
        }
        assert_eq!(
            steps,
            [
                sel(p(0, 0), p(1, 0)),
                sel(p(0, 0), p(2, 0)),
                sel(p(0, 0), p(2, 2)),
                sel(p(0, 0), p(2, 2)),
            ]
        );
        let mut m = model_with("ab\ncd\nef", vec![sel(p(0, 1), p(1, 1))]);
        m.apply(EditCommand::SelectLine, 0.0);
        assert_eq!(m.carets(), [sel(p(0, 0), p(2, 0))]);
        let mut m = model_with("ab\ncd", vec![Caret::at(p(0, 0)), Caret::at(p(1, 1))]);
        m.apply(EditCommand::SelectLine, 0.0);
        assert_eq!(m.carets(), [sel(p(0, 0), p(1, 0)), sel(p(1, 0), p(1, 2))]);
    }

    #[test]
    fn indent_and_outdent_line_whatever_the_selection() {
        check_line_command(
            "a\nb",
            &[Caret::at(p(0, 1))],
            EditCommand::IndentLine,
            "    a\nb",
            &[Caret::at(p(0, 5))],
        );
        check_line_command(
            "a\nb\nc",
            &[Caret::at(p(0, 0)), sel(p(2, 0), p(2, 1))],
            EditCommand::IndentLine,
            "    a\nb\n    c",
            &[Caret::at(p(0, 4)), sel(p(2, 4), p(2, 5))],
        );
        check_line_command(
            "    a\n  b",
            &[Caret::at(p(0, 5)), Caret::at(p(1, 1))],
            EditCommand::OutdentLine,
            "a\nb",
            &[Caret::at(p(0, 1)), Caret::at(p(1, 0))],
        );
    }

    #[test]
    fn add_carets_to_line_ends() {
        let mut m = model_with("ab\ncde\nf", vec![sel(p(0, 1), p(2, 0))]);
        m.apply(EditCommand::AddCaretsToLineEnds, 0.0);
        assert_eq!(m.carets(), [Caret::at(p(0, 2)), Caret::at(p(1, 3))]);
        assert_eq!(m.primary(), Caret::at(p(1, 3)));
        m.apply(EditCommand::UndoCursor, 0.0);
        assert_eq!(m.carets(), [sel(p(0, 1), p(2, 0))]);

        let mut m = model_with("ab\ncde", vec![sel(p(1, 0), p(1, 1))]);
        m.apply(EditCommand::AddCaretsToLineEnds, 0.0);
        assert_eq!(m.carets(), [Caret::at(p(1, 3))]);
    }
}
