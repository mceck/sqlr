//! Transfers across real servers (Docker): PostgreSQL to MySQL and SQLite with new tables,
//! type mapping, upsert and rejected rows. Needs `DOCKER_HOST` (see CLAUDE.md).

mod support;

use std::time::Duration;

use sqlr_core::{
    ConnectionConfig, Endpoint, Secrets, TableRef, TransferOptions, TransferSpec, Value,
};
use sqlr_engine::{DriverRegistry, Engine, JobId, JobOutcome, QueryId, QueryStatus, SessionStatus};
use support::endpoint;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

struct Servers {
    pg: ConnectionConfig,
    my: ConnectionConfig,
}

fn values(pairs: &[(&str, String)]) -> std::collections::BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

async fn start() -> Servers {
    let (pg_host, pg_port) = endpoint("POSTGRES");
    let (my_host, my_port) = endpoint("MYSQL");
    let pg_cfg = ConnectionConfig {
        values: values(&[
            ("host", pg_host),
            ("port", pg_port.to_string()),
            ("database", "postgres".into()),
            ("user", "postgres".into()),
        ]),
        ..config("pg", "postgres")
    };
    let my_cfg = ConnectionConfig {
        values: values(&[
            ("host", my_host),
            ("port", my_port.to_string()),
            ("database", "test".into()),
            ("user", "root".into()),
        ]),
        ..config("my", "mysql")
    };
    Servers {
        pg: pg_cfg,
        my: my_cfg,
    }
}

fn connect(engine: &Engine, cfg: ConnectionConfig, secrets: Secrets) {
    let id = cfg.id.clone();
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        engine.connect(cfg.clone(), secrets.clone(), dev());
        wait_until(Duration::from_secs(70), || {
            !matches!(engine.session_status(&id), Some(SessionStatus::Connecting))
        });
        if matches!(
            engine.session_status(&id),
            Some(SessionStatus::Connected(_))
        ) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{id} never connected");
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn run(engine: &Engine, conn: &str, sql: &str) -> QueryId {
    let q = engine.execute(conn, sql.into(), 100_000);
    assert!(wait_until(TIMEOUT * 12, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
    let status = engine.result(q).unwrap().read().status.clone();
    assert!(
        matches!(status, QueryStatus::Ready { more: false }),
        "{sql}: {status:?}"
    );
    q
}

fn rows(engine: &Engine, conn: &str, sql: &str) -> Vec<Vec<Value>> {
    let q = run(engine, conn, sql);
    let result = engine.result(q).unwrap();
    let r = result.read();
    (0..r.rows.row_count())
        .map(|row| {
            (0..r.rows.columns().len())
                .map(|col| Value::from_cell(r.rows.cell(row, col).unwrap()))
                .collect()
        })
        .collect()
}

fn finished(engine: &Engine, job: JobId) -> JobOutcome {
    assert!(
        wait_until(Duration::from_secs(180), || engine
            .job(job)
            .is_some_and(|j| j.outcome.is_some())),
        "the job never finished"
    );
    engine.job(job).unwrap().outcome.unwrap()
}

fn spec(source: Endpoint, target: Endpoint) -> TransferSpec {
    TransferSpec {
        source,
        target,
        mapping: Vec::new(),
        options: TransferOptions::default(),
    }
}

fn table(schema: Option<&str>, name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: schema.map(str::to_string),
        name: name.into(),
    }
}

fn text(v: &Value) -> String {
    v.display().into_owned()
}

#[test]
fn cross_dialect_matrix() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let servers = rt.block_on(start());
    let dir = tempfile::tempdir().unwrap();
    let mut drivers = DriverRegistry::default();
    drivers.register(sqlr_driver_postgres::PostgresDriver::new());
    drivers.register(sqlr_driver_mysql::MysqlDriver::new());
    drivers.register(sqlr_driver_sqlite::SqliteDriver::new());
    let engine = Engine::new(drivers, no_repaint());
    engine.set_connect_timeout(60);
    connect(
        &engine,
        servers.pg.clone(),
        Secrets([("password".to_string(), "postgres".to_string())].into()),
    );
    connect(&engine, servers.my.clone(), Secrets::default());
    for table in ["m", "m_back", "k", "narrow"] {
        run(
            &engine,
            "pg",
            &format!("DROP TABLE IF EXISTS public.{table} CASCADE"),
        );
        run(&engine, "my", &format!("DROP TABLE IF EXISTS {table}"));
    }
    let lite = ConnectionConfig {
        values: [(
            "path".to_string(),
            dir.path().join("m.db").display().to_string(),
        )]
        .into(),
        ..config("lite", "sqlite")
    };
    connect(&engine, lite, Secrets::default());

    run(
        &engine,
        "pg",
        "CREATE TABLE m (i2 smallint, i4 integer, i8 bigint, num numeric(30,10), \
         flt double precision, flag boolean, t text, bin bytea, ts timestamptz, d date, \
         u uuid, j jsonb)",
    );
    run(
        &engine,
        "pg",
        "INSERT INTO m VALUES \
         (1, 2, 3000000000, 12345678901234567890.1234567891, 0.1, true, 'è ü 😀', \
          '\\x00ff10', '2024-03-09 12:34:56+00', '2024-03-09', \
          'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '{\"k\": [1, 2]}'), \
         (NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)",
    );

    // PostgreSQL → MySQL, a new table.
    let job = engine.transfer(spec(
        Endpoint::Table {
            conn: "pg".into(),
            table: table(Some("public"), "m"),
        },
        Endpoint::NewTable {
            conn: "my".into(),
            table: table(None, "m"),
            ddl: None,
        },
    ));
    let outcome = finished(&engine, job);
    assert!(
        matches!(outcome, JobOutcome::Imported { written: 2, .. }),
        "{outcome:?} {:?}",
        engine.job(job).map(|j| j.log)
    );
    let got = rows(
        &engine,
        "my",
        "SELECT i2, i4, i8, CAST(num AS CHAR), flt, flag, t, hex(bin), CAST(u AS CHAR), CAST(d AS CHAR) FROM m \
         WHERE i2 IS NOT NULL",
    );
    assert_eq!(
        text(&got[0][3]),
        "12345678901234567890.1234567891",
        "exact decimal"
    );
    assert_eq!(text(&got[0][6]), "è ü 😀");
    assert_eq!(text(&got[0][7]), "00FF10");
    assert_eq!(text(&got[0][8]), "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11");
    assert_eq!(text(&got[0][9]), "2024-03-09");
    assert_eq!(
        rows(&engine, "my", "SELECT count(*) FROM m WHERE i2 IS NULL")[0][0],
        Value::Int(1)
    );

    // PostgreSQL → SQLite, a new table.
    let job = engine.transfer(spec(
        Endpoint::Table {
            conn: "pg".into(),
            table: table(Some("public"), "m"),
        },
        Endpoint::NewTable {
            conn: "lite".into(),
            table: table(Some("main"), "m"),
            ddl: None,
        },
    ));
    assert!(matches!(
        finished(&engine, job),
        JobOutcome::Imported { written: 2, .. }
    ));
    let got = rows(
        &engine,
        "lite",
        "SELECT i8, num, t, hex(bin) FROM m WHERE i2 IS NOT NULL",
    );
    assert_eq!(got[0][0], Value::Int(3_000_000_000));
    assert_eq!(text(&got[0][1]), "12345678901234567890.1234567891");
    assert_eq!(text(&got[0][3]), "00FF10");

    // SQLite → PostgreSQL, back through a new table.
    let job = engine.transfer(spec(
        Endpoint::Table {
            conn: "lite".into(),
            table: table(Some("main"), "m"),
        },
        Endpoint::NewTable {
            conn: "pg".into(),
            table: table(Some("public"), "m_back"),
            ddl: None,
        },
    ));
    assert!(matches!(
        finished(&engine, job),
        JobOutcome::Imported { written: 2, .. }
    ));
    let back = rows(
        &engine,
        "pg",
        "SELECT i8, t FROM m_back WHERE i8 IS NOT NULL",
    );
    assert_eq!(back[0][0], Value::Int(3_000_000_000));
    assert_eq!(text(&back[0][1]), "è ü 😀");

    // Upsert on MySQL by key: a second transfer into a keyed table updates in place.
    run(
        &engine,
        "my",
        "CREATE TABLE k (id INT PRIMARY KEY, name VARCHAR(20))",
    );
    run(&engine, "my", "INSERT INTO k VALUES (1, 'old')");
    run(&engine, "pg", "CREATE TABLE k (id int, name text)");
    run(&engine, "pg", "INSERT INTO k VALUES (1, 'new'), (2, 'two')");
    let mut s = spec(
        Endpoint::Table {
            conn: "pg".into(),
            table: table(Some("public"), "k"),
        },
        Endpoint::Table {
            conn: "my".into(),
            table: table(None, "k"),
        },
    );
    s.options.upsert_key = Some(vec!["id".into()]);
    let job = engine.transfer(s);
    assert!(matches!(
        finished(&engine, job),
        JobOutcome::Imported { .. }
    ));
    let got = rows(&engine, "my", "SELECT id, name FROM k ORDER BY id");
    assert_eq!(text(&got[0][1]), "new");
    assert_eq!(text(&got[1][1]), "two");

    // Emoji into a utf8mb3 column: the row is rejected with the server's reason, not
    // truncated.
    run(
        &engine,
        "my",
        "CREATE TABLE narrow (id INT, t VARCHAR(20) CHARACTER SET utf8mb3)",
    );
    run(&engine, "pg", "CREATE TABLE narrow (id int, t text)");
    run(
        &engine,
        "pg",
        "INSERT INTO narrow VALUES (1, 'plain'), (2, 'face 😀')",
    );
    let mut s = spec(
        Endpoint::Table {
            conn: "pg".into(),
            table: table(Some("public"), "narrow"),
        },
        Endpoint::Table {
            conn: "my".into(),
            table: table(None, "narrow"),
        },
    );
    s.options.skip_bad_rows = true;
    let job = engine.transfer(s);
    match finished(&engine, job) {
        JobOutcome::Imported {
            written, rejected, ..
        } => {
            assert_eq!(written, 1);
            assert_eq!(rejected.len(), 1, "{rejected:?}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        rows(&engine, "my", "SELECT t FROM narrow")
            .iter()
            .map(|r| text(&r[0]))
            .collect::<Vec<_>>(),
        ["plain"]
    );
}
