//! Control messages of the sidecar protocol: MessagePack in [`CONTROL`](crate::frame::CONTROL)
//! frames. Request `Debug` output omits SQL and parameter values; [`Secrets`] prints keys only.

use std::fmt;

use serde::{Deserialize, Serialize};
use sqlr_core::{
    ColumnMeta, ConnectionConfig, DriverError, DriverManifest, Node, NodePath, SchemaCatalog,
    Secrets, ServerInfo, TableInfo, TableRef, Value,
};

use crate::dialect::DialectSpec;

/// Semver of the protocol; a major difference refuses to start the driver.
pub const PROTOCOL: &str = "1.0.0";

/// Client → driver.
#[derive(Clone, Serialize, Deserialize)]
pub enum Request {
    Hello {
        protocol: String,
        sqlr_version: String,
    },
    /// Connection secrets cross the pipe here.
    Connect {
        conn: u64,
        config: ConnectionConfig,
        secrets: Secrets,
    },
    /// Another database on the same server, as connection `new_conn`.
    ConnectDatabase {
        conn: u64,
        new_conn: u64,
        database: String,
    },
    Ping {
        conn: u64,
    },
    Browse {
        conn: u64,
        path: NodePath,
    },
    /// Runs `sql`; `params` binds placeholders when present.
    Execute {
        conn: u64,
        sql: String,
        params: Option<Vec<Value>>,
    },
    /// Up to `max` more rows of a stream: chunk frames, then `FetchDone`.
    Fetch {
        stream: u64,
        max: u32,
    },
    CloseStream {
        stream: u64,
    },
    Cancel {
        conn: u64,
    },
    Describe {
        conn: u64,
        table: TableRef,
    },
    Catalog {
        conn: u64,
        schema: String,
    },
    TakeNotices {
        conn: u64,
    },
    Disconnect {
        conn: u64,
    },
    Shutdown,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hello {
                protocol,
                sqlr_version,
            } => f
                .debug_struct("Hello")
                .field("protocol", protocol)
                .field("sqlr_version", sqlr_version)
                .finish(),
            Self::Connect {
                conn,
                config,
                secrets,
            } => f
                .debug_struct("Connect")
                .field("conn", conn)
                .field("config", config)
                .field("secrets", secrets)
                .finish(),
            Self::ConnectDatabase {
                conn,
                new_conn,
                database,
            } => f
                .debug_struct("ConnectDatabase")
                .field("conn", conn)
                .field("new_conn", new_conn)
                .field("database", database)
                .finish(),
            Self::Ping { conn } => f.debug_struct("Ping").field("conn", conn).finish(),
            Self::Browse { conn, path } => f
                .debug_struct("Browse")
                .field("conn", conn)
                .field("path", path)
                .finish(),
            // SQL literals and every parameter type can contain credentials or private data.
            Self::Execute { conn, sql, params } => f
                .debug_struct("Execute")
                .field("conn", conn)
                .field("sql_bytes", &sql.len())
                .field("param_count", &params.as_ref().map(Vec::len))
                .finish(),
            Self::Fetch { stream, max } => f
                .debug_struct("Fetch")
                .field("stream", stream)
                .field("max", max)
                .finish(),
            Self::CloseStream { stream } => f
                .debug_struct("CloseStream")
                .field("stream", stream)
                .finish(),
            Self::Cancel { conn } => f.debug_struct("Cancel").field("conn", conn).finish(),
            Self::Describe { conn, table } => f
                .debug_struct("Describe")
                .field("conn", conn)
                .field("table", table)
                .finish(),
            Self::Catalog { conn, schema } => f
                .debug_struct("Catalog")
                .field("conn", conn)
                .field("schema", schema)
                .finish(),
            Self::TakeNotices { conn } => {
                f.debug_struct("TakeNotices").field("conn", conn).finish()
            }
            Self::Disconnect { conn } => f.debug_struct("Disconnect").field("conn", conn).finish(),
            Self::Shutdown => f.write_str("Shutdown"),
        }
    }
}

/// Driver → client.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Welcome {
        protocol: String,
        manifest: DriverManifest,
    },
    Connected {
        server_info: ServerInfo,
        /// Present for relational drivers.
        dialect: Option<DialectSpec>,
        database_name: Option<String>,
        schema_path: NodePath,
    },
    Pong(ServerInfo),
    Nodes(Vec<Node>),
    Rows {
        stream: u64,
        columns: Vec<ColumnMeta>,
    },
    Affected(u64),
    FetchDone {
        exhausted: bool,
    },
    Ok,
    Table(TableInfo),
    Catalog(SchemaCatalog),
    Notices(Vec<String>),
    Error(DriverError),
}

pub fn encode<T: Serialize>(message: &T) -> Vec<u8> {
    rmp_serde::to_vec_named(message).expect("protocol messages serialize")
}

pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, rmp_serde::decode::Error> {
    rmp_serde::from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_debug_hides_sql() {
        let request = Request::Execute {
            conn: 7,
            sql: "ALTER USER alice PASSWORD 'sql-password-marker'".into(),
            params: None,
        };
        for debug in [format!("{request:?}"), format!("{request:#?}")] {
            assert!(debug.contains("Execute"));
            assert!(!debug.contains("sql-password-marker"), "{debug}");
            assert!(!debug.contains("ALTER USER"), "{debug}");
        }
    }

    #[test]
    fn execute_debug_hides_parameters() {
        let request = Request::Execute {
            conn: 7,
            sql: "INSERT INTO credentials VALUES (?, ?, ?)".into(),
            params: Some(vec![
                Value::Text("bound-password-marker".into()),
                Value::Bytes(vec![222, 173, 190, 239]),
                Value::Int(987654321),
            ]),
        };
        for debug in [format!("{request:?}"), format!("{request:#?}")] {
            assert!(debug.contains("Execute"));
            for secret in ["bound-password-marker", "222", "987654321"] {
                assert!(!debug.contains(secret), "{debug}");
            }
        }
    }

    #[test]
    fn execute_serialization_preserves_payload() {
        let sql = "SELECT 'sql-password-marker', ?, ?";
        let params = Some(vec![
            Value::Text("bound-password-marker".into()),
            Value::Bytes(vec![222, 173, 190, 239]),
        ]);
        let request = Request::Execute {
            conn: 7,
            sql: sql.into(),
            params: params.clone(),
        };
        let encoded = encode(&request);
        let Request::Execute {
            conn,
            sql: decoded_sql,
            params: decoded_params,
        } = decode(&encoded).unwrap()
        else {
            panic!("expected Execute");
        };
        assert_eq!(conn, 7);
        assert_eq!(decoded_sql, sql);
        assert_eq!(decoded_params, params);
    }
}
