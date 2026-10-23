//! Portable client connection definitions. Credentials and agent consent never travel.
//! Parsing is pure; discovery and file reads belong on the store worker.
mod discovery;
mod export;
mod import;

pub use discovery::{DetectedSource, discover, read_source};
pub use export::encode;
pub use import::{decode, detect};
use sqlr_core::{AgentAccess, ConnectionConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientFormat {
    Sqlr,
    Dbeaver,
    Datagrip,
    TablePlus,
    PgAdmin,
}

impl ClientFormat {
    pub const ALL: [Self; 5] = [
        Self::Sqlr,
        Self::Dbeaver,
        Self::Datagrip,
        Self::TablePlus,
        Self::PgAdmin,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Sqlr => "sqlr",
            Self::Dbeaver => "DBeaver",
            Self::Datagrip => "DataGrip / JetBrains",
            Self::TablePlus => "TablePlus",
            Self::PgAdmin => "pgAdmin",
        }
    }
    pub fn filename(self) -> &'static str {
        match self {
            Self::Sqlr => "sqlr-connections.json",
            Self::Dbeaver => "data-sources-sqlr.json",
            Self::Datagrip => "dataSources.xml",
            Self::TablePlus => "tableplus-connections.txt",
            Self::PgAdmin => "servers.json",
        }
    }
    pub fn export_help(self) -> &'static str {
        match self {
            Self::Sqlr => "Import this backup with File → Import… in sqlr.",
            Self::Dbeaver => {
                "Place this additional data-sources-sqlr.json file in your project's .dbeaver folder, then restart DBeaver. Keep the existing data-sources.json."
            }
            Self::Datagrip => {
                "Copy the XML file's contents, then use New → Import from Clipboard in DataGrip or another JetBrains database tool."
            }
            Self::TablePlus => {
                "One connection URL per line. In TablePlus, use Create a new connection → Import from URL for each URL. This is not an encrypted .tableplusconnection archive."
            }
            Self::PgAdmin => {
                "Use Tools → Import/Export Servers in pgAdmin to import this JSON file (PostgreSQL only)."
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct ImportBatch {
    pub connections: Vec<ConnectionConfig>,
    pub warnings: Vec<String>,
}

pub struct ExportDocument {
    pub contents: String,
    pub warnings: Vec<String>,
}

pub fn supports_export(format: ClientFormat, c: &ConnectionConfig) -> bool {
    match format {
        ClientFormat::Sqlr => true,
        ClientFormat::PgAdmin => c.driver == "postgres",
        ClientFormat::Dbeaver => matches!(
            c.driver.as_str(),
            "postgres" | "mysql" | "sqlite" | "mssql" | "oracle"
        ),
        ClientFormat::Datagrip | ClientFormat::TablePlus => {
            matches!(c.driver.as_str(), "postgres" | "mysql" | "sqlite" | "mssql")
                && c.values.get("ssh_enabled").is_none_or(|v| v != "true")
        }
    }
}

pub fn save_export(
    path: &std::path::Path,
    format: ClientFormat,
    connections: &[ConnectionConfig],
) -> Result<Vec<String>, String> {
    let doc = encode(format, connections)?;
    crate::write_atomic(path, &doc.contents).map_err(|_| {
        "Could not save the export. Check the destination and permissions.".to_string()
    })?;
    Ok(doc.warnings)
}

pub(super) fn driver(id: &str) -> Option<&'static str> {
    let id = id.to_ascii_lowercase();
    let id = id.trim();
    if id.starts_with("postgres") || matches!(id, "cockroachdb" | "redshift" | "greenplum") {
        Some("postgres")
    } else if id.starts_with("mysql") || id.starts_with("mariadb") {
        Some("mysql")
    } else if id.starts_with("sqlite") {
        Some("sqlite")
    } else if id.contains("sqlserver")
        || matches!(id, "mssql" | "sql server" | "microsoft sql server")
    {
        Some("mssql")
    } else if id.starts_with("oracle") {
        Some("oracle")
    } else if id.starts_with("mongo") {
        Some("mongodb")
    } else if id.starts_with("redis") {
        Some("redis")
    } else if id.starts_with("elastic") {
        Some("elasticsearch")
    } else {
        None
    }
}

pub(super) fn blank(name: &str, driver: &str) -> ConnectionConfig {
    ConnectionConfig {
        id: String::new(),
        name: name.to_string(),
        driver: driver.to_string(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: false,
        agent_access: AgentAccess::None,
    }
}

/// An allowlist prevents credential aliases, arbitrary JDBC properties, embedded URLs,
/// startup commands and unknown driver fields from reaching the connections file.
const PORTABLE_FIELDS: &[&str] = &[
    "host",
    "port",
    "database",
    "user",
    "path",
    "service",
    "service_type",
    "service_name",
    "sid",
    "instance",
    "tls",
    "scheme",
    "tls_ca",
    "tls_cert",
    "tls_key",
    "tls_skip_verify",
    "read_only",
    "auth_source",
    "replica_set",
    "direct_connection",
    "encryption",
    "ssl_mode",
    "ssl_root_cert",
    "ssl_ca",
    "ssl_cert",
    "ssl_key",
    "encrypt",
    "trust_server_certificate",
    "ssh_enabled",
    "ssh_host",
    "ssh_port",
    "ssh_user",
    "ssh_auth",
    "ssh_key_path",
    "ssh_jump_host",
    "ssh_jump_port",
    "ssh_jump_user",
    "show_all_databases",
];

pub fn portable(mut cfg: ConnectionConfig) -> ConnectionConfig {
    cfg.values
        .retain(|k, _| PORTABLE_FIELDS.contains(&k.as_str()));
    cfg.agent_access = AgentAccess::None;
    cfg.save_secrets = false;
    cfg
}

/// Connection identity deliberately ignores names, folders and display preferences.
pub fn same_connection(a: &ConnectionConfig, b: &ConnectionConfig) -> bool {
    fn value<'a>(c: &'a ConnectionConfig, k: &str) -> &'a str {
        c.values.get(k).map(String::as_str).unwrap_or("")
    }
    fn port(c: &ConnectionConfig) -> &str {
        match value(c, "port") {
            "" => match c.driver.as_str() {
                "postgres" => "5432",
                "mysql" => "3306",
                "mssql" => "1433",
                "oracle" => "1521",
                "mongodb" => "27017",
                "redis" => "6379",
                _ => "",
            },
            p => p,
        }
    }
    a.driver == b.driver
        && port(a) == port(b)
        && value(a, "host").eq_ignore_ascii_case(value(b, "host"))
        && [
            "database",
            "path",
            "user",
            "service",
            "service_type",
            "service_name",
            "sid",
            "instance",
            "ssh_enabled",
            "ssh_host",
            "ssh_port",
            "ssh_user",
        ]
        .iter()
        .all(|k| value(a, k) == value(b, k))
}
