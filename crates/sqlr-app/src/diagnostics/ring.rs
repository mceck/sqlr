//! The bounded in-memory log ring: the lines immediately before a crash, available to the
//! panic hook even when the asynchronous file writer has not flushed yet.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// The ring keeps at most this many lines.
pub const RING_LINES: usize = 500;
/// And at most this many bytes of them.
pub const RING_BYTES: usize = 256 * 1024;
/// A single line is capped so one huge value cannot evict the rest of the ring.
const LINE_BYTES: usize = 8 * 1024;

/// What the ring held when it was read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RingSnapshot {
    pub lines: Vec<String>,
    pub dropped_lines: usize,
    pub dropped_bytes: usize,
}

impl RingSnapshot {
    /// The excerpt embedded in incidents and bundles, oldest first. A dropped prefix is
    /// named explicitly instead of being silently missing.
    pub fn excerpt(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.lines.len() + 1);
        if self.dropped_lines > 0 || self.dropped_bytes > 0 {
            out.push(format!(
                "…{} earlier log line(s) omitted ({} bytes)…",
                self.dropped_lines, self.dropped_bytes
            ));
        }
        out.extend(self.lines.iter().cloned());
        out
    }
}

struct Inner {
    lines: VecDeque<String>,
    bytes: usize,
    dropped_lines: usize,
    dropped_bytes: usize,
}

/// A thread-safe ring, readable from the panic hook without application locks.
pub struct LogRing {
    inner: Mutex<Inner>,
}

impl Default for LogRing {
    fn default() -> Self {
        Self::new()
    }
}

impl LogRing {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                lines: VecDeque::new(),
                bytes: 0,
                dropped_lines: 0,
                dropped_bytes: 0,
            }),
        }
    }

    pub fn push(&self, line: impl Into<String>) {
        let mut line = line.into();
        if line.len() > LINE_BYTES {
            let cut = floor_char_boundary(&line, LINE_BYTES);
            line.truncate(cut);
            line.push_str("… (line truncated)");
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.bytes += line.len();
        inner.lines.push_back(line);
        while inner.lines.len() > RING_LINES || inner.bytes > RING_BYTES {
            let Some(old) = inner.lines.pop_front() else {
                inner.bytes = 0;
                break;
            };
            inner.bytes = inner.bytes.saturating_sub(old.len());
            inner.dropped_lines += 1;
            inner.dropped_bytes += old.len();
        }
    }

    pub fn snapshot(&self) -> RingSnapshot {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        RingSnapshot {
            lines: inner.lines.iter().cloned().collect(),
            dropped_lines: inner.dropped_lines,
            dropped_bytes: inner.dropped_bytes,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lines
            .len()
    }
}

/// The process-wide ring, so the panic hook and the logging layer share it without holding
/// the application value.
pub fn global() -> &'static LogRing {
    static RING: OnceLock<LogRing> = OnceLock::new();
    RING.get_or_init(LogRing::new)
}

fn floor_char_boundary(text: &str, cap: usize) -> usize {
    if cap >= text.len() {
        return text.len();
    }
    let mut cut = cap;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_is_bounded_by_lines_and_keeps_the_newest() {
        let ring = LogRing::new();
        for i in 0..RING_LINES + 25 {
            ring.push(format!("line {i}"));
        }
        let snapshot = ring.snapshot();
        assert_eq!(snapshot.lines.len(), RING_LINES);
        assert_eq!(snapshot.dropped_lines, 25);
        assert!(snapshot.lines.last().unwrap().ends_with("524"));
        let excerpt = snapshot.excerpt();
        assert!(excerpt[0].contains("25 earlier log line(s) omitted"));
        assert!(excerpt.len() == RING_LINES + 1);
    }

    #[test]
    fn the_ring_is_bounded_by_bytes_and_keeps_the_newest() {
        let ring = LogRing::new();
        let line = "x".repeat(16 * 1024);
        for _ in 0..40 {
            ring.push(line.clone());
        }
        let snapshot = ring.snapshot();
        assert!(snapshot.lines.len() < 40);
        assert!(snapshot.lines.iter().map(String::len).sum::<usize>() <= RING_BYTES);
        assert!(snapshot.dropped_bytes > 0);
        assert!(snapshot.excerpt()[0].contains("omitted"));
    }

    #[test]
    fn a_long_line_is_cut_at_a_character_boundary() {
        let ring = LogRing::new();
        let line = "é".repeat(LINE_BYTES);
        ring.push(line);
        let snapshot = ring.snapshot();
        assert!(snapshot.lines[0].ends_with("(line truncated)"));
        assert!(snapshot.lines[0].is_char_boundary(snapshot.lines[0].len()));
    }

    #[test]
    fn an_empty_ring_has_no_truncation_marker() {
        let snapshot = LogRing::new().snapshot();
        assert!(snapshot.excerpt().is_empty());
    }
}
