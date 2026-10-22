//! The sidecar executable: speaks the sqlr sidecar protocol on stdin and stdout. The log
//! goes to stderr.

fn main() -> ! {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    sqlr_driver_sdk::run(sqlr_driver_hana::HanaDriver::new())
}
