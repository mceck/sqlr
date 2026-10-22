//! Builds the Java bridge with `javac` when a JDK is installed; without one the launcher
//! still builds, and the bridge jar comes with the driver's registry download instead.

use std::path::PathBuf;
use std::process::Command;

fn javac() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("JAVA_HOME").map(|h| PathBuf::from(h).join("bin/javac")),
        Some(PathBuf::from("javac")),
    ];
    candidates.into_iter().flatten().find(|c| {
        Command::new(c)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

fn main() {
    println!("cargo:rerun-if-changed=../../drivers/jdbc-bridge/java");
    println!("cargo:rerun-if-changed=../../drivers/jdbc-bridge/build.sh");
    println!("cargo:rerun-if-env-changed=JAVA_HOME");
    let out =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("sqlr-jdbc-bridge.jar");
    let script = PathBuf::from("../../drivers/jdbc-bridge/build.sh");
    let built = javac().is_some()
        && Command::new("sh")
            .arg(&script)
            .arg(&out)
            .output()
            .is_ok_and(|o| {
                if !o.status.success() {
                    println!(
                        "cargo:warning=the JDBC bridge did not build: {}",
                        String::from_utf8_lossy(&o.stderr)
                            .lines()
                            .next()
                            .unwrap_or("")
                    );
                }
                o.status.success()
            });
    if !built {
        println!("cargo:warning=no JDK found: the JDBC bridge jar is not built");
    }
    println!(
        "cargo:rustc-env=SQLR_JDBC_BRIDGE_JAR={}",
        if built {
            out.display().to_string()
        } else {
            String::new()
        }
    );
}
