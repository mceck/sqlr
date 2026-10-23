//! Literal defaults survive introspection and table recreation on both servers.

mod support;

use sqlr_core::{Cell, Driver, Execution, ObjectKind, ObjectRef, SchemaChange};
use sqlr_driver_mysql::MysqlDriver;
use support::Server;

async fn string_defaults_with_bit_prefix_round_trip(server: Server) {
    let (_server, cfg, secrets) = support::start(server).await;
    let conn = MysqlDriver::new().connect(&cfg, &secrets).await.unwrap();
    let rel = conn.relational().unwrap();
    rel.execute(
        "CREATE TABLE defaults_round_trip (\
         note VARCHAR(32) DEFAULT 'b''hello', \
         tag VARCHAR(32) DEFAULT 'b''101''', \
         flags BIT(3) DEFAULT b'101')",
    )
    .await
    .unwrap();
    let object = rel
        .introspect()
        .unwrap()
        .load_object(&ObjectRef {
            kind: ObjectKind::Table,
            database: None,
            schema: Some(cfg.values["database"].clone()),
            name: "defaults_round_trip".into(),
            parent: None,
            signature: None,
        })
        .await
        .unwrap();
    let ddl = rel
        .dialect()
        .ddl_gen()
        .unwrap()
        .ddl(&[SchemaChange::Create(object)])
        .unwrap();
    rel.execute("DROP TABLE defaults_round_trip").await.unwrap();
    for sql in ddl {
        rel.execute(&sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
    }
    rel.execute("INSERT INTO defaults_round_trip () VALUES ()")
        .await
        .unwrap();
    let Execution::Rows(mut rows) = rel
        .execute("SELECT note, tag, CAST(flags AS UNSIGNED) FROM defaults_round_trip")
        .await
        .unwrap()
    else {
        panic!("rows expected");
    };
    let chunk = rows.next_chunk(10).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Text("b'hello"));
    assert_eq!(chunk.cell(0, 1), Cell::Text("b'101'"));
    assert_eq!(chunk.cell(0, 2), Cell::Int(5));
    rows.close().await.unwrap();
}

#[tokio::test]
async fn mysql_string_defaults_with_bit_prefix_round_trip() {
    sqlr_testkit::skip_container_tests!();
    string_defaults_with_bit_prefix_round_trip(Server::MySql).await;
}

#[tokio::test]
async fn mariadb_string_defaults_with_bit_prefix_round_trip() {
    sqlr_testkit::skip_container_tests!();
    string_defaults_with_bit_prefix_round_trip(Server::MariaDb).await;
}
