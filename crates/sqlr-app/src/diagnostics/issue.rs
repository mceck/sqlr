//! The pre-filled GitHub issue offered from a reviewed diagnostics bundle.
//!
//! sqlr never submits anything: it opens the browser's new-issue page with a title and a
//! body and the person decides whether to send it. GitHub rejects long request lines, so a
//! bundle that does not fit goes to the clipboard and the body asks for one paste.

use serde_json::Value;

/// GitHub's new-issue page, prefilled through `title` and `body`.
pub const NEW_ISSUE: &str = "https://github.com/mceck/sqlr/issues/new";
/// The whole URL stays well below the request line GitHub accepts (about 8 KiB).
pub const URL_BUDGET: usize = 6_000;
const TITLE_CHARS: usize = 80;
const MESSAGE_CHARS: usize = 300;
const FIELD_CHARS: usize = 100;
const FIELD_COUNT: usize = 4;
const LOG_LINES: usize = 6;
const LOG_CHARS: usize = 600;

/// A new issue ready to open in the browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueDraft {
    pub title: String,
    pub body: String,
    pub url: String,
    /// The bundle did not fit in the URL: the caller copies it for one paste.
    pub paste_required: bool,
}

/// Builds the issue from the bundle text, which is already sanitized.
pub fn draft(bundle: &str) -> IssueDraft {
    let value = serde_json::from_str::<Value>(bundle).unwrap_or(Value::Null);
    let title = title(&value);
    let summary = summary(&value);

    let inline_body = format!(
        "{summary}\n\n<details>\n<summary>Diagnostics bundle</summary>\n\n{}\n\n</details>\n",
        code_block("json", bundle)
    );
    // Percent-encoding never shrinks the body, so a bundle over the budget is never
    // encoded: most bundles take this path and are several hundred kilobytes.
    if inline_body.len() <= URL_BUDGET {
        let inline_url = new_issue_url(&title, &inline_body);
        if inline_url.len() <= URL_BUDGET {
            return IssueDraft {
                title,
                body: inline_body,
                url: inline_url,
                paste_required: false,
            };
        }
    }

    let marker = "### Diagnostics\n\n<!-- sqlr copied the full diagnostics bundle to the clipboard: paste it here before submitting. -->\n";
    let body = format!("{summary}\n\n{marker}");
    let url = new_issue_url(&title, &body);
    if url.len() <= URL_BUDGET {
        return IssueDraft {
            title,
            body,
            url,
            paste_required: true,
        };
    }

    // A pathological summary must still produce a URL that opens.
    let body = format!(
        "{}\n\n{marker}",
        truncate_chars(description(&value), MESSAGE_CHARS)
    );
    let url = new_issue_url(&title, &body);
    IssueDraft {
        title,
        body,
        url,
        paste_required: true,
    }
}

/// The issue page for `title` and `body`, percent-encoded for a query string.
pub fn new_issue_url(title: &str, body: &str) -> String {
    format!("{NEW_ISSUE}?title={}&body={}", encode(title), encode(body))
}

fn encode(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn title(value: &Value) -> String {
    let prefix = match selected(value)
        .map(|incident| text(incident, "kind"))
        .unwrap_or("")
    {
        "panic" | "engine_panic" => "Crash",
        "startup" => "Startup failure",
        "unclean_shutdown" => "Unclean shutdown",
        "sidecar_crash" => "Sidecar crash",
        _ => "Bug report",
    };
    let first = selected(value)
        .and_then(|incident| opt(incident, "message"))
        .or_else(|| {
            value
                .pointer("/incidents/recent/0/message")
                .and_then(Value::as_str)
        })
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim();
    let first = truncate_chars(first, TITLE_CHARS);
    if first.is_empty() {
        match value
            .pointer("/application/version")
            .and_then(Value::as_str)
        {
            Some(version) if !version.is_empty() => format!("{prefix} in sqlr {version}"),
            _ => prefix.to_string(),
        }
    } else {
        format!("{prefix}: {first}")
    }
}

fn description(value: &Value) -> &str {
    selected(value)
        .and_then(|incident| opt(incident, "note"))
        .or_else(|| selected(value).and_then(|incident| opt(incident, "message")))
        .or_else(|| {
            value
                .pointer("/incidents/recent/0/message")
                .and_then(Value::as_str)
        })
        .unwrap_or("")
}

fn summary(value: &Value) -> String {
    let application = value.get("application");
    let environment = value.get("environment");
    let mut out = String::from("### What happened\n\n");
    let description = description(value);
    if description.is_empty() {
        out.push_str("The report carries no description.");
    } else {
        out.push_str(&truncate_chars(description, MESSAGE_CHARS));
    }
    out.push_str("\n\n### Environment\n\n");

    let mut build = format!(
        "sqlr {}",
        text(application.unwrap_or(&Value::Null), "version")
    );
    let profile = text(application.unwrap_or(&Value::Null), "profile");
    let target = text(application.unwrap_or(&Value::Null), "target");
    if !profile.is_empty() {
        build.push_str(" · ");
        build.push_str(profile);
    }
    if !target.is_empty() {
        build.push_str(" · ");
        build.push_str(target);
    }
    out.push_str(&format!("- {build}\n"));

    let os = text(environment.unwrap_or(&Value::Null), "os");
    let os_version = text(environment.unwrap_or(&Value::Null), "os_version");
    let arch = text(environment.unwrap_or(&Value::Null), "arch");
    out.push_str(&format!("- {os}"));
    if !os_version.is_empty() {
        out.push(' ');
        out.push_str(os_version);
    }
    out.push_str(&format!(" · {arch}\n"));

    if let Some(graphics) = application.and_then(|app| opt(app, "graphics_backend")) {
        out.push_str(&format!("- Graphics: {graphics}\n"));
    }
    let commit = application
        .and_then(|app| opt(app, "commit"))
        .or_else(|| environment.and_then(|env| opt(env, "commit")));
    if let Some(commit) = commit {
        out.push_str(&format!("- Commit: {commit}\n"));
    }
    if let Some(session) = value.pointer("/session/id").and_then(Value::as_str) {
        out.push_str(&format!("- Session: `{session}`\n"));
    }

    let Some(incident) = selected(value) else {
        return out;
    };
    out.push_str("\n### Incident\n\n");
    out.push_str(&format!("- Kind: {}\n", text(incident, "kind")));
    if let Some(at) = incident.get("at_ms").and_then(Value::as_i64) {
        out.push_str(&format!("- When: {}\n", sqlr_ui::format_when(at)));
    }
    if let Some(thread) = opt(incident, "thread") {
        out.push_str(&format!("- Thread: {thread}\n"));
    }
    if let Some(file) = opt(incident, "file") {
        let line = incident
            .get("line")
            .and_then(Value::as_u64)
            .map(|line| format!(":{line}"))
            .unwrap_or_default();
        out.push_str(&format!("- Location: {file}{line}\n"));
    }
    if let Some(fields) = incident.get("fields").and_then(Value::as_object) {
        for (key, value) in fields.iter().take(FIELD_COUNT) {
            out.push_str(&format!(
                "- `{key}`: `{}`\n",
                truncate_chars(&scalar(value), FIELD_CHARS)
            ));
        }
        if fields.len() > FIELD_COUNT {
            out.push_str(&format!(
                "- … {} more field(s) in the bundle\n",
                fields.len() - FIELD_COUNT
            ));
        }
    }
    if let Some(logs) = incident.get("logs").and_then(Value::as_array) {
        let mut excerpt = String::new();
        for line in logs
            .iter()
            .rev()
            .take(LOG_LINES)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .filter_map(Value::as_str)
        {
            excerpt.push_str(line);
            excerpt.push('\n');
        }
        if !excerpt.trim().is_empty() {
            out.push_str(&format!(
                "\n<details>\n<summary>Log excerpt</summary>\n\n{}\n\n</details>\n",
                code_block("text", &tail_chars(&excerpt, LOG_CHARS))
            ));
        }
    }
    out
}

fn selected(value: &Value) -> Option<&Value> {
    value.pointer("/incidents/selected/incident")
}

fn text<'a>(object: &'a Value, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or("")
}

fn opt<'a>(object: &'a Value, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
}

/// A fenced block whose fence is longer than any backtick run in `text`.
fn code_block(language: &str, text: &str) -> String {
    let mut fence = String::from("```");
    while text.contains(&fence) {
        fence.push('`');
    }
    format!("{fence}{language}\n{text}\n{fence}")
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let mut out = String::from("…");
    out.extend(text.chars().skip(count - max));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bundle(kind: &str, note: Option<&str>, logs: Vec<Value>) -> String {
        let mut incident = json!({
            "schema": 1,
            "id": "i-1",
            "session_id": "s-1",
            "at_ms": 1_700_000_000_000i64,
            "kind": kind,
            "message": "assertion failed: the plan is empty",
            "thread": "main",
            "file": "src/planner.rs",
            "line": 77,
            "environment": {"version": "0.1.0", "os": "macos", "arch": "aarch64"},
            "fields": {"conn": "c1", "stmt": "select 1"},
            "logs": ["line one", "line two"]
        });
        if let Some(note) = note {
            incident["note"] = json!(note);
        }
        serde_json::to_string(&json!({
            "schema": 1,
            "generated_at_ms": 1,
            "session": {
                "id": "s-1",
                "started_ms": 1,
                "uptime_s": 60,
                "pid": 42,
                "version": "0.1.0",
                "other_instance": false
            },
            "application": {
                "version": "0.1.0",
                "profile": "release",
                "target": "aarch64-apple-darwin",
                "commit": "abc1234",
                "graphics_backend": "metal"
            },
            "environment": {
                "version": "0.1.0",
                "os": "macos",
                "os_version": "15.3",
                "arch": "aarch64"
            },
            "drivers": [],
            "settings": {"values": [], "omitted": 0},
            "connections": [],
            "interface": {
                "open_tabs": {"open": [], "titles_included": true, "truncated": false},
                "open_transactions": [],
                "panels": []
            },
            "operations": {"entries": [], "truncated": false},
            "logs": {"lines": logs, "truncated": false},
            "incidents": {
                "selected": {"incident": incident, "logs_truncated": false},
                "selected_logs_truncated": false,
                "recent": [],
                "recent_truncated": false
            }
        }))
        .unwrap()
    }

    #[test]
    fn a_small_bundle_rides_inside_the_issue_body() {
        let draft = draft(&bundle("panic", None, Vec::new()));
        assert!(!draft.paste_required);
        assert!(draft.body.contains("\"session\""));
        assert_eq!(draft.title, "Crash: assertion failed: the plan is empty");
        assert!(draft.url.starts_with(NEW_ISSUE));
        assert!(draft.url.len() <= URL_BUDGET);
    }

    #[test]
    fn a_large_bundle_is_copied_instead_and_the_issue_asks_for_a_paste() {
        let logs = (0..2_000)
            .map(|i| {
                json!(format!(
                    "2026-01-01T00:00:00Z INFO line {i} with some padding text"
                ))
            })
            .collect();
        let draft = draft(&bundle("panic", None, logs));
        assert!(draft.paste_required);
        assert!(!draft.body.contains("line 1999"));
        assert!(draft.body.contains("copied the full diagnostics bundle"));
        assert!(draft.url.len() <= URL_BUDGET);
    }

    #[test]
    fn a_manual_report_is_named_after_its_first_line() {
        let draft = draft(&bundle(
            "manual",
            Some("the migration stopped\nhalf way"),
            vec![],
        ));
        assert_eq!(
            draft.title,
            "Bug report: assertion failed: the plan is empty"
        );
        assert!(draft.body.contains("the migration stopped"));
    }

    #[test]
    fn backticks_in_the_bundle_use_a_longer_fence() {
        let bundle = bundle("panic", None, Vec::new()).replace("line two", "``` inside a log");
        let draft = draft(&bundle);
        assert!(draft.body.contains("````json"));
        assert!(draft.body.contains("````\n\n</details>"));
    }

    #[test]
    fn the_url_encodes_reserved_characters() {
        let url = new_issue_url("Crash: no & idea", "a b\nc");
        assert!(url.contains("Crash%3A%20no%20%26%20idea"));
        assert!(url.contains("a%20b%0Ac"));
        assert!(!url.contains(' '));
        assert!(!url.contains('\n'));
    }
}
