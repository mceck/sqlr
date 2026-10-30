//! The resumable line-state syntax layer: highlighting state per line, lexed lazily.
//!
//! Each line stores the [`LexState`] it starts in. Lexing runs only up to the lines that are
//! asked for, within a time budget, and after an edit it stops as soon as a line ends in the
//! state the following line was already lexed from. Lines longer than [`LONG_LINE_BYTES`]
//! keep checkpoints, so a window of a 1 MB line, or an edit in it, lexes a few KB. Lines are
//! lexed without their terminator: a backslash ending an `E'…'` line escapes the newline
//! itself, so the next line starts unescaped exactly as in a full-text lex.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use sqlr_core::{LexEnd, LexState, QueryLanguage, SqlSyntax, Token, TokenKind, is_es_request_line};

use crate::editor::buffer::{Change, TextBuffer};

/// Lines longer than this (bytes) keep checkpoints.
pub const LONG_LINE_BYTES: usize = 4096;
/// A long line gets a checkpoint at the first token start at or after each multiple of this.
pub const CHECKPOINT_BYTES: usize = 4096;
/// The per-frame lexing budget the editor passes to [`SyntaxLayer::ensure`].
pub const LEX_BUDGET: Duration = Duration::from_millis(4);

const NO_SEMI: u32 = u32::MAX;
/// More dirty ranges than this collapse into their hull.
const MAX_DIRTY_RANGES: usize = 64;
/// A batch with more line-count changes than this is applied as one edit of its hull.
const BATCH_SPLICES: usize = 16;
/// The lexer peeks at most this many bytes past a token's end. A text cut at least this far
/// after a token start leaves that token start, and all tokens before it, as in the full text.
const LOOKAHEAD: usize = 3;

/// A token start in a long line, with the state there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Checkpoint {
    at: u32,
    state: LexState,
    /// The first top-level `;` at or after `at`, or [`NO_SEMI`].
    next_semi: u32,
}

/// How far a line's stored info still matches its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    /// Consistent with the line's text and `start`.
    Clean,
    /// One edit inside a long line: from byte `first`, `removed_chars` chars were replaced.
    Local { first: usize, removed_chars: usize },
    /// Must be lexed in full.
    Full,
}

#[derive(Clone, Debug)]
struct LineInfo {
    /// The state the line starts in. For lines below the frontier, and for the frontier line
    /// itself, it is the true state; past it, the state the line was last lexed from.
    start: LexState,
    /// The state at the line's end, when lexed.
    end: LexState,
    /// Byte offset of the line's first top-level `;`.
    first_semi: Option<u32>,
    /// The line's length in bytes, when lexed.
    len: u32,
    checkpoints: Option<Box<[Checkpoint]>>,
    pending: Pending,
}

impl LineInfo {
    fn unlexed() -> Self {
        Self {
            start: LexState::Normal,
            end: LexState::Normal,
            first_semi: None,
            len: 0,
            checkpoints: None,
            pending: Pending::Full,
        }
    }
}

/// What lexing one line found.
struct Lexed {
    end: LexState,
    first_semi: Option<u32>,
    checkpoints: Vec<Checkpoint>,
}

/// Collects a line's `;` and checkpoints while its tokens are lexed, segment by segment.
struct Recorder {
    long: bool,
    next_mult: usize,
    first_semi: Option<u32>,
    checkpoints: Vec<Checkpoint>,
    /// Checkpoints from this index on don't know their next `;` yet.
    unresolved: usize,
}

impl Recorder {
    fn new(from: usize, long: bool) -> Self {
        Self {
            long,
            next_mult: (from / CHECKPOINT_BYTES + 1) * CHECKPOINT_BYTES,
            first_semi: None,
            checkpoints: Vec::new(),
            unresolved: 0,
        }
    }

    /// Feeds tokens lexed from byte `base` of the line, starting in `state`.
    fn feed(&mut self, tokens: &[Token], base: usize, state: LexState) {
        for t in tokens {
            let s = base + t.start;
            if self.long && s >= self.next_mult {
                // Only a token resuming an open construct starts outside Normal.
                let st = if t.start == 0 {
                    state
                } else {
                    LexState::Normal
                };
                self.checkpoints.push(Checkpoint {
                    at: s as u32,
                    state: st,
                    next_semi: NO_SEMI,
                });
                self.next_mult = (s / CHECKPOINT_BYTES + 1) * CHECKPOINT_BYTES;
            }
            if t.kind == TokenKind::Semicolon {
                self.semicolon(s as u32);
            }
        }
    }

    fn semicolon(&mut self, at: u32) {
        self.first_semi.get_or_insert(at);
        for c in &mut self.checkpoints[self.unresolved..] {
            c.next_semi = at;
        }
        self.unresolved = self.checkpoints.len();
    }

    fn finish(self, end: LexState) -> Lexed {
        Lexed {
            end,
            first_semi: self.first_semi,
            checkpoints: self.checkpoints,
        }
    }
}

pub struct SyntaxLayer {
    syntax: Arc<SqlSyntax>,
    /// Lexes and splits with `syntax` for SQL, with the language's own lexer otherwise.
    language: QueryLanguage,
    lines: Vec<LineInfo>,
    /// Lines `0..done` are lexed and consistent; `lines[done].start` is that line's true
    /// start. [`Self::valid_lines`] is `done + 1`.
    done: usize,
    /// The frontier of an earlier pass, shifted by later edits. Below it, each clean line
    /// starts in the state its predecessor ends in, so a converged lex can jump ahead.
    stale_done: usize,
    /// Sorted, disjoint line ranges whose stored info no longer matches their text.
    dirty: Vec<Range<usize>>,
    scratch: Vec<Token>,
    lexed_lines: u64,
    lexed_bytes: u64,
    /// Bytes lexed by [`Self::tokens`], which takes `&self`.
    window_bytes: AtomicU64,
    /// Bytes copied out of the buffer and split into statements.
    split_bytes: u64,
}

impl SyntaxLayer {
    /// A layer over `buf` that has lexed nothing yet; line 0 starts `Normal`.
    pub fn new(syntax: Arc<SqlSyntax>, buf: &TextBuffer) -> Self {
        Self::with_language(syntax, QueryLanguage::Sql, buf)
    }

    /// A layer for `language`; `syntax` still serves SQL completion helpers.
    pub fn with_language(
        syntax: Arc<SqlSyntax>,
        language: QueryLanguage,
        buf: &TextBuffer,
    ) -> Self {
        Self {
            syntax,
            language,
            lines: vec![LineInfo::unlexed(); buf.len_lines()],
            done: 0,
            stale_done: 0,
            dirty: Vec::new(),
            scratch: Vec::new(),
            lexed_lines: 0,
            lexed_bytes: 0,
            window_bytes: AtomicU64::new(0),
            split_bytes: 0,
        }
    }

    pub fn syntax(&self) -> &Arc<SqlSyntax> {
        &self.syntax
    }

    pub fn language(&self) -> QueryLanguage {
        self.language
    }

    /// Lines whose start state is known.
    pub fn valid_lines(&self) -> usize {
        (self.done + 1).min(self.lines.len())
    }

    /// The state `line` starts in; `None` past the frontier.
    pub fn line_state(&self, line: usize) -> Option<LexState> {
        (line < self.valid_lines()).then(|| self.lines[line].start)
    }

    /// A byte offset at or before `byte` of `buf` where lexing can resume, with the state
    /// there: the last checkpoint of a long line, else the line start. `None` past the
    /// frontier.
    pub fn resume_point(&self, buf: &TextBuffer, byte: usize) -> Option<(usize, LexState)> {
        let rope = buf.rope();
        let byte = byte.min(rope.len_bytes());
        let line = rope.byte_to_line(byte);
        if line >= self.valid_lines() {
            return None;
        }
        let info = &self.lines[line];
        let line_start = rope.line_to_byte(line);
        let rel = byte - line_start;
        if let Some(cps) = &info.checkpoints {
            match info.pending {
                Pending::Clean => {
                    let i = cps.partition_point(|c| c.at as usize <= rel);
                    if i > 0 {
                        return Some((line_start + cps[i - 1].at as usize, cps[i - 1].state));
                    }
                }
                Pending::Local { first, .. } => {
                    if let Some(c) =
                        unreached_checkpoint(cps, rope.line(line), first, rel.min(first))
                    {
                        return Some((line_start + c.at as usize, c.state));
                    }
                }
                Pending::Full => {}
            }
        }
        Some((line_start, info.start))
    }

    /// Lines lexed so far (a long-line partial relex counts as one).
    pub fn lexed_lines(&self) -> u64 {
        self.lexed_lines
    }

    /// Bytes lexed by [`Self::tokens`] so far.
    pub fn window_lexed_bytes(&self) -> u64 {
        self.window_bytes.load(Ordering::Relaxed)
    }

    /// Bytes handed to the lexer by the line-state passes.
    pub fn lexed_bytes(&self) -> u64 {
        self.lexed_bytes
    }

    /// Bytes copied out of the buffer and split by [`Self::statement_at`] and
    /// [`Self::statements`] so far.
    pub fn split_bytes(&self) -> u64 {
        self.split_bytes
    }

    /// Records one buffer change, in the order the model made them. It splices the line table
    /// and moves the frontier back; it lexes nothing. Prefer [`Self::apply_all`] for the
    /// changes of one command.
    pub fn apply(&mut self, _buf: &TextBuffer, change: &Change) {
        let local = Pending::Local {
            first: change.start_byte_in_line,
            removed_chars: change.removed_chars,
        };
        self.apply_lines(change.line, change.old_lines, change.new_lines, local);
    }

    /// Records the changes of one command, in order. A batch that changes the line count
    /// many times is applied as one edit of the lines it spans, so the line table is spliced
    /// once instead of once per change.
    pub fn apply_all(&mut self, buf: &TextBuffer, changes: &[Change]) {
        let splices = changes
            .iter()
            .filter(|c| c.old_lines != 1 || c.new_lines != 1)
            .count();
        if splices <= BATCH_SPLICES {
            for c in changes {
                self.apply(buf, c);
            }
            return;
        }
        // The hull of the edited lines, in current coordinates, and the line delta.
        let (mut lo, mut hi, mut delta) = (usize::MAX, 0, 0isize);
        for c in changes {
            let (l, old, new) = (c.line, c.old_lines, c.new_lines);
            let map = |x: usize| match x {
                x if x <= l => x,
                x if x >= l + old => x - old + new,
                _ => l + new,
            };
            hi = if lo == usize::MAX {
                l + new
            } else {
                map(hi).max(l + new)
            };
            lo = lo.min(l);
            delta += new as isize - old as isize;
        }
        let old_end = (hi as isize - delta) as usize;
        self.apply_lines(lo, old_end - lo, hi - lo, Pending::Full);
    }

    /// Replaces lines `l..l + old` with `new` lines. `local` is what line `l` becomes when it
    /// was clean, long and its line count is unchanged.
    fn apply_lines(&mut self, l: usize, old: usize, new: usize, local: Pending) {
        let frontier = self.done.max(self.stale_done);
        if l < frontier {
            self.stale_done = if l + old <= frontier {
                frontier - old + new
            } else {
                l + new
            };
        }
        self.done = self.done.min(l);
        let info = &mut self.lines[l];
        info.pending = match info.pending {
            Pending::Clean if old == 1 && new == 1 && info.checkpoints.is_some() => local,
            _ => Pending::Full,
        };
        if old != 1 || new != 1 {
            let fresh = std::iter::repeat_n(LineInfo::unlexed(), new - 1);
            self.lines.splice(l + 1..l + old, fresh);
        }
        self.mark_dirty(l, old, new);
    }

    /// Shifts the dirty ranges past an edit of lines `l..l + old` (now `l..l + new`), which
    /// becomes dirty itself.
    fn mark_dirty(&mut self, l: usize, old: usize, new: usize) {
        let shift = |x: usize| x - old + new;
        let mut fresh = l..l + new;
        let mut out: Vec<Range<usize>> = Vec::with_capacity(self.dirty.len() + 1);
        for r in self.dirty.drain(..) {
            if r.end <= l {
                out.push(r);
            } else if r.start >= l + old {
                out.push(shift(r.start)..shift(r.end));
            } else {
                fresh.start = fresh.start.min(r.start);
                if r.end > l + old {
                    fresh.end = fresh.end.max(shift(r.end));
                }
            }
        }
        out.push(fresh);
        out.sort_by_key(|r| r.start);
        self.dirty = merge_ranges(out);
        if self.dirty.len() > MAX_DIRTY_RANGES {
            // Bulk edits: one hull keeps this O(1) per change; it only forgoes convergence
            // inside the hull.
            let hull = self.dirty[0].start..self.dirty[self.dirty.len() - 1].end;
            self.dirty = vec![hull];
        }
    }

    /// The first dirty line at or after `line` (`usize::MAX` if none); forgets ranges below it.
    fn first_dirty_from(&mut self, line: usize) -> usize {
        let gone = self.dirty.partition_point(|r| r.end <= line);
        self.dirty.drain(..gone);
        self.dirty.first().map_or(usize::MAX, |r| r.start.max(line))
    }

    /// Lexes lines from the frontier until `upto_line` is lexed, or until `budget` is spent
    /// (at least one line is always lexed). True when lines `0..=upto_line` are valid.
    pub fn ensure(&mut self, buf: &TextBuffer, upto_line: usize, budget: Option<Duration>) -> bool {
        let n = self.lines.len();
        let upto = upto_line.min(n - 1);
        let started = Instant::now();
        let mut first = true;
        while self.done <= upto {
            if !first && budget.is_some_and(|b| started.elapsed() >= b) {
                break;
            }
            first = false;
            self.lex_next(buf);
        }
        if self.done < n && self.lines[self.done].pending != Pending::Clean {
            let d = self.done;
            let at = self.dirty.partition_point(|r| r.start < d);
            self.dirty.insert(at, d..d + 1);
            self.dirty = merge_ranges(std::mem::take(&mut self.dirty));
        }
        self.done > upto
    }

    /// Lexes the frontier line and advances the frontier, jumping ahead once converged.
    fn lex_next(&mut self, buf: &TextBuffer) {
        let k = self.done;
        let len = buf.line_len_bytes(k);
        let lexed = match self.lines[k].pending {
            Pending::Local {
                first,
                removed_chars,
            } if len > LONG_LINE_BYTES => self
                .lex_local(buf, k, first, removed_chars)
                .unwrap_or_else(|| self.lex_full(buf, k)),
            _ => self.lex_full(buf, k),
        };
        self.lexed_lines += 1;
        let info = &mut self.lines[k];
        info.end = lexed.end;
        info.first_semi = lexed.first_semi;
        info.len = len as u32;
        info.checkpoints = (len > LONG_LINE_BYTES).then(|| lexed.checkpoints.into_boxed_slice());
        info.pending = Pending::Clean;
        self.done = k + 1;
        let Some(next) = self.lines.get_mut(k + 1) else {
            return;
        };
        if next.start != lexed.end {
            next.start = lexed.end;
            next.pending = Pending::Full;
            return;
        }
        if next.pending == Pending::Clean && k + 1 < self.stale_done {
            // Converged: the clean lines after this one chain from an earlier pass.
            let target = self.stale_done.min(self.first_dirty_from(k + 1));
            self.done = target.max(k + 1);
        }
    }

    fn lex_full(&mut self, buf: &TextBuffer, k: usize) -> Lexed {
        let len = buf.line_len_bytes(k);
        let start = self.lines[k].start;
        let text: Cow<str> = buf.rope().line(k).byte_slice(..len).into();
        self.scratch.clear();
        let end = lex(&self.syntax, self.language, &text, start, &mut self.scratch);
        self.lexed_bytes += len as u64;
        let mut rec = Recorder::new(0, len > LONG_LINE_BYTES);
        rec.feed(&self.scratch, 0, start);
        rec.finish(end.state)
    }

    /// Re-lexes a long line after one edit from the last checkpoint before it, and reuses the
    /// old tail from the first old checkpoint that a token starts exactly at again (shifted by
    /// the byte delta). `None` when the line has no checkpoints to work from.
    fn lex_local(
        &mut self,
        buf: &TextBuffer,
        k: usize,
        first: usize,
        removed_chars: usize,
    ) -> Option<Lexed> {
        let info = &self.lines[k];
        let cps = info.checkpoints.as_deref()?;
        let new_len = buf.line_len_bytes(k);
        let delta = new_len as isize - info.len as isize;
        let shifted = |at: u32| (at as isize + delta) as usize;
        let line = buf.rope().line(k);
        // A checkpoint's token start and state depend on the bytes after it: up to LOOKAHEAD
        // of them, and a `$` before it scans the word run after it for a `$tag$`. Start from
        // the last checkpoint the edit can't reach that way.
        let mut ci = cps.partition_point(|c| c.at as usize + LOOKAHEAD <= first);
        while ci > 0
            && !line
                .byte_slice(cps[ci - 1].at as usize..first)
                .bytes()
                .any(ends_tag_scan)
        {
            ci -= 1;
        }
        let (c0, st0) = match ci {
            0 => (0, info.start),
            _ => (cps[ci - 1].at as usize, cps[ci - 1].state),
        };
        // Old text from here on is untouched (a char is at most 4 bytes).
        let reuse_from = first + 4 * removed_chars;
        let candidates: Vec<usize> = (ci..cps.len())
            .filter(|&j| cps[j].at as usize >= reuse_from && shifted(cps[j].at) > c0)
            .collect();

        let mut rec = Recorder::new(c0, true);
        rec.checkpoints = cps[..ci].to_vec();
        rec.first_semi = info.first_semi.filter(|&s| (s as usize) < c0);
        rec.unresolved = rec
            .checkpoints
            .partition_point(|c| c.next_semi != NO_SEMI && (c.next_semi as usize) < c0);
        for c in &mut rec.checkpoints[rec.unresolved..] {
            c.next_semi = NO_SEMI;
        }
        let old_end = info.end;
        let old_cps = cps.to_vec();
        let (mut p, mut st) = (c0, st0);
        let mut idx = 0;
        loop {
            let cand = candidates.get(idx).copied();
            let cut = match cand {
                Some(j) => {
                    let t = shifted(old_cps[j].at);
                    candidates[idx + 1..]
                        .iter()
                        .map(|&m| shifted(old_cps[m].at))
                        .find(|&c| c >= t + LOOKAHEAD)
                        .unwrap_or(new_len)
                }
                None => new_len,
            };
            let end = self.lex_segment(&line, p, cut, st);
            let Some(j) = cand else {
                rec.feed(&self.scratch, p, st);
                return Some(rec.finish(end));
            };
            let t = shifted(old_cps[j].at);
            if let Some(pos) = reuse_hit(&self.scratch, p, t, &old_cps[j]) {
                reuse_tail(&mut rec, &self.scratch[..pos], p, st, &old_cps[j..], delta);
                return Some(rec.finish(old_end));
            }
            idx += 1;
            // No reuse yet: keep what is certain (all but the last token, which the cut may
            // have truncated) and continue from there.
            let keep = self.scratch.len().saturating_sub(2);
            let resume = self.scratch.get(keep).map_or(0, |t| t.start);
            if self.scratch.len() >= 2 && resume > 0 {
                rec.feed(&self.scratch[..keep], p, st);
                p += resume;
                st = LexState::Normal;
            } else if cut < new_len {
                // One token spans the whole segment (an unclosed comment, string or dollar
                // quote): lex the rest of the line once instead of once per checkpoint.
                let end = self.lex_segment(&line, p, new_len, st);
                for &j in &candidates[idx..] {
                    let t = shifted(old_cps[j].at);
                    if let Some(pos) = reuse_hit(&self.scratch, p, t, &old_cps[j]) {
                        reuse_tail(&mut rec, &self.scratch[..pos], p, st, &old_cps[j..], delta);
                        return Some(rec.finish(old_end));
                    }
                }
                rec.feed(&self.scratch, p, st);
                return Some(rec.finish(end));
            }
        }
    }

    /// Lexes bytes `from..to` of a line into the scratch buffer; returns the end state.
    fn lex_segment(
        &mut self,
        line: &ropey::RopeSlice<'_>,
        from: usize,
        to: usize,
        state: LexState,
    ) -> LexState {
        let text: Cow<str> = line.byte_slice(from..to).into();
        self.scratch.clear();
        self.lexed_bytes += (to - from) as u64;
        lex(&self.syntax, self.language, &text, state, &mut self.scratch).state
    }

    /// Tokens of `line` that overlap `bytes` (offsets relative to the line); false when the
    /// line's start state isn't known yet.
    pub fn tokens(
        &self,
        buf: &TextBuffer,
        line: usize,
        bytes: Range<usize>,
        out: &mut Vec<Token>,
    ) -> bool {
        out.clear();
        if line >= self.valid_lines() {
            return false;
        }
        let info = &self.lines[line];
        let len = buf.line_len_bytes(line);
        let (ws, we) = (bytes.start.min(len), bytes.end.min(len));
        let (mut from, mut state, mut cut) = (0, info.start, len);
        match (info.pending, &info.checkpoints) {
            (Pending::Clean, Some(cps)) => {
                let i = cps.partition_point(|c| c.at as usize <= ws);
                if i > 0 {
                    (from, state) = (cps[i - 1].at as usize, cps[i - 1].state);
                }
                // Stop one checkpoint past the window, so the cut can't affect tokens in it.
                let j = cps.partition_point(|c| c.at as usize <= we);
                if let Some(first_past) = cps.get(j) {
                    let min = first_past.at as usize + LOOKAHEAD;
                    if let Some(c) = cps[j + 1..].iter().find(|c| c.at as usize >= min) {
                        cut = c.at as usize;
                    }
                }
            }
            (Pending::Local { first, .. }, Some(cps)) => {
                // One edit at `first` not yet re-lexed: the checkpoints the edit can't reach
                // (see `lex_local`) are still right, so lex on from the last of them before
                // the window or the edit, and stop a little past the window.
                let line = buf.rope().line(line);
                if let Some(c) = unreached_checkpoint(cps, line, first, ws.min(first)) {
                    (from, state) = (c.at as usize, c.state);
                }
                let stop = (we + CHECKPOINT_BYTES + LOOKAHEAD).min(len);
                cut = line.char_to_byte(line.byte_to_char(stop));
            }
            _ => {}
        }
        self.window_bytes
            .fetch_add((cut - from) as u64, Ordering::Relaxed);
        let text: Cow<str> = buf.rope().line(line).byte_slice(from..cut).into();
        lex(&self.syntax, self.language, &text, state, out);
        for t in out.iter_mut() {
            t.start += from;
            t.end += from;
        }
        out.retain(|t| t.end > ws && t.start < we);
        true
    }

    /// The statement (char range, trimmed) the caret belongs to: the one containing it, else
    /// the nearest one ending at or before it, else the first one after it.
    pub fn statement_at(&mut self, buf: &TextBuffer, caret_char: usize) -> Option<Range<usize>> {
        self.statement_within(buf, caret_char, usize::MAX)
            .unwrap_or_else(|_| unreachable!("no byte limit"))
    }

    /// Like [`Self::statement_at`], but the region it would copy and split (whole lines
    /// between `;` lines) is limited to `max_bytes`: past that it returns `Err` with the
    /// region's byte range, having copied nothing of it, and lexed and scanned lines only up
    /// to `max_bytes` away from the caret.
    pub fn statement_within(
        &mut self,
        buf: &TextBuffer,
        caret_char: usize,
        max_bytes: usize,
    ) -> Result<Option<Range<usize>>, Range<usize>> {
        if self.language == QueryLanguage::EsConsole {
            return self.es_statement_within(buf, caret_char, max_bytes);
        }
        let n = self.lines.len();
        let rope = buf.rope();
        let caret = caret_char.min(buf.len_chars());
        let cl = rope.char_to_line(caret);
        let caret_byte = rope.char_to_byte(caret);
        // Lines beyond these hold no part of a region within `max_bytes`.
        let floor = rope.byte_to_line(caret_byte.saturating_sub(max_bytes));
        let ceil = rope.byte_to_line(caret_byte.saturating_add(max_bytes).min(rope.len_bytes()));
        self.ensure(buf, cl, None);
        // How many `;` lines the region spans before and from the caret line.
        let (mut back, mut fwd) = (2, 2);
        loop {
            let lo = self.semi_line_before(cl, back, floor);
            let hi = self.semi_line_from(buf, cl, fwd, ceil);
            let origin = rope.line_to_char(lo);
            let end = if hi + 1 < n {
                rope.line_to_char(hi + 1)
            } else {
                buf.len_chars()
            };
            let (origin_byte, end_byte) = (rope.char_to_byte(origin), rope.char_to_byte(end));
            if end_byte - origin_byte > max_bytes {
                return Err(origin_byte..end_byte);
            }
            let text = rope.slice(origin..end).to_string();
            self.split_bytes += text.len() as u64;
            let mut ranges = split(&self.syntax, self.language, &text, self.lines[lo].start);
            if lo > 0 {
                // The region starts on a `;` line, maybe inside a statement: drop that part.
                let first = self.lines[lo].first_semi.map_or(0, |s| s as usize);
                ranges.retain(|r| r.start > first);
            }
            let at = caret_byte - origin_byte;
            // A range is whole unless it is the last one and the region stops short of the end.
            let whole = |i: usize| i + 1 < ranges.len() || hi + 1 >= n;
            let pick = match ranges.iter().position(|r| r.start <= at && at <= r.end) {
                Some(i) => Some(i),
                None => match ranges.iter().rposition(|r| r.end <= at) {
                    Some(i) => Some(i),
                    None if lo > 0 => {
                        back += 1;
                        continue;
                    }
                    None => ranges.iter().position(|r| r.start >= at),
                },
            };
            match pick {
                Some(i) if whole(i) => {
                    return Ok(char_ranges(&text, origin, [ranges[i].clone()]).pop());
                }
                None if hi + 1 >= n => return Ok(None),
                _ => fwd += 1,
            }
        }
    }

    /// [`Self::statement_within`] for the Elasticsearch console: the region runs from the
    /// request line at or before the caret to the next request line, so only the requests
    /// around the caret are copied and split.
    fn es_statement_within(
        &mut self,
        buf: &TextBuffer,
        caret_char: usize,
        max_bytes: usize,
    ) -> Result<Option<Range<usize>>, Range<usize>> {
        let n = self.lines.len();
        let rope = buf.rope();
        let caret = caret_char.min(buf.len_chars());
        let cl = rope.char_to_line(caret);
        let caret_byte = rope.char_to_byte(caret);
        let floor = rope.byte_to_line(caret_byte.saturating_sub(max_bytes));
        let ceil = rope.byte_to_line(caret_byte.saturating_add(max_bytes).min(rope.len_bytes()));
        self.ensure(buf, cl, None);
        let is_head = |layer: &Self, l: usize| {
            layer.lines[l].start == LexState::Normal && is_es_request_line(&buf.line(l))
        };
        let mut lo = cl;
        while lo > floor && !is_head(self, lo) {
            lo -= 1;
        }
        let mut hi = cl + 1;
        while hi < n && hi <= ceil {
            self.ensure(buf, hi, None);
            if is_head(self, hi) {
                break;
            }
            hi += 1;
        }
        let origin = rope.line_to_char(lo);
        let end = if hi < n {
            rope.line_to_char(hi)
        } else {
            buf.len_chars()
        };
        let (origin_byte, end_byte) = (rope.char_to_byte(origin), rope.char_to_byte(end));
        if end_byte - origin_byte > max_bytes {
            return Err(origin_byte..end_byte);
        }
        let text = rope.slice(origin..end).to_string();
        self.split_bytes += text.len() as u64;
        let ranges = QueryLanguage::EsConsole.split(&text);
        let at = caret_byte - origin_byte;
        let pick = ranges
            .iter()
            .position(|r| r.start <= at && at <= r.end)
            .or_else(|| ranges.iter().rposition(|r| r.end <= at))
            .or_else(|| ranges.iter().position(|r| r.start >= at));
        Ok(pick.and_then(|i| char_ranges(&text, origin, [ranges[i].clone()]).pop()))
    }

    /// The statements in `chars` (char ranges, trimmed), split as that text would be run.
    pub fn statements(&mut self, buf: &TextBuffer, chars: Range<usize>) -> Vec<Range<usize>> {
        let rope = buf.rope();
        let len = buf.len_chars();
        let (a, b) = (chars.start.min(len), chars.end.min(len));
        if a >= b {
            return Vec::new();
        }
        let first = rope.char_to_line(a);
        self.ensure(buf, first, None);
        let origin = rope.line_to_char(first);
        let text = rope.slice(origin..b).to_string();
        self.split_bytes += text.len() as u64;
        let skip = rope.char_to_byte(a) - rope.char_to_byte(origin);
        let ranges = split(&self.syntax, self.language, &text, self.lines[first].start)
            .into_iter()
            .filter_map(|r| {
                let s = r.start.max(skip);
                if s >= r.end {
                    // The statement ends before the selection starts.
                    return None;
                }
                let s = s + (text[s..r.end].len() - text[s..r.end].trim_start().len());
                (s < r.end).then_some(s..r.end)
            });
        char_ranges(&text, origin, ranges)
    }

    /// The `count`-th line before `line` holding a top-level `;`, or 0. Lines before
    /// `floor` are not searched: 0 when the search would reach them.
    fn semi_line_before(&self, line: usize, count: usize, floor: usize) -> usize {
        let mut lo = line;
        for _ in 0..count {
            match (floor..lo)
                .rev()
                .find(|&i| self.lines[i].first_semi.is_some())
            {
                Some(i) => lo = i,
                None => return 0,
            }
        }
        lo
    }

    /// The `count`-th line from `line` on holding a top-level `;`, or the last line. Lines
    /// after `ceil` are neither lexed nor searched: the last line when the search would
    /// reach them.
    fn semi_line_from(
        &mut self,
        buf: &TextBuffer,
        line: usize,
        count: usize,
        ceil: usize,
    ) -> usize {
        let last = self.lines.len() - 1;
        let mut found = 0;
        for l in line..last.min(ceil.saturating_add(1)) {
            self.ensure(buf, l, None);
            if self.lines[l].first_semi.is_some() {
                found += 1;
                if found == count {
                    return l;
                }
            }
        }
        last
    }
}

/// Lexes with the SQL `syntax`, or with `language`'s own lexer.
fn lex(
    syntax: &SqlSyntax,
    language: QueryLanguage,
    text: &str,
    state: LexState,
    out: &mut Vec<Token>,
) -> LexEnd {
    match language {
        QueryLanguage::Sql => syntax.lex(text, state, out),
        other => other.lex(text, state, out),
    }
}

/// Statement ranges of `text`, which starts in `state`.
fn split(
    syntax: &SqlSyntax,
    language: QueryLanguage,
    text: &str,
    state: LexState,
) -> Vec<Range<usize>> {
    match language {
        QueryLanguage::Sql => syntax.split(text, state),
        other => other.split_from(text, state),
    }
}

/// The last checkpoint of a long line with one pending edit at byte `first` that the edit
/// cannot reach (see `lex_local`) and that is at or before `limit`.
fn unreached_checkpoint(
    cps: &[Checkpoint],
    line: ropey::RopeSlice<'_>,
    first: usize,
    limit: usize,
) -> Option<Checkpoint> {
    let mut ci = cps.partition_point(|c| c.at as usize + LOOKAHEAD <= limit);
    while ci > 0
        && !line
            .byte_slice(cps[ci - 1].at as usize..first.min(line.len_bytes()))
            .bytes()
            .any(ends_tag_scan)
    {
        ci -= 1;
    }
    ci.checked_sub(1).map(|i| cps[i])
}

/// Whether byte `b` stops the lexer's scan for a `$tag$` (a `$` or a non-word ASCII byte).
fn ends_tag_scan(b: u8) -> bool {
    b == b'$' || !(b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80)
}

/// The index of the token lexed from byte `p` that starts exactly at `t`, when `t` is past
/// `p` and the old checkpoint there was in `Normal`.
fn reuse_hit(tokens: &[Token], p: usize, t: usize, old: &Checkpoint) -> Option<usize> {
    if t <= p || old.state != LexState::Normal {
        return None;
    }
    tokens.binary_search_by_key(&(t - p), |tk| tk.start).ok()
}

/// Feeds the re-lexed tokens before the reuse point, then takes the old tail: its first `;`
/// and its checkpoints, shifted by `delta`.
fn reuse_tail(
    rec: &mut Recorder,
    before: &[Token],
    p: usize,
    st: LexState,
    tail: &[Checkpoint],
    delta: isize,
) {
    let shifted = |at: u32| (at as isize + delta) as u32;
    rec.feed(before, p, st);
    if tail[0].next_semi != NO_SEMI {
        rec.semicolon(shifted(tail[0].next_semi));
    }
    rec.checkpoints.extend(tail.iter().map(|c| Checkpoint {
        at: shifted(c.at),
        state: c.state,
        next_semi: match c.next_semi {
            NO_SEMI => NO_SEMI,
            s => shifted(s),
        },
    }));
}

/// Merges sorted ranges that overlap or touch.
fn merge_ranges(sorted: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::with_capacity(sorted.len());
    for r in sorted {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// Maps ascending byte ranges of `text` (which starts at buffer char `origin`) to char ranges.
fn char_ranges(
    text: &str,
    origin: usize,
    ranges: impl IntoIterator<Item = Range<usize>>,
) -> Vec<Range<usize>> {
    let (mut byte, mut ch) = (0, origin);
    let mut advance = |to: usize| {
        ch += text[byte..to].chars().count();
        byte = to;
        ch
    };
    ranges
        .into_iter()
        .map(|r| {
            let s = advance(r.start);
            s..advance(r.end)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use sqlr_core::{LexState, SqlSyntax, Token, TokenKind};

    use super::*;
    use crate::editor::{Pos, TextBuffer};

    fn pg_like() -> Arc<SqlSyntax> {
        Arc::new(SqlSyntax {
            dollar_quotes: true,
            nested_comments: true,
            escape_strings: true,
            ..(*SqlSyntax::ansi()).clone()
        })
    }

    fn layer(text: &str) -> (TextBuffer, SyntaxLayer) {
        let buf = TextBuffer::from_str(text);
        let layer = SyntaxLayer::new(pg_like(), &buf);
        (buf, layer)
    }

    fn line_tokens(layer: &SyntaxLayer, buf: &TextBuffer, line: usize) -> Vec<Token> {
        let mut out = Vec::new();
        assert!(
            layer.tokens(buf, line, 0..usize::MAX, &mut out),
            "line {line} not valid"
        );
        out
    }

    fn full_lex(syntax: &SqlSyntax, text: &str, window: std::ops::Range<usize>) -> Vec<Token> {
        let mut out = Vec::new();
        syntax.lex(text, LexState::Normal, &mut out);
        out.retain(|t| t.end > window.start && t.start < window.end);
        out
    }

    fn text_of(buf: &TextBuffer, r: std::ops::Range<usize>) -> String {
        buf.rope().slice(r).to_string()
    }

    #[test]
    fn lazy_until_ensured() {
        let (buf, mut l) = layer(&"select 1;\n".repeat(999));
        assert_eq!(buf.len_lines(), 1000);
        assert_eq!(l.valid_lines(), 1);
        assert_eq!(l.line_state(0), Some(LexState::Normal));
        assert!(l.ensure(&buf, 99, None));
        assert!(l.valid_lines() >= 100);
        assert_eq!(l.line_state(500), None);
    }

    #[test]
    fn budget_lexes_at_least_one_line() {
        let (buf, mut l) = layer(&"select 1;\n".repeat(999));
        assert!(!l.ensure(&buf, 999, Some(Duration::ZERO)));
        assert!(l.valid_lines() >= 2);
        assert!(l.lexed_lines() >= 1);
    }

    #[test]
    fn edit_converges_quickly() {
        let (mut buf, mut l) = layer(&"select 1;\n".repeat(99_999));
        assert_eq!(buf.len_lines(), 100_000);
        assert!(l.ensure(&buf, 99_999, None));
        let before = l.lexed_lines();
        let c = buf.insert(
            Pos {
                line: 50_000,
                col: 4,
            },
            "x",
        );
        l.apply(&buf, &c);
        assert!(l.ensure(&buf, 50_010, None));
        assert!(
            l.lexed_lines() - before <= 3,
            "lexed {}",
            l.lexed_lines() - before
        );
        assert_eq!(l.valid_lines(), 100_000);
    }

    #[test]
    fn unterminated_comment_at_top_stays_lazy() {
        let (mut buf, mut l) = layer(&"select 1;\n".repeat(99_999));
        assert!(l.ensure(&buf, 99_999, None));
        let c = buf.insert(Pos::default(), "/*");
        l.apply(&buf, &c);
        assert!(l.ensure(&buf, 60, None));
        let tokens = line_tokens(&l, &buf, 40);
        assert!(!tokens.is_empty());
        assert!(tokens.iter().all(|t| t.kind == TokenKind::Comment));
        assert!(l.valid_lines() <= 62, "valid {}", l.valid_lines());

        let c = buf.delete(Pos::default(), Pos { line: 0, col: 2 });
        l.apply(&buf, &c);
        assert!(l.ensure(&buf, 60, None));
        assert!(
            line_tokens(&l, &buf, 40)
                .iter()
                .any(|t| t.kind == TokenKind::Keyword)
        );
        // Line 61 ends in its old state again, so lexing it converges with the old pass.
        let before = l.lexed_lines();
        assert!(l.ensure(&buf, 61, None));
        assert_eq!(l.lexed_lines() - before, 1);
        assert_eq!(l.valid_lines(), 100_000);
    }

    fn long_line() -> String {
        let unit = "select 'abc', 12 ;";
        unit.repeat((1 << 20) / unit.len() + 1)
    }

    #[test]
    fn long_line_edit_relexes_locally() {
        let text = long_line();
        let (mut buf, mut l) = layer(&text);
        assert!(l.ensure(&buf, 0, None));
        let before = l.lexed_bytes();
        let c = buf.insert(
            Pos {
                line: 0,
                col: 500_000,
            },
            "x",
        );
        l.apply(&buf, &c);
        assert!(l.ensure(&buf, 0, None));
        let grown = l.lexed_bytes() - before;
        assert!(
            grown < 3 * CHECKPOINT_BYTES as u64 + 64,
            "lexed {grown} bytes"
        );

        // The reused tail still matches a full lex of the edited line.
        let edited = buf.line(0).into_owned();
        let syntax = l.syntax().clone();
        for window in [499_900..500_300, 900_000..900_100, 0..64] {
            let mut out = Vec::new();
            assert!(l.tokens(&buf, 0, window.clone(), &mut out));
            assert_eq!(out, full_lex(&syntax, &edited, window));
        }
    }

    #[test]
    fn window_tokens_after_an_unrelexed_edit_lex_near_the_edit() {
        let text = long_line();
        let (mut buf, mut l) = layer(&text);
        assert!(l.ensure(&buf, 0, None));
        let c = buf.insert(
            Pos {
                line: 0,
                col: 500_000,
            },
            "x",
        );
        l.apply(&buf, &c);
        // No `ensure` yet: the line is pending, as when the popup reads the caret context in
        // the frame of the keystroke.
        let edited = buf.line(0).into_owned();
        let syntax = l.syntax().clone();
        let before = l.window_lexed_bytes();
        let mut out = Vec::new();
        assert!(l.tokens(&buf, 0, 497_000..500_001, &mut out));
        let grown = l.window_lexed_bytes() - before;
        assert!(
            grown < 6 * CHECKPOINT_BYTES as u64,
            "lexed {grown} bytes for a 3 KB window"
        );
        assert_eq!(out, full_lex(&syntax, &edited, 497_000..500_001));
        // A window past the edit is still right.
        assert!(l.tokens(&buf, 0, 900_000..900_100, &mut out));
        assert_eq!(out, full_lex(&syntax, &edited, 900_000..900_100));
    }

    #[test]
    fn resume_point_of_a_pending_long_line_is_near_the_window() {
        let text = long_line();
        let (mut buf, mut l) = layer(&text);
        assert!(l.ensure(&buf, 0, None));
        let c = buf.insert(
            Pos {
                line: 0,
                col: 500_000,
            },
            "x",
        );
        l.apply(&buf, &c);
        let (at, state) = l.resume_point(&buf, 468_000).expect("lexed");
        assert!(
            (468_000 - 2 * CHECKPOINT_BYTES..=468_000).contains(&at),
            "{at}"
        );
        assert_eq!(state, LexState::Normal);
    }

    #[test]
    fn window_tokens_match_full_line() {
        let text = long_line();
        let (buf, mut l) = layer(&text);
        assert!(l.ensure(&buf, 0, None));
        let mut out = Vec::new();
        assert!(l.tokens(&buf, 0, 600_000..600_200, &mut out));
        assert!(!out.is_empty());
        assert_eq!(out, full_lex(l.syntax(), &text, 600_000..600_200));
    }

    #[test]
    fn escape_at_line_end_matches_full_text() {
        // Lines are lexed without their terminator: a trailing backslash in E'…' escapes the
        // newline itself, so the next line starts unescaped, exactly as in a full-text lex.
        let text = "select E'a\\\n'x";
        let (buf, mut l) = layer(text);
        assert!(l.ensure(&buf, 1, None));
        assert_eq!(l.line_state(1), Some(LexState::Str { escapes: true }));
        let mut full = Vec::new();
        l.syntax().lex(text, LexState::Normal, &mut full);
        // In the full text the string closes at the `'` after the newline, then `x` follows.
        let line1 = buf.char_index(Pos { line: 1, col: 0 });
        assert_eq!(full[1].end, line1 + 1);
        let tokens = line_tokens(&l, &buf, 1);
        let kinds: Vec<(usize, usize, TokenKind)> =
            tokens.iter().map(|t| (t.start, t.end, t.kind)).collect();
        assert_eq!(kinds, [(0, 1, TokenKind::String), (1, 2, TokenKind::Ident)]);
    }

    #[test]
    fn statement_at_caret_positions() {
        let text = "select 1;\nselect ';' ;\n\nselect 3;\n  ";
        let (buf, mut l) = layer(text);
        let at = |l: &mut SyntaxLayer, line, col| {
            let c = buf.char_index(Pos { line, col });
            l.statement_at(&buf, c).map(|r| text_of(&buf, r))
        };
        assert_eq!(at(&mut l, 0, 3).as_deref(), Some("select 1"));
        assert_eq!(at(&mut l, 1, 8).as_deref(), Some("select ';'"));
        assert_eq!(at(&mut l, 2, 0).as_deref(), Some("select ';'"));
        assert_eq!(at(&mut l, 4, 2).as_deref(), Some("select 3"));
        assert_eq!(at(&mut l, 0, 0).as_deref(), Some("select 1"));
        assert_eq!(at(&mut l, 0, 9).as_deref(), Some("select 1"));

        let func = "select 0;\ncreate function f() returns int as $$\nbegin\n  return 1;\nend;\n$$ language plpgsql;\nselect 2;";
        let (buf, mut l) = layer(func);
        let c = buf.char_index(Pos { line: 3, col: 4 });
        let r = l.statement_at(&buf, c).expect("a statement");
        assert!(text_of(&buf, r.clone()).starts_with("create function f()"));
        assert!(text_of(&buf, r).ends_with("language plpgsql"));
    }

    #[test]
    fn mongo_regions_split_from_their_start_state() {
        // The region for the last line starts on line 1, inside the block comment line 0
        // opened: splitting it from `Normal` would read `*/ … "/*` as a regex and a string.
        let text = "db.a.find() /* c\na' */ db.b.find(); db.c.find({s: \"'; db.zz.find()\"})\ndb.d.find();\ndb.e.find()";
        let buf = TextBuffer::from_str(text);
        let mut l = SyntaxLayer::with_language(SqlSyntax::ansi(), QueryLanguage::MongoShell, &buf);
        let at = |l: &mut SyntaxLayer, line, col| {
            let c = buf.char_index(Pos { line, col });
            l.statement_at(&buf, c).map(|r| text_of(&buf, r))
        };
        assert_eq!(at(&mut l, 3, 4).as_deref(), Some("db.e.find()"));
        assert_eq!(at(&mut l, 2, 4).as_deref(), Some("db.d.find()"));
        assert_eq!(
            at(&mut l, 1, 25).as_deref(),
            Some("db.c.find({s: \"'; db.zz.find()\"})")
        );
    }

    #[test]
    fn es_statement_at_reads_only_the_request_around_the_caret() {
        let mut text = String::new();
        for i in 0..2000 {
            text.push_str(&format!("GET idx/_doc/{i}\n\n"));
        }
        text.push_str("POST idx/_search\n{\n  \"query\": {}\n}\n");
        let buf = TextBuffer::from_str(&text);
        let mut l = SyntaxLayer::with_language(SqlSyntax::ansi(), QueryLanguage::EsConsole, &buf);
        let last = buf.len_lines() - 3;
        let c = buf.char_index(Pos { line: last, col: 4 });
        let r = l.statement_at(&buf, c).expect("a request");
        assert_eq!(text_of(&buf, r), "POST idx/_search\n{\n  \"query\": {}\n}");
        assert!(l.split_bytes() < 1000, "split {} bytes", l.split_bytes());
        let c = buf.char_index(Pos { line: 2000, col: 2 });
        let r = l.statement_at(&buf, c).expect("a request");
        assert_eq!(text_of(&buf, r), "GET idx/_doc/1000");
    }

    #[test]
    fn other_languages_lex_and_split_with_their_own_rules() {
        let text = "db.c.find({_id: ObjectId(\"65\")})\n  .limit(1)\n/* open\nstill */ db.d.drop()";
        let buf = TextBuffer::from_str(text);
        let mut l = SyntaxLayer::with_language(SqlSyntax::ansi(), QueryLanguage::MongoShell, &buf);
        assert_eq!(l.language(), QueryLanguage::MongoShell);
        l.ensure(&buf, 3, None);
        let toks = line_tokens(&l, &buf, 0);
        let line0 = buf.rope().line(0).to_string();
        let kind = |w: &str| {
            toks.iter()
                .find(|t| &line0[t.start..t.end] == w)
                .map(|t| t.kind)
        };
        assert_eq!(kind("ObjectId"), Some(TokenKind::Function));
        assert_eq!(kind("db"), Some(TokenKind::Keyword));
        assert_eq!(l.line_state(3), Some(LexState::BlockComment { depth: 1 }));
        // A chained `.limit()` on the next line belongs to the statement; the next line
        // break starts a new one.
        let at = |l: &mut SyntaxLayer, line, col| {
            let c = buf.char_index(Pos { line, col });
            l.statement_at(&buf, c).map(|r| text_of(&buf, r))
        };
        assert_eq!(
            at(&mut l, 1, 4).as_deref(),
            Some("db.c.find({_id: ObjectId(\"65\")})\n  .limit(1)")
        );
        assert_eq!(at(&mut l, 3, 12).as_deref(), Some("db.d.drop()"));

        let es = "GET idx/_search\n{\"query\": {}}\n\nDELETE idx";
        let buf = TextBuffer::from_str(es);
        let mut l = SyntaxLayer::with_language(SqlSyntax::ansi(), QueryLanguage::EsConsole, &buf);
        l.ensure(&buf, 3, None);
        let toks = line_tokens(&l, &buf, 0);
        assert_eq!(toks[0].kind, TokenKind::Keyword);
        assert_eq!(toks[1].kind, TokenKind::Ident);
        let c = buf.char_index(Pos { line: 1, col: 2 });
        let r = l.statement_at(&buf, c).unwrap();
        assert_eq!(text_of(&buf, r), "GET idx/_search\n{\"query\": {}}");
    }

    #[test]
    fn statement_within_gives_up_on_a_large_region_without_copying_it() {
        let mut text = String::from("select 1;\nselect\n");
        for _ in 0..1000 {
            text.push_str("  a + b,\n");
        }
        text.push_str("  c\nfrom t;\nselect 3;\nselect 4;\n");
        let (buf, mut l) = layer(&text);
        let mid = buf.char_index(Pos { line: 500, col: 3 });
        let whole = l.statement_at(&buf, mid).expect("a statement");
        l.split_bytes = 0;
        // Within the limit: the same statement.
        assert_eq!(l.statement_within(&buf, mid, text.len()), Ok(Some(whole)));
        // Over it: the region (from line 0, two `;` lines back, to the end), uncopied.
        l.split_bytes = 0;
        assert_eq!(l.statement_within(&buf, mid, 4096), Err(0..text.len()));
        assert_eq!(l.split_bytes(), 0);
        // Two `;` lines past it, a small statement's region fits again.
        let last = buf.char_index(Pos { line: 1005, col: 3 });
        let r = l
            .statement_within(&buf, last, 4096)
            .expect("a small region");
        assert_eq!(r.map(|r| text_of(&buf, r)).as_deref(), Some("select 4"));
    }

    #[test]
    fn statement_spanning_lines_after_semicolon_line() {
        // The region starts mid-statement ("2\n;"); the caret after it still finds it whole.
        let text = "select 1;\nselect\n2\n;\n\n";
        let (buf, mut l) = layer(text);
        let r = l.statement_at(&buf, buf.len_chars()).expect("a statement");
        assert_eq!(text_of(&buf, r), "select\n2");
        let c = buf.char_index(Pos { line: 1, col: 3 });
        let r = l.statement_at(&buf, c).expect("a statement");
        assert_eq!(text_of(&buf, r), "select\n2");
        // A caret in whitespace inside a statement belongs to it.
        let text = "a;\nselect 1  \nfrom t;";
        let (buf, mut l) = layer(text);
        let c = buf.char_index(Pos { line: 1, col: 10 });
        let r = l.statement_at(&buf, c).expect("a statement");
        assert_eq!(text_of(&buf, r), "select 1  \nfrom t");
    }

    #[test]
    fn statements_beginning_after_a_statement_on_the_same_line() {
        // The selection starts at the second statement, after the first one ends on the
        // same line: the split range before the selection must be skipped, not sliced.
        let text = "select 1; select 2";
        let (buf, mut l) = layer(text);
        let start = text.find("select 2").expect("second statement");
        let start_char = text[..start].chars().count();
        let all = l.statements(&buf, start_char..buf.len_chars());
        assert_eq!(all, vec![start_char..buf.len_chars()]);
        assert_eq!(text_of(&buf, all[0].clone()), "select 2");
    }

    #[test]
    fn statements_with_multibyte_origins() {
        let text = "select 'é🚀';\nselect 2";
        let (buf, mut l) = layer(text);
        let all = l.statements(&buf, 0..buf.len_chars());
        assert_eq!(all, vec![0..11, 13..21]);
        assert_eq!(text_of(&buf, all[1].clone()), "select 2");
        // A selection is split as selected.
        let part = l.statements(&buf, 3..buf.len_chars());
        assert_eq!(part, vec![3..11, 13..21]);
    }

    /// A tiny deterministic generator for the edit fuzz test.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as usize) % n
        }
    }

    #[test]
    fn incremental_matches_fresh_lex() {
        let pieces = [
            "/*",
            "*/",
            "'",
            "\"",
            ";",
            "\n",
            "$$",
            "x",
            " ",
            "--",
            "\nselect 1;\n",
        ];
        let mut rng = Lcg(7);
        let base = "select 1; -- c\n/* a */ select 'x';\n\"q\" $$ body; $$;\n".repeat(40);
        let (mut buf, mut l) = layer(&base);
        for round in 0..300 {
            // One to three edits before each ensure, like a multi-caret keystroke.
            for _ in 0..1 + rng.next(3) {
                let len = buf.len_chars();
                let at = rng.next(len + 1);
                let c = if rng.next(3) == 0 && at < len {
                    let end = (at + 1 + rng.next(8)).min(len);
                    buf.replace(at..end, "")
                } else {
                    buf.replace(at..at, pieces[rng.next(pieces.len())])
                };
                l.apply(&buf, &c);
            }
            let upto = rng.next(buf.len_lines());
            l.ensure(
                &buf,
                upto,
                if round % 2 == 0 {
                    None
                } else {
                    Some(Duration::ZERO)
                },
            );
            let mut fresh = SyntaxLayer::new(pg_like(), &buf);
            assert!(fresh.ensure(&buf, buf.len_lines() - 1, None));
            for i in 0..l.valid_lines() {
                assert_eq!(
                    l.line_state(i),
                    fresh.line_state(i),
                    "round {round} line {i}"
                );
                assert_eq!(
                    line_tokens(&l, &buf, i),
                    line_tokens(&fresh, &buf, i),
                    "round {round} line {i}"
                );
            }
            let all = 0..buf.len_chars();
            assert_eq!(
                l.statements(&buf, all.clone()),
                fresh.statements(&buf, all),
                "round {round}"
            );
        }
    }

    #[test]
    fn long_line_edits_match_fresh_lex() {
        let pieces = [
            "/*", "*/", "'", ";", "$$", "x", " ", "--", "é", "🚀", "(", "1e", "*", "-", "/", "$",
            "5",
        ];
        long_line_fuzz(&pieces, 11);
        long_line_fuzz(&pieces, 23);
        // Edits that open no construct re-lex about three checkpoints (from the one before
        // the edit to one past the reuse point), not the whole line. Deletes may still cut a
        // quote or comment open, so the bound allows four on average.
        let (lexed, line) = long_line_fuzz(&["x", " ", "é", "🚀", "(", ";", "1e"], 5);
        let bound = 200 * 4 * CHECKPOINT_BYTES as u64;
        assert!(
            bound < 200 * line / 3,
            "the bound must stay well below full re-lexes"
        );
        assert!(
            lexed < bound,
            "{lexed} bytes for 200 edits of a {line}-byte line"
        );
    }

    /// 200 random edits inside a long line, each checked against a fresh layer. Returns the
    /// bytes the edits lexed and the line's initial length.
    fn long_line_fuzz(pieces: &[&str], seed: u64) -> (u64, u64) {
        let mut rng = Lcg(seed);
        let unit = "select 'a;b', 12 ; /* c; */ x$$ y; $$ \"q;\" 3.5e+2; a/(b)-(c)1e+x $abc d;";
        let base = format!(
            "{}\nselect 1;\nselect 'z';",
            unit.repeat(64 * 1024 / unit.len())
        );
        let (mut buf, mut l) = layer(&base);
        assert!(l.ensure(&buf, 2, None));
        let initial = buf.line_len_bytes(0) as u64;
        let mut local_bytes = 0;
        for round in 0..200 {
            let len0 = buf.line_len_chars(0);
            let mut at = rng.next(len0 + 1);
            // Half the edits land on, or up to 3 bytes after, an existing checkpoint.
            if rng.next(2) == 0
                && let Some(cps) = l.lines[0].checkpoints.as_deref()
                && !cps.is_empty()
            {
                let byte = cps[rng.next(cps.len())].at as usize + rng.next(4);
                at = buf.rope().byte_to_char(byte.min(buf.line_len_bytes(0)));
            }
            let c = if rng.next(3) == 0 && at < len0 {
                let end = (at + 1 + rng.next(6)).min(len0);
                buf.replace(at..end, "")
            } else {
                buf.replace(at..at, pieces[rng.next(pieces.len())])
            };
            l.apply(&buf, &c);
            let before = l.lexed_bytes();
            assert!(l.ensure(&buf, 2, None));
            local_bytes += l.lexed_bytes() - before;
            let mut fresh = SyntaxLayer::new(pg_like(), &buf);
            assert!(fresh.ensure(&buf, 2, None));
            for i in 0..3 {
                assert_eq!(
                    l.line_state(i),
                    fresh.line_state(i),
                    "round {round} line {i}"
                );
            }
            let line_len = buf.line_len_bytes(0);
            let s = rng.next(line_len);
            for window in [0..line_len, s..s + 300] {
                let (mut a, mut b) = (Vec::new(), Vec::new());
                assert!(l.tokens(&buf, 0, window.clone(), &mut a));
                assert!(fresh.tokens(&buf, 0, window.clone(), &mut b));
                assert_eq!(a, b, "round {round} window {window:?}");
            }
            let all = 0..buf.len_chars();
            assert_eq!(
                l.statements(&buf, all.clone()),
                fresh.statements(&buf, all),
                "round {round}"
            );
            let caret = rng.next(buf.len_chars() + 1);
            assert_eq!(
                l.statement_at(&buf, caret),
                fresh.statement_at(&buf, caret),
                "round {round}"
            );
        }
        (local_bytes, initial)
    }

    /// A long line whose byte 8192 (a checkpoint) is `pattern[cp]`, then a short second line.
    fn line_with_checkpoint_at(pattern: &str, cp: usize) -> String {
        let lead = 2 * CHECKPOINT_BYTES - cp;
        let mut filler = "a ".repeat(lead / 2);
        if lead % 2 == 1 {
            filler.push(' ');
        }
        format!("{filler}{pattern}{}\nselect 1;", " a".repeat(20_000))
    }

    #[test]
    fn edits_next_to_checkpoints_relex_from_a_safe_point() {
        let at = 2 * CHECKPOINT_BYTES;
        // (pattern, index of the checkpointed token in it, inserted text, offset from it)
        let cases = [
            ("/(b);", 1, "*", 0),      // `/` + `*` opens a comment before the checkpoint
            ("-(b);", 1, "-", 0),      // `-` + `-` opens a line comment before it
            ("1e(x);", 1, "5", 1),     // `1e` + `5` is one number `1e5`
            ("1e+x;", 1, "5", 2),      // `1e+` + `5` is one number `1e+5`
            ("$abcdef x;", 1, "$", 4), // `$abcd$` opens a dollar quote from before it
        ];
        for (pattern, cp, insert, off) in cases {
            let text = line_with_checkpoint_at(pattern, cp);
            assert_eq!(&text[at - cp..at - cp + pattern.len()], pattern);
            let (mut buf, mut l) = layer(&text);
            assert!(l.ensure(&buf, 1, None));
            let c = buf.insert(
                Pos {
                    line: 0,
                    col: at + off,
                },
                insert,
            );
            l.apply(&buf, &c);
            assert!(l.ensure(&buf, 1, None));
            let mut fresh = SyntaxLayer::new(pg_like(), &buf);
            assert!(fresh.ensure(&buf, 1, None));
            assert_eq!(l.line_state(1), fresh.line_state(1), "{pattern} + {insert}");
            assert_eq!(
                line_tokens(&l, &buf, 0),
                line_tokens(&fresh, &buf, 0),
                "{pattern} + {insert}"
            );
            let all = 0..buf.len_chars();
            assert_eq!(
                l.statements(&buf, all.clone()),
                fresh.statements(&buf, all),
                "{pattern}"
            );
        }
    }

    #[test]
    fn unclosed_construct_relex_is_linear() {
        let text = long_line();
        let (mut buf, mut l) = layer(&text);
        assert!(l.ensure(&buf, 0, None));
        let before = l.lexed_bytes();
        let c = buf.insert(
            Pos {
                line: 0,
                col: 300_000,
            },
            "/*",
        );
        l.apply(&buf, &c);
        assert!(l.ensure(&buf, 0, None));
        let grown = l.lexed_bytes() - before;
        let rest = (buf.line_len_bytes(0) - 300_000) as u64;
        assert!(
            grown < 2 * rest,
            "lexed {grown} bytes for a {rest}-byte tail"
        );
        let len = buf.line_len_bytes(0);
        let mut out = Vec::new();
        assert!(l.tokens(&buf, 0, len - 10..len, &mut out));
        assert!(out.iter().all(|t| t.kind == TokenKind::Comment));
    }

    fn assert_matches_fresh(l: &mut SyntaxLayer, buf: &TextBuffer) {
        let last = buf.len_lines() - 1;
        assert!(l.ensure(buf, last, None));
        let mut fresh = SyntaxLayer::new(pg_like(), buf);
        assert!(fresh.ensure(buf, last, None));
        for i in 0..buf.len_lines() {
            assert_eq!(l.line_state(i), fresh.line_state(i), "line {i}");
        }
        let all = 0..buf.len_chars();
        assert_eq!(l.statements(buf, all.clone()), fresh.statements(buf, all));
    }

    #[test]
    fn bulk_single_line_changes_apply_fast() {
        use crate::editor::{EditCommand, EditorModel};
        // ToggleComment over Select All: one change per non-blank line, applied bottom-up.
        let mut m = EditorModel::new(&"select 1;\n\n".repeat(32_000));
        let mut l = SyntaxLayer::new(pg_like(), m.buffer());
        assert!(l.ensure(m.buffer(), m.buffer().len_lines() - 1, None));
        m.apply(EditCommand::SelectAll, 0.0);
        let changes = m.apply(EditCommand::ToggleComment, 0.0);
        assert_eq!(changes.len(), 32_000);
        let t = std::time::Instant::now();
        for c in &changes {
            l.apply(m.buffer(), c);
        }
        let took = t.elapsed();
        assert!(took < Duration::from_millis(500), "applying took {took:?}");
        assert_matches_fresh(&mut l, m.buffer());
    }

    #[test]
    fn bulk_line_count_changes_apply_fast() {
        use crate::editor::{Caret, EditCommand, EditorModel};
        // A newline at 32 000 carets: every change splits a line.
        let mut m = EditorModel::new(&"select 1;\n\n".repeat(32_000));
        let mut l = SyntaxLayer::new(pg_like(), m.buffer());
        assert!(l.ensure(m.buffer(), m.buffer().len_lines() - 1, None));
        let carets = (0..32_000)
            .map(|i| {
                Caret::at(Pos {
                    line: 2 * i,
                    col: 6,
                })
            })
            .collect();
        m.set_carets(carets);
        let changes = m.apply(EditCommand::Newline, 0.0);
        assert_eq!(changes.len(), 32_000);
        let t = std::time::Instant::now();
        l.apply_all(m.buffer(), &changes);
        let took = t.elapsed();
        assert!(took < Duration::from_millis(500), "applying took {took:?}");
        assert_matches_fresh(&mut l, m.buffer());
        // A small batch keeps far lines converging.
        let carets = vec![
            Caret::at(Pos { line: 0, col: 0 }),
            Caret::at(Pos {
                line: 50_000,
                col: 0,
            }),
        ];
        m.set_carets(carets);
        let changes = m.apply(EditCommand::Newline, 0.0);
        l.apply_all(m.buffer(), &changes);
        assert_matches_fresh(&mut l, m.buffer());
    }
}
