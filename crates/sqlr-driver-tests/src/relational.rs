//! The relational checks.

use std::sync::Arc;
use std::time::Duration;

use sqlr_core::{
    Cell, Connection, DriverError, ErrorKind, Execution, NodePath, ResultChunk, TableColumn, Value,
};

use crate::{SuiteReport, Target};

/// Why a conformance check failed; its message is what the suite report shows.
#[derive(Debug, thiserror::Error)]
pub enum CheckFailure {
    /// The driver returned an error where none was expected.
    #[error("{:?}: {}", .0.kind, .0.message)]
    Driver(#[from] DriverError),
    /// The connection has no relational interface.
    #[error("no relational interface")]
    NoRelational,
    /// `ping` returned an empty product name.
    #[error("empty product name")]
    EmptyProduct,
    /// An `UPDATE` affected another number of rows than expected.
    #[error("{0} rows affected, expected 3")]
    WrongAffected(u64),
    /// An `UPDATE` returned rows.
    #[error("an UPDATE returned rows")]
    UpdateReturnedRows,
    /// A syntax error came back as another kind of error.
    #[error("expected a Syntax error, got {:?}: {}", .0.kind, .0.message)]
    NotASyntaxError(DriverError),
    /// Invalid SQL ran without an error.
    #[error("invalid SQL ran")]
    InvalidSqlRan,
    /// `quote_ident` left a quote unescaped.
    #[error("quote_ident left {quoted} unescaped")]
    QuoteUnescaped {
        /// The quoted identifier.
        quoted: String,
    },
    /// `describe` did not list the conformance table's columns.
    #[error("describe lists {columns:?}")]
    DescribeMismatch {
        /// The columns it listed.
        columns: Vec<TableColumn>,
    },
    /// A statement that must return rows did not.
    #[error("`{sql}` returned no rows")]
    NoRows {
        /// The statement.
        sql: String,
    },
    /// The pages of a result have other sizes than expected.
    #[error("pages of {sizes:?} rows, expected {expected:?}")]
    PageSizes {
        /// The sizes found.
        sizes: Vec<usize>,
        /// The sizes expected.
        expected: Vec<usize>,
    },
    /// The result has no chunks.
    #[error("no chunks")]
    NoChunks,
    /// The first row is not the expected one.
    #[error("unexpected first row {first}")]
    UnexpectedFirstRow {
        /// The first cell found, as its debug text (the cell borrows its chunk).
        first: String,
    },
    /// The statement kept running after cancel.
    #[error("the statement kept running 15 s after cancel")]
    CancelTimeout,
    /// The task running the statement panicked.
    #[error("the statement task panicked: {0}")]
    TaskPanicked(tokio::task::JoinError),
    /// A type case returned no rows.
    #[error("{case}: no rows")]
    TypeCaseNoRows {
        /// The case name.
        case: String,
    },
    /// A type case returned another value than expected.
    #[error("{case}: got {got:?}, expected {expected:?}")]
    TypeCaseMismatch {
        /// The case name.
        case: String,
        /// The value found.
        got: Value,
        /// The value expected.
        expected: Value,
    },
}

type Check = Result<(), CheckFailure>;

async fn drain(
    conn: &dyn Connection,
    sql: &str,
    page: usize,
) -> Result<Vec<ResultChunk>, CheckFailure> {
    let relational = conn.relational().ok_or(CheckFailure::NoRelational)?;
    match relational.execute(sql).await? {
        Execution::Affected(_) => Err(CheckFailure::NoRows {
            sql: sql.to_string(),
        }),
        Execution::Rows(mut stream) => {
            let mut chunks = Vec::new();
            while let Some(chunk) = stream.next_chunk(page).await? {
                chunks.push(chunk);
            }
            stream.close().await?;
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
                Err(why) => report.failed.push(($name.to_string(), why.to_string())),
            }
        }};
    }

    let conn = match (target.connect)().await {
        Ok(conn) => {
            report.passed.push("connect".into());
            conn
        }
        Err(e) => {
            report
                .failed
                .push(("connect".into(), CheckFailure::from(e).to_string()));
            return report;
        }
    };
    let setup: Check = async {
        let relational = conn.relational().ok_or(CheckFailure::NoRelational)?;
        for sql in &target.setup_sql {
            relational.execute(sql).await?;
        }
        Ok::<(), CheckFailure>(())
    }
    .await;
    if let Err(why) = setup {
        report.failed.push(("setup".into(), why.to_string()));
        return report;
    }

    check!("ping", async {
        let info = conn.ping().await?;
        (!info.product.is_empty())
            .then_some(())
            .ok_or(CheckFailure::EmptyProduct)
    });
    check!("browse", async {
        conn.browse(&NodePath::root()).await?;
        Ok::<(), CheckFailure>(())
    });
    check!("execute_and_stream", pages(&*conn, 10, &[10, 10, 5]));
    check!("fetch_more", pages(&*conn, 7, &[7, 7, 7, 4]));
    check!("affected_rows", async {
        let relational = conn.relational().ok_or(CheckFailure::NoRelational)?;
        let d = relational.dialect();
        let (table, n, t) = (
            d.qualified(&target.table),
            d.quote_ident("n"),
            d.quote_ident("t"),
        );
        let sql = target
            .affected_sql
            .clone()
            .unwrap_or_else(|| format!("UPDATE {table} SET {t} = {t} WHERE {n} <= 3"));
        match relational.execute(&sql).await? {
            Execution::Affected(3) => Ok(()),
            Execution::Affected(n) => Err(CheckFailure::WrongAffected(n)),
            Execution::Rows(_) => Err(CheckFailure::UpdateReturnedRows),
        }
    });
    check!("syntax_error_is_an_error", async {
        let relational = conn.relational().ok_or(CheckFailure::NoRelational)?;
        match relational.execute("SELEKT 1 FROM").await {
            Err(e) if e.kind == ErrorKind::Syntax => Ok(()),
            Err(e) => Err(CheckFailure::NotASyntaxError(e)),
            Ok(_) => Err(CheckFailure::InvalidSqlRan),
        }
    });
    check!("quote_ident_round_trips", async {
        let relational = conn.relational().ok_or(CheckFailure::NoRelational)?;
        let quoted = relational.dialect().quote_ident("we\"ird name");
        (quoted.len() > "we\"ird name".len())
            .then_some(())
            .ok_or_else(|| CheckFailure::QuoteUnescaped { quoted })
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
            let relational = conn.relational().ok_or(CheckFailure::NoRelational)?;
            let info = relational.describe(&target.table).await?;
            (info.column("n").is_some() && info.column("t").is_some())
                .then_some(())
                .ok_or_else(|| CheckFailure::DescribeMismatch {
                    columns: info.columns.clone(),
                })
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
    let relational = conn.relational().ok_or(CheckFailure::NoRelational)?;
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
        return Err(CheckFailure::PageSizes {
            sizes,
            expected: expect.to_vec(),
        });
    }
    let first = chunks.first().ok_or(CheckFailure::NoChunks)?;
    if first.columns().len() != 2 || first.cell(0, 0) != Cell::Int(1) {
        return Err(CheckFailure::UnexpectedFirstRow {
            first: format!("{:?}", first.cell(0, 0)),
        });
    }
    Ok(())
}

async fn cancel(target: &Target, slow_sql: &str) -> Check {
    let conn: Arc<dyn Connection> = (target.connect)().await?;
    let handle = conn.cancel_handle();
    let runner = conn.clone();
    let sql = slow_sql.to_string();
    let running = tokio::spawn(async move {
        let relational = runner.relational().ok_or(CheckFailure::NoRelational)?;
        match relational.execute(&sql).await? {
            Execution::Rows(mut stream) => {
                while stream.next_chunk(100).await?.is_some() {}
                Ok::<(), CheckFailure>(())
            }
            Execution::Affected(_) => Ok(()),
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    handle.cancel().await?;
    match tokio::time::timeout(Duration::from_secs(15), running).await {
        Err(_) => Err(CheckFailure::CancelTimeout),
        Ok(Err(join)) => Err(CheckFailure::TaskPanicked(join)),
        // An error, or a normal end (MySQL's `SLEEP` returns 1 when interrupted): either way
        // the statement stopped.
        Ok(Ok(_)) => Ok(()),
    }
}

async fn type_matrix(conn: &dyn Connection, target: &Target) -> Check {
    for case in &target.type_cases {
        let chunks = drain(conn, &case.sql, 10).await?;
        let chunk = chunks.first().ok_or_else(|| CheckFailure::TypeCaseNoRows {
            case: case.name.clone(),
        })?;
        let got = Value::from_cell(chunk.cell(0, 0));
        let same = match (&got, &case.expect) {
            (Value::Float(a), Value::Float(b)) => (a - b).abs() < 1e-9,
            (a, b) => a == b,
        };
        if !same {
            return Err(CheckFailure::TypeCaseMismatch {
                case: case.name.clone(),
                got,
                expected: case.expect.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_read_as_the_suite_always_reported_them() {
        let driver = DriverError::new(ErrorKind::Syntax, "bad");
        assert_eq!(
            CheckFailure::from(driver.clone()).to_string(),
            "Syntax: bad"
        );
        assert_eq!(
            CheckFailure::NotASyntaxError(driver).to_string(),
            "expected a Syntax error, got Syntax: bad"
        );
        assert_eq!(
            CheckFailure::PageSizes {
                sizes: vec![10, 5],
                expected: vec![10, 10, 5]
            }
            .to_string(),
            "pages of [10, 5] rows, expected [10, 10, 5]"
        );
        assert_eq!(
            CheckFailure::WrongAffected(2).to_string(),
            "2 rows affected, expected 3"
        );
    }
}
