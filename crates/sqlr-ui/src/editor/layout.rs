//! Per-line galley cache, so an unchanged line is never laid out twice. A galley is keyed on
//! its text (a line, or a window of a long
//! line), the line's start state, the window start, the tokens coloring it and the colors.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, FontId, Galley};
use sqlr_core::{LexState, Token};

use crate::theme::EditorColors;

/// Galleys kept across frames.
pub(crate) const LAYOUT_CACHE_CAPACITY: usize = 4096;
/// Chars of text the cached galleys may hold (about 200 bytes of galley per char; a screen holds a few thousand). Without
/// this bound, retyping a long line fills the cache with one large galley per keystroke.
pub(crate) const LAYOUT_CACHE_CHARS: usize = 32_768;

pub(crate) struct LineLayoutCache {
    /// Galley, frame of last use, chars of its text.
    map: HashMap<u64, (Arc<Galley>, u64, usize)>,
    capacity: usize,
    chars: usize,
    /// Eviction passes run once `chars` passes this: the bound, or twice what the last pass
    /// kept when the frame's own galleys exceed the bound.
    chars_limit: usize,
    #[cfg(test)]
    evictions: u64,
    frame: u64,
    hits: u64,
    misses: u64,
}

impl LineLayoutCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            map: HashMap::new(),
            capacity,
            chars: 0,
            chars_limit: LAYOUT_CACHE_CHARS,
            #[cfg(test)]
            evictions: 0,
            frame: 0,
            hits: 0,
            misses: 0,
        }
    }

    /// Starts a frame; over capacity, drops the entries the previous frame didn't use.
    pub(crate) fn begin_frame(&mut self) {
        self.frame += 1;
        if self.map.len() > self.capacity || self.chars > self.chars_limit {
            let keep_from = self.frame - 1;
            self.map.retain(|_, (_, used, _)| *used >= keep_from);
            self.chars = self.map.values().map(|(_, _, n)| n).sum();
            self.chars_limit = LAYOUT_CACHE_CHARS.max(self.chars * 2);
            #[cfg(test)]
            {
                self.evictions += 1;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Chars of text held by the cached galleys.
    #[cfg(test)]
    pub(crate) fn cached_chars(&self) -> usize {
        self.chars
    }

    /// The galley of `text` colored by `tokens` (byte offsets into `text`). `tokens` is
    /// `None` past the lexing frontier: the text is then plain. Plain text is laid out in
    /// [`Color32::PLACEHOLDER`], so the painter's fallback color applies. `wrap` is the
    /// soft-wrap width in points, `None` when the line does not wrap.
    #[expect(
        clippy::too_many_arguments,
        reason = "the egui context, text, lexer state, window offset, wrap width, tokens, font and colors are all part of the galley cache key and are borrowed from different owners; a parameter struct would be built on every call just to be unpacked here"
    )]
    pub(crate) fn get(
        &mut self,
        ctx: &egui::Context,
        text: &str,
        start: Option<LexState>,
        window_start: usize,
        wrap: Option<f32>,
        tokens: Option<&[Token]>,
        font: &FontId,
        colors: &EditorColors,
    ) -> Arc<Galley> {
        let mut h = DefaultHasher::new();
        (text, start, window_start, font.size.to_bits(), &font.family).hash(&mut h);
        wrap.map(f32::to_bits).hash(&mut h);
        colors.hash(&mut h);
        if let Some(tokens) = tokens {
            for t in tokens {
                (t.start, t.end, t.kind).hash(&mut h);
            }
        }
        let key = h.finish();
        if let Some((g, used, _)) = self.map.get_mut(&key) {
            *used = self.frame;
            self.hits += 1;
            return g.clone();
        }
        self.misses += 1;
        let g =
            ctx.fonts_mut(|f| f.layout_job(job(text, wrap, tokens.unwrap_or(&[]), font, colors)));
        let n = text.chars().count();
        self.chars += n;
        self.map.insert(key, (g.clone(), self.frame, n));
        g
    }

    pub(crate) fn hits(&self) -> u64 {
        self.hits
    }

    pub(crate) fn misses(&self) -> u64 {
        self.misses
    }
}

fn job(
    text: &str,
    wrap: Option<f32>,
    tokens: &[Token],
    font: &FontId,
    colors: &EditorColors,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    if let Some(max_width) = wrap {
        job.wrap = TextWrapping {
            max_width,
            break_anywhere: true,
            ..Default::default()
        };
    }
    let fmt = |color| TextFormat::simple(font.clone(), color);
    let mut at = 0;
    for t in tokens {
        let (s, e) = (t.start.min(text.len()), t.end.min(text.len()));
        if s > at {
            job.append(&text[at..s], 0.0, fmt(Color32::PLACEHOLDER));
        }
        let s = s.max(at);
        if e > s {
            job.append(&text[s..e], 0.0, fmt(colors.token(t.kind)));
        }
        at = at.max(e);
    }
    if at < text.len() {
        job.append(&text[at..], 0.0, fmt(Color32::PLACEHOLDER));
    }
    job
}

#[cfg(test)]
mod tests {
    use sqlr_core::TokenKind;

    use super::*;

    fn in_frame(f: impl FnOnce(&egui::Context)) {
        let ctx = egui::Context::default();
        let mut f = Some(f);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            (f.take().expect("runs once"))(ui.ctx())
        });
        // No renderer consumes the font atlas upload here.
        out.textures_delta.clear();
    }

    #[test]
    fn same_line_hits_and_changes_miss() {
        in_frame(|ctx| {
            let mut c = LineLayoutCache::new(LAYOUT_CACHE_CAPACITY);
            let font = egui::FontId::monospace(13.0);
            let dark = EditorColors::dark();
            let kw = [Token {
                start: 0,
                end: 6,
                kind: TokenKind::Keyword,
            }];
            let st = Some(LexState::Normal);
            c.get(ctx, "SELECT 1", st, 0, None, Some(&kw), &font, &dark);
            c.get(ctx, "SELECT 1", st, 0, None, Some(&kw), &font, &dark);
            assert_eq!((c.hits(), c.misses()), (1, 1));
            c.get(ctx, "SELECT 1", None, 0, None, None, &font, &dark);
            c.get(ctx, "SELECT 1", st, 128, None, Some(&kw), &font, &dark);
            c.get(ctx, "SELECT 2", st, 0, None, Some(&kw), &font, &dark);
            assert_eq!((c.hits(), c.misses()), (1, 4));
            // A theme switch recolors: the same line under other colors is a miss.
            c.get(
                ctx,
                "SELECT 1",
                st,
                0,
                None,
                Some(&kw),
                &font,
                &EditorColors::light(),
            );
            assert_eq!((c.hits(), c.misses()), (1, 5));
        });
    }

    #[test]
    fn eviction_drops_unused_entries() {
        in_frame(|ctx| {
            let mut c = LineLayoutCache::new(2);
            let font = egui::FontId::monospace(13.0);
            let dark = EditorColors::dark();
            for l in ["A", "B", "C"] {
                c.get(ctx, l, None, 0, None, None, &font, &dark);
            }
            c.begin_frame();
            c.get(ctx, "A", None, 0, None, None, &font, &dark);
            c.begin_frame();
            let (h0, m0) = (c.hits(), c.misses());
            c.get(ctx, "A", None, 0, None, None, &font, &dark);
            assert_eq!((c.hits(), c.misses()), (h0 + 1, m0), "A survives");
            c.get(ctx, "B", None, 0, None, None, &font, &dark);
            assert_eq!((c.hits(), c.misses()), (h0 + 1, m0 + 1), "B was evicted");
        });
    }

    #[test]
    fn eviction_bounds_the_cached_chars() {
        in_frame(|ctx| {
            let mut c = LineLayoutCache::new(LAYOUT_CACHE_CAPACITY);
            let font = egui::FontId::monospace(13.0);
            let dark = EditorColors::dark();
            // 400 distinct 1 000-char lines, one per frame: far fewer entries than the
            // count capacity, but 400 000 chars.
            for i in 0..400 {
                c.begin_frame();
                c.get(
                    ctx,
                    &format!("{i:04}{}", "x".repeat(996)),
                    None,
                    0,
                    None,
                    None,
                    &font,
                    &dark,
                );
            }
            c.begin_frame();
            assert!(
                c.cached_chars() <= LAYOUT_CACHE_CHARS,
                "{}",
                c.cached_chars()
            );
        });
    }

    #[test]
    fn a_frame_over_the_char_bound_does_not_evict_every_frame() {
        in_frame(|ctx| {
            let mut c = LineLayoutCache::new(LAYOUT_CACHE_CAPACITY);
            let font = egui::FontId::monospace(13.0);
            let dark = EditorColors::dark();
            // One frame's own galleys hold more chars than the bound (3 x 50 000).
            let lines: Vec<String> = (0..3)
                .map(|i| format!("{i}{}", "y".repeat(49_999)))
                .collect();
            for _ in 0..10 {
                c.begin_frame();
                for l in &lines {
                    c.get(ctx, l, None, 0, None, None, &font, &dark);
                }
            }
            assert!(c.cached_chars() > LAYOUT_CACHE_CHARS);
            assert!(c.evictions() <= 1, "{} eviction passes", c.evictions());
        });
    }
}
