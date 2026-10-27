//! Query languages and their lexers: SQL, the Mongo shell (and JSON), the Elasticsearch
//! console and Redis commands.
//!
//! The non-SQL lexers produce the same resumable [`Token`]s and [`LexState`]s as
//! [`SqlSyntax::lex`], so the editor highlights every language line by line:
//! - Mongo shell / JSON: `db`, `new`, `true`… are keywords, a word before `(` is a function,
//!   a key is an identifier (a quoted key a [`TokenKind::QuotedIdent`]), `/re/i` is a string.
//! - JSON: one value, with strings, numbers and the `true`/`false`/`null` literals colored;
//!   an open string resumes on the next line. It has no statements.
//! - Elasticsearch console: the method (or `ESQL`/`SQL`) is a keyword, the path an
//!   identifier, bodies lex as JSON, `"""…"""` strings span lines, `#` and `//` comment.
//! - Redis: the first word of a line is the command keyword, the rest are arguments.

use std::ops::Range;

use crate::syntax::{LexEnd, LexState, SqlSyntax, Token, TokenKind, dollar_tag_hash};

/// The language of a query editor, which decides lexing, statement splitting and highlighting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum QueryLanguage {
    /// SQL, in the dialect of the connection.
    #[default]
    Sql,
    /// The MongoDB shell subset.
    MongoShell,
    /// The Elasticsearch console: a request line and a JSON body.
    EsConsole,
    /// Redis commands, one per line.
    Redis,
    /// A JSON value, edited on its own (a cell, a document). No statements.
    Json,
}

/// The lexer state inside an Elasticsearch `"""…"""` string (a dollar quote never hashes
/// a tag of quotes, so this cannot collide with SQL).
fn triple_quote_state() -> LexState {
    LexState::DollarQuote {
        tag: dollar_tag_hash("\"\"\""),
    }
}

impl QueryLanguage {
    /// The language of a driver's dialect id (`DriverManifest::dialect`).
    pub fn for_dialect(dialect: &str) -> Self {
        match dialect {
            "mongodb" => QueryLanguage::MongoShell,
            "elasticsearch" => QueryLanguage::EsConsole,
            "redis" => QueryLanguage::Redis,
            "json" => QueryLanguage::Json,
            _ => QueryLanguage::Sql,
        }
    }

    /// The prefix that comments out a line, for the editor's toggle-comment. Redis commands
    /// have no comments: the prefix is empty and toggle-comment does nothing.
    pub fn line_comment(self) -> &'static str {
        match self {
            QueryLanguage::Sql => "--",
            QueryLanguage::MongoShell => "//",
            QueryLanguage::EsConsole => "#",
            QueryLanguage::Redis | QueryLanguage::Json => "",
        }
    }

    /// Lexes `text` starting in `start`, appending tokens (no whitespace) to `out`. SQL uses
    /// the ANSI syntax; editors with a dialect call [`SqlSyntax::lex`] instead.
    pub fn lex(self, text: &str, start: LexState, out: &mut Vec<Token>) -> LexEnd {
        match self {
            QueryLanguage::Sql => SqlSyntax::ansi().lex(text, start, out),
            QueryLanguage::MongoShell => {
                let mut semicolon = false;
                let state = lex_js(text, start, Js::Mongo, 0, out, &mut semicolon);
                LexEnd { state, semicolon }
            }
            QueryLanguage::EsConsole => LexEnd {
                state: lex_es(text, start, out),
                semicolon: false,
            },
            QueryLanguage::Redis => {
                lex_redis(text, out);
                LexEnd {
                    state: LexState::Normal,
                    semicolon: false,
                }
            }
            QueryLanguage::Json => {
                let state = lex_json(text, start, out);
                LexEnd {
                    state,
                    semicolon: false,
                }
            }
        }
    }

    /// Byte ranges of the statements (requests, commands) in `text`, trimmed, in order.
    /// Works on any text, valid or not.
    pub fn split(self, text: &str) -> Vec<Range<usize>> {
        self.split_from(text, LexState::Normal)
    }

    /// Like [`Self::split`], for text that starts in lexer state `start` (a region of a
    /// larger text that begins inside a comment or string). The console and Redis start
    /// every line afresh, so only SQL and the Mongo shell use `start`.
    pub fn split_from(self, text: &str, start: LexState) -> Vec<Range<usize>> {
        match self {
            QueryLanguage::Sql => SqlSyntax::ansi().split(text, start),
            QueryLanguage::MongoShell => split_mongo_shell_from(text, start),
            QueryLanguage::EsConsole => crate::es_request::split_es_console(text),
            QueryLanguage::Redis => {
                let mut out = Vec::new();
                let mut at = 0;
                for line in text.split_inclusive('\n') {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        let s = at + (line.len() - line.trim_start().len());
                        out.push(s..s + trimmed.len());
                    }
                    at += line.len();
                }
                out
            }
            // One document, trimmed of the whitespace around it.
            QueryLanguage::Json => {
                let start = text.len() - text.trim_start().len();
                let end = text.trim_end().len();
                if start < end {
                    std::iter::once(start..end).collect()
                } else {
                    Vec::new()
                }
            }
        }
    }
}

/// Statements of a Mongo shell script: split at `;` and at line breaks outside brackets,
/// except before a chained `.method()` or after a token that cannot end a statement.
pub fn split_mongo_shell(text: &str) -> Vec<Range<usize>> {
    split_mongo_shell_from(text, LexState::Normal)
}

/// [`split_mongo_shell`] for text that starts in lexer state `start`.
pub fn split_mongo_shell_from(text: &str, start: LexState) -> Vec<Range<usize>> {
    let mut toks = Vec::new();
    QueryLanguage::MongoShell.lex(text, start, &mut toks);
    toks.retain(|t| t.kind != TokenKind::Comment);
    let mut out = Vec::new();
    let mut cur: Option<Range<usize>> = None;
    let mut depth = 0u32;
    let mut prev: Option<Token> = None;
    for t in toks {
        let s = &text[t.start..t.end];
        if t.kind == TokenKind::Semicolon && depth == 0 {
            out.extend(cur.take());
            prev = None;
            continue;
        }
        if let (Some(c), Some(p)) = (&cur, prev)
            && depth == 0
            && text[p.end..t.start].contains('\n')
            && !continues(&text[p.start..p.end], p.kind, s)
        {
            out.push(c.clone());
            cur = None;
        }
        match &mut cur {
            Some(c) => c.end = t.end,
            None => cur = Some(t.start..t.end),
        }
        if t.kind == TokenKind::Punct {
            match s {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        prev = Some(t);
    }
    out.extend(cur);
    out
}

/// Whether a line break between `prev` and `next` continues the statement.
fn continues(prev: &str, prev_kind: TokenKind, next: &str) -> bool {
    next.starts_with('.') || matches!(prev, "." | ",") || prev_kind == TokenKind::Operator
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Js {
    /// The Mongo shell: regex literals, `db`/`new`/… keywords.
    Mongo,
    /// JSON bodies of the Elasticsearch console: `#` comments, `"""` strings.
    Json,
}

const MONGO_KEYWORDS: &[&str] = &[
    "db",
    "false",
    "new",
    "null",
    "show",
    "true",
    "undefined",
    "use",
];

fn is_js_word_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn is_js_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn char_len(text: &str, i: usize) -> usize {
    text[i..].chars().next().map_or(1, char::len_utf8)
}

/// The end of a word starting at `i` (ASCII word bytes and Unicode alphanumerics).
fn js_word_end(text: &str, mut i: usize) -> usize {
    let b = text.as_bytes();
    while i < b.len() {
        if b[i] < 0x80 {
            if !is_js_word_byte(b[i]) {
                break;
            }
            i += 1;
        } else if text[i..].chars().next().is_some_and(char::is_alphanumeric) {
            i += char_len(text, i);
        } else {
            break;
        }
    }
    i
}

/// The next non-whitespace byte at or after `i` on the same text.
fn next_byte(b: &[u8], mut i: usize) -> Option<u8> {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    b.get(i).copied()
}

/// Scans a `quote`-delimited string body with backslash escapes from `i`.
fn scan_js_string(text: &str, mut i: usize, quote: u8) -> (usize, bool) {
    let b = text.as_bytes();
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 1;
                if i < b.len() {
                    i += char_len(text, i);
                }
            }
            c if c == quote => return (i + 1, true),
            _ => i += 1,
        }
    }
    (b.len(), false)
}

/// Whether a quoted string token (`'…'` or `"…"`) ends with its own closing quote.
pub(crate) fn js_string_closed(tok: &str) -> bool {
    match tok.as_bytes().first() {
        Some(&q @ (b'"' | b'\'')) => scan_js_string(tok, 1, q) == (tok.len(), true),
        _ => false,
    }
}

/// Scans a regex body from `i` (after the opening `/`): `/` inside `[…]` does not close
/// it, and a regex never spans lines. The end after the closing `/`, if any.
fn scan_regex(b: &[u8], mut i: usize) -> Option<usize> {
    let mut class = false;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'\n' => return None,
            b'[' => {
                class = true;
                i += 1;
            }
            b']' => {
                class = false;
                i += 1;
            }
            b'/' if !class => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// The end of a number starting at `i` (optional sign, hex, fraction, exponent).
fn scan_number(b: &[u8], mut i: usize) -> usize {
    if matches!(b[i], b'-' | b'+') {
        i += 1;
    }
    if b[i] == b'0' && matches!(b.get(i + 1), Some(b'x' | b'X')) {
        i += 2;
        while i < b.len() && b[i].is_ascii_hexdigit() {
            i += 1;
        }
        return i;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if b.get(j).is_some_and(u8::is_ascii_digit) {
            i = j;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    i
}

fn starts_number(b: &[u8], i: usize) -> bool {
    let digit = |j: usize| b.get(j).is_some_and(u8::is_ascii_digit);
    match b[i] {
        b'0'..=b'9' => true,
        b'-' | b'+' => digit(i + 1) || (b.get(i + 1) == Some(&b'.') && digit(i + 2)),
        b'.' => digit(i + 1),
        _ => false,
    }
}

/// Whether a `/` after `prev` starts a regex (a value position) rather than dividing.
fn regex_allowed(prev: Option<(TokenKind, u8)>) -> bool {
    match prev {
        None => true,
        Some((TokenKind::Punct, c)) => matches!(c, b'(' | b',' | b':' | b'[' | b'{'),
        Some((TokenKind::Semicolon | TokenKind::Operator, _)) => true,
        Some((TokenKind::Keyword, _)) => true,
        _ => false,
    }
}

/// Lexes JavaScript-ish text (Mongo shell or JSON) from `start`; token offsets are shifted
/// by `base`. Returns the end state.
fn lex_js(
    text: &str,
    start: LexState,
    mode: Js,
    base: usize,
    out: &mut Vec<Token>,
    semicolon: &mut bool,
) -> LexState {
    let b = text.as_bytes();
    let len = b.len();
    let mut push = |s: usize, e: usize, kind: TokenKind| {
        if s < e {
            out.push(Token {
                start: base + s,
                end: base + e,
                kind,
            });
        }
    };
    let mut i = 0;
    // Resume an open construct.
    match start {
        LexState::BlockComment { depth } => match text.find("*/") {
            Some(e) => {
                push(0, e + 2, TokenKind::Comment);
                i = e + 2;
            }
            None => {
                push(0, len, TokenKind::Comment);
                return LexState::BlockComment { depth };
            }
        },
        LexState::Str { .. } | LexState::DoubleStr { .. } => {
            let quote = if matches!(start, LexState::Str { .. }) {
                b'\''
            } else {
                b'"'
            };
            let (e, closed) = scan_js_string(text, 0, quote);
            push(0, e, TokenKind::String);
            if !closed {
                return start;
            }
            i = e;
        }
        s if s == triple_quote_state() => match text.find("\"\"\"") {
            Some(e) => {
                push(0, e + 3, TokenKind::String);
                i = e + 3;
            }
            None => {
                push(0, len, TokenKind::String);
                return s;
            }
        },
        _ => {}
    }
    let mut prev: Option<(TokenKind, u8)> = None;
    while i < len {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start_i = i;
        let kind = if c == b'/' && b.get(i + 1) == Some(&b'/') || (c == b'#' && mode == Js::Json) {
            i = text[i..].find('\n').map_or(len, |n| i + n);
            TokenKind::Comment
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            match text[i + 2..].find("*/") {
                Some(n) => i = i + 2 + n + 2,
                None => {
                    push(start_i, len, TokenKind::Comment);
                    return LexState::BlockComment { depth: 1 };
                }
            }
            TokenKind::Comment
        } else if c == b'"' && mode == Js::Json && text[i..].starts_with("\"\"\"") {
            match text[i + 3..].find("\"\"\"") {
                Some(n) => i = i + 3 + n + 3,
                None => {
                    push(start_i, len, TokenKind::String);
                    return triple_quote_state();
                }
            }
            TokenKind::String
        } else if c == b'"' || c == b'\'' {
            let (e, closed) = scan_js_string(text, i + 1, c);
            if !closed {
                push(start_i, len, TokenKind::String);
                return if c == b'"' {
                    LexState::DoubleStr { escapes: true }
                } else {
                    LexState::Str { escapes: true }
                };
            }
            i = e;
            if next_byte(b, i) == Some(b':') {
                TokenKind::QuotedIdent
            } else {
                TokenKind::String
            }
        } else if c == b'/' && mode == Js::Mongo && regex_allowed(prev) {
            match scan_regex(b, i + 1) {
                Some(e) => {
                    i = e;
                    while i < len && b[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    TokenKind::String
                }
                None => {
                    i += 1;
                    TokenKind::Operator
                }
            }
        } else if starts_number(b, i) {
            i = scan_number(b, i);
            TokenKind::Number
        } else if is_js_word_start(c)
            || (c >= 0x80 && text[i..].chars().next().is_some_and(char::is_alphanumeric))
        {
            i = js_word_end(text, i);
            let word = &text[start_i..i];
            if next_byte(b, i) == Some(b'(') {
                TokenKind::Function
            } else if match mode {
                Js::Mongo => MONGO_KEYWORDS.contains(&word),
                Js::Json => matches!(word, "true" | "false" | "null"),
            } {
                TokenKind::Keyword
            } else {
                TokenKind::Ident
            }
        } else if c == b';' {
            i += 1;
            *semicolon = true;
            TokenKind::Semicolon
        } else if matches!(
            c,
            b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b':' | b'.'
        ) {
            i += 1;
            TokenKind::Punct
        } else if c < 0x80 {
            i += 1;
            TokenKind::Operator
        } else {
            i += char_len(text, i);
            TokenKind::Punct
        };
        push(start_i, i, kind);
        if kind != TokenKind::Comment {
            prev = Some((kind, c));
        }
    }
    LexState::Normal
}

/// Lexes one line of a console JSON body from `start`; returns the end state.
pub(crate) fn lex_json(text: &str, start: LexState, out: &mut Vec<Token>) -> LexState {
    let mut semicolon = false;
    lex_js(text, start, Js::Json, 0, out, &mut semicolon)
}

pub(crate) const ES_METHODS: &[&str] = &["DELETE", "GET", "HEAD", "PATCH", "POST", "PUT"];

/// What starts a console line: `(keyword end, kind)` for a request line.
pub(crate) enum EsHead {
    Method,
    Esql,
    Sql,
}

/// Whether `line` starts an Elasticsearch console request (`GET …`, `ESQL …`, `SQL …`).
pub fn is_es_request_line(line: &str) -> bool {
    es_head(line).is_some()
}

/// A request line: its first word (any case) is a method, `ESQL` or `SQL`, followed by
/// whitespace or the end of the line. Returns the head kind and the word's byte range.
pub(crate) fn es_head(line: &str) -> Option<(EsHead, Range<usize>)> {
    let s = line.len() - line.trim_start().len();
    let rest = &line[s..];
    let w = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    if w == 0 || rest[w..].chars().next().is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let word = rest[..w].to_ascii_uppercase();
    let head = match word.as_str() {
        "ESQL" => EsHead::Esql,
        "SQL" => EsHead::Sql,
        m if ES_METHODS.contains(&m) => EsHead::Method,
        _ => return None,
    };
    Some((head, s..s + w))
}

fn lex_es(text: &str, start: LexState, out: &mut Vec<Token>) -> LexState {
    let mut state = start;
    let mut at = 0;
    let mut semi = false;
    for raw in text.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if state == LexState::Normal
            && let Some((head, word)) = es_head(line)
        {
            out.push(Token {
                start: at + word.start,
                end: at + word.end,
                kind: TokenKind::Keyword,
            });
            let rest = &line[word.end..];
            match head {
                EsHead::Method => {
                    let p = rest.len() - rest.trim_start().len();
                    let path = rest[p..]
                        .find(char::is_whitespace)
                        .map_or(rest.len(), |n| p + n);
                    if p < path {
                        out.push(Token {
                            start: at + word.end + p,
                            end: at + word.end + path,
                            kind: TokenKind::Ident,
                        });
                    }
                    state = lex_js(
                        &rest[path..],
                        LexState::Normal,
                        Js::Json,
                        at + word.end + path,
                        out,
                        &mut semi,
                    );
                }
                EsHead::Esql | EsHead::Sql => {
                    let mut toks = Vec::new();
                    SqlSyntax::ansi().lex(rest, LexState::Normal, &mut toks);
                    out.extend(toks.into_iter().map(|t| Token {
                        start: at + word.end + t.start,
                        end: at + word.end + t.end,
                        kind: t.kind,
                    }));
                }
            }
        } else {
            state = lex_js(line, state, Js::Json, at, out, &mut semi);
        }
        at += raw.len();
    }
    state
}

fn lex_redis(text: &str, out: &mut Vec<Token>) {
    let b = text.as_bytes();
    let mut i = 0;
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let s = i;
        let kind = if c == b'"' || c == b'\'' {
            i += 1;
            while i < b.len() && b[i] != b'\n' {
                if b[i] == b'\\' && i + 1 < b.len() && b[i + 1] != b'\n' {
                    i += 1 + char_len(text, i + 1);
                    continue;
                }
                if b[i] == c {
                    i += 1;
                    break;
                }
                i += 1;
            }
            TokenKind::String
        } else {
            while i < b.len() && !b[i].is_ascii_whitespace() {
                i += char_len(text, i);
            }
            if line_start {
                TokenKind::Keyword
            } else if text[s..i].parse::<f64>().is_ok() {
                TokenKind::Number
            } else {
                TokenKind::Ident
            }
        };
        line_start = false;
        out.push(Token {
            start: s,
            end: i,
            kind,
        });
    }
}
