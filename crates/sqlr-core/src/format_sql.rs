//! Conservative token formatter: only keyword case and inter-token whitespace change.
use crate::{LexState, SqlSyntax, TokenKind};
/// How the case of keywords changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Case {
    /// Upper case.
    #[default]
    Upper,
    /// Lower case.
    Lower,
    /// As typed.
    Preserve,
}
/// The indentation unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indent {
    /// That many spaces.
    Spaces(usize),
    /// One tab.
    Tabs,
}
/// The options of [`format_sql`].
#[derive(Clone, Debug)]
pub struct FormatOptions {
    /// Case of keywords.
    pub keyword_case: Case,
    /// Indentation unit for continuation lines.
    pub indent: Indent,
    /// Column past which a comma starts a new line.
    pub max_line: usize,
    /// Puts commas at the start of a line instead of the end.
    pub comma_first: bool,
    /// Starts a new line before `AND` and `OR`.
    pub break_before_and_or: bool,
    /// Whether statements are separated by an empty line.
    pub blank_line_between_statements: bool,
}
impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            keyword_case: Case::Upper,
            indent: Indent::Spaces(4),
            max_line: 100,
            comma_first: false,
            break_before_and_or: true,
            blank_line_between_statements: true,
        }
    }
}
/// Formats `text` for the SQL dialect of `syntax`; only keyword case and whitespace between tokens change.
pub fn format_sql(syntax: &SqlSyntax, text: &str, opts: &FormatOptions) -> String {
    let mut tokens = Vec::new();
    let end = syntax.lex(text, LexState::Normal, &mut tokens);
    // Unknown/unfinished constructs stay editable exactly as typed.
    if end.state != LexState::Normal {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut depth = 0usize;
    let mut line_len = 0usize;
    let mut previous: Option<(TokenKind, &str)> = None;
    for (index, token) in tokens.iter().enumerate() {
        let raw = &text[token.start..token.end];
        let upper = if token.kind == TokenKind::Keyword {
            raw.to_ascii_uppercase()
        } else {
            String::new()
        };
        let value = if token.kind == TokenKind::Keyword {
            match opts.keyword_case {
                Case::Upper => upper.as_str(),
                Case::Lower => raw,
                Case::Preserve => raw,
            }
        } else {
            raw
        };
        let is_clause = token.kind == TokenKind::Keyword
            && matches!(
                upper.as_str(),
                "SELECT"
                    | "FROM"
                    | "WHERE"
                    | "GROUP"
                    | "ORDER"
                    | "HAVING"
                    | "JOIN"
                    | "LEFT"
                    | "RIGHT"
                    | "INNER"
                    | "OUTER"
                    | "LIMIT"
                    | "OFFSET"
                    | "UNION"
                    | "VALUES"
                    | "SET"
                    | "RETURNING"
            );
        let is_bool = opts.break_before_and_or
            && token.kind == TokenKind::Keyword
            && matches!(upper.as_str(), "AND" | "OR");
        let prev = previous.map_or("", |(_, s)| s);
        let prev_comment =
            previous.is_some_and(|(k, s)| k == TokenKind::Comment && !s.starts_with("/*"));
        let prev_semi = previous.is_some_and(|(k, _)| k == TokenKind::Semicolon);
        let comma_break = (prev == "," && !opts.comma_first || raw == "," && opts.comma_first)
            && line_len >= opts.max_line;
        let gap = if index > 0 {
            &text[tokens[index - 1].end..token.start]
        } else {
            ""
        };
        let string_newline = token.kind == TokenKind::String
            && previous.is_some_and(|(k, _)| k == TokenKind::String)
            && gap.contains(['\n', '\r']);
        let newline = index > 0
            && (is_clause
                && !matches!(
                    (prev.to_ascii_uppercase().as_str(), upper.as_str()),
                    ("LEFT" | "RIGHT" | "INNER" | "OUTER", "JOIN")
                )
                || is_bool
                || prev_comment
                || prev_semi
                || comma_break
                || string_newline);
        if newline {
            out.push('\n');
            if prev_semi && opts.blank_line_between_statements {
                out.push('\n');
            }
            let indent = match opts.indent {
                Indent::Spaces(n) => " ".repeat(n.min(16) * depth.min(50)),
                Indent::Tabs => "\t".repeat(depth.min(50)),
            };
            out.push_str(&indent);
            line_len = indent.len();
        } else if index > 0 {
            let prefix = gap.is_empty()
                && (token.kind == TokenKind::String
                    || raw == "&"
                        && prev.eq_ignore_ascii_case("u")
                        && tokens
                            .get(index + 1)
                            .is_some_and(|t| t.kind == TokenKind::String && t.start == token.end));
            let spaced_operator = |kind: TokenKind, value: &str| {
                kind == TokenKind::Operator
                    && matches!(
                        value,
                        "=" | "!=" | "<>" | "<" | ">" | "<=" | ">=" | "+" | "-" | "*" | "/" | "%"
                    )
            };
            let ambiguous = gap.is_empty()
                && prev != ","
                && !spaced_operator(token.kind, raw)
                && !previous.is_some_and(|(kind, value)| spaced_operator(kind, value));
            let tight = ambiguous
                || prefix
                || matches!(raw, "," | ";" | ")" | ".")
                || matches!(prev, "(" | ".")
                || (raw == "("
                    && previous.is_some_and(|(k, _)| {
                        matches!(
                            k,
                            TokenKind::Function | TokenKind::Ident | TokenKind::QuotedIdent
                        )
                    }))
                || (prev == ":" && raw == ":");
            if !tight {
                out.push(' ');
                line_len += 1;
            }
        }
        if token.kind == TokenKind::Keyword && opts.keyword_case == Case::Lower {
            out.push_str(&raw.to_ascii_lowercase());
        } else {
            out.push_str(value);
        }
        line_len += value.len();
        if raw == "(" {
            depth += 1;
        } else if raw == ")" {
            depth = depth.saturating_sub(1);
        }
        previous = Some((token.kind, raw));
    }
    out
}

impl FormatOptions {
    /// Reads the options from settings, falling back to the defaults for unknown values.
    pub fn from_values(mut get: impl FnMut(&str) -> crate::SettingValue) -> Self {
        Self {
            keyword_case: match get("formatter.keyword_case").as_str() {
                "lower" => Case::Lower,
                "preserve" => Case::Preserve,
                _ => Case::Upper,
            },
            indent: match get("formatter.indent").as_str() {
                "2" => Indent::Spaces(2),
                "tabs" => Indent::Tabs,
                _ => Indent::Spaces(4),
            },
            max_line: get("formatter.max_line").as_i64().max(20) as usize,
            comma_first: get("formatter.comma_first").as_bool(),
            break_before_and_or: get("formatter.break_before_and_or").as_bool(),
            blank_line_between_statements: get("formatter.blank_line_between_statements").as_bool(),
        }
    }
}
