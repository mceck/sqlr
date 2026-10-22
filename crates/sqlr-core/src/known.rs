//! The databases sqlr knows about: those its drivers serve, those that speak a bundled
//! driver's protocol (with the connection fields to preset), and those still planned.
//! The new-connection catalog and the "Databases…" matrix read it.

use crate::manifest::Family;

/// How sqlr reaches a known database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownStatus {
    /// A driver of its own (`driver` is the driver id).
    Bundled { driver: &'static str },
    /// It speaks a bundled driver's protocol; `preset` fills the connection form's fields
    /// (keys of that driver's manifest).
    Compatible {
        driver: &'static str,
        preset: &'static [(&'static str, &'static str)],
    },
    /// A driver that does not ship with sqlr and installs from the registry (`driver` is
    /// the driver id; the Driver Manager and the new-connection catalog offer "Install").
    Installable { driver: &'static str },
    /// Not reachable yet; `note` describes its support status.
    Planned { note: &'static str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownDatabase {
    pub id: &'static str,
    pub name: &'static str,
    pub family: Family,
    pub status: KnownStatus,
    pub default_port: Option<u16>,
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

const JDBC: &str = "installable, through the JDBC bridge";

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
    bundled("sqlite", "SQLite", Relational, "sqlite", None, "File"),
    bundled(
        "mongodb",
        "MongoDB",
        Document,
        "mongodb",
        Some(27017),
        "Document store",
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
    installable(
        "sap-hana",
        "SAP HANA",
        "hana",
        Some(30015),
        "installable driver (hdbconnect)",
    ),
    installable("jdbc", "Generic JDBC", "jdbc", None, JDBC),
    installable(
        "sqlserver",
        "SQL Server",
        "jdbc-sqlserver",
        Some(1433),
        JDBC,
    ),
    installable("oracle", "Oracle", "jdbc-oracle", Some(1521), JDBC),
    installable("db2", "Db2", "jdbc-db2", Some(50000), JDBC),
    installable(
        "clickhouse",
        "ClickHouse",
        "jdbc-clickhouse",
        Some(8123),
        JDBC,
    ),
    installable("snowflake", "Snowflake", "jdbc-snowflake", None, JDBC),
    installable("duckdb", "DuckDB", "jdbc-duckdb", None, JDBC),
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
    const DRIVERS: [&str; 6] = [
        "postgres",
        "mysql",
        "sqlite",
        "mongodb",
        "elasticsearch",
        "redis",
    ];

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
                    // Installable ids are registry ids: plain names, never a bundled one.
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
