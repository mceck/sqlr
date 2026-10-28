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
    Cell, ChunkBuilder, Connection, DocChunk, DocExecution, DocStream, Driver, DriverError,
    ErrorKind, Execution, Family, ResultChunk, RowStream, encode_chunk, encode_doc_chunk,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::dialect::DialectSpec;
use crate::frame::{CHUNK, CONTROL, DOC_CHUNK, MAX_FRAME, encode_frame, read_frame};
use crate::messages::{DocValues, DocumentNode, PROTOCOL, Request, Response, decode, encode};

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

struct DocumentStream {
    conn: u64,
    stream: tokio::sync::Mutex<Box<dyn DocStream>>,
}

struct DocumentQueue {
    out: mpsc::UnboundedSender<(u64, Request)>,
    worker: tokio::task::AbortHandle,
}

impl Drop for DocumentQueue {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

struct State {
    driver: Arc<dyn Driver>,
    protocol: String,
    conns: Mutex<HashMap<u64, Arc<dyn Connection>>>,
    streams: Mutex<HashMap<u64, Arc<Stream>>>,
    doc_streams: Mutex<HashMap<u64, Arc<DocumentStream>>>,
    document_queues: Mutex<HashMap<u64, DocumentQueue>>,
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
        doc_streams: Mutex::default(),
        document_queues: Mutex::default(),
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
        if let Request::Hello {
            protocol: ref peer, ..
        } = request
        {
            let compatible = peer.split('.').next() == Some("1")
                && (state.driver.manifest().family != Family::Document
                    || crate::messages::supports_documents(peer));
            if !compatible {
                state.reply(
                    id,
                    &Response::Error(DriverError::new(
                        ErrorKind::Protocol,
                        "Document drivers require protocol 1.1 or newer within major version 1",
                    )),
                );
                break Ok(());
            }
        }
        if let Some(conn) = document_connection(&request) {
            state.enqueue_document(conn, id, request);
            continue;
        }
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
    state.document_queues.lock().clear();
    let streams: Vec<_> = state.streams.lock().drain().map(|(_, s)| s).collect();
    for s in streams {
        let _ = s.stream.lock().await.close().await;
    }
    let docs: Vec<_> = state.doc_streams.lock().drain().map(|(_, s)| s).collect();
    for s in docs {
        let _ = s.stream.lock().await.close().await;
    }
    state.conns.lock().clear();
    drop(state);
    let _ = writer.await;
    result
}

impl State {
    // One queue per Document connection preserves toggle/write order even when several
    // requests are already buffered. Browse and Cancel bypass it, so slow statements
    // remain cancellable and catalog reads can run while a statement is executing.
    fn enqueue_document(self: &Arc<Self>, conn: u64, id: u64, request: Request) {
        let mut queues = self.document_queues.lock();
        let queue = queues.entry(conn).or_insert_with(|| {
            let (out, mut input) = mpsc::unbounded_channel::<(u64, Request)>();
            let state = Arc::downgrade(self);
            let worker = tokio::spawn(async move {
                while let Some((id, request)) = input.recv().await {
                    let Some(state) = state.upgrade() else {
                        break;
                    };
                    let response = state.handle(id, request).await;
                    state.reply(id, &response);
                }
            });
            DocumentQueue {
                out,
                worker: worker.abort_handle(),
            }
        });
        let _ = queue.out.send((id, request));
    }

    fn reply(&self, id: u64, response: &Response) {
        let mut buf = Vec::new();
        if encode_frame(CONTROL, id, &encode(response), &mut buf).is_err() {
            let error = Response::Error(DriverError::new(
                ErrorKind::Other,
                "a response is larger than the 64 MiB frame limit of the sidecar protocol",
            ));
            if encode_frame(CONTROL, id, &encode(&error), &mut buf).is_err() {
                return;
            }
        }
        let _ = self.out.send(buf);
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

    fn send_doc_chunk(&self, id: u64, chunk: &DocChunk) -> Result<(), DriverError> {
        let mut bytes = Vec::new();
        encode_doc_chunk(chunk, &mut bytes);
        if bytes.len() + 9 > MAX_FRAME && chunk.len() > 1 {
            let mid = chunk.len() / 2;
            let docs: Vec<_> = chunk.docs().collect();
            self.send_doc_chunk(id, &DocChunk::from_docs(&docs[..mid]))?;
            return self.send_doc_chunk(id, &DocChunk::from_docs(&docs[mid..]));
        }
        let mut frame = Vec::new();
        encode_frame(DOC_CHUNK, id, &bytes, &mut frame).map_err(|_| DriverError::new(
            ErrorKind::Other,
            "a document is larger than the 64 MiB frame limit of the sidecar protocol; select fewer or smaller fields"))?;
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
                let connection = self.conn(conn)?;
                let nodes = connection.browse(&path).await?;
                match connection.document() {
                    Some(doc) => Response::DocumentNodes(
                        nodes
                            .into_iter()
                            .map(|node| DocumentNode {
                                target: doc.node_target(&node.path),
                                node,
                            })
                            .collect(),
                    ),
                    None => Response::Nodes(nodes),
                }
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
            Request::DocRun {
                conn,
                database,
                text,
            } => {
                let connection = self.conn(conn)?;
                let doc = connection.document().ok_or_else(document_unsupported)?;
                match doc.run(database.as_deref(), &text).await? {
                    DocExecution::Docs(stream) => {
                        let id = self.next_stream.fetch_add(1, Ordering::Relaxed);
                        self.doc_streams.lock().insert(
                            id,
                            Arc::new(DocumentStream {
                                conn,
                                stream: tokio::sync::Mutex::new(stream),
                            }),
                        );
                        Response::Docs { stream: id }
                    }
                    DocExecution::Rows(stream) => {
                        let columns = stream.columns().to_vec();
                        let id = self.next_stream.fetch_add(1, Ordering::Relaxed);
                        self.streams.lock().insert(
                            id,
                            Arc::new(Stream {
                                conn,
                                stream: tokio::sync::Mutex::new(stream),
                            }),
                        );
                        Response::Rows {
                            stream: id,
                            columns,
                        }
                    }
                    DocExecution::Written {
                        matched,
                        modified,
                        inserted,
                        deleted,
                        ids,
                    } => Response::Written {
                        matched,
                        modified,
                        inserted,
                        deleted,
                        ids: DocValues::new(&ids),
                    },
                    DocExecution::Value(value) => Response::DocValue(DocValues::new(&[value])),
                }
            }
            Request::DocFetch { conn, stream, max } => {
                let connection = self.conn(conn)?;
                connection.document().ok_or_else(document_unsupported)?;
                let s = self
                    .doc_streams
                    .lock()
                    .get(&stream)
                    .filter(|s| s.conn == conn)
                    .cloned()
                    .ok_or_else(|| {
                        DriverError::new(ErrorKind::Protocol, "unknown document stream")
                    })?;
                let mut source = s.stream.lock().await;
                let chunk = source.next_chunk(max.max(1) as usize).await?;
                let exhausted = chunk.is_none();
                if let Some(chunk) = chunk {
                    self.send_doc_chunk(id, &chunk)?;
                }
                let notices = source.take_notices();
                if exhausted {
                    source.close().await?;
                    self.doc_streams.lock().remove(&stream);
                }
                Response::DocFetchDone { exhausted, notices }
            }
            Request::DocClose { conn, stream } => {
                let connection = self.conn(conn)?;
                connection.document().ok_or_else(document_unsupported)?;
                let owned = {
                    let mut streams = self.doc_streams.lock();
                    if streams.get(&stream).is_some_and(|s| s.conn != conn) {
                        return Err(DriverError::new(
                            ErrorKind::Protocol,
                            "unknown document stream",
                        ));
                    }
                    streams.remove(&stream)
                };
                if let Some(s) = owned {
                    s.stream.lock().await.close().await?;
                }
                Response::Ok
            }
            Request::DocReplace {
                conn,
                target,
                values,
            } => {
                let c = self.conn(conn)?;
                let doc = c.document().ok_or_else(document_unsupported)?;
                let [id, value] = values.exactly()?;
                Response::Affected(doc.replace_doc(&target, &id, &value).await?)
            }
            Request::DocInsert {
                conn,
                target,
                values,
            } => {
                let c = self.conn(conn)?;
                let doc = c.document().ok_or_else(document_unsupported)?;
                let [value] = values.exactly()?;
                Response::DocValue(DocValues::new(&[doc.insert_doc(&target, &value).await?]))
            }
            Request::DocDelete {
                conn,
                target,
                values,
            } => {
                let c = self.conn(conn)?;
                let doc = c.document().ok_or_else(document_unsupported)?;
                let [id] = values.exactly()?;
                Response::Affected(doc.delete_doc(&target, &id).await?)
            }
            Request::DocIsView { conn, target } => {
                let c = self.conn(conn)?;
                Response::DocIsView(
                    c.document()
                        .ok_or_else(document_unsupported)?
                        .is_view(&target)
                        .await?,
                )
            }
            Request::DocIndexes { conn, target } => {
                let c = self.conn(conn)?;
                Response::DocIndexes(DocValues::new(
                    &c.document()
                        .ok_or_else(document_unsupported)?
                        .indexes(&target)
                        .await?,
                ))
            }
            Request::DocSetReadOnly { conn, on } => {
                let c = self.conn(conn)?;
                c.document()
                    .ok_or_else(document_unsupported)?
                    .set_read_only(on);
                Response::Ok
            }
            Request::Disconnect { conn } => {
                self.document_queues.lock().remove(&conn);
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
                let owned: Vec<_> = {
                    let mut streams = self.doc_streams.lock();
                    let ids: Vec<_> = streams
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

fn document_unsupported() -> DriverError {
    DriverError::new(
        ErrorKind::Unsupported,
        "the driver has no document interface",
    )
}

fn document_connection(request: &Request) -> Option<u64> {
    match request {
        Request::DocRun { conn, .. }
        | Request::DocFetch { conn, .. }
        | Request::DocClose { conn, .. }
        | Request::DocReplace { conn, .. }
        | Request::DocInsert { conn, .. }
        | Request::DocDelete { conn, .. }
        | Request::DocIsView { conn, .. }
        | Request::DocIndexes { conn, .. }
        | Request::DocSetReadOnly { conn, .. } => Some(*conn),
        _ => None,
    }
}
