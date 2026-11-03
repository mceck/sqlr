//! Test-data generation through the transfer pipeline (spec §8.6), on a SQLite file.

use crate::support;

use sqlr_core::{
    ColumnGen, ConnectionConfig, DataGenSpec, Endpoint, Generator, Secrets, TableRef,
    TransferOptions, TransferSpec, Value,
};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{DriverRegistry, Engine, JobId, JobOutcome, QueryStatus, SessionStatus};
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn engine(path: &std::path::Path) -> Engine {
    let mut drivers = DriverRegistry::default();
    drivers.register(SqliteDriver::new());
    let engine = Engine::new(drivers, no_repaint());
    let cfg = ConnectionConfig {
        values: [("path".to_string(), path.display().to_string())].into(),
        ..config("db", "sqlite")
    };
    engine.connect(cfg, Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("db"),
        Some(SessionStatus::Connected(_))
    )));
    engine
}

fn run(engine: &Engine, sql: &str) {
    let q = engine.execute("db", sql.into(), 1_000_000);
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
}

fn count(engine: &Engine, sql: &str) -> i64 {
    let q = engine.execute("db", sql.into(), 100);
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
    let result = engine.result(q).unwrap();
    let r = result.read();
    match Value::from_cell(r.rows.cell(0, 0).unwrap()) {
        Value::Int(n) => n,
        other => panic!("{sql}: {other:?}"),
    }
}

/// Runs `sql` on the connection `conn`, on `database` when it is not the connection's own.
fn run_on(engine: &Engine, conn: &str, database: Option<&str>, sql: &str) {
    let q = engine.execute_on(conn, database, sql.into(), 10_000);
    assert!(wait_until(TIMEOUT * 12, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
    let result = engine.result(q).unwrap();
    let status = result.read().status.clone();
    assert!(
        matches!(status, QueryStatus::Ready { more: false }),
        "{sql}: {status:?}"
    );
}

fn count_on(engine: &Engine, conn: &str, database: Option<&str>, sql: &str) -> i64 {
    let q = engine.execute_on(conn, database, sql.into(), 100);
    assert!(wait_until(TIMEOUT * 12, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
    let result = engine.result(q).unwrap();
    let r = result.read();
    match Value::from_cell(r.rows.cell(0, 0).unwrap()) {
        Value::Int(n) => n,
        other => panic!("{sql}: {other:?}"),
    }
}

fn finished(engine: &Engine, job: JobId) -> JobOutcome {
    assert!(
        wait_until(TIMEOUT * 12, || engine
            .job(job)
            .is_some_and(|j| j.outcome.is_some())),
        "the job never finished"
    );
    engine.job(job).unwrap().outcome.unwrap()
}

fn table(name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some("main".into()),
        name: name.into(),
    }
}

fn gen_spec(rows: u64, columns: Vec<ColumnGen>) -> TransferSpec {
    TransferSpec {
        source: Endpoint::Generator(DataGenSpec {
            table: table("child"),
            rows,
            seed: 42,
            columns,
        }),
        target: Endpoint::Table {
            conn: "db".into(),
            table: table("child"),
        },
        mapping: Vec::new(),
        options: TransferOptions {
            batch: 1000,
            ..TransferOptions::default()
        },
    }
}

fn col(name: &str, generator: Generator) -> ColumnGen {
    ColumnGen {
        column: name.into(),
        generator,
        null_ratio: 0.0,
        unique: false,
    }
}

#[test]
fn datagen_respects_unique_and_fk() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("t.db"));
    run(&engine, "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    run(
        &engine,
        &format!(
            "INSERT INTO parent VALUES {}",
            (1..=100)
                .map(|i| format!("({i})"))
                .collect::<Vec<_>>()
                .join(",")
        ),
    );
    run(
        &engine,
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parent(id), \
         code TEXT NOT NULL UNIQUE)",
    );
    let columns = vec![
        col("id", Generator::Default),
        col(
            "parent_id",
            Generator::ForeignKey {
                table: table("parent"),
                column: "id".into(),
            },
        ),
        ColumnGen {
            unique: true,
            ..col("code", Generator::Pattern("C-#####".into()))
        },
    ];
    let job = engine.transfer(gen_spec(10_000, columns));
    match finished(&engine, job) {
        JobOutcome::Imported {
            written, rejected, ..
        } => {
            assert_eq!(written, 10_000, "{rejected:?}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(count(&engine, "SELECT count(*) FROM child"), 10_000);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM child WHERE parent_id NOT IN (SELECT id FROM parent)"
        ),
        0,
        "every foreign key exists"
    );
    assert_eq!(
        count(&engine, "SELECT count(DISTINCT code) FROM child"),
        10_000
    );
}

#[test]
fn datagen_null_ratio() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("t.db"));
    run(&engine, "CREATE TABLE child (id INTEGER, note TEXT)");
    let columns = vec![
        col("id", Generator::Sequence { start: 1, step: 1 }),
        ColumnGen {
            null_ratio: 0.3,
            ..col("note", Generator::Lorem { words: 2 })
        },
    ];
    let job = engine.transfer(gen_spec(5_000, columns));
    assert!(matches!(
        finished(&engine, job),
        JobOutcome::Imported { written: 5_000, .. }
    ));
    let nulls = count(&engine, "SELECT count(*) FROM child WHERE note IS NULL");
    assert!(
        (1200..1800).contains(&nulls),
        "{nulls} nulls of 5000 near 30 %"
    );
    assert_eq!(count(&engine, "SELECT max(id) FROM child"), 5_000);
}

#[test]
fn a_too_small_domain_fails_with_a_message() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("t.db"));
    run(&engine, "CREATE TABLE child (code TEXT UNIQUE)");
    let columns = vec![ColumnGen {
        unique: true,
        ..col("code", Generator::OneOf(vec!["a".into(), "b".into()]))
    }];
    let job = engine.transfer(gen_spec(10, columns));
    match finished(&engine, job) {
        JobOutcome::Failed(e) => {
            assert!(e.message.contains("domain is too small"), "{}", e.message)
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        count(&engine, "SELECT count(*) FROM child"),
        0,
        "nothing is left behind"
    );
}

/// A table in another database of the same server: its foreign-key pool is read from that
/// database, and JSON columns get valid documents (spec §8.6).
#[test]
fn datagen_in_another_database_uses_its_foreign_keys_and_json() {
    sqlr_testkit::skip_container_tests!();
    let (host, port) = support::endpoint("POSTGRES");
    let mut drivers = DriverRegistry::default();
    drivers.register(sqlr_driver_postgres::PostgresDriver::new());
    let engine = Engine::new(drivers, no_repaint());
    engine.set_connect_timeout(30);
    let cfg = ConnectionConfig {
        values: [
            ("host".to_string(), host),
            ("port".to_string(), port.to_string()),
            ("database".to_string(), "postgres".to_string()),
            ("user".to_string(), "postgres".to_string()),
            ("show_all_databases".to_string(), "true".to_string()),
        ]
        .into(),
        ..config("pg", "postgres")
    };
    engine.connect(
        cfg,
        Secrets([("password".to_string(), "postgres".to_string())].into()),
        dev(),
    );
    assert!(
        wait_until(TIMEOUT * 12, || matches!(
            engine.session_status("pg"),
            Some(SessionStatus::Connected(_))
        )),
        "the connection never came up: {:?}",
        engine.session_status("pg")
    );
    let database = sqlr_testkit::unique_name("sqlr_dg");
    run_on(
        &engine,
        "pg",
        None,
        &format!("CREATE DATABASE \"{database}\" TEMPLATE template0"),
    );
    run_on(
        &engine,
        "pg",
        Some(&database),
        "CREATE TABLE app_users (id bigint PRIMARY KEY)",
    );
    run_on(
        &engine,
        "pg",
        Some(&database),
        "INSERT INTO app_users SELECT generate_series(1, 50)",
    );
    run_on(
        &engine,
        "pg",
        Some(&database),
        "CREATE TABLE app_user_devices (\
           id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
           app_user_id bigint REFERENCES app_users(id), \
           payload jsonb, \
           device_token uuid)",
    );
    let table = TableRef {
        database: Some(database.clone()),
        schema: Some("public".into()),
        name: "app_user_devices".into(),
    };
    // The foreign key's target comes from introspection with no database: it lives in the
    // same database as the table being filled.
    let target = TableRef {
        database: None,
        schema: Some("public".into()),
        name: "app_users".into(),
    };
    let columns = vec![
        col("id", Generator::Default),
        col(
            "app_user_id",
            Generator::ForeignKey {
                table: target,
                column: "id".into(),
            },
        ),
        col("payload", Generator::Json { fields: 3 }),
        col("device_token", Generator::Uuid),
    ];
    let job = engine.transfer(TransferSpec {
        source: Endpoint::Generator(DataGenSpec {
            table: table.clone(),
            rows: 200,
            seed: 42,
            columns,
        }),
        target: Endpoint::Table {
            conn: "pg".into(),
            table,
        },
        mapping: Vec::new(),
        options: TransferOptions {
            batch: 100,
            ..TransferOptions::default()
        },
    });
    match finished(&engine, job) {
        JobOutcome::Imported {
            written, rejected, ..
        } => assert_eq!(written, 200, "{rejected:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        count_on(
            &engine,
            "pg",
            Some(&database),
            "SELECT count(*) FROM app_user_devices \
             WHERE app_user_id NOT BETWEEN 1 AND 50",
        ),
        0,
        "every foreign key exists"
    );
    assert_eq!(
        count_on(
            &engine,
            "pg",
            Some(&database),
            "SELECT count(*) FROM app_user_devices \
             WHERE jsonb_typeof(payload) <> 'object'",
        ),
        0,
        "every JSON value is an object"
    );
    run_on(
        &engine,
        "pg",
        None,
        &format!("DROP DATABASE \"{database}\" WITH (FORCE)"),
    );
}
