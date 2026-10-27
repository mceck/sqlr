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
    /// The first message: the client offers its protocol version and expects `Welcome`.
    Hello {
        /// The client's protocol version, as in [`PROTOCOL`].
        protocol: String,
        /// The version of sqlr that started the driver.
        sqlr_version: String,
    },
    /// Connection secrets cross the pipe here.
    Connect {
        /// The id the client gives the new connection.
        conn: u64,
        /// The saved connection configuration.
        config: ConnectionConfig,
        /// The secrets for the connection.
        secrets: Secrets,
    },
    /// Another database on the same server, as connection `new_conn`.
    ConnectDatabase {
        /// The id of the connection to open it from.
        conn: u64,
        /// The id the client gives the new connection.
        new_conn: u64,
        /// The database to connect to.
        database: String,
    },
    /// Checks that a connection still answers.
    Ping {
        /// The connection to ping.
        conn: u64,
    },
    /// Lists the children of a node.
    Browse {
        /// The connection to browse.
        conn: u64,
        /// The node whose children are listed.
        path: NodePath,
    },
    /// Runs `sql`; `params` binds placeholders when present.
    Execute {
        /// The connection to run on.
        conn: u64,
        /// The statement text.
        sql: String,
        /// Values bound to the placeholders of `sql`, in order.
        params: Option<Vec<Value>>,
    },
    /// Up to `max` more rows of a stream: chunk frames, then `FetchDone`.
    Fetch {
        /// The stream id from `Rows`.
        stream: u64,
        /// The most rows to send.
        max: u32,
    },
    /// Releases a stream's server resources.
    CloseStream {
        /// The stream to close.
        stream: u64,
    },
    /// Cancels the statement a connection is running.
    Cancel {
        /// The connection to cancel on.
        conn: u64,
    },
    /// Asks for the description of a table.
    Describe {
        /// The connection to describe on.
        conn: u64,
        /// The table to describe.
        table: TableRef,
    },
    /// Asks for the tables, views and columns of a schema.
    Catalog {
        /// The connection to read from.
        conn: u64,
        /// The schema to read.
        schema: String,
    },
    /// Asks for the messages the server sent since the last call.
    TakeNotices {
        /// The connection to read from.
        conn: u64,
    },
    /// Closes a connection and its streams.
    Disconnect {
        /// The connection to close.
        conn: u64,
    },
    /// Asks the driver to exit.
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
    /// The answer to `Hello`.
    Welcome {
        /// The driver's protocol version.
        protocol: String,
        /// The manifest of the driver.
        manifest: DriverManifest,
    },
    /// The answer to `Connect` and `ConnectDatabase`.
    Connected {
        /// What the server reports about itself.
        server_info: ServerInfo,
        /// Present for relational drivers.
        dialect: Option<DialectSpec>,
        /// The database the connection is attached to, when the driver browses several.
        database_name: Option<String>,
        /// The path whose children are the connection's schemas.
        schema_path: NodePath,
    },
    /// The answer to `Ping`.
    Pong(ServerInfo),
    /// The answer to `Browse`.
    Nodes(Vec<Node>),
    /// The answer to `Execute` for a statement that returns rows; `Fetch` reads them.
    Rows {
        /// The id of the stream to fetch from.
        stream: u64,
        /// The columns of the result.
        columns: Vec<ColumnMeta>,
    },
    /// The answer to `Execute` for a statement that returns no rows: how many it changed.
    Affected(u64),
    /// The end of the chunk frames sent for a `Fetch`.
    FetchDone {
        /// Whether the stream has no more rows.
        exhausted: bool,
    },
    /// The answer to requests that return nothing.
    Ok,
    /// The answer to `Describe`.
    Table(TableInfo),
    /// The answer to `Catalog`.
    Catalog(SchemaCatalog),
    /// The answer to `TakeNotices`.
    Notices(Vec<String>),
    /// The request failed.
    Error(DriverError),
}

/// Encodes `message` as MessagePack with named fields.
pub fn encode<T: Serialize>(message: &T) -> Vec<u8> {
    rmp_serde::to_vec_named(message).expect("protocol messages serialize")
}

/// Decodes a MessagePack message.
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
