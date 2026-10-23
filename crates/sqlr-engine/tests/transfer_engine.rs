//! The data-transfer pipeline between connections and files (§8.5), on SQLite files so it
//! needs no Docker.

mod support;

use std::path::{Path, PathBuf};

use sqlr_core::{
    ColumnMapping, ConnectionConfig, CsvOptions, Endpoint, Secrets, TableRef, TextFormat,
    TransferOptions, TransferSpec, Transform, Value,
};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{DriverRegistry, Engine, JobId, JobOutcome, QueryId, QueryStatus, SessionStatus};
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

/// An engine with one SQLite connection per `(id, file)`.
fn engine_with(dbs: &[(&str, &Path)]) -> Engine {
    let mut drivers = DriverRegistry::default();
    drivers.register(SqliteDriver::new());
    let engine = Engine::new(drivers, no_repaint());
    for (id, path) in dbs {
        let cfg = ConnectionConfig {
            values: [("path".to_string(), path.display().to_string())].into(),
            ..config(id, "sqlite")
        };
        engine.connect(cfg, Secrets::default(), dev());
        assert!(wait_until(TIMEOUT, || matches!(
            engine.session_status(id),
            Some(SessionStatus::Connected(_))
        )));
    }
    engine
}

fn run(engine: &Engine, conn: &str, sql: &str) -> QueryId {
    let q = engine.execute(conn, sql.into(), 1_000_000);
    assert!(wait_until(TIMEOUT, || engine
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
        wait_until(TIMEOUT * 6, || engine
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

fn t(s: &str) -> Value {
    Value::Text(s.into())
}

fn spec(source: Endpoint, target: Endpoint) -> TransferSpec {
    TransferSpec {
        source,
        target,
        mapping: Vec::new(),
        options: TransferOptions::default(),
    }
}

fn src_table() -> Endpoint {
    Endpoint::Table {
        conn: "a".into(),
        table: table("src"),
    }
}

/// Connections `a` (with `src`: ids 1–3) and `b`.
fn two_dbs(dir: &Path) -> Engine {
    let engine = engine_with(&[("a", &dir.join("a.db")), ("b", &dir.join("b.db"))]);
    run(
        &engine,
        "a",
        "CREATE TABLE src (id INTEGER PRIMARY KEY, name TEXT, score REAL)",
    );
    run(
        &engine,
        "a",
        "INSERT INTO src VALUES (1, '  ann ', 1.5), (2, 'bob', NULL), (3, '', 3.0)",
    );
    engine
}

#[test]
fn table_to_table_between_connections() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    run(
        &engine,
        "b",
        "CREATE TABLE dst (id INTEGER PRIMARY KEY, name TEXT, score REAL)",
    );
    let job = engine.transfer(spec(
        src_table(),
        Endpoint::Table {
            conn: "b".into(),
            table: table("dst"),
        },
    ));
    match finished(&engine, job) {
        JobOutcome::Imported {
            read,
            written,
            rejected,
        } => {
            assert_eq!((read, written), (3, 3));
            assert!(rejected.is_empty());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        rows(&engine, "b", "SELECT id, name, score FROM dst ORDER BY id"),
        vec![
            vec![Value::Int(1), t("  ann "), Value::Float(1.5)],
            vec![Value::Int(2), t("bob"), Value::Null],
            vec![Value::Int(3), t(""), Value::Float(3.0)],
        ]
    );
}

#[test]
fn a_new_table_is_created_from_the_source_columns() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    let job = engine.transfer(spec(
        src_table(),
        Endpoint::NewTable {
            conn: "b".into(),
            table: table("copy"),
            ddl: None,
        },
    ));
    assert!(matches!(
        finished(&engine, job),
        JobOutcome::Imported { written: 3, .. }
    ));
    let columns = rows(
        &engine,
        "b",
        "SELECT name, type FROM pragma_table_info('copy')",
    );
    assert_eq!(
        columns,
        vec![
            vec![t("id"), t("INTEGER")],
            vec![t("name"), t("TEXT")],
            vec![t("score"), t("REAL")],
        ]
    );
    assert_eq!(
        rows(&engine, "b", "SELECT count(*) FROM copy")[0][0],
        Value::Int(3)
    );
}

#[test]
fn a_new_table_follows_the_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    let mut s = spec(
        src_table(),
        Endpoint::NewTable {
            conn: "b".into(),
            table: table("renamed"),
            ddl: None,
        },
    );
    // `id` becomes `key`, `name` keeps its name with a forced type, `score` is skipped.
    s.mapping = vec![
        ColumnMapping::new(0, Some("key".into())),
        ColumnMapping {
            target_type: Some("VARCHAR(30)".into()),
            ..ColumnMapping::new(1, Some("name".into()))
        },
        ColumnMapping::new(2, None),
    ];
    let job = engine.transfer(s);
    assert!(matches!(
        finished(&engine, job),
        JobOutcome::Imported { written: 3, .. }
    ));
    assert_eq!(
        rows(
            &engine,
            "b",
            "SELECT name, type FROM pragma_table_info('renamed')"
        ),
        vec![
            vec![t("key"), t("INTEGER")],
            vec![t("name"), t("VARCHAR(30)")]
        ]
    );
    assert_eq!(
        rows(&engine, "b", "SELECT key, name FROM renamed ORDER BY key")[1],
        vec![Value::Int(2), t("bob")]
    );
}

#[test]
fn an_edited_ddl_replaces_the_generated_one() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    let job = engine.transfer(spec(
        src_table(),
        Endpoint::NewTable {
            conn: "b".into(),
            table: table("copy"),
            ddl: Some("CREATE TABLE \"copy\" (id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT 'x', score REAL, extra TEXT)".into()),
        },
    ));
    finished(&engine, job);
    let names = rows(&engine, "b", "SELECT name FROM pragma_table_info('copy')");
    assert_eq!(
        names.len(),
        4,
        "the edited DDL's extra column exists: {names:?}"
    );
}

#[test]
fn a_query_source_with_a_second_statement_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    run(&engine, "b", "CREATE TABLE dst (id INTEGER PRIMARY KEY)");
    let job = engine.transfer(spec(
        Endpoint::Query {
            conn: "a".into(),
            database: None,
            sql: "SELECT id FROM src; DELETE FROM src".into(),
        },
        Endpoint::Table {
            conn: "b".into(),
            table: table("dst"),
        },
    ));
    let JobOutcome::Failed(e) = finished(&engine, job) else {
        panic!("a multi-statement source must be refused");
    };
    assert!(e.message.contains("only reads"), "{}", e.message);
    // The source was not touched.
    assert_eq!(
        rows(&engine, "a", "SELECT count(*) FROM src"),
        vec![vec![Value::Int(3)]]
    );
}

#[test]
fn a_query_is_a_source_and_truncate_then_load_replaces_rows() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    run(
        &engine,
        "b",
        "CREATE TABLE dst (id INTEGER PRIMARY KEY, name TEXT, score REAL)",
    );
    run(&engine, "b", "INSERT INTO dst VALUES (99, 'old', 0)");
    let mut s = spec(
        Endpoint::Query {
            conn: "a".into(),
            database: None,
            sql: "SELECT id, name, score FROM src WHERE id < 3".into(),
        },
        Endpoint::Table {
            conn: "b".into(),
            table: table("dst"),
        },
    );
    s.options.truncate_first = true;
    let job = engine.transfer(s);
    assert!(matches!(
        finished(&engine, job),
        JobOutcome::Imported { written: 2, .. }
    ));
    let ids = rows(&engine, "b", "SELECT id FROM dst ORDER BY id");
    assert_eq!(ids, vec![vec![Value::Int(1)], vec![Value::Int(2)]]);
}

#[test]
fn upsert_by_key_updates_and_inserts() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    run(
        &engine,
        "b",
        "CREATE TABLE dst (id INTEGER PRIMARY KEY, name TEXT, score REAL)",
    );
    run(
        &engine,
        "b",
        "INSERT INTO dst VALUES (2, 'stale', 9.0), (50, 'keep', 1.0)",
    );
    let mut s = spec(
        src_table(),
        Endpoint::Table {
            conn: "b".into(),
            table: table("dst"),
        },
    );
    s.options.upsert_key = Some(vec!["id".into()]);
    let job = engine.transfer(s);
    assert!(matches!(
        finished(&engine, job),
        JobOutcome::Imported { written: 3, .. }
    ));
    assert_eq!(
        rows(&engine, "b", "SELECT id, name FROM dst ORDER BY id"),
        vec![
            vec![Value::Int(1), t("  ann ")],
            vec![Value::Int(2), t("bob")],
            vec![Value::Int(3), t("")],
            vec![Value::Int(50), t("keep")],
        ]
    );
}

#[test]
fn mapping_converts_types_and_applies_transforms() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    run(
        &engine,
        "b",
        "CREATE TABLE dst (id INTEGER, name TEXT, score TEXT, tag TEXT)",
    );
    let mut s = spec(
        src_table(),
        Endpoint::Table {
            conn: "b".into(),
            table: table("dst"),
        },
    );
    s.mapping = vec![
        ColumnMapping::new(0, Some("id".into())),
        ColumnMapping {
            transform: Some(Transform::Trim),
            ..ColumnMapping::new(1, Some("name".into()))
        },
        ColumnMapping {
            transform: Some(Transform::Upper),
            ..ColumnMapping::new(2, Some("score".into()))
        },
        ColumnMapping {
            source: 1,
            target: Some("tag".into()),
            target_type: None,
            transform: Some(Transform::NullIfEmpty),
        },
    ];
    let job = engine.transfer(s);
    assert!(matches!(
        finished(&engine, job),
        JobOutcome::Imported { written: 3, .. }
    ));
    let got = rows(&engine, "b", "SELECT name, tag FROM dst ORDER BY id");
    // Trim cleans the name; NullIfEmpty only nulls blank text and keeps "  ann " as is.
    assert_eq!(got[0], vec![t("ann"), t("  ann ")]);
    assert_eq!(got[2], vec![t(""), Value::Null]);

    // A constant fills a column whatever the source holds.
    run(&engine, "b", "DELETE FROM dst");
    let mut s = spec(
        src_table(),
        Endpoint::Table {
            conn: "b".into(),
            table: table("dst"),
        },
    );
    s.mapping = vec![
        ColumnMapping::new(0, Some("id".into())),
        ColumnMapping {
            transform: Some(Transform::Constant("fixed".into())),
            ..ColumnMapping::new(1, Some("tag".into()))
        },
    ];
    let job = engine.transfer(s);
    finished(&engine, job);
    let got = rows(&engine, "b", "SELECT DISTINCT tag FROM dst");
    assert_eq!(got, vec![vec![t("fixed")]]);
}

#[test]
fn disable_constraints_loads_orphans_and_restores_the_setting() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    run(&engine, "b", "CREATE TABLE parent (id INTEGER PRIMARY KEY)");
    run(
        &engine,
        "b",
        "CREATE TABLE dst (id INTEGER PRIMARY KEY, name TEXT, score REAL, \
         p INTEGER REFERENCES parent(id))",
    );
    let target = || Endpoint::Table {
        conn: "b".into(),
        table: table("dst"),
    };
    let mut s = spec(
        Endpoint::Query {
            conn: "a".into(),
            database: None,
            sql: "SELECT id, name, score, id AS p FROM src".into(),
        },
        target(),
    );
    s.options.disable_constraints = true;
    let job = engine.transfer(s);
    assert!(matches!(
        finished(&engine, job),
        JobOutcome::Imported { written: 3, .. }
    ));
    let enforced = rows(&engine, "b", "PRAGMA foreign_keys");
    assert_eq!(
        enforced[0][0],
        Value::Int(1),
        "foreign keys are enforced again"
    );

    // Without the option the orphans are refused.
    run(&engine, "b", "DELETE FROM dst");
    let s = spec(
        Endpoint::Query {
            conn: "a".into(),
            database: None,
            sql: "SELECT id, name, score, id AS p FROM src".into(),
        },
        target(),
    );
    let job = engine.transfer(s);
    assert!(matches!(finished(&engine, job), JobOutcome::Failed(_)));
    assert_eq!(
        rows(&engine, "b", "SELECT count(*) FROM dst")[0][0],
        Value::Int(0)
    );
}

#[test]
fn rejected_rows_are_written_to_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    run(
        &engine,
        "b",
        "CREATE TABLE dst (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL)",
    );
    run(&engine, "b", "INSERT INTO dst VALUES (2, 'taken', 0)");
    let rejected = dir.path().join("rejected.csv");
    let mut s = spec(
        Endpoint::Query {
            conn: "a".into(),
            database: None,
            sql: "SELECT id, name, score FROM src".into(),
        },
        Endpoint::Table {
            conn: "b".into(),
            table: table("dst"),
        },
    );
    s.options.skip_bad_rows = true;
    s.options.rejected_file = Some(rejected.clone());
    let job = engine.transfer(s);
    match finished(&engine, job) {
        JobOutcome::Imported {
            read,
            written,
            rejected: bad,
        } => {
            assert_eq!((read, written), (3, 2));
            assert_eq!(bad.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    let text = std::fs::read_to_string(&rejected).unwrap();
    assert!(text.starts_with("line,reason,"), "{text}");
    assert!(
        text.contains("bob"),
        "the rejected row's values are kept: {text}"
    );
}

#[test]
fn probing_a_source_reports_columns_types_and_the_first_rows() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    // 150 rows: the probe stops at 100.
    run(&engine, "a", "CREATE TABLE many (id INTEGER, label TEXT)");
    let values: Vec<String> = (0..150).map(|i| format!("({i}, 'n{i}')")).collect();
    run(
        &engine,
        "a",
        &format!("INSERT INTO many VALUES {}", values.join(",")),
    );

    let probe = |source: Endpoint| {
        let job = engine.probe_source(source, TransferOptions::default());
        match finished(&engine, job) {
            JobOutcome::Probed(probe) => probe,
            other => panic!("{other:?}"),
        }
    };
    let table = probe(Endpoint::Table {
        conn: "a".into(),
        table: table("many"),
    });
    let columns: Vec<(&str, &str)> = table
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_name.as_str()))
        .collect();
    assert_eq!(columns, [("id", "INTEGER"), ("label", "TEXT")]);
    assert_eq!(table.rows.len(), 100);
    assert_eq!(table.rows[1], vec![Value::Int(1), t("n1")]);

    let query = probe(Endpoint::Query {
        conn: "a".into(),
        database: None,
        sql: "SELECT id FROM src ORDER BY id".into(),
    });
    assert_eq!(query.rows.len(), 3);

    let csv = dir.path().join("p.csv");
    std::fs::write(&csv, "x,y\n1,2\n").unwrap();
    let file = probe(csv_source(csv));
    assert_eq!(
        file.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["x", "y"]
    );
    assert_eq!(file.rows, vec![vec![t("1"), t("2")]]);

    // A workbook lists its sheets.
    let book = dir.path().join("b.xlsx");
    {
        let mut workbook = rust_xlsxwriter::Workbook::new();
        workbook.add_worksheet().set_name("First").unwrap();
        let second = workbook.add_worksheet();
        second.set_name("Second").unwrap();
        second.write_string(0, 0, "h").unwrap();
        second.write_string(1, 0, "v").unwrap();
        workbook.save(&book).unwrap();
    }
    let sheets = probe(Endpoint::Workbook {
        path: book,
        sheet: Some("Second".into()),
        header: true,
    });
    assert_eq!(sheets.sheets, ["First", "Second"]);
    assert_eq!(sheets.rows, vec![vec![t("v")]]);
}

#[test]
fn the_rejected_file_is_complete_when_the_load_stops() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    // Every id of the source is taken already: all three rows conflict.
    run(
        &engine,
        "b",
        "CREATE TABLE dst (id INTEGER PRIMARY KEY, name TEXT, score REAL)",
    );
    run(
        &engine,
        "b",
        "INSERT INTO dst VALUES (1, 'a', 0), (2, 'b', 0), (3, 'c', 0)",
    );
    let rejected = dir.path().join("rejected.csv");
    let mut s = spec(
        Endpoint::Query {
            conn: "a".into(),
            database: None,
            sql: "SELECT id, name, score FROM src ORDER BY id".into(),
        },
        Endpoint::Table {
            conn: "b".into(),
            table: table("dst"),
        },
    );
    s.options.skip_bad_rows = true;
    s.options.max_rejected = Some(1);
    s.options.rejected_file = Some(rejected.clone());
    let job = engine.transfer(s);
    let JobOutcome::Failed(e) = finished(&engine, job) else {
        panic!("the load stops after too many rejected rows");
    };
    assert!(
        e.message.contains("Stopped after 2 rejected rows"),
        "{}",
        e.message
    );
    let text = std::fs::read_to_string(&rejected).unwrap();
    assert_eq!(
        text.lines().count(),
        3,
        "header and the two rejected rows: {text}"
    );
}

/// A CSV of `n` rows (`id,name`).
fn big_csv(dir: &Path, n: usize) -> PathBuf {
    let path = dir.join("big.csv");
    let mut text = String::from("id,name\n");
    for i in 0..n {
        text.push_str(&format!("{i},name {i}\n"));
    }
    std::fs::write(&path, text).unwrap();
    path
}

fn csv_source(path: PathBuf) -> Endpoint {
    Endpoint::File {
        path,
        format: TextFormat::Csv(CsvOptions::default()),
    }
}

fn cancel_midway(engine: &Engine, job: JobId) {
    assert!(wait_until(TIMEOUT * 6, || engine.job(job).is_some_and(
        |j| j.progress.rows >= 2_000 || j.outcome.is_some()
    )));
    engine.cancel_job(job);
}

#[test]
fn cancel_in_commit_once_mode_rolls_everything_back() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    run(&engine, "b", "CREATE TABLE dst (id INTEGER, name TEXT)");
    let mut s = spec(
        csv_source(big_csv(dir.path(), 400_000)),
        Endpoint::Table {
            conn: "b".into(),
            table: table("dst"),
        },
    );
    s.options.batch = 100;
    let job = engine.transfer(s);
    cancel_midway(&engine, job);
    assert_eq!(finished(&engine, job), JobOutcome::Cancelled);
    assert_eq!(
        rows(&engine, "b", "SELECT count(*) FROM dst")[0][0],
        Value::Int(0)
    );
}

#[test]
fn cancel_in_per_batch_mode_keeps_and_reports_committed_batches() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    run(&engine, "b", "CREATE TABLE dst (id INTEGER, name TEXT)");
    let mut s = spec(
        csv_source(big_csv(dir.path(), 400_000)),
        Endpoint::Table {
            conn: "b".into(),
            table: table("dst"),
        },
    );
    s.options.batch = 100;
    s.options.commit_per_batch = true;
    let job = engine.transfer(s);
    cancel_midway(&engine, job);
    assert_eq!(finished(&engine, job), JobOutcome::Cancelled);
    let kept = match rows(&engine, "b", "SELECT count(*) FROM dst")[0][0] {
        Value::Int(n) => n,
        ref other => panic!("{other:?}"),
    };
    assert!(kept > 0 && kept % 100 == 0, "whole batches stay: {kept}");
    let log = engine.job(job).unwrap().log;
    assert!(
        log.iter()
            .any(|l| l.contains(&format!("{kept} rows committed"))),
        "the summary says how many rows stay: {log:?}"
    );
}
