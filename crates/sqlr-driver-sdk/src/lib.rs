//! Write an sqlr driver in Rust (spec §6.3): implement [`Driver`] and call [`run`].
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use sqlr_driver_sdk::*;
//! # struct MyDriver;
//! # #[async_trait::async_trait]
//! # impl Driver for MyDriver {
//! #     fn manifest(&self) -> &DriverManifest { unimplemented!() }
//! #     async fn connect(&self, _: &ConnectionConfig, _: &Secrets)
//! #         -> Result<Arc<dyn Connection>, DriverError> { unimplemented!() }
//! # }
//! fn main() -> ! {
//!     run(MyDriver)
//! }
//! ```
//!
//! Document drivers implement [`DocumentConn`] and return [`DocExecution`]: a [`DocStream`],
//! a [`RowStream`], a write outcome or a [`DocValue`]. Targets use [`DocTarget`]; pages use
//! [`DocChunk`]. Protocol 1.1 carries these operations through the same serve loop.
//!
//! The process talks to sqlr over stdin and stdout; never print to stdout. Log to stderr —
//! sqlr shows it in its log, prefixed with the driver id.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![deny(missing_docs)]

pub use sqlr_core::*;
pub use sqlr_sidecar::{LimitStyle, PROTOCOL, serve, serve_arc, serve_io};

/// How a dialect travels to sqlr: the data a sidecar sends in its `Connected` answer.
pub fn sidecar_dialect_spec(dialect: &dyn Dialect) -> sqlr_sidecar::DialectSpec {
    sqlr_sidecar::DialectSpec::from_dialect(dialect)
}

/// Serves `driver` until sqlr closes the pipe, then exits the process: code 0 on a clean
/// shutdown, 1 when the pipe broke.
pub fn run<D: Driver + 'static>(driver: D) -> ! {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime builds");
    std::process::exit(exit_code(runtime.block_on(serve(driver))));
}

fn exit_code(result: std::io::Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("driver stopped: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_driver_sqlite::SqliteDialect;

    #[test]
    fn a_dialect_travels_as_data() {
        let spec = sidecar_dialect_spec(&SqliteDialect);
        assert_eq!(spec.default_schemas, ["main"]);
        assert!(!spec.database_is_schema);
        assert!(matches!(
            spec.placeholder,
            sqlr_sidecar::PlaceholderStyle::Question
        ));
    }

    #[test]
    fn the_exit_code_tells_a_clean_shutdown_from_a_broken_pipe() {
        assert_eq!(exit_code(Ok(())), 0);
        assert_eq!(
            exit_code(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "pipe closed"
            ))),
            1
        );
    }

    /// `run` exits the process, so this test starts a child of itself: with no input, the
    /// protocol loop sees EOF, serves a clean shutdown and exits 0.
    #[test]
    fn run_serves_an_empty_pipe_and_exits_zero() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::run_child", "--nocapture"])
            .env("SQLR_SDK_RUN_CHILD", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the child test starts");
        assert_eq!(
            out.status.code(),
            Some(0),
            "child failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn run_child() {
        if std::env::var_os("SQLR_SDK_RUN_CHILD").is_none() {
            return;
        }
        struct Shared(std::sync::Arc<sqlr_driver_sqlite::SqliteDriver>);
        #[async_trait::async_trait]
        impl Driver for Shared {
            fn manifest(&self) -> &DriverManifest {
                self.0.manifest()
            }

            async fn connect(
                &self,
                cfg: &ConnectionConfig,
                secrets: &Secrets,
            ) -> Result<std::sync::Arc<dyn Connection>, DriverError> {
                self.0.connect(cfg, secrets).await
            }
        }
        run(Shared(sqlr_driver_sqlite::SqliteDriver::new()));
    }
}
