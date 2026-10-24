//! Keeps Apple's linker quiet about the debug binary's `__eh_frame` exceeding the
//! 16 MB compact-unwind encoding limit. The unoptimized binary is just over it;
//! the release binary (~2 MB) is not. Only the warning is suppressed: affected
//! functions keep their DWARF unwind info and the compact unwind table stays on.

use std::process::Command;

/// Older linkers do not know `-no_warn_eh_frame_too_large`; probe before passing it.
fn ld_knows_flag() -> bool {
    Command::new("ld")
        .arg("-no_warn_eh_frame_too_large")
        .output()
        .is_ok_and(|out| !String::from_utf8_lossy(&out.stderr).contains("unknown option"))
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The short commit is embedded when the build happens in a git checkout; it is omitted
    // otherwise instead of being guessed.
    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|commit| !commit.is_empty());
    if let Some(commit) = commit {
        println!("cargo:rustc-env=SQLR_GIT_COMMIT={commit}");
    }
    println!(
        "cargo:rustc-env=SQLR_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=SQLR_TARGET={}",
        std::env::var("TARGET").unwrap_or_default()
    );
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    if std::path::Path::new("../../.git/packed-refs").exists() {
        println!("cargo:rerun-if-changed=../../.git/packed-refs");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") && ld_knows_flag() {
        println!("cargo:rustc-link-arg=-Wl,-no_warn_eh_frame_too_large");
    }
}
