//! Soft-wrap row index: how many visual rows each buffer line takes at the current width.
//!
//! Counts come from a char-by-char simulation of `break_anywhere` wrapping, using the real
//! glyph advances (ASCII takes the monospace advance, so a large document costs one add per
//! char), so wide chars such as CJK or emoji count for what they draw. The galleys laid out
//! for the visible lines then correct any line the simulation got wrong; a Fenwick tree keeps
//! a correction an O(log n) update and maps an absolute visual row back to its
//! `(line, row within line)` in O(log n). An edit recomputes only the lines it touched.

use std::collections::HashMap;

use egui::FontId;
use egui::epaint::text::FontsView;

use crate::editor::buffer::{Change, TextBuffer};
use crate::editor::syntax::LONG_LINE_BYTES;

/// Per-line visual row counts at one width and font size.
#[derive(Default)]
pub(crate) struct WrapIndex {
    width: u32,
    font: u32,
    counts: Vec<u32>,
    /// Fenwick tree over `counts`, 1-based.
    tree: Vec<u32>,
    /// The advance of an ASCII char: the editor's font is monospace.
    cw: f32,
    /// Advances of the non-ASCII chars seen so far (CJK fallback, emoji).
    widths: HashMap<char, f32>,
}

impl WrapIndex {
    /// Rebuilds the counts, or splices in the edited lines when `changes` are consistent
    /// with this index.
    pub(crate) fn update(
        &mut self,
        fonts: &mut FontsView<'_>,
        font: &FontId,
        buf: &TextBuffer,
        width: f32,
        changes: &[Change],
    ) {
        let width = width.max(1.0);
        // One change can be spliced whatever its shape: its line numbers are valid on the
        // buffer we see. Several changes may not: each was computed against the buffer
        // before the later ones, so only line-preserving edits (typing on several carets)
        // can be applied to the final line numbering.
        let simple =
            changes.len() <= 1 || changes.iter().all(|c| c.old_lines == 1 && c.new_lines == 1);
        let rebuild = self.width != width.to_bits()
            || self.font != font.size.to_bits()
            || self.counts.is_empty()
            || !simple;
        if !rebuild {
            for c in changes {
                self.splice(fonts, font, buf, width, c);
            }
            if self.counts.len() == buf.len_lines() {
                self.rebuild_tree();
                return;
            }
        }
        self.rebuild(fonts, font, buf, width);
    }

    /// Counts every line from scratch, in one pass over the rope (a rope line slice per
    /// line would dominate a large document).
    fn rebuild(&mut self, fonts: &mut FontsView<'_>, font: &FontId, buf: &TextBuffer, width: f32) {
        self.width = width.to_bits();
        self.font = font.size.to_bits();
        self.cw = fonts.glyph_width(font, 'M');
        self.widths.clear();
        let cw = self.cw;
        let mut counts = Vec::with_capacity(buf.len_lines());
        let mut x = 0.0;
        let mut rows = 1u32;
        let mut bytes = 0usize;
        let mut chars = buf.rope().chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\n' {
                // Lines past the long-line threshold are windowed, never wrapped.
                counts.push(if bytes > LONG_LINE_BYTES { 1 } else { rows });
                x = 0.0;
                rows = 1;
                bytes = 0;
                continue;
            }
            // The `\r` of a CRLF terminator is hidden, like the `\n` above.
            if c == '\r' && chars.peek() == Some(&'\n') {
                continue;
            }
            bytes += c.len_utf8();
            let w = if c.is_ascii() {
                cw
            } else if let Some(&w) = self.widths.get(&c) {
                w
            } else {
                let w = fonts.glyph_width(font, c);
                self.widths.insert(c, w);
                w
            };
            if x > 0.0 && x + w > width {
                rows += 1;
                x = 0.0;
            }
            x += w;
        }
        counts.push(if bytes > LONG_LINE_BYTES { 1 } else { rows });
        self.counts = counts;
        self.rebuild_tree();
    }

    /// Recomputes the lines one edit touched.
    fn splice(
        &mut self,
        fonts: &mut FontsView<'_>,
        font: &FontId,
        buf: &TextBuffer,
        width: f32,
        c: &Change,
    ) {
        let start = c.line.min(self.counts.len());
        let old_end = (start + c.old_lines).min(self.counts.len());
        let new_end = (start + c.new_lines).min(buf.len_lines());
        let fresh: Vec<u32> = (start..new_end)
            .map(|line| {
                rows_of(
                    &buf.line(line),
                    fonts,
                    font,
                    width,
                    &mut self.widths,
                    self.cw,
                )
            })
            .collect();
        self.counts.splice(start..old_end, fresh);
    }

    fn rebuild_tree(&mut self) {
        let n = self.counts.len();
        self.tree = vec![0; n + 1];
        for (i, &c) in self.counts.iter().enumerate() {
            let mut j = i + 1;
            while j <= n {
                self.tree[j] += c;
                j += j.isolate_lowest_one();
            }
        }
    }

    /// Visual rows of `line`, if it exists.
    pub(crate) fn rows(&self, line: usize) -> Option<usize> {
        self.counts.get(line).map(|&c| c as usize)
    }

    /// Absolute visual row at which `line` starts.
    pub(crate) fn row_start(&self, line: usize) -> usize {
        let mut sum = 0u32;
        let mut i = line.min(self.counts.len());
        while i > 0 {
            sum += self.tree[i];
            i -= i.isolate_lowest_one();
        }
        sum as usize
    }

    /// Total visual rows.
    pub(crate) fn total(&self) -> usize {
        self.row_start(self.counts.len())
    }

    /// The `(line, row within line)` of absolute visual row `row`, clamped into the text.
    pub(crate) fn line_of_row(&self, row: usize) -> (usize, usize) {
        let n = self.counts.len();
        if n == 0 {
            return (0, 0);
        }
        let row = row.min(self.total().saturating_sub(1)) as u32;
        let mut idx = 0usize;
        let mut rem = row;
        let mut bit = 1usize;
        while bit * 2 <= n {
            bit *= 2;
        }
        while bit > 0 {
            let next = idx + bit;
            if next <= n && self.tree[next] <= rem {
                idx = next;
                rem -= self.tree[next];
            }
            bit /= 2;
        }
        let line = idx.min(n - 1);
        (
            line,
            (rem as usize).min(self.counts[line].saturating_sub(1) as usize),
        )
    }

    /// Records `line`'s actual row count from its laid-out galley; true if it changed.
    pub(crate) fn set_rows(&mut self, line: usize, rows: usize) -> bool {
        let Some(count) = self.counts.get_mut(line) else {
            return false;
        };
        let rows = rows.max(1) as u32;
        let old = *count;
        if old == rows {
            return false;
        }
        *count = rows;
        let n = self.counts.len();
        let mut j = line + 1;
        while j <= n {
            self.tree[j] = self.tree[j] - old + rows;
            j += j.isolate_lowest_one();
        }
        true
    }
}

/// Visual rows `text` takes at `max_width` with `break_anywhere`. A long line is laid out over
/// a window and never wrapped, so it counts as one row.
fn rows_of(
    text: &str,
    fonts: &mut FontsView<'_>,
    font: &FontId,
    max_width: f32,
    widths: &mut HashMap<char, f32>,
    cw: f32,
) -> u32 {
    if text.len() > LONG_LINE_BYTES {
        return 1;
    }
    if text.is_ascii() {
        // Every ASCII char takes the same advance: the rows are a division, not a loop.
        let chars = text.len();
        let per_row = (max_width / cw.max(0.001) + 1e-4).floor() as usize;
        if per_row == 0 {
            return chars.max(1) as u32;
        }
        return chars.div_ceil(per_row).max(1) as u32;
    }
    let mut rows = 1;
    let mut x = 0.0;
    for c in text.chars() {
        let w = if c.is_ascii() {
            cw
        } else if let Some(&w) = widths.get(&c) {
            w
        } else {
            let w = fonts.glyph_width(font, c);
            widths.insert(c, w);
            w
        };
        if x > 0.0 && x + w > max_width {
            rows += 1;
            x = 0.0;
        }
        x += w;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_for(text: &str) -> WrapIndex {
        let ctx = egui::Context::default();
        let mut index = WrapIndex::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let font = egui::FontId::monospace(13.0);
            let buf = TextBuffer::from_str(text);
            let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&font, 'M'));
            ui.ctx()
                .fonts_mut(|f| index.update(f, &font, &buf, cw * 2.5, &[]));
        });
        // No renderer consumes the font atlas upload here.
        out.textures_delta.clear();
        index
    }

    #[test]
    fn crlf_terminators_do_not_count_toward_wrapping() {
        // Both lines hold two visible chars: a hidden `\r` must not make the CRLF one wrap.
        let lf = index_for("ab\ncd");
        let crlf = index_for("ab\r\ncd");
        assert_eq!(lf.rows(0), Some(1));
        assert_eq!(crlf.rows(0), Some(1));
        assert_eq!(crlf.rows(1), Some(1));
        assert_eq!(crlf.total(), 2);
    }
}
