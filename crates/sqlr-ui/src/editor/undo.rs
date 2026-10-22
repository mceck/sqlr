//! Grouped undo: each group holds the forward edits it made plus the carets around them.

use std::collections::VecDeque;

use crate::editor::buffer::{Change, TextBuffer};
use crate::editor::model::Caret;

/// Typing (or backspacing) further apart than this, in seconds, starts a new undo group.
pub const UNDO_GROUP_GAP_S: f64 = 1.0;
/// At most this many undo groups are kept; the oldest are dropped.
pub const UNDO_LIMIT: usize = 10_000;

/// One buffer replacement, recorded in the order it was applied: at char `at`, `removed`
/// was replaced by `inserted`.
#[derive(Clone, Debug)]
pub(crate) struct UndoEdit {
    pub at: usize,
    pub removed: String,
    pub inserted: String,
}

/// Which commands a group may merge with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupKind {
    Type,
    Backspace,
    Other,
}

struct Group {
    kind: GroupKind,
    edits: Vec<UndoEdit>,
    carets_before: Vec<Caret>,
    carets_after: Vec<Caret>,
    /// Input time of the last edit merged in.
    time: f64,
    /// The char at the far end of the run so far (last typed, or last backspaced over).
    last_char: Option<char>,
}

/// What a recorded command looked like, for the merge rules.
pub(crate) struct Record {
    pub kind: GroupKind,
    pub edits: Vec<UndoEdit>,
    pub carets_before: Vec<Caret>,
    pub carets_after: Vec<Caret>,
    pub now: f64,
    /// The char this command adds next to the run (first typed, or the one backspaced over).
    pub first_char: Option<char>,
    /// The char it leaves at the run's far end.
    pub last_char: Option<char>,
}

#[derive(Default)]
pub(crate) struct UndoStack {
    done: VecDeque<Group>,
    undone: Vec<Group>,
    /// Whether the top group may still absorb the next `Type` / `Backspace`.
    open: bool,
}

impl UndoStack {
    /// Closes the top group: the next edit starts its own.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Records a command's edits, merging them into the open group when the rules allow.
    pub fn record(&mut self, r: Record) {
        let mergeable = r.kind != GroupKind::Other;
        let merge = mergeable
            && self.open
            && self.done.back().is_some_and(|g| {
                g.kind == r.kind
                    && r.now - g.time < UNDO_GROUP_GAP_S
                    && g.carets_after == r.carets_before
                    && !(r.first_char.is_some_and(char::is_whitespace)
                        && g.last_char.is_some_and(|c| !c.is_whitespace()))
            });
        if r.edits.is_empty() {
            // Moving over a closing char while typing keeps the run going.
            match self.done.back_mut() {
                Some(g) if merge => {
                    g.carets_after = r.carets_after;
                    g.time = r.now;
                }
                _ => self.open = false,
            }
            return;
        }
        self.undone.clear();
        if merge && let Some(g) = self.done.back_mut() {
            g.edits.extend(r.edits);
            g.carets_after = r.carets_after;
            g.time = r.now;
            g.last_char = r.last_char.or(g.last_char);
            return;
        }
        self.push_done(Group {
            kind: r.kind,
            edits: r.edits,
            carets_before: r.carets_before,
            carets_after: r.carets_after,
            time: r.now,
            last_char: r.last_char,
        });
        self.open = mergeable;
    }

    fn push_done(&mut self, g: Group) {
        if self.done.len() == UNDO_LIMIT {
            self.done.pop_front();
        }
        self.done.push_back(g);
    }

    /// Reverts the newest group; returns its changes and the carets from before it.
    pub fn undo(&mut self, buf: &mut TextBuffer) -> Option<(Vec<Change>, Vec<Caret>)> {
        self.open = false;
        let g = self.done.pop_back()?;
        let changes = g
            .edits
            .iter()
            .rev()
            .map(|e| buf.replace(e.at..e.at + e.inserted.chars().count(), &e.removed))
            .collect();
        let carets = g.carets_before.clone();
        self.undone.push(g);
        Some((changes, carets))
    }

    /// Re-applies the newest undone group; returns its changes and the carets from after it.
    pub fn redo(&mut self, buf: &mut TextBuffer) -> Option<(Vec<Change>, Vec<Caret>)> {
        self.open = false;
        let g = self.undone.pop()?;
        let changes = g
            .edits
            .iter()
            .map(|e| buf.replace(e.at..e.at + e.removed.chars().count(), &e.inserted))
            .collect();
        let carets = g.carets_after.clone();
        self.push_done(g);
        Some((changes, carets))
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::{Caret, EditCommand, EditorModel, Pos, UNDO_LIMIT};

    fn text(m: &EditorModel) -> String {
        m.buffer().rope().to_string()
    }

    fn ty(s: &str) -> EditCommand {
        EditCommand::Type(s.into())
    }

    fn line_starts() -> Vec<Caret> {
        (0..3).map(|line| Caret::at(Pos { line, col: 0 })).collect()
    }

    #[test]
    fn typing_groups_by_word_and_time() {
        let mut m = EditorModel::new("");
        for (c, t) in [("a", 0.0), ("b", 0.1), (" ", 0.2), ("c", 0.3)] {
            m.apply(ty(c), t);
        }
        assert_eq!(text(&m), "ab c");
        m.apply(EditCommand::Undo, 0.4);
        assert_eq!(text(&m), "ab");
        m.apply(EditCommand::Undo, 0.5);
        assert_eq!(text(&m), "");

        let mut m = EditorModel::new("");
        m.apply(ty("a"), 0.0);
        m.apply(ty("b"), 1.5);
        m.apply(EditCommand::Undo, 1.6);
        assert_eq!(text(&m), "a");
    }

    #[test]
    fn caret_move_closes_group() {
        let mut m = EditorModel::new("");
        m.apply(ty("a"), 0.0);
        m.apply(
            EditCommand::Move {
                motion: crate::editor::Motion::LineStart,
                select: false,
            },
            0.1,
        );
        m.apply(ty("b"), 0.2);
        assert_eq!(text(&m), "ba");
        m.apply(EditCommand::Undo, 0.3);
        assert_eq!(text(&m), "a");
    }

    #[test]
    fn backspaces_group() {
        let mut m = EditorModel::new("ab cd");
        m.set_carets(vec![Caret::at(Pos { line: 0, col: 5 })]);
        for t in [0.0, 0.1, 0.2, 0.3] {
            m.apply(EditCommand::Backspace, t);
        }
        assert_eq!(text(&m), "a");
        // Backspacing over the space after "d", "c" starts a new group.
        m.apply(EditCommand::Undo, 0.4);
        assert_eq!(text(&m), "ab ");
        m.apply(EditCommand::Undo, 0.5);
        assert_eq!(text(&m), "ab cd");
    }

    #[test]
    fn undo_restores_carets() {
        let mut m = EditorModel::new("a\nb\nc");
        m.set_carets(line_starts());
        m.apply(ty("x"), 0.0);
        let after = m.carets().to_vec();
        m.set_carets(vec![Caret::at(Pos::default())]);
        m.apply(EditCommand::Undo, 0.1);
        assert_eq!(text(&m), "a\nb\nc");
        assert_eq!(m.carets(), line_starts());
        m.apply(EditCommand::Redo, 0.2);
        assert_eq!(text(&m), "xa\nxb\nxc");
        assert_eq!(m.carets(), after);
    }

    #[test]
    fn redo_cleared_by_new_edit() {
        let mut m = EditorModel::new("");
        m.apply(ty("a"), 0.0);
        m.apply(EditCommand::Undo, 0.1);
        m.apply(ty("b"), 0.2);
        let changes = m.apply(EditCommand::Redo, 0.3);
        assert!(changes.is_empty());
        assert_eq!(text(&m), "b");
    }

    #[test]
    fn paste_is_one_group() {
        let line = format!("{}\n", "x".repeat(63));
        let big = line.repeat((1 << 20) / 64);
        let mut m = EditorModel::new("");
        m.apply(EditCommand::Paste(big.clone()), 0.0);
        assert_eq!(m.buffer().len_chars(), big.len());
        m.apply(EditCommand::Undo, 0.1);
        assert_eq!(text(&m), "");
        m.apply(EditCommand::Redo, 0.2);
        assert_eq!(m.buffer().len_chars(), big.len());
    }

    #[test]
    fn limit_drops_oldest() {
        let mut m = EditorModel::new("");
        for i in 0..=UNDO_LIMIT {
            m.apply(ty("a"), i as f64 * 2.0);
        }
        for _ in 0..=UNDO_LIMIT {
            m.apply(EditCommand::Undo, 1e9);
        }
        assert_eq!(text(&m), "a");
    }
}
