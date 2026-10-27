//! The relational checks.

use std::sync::Arc;
use std::time::Duration;

use sqlr_core::{
    Cell, Connection, DriverError, ErrorKind, Execution, NodePath, ResultChunk, Value,
};

use crate::{SuiteReport, Target};

type Check = Result<(), String>;

#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` function pointer, which receives the error by value"
)]
fn err(e: DriverError) -> String {
    format!("{:?}: {}", e.kind, e.message)
}

async fn drain(conn: &dyn Connection, sql: &str, page: usize) -> Result<Vec<ResultChunk>, String> {
    let relational = conn.relational().ok_or("no relational interface")?;
    match relational.execute(sql).await.map_err(err)? {
        Execution::Affected(_) => Err(format!("`{sql}` returned no rows")),
        Execution::Rows(mut stream) => {
            let mut chunks = Vec::new();
            while let Some(chunk) = stream.next_chunk(page).await.map_err(err)? {
                chunks.push(chunk);
            }
            stream.close().await.map_err(err)?;
            Ok(chunks)
        }
    }
}

/// Runs the relational checks against `target`.
pub async fn relational_suite(target: &Target) -> SuiteReport {
    let mut report = SuiteReport::default();
    macro_rules! check {
        ($name:expr, $body:expr) => {{
            match $body.await {
                Ok(()) => report.passed.push($name.to_string()),
                Err(why) => report.failed.push(($name.to_string(), why)),
            }
        }};
    }

    let conn = match (target.connect)().await {
        Ok(conn) => {
            report.passed.push("connect".into());
            conn
        }
        Err(e) => {
            report.failed.push(("connect".into(), err(e)));
            return report;
        }
    };
    let setup: Check = async {
        let relational = conn.relational().ok_or("no relational interface")?;
        for sql in &target.setup_sql {
            relational.execute(sql).await.map_err(err)?;
        }
        Ok(())
    }
    .await;
    if let Err(why) = setup {
        report.failed.push(("setup".into(), why));
        return report;
    }

    check!("ping", async {
        let info = conn.ping().await.map_err(err)?;
        (!info.product.is_empty())
            .then_some(())
            .ok_or_else(|| "empty product name".to_string())
    });
    check!("browse", async {
        conn.browse(&NodePath::root()).await.map_err(err)?;
        Ok(())
    });
    check!("execute_and_stream", pages(&*conn, 10, &[10, 10, 5]));
    check!("fetch_more", pages(&*conn, 7, &[7, 7, 7, 4]));
    check!("affected_rows", async {
        let relational = conn.relational().ok_or("no relational interface")?;
        let d = relational.dialect();
        let (table, n, t) = (
            d.qualified(&target.table),
            d.quote_ident("n"),
            d.quote_ident("t"),
        );
        let sql = format!("UPDATE {table} SET {t} = {t} WHERE {n} <= 3");
        match relational.execute(&sql).await.map_err(err)? {
            Execution::Affected(3) => Ok(()),
            Execution::Affected(n) => Err(format!("{n} rows affected, expected 3")),
            Execution::Rows(_) => Err("an UPDATE returned rows".into()),
        }
    });
    check!("syntax_error_is_an_error", async {
        let relational = conn.relational().ok_or("no relational interface")?;
        match relational.execute("SELEKT 1 FROM").await {
            Err(e) if e.kind == ErrorKind::Syntax => Ok(()),
            Err(e) => Err(format!("expected a Syntax error, got {}", err(e))),
            Ok(_) => Err("invalid SQL ran".into()),
        }
    });
    check!("quote_ident_round_trips", async {
        let relational = conn.relational().ok_or("no relational interface")?;
        let quoted = relational.dialect().quote_ident("we\"ird name");
        (quoted.len() > "we\"ird name".len())
            .then_some(())
            .ok_or_else(|| format!("quote_ident left {quoted} unescaped"))
    });

    if target.capabilities.server_cancel {
        match &target.slow_sql {
            Some(sql) => check!("cancel", cancel(target, sql)),
            None => report
                .skipped
                .push(("cancel".into(), "the target names no slow statement".into())),
        }
    } else {
        report
            .skipped
            .push(("cancel".into(), "the driver does not declare cancel".into()));
    }

    if target.type_cases.is_empty() {
        report.skipped.push((
            "type_matrix".into(),
            "the target lists no type cases".into(),
        ));
    } else {
        check!("type_matrix", type_matrix(&*conn, target));
    }

    if target.capabilities.editable_results {
        check!("describe_for_editing", async {
            let relational = conn.relational().ok_or("no relational interface")?;
            let info = relational.describe(&target.table).await.map_err(err)?;
            (info.column("n").is_some() && info.column("t").is_some())
                .then_some(())
                .ok_or_else(|| format!("describe lists {:?}", info.columns))
        });
    } else {
        report.skipped.push((
            "describe_for_editing".into(),
            "the driver does not declare editable results".into(),
        ));
    }
    report
}

async fn pages(conn: &dyn Connection, page: usize, expect: &[usize]) -> Check {
    let relational = conn.relational().ok_or("no relational interface")?;
    let d = relational.dialect();
    let sql = format!(
        "SELECT {n}, {t} FROM {table} ORDER BY {n}",
        n = d.quote_ident("n"),
        t = d.quote_ident("t"),
        table = d.quote_ident("sqlr_conformance"),
    );
    let chunks = drain(conn, &sql, page).await?;
    let sizes: Vec<usize> = chunks.iter().map(ResultChunk::len).collect();
    if sizes != expect {
        return Err(format!("pages of {sizes:?} rows, expected {expect:?}"));
    }
    let first = chunks.first().ok_or("no chunks")?;
    if first.columns().len() != 2 || first.cell(0, 0) != Cell::Int(1) {
        return Err(format!("unexpected first row {:?}", first.cell(0, 0)));
    }
    Ok(())
}

async fn cancel(target: &Target, slow_sql: &str) -> Check {
    let conn: Arc<dyn Connection> = (target.connect)().await.map_err(err)?;
    let handle = conn.cancel_handle();
    let runner = conn.clone();
    let sql = slow_sql.to_string();
    let running = tokio::spawn(async move {
        let relational = runner.relational().ok_or("no relational interface")?;
        match relational.execute(&sql).await.map_err(err)? {
            Execution::Rows(mut stream) => {
                while stream.next_chunk(100).await.map_err(err)?.is_some() {}
                Ok::<(), String>(())
            }
            Execution::Affected(_) => Ok(()),
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    handle.cancel().await.map_err(err)?;
    match tokio::time::timeout(Duration::from_secs(15), running).await {
        Err(_) => Err("the statement kept running 15 s after cancel".into()),
        Ok(Err(join)) => Err(format!("the statement task panicked: {join}")),
        // An error, or a normal end (MySQL's `SLEEP` returns 1 when interrupted): either way
        // the statement stopped.
        Ok(Ok(_)) => Ok(()),
    }
}

async fn type_matrix(conn: &dyn Connection, target: &Target) -> Check {
    for case in &target.type_cases {
        let chunks = drain(conn, &case.sql, 10).await?;
        let chunk = chunks
            .first()
            .ok_or_else(|| format!("{}: no rows", case.name))?;
        let got = Value::from_cell(chunk.cell(0, 0));
        let same = match (&got, &case.expect) {
            (Value::Float(a), Value::Float(b)) => (a - b).abs() < 1e-9,
            (a, b) => a == b,
        };
        if !same {
            return Err(format!(
                "{}: got {got:?}, expected {:?}",
                case.name, case.expect
            ));
        }
    }
    Ok(())
}
