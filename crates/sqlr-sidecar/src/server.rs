//! The driver side of the protocol: reads requests from stdin, runs them against a
//! [`Driver`] and writes responses to stdout. Requests run concurrently, so a `Cancel`
//! reaches a statement that is still executing. Nothing but protocol frames may be written
//! to stdout; a driver logs to stderr.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use sqlr_core::{
    Cell, ChunkBuilder, Connection, Driver, DriverError, ErrorKind, Execution, ResultChunk,
    RowStream, encode_chunk,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::dialect::DialectSpec;
use crate::frame::{CHUNK, CONTROL, MAX_FRAME, encode_frame, read_frame};
use crate::messages::{PROTOCOL, Request, Response, decode, encode};

/// Serves `driver` over stdin and stdout until the client shuts it down or closes the pipe.
pub async fn serve<D: Driver + 'static>(driver: D) -> io::Result<()> {
    serve_arc(Arc::new(driver)).await
}

/// [`serve`] for a driver that is already shared.
pub async fn serve_arc(driver: Arc<dyn Driver>) -> io::Result<()> {
    serve_io(
        driver,
        PROTOCOL.to_string(),
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await
}

struct Stream {
    conn: u64,
    stream: tokio::sync::Mutex<Box<dyn RowStream>>,
}

struct State {
    driver: Arc<dyn Driver>,
    protocol: String,
    conns: Mutex<HashMap<u64, Arc<dyn Connection>>>,
    streams: Mutex<HashMap<u64, Arc<Stream>>>,
    next_stream: AtomicU64,
    out: mpsc::UnboundedSender<Vec<u8>>,
}

/// Serves over any pair of pipes. `protocol` is what the handshake announces (tests announce
/// another major version to see the client refuse it).
pub async fn serve_io<R, W>(
    driver: Arc<dyn Driver>,
    protocol: String,
    mut input: R,
    mut output: W,
) -> io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(frame) = queue.recv().await {
            if output.write_all(&frame).await.is_err() || output.flush().await.is_err() {
                break;
            }
        }
    });
    let state = Arc::new(State {
        driver,
        protocol,
        conns: Mutex::default(),
        streams: Mutex::default(),
        next_stream: AtomicU64::new(1),
        out,
    });
    let mut connecting = HashMap::<u64, tokio::task::AbortHandle>::new();
    let result = loop {
        let frame = match read_frame(&mut input).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break Ok(()),
            Err(e) => break Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
        };
        if frame.ty != CONTROL {
            continue;
        }
        let id = frame.id;
        let request: Request = match decode(&frame.payload) {
            Ok(request) => request,
            Err(e) => {
                state.reply(
                    id,
                    &Response::Error(DriverError::new(
                        ErrorKind::Protocol,
                        format!("malformed request: {e}"),
                    )),
                );
                continue;
            }
        };
        if matches!(request, Request::Shutdown) {
            state.reply(id, &Response::Ok);
            break Ok(());
        }
        connecting.retain(|_, task| !task.is_finished());
        // Register in wire order so Disconnect also cancels a still pending Connect.
        let opening = match &request {
            Request::Connect { conn, .. } => Some(*conn),
            Request::ConnectDatabase { new_conn, .. } => Some(*new_conn),
            Request::Disconnect { conn } => {
                if let Some(task) = connecting.remove(conn) {
                    task.abort();
                }
                None
            }
            _ => None,
        };
        let state = state.clone();
        let task = tokio::spawn(async move {
            let response = state.handle(id, request).await;
            state.reply(id, &response);
        });
        if let Some(conn) = opening {
            connecting.insert(conn, task.abort_handle());
        }
    };
    // Let queued replies (the shutdown acknowledgement) reach the pipe, then drop the
    // connections: a driver releases its server resources when its objects drop.
    for task in connecting.into_values() {
        task.abort();
    }
    let streams: Vec<_> = state.streams.lock().drain().map(|(_, s)| s).collect();
    for s in streams {
        let _ = s.stream.lock().await.close().await;
    }
    state.conns.lock().clear();
    drop(state);
    let _ = writer.await;
    result
}

impl State {
    fn reply(&self, id: u64, response: &Response) {
        let mut buf = Vec::new();
        if encode_frame(CONTROL, id, &encode(response), &mut buf).is_ok() {
            let _ = self.out.send(buf);
        }
    }

    /// Sends `chunk`, split in halves while a frame would pass the size limit.
    ///
    /// A single row larger than a frame cannot be sent: that is an error for the fetch,
    /// not an empty page the client would take for the end of the data.
    fn send_chunk(&self, id: u64, chunk: &ResultChunk) -> Result<(), DriverError> {
        let mut bytes = Vec::new();
        encode_chunk(chunk, &mut bytes);
        if bytes.len() + 16 > MAX_FRAME && chunk.len() > 1 {
            let (a, b) = split(chunk);
            self.send_chunk(id, &a)?;
            return self.send_chunk(id, &b);
        }
        let mut frame = Vec::new();
        encode_frame(CHUNK, id, &bytes, &mut frame).map_err(|_| {
            DriverError::new(
                ErrorKind::Other,
                "a row is larger than the 64 MiB frame limit of the sidecar protocol; \
                 select fewer or smaller columns",
            )
        })?;
        let _ = self.out.send(frame);
        Ok(())
    }

    fn conn(&self, conn: u64) -> Result<Arc<dyn Connection>, DriverError> {
        self.conns
            .lock()
            .get(&conn)
            .cloned()
            .ok_or_else(|| DriverError::new(ErrorKind::Protocol, format!("no connection {conn}")))
    }

    async fn handle(&self, id: u64, request: Request) -> Response {
        match self.run(id, request).await {
            Ok(response) => response,
            Err(e) => Response::Error(e),
        }
    }

    async fn run(&self, id: u64, request: Request) -> Result<Response, DriverError> {
        Ok(match request {
            Request::Hello { .. } => Response::Welcome {
                protocol: self.protocol.clone(),
                manifest: self.driver.manifest().clone(),
            },
            Request::Connect {
                conn,
                config,
                secrets,
            } => {
                let connection = self.driver.connect(&config, &secrets).await?;
                self.attach(conn, connection).await?
            }
            Request::ConnectDatabase {
                conn,
                new_conn,
                database,
            } => {
                let connection = self.conn(conn)?.connect_database(&database).await?;
                self.attach(new_conn, connection).await?
            }
            Request::Ping { conn } => Response::Pong(self.conn(conn)?.ping().await?),
            Request::Browse { conn, path } => {
                Response::Nodes(self.conn(conn)?.browse(&path).await?)
            }
            Request::Execute { conn, sql, params } => {
                let connection = self.conn(conn)?;
                let relational = connection.relational().ok_or_else(unsupported)?;
                let execution = match params {
                    Some(params) => relational.execute_with(&sql, &params).await?,
                    None => relational.execute(&sql).await?,
                };
                match execution {
                    Execution::Affected(n) => Response::Affected(n),
                    Execution::Rows(stream) => {
                        let columns = stream.columns().to_vec();
                        let stream_id = self.next_stream.fetch_add(1, Ordering::Relaxed);
                        self.streams.lock().insert(
                            stream_id,
                            Arc::new(Stream {
                                conn,
                                stream: tokio::sync::Mutex::new(stream),
                            }),
                        );
                        Response::Rows {
                            stream: stream_id,
                            columns,
                        }
                    }
                }
            }
            Request::Fetch { stream, max } => {
                let entry = self.streams.lock().get(&stream).cloned().ok_or_else(|| {
                    DriverError::new(ErrorKind::Protocol, format!("no stream {stream}"))
                })?;
                let chunk = entry.stream.lock().await.next_chunk(max as usize).await?;
                match chunk {
                    Some(chunk) => {
                        self.send_chunk(id, &chunk)?;
                        Response::FetchDone { exhausted: false }
                    }
                    None => Response::FetchDone { exhausted: true },
                }
            }
            Request::CloseStream { stream } => {
                let entry = self.streams.lock().remove(&stream);
                if let Some(entry) = entry {
                    entry.stream.lock().await.close().await?;
                }
                Response::Ok
            }
            Request::Cancel { conn } => {
                self.conn(conn)?.cancel_handle().cancel().await?;
                Response::Ok
            }
            Request::Describe { conn, table } => {
                let connection = self.conn(conn)?;
                let relational = connection.relational().ok_or_else(unsupported)?;
                Response::Table(relational.describe(&table).await?)
            }
            Request::Catalog { conn, schema } => {
                let connection = self.conn(conn)?;
                let relational = connection.relational().ok_or_else(unsupported)?;
                Response::Catalog(relational.catalog(&schema).await?)
            }
            Request::TakeNotices { conn } => {
                let connection = self.conn(conn)?;
                let relational = connection.relational().ok_or_else(unsupported)?;
                Response::Notices(relational.take_notices().await)
            }
            Request::Disconnect { conn } => {
                let owned: Vec<Arc<Stream>> = {
                    let mut streams = self.streams.lock();
                    let ids: Vec<u64> = streams
                        .iter()
                        .filter(|(_, s)| s.conn == conn)
                        .map(|(id, _)| *id)
                        .collect();
                    ids.iter().filter_map(|id| streams.remove(id)).collect()
                };
                for s in owned {
                    let _ = s.stream.lock().await.close().await;
                }
                self.conns.lock().remove(&conn);
                Response::Ok
            }
            Request::Shutdown => Response::Ok,
        })
    }

    async fn attach(
        &self,
        conn: u64,
        connection: Arc<dyn Connection>,
    ) -> Result<Response, DriverError> {
        let server_info = connection.ping().await?;
        let dialect = connection
            .relational()
            .map(|r| DialectSpec::from_dialect(r.dialect()));
        let response = Response::Connected {
            server_info,
            dialect,
            database_name: connection.database_name().map(str::to_string),
            schema_path: connection.schema_path(),
        };
        self.conns.lock().insert(conn, connection);
        Ok(response)
    }
}

fn unsupported() -> DriverError {
    DriverError::new(
        ErrorKind::Unsupported,
        "the driver has no relational interface",
    )
}

/// `chunk` as two chunks of its first and second half of rows.
fn split(chunk: &ResultChunk) -> (ResultChunk, ResultChunk) {
    let mid = chunk.len() / 2;
    let part = |rows: std::ops::Range<usize>| {
        let mut builder = ChunkBuilder::new(chunk.columns().clone());
        for row in rows {
            let cells: Vec<Cell<'_>> = (0..chunk.columns().len())
                .map(|col| chunk.cell(row, col))
                .collect();
            builder
                .push_row(&cells)
                .expect("cells of a chunk fit its columns");
        }
        builder.finish()
    };
    (part(0..mid), part(mid..chunk.len()))
}
