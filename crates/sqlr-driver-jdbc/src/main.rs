//! `sqlr-driver-jdbc --preset <id>`: starts the Java bridge for a database.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

use std::path::PathBuf;
use std::process::ExitCode;

use sqlr_driver_jdbc::{
    answer_with_error, bridge_jar, classpath, find_java, java_major, launch_command, preset,
};

fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    // Say it over the protocol too, so the app shows it.
    let _ = answer_with_error(std::io::stdin().lock(), std::io::stdout().lock(), message);
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut id = "jdbc".to_string();
    while let Some(arg) = args.next() {
        if arg == "--preset" {
            id = args.next().unwrap_or_default();
        }
    }
    let Some(preset) = preset(&id) else {
        return fail(&format!("unknown JDBC preset {id:?}"));
    };
    let env = |k: &str| std::env::var_os(k);
    let path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let homes = sqlr_driver_jdbc::system_java_homes();
    let java = match find_java(&env, &path, &homes, &java_major) {
        Ok(java) => java,
        Err(e) => return fail(&e.to_string()),
    };
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(PathBuf::from))
        .unwrap_or_default();
    let Some(bridge) = bridge_jar(&exe_dir, env!("SQLR_JDBC_BRIDGE_JAR")) else {
        return fail(
            "The JDBC bridge jar (sqlr-jdbc-bridge.jar) is missing next to the driver. \
             Reinstall the driver from the Driver Manager.",
        );
    };
    let extra = std::env::var("SQLR_JDBC_CLASSPATH").ok();
    let cp = classpath(&bridge, &exe_dir.join("jars"), extra.as_deref());
    let mut command = launch_command(&java, &cp, &preset.bridge_args());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Replace this process: the Java bridge talks to sqlr directly.
        let error = command.exec();
        fail(&format!("cannot start Java ({}): {error}", java.display()))
    }
    #[cfg(not(unix))]
    {
        match command.status() {
            Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
            Err(error) => fail(&format!("cannot start Java ({}): {error}", java.display())),
        }
    }
}
