//! The JDBC bridge against H2 (`com.h2database:h2:2.3.232`, downloaded once into
//! `target/jdbc-test-jars/` and checked against its sha256). Tests that need Java, a
//! built bridge or the jar print why they are skipped.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlr_driver_jdbc::{find_java, java_major};
use sqlr_driver_sdk::{
    Cell, ColumnKind, Connection, ConnectionConfig, DisplayHint, Driver, Execution, Secrets,
    TableRef, Value,
};
use sqlr_driver_tests::{Target, TypeCase, relational_suite};
use sqlr_sidecar::{SidecarDriver, SidecarSpec};

const H2_URL: &str = "https://repo1.maven.org/maven2/com/h2database/h2/2.3.232/h2-2.3.232.jar";
const H2_SHA256: &str = "8dae62d22db8982c3dcb3826edb9c727c5d302063a67eef7d63d82de401f07d3";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn have_java() -> bool {
    let env = |k: &str| std::env::var_os(k);
    let path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    find_java(
        &env,
        &path,
        &sqlr_driver_jdbc::macos_java_homes_for_launcher(),
        &java_major,
    )
    .is_ok()
}

/// The H2 jar, downloaded on first use; `None` (with the reason printed) when offline.
fn h2_jar() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let target = exe.ancestors().nth(3)?; // …/target
    let dir = target.join("jdbc-test-jars");
    let jar = dir.join("h2-2.3.232.jar");
    let valid = |p: &Path| std::fs::read(p).is_ok_and(|b| hex(&Sha256::digest(&b)) == H2_SHA256);
    if valid(&jar) {
        return Some(jar);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let part = dir.join("h2.part");
    let ok = Command::new("curl")
        .args(["-fsSL", "--max-time", "120", "-o"])
        .arg(&part)
        .arg(H2_URL)
        .status()
        .is_ok_and(|s| s.success());
    if !ok || !valid(&part) {
        let _ = std::fs::remove_file(&part);
        eprintln!("skipped: cannot download H2 (offline, or the file does not match its sha256)");
        return None;
    }
    std::fs::rename(&part, &jar).ok()?;
    Some(jar)
}

/// What the tests need, or `None` with the reason printed.
fn rig() -> Option<PathBuf> {
    if !have_java() {
        eprintln!("skipped: no Java 17+ found");
        return None;
    }
    if env!("SQLR_JDBC_BRIDGE_JAR").is_empty() {
        eprintln!("skipped: the JDBC bridge was built without a JDK");
        return None;
    }
    h2_jar()
}

fn spec(preset: &str, jar: Option<&Path>) -> SidecarSpec {
    let mut spec = SidecarSpec::new(preset, "0.1.0", env!("CARGO_BIN_EXE_sqlr-driver-jdbc"));
    spec.args = vec!["--preset".into(), preset.into()];
    if let Some(jar) = jar {
        spec.env
            .push(("SQLR_JDBC_CLASSPATH".into(), jar.display().to_string()));
    }
    spec.handshake_timeout = Duration::from_secs(60);
    spec
}

fn config(driver: &str, values: &[(&str, &str)]) -> ConnectionConfig {
    ConnectionConfig {
        id: "j".into(),
        name: "j".into(),
        driver: driver.into(),
        group: None,
        color: None,
        values: values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}

async fn h2(name: &str) -> Arc<dyn Connection> {
    let jar = rig().expect("checked by the caller");
    let (driver, _) = SidecarDriver::probe(spec("jdbc-h2", Some(&jar)))
        .await
        .unwrap();
    let path = format!("mem:{name};DB_CLOSE_DELAY=-1");
    driver
        .connect(
            &config("jdbc-h2", &[("path", &path), ("user", "sa")]),
            &Secrets::default(),
        )
        .await
        .unwrap()
}

#[test]
fn bridge_builds_with_javac() {
    let javac = Command::new("javac").arg("-version").output();
    let java_home = std::env::var("JAVA_HOME")
        .ok()
        .map(|h| Path::new(&h).join("bin/javac"));
    if javac.is_err() && !java_home.as_ref().is_some_and(|p| p.is_file()) {
        eprintln!("skipped: no javac");
        return;
    }
    let out = tempfile::tempdir().unwrap();
    let jar = out.path().join("bridge.jar");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../drivers/jdbc-bridge/build.sh");
    let built = Command::new("sh").arg(&script).arg(&jar).output().unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let listing = Command::new("unzip").arg("-l").arg(&jar).output();
    if let Ok(listing) = listing {
        let text = String::from_utf8_lossy(&listing.stdout);
        assert!(text.contains("dev/sqlr/jdbc/Main.class"), "{text}");
        assert!(text.contains("dev/sqlr/jdbc/Bridge.class"), "{text}");
    }
    assert!(jar.metadata().unwrap().len() > 10_000);
}

#[tokio::test]
async fn jdbc_h2_conformance() {
    let Some(jar) = rig() else { return };
    let (driver, manifest) = SidecarDriver::probe(spec("jdbc-h2", Some(&jar)))
        .await
        .unwrap();
    assert_eq!(manifest.id, "jdbc-h2");
    assert!(manifest.fields.iter().any(|f| f.key == "path"));
    let driver = Arc::new(driver);
    let mut target = Target::new(
        "h2 over jdbc",
        Box::new(move || {
            let driver = driver.clone();
            Box::pin(async move {
                driver
                    .connect(
                        &config(
                            "jdbc-h2",
                            &[
                                ("path", "mem:conformance;DB_CLOSE_DELAY=-1"),
                                ("user", "sa"),
                            ],
                        ),
                        &Secrets::default(),
                    )
                    .await
            })
        }),
    )
    .with_setup([
        "DROP TABLE IF EXISTS \"sqlr_conformance\"",
        "CREATE TABLE \"sqlr_conformance\" (\"n\" INT PRIMARY KEY, \"t\" VARCHAR(50))",
        "INSERT INTO \"sqlr_conformance\" SELECT X, 'row ' || X FROM SYSTEM_RANGE(1, 25)",
    ]);
    target.table = TableRef {
        database: None,
        schema: Some("PUBLIC".into()),
        name: "sqlr_conformance".into(),
    };
    target.capabilities.server_cancel = true;
    target.capabilities.editable_results = true;
    target.slow_sql = Some("SELECT COUNT(*) FROM SYSTEM_RANGE(1, 100000000000)".into());
    target.type_cases = vec![
        TypeCase {
            name: "integer".into(),
            sql: "SELECT 42".into(),
            expect: Value::Int(42),
        },
        TypeCase {
            name: "decimal".into(),
            sql: "SELECT CAST('12345678901234567890.0123456789' AS DECIMAL(38,10))".into(),
            expect: Value::Text("12345678901234567890.0123456789".into()),
        },
        TypeCase {
            name: "text".into(),
            sql: "SELECT 'héllo 🚀'".into(),
            expect: Value::Text("héllo 🚀".into()),
        },
        TypeCase {
            name: "varbinary".into(),
            sql: "SELECT X'00ff'".into(),
            expect: Value::Bytes(vec![0, 255]),
        },
        TypeCase {
            name: "null".into(),
            sql: "SELECT CAST(NULL AS INT)".into(),
            expect: Value::Null,
        },
    ];
    let report = relational_suite(&target).await;
    assert!(report.failed.is_empty(), "{report}");
    assert!(report.passed.len() >= 11, "{report}");
}

#[tokio::test]
async fn jdbc_type_matrix_h2() {
    if rig().is_none() {
        return;
    }
    let conn = h2("matrix").await;
    let rel = conn.relational().unwrap();
    let sql = "SELECT \
        CAST('12345678901234567890123456.7890123456' AS DECIMAL(38,10)) AS dec, \
        TIMESTAMP WITH TIME ZONE '2024-01-02 03:04:05.123456+02:00' AS tstz, \
        TIMESTAMP '2024-01-02 03:04:05' AS ts, DATE '2024-01-02' AS d, TIME '03:04:05' AS t, \
        CAST(X'0102' AS BLOB) AS b, CAST('long text' AS CLOB) AS c, \
        ARRAY[1, 2, NULL] AS a, ARRAY['a', 'b''c'] AS sa, \
        CAST('6ba7b810-9dad-11d1-80b4-00c04fd430c8' AS UUID) AS u, \
        JSON '{\"a\": [1, 2]}' AS j, INTERVAL '1 2:03:04' DAY TO SECOND AS iv, \
        TRUE AS bo, CAST(7 AS TINYINT) AS ti, CAST(9223372036854775807 AS BIGINT) AS bi, \
        CAST(1.5 AS REAL) AS re, CAST(2.25 AS DOUBLE PRECISION) AS dp, CAST('ab' AS CHAR(4)) AS ch, \
        ROW(1, 2) AS r, CAST(NULL AS VARCHAR(5)) AS nl";
    let Execution::Rows(mut rows) = rel.execute(sql).await.unwrap() else {
        panic!("rows expected");
    };
    let columns = rows.columns();
    let chunk = rows.next_chunk(10).await.unwrap().unwrap();
    let col = |name: &str| {
        columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
            .unwrap()
    };
    let cell = |name: &str| chunk.cell(0, col(name));
    let meta = |name: &str| (columns[col(name)].kind, columns[col(name)].hint);

    // DECIMAL beyond f64: exact text with the decimal hint.
    assert_eq!(
        cell("dec"),
        Cell::Text("12345678901234567890123456.7890123456")
    );
    assert_eq!(meta("dec"), (ColumnKind::Text, DisplayHint::Decimal));
    // Time zones survive; dates and times are ISO text.
    assert_eq!(cell("tstz"), Cell::Text("2024-01-02T03:04:05.123456+02:00"));
    assert_eq!(cell("ts"), Cell::Text("2024-01-02T03:04:05"));
    assert_eq!(cell("d"), Cell::Text("2024-01-02"));
    assert_eq!(cell("t"), Cell::Text("03:04:05"));
    assert_eq!(meta("tstz").1, DisplayHint::Temporal);
    // Large objects arrive as bytes and text.
    assert_eq!(cell("b"), Cell::Bytes(&[1, 2]));
    assert_eq!(meta("b"), (ColumnKind::Bytes, DisplayHint::Binary));
    assert_eq!(cell("c"), Cell::Text("long text"));
    // Arrays are readable text, never an object's toString().
    assert_eq!(cell("a"), Cell::Text("[1, 2, NULL]"));
    assert_eq!(cell("sa"), Cell::Text("['a', 'b''c']"));
    assert_eq!(meta("a").1, DisplayHint::Array);
    // Vendor types come out as text.
    assert_eq!(
        cell("u"),
        Cell::Text("6ba7b810-9dad-11d1-80b4-00c04fd430c8")
    );
    assert_eq!(meta("u").0, ColumnKind::Text);
    match cell("j") {
        Cell::Text(t) => assert!(t.contains("\"a\"") && t.contains('['), "{t}"),
        other => panic!("json: {other:?}"),
    }
    match cell("iv") {
        Cell::Text(t) => assert!(t.contains("1") && t.contains("03"), "{t}"),
        other => panic!("interval: {other:?}"),
    }
    // Numbers and booleans keep their kinds.
    assert_eq!(cell("bo"), Cell::Bool(true));
    assert_eq!(cell("ti"), Cell::Int(7));
    assert_eq!(cell("bi"), Cell::Int(i64::MAX));
    assert_eq!(cell("re"), Cell::Float(1.5));
    assert_eq!(cell("dp"), Cell::Float(2.25));
    assert_eq!(cell("ch"), Cell::Text("ab  "));
    assert_eq!(cell("nl"), Cell::Null);
    // A driver object (a ROW) is not guessed at: no `Type@1a2b3c`.
    match cell("r") {
        Cell::Text(t) => assert!(!t.contains('@'), "{t}"),
        other => panic!("row: {other:?}"),
    }
}

#[tokio::test]
async fn jdbc_bound_parameters_and_affected_rows() {
    if rig().is_none() {
        return;
    }
    let conn = h2("params").await;
    let rel = conn.relational().unwrap();
    rel.execute("CREATE TABLE p (a INT, b VARCHAR(10), c VARBINARY(4))")
        .await
        .unwrap();
    let Execution::Affected(n) = rel
        .execute_with(
            "INSERT INTO p VALUES (?, ?, ?)",
            &[
                Value::Int(1),
                Value::Text("x".into()),
                Value::Bytes(vec![9, 8]),
            ],
        )
        .await
        .unwrap()
    else {
        panic!("affected rows expected");
    };
    assert_eq!(n, 1);
    let Execution::Rows(mut rows) = rel.execute("SELECT a, b, c FROM p").await.unwrap() else {
        panic!("rows expected");
    };
    let chunk = rows.next_chunk(10).await.unwrap().unwrap();
    assert_eq!(
        (chunk.cell(0, 0), chunk.cell(0, 1), chunk.cell(0, 2)),
        (Cell::Int(1), Cell::Text("x"), Cell::Bytes(&[9, 8]))
    );
    // SQL errors keep their kind.
    let e = rel.execute("SELEKT 1").await.err().unwrap();
    assert_eq!(e.kind, sqlr_driver_sdk::ErrorKind::Syntax, "{e:?}");
}

#[tokio::test]
async fn generic_jdbc_with_user_jar() {
    let Some(jar) = rig() else { return };
    // No jar on the launcher's class path: the user names it in the form.
    let (driver, manifest) = SidecarDriver::probe(spec("jdbc", None)).await.unwrap();
    assert_eq!(manifest.id, "jdbc");
    assert!(manifest.fields.iter().any(|f| f.key == "jdbc_url"));
    assert!(manifest.fields.iter().any(|f| f.key == "jar"));
    let conn = driver
        .connect(
            &config(
                "jdbc",
                &[
                    ("jdbc_url", "jdbc:h2:mem:generic;DB_CLOSE_DELAY=-1"),
                    ("driver_class", "org.h2.Driver"),
                    ("jar", &jar.display().to_string()),
                    ("user", "sa"),
                ],
            ),
            &Secrets::default(),
        )
        .await
        .unwrap();
    assert_eq!(conn.ping().await.unwrap().product, "H2");
    // Without the jar the failure says what is missing.
    let e = driver
        .connect(
            &config(
                "jdbc",
                &[
                    ("jdbc_url", "jdbc:h2:mem:x"),
                    ("driver_class", "org.h2.Driver"),
                ],
            ),
            &Secrets::default(),
        )
        .await
        .err()
        .unwrap();
    assert!(e.message.contains("org.h2.Driver"), "{}", e.message);
}

#[tokio::test]
async fn browse_catalog_and_describe_through_metadata() {
    if rig().is_none() {
        return;
    }
    let conn = h2("browse").await;
    let rel = conn.relational().unwrap();
    rel.execute("CREATE TABLE \"orders\" (\"id\" INT PRIMARY KEY, \"note\" VARCHAR(20) NOT NULL, \"code\" INT UNIQUE)")
        .await
        .unwrap();
    let schemas = conn
        .browse(&sqlr_driver_sdk::NodePath::root())
        .await
        .unwrap();
    assert!(schemas.iter().any(|n| n.name == "PUBLIC"), "{schemas:?}");
    let folders = conn
        .browse(&sqlr_driver_sdk::NodePath(vec!["PUBLIC".into()]))
        .await
        .unwrap();
    assert!(folders.iter().any(|n| n.name == "Tables"));
    let tables = conn
        .browse(&sqlr_driver_sdk::NodePath(vec![
            "PUBLIC".into(),
            "tables".into(),
        ]))
        .await
        .unwrap();
    let orders = tables
        .iter()
        .find(|n| n.name == "orders")
        .expect("the table");
    assert_eq!(orders.path.segments(), ["PUBLIC", "tables", "orders"]);
    let columns = conn.browse(&orders.path).await.unwrap();
    assert_eq!(
        columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["id", "note", "code"]
    );
    // The dialect built from the wire writes the same SELECT.
    assert_eq!(
        rel.dialect().select_all(orders, Some(5)).as_deref(),
        Some("SELECT * FROM \"PUBLIC\".\"orders\" LIMIT 5")
    );
    let catalog = rel.catalog("PUBLIC").await.unwrap();
    assert!(
        catalog
            .relations
            .iter()
            .any(|r| r.name == "orders" && r.columns.len() == 3)
    );
    let info = rel
        .describe(&TableRef {
            database: None,
            schema: Some("PUBLIC".into()),
            name: "orders".into(),
        })
        .await
        .unwrap();
    assert_eq!(info.primary_key, ["id"]);
    assert!(!info.column("note").unwrap().nullable);
    assert!(info.unique_keys.iter().any(|k| k == &["code".to_string()]));
}

#[tokio::test]
async fn missing_jre_is_explained_over_the_protocol() {
    let mut spec = spec("jdbc-h2", None);
    spec.env.push(("JAVA_HOME".into(), "/no/such/java".into()));
    spec.env.push(("PATH".into(), "/no/such/bin".into()));
    spec.env
        .push(("SQLR_JDBC_NO_SYSTEM_JAVA".into(), "1".into()));
    let error = SidecarDriver::probe(spec).await.err().unwrap();
    assert!(error.message.contains("Java 17"), "{}", error.message);
    assert!(error.message.contains("JAVA_HOME"), "{}", error.message);
}

#[tokio::test]
async fn a_packaged_jdbc_driver_installs_from_a_file_and_runs() {
    let Some(jar) = rig() else { return };
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("jdbc-h2.tar.gz");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/package-driver.sh");
    let packed = Command::new("sh")
        .arg(&script)
        .args(["jdbc-h2"])
        .arg(&archive)
        .args([
            "--bin",
            env!("CARGO_BIN_EXE_sqlr-driver-jdbc"),
            "--bridge",
            env!("SQLR_JDBC_BRIDGE_JAR"),
        ])
        .arg("--jar")
        .arg(&jar)
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );

    // Install it as a user would: inspect, confirm. The install starts it once (handshake).
    let registry =
        sqlr_registry::Registry::new(work.path().join("data"), "file:///none".into(), "unused");
    let inspection = registry.install_manual(&archive).await.unwrap();
    assert_eq!(
        (inspection.id.as_str(), inspection.version.as_str()),
        ("jdbc-h2", "0.1.0")
    );
    let installed = registry.confirm_manual(&inspection).await.unwrap();
    assert!(installed.manual);
    assert_eq!(installed.args(), ["--preset", "jdbc-h2"]);
    assert!(
        installed
            .path
            .join("jars")
            .join(jar.file_name().unwrap())
            .is_file()
    );
    assert!(installed.path.join("sqlr-jdbc-bridge.jar").is_file());

    // And it runs from there, with its own jars/ on the class path (no env help).
    let mut spec = SidecarSpec::new("jdbc-h2", "0.1.0", installed.executable().unwrap());
    spec.args = installed.args();
    spec.handshake_timeout = Duration::from_secs(60);
    let driver = SidecarDriver::with_manifest(spec, installed.manifest.clone());
    let conn = driver
        .connect(
            &config("jdbc-h2", &[("path", "mem:pkg"), ("user", "sa")]),
            &Secrets::default(),
        )
        .await
        .unwrap();
    assert_eq!(conn.ping().await.unwrap().product, "H2");
}

#[tokio::test]
async fn a_page_larger_than_a_frame_arrives_in_several_chunks() {
    if rig().is_none() {
        return;
    }
    let conn = h2("bigpage").await;
    let rel = conn.relational().unwrap();
    // 8 rows of 9 MB: 72 MB in one fetch, more than a 64 MiB frame can hold.
    let Execution::Rows(mut rows) = rel
        .execute("SELECT REPEAT('x', 9000000) AS big, X AS n FROM SYSTEM_RANGE(1, 8)")
        .await
        .unwrap()
    else {
        panic!("rows expected");
    };
    let mut seen = 0;
    while let Some(chunk) = rows.next_chunk(500).await.unwrap() {
        for r in 0..chunk.len() {
            assert!(matches!(chunk.cell(r, 0), Cell::Text(t) if t.len() == 9_000_000));
        }
        seen += chunk.len();
    }
    assert_eq!(seen, 8);
    // The bridge is still there.
    assert_eq!(conn.ping().await.unwrap().product, "H2");
}
