//! The databases the bridge knows by name: JDBC class, URL shape, form, SQL quirks. Maven
//! coordinates of their jars live in the signed registry index, not here.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    /// The driver id, as the registry and saved connections name it.
    pub id: &'static str,
    pub name: &'static str,
    /// The JDBC driver class; empty for the generic bridge, which asks for it.
    pub class: &'static str,
    /// `{host}`, `{port}`, `{database}` and `{path}` are replaced from the form.
    pub url: &'static str,
    pub port: u16,
    /// The connection form's fields, in order.
    pub fields: &'static str,
    /// `LIMIT`, `FETCH_FIRST`, `TOP` or `NONE`.
    pub limit: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "jdbc",
        name: "Generic JDBC",
        class: "",
        url: "",
        port: 0,
        fields: "jdbc_url,driver_class,jar,extra_jars,user,password",
        limit: "LIMIT",
    },
    Preset {
        id: "jdbc-h2",
        name: "H2",
        class: "org.h2.Driver",
        url: "jdbc:h2:{path}",
        port: 0,
        fields: "path,user,password",
        limit: "LIMIT",
    },
    Preset {
        id: "jdbc-duckdb",
        name: "DuckDB",
        class: "org.duckdb.DuckDBDriver",
        url: "jdbc:duckdb:{path}",
        port: 0,
        fields: "path",
        limit: "LIMIT",
    },
    Preset {
        id: "jdbc-sqlserver",
        name: "SQL Server",
        class: "com.microsoft.sqlserver.jdbc.SQLServerDriver",
        url: "jdbc:sqlserver://{host}:{port};databaseName={database};encrypt=true;trustServerCertificate=false",
        port: 1433,
        fields: "host,port,database,user,password",
        limit: "TOP",
    },
    Preset {
        id: "jdbc-oracle",
        name: "Oracle",
        class: "oracle.jdbc.OracleDriver",
        url: "jdbc:oracle:thin:@//{host}:{port}/{database}",
        port: 1521,
        fields: "host,port,database,user,password",
        limit: "FETCH_FIRST",
    },
    Preset {
        id: "jdbc-db2",
        name: "Db2",
        class: "com.ibm.db2.jcc.DB2Driver",
        url: "jdbc:db2://{host}:{port}/{database}",
        port: 50000,
        fields: "host,port,database,user,password",
        limit: "FETCH_FIRST",
    },
    Preset {
        id: "jdbc-clickhouse",
        name: "ClickHouse",
        class: "com.clickhouse.jdbc.ClickHouseDriver",
        url: "jdbc:clickhouse://{host}:{port}/{database}",
        port: 8123,
        fields: "host,port,database,user,password",
        limit: "LIMIT",
    },
    Preset {
        id: "jdbc-snowflake",
        name: "Snowflake",
        class: "net.snowflake.client.jdbc.SnowflakeDriver",
        url: "jdbc:snowflake://{host}/?db={database}",
        port: 0,
        fields: "host,database,user,password",
        limit: "LIMIT",
    },
];

/// The preset with this id (`"jdbc"` is the generic one).
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

impl Preset {
    /// The `key=value` arguments the Java bridge starts with.
    pub fn bridge_args(&self) -> Vec<String> {
        let mut args = vec![
            format!("id={}", self.id),
            format!("name={}", self.name),
            format!("class={}", self.class),
            format!("url={}", self.url),
            format!("fields={}", self.fields),
            format!("limit={}", self.limit),
        ];
        if self.port > 0 {
            args.push(format!("port={}", self.port));
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_consistent() {
        let mut ids = std::collections::HashSet::new();
        for p in PRESETS {
            assert!(ids.insert(p.id), "{} twice", p.id);
            assert!(p.id == "jdbc" || p.id.starts_with("jdbc-"), "{}", p.id);
            assert!(["LIMIT", "FETCH_FIRST", "TOP", "NONE"].contains(&p.limit));
            // Every placeholder of the URL is a field of the form.
            for key in ["host", "port", "database", "path"] {
                if p.url.contains(&format!("{{{key}}}")) {
                    assert!(p.fields.split(',').any(|f| f == key), "{}: {key}", p.id);
                }
            }
            // Arguments are key=value pairs without line breaks.
            assert!(
                p.bridge_args()
                    .iter()
                    .all(|a| a.contains('=') && !a.contains('\n'))
            );
        }
        assert!(preset("jdbc").unwrap().class.is_empty());
        assert_eq!(preset("jdbc-h2").unwrap().class, "org.h2.Driver");
        assert!(preset("nope").is_none());
    }

    #[test]
    fn registry_ids_match_the_known_databases() {
        // The ids the catalog (sqlr-core/known.rs) offers for install.
        for id in [
            "jdbc",
            "jdbc-sqlserver",
            "jdbc-oracle",
            "jdbc-db2",
            "jdbc-clickhouse",
            "jdbc-snowflake",
            "jdbc-duckdb",
            "jdbc-h2",
        ] {
            assert!(preset(id).is_some(), "{id}");
        }
    }
}
