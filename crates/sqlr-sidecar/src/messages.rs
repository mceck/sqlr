//! Control messages of the sidecar protocol: MessagePack in [`CONTROL`](crate::frame::CONTROL)
//! frames. `Debug` output is safe to log: [`Secrets`] prints keys only.

use serde::{Deserialize, Serialize};
use sqlr_core::{
    ColumnMeta, ConnectionConfig, DriverError, DriverManifest, Node, NodePath, SchemaCatalog,
    Secrets, ServerInfo, TableInfo, TableRef, Value,
};

use crate::dialect::DialectSpec;

/// Semver of the protocol; a major difference refuses to start the driver.
pub const PROTOCOL: &str = "1.0.0";

/// Client → driver.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Hello {
        protocol: String,
        sqlr_version: String,
    },
    /// Secrets cross the pipe here and nowhere else.
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
