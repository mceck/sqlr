//! Bundle composition: the JSON document the Diagnostics window copies or saves.
//!
//! The caller collects cheap snapshots on the interface thread; this module sanitizes,
//! shortens paths, applies the SQL and title choices, bounds every section and enforces the
//! 512 KiB total.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::diagnostics::model::{
    BUNDLE_SCHEMA, BundleIncident, BundleInputs, BundleOutline, Environment, Incident,
    IncidentSummary, SessionSummary,
};
use crate::diagnostics::ring::RingSnapshot;
use crate::diagnostics::store::now_ms;

/// The whole bundle never exceeds this many bytes.
pub const BUNDLE_BYTES: usize = 512 * 1024;
/// Log lines embedded in the bundle's own section.
pub const LOG_BYTES: usize = 128 * 1024;
/// Log lines embedded inside the selected incident.
pub const INCIDENT_LOG_BYTES: usize = 64 * 1024;
/// The last operations a bundle carries.
pub const OPERATIONS: usize = 50;
/// Open tabs a bundle carries.
pub const TABS: usize = 100;
/// Recent incident summaries a bundle carries.
pub const RECENT_INCIDENTS: usize = 20;
/// A single string is capped, so one huge statement or path cannot dominate.
pub const STRING_BYTES: usize = 4 * 1024;
/// Override values are smaller than statements.
const SETTING_BYTES: usize = 1024;

/// Everything the renderer reads besides the inputs.
pub struct BundleContext<'a> {
    pub session: &'a SessionSummary,
    pub environment: &'a Environment,
    pub ring: &'a RingSnapshot,
    pub recent: &'a [IncidentSummary],
    pub data_dir: Option<&'a Path>,
    pub home: Option<PathBuf>,
}

/// Renders the bundle and an outline of what it contains.
pub fn render(inputs: &BundleInputs, context: &BundleContext<'_>) -> (String, BundleOutline) {
    let mut outline = BundleOutline::default();
    let mut value = build(inputs, context, &mut outline);
    if !inputs.include_sql {
        strip_sql(&mut value);
    }
    shorten_paths(&mut value, context);
    for sentinel in &inputs.redact {
        if !sentinel.is_empty() {
            walk_strings(&mut value, &mut |text| {
                *text = text.replace(sentinel, "<redacted>")
            });
        }
    }
    truncate_strings(&mut value, STRING_BYTES);
    let mut text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into());
    let mut round = 0;
    while text.len() > BUNDLE_BYTES && round < 128 {
        shrink(&mut value, round, &mut outline);
        value["truncation"] = json!(outline.truncated);
        text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into());
        round += 1;
    }
    if text.len() > BUNDLE_BYTES {
        // Last resort: keep the document valid and small, and say what went.
        let mut minimal = Map::new();
        for key in [
            "schema",
            "generated_at_ms",
            "session",
            "application",
            "environment",
        ] {
            minimal.insert(key.into(), value[key].clone());
        }
        for key in [
            "drivers",
            "settings",
            "connections",
            "interface",
            "operations",
            "logs",
            "incidents",
        ] {
            minimal.insert(key.into(), json!({"truncated": true, "note": format!("{key} omitted to stay within the bundle size limit")}));
        }
        minimal.insert(
            "truncation".into(),
            json!(["Large sections omitted to stay within the bundle size limit"]),
        );
        text = serde_json::to_string_pretty(&minimal).unwrap_or_default();
        outline.truncated.push("large sections omitted".into());
    }
    outline.tabs = value
        .pointer("/interface/open_tabs/open")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    outline.operations = value
        .pointer("/operations/entries")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    outline.log_lines = value
        .pointer("/logs/lines")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    outline.bytes = text.len();
    (text, outline)
}

fn build(inputs: &BundleInputs, context: &BundleContext<'_>, outline: &mut BundleOutline) -> Value {
    let mut settings = Vec::new();
    let mut settings_omitted = 0;
    for (level, id, value) in &inputs.settings {
        if sensitive_setting(id) {
            settings_omitted += 1;
            continue;
        }
        let mut value = value.clone();
        truncate_strings(&mut value, SETTING_BYTES);
        settings.push(json!({
            "level": level,
            "id": id,
            "value": value,
        }));
    }
    if settings_omitted > 0 {
        outline
            .truncated
            .push(format!("{settings_omitted} sensitive setting(s) omitted"));
    }

    if settings.len() > 200 {
        outline
            .truncated
            .push(format!("{} setting(s) omitted", settings.len() - 200));
        settings.truncate(200);
    }
    let mut connections = inputs.connections.clone();
    if connections.len() > 200 {
        connections.truncate(200);
        outline.truncated.push(format!(
            "{} connection(s) omitted",
            inputs.connections.len() - 200
        ));
    }

    let mut drivers = inputs.drivers.clone();
    if drivers.len() > 200 {
        drivers.truncate(200);
        outline
            .truncated
            .push(format!("{} driver(s) omitted", inputs.drivers.len() - 200));
    }

    // Interface: titles are a per-report choice; everything else is always included.
    let mut tabs = inputs.interface.open_tabs.clone();
    let tab_count = tabs.len();
    let tabs_truncated = tab_count > TABS;
    if tabs_truncated {
        tabs.truncate(TABS);
        outline
            .truncated
            .push(format!("{} tab(s) omitted", tab_count - TABS));
    }
    let titles_included = inputs.include_titles;
    if !titles_included {
        for tab in &mut tabs {
            tab.title = "<title omitted>".into();
        }
    }
    outline.tabs = tabs.len();
    let tabs_value = json!({
        "open": tabs,
        "titles_included": titles_included,
        "truncated": tabs_truncated,
        "note": tabs_truncated.then(|| format!("{} tab(s) omitted; the first {TABS} are listed", tab_count - TABS)),
    });

    // Operations.
    let mut operations = inputs.operations.clone();
    if !inputs.include_sql {
        for operation in &mut operations {
            if matches!(
                operation.kind.to_ascii_lowercase().as_str(),
                "query" | "fetch"
            ) {
                operation.label = "<SQL omitted>".into();
            }
        }
    }
    let operation_count = operations.len();
    let operations_truncated = operation_count > OPERATIONS;
    if operations_truncated {
        operations.truncate(OPERATIONS);
        outline.truncated.push(format!(
            "{} operation(s) omitted",
            operation_count - OPERATIONS
        ));
    }
    outline.operations = operations.len();

    // The bundle's own log section: newest lines that fit the byte budget.
    let all_logs = context.ring.excerpt();
    let (logs, log_omitted) = tail_by_bytes(&all_logs, LOG_BYTES);
    if log_omitted > 0 {
        outline
            .truncated
            .push(format!("{log_omitted} log line(s) omitted"));
    }
    outline.log_lines = logs.len();

    // Incidents: the selected one in full (its log excerpt capped) and recent summaries.
    let selected = inputs.incident.as_ref().map(|incident| {
        let (incident, logs_truncated) = cap_incident(incident.clone());
        json!(BundleIncident {
            incident,
            logs_truncated,
        })
    });
    let mut recent = context.recent.to_vec();
    let recent_count = recent.len();
    let recent_truncated = recent_count > RECENT_INCIDENTS;
    if recent_truncated {
        recent.truncate(RECENT_INCIDENTS);
        outline.truncated.push(format!(
            "{} incident summary omitted",
            recent_count - RECENT_INCIDENTS
        ));
    }
    outline.incidents = recent.len() + usize::from(inputs.incident.is_some());
    let selected_logs_truncated = inputs
        .incident
        .as_ref()
        .is_some_and(|i| i.logs.len() > incident_log_cap(&i.logs));

    json!({
        "truncation": outline.truncated,
        "schema": BUNDLE_SCHEMA,
        "generated_at_ms": now_ms(),
        "session": context.session,
        "application": inputs.application,
        "environment": context.environment,
        "drivers": drivers,
        "settings": {
            "values": settings,
            "omitted": settings_omitted,
        },
        "connections": connections,
        "interface": {
            "open_tabs": tabs_value,
            "open_transactions": inputs.interface.open_transactions,
            "panels": inputs.interface.panels,
        },
        "operations": {
            "entries": operations,
            "truncated": operations_truncated,
            "note": operations_truncated.then(|| format!("{} operation(s) omitted; the newest {OPERATIONS} are listed", operation_count - OPERATIONS)),
        },
        "logs": {
            "lines": logs,
            "truncated": log_omitted > 0,
            "note": (log_omitted > 0).then(|| format!("{log_omitted} log line(s) omitted; the newest are kept")),
        },
        "incidents": {
            "selected": selected,
            "selected_logs_truncated": selected_logs_truncated,
            "recent": recent,
            "recent_truncated": recent_truncated,
            "note": recent_truncated.then(|| format!("{} incident summary omitted; the newest {RECENT_INCIDENTS} are listed", recent_count - RECENT_INCIDENTS)),
        },
    })
}

/// The selected incident, with its log excerpt capped to [`INCIDENT_LOG_BYTES`].
fn cap_incident(mut incident: Incident) -> (Incident, bool) {
    let cap = incident_log_cap(&incident.logs);
    if incident.logs.len() <= cap {
        return (incident, false);
    }
    let omitted = incident.logs.len() - cap;
    let logs: Vec<String> = incident.logs.split_off(incident.logs.len() - cap);
    incident.logs = logs;
    incident.logs.push(format!(
        "…{omitted} incident log line(s) omitted from the bundle…"
    ));
    (incident, true)
}

fn incident_log_cap(lines: &[String]) -> usize {
    let mut total = 0;
    let mut keep = 0;
    for line in lines.iter().rev() {
        if total + line.len() > INCIDENT_LOG_BYTES && keep > 0 {
            break;
        }
        total += line.len();
        keep += 1;
    }
    keep
}

/// The newest lines that fit `budget` bytes, and how many were left out.
fn tail_by_bytes(lines: &[String], budget: usize) -> (Vec<String>, usize) {
    let mut total = 0;
    let mut start = lines.len();
    for (index, line) in lines.iter().enumerate().rev() {
        if total + line.len() > budget && start < lines.len() {
            break;
        }
        total += line.len();
        start = index;
    }
    let omitted = start;
    (lines[start..].to_vec(), omitted)
}

fn sensitive_setting(id: &str) -> bool {
    let id = id.to_lowercase();
    // The terminal command can embed credentials; it is never logged or reported.
    id == "terminal.shell"
        || id.contains("password")
        || id.contains("passphrase")
        || id.contains("secret")
        || id.contains("token")
}

/// Shortens every string under the home and data directories.
fn shorten_paths(value: &mut Value, context: &BundleContext<'_>) {
    let mut replacements: Vec<(String, &str)> = Vec::new();
    if let Some(home) = &context.home {
        replacements.push((home.to_string_lossy().into_owned(), "<home>"));
    }
    if let Some(data) = context.data_dir {
        // Data dir first: it may itself be under the home directory.
        replacements.insert(0, (data.to_string_lossy().into_owned(), "<data-dir>"));
    }
    replacements.retain(|(from, _)| !from.is_empty());
    walk_strings(value, &mut |text| {
        for (from, to) in &replacements {
            *text = text.replace(from.as_str(), to);
        }
    });
}

/// Applies `f` to every string in the JSON document.
pub fn walk_strings(value: &mut Value, f: &mut impl FnMut(&mut String)) {
    match value {
        Value::String(text) => f(text),
        Value::Array(items) => {
            for item in items {
                walk_strings(item, f);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                walk_strings(item, f);
            }
        }
        _ => {}
    }
}

/// Caps every string at `bytes`, at a character boundary, with an explicit marker.
fn truncate_strings(value: &mut Value, bytes: usize) {
    walk_strings(value, &mut |text| {
        if text.len() > bytes {
            let mut cut = bytes;
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("… (truncated)");
        }
    });
}

/// Removes every `sql` field, at any depth. One setting controls logs and reports alike.
fn strip_sql(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                strip_sql(item);
            }
        }
        Value::Object(map) => {
            map.remove("sql");
            for item in map.values_mut() {
                strip_sql(item);
            }
        }
        Value::String(text) => *text = without_sql_log(text),
        _ => {}
    }
}

/// Capture always appends SQL last; remove it from historical ring excerpts as well.
pub(super) fn without_sql_log(line: &str) -> String {
    line.split_once(" sql=")
        .map_or(line, |(prefix, _)| prefix)
        .to_string()
}

/// Reduces oversized sections one at a time, in the order that keeps the report useful
/// longest: logs first, then the selected incident's logs, then operations, tabs and
/// summaries.
fn shrink(value: &mut Value, round: usize, outline: &mut BundleOutline) {
    if let Some(logs) = value
        .get_mut("logs")
        .and_then(|l| l.get_mut("lines"))
        .and_then(Value::as_array_mut)
        && !logs.is_empty()
    {
        let drop = logs.len().div_ceil(2);
        logs.drain(..drop);
        if let Some(logs) = value.get_mut("logs") {
            logs["truncated"] = json!(true);
            logs["note"] = json!(format!(
                "{} more log line(s) omitted to stay within the bundle size limit",
                drop
            ));
        }
        outline
            .truncated
            .push(format!("{drop} log line(s) omitted"));
        return;
    }
    if let Some(selected) = value
        .get_mut("incidents")
        .and_then(|i| i.get_mut("selected"))
        .and_then(|s| s.get_mut("incident"))
        .and_then(|i| i.get_mut("logs"))
        .and_then(Value::as_array_mut)
        && !selected.is_empty()
    {
        let drop = selected.len().div_ceil(2);
        selected.drain(..drop);
        outline
            .truncated
            .push(format!("{drop} incident log line(s) omitted"));
        return;
    }
    let (key, name) = match round % 3 {
        0 => ("operations", "operation(s)"),
        1 => ("connections", "connection(s)"),
        _ => ("drivers", "driver(s)"),
    };
    let mut array = value.get_mut(key).and_then(Value::as_array_mut);
    if array.is_none() {
        array = value
            .get_mut(key)
            .and_then(Value::as_object_mut)
            .and_then(|object| object.get_mut("entries"))
            .and_then(Value::as_array_mut);
    }
    if let Some(array) = array
        && !array.is_empty()
    {
        let drop = array.len().div_ceil(2);
        array.drain(..drop);
        outline.truncated.push(format!("{drop} {name} omitted"));
        return;
    }
    if let Some(tabs) = value
        .get_mut("interface")
        .and_then(|i| i.get_mut("open_tabs"))
        .and_then(|t| t.get_mut("open"))
        .and_then(Value::as_array_mut)
        && !tabs.is_empty()
    {
        let drop = tabs.len().div_ceil(2);
        tabs.drain(..drop);
        outline.truncated.push(format!("{drop} tab(s) omitted"));
        return;
    }
    if let Some(recent) = value
        .get_mut("incidents")
        .and_then(|i| i.get_mut("recent"))
        .and_then(Value::as_array_mut)
        && !recent.is_empty()
    {
        recent.clear();
        outline.truncated.push("recent incidents omitted".into());
        return;
    }
    if let Some(settings) = value
        .get_mut("settings")
        .and_then(|s| s.get_mut("values"))
        .and_then(Value::as_array_mut)
        && !settings.is_empty()
    {
        settings.clear();
        outline.truncated.push("settings omitted".into());
    }
}

/// The home directory, used to shorten paths in a bundle.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::model::{
        ApplicationInfo, ConnectionInfo, DriverInfo, IncidentKind, InterfaceInfo,
        OperationSnapshot, TabInfo,
    };
    use crate::diagnostics::ring::RingSnapshot;
    use std::collections::BTreeMap;

    fn incident(id: &str, logs: Vec<String>) -> Incident {
        Incident {
            schema: 1,
            id: id.into(),
            session_id: "s".into(),
            at_ms: 1,
            kind: IncidentKind::Error,
            message: "boom select * from users".into(),
            target: None,
            thread: None,
            file: None,
            line: None,
            error_kind: None,
            fields: BTreeMap::from([("sql".into(), json!("select secret from t"))]),
            note: None,
            acknowledged: false,
            environment: Environment::basic(),
            logs,
            backtrace: None,
        }
    }

    fn context<'a>(
        session: &'a SessionSummary,
        env: &'a Environment,
        ring: &'a RingSnapshot,
        recent: &'a [IncidentSummary],
    ) -> BundleContext<'a> {
        BundleContext {
            session,
            environment: env,
            ring,
            recent,
            data_dir: None,
            home: None,
        }
    }

    fn session() -> SessionSummary {
        SessionSummary {
            id: "s".into(),
            started_ms: 1,
            uptime_s: 1,
            pid: 2,
            version: "0.1.0".into(),
            data_dir: None,
            other_instance: false,
        }
    }

    fn basic_inputs() -> BundleInputs {
        BundleInputs {
            application: ApplicationInfo {
                version: "0.1.0".into(),
                profile: "release".into(),
                target: "aarch64-apple-darwin".into(),
                ..Default::default()
            },
            drivers: vec![DriverInfo {
                id: "postgres".into(),
                name: "PostgreSQL".into(),
                version: "0.1.0".into(),
                source: "bundled".into(),
                sidecar_running: None,
                sidecar_restarts: None,
            }],
            settings: vec![
                ("global".into(), "grid.page_size".into(), json!(500)),
                (
                    "global".into(),
                    "terminal.shell".into(),
                    json!("sh -c 'password=hunter2'"),
                ),
            ],
            connections: vec![ConnectionInfo {
                id: "c1".into(),
                name: "local".into(),
                driver: "postgres".into(),
                ..Default::default()
            }],
            interface: InterfaceInfo {
                open_tabs: vec![TabInfo {
                    title: "Query 1".into(),
                    connection: Some("c1".into()),
                    database: None,
                    kind: "query".into(),
                }],
                titles_included: true,
                ..Default::default()
            },
            operations: vec![OperationSnapshot {
                kind: "query".into(),
                connection: Some("c1".into()),
                label: "select 1".into(),
                duration_ms: 5,
                outcome: "failed".into(),
                error: Some("syntax".into()),
            }],
            incident: Some(incident("i", vec!["log".into()])),
            include_sql: true,
            include_titles: true,
            redact: Vec::new(),
        }
    }

    #[test]
    fn a_bundle_has_every_documented_section() {
        let session = session();
        let env = Environment::basic();
        let ring = RingSnapshot {
            lines: vec!["one".into()],
            ..Default::default()
        };
        let (text, outline) = render(&basic_inputs(), &context(&session, &env, &ring, &[]));
        let value: Value = serde_json::from_str(&text).unwrap();
        for key in [
            "schema",
            "session",
            "application",
            "environment",
            "drivers",
            "settings",
            "connections",
            "interface",
            "operations",
            "logs",
            "incidents",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert!(outline.bytes > 0);
        assert!(value["settings"]["omitted"].as_u64() == Some(1));
        assert!(!text.contains("hunter2"));
    }

    #[test]
    fn sql_is_absent_when_the_setting_is_off() {
        let session = session();
        let env = Environment::basic();
        let mut inputs = basic_inputs();
        inputs.include_sql = false;
        let (text, _) = render(
            &inputs,
            &context(&session, &env, &RingSnapshot::default(), &[]),
        );
        assert!(!text.contains("select secret"));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert!(
            !serde_json::to_string(&value["incidents"])
                .unwrap()
                .contains("\"sql\"")
        );
    }

    #[test]
    fn secret_sentinels_never_survive_redaction() {
        let session = session();
        let env = Environment::basic();
        let ring = RingSnapshot {
            lines: vec!["token=sentinel-token password=sentinel-password".into()],
            ..Default::default()
        };
        let mut inputs = basic_inputs();
        inputs.redact = vec!["sentinel-token".into(), "sentinel-password".into()];
        inputs.connections[0].name = "sentinel-password".into();
        let (text, _) = render(&inputs, &context(&session, &env, &ring, &[]));
        assert!(!text.contains("sentinel-token"));
        assert!(!text.contains("sentinel-password"));
        assert!(text.contains("<redacted>"));
    }

    #[test]
    fn many_tabs_operations_and_logs_stay_bounded_with_markers() {
        let session = session();
        let env = Environment::basic();
        let big_sql = "x".repeat(200_000);
        let ring = RingSnapshot {
            lines: (0..600).map(|_| big_sql.clone()).collect(),
            ..Default::default()
        };
        let mut inputs = basic_inputs();
        inputs.interface.open_tabs = (0..500)
            .map(|i| TabInfo {
                title: format!("Tab {i}"),
                connection: Some("c1".into()),
                database: Some("db".into()),
                kind: "query".into(),
            })
            .collect();
        inputs.operations = (0..500)
            .map(|i| OperationSnapshot {
                kind: "query".into(),
                connection: Some("c1".into()),
                label: big_sql.clone(),
                duration_ms: i,
                outcome: "done".into(),
                error: None,
            })
            .collect();
        inputs.incident = Some(incident("big", (0..500).map(|_| big_sql.clone()).collect()));
        let (text, outline) = render(&inputs, &context(&session, &env, &ring, &[]));
        assert!(text.len() <= BUNDLE_BYTES, "{} bytes", text.len());
        serde_json::from_str::<Value>(&text).expect("valid JSON");
        assert!(!outline.truncated.is_empty());
        assert!(text.contains("truncated") || text.contains("omitted"));
    }

    #[test]
    fn paths_under_home_and_the_data_directory_are_shortened() {
        let session = session();
        let env = Environment::basic();
        let ring = RingSnapshot {
            lines: vec!["at /home/me/sqlr/logs/sqlr.log and /home/me/data/sqlr.db".into()],
            ..Default::default()
        };
        let mut inputs = basic_inputs();
        inputs.interface.open_tabs[0].title = "/home/me/data/query.sql".into();
        let context = BundleContext {
            session: &session,
            environment: &env,
            ring: &ring,
            recent: &[],
            data_dir: Some(Path::new("/home/me/data")),
            home: Some(PathBuf::from("/home/me")),
        };
        let (text, _) = render(&inputs, &context);
        assert!(text.contains("<home>/sqlr/logs/sqlr.log"));
        assert!(text.contains("<data-dir>/sqlr.db"));
        assert!(text.contains("<data-dir>/query.sql"));
        assert!(!text.contains("/home/me"));
    }

    #[test]
    fn omitted_titles_are_labelled() {
        let session = session();
        let env = Environment::basic();
        let mut inputs = basic_inputs();
        inputs.include_titles = false;
        let (text, _) = render(
            &inputs,
            &context(&session, &env, &RingSnapshot::default(), &[]),
        );
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["interface"]["open_tabs"]["titles_included"], false);
        assert_eq!(
            value["interface"]["open_tabs"]["open"][0]["title"],
            "<title omitted>"
        );
        assert!(!text.contains("Query 1"));
    }

    #[test]
    fn a_bundle_is_still_valid_when_every_section_is_cleared() {
        let session = session();
        let env = Environment::basic();
        let (text, _) = render(
            &basic_inputs(),
            &context(&session, &env, &RingSnapshot::default(), &[]),
        );
        let _: Value = serde_json::from_str(&text).unwrap();
    }
}
