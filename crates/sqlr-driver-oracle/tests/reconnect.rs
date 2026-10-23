//! On Oracle Free a failed statement makes the server close the connection (oracle-rs reads
//! it as a closed connection). The driver reopens it, so the session carries on.

mod support;

use sqlr_core::{ErrorKind, Execution, Value};

async fn one(conn: &std::sync::Arc<dyn sqlr_core::Connection>) -> String {
    let Execution::Rows(mut rows) = conn
        .relational()
        .unwrap()
        .execute("SELECT 1 FROM dual")
        .await
        .unwrap()
    else {
        panic!("no rows");
    };
    let chunk = rows.next_chunk(10).await.unwrap().unwrap();
    Value::from_cell(chunk.cell(0, 0)).display().into_owned()
}

#[tokio::test]
async fn the_session_survives_a_failed_statement() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    assert_eq!(one(&conn).await, "1");
    let err = conn
        .relational()
        .unwrap()
        .execute("SELECT * FROM sqlr_no_such_table")
        .await
        .err()
        .expect("the statement fails");
    assert_eq!(err.kind, ErrorKind::Connection, "{err:?}");
    assert!(err.message.contains("reopened"), "{err:?}");
    // The next statements run, repeatedly.
    assert_eq!(one(&conn).await, "1");
    assert!(
        conn.relational()
            .unwrap()
            .execute("DROP TABLE sqlr_no_such_table")
            .await
            .is_err()
    );
    assert_eq!(one(&conn).await, "1");
    assert!(conn.ping().await.is_ok());
}
