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
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") && ld_knows_flag() {
        println!("cargo:rustc-link-arg=-Wl,-no_warn_eh_frame_too_large");
    }
}
