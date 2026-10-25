//! The typed catalogue of settings (§4.3): every key with its section, label, type, default,
//! range, the levels it may be overridden at and the words a search matches.

use std::sync::LazyLock;

/// A setting's value; [`SettingKind`] says which variant a key takes.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    /// One of a [`SettingKind::Choice`]'s ids.
    Choice(String),
    Color([u8; 3]),
    Path(String),
}

impl SettingValue {
    pub fn as_bool(&self) -> bool {
        matches!(self, Self::Bool(true))
    }

    pub fn as_i64(&self) -> i64 {
        match self {
            Self::Int(i) => *i,
            Self::Float(f) => *f as i64,
            _ => 0,
        }
    }

    pub fn as_f64(&self) -> f64 {
        match self {
            Self::Float(f) => *f,
            Self::Int(i) => *i as f64,
            _ => 0.0,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Text(s) | Self::Choice(s) | Self::Path(s) => s,
            _ => "",
        }
    }

    /// The JSON form stored on disk.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Bool(b) => (*b).into(),
            Self::Int(i) => (*i).into(),
            Self::Float(f) => (*f).into(),
            Self::Text(s) | Self::Choice(s) | Self::Path(s) => s.as_str().into(),
            Self::Color(c) => c.to_vec().into(),
        }
    }
}

/// The type of a setting, with its limits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingKind {
    Bool,
    Int {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
        step: f64,
    },
    Text,
    /// `(id, label)` pairs.
    Choice(&'static [(&'static str, &'static str)]),
    Color,
    Path {
        dir: bool,
    },
}

/// The levels a setting may be set at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Levels {
    GlobalOnly,
    /// Global, connection profile and connection.
    All,
}

/// Whether a lower level may loosen a higher one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Safety {
    None,
    /// A safety flag: a profile or connection can switch it on, never off.
    TightenOnly,
}

/// Where a value was set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Level {
    Global,
    /// A connection profile, by id.
    Profile(String),
    /// A connection, by id.
    Connection(String),
}

pub struct SettingDef {
    pub id: &'static str,
    pub section: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub keywords: &'static [&'static str],
    pub kind: SettingKind,
    pub default: SettingValue,
    pub levels: Levels,
    pub safety: Safety,
    /// Whether an MCP agent may read the value.
    pub agent_readable: bool,
    /// Whether an MCP agent may set or reset the value.
    pub agent_writable: bool,
}

impl SettingDef {
    /// Whether the setting may be set at `level`.
    pub fn allows_level(&self, level: &Level) -> bool {
        matches!(
            (self.levels, level),
            (Levels::GlobalOnly, Level::Global) | (Levels::All, _)
        )
    }
}

impl SettingDef {
    /// `value` when it is the right type and within the limits, else why not.
    pub fn check(&self, value: &SettingValue) -> Result<(), String> {
        if let Some(rest) = self.id.strip_prefix("drivers.")
            && let Some((driver, key)) = rest.split_once('.')
            && let SettingValue::Text(text) = value
            && !text.is_empty()
        {
            let fields = crate::driver_property_fields(driver);
            if key == "properties" && !fields.is_empty() {
                let properties: std::collections::BTreeMap<String, String> =
                    serde_json::from_str(text)
                        .map_err(|e| format!("Expected a JSON object of property strings: {e}"))?;
                let properties = properties
                    .into_iter()
                    .map(|(k, v)| (format!("property.{k}"), v))
                    .collect();
                return crate::validate_driver_properties(&fields, &properties)
                    .map_err(|e| e.to_string());
            }
            if !fields.is_empty() {
                crate::validate_driver_properties(
                    &fields,
                    &[(format!("property.{key}"), text.clone())].into(),
                )
                .map_err(|e| e.to_string())?;
            }
        }

        match (&self.kind, value) {
            (SettingKind::Bool, SettingValue::Bool(_))
            | (SettingKind::Text, SettingValue::Text(_))
            | (SettingKind::Color, SettingValue::Color(_))
            | (SettingKind::Path { .. }, SettingValue::Path(_)) => Ok(()),
            (SettingKind::Int { min, max }, SettingValue::Int(i)) if (min..=max).contains(&i) => {
                Ok(())
            }
            (SettingKind::Int { min, max }, SettingValue::Int(_)) => {
                Err(format!("{} must be between {min} and {max}", self.label))
            }
            (SettingKind::Float { min, max, .. }, SettingValue::Float(f))
                if !f.is_nan() && (*min..=*max).contains(f) =>
            {
                Ok(())
            }
            (SettingKind::Float { min, max, .. }, SettingValue::Float(_)) => {
                Err(format!("{} must be between {min} and {max}", self.label))
            }
            (SettingKind::Choice(options), SettingValue::Choice(c))
                if options.iter().any(|(id, _)| id == c) =>
            {
                Ok(())
            }
            (SettingKind::Choice(_), SettingValue::Choice(c)) => {
                Err(format!("{c:?} is not an option of {}", self.label))
            }
            _ => Err(format!("wrong type of value for {}", self.label)),
        }
    }

    /// A stored JSON value as this setting's type, clamped into its limits. `None` when the
    /// shape is wrong (the caller keeps the default).
    pub fn from_json(&self, json: &serde_json::Value) -> Option<SettingValue> {
        match self.kind {
            SettingKind::Bool => json.as_bool().map(SettingValue::Bool),
            SettingKind::Int { min, max } => json
                .as_i64()
                .or_else(|| json.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
                .map(|i| SettingValue::Int(i.clamp(min, max))),
            SettingKind::Float { min, max, .. } => json
                .as_f64()
                .filter(|f| !f.is_nan())
                .map(|f| SettingValue::Float(f.clamp(min, max))),
            SettingKind::Text => json.as_str().map(|s| SettingValue::Text(s.to_string())),
            SettingKind::Path { .. } => json.as_str().map(|s| SettingValue::Path(s.to_string())),
            SettingKind::Choice(options) => json
                .as_str()
                .filter(|s| options.iter().any(|(id, _)| id == s))
                .map(|s| SettingValue::Choice(s.to_string())),
            SettingKind::Color => {
                let a = json.as_array()?;
                let c: Vec<u8> = a
                    .iter()
                    .filter_map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
                    .collect();
                <[u8; 3]>::try_from(c).ok().map(SettingValue::Color)
            }
        }
    }

    /// Whether this setting matches every word of a (lowercase) search query.
    fn matches(&self, words: &[String]) -> bool {
        let hay = [self.id, self.section, self.label, self.description]
            .into_iter()
            .chain(self.keywords.iter().copied())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
            .join("\n");
        words.iter().all(|w| hay.contains(w.as_str()))
    }
}

/// The sections of the preferences window, in order.
pub const SECTIONS: &[&str] = &[
    "General",
    "Appearance",
    "Editor",
    "Result grid",
    "Data formats",
    "Keymap",
    "Connections",
    "Agents",
    "Terminal",
    "Drivers",
];

/// The settings whose label, description, keywords, id or section contain every word of `q`
/// (case-insensitive); all of them for a blank query.
pub fn search_settings(q: &str) -> Vec<&'static SettingDef> {
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    setting_defs()
        .iter()
        .filter(|d| d.matches(&words))
        .collect()
}

pub fn setting_def(id: &str) -> Option<&'static SettingDef> {
    setting_defs().iter().find(|d| d.id == id)
}

pub const THEMES: &[(&str, &str)] = &[("system", "System"), ("light", "Light"), ("dark", "Dark")];
pub const BOOL_FORMATS: &[(&str, &str)] = &[
    ("true_false", "true / false"),
    ("one_zero", "1 / 0"),
    ("check", "✓ / ✗"),
];
pub const BINARY_FORMATS: &[(&str, &str)] = &[
    ("hex", "Hex"),
    ("base64", "Base64"),
    ("utf8", "UTF-8 when valid, else hex"),
];
pub const JSON_FORMATS: &[(&str, &str)] = &[("compact", "Compact"), ("pretty", "Pretty")];
pub const GROUPINGS: &[(&str, &str)] = &[
    ("none", "None"),
    ("comma", "1,234,567"),
    ("space", "1 234 567"),
    ("underscore", "Underscore (1_234_567)"),
];

#[allow(clippy::too_many_arguments)]
fn def(
    id: &'static str,
    section: &'static str,
    label: &'static str,
    description: &'static str,
    keywords: &'static [&'static str],
    kind: SettingKind,
    default: SettingValue,
    levels: Levels,
) -> SettingDef {
    SettingDef {
        id,
        section,
        label,
        description,
        keywords,
        kind,
        default,
        levels,
        safety: Safety::None,
        agent_readable: true,
        agent_writable: true,
    }
}

/// A setting agents may read but never write (the MCP server's own configuration).
fn agents_readable(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    keywords: &'static [&'static str],
    kind: SettingKind,
    default: SettingValue,
) -> SettingDef {
    SettingDef {
        agent_writable: false,
        ..def(
            id,
            "Agents",
            label,
            description,
            keywords,
            kind,
            default,
            Levels::GlobalOnly,
        )
    }
}

/// A privacy switch: readable by agents, never written by them.
fn privacy_flag(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    keywords: &'static [&'static str],
) -> SettingDef {
    SettingDef {
        agent_writable: false,
        ..def(
            id,
            "General",
            label,
            description,
            keywords,
            SettingKind::Bool,
            SettingValue::Bool(true),
            Levels::GlobalOnly,
        )
    }
}

/// A setting neither readable nor writable by agents (`terminal.shell`).
fn human_only(
    id: &'static str,
    section: &'static str,
    label: &'static str,
    description: &'static str,
    keywords: &'static [&'static str],
    kind: SettingKind,
    default: SettingValue,
) -> SettingDef {
    SettingDef {
        agent_readable: false,
        agent_writable: false,
        ..def(
            id,
            section,
            label,
            description,
            keywords,
            kind,
            default,
            Levels::GlobalOnly,
        )
    }
}

fn safety(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    keywords: &'static [&'static str],
) -> SettingDef {
    SettingDef {
        safety: Safety::TightenOnly,
        ..def(
            id,
            "Connections",
            label,
            description,
            keywords,
            SettingKind::Bool,
            SettingValue::Bool(false),
            Levels::All,
        )
    }
}

static DEFS: LazyLock<Vec<SettingDef>> = LazyLock::new(build_defs);

/// Every setting, in the order the window lists them.
pub fn setting_defs() -> &'static [SettingDef] {
    &DEFS
}

fn build_defs() -> Vec<SettingDef> {
    use Levels::{All, GlobalOnly};
    use SettingKind as K;
    use SettingValue as V;
    let text = |s: &str| V::Text(s.to_string());
    let choice = |s: &str| V::Choice(s.to_string());
    vec![
        def(
            "general.stop_script_on_error",
            "General",
            "Stop a script at its first error",
            "A script stops at the first statement that fails instead of going on.",
            &["script", "error", "abort"],
            K::Bool,
            V::Bool(true),
            All,
        ),
        def(
            "operations.keep_finished_s",
            "General",
            "Keep finished operations for (s)",
            "How long the Operations panel lists an operation after it ended; 0 removes it at once.",
            &["operations", "monitor", "activity"],
            K::Int { min: 0, max: 3600 },
            V::Int(30),
            GlobalOnly,
        ),
        privacy_flag(
            "updates.check_on_start",
            "Check for updates at startup",
            "Once a day, sqlr asks GitHub whether a newer release exists. Nothing is downloaded until you choose Install, and a check that cannot reach GitHub is never reported.",
            &["update", "upgrade", "version", "release", "github"],
        ),
        privacy_flag(
            "diagnostics.include_sql",
            "Include SQL in logs and reports",
            "Failed statements are recorded as typed in logs, incidents and diagnostics bundles. Turn this off to keep SQL out of everything diagnostics write; there is no automatic literal redaction.",
            &[
                "diagnostics",
                "crash",
                "report",
                "privacy",
                "sql",
                "statement",
            ],
        ),
        def(
            "appearance.theme",
            "Appearance",
            "Theme",
            "Light, dark or follow the system.",
            &["dark", "light", "color", "mode"],
            K::Choice(THEMES),
            choice("system"),
            GlobalOnly,
        ),
        def(
            "appearance.ui_scale",
            "Appearance",
            "UI scale",
            "Zoom of the whole interface.",
            &["zoom", "size", "dpi"],
            K::Float {
                min: 0.75,
                max: 2.0,
                step: 0.05,
            },
            V::Float(1.0),
            GlobalOnly,
        ),
        def(
            "appearance.accent",
            "Appearance",
            "Accent color",
            "The color of the caret, the focus ring, links and primary buttons.",
            &["colour", "highlight"],
            K::Color,
            V::Color([0x2e, 0x6f, 0xf0]),
            GlobalOnly,
        ),
        def(
            "editor.font_size",
            "Editor",
            "Font size",
            "The monospace size of the editor and the result grid, in points.",
            &["mono", "text", "points"],
            K::Float {
                min: 10.0,
                max: 24.0,
                step: 1.0,
            },
            V::Float(13.0),
            GlobalOnly,
        ),
        def(
            "editor.completion_auto_open",
            "Editor",
            "Open completion while typing",
            "Choose which suggestions appear while you type.",
            &["autocomplete", "suggestions"],
            K::Bool,
            V::Bool(true),
            GlobalOnly,
        ),
        def(
            "editor.completion_delay_ms",
            "Editor",
            "Completion delay (ms)",
            "Suggest names from the active database catalog.",
            &["autocomplete", "suggestions"],
            K::Int { min: 0, max: 2000 },
            V::Int(0),
            GlobalOnly,
        ),
        def(
            "editor.completion_insert_on_tab",
            "Editor",
            "Insert completion on Tab",
            "Suggest matching SQL keywords.",
            &["autocomplete", "suggestions"],
            K::Bool,
            V::Bool(true),
            GlobalOnly,
        ),
        def(
            "editor.completion_keyword_case",
            "Editor",
            "Completion keyword case",
            "Suggest common SQL functions.",
            &["autocomplete", "suggestions"],
            K::Choice(&[
                ("upper", "UPPER"),
                ("lower", "lower"),
                ("preserve", "Preserve"),
            ]),
            V::Choice("preserve".into()),
            GlobalOnly,
        ),
        def(
            "formatter.keyword_case",
            "Editor",
            "SQL keyword case",
            "Configure SQL formatting.",
            &["format", "sql"],
            K::Choice(&[
                ("upper", "UPPER"),
                ("lower", "lower"),
                ("preserve", "Preserve"),
            ]),
            V::Choice("upper".into()),
            GlobalOnly,
        ),
        def(
            "formatter.indent",
            "Editor",
            "SQL indentation",
            "Configure SQL formatting.",
            &["format", "sql"],
            K::Choice(&[("2", "2 spaces"), ("4", "4 spaces"), ("tabs", "Tabs")]),
            V::Choice("4".into()),
            GlobalOnly,
        ),
        def(
            "formatter.max_line",
            "Editor",
            "SQL maximum line length",
            "Configure SQL formatting.",
            &["format", "sql"],
            K::Int { min: 20, max: 1000 },
            V::Int(100),
            GlobalOnly,
        ),
        def(
            "formatter.comma_first",
            "Editor",
            "Commas at start of line",
            "Configure SQL formatting.",
            &["format", "sql"],
            K::Bool,
            V::Bool(false),
            GlobalOnly,
        ),
        def(
            "formatter.break_before_and_or",
            "Editor",
            "Newline before AND and OR",
            "Configure SQL formatting.",
            &["format", "sql"],
            K::Bool,
            V::Bool(true),
            GlobalOnly,
        ),
        def(
            "formatter.blank_line_between_statements",
            "Editor",
            "Blank line between statements",
            "Configure SQL formatting.",
            &["format", "sql"],
            K::Bool,
            V::Bool(true),
            GlobalOnly,
        ),
        def(
            "editor.line_numbers",
            "Editor",
            "Line numbers",
            "Show the line-number gutter.",
            &["gutter"],
            K::Bool,
            V::Bool(true),
            GlobalOnly,
        ),
        def(
            "editor.soft_wrap",
            "Editor",
            "Soft wrap",
            "Wrap long lines at the width of the editor.",
            &["wrap", "long lines"],
            K::Bool,
            V::Bool(false),
            GlobalOnly,
        ),
        def(
            "grid.page_size",
            "Result grid",
            "Rows per fetch",
            "How many rows a query fetches at a time.",
            &["page", "fetch", "batch"],
            K::Int {
                min: 1,
                max: 1_000_000,
            },
            V::Int(500),
            All,
        ),
        def(
            "grid.row_cap",
            "Result grid",
            "Maximum rows when loading all results",
            "\"Fetch all\" stops at this many rows; 0 means no cap.",
            &["limit", "maximum", "rows"],
            K::Int {
                min: 0,
                max: i64::MAX / 2,
            },
            V::Int(100_000),
            All,
        ),
        def(
            "grid.result_memory_mib",
            "Result grid",
            "Memory per result",
            "Stop loading a SQL result before it exceeds this memory budget; 0 means no limit.",
            &["ram", "memory", "limit"],
            K::Int {
                min: 0,
                max: 1_048_576,
            },
            V::Int(128),
            GlobalOnly,
        ),
        def(
            "grid.total_memory_mib",
            "Result grid",
            "Memory for all results",
            "The combined memory budget of loaded SQL results; 0 means no limit. Existing rows are kept.",
            &["ram", "memory", "limit", "tabs"],
            K::Int {
                min: 0,
                max: 1_048_576,
            },
            V::Int(512),
            GlobalOnly,
        ),
        def(
            "grid.null_text",
            "Result grid",
            "NULL text",
            "What the grid shows for a NULL value.",
            &["null", "empty", "placeholder"],
            K::Text,
            text("NULL"),
            All,
        ),
        def(
            "formats.date",
            "Data formats",
            "Date pattern",
            "Date format, e.g. %d/%m/%Y; empty shows what the server sends.",
            &["strftime", "calendar", "day"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "formats.time",
            "Data formats",
            "Time pattern",
            "Time format, e.g. %H:%M; empty shows what the server sends.",
            &["strftime", "clock", "hour"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "formats.timestamp",
            "Data formats",
            "Timestamp pattern",
            "Date and time format, e.g. %Y-%m-%d %H:%M:%S; empty shows what the server sends.",
            &["strftime", "datetime", "date time"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "formats.timestamptz",
            "Data formats",
            "Timestamp with time zone pattern",
            "Date and time format with a zone, e.g. %Y-%m-%d %H:%M:%S %:z (shown in UTC); empty shows what the server sends.",
            &["strftime", "timezone", "offset"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "formats.number_grouping",
            "Data formats",
            "Digit grouping",
            "Separator between the thousands of whole numbers.",
            &["thousands", "separator", "number"],
            K::Choice(GROUPINGS),
            choice("none"),
            All,
        ),
        def(
            "formats.decimals",
            "Data formats",
            "Decimals of floating-point numbers",
            "Fixed number of decimals for float columns; -1 shows the value as it is.",
            &["precision", "float", "round"],
            K::Int { min: -1, max: 20 },
            V::Int(-1),
            All,
        ),
        def(
            "formats.boolean",
            "Data formats",
            "Booleans",
            "How true and false are written.",
            &["bool", "checkmark", "yes no"],
            K::Choice(BOOL_FORMATS),
            choice("true_false"),
            All,
        ),
        def(
            "formats.binary",
            "Data formats",
            "Binary values",
            "How bytes are written in a cell.",
            &["bytes", "blob", "bytea", "hex", "base64"],
            K::Choice(BINARY_FORMATS),
            choice("hex"),
            All,
        ),
        def(
            "formats.json",
            "Data formats",
            "JSON in cells",
            "Compact shows one line; pretty keeps the line breaks as ⏎.",
            &["document", "object"],
            K::Choice(JSON_FORMATS),
            choice("compact"),
            All,
        ),
        def(
            "formats.use_for_export",
            "Data formats",
            "Use these formats for export",
            "Exports write the formatted text instead of the raw values.",
            &["csv", "excel", "export"],
            K::Bool,
            V::Bool(false),
            All,
        ),
        def(
            "connection.session_per_tab",
            "Connections",
            "Open a separate session for each editor tab",
            "Each editor tab runs on its own connection instead of sharing the explorer's.",
            &["tab", "session", "transaction"],
            K::Bool,
            V::Bool(true),
            All,
        ),
        def(
            "connection.connect_timeout_s",
            "Connections",
            "Connect timeout (s)",
            "Seconds allowed for the SSH setup plus the driver's connect.",
            &["timeout", "ssh", "network"],
            K::Int { min: 1, max: 600 },
            V::Int(15),
            All,
        ),
        safety(
            "safety.read_only",
            "Read-only",
            "Refuse statements that write. Connections and profiles can make this stricter, never looser.",
            &["readonly", "write", "protect"],
        ),
        safety(
            "safety.confirm_writes",
            "Confirm writes",
            "Ask before running statements that modify data. Connections and profiles can require this confirmation, even when the global default is off.",
            &["confirm", "dml", "update"],
        ),
        safety(
            "safety.confirm_destructive",
            "Confirm destructive statements",
            "Ask before DROP, TRUNCATE and DELETE without WHERE. Connections and profiles can require this confirmation, even when the global default is off.",
            &["confirm", "drop", "truncate", "delete"],
        ),
        agents_readable(
            "agents.enabled",
            "Enable agent access (MCP)",
            "Allow local AI assistants to connect while sqlr is running. Choose their connections below.",
            &["mcp", "agent", "server", "remote", "claude", "codex"],
            K::Bool,
            V::Bool(false),
        ),
        agents_readable(
            "agents.port",
            "MCP port",
            "The loopback port the MCP server listens on; changing it restarts the server.",
            &["mcp", "port", "listen"],
            K::Int {
                min: 1024,
                max: 65535,
            },
            V::Int(47821),
        ),
        human_only(
            "terminal.shell",
            "Terminal",
            "Shell command",
            "Empty uses your login shell. Run through the platform shell wrapper; the command is never shown to agents or logged.",
            &["pty", "bash", "zsh", "powershell", "command"],
            K::Text,
            text(""),
        ),
        def(
            "terminal.cwd",
            "Terminal",
            "Working directory",
            "Empty uses the terminal folder in sqlr's configuration directory, with AGENTS.md and MCP configuration.",
            &["folder", "path", "directory"],
            K::Text,
            text(""),
            GlobalOnly,
        ),
        def(
            "terminal.font_size",
            "Terminal",
            "Font size",
            "The terminal's monospace size, in points.",
            &["mono", "text", "points", "zoom"],
            K::Float {
                min: 10.0,
                max: 24.0,
                step: 1.0,
            },
            V::Float(12.0),
            GlobalOnly,
        ),
        def(
            "terminal.scrollback",
            "Terminal",
            "Scrollback lines",
            "Lines the terminal keeps above the visible screen.",
            &["history", "buffer", "lines"],
            K::Int {
                min: 100,
                max: 10_000,
            },
            V::Int(2000),
            GlobalOnly,
        ),
        def(
            "drivers.postgres.properties",
            "Drivers",
            "PostgreSQL extra properties",
            "Additional non-secret startup properties.",
            &["session", "property"],
            K::Text,
            text("{}"),
            All,
        ),
        def(
            "drivers.mysql.properties",
            "Drivers",
            "MySQL extra properties",
            "Additional non-secret session variables as a JSON object.",
            &["session", "property"],
            K::Text,
            text("{}"),
            All,
        ),
        def(
            "drivers.postgres.application_name",
            "Drivers",
            "postgres: application_name",
            "Session property; empty keeps the driver default.",
            &["property", "connection"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "drivers.postgres.options",
            "Drivers",
            "postgres: options",
            "Session property; empty keeps the driver default.",
            &["property", "connection"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "drivers.postgres.statement_timeout",
            "Drivers",
            "postgres: statement_timeout",
            "Session property; empty keeps the driver default.",
            &["property", "connection"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "drivers.postgres.search_path",
            "Drivers",
            "postgres: search_path",
            "Session property; empty keeps the driver default.",
            &["property", "connection"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "drivers.mysql.sql_mode",
            "Drivers",
            "mysql: sql_mode",
            "Session property; empty keeps the driver default.",
            &["property", "connection"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "drivers.mysql.time_zone",
            "Drivers",
            "mysql: time_zone",
            "Session property; empty keeps the driver default.",
            &["property", "connection"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "drivers.mysql.wait_timeout",
            "Drivers",
            "mysql: wait_timeout",
            "Session property; empty keeps the driver default.",
            &["property", "connection"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "drivers.sqlite.foreign_keys",
            "Drivers",
            "sqlite: foreign_keys",
            "Session property; empty keeps the driver default.",
            &["property", "connection"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "drivers.sqlite.journal_mode",
            "Drivers",
            "sqlite: journal_mode",
            "Session property; empty keeps the driver default.",
            &["property", "connection"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "drivers.sqlite.busy_timeout",
            "Drivers",
            "sqlite: busy_timeout",
            "Session property; empty keeps the driver default.",
            &["property", "connection"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "drivers.sqlite.cache_size",
            "Drivers",
            "sqlite: cache_size",
            "Session property; empty keeps the driver default.",
            &["property", "connection"],
            K::Text,
            text(""),
            All,
        ),
        def(
            "drivers.sidecar_idle_minutes",
            "Drivers",
            "Sidecar idle time (min)",
            "Minutes a sidecar driver keeps running after its last connection closes.",
            &["sidecar", "idle", "process"],
            K::Int { min: 1, max: 240 },
            V::Int(5),
            GlobalOnly,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_defs_unique_and_defaults_valid() {
        let mut seen = std::collections::HashSet::new();
        for d in setting_defs() {
            assert!(seen.insert(d.id), "duplicate id {}", d.id);
            assert_eq!(d.check(&d.default), Ok(()), "{}", d.id);
            assert!(SECTIONS.contains(&d.section), "{} in {}", d.id, d.section);
            assert!(!d.label.is_empty() && !d.description.is_empty(), "{}", d.id);
            if d.safety == Safety::TightenOnly {
                assert_eq!(d.default, SettingValue::Bool(false));
                assert_eq!(d.levels, Levels::All);
            }
            // The stored form reads back as the same value.
            assert_eq!(d.from_json(&d.default.to_json()), Some(d.default.clone()));
        }
    }

    #[test]
    fn agent_visibility_of_settings() {
        let shell = setting_def("terminal.shell").unwrap();
        assert!(!shell.agent_readable && !shell.agent_writable);
        for id in ["agents.enabled", "agents.port", "updates.check_on_start"] {
            let def = setting_def(id).unwrap();
            assert!(def.agent_readable && !def.agent_writable, "{id}");
        }
        for id in ["terminal.cwd", "terminal.font_size", "terminal.scrollback"] {
            let def = setting_def(id).unwrap();
            assert!(def.agent_readable && def.agent_writable, "{id}");
        }
        assert_eq!(
            setting_def("agents.port").unwrap().default,
            SettingValue::Int(47821)
        );
        assert_eq!(
            setting_def("terminal.font_size").unwrap().default,
            SettingValue::Float(12.0)
        );
        assert_eq!(
            setting_def("terminal.scrollback").unwrap().default,
            SettingValue::Int(2000)
        );
    }

    #[test]
    fn search_matches_label_keyword_id() {
        let ids = |q: &str| -> Vec<&str> { search_settings(q).iter().map(|d| d.id).collect() };
        assert_eq!(ids("").len(), setting_defs().len());
        // Label, case-insensitive.
        assert!(ids("UI SCALE").contains(&"appearance.ui_scale"));
        // A keyword the label does not contain.
        assert!(ids("zoom").contains(&"appearance.ui_scale"));
        // The id.
        assert_eq!(ids("grid.row_cap"), ["grid.row_cap"]);
        // Every word must match.
        assert!(ids("zoom wrap").is_empty());
        assert!(ids("nothing like this").is_empty());
    }

    #[test]
    fn from_json_clamps_and_rejects_wrong_shapes() {
        let d = setting_def("grid.page_size").unwrap();
        assert_eq!(
            d.from_json(&serde_json::json!(0)),
            Some(SettingValue::Int(1))
        );
        assert_eq!(d.from_json(&serde_json::json!("x")), None);
        let theme = setting_def("appearance.theme").unwrap();
        assert_eq!(theme.from_json(&serde_json::json!("neon")), None);
        let accent = setting_def("appearance.accent").unwrap();
        assert_eq!(
            accent.from_json(&serde_json::json!([1, 2, 3])),
            Some(SettingValue::Color([1, 2, 3]))
        );
        assert_eq!(accent.from_json(&serde_json::json!([1, 2, 300])), None);
    }
}
