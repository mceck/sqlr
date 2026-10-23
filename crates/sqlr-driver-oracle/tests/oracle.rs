//! Oracle Free integration tests: connection, DDL/DML, binds, paging, transactions,
//! explorer and catalog. The fixed server lives in `docker-compose.test.yml`.

mod support;

use std::sync::Arc;
use std::time::Duration;

use sqlr_core::{
    Connection, Driver, ErrorKind, Execution, NodeKind, NodePath, RowStream, Secrets, TableRef,
    Value,
};
use sqlr_driver_oracle::OracleDriver;
use sqlr_testkit::{service, unique_name};

fn path(segs: &[&str]) -> NodePath {
    NodePath(segs.iter().map(|s| s.to_string()).collect())
}

fn table(name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some("SQLR_TEST".into()),
        name: name.into(),
    }
}

async fn collect(mut stream: Box<dyn RowStream>) -> Vec<Vec<Value>> {
    let mut rows = Vec::new();
    while let Some(chunk) = stream.next_chunk(1000).await.unwrap() {
        for row in 0..chunk.len() {
            rows.push(
                (0..chunk.columns().len())
                    .map(|col| Value::from_cell(chunk.cell(row, col)))
                    .collect(),
            );
        }
    }
    stream.close().await.unwrap();
    rows
}

async fn query(conn: &Arc<dyn Connection>, sql: &str) -> Vec<Vec<Value>> {
    match conn.relational().unwrap().execute(sql).await.unwrap() {
        Execution::Rows(stream) => collect(stream).await,
        Execution::Affected(n) => panic!("{sql}: affected {n}"),
    }
}

async fn run(conn: &Arc<dyn Connection>, sql: &str) {
    conn.relational()
        .unwrap()
        .execute(sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
}

async fn drop_table(conn: &Arc<dyn Connection>, name: &str) {
    let _ = conn
        .relational()
        .unwrap()
        .execute(&format!("DROP TABLE {name} PURGE"))
        .await;
}

#[tokio::test]
async fn connect_and_ping() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let info = conn.ping().await.unwrap();
    assert_eq!(info.product, "Oracle");
    assert!(!info.version.is_empty(), "{info:?}");
    assert_eq!(conn.database_name(), Some("FREEPDB1"));
}

#[tokio::test]
async fn ddl_dml_binds_and_paging() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let name = unique_name("SQLR_VALUES").to_uppercase();
    run(
        &conn,
        &format!(
            "CREATE TABLE {name} (id NUMBER(10) PRIMARY KEY, name VARCHAR2(100), \
             amount NUMBER(10,2), note VARCHAR2(200))"
        ),
    )
    .await;

    let insert = format!("INSERT INTO {name} (id, name, amount, note) VALUES (:1, :2, :3, :4)");
    let done = conn
        .relational()
        .unwrap()
        .execute_with(
            &insert,
            &[
                Value::Int(1),
                Value::Text("città".into()),
                Value::Float(12.5),
                Value::Null,
            ],
        )
        .await
        .unwrap();
    assert!(matches!(done, Execution::Affected(1)));
    let done = conn
        .relational()
        .unwrap()
        .execute_with(
            &insert,
            &[
                Value::Int(2),
                Value::Text("ann".into()),
                Value::Float(3.25),
                Value::Text("x".into()),
            ],
        )
        .await
        .unwrap();
    assert!(matches!(done, Execution::Affected(1)));

    let rows = query(
        &conn,
        &format!("SELECT id, name, amount, note FROM {name} ORDER BY id"),
    )
    .await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0][0].display().to_string(), "1");
    assert_eq!(rows[0][1], Value::Text("città".into()));
    assert_eq!(rows[0][2].display().to_string(), "12.5");
    assert_eq!(rows[0][3], Value::Null);
    assert_eq!(rows[1][0].display().to_string(), "2");

    let update = format!("UPDATE {name} SET note = :1 WHERE id = :2");
    let done = conn
        .relational()
        .unwrap()
        .execute_with(&update, &[Value::Text("done".into()), Value::Int(2)])
        .await
        .unwrap();
    assert!(matches!(done, Execution::Affected(1)));

    // A result set pages through its prefetch buffer and server cursor.
    run(&conn, &format!("DELETE FROM {name}")).await;
    run(
        &conn,
        &format!(
            "INSERT INTO {name} (id, name) SELECT LEVEL, 'row ' || LEVEL FROM dual \
             CONNECT BY LEVEL <= 25"
        ),
    )
    .await;
    let Execution::Rows(mut stream) = conn
        .relational()
        .unwrap()
        .execute(&format!("SELECT id FROM {name} ORDER BY id"))
        .await
        .unwrap()
    else {
        panic!("expected rows");
    };
    let mut sizes = Vec::new();
    while let Some(chunk) = stream.next_chunk(10).await.unwrap() {
        sizes.push(chunk.len());
    }
    stream.close().await.unwrap();
    assert_eq!(sizes, [10, 10, 5]);
    drop_table(&conn, &name).await;
}

#[tokio::test]
async fn transactions_roll_back_and_commit() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let name = unique_name("SQLR_TX").to_uppercase();
    run(&conn, &format!("CREATE TABLE {name} (id NUMBER(10))")).await;
    // Outside an explicit transaction every statement commits on its own (Auto mode);
    // the engine opens one with the dialect's `begin_sql` (spec §7.9).
    let begin = conn.relational().unwrap().dialect().begin_sql().to_string();
    run(&conn, &begin).await;
    run(&conn, &format!("INSERT INTO {name} VALUES (1)")).await;
    run(&conn, "ROLLBACK").await;
    assert_eq!(
        query(&conn, &format!("SELECT COUNT(*) FROM {name}")).await[0][0]
            .display()
            .to_string(),
        "0"
    );
    run(&conn, &begin).await;
    run(&conn, &format!("INSERT INTO {name} VALUES (2)")).await;
    run(&conn, "COMMIT").await;
    assert_eq!(
        query(&conn, &format!("SELECT COUNT(*) FROM {name}")).await[0][0]
            .display()
            .to_string(),
        "1"
    );
    drop_table(&conn, &name).await;
}

async fn transaction_ending_statement_restores_autocommit(ending: &str, initial_rows: &str) {
    let conn = support::connect().await;
    let other = support::connect().await;
    let name = unique_name("SQLR_TX_END").to_uppercase();
    run(&conn, &format!("CREATE TABLE {name} (id NUMBER(10))")).await;
    run(&conn, "SET TRANSACTION READ WRITE").await;
    run(&conn, &format!("INSERT INTO {name} VALUES (1)")).await;
    run(&conn, ending).await;
    assert_eq!(
        query(&other, &format!("SELECT COUNT(*) FROM {name}")).await[0][0]
            .display()
            .as_ref(),
        initial_rows,
        "the ending statement must take effect on the server"
    );
    run(&conn, &format!("INSERT INTO {name} VALUES (2)")).await;
    let rows = query(&other, &format!("SELECT id FROM {name} WHERE id = 2")).await;
    drop_table(&conn, &name).await;
    assert_eq!(rows.len(), 1, "{ending} must restore automatic commits");
}

#[tokio::test]
async fn commit_work_restores_autocommit() {
    sqlr_testkit::skip_container_tests!();
    transaction_ending_statement_restores_autocommit("COMMIT WORK", "1").await;
}

#[tokio::test]
async fn rollback_work_restores_autocommit() {
    sqlr_testkit::skip_container_tests!();
    transaction_ending_statement_restores_autocommit("ROLLBACK WORK", "0").await;
}

#[tokio::test]
async fn commit_comment_restores_autocommit() {
    sqlr_testkit::skip_container_tests!();
    transaction_ending_statement_restores_autocommit("COMMIT COMMENT 'sqlr regression'", "1").await;
}

#[tokio::test]
async fn failed_ddl_ends_the_transaction_before_following_automatic_writes() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let other = support::connect().await;
    let name = unique_name("SQLR_FAILED_DDL").to_uppercase();
    let create = format!("CREATE TABLE {name} (id NUMBER(10))");
    run(&conn, &create).await;
    run(&conn, "SET TRANSACTION READ WRITE").await;
    run(&conn, &format!("INSERT INTO {name} VALUES (1)")).await;
    assert!(conn.relational().unwrap().execute(&create).await.is_err());
    assert_eq!(
        query(&other, &format!("SELECT COUNT(*) FROM {name}")).await[0][0]
            .display()
            .as_ref(),
        "1",
        "Oracle commits before executing the failing DDL"
    );
    let error = conn
        .relational()
        .unwrap()
        .execute(&format!("INSERT INTO {name} VALUES (2)"))
        .await
        .err();
    let rows = query(&other, &format!("SELECT COUNT(*) FROM {name}")).await;
    drop_table(&conn, &name).await;
    assert!(
        error.is_none(),
        "the DDL already ended the transaction: {error:?}"
    );
    assert_eq!(rows[0][0].display().as_ref(), "2");
}

#[tokio::test]
async fn explorer_catalog_and_describe() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let name = unique_name("SQLR_EXPLORER").to_uppercase();
    run(
        &conn,
        &format!("CREATE TABLE {name} (id NUMBER(10) PRIMARY KEY, name VARCHAR2(50) NOT NULL)"),
    )
    .await;
    run(
        &conn,
        &format!("CREATE VIEW {name}_V AS SELECT id, name FROM {name}"),
    )
    .await;

    let root = conn.browse(&NodePath::root()).await.unwrap();
    let schema = root
        .iter()
        .find(|n| n.name == "SQLR_TEST")
        .expect("the current schema is listed");
    assert_eq!(schema.kind, NodeKind::Schema);
    let folders = conn.browse(&path(&["SQLR_TEST"])).await.unwrap();
    assert!(folders.iter().any(|n| n.name == "tables"));
    let tables = conn.browse(&path(&["SQLR_TEST", "tables"])).await.unwrap();
    assert!(tables.iter().any(|n| n.name == name), "{tables:?}");
    let columns = conn
        .browse(&path(&["SQLR_TEST", "tables", &name, "columns"]))
        .await
        .unwrap();
    assert_eq!(
        columns.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
        ["ID", "NAME"]
    );

    let catalog = conn
        .relational()
        .unwrap()
        .catalog("SQLR_TEST")
        .await
        .unwrap();
    let relation = catalog
        .relations
        .iter()
        .find(|r| r.name == name)
        .expect("the table is in the catalog");
    assert_eq!(relation.columns.len(), 2);
    assert_eq!(relation.columns[0].name, "ID");

    let info = conn
        .relational()
        .unwrap()
        .describe(&table(&name))
        .await
        .unwrap();
    assert_eq!(info.primary_key, ["ID"]);
    assert_eq!(info.columns[1].name, "NAME");
    assert!(!info.columns[1].nullable);

    run(&conn, &format!("DROP VIEW {name}_V")).await;
    drop_table(&conn, &name).await;
}

#[tokio::test]
async fn explain_returns_the_estimated_plan() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let name = unique_name("SQLR_PLAN").to_uppercase();
    run(
        &conn,
        &format!("CREATE TABLE {name} (id NUMBER(10) PRIMARY KEY, name VARCHAR2(20))"),
    )
    .await;
    run(&conn, &format!("INSERT INTO {name} VALUES (1, 'a')")).await;

    let plan = conn
        .relational()
        .unwrap()
        .explain(&format!("SELECT * FROM {name} WHERE id = 1"), false)
        .await
        .unwrap();
    assert!(plan.root.cost.is_some(), "{plan:#?}");
    assert!(!plan.root.children.is_empty(), "{plan:#?}");
    drop_table(&conn, &name).await;
}

#[tokio::test]
async fn cancel_is_unsupported_and_says_so() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let e = conn.cancel_handle().cancel().await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unsupported);
    assert!(e.message.contains("cancel"), "{e:?}");
}

#[tokio::test]
async fn wrong_password_is_an_auth_error() {
    sqlr_testkit::skip_container_tests!();
    let (host, port) = service("ORACLE");
    let cfg = sqlr_core::ConnectionConfig {
        id: "o".into(),
        name: "oracle".into(),
        driver: "oracle".into(),
        group: None,
        color: None,
        values: [
            ("host", host),
            ("port", port.to_string()),
            ("service", "FREEPDB1".into()),
            ("service_type", "service".into()),
            ("user", "sqlr_test".into()),
            ("tls", "false".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    let secrets = Secrets([("password".to_string(), "wrong".to_string())].into());
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        OracleDriver::new().connect(&cfg, &secrets),
    )
    .await
    .expect("must not hang");
    let err = match result {
        Ok(_) => panic!("a wrong password fails"),
        Err(e) => e,
    };
    assert_eq!(err.kind, ErrorKind::Auth, "{err:?}");
}

/// Auto mode (spec §7.9): every statement commits on its own. The engine runs no COMMIT
/// after a statement, so the driver must leave no transaction open. A second connection
/// only sees a row that was really committed.
#[tokio::test]
async fn a_statement_commits_on_its_own() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let name = unique_name("SQLR_AUTOCOMMIT").to_uppercase();
    run(&conn, &format!("CREATE TABLE {name} (id NUMBER(10))")).await;
    run(&conn, &format!("INSERT INTO {name} VALUES (1)")).await;
    let other = support::connect().await;
    assert_eq!(
        query(&other, &format!("SELECT COUNT(*) FROM {name}")).await[0][0]
            .display()
            .to_string(),
        "1",
        "the insert is still uncommitted in auto-commit mode"
    );
    drop_table(&conn, &name).await;
}
