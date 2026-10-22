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
//! The process talks to sqlr over stdin and stdout; never print to stdout. Log to stderr —
//! sqlr shows it in its log, prefixed with the driver id.

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
    match runtime.block_on(serve(driver)) {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("driver stopped: {e}");
            std::process::exit(1);
        }
    }
}
