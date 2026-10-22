//! SSH tunnels (§9) against in-process `russh` servers that forward `direct-tcpip` channels
//! to a local TCP echo target.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use russh::keys::ssh_key::LineEnding;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelOpenFailure};
use sqlr_core::{ConnectionConfig, ErrorKind, Secrets};
use sqlr_engine::{DriverRegistry, Engine, QueryStatus, SessionKey, SessionStatus};
use sqlr_testkit::{FakeDriver, FakeScript};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

const USER: &str = "tunnel";
const PASSWORD: &str = "s3cret";

fn key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

/// The SSH server fixture: password `PASSWORD` or the public key of `client_key` for
/// `USER`; forwards `direct-tcpip` channels to whatever they name.
#[derive(Clone)]
struct Fixture {
    client_key: PublicKey,
    /// Client sessions alive.
    sessions: Arc<AtomicUsize>,
}

struct Handler {
    fixture: Fixture,
}

impl Drop for Handler {
    fn drop(&mut self) {
        self.fixture.sessions.fetch_sub(1, Ordering::SeqCst);
    }
}

impl server::Server for Fixture {
    type Handler = Handler;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Handler {
        self.sessions.fetch_add(1, Ordering::SeqCst);
        Handler {
            fixture: self.clone(),
        }
    }
}

impl server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == USER && password == PASSWORD {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        Ok(
            if user == USER && key.key_data() == self.fixture.client_key.key_data() {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        match TcpStream::connect((host, port as u16)).await {
            Ok(mut tcp) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
                });
            }
            Err(_) => reply.reject(ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }
}

struct SshServer {
    port: u16,
    sessions: Arc<AtomicUsize>,
    host_key: PublicKey,
}

/// Starts an SSH server with host key `seed` on a free local port.
async fn ssh_server(seed: u8, client_key: PublicKey) -> SshServer {
    let host = key(seed);
    let host_key = host.public_key().clone();
    let config = Arc::new(server::Config {
        keys: vec![host],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    let sessions = Arc::new(AtomicUsize::new(0));
    let mut fixture = Fixture {
        client_key,
        sessions: sessions.clone(),
    };
    tokio::spawn(async move {
        let _ = fixture.run_on_socket(config, &socket).await;
    });
    SshServer {
        port,
        sessions,
        host_key,
    }
}

/// A TCP server that echoes every byte back.
async fn echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

/// A TCP proxy on `port` (0: any free port) to `target`, standing for the network path to
/// the SSH server: aborting its task drops every connection through it.
async fn proxy(port: u16, target: u16) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let mut pipes = tokio::task::JoinSet::new();
        while let Ok((mut inbound, _)) = listener.accept().await {
            pipes.spawn(async move {
                if let Ok(mut outbound) = TcpStream::connect(("127.0.0.1", target)).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    (port, task)
}

fn engine(known_hosts: &Path) -> (Engine, Arc<FakeDriver>) {
    let fake = FakeDriver::with_ssh(FakeScript {
        tcp_echo: true,
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.set_known_hosts(known_hosts.to_path_buf());
    engine.set_connect_timeout(5);
    (engine, fake)
}

/// A config whose target is the echo server, through the SSH server on `ssh_port`.
fn tunneled(echo: u16, ssh_port: u16, values: &[(&str, &str)]) -> ConnectionConfig {
    let mut cfg = config("c", "fake");
    let base = [
        ("host", "127.0.0.1".to_string()),
        ("port", echo.to_string()),
        ("ssh_enabled", "true".to_string()),
        ("ssh_host", "127.0.0.1".to_string()),
        ("ssh_port", ssh_port.to_string()),
        ("ssh_user", USER.to_string()),
        ("ssh_auth", "password".to_string()),
    ];
    for (k, v) in base {
        cfg.values.insert(k.into(), v);
    }
    for (k, v) in values {
        cfg.values.insert(k.to_string(), v.to_string());
    }
    cfg
}

fn password(pw: &str) -> Secrets {
    Secrets([("ssh_password".to_string(), pw.to_string())].into())
}

/// Waits for the explorer session to leave `Connecting`.
fn outcome(engine: &Engine) -> SessionStatus {
    assert!(wait_until(Duration::from_secs(10), || !matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connecting) | None
    )));
    engine.session_status("c").unwrap()
}

#[test]
fn manifests_gain_ssh_fields() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _) = engine(&dir.path().join("known_hosts"));
    let manifest = engine.manifests().remove(0);
    let ssh: Vec<&str> = manifest
        .fields
        .iter()
        .filter(|f| f.group == "SSH")
        .map(|f| f.key.as_str())
        .collect();
    assert_eq!(
        ssh,
        [
            "ssh_enabled",
            "ssh_host",
            "ssh_port",
            "ssh_user",
            "ssh_auth",
            "ssh_password",
            "ssh_key_path",
            "ssh_key_passphrase",
            "ssh_jump_host",
            "ssh_jump_port",
            "ssh_jump_user"
        ]
    );
    let secret: Vec<&str> = manifest
        .fields
        .iter()
        .filter(|f| f.secret && f.group == "SSH")
        .map(|f| f.key.as_str())
        .collect();
    assert_eq!(secret, ["ssh_password", "ssh_key_passphrase"]);
    // Drivers without the capability are left alone.
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(FakeScript::default()));
    let plain = Engine::new(drivers, no_repaint());
    assert!(plain.manifests()[0].fields.iter().all(|f| f.group != "SSH"));
}

#[tokio::test(flavor = "multi_thread")]
async fn tunnel_forwards_tcp() {
    let dir = tempfile::tempdir().unwrap();
    let known = dir.path().join("known_hosts");
    let echo = echo_server().await;
    let server = ssh_server(1, key(9).public_key().clone()).await;
    let (engine, _) = engine(&known);
    engine.connect(tunneled(echo, server.port, &[]), password(PASSWORD), dev());
    let status = tokio::task::block_in_place(|| outcome(&engine));
    assert!(matches!(status, SessionStatus::Connected(_)), "{status:?}");

    // First use: the host key was accepted, recorded and reported.
    let report = engine.connect_report("c").unwrap();
    let host_key = report.host_key.unwrap();
    assert!(host_key.new);
    assert!(host_key.fingerprint.starts_with("SHA256:"), "{host_key:?}");
    assert!(
        std::fs::read_to_string(&known)
            .unwrap()
            .contains("ssh-ed25519")
    );
    let local = report.tunnel_port.unwrap();

    // A tab session shares the tunnel (one SSH session for the connection).
    let tab = engine.session("c", Some(1));
    let q = engine.execute_in(&tab, None, "select 1".into(), 10);
    tokio::task::block_in_place(|| {
        assert!(wait_until(TIMEOUT, || engine
            .result(q)
            .is_some_and(
                |r| r.read().status != sqlr_engine::QueryStatus::Running
            )));
    });
    assert_eq!(server.sessions.load(Ordering::SeqCst), 1);

    // Disconnecting closes the tunnel: its port and the SSH session go away.
    engine.disconnect("c");
    tokio::task::block_in_place(|| {
        assert!(wait_until(TIMEOUT, || server
            .sessions
            .load(Ordering::SeqCst)
            == 0));
    });
    assert!(TcpStream::connect(("127.0.0.1", local)).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn tunnel_with_key_file() {
    let dir = tempfile::tempdir().unwrap();
    let client = key(7);
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client.to_openssh(LineEnding::LF).unwrap().as_bytes(),
    )
    .unwrap();
    let echo = echo_server().await;
    let server = ssh_server(2, client.public_key().clone()).await;
    let (engine, _) = engine(&dir.path().join("known_hosts"));
    let key_path = key_path.display().to_string();
    let cfg = tunneled(
        echo,
        server.port,
        &[("ssh_auth", "key"), ("ssh_key_path", key_path.as_str())],
    );
    engine.connect(cfg, Secrets::default(), dev());
    let status = tokio::task::block_in_place(|| outcome(&engine));
    assert!(matches!(status, SessionStatus::Connected(_)), "{status:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_bad_auth_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let echo = echo_server().await;
    let server = ssh_server(3, key(9).public_key().clone()).await;
    let (engine, fake) = engine(&dir.path().join("known_hosts"));
    let started = Instant::now();
    engine.connect(tunneled(echo, server.port, &[]), password("wrong"), dev());
    let status = tokio::task::block_in_place(|| outcome(&engine));
    assert!(started.elapsed() < Duration::from_secs(5));
    let SessionStatus::Failed(e) = status else {
        panic!("connected with a wrong password: {status:?}")
    };
    assert_eq!(e.kind, ErrorKind::Auth, "{e:?}");
    assert!(e.message.contains("authentication"), "{e:?}");
    assert!(engine.connect_report("c").is_none());
    assert_eq!(fake.open_connections(), 0, "the driver was never called");
    // The SSH session was closed, and no tunnel was left listening.
    tokio::task::block_in_place(|| {
        assert!(wait_until(TIMEOUT, || server
            .sessions
            .load(Ordering::SeqCst)
            == 0));
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn host_key_mismatch_refused() {
    let dir = tempfile::tempdir().unwrap();
    let known = dir.path().join("known_hosts");
    let echo = echo_server().await;
    let server = ssh_server(4, key(9).public_key().clone()).await;
    // The known_hosts file holds another key for this host and port.
    russh::keys::known_hosts::learn_known_hosts_path(
        "127.0.0.1",
        server.port,
        key(5).public_key(),
        &known,
    )
    .unwrap();
    assert_ne!(server.host_key, *key(5).public_key());
    let (engine, _) = engine(&known);
    engine.connect(tunneled(echo, server.port, &[]), password(PASSWORD), dev());
    let status = tokio::task::block_in_place(|| outcome(&engine));
    let SessionStatus::Failed(e) = status else {
        panic!("connected despite a changed host key: {status:?}")
    };
    assert_eq!(e.message, "Host key changed for 127.0.0.1");
}

#[tokio::test(flavor = "multi_thread")]
async fn jump_host_chain() {
    let dir = tempfile::tempdir().unwrap();
    let echo = echo_server().await;
    let target = ssh_server(6, key(9).public_key().clone()).await;
    let jump = ssh_server(8, key(9).public_key().clone()).await;
    let (engine, _) = engine(&dir.path().join("known_hosts"));
    let jump_port = jump.port.to_string();
    let cfg = tunneled(
        echo,
        target.port,
        &[
            ("ssh_jump_host", "127.0.0.1"),
            ("ssh_jump_port", jump_port.as_str()),
            ("ssh_jump_user", USER),
        ],
    );
    engine.connect(cfg, password(PASSWORD), dev());
    let status = tokio::task::block_in_place(|| outcome(&engine));
    assert!(matches!(status, SessionStatus::Connected(_)), "{status:?}");
    assert_eq!(jump.sessions.load(Ordering::SeqCst), 1);
    assert_eq!(target.sessions.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_jump_host_fails_within_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let echo = echo_server().await;
    let target = ssh_server(10, key(9).public_key().clone()).await;
    // A port nothing listens on.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_port = closed.local_addr().unwrap().port().to_string();
    drop(closed);
    let (engine, _) = engine(&dir.path().join("known_hosts"));
    let cfg = tunneled(
        echo,
        target.port,
        &[
            ("ssh_jump_host", "127.0.0.1"),
            ("ssh_jump_port", closed_port.as_str()),
        ],
    );
    engine.connect(cfg, password(PASSWORD), dev());
    let status = tokio::task::block_in_place(|| outcome(&engine));
    let SessionStatus::Failed(e) = status else {
        panic!("{status:?}")
    };
    assert_eq!(e.kind, ErrorKind::Connection, "{e:?}");
    assert!(e.message.contains("127.0.0.1"), "{e:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tunnel_keeps_the_original_host_for_tls() {
    let dir = tempfile::tempdir().unwrap();
    let echo = echo_server().await;
    let server = ssh_server(13, key(9).public_key().clone()).await;
    let (engine, fake) = engine(&dir.path().join("known_hosts"));
    let cfg = tunneled(echo, server.port, &[("host", "localhost")]);
    engine.connect(cfg, password(PASSWORD), dev());
    let status = tokio::task::block_in_place(|| outcome(&engine));
    assert!(matches!(status, SessionStatus::Connected(_)), "{status:?}");
    let (seen, _) = fake.connects().remove(0);
    assert_eq!(seen.values["host"], "127.0.0.1");
    // The driver verifies the server's certificate against the host the user named.
    assert_eq!(seen.tls_server_name(), Some("localhost"));
}

fn run(engine: &Engine, key: &SessionKey, sql: &str) -> QueryStatus {
    let q = engine.execute_in(key, None, sql.into(), 10);
    assert!(wait_until(Duration::from_secs(10), || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
    engine.result(q).unwrap().read().status.clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn dead_tunnel_is_reopened() {
    let dir = tempfile::tempdir().unwrap();
    let echo = echo_server().await;
    let server = ssh_server(12, key(9).public_key().clone()).await;
    let (port, path) = proxy(0, server.port).await;
    let (engine, _) = engine(&dir.path().join("known_hosts"));
    engine.connect(tunneled(echo, port, &[]), password(PASSWORD), dev());
    let status = tokio::task::block_in_place(|| outcome(&engine));
    assert!(matches!(status, SessionStatus::Connected(_)), "{status:?}");
    let tab = engine.session("c", Some(1));
    let first = tokio::task::block_in_place(|| run(&engine, &tab, "select 1"));
    assert!(matches!(first, QueryStatus::Ready { .. }), "{first:?}");

    // The network to the SSH server goes away: the session's connection is lost.
    path.abort();
    let _ = path.await;
    let lost = tokio::task::block_in_place(|| run(&engine, &tab, "select 2"));
    let QueryStatus::Failed(e) = lost else {
        panic!("{lost:?}")
    };
    assert_eq!(e.kind, ErrorKind::Connection, "{e:?}");

    // Back again: the next run reconnects through a new tunnel.
    let (_, _path) = proxy(port, server.port).await;
    let again = tokio::task::block_in_place(|| run(&engine, &tab, "select 3"));
    assert!(matches!(again, QueryStatus::Ready { .. }), "{again:?}");
}
