//! The Elasticsearch console (Kibana Dev Tools style).
//!
//! A request is a `METHOD path[?query]` line followed by an optional body: one JSON value,
//! or several (one per line, NDJSON, for `_bulk` and `_msearch`). Bodies may hold `//`,
//! `/* */` and `#` comments and `"""…"""` strings that span lines. `ESQL <query>` and
//! `SQL <query>` run a query whose text continues until a blank line or the next request.
//! Requests end at a blank line outside a body or at the next request line.

use std::ops::Range;

use crate::lang::{EsHead, es_head, lex_json};
use crate::mongo_shell::ShellError;
use crate::syntax::{LexState, TokenKind};

/// One request of an Elasticsearch console script, ready for the Elasticsearch Driver.
#[derive(Clone, Debug, PartialEq)]
pub enum EsRequest {
    /// `path` always starts with `/`. A body of several NDJSON values is a JSON array.
    Http {
        /// The HTTP method, upper case.
        method: String,
        /// The request path, starting with `/`.
        path: String,
        /// The JSON body, when there is one.
        body: Option<serde_json::Value>,
    },
    /// An ES|QL query.
    Esql(String),
    /// An Elasticsearch SQL query.
    Sql(String),
}

impl EsRequest {
    /// Whether the body is sent as NDJSON (`_bulk`, `_msearch`): several values on lines.
    pub fn is_ndjson(&self) -> bool {
        match self {
            EsRequest::Http { path, body, .. } => {
                let p = path.split('?').next().unwrap_or("");
                matches!(body, Some(serde_json::Value::Array(_)))
                    && ["_bulk", "_msearch", "_msearch/template"]
                        .iter()
                        .any(|e| p.ends_with(e))
            }
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Http,
    Esql,
    Sql,
    /// Text before any request line.
    Stray,
}

#[derive(Clone, Debug)]
struct Block {
    kind: BlockKind,
    /// The head line's keyword (`GET`, `ESQL`, …).
    word: Range<usize>,
    /// Trimmed: from the head (or stray text) to the end of the last non-blank line.
    range: Range<usize>,
    /// Where the body starts (the line after the head).
    body_start: usize,
}

/// Splits the console text into blocks by its lines.
fn scan(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut open: Option<Block> = None;
    // JSON state across body lines: the lexer state and the bracket depth.
    let mut state = LexState::Normal;
    let mut depth = 0i32;
    let mut at = 0;
    for raw in text.split_inclusive('\n') {
        let line_start = at;
        at += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim();
        let content = line_start + (line.len() - line.trim_start().len());
        let content_end = line_start + line.trim_end().len();
        let top = state == LexState::Normal && depth <= 0;
        if top {
            if trimmed.is_empty() {
                blocks.extend(open.take());
                continue;
            }
            if trimmed.starts_with('#') || trimmed.starts_with("//") {
                continue;
            }
            if let Some((head, word)) = es_head(line) {
                blocks.extend(open.take());
                open = Some(Block {
                    kind: match head {
                        EsHead::Method => BlockKind::Http,
                        EsHead::Esql => BlockKind::Esql,
                        EsHead::Sql => BlockKind::Sql,
                    },
                    word: line_start + word.start..line_start + word.end,
                    range: content..content_end,
                    body_start: at,
                });
                (state, depth) = (LexState::Normal, 0);
                continue;
            }
        }
        if trimmed.is_empty() {
            continue;
        }
        let block = open.get_or_insert(Block {
            kind: BlockKind::Stray,
            word: content..content,
            range: content..content_end,
            body_start: line_start,
        });
        block.range.end = content_end;
        if block.kind == BlockKind::Http {
            let mut toks = Vec::new();
            let end = lex_json(line, state, &mut toks);
            for t in toks {
                if t.kind == TokenKind::Punct {
                    match &line[t.start..t.end] {
                        "{" | "[" => depth += 1,
                        "}" | "]" => depth -= 1,
                        _ => {}
                    }
                }
            }
            state = end;
        }
    }
    blocks.extend(open);
    blocks
}

/// Request ranges in `text` (also for text that does not parse).
pub fn split_es_console(text: &str) -> Vec<Range<usize>> {
    scan(text).into_iter().map(|b| b.range).collect()
}

/// Parses an Elasticsearch console script into its requests with the text range of each.
pub fn parse_es_console(text: &str) -> Result<Vec<(Range<usize>, EsRequest)>, ShellError> {
    scan(text)
        .into_iter()
        .map(|b| {
            let req = match b.kind {
                BlockKind::Stray => {
                    return Err(ShellError::new(
                        "expected a request line such as `GET my-index/_search`, \
                         `ESQL FROM my-index` or `SQL SELECT …`",
                        b.range.start,
                    ));
                }
                BlockKind::Esql => EsRequest::Esql(text[b.word.end..b.range.end].trim().into()),
                BlockKind::Sql => EsRequest::Sql(text[b.word.end..b.range.end].trim().into()),
                BlockKind::Http => {
                    let head_end = text[b.word.end..]
                        .find('\n')
                        .map_or(text.len(), |n| b.word.end + n);
                    let head = text[b.word.end..head_end].trim();
                    let mut parts = head.split_whitespace();
                    let path = parts.next().unwrap_or("/");
                    if let Some(extra) = parts.next() {
                        let pos = b.word.end + text[b.word.end..].find(extra).unwrap_or(0);
                        return Err(ShellError::new(
                            format!(
                                "unexpected `{extra}` after the path; put the body on the next line"
                            ),
                            pos,
                        ));
                    }
                    let path = if path.starts_with('/') {
                        path.to_string()
                    } else {
                        format!("/{path}")
                    };
                    let body = if b.body_start < b.range.end {
                        parse_body(&text[b.body_start..b.range.end], b.body_start)?
                    } else {
                        None
                    };
                    EsRequest::Http {
                        method: text[b.word.clone()].to_ascii_uppercase(),
                        path,
                        body,
                    }
                }
            };
            Ok((b.range, req))
        })
        .collect()
}

/// Rewrites a console body as strict JSON: comments dropped, `"""…"""` as JSON strings.
/// Also returns, for each output byte, the source byte it came from.
fn to_strict_json(src: &str) -> (String, Vec<usize>) {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut map = Vec::with_capacity(src.len());
    let mut i = 0;
    let push_str = |out: &mut String, map: &mut Vec<usize>, s: &str, from: usize| {
        out.push_str(s);
        map.extend(std::iter::repeat_n(from, s.len()));
    };
    while i < b.len() {
        if src[i..].starts_with("\"\"\"") {
            let body_start = i + 3;
            match src[body_start..].find("\"\"\"") {
                Some(n) => {
                    let s = &src[body_start..body_start + n];
                    let json = serde_json::to_string(s).expect("strings serialize");
                    push_str(&mut out, &mut map, &json, i);
                    i = body_start + n + 3;
                }
                None => {
                    // Unclosed: let the JSON parser report it at the end.
                    push_str(&mut out, &mut map, "\"", i);
                    i = b.len();
                }
            }
        } else if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(b.len());
            for (k, ch) in src[i..end].char_indices() {
                let mut tmp = [0u8; 4];
                push_str(&mut out, &mut map, ch.encode_utf8(&mut tmp), i + k);
            }
            i = end;
        } else if src[i..].starts_with("//") || b[i] == b'#' {
            i = src[i..].find('\n').map_or(b.len(), |n| i + n);
        } else if src[i..].starts_with("/*") {
            i = src[i + 2..].find("*/").map_or(b.len(), |n| i + 2 + n + 2);
        } else {
            let Some(ch) = src[i..].chars().next() else {
                break;
            };
            let mut tmp = [0u8; 4];
            push_str(&mut out, &mut map, ch.encode_utf8(&mut tmp), i);
            i += ch.len_utf8();
        }
    }
    (out, map)
}

fn parse_body(src: &str, base: usize) -> Result<Option<serde_json::Value>, ShellError> {
    let (json, map) = to_strict_json(src);
    let mut values = Vec::new();
    for v in serde_json::Deserializer::from_str(&json).into_iter::<serde_json::Value>() {
        match v {
            Ok(v) => values.push(v),
            Err(e) => {
                let line_start: usize = json
                    .split_inclusive('\n')
                    .take(e.line().saturating_sub(1))
                    .map(str::len)
                    .sum();
                let off = (line_start + e.column().saturating_sub(1)).min(json.len());
                let pos = map.get(off).copied().unwrap_or(src.trim_end().len());
                let text = e.to_string();
                let message = text
                    .rfind(" at line ")
                    .map_or(text.as_str(), |n| &text[..n]);
                return Err(ShellError::new(
                    format!("invalid JSON in the request body: {message}"),
                    base + pos,
                ));
            }
        }
    }
    Ok(match values.len() {
        0 => None,
        1 => values.pop(),
        _ => Some(serde_json::Value::Array(values)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndjson_only_for_bulk_and_msearch() {
        let reqs =
            parse_es_console("POST _bulk\n{\"a\":1}\n{\"b\":2}\nPOST x/_search\n{}").unwrap();
        assert!(reqs[0].1.is_ndjson());
        assert!(!reqs[1].1.is_ndjson());
    }

    #[test]
    fn blank_lines_inside_a_body_do_not_end_it() {
        let reqs = parse_es_console("PUT idx\n{\n\n  \"settings\": {}\n\n}\n\nGET idx").unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(
            reqs[0].1,
            EsRequest::Http {
                method: "PUT".into(),
                path: "/idx".into(),
                body: Some(serde_json::json!({"settings": {}}))
            }
        );
    }

    #[test]
    fn extra_text_after_the_path_is_an_error() {
        let e = parse_es_console("GET x/_search {\"a\": 1}").unwrap_err();
        assert_eq!(e.position, 14);
    }
}
