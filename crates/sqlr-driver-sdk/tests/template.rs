//! The driver template generates a crate that builds and passes its own test.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn copy_rendered(from: &Path, to: &Path, vars: &[(&str, String)]) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            copy_rendered(&src, &dst, vars);
        } else if entry.file_name() == "cargo-generate.toml" {
            continue;
        } else {
            let mut text = fs::read_to_string(&src).unwrap();
            for (name, value) in vars {
                text = text.replace(&format!("{{{{{name}}}}}"), value);
            }
            fs::write(dst, text).unwrap();
        }
    }
}

#[test]
fn template_builds_and_passes_its_test() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf();
    let out = tempfile::tempdir().unwrap();
    let crate_dir = out.path().join("my-sidecar-driver");
    let path_dep = |name: &str| {
        format!(
            "{{ path = {:?} }}",
            workspace.join("crates").join(name).display().to_string()
        )
    };
    copy_rendered(
        &workspace.join("templates/sqlr-driver-template"),
        &crate_dir,
        &[
            ("project-name", "my-sidecar-driver".into()),
            ("crate_name", "my_sidecar_driver".into()),
            ("sdk_dependency", path_dep("sqlr-driver-sdk")),
            ("tests_dependency", path_dep("sqlr-driver-tests")),
        ],
    );
    // The same dependency versions as the workspace, so nothing builds twice.
    fs::copy(workspace.join("Cargo.lock"), crate_dir.join("Cargo.lock")).unwrap();
    let exe = std::env::current_exe().unwrap();
    let target = exe.parent().unwrap().parent().unwrap().parent().unwrap();
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["test", "--manifest-path"])
        .arg(crate_dir.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", target.join("template-check"))
        .status()
        .unwrap();
    assert!(status.success(), "the generated crate failed its own test");
}
