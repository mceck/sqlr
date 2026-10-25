//! Headless frame-time measurement of the query editor (CPU only, no GPU renderer): the
//! Performance harness for `EditorState`.
//!
//! `cargo run --release -p sqlr-ui --example editor_measure` prints a table and writes
//! `target/editor-measure.json`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::Harness;
use sqlr_core::{CatalogColumn, CatalogRelation, RelationKind, SchemaCatalog, SqlSyntax};
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{
    Caret, CompletionSource, EditorState, PALETTE_ROWS, PaletteGroup, PaletteItem, Pos,
    SyntaxLayer, TextBuffer, fuzzy_rank, query_editor,
};

// ---------------------------------------------------------------------------------------
// Fixtures use a deterministic xorshift64 random number generator.
// ---------------------------------------------------------------------------------------

const TABLES: &[&str] = &[
    "users",
    "orders",
    "invoices",
    "products",
    "sessions",
    "events",
    "line_items",
];
const COLUMNS: &[&str] = &[
    "id",
    "email",
    "name",
    "created_at",
    "total",
    "status",
    "user_id",
    "amount",
    "note",
];
const TEXTS: &[&str] = &[
    "hello",
    "caffè è pronto",
    "日本語のテキスト",
    "launch 🚀 now",
    "it''s fine",
    "plain text",
];

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self) -> bool {
        self.next() & 1 == 0
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

fn literal(rng: &mut Rng) -> String {
    if rng.chance() {
        rng.below(1_000_000).to_string()
    } else {
        format!("'{}'", rng.pick(TEXTS))
    }
}

fn statement(rng: &mut Rng) -> String {
    let t = rng.pick(TABLES);
    let c = rng.pick(COLUMNS);
    let c2 = rng.pick(COLUMNS);
    match rng.below(9) {
        0 => format!(
            "SELECT {c}, {c2} FROM {t} WHERE {c} = {} ORDER BY {c2} LIMIT 50",
            literal(rng)
        ),
        1 => format!(
            "INSERT INTO {t} ({c}, {c2}) VALUES ({}, {})",
            literal(rng),
            literal(rng)
        ),
        2 => format!(
            "UPDATE {t} SET {c} = {} WHERE {c2} > {}",
            literal(rng),
            rng.below(1000)
        ),
        3 => format!(
            "DELETE FROM {t} WHERE {c} IN ({}, {})",
            literal(rng),
            literal(rng)
        ),
        4 => format!(
            "CREATE TABLE {t}_{} (\n  {c} BIGINT PRIMARY KEY,\n  {c2} VARCHAR(200) NOT NULL\n)",
            rng.below(1000)
        ),
        5 => format!(
            "WITH recent AS (SELECT * FROM {t} WHERE {c} > {})\nSELECT count(*) FROM recent",
            literal(rng)
        ),
        6 => format!("-- {}\nSELECT {c} FROM {t}", rng.pick(TEXTS)),
        7 => format!(
            "/* {} */\nSELECT {c2} FROM {t} JOIN orders o ON o.user_id = {t}.id",
            rng.pick(TEXTS)
        ),
        _ => format!(
            "SELECT {c}, sum({c2}) AS total FROM {t} GROUP BY {c} HAVING sum({c2}) > {}",
            rng.below(500)
        ),
    }
}

/// SQL script of at least `target_bytes`, deterministic per `seed`.
fn generate_sql(target_bytes: usize, seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(target_bytes + 4096);
    while out.len() < target_bytes {
        out.push_str(&statement(&mut rng));
        out.push_str(";\n");
    }
    out
}

/// One `INSERT ... VALUES (...),(...);` statement on a single line of at least `line_bytes`.
fn generate_long_line_sql(line_bytes: usize, seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(line_bytes + 256);
    out.push_str("INSERT INTO events (id, user_id, note) VALUES ");
    let mut first = true;
    while out.len() < line_bytes {
        if !first {
            out.push(',');
        }
        first = false;
        let note = rng.pick(TEXTS);
        out.push_str(&format!(
            "({}, {}, '{note}')",
            rng.below(1_000_000),
            rng.below(10_000)
        ));
    }
    out.push(';');
    out
}

/// One `INSERT ... VALUES` statement of at least `bytes`, a tuple per line, ended by a single
/// `;`, between two short statements.
fn generate_multi_line_sql(bytes: usize, seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(bytes + 256);
    out.push_str("SELECT 1;\nINSERT INTO events (id, user_id, note) VALUES\n");
    let mut first = true;
    while out.len() < bytes {
        if !first {
            out.push_str(",\n");
        }
        first = false;
        let note = rng.pick(TEXTS);
        out.push_str(&format!(
            "({}, {}, '{note}')",
            rng.below(1_000_000),
            rng.below(10_000)
        ));
    }
    out.push_str(";\nSELECT 2;\n");
    out
}

// ---------------------------------------------------------------------------------------
// Completion stub: schema `public`, 2 000 tables of 20 columns
// ---------------------------------------------------------------------------------------

struct StubSource {
    schemas: Vec<String>,
    catalogs: HashMap<String, SchemaCatalog>,
    defaults: Vec<String>,
}

impl StubSource {
    fn new() -> Self {
        let columns: Vec<CatalogColumn> = COLUMNS
            .iter()
            .map(|c| c.to_string())
            .chain((COLUMNS.len()..20).map(|i| format!("col{i:02}")))
            .map(|name| CatalogColumn {
                name,
                type_name: "text".into(),
            })
            .collect();
        let names = TABLES
            .iter()
            .map(|t| t.to_string())
            .chain((0..2000 - TABLES.len()).map(|i| format!("t{i:04}")));
        let relations = names
            .map(|name| CatalogRelation {
                name,
                kind: RelationKind::Table,
                columns: columns.clone(),
            })
            .collect();
        let mut catalogs = HashMap::new();
        catalogs.insert(
            "public".to_string(),
            SchemaCatalog {
                schema: "public".into(),
                relations,
            },
        );
        Self {
            schemas: vec!["public".into()],
            catalogs,
            defaults: vec!["public".into()],
        }
    }
}

impl CompletionSource for StubSource {
    fn available(&self) -> bool {
        true
    }

    fn schemas(&self) -> Option<&[String]> {
        Some(&self.schemas)
    }

    fn schema(&self, name: &str) -> Option<&SchemaCatalog> {
        self.catalogs.get(name)
    }

    fn default_schemas(&self) -> &[String] {
        &self.defaults
    }
}

// ---------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------

struct State {
    editor: EditorState,
    source: StubSource,
    themed: bool,
    frames: u64,
    galley_hits: u64,
    galley_misses: u64,
    scope_parses: Vec<Duration>,
    popup_frames: u64,
}

type H = Harness<'static, State>;

/// `--wrap` measures the `big` phase with soft wrap on.
static WRAP: AtomicBool = AtomicBool::new(false);

fn harness(mut editor: EditorState) -> H {
    if WRAP.load(Ordering::Relaxed) {
        editor.set_soft_wrap(true);
    }
    let state = State {
        editor,
        source: StubSource::new(),
        themed: false,
        frames: 0,
        galley_hits: 0,
        galley_misses: 0,
        scope_parses: Vec::new(),
        popup_frames: 0,
    };
    Harness::builder()
        .with_size(vec2(1200.0, 800.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, s: &mut State| {
                if !s.themed {
                    theme::install_fonts(ui.ctx(), &[]);
                    s.themed = true;
                }
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                // Jumps must land in one frame, not animate over several.
                ui.ctx().all_styles_mut(|st| {
                    st.scroll_animation = egui::style::ScrollAnimation::none()
                });
                query_editor(ui, &mut s.editor, &s.source);
                let fs = s.editor.frame_stats();
                s.frames += 1;
                s.popup_frames += u64::from(s.editor.completion_open());
                s.galley_hits += fs.galley_hits;
                s.galley_misses += fs.galley_misses;
                if let Some(d) = fs.scope_parse {
                    s.scope_parses.push(d);
                }
            },
            state,
        )
}

fn timed_step(h: &mut H, out: &mut Vec<Duration>) {
    let t = Instant::now();
    h.step();
    out.push(t.elapsed());
}

fn focus(h: &mut H) {
    h.state_mut().editor.request_focus();
    h.run_steps(2);
}

/// Puts the caret at `p` and scrolls it into view (the next frame draws the jump).
fn jump_to(h: &mut H, p: Pos) {
    h.state_mut().editor.set_carets(vec![Caret::at(p)]);
    h.state_mut().editor.reveal_caret();
}

/// Queues one key press. `Harness::step` runs one frame per queued event, so the press is a
/// single event (no modifier or release events) and the step that follows is a single frame.
fn key(h: &H, modifiers: Modifiers, k: Key) {
    h.event(Event::Key {
        key: k,
        physical_key: Some(k),
        pressed: true,
        repeat: false,
        modifiers,
    });
}

fn rss_mb() -> f64 {
    memory_stats::memory_stats().map_or(0.0, |m| m.physical_mem as f64 / 1_048_576.0)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn pct(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[i]
}

struct Stats {
    name: &'static str,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
    steps: usize,
    /// Scenario sanity counters (for example how often the popup opened).
    checks: Vec<(&'static str, usize)>,
}

fn stats(name: &'static str, d: &[Duration], checks: Vec<(&'static str, usize)>) -> Stats {
    let mut v: Vec<f64> = d.iter().copied().map(ms).collect();
    v.sort_by(f64::total_cmp);
    Stats {
        name,
        p50: pct(&v, 0.5),
        p95: pct(&v, 0.95),
        p99: pct(&v, 0.99),
        max: *v.last().unwrap(),
        steps: v.len(),
        checks,
    }
}

// ---------------------------------------------------------------------------------------
// Phases: each runs in its own process, so a phase's RSS counts one document only
// ---------------------------------------------------------------------------------------

const BAR_FRAME_MS: f64 = 8.0;
const BAR_SCOPE_MS: f64 = 5.0;
const BAR_FIRST_FRAME_MS: f64 = 200.0;
const BAR_RSS_MB: f64 = 120.0;

/// Samples the resident set every 2 ms on a thread, so the peak is a peak.
struct PeakRss {
    max_kb: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PeakRss {
    fn start() -> Self {
        let max_kb = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (m, s) = (max_kb.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                m.fetch_max((rss_mb() * 1024.0) as u64, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        Self {
            max_kb,
            stop,
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> f64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.max_kb.load(Ordering::Relaxed) as f64 / 1024.0
    }
}

const BIG_BYTES: usize = 10 * 1024 * 1024;

/// The 10 MB document as a fresh editor; the source text is dropped on return.
fn big_editor() -> EditorState {
    let text = generate_sql(BIG_BYTES, 42);
    let mut editor = EditorState::new(&text, SqlSyntax::ansi());
    if WRAP.load(Ordering::Relaxed) {
        editor.set_soft_wrap(true);
    }
    editor
}

struct PhaseOut {
    scenarios: Vec<Stats>,
    /// (name, value, bar) in ms; a bar of `None` is informational.
    scalars: Vec<(&'static str, f64, Option<f64>)>,
    rss_mb: f64,
}

fn big_phase() -> PhaseOut {
    let rss = PeakRss::start();
    let mut scenarios = Vec::new();
    let mut hits = 0u64;
    let mut misses = 0u64;
    let mut scope_parses: Vec<Duration> = Vec::new();
    let mut absorb = |h: &H| {
        hits += h.state().galley_hits;
        misses += h.state().galley_misses;
        scope_parses.extend(h.state().scope_parses.iter().copied());
    };

    // First frame: from state construction to the end of the first step.
    let t0 = Instant::now();
    let mut h = harness(big_editor());
    h.step();
    let first_frame = t0.elapsed();
    focus(&mut h);
    let n = h.state().editor.model().buffer().len_lines();
    // The `orders o` statement, found in the pristine document before any edit.
    let at_line = {
        let buf = h.state().editor.model().buffer();
        (n / 2..n)
            .find(|&i| buf.line(i).contains("JOIN orders o ON o.user_id"))
            .expect("fixture has an orders alias")
    };

    // scroll: 2 000 jumps across the file; every frame until the target line is drawn.
    let mut d = Vec::new();
    let mut landed = 0;
    for i in 0..2000 {
        let line = i * (n - 1) / 1999;
        jump_to(&mut h, Pos { line, col: 0 });
        for _ in 0..8 {
            timed_step(&mut h, &mut d);
            if h.state().editor.screen_pos(Pos { line, col: 0 }).is_some() {
                landed += 1;
                break;
            }
        }
    }
    scenarios.push(stats("scroll", &d, vec![("jumps landed of 2000", landed)]));

    // type_middle: 1 000 chars at line 115 k.
    let mid = 115_000.min(n / 2);
    jump_to(&mut h, Pos { line: mid, col: 0 });
    h.run_steps(2);
    let mut d = Vec::new();
    for _ in 0..1000 {
        h.event(Event::Text("x".into()));
        timed_step(&mut h, &mut d);
    }
    scenarios.push(stats("type_middle", &d, vec![]));

    // enter_backspace: 200 pairs.
    let mut d = Vec::new();
    for _ in 0..200 {
        for k in [Key::Enter, Key::Backspace] {
            key(&h, Modifiers::NONE, k);
            timed_step(&mut h, &mut d);
        }
    }
    scenarios.push(stats("enter_backspace", &d, vec![]));

    // completion: type " se", then Enter, 100 times.
    let mut d = Vec::new();
    let mut opened = 0;
    for _ in 0..100 {
        h.event(Event::Text(" se".into()));
        timed_step(&mut h, &mut d);
        opened += usize::from(h.state().editor.completion_open());
        key(&h, Modifiers::NONE, Key::Enter);
        timed_step(&mut h, &mut d);
    }
    scenarios.push(stats(
        "completion",
        &d,
        vec![("popup opened of 100", opened)],
    ));

    // completion_member: type `o.` in a mid-file statement with `orders o`, then Esc.
    let mid_line = h.state().editor.model().buffer().line(at_line).to_string();
    assert!(mid_line.contains("orders o"), "{mid_line}");
    // Before the closing `;`, so the caret stays inside the statement.
    let col = mid_line
        .trim_end_matches('\n')
        .trim_end_matches(';')
        .chars()
        .count();
    jump_to(&mut h, Pos { line: at_line, col });
    h.run_steps(2);
    h.event(Event::Text(" AND ".into()));
    h.run_steps(1);
    let mut d = Vec::new();
    let mut opened = 0;
    let mut members = 0;
    for _ in 0..100 {
        h.event(Event::Text("o.".into()));
        timed_step(&mut h, &mut d);
        opened += usize::from(h.state().editor.completion_open());
        members += usize::from(
            h.state()
                .editor
                .completion_labels()
                .iter()
                .any(|l| l == "user_id"),
        );
        key(&h, Modifiers::NONE, Key::Escape);
        timed_step(&mut h, &mut d);
        for _ in 0..2 {
            key(&h, Modifiers::NONE, Key::Backspace);
            h.step();
        }
    }
    scenarios.push(stats(
        "completion_member",
        &d,
        vec![
            ("popup opened of 100", opened),
            ("offered user_id", members),
        ],
    ));
    absorb(&h);

    // jump_to_end_first_frames: a fresh state, jump to the last line, 30 frames. Five fresh
    // states in one harness, so every run after the first has warm fonts, as in a running
    // app. The first run, on a cold context, is reported on its own as `jump_to_end_cold`.
    // DocEnd is ⌘↓ on macOS and Ctrl+End elsewhere; the harness context picks its own OS.
    let mac = h.ctx.os() == egui::os::OperatingSystem::Mac;
    let (doc_start, doc_end) = if mac {
        (Key::ArrowUp, Key::ArrowDown)
    } else {
        (Key::Home, Key::End)
    };
    drop(h);
    let mut h = harness(big_editor());
    h.run_steps(2);
    focus(&mut h);
    let mut cold = Vec::new();
    let mut warm = Vec::new();
    let mut at_end = 0;
    for run in 0..5 {
        if run > 0 {
            // Back to the top, so the scroll offset does not carry over, then a fresh state.
            key(&h, Modifiers::COMMAND, doc_start);
            h.run_steps(3);
            // Drop the old document before the new one is built: one is resident at a time.
            h.state_mut().editor = EditorState::new("", SqlSyntax::ansi());
            h.state_mut().editor = big_editor();
            h.run_steps(2);
            focus(&mut h);
        }
        key(&h, Modifiers::COMMAND, doc_end);
        let out = if run == 0 { &mut cold } else { &mut warm };
        for _ in 0..30 {
            timed_step(&mut h, out);
        }
        let last = h.state().editor.model().buffer().len_lines() - 1;
        at_end += usize::from(
            h.state()
                .editor
                .screen_pos(Pos { line: last, col: 0 })
                .is_some(),
        );
    }
    absorb(&h);
    drop(h);
    scenarios.push(stats(
        "jump_to_end_first_frames",
        &warm,
        vec![("runs that reached the end of 5", at_end)],
    ));
    scenarios.push(stats("jump_to_end_cold", &cold, vec![]));

    let mut sp: Vec<f64> = scope_parses.iter().copied().map(ms).collect();
    sp.sort_by(f64::total_cmp);
    let hit_rate = hits as f64 / (hits + misses).max(1) as f64;
    PhaseOut {
        scenarios,
        scalars: vec![
            ("scope_parse_p99_ms", pct(&sp, 0.99), Some(BAR_SCOPE_MS)),
            ("scope_parses", sp.len() as f64, None),
            ("first_frame_ms", ms(first_frame), Some(BAR_FIRST_FRAME_MS)),
            ("galley_hit_rate", hit_rate, None),
        ],
        rss_mb: rss.finish(),
    }
}

/// A full lex of the 10 MB text on its own: informational.
fn lex_phase() -> PhaseOut {
    let rss = PeakRss::start();
    let buf = {
        let text = generate_sql(BIG_BYTES, 42);
        TextBuffer::from_str(&text)
    };
    let mut layer = SyntaxLayer::new(SqlSyntax::ansi(), &buf);
    let last = buf.len_lines() - 1;
    let t = Instant::now();
    layer.ensure(&buf, last, None);
    let full_lex = t.elapsed();
    PhaseOut {
        scenarios: vec![],
        scalars: vec![("full_lex_ms", ms(full_lex), None)],
        rss_mb: rss.finish(),
    }
}

fn long_phase() -> PhaseOut {
    let rss = PeakRss::start();
    let mut h = harness({
        let text = generate_long_line_sql(1024 * 1024, 42);
        EditorState::new(&text, SqlSyntax::ansi())
    });
    h.run_steps(2);
    focus(&mut h);
    let len = h.state().editor.model().buffer().line_len_chars(0);
    jump_to(
        &mut h,
        Pos {
            line: 0,
            col: len / 2 - 250,
        },
    );
    h.run_steps(2);
    let mut d = Vec::new();
    for _ in 0..500 {
        key(&h, Modifiers::NONE, Key::ArrowRight);
        timed_step(&mut h, &mut d);
    }
    for _ in 0..200 {
        h.event(Event::Text("y".into()));
        timed_step(&mut h, &mut d);
    }
    let scenario = stats("long_line", &d, vec![]);

    // Completion on the huge statement: every keystroke asks the scope worker, which must
    // get a window around the caret, not the 1 MB statement.
    let line = h.state().editor.model().buffer().line(0).to_string();
    let col = line[line.len() / 2..]
        .find("),")
        .map(|i| line[..line.len() / 2 + i + 2].chars().count())
        .expect("a tuple boundary");
    jump_to(&mut h, Pos { line: 0, col });
    h.run_steps(2);
    let mut d = Vec::new();
    let (r0, b0) = h.state().editor.scope_requests();
    let popup0 = h.state().popup_frames;
    for _ in 0..100 {
        h.event(Event::Text(" se".into()));
        timed_step(&mut h, &mut d);
        key(&h, Modifiers::NONE, Key::Escape);
        timed_step(&mut h, &mut d);
        for _ in 0..3 {
            key(&h, Modifiers::NONE, Key::Backspace);
            h.step();
        }
    }
    let (r1, b1) = h.state().editor.scope_requests();
    let requests = r1 - r0;
    let completion = stats(
        "long_line_completion",
        &d,
        vec![
            (
                "frames with the popup open",
                (h.state().popup_frames - popup0) as usize,
            ),
            ("worker requests", requests as usize),
            (
                "mean bytes per request",
                ((b1 - b0) / requests.max(1)) as usize,
            ),
        ],
    );
    PhaseOut {
        scenarios: vec![scenario, completion],
        scalars: vec![],
        rss_mb: rss.finish(),
    }
}

const MULTI_BYTES: usize = 8 * 1024 * 1024;

/// One 8 MB statement over many short lines: completion must not copy or split it on the UI
/// thread on any keystroke.
fn multi_phase() -> PhaseOut {
    let rss = PeakRss::start();
    let mut h = harness({
        let text = generate_multi_line_sql(MULTI_BYTES, 42);
        EditorState::new(&text, SqlSyntax::ansi())
    });
    h.run_steps(2);
    focus(&mut h);
    let n = h.state().editor.model().buffer().len_lines();
    // The end of a tuple line in the middle of the statement (after its `,`).
    let line = n / 2;
    let col = h.state().editor.model().buffer().line_len_chars(line);
    jump_to(&mut h, Pos { line, col });
    h.run_steps(2);

    // Typing in the middle of the statement.
    let mut d = Vec::new();
    for _ in 0..200 {
        h.event(Event::Text("y".into()));
        timed_step(&mut h, &mut d);
    }
    for _ in 0..200 {
        key(&h, Modifiers::NONE, Key::Backspace);
        timed_step(&mut h, &mut d);
    }
    let typing = stats("multi_line_statement_type", &d, vec![]);

    // Completion: type " se" (the popup opens), Esc, then erase it; 100 times.
    let mut d = Vec::new();
    let (r0, b0) = h.state().editor.scope_requests();
    let split0 = h.state().editor.split_bytes();
    let popup0 = h.state().popup_frames;
    for _ in 0..100 {
        h.event(Event::Text(" se".into()));
        timed_step(&mut h, &mut d);
        key(&h, Modifiers::NONE, Key::Escape);
        timed_step(&mut h, &mut d);
        for _ in 0..3 {
            key(&h, Modifiers::NONE, Key::Backspace);
            h.step();
        }
    }
    let (r1, b1) = h.state().editor.scope_requests();
    let requests = r1 - r0;
    let completion = stats(
        "multi_line_completion",
        &d,
        vec![
            (
                "frames with the popup open",
                (h.state().popup_frames - popup0) as usize,
            ),
            ("worker requests", requests as usize),
            (
                "mean bytes per request",
                ((b1 - b0) / requests.max(1)) as usize,
            ),
            (
                "bytes split on the UI thread",
                (h.state().editor.split_bytes() - split0) as usize,
            ),
        ],
    );
    PhaseOut {
        scenarios: vec![typing, completion],
        scalars: vec![],
        rss_mb: rss.finish(),
    }
}

/// ⌘P ranking over 50 000 relations, one query per keystroke of "user_ev": informational.
fn palette_phase() -> PhaseOut {
    let rss = PeakRss::start();
    let items: Vec<PaletteItem> = (0..50_000)
        .map(|i| PaletteItem {
            label: format!("{}_{i:05}", TABLES[i % TABLES.len()]),
            detail: format!("schema_{:02}", i % 40),
            group: PaletteGroup::Object,
            fuzzy: true,
            icon: None,
            shortcut: None,
        })
        .collect();
    let query = "user_ev";
    let mut d = Vec::new();
    for _ in 0..20 {
        for end in 1..=query.len() {
            let t = Instant::now();
            std::hint::black_box(fuzzy_rank(&query[..end], &items, PALETTE_ROWS));
            d.push(t.elapsed());
        }
    }
    let mut v: Vec<f64> = d.iter().copied().map(ms).collect();
    v.sort_by(f64::total_cmp);
    PhaseOut {
        scenarios: vec![],
        scalars: vec![
            ("palette_rank_50k_p50_ms", pct(&v, 0.5), None),
            ("palette_rank_50k_p99_ms", pct(&v, 0.99), None),
        ],
        rss_mb: rss.finish(),
    }
}

fn phase_json(p: &PhaseOut) -> String {
    let sc: Vec<String> = p
        .scenarios
        .iter()
        .map(|s| {
            let checks: Vec<String> = s
                .checks
                .iter()
                .map(|(k, v)| format!("\"{k}\": {v}"))
                .collect();
            format!(
                "\"{}\": {{ \"p50_ms\": {:.3}, \"p95_ms\": {:.3}, \"p99_ms\": {:.3}, \"max_ms\": {:.3}, \"steps\": {}, \"bar_ms\": {BAR_FRAME_MS}, \"pass\": {}, \"checks\": {{ {} }} }}",
                s.name,
                s.p50,
                s.p95,
                s.p99,
                s.max,
                s.steps,
                s.p99 < BAR_FRAME_MS,
                checks.join(", ")
            )
        })
        .collect();
    let sv: Vec<String> = p
        .scalars
        .iter()
        .map(|(k, v, bar)| match bar {
            Some(b) => format!(
                "\"{k}\": {{ \"value\": {v:.3}, \"bar\": {b}, \"pass\": {} }}",
                *v < *b
            ),
            None => format!("\"{k}\": {{ \"value\": {v:.3} }}"),
        })
        .collect();
    format!(
        "{{ \"scenarios\": {{ {} }}, \"scalars\": {{ {} }}, \"peak_rss_mb\": {{ \"value\": {:.1}, \"bar\": {BAR_RSS_MB}, \"pass\": {} }} }}",
        sc.join(", "),
        sv.join(", "),
        p.rss_mb,
        p.rss_mb < BAR_RSS_MB
    )
}

fn print_phase(name: &str, p: &PhaseOut) {
    println!("phase {name}");
    println!(
        "{:<26} {:>8} {:>8} {:>8} {:>8} {:>6}  {:<5}",
        "scenario", "p50 ms", "p95 ms", "p99 ms", "max ms", "steps", "bar"
    );
    for s in &p.scenarios {
        let verdict = if s.p99 < BAR_FRAME_MS { "ok" } else { "MISS" };
        let checks: Vec<String> = s.checks.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        let checks = if checks.is_empty() {
            String::new()
        } else {
            format!("  ({})", checks.join("; "))
        };
        println!(
            "{:<26} {:>8.3} {:>8.3} {:>8.3} {:>8.3} {:>6}  {:<5}{checks}",
            s.name, s.p50, s.p95, s.p99, s.max, s.steps, verdict
        );
    }
    for (k, v, bar) in &p.scalars {
        match bar {
            Some(b) => println!(
                "{k:<26} {v:>8.3}  bar < {b}  {}",
                if v < b { "ok" } else { "MISS" }
            ),
            None => println!("{k:<26} {v:>8.3}  info"),
        }
    }
    println!(
        "{:<26} {:>8.1}  bar < {BAR_RSS_MB}  {}",
        "peak_rss_mb",
        p.rss_mb,
        if p.rss_mb < BAR_RSS_MB { "ok" } else { "MISS" }
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--wrap") {
        WRAP.store(true, Ordering::Relaxed);
        let p = big_phase();
        print_phase("big (soft wrap)", &p);
        println!("JSON {}", phase_json(&p));
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--phase") {
        let name = args.get(i + 1).map(String::as_str).unwrap_or_default();
        let p = match name {
            "big" => big_phase(),
            "long" => long_phase(),
            "multi" => multi_phase(),
            "lex" => lex_phase(),
            "palette" => palette_phase(),
            other => panic!("unknown phase {other}"),
        };
        print_phase(name, &p);
        println!("JSON {}", phase_json(&p));
        return;
    }
    // One process per phase: the resident set of each counts a single document.
    let exe = std::env::current_exe().expect("own path");
    let mut json = Vec::new();
    for name in ["big", "long", "multi", "lex", "palette"] {
        let out = std::process::Command::new(&exe)
            .args(["--phase", name])
            .output()
            .expect("run a phase");
        assert!(
            out.status.success(),
            "phase {name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            match line.strip_prefix("JSON ") {
                Some(j) => json.push(format!("\"{name}\": {j}")),
                None => println!("{line}"),
            }
        }
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/editor-measure.json");
    std::fs::write(
        &path,
        format!("{{ \"phases\": {{ {} }} }}\n", json.join(",\n  ")),
    )
    .expect("write target/editor-measure.json");
    println!("wrote {}", path.display());
}
