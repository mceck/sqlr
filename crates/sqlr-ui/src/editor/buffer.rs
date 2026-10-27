//! Rope-backed text buffer. Lines break at `\n` only; a `\r\n` terminator is hidden, so a
//! position can never land inside it.

use std::borrow::Cow;
use std::ops::Range;

use ropey::Rope;

/// A caret position. `col` counts chars and excludes the line terminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

/// One buffer edit, as the syntax layer and the markers need it.
///
/// `start_char..start_char + removed_chars` was replaced by `inserted_chars` chars. The edit
/// touched `old_lines` lines from `line` before it and `new_lines` lines after it;
/// `start_byte_in_line` is the byte offset of `start_char` within `line`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub start_char: usize,
    pub removed_chars: usize,
    pub inserted_chars: usize,
    pub line: usize,
    pub old_lines: usize,
    pub new_lines: usize,
    pub start_byte_in_line: usize,
}

pub struct TextBuffer {
    rope: Rope,
    version: u64,
}

impl TextBuffer {
    #[expect(
        clippy::should_implement_trait,
        reason = "an inherent infallible constructor taking `&str`; `FromStr` would force a `Result` return for an operation that cannot fail"
    )]
    pub fn from_str(s: &str) -> Self {
        Self {
            rope: Rope::from_str(s),
            version: 0,
        }
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Chars (= bytes) of the line terminator ("\n" or "\r\n") at the end of `idx`, if any.
    fn terminator_len(&self, idx: usize) -> usize {
        let line = self.rope.line(idx);
        let n = line.len_chars();
        if n == 0 || line.char(n - 1) != '\n' {
            0
        } else if n >= 2 && line.char(n - 2) == '\r' {
            2
        } else {
            1
        }
    }

    pub fn line_len_chars(&self, idx: usize) -> usize {
        self.rope.line(idx).len_chars() - self.terminator_len(idx)
    }

    /// Bytes of line `idx`, terminator excluded.
    pub fn line_len_bytes(&self, idx: usize) -> usize {
        self.rope.line(idx).len_bytes() - self.terminator_len(idx)
    }

    /// Line `idx` without its terminator; borrowed unless the line spans rope chunks.
    pub fn line(&self, idx: usize) -> Cow<'_, str> {
        let len = self.line_len_chars(idx);
        self.rope.line(idx).slice(..len).into()
    }

    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.len_lines() - 1);
        Pos {
            line,
            col: p.col.min(self.line_len_chars(line)),
        }
    }

    /// Char index of `p` (clamped) in the whole text.
    pub fn char_index(&self, p: Pos) -> usize {
        let p = self.clamp(p);
        self.rope.line_to_char(p.line) + p.col
    }

    /// Inverse of [`Self::char_index`], clamped so it never lands inside a "\r\n".
    pub fn pos_of_char(&self, c: usize) -> Pos {
        let c = c.min(self.rope.len_chars());
        let line = self.rope.char_to_line(c);
        self.clamp(Pos {
            line,
            col: c - self.rope.line_to_char(line),
        })
    }

    pub fn insert(&mut self, at: Pos, text: &str) -> Change {
        let c = self.char_index(at);
        self.replace(c..c, text)
    }

    pub fn delete(&mut self, from: Pos, to: Pos) -> Change {
        let (a, b) = (self.char_index(from), self.char_index(to));
        self.replace(a.min(b)..a.max(b), "")
    }

    /// Replaces the chars `chars` (clamped to the text) with `text`.
    pub fn replace(&mut self, chars: Range<usize>, text: &str) -> Change {
        let len = self.rope.len_chars();
        let b = chars.end.min(len);
        let a = chars.start.min(b);
        let line = self.rope.char_to_line(a);
        let start_byte_in_line = self.rope.char_to_byte(a) - self.rope.line_to_byte(line);
        let old_lines = self.rope.char_to_line(b) - line + 1;
        if a < b {
            self.rope.remove(a..b);
        }
        if !text.is_empty() {
            self.rope.insert(a, text);
        }
        self.version += 1;
        Change {
            start_char: a,
            removed_chars: b - a,
            inserted_chars: text.chars().count(),
            line,
            old_lines,
            new_lines: 1 + text.bytes().filter(|&b| b == b'\n').count(),
            start_byte_in_line,
        }
    }

    /// Bumps on every edit.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    #[test]
    fn insert_newline_splits_line() {
        let mut b = TextBuffer::from_str("SELECT 1");
        b.insert(p(0, 6), "\n");
        assert_eq!(b.len_lines(), 2);
        assert_eq!(b.line(1), " 1");
    }

    #[test]
    fn delete_across_lines_joins() {
        let mut b = TextBuffer::from_str("a\nb\nc");
        let c = b.delete(p(0, 1), p(2, 0));
        assert_eq!(b.rope().to_string(), "ac");
        assert_eq!(
            c,
            Change {
                start_char: 1,
                removed_chars: 3,
                inserted_chars: 0,
                line: 0,
                old_lines: 3,
                new_lines: 1,
                start_byte_in_line: 1,
            }
        );
    }

    #[test]
    fn edit_offsets_are_bytes_for_multibyte() {
        let mut b = TextBuffer::from_str("è日🚀x");
        let c = b.insert(p(0, 3), "y");
        assert_eq!(c.start_char, 3);
        assert_eq!(c.inserted_chars, 1);
        assert_eq!(c.start_byte_in_line, 2 + 3 + 4);
        assert_eq!(b.line(0), "è日🚀yx");
        assert_eq!(b.line_len_bytes(0), 2 + 3 + 4 + 2);
    }

    #[test]
    fn crlf_is_hidden_and_not_addressable() {
        let mut b = TextBuffer::from_str("ab\r\ncd");
        assert_eq!(b.line(0), "ab");
        assert_eq!(b.line_len_chars(0), 2);
        assert_eq!(b.line_len_bytes(0), 2);
        let q = b.clamp(p(0, 3));
        assert_eq!(q, p(0, 2));
        b.insert(q, "X");
        assert_eq!(b.rope().to_string(), "abX\r\ncd");
    }

    #[test]
    fn clamp_past_end() {
        let b = TextBuffer::from_str("a\nbc");
        assert_eq!(b.clamp(p(99, 99)), p(1, 2));
    }

    #[test]
    fn char_index_round_trips_through_pos() {
        let b = TextBuffer::from_str("è\r\nab\nc");
        for q in [p(0, 1), p(1, 2), p(2, 1)] {
            assert_eq!(b.pos_of_char(b.char_index(q)), q);
        }
        assert_eq!(b.char_index(p(1, 0)), 3);
        // The char between '\r' and '\n' maps back to the end of the line.
        assert_eq!(b.pos_of_char(2), p(0, 1));
        assert_eq!(b.len_chars(), 7);
    }

    #[test]
    fn version_increments() {
        let mut b = TextBuffer::from_str("abc");
        b.insert(p(0, 0), "x");
        b.delete(p(0, 0), p(0, 1));
        assert_eq!(b.version(), 2);
    }

    #[test]
    fn change_reports_lines() {
        let mut b = TextBuffer::from_str("l0\nl1\nl2\nabcd\nl4");
        let c = b.insert(p(3, 2), "a\nb");
        assert_eq!(
            c,
            Change {
                start_char: 11,
                removed_chars: 0,
                inserted_chars: 3,
                line: 3,
                old_lines: 1,
                new_lines: 2,
                start_byte_in_line: 2,
            }
        );
        assert_eq!(b.line(3), "aba");
        assert_eq!(b.line(4), "bcd");
    }
}
