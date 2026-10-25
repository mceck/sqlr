//! Export, import, file preview and clipboard jobs (§8.3).

use crate::support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqlr_core::{ConnectionConfig, CsvOptions, Secrets, TableRef, TextFormat, Value};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{
    DriverRegistry, Engine, ExportSource, ImportOptions, JobId, JobKind, JobOutcome, QueryId,
    QueryStatus, SessionKey, SessionStatus,
};
use sqlr_testkit::{FakeDriver, FakeScript};
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn fake_engine(script: FakeScript) -> (Engine, Arc<FakeDriver>) {
    let fake = FakeDriver::new(script);
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Default::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    (engine, fake)
}

fn sqlite_engine(path: &Path) -> Engine {
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

#[test]
fn nontransactional_targets_reject_imports_and_new_tables_without_writes() {
    use sqlr_core::{Endpoint, ErrorKind, TransferSpec};
    let fake = FakeDriver::without_transactions(FakeScript::default());
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.csv");
    std::fs::write(&path, "id\n1\n").unwrap();
    let target = TableRef {
        database: None,
        schema: None,
        name: "target".into(),
    };
    let options = ImportOptions {
        truncate_first: true,
        ..Default::default()
    };
    let job = engine.import(
        &SessionKey::meta("c"),
        path.clone(),
        TextFormat::Csv(CsvOptions::default()),
        target.clone(),
        Vec::new(),
        options.clone(),
    );
    assert!(
        matches!(finished(&engine, job), JobOutcome::Failed(e) if e.kind == ErrorKind::Unsupported && e.message.contains("transaction"))
    );
    let job = engine.transfer(TransferSpec {
        source: Endpoint::File {
            path,
            format: TextFormat::Csv(CsvOptions::default()),
        },
        target: Endpoint::NewTable {
            conn: "c".into(),
            table: target,
            ddl: Some("CREATE TABLE target(id INTEGER)".into()),
        },
        mapping: Vec::new(),
        options,
    });
    assert!(
        matches!(finished(&engine, job), JobOutcome::Failed(e) if e.kind == ErrorKind::Unsupported && e.message.contains("transaction"))
    );
    assert!(
        fake.executed().is_empty(),
        "no CREATE, DELETE, INSERT or transaction statements may run"
    );
}

fn finished(engine: &Engine, job: JobId) -> JobOutcome {
    assert!(
        wait_until(TIMEOUT * 4, || engine
            .job(job)
            .is_some_and(|j| j.outcome.is_some())),
        "the job never finished"
    );
    engine.job(job).unwrap().outcome.unwrap()
}

/// Runs `sql` on the explorer session of `conn` to its end; panics on failure.
fn run(engine: &Engine, conn: &str, sql: &str) -> QueryId {
    let q = engine.execute(conn, sql.into(), 100_000);
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

/// No file in `dir` but the ones named.
fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn export_rerun_streams_all_rows() {
    let (engine, _fake) = fake_engine(FakeScript {
        rows: 10_000,
        ..Default::default()
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.csv");
    let job = engine.export(
        ExportSource::Rerun {
            key: SessionKey::meta("c"),
            database: None,
            sql: "SELECT * FROM t".into(),
        },
        TextFormat::Csv(CsvOptions::default()),
        path.clone(),
    );
    assert_eq!(engine.job(job).unwrap().kind, JobKind::Export);
    assert_eq!(
        finished(&engine, job),
        JobOutcome::Exported {
            rows: 10_000,
            path: path.clone()
        }
    );
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 10_001);
    assert_eq!(lines[0], "n");
    assert_eq!(lines[1], "0");
    assert_eq!(lines[10_000], "9999");
    let progress = engine.job(job).unwrap().progress;
    assert_eq!(progress.rows, 10_000);
    assert_eq!(progress.bytes, text.len() as u64);
    assert_eq!(files_in(dir.path()), ["out.csv"]);
}

#[test]
fn export_loaded_rows_writes_the_buffer() {
    let (engine, _fake) = fake_engine(FakeScript {
        rows: 1200,
        ..Default::default()
    });
    let q = engine.execute("c", "SELECT * FROM t".into(), 500);
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(
            |r| r.read().status == QueryStatus::Ready { more: true }
        )));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.jsonl");
    let job = engine.export(ExportSource::Loaded(q), TextFormat::JsonLines, path.clone());
    assert_eq!(
        finished(&engine, job),
        JobOutcome::Exported {
            rows: 500,
            path: path.clone()
        }
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 500);
    assert_eq!(text.lines().last(), Some("{\"n\":499}"));
}

#[test]
fn export_cancel_leaves_no_partial_file() {
    let (engine, _fake) = fake_engine(FakeScript {
        rows: 10,
        block_on_fetch: true,
        ..Default::default()
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.csv");
    let job = engine.export(
        ExportSource::Rerun {
            key: SessionKey::meta("c"),
            database: None,
            sql: "SELECT * FROM t".into(),
        },
        TextFormat::Csv(CsvOptions::default()),
        path.clone(),
    );
    // The header is written before the first chunk blocks.
    std::thread::sleep(std::time::Duration::from_millis(100));
    engine.cancel_job(job);
    assert_eq!(finished(&engine, job), JobOutcome::Cancelled);
    assert!(!path.exists());
    assert!(
        files_in(dir.path()).is_empty(),
        "{:?}",
        files_in(dir.path())
    );
}

#[test]
fn preview_reads_100_rows() {
    let (engine, _fake) = fake_engine(FakeScript::default());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.csv");
    let mut text = String::from("id,name\n");
    for i in 0..250 {
        text.push_str(&format!("{i},\"n,{i}\"\n"));
    }
    std::fs::write(&path, text).unwrap();
    let job = engine.preview_file(path, TextFormat::Csv(CsvOptions::default()));
    assert_eq!(engine.job(job).unwrap().kind, JobKind::Preview);
    let JobOutcome::Preview { columns, rows } = finished(&engine, job) else {
        panic!("expected a preview");
    };
    assert_eq!(columns, ["id", "name"]);
    assert_eq!(rows.len(), 100);
    assert_eq!(
        rows[99],
        vec![Value::Text("99".into()), Value::Text("n,99".into())]
    );
}

#[test]
fn preview_stops_reading_after_100_rows() {
    let (engine, _fake) = fake_engine(FakeScript::default());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.csv");
    let mut text = String::from("id\n");
    for i in 0..150 {
        text.push_str(&format!("{i}\n"));
    }
    // Broken far below the preview: never read.
    text.push_str("\"unclosed\n");
    std::fs::write(&path, text).unwrap();
    let job = engine.preview_file(path, TextFormat::Csv(CsvOptions::default()));
    let JobOutcome::Preview { rows, .. } = finished(&engine, job) else {
        panic!("the preview reads only the first rows");
    };
    assert_eq!(rows.len(), 100);
}

#[test]
fn import_inserts_batches_while_it_reads() {
    let dir = tempfile::tempdir().unwrap();
    let engine = sqlite_engine(&dir.path().join("t.db"));
    run(&engine, "db", "CREATE TABLE p (id INTEGER)");
    let path = dir.path().join("in.csv");
    std::fs::write(&path, "id\n1\n2\n3\n4\n\"5\n").unwrap();
    let job = engine.import(
        &SessionKey::meta("db"),
        path,
        TextFormat::Csv(CsvOptions::default()),
        table("p"),
        vec![(0, Some("id".into()))],
        ImportOptions {
            batch: 2,
            commit_per_batch: true,
            skip_bad_rows: false,
            truncate_first: false,
            ..Default::default()
        },
    );
    let JobOutcome::Failed(e) = finished(&engine, job) else {
        panic!("the broken end of the file fails the import");
    };
    assert!(e.message.contains("Unclosed quote"), "{}", e.message);
    // The batches read before the broken record were committed.
    assert_eq!(
        rows(&engine, "db", "SELECT id FROM p ORDER BY id"),
        (1..=4).map(|i| vec![Value::Int(i)]).collect::<Vec<_>>()
    );
}

#[test]
fn preview_reports_unreadable_files() {
    let (engine, _fake) = fake_engine(FakeScript::default());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.json");
    std::fs::write(&path, "{ not json").unwrap();
    let job = engine.preview_file(path, TextFormat::Json);
    assert!(matches!(finished(&engine, job), JobOutcome::Failed(_)));
}

/// Values that trip encoders: separators, quotes, line breaks, Markdown pipes, emoji, empty
/// text next to NULL, a 1 MB text, bytes and exact floats.
fn awkward_rows() -> Vec<Vec<Value>> {
    let texts = [
        Some("a,b"),
        Some("\"q\""),
        Some("tab\there"),
        Some("line\nbreak"),
        Some("cr\r\nlf"),
        Some("|pipe|"),
        Some("😀 è"),
        Some(""),
        None,
        Some("0x41"),
        Some("NULL"),
    ];
    let mut rows: Vec<Vec<Value>> = texts
        .iter()
        .enumerate()
        .map(|(i, t)| {
            vec![
                Value::Int(i as i64 + 1),
                t.map_or(Value::Null, |t| Value::Text(t.into())),
                if i % 2 == 0 {
                    Value::Float(0.1 + i as f64)
                } else {
                    Value::Null
                },
                if i % 3 == 0 {
                    Value::Bytes(vec![0, 1, i as u8, 255])
                } else {
                    Value::Null
                },
            ]
        })
        .collect();
    rows.push(vec![
        Value::Int(100),
        Value::Text("x".repeat(1 << 20)),
        Value::Float(-1e300),
        Value::Bytes(Vec::new()),
    ]);
    rows
}

fn setup_tables(engine: &Engine) {
    for sql in [
        "CREATE TABLE src (id INTEGER PRIMARY KEY, t TEXT, f REAL, b BLOB)",
        "CREATE TABLE dst (id INTEGER PRIMARY KEY, t TEXT, f REAL, b BLOB)",
    ] {
        run(engine, "db", sql);
    }
}

fn insert_awkward(engine: &Engine) {
    // Through an apply job, so every value is bound as a parameter.
    let changeset = sqlr_core::Changeset {
        table: table("src"),
        key_columns: vec!["id".into()],
        changes: awkward_rows()
            .into_iter()
            .map(|row| sqlr_core::Change::Insert {
                values: ["id", "t", "f", "b"]
                    .iter()
                    .map(|c| c.to_string())
                    .zip(row)
                    .collect(),
            })
            .collect(),
    };
    let job = engine.apply_changeset(&SessionKey::meta("db"), changeset);
    assert!(matches!(
        finished(engine, job),
        JobOutcome::Applied(r) if r.failed.is_none()
    ));
}

fn table(name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some("main".into()),
        name: name.into(),
    }
}

fn identity_mapping() -> Vec<(usize, Option<String>)> {
    ["id", "t", "f", "b"]
        .iter()
        .enumerate()
        .map(|(i, c)| (i, Some(c.to_string())))
        .collect()
}

fn options() -> ImportOptions {
    ImportOptions {
        batch: 500,
        commit_per_batch: false,
        skip_bad_rows: false,
        truncate_first: true,
        ..Default::default()
    }
}

#[test]
fn import_round_trips_export() {
    let dir = tempfile::tempdir().unwrap();
    let engine = sqlite_engine(&dir.path().join("t.db"));
    setup_tables(&engine);
    insert_awkward(&engine);
    let expected = rows(&engine, "db", "SELECT * FROM src ORDER BY id");
    assert_eq!(expected, awkward_rows());
    let formats = [
        ("csv", TextFormat::Csv(CsvOptions::default())),
        ("tsv", TextFormat::Tsv),
        ("json", TextFormat::Json),
        ("jsonl", TextFormat::JsonLines),
        (
            "semicolon.csv",
            TextFormat::Csv(CsvOptions {
                delimiter: ';',
                null_text: "\\N".into(),
                crlf: true,
                ..CsvOptions::default()
            }),
        ),
    ];
    for (ext, format) in formats {
        let path: PathBuf = dir.path().join(format!("src.{ext}"));
        let job = engine.export(
            ExportSource::Rerun {
                key: SessionKey::meta("db"),
                database: None,
                sql: "SELECT * FROM src ORDER BY id".into(),
            },
            format.clone(),
            path.clone(),
        );
        assert!(
            matches!(
                finished(&engine, job),
                JobOutcome::Exported { rows: 12, .. }
            ),
            "{ext}"
        );
        let job = engine.import(
            &SessionKey::meta("db"),
            path,
            format,
            table("dst"),
            identity_mapping(),
            options(),
        );
        assert_eq!(engine.job(job).unwrap().kind, JobKind::Import);
        assert_eq!(
            finished(&engine, job),
            JobOutcome::Imported {
                read: 12,
                written: 12,
                rejected: Vec::new()
            },
            "{ext}"
        );
        assert_eq!(
            rows(&engine, "db", "SELECT * FROM dst ORDER BY id"),
            expected,
            "{ext}"
        );
    }
}

#[test]
fn import_reports_rejected_rows() {
    let dir = tempfile::tempdir().unwrap();
    let engine = sqlite_engine(&dir.path().join("t.db"));
    run(
        &engine,
        "db",
        "CREATE TABLE p (id INTEGER NOT NULL, name TEXT)",
    );
    let path = dir.path().join("in.csv");
    std::fs::write(
        &path,
        "ID,Name,extra\n1,a,x\n\"2\",\"multi\nline\",x\nabc,c,x\n4,d,x\n",
    )
    .unwrap();
    // Mapped by name, case-insensitively; "extra" is skipped.
    let mapping = vec![(0, Some("id".into())), (1, Some("name".into())), (2, None)];
    let job = engine.import(
        &SessionKey::meta("db"),
        path.clone(),
        TextFormat::Csv(CsvOptions::default()),
        table("p"),
        mapping.clone(),
        ImportOptions {
            skip_bad_rows: true,
            truncate_first: false,
            ..options()
        },
    );
    let JobOutcome::Imported {
        read,
        written,
        rejected,
    } = finished(&engine, job)
    else {
        panic!("expected an import report");
    };
    assert_eq!((read, written), (4, 3));
    // "abc" is on line 5: the quoted line break moved it down.
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].0, 5);
    assert!(rejected[0].1.contains("abc"), "{}", rejected[0].1);
    assert_eq!(
        rows(&engine, "db", "SELECT id, name FROM p ORDER BY id"),
        vec![
            vec![Value::Int(1), Value::Text("a".into())],
            vec![Value::Int(2), Value::Text("multi\nline".into())],
            vec![Value::Int(4), Value::Text("d".into())],
        ]
    );

    // By default the first bad row stops the import and nothing is written.
    run(&engine, "db", "DELETE FROM p");
    let job = engine.import(
        &SessionKey::meta("db"),
        path,
        TextFormat::Csv(CsvOptions::default()),
        table("p"),
        mapping,
        ImportOptions {
            truncate_first: false,
            ..options()
        },
    );
    let JobOutcome::Failed(e) = finished(&engine, job) else {
        panic!("the bad row stops the import");
    };
    assert!(e.message.contains("Line 5"), "{}", e.message);
    assert!(rows(&engine, "db", "SELECT * FROM p").is_empty());
}

#[test]
fn import_rejects_constraint_violations_per_row() {
    let dir = tempfile::tempdir().unwrap();
    let engine = sqlite_engine(&dir.path().join("t.db"));
    run(&engine, "db", "CREATE TABLE p (id INTEGER PRIMARY KEY)");
    let path = dir.path().join("in.jsonl");
    std::fs::write(&path, "{\"id\":1}\n{\"id\":1}\n{\"id\":2}\n").unwrap();
    let job = engine.import(
        &SessionKey::meta("db"),
        path,
        TextFormat::JsonLines,
        table("p"),
        vec![(0, Some("id".into()))],
        ImportOptions {
            skip_bad_rows: true,
            batch: 2,
            commit_per_batch: true,
            truncate_first: false,
            ..Default::default()
        },
    );
    let JobOutcome::Imported {
        written, rejected, ..
    } = finished(&engine, job)
    else {
        panic!("expected an import report");
    };
    assert_eq!(written, 2);
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].0, 2);
    assert_eq!(
        rows(&engine, "db", "SELECT id FROM p ORDER BY id"),
        vec![vec![Value::Int(1)], vec![Value::Int(2)]]
    );
}

#[test]
fn save_text_writes_the_file() {
    let (engine, _fake) = fake_engine(FakeScript::default());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rejected.csv");
    let job = engine.save_text(path.clone(), "line,reason\n3,bad\n".into());
    assert_eq!(
        finished(&engine, job),
        JobOutcome::Exported {
            rows: 2,
            path: path.clone()
        }
    );
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "line,reason\n3,bad\n"
    );
}

#[test]
fn copy_job_builds_clipboard_text() {
    let (engine, _fake) = fake_engine(FakeScript {
        rows: 5,
        ..Default::default()
    });
    let q = run(&engine, "c", "SELECT * FROM t");
    let job = engine.copy_text(
        q,
        vec![4, 0, 2],
        vec![0],
        TextFormat::Csv(CsvOptions {
            header: false,
            ..CsvOptions::tsv()
        }),
    );
    assert_eq!(engine.job(job).unwrap().kind, JobKind::Clipboard);
    assert_eq!(
        finished(&engine, job),
        JobOutcome::Copied("4\n0\n2\n".into())
    );
}

#[test]
fn copy_job_spans_several_batches() {
    let (engine, _fake) = fake_engine(FakeScript {
        rows: 2500,
        ..Default::default()
    });
    let q = run(&engine, "c", "SELECT * FROM t");
    let rows: Vec<usize> = (0..2500).rev().collect();
    let job = engine.copy_text(q, rows, vec![0], TextFormat::JsonLines);
    let JobOutcome::Copied(text) = finished(&engine, job) else {
        panic!("expected clipboard text");
    };
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2500);
    assert_eq!(lines[0], "{\"n\":2499}");
    assert_eq!(lines[2499], "{\"n\":0}");
}

#[test]
fn export_rerun_refuses_a_multi_statement_source() {
    let (engine, fake) = fake_engine(FakeScript {
        rows: 3,
        ..Default::default()
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.csv");
    let before = fake.executed().len();
    let job = engine.export(
        ExportSource::Rerun {
            key: SessionKey::meta("c"),
            database: None,
            sql: "SELECT 1; DELETE FROM t".into(),
        },
        TextFormat::Csv(CsvOptions::default()),
        path.clone(),
    );
    let JobOutcome::Failed(e) = finished(&engine, job) else {
        panic!("a multi-statement source is never run again by an export");
    };
    assert!(e.message.contains("only reads"), "{}", e.message);
    assert_eq!(fake.executed().len(), before, "nothing ran");
    assert!(!path.exists());
}

#[test]
fn export_rerun_refuses_a_statement_that_writes() {
    let (engine, fake) = fake_engine(FakeScript {
        rows: 3,
        ..Default::default()
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.csv");
    let before = fake.executed().len();
    let job = engine.export(
        ExportSource::Rerun {
            key: SessionKey::meta("c"),
            database: None,
            sql: "INSERT INTO t VALUES (1) RETURNING *".into(),
        },
        TextFormat::Csv(CsvOptions::default()),
        path.clone(),
    );
    let JobOutcome::Failed(e) = finished(&engine, job) else {
        panic!("a statement that writes is never run again by an export");
    };
    assert!(e.message.contains("only reads"), "{}", e.message);
    assert_eq!(fake.executed().len(), before, "nothing ran");
    assert!(!path.exists());
}

#[test]
fn copy_all_results_reads_beyond_the_loaded_page() {
    let (engine, _) = fake_engine(FakeScript {
        rows: 2500,
        ..Default::default()
    });
    let q = engine.execute("c", "SELECT * FROM t".into(), 10);
    assert!(wait_until(TIMEOUT, || engine.result(q).is_some_and(
        |r| matches!(r.read().status, QueryStatus::Ready { .. })
    )));
    assert_eq!(engine.result(q).unwrap().read().rows.row_count(), 10);
    let job = engine.copy_query(&SessionKey::meta("c"), None, "SELECT * FROM t".into());
    let JobOutcome::GridCopied(data) = finished(&engine, job) else {
        panic!("expected typed clipboard");
    };
    assert_eq!(data.rows.len(), 2500);
    assert_eq!(data.rows[2499][0], Some(Value::Int(2499)));
    assert_eq!(engine.job(job).unwrap().progress.rows, 2500);
}

#[test]
fn copy_all_results_never_reruns_a_writing_statement() {
    let (engine, fake) = fake_engine(FakeScript::default());
    let before = fake.executed().len();
    let job = engine.copy_query(
        &SessionKey::meta("c"),
        None,
        "SELECT 1; DELETE FROM t".into(),
    );
    let JobOutcome::Failed(error) = finished(&engine, job) else {
        panic!("expected read-only validation");
    };
    assert!(error.message.contains("only reads"));
    assert_eq!(fake.executed().len(), before);
}

#[test]
fn copying_all_results_can_be_cancelled_while_fetching() {
    let (engine, fake) = fake_engine(FakeScript {
        rows: 10,
        block_on_fetch: true,
        ..Default::default()
    });
    let job = engine.copy_query(&SessionKey::meta("c"), None, "SELECT * FROM t".into());
    assert!(wait_until(TIMEOUT, || !fake.executed().is_empty()));
    engine.cancel_job(job);
    assert_eq!(finished(&engine, job), JobOutcome::Cancelled);
}

#[test]
fn background_grid_copy_keeps_pending_values_and_selection_order() {
    let (engine, _) = fake_engine(FakeScript {
        rows: 5,
        ..Default::default()
    });
    let q = run(&engine, "c", "SELECT * FROM t");
    let rows = engine.result(q).unwrap().read().rows.clone();
    let source = sqlr_core::ClipboardSource {
        rows,
        order: vec![(0, 4), (1, 0)],
        cols: vec![0],
        regions: vec![sqlr_core::ClipboardRegion {
            rows: 0..=1,
            cols: 0..=0,
        }],
        overrides: [((4, 0), Value::Int(99))].into(),
    };
    let job = engine.copy_grid(
        q,
        source,
        TextFormat::Csv(CsvOptions {
            header: false,
            ..CsvOptions::tsv()
        }),
    );
    let JobOutcome::GridCopied(data) = finished(&engine, job) else {
        panic!("expected typed clipboard");
    };
    assert_eq!(data.text, "99\n0\n");
    assert_eq!(
        data.rows,
        vec![vec![Some(Value::Int(99))], vec![Some(Value::Int(0))]]
    );
    assert_eq!(engine.job(job).unwrap().progress.rows, 2);
    assert_eq!(engine.job(job).unwrap().total, Some(2));
}
