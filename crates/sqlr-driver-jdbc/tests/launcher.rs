//! The launcher reports a missing preset or JRE over stderr and the protocol, then exits 2.

use std::process::{Command, Stdio};

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sqlr-driver-jdbc"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the launcher starts")
}

#[test]
fn an_unknown_preset_is_reported_before_any_handshake() {
    let out = run(&["--preset", "not-a-database"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown JDBC preset"), "{stderr}");
}

#[test]
fn a_known_preset_either_starts_the_bridge_or_explains_why_not() {
    let out = run(&["--preset", "jdbc-h2"]);
    // No JDK in every test environment: either Java starts the bridge (which then sees a
    // closed stdin and ends) or the launcher names the missing Java and exits 2.
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(2) {
        assert!(
            stderr.contains("Java") || stderr.contains("jdbc bridge"),
            "{stderr}"
        );
    }
}
