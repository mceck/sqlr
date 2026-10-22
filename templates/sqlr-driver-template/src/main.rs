//! The sidecar executable. Never print to stdout: it carries the protocol. Log to stderr.

fn main() -> ! {
    sqlr_driver_sdk::run({{crate_name}}::MyDriver::new())
}
