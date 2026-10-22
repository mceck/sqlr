//! Reading and writing every transfer file format (§8.5), on SQLite so it needs no Docker.

mod support;

use std::path::Path;
use std::sync::Arc;

use sqlr_core::{
    ConnectionConfig, CsvOptions, Endpoint, Secrets, TableRef, TextFormat, TransferOptions,
    TransferSpec, Value,
};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{DriverRegistry, Engine, JobId, JobOutcome, QueryId, QueryStatus, SessionStatus};
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn engine(path: &Path) -> Engine {
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

fn run(engine: &Engine, sql: &str) -> QueryId {
    let q = engine.execute("db", sql.into(), 1_000_000);
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

fn rows(engine: &Engine, sql: &str) -> Vec<Vec<Value>> {
    let q = run(engine, sql);
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

fn table_endpoint(name: &str) -> Endpoint {
    Endpoint::Table {
        conn: "db".into(),
        table: table(name),
    }
}

fn spec(source: Endpoint, target: Endpoint) -> TransferSpec {
    TransferSpec {
        source,
        target,
        mapping: Vec::new(),
        options: TransferOptions::default(),
    }
}

fn t(s: &str) -> Value {
    Value::Text(s.into())
}

/// Connections need two tables to move between; this is a single-connection engine, so
/// the files go through `Query` sources and `Table` targets.
fn query(sql: &str) -> Endpoint {
    Endpoint::Query {
        conn: "db".into(),
        database: None,
        sql: sql.into(),
    }
}

#[test]
fn file_round_trips_each_format() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("t.db"));
    run(
        &engine,
        "CREATE TABLE src (id INTEGER, t TEXT, f REAL, b BLOB)",
    );
    run(
        &engine,
        "INSERT INTO src VALUES \
         (1, 'plain', 1.5, NULL), (2, 'a,b', NULL, NULL), (3, 'say \"hi\"', -2.25, NULL), \
         (4, 'line' || char(10) || 'break', 0.1, NULL), (5, '😀 è', 1e10, NULL), \
         (6, NULL, NULL, NULL), (7, 'bytes', 3.0, X'00ff10')",
    );
    let expected = rows(&engine, "SELECT id, t, f, b FROM src ORDER BY id");
    assert_eq!(expected.len(), 7);

    type Make = Box<dyn Fn(std::path::PathBuf) -> Endpoint>;
    let formats: Vec<(&str, Make)> = vec![
        (
            "csv",
            Box::new(|path| Endpoint::File {
                path,
                format: TextFormat::Csv(CsvOptions::default()),
            }),
        ),
        (
            "tsv",
            Box::new(|path| Endpoint::File {
                path,
                format: TextFormat::Tsv,
            }),
        ),
        (
            "json",
            Box::new(|path| Endpoint::File {
                path,
                format: TextFormat::Json,
            }),
        ),
        (
            "jsonl",
            Box::new(|path| Endpoint::File {
                path,
                format: TextFormat::JsonLines,
            }),
        ),
        (
            "xlsx",
            Box::new(|path| Endpoint::Workbook {
                path,
                sheet: None,
                header: true,
            }),
        ),
        ("parquet", Box::new(|path| Endpoint::Parquet { path })),
    ];
    for (name, endpoint) in formats {
        let path = dir.path().join(format!("out.{name}"));
        let job = engine.transfer(spec(
            query("SELECT id, t, f, b FROM src ORDER BY id"),
            endpoint(path.clone()),
        ));
        match finished(&engine, job) {
            JobOutcome::Exported { rows, .. } => assert_eq!(rows, 7, "{name}"),
            other => panic!("{name}: {other:?}"),
        }
        assert!(path.exists(), "{name}: the file was written");

        run(
            &engine,
            &format!("CREATE TABLE dst_{name} (id INTEGER, t TEXT, f REAL, b BLOB)"),
        );
        let job = engine.transfer(spec(endpoint(path), table_endpoint(&format!("dst_{name}"))));
        match finished(&engine, job) {
            JobOutcome::Imported {
                read,
                written,
                rejected,
            } => {
                assert_eq!((read, written), (7, 7), "{name}: {rejected:?}");
            }
            other => panic!("{name}: {other:?}"),
        }
        assert_eq!(
            rows(
                &engine,
                &format!("SELECT id, t, f, b FROM dst_{name} ORDER BY id")
            ),
            expected,
            "{name} round trip"
        );
    }
}

fn csv_endpoint(path: &Path) -> Endpoint {
    Endpoint::File {
        path: path.to_path_buf(),
        format: TextFormat::Csv(CsvOptions::default()),
    }
}

fn load_into_text_table(engine: &Engine, source: Endpoint, options: TransferOptions) -> JobOutcome {
    run(engine, "DROP TABLE IF EXISTS msg");
    run(engine, "CREATE TABLE msg (a TEXT, b TEXT, c TEXT)");
    let mut s = spec(source, table_endpoint("msg"));
    s.options = options;
    let job = engine.transfer(s);
    finished(engine, job)
}

#[test]
fn readers_handle_messy_csv() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("t.db"));

    // BOM, CRLF, a quoted newline, a ragged short row and blank lines at the end.
    let path = dir.path().join("messy.csv");
    std::fs::write(
        &path,
        b"\xEF\xBB\xBFa,b,c\r\n1,\"two\r\nlines\",3\r\n4,5\r\n\r\n\r\n",
    )
    .unwrap();
    let outcome = load_into_text_table(&engine, csv_endpoint(&path), TransferOptions::default());
    assert!(
        matches!(outcome, JobOutcome::Imported { written: 2, .. }),
        "{outcome:?}"
    );
    let got = rows(&engine, "SELECT a, b, c FROM msg ORDER BY a");
    assert_eq!(got[0], vec![t("1"), t("two\r\nlines"), t("3")]);
    assert_eq!(got[1], vec![t("4"), t("5"), Value::Null]);

    // Latin-1 text needs its label.
    let path = dir.path().join("latin1.csv");
    std::fs::write(&path, b"a,b,c\ncaf\xe9,\xfcber,x\n").unwrap();
    let options = TransferOptions {
        encoding: Some("latin1".into()),
        ..TransferOptions::default()
    };
    let outcome = load_into_text_table(&engine, csv_endpoint(&path), options);
    assert!(matches!(outcome, JobOutcome::Imported { written: 1, .. }));
    assert_eq!(
        rows(&engine, "SELECT a, b FROM msg"),
        vec![vec![t("café"), t("über")]]
    );

    // UTF-16 with a byte-order mark is detected.
    let path = dir.path().join("utf16.csv");
    let bytes: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain("a,b,c\nß,ñ,€\n".encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    std::fs::write(&path, bytes).unwrap();
    let outcome = load_into_text_table(&engine, csv_endpoint(&path), TransferOptions::default());
    assert!(matches!(outcome, JobOutcome::Imported { written: 1, .. }));
    assert_eq!(
        rows(&engine, "SELECT a, b, c FROM msg"),
        vec![vec![t("ß"), t("ñ"), t("€")]]
    );
}

#[test]
fn readers_handle_excel_dates_formulas_and_blanks() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("t.db"));
    let path = dir.path().join("book.xlsx");
    {
        use rust_xlsxwriter::{ExcelDateTime, Format, Formula, Workbook};
        let mut book = Workbook::new();
        let sheet = book.add_worksheet();
        sheet.set_name("Data").unwrap();
        sheet.write_string(0, 0, "a").unwrap();
        sheet.write_string(0, 1, "b").unwrap();
        sheet.write_string(0, 2, "c").unwrap();
        let date = Format::new().set_num_format("yyyy-mm-dd");
        let when = ExcelDateTime::from_ymd(2024, 3, 9).unwrap();
        sheet
            .write_datetime_with_format(1, 0, &when, &date)
            .unwrap();
        sheet.write_number(1, 1, 2.0).unwrap();
        // The cached result of a formula is what a reader sees.
        sheet
            .write_formula(1, 2, Formula::new("=B2*21").set_result("42"))
            .unwrap();
        // Row 3: a blank cell in the middle.
        sheet.write_string(2, 0, "x").unwrap();
        sheet.write_string(2, 2, "z").unwrap();
        book.save(&path).unwrap();
    }
    let outcome = load_into_text_table(
        &engine,
        Endpoint::Workbook {
            path,
            sheet: Some("Data".into()),
            header: true,
        },
        TransferOptions::default(),
    );
    assert!(
        matches!(outcome, JobOutcome::Imported { written: 2, .. }),
        "{outcome:?}"
    );
    let got = rows(&engine, "SELECT a, c FROM msg ORDER BY a");
    assert_eq!(got[0], vec![t("2024-03-09"), t("42")], "{got:?}");
    assert_eq!(got[1], vec![t("x"), t("z")]);
    assert_eq!(
        rows(&engine, "SELECT b FROM msg WHERE a = 'x'"),
        vec![vec![Value::Null]]
    );
}

#[test]
fn readers_handle_parquet_nulls_and_decimals() {
    use parquet::basic::{LogicalType, Repetition, Type as Physical};
    use parquet::data_type::{ByteArray, ByteArrayType, Int32Type};
    use parquet::file::properties::WriterProperties;
    use parquet::file::writer::SerializedFileWriter;
    use parquet::schema::types::Type;

    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("t.db"));
    let path = dir.path().join("typed.parquet");
    {
        let fields = vec![
            Arc::new(
                Type::primitive_type_builder("a", Physical::BYTE_ARRAY)
                    .with_repetition(Repetition::OPTIONAL)
                    .with_logical_type(Some(LogicalType::String))
                    .build()
                    .unwrap(),
            ),
            Arc::new(
                Type::primitive_type_builder("b", Physical::INT32)
                    .with_repetition(Repetition::OPTIONAL)
                    .with_logical_type(Some(LogicalType::decimal(2, 9)))
                    .with_precision(9)
                    .with_scale(2)
                    .build()
                    .unwrap(),
            ),
            Arc::new(
                Type::primitive_type_builder("c", Physical::INT32)
                    .with_repetition(Repetition::OPTIONAL)
                    .build()
                    .unwrap(),
            ),
        ];
        let schema = Arc::new(
            Type::group_type_builder("schema")
                .with_fields(fields)
                .build()
                .unwrap(),
        );
        let file = std::fs::File::create(&path).unwrap();
        let mut writer =
            SerializedFileWriter::new(file, schema, Arc::new(WriterProperties::builder().build()))
                .unwrap();
        let mut group = writer.next_row_group().unwrap();
        let mut col = group.next_column().unwrap().unwrap();
        col.typed::<ByteArrayType>()
            .write_batch(&[ByteArray::from("one")], Some(&[1, 0]), None)
            .unwrap();
        col.close().unwrap();
        let mut col = group.next_column().unwrap().unwrap();
        col.typed::<Int32Type>()
            .write_batch(&[1234], Some(&[1, 0]), None)
            .unwrap();
        col.close().unwrap();
        let mut col = group.next_column().unwrap().unwrap();
        col.typed::<Int32Type>()
            .write_batch(&[], Some(&[0, 0]), None)
            .unwrap();
        col.close().unwrap();
        group.close().unwrap();
        writer.close().unwrap();
    }
    let outcome = load_into_text_table(
        &engine,
        Endpoint::Parquet { path },
        TransferOptions::default(),
    );
    assert!(
        matches!(outcome, JobOutcome::Imported { written: 2, .. }),
        "{outcome:?}"
    );
    let got = rows(&engine, "SELECT a, b, c FROM msg ORDER BY a IS NULL, a");
    // The decimal keeps its exact digits; nulls stay NULL.
    assert_eq!(got[0], vec![t("one"), t("12.34"), Value::Null], "{got:?}");
    assert_eq!(got[1], vec![Value::Null, Value::Null, Value::Null]);
}
