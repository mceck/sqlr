//! The sidecar executable: speaks the sqlr sidecar protocol on stdin and stdout. The log
//! goes to stderr.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

#[cfg(target_os = "linux")]
unsafe extern "C" {
    /// Defined in DuckDB's generated extension loader and called by its static initializer.
    fn duckdb_register_static_extensions() -> i32;
}

fn main() -> ! {
    // GNU ld resolves archive members in command-line order, and libduckdb-sys puts the
    // static initializer's archive after the one defining this symbol. A reference from
    // this crate's object, which comes first, pulls the defining member in (the link
    // failed on aarch64 Linux without it).
    #[cfg(target_os = "linux")]
    std::hint::black_box(duckdb_register_static_extensions as unsafe extern "C" fn() -> i32);
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    sqlr_driver_sdk::run(sqlr_driver_duckdb::DuckdbDriver::new())
}
