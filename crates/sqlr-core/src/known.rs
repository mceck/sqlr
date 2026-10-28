//! The databases sqlr knows about: those its drivers serve, those that speak a bundled
//! driver's protocol (with the connection fields to preset), and those still planned.
//! The new-connection catalog and the "Databases…" matrix read it.

use crate::manifest::Family;

/// How sqlr reaches a known database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownStatus {
    /// A driver of its own (`driver` is the driver id).
    Bundled {
        /// The id of the driver that serves it.
        driver: &'static str,
    },
    /// It speaks a bundled driver's protocol; `preset` fills the connection form's fields
    /// (keys of that driver's manifest).
    Compatible {
        /// The id of the bundled driver whose protocol it speaks.
        driver: &'static str,
        /// Connection field values to preset, as (manifest field key, value).
        preset: &'static [(&'static str, &'static str)],
    },
    /// A driver that does not ship with sqlr: the Driver Manager downloads it from the sqlr
    /// release (the release workflow packs every one of these), or installs it from a local
    /// driver file (`driver` is the driver id).
    Installable {
        /// The id of the driver to install.
        driver: &'static str,
    },
    /// Not reachable yet; `note` describes its support status.
    Planned {
        /// What is planned or why it is not supported yet.
        note: &'static str,
    },
}

/// A database in the catalog of known databases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownDatabase {
    /// Stable identifier.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// The family of its driver.
    pub family: Family,
    /// How sqlr reaches it.
    pub status: KnownStatus,
    /// The port it listens on by default, if it has one.
    pub default_port: Option<u16>,
    /// A short note shown in the database matrix.
    pub notes: &'static str,
}

impl KnownDatabase {
    /// The driver that connects to it (installed or installable), unless it is planned.
    pub fn driver(&self) -> Option<&'static str> {
        match self.status {
            KnownStatus::Bundled { driver }
            | KnownStatus::Compatible { driver, .. }
            | KnownStatus::Installable { driver } => Some(driver),
            KnownStatus::Planned { .. } => None,
        }
    }

    /// The connection fields it presets (none for bundled and planned databases).
    pub fn preset(&self) -> &'static [(&'static str, &'static str)] {
        match self.status {
            KnownStatus::Compatible { preset, .. } => preset,
            _ => &[],
        }
    }
}

const JDBC: &str = "JDBC bridge, downloaded from the sqlr release; needs Java 17+";

const fn bundled(
    id: &'static str,
    name: &'static str,
    family: Family,
    driver: &'static str,
    default_port: Option<u16>,
    notes: &'static str,
) -> KnownDatabase {
    KnownDatabase {
        id,
        name,
        family,
        status: KnownStatus::Bundled { driver },
        default_port,
        notes,
    }
}

const fn compatible(
    id: &'static str,
    name: &'static str,
    driver: &'static str,
    preset: &'static [(&'static str, &'static str)],
    default_port: Option<u16>,
    notes: &'static str,
) -> KnownDatabase {
    KnownDatabase {
        id,
        name,
        family: Family::Relational,
        status: KnownStatus::Compatible { driver, preset },
        default_port,
        notes,
    }
}

const fn installable(
    id: &'static str,
    name: &'static str,
    driver: &'static str,
    default_port: Option<u16>,
    notes: &'static str,
) -> KnownDatabase {
    KnownDatabase {
        id,
        name,
        family: Family::Relational,
        status: KnownStatus::Installable { driver },
        default_port,
        notes,
    }
}

const fn planned(
    id: &'static str,
    name: &'static str,
    family: Family,
    note: &'static str,
    default_port: Option<u16>,
) -> KnownDatabase {
    KnownDatabase {
        id,
        name,
        family,
        status: KnownStatus::Planned { note },
        default_port,
        notes: note,
    }
}

use Family::{Document, KeyValue, Relational};

static KNOWN: &[KnownDatabase] = &[
    bundled(
        "postgresql",
        "PostgreSQL",
        Relational,
        "postgres",
        Some(5432),
        "TCP server",
    ),
    bundled(
        "mysql",
        "MySQL",
        Relational,
        "mysql",
        Some(3306),
        "TCP server",
    ),
    bundled(
        "mariadb",
        "MariaDB",
        Relational,
        "mysql",
        Some(3306),
        "TCP server, through the MySQL driver",
    ),
    bundled(
        "sqlserver",
        "SQL Server",
        Relational,
        "sqlserver",
        Some(1433),
        "TCP server, native TDS driver",
    ),
    bundled(
        "oracle",
        "Oracle",
        Relational,
        "oracle",
        Some(1521),
        "TCP server, pure-Rust Oracle driver",
    ),
    bundled("sqlite", "SQLite", Relational, "sqlite", None, "File"),
    bundled(
        "snowflake",
        "Snowflake",
        Relational,
        "snowflake",
        Some(443),
        "SQL API over HTTPS, token authentication",
    ),
    bundled(
        "elasticsearch",
        "Elasticsearch",
        Document,
        "elasticsearch",
        Some(9200),
        "Search engine, over HTTP",
    ),
    bundled(
        "redis",
        "Redis",
        KeyValue,
        "redis",
        Some(6379),
        "Key-value store",
    ),
    compatible(
        "cockroachdb",
        "CockroachDB",
        "postgres",
        &[("port", "26257"), ("database", "defaultdb")],
        Some(26257),
        "PostgreSQL wire protocol",
    ),
    compatible(
        "yugabytedb",
        "YugabyteDB",
        "postgres",
        &[("port", "5433"), ("database", "yugabyte")],
        Some(5433),
        "PostgreSQL wire protocol (YSQL)",
    ),
    compatible(
        "redshift",
        "Amazon Redshift",
        "postgres",
        &[("port", "5439"), ("database", "dev")],
        Some(5439),
        "PostgreSQL wire protocol",
    ),
    compatible(
        "greenplum",
        "Greenplum",
        "postgres",
        &[],
        Some(5432),
        "PostgreSQL wire protocol",
    ),
    compatible(
        "timescaledb",
        "TimescaleDB",
        "postgres",
        &[],
        Some(5432),
        "A PostgreSQL extension",
    ),
    compatible(
        "neon",
        "Neon",
        "postgres",
        &[("ssl_mode", "require")],
        Some(5432),
        "Serverless PostgreSQL",
    ),
    compatible(
        "supabase",
        "Supabase",
        "postgres",
        &[("ssl_mode", "require")],
        Some(5432),
        "Hosted PostgreSQL",
    ),
    compatible(
        "aurora-postgresql",
        "Amazon Aurora PostgreSQL",
        "postgres",
        &[],
        Some(5432),
        "PostgreSQL wire protocol",
    ),
    compatible(
        "tidb",
        "TiDB",
        "mysql",
        &[("port", "4000")],
        Some(4000),
        "MySQL wire protocol",
    ),
    compatible(
        "singlestore",
        "SingleStore",
        "mysql",
        &[],
        Some(3306),
        "MySQL wire protocol",
    ),
    compatible(
        "aurora-mysql",
        "Amazon Aurora MySQL",
        "mysql",
        &[],
        Some(3306),
        "MySQL wire protocol",
    ),
    KnownDatabase {
        id: "mongodb",
        name: "MongoDB",
        family: Document,
        status: KnownStatus::Installable { driver: "mongodb" },
        default_port: Some(27017),
        notes: "Document sidecar, downloaded from the sqlr release",
    },
    installable(
        "sap-hana",
        "SAP HANA",
        "hana",
        Some(30015),
        "hdbconnect sidecar, downloaded from the sqlr release",
    ),
    installable(
        "duckdb",
        "DuckDB",
        "duckdb",
        None,
        "embedded DuckDB sidecar, downloaded from the sqlr release",
    ),
    installable("jdbc", "Generic JDBC", "jdbc", None, JDBC),
    installable("db2", "Db2", "jdbc-db2", Some(50000), JDBC),
    installable(
        "clickhouse",
        "ClickHouse",
        "jdbc-clickhouse",
        Some(8123),
        JDBC,
    ),
    installable("h2", "H2", "jdbc-h2", None, JDBC),
    planned("bigquery", "BigQuery", Relational, "later", None),
    planned("cassandra", "Cassandra", Document, "later", Some(9042)),
];

/// Every known database: bundled first, then compatible, installable, planned.
pub fn known_databases() -> &'static [KnownDatabase] {
    KNOWN
}

/// The known database with `id`.
pub fn known_database(id: &str) -> Option<&'static KnownDatabase> {
    KNOWN.iter().find(|k| k.id == id)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// The driver ids sqlr ships.
    const DRIVERS: [&str; 8] = [
        "postgres",
        "mysql",
        "sqlite",
        "sqlserver",
        "oracle",
        "elasticsearch",
        "redis",
        "snowflake",
    ];

    #[test]
    fn mongodb_installs_as_a_document_sidecar() {
        let mongo = known_database("mongodb").unwrap();
        assert_eq!(mongo.status, KnownStatus::Installable { driver: "mongodb" });
        assert_eq!(mongo.family, Family::Document);
        assert!(mongo.notes.contains("downloaded"));
    }

    #[test]
    fn snowflake_is_built_in_and_duckdb_installs_from_a_file() {
        let snowflake = known_database("snowflake").unwrap();
        assert_eq!(
            snowflake.status,
            KnownStatus::Bundled {
                driver: "snowflake"
            }
        );
        // The embedded engine would double the app's size: it is a sidecar driver file.
        let duckdb = known_database("duckdb").unwrap();
        assert_eq!(duckdb.status, KnownStatus::Installable { driver: "duckdb" });
        assert_eq!(duckdb.family, Family::Relational);
    }

    #[test]
    fn known_catalog_is_consistent() {
        let all = known_databases();
        let mut ids = HashSet::new();
        let mut rank = 0;
        for db in all {
            assert!(ids.insert(db.id), "duplicate id {}", db.id);
            assert!(!db.name.is_empty() && !db.notes.is_empty(), "{}", db.id);
            let this = match db.status {
                KnownStatus::Bundled { driver } => {
                    assert!(DRIVERS.contains(&driver), "{}: {driver}", db.id);
                    0
                }
                KnownStatus::Compatible { driver, preset } => {
                    assert!(DRIVERS.contains(&driver), "{}: {driver}", db.id);
                    // A preset port is the database's default port.
                    if let Some((_, port)) = preset.iter().find(|(k, _)| *k == "port") {
                        assert_eq!(
                            db.default_port.map(|p| p.to_string()),
                            Some(port.to_string())
                        );
                    }
                    1
                }
                KnownStatus::Installable { driver } => {
                    // Installable ids are driver-file ids: plain names, never a bundled one.
                    assert!(!DRIVERS.contains(&driver), "{}: {driver}", db.id);
                    assert!(
                        driver
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                    );
                    2
                }
                KnownStatus::Planned { note } => {
                    assert!(!note.is_empty(), "{}", db.id);
                    assert_eq!(db.driver(), None);
                    3
                }
            };
            assert!(
                this >= rank,
                "{} is out of the bundled/compatible/installable/planned order",
                db.id
            );
            rank = this;
        }
        for id in [
            "postgresql",
            "mysql",
            "mariadb",
            "sqlite",
            "redis",
            "sqlserver",
        ] {
            assert!(known_database(id).is_some(), "{id}");
        }
        let cockroach = known_database("cockroachdb").unwrap();
        assert_eq!(cockroach.driver(), Some("postgres"));
        assert_eq!(
            cockroach.preset(),
            &[("port", "26257"), ("database", "defaultdb")]
        );
        assert_eq!(known_database("mariadb").unwrap().driver(), Some("mysql"));
    }
}
