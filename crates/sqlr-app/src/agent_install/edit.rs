//! Format-preserving edits of an agent's configuration text: sqlr's entry is read, written
//! and removed, and every other byte of meaning stays as the person had it.
//!
//! JSON and JSONC go through `jsonc-parser`'s concrete syntax tree, which keeps comments,
//! whitespace, order and trailing commas. TOML goes through `toml_edit`, which keeps
//! comments, formatting and order. Both only touch the `sqlr` entry and the keys inside it.

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use serde_json::Value;

use super::{AgentKind, Endpoint};

/// The name of sqlr's entry in every agent's server map.
pub(super) const ENTRY: &str = "sqlr";

/// Where an agent keeps its servers and what its entry looks like.
#[derive(Clone, Copy, Debug)]
pub(super) enum Format {
    Json {
        /// The top-level map of servers (`mcpServers`, `servers`, `mcp`).
        container: &'static str,
        /// The key that holds the URL (`url`, `httpUrl`).
        url_key: &'static str,
        /// The transport `type` the agent needs, when it needs one.
        kind: Option<&'static str>,
    },
    /// Codex: `[mcp_servers.sqlr]` with `url` and `http_headers`.
    Toml,
}

/// What the configuration says about sqlr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Entry {
    Absent,
    /// The entry exists; `current` says whether it matches the expected endpoint.
    Present {
        current: bool,
    },
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// Reads sqlr's entry. `Err` carries the Manual setup reason, which never quotes the file.
pub(super) fn read_entry(format: Format, text: &str, endpoint: &Endpoint) -> Result<Entry, String> {
    match format {
        Format::Json {
            container,
            url_key,
            kind,
        } => {
            let root = parse_json(text)?;
            let Some(value) = root.value() else {
                return Ok(Entry::Absent);
            };
            let Some(object) = value.as_object() else {
                return Err("the file is not a JSON object".into());
            };
            let Some(servers) = object.get(container) else {
                return Ok(Entry::Absent);
            };
            let Some(servers) = servers.object_value() else {
                return Err(format!("`{container}` is not an object"));
            };
            let Some(entry) = servers.get(ENTRY) else {
                return Ok(Entry::Absent);
            };
            let entry = entry.value().and_then(|value| value.to_serde_value());
            Ok(Entry::Present {
                current: entry.is_some_and(|entry| json_matches(&entry, url_key, kind, endpoint)),
            })
        }
        Format::Toml => {
            let doc = parse_toml(text)?;
            let Some(servers) = doc.get("mcp_servers") else {
                return Ok(Entry::Absent);
            };
            let Some(servers) = servers.as_table_like() else {
                return Err("`mcp_servers` is not a table".into());
            };
            let Some(entry) = servers.get(ENTRY) else {
                return Ok(Entry::Absent);
            };
            let current = entry.as_table_like().is_some_and(|entry| {
                let url = entry.get("url").and_then(|url| url.as_str());
                let auth = entry
                    .get("http_headers")
                    .and_then(|headers| headers.as_table_like())
                    .and_then(|headers| headers.get("Authorization"))
                    .and_then(|auth| auth.as_str());
                url == Some(endpoint.url.as_str()) && token_matches(auth, endpoint)
            });
            Ok(Entry::Present { current })
        }
    }
}

fn json_matches(entry: &Value, url_key: &str, kind: Option<&str>, endpoint: &Endpoint) -> bool {
    let url = entry.get(url_key).and_then(Value::as_str);
    let auth = entry
        .get("headers")
        .and_then(|headers| headers.get("Authorization"))
        .and_then(Value::as_str);
    let kind_ok = kind.is_none_or(|kind| entry.get("type").and_then(Value::as_str) == Some(kind));
    kind_ok && url == Some(endpoint.url.as_str()) && token_matches(auth, endpoint)
}

/// Without a running server the token is unknown: only the URL is compared.
fn token_matches(auth: Option<&str>, endpoint: &Endpoint) -> bool {
    match endpoint.token() {
        Some(token) => auth == Some(bearer(token).as_str()),
        None => auth.is_some(),
    }
}

/// Writes sqlr's entry, creating the server map when needed, and returns the new text.
pub(super) fn install(
    format: Format,
    text: &str,
    url: &str,
    token: &str,
) -> Result<String, String> {
    match format {
        Format::Json {
            container,
            url_key,
            kind,
        } => {
            let root = parse_json(text)?;
            let object = match root.value() {
                None => root.object_value_or_set(),
                Some(value) => value
                    .as_object()
                    .ok_or_else(|| "the file is not a JSON object".to_string())?,
            };
            let servers = object
                .object_value_or_create(container)
                .ok_or_else(|| format!("`{container}` is not an object"))?;
            match servers.get(ENTRY).and_then(|prop| prop.object_value()) {
                // Update in place, so keys the person added to sqlr's entry stay.
                Some(entry) => {
                    if let Some(kind) = kind {
                        set_prop(&entry, "type", kind.into());
                    }
                    set_prop(&entry, url_key, url.into());
                    match entry.object_value_or_create("headers") {
                        Some(headers) => set_prop(&headers, "Authorization", bearer(token).into()),
                        None => set_prop(&entry, "headers", json_headers(token)),
                    }
                }
                None => {
                    let mut fields = Vec::new();
                    if let Some(kind) = kind {
                        fields.push(("type".to_string(), CstInputValue::from(kind)));
                    }
                    fields.push((url_key.to_string(), url.into()));
                    fields.push(("headers".to_string(), json_headers(token)));
                    set_prop(&servers, ENTRY, CstInputValue::Object(fields));
                }
            }
            Ok(with_final_newline(text, root.to_string()))
        }
        Format::Toml => {
            let mut doc = parse_toml(text)?;
            let servers = doc.entry("mcp_servers").or_insert_with(|| {
                let mut table = toml_edit::Table::new();
                table.set_implicit(true);
                toml_edit::Item::Table(table)
            });
            let servers = servers
                .as_table_like_mut()
                .ok_or_else(|| "`mcp_servers` is not a table".to_string())?;
            match servers
                .get_mut(ENTRY)
                .and_then(|entry| entry.as_table_like_mut())
            {
                Some(entry) => {
                    entry.insert("url", toml_edit::value(url));
                    match entry
                        .get_mut("http_headers")
                        .and_then(|headers| headers.as_table_like_mut())
                    {
                        Some(headers) => {
                            headers.insert("Authorization", toml_edit::value(bearer(token)));
                        }
                        None => {
                            entry.insert("http_headers", toml_headers(token));
                        }
                    }
                }
                None => {
                    let mut entry = toml_edit::Table::new();
                    entry.insert("url", toml_edit::value(url));
                    entry.insert("http_headers", toml_headers(token));
                    servers.insert(ENTRY, toml_edit::Item::Table(entry));
                }
            }
            Ok(with_final_newline(text, doc.to_string()))
        }
    }
}

/// Removes sqlr's entry and leaves the server map in place. `None` when there is none.
pub(super) fn remove(format: Format, text: &str) -> Result<Option<String>, String> {
    match format {
        Format::Json { container, .. } => {
            let root = parse_json(text)?;
            let Some(value) = root.value() else {
                return Ok(None);
            };
            let entry = value
                .as_object()
                .and_then(|object| object.object_value(container))
                .and_then(|servers| servers.get(ENTRY));
            let Some(entry) = entry else {
                return Ok(None);
            };
            entry.remove();
            Ok(Some(root.to_string()))
        }
        Format::Toml => {
            let mut doc = parse_toml(text)?;
            let removed = doc
                .get_mut("mcp_servers")
                .and_then(|servers| servers.as_table_like_mut())
                .and_then(|servers| servers.remove(ENTRY));
            Ok(removed.map(|_| doc.to_string()))
        }
    }
}

/// The agent's own format for a person to paste by hand (Manual setup's Copy config).
pub(super) fn snippet(format: Format, url: &str, token: &str) -> String {
    match format {
        Format::Json {
            container,
            url_key,
            kind,
        } => {
            let mut entry = serde_json::Map::new();
            if let Some(kind) = kind {
                entry.insert("type".into(), kind.into());
            }
            entry.insert(url_key.into(), url.into());
            entry.insert(
                "headers".into(),
                serde_json::json!({ "Authorization": bearer(token) }),
            );
            let config = serde_json::json!({ container: { ENTRY: Value::Object(entry) } });
            serde_json::to_string_pretty(&config).unwrap_or_default()
        }
        Format::Toml => install(Format::Toml, "", url, token).unwrap_or_default(),
    }
}

fn set_prop(object: &CstObject, key: &str, value: CstInputValue) {
    match object.get(key) {
        Some(prop) => prop.set_value(value),
        None => {
            object.append(key, value);
        }
    }
}

fn json_headers(token: &str) -> CstInputValue {
    CstInputValue::Object(vec![("Authorization".into(), bearer(token).into())])
}

fn toml_headers(token: &str) -> toml_edit::Item {
    let mut headers = toml_edit::InlineTable::new();
    headers.insert("Authorization", bearer(token).into());
    toml_edit::value(headers)
}

fn parse_json(text: &str) -> Result<CstRootNode, String> {
    CstRootNode::parse(text, &ParseOptions::default()).map_err(|error| {
        // The message names the problem and its position, never the file's text.
        format!("the file is not valid JSON: {error}")
    })
}

fn parse_toml(text: &str) -> Result<toml_edit::DocumentMut, String> {
    text.parse::<toml_edit::DocumentMut>().map_err(|error| {
        // `Display` would quote the offending line, which may hold a secret.
        let line = error.span().map(|span| {
            text.get(..span.start)
                .unwrap_or(text)
                .lines()
                .count()
                .max(1)
        });
        match line {
            Some(line) => format!(
                "the file is not valid TOML: {} on line {line}",
                error.message().trim_end()
            ),
            None => format!("the file is not valid TOML: {}", error.message().trim_end()),
        }
    })
}

/// A file sqlr creates, or one that ended with a newline, ends with one.
fn with_final_newline(original: &str, mut text: String) -> String {
    if (original.trim().is_empty() || original.ends_with('\n')) && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

impl AgentKind {
    pub(super) fn format(self) -> Format {
        match self {
            Self::ClaudeCode => Format::Json {
                container: "mcpServers",
                url_key: "url",
                kind: Some("http"),
            },
            Self::CodexCli => Format::Toml,
            Self::GeminiCli => Format::Json {
                container: "mcpServers",
                url_key: "httpUrl",
                kind: None,
            },
            Self::Cursor => Format::Json {
                container: "mcpServers",
                url_key: "url",
                kind: None,
            },
            Self::VsCode => Format::Json {
                container: "servers",
                url_key: "url",
                kind: Some("http"),
            },
            Self::OpenCode => Format::Json {
                container: "mcp",
                url_key: "url",
                kind: Some("remote"),
            },
        }
    }
}
