//! The completion scope of one statement: the relations it reads or writes (with aliases)
//! and its CTE names. Only the statement around the caret is parsed, freshly, with
//! tree-sitter; a statement over [`STRUCTURE_SYNC_LIMIT`] bytes goes to a [`ScopeWorker`].

use std::cell::RefCell;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use ropey::Rope;
use sqlr_core::{LexState, SqlSyntax};
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

/// Statements longer than this (bytes) are parsed off the UI thread.
pub const STRUCTURE_SYNC_LIMIT: usize = 64 * 1024;
/// Inserted at the caret before parsing, so an incomplete name still parses as one.
pub const CARET_PLACEHOLDER: &str = "sqlr_caret";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RelationRef {
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
    /// The name was written as a quoted identifier.
    pub quoted: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatementScope {
    pub relations: Vec<RelationRef>,
    pub ctes: Vec<String>,
}

const SEQUEL: &str = "sequel";

fn language() -> Language {
    tree_sitter_sequel::LANGUAGE.into()
}

/// The scope query and its capture indices, compiled once per process.
struct ScopeQuery {
    query: Query,
    schema: u32,
    name: u32,
    alias: u32,
    cte: u32,
}

static QUERY: LazyLock<ScopeQuery> = LazyLock::new(|| {
    let query = Query::new(&language(), include_str!("queries/scope.scm"))
        .expect("scope.scm compiles against tree-sitter-sequel");
    let idx = |name| {
        query
            .capture_index_for_name(name)
            .expect("scope.scm defines the capture")
    };
    ScopeQuery {
        schema: idx("schema"),
        name: idx("name"),
        alias: idx("alias"),
        cte: idx("cte"),
        query,
    }
});

/// Compiles the scope query on a background thread (once per process), so the first
/// completion doesn't pay for it on the UI thread.
pub(crate) fn warm_up() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("sqlr-scope-warmup".into())
            .spawn(|| LazyLock::force(&QUERY));
    });
}

fn new_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&language())
        .expect("grammar ABI matches tree-sitter");
    parser
}

thread_local! {
    static PARSER: RefCell<Option<Parser>> = const { RefCell::new(None) };
}

/// The scope of the statement `text` with the caret at byte `caret`; empty for a grammar
/// other than tree-sitter-sequel.
pub fn statement_scope(syntax: &SqlSyntax, text: &str, caret: usize) -> StatementScope {
    if syntax.grammar != SEQUEL {
        return StatementScope::default();
    }
    PARSER.with_borrow_mut(|p| scope_with(p.get_or_insert_with(new_parser), syntax, text, caret))
}

fn scope_with(parser: &mut Parser, syntax: &SqlSyntax, text: &str, caret: usize) -> StatementScope {
    let mut caret = caret.min(text.len());
    while !text.is_char_boundary(caret) {
        caret -= 1;
    }
    let mut src = String::with_capacity(text.len() + CARET_PLACEHOLDER.len());
    src.push_str(&text[..caret]);
    src.push_str(CARET_PLACEHOLDER);
    src.push_str(&text[caret..]);
    let Some(tree) = parser.parse(&src, None) else {
        return StatementScope::default();
    };
    let q = &*QUERY;
    let mut out = StatementScope::default();
    // Byte offset of each relation's name node, so a name two patterns match counts once.
    let mut seen: Vec<usize> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q.query, tree.root_node(), src.as_bytes());
    while let Some(m) = matches.next() {
        let mut rel = RelationRef::default();
        let mut name_at = None;
        for cap in m.captures() {
            let raw = &src[cap.node.byte_range()];
            let (value, quoted) = syntax.unquote(raw);
            if cap.index == q.cte {
                if !out.ctes.contains(&value) {
                    out.ctes.push(value);
                }
            } else if cap.index == q.name {
                rel.name = value;
                rel.quoted = quoted;
                name_at = Some(cap.node.start_byte());
            } else if cap.index == q.schema {
                rel.schema = Some(value);
            } else if cap.index == q.alias {
                rel.alias = Some(value);
            }
        }
        let Some(at) = name_at else { continue };
        if rel.name.contains(CARET_PLACEHOLDER) {
            continue;
        }
        if rel
            .alias
            .as_deref()
            .is_some_and(|a| a.contains(CARET_PLACEHOLDER))
        {
            rel.alias = None;
        }
        match seen.iter().position(|&s| s == at) {
            Some(i) => {
                if out.relations[i].alias.is_none() {
                    out.relations[i].alias = rel.alias;
                }
            }
            None => {
                seen.push(at);
                out.relations.push(rel);
            }
        }
    }
    out
}

/// The text a job parses.
enum Source {
    /// A statement.
    Text(String),
    /// Bytes `range` of a rope (a cheap clone), sliced on the worker.
    Rope { rope: Rope, range: Range<usize> },
}

struct Job {
    key: u64,
    syntax: Arc<SqlSyntax>,
    source: Source,
    /// The caret, as a byte offset into the job's text.
    caret: usize,
    /// The text is a region to split first, lexed from this state.
    split_from: Option<LexState>,
}

struct Channels {
    jobs: Sender<Job>,
    results: Mutex<Receiver<(u64, StatementScope)>>,
}

/// Parses large statements on its own thread, started on the first request. The thread
/// owns its own parser and ends when the worker is dropped.
pub struct ScopeWorker {
    repaint: Option<egui::Context>,
    channels: OnceLock<Channels>,
    /// The key of the newest request; results for older keys are dropped.
    latest: Mutex<Option<u64>>,
    requests: AtomicU64,
    requested_bytes: AtomicU64,
}

impl ScopeWorker {
    pub fn new(repaint: Option<egui::Context>) -> Self {
        Self {
            repaint,
            channels: OnceLock::new(),
            latest: Mutex::new(None),
            requests: AtomicU64::new(0),
            requested_bytes: AtomicU64::new(0),
        }
    }

    /// How many parses were requested, and the bytes of text they covered in total.
    pub fn request_stats(&self) -> (u64, u64) {
        (
            self.requests.load(Ordering::Relaxed),
            self.requested_bytes.load(Ordering::Relaxed),
        )
    }

    fn channels(&self) -> &Channels {
        self.channels.get_or_init(|| {
            let (jobs, job_rx) = channel::<Job>();
            let (result_tx, results) = channel();
            let repaint = self.repaint.clone();
            std::thread::Builder::new()
                .name("sqlr-scope".into())
                .spawn(move || run_worker(job_rx, result_tx, repaint))
                .expect("spawn the scope worker thread");
            Channels {
                jobs,
                results: Mutex::new(results),
            }
        })
    }

    /// Queues a parse; a newer request supersedes any older one not yet polled.
    pub fn request(&self, key: u64, syntax: Arc<SqlSyntax>, text: String, caret: usize) {
        self.send(Job {
            key,
            syntax,
            source: Source::Text(text),
            caret,
            split_from: None,
        });
    }

    fn send(&self, job: Job) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let bytes = match &job.source {
            Source::Text(t) => t.len(),
            Source::Rope { range, .. } => range.len(),
        };
        self.requested_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        *self.latest.lock().expect("scope worker lock") = Some(job.key);
        // A send fails only if the thread is gone; the request is then dropped.
        let _ = self.channels().jobs.send(job);
    }

    /// Like [`Self::request`] for bytes `range` of `rope`, with the caret at byte `caret` of
    /// the rope. The worker slices the text, so the caller pays only the O(1) rope clone.
    /// With `split_from`, the range is a region (such as one very long line) that may hold
    /// several statements: the worker splits it, lexed from that state, and parses the
    /// statement the caret belongs to.
    pub fn request_rope(
        &self,
        key: u64,
        syntax: Arc<SqlSyntax>,
        rope: Rope,
        range: Range<usize>,
        caret: usize,
        split_from: Option<LexState>,
    ) {
        let caret = caret.clamp(range.start, range.end) - range.start;
        self.send(Job {
            key,
            syntax,
            source: Source::Rope { rope, range },
            caret,
            split_from,
        });
    }

    /// The scope of the newest request, once parsed.
    pub fn poll(&self) -> Option<(u64, StatementScope)> {
        let channels = self.channels.get()?;
        let latest = *self.latest.lock().expect("scope worker lock");
        let results = channels.results.lock().expect("scope worker lock");
        let mut found = None;
        while let Ok((key, scope)) = results.try_recv() {
            if Some(key) == latest {
                found = Some((key, scope));
            }
        }
        found
    }
}

/// The statement (of ascending `ranges`) the caret belongs to: the one containing it, else the
/// nearest one ending before it, else the first one.
fn statement_of(ranges: &[Range<usize>], caret: usize) -> Option<&Range<usize>> {
    ranges
        .iter()
        .find(|r| r.start <= caret && caret <= r.end)
        .or_else(|| ranges.iter().rev().find(|r| r.end <= caret))
        .or_else(|| ranges.first())
}

fn run_worker(
    jobs: Receiver<Job>,
    results: Sender<(u64, StatementScope)>,
    repaint: Option<egui::Context>,
) {
    let mut parser = new_parser();
    while let Ok(mut job) = jobs.recv() {
        // Only the newest queued request matters.
        while let Ok(newer) = jobs.try_recv() {
            job = newer;
        }
        let scope = if job.syntax.grammar == SEQUEL {
            let text = match job.source {
                Source::Text(text) => text,
                Source::Rope { rope, range } => rope.byte_slice(range).to_string(),
            };
            match job.split_from {
                None => scope_with(&mut parser, &job.syntax, &text, job.caret),
                Some(start) => {
                    let ranges = job.syntax.split(&text, start);
                    match statement_of(&ranges, job.caret) {
                        Some(r) => {
                            let caret = job.caret.clamp(r.start, r.end) - r.start;
                            scope_with(&mut parser, &job.syntax, &text[r.clone()], caret)
                        }
                        None => StatementScope::default(),
                    }
                }
            }
        } else {
            StatementScope::default()
        };
        if results.send((job.key, scope)).is_err() {
            return;
        }
        if let Some(ctx) = &repaint {
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn scope_at(marked: &str) -> StatementScope {
        let caret = marked.find('|').expect("caret marker");
        let text = marked.replacen('|', "", 1);
        statement_scope(&SqlSyntax::ansi(), &text, caret)
    }

    fn rel(schema: Option<&str>, name: &str, alias: Option<&str>) -> RelationRef {
        RelationRef {
            schema: schema.map(Into::into),
            name: name.into(),
            alias: alias.map(Into::into),
            quoted: false,
        }
    }

    #[test]
    fn relations_with_aliases() {
        let s = scope_at("SELECT o.| FROM users u JOIN public.orders AS o ON o.user_id = u.id");
        assert_eq!(
            s.relations,
            [
                rel(None, "users", Some("u")),
                rel(Some("public"), "orders", Some("o"))
            ]
        );
    }

    #[test]
    fn cte_names() {
        let s = scope_at("WITH t AS (SELECT 1 AS x) SELECT t.| FROM t");
        assert_eq!(s.ctes, ["t"]);
        assert_eq!(s.relations, [rel(None, "t", None)]);
    }

    #[test]
    fn quoted_relation() {
        let s = scope_at(r#"SELECT a FROM "My T" m WHERE m.|"#);
        assert_eq!(
            s.relations,
            [RelationRef {
                name: "My T".into(),
                alias: Some("m".into()),
                quoted: true,
                schema: None,
            }]
        );
    }

    #[test]
    fn delete_and_insert_targets() {
        let s = scope_at("DELETE FROM users d WHERE d.|");
        assert_eq!(
            s.relations
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["users"]
        );
        let s = scope_at("INSERT INTO public.users (a, |) VALUES (1)");
        assert_eq!(s.relations, [rel(Some("public"), "users", None)]);
    }

    #[test]
    fn placeholder_never_listed() {
        assert!(scope_at("SELECT * FROM |").relations.is_empty());
        assert!(scope_at("SELECT * FROM public.|").relations.is_empty());
        assert_eq!(
            scope_at("SELECT * FROM users |").relations,
            [rel(None, "users", None)]
        );
    }

    #[test]
    fn unknown_grammar_is_empty() {
        let other = SqlSyntax {
            grammar: "none",
            ..(*SqlSyntax::ansi()).clone()
        };
        assert_eq!(
            statement_scope(&other, "SELECT * FROM users", 19),
            StatementScope::default()
        );
    }

    /// Polls for up to `timeout`, and for 200 ms more after the first result.
    fn poll_for(w: &ScopeWorker, timeout: Duration) -> Vec<(u64, StatementScope)> {
        let mut deadline = Instant::now() + timeout;
        let mut got = Vec::new();
        while Instant::now() < deadline {
            if let Some(r) = w.poll() {
                if got.is_empty() {
                    deadline = Instant::now() + Duration::from_millis(200);
                }
                got.push(r);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        got
    }

    #[test]
    fn worker_parses_large_statement_off_thread() {
        let mut text = String::from("SELECT o. FROM users u JOIN public.orders o ON ");
        let caret = "SELECT o.".len();
        while text.len() < 200 * 1024 {
            text.push_str("o.id = u.id AND ");
        }
        text.push_str("TRUE");
        let w = ScopeWorker::new(None);
        w.request(7, SqlSyntax::ansi(), text.clone(), caret);
        let got = poll_for(&w, Duration::from_secs(2));
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, 7);
        assert_eq!(
            got[0].1.relations,
            [
                rel(None, "users", Some("u")),
                rel(Some("public"), "orders", Some("o"))
            ]
        );

        // A region of a rope, split on the worker: only its byte range is sent.
        let rope =
            ropey::Rope::from_str("select 0;\nselect 1; SELECT o. FROM orders o; select 3\n");
        let line = 10..rope.len_bytes() - 1;
        w.request_rope(
            5,
            SqlSyntax::ansi(),
            rope,
            line,
            10 + 19,
            Some(LexState::Normal),
        );
        let got = poll_for(&w, Duration::from_secs(2));
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].1.relations, [rel(None, "orders", Some("o"))]);

        w.request(8, SqlSyntax::ansi(), text.clone(), caret);
        let small = "SELECT * FROM t WHERE ";
        w.request(9, SqlSyntax::ansi(), small.into(), small.len());
        let got = poll_for(&w, Duration::from_secs(2));
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, 9);
        assert_eq!(got[0].1.relations, [rel(None, "t", None)]);
    }
}
