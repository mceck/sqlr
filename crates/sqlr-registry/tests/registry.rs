//! Signed index, verified downloads, install layout, versions and manual installs.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sqlr_registry::{Registry, RegistryError, platform_key};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ----- a driver to install: the SDK's echo example -----

fn echo_binary() -> PathBuf {
    sqlr_testkit::workspace_binary("examples/echo_driver")
}

/// A `.tar.gz` holding `driver.toml` and the echo driver as `bin/echo`.
fn archive(id: &str, version: &str) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    let toml = format!("id = \"{id}\"\nversion = \"{version}\"\nexecutable = \"bin/echo\"\n");
    add(&mut tar, "driver.toml", toml.as_bytes(), 0o644);
    add(
        &mut tar,
        "bin/echo",
        &std::fs::read(echo_binary()).unwrap(),
        0o644,
    );
    gzip(tar.into_inner().unwrap())
}

fn add(tar: &mut tar::Builder<Vec<u8>>, path: &str, data: &[u8], mode: u32) {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    tar.append_data(&mut header, path, data).unwrap();
}

fn gzip(bytes: Vec<u8>) -> Vec<u8> {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&bytes).unwrap();
    gz.finish().unwrap()
}

// ----- keys and signatures -----

struct Keys {
    pair: minisign::KeyPair,
}

impl Keys {
    fn new() -> Self {
        Self {
            pair: minisign::KeyPair::generate_unencrypted_keypair().unwrap(),
        }
    }
    fn public(&self) -> String {
        self.pair.pk.to_box().unwrap().into_string()
    }
    fn sign(&self, data: &[u8]) -> String {
        minisign::sign(
            Some(&self.pair.pk),
            &self.pair.sk,
            Cursor::new(data),
            Some("sqlr test"),
            None,
        )
        .unwrap()
        .into_string()
    }
}

fn sha(bytes: &[u8]) -> String {
    sqlr_registry::sha256_hex(bytes)
}

// ----- a tiny HTTP server -----

#[derive(Clone)]
enum Route {
    Ok(Vec<u8>),
    Redirect(String),
    /// Announces the full length, sends half, closes.
    Truncated(Vec<u8>),
}

struct Server {
    port: u16,
    routes: Arc<Mutex<HashMap<String, Route>>>,
}

impl Server {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes: Arc<Mutex<HashMap<String, Route>>> = Arc::default();
        let shared = routes.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let routes = shared.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = request
                        .lines()
                        .next()
                        .and_then(|l| l.split(' ').nth(1))
                        .unwrap_or("/")
                        .to_string();
                    let route = routes.lock().unwrap().get(&path).cloned();
                    let _ = match route {
                        Some(Route::Ok(body)) => {
                            let head = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            socket.write_all(head.as_bytes()).await.ok();
                            socket.write_all(&body).await
                        }
                        Some(Route::Redirect(to)) => {
                            let head = format!(
                                "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            );
                            socket.write_all(head.as_bytes()).await
                        }
                        Some(Route::Truncated(body)) => {
                            let head = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            socket.write_all(head.as_bytes()).await.ok();
                            socket.write_all(&body[..body.len() / 2]).await
                        }
                        None => {
                            socket
                                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                                .await
                        }
                    };
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self { port, routes }
    }
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
    fn set(&self, path: &str, route: Route) {
        self.routes.lock().unwrap().insert(path.into(), route);
    }
}

// ----- a registry world -----

struct World {
    server: Server,
    keys: Keys,
    dir: tempfile::TempDir,
}

impl World {
    async fn new() -> Self {
        Self {
            server: Server::start().await,
            keys: Keys::new(),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// Publishes `echo` at `version`: its archive and an index naming every version in `index`.
    fn publish(&self, version: &str) -> Vec<u8> {
        let bytes = archive("echo", version);
        self.server
            .set(&format!("/echo-{version}.tar.gz"), Route::Ok(bytes.clone()));
        bytes
    }

    fn index_json(&self, entries: &[(&str, String, String)]) -> Vec<u8> {
        let drivers: Vec<_> = entries
            .iter()
            .map(|(version, sha256, signature)| {
                serde_json::json!({
                    "id": "echo", "name": "Echo", "version": version,
                    "min_protocol": "1.0.0", "family": "relational",
                    "description": "A test driver", "homepage": "https://example.com",
                    "license": "MIT",
                    "artifacts": { platform_key(): {
                        "url": self.server.url(&format!("/echo-{version}.tar.gz")),
                        "sha256": sha256, "signature": signature,
                    }},
                })
            })
            .collect();
        // One entry per id: the index lists the newest.
        serde_json::to_vec(
            &serde_json::json!({ "format": 1, "drivers": [drivers.last().unwrap()] }),
        )
        .unwrap()
    }

    /// Serves a correctly signed index for `version` and returns the registry reading it.
    fn registry_for(&self, version: &str) -> Registry {
        let bytes = self.publish(version);
        let index = self.index_json(&[(version, sha(&bytes), self.keys.sign(&bytes))]);
        self.serve_index(&index, &self.keys.sign(&index));
        self.registry()
    }

    fn serve_index(&self, index: &[u8], signature: &str) {
        self.server.set("/index.json", Route::Ok(index.to_vec()));
        self.server.set(
            "/index.json.minisig",
            Route::Ok(signature.as_bytes().to_vec()),
        );
    }

    fn registry(&self) -> Registry {
        Registry::new(
            self.dir.path().to_path_buf(),
            self.server.url("/index.json"),
            &self.keys.public(),
        )
    }

    fn staging_is_empty(&self) -> bool {
        let staging = self.dir.path().join("drivers/.staging");
        std::fs::read_dir(staging).map_or(true, |mut d| d.next().is_none())
    }
}

fn no_progress(_: u64, _: Option<u64>) {}

#[tokio::test]
async fn index_signature_verified() {
    let world = World::new().await;
    let registry = world.registry_for("1.0.0");
    let entries = registry.fetch_index().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "echo");
    assert_eq!(entries[0].version, "1.0.0");
    assert!(
        entries[0].artifact.is_some(),
        "a build for {}",
        platform_key()
    );

    // The same signature over a different index is refused.
    let tampered = world.index_json(&[("9.9.9", "00".into(), "x".into())]);
    world.server.set("/index.json", Route::Ok(tampered));
    let error = registry.fetch_index().await.unwrap_err();
    assert!(matches!(error, RegistryError::Signature { .. }), "{error}");
}

#[tokio::test]
async fn index_signed_by_other_key_refused() {
    let world = World::new().await;
    let registry = world.registry_for("1.0.0");
    let other = Keys::new();
    let bytes = world.publish("1.0.0");
    let index = world.index_json(&[("1.0.0", sha(&bytes), other.sign(&bytes))]);
    world.serve_index(&index, &other.sign(&index));
    let error = registry.fetch_index().await.unwrap_err();
    assert!(matches!(error, RegistryError::Signature { .. }), "{error}");
}

#[tokio::test]
async fn index_can_be_a_local_mirror() {
    let world = World::new().await;
    let bytes = world.publish("1.0.0");
    let index = world.index_json(&[("1.0.0", sha(&bytes), world.keys.sign(&bytes))]);
    let mirror = tempfile::tempdir().unwrap();
    std::fs::write(mirror.path().join("index.json"), &index).unwrap();
    std::fs::write(
        mirror.path().join("index.json.minisig"),
        world.keys.sign(&index),
    )
    .unwrap();
    let registry = Registry::new(
        world.dir.path().to_path_buf(),
        format!("file://{}", mirror.path().join("index.json").display()),
        &world.keys.public(),
    );
    assert_eq!(registry.fetch_index().await.unwrap().len(), 1);
}

#[tokio::test]
async fn install_layout_and_active() {
    let world = World::new().await;
    let registry = world.registry_for("1.0.0");
    let entry = registry.fetch_index().await.unwrap().remove(0);
    let seen = Mutex::new(Vec::new());
    let installed = registry
        .install(&entry, |done, total| {
            seen.lock().unwrap().push((done, total))
        })
        .await
        .unwrap();
    assert_eq!(
        (installed.id.as_str(), installed.active.as_str()),
        ("echo", "1.0.0")
    );
    assert_eq!(installed.versions, ["1.0.0"]);
    assert_eq!(installed.manifest.id, "echo");
    assert!(!installed.manual);
    let root = world.dir.path().join("drivers/echo");
    assert!(root.join("1.0.0/driver.toml").is_file());
    assert!(root.join("1.0.0/manifest.json").is_file());
    assert!(root.join("1.0.0/bin/echo").is_file());
    assert_eq!(
        std::fs::read_to_string(root.join("active")).unwrap(),
        "1.0.0"
    );
    assert!(!seen.lock().unwrap().is_empty(), "progress was reported");
    assert!(world.staging_is_empty());

    // The installed executable is the one that runs.
    assert_eq!(installed.executable().unwrap(), root.join("1.0.0/bin/echo"));
    // Installing the same version again is refused, not overwritten.
    let again = registry.install(&entry, no_progress).await.unwrap_err();
    assert!(
        matches!(again, RegistryError::AlreadyInstalled { .. }),
        "{again}"
    );
    // A fresh Registry sees the install from disk.
    let listed = world.registry().installed().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].manifest.name, "Echo");
}

#[tokio::test]
async fn tampered_artifact_refused() {
    let world = World::new().await;
    let good = world.publish("1.0.0");
    let registry = world.registry();

    for (case, sha256, signature, expect_sha) in [
        (
            "wrong sha256",
            sha(b"not the artifact"),
            world.keys.sign(&good),
            true,
        ),
        ("other key", sha(&good), Keys::new().sign(&good), false),
        ("no signature", sha(&good), String::new(), false),
        (
            "signature of other bytes",
            sha(&good),
            world.keys.sign(b"other"),
            false,
        ),
    ] {
        let index = world.index_json(&[("1.0.0", sha256, signature)]);
        world.serve_index(&index, &world.keys.sign(&index));
        let entry = registry.fetch_index().await.unwrap().remove(0);
        let error = registry.install(&entry, no_progress).await.unwrap_err();
        if expect_sha {
            assert!(
                matches!(error, RegistryError::Sha256Mismatch { .. }),
                "{case}: {error}"
            );
        } else {
            assert!(
                matches!(error, RegistryError::Signature { .. }),
                "{case}: {error}"
            );
        }
        assert!(
            !world.dir.path().join("drivers/echo").exists(),
            "{case}: nothing installed"
        );
        assert!(world.staging_is_empty(), "{case}: nothing left over");
    }
}

#[tokio::test]
async fn partial_download_cleaned() {
    let world = World::new().await;
    let registry = world.registry_for("1.0.0");
    let entry = registry.fetch_index().await.unwrap().remove(0);
    let bytes = archive("echo", "1.0.0");
    world
        .server
        .set("/echo-1.0.0.tar.gz", Route::Truncated(bytes));
    let error = registry.install(&entry, no_progress).await.unwrap_err();
    assert!(matches!(error, RegistryError::Fetch { .. }), "{error}");
    assert!(world.staging_is_empty());
    assert!(!world.dir.path().join("drivers/echo").exists());
}

#[tokio::test]
async fn redirect_to_other_host_refused() {
    let world = World::new().await;
    let registry = world.registry_for("1.0.0");
    let entry = registry.fetch_index().await.unwrap().remove(0);
    let bytes = archive("echo", "1.0.0");
    // `localhost` is another host than `127.0.0.1`, though the same server answers.
    world.server.set("/elsewhere", Route::Ok(bytes.clone()));
    world.server.set(
        "/echo-1.0.0.tar.gz",
        Route::Redirect(format!("http://localhost:{}/elsewhere", world.server.port)),
    );
    let error = registry.install(&entry, no_progress).await.unwrap_err();
    assert!(
        matches!(error, RegistryError::RedirectToOtherHost { .. }),
        "{error}"
    );
    assert!(world.staging_is_empty());

    // A redirect inside the same host is followed.
    world
        .server
        .set("/echo-1.0.0.tar.gz", Route::Redirect("/elsewhere".into()));
    registry.install(&entry, no_progress).await.unwrap();
}

#[tokio::test]
async fn rollback_by_activate() {
    let world = World::new().await;
    for version in ["1.0.0", "1.1.0"] {
        let registry = world.registry_for(version);
        let entry = registry.fetch_index().await.unwrap().remove(0);
        registry.install(&entry, no_progress).await.unwrap();
    }
    let registry = world.registry();
    let driver = &registry.installed().unwrap()[0];
    assert_eq!(driver.versions, ["1.0.0", "1.1.0"]);
    assert_eq!(driver.active, "1.1.0");
    registry.activate("echo", "1.0.0").unwrap();
    assert_eq!(registry.installed().unwrap()[0].active, "1.0.0");
    assert!(matches!(
        registry.activate("echo", "9.9.9"),
        Err(RegistryError::NotInstalled(_))
    ));
    assert!(registry.activate("../x", "1.0.0").is_err());
}

#[tokio::test]
async fn remove_version_and_all() {
    let world = World::new().await;
    for version in ["1.0.0", "1.1.0"] {
        let registry = world.registry_for(version);
        let entry = registry.fetch_index().await.unwrap().remove(0);
        registry.install(&entry, no_progress).await.unwrap();
    }
    let registry = world.registry();
    registry.remove("echo", Some("1.1.0")).unwrap();
    let left = &registry.installed().unwrap()[0];
    assert_eq!(
        (left.versions.as_slice(), left.active.as_str()),
        (&["1.0.0".to_string()][..], "1.0.0")
    );
    registry.remove("echo", Some("1.0.0")).unwrap();
    assert!(registry.installed().unwrap().is_empty());

    let registry2 = world.registry_for("2.0.0");
    let entry = registry2.fetch_index().await.unwrap().remove(0);
    registry2.install(&entry, no_progress).await.unwrap();
    registry2.remove("echo", None).unwrap();
    assert!(registry2.installed().unwrap().is_empty());
    assert!(matches!(
        registry2.remove("echo", None),
        Err(RegistryError::NotInstalled(_))
    ));
    assert!(registry2.remove("../..", None).is_err());
}

#[tokio::test]
async fn manual_install_requires_confirmation() {
    let world = World::new().await;
    let registry = world.registry();
    let bytes = archive("echo", "0.3.0");
    let file = world.dir.path().join("echo.tar.gz");
    std::fs::write(&file, &bytes).unwrap();

    let inspection = registry.install_manual(&file).await.unwrap();
    assert_eq!(
        (inspection.id.as_str(), inspection.version.as_str()),
        ("echo", "0.3.0")
    );
    assert_eq!(inspection.sha256, sha(&bytes), "the user sees the checksum");
    assert!(
        registry.installed().unwrap().is_empty(),
        "inspecting installs nothing"
    );
    assert!(!world.dir.path().join("drivers/echo").exists());

    // The file changes after the user looked: the confirmation is refused.
    std::fs::write(
        &file,
        archive("echo", "0.3.0")
            .iter()
            .chain(b"x")
            .copied()
            .collect::<Vec<u8>>(),
    )
    .unwrap();
    let error = registry.confirm_manual(&inspection).await.unwrap_err();
    assert!(
        matches!(error, RegistryError::Sha256Mismatch { .. }),
        "{error}"
    );
    assert!(registry.installed().unwrap().is_empty());

    std::fs::write(&file, &bytes).unwrap();
    let installed = registry.confirm_manual(&inspection).await.unwrap();
    assert!(installed.manual);
    assert_eq!(installed.active, "0.3.0");
    assert!(world.staging_is_empty());
}

fn raw_tar(entries: &[(&str, tar::EntryType, &[u8], Option<&str>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, kind, data, link) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(*kind);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        // Write the name raw: the safe setters refuse `..` and absolute paths.
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        if let Some(link) = link {
            header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        header.set_cksum();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.extend_from_slice(&[0u8; 1024]);
    gzip(out)
}

#[tokio::test]
async fn hostile_archives_are_refused() {
    let world = World::new().await;
    let registry = world.registry();
    let toml = b"id = \"echo\"\nversion = \"1.0.0\"\nexecutable = \"bin/echo\"\n";
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "parent traversal",
            raw_tar(&[
                ("driver.toml", tar::EntryType::Regular, toml, None),
                ("../escape", tar::EntryType::Regular, b"x", None),
            ]),
        ),
        (
            "absolute path",
            raw_tar(&[
                ("driver.toml", tar::EntryType::Regular, toml, None),
                ("/tmp/escape", tar::EntryType::Regular, b"x", None),
            ]),
        ),
        (
            "symlink",
            raw_tar(&[
                ("driver.toml", tar::EntryType::Regular, toml, None),
                ("bin", tar::EntryType::Symlink, b"", Some("/etc")),
            ]),
        ),
        (
            "no driver.toml",
            raw_tar(&[("readme", tar::EntryType::Regular, b"x", None)]),
        ),
        (
            "executable escapes",
            raw_tar(&[(
                "driver.toml",
                tar::EntryType::Regular,
                b"id = \"echo\"\nversion = \"1.0.0\"\nexecutable = \"../../bin/sh\"\n",
                None,
            )]),
        ),
        (
            "executable missing",
            raw_tar(&[("driver.toml", tar::EntryType::Regular, toml, None)]),
        ),
        (
            "bad id",
            raw_tar(&[(
                "driver.toml",
                tar::EntryType::Regular,
                b"id = \"../x\"\nversion = \"1.0.0\"\nexecutable = \"a\"\n",
                None,
            )]),
        ),
        ("not an archive", b"plain bytes".to_vec()),
    ];
    for (case, bytes) in cases {
        let file = world.dir.path().join("evil.tar.gz");
        std::fs::write(&file, &bytes).unwrap();
        let result = match registry.install_manual(&file).await {
            Ok(inspection) => registry.confirm_manual(&inspection).await.map(|_| ()),
            Err(e) => Err(e),
        };
        assert!(result.is_err(), "{case} was accepted");
        assert!(registry.installed().unwrap().is_empty(), "{case}");
        assert!(world.staging_is_empty(), "{case}");
    }
    assert!(!Path::new("/tmp/escape").exists());
}

#[tokio::test]
async fn a_driver_that_does_not_start_is_not_installed() {
    let world = World::new().await;
    let registry = world.registry();
    let mut tar = tar::Builder::new(Vec::new());
    add(
        &mut tar,
        "driver.toml",
        b"id = \"dud\"\nversion = \"1.0.0\"\nexecutable = \"dud\"\n",
        0o644,
    );
    add(&mut tar, "dud", b"#!/bin/sh\nexit 1\n", 0o755);
    let file = world.dir.path().join("dud.tar.gz");
    std::fs::write(&file, gzip(tar.into_inner().unwrap())).unwrap();
    let inspection = registry.install_manual(&file).await.unwrap();
    let error = registry.confirm_manual(&inspection).await.unwrap_err();
    assert!(matches!(error, RegistryError::Probe(_)), "{error}");
    assert!(registry.installed().unwrap().is_empty());
    assert!(world.staging_is_empty());
}

// ----- JDBC jars -----

/// A mirror directory in Maven layout holding `h2-1.0.jar` for group `com.example`.
fn maven_mirror(dir: &Path, bytes: &[u8]) -> PathBuf {
    let base = dir.join("maven");
    let folder = base.join("com/example/h2/1.0");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("h2-1.0.jar"), bytes).unwrap();
    base
}

fn jdbc_index(world: &World, version: &str, jar_sha: &str) -> Vec<u8> {
    let bytes = world.publish(version);
    let mut index: serde_json::Value = serde_json::from_slice(&world.index_json(&[(
        version,
        sha(&bytes),
        world.keys.sign(&bytes),
    )]))
    .unwrap();
    index["drivers"][0]["jdbc"] = serde_json::json!({ "maven": [
        { "group": "com.example", "artifact": "h2", "version": "1.0", "sha256": jar_sha }
    ]});
    serde_json::to_vec(&index).unwrap()
}

#[tokio::test]
async fn jdbc_jars_are_downloaded_and_verified() {
    let world = World::new().await;
    let jar = b"PK pretend this is a jar";
    let mirror = tempfile::tempdir().unwrap();
    let base = maven_mirror(mirror.path(), jar);
    world.publish("1.0.0");
    let index = jdbc_index(&world, "1.0.0", &sha(jar));
    world.serve_index(&index, &world.keys.sign(&index));
    let registry = world
        .registry()
        .with_maven_base(format!("file://{}", base.display()));
    let entry = registry.fetch_index().await.unwrap().remove(0);
    assert_eq!(entry.jdbc.len(), 1);
    let installed = registry.install(&entry, no_progress).await.unwrap();
    let kept = installed.path.join("jars/h2-1.0.jar");
    assert_eq!(std::fs::read(kept).unwrap(), jar);
}

#[tokio::test]
async fn a_tampered_jdbc_jar_is_refused_and_leaves_nothing() {
    let world = World::new().await;
    let mirror = tempfile::tempdir().unwrap();
    let base = maven_mirror(mirror.path(), b"PK evil replacement");
    world.publish("1.0.0");
    let index = jdbc_index(&world, "1.0.0", &sha(b"PK the jar the index names"));
    world.serve_index(&index, &world.keys.sign(&index));
    let registry = world
        .registry()
        .with_maven_base(format!("file://{}", base.display()));
    let entry = registry.fetch_index().await.unwrap().remove(0);
    let error = registry.install(&entry, no_progress).await.unwrap_err();
    assert!(
        matches!(error, RegistryError::Sha256Mismatch { .. }),
        "{error}"
    );
    assert!(registry.installed().unwrap().is_empty());
    assert!(world.staging_is_empty());
}

#[tokio::test]
async fn jdbc_jar_names_cannot_escape_the_install() {
    let world = World::new().await;
    world.publish("1.0.0");
    let mut index: serde_json::Value =
        serde_json::from_slice(&jdbc_index(&world, "1.0.0", "00")).unwrap();
    index["drivers"][0]["jdbc"]["maven"][0]["artifact"] = "../../evil".into();
    let index = serde_json::to_vec(&index).unwrap();
    world.serve_index(&index, &world.keys.sign(&index));
    let registry = world.registry();
    let entry = registry.fetch_index().await.unwrap().remove(0);
    let error = registry.install(&entry, no_progress).await.unwrap_err();
    assert!(matches!(error, RegistryError::BadIndex(_)), "{error}");
    assert!(registry.installed().unwrap().is_empty());
}

#[tokio::test]
async fn driver_toml_args_reach_the_installed_driver() {
    let world = World::new().await;
    let registry = world.registry();
    let mut tar = tar::Builder::new(Vec::new());
    add(
        &mut tar,
        "driver.toml",
        b"id = \"echo\"\nversion = \"0.1.0\"\nexecutable = \"bin/echo\"\nargs = [\"--preset\", \"h2\"]\n",
        0o644,
    );
    add(
        &mut tar,
        "bin/echo",
        &std::fs::read(echo_binary()).unwrap(),
        0o644,
    );
    let file = world.dir.path().join("a.tar.gz");
    std::fs::write(&file, gzip(tar.into_inner().unwrap())).unwrap();
    let inspection = registry.install_manual(&file).await.unwrap();
    let installed = registry.confirm_manual(&inspection).await.unwrap();
    assert_eq!(installed.args(), ["--preset", "h2"]);
}

#[test]
fn the_documented_example_index_is_valid() {
    let text = include_str!("../../../docs/drivers/example-index.json");
    let value: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(value["format"], 1);
    let drivers = value["drivers"].as_array().unwrap();
    assert!(drivers.iter().any(|d| d["id"] == "hana"));
    let h2 = drivers.iter().find(|d| d["id"] == "jdbc-h2").unwrap();
    assert_eq!(h2["jdbc"]["maven"][0]["sha256"].as_str().unwrap().len(), 64);
}
