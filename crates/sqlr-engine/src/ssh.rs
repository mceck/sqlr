//! SSH tunnels (§9): the engine connects drivers through a local port forwarded over SSH.
//! Drivers never see SSH; the engine adds its fields to their manifests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use russh::client::{self, Handle};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use sqlr_core::{
    ConnectionConfig, DriverError, DriverManifest, ErrorKind, FieldSpec, FieldType, Secrets,
};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinSet;

use crate::engine::Shared;

/// The SSH host key a connection met, for the connection test's report.
#[derive(Clone, Debug, PartialEq)]
pub struct HostKeyInfo {
    pub host: String,
    /// `SHA256:…`, as OpenSSH prints it.
    pub fingerprint: String,
    /// First use: the key was accepted and recorded now.
    pub new: bool,
}

/// How a session's connect went.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectReport {
    /// SSH setup plus the driver's connect and ping, in milliseconds.
    pub ms: u64,
    /// The SSH server's host key, when a tunnel was used.
    pub host_key: Option<HostKeyInfo>,
    /// The tunnel's local port, when a tunnel was used.
    pub tunnel_port: Option<u16>,
}

/// An open tunnel: a local listener whose connections are forwarded over SSH to the
/// connection's `host:port`. Dropping it closes the listener, its forwards and the SSH
/// session(s).
pub(crate) struct Tunnel {
    /// The local port the driver connects to.
    pub port: u16,
    pub host_key: HostKeyInfo,
    /// Cleared when the forwarding task ends (its SSH session closed).
    alive: Arc<AtomicBool>,
    /// The SSH session to the server that forwards.
    handle: Arc<Handle<Client>>,
    /// Dropped with the tunnel, which stops the forwarding task.
    _stop: oneshot::Sender<()>,
}

impl Tunnel {
    /// Whether the tunnel still forwards: a dead one (its SSH session closed, e.g. by a
    /// network loss) must be replaced, not reused.
    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst) && !self.handle.is_closed()
    }
}

/// The connection's one tunnel, while any session holds it.
pub(crate) type TunnelSlot = tokio::sync::Mutex<Weak<Tunnel>>;

/// Where host keys are checked and recorded.
#[derive(Clone, Default)]
pub(crate) struct KnownHosts {
    /// An OpenSSH `known_hosts` file; `None` keeps keys in `memory` only.
    path: Option<PathBuf>,
    memory: Arc<Mutex<HashMap<(String, u16), PublicKey>>>,
}

/// Engine-wide SSH settings.
#[derive(Default)]
pub(crate) struct SshContext {
    /// `<data dir>/known_hosts`; `None` keeps host keys in memory only.
    pub known_hosts: Mutex<Option<PathBuf>>,
    memory: Arc<Mutex<HashMap<(String, u16), PublicKey>>>,
}

impl SshContext {
    fn known_hosts(&self) -> KnownHosts {
        KnownHosts {
            path: self.known_hosts.lock().clone(),
            memory: self.memory.clone(),
        }
    }
}

/// What checking a host key found.
enum KeyCheck {
    Known,
    New,
    Changed,
    /// No entry, and the connection does not allow learning one during authentication.
    Unknown,
}

impl KnownHosts {
    fn check(
        &self,
        host: &str,
        port: u16,
        key: &PublicKey,
        trust_new: bool,
    ) -> Result<KeyCheck, String> {
        match &self.path {
            None => {
                let mut memory = self.memory.lock();
                match memory.get(&(host.to_string(), port)) {
                    Some(known) if known == key => Ok(KeyCheck::Known),
                    Some(_) => Ok(KeyCheck::Changed),
                    None if trust_new => {
                        memory.insert((host.to_string(), port), key.clone());
                        Ok(KeyCheck::New)
                    }
                    None => Ok(KeyCheck::Unknown),
                }
            }
            Some(path) => {
                let known = if path.exists() {
                    match russh::keys::check_known_hosts_path(host, port, key, path) {
                        Ok(known) => known,
                        Err(russh::keys::Error::KeyChanged { .. }) => {
                            return Ok(KeyCheck::Changed);
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                } else {
                    false
                };
                if known {
                    return Ok(KeyCheck::Known);
                }
                if !trust_new {
                    return Ok(KeyCheck::Unknown);
                }
                russh::keys::known_hosts::learn_known_hosts_path(host, port, key, path)
                    .map_err(|e| e.to_string())?;
                Ok(KeyCheck::New)
            }
        }
    }
}

/// The client side of one SSH hop: checks the host key against `known_hosts`.
struct Client {
    host: String,
    port: u16,
    known: KnownHosts,
    /// Learn an unknown host key instead of refusing it (an explicit per-connection opt-in).
    trust_new: bool,
    /// What the host key check found, for the error or the report.
    seen: Arc<Mutex<Option<Result<HostKeyInfo, String>>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_key else {
            *self.seen.lock() = Some(Err(format!(
                "SSH host certificates are not supported ({})",
                self.host
            )));
            return Ok(false);
        };
        let info = |new| HostKeyInfo {
            host: self.host.clone(),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            new,
        };
        let (seen, ok) = match self.known.check(&self.host, self.port, key, self.trust_new) {
            Ok(KeyCheck::Known) => (Ok(info(false)), true),
            Ok(KeyCheck::New) => (Ok(info(true)), true),
            Ok(KeyCheck::Changed) => (Err(format!("Host key changed for {}", self.host)), false),
            Ok(KeyCheck::Unknown) => (
                Err(format!(
                    "Host key for {} is not known ({}). Verify it and add it to known_hosts, \
                     or enable \"Trust new host keys\" for this connection",
                    self.host,
                    key.fingerprint(HashAlg::Sha256)
                )),
                false,
            ),
            Err(e) => (
                Err(format!(
                    "Could not check the host key of {}: {e}",
                    self.host
                )),
                false,
            ),
        };
        *self.seen.lock() = Some(seen);
        Ok(ok)
    }
}

/// How the client proves who it is (the jump host uses the same).
enum Auth {
    Password(String),
    Key {
        path: String,
        passphrase: Option<String>,
    },
    Agent,
}

/// One SSH server to reach.
struct Hop {
    host: String,
    port: u16,
    user: String,
}

/// `manifest` with the SSH group added when its capabilities include `ssh`.
pub(crate) fn with_ssh_fields(mut manifest: DriverManifest) -> DriverManifest {
    if !manifest.capabilities.ssh {
        return manifest;
    }
    let field =
        |key: &str, label: &str, ty: FieldType, default: Option<&str>, secret: bool| FieldSpec {
            free_form: false,
            key: key.into(),
            label: label.into(),
            ty,
            default: default.map(Into::into),
            required: false,
            secret,
            group: "SSH".into(),
        };
    let auth = FieldType::Choice(vec![
        ("password".into(), "Password".into()),
        ("key".into(), "Key file".into()),
        ("agent".into(), "SSH agent".into()),
    ]);
    let key_file = FieldType::File {
        save: false,
        extensions: Vec::new(),
    };
    let fields = [
        field(
            "ssh_enabled",
            "Use SSH tunnel",
            FieldType::Bool,
            Some("false"),
            false,
        ),
        field("ssh_host", "SSH host", FieldType::Text, None, false),
        field("ssh_port", "SSH port", FieldType::Port, Some("22"), false),
        field("ssh_user", "SSH user", FieldType::Text, None, false),
        field("ssh_auth", "Authentication", auth, Some("password"), false),
        field(
            "ssh_password",
            "SSH password",
            FieldType::Password,
            None,
            true,
        ),
        field("ssh_key_path", "Private key", key_file, None, false),
        field(
            "ssh_key_passphrase",
            "Key passphrase",
            FieldType::Password,
            None,
            true,
        ),
        field(
            "ssh_trust_new_host_keys",
            "Trust new host keys",
            FieldType::Bool,
            Some("false"),
            false,
        ),
        field("ssh_jump_host", "Jump host", FieldType::Text, None, false),
        field(
            "ssh_jump_port",
            "Jump port",
            FieldType::Port,
            Some("22"),
            false,
        ),
        field("ssh_jump_user", "Jump user", FieldType::Text, None, false),
    ];
    for f in fields {
        if !manifest.fields.iter().any(|m| m.key == f.key) {
            manifest.fields.push(f);
        }
    }
    manifest
}

/// Whether `cfg` asks for an SSH tunnel.
pub(crate) fn ssh_enabled(cfg: &ConnectionConfig) -> bool {
    cfg.values.get("ssh_enabled").map(String::as_str) == Some("true")
}

fn config_error(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Other, message)
}

fn value<'a>(cfg: &'a ConnectionConfig, key: &str) -> Option<&'a str> {
    cfg.values
        .get(key)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
}

fn port_of(cfg: &ConnectionConfig, key: &str, default: u16) -> Result<u16, DriverError> {
    match value(cfg, key) {
        None => Ok(default),
        Some(p) => p
            .parse()
            .map_err(|_| config_error(format!("Invalid port \"{p}\" for {key}"))),
    }
}

impl Shared {
    /// The config the driver connects with: `cfg` itself without SSH, else `cfg` pointed
    /// at the connection's tunnel (opened when no session holds one).
    pub(crate) async fn tunnel_for(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
        manifest: &DriverManifest,
        slot: &TunnelSlot,
    ) -> Result<(ConnectionConfig, Option<Arc<Tunnel>>), DriverError> {
        if !ssh_enabled(cfg) {
            return Ok((cfg.clone(), None));
        }
        let mut guard = slot.lock().await;
        let tunnel = match guard.upgrade().filter(|t| t.is_alive()) {
            Some(tunnel) => tunnel,
            None => {
                let target_host = cfg
                    .value(manifest, "host")
                    .unwrap_or("127.0.0.1")
                    .to_string();
                let target_port = cfg
                    .value(manifest, "port")
                    .and_then(|p| p.parse().ok())
                    .or(manifest.default_port)
                    .ok_or_else(|| config_error("The connection has no port to tunnel to"))?;
                let trust_new = cfg.value(manifest, "ssh_trust_new_host_keys") == Some("true");
                let tunnel = Arc::new(
                    open_tunnel(
                        cfg,
                        secrets,
                        target_host,
                        target_port,
                        self.ssh.known_hosts(),
                        trust_new,
                    )
                    .await?,
                );
                *guard = Arc::downgrade(&tunnel);
                tunnel
            }
        };
        let mut via = cfg.clone();
        via.values.insert("host".into(), "127.0.0.1".into());
        via.values.insert("port".into(), tunnel.port.to_string());
        // TLS still verifies the host the user named, not the tunnel's address.
        if let Some(host) = cfg.value(manifest, "host").filter(|h| !h.trim().is_empty()) {
            via.values
                .insert(sqlr_core::TLS_SERVER_NAME.into(), host.trim().to_string());
        }
        Ok((via, Some(tunnel)))
    }
}

/// Connects (through the jump host, when set), authenticates and starts forwarding.
async fn open_tunnel(
    cfg: &ConnectionConfig,
    secrets: &Secrets,
    target_host: String,
    target_port: u16,
    known: KnownHosts,
    trust_new: bool,
) -> Result<Tunnel, DriverError> {
    let ssh = Hop {
        host: value(cfg, "ssh_host")
            .ok_or_else(|| config_error("SSH host is required"))?
            .to_string(),
        port: port_of(cfg, "ssh_port", 22)?,
        user: value(cfg, "ssh_user")
            .ok_or_else(|| config_error("SSH user is required"))?
            .to_string(),
    };
    let secret = |key: &str| secrets.0.get(key).filter(|v| !v.is_empty()).cloned();
    let auth = match value(cfg, "ssh_auth").unwrap_or("password") {
        "key" => Auth::Key {
            path: value(cfg, "ssh_key_path")
                .ok_or_else(|| config_error("SSH private key file is required"))?
                .to_string(),
            passphrase: secret("ssh_key_passphrase"),
        },
        "agent" => Auth::Agent,
        _ => Auth::Password(secret("ssh_password").unwrap_or_default()),
    };
    let jump = match value(cfg, "ssh_jump_host") {
        None => None,
        Some(host) => Some(Hop {
            host: host.to_string(),
            port: port_of(cfg, "ssh_jump_port", 22)?,
            user: value(cfg, "ssh_jump_user").unwrap_or(&ssh.user).to_string(),
        }),
    };
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        nodelay: true,
        ..Default::default()
    });

    let (jump_handle, handle, host_key) = match jump {
        None => {
            let (mut handle, key) = connect_hop(&config, &ssh, &known, None, trust_new).await?;
            authenticate(&mut handle, &ssh, &auth).await?;
            (None, handle, key)
        }
        Some(jump) => {
            let (mut jump_handle, _) = connect_hop(&config, &jump, &known, None, trust_new).await?;
            authenticate(&mut jump_handle, &jump, &auth).await?;
            let channel = jump_handle
                .channel_open_direct_tcpip(ssh.host.clone(), u32::from(ssh.port), "127.0.0.1", 0)
                .await
                .map_err(|e| {
                    DriverError::new(
                        ErrorKind::Connection,
                        format!(
                            "The jump host {} could not reach {}:{}: {e}",
                            jump.host, ssh.host, ssh.port
                        ),
                    )
                })?;
            let (mut handle, key) = connect_hop(
                &config,
                &ssh,
                &known,
                Some(channel.into_stream()),
                trust_new,
            )
            .await?;
            authenticate(&mut handle, &ssh, &auth).await?;
            (Some(jump_handle), handle, key)
        }
    };

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| {
        DriverError::new(ErrorKind::Connection, format!("SSH tunnel listener: {e}"))
    })?;
    let port = listener
        .local_addr()
        .map_err(|e| DriverError::new(ErrorKind::Connection, e.to_string()))?
        .port();
    let (stop, stopped) = oneshot::channel();
    let handle = Arc::new(handle);
    let alive = Arc::new(AtomicBool::new(true));
    tokio::spawn(forward(
        listener,
        handle.clone(),
        jump_handle,
        target_host,
        target_port,
        stopped,
        alive.clone(),
    ));
    Ok(Tunnel {
        port,
        host_key,
        alive,
        handle,
        _stop: stop,
    })
}

/// How often the forwarding task checks that its SSH session is still open.
const LIVENESS_CHECK: Duration = Duration::from_millis(500);

/// Accepts local connections and forwards each over a `direct-tcpip` channel until the
/// tunnel is dropped; then closes the listener, the forwards and the SSH sessions.
async fn forward(
    listener: TcpListener,
    handle: Arc<Handle<Client>>,
    jump: Option<Handle<Client>>,
    host: String,
    port: u16,
    mut stopped: oneshot::Receiver<()>,
    alive: Arc<AtomicBool>,
) {
    let mut forwards = JoinSet::new();
    let mut check = tokio::time::interval(LIVENESS_CHECK);
    loop {
        let accepted = tokio::select! {
            _ = &mut stopped => break,
            _ = check.tick() => {
                if handle.is_closed() {
                    break;
                }
                continue;
            }
            accepted = listener.accept() => accepted,
        };
        let Ok((mut socket, peer)) = accepted else {
            break;
        };
        let handle = handle.clone();
        let host = host.clone();
        forwards.spawn(async move {
            let channel = handle
                .channel_open_direct_tcpip(
                    host.clone(),
                    u32::from(port),
                    peer.ip().to_string(),
                    u32::from(peer.port()),
                )
                .await;
            match channel {
                Ok(channel) => {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                }
                Err(e) => tracing::warn!(%host, port, error = %e, "SSH forward refused"),
            }
        });
        // Finished forwards are reaped as new ones arrive.
        while forwards.try_join_next().is_some() {}
    }
    alive.store(false, Ordering::SeqCst);
    drop(listener);
    forwards.shutdown().await;
    let bye = russh::Disconnect::ByApplication;
    let _ = handle.disconnect(bye, "", "").await;
    if let Some(jump) = jump {
        let _ = jump
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
    }
}

/// Opens the SSH session to `hop` (over `stream` when it is reached through a jump host)
/// and checks its host key.
async fn connect_hop(
    config: &Arc<client::Config>,
    hop: &Hop,
    known: &KnownHosts,
    stream: Option<russh::ChannelStream<client::Msg>>,
    trust_new: bool,
) -> Result<(Handle<Client>, HostKeyInfo), DriverError> {
    let seen = Arc::new(Mutex::new(None));
    let client = Client {
        host: hop.host.clone(),
        port: hop.port,
        known: known.clone(),
        trust_new,
        seen: seen.clone(),
    };
    let connected = match stream {
        None => client::connect(config.clone(), (hop.host.as_str(), hop.port), client).await,
        Some(stream) => client::connect_stream(config.clone(), stream, client).await,
    };
    let seen = seen.lock().take();
    match (connected, seen) {
        (Ok(handle), Some(Ok(info))) => Ok((handle, info)),
        (_, Some(Err(message))) => Err(DriverError::new(ErrorKind::Connection, message)),
        (Err(e), _) => Err(DriverError::new(
            ErrorKind::Connection,
            format!("SSH connection to {}:{} failed: {e}", hop.host, hop.port),
        )),
        (Ok(_), None) => Err(DriverError::new(
            ErrorKind::Protocol,
            format!("{} sent no host key", hop.host),
        )),
    }
}

fn auth_failed(hop: &Hop) -> DriverError {
    DriverError::new(
        ErrorKind::Auth,
        format!("SSH authentication failed for {}@{}", hop.user, hop.host),
    )
}

fn ssh_error(hop: &Hop, e: impl std::fmt::Display) -> DriverError {
    DriverError::new(
        ErrorKind::Connection,
        format!("SSH session with {} failed: {e}", hop.host),
    )
}

/// A boxed agent stream, so the Unix socket and the Windows transports share one type.
type AgentConnection = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// Connects to the SSH agent the platform provides.
async fn connect_agent() -> Result<AgentConnection, String> {
    #[cfg(unix)]
    {
        AgentClient::connect_env()
            .await
            .map(|agent| agent.dynamic())
            .map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        // Windows OpenSSH serves the agent through this named pipe; PuTTY users run
        // Pageant instead, so fall back to it.
        match AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
            Ok(agent) => Ok(agent.dynamic()),
            Err(pipe) => AgentClient::connect_pageant()
                .await
                .map(|agent| agent.dynamic())
                .map_err(|pageant| format!("OpenSSH agent pipe: {pipe}; Pageant: {pageant}")),
        }
    }
}

async fn authenticate(
    handle: &mut Handle<Client>,
    hop: &Hop,
    auth: &Auth,
) -> Result<(), DriverError> {
    let ok = match auth {
        Auth::Password(password) => handle
            .authenticate_password(hop.user.clone(), password.clone())
            .await
            .map_err(|e| ssh_error(hop, e))?
            .success(),
        Auth::Key { path, passphrase } => {
            let key = russh::keys::load_secret_key(path, passphrase.as_deref()).map_err(|e| {
                DriverError::new(
                    ErrorKind::Auth,
                    format!("Could not read the SSH key {path}: {e}"),
                )
            })?;
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(|e| ssh_error(hop, e))?
                .flatten();
            handle
                .authenticate_publickey(
                    hop.user.clone(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .map_err(|e| ssh_error(hop, e))?
                .success()
        }
        Auth::Agent => {
            let mut agent = connect_agent().await.map_err(|e| {
                DriverError::new(ErrorKind::Auth, format!("SSH agent unavailable: {e}"))
            })?;
            let identities = agent
                .request_identities()
                .await
                .map_err(|e| DriverError::new(ErrorKind::Auth, format!("SSH agent: {e}")))?;
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(|e| ssh_error(hop, e))?
                .flatten();
            let mut ok = false;
            for identity in identities {
                let key = identity.public_key().into_owned();
                let result = handle
                    .authenticate_publickey_with(hop.user.clone(), key, hash, &mut agent)
                    .await
                    .map_err(|e| ssh_error(hop, e))?;
                if result.success() {
                    ok = true;
                    break;
                }
            }
            ok
        }
    };
    if ok { Ok(()) } else { Err(auth_failed(hop)) }
}
