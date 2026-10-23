//! The terminal parser and the snapshot the UI paints.

use std::num::NonZeroU16;
use std::sync::Mutex;

/// A terminal colour: the theme default, an ANSI index, or true colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermColor {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

impl From<vt100::Color> for TermColor {
    fn from(value: vt100::Color) -> Self {
        match value {
            vt100::Color::Default => Self::Default,
            vt100::Color::Idx(i) => Self::Idx(i),
            vt100::Color::Rgb(r, g, b) => Self::Rgb(r, g, b),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellView {
    pub text: String,
    pub fg: TermColor,
    pub bg: TermColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    /// The first half of a wide character; the next cell is its continuation.
    pub wide: bool,
}

impl Default for CellView {
    fn default() -> Self {
        Self {
            text: " ".into(),
            fg: TermColor::Default,
            bg: TermColor::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
            wide: false,
        }
    }
}

impl CellView {
    fn from_cell(cell: &vt100::Cell) -> Self {
        Self {
            text: cell.contents().to_string(),
            fg: cell.fgcolor().into(),
            bg: cell.bgcolor().into(),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
            wide: cell.is_wide(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// One painted screen, with the scrollback position already applied.
#[derive(Clone, Debug, Default)]
pub struct ScreenSnapshot {
    pub rows: u16,
    pub cols: u16,
    /// Row-major cells, `rows * cols`.
    pub cells: Vec<CellView>,
    pub cursor: (u16, u16),
    pub cursor_visible: bool,
    pub cursor_shape: CursorShape,
    pub alternate: bool,
    pub bracketed_paste: bool,
    pub application_cursor: bool,
    pub scrollback_len: usize,
    pub scrollback_offset: usize,
}

impl ScreenSnapshot {
    pub fn cell(&self, row: u16, col: u16) -> Option<&CellView> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }

    /// The text between two cell positions, `end` exclusive; rows joined with `\n`.
    pub fn text_between(&self, start: (u16, u16), end: (u16, u16)) -> String {
        let (start, end) = ordered(start, end);
        let mut out = String::new();
        let mut row = start.0;
        let mut col = start.1;
        loop {
            if row > end.0 || (row == end.0 && col >= end.1) {
                break;
            }
            if let Some(cell) = self.cell(row, col) {
                if cell.wide {
                    out.push_str(&cell.text);
                } else if cell.text.is_empty() {
                    out.push(' ');
                } else {
                    out.push_str(&cell.text);
                }
            }
            col += 1;
            if col >= self.cols {
                col = 0;
                row += 1;
                if row <= end.0 {
                    out.push('\n');
                }
            }
        }
        // The grid pads every line; drop it from the end of each line.
        out.split('\n')
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn ordered(a: (u16, u16), b: (u16, u16)) -> ((u16, u16), (u16, u16)) {
    if (a.0, a.1) <= (b.0, b.1) {
        (a, b)
    } else {
        (b, a)
    }
}

/// The parser plus its lock: the reader thread feeds it and the UI snapshots it.
pub struct Emulator {
    parser: Mutex<vt100::Parser>,
    bell: std::sync::atomic::AtomicBool,
}

impl Emulator {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: Mutex::new(vt100::Parser::new(nz(rows), nz(cols), scrollback)),
            bell: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn feed(&self, bytes: &[u8]) {
        if bytes.contains(&0x07) {
            self.bell.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.parser.lock().unwrap().process(bytes);
    }

    /// Takes the visual bell raised since the last call.
    pub fn take_bell(&self) -> bool {
        self.bell.swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    /// The number of rows kept above the live screen.
    pub fn scrollback_len(&self) -> usize {
        self.parser.lock().unwrap().screen().scrollback_len()
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        self.parser.lock().unwrap().set_size(nz(rows), nz(cols));
    }

    pub fn clear(&self) {
        let mut parser = self.parser.lock().unwrap();
        let (rows, cols) = parser.screen().size();
        *parser = vt100::Parser::new(rows, cols, 0);
    }

    /// A snapshot at `scrollback_offset` (0 is the live screen), without disturbing the
    /// screen's own scroll position.
    pub fn snapshot(&self, scrollback_offset: usize) -> ScreenSnapshot {
        let mut parser = self.parser.lock().unwrap();
        let screen = parser.screen_mut();
        let previous = screen.scrollback();
        screen.set_scrollback(scrollback_offset);
        let snapshot = read_screen(screen);
        screen.set_scrollback(previous);
        snapshot
    }
}

fn nz(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value).unwrap_or(NonZeroU16::MIN)
}

fn read_screen(screen: &vt100::Screen) -> ScreenSnapshot {
    let (rows, cols) = screen.size();
    let (rows, cols) = (rows.get(), cols.get());
    let mut cells = Vec::with_capacity(usize::from(rows) * usize::from(cols));
    for row in 0..rows {
        for col in 0..cols {
            cells.push(
                screen
                    .cell(row, col)
                    .map(CellView::from_cell)
                    .unwrap_or_default(),
            );
        }
    }
    ScreenSnapshot {
        rows,
        cols,
        cells,
        cursor: screen.cursor_position(),
        cursor_visible: !screen.hide_cursor(),
        cursor_shape: match screen.cursor_style() {
            vt100::CursorStyle::BlinkUnderline | vt100::CursorStyle::SteadyUnderline => {
                CursorShape::Underline
            }
            vt100::CursorStyle::BlinkVerticalBar | vt100::CursorStyle::SteadyVerticalBar => {
                CursorShape::Bar
            }
            _ => CursorShape::Block,
        },
        alternate: screen.alternate_screen(),
        bracketed_paste: screen.bracketed_paste(),
        application_cursor: screen.application_cursor(),
        scrollback_len: screen.scrollback_len(),
        scrollback_offset: screen.scrollback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emulator() -> Emulator {
        Emulator::new(4, 20, 100)
    }

    #[test]
    fn plain_text_and_cursor() {
        let em = emulator();
        em.feed(b"hello");
        let s = em.snapshot(0);
        assert_eq!(s.cell(0, 0).unwrap().text, "h");
        assert_eq!(s.cursor, (0, 5));
        assert!(s.cursor_visible);
        assert_eq!(s.text_between((0, 0), (0, 5)), "hello");
    }

    #[test]
    fn ansi_colours_16_256_and_truecolour() {
        let em = emulator();
        em.feed(b"\x1b[31mred\x1b[0m \x1b[38;5;196mpink\x1b[0m \x1b[38;2;1;2;3mrgb\x1b[0m");
        let s = em.snapshot(0);
        assert_eq!(s.cell(0, 0).unwrap().fg, TermColor::Idx(1));
        assert_eq!(s.cell(0, 4).unwrap().fg, TermColor::Idx(196));
        assert_eq!(s.cell(0, 9).unwrap().fg, TermColor::Rgb(1, 2, 3));
    }

    #[test]
    fn bold_underline_and_reverse() {
        let em = emulator();
        em.feed(b"\x1b[1;4;7mx\x1b[0m");
        let cell = em.snapshot(0).cell(0, 0).unwrap().clone();
        assert!(cell.bold && cell.underline && cell.inverse);
    }

    #[test]
    fn wide_and_combining_characters() {
        let em = emulator();
        em.feed("界e\u{301}".as_bytes());
        let s = em.snapshot(0);
        assert_eq!(s.cell(0, 0).unwrap().text, "界");
        assert!(s.cell(0, 0).unwrap().wide);
        assert_eq!(s.cell(0, 2).unwrap().text, "e\u{301}");
        assert_eq!(s.cursor, (0, 3));
    }

    #[test]
    fn alternate_screen_and_resize() {
        let em = emulator();
        em.feed(b"main");
        em.feed(b"\x1b[?1049h");
        assert!(em.snapshot(0).alternate);
        em.feed(b"\x1b[?1049l");
        assert!(!em.snapshot(0).alternate);
        assert_eq!(em.snapshot(0).cell(0, 0).unwrap().text, "m");
        em.resize(6, 30);
        let s = em.snapshot(0);
        assert_eq!((s.rows, s.cols), (6, 30));
        assert_eq!(s.cells.len(), 6 * 30);
    }

    #[test]
    fn scrollback_keeps_history() {
        let em = Emulator::new(2, 10, 100);
        for line in 0..5 {
            em.feed(format!("line{line}\r\n").as_bytes());
        }
        let live = em.snapshot(0);
        assert!(live.scrollback_len >= 4);
        let back = em.snapshot(2);
        assert_eq!(back.scrollback_offset, 2);
        // The live screen is unchanged after a scrolled snapshot.
        assert_eq!(em.snapshot(0).scrollback_offset, 0);
    }

    #[test]
    fn bracketed_paste_and_application_cursor_modes() {
        let em = emulator();
        em.feed(b"\x1b[?2004h\x1b[?1h");
        let s = em.snapshot(0);
        assert!(s.bracketed_paste && s.application_cursor);
    }

    #[test]
    fn text_selection_joins_lines() {
        let em = Emulator::new(3, 10, 10);
        em.feed(b"abc\r\ndef");
        let s = em.snapshot(0);
        assert_eq!(s.text_between((0, 1), (1, 2)), "bc\nde");
        assert_eq!(s.text_between((1, 2), (0, 1)), "bc\nde");
    }
}
