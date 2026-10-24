//! The incident model and the shapes of a diagnostics bundle.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The incident schema version. Unknown fields are kept, so new instrumentation does not
/// need a schema change.
pub const INCIDENT_SCHEMA: u32 = 1;
/// The bundle schema version.
pub const BUNDLE_SCHEMA: u32 = 1;

/// Why an incident exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentKind {
    /// A panic on any thread.
    Panic,
    /// A failure before or during application startup.
    Startup,
    /// The previous session ended without a clean exit and without a recorded panic.
    UncleanShutdown,
    /// A panic inside an engine command task.
    EnginePanic,
    /// A sidecar driver process exited or broke the protocol.
    SidecarCrash,
    /// An unexpected application-level error emitted at error level.
    Error,
    /// A report created by the person through Report a Problem.
    Manual,
}

impl IncidentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Panic => "panic",
            Self::Startup => "startup",
            Self::UncleanShutdown => "unclean_shutdown",
            Self::EnginePanic => "engine_panic",
            Self::SidecarCrash => "sidecar_crash",
            Self::Error => "error",
            Self::Manual => "manual",
        }
    }

    /// The kind a structured `error!` event names in its `incident` field; an error event
    /// without one is an [`IncidentKind::Error`].
    pub fn from_event(value: Option<&str>) -> Self {
        match value {
            Some("panic") => Self::Panic,
            Some("startup") => Self::Startup,
            Some("unclean_shutdown") => Self::UncleanShutdown,
            Some("engine_panic") => Self::EnginePanic,
            Some("sidecar_crash") => Self::SidecarCrash,
            Some("manual") => Self::Manual,
            _ => Self::Error,
        }
    }
}

/// The application and platform summary attached to an incident or bundle.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub version: String,
    pub os: String,
    pub arch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphics_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_memory_mb: Option<f64>,
}

impl Environment {
    /// The always-known part, usable before the background collection finished.
    pub fn basic() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            target: option_env!("SQLR_TARGET").map(str::to_string),
            profile: option_env!("SQLR_PROFILE").map(str::to_string),
            commit: option_env!("SQLR_GIT_COMMIT").map(str::to_string),
            ..Self::default()
        }
    }
}

/// One recorded unexpected event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Incident {
    pub schema: u32,
    pub id: String,
    pub session_id: String,
    pub at_ms: i64,
    pub kind: IncidentKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub acknowledged: bool,
    pub environment: Environment,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
    /// Reserved for the day symbols become available; release builds stay stripped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backtrace: Option<String>,
}

impl Incident {
    /// The small list entry the Diagnostics window and startup banner use.
    pub fn summary(&self) -> IncidentSummary {
        IncidentSummary {
            id: self.id.clone(),
            session_id: self.session_id.clone(),
            at_ms: self.at_ms,
            kind: self.kind,
            message: self.message.clone(),
            acknowledged: self.acknowledged,
        }
    }
}

/// An incident as listed, without its fields and logs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentSummary {
    pub id: String,
    pub session_id: String,
    pub at_ms: i64,
    pub kind: IncidentKind,
    pub message: String,
    #[serde(default)]
    pub acknowledged: bool,
}

/// What the previous session left behind, and what the next launch should say.
#[derive(Clone, Debug)]
pub enum StartupNotice {
    /// The previous session panicked; the full report exists.
    Panic {
        incident: IncidentSummary,
        json: String,
    },
    /// The previous session did not close cleanly and left no panic report.
    Unclean {
        incident: IncidentSummary,
        json: String,
    },
}

impl StartupNotice {
    pub fn incident(&self) -> &IncidentSummary {
        match self {
            Self::Panic { incident, .. } | Self::Unclean { incident, .. } => incident,
        }
    }

    pub fn json(&self) -> &str {
        match self {
            Self::Panic { json, .. } | Self::Unclean { json, .. } => json,
        }
    }

    pub fn is_panic(&self) -> bool {
        matches!(self, Self::Panic { .. })
    }
}

/// The current session, as the Diagnostics window shows it and a bundle carries it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub started_ms: i64,
    pub uptime_s: u64,
    pub pid: u32,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<String>,
    pub other_instance: bool,
}

/// The application section of a bundle.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ApplicationInfo {
    pub version: String,
    pub profile: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphics_backend: Option<String>,
}

/// One driver in a bundle's drivers section.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriverInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    /// `bundled` or `installed`.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_running: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_restarts: Option<u64>,
}

/// The non-secret metadata of a saved connection.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub id: String,
    pub name: String,
    pub driver: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
}

/// One open tab in a bundle's interface section.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TabInfo {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    pub kind: String,
}

/// The interface state at the moment a bundle was requested.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InterfaceInfo {
    pub open_tabs: Vec<TabInfo>,
    #[serde(default)]
    pub titles_included: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_transactions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panels: Vec<String>,
}

/// One settled operation in a bundle.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationSnapshot {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    pub label: String,
    pub duration_ms: u64,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Everything the interface copies out of the live application for a bundle. Cheap to
/// build: no file I/O and no secrets.
#[derive(Clone, Debug, Default)]
pub struct BundleInputs {
    pub application: ApplicationInfo,
    pub drivers: Vec<DriverInfo>,
    pub settings: Vec<(String, String, Value)>,
    pub connections: Vec<ConnectionInfo>,
    pub interface: InterfaceInfo,
    pub operations: Vec<OperationSnapshot>,
    /// The incident the window selected, if any.
    pub incident: Option<Incident>,
    /// Per-report choice; the global setting is already reflected by the caller.
    pub include_sql: bool,
    /// Per-report choice for manual reports; automatic bundles include titles.
    pub include_titles: bool,
    /// Values that must never appear, redacted as a last line of defence. Never serialized.
    pub redact: Vec<String>,
}

/// A selected incident in a bundle, separated so its log excerpt can be capped.
#[derive(Clone, Debug, Serialize)]
pub struct BundleIncident {
    pub incident: Incident,
    pub logs_truncated: bool,
}

/// The rendered summary of what a bundle contains, for the preview.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BundleOutline {
    pub bytes: usize,
    pub tabs: usize,
    pub operations: usize,
    pub log_lines: usize,
    pub incidents: usize,
    pub truncated: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Incident {
        Incident {
            schema: INCIDENT_SCHEMA,
            id: "2026-10-24T20-15-03Z-4711-a1b2".into(),
            session_id: "6f0c".into(),
            at_ms: 1_790_000_000_000,
            kind: IncidentKind::Panic,
            message: "assertion failed".into(),
            target: Some("sqlr_app::diagnostics".into()),
            thread: Some("main".into()),
            file: Some("src/main.rs".into()),
            line: Some(812),
            error_kind: None,
            fields: BTreeMap::from([
                ("op".into(), Value::String("query".into())),
                ("conn".into(), Value::String("c1".into())),
                ("new_field".into(), Value::Number(7.into())),
            ]),
            note: None,
            acknowledged: false,
            environment: Environment::basic(),
            logs: vec!["one".into(), "two".into()],
            backtrace: None,
        }
    }

    #[test]
    fn incident_round_trips_unknown_fields_and_a_manual_note() {
        let mut incident = sample();
        incident.kind = IncidentKind::Manual;
        incident.note = Some("I was running the migration".into());
        let text = serde_json::to_string(&incident).unwrap();
        let back: Incident = serde_json::from_str(&text).unwrap();
        assert_eq!(back, incident);
        assert_eq!(back.fields["new_field"], Value::Number(7.into()));
        assert_eq!(back.note.as_deref(), Some("I was running the migration"));
    }

    #[test]
    fn unknown_incident_fields_survive_a_read() {
        let mut value = serde_json::to_value(sample()).unwrap();
        value["future"] = Value::String("kept by tools, ignored here".into());
        let back: Incident = serde_json::from_value(value).unwrap();
        assert_eq!(back.message, "assertion failed");
    }

    #[test]
    fn event_kinds_fall_back_to_error() {
        assert_eq!(IncidentKind::from_event(None), IncidentKind::Error);
        assert_eq!(
            IncidentKind::from_event(Some("sidecar_crash")),
            IncidentKind::SidecarCrash
        );
    }
}
