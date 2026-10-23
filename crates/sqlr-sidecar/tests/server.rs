//! The driver side of the protocol, in process: a raw client over a pipe sends requests
//! to `serve_io` and checks the answers, including the error paths.

use std::sync::Arc;

use sqlr_core::{ConnectionConfig, ErrorKind, NodePath, Secrets, TableRef, Value};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_sidecar::frame::{CHUNK, CONTROL, Frame, read_frame, write_frame};
use sqlr_sidecar::messages::{decode, encode};
use sqlr_sidecar::{PROTOCOL, Request, Response, serve_io};
use tokio::io::{DuplexStream, WriteHalf, split};

struct Rig {
    tx: WriteHalf<DuplexStream>,
    rx: tokio::io::ReadHalf<DuplexStream>,
    next: u64,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    _dir: tempfile::TempDir,
}

impl Rig {
    async fn new() -> Self {
        let (client, server_end) = tokio::io::duplex(1 << 20);
        let (sr, sw) = split(server_end);
        let server = tokio::spawn(serve_io(SqliteDriver::new(), PROTOCOL.to_string(), sr, sw));
        let (rx, tx) = split(client);
        Self {
            tx,
            rx,
            next: 1,
            server,
            _dir: tempfile::tempdir().unwrap(),
        }
    }

    /// Sends `request`; returns the chunk frames and the control answer.
    async fn call(&mut self, request: Request) -> (Vec<Frame>, Response) {
        let id = self.next;
        self.next += 1;
        write_frame(&mut self.tx, CONTROL, id, &encode(&request))
            .await
            .unwrap();
        self.answer(id).await
    }

    async fn answer(&mut self, id: u64) -> (Vec<Frame>, Response) {
        let mut chunks = Vec::new();
        loop {
            let frame = read_frame(&mut self.rx).await.unwrap().expect("an answer");
            assert_eq!(frame.id, id);
            if frame.ty == CHUNK {
                chunks.push(frame);
            } else {
                return (chunks, decode(&frame.payload).unwrap());
            }
        }
    }

    async fn ok(&mut self, request: Request) -> Response {
        self.call(request).await.1
    }

    async fn connect(&mut self, conn: u64) -> Response {
        let path = self._dir.path().join(format!("db{conn}.db"));
        let config = ConnectionConfig {
            id: "c".into(),
            name: "c".into(),
            driver: "sqlite".into(),
            group: None,
            color: None,
            values: [("path".to_string(), path.display().to_string())].into(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        self.ok(Request::Connect {
            conn,
            config,
            secrets: Secrets::default(),
        })
        .await
    }
}

fn error_kind(response: Response) -> ErrorKind {
    match response {
        Response::Error(e) => e.kind,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn exec(conn: u64, sql: &str, params: Option<Vec<Value>>) -> Request {
    Request::Execute {
        conn,
        sql: sql.into(),
        params,
    }
}

#[tokio::test]
async fn unknown_connections_and_streams_are_protocol_errors() {
    let mut rig = Rig::new().await;
    for request in [
        Request::Ping { conn: 9 },
        Request::Browse {
            conn: 9,
            path: NodePath::root(),
        },
        exec(9, "select 1", None),
        Request::Cancel { conn: 9 },
        Request::Describe {
            conn: 9,
            table: TableRef {
                database: None,
                schema: None,
                name: "t".into(),
            },
        },
        Request::Catalog {
            conn: 9,
            schema: "main".into(),
        },
        Request::TakeNotices { conn: 9 },
        Request::ConnectDatabase {
            conn: 9,
            new_conn: 10,
            database: "x".into(),
        },
        Request::Fetch { stream: 9, max: 1 },
    ] {
        let response = rig.ok(request.clone()).await;
        assert_eq!(error_kind(response), ErrorKind::Protocol, "{request:?}");
    }
    // Closing or disconnecting what does not exist is harmless.
    assert!(matches!(
        rig.ok(Request::CloseStream { stream: 9 }).await,
        Response::Ok
    ));
    assert!(matches!(
        rig.ok(Request::Disconnect { conn: 9 }).await,
        Response::Ok
    ));
}

#[tokio::test]
async fn malformed_requests_and_foreign_frames_are_survivable() {
    let mut rig = Rig::new().await;
    // A frame of another type is skipped without an answer.
    write_frame(&mut rig.tx, 0x04, 77, b"log line")
        .await
        .unwrap();
    write_frame(&mut rig.tx, CONTROL, 5, b"\xc1not msgpack")
        .await
        .unwrap();
    let (_, response) = rig.answer(5).await;
    assert_eq!(error_kind(response), ErrorKind::Protocol);
    let hello = rig
        .ok(Request::Hello {
            protocol: PROTOCOL.into(),
            sqlr_version: "0".into(),
        })
        .await;
    assert!(matches!(hello, Response::Welcome { .. }));
}

#[tokio::test]
async fn a_full_session_runs_over_the_pipe() {
    let mut rig = Rig::new().await;
    let Response::Connected {
        dialect,
        server_info,
        ..
    } = rig.connect(1).await
    else {
        panic!("connected");
    };
    assert!(dialect.is_some());
    assert!(!server_info.version.is_empty());
    assert!(matches!(
        rig.ok(Request::Ping { conn: 1 }).await,
        Response::Pong(_)
    ));
    assert!(matches!(
        rig.ok(Request::Browse {
            conn: 1,
            path: NodePath::root()
        })
        .await,
        Response::Nodes(_)
    ));

    let r = rig
        .ok(exec(1, "create table t (n integer, s text)", None))
        .await;
    assert!(matches!(r, Response::Affected(_)));
    let r = rig
        .ok(exec(
            1,
            "insert into t values (?, ?)",
            Some(vec![Value::Int(1), Value::Text("a".into())]),
        ))
        .await;
    assert!(matches!(r, Response::Affected(1)), "{r:?}");
    rig.ok(exec(1, "insert into t values (2, 'b')", None)).await;

    let Response::Rows { stream, columns } = rig.ok(exec(1, "select * from t", None)).await else {
        panic!("rows");
    };
    assert_eq!(columns.len(), 2);
    let (chunks, done) = rig.call(Request::Fetch { stream, max: 10 }).await;
    assert_eq!(chunks.len(), 1);
    assert!(matches!(done, Response::FetchDone { exhausted: false }));
    let (chunks, done) = rig.call(Request::Fetch { stream, max: 10 }).await;
    assert!(chunks.is_empty());
    assert!(matches!(done, Response::FetchDone { exhausted: true }));
    assert!(matches!(
        rig.ok(Request::CloseStream { stream }).await,
        Response::Ok
    ));

    // A statement that fails is an error answer, not a dead server.
    assert!(matches!(
        rig.ok(exec(1, "select * from nope", None)).await,
        Response::Error(_)
    ));

    let table = TableRef {
        database: None,
        schema: None,
        name: "t".into(),
    };
    assert!(matches!(
        rig.ok(Request::Describe { conn: 1, table }).await,
        Response::Table(_)
    ));
    assert!(matches!(
        rig.ok(Request::Catalog {
            conn: 1,
            schema: "main".into()
        })
        .await,
        Response::Catalog(_)
    ));
    assert!(matches!(
        rig.ok(Request::TakeNotices { conn: 1 }).await,
        Response::Notices(_)
    ));
    assert!(matches!(
        rig.ok(Request::Cancel { conn: 1 }).await,
        Response::Ok
    ));

    // A second connection on the same server, then streams die with their connection.
    let r = rig
        .ok(Request::ConnectDatabase {
            conn: 1,
            new_conn: 2,
            database: "other".into(),
        })
        .await;
    assert!(matches!(r, Response::Connected { .. } | Response::Error(_)));
    let Response::Rows { stream, .. } = rig.ok(exec(1, "select * from t", None)).await else {
        panic!("rows");
    };
    assert!(matches!(
        rig.ok(Request::Disconnect { conn: 1 }).await,
        Response::Ok
    ));
    let r = rig.ok(Request::Fetch { stream, max: 1 }).await;
    assert_eq!(error_kind(r), ErrorKind::Protocol);
    assert_eq!(
        error_kind(rig.ok(Request::Ping { conn: 1 }).await),
        ErrorKind::Protocol
    );

    assert!(matches!(rig.ok(Request::Shutdown).await, Response::Ok));
    rig.server.await.unwrap().unwrap();
}

#[tokio::test]
async fn open_streams_are_closed_when_the_pipe_ends() {
    let mut rig = Rig::new().await;
    rig.connect(1).await;
    rig.ok(exec(1, "select 1", None)).await;
    let Rig { tx, rx, server, .. } = rig;
    drop(tx);
    drop(rx);
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_corrupt_frame_ends_the_server_with_an_error() {
    let mut rig = Rig::new().await;
    use tokio::io::AsyncWriteExt;
    // A length header that promises more than ever arrives, then EOF.
    rig.tx.write_all(&[0, 0, 0, 40, 1, 2]).await.unwrap();
    rig.tx.shutdown().await.unwrap();
    let err = rig.server.await.unwrap().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let _ = Arc::new(());
}
