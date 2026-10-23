//! The binary's command line exits before opening a window.

use std::process::{Command, Stdio};

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sqlr"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("sqlr starts")
}

#[test]
fn version_prints_the_crate_version() {
    let out = run(&["--version"]);
    assert!(out.status.success(), "{:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), format!("sqlr {}", env!("CARGO_PKG_VERSION")));
    assert!(out.stderr.is_empty());
}

#[test]
fn help_prints_the_usage() {
    let out = run(&["--help"]);
    assert!(out.status.success(), "{:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage: sqlr [OPTIONS]"), "{stdout}");
    assert!(stdout.contains("--data-dir"), "{stdout}");
    assert!(stdout.contains("--config-dir"), "{stdout}");
    assert!(out.stderr.is_empty());
}

#[test]
fn an_unknown_option_explains_itself_and_exits_two() {
    let out = run(&["--frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown option: --frobnicate"), "{stderr}");
    assert!(stderr.contains("Usage: sqlr [OPTIONS]"), "{stderr}");
}
