//! Introspection corners, statement execution and connection failures on SQL Server 2025.

mod support;

use std::sync::Arc;

use sqlr_core::{
    ConnectionConfig, ConstraintDef, Driver, ErrorKind, Execution, FkAction, ObjectDef, ObjectKind,
    ObjectRef, Secrets, Value,
};
use sqlr_driver_mssql::SqlServerDriver;
use sqlr_testkit::service;
use support::server;

fn object(kind: ObjectKind, schema: Option<&str>, name: &str, parent: Option<&str>) -> ObjectRef {
    ObjectRef {
        kind,
        database: None,
        schema: schema.map(String::from),
        name: name.into(),
        parent: parent.map(String::from),
        signature: None,
    }
}

async fn run(conn: &Arc<dyn sqlr_core::Connection>, sql: &str) {
    conn.relational()
        .unwrap()
        .execute(sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
}

#[tokio::test]
async fn schema_corners_and_object_loading() {
    sqlr_testkit::skip_container_tests!();
    let (_c, conn) = server().await;
    for sql in [
        "CREATE SCHEMA shop",
        "CREATE TABLE shop.parent (id int NOT NULL PRIMARY KEY, a int NOT NULL, b int NOT NULL, \
         CONSTRAINT uq_ab UNIQUE (a, b))",
        "CREATE TABLE shop.child (id int NOT NULL PRIMARY KEY, a int NULL, b int NULL, \
         pid int NULL, area AS (a * b) PERSISTED, \
         CONSTRAINT fk_pid FOREIGN KEY (pid) REFERENCES shop.parent (id) \
         ON DELETE SET NULL ON UPDATE NO ACTION, \
         CONSTRAINT fk_ab FOREIGN KEY (a, b) REFERENCES shop.parent (a, b))",
        "CREATE TABLE shop.child2 (id int NOT NULL PRIMARY KEY, pid int NOT NULL DEFAULT 0, \
         CONSTRAINT fk_c2 FOREIGN KEY (pid) REFERENCES shop.parent (id) \
         ON DELETE SET DEFAULT ON UPDATE CASCADE)",
        "CREATE UNIQUE INDEX ux_child_a ON shop.child (a) WHERE a IS NOT NULL",
        "CREATE VIEW shop.v (x, y) AS SELECT a, b FROM shop.child",
    ] {
        run(&conn, sql).await;
    }
    let intro = conn.relational().unwrap().introspect().unwrap();
    let snapshot = intro.load_schema(None, "shop").await.unwrap();
    assert_eq!(snapshot.engine, "sqlserver");
    let child = snapshot
        .objects
        .iter()
        .find_map(|o| match o {
            ObjectDef::Table(t) if t.table.name == "child" => Some(t),
            _ => None,
        })
        .unwrap();
    let area = child.columns.iter().find(|c| c.name == "area").unwrap();
    assert!(area.generated.is_some() && area.generated_stored);
    let actions: Vec<_> = child
        .constraints
        .iter()
        .filter_map(|c| match c {
            ConstraintDef::ForeignKey {
                name,
                columns,
                on_delete,
                on_update,
                ..
            } => Some((name.clone().unwrap(), columns.len(), *on_delete, *on_update)),
            _ => None,
        })
        .collect();
    assert!(actions.contains(&("fk_pid".into(), 1, FkAction::SetNull, FkAction::NoAction)));
    assert!(actions.contains(&("fk_ab".into(), 2, FkAction::NoAction, FkAction::NoAction)));
    let child2 = snapshot
        .objects
        .iter()
        .find_map(|o| match o {
            ObjectDef::Table(t) if t.table.name == "child2" => Some(t),
            _ => None,
        })
        .unwrap();
    assert!(child2.constraints.iter().any(|c| matches!(
        c,
        ConstraintDef::ForeignKey {
            on_delete: FkAction::SetDefault,
            on_update: FkAction::Cascade,
            ..
        }
    )));
    let ux = child
        .indexes
        .iter()
        .find(|i| i.name == "ux_child_a")
        .unwrap();
    assert!(ux.unique && ux.predicate.is_some());
    let parent = snapshot
        .objects
        .iter()
        .find_map(|o| match o {
            ObjectDef::Table(t) if t.table.name == "parent" => Some(t),
            _ => None,
        })
        .unwrap();
    assert!(parent.constraints.iter().any(|c| matches!(
        c, ConstraintDef::Unique { columns, .. } if columns.len() == 2)));
    let view = snapshot
        .objects
        .iter()
        .find_map(|o| match o {
            ObjectDef::View(v) => Some(v),
            _ => None,
        })
        .unwrap();
    assert!(
        view.query.to_uppercase().starts_with("SELECT"),
        "{}",
        view.query
    );

    // Objects by reference.
    let db = intro
        .load_object(&object(ObjectKind::Database, None, "master", None))
        .await
        .unwrap();
    assert!(matches!(db, ObjectDef::Database(d) if d.name == "master"));
    let schema = intro
        .load_object(&object(ObjectKind::Schema, None, "shop", None))
        .await
        .unwrap();
    assert!(matches!(schema, ObjectDef::Schema(s) if s.name == "shop"));
    let view = intro
        .load_object(&object(ObjectKind::View, Some("shop"), "v", None))
        .await
        .unwrap();
    assert!(matches!(view, ObjectDef::View(_)));
    let index = intro
        .load_object(&object(
            ObjectKind::Index,
            Some("shop"),
            "ux_child_a",
            Some("child"),
        ))
        .await
        .unwrap();
    assert!(matches!(index, ObjectDef::Index { index, .. } if index.name == "ux_child_a"));
    for missing in [
        object(ObjectKind::Index, Some("shop"), "nope", Some("child")),
        object(ObjectKind::Table, Some("shop"), "nope", None),
        object(ObjectKind::View, Some("shop"), "child", None),
    ] {
        let err = intro.load_object(&missing).await.unwrap_err();
        assert!(err.message.contains("not found"), "{err:?}");
    }
    let err = intro
        .load_object(&object(ObjectKind::Function, Some("shop"), "f", None))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    let err = intro.load_schema(Some("other"), "shop").await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert!(intro.load_schema(Some("master"), "shop").await.is_ok());

    run(&conn, "CREATE ROLE sqlr_cov_role").await;
    let roles = intro.list_roles().await.unwrap();
    run(&conn, "DROP ROLE sqlr_cov_role").await;
    assert!(roles.contains(&"sqlr_cov_role".to_string()), "{roles:?}");
}

#[tokio::test]
async fn statements_report_rows_counts_and_errors() {
    sqlr_testkit::skip_container_tests!();
    let (_c, conn) = server().await;
    let rel = conn.relational().unwrap();
    run(&conn, "CREATE SCHEMA shop").await;
    run(
        &conn,
        "CREATE TABLE shop.t (id int PRIMARY KEY, v nvarchar(20))",
    )
    .await;
    let affected = |e: Execution| match e {
        Execution::Affected(n) => n,
        Execution::Rows(_) => panic!("rows"),
    };
    assert_eq!(
        affected(
            rel.execute("INSERT INTO shop.t VALUES (1, N'a'), (2, N'b')")
                .await
                .unwrap()
        ),
        2
    );
    assert_eq!(
        affected(
            rel.execute_with(
                "UPDATE shop.t SET v = @P1 WHERE id = @P2",
                &[Value::Text("z".into()), Value::Int(1)]
            )
            .await
            .unwrap()
        ),
        1
    );
    // A row value holding the word OUTPUT is not an OUTPUT clause: the count must survive.
    assert_eq!(
        affected(
            rel.execute("UPDATE shop.t SET v = N'a OUTPUT b' WHERE id = 1")
                .await
                .unwrap()
        ),
        1
    );
    // MERGE needs its terminating semicolon.
    let merged = rel
        .execute(
            "MERGE shop.t AS d USING (SELECT 3 AS id, N'c' AS v) AS s ON d.id = s.id \
             WHEN NOT MATCHED THEN INSERT (id, v) VALUES (s.id, s.v)",
        )
        .await
        .unwrap();
    assert_eq!(affected(merged), 1);
    // OUTPUT returns rows; a statement with no result set yields a zero count.
    let Execution::Rows(mut s) = rel
        .execute("DELETE FROM shop.t OUTPUT deleted.id WHERE id = 3")
        .await
        .unwrap()
    else {
        panic!("rows")
    };
    assert_eq!(s.next_chunk(10).await.unwrap().unwrap().len(), 1);
    assert!(s.next_chunk(10).await.unwrap().is_none());
    s.close().await.unwrap();
    assert_eq!(affected(rel.execute("SET NOCOUNT ON").await.unwrap()), 0);
    // Paging: a small chunk size splits the result.
    let Execution::Rows(mut s) = rel
        .execute("SELECT id FROM shop.t ORDER BY id")
        .await
        .unwrap()
    else {
        panic!("rows")
    };
    assert_eq!(s.next_chunk(1).await.unwrap().unwrap().len(), 1);
    assert_eq!(s.next_chunk(1).await.unwrap().unwrap().len(), 1);
    assert!(s.next_chunk(1).await.unwrap().is_none());

    let Err(e) = rel.execute("SELECT FROM WHERE").await else {
        panic!("syntax")
    };
    assert_eq!(e.kind, ErrorKind::Syntax);
    let Err(e) = rel.execute("INSERT INTO shop.t VALUES (1, N'dup')").await else {
        panic!("duplicate")
    };
    assert_eq!(e.kind, ErrorKind::Other);
    let Err(e) = rel
        .execute_with("SELECT FROM WHERE @P1", &[Value::Int(1)])
        .await
    else {
        panic!("syntax")
    };
    assert_eq!(e.kind, ErrorKind::Syntax);
}

fn config(values: &[(&str, &str)]) -> ConnectionConfig {
    let (host, port) = service("MSSQL");
    let mut all: std::collections::BTreeMap<String, String> = [
        ("host", host),
        ("port", port.to_string()),
        ("database", "master".into()),
        ("user", "sa".into()),
        ("encryption", "not_supported".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for (k, v) in values {
        if v.is_empty() {
            all.remove(*k);
        } else {
            all.insert(k.to_string(), v.to_string());
        }
    }
    ConnectionConfig {
        id: "t".into(),
        name: "t".into(),
        driver: "sqlserver".into(),
        group: None,
        color: None,
        values: all,
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}

#[tokio::test]
async fn bad_connection_settings_fail_with_a_useful_kind() {
    sqlr_testkit::skip_container_tests!();
    let driver = SqlServerDriver::new();
    let pw = |p: &str| Secrets([("password".to_string(), p.to_string())].into());
    let good = pw("yourStrong(!)Password");
    let err = |r: Result<_, sqlr_core::DriverError>| match r {
        Err(e) => e,
        Ok(_) => panic!("connected"),
    };
    let e = err(driver
        .connect(&config(&[("encryption", "bogus")]), &good)
        .await);
    assert!(e.message.contains("invalid encryption"), "{e:?}");
    let e = err(driver.connect(&config(&[("port", "abc")]), &good).await);
    assert!(e.message.contains("invalid port"), "{e:?}");
    let e = err(driver
        .connect(&config(&[("property.1bad", "x")]), &good)
        .await);
    assert!(e.message.contains("Invalid driver property"), "{e:?}");
    // Reaching the server requires it to be up, which the shared helper guarantees.
    let _server = server().await;
    let e = err(driver.connect(&config(&[]), &pw("wrong")).await);
    assert_eq!(e.kind, ErrorKind::Auth);
    let e = err(driver.connect(&config(&[("port", "1")]), &good).await);
    assert!(
        matches!(e.kind, ErrorKind::Connection | ErrorKind::Other),
        "{e:?}"
    );
    for mode in ["on", "off", "required"] {
        let _ = driver
            .connect(&config(&[("encryption", mode)]), &good)
            .await;
    }
    assert!(driver.dialect().is_some());
    assert_eq!(driver.manifest().id, "sqlserver");
}
