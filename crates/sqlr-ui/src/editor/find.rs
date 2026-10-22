//! Plain-text find and replace. The text is scanned line by line from the start position
//! through [`TextBuffer::line`], so the full text is never built. A needle never spans lines.

use std::ops::Range;

use crate::editor::buffer::{Change, TextBuffer};
use crate::editor::model::EditorModel;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl EditorModel {
    /// The first match (char range) starting at or after `from_char`, or with `backwards`
    /// the last one starting before it; either way it wraps around the document.
    pub fn find(
        &self,
        needle: &str,
        opts: FindOptions,
        from_char: usize,
        backwards: bool,
    ) -> Option<Range<usize>> {
        let buf = self.buffer();
        if needle.is_empty() || needle.contains('\n') {
            return None;
        }
        let n = buf.len_lines();
        let from = buf.pos_of_char(from_char);
        let rope = buf.rope();
        let from_byte = rope.char_to_byte(buf.char_index(from)) - rope.line_to_byte(from.line);
        // The start line is visited first and again after wrapping (k == n).
        for k in 0..=n {
            let l = if backwards {
                (from.line + n - k % n) % n
            } else {
                (from.line + k) % n
            };
            let line = buf.line(l);
            let starts = match (k, backwards) {
                (0, false) => from_byte..usize::MAX,
                (0, true) => 0..from_byte,
                (k, false) if k == n => 0..from_byte,
                (k, true) if k == n => from_byte..usize::MAX,
                _ => 0..usize::MAX,
            };
            let found = if backwards {
                matches_in_line(&line, needle, opts, starts.start, true)
                    .take_while(|&(s, _)| s < starts.end)
                    .last()
            } else {
                matches_in_line(&line, needle, opts, starts.start, true)
                    .next()
                    .filter(|&(s, _)| s < starts.end)
            };
            if let Some((s, e)) = found {
                return Some(char_range(buf, l, &line, s, e));
            }
        }
        None
    }

    /// Replaces every match with `with` as one undo group; returns the match count.
    pub fn replace_all(
        &mut self,
        needle: &str,
        opts: FindOptions,
        with: &str,
        now: f64,
    ) -> (usize, Vec<Change>) {
        if needle.is_empty() || needle.contains('\n') {
            return (0, Vec::new());
        }
        let buf = self.buffer();
        let mut found = Vec::new();
        for l in 0..buf.len_lines() {
            let line = buf.line(l);
            for (s, e) in matches_in_line(&line, needle, opts, 0, false) {
                found.push(char_range(buf, l, &line, s, e));
            }
        }
        if found.is_empty() {
            return (0, Vec::new());
        }
        let count = found.len();
        let reps = found.into_iter().map(|r| (r, with)).collect();
        (count, self.replace_mapped(reps, now))
    }
}

/// Matches in `line` (byte ranges) starting at or after byte `from`, in order. `overlapping`
/// also yields matches that start inside a previous one. `needle` must not be empty.
pub(crate) fn matches_in_line<'a>(
    line: &'a str,
    needle: &'a str,
    opts: FindOptions,
    from: usize,
    overlapping: bool,
) -> impl Iterator<Item = (usize, usize)> + 'a {
    let mut i = from.min(line.len());
    std::iter::from_fn(move || {
        while i < line.len() {
            let (s, e) = next_candidate(line, i, needle, opts.case_sensitive)?;
            let next_char = s + line[s..].chars().next().map_or(1, char::len_utf8);
            if !opts.whole_word || is_whole_word(line, s, e) {
                i = if overlapping { next_char } else { e };
                return Some((s, e));
            }
            // A rejected candidate may overlap a valid match: retry one char further.
            i = next_char;
        }
        None
    })
}

/// The first occurrence of `needle` in `line` starting at or after byte `from`.
fn next_candidate(
    line: &str,
    from: usize,
    needle: &str,
    case_sensitive: bool,
) -> Option<(usize, usize)> {
    if case_sensitive {
        let s = from + line[from..].find(needle)?;
        return Some((s, s + needle.len()));
    }
    line[from..]
        .char_indices()
        .find_map(|(k, _)| match_ci(line, from + k, needle).map(|e| (from + k, e)))
}

/// End byte of a case-insensitive match of `needle` at byte `s` of `line`.
fn match_ci(line: &str, s: usize, needle: &str) -> Option<usize> {
    let mut hay = line[s..].char_indices();
    let mut end = s;
    for n in needle.chars() {
        let (k, h) = hay.next()?;
        if !(h == n || h.to_lowercase().eq(n.to_lowercase())) {
            return None;
        }
        end = s + k + h.len_utf8();
    }
    Some(end)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_whole_word(line: &str, s: usize, e: usize) -> bool {
    !line[..s].chars().next_back().is_some_and(is_word_char)
        && !line[e..].chars().next().is_some_and(is_word_char)
}

/// The buffer char range of bytes `s..e` of line `l`.
fn char_range(buf: &TextBuffer, l: usize, line: &str, s: usize, e: usize) -> Range<usize> {
    let start = buf.rope().line_to_char(l) + line[..s].chars().count();
    start..start + line[s..e].chars().count()
}

#[cfg(test)]
mod tests {
    use crate::editor::{EditCommand, EditorModel, FindOptions};

    #[test]
    fn find_wraps_and_respects_options() {
        let m = EditorModel::new("Abc abc xabc");
        let ci = FindOptions::default();
        let cs = FindOptions {
            case_sensitive: true,
            ..ci
        };
        let ww = FindOptions {
            whole_word: true,
            ..ci
        };
        assert_eq!(m.find("abc", ci, 1, false), Some(4..7));
        assert_eq!(m.find("abc", cs, 0, false), Some(4..7));
        assert_eq!(m.find("abc", ww, 5, false), Some(0..3));
        // The brief says 8..11, but the last "abc" is at 9..12 (8..11 is "xab").
        assert_eq!(m.find("abc", ci, 0, true), Some(9..12));
        assert_eq!(m.find("abc", ww, 0, true), Some(4..7));
        assert_eq!(m.find("ABC", ci, 9, true), Some(4..7));
        assert_eq!(m.find("zzz", ci, 0, false), None);
        assert_eq!(m.find("", ci, 0, false), None);
    }

    #[test]
    fn find_offsets_are_chars_across_crlf_and_emoji() {
        let m = EditorModel::new("🚀x\r\nÈÉ abc\nabc");
        assert_eq!(m.find("èé", FindOptions::default(), 0, false), Some(4..6));
        assert_eq!(m.find("abc", FindOptions::default(), 0, false), Some(7..10));
        assert_eq!(
            m.find("abc", FindOptions::default(), 8, false),
            Some(11..14)
        );
        assert_eq!(m.find("abc", FindOptions::default(), 7, true), Some(11..14));
    }

    #[test]
    fn replace_all_counts_and_undoes_once() {
        let original = "abc\nxabc abc";
        let mut m = EditorModel::new(original);
        let (n, changes) = m.replace_all("abc", FindOptions::default(), "z", 0.0);
        assert_eq!(n, 3);
        assert_eq!(changes.len(), 3);
        assert_eq!(m.buffer().rope().to_string(), "z\nxz z");
        m.apply(EditCommand::Undo, 0.1);
        assert_eq!(m.buffer().rope().to_string(), original);

        let ww = FindOptions {
            whole_word: true,
            ..FindOptions::default()
        };
        let (n, _) = m.replace_all("abc", ww, "abcabc", 0.2);
        assert_eq!(n, 2);
        assert_eq!(m.buffer().rope().to_string(), "abcabc\nxabc abcabc");
    }

    #[test]
    fn whole_word_miss_does_not_skip_overlapping_match() {
        // "a a" at 1..4 fails the whole-word check ("xa"); the valid match at 3..6 overlaps it.
        let ww = FindOptions {
            whole_word: true,
            ..FindOptions::default()
        };
        let mut m = EditorModel::new("xa a a");
        assert_eq!(m.find("a a", ww, 0, false), Some(3..6));
        assert_eq!(m.find("a a", ww, 6, true), Some(3..6));
        let (n, _) = m.replace_all("a a", ww, "z", 0.0);
        assert_eq!(n, 1);
        assert_eq!(m.buffer().rope().to_string(), "xa z");
    }
}
