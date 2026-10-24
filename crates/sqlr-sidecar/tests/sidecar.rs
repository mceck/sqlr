//! The sidecar protocol end to end: real child processes (the SDK's example drivers).

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{
    Cell, Connection, ConnectionConfig, Driver, DriverError, ErrorKind, Execution, Node, NodeKind,
    NodePath, Secrets, Value,
};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_driver_tests::{Target, TypeCase, relational_suite};
use sqlr_sidecar::{SidecarDriver, SidecarSpec};

/// The SDK examples are built together with the workspace tests.
fn example(name: &str) -> PathBuf {
    sqlr_testkit::workspace_binary(&format!("examples/{name}"))
}

fn config() -> ConnectionConfig {
    ConnectionConfig {
        id: "c".into(),
        name: "c".into(),
        driver: "echo".into(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}

fn echo_spec() -> SidecarSpec {
    SidecarSpec::new("echo", "1.0.0", example("echo_driver"))
}

async fn echo() -> SidecarDriver {
    SidecarDriver::probe(echo_spec()).await.unwrap().0
}

async fn run(conn: &Arc<dyn Connection>, sql: &str) -> Result<Execution, DriverError> {
    conn.relational().unwrap().execute(sql).await
}

async fn count_rows(conn: &Arc<dyn Connection>, sql: &str, page: usize) -> usize {
    let Execution::Rows(mut stream) = run(conn, sql).await.unwrap() else {
        panic!("{sql} returned no rows");
    };
    let mut n = 0;
    while let Some(chunk) = stream.next_chunk(page).await.unwrap() {
        n += chunk.len();
    }
    n
}

#[tokio::test]
async fn handshake_and_manifest() {
    let (driver, manifest) = SidecarDriver::probe(echo_spec()).await.unwrap();
    assert_eq!(manifest.id, "echo");
    assert_eq!(driver.manifest().name, "Echo");
    assert!(!driver.is_running(), "probing stops the process again");

    let conn = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    assert!(driver.is_running(), "the first connection starts it");
    assert_eq!(conn.ping().await.unwrap().product, "echo");
    let nodes = conn.browse(&NodePath::root()).await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].kind, NodeKind::Table);

    // Streams page through the protocol, in several chunks.
    assert_eq!(count_rows(&conn, "rows 25000", 4096).await, 25_000);
    // Errors keep their kind and position.
    let e = run(&conn, "error boom").await.err().unwrap();
    assert_eq!(
        (e.kind, e.message.as_str(), e.position),
        (ErrorKind::Syntax, "boom", Some(3))
    );
    match run(&conn, "affected 9").await.unwrap() {
        Execution::Affected(9) => {}
        _ => panic!("expected 9 affected rows"),
    }
    // Several connections share one process.
    let second = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    assert_eq!(count_rows(&second, "rows 3", 10).await, 3);
    assert_eq!(count_rows(&conn, "rows 5", 10).await, 5);
}

#[tokio::test]
async fn dialect_travels_as_data() {
    let driver = echo().await;
    let conn = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    let dialect = conn.relational().unwrap().dialect();
    assert_eq!(dialect.quote_ident("a\"b"), "\"a\"\"b\"");
    assert_eq!(dialect.placeholder(2), "$2");
    let table = Node {
        path: NodePath(vec!["t".into()]),
        name: "t".into(),
        kind: NodeKind::Table,
        has_children: false,
        detail: None,
    };
    assert_eq!(
        dialect.select_all(&table, Some(5)).as_deref(),
        Some("SELECT * FROM \"t\" LIMIT 5")
    );
    assert_eq!(
        dialect.select_all(
            &Node {
                kind: NodeKind::Column,
                ..table
            },
            None
        ),
        None
    );
}

#[tokio::test]
async fn protocol_major_mismatch_refused() {
    let mut spec = echo_spec();
    spec.env.push(("ECHO_PROTOCOL".into(), "2.0.0".into()));
    let e = SidecarDriver::probe(spec).await.err().unwrap();
    assert_eq!(e.kind, ErrorKind::Protocol);
    assert_eq!(
        e.message,
        "Driver echo speaks protocol 2.0.0, sqlr speaks 1.0.0"
    );
}

#[tokio::test]
async fn a_minor_version_difference_is_fine() {
    let mut spec = echo_spec();
    spec.env.push(("ECHO_PROTOCOL".into(), "1.7.2".into()));
    assert!(SidecarDriver::probe(spec).await.is_ok());
}

#[tokio::test]
async fn missing_program_is_a_connection_error() {
    let spec = SidecarSpec::new("ghost", "1.0.0", "/nonexistent/sqlr-driver-ghost");
    let e = SidecarDriver::probe(spec).await.err().unwrap();
    assert_eq!(e.kind, ErrorKind::Connection);
    assert!(e.message.contains("ghost"), "{}", e.message);
}

#[tokio::test]
async fn hung_sidecar_times_out() {
    // A process that reads nothing and says nothing.
    let mut spec = SidecarSpec::new("hung", "1.0.0", "/bin/sh");
    spec.args = vec!["-c".into(), "sleep 30".into()];
    spec.handshake_timeout = Duration::from_millis(300);
    let started = Instant::now();
    let e = SidecarDriver::probe(spec).await.err().unwrap();
    assert_eq!(e.kind, ErrorKind::Timeout, "{e:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn garbage_on_stdout_fails_the_handshake() {
    let mut spec = SidecarSpec::new("noisy", "1.0.0", "/bin/sh");
    spec.args = vec!["-c".into(), "echo this is not a frame; sleep 30".into()];
    spec.handshake_timeout = Duration::from_secs(5);
    let started = Instant::now();
    let e = SidecarDriver::probe(spec).await.err().unwrap();
    assert_eq!(e.kind, ErrorKind::Protocol, "{e:?}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "failed fast, not by timeout"
    );
}

#[tokio::test]
async fn crash_fails_only_its_connections() {
    let dir = tempfile::tempdir().unwrap();
    let sqlite: Arc<dyn Connection> = SqliteDriver::new()
        .connect(
            &ConnectionConfig {
                values: [(
                    "path".to_string(),
                    dir.path().join("a.db").display().to_string(),
                )]
                .into(),
                ..config()
            },
            &Secrets::default(),
        )
        .await
        .unwrap();

    let driver = echo().await;
    let conn = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    let other = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    let e = run(&conn, "crash").await.err().unwrap();
    assert_eq!(e.kind, ErrorKind::Protocol, "{e:?}");
    assert!(e.message.contains("echo"), "{}", e.message);
    assert!(!driver.is_running());
    // Every connection of the dead process fails with the same kind of error…
    assert_eq!(other.ping().await.err().unwrap().kind, ErrorKind::Protocol);
    assert_eq!(conn.ping().await.err().unwrap().kind, ErrorKind::Protocol);
    // …while a connection to another driver is untouched…
    assert_eq!(sqlite.ping().await.unwrap().product, "SQLite");
    // …and the driver recovers on the next connect.
    let again = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    assert_eq!(count_rows(&again, "rows 2", 10).await, 2);
    // restart() kills the process on demand; connections fail, connect works again.
    driver.restart();
    assert!(!driver.is_running());
    assert_eq!(again.ping().await.err().unwrap().kind, ErrorKind::Protocol);
    let fresh = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    assert_eq!(fresh.ping().await.unwrap().product, "echo");
}

#[tokio::test]
async fn cancel_stops_a_statement_and_keeps_the_process() {
    let driver = echo().await;
    let conn = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    let handle = conn.cancel_handle();
    let runner = conn.clone();
    let running = tokio::spawn(async move { run(&runner, "sleep").await.err().unwrap() });
    tokio::time::sleep(Duration::from_millis(200)).await;
    handle.cancel().await.unwrap();
    let e = running.await.unwrap();
    assert_eq!(e.kind, ErrorKind::Cancelled, "{e:?}");
    assert!(driver.is_running());
    assert_eq!(conn.ping().await.unwrap().product, "echo");
}

#[tokio::test]
async fn cancel_escalates_to_kill() {
    let mut spec = echo_spec();
    spec.env.push(("ECHO_IGNORE_CANCEL".into(), "1".into()));
    spec.cancel_grace = Duration::from_millis(300);
    let (driver, _) = SidecarDriver::probe(spec).await.unwrap();
    let conn = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    let handle = conn.cancel_handle();
    let runner = conn.clone();
    let running = tokio::spawn(async move { run(&runner, "sleep").await.err().unwrap() });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    handle.cancel().await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "killed after the grace period"
    );
    let e = running.await.unwrap();
    assert_eq!(e.kind, ErrorKind::Cancelled, "{e:?}");
    assert!(!driver.is_running());
    let fresh = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    assert_eq!(fresh.ping().await.unwrap().product, "echo");
}

#[tokio::test]
async fn idle_sidecar_stops() {
    let mut spec = echo_spec();
    spec.idle_timeout = Duration::from_millis(150);
    let (driver, _) = SidecarDriver::probe(spec).await.unwrap();
    let conn = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    let second = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    drop(conn);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(driver.is_running(), "one connection is still open");
    assert_eq!(second.ping().await.unwrap().product, "echo");
    drop(second);
    let deadline = Instant::now() + Duration::from_secs(5);
    while driver.is_running() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !driver.is_running(),
        "the process stopped after the last connection closed"
    );
    // A new connection starts it again.
    let again = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    assert_eq!(again.ping().await.unwrap().product, "echo");
}

#[derive(Clone, Default)]
struct Capture(Arc<parking_lot::Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

#[tokio::test]
async fn secrets_redacted_in_protocol_log() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .with_ansi(false)
        .finish();
    // Events come from the client's spawned tasks, so the subscriber must be global.
    let _ = tracing::subscriber::set_global_default(subscriber);

    let driver = echo().await;
    let secrets = Secrets([("password".to_string(), "hunter2-correct-horse".to_string())].into());
    let conn = driver.connect(&config(), &secrets).await.unwrap();
    conn.ping().await.unwrap();

    let (sqlite, _) = SidecarDriver::probe(SidecarSpec::new(
        "sqlite",
        "1.0.0",
        example("sqlite_sidecar"),
    ))
    .await
    .unwrap();
    let conn = sqlite
        .connect(
            &ConnectionConfig {
                driver: "sqlite".into(),
                values: [("path".into(), ":memory:".into())].into(),
                ..config()
            },
            &Secrets::default(),
        )
        .await
        .unwrap();
    let rel = conn.relational().unwrap();
    let Execution::Rows(mut literal) = rel.execute("SELECT 'sidecar-sql-password'").await.unwrap()
    else {
        panic!("rows expected");
    };
    let chunk = literal.next_chunk(1).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Text("sidecar-sql-password"));
    let Execution::Rows(mut bound) = rel
        .execute_with(
            "SELECT ?1, ?2, ?3",
            &[
                Value::Text("sidecar-bound-password".into()),
                Value::Bytes(vec![222, 173, 190, 239]),
                Value::Int(987654321),
            ],
        )
        .await
        .unwrap()
    else {
        panic!("rows expected");
    };
    let chunk = bound.next_chunk(1).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Text("sidecar-bound-password"));
    assert_eq!(chunk.cell(0, 1), Cell::Bytes(&[222, 173, 190, 239]));
    assert_eq!(chunk.cell(0, 2), Cell::Int(987654321));

    let log = String::from_utf8_lossy(&capture.0.lock()).to_string();
    assert!(
        log.contains("Connect"),
        "the connect request is logged:\n{log}"
    );
    assert!(log.contains("password"), "its secret keys are:\n{log}");
    assert!(
        !log.contains("hunter2-correct-horse"),
        "its secret values are not:\n{log}"
    );
    assert!(
        log.contains("Execute"),
        "execute requests are logged:\n{log}"
    );
    for secret in [
        "sidecar-sql-password",
        "sidecar-bound-password",
        "222, 173, 190, 239",
        "987654321",
    ] {
        assert!(
            !log.contains(secret),
            "query values must not be logged:\n{log}"
        );
    }
}

fn sqlite_target(name: &str, make: impl Fn() -> SidecarOrDirect + Send + Sync + 'static) -> Target {
    let mut target = Target::new(
        name,
        Box::new(move || {
            let make = make();
            Box::pin(async move { make.connect().await })
        }),
    )
    .with_setup([
        "DROP TABLE IF EXISTS sqlr_conformance",
        "CREATE TABLE sqlr_conformance (n INTEGER PRIMARY KEY, t TEXT)",
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 25) \
         INSERT INTO sqlr_conformance SELECT n, 'row ' || n FROM c",
    ]);
    target.capabilities.server_cancel = true;
    target.capabilities.editable_results = true;
    target.slow_sql = Some(
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c) SELECT count(*) FROM c"
            .into(),
    );
    target.type_cases = vec![
        TypeCase {
            name: "integer".into(),
            sql: "SELECT 42".into(),
            expect: Value::Int(42),
        },
        TypeCase {
            name: "real".into(),
            sql: "SELECT 1.5".into(),
            expect: Value::Float(1.5),
        },
        TypeCase {
            name: "text".into(),
            sql: "SELECT 'héllo 🚀'".into(),
            expect: Value::Text("héllo 🚀".into()),
        },
        TypeCase {
            name: "blob".into(),
            sql: "SELECT x'00ff10'".into(),
            expect: Value::Bytes(vec![0, 255, 16]),
        },
        TypeCase {
            name: "null".into(),
            sql: "SELECT NULL".into(),
            expect: Value::Null,
        },
    ];
    target
}

/// Both ways to reach SQLite: in this process, or through a sidecar.
enum SidecarOrDirect {
    Direct(PathBuf),
    Sidecar(Arc<SidecarDriver>, PathBuf),
}

impl SidecarOrDirect {
    async fn connect(self) -> Result<Arc<dyn Connection>, DriverError> {
        let cfg = |path: &PathBuf| ConnectionConfig {
            driver: "sqlite".into(),
            values: [
                ("path".to_string(), path.display().to_string()),
                ("create_if_missing".to_string(), "true".to_string()),
            ]
            .into(),
            ..config()
        };
        match self {
            Self::Direct(path) => {
                SqliteDriver::new()
                    .connect(&cfg(&path), &Secrets::default())
                    .await
            }
            Self::Sidecar(driver, path) => driver.connect(&cfg(&path), &Secrets::default()).await,
        }
    }
}

#[tokio::test]
async fn sqlite_over_sidecar_passes_conformance() {
    let dir = tempfile::tempdir().unwrap();
    let direct_path = dir.path().join("direct.db");
    let sidecar_path = dir.path().join("sidecar.db");

    let direct = relational_suite(&sqlite_target("sqlite", move || {
        SidecarOrDirect::Direct(direct_path.clone())
    }))
    .await;
    assert!(direct.failed.is_empty(), "in process:\n{direct}");

    let (driver, _) = SidecarDriver::probe(SidecarSpec::new(
        "sqlite",
        "1.0.0",
        example("sqlite_sidecar"),
    ))
    .await
    .unwrap();
    let driver = Arc::new(driver);
    let over_wire = relational_suite(&sqlite_target("sqlite sidecar", move || {
        SidecarOrDirect::Sidecar(driver.clone(), sidecar_path.clone())
    }))
    .await;
    assert!(
        over_wire.failed.is_empty(),
        "through the sidecar:\n{over_wire}"
    );
    assert_eq!(direct.passed, over_wire.passed, "the same checks pass");
    assert_eq!(direct.skipped, over_wire.skipped);
    assert!(over_wire.passed.len() >= 10, "{over_wire}");
}

#[tokio::test]
async fn sqlite_values_survive_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let (driver, _) = SidecarDriver::probe(SidecarSpec::new(
        "sqlite",
        "1.0.0",
        example("sqlite_sidecar"),
    ))
    .await
    .unwrap();
    let conn = driver
        .connect(
            &ConnectionConfig {
                values: [
                    (
                        "path".to_string(),
                        dir.path().join("w.db").display().to_string(),
                    ),
                    ("create_if_missing".to_string(), "true".to_string()),
                ]
                .into(),
                ..config()
            },
            &Secrets::default(),
        )
        .await
        .unwrap();
    let Execution::Rows(mut stream) = run(
        &conn,
        "SELECT 1, 2.5, 'ü', x'0102', NULL, 'a' || char(0) || 'b'",
    )
    .await
    .unwrap() else {
        panic!("rows expected");
    };
    let chunk = stream.next_chunk(10).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Int(1));
    assert_eq!(chunk.cell(0, 1), Cell::Float(2.5));
    assert_eq!(chunk.cell(0, 2), Cell::Text("ü"));
    assert_eq!(chunk.cell(0, 3), Cell::Bytes(&[1, 2]));
    assert_eq!(chunk.cell(0, 4), Cell::Null);
    assert_eq!(chunk.cell(0, 5), Cell::Text("a\0b"));
    // The bound-parameter path crosses the wire too.
    let rel = conn.relational().unwrap();
    let Execution::Rows(mut stream) = rel
        .execute_with(
            "SELECT ?1 + 1, ?2",
            &[Value::Int(41), Value::Text("x".into())],
        )
        .await
        .unwrap()
    else {
        panic!("rows expected");
    };
    let chunk = stream.next_chunk(10).await.unwrap().unwrap();
    assert_eq!(
        (chunk.cell(0, 0), chunk.cell(0, 1)),
        (Cell::Int(42), Cell::Text("x"))
    );
}

// ----- sidecar compatibility fixes -----

#[tokio::test]
async fn a_cancel_the_sidecar_never_answers_escalates_to_kill() {
    let mut spec = echo_spec();
    spec.env.push(("ECHO_HANG_CANCEL".into(), "1".into()));
    spec.cancel_grace = Duration::from_millis(300);
    let (driver, _) = SidecarDriver::probe(spec).await.unwrap();
    let conn = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    let handle = conn.cancel_handle();
    let runner = conn.clone();
    let running = tokio::spawn(async move { run(&runner, "sleep").await.err().unwrap() });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    let cancelled = tokio::time::timeout(Duration::from_secs(5), handle.cancel()).await;
    assert!(
        cancelled.is_ok(),
        "cancel hung on a sidecar that never answers it"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    let e = running.await.unwrap();
    assert_eq!(e.kind, ErrorKind::Cancelled, "{e:?}");
    assert!(!driver.is_running());
}

#[tokio::test]
async fn a_chunk_that_does_not_match_its_header_fails_the_connection() {
    let driver = echo().await;
    let conn = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    let Execution::Rows(mut rows) = run(&conn, "badrows").await.unwrap() else {
        panic!("rows expected");
    };
    let e = rows
        .next_chunk(10)
        .await
        .expect_err("a mismatched chunk is refused");
    assert_eq!(e.kind, ErrorKind::Protocol, "{e:?}");
    assert!(
        !driver.is_running(),
        "a driver that breaks the protocol is stopped"
    );
}

#[tokio::test]
async fn a_row_larger_than_a_frame_is_an_error_not_a_silent_end() {
    let driver = echo().await;
    let conn = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    let Execution::Rows(mut rows) = run(&conn, "bigrow").await.unwrap() else {
        panic!("rows expected");
    };
    let e = rows.next_chunk(10).await.expect_err("not Ok(None)");
    assert!(e.message.contains("64 MiB"), "{}", e.message);
    // The connection is still usable.
    assert_eq!(count_rows(&conn, "rows 3", 10).await, 3);
}

#[tokio::test]
async fn the_sidecars_dialect_is_known_to_the_app_after_connecting() {
    let driver = echo().await;
    assert!(
        driver.dialect().is_none(),
        "nothing is known before the first connection"
    );
    let _conn = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    let dialect = driver
        .dialect()
        .expect("the dialect of the first connection");
    assert_eq!(dialect.placeholder(3), "$3");
    assert_eq!(dialect.quote_ident("a"), "\"a\"");
}

#[tokio::test]
async fn cancelled_connect_restarts_unshared_process_and_preserves_live_connections() {
    let driver = echo().await;
    let mut blocked = config();
    blocked.values.insert("block_connect".into(), "true".into());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(150),
            driver.connect(&blocked, &Secrets::default())
        )
        .await
        .is_err()
    );
    assert!(!driver.is_running());
    let live = driver
        .connect(&config(), &Secrets::default())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(150),
            driver.connect(&blocked, &Secrets::default())
        )
        .await
        .is_err()
    );
    assert!(driver.is_running());
    live.ping().await.unwrap();
    driver.restart();
}
