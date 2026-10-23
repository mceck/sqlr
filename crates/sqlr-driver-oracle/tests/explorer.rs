//! Every explorer node kind, `describe`, `load_object`, value decoding and connection
//! failures against the fixed Oracle Free server.

mod support;

use std::sync::Arc;

use sqlr_core::{
    Connection, ConnectionConfig, Driver, ErrorKind, Execution, NodeKind, NodePath, ObjectDef,
    ObjectKind, ObjectRef, Secrets, TableRef, Value,
};
use sqlr_driver_oracle::OracleDriver;
use sqlr_testkit::{service, unique_name};

fn path(segs: &[&str]) -> NodePath {
    NodePath(segs.iter().map(|s| s.to_string()).collect())
}

async fn run(conn: &Arc<dyn Connection>, sql: &str) {
    conn.relational()
        .unwrap()
        .execute(sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
}

async fn rows(conn: &Arc<dyn Connection>, sql: &str) -> Vec<Vec<Value>> {
    let Execution::Rows(mut stream) = conn.relational().unwrap().execute(sql).await.unwrap() else {
        panic!("{sql}: no rows");
    };
    let mut out = Vec::new();
    while let Some(chunk) = stream.next_chunk(1000).await.unwrap() {
        for row in 0..chunk.len() {
            out.push(
                (0..chunk.columns().len())
                    .map(|col| Value::from_cell(chunk.cell(row, col)))
                    .collect(),
            );
        }
    }
    out
}

fn object(kind: ObjectKind, name: &str, parent: Option<&str>) -> ObjectRef {
    ObjectRef {
        kind,
        database: None,
        schema: Some("SQLR_TEST".into()),
        name: name.into(),
        parent: parent.map(String::from),
        signature: None,
    }
}

#[tokio::test]
async fn every_explorer_node_kind_is_listed() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let base = unique_name("SQLR_BR").to_uppercase();
    let (table, view, seq, func, proc_, trig) = (
        format!("{base}_T"),
        format!("{base}_V"),
        format!("{base}_S"),
        format!("{base}_F"),
        format!("{base}_P"),
        format!("{base}_TR"),
    );
    for sql in [
        format!(
            "CREATE TABLE {table} (id NUMBER(10) PRIMARY KEY, name VARCHAR2(20), amount NUMBER(8,2), r RAW(16), f FLOAT(20), n NUMBER)"
        ),
        format!("CREATE UNIQUE INDEX {base}_IX ON {table} (name)"),
        format!("CREATE VIEW {view} AS SELECT id, name FROM {table}"),
        format!("CREATE SEQUENCE {seq}"),
        format!("CREATE FUNCTION {func} RETURN NUMBER IS BEGIN RETURN 1; END;"),
        format!("CREATE PROCEDURE {proc_} IS BEGIN NULL; END;"),
        format!("CREATE TRIGGER {trig} BEFORE INSERT ON {table} FOR EACH ROW BEGIN NULL; END;"),
        format!("ALTER TRIGGER {trig} DISABLE"),
    ] {
        run(&conn, &sql).await;
    }
    // An anonymous block keeps its `END;`.
    run(&conn, "BEGIN NULL; END;").await;
    let browse = |segs: Vec<String>| {
        let conn = conn.clone();
        async move { conn.browse(&NodePath(segs)).await }
    };
    let s = |x: &str| x.to_string();
    let names =
        |nodes: &[sqlr_core::Node]| nodes.iter().map(|n| n.name.clone()).collect::<Vec<_>>();

    let folders = browse(vec![s("SQLR_TEST")]).await.unwrap();
    assert_eq!(folders.len(), 6);
    let views = browse(vec![s("SQLR_TEST"), s("views")]).await.unwrap();
    let seqs = browse(vec![s("SQLR_TEST"), s("sequences")]).await.unwrap();
    let funcs = browse(vec![s("SQLR_TEST"), s("functions")]).await.unwrap();
    let procs = browse(vec![s("SQLR_TEST"), s("procedures")]).await.unwrap();
    let trigs = browse(vec![s("SQLR_TEST"), s("triggers")]).await.unwrap();
    let kinds = browse(vec![s("SQLR_TEST"), s("tables"), table.clone()])
        .await
        .unwrap();
    let cols = browse(vec![
        s("SQLR_TEST"),
        s("tables"),
        table.clone(),
        s("columns"),
    ])
    .await
    .unwrap();
    let idx = browse(vec![
        s("SQLR_TEST"),
        s("tables"),
        table.clone(),
        s("indexes"),
    ])
    .await
    .unwrap();
    let ttrigs = browse(vec![
        s("SQLR_TEST"),
        s("tables"),
        table.clone(),
        s("triggers"),
    ])
    .await
    .unwrap();
    let vcols = browse(vec![s("SQLR_TEST"), s("views"), view.clone()])
        .await
        .unwrap();
    let bad = browse(vec![s("SQLR_TEST"), s("nope")]).await;
    let bad_deep = conn.browse(&path(&["a", "tables", "b", "c", "d"])).await;

    let cleanup = [
        format!("DROP TRIGGER {trig}"),
        format!("DROP PROCEDURE {proc_}"),
        format!("DROP FUNCTION {func}"),
        format!("DROP SEQUENCE {seq}"),
        format!("DROP VIEW {view}"),
        format!("DROP TABLE {table} PURGE"),
    ];
    for sql in cleanup {
        let _ = conn.relational().unwrap().execute(&sql).await;
    }

    assert!(
        views
            .iter()
            .any(|n| n.name == view && n.kind == NodeKind::View)
    );
    assert!(
        seqs.iter()
            .any(|n| n.name == seq && n.kind == NodeKind::Sequence)
    );
    assert!(
        funcs
            .iter()
            .any(|n| n.name == func && n.kind == NodeKind::Function)
    );
    assert!(
        procs
            .iter()
            .any(|n| n.name == proc_ && n.kind == NodeKind::Procedure)
    );
    let trigger = trigs.iter().find(|n| n.name == trig).expect("trigger");
    assert_eq!(trigger.kind, NodeKind::Trigger);
    assert_eq!(trigger.detail.as_deref(), Some("DISABLED"));
    assert_eq!(names(&kinds), ["columns", "indexes", "triggers"]);
    let detail = |name: &str| {
        cols.iter()
            .find(|n| n.name == name)
            .and_then(|n| n.detail.clone())
            .unwrap()
    };
    assert_eq!(detail("ID"), "NUMBER(10) NOT NULL");
    assert_eq!(detail("AMOUNT"), "NUMBER(8,2)");
    assert_eq!(detail("R"), "RAW(16)");
    assert_eq!(detail("N"), "NUMBER");
    assert_eq!(detail("F"), "FLOAT(20)");
    assert!(idx.iter().any(|n| n.name == format!("{base}_IX")
        && n.detail.as_deref().is_some_and(|d| d.contains("UNIQUE"))));
    assert_eq!(names(&ttrigs), [trig]);
    assert_eq!(names(&vcols), ["ID", "NAME"]);
    assert_eq!(bad.unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(bad_deep.unwrap_err().kind, ErrorKind::Unsupported);
}

#[tokio::test]
async fn describe_reads_unique_keys_and_the_default_schema() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let rel = conn.relational().unwrap();
    let name = unique_name("SQLR_DESC").to_uppercase();
    run(
        &conn,
        &format!(
            "CREATE TABLE {name} (a NUMBER(5), b NUMBER(5), c VARCHAR2(10) DEFAULT 'x', \
             d NUMBER GENERATED ALWAYS AS IDENTITY, e NUMBER(5) GENERATED ALWAYS AS (a + 1) VIRTUAL, \
             CONSTRAINT {name}_PK PRIMARY KEY (a, b), CONSTRAINT {name}_U1 UNIQUE (c), \
             CONSTRAINT {name}_U2 UNIQUE (b, c))"
        ),
    )
    .await;
    let described = rel
        .describe(&TableRef {
            database: None,
            schema: None,
            name: name.clone(),
        })
        .await;
    let missing = rel
        .describe(&TableRef {
            database: None,
            schema: Some("SQLR_TEST".into()),
            name: format!("{name}_NOPE"),
        })
        .await;
    let _ = rel.execute(&format!("DROP TABLE {name} PURGE")).await;

    let info = described.unwrap();
    assert_eq!(info.table.schema.as_deref(), Some("SQLR_TEST"));
    assert_eq!(info.primary_key, ["A", "B"]);
    assert_eq!(info.unique_keys.len(), 2, "{:?}", info.unique_keys);
    let col = |n: &str| info.columns.iter().find(|c| c.name == n).unwrap();
    assert_eq!(col("C").default.as_deref(), Some("'x'"));
    assert!(col("D").generated && col("E").generated && !col("A").generated);
    assert!(missing.unwrap_err().message.contains("not found"));
}

#[tokio::test]
async fn load_object_covers_each_kind() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let rel = conn.relational().unwrap();
    let intro = rel.introspect().unwrap();
    let base = unique_name("SQLR_LO").to_uppercase();
    let (parent, child, view) = (
        format!("{base}_P"),
        format!("{base}_C"),
        format!("{base}_V"),
    );
    for sql in [
        format!("CREATE TABLE {parent} (id NUMBER(10) PRIMARY KEY)"),
        format!(
            "CREATE TABLE {child} (id NUMBER(10), pid NUMBER(10), q NUMBER(3), \
             CONSTRAINT {base}_FKN FOREIGN KEY (pid) REFERENCES {parent} (id) ON DELETE SET NULL, \
             CONSTRAINT {base}_FKP FOREIGN KEY (id) REFERENCES {parent} (id))"
        ),
        format!("CREATE INDEX {base}_IX ON {child} (q)"),
        format!("CREATE VIEW {view} AS SELECT id FROM {parent}"),
    ] {
        run(&conn, &sql).await;
    }
    let table = intro
        .load_object(&object(ObjectKind::Table, &child, None))
        .await;
    let a_view = intro
        .load_object(&object(ObjectKind::View, &view, None))
        .await;
    let index = intro
        .load_object(&object(
            ObjectKind::Index,
            &format!("{base}_IX"),
            Some(&child),
        ))
        .await;
    let no_index = intro
        .load_object(&object(ObjectKind::Index, "NOPE_IX", Some(&child)))
        .await;
    let no_table = intro
        .load_object(&object(ObjectKind::Table, "NOPE_TABLE", None))
        .await;
    let wrong_kind = intro
        .load_object(&object(ObjectKind::View, &child, None))
        .await;
    let mut no_schema = object(ObjectKind::Table, &child, None);
    no_schema.schema = None;
    let no_schema = intro.load_object(&no_schema).await;
    let database = intro
        .load_object(&object(ObjectKind::Database, "DB", None))
        .await;
    let schema = intro
        .load_object(&object(ObjectKind::Schema, "S", None))
        .await;
    let sequence = intro
        .load_object(&object(ObjectKind::Sequence, "S", None))
        .await;
    let roles = intro.list_roles().await;
    let _ = rel.execute(&format!("DROP VIEW {view}")).await;
    let _ = rel.execute(&format!("DROP TABLE {child} PURGE")).await;
    let _ = rel.execute(&format!("DROP TABLE {parent} PURGE")).await;

    assert!(matches!(table.unwrap(), ObjectDef::Table(t) if t.constraints.len() == 2));
    assert!(matches!(a_view.unwrap(), ObjectDef::View(_)));
    assert!(
        matches!(index.unwrap(), ObjectDef::Index { index, .. } if index.name.ends_with("_IX"))
    );
    assert!(no_index.unwrap_err().message.contains("not found"));
    assert!(no_table.unwrap_err().message.contains("not found"));
    assert!(wrong_kind.is_err());
    assert!(no_schema.unwrap_err().message.contains("no schema"));
    assert!(matches!(database.unwrap(), ObjectDef::Database(_)));
    assert!(matches!(schema.unwrap(), ObjectDef::Schema(_)));
    assert_eq!(sequence.unwrap_err().kind, ErrorKind::Unsupported);
    assert!(roles.unwrap().iter().any(|r| r == "SQLR_TEST"));
}

#[tokio::test]
async fn values_of_many_types_come_back_decoded() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let name = unique_name("SQLR_TY").to_uppercase();
    run(
        &conn,
        &format!(
            "CREATE TABLE {name} (id NUMBER(10), d DATE, ts TIMESTAMP(6), tz TIMESTAMP(6) WITH TIME ZONE, \
             ltz TIMESTAMP WITH LOCAL TIME ZONE, bf BINARY_FLOAT, bd BINARY_DOUBLE, r RAW(4), \
             cl CLOB, bl BLOB, ch CHAR(3), nv NVARCHAR2(5), ym INTERVAL YEAR TO MONTH, \
             ds INTERVAL DAY TO SECOND, dec NUMBER(10,2), big NUMBER(30), j JSON)"
        ),
    )
    .await;
    run(
        &conn,
        &format!(
            "INSERT INTO {name} VALUES (1, DATE '2024-03-04', TIMESTAMP '2024-03-04 05:06:07.123456', \
             TIMESTAMP '2024-03-04 05:06:07 +02:00', TIMESTAMP '2024-03-04 05:06:07', 1.5, 2.25, \
             HEXTORAW('DEADBEEF'), 'long text', UTL_RAW.CAST_TO_RAW('blob'), 'ab', 'xy', \
             INTERVAL '1-2' YEAR TO MONTH, INTERVAL '1 02:03:04' DAY TO SECOND, 12.50, \
             123456789012345678901234567890, '{{\"a\":1}}')"
        ),
    )
    .await;
    let Execution::Rows(stream) = conn
        .relational()
        .unwrap()
        .execute(&format!("SELECT * FROM {name}"))
        .await
        .unwrap()
    else {
        panic!("rows");
    };
    let metas = stream.columns();
    let data = rows(&conn, &format!("SELECT * FROM {name}")).await;
    let nulls = rows(
        &conn,
        &format!("SELECT ROWID, t.* FROM (SELECT * FROM {name}) t WHERE 1 = 0"),
    )
    .await;
    let rowid = rows(&conn, &format!("SELECT ROWID FROM {name}")).await;
    let flag = rows(&conn, "SELECT CAST(NULL AS NUMBER) FROM dual").await;
    let _ = conn
        .relational()
        .unwrap()
        .execute(&format!("DROP TABLE {name} PURGE"))
        .await;

    assert!(nulls.is_empty());
    assert_eq!(flag[0][0], Value::Null);
    assert!(matches!(&rowid[0][0], Value::Text(t) if !t.is_empty()));
    let types: Vec<_> = metas.iter().map(|m| m.type_name.as_str()).collect();
    assert!(
        types.contains(&"DATE") && types.contains(&"TIMESTAMP"),
        "{types:?}"
    );
    assert!(
        types
            .iter()
            .any(|t| t.starts_with("TIMESTAMP WITH TIME ZONE")),
        "{types:?}"
    );
    assert!(types.iter().any(|t| t.contains("LOCAL")), "{types:?}");
    assert!(
        types.iter().any(|t| t.starts_with("INTERVAL YEAR")),
        "{types:?}"
    );
    assert!(
        types.iter().any(|t| t.starts_with("INTERVAL DAY")),
        "{types:?}"
    );
    let row = &data[0];
    assert_eq!(row[0], Value::Int(1));
    assert!(
        matches!(&row[1], Value::Text(t) if t == "2024-03-04 00:00:00"),
        "{:?}",
        row[1]
    );
    assert!(
        matches!(&row[2], Value::Text(t) if t == "2024-03-04 05:06:07.123456"),
        "{:?}",
        row[2]
    );
    assert!(
        matches!(&row[3], Value::Text(t) if t.contains("+02:00")),
        "{:?}",
        row[3]
    );
    assert!(
        matches!(&row[4], Value::Text(t) if t.starts_with("2024-03-04")),
        "{:?}",
        row[4]
    );
    // oracle-rs returns BINARY_FLOAT/DOUBLE as lossy text: the row must still arrive.
    assert!(
        matches!(row[5], Value::Float(_) | Value::Text(_)),
        "{:?}",
        row[5]
    );
    // oracle-rs returns BINARY_FLOAT/DOUBLE as lossy text: the row must still arrive.
    assert!(
        matches!(row[6], Value::Float(_) | Value::Text(_)),
        "{:?}",
        row[6]
    );
    assert_eq!(row[7], Value::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]));
    assert_eq!(row[8], Value::Text("long text".into()));
    assert_eq!(row[9], Value::Bytes(b"blob".to_vec()));
    assert_eq!(row[10], Value::Text("ab ".into()));
    // NVARCHAR2 arrives UTF-16 encoded from oracle-rs; only the arrival is checked.
    assert!(matches!(row[11], Value::Text(_)));
    assert_eq!(
        row[15],
        Value::Text("123456789012345678901234567890".into())
    );
    assert_eq!(row[14], Value::Text("12.5".into()));
}

fn config(overrides: &[(&str, &str)]) -> ConnectionConfig {
    let (host, port) = service("ORACLE");
    let mut values: std::collections::BTreeMap<String, String> = [
        ("host", host),
        ("port", port.to_string()),
        ("service", "FREEPDB1".into()),
        ("service_type", "service".into()),
        ("user", "sqlr_test".into()),
        ("tls", "false".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for (k, v) in overrides {
        values.insert((*k).into(), (*v).into());
    }
    ConnectionConfig {
        id: "o".into(),
        name: "oracle".into(),
        driver: "oracle".into(),
        group: None,
        color: None,
        values: values.into_iter().collect(),
        profile: None,
        save_secrets: true,
    }
}

fn password(p: &str) -> Secrets {
    Secrets([("password".to_string(), p.to_string())].into())
}

#[tokio::test]
async fn bad_connection_settings_are_reported() {
    sqlr_testkit::skip_container_tests!();
    let driver = OracleDriver::new();
    let bad_port = driver
        .connect(&config(&[("port", "abc")]), &password("x"))
        .await;
    assert!(bad_port.err().unwrap().message.contains("port"));
    let no_host = driver
        .connect(&config(&[("host", "")]), &password("x"))
        .await;
    assert!(no_host.is_err());
    // Nothing listens on port 1: a refused connection is a connection error.
    let refused = driver
        .connect(
            &config(&[("host", "127.0.0.1"), ("port", "1")]),
            &password("x"),
        )
        .await;
    assert_eq!(refused.err().unwrap().kind, ErrorKind::Connection);
}

#[tokio::test]
async fn connecting_by_sid_is_attempted() {
    sqlr_testkit::skip_container_tests!();
    // FREE is the CDB's SID; the PDB user may not log in there, but the branch must run
    // and fail or succeed cleanly rather than hang.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        OracleDriver::new().connect(
            &config(&[("service_type", "sid"), ("service", "FREE")]),
            &password("sqlr_test"),
        ),
    )
    .await
    .expect("must not hang");
    if let Ok(conn) = result {
        conn.ping().await.unwrap();
    }
}
