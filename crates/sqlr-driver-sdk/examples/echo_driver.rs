//! A scripted driver for the sidecar tests. SQL it understands:
//!
//! - `echo <text>`: one row, one text column
//! - `rows <n>`: a stream of `n` integer rows
//! - `affected <n>`: an affected-rows count
//! - `error <message>`: a syntax error
//! - `sleep`: runs until cancelled (never, with `ECHO_IGNORE_CANCEL=1`)
//! - `crash`: the process exits
//! - `bigrow`: one row of 70 MiB of text (more than a frame holds)
//! - `badrows`: announces one column, then sends a chunk of two
//!
//! `ECHO_HANG_CANCEL=1` makes a cancel never return.
//!
//! `ECHO_PROTOCOL` overrides the protocol version the handshake announces.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use sqlr_driver_sdk::*;

struct Echo {
    manifest: DriverManifest,
    ignore_cancel: bool,
    hang_cancel: bool,
}

struct EchoConn {
    cancelled: Arc<AtomicBool>,
    ignore_cancel: bool,
    hang_cancel: bool,
    dialect: EchoDialect,
}

struct EchoDialect;

impl Dialect for EchoDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }
    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String> {
        node.kind.opens_data().then(|| {
            let mut sql = format!("SELECT * FROM {}", self.quote_ident(&node.name));
            if let Some(n) = limit {
                sql.push_str(&format!(" LIMIT {n}"));
            }
            sql
        })
    }
    fn placeholder(&self, index: usize) -> String {
        format!("${index}")
    }
}

struct Cancel {
    flag: Arc<AtomicBool>,
    ignore: bool,
    hang: bool,
}

#[async_trait]
impl CancelHandle for Cancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        if self.hang {
            std::future::pending::<()>().await;
        }
        if !self.ignore {
            self.flag.store(true, Ordering::Release);
        }
        Ok(())
    }
}

struct Rows {
    columns: Arc<[ColumnMeta]>,
    next: i64,
    end: i64,
}

#[async_trait]
impl RowStream for Rows {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }
    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        if self.next >= self.end {
            return Ok(None);
        }
        let mut b = ChunkBuilder::new(self.columns.clone());
        while self.next < self.end && b.len() < max_rows {
            b.push_row(&[Cell::Int(self.next)]).expect("one int column");
            self.next += 1;
        }
        Ok(Some(b.finish()))
    }
    async fn close(&mut self) -> Result<(), DriverError> {
        self.next = self.end;
        Ok(())
    }
}

#[async_trait]
impl Driver for Echo {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }
    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        _secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        if cfg.values.get("block_connect").is_some_and(|v| v == "true") {
            std::future::pending::<()>().await;
        }
        Ok(Arc::new(EchoConn {
            cancelled: Arc::default(),
            ignore_cancel: self.ignore_cancel,
            hang_cancel: self.hang_cancel,
            dialect: EchoDialect,
        }))
    }
}

#[async_trait]
impl Connection for EchoConn {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        Ok(ServerInfo {
            product: "echo".into(),
            version: "1".into(),
        })
    }
    async fn browse(&self, _path: &NodePath) -> Result<Vec<Node>, DriverError> {
        Ok(vec![Node {
            path: NodePath(vec!["t".into()]),
            name: "t".into(),
            kind: NodeKind::Table,
            has_children: false,
            detail: None,
        }])
    }
    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(Cancel {
            flag: self.cancelled.clone(),
            ignore: self.ignore_cancel,
            hang: self.hang_cancel,
        })
    }
    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }
}

#[async_trait]
impl RelationalConn for EchoConn {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        let (word, rest) = sql.split_once(' ').unwrap_or((sql, ""));
        let one_column = |name: &str, kind| -> Arc<[ColumnMeta]> {
            Arc::from(vec![ColumnMeta::new(name, name, kind, DisplayHint::Plain)])
        };
        match word {
            "echo" => {
                let columns = one_column("text", ColumnKind::Text);
                let mut b = ChunkBuilder::new(columns.clone());
                b.push_row(&[Cell::Text(rest)]).expect("one text column");
                Ok(Execution::Rows(Box::new(OneChunk {
                    columns,
                    chunk: Some(b.finish()),
                })))
            }
            "rows" => Ok(Execution::Rows(Box::new(Rows {
                columns: one_column("n", ColumnKind::Int),
                next: 0,
                end: rest.parse().unwrap_or(0),
            }))),
            "affected" => Ok(Execution::Affected(rest.parse().unwrap_or(0))),
            "error" => Err(DriverError::new(ErrorKind::Syntax, rest).with_position(3)),
            "sleep" => {
                self.cancelled.store(false, Ordering::Release);
                for _ in 0..6000 {
                    if self.cancelled.load(Ordering::Acquire) {
                        return Err(DriverError::new(ErrorKind::Cancelled, "cancelled"));
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok(Execution::Affected(0))
            }
            "crash" => std::process::exit(3),
            "bigrow" => {
                let columns = one_column("text", ColumnKind::Text);
                let mut b = ChunkBuilder::new(columns.clone());
                let big = "x".repeat(70 * 1024 * 1024);
                b.push_row(&[Cell::Text(&big)]).expect("one text column");
                Ok(Execution::Rows(Box::new(OneChunk {
                    columns,
                    chunk: Some(b.finish()),
                })))
            }
            "badrows" => {
                let announced = one_column("n", ColumnKind::Int);
                let two: Arc<[ColumnMeta]> = Arc::from(vec![
                    ColumnMeta::new("a", "a", ColumnKind::Int, DisplayHint::Plain),
                    ColumnMeta::new("b", "b", ColumnKind::Int, DisplayHint::Plain),
                ]);
                let mut b = ChunkBuilder::new(two);
                b.push_row(&[Cell::Int(1), Cell::Int(2)])
                    .expect("two int columns");
                Ok(Execution::Rows(Box::new(OneChunk {
                    columns: announced,
                    chunk: Some(b.finish()),
                })))
            }
            _ => Err(DriverError::new(
                ErrorKind::Syntax,
                format!("unknown: {sql}"),
            )),
        }
    }
    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }
}

struct OneChunk {
    columns: Arc<[ColumnMeta]>,
    chunk: Option<ResultChunk>,
}

#[async_trait]
impl RowStream for OneChunk {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }
    async fn next_chunk(&mut self, _max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        Ok(self.chunk.take())
    }
    async fn close(&mut self) -> Result<(), DriverError> {
        self.chunk = None;
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let driver = Echo {
        manifest: DriverManifest {
            id: "echo".into(),
            name: "Echo".into(),
            family: Family::Relational,
            capabilities: Capabilities {
                server_cancel: true,
                ..Capabilities::default()
            },
            fields: Vec::new(),
            dialect: "ansi".into(),
            url_schemes: Vec::new(),
            default_port: None,
            editing: None,
        },
        ignore_cancel: std::env::var_os("ECHO_IGNORE_CANCEL").is_some(),
        hang_cancel: std::env::var_os("ECHO_HANG_CANCEL").is_some(),
    };
    let protocol = std::env::var("ECHO_PROTOCOL").unwrap_or_else(|_| PROTOCOL.to_string());
    let _ = serve_io(
        Arc::new(driver),
        protocol,
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await;
}
