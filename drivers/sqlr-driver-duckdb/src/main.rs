//! The sidecar executable: speaks the sqlr sidecar protocol on stdin and stdout. The log
//! goes to stderr.

#![cfg_attr(not(test), warn(clippy::unwrap_used))]

fn main() -> ! {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    sqlr_driver_sdk::run(sqlr_driver_duckdb::DuckdbDriver::new())
}
