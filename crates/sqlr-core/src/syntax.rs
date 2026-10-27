//! Dialect-aware SQL lexer and statement splitter.
//!
//! The lexer is resumable: it takes the [`LexState`] a previous line ended in and
//! reports the state it ends in, so an editor can highlight line by line. It never
//! allocates per token and uses no regex.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::{Arc, LazyLock};

/// What the lexer is inside of at the end of a text (and so at the start of the next).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LexState {
    /// Not inside anything.
    #[default]
    Normal,
    /// Inside a block comment.
    BlockComment {
        /// How many comments are open (nested comments only).
        depth: u16,
    },
    /// Inside a single-quoted string.
    Str {
        /// Whether backslash escapes the next character.
        escapes: bool,
    },
    /// A `"…"` string (MySQL).
    DoubleStr {
        /// Whether backslash escapes the next character.
        escapes: bool,
    },
    /// A quoted identifier that ends at `close` (`"`, `]` or `` ` ``).
    QuotedIdent {
        /// The byte that closes it.
        close: u8,
    },
    /// Inside a dollar-quoted string (PostgreSQL).
    DollarQuote {
        /// Hash of the opening tag, so only the same tag closes it.
        tag: u64,
    },
}

/// The kind of a lexed token, which decides its highlighting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// A keyword of the dialect.
    Keyword,
    /// A function name of the dialect.
    Function,
    /// An unquoted identifier.
    Ident,
    /// An identifier in the dialect's quotes.
    QuotedIdent,
    /// A string literal.
    String,
    /// A number literal.
    Number,
    /// A line or block comment.
    Comment,
    /// An operator.
    Operator,
    /// Other punctuation.
    Punct,
    /// A top-level `;`.
    Semicolon,
    /// A bound parameter placeholder.
    Param,
}

/// A lexed token; `start..end` are byte offsets into the lexed text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    /// Byte offset of the first byte.
    pub start: usize,
    /// Byte offset after the last byte.
    pub end: usize,
    /// What the token is.
    pub kind: TokenKind,
}

/// Where lexing a text ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LexEnd {
    /// What the lexer is inside of at the end.
    pub state: LexState,
    /// A top-level `;` was seen.
    pub semicolon: bool,
}

/// The lexical rules of one SQL dialect: keywords, functions, quotes and comment styles.
#[derive(Clone, Debug)]
pub struct SqlSyntax {
    /// UPPERCASE, sorted, deduped.
    pub keywords: Vec<&'static str>,
    /// UPPERCASE, sorted: identifiers equal to one of these are quoted on insert.
    pub reserved: Vec<&'static str>,
    /// lowercase, sorted.
    pub functions: Vec<&'static str>,
    /// `$tag$ ... $tag$` (PostgreSQL).
    pub dollar_quotes: bool,
    /// `/* /* */ */` (PostgreSQL).
    pub nested_comments: bool,
    /// `E'...'` with backslash escapes (PostgreSQL).
    pub escape_strings: bool,
    /// The text that starts a line comment.
    pub line_comment: &'static str,
    /// The line comment needs whitespace after it (MySQL `-- `).
    pub line_comment_needs_space: bool,
    /// `#` starts a line comment (MySQL).
    pub hash_comments: bool,
    /// Identifier quotes as `(open, close)`; the first pair is used to quote names.
    pub ident_quotes: &'static [(u8, u8)],
    /// `"…"` is a string literal, not an identifier (MySQL).
    pub double_quote_strings: bool,
    /// Every string literal takes backslash escapes (MySQL).
    pub backslash_escapes: bool,
    /// tree-sitter grammar id; `"sequel"` is tree-sitter-sequel.
    pub grammar: &'static str,
}

/// ANSI identifier quotes.
pub const DOUBLE_QUOTE_IDENTS: &[(u8, u8)] = &[(b'"', b'"')];

const ANSI_KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BETWEEN",
    "BY",
    "CASE",
    "CAST",
    "CHECK",
    "COLUMN",
    "COMMIT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXISTS",
    "FALSE",
    "FETCH",
    "FOREIGN",
    "FROM",
    "FULL",
    "GRANT",
    "GROUP",
    "HAVING",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "PRIMARY",
    "REFERENCES",
    "RIGHT",
    "ROLLBACK",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "TRUE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WITH",
];

const ANSI_FUNCTIONS: &[&str] = &[
    "abs", "avg", "cast", "coalesce", "count", "lower", "max", "min", "nullif", "round", "sum",
    "upper",
];

static ANSI: LazyLock<Arc<SqlSyntax>> = LazyLock::new(|| {
    let sorted = |list: &[&'static str]| {
        let mut v = list.to_vec();
        v.sort_unstable();
        v.dedup();
        v
    };
    Arc::new(SqlSyntax {
        keywords: sorted(ANSI_KEYWORDS),
        reserved: sorted(ANSI_KEYWORDS),
        functions: sorted(ANSI_FUNCTIONS),
        dollar_quotes: false,
        nested_comments: false,
        escape_strings: false,
        line_comment: "--",
        line_comment_needs_space: false,
        hash_comments: false,
        ident_quotes: DOUBLE_QUOTE_IDENTS,
        double_quote_strings: false,
        backslash_escapes: false,
        grammar: "sequel",
    })
});

/// FNV-1a 64 of a dollar-quote tag.
pub fn dollar_tag_hash(tag: &str) -> u64 {
    fnv1a(tag.as_bytes())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Longest word that can be looked up; longer words are never keywords or functions.
const LOOKUP_MAX: usize = 64;

fn contains_word(list: &[&str], word: &str, upper: bool) -> bool {
    let bytes = word.as_bytes();
    if bytes.len() > LOOKUP_MAX {
        return false;
    }
    let mut buf = [0u8; LOOKUP_MAX];
    let buf = &mut buf[..bytes.len()];
    for (dst, &b) in buf.iter_mut().zip(bytes) {
        *dst = if upper {
            b.to_ascii_uppercase()
        } else {
            b.to_ascii_lowercase()
        };
    }
    list.binary_search_by(|k| k.as_bytes().cmp(&*buf)).is_ok()
}

fn is_word_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn is_operator_byte(b: u8) -> bool {
    // `#` is an operator in PostgreSQL and a comment in MySQL (checked first there).
    matches!(
        b,
        b'+' | b'-'
            | b'*'
            | b'/'
            | b'<'
            | b'>'
            | b'='
            | b'~'
            | b'!'
            | b'@'
            | b'#'
            | b'%'
            | b'^'
            | b'&'
            | b'|'
    )
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xff => 4,
        _ => 1,
    }
}

/// End of the word starting at `i`, or `i` if no word starts there.
fn word_end(text: &str, i: usize) -> usize {
    let bytes = text.as_bytes();
    if !(is_word_start(bytes[i]) || (bytes[i] >= 0x80 && starts_alnum(text, i))) {
        return i;
    }
    let mut j = i;
    while j < bytes.len() {
        let b = bytes[j];
        if b < 0x80 {
            if !is_word_byte(b) {
                break;
            }
            j += 1;
        } else if starts_alnum(text, j) {
            j += utf8_len(b);
        } else {
            break;
        }
    }
    j
}

fn starts_alnum(text: &str, i: usize) -> bool {
    text[i..].chars().next().is_some_and(char::is_alphanumeric)
}

/// Scans a (possibly nested) block comment body from `i`. Returns the end and the
/// remaining depth (0 when closed).
fn scan_block_comment(bytes: &[u8], mut i: usize, mut depth: u16, nested: bool) -> (usize, u16) {
    while i < bytes.len() {
        if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            i += 2;
            depth -= 1;
            if depth == 0 {
                return (i, 0);
            }
        } else if nested && bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            depth = depth.saturating_add(1);
        } else {
            i += 1;
        }
    }
    (bytes.len(), depth)
}

/// Scans a quoted body (`quote` doubled to escape) from `i`. Returns the end and
/// whether the closing quote was found.
fn scan_quoted(bytes: &[u8], mut i: usize, quote: u8, escapes: bool) -> (usize, bool) {
    while i < bytes.len() {
        let b = bytes[i];
        if escapes && b == b'\\' {
            i += 1;
            if i < bytes.len() {
                i += utf8_len(bytes[i]);
            }
        } else if b == quote {
            if bytes.get(i + 1) == Some(&quote) {
                i += 2;
            } else {
                return (i + 1, true);
            }
        } else {
            i += 1;
        }
    }
    (bytes.len(), false)
}

/// With `bytes[i] == b'$'`, the index of the `$` closing a tag (`$tag$` or `$$`).
fn dollar_tag_close(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    let first = *bytes.get(j)?;
    if first != b'$' {
        if !(is_word_start(first) || first >= 0x80) {
            return None;
        }
        while j < bytes.len() && bytes[j] != b'$' && (is_word_byte(bytes[j]) || bytes[j] >= 0x80) {
            j += 1;
        }
    }
    (bytes.get(j) == Some(&b'$')).then_some(j)
}

/// Scans a dollar-quoted body from `i` until its closing tag. Returns the end and
/// whether it was closed.
fn scan_dollar(bytes: &[u8], mut i: usize, tag: u64) -> (usize, bool) {
    while i < bytes.len() {
        if bytes[i] == b'$'
            && let Some(j) = dollar_tag_close(bytes, i)
            && fnv1a(&bytes[i + 1..j]) == tag
        {
            return (j + 1, true);
        }
        i += 1;
    }
    (bytes.len(), false)
}

impl SqlSyntax {
    /// A shared ANSI-ish syntax with all dialect flags off.
    pub fn ansi() -> Arc<SqlSyntax> {
        ANSI.clone()
    }

    /// Lexes `text` starting in `start`, appending tokens (no whitespace tokens) to `out`.
    pub fn lex(&self, text: &str, start: LexState, out: &mut Vec<Token>) -> LexEnd {
        self.lex_with(text, start, &mut |t| {
            out.push(t);
            true
        })
    }

    /// Byte ranges of the statements in `text`: split at top-level `;`, trimmed of
    /// whitespace, dropping ranges that hold only whitespace and comments.
    pub fn split(&self, text: &str, start: LexState) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        // The current statement: its first token start, last token end, and whether
        // it holds anything but comments.
        let mut cur: Option<(usize, usize, bool)> = None;
        let mut flush = |cur: &mut Option<(usize, usize, bool)>| {
            if let Some((s, e, true)) = cur.take() {
                ranges.push(s..e);
            }
            *cur = None;
        };
        self.lex_with(text, start, &mut |t| {
            if t.kind == TokenKind::Semicolon {
                flush(&mut cur);
            } else {
                let significant = t.kind != TokenKind::Comment;
                cur = Some(match cur {
                    Some((s, _, sig)) => (s, t.end, sig || significant),
                    None => (t.start, t.end, significant),
                });
            }
            true
        });
        flush(&mut cur);
        ranges
    }

    /// Whether the first keyword (skipping comments) starts a DDL statement.
    pub fn is_ddl(&self, sql: &str) -> bool {
        let mut ddl = false;
        self.lex_with(sql, LexState::Normal, &mut |t| {
            if t.kind == TokenKind::Comment {
                return true;
            }
            if matches!(
                t.kind,
                TokenKind::Keyword | TokenKind::Ident | TokenKind::Function
            ) {
                let word = &sql[t.start..t.end];
                ddl = ["CREATE", "ALTER", "DROP", "COMMENT", "RENAME"]
                    .iter()
                    .any(|k| word.eq_ignore_ascii_case(k));
            }
            false
        });
        ddl
    }

    /// Whether `word` is a keyword, ignoring case.
    pub fn is_keyword(&self, word: &str) -> bool {
        contains_word(&self.keywords, word, true)
    }

    /// Whether `word` is reserved, so an identifier of that name must be quoted; ignores case.
    pub fn is_reserved(&self, word: &str) -> bool {
        contains_word(&self.reserved, word, true)
    }

    /// Whether `word` is a function name, ignoring case.
    pub fn is_function(&self, word: &str) -> bool {
        contains_word(&self.functions, word, false)
    }

    /// `ident` as-is when it is a plain lowercase name that is not reserved, else
    /// wrapped in the dialect's identifier quotes with the closing quote doubled.
    pub fn quote_if_needed<'a>(&self, ident: &'a str) -> Cow<'a, str> {
        let b = ident.as_bytes();
        let plain = !b.is_empty()
            && (b[0].is_ascii_lowercase() || b[0] == b'_')
            && b.iter()
                .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_');
        if plain && !self.is_reserved(ident) {
            Cow::Borrowed(ident)
        } else {
            let (open, close) = self.ident_quotes.first().copied().unwrap_or((b'"', b'"'));
            let (open, close) = (char::from(open), char::from(close));
            let doubled: String = [close, close].iter().collect();
            Cow::Owned(format!("{open}{}{close}", ident.replace(close, &doubled)))
        }
    }

    /// `(name, was_quoted)`; a quoted identifier loses its quotes and the doubling of its
    /// closing quote.
    pub fn unquote(&self, ident: &str) -> (String, bool) {
        let b = ident.as_bytes();
        for &(open, close) in self.ident_quotes {
            if b.len() >= 2 && b[0] == open && b[b.len() - 1] == close {
                let close = char::from(close);
                let inner = &ident[1..ident.len() - 1];
                let doubled: String = [close, close].iter().collect();
                return (inner.replace(&doubled, &close.to_string()), true);
            }
        }
        (ident.to_string(), false)
    }

    /// The identifier quote pair that opens with `b`, if any.
    fn ident_close(&self, b: u8) -> Option<u8> {
        self.ident_quotes
            .iter()
            .find(|(open, _)| *open == b)
            .map(|&(_, close)| close)
    }

    /// Whether a line comment starts at `i`.
    fn line_comment_at(&self, bytes: &[u8], i: usize) -> bool {
        let lc = self.line_comment.as_bytes();
        if self.hash_comments && bytes[i] == b'#' {
            return true;
        }
        if lc.is_empty() || !bytes[i..].starts_with(lc) {
            return false;
        }
        !self.line_comment_needs_space
            || bytes
                .get(i + lc.len())
                .is_none_or(|&c| c.is_ascii_whitespace() || c.is_ascii_control())
    }

    /// The lexer core: `emit` gets every token and returns `false` to stop early.
    fn lex_with(&self, text: &str, start: LexState, emit: &mut dyn FnMut(Token) -> bool) -> LexEnd {
        let bytes = text.as_bytes();
        let len = bytes.len();
        let mut semicolon = false;
        let mut i = 0;
        let mut state = start;

        macro_rules! emit {
            ($s:expr, $e:expr, $k:expr) => {
                if $s < $e
                    && !emit(Token {
                        start: $s,
                        end: $e,
                        kind: $k,
                    })
                {
                    return LexEnd { state, semicolon };
                }
            };
        }

        // Resume a construct left open by the previous text.
        match start {
            LexState::Normal => {}
            LexState::BlockComment { depth } => {
                let (end, left) = scan_block_comment(bytes, 0, depth.max(1), self.nested_comments);
                state = if left == 0 {
                    LexState::Normal
                } else {
                    LexState::BlockComment { depth: left }
                };
                emit!(0, end, TokenKind::Comment);
                i = end;
            }
            LexState::Str { escapes } => {
                let (end, closed) = scan_quoted(bytes, 0, b'\'', escapes);
                if closed {
                    state = LexState::Normal;
                }
                emit!(0, end, TokenKind::String);
                i = end;
            }
            LexState::DoubleStr { escapes } => {
                let (end, closed) = scan_quoted(bytes, 0, b'"', escapes);
                if closed {
                    state = LexState::Normal;
                }
                emit!(0, end, TokenKind::String);
                i = end;
            }
            LexState::QuotedIdent { close } => {
                let (end, closed) = scan_quoted(bytes, 0, close, false);
                if closed {
                    state = LexState::Normal;
                }
                emit!(0, end, TokenKind::QuotedIdent);
                i = end;
            }
            LexState::DollarQuote { tag } => {
                let (end, closed) = scan_dollar(bytes, 0, tag);
                if closed {
                    state = LexState::Normal;
                }
                emit!(0, end, TokenKind::String);
                i = end;
            }
        }
        if state != LexState::Normal {
            return LexEnd { state, semicolon };
        }

        while i < len {
            let b = bytes[i];
            let s = i;
            if b.is_ascii_whitespace() {
                i += 1;
                continue;
            }
            if self.line_comment_at(bytes, i) {
                i = bytes[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(len, |p| i + p);
                emit!(s, i, TokenKind::Comment);
                continue;
            }
            match b {
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    let (end, left) = scan_block_comment(bytes, i + 2, 1, self.nested_comments);
                    if left != 0 {
                        state = LexState::BlockComment { depth: left };
                    }
                    i = end;
                    emit!(s, i, TokenKind::Comment);
                    if left != 0 {
                        break;
                    }
                }
                b'\'' => {
                    let escapes = self.backslash_escapes;
                    let (end, closed) = scan_quoted(bytes, i + 1, b'\'', escapes);
                    i = end;
                    if !closed {
                        state = LexState::Str { escapes };
                    }
                    emit!(s, i, TokenKind::String);
                    if !closed {
                        break;
                    }
                }
                _ if let Some(close) = self.ident_close(b) => {
                    let (end, closed) = scan_quoted(bytes, i + 1, close, false);
                    i = end;
                    if !closed {
                        state = LexState::QuotedIdent { close };
                    }
                    emit!(s, i, TokenKind::QuotedIdent);
                    if !closed {
                        break;
                    }
                }
                b'"' if self.double_quote_strings => {
                    let escapes = self.backslash_escapes;
                    let (end, closed) = scan_quoted(bytes, i + 1, b'"', escapes);
                    i = end;
                    if !closed {
                        state = LexState::DoubleStr { escapes };
                    }
                    emit!(s, i, TokenKind::String);
                    if !closed {
                        break;
                    }
                }
                b'$' if self.dollar_quotes => {
                    if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                        while i < len && bytes[i].is_ascii_digit() {
                            i += 1;
                        }
                        emit!(s, i, TokenKind::Param);
                    } else if let Some(j) = dollar_tag_close(bytes, i) {
                        let tag = fnv1a(&bytes[i + 1..j]);
                        let (end, closed) = scan_dollar(bytes, j + 1, tag);
                        i = end;
                        if !closed {
                            state = LexState::DollarQuote { tag };
                        }
                        emit!(s, i, TokenKind::String);
                        if !closed {
                            break;
                        }
                    } else {
                        i += 1;
                        emit!(s, i, TokenKind::Punct);
                    }
                }
                b'0'..=b'9' => {
                    while i < len && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    if bytes.get(i) == Some(&b'.')
                        && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
                    {
                        i += 1;
                        while i < len && bytes[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                    if matches!(bytes.get(i), Some(b'e' | b'E')) {
                        let mut j = i + 1;
                        if matches!(bytes.get(j), Some(b'+' | b'-')) {
                            j += 1;
                        }
                        if bytes.get(j).is_some_and(u8::is_ascii_digit) {
                            while j < len && bytes[j].is_ascii_digit() {
                                j += 1;
                            }
                            i = j;
                        }
                    }
                    emit!(s, i, TokenKind::Number);
                }
                b';' => {
                    semicolon = true;
                    i += 1;
                    emit!(s, i, TokenKind::Semicolon);
                }
                b'(' | b')' | b'[' | b']' | b',' | b'.' => {
                    i += 1;
                    emit!(s, i, TokenKind::Punct);
                }
                b'?' => {
                    i += 1;
                    emit!(s, i, TokenKind::Param);
                }
                b':' if bytes.get(i + 1) != Some(&b':') => {
                    i += 1;
                    let kind = if bytes.get(i).is_some_and(|&c| is_word_start(c)) {
                        i = word_end(text, i);
                        TokenKind::Param
                    } else {
                        TokenKind::Punct
                    };
                    emit!(s, i, kind);
                }
                _ if is_operator_byte(b) || b == b':' => {
                    while i < len {
                        let c = bytes[i];
                        if c == b':' && bytes.get(i + 1) == Some(&b':') {
                            i += 2;
                        } else if is_operator_byte(c)
                            && !(c == b'/' && bytes.get(i + 1) == Some(&b'*'))
                            && !self.line_comment_at(bytes, i)
                        {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    emit!(s, i, TokenKind::Operator);
                }
                _ => {
                    let end = word_end(text, i);
                    if end == i {
                        // Not a word: a stray character becomes one Punct token.
                        i += utf8_len(b);
                        emit!(s, i, TokenKind::Punct);
                        continue;
                    }
                    i = end;
                    let word = &text[s..i];
                    if self.escape_strings
                        && (word == "E" || word == "e")
                        && bytes.get(i) == Some(&b'\'')
                    {
                        let (end, closed) = scan_quoted(bytes, i + 1, b'\'', true);
                        i = end;
                        if !closed {
                            state = LexState::Str { escapes: true };
                        }
                        emit!(s, i, TokenKind::String);
                        if !closed {
                            break;
                        }
                        continue;
                    }
                    let kind = if bytes.get(i) == Some(&b'(') && self.is_function(word) {
                        TokenKind::Function
                    } else if self.is_keyword(word) {
                        TokenKind::Keyword
                    } else {
                        TokenKind::Ident
                    };
                    emit!(s, i, kind);
                }
            }
        }
        LexEnd { state, semicolon }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg_like() -> SqlSyntax {
        SqlSyntax {
            dollar_quotes: true,
            nested_comments: true,
            escape_strings: true,
            ..(*SqlSyntax::ansi()).clone()
        }
    }

    fn lex(s: &SqlSyntax, text: &str, start: LexState) -> (Vec<Token>, LexEnd) {
        let mut out = Vec::new();
        let end = s.lex(text, start, &mut out);
        (out, end)
    }

    fn kinds(tokens: &[Token]) -> Vec<TokenKind> {
        tokens.iter().map(|t| t.kind).collect()
    }

    fn texts<'a>(text: &'a str, ranges: &[std::ops::Range<usize>]) -> Vec<&'a str> {
        ranges.iter().map(|r| &text[r.clone()]).collect()
    }

    #[test]
    fn lex_basic_kinds() {
        use TokenKind::*;
        let (t, _) = lex(
            &SqlSyntax::ansi(),
            r#"select count(*) from "My T" where x = 'a''b' -- c"#,
            LexState::Normal,
        );
        assert_eq!(
            kinds(&t),
            [
                Keyword,
                Function,
                Punct,
                Operator,
                Punct,
                Keyword,
                QuotedIdent,
                Keyword,
                Ident,
                Operator,
                String,
                Comment
            ]
        );
    }

    #[test]
    fn state_carries_across_lines() {
        let s = SqlSyntax::ansi();
        let (_, end) = lex(&s, "select '", LexState::Normal);
        assert_eq!(
            end,
            LexEnd {
                state: LexState::Str { escapes: false },
                semicolon: false
            }
        );
        let (t, end) = lex(&s, "abc'; x", LexState::Str { escapes: false });
        assert_eq!(
            t,
            [
                Token {
                    start: 0,
                    end: 4,
                    kind: TokenKind::String
                },
                Token {
                    start: 4,
                    end: 5,
                    kind: TokenKind::Semicolon
                },
                Token {
                    start: 6,
                    end: 7,
                    kind: TokenKind::Ident
                },
            ]
        );
        assert!(end.semicolon);
        assert_eq!(end.state, LexState::Normal);
    }

    #[test]
    fn nested_comment_depth() {
        let (_, end) = lex(&pg_like(), "/* a /* b */", LexState::Normal);
        assert_eq!(end.state, LexState::BlockComment { depth: 1 });
        let (_, end) = lex(&SqlSyntax::ansi(), "/* a /* b */", LexState::Normal);
        assert_eq!(end.state, LexState::Normal);
    }

    #[test]
    fn dollar_quote_tags() {
        let (t, end) = lex(&pg_like(), "$fn$ a; $x$ b $fn$;", LexState::Normal);
        assert_eq!(kinds(&t), [TokenKind::String, TokenKind::Semicolon]);
        assert_eq!(end.state, LexState::Normal);
        let (t, _) = lex(&pg_like(), "$1", LexState::Normal);
        assert_eq!(kinds(&t), [TokenKind::Param]);
        let (_, end) = lex(&pg_like(), "$fn$ open", LexState::Normal);
        assert_eq!(
            end.state,
            LexState::DollarQuote {
                tag: dollar_tag_hash("fn")
            }
        );
    }

    #[test]
    fn escape_strings_take_backslashes() {
        let (t, end) = lex(&pg_like(), r"E'a\'b' x", LexState::Normal);
        assert_eq!(kinds(&t), [TokenKind::String, TokenKind::Ident]);
        assert_eq!(end.state, LexState::Normal);
        let (_, end) = lex(&pg_like(), "E'a", LexState::Normal);
        assert_eq!(end.state, LexState::Str { escapes: true });
    }

    #[test]
    fn numbers_params_operators() {
        use TokenKind::*;
        let (t, _) = lex(
            &SqlSyntax::ansi(),
            "1.5e-3 ? :name a::int -1",
            LexState::Normal,
        );
        assert_eq!(
            kinds(&t),
            [
                Number, Param, Param, Ident, Operator, Ident, Operator, Number
            ]
        );
        let (t, _) = lex(&SqlSyntax::ansi(), "a<>b--c", LexState::Normal);
        assert_eq!(kinds(&t), [Ident, Operator, Ident, Comment]);
    }

    #[test]
    fn multibyte_tokens_stay_on_char_boundaries() {
        let text = "select 'é😀', café, 😀 from \"ü\"";
        let (t, _) = lex(&SqlSyntax::ansi(), text, LexState::Normal);
        for tok in &t {
            assert!(text.is_char_boundary(tok.start) && text.is_char_boundary(tok.end));
        }
        assert!(
            t.iter()
                .any(|k| &text[k.start..k.end] == "café" && k.kind == TokenKind::Ident)
        );
    }

    #[test]
    fn split_ignores_semicolons_in_literals() {
        let text = "select ';' ; select \";\"; -- ; x\n/* ; */ select 3;;  -- end";
        let r = SqlSyntax::ansi().split(text, LexState::Normal);
        assert_eq!(
            texts(text, &r),
            ["select ';'", "select \";\"", "-- ; x\n/* ; */ select 3"]
        );
    }

    #[test]
    fn split_dollar_quoted_body() {
        let text = "create function f() returns int as $$ begin; return 1; end $$ language plpgsql; select 1";
        let r = pg_like().split(text, LexState::Normal);
        assert_eq!(r.len(), 2);
        assert_eq!(&text[r[1].clone()], "select 1");
    }

    #[test]
    fn split_from_inside_comment() {
        let text = "x */ select 1; select 2";
        let r = SqlSyntax::ansi().split(text, LexState::BlockComment { depth: 1 });
        assert_eq!(texts(text, &r), ["x */ select 1", "select 2"]);
        assert_eq!(r[0].start, 0);
    }

    #[test]
    fn is_ddl_skips_comments() {
        let s = SqlSyntax::ansi();
        assert!(s.is_ddl("-- c\n  create table t()"));
        assert!(s.is_ddl("COMMENT ON TABLE t IS 'x'"));
        assert!(!s.is_ddl("select 1"));
        assert!(!s.is_ddl("/* drop */ select"));
        assert!(!s.is_ddl(""));
    }

    #[test]
    fn keyword_lookup_is_case_insensitive() {
        let s = SqlSyntax::ansi();
        assert!(s.is_keyword("SeLeCt"));
        assert!(!s.is_keyword("users"));
        assert!(!s.is_keyword(&"select".repeat(20)));
        assert!(s.is_function("COUNT"));
        assert!(s.is_reserved("table"));
        assert!(!s.is_reserved("users"));
    }

    #[test]
    fn quote_if_needed_rules() {
        let s = SqlSyntax::ansi();
        assert_eq!(s.quote_if_needed("users"), "users");
        assert_eq!(s.quote_if_needed("Users"), "\"Users\"");
        assert_eq!(s.quote_if_needed("order items"), "\"order items\"");
        assert_eq!(s.quote_if_needed("select"), "\"select\"");
        assert_eq!(s.quote_if_needed("a\"b"), "\"a\"\"b\"");
        assert!(matches!(
            s.quote_if_needed("users"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn unquote_rules() {
        let s = SqlSyntax::ansi();
        assert_eq!(
            s.unquote("\"My \"\"T\"\"\""),
            ("My \"T\"".to_string(), true)
        );
        assert_eq!(s.unquote("abc"), ("abc".to_string(), false));
    }

    fn mysql_like() -> SqlSyntax {
        SqlSyntax {
            line_comment_needs_space: true,
            hash_comments: true,
            ident_quotes: &[(b'`', b'`')],
            double_quote_strings: true,
            backslash_escapes: true,
            ..(*SqlSyntax::ansi()).clone()
        }
    }

    fn sqlite_like() -> SqlSyntax {
        SqlSyntax {
            ident_quotes: &[(b'"', b'"'), (b'[', b']'), (b'`', b'`')],
            ..(*SqlSyntax::ansi()).clone()
        }
    }

    #[test]
    fn mysql_quotes_and_comments() {
        use TokenKind::*;
        let s = mysql_like();
        let (t, end) = lex(
            &s,
            r#"select `a``b`, "x;\"y", 'it\'s' # c ; d"#,
            LexState::Normal,
        );
        assert_eq!(
            kinds(&t),
            [Keyword, QuotedIdent, Punct, String, Punct, String, Comment]
        );
        assert!(!end.semicolon);
        let (t, _) = lex(&s, "a--b -- c\n/*! STRAIGHT_JOIN */ x", LexState::Normal);
        assert_eq!(kinds(&t), [Ident, Operator, Ident, Comment, Comment, Ident]);
        let (_, end) = lex(&s, "select \"open", LexState::Normal);
        assert_eq!(end.state, LexState::DoubleStr { escapes: true });
        let (_, end) = lex(&s, "select `open", LexState::Normal);
        assert_eq!(end.state, LexState::QuotedIdent { close: b'`' });
        assert_eq!(s.quote_if_needed("my table"), "`my table`");
        assert_eq!(s.quote_if_needed("a`b"), "`a``b`");
        assert_eq!(s.unquote("`a``b`"), ("a`b".to_string(), true));
        let r = s.split(
            "select '\\\\'; select \";\"; # ; x\nselect 3",
            LexState::Normal,
        );
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn sqlite_bracket_and_backtick_identifiers() {
        let s = sqlite_like();
        let text = "select [a ; b], `c;d`, \"e\" from t; select 2";
        let (t, _) = lex(&s, text, LexState::Normal);
        let quoted: Vec<&str> = t
            .iter()
            .filter(|t| t.kind == TokenKind::QuotedIdent)
            .map(|t| &text[t.start..t.end])
            .collect();
        assert_eq!(quoted, ["[a ; b]", "`c;d`", "\"e\""]);
        assert_eq!(s.split(text, LexState::Normal).len(), 2);
        assert_eq!(s.unquote("[a]]b]"), ("a]b".to_string(), true));
        assert_eq!(s.quote_if_needed("A"), "\"A\"");
        let (_, end) = lex(&s, "[open", LexState::Normal);
        assert_eq!(end.state, LexState::QuotedIdent { close: b']' });
        let (t, end) = lex(&s, "x] y", LexState::QuotedIdent { close: b']' });
        assert_eq!(kinds(&t), [TokenKind::QuotedIdent, TokenKind::Ident]);
        assert_eq!(end.state, LexState::Normal);
    }

    #[test]
    fn ansi_lists_are_sorted_and_deduped() {
        let s = SqlSyntax::ansi();
        for l in [&s.keywords, &s.reserved, &s.functions] {
            assert!(l.windows(2).all(|w| w[0] < w[1]));
        }
    }
}
