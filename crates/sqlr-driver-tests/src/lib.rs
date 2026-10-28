//! The reusable conformance suite (spec §11): a library of checks parameterized by a
//! connection factory. A driver passes when [`SuiteReport::failed`] is empty; the bundled
//! drivers run it in their own tests, a sidecar runs it through the protocol and must
//! report the same passes as the in-process driver.
//!
//! ```ignore
//! let target = Target::new("mydb", Box::new(|| Box::pin(async { connect().await })))
//!     .with_setup(["CREATE TABLE sqlr_conformance (n INTEGER, t TEXT)", "..."]);
//! let report = relational_suite(&target).await;
//! assert!(report.failed.is_empty(), "{report}");
//! ```

mod document;
mod relational;
pub use document::{DocumentTarget, document_suite};

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sqlr_core::{Capabilities, Connection, DriverError, TableRef, Value};

pub use relational::relational_suite;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Opens a fresh connection to the database under test.
pub type Connect =
    Box<dyn Fn() -> BoxFuture<Result<Arc<dyn Connection>, DriverError>> + Send + Sync>;

/// A value the type round-trip matrix expects back from a `SELECT`.
#[derive(Clone, Debug)]
pub struct TypeCase {
    pub name: String,
    /// A one-row, one-column query.
    pub sql: String,
    pub expect: Value,
}

pub struct Target {
    pub name: String,
    pub connect: Connect,
    /// Statements that create `sqlr_conformance(n integer, t text)` (table and column names
    /// lowercase, so a dialect quotes them) holding the rows `n = 1..=25` with
    /// `t = 'row <n>'`, dropping any previous copy.
    pub setup_sql: Vec<String>,
    pub capabilities: Capabilities,
    /// The conformance table as the driver's `describe` names it.
    pub table: TableRef,
    /// A statement that runs for a long time unless cancelled; `None` skips the cancel check.
    pub slow_sql: Option<String>,
    pub type_cases: Vec<TypeCase>,
}

impl Target {
    pub fn new(name: impl Into<String>, connect: Connect) -> Self {
        Self {
            name: name.into(),
            connect,
            setup_sql: Vec::new(),
            capabilities: Capabilities::default(),
            table: TableRef {
                database: None,
                schema: None,
                name: "sqlr_conformance".into(),
            },
            slow_sql: None,
            type_cases: Vec::new(),
        }
    }

    pub fn with_setup<S: Into<String>>(mut self, sql: impl IntoIterator<Item = S>) -> Self {
        self.setup_sql = sql.into_iter().map(Into::into).collect();
        self
    }
}

/// What a suite run found, by check name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SuiteReport {
    pub passed: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub skipped: Vec<(String, String)>,
}

impl fmt::Display for SuiteReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} passed, {} failed, {} skipped",
            self.passed.len(),
            self.failed.len(),
            self.skipped.len()
        )?;
        for (name, why) in &self.failed {
            writeln!(f, "  FAILED {name}: {why}")?;
        }
        for (name, why) in &self.skipped {
            writeln!(f, "  skipped {name}: {why}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_connected() -> Connect {
        Box::new(|| Box::pin(async { Err(DriverError::new(sqlr_core::ErrorKind::Other, "no db")) }))
    }

    #[test]
    fn a_target_starts_with_the_conformance_defaults() {
        let target = Target::new("fake", never_connected());
        assert_eq!(target.name, "fake");
        assert_eq!(target.table.name, "sqlr_conformance");
        assert!(target.setup_sql.is_empty());
        assert_eq!(target.capabilities, Capabilities::default());
        assert!(target.slow_sql.is_none());
        assert!(target.type_cases.is_empty());
    }

    #[test]
    fn setup_statements_are_collected_in_order() {
        let target = Target::new("fake", never_connected())
            .with_setup(["CREATE TABLE t (n INTEGER)", "DELETE FROM t"]);
        assert_eq!(
            target.setup_sql,
            ["CREATE TABLE t (n INTEGER)", "DELETE FROM t"]
        );
        let target = Target::new("fake", never_connected()).with_setup(Vec::<String>::new());
        assert!(target.setup_sql.is_empty());
    }

    #[test]
    fn a_report_names_every_outcome() {
        let report = SuiteReport {
            passed: vec!["connect".into(), "query".into()],
            failed: vec![("insert".into(), "syntax error".into())],
            skipped: vec![("cancel".into(), "no slow query".into())],
        };
        let text = report.to_string();
        assert!(
            text.starts_with("2 passed, 1 failed, 1 skipped\n"),
            "{text}"
        );
        assert!(text.contains("  FAILED insert: syntax error"), "{text}");
        assert!(text.contains("  skipped cancel: no slow query"), "{text}");

        let empty = SuiteReport::default().to_string();
        assert_eq!(empty, "0 passed, 0 failed, 0 skipped\n");
    }
}
