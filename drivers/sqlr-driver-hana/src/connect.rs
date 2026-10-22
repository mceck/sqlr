//! Connection parameters from the form: host, port, tenant, TLS and the redirect pin.

use std::sync::Arc;

use hdbconnect_async::{ConnectParams, ConnectParamsBuilder, ServerCerts};
use sqlr_driver_sdk::{ConnectionConfig, DriverError, DriverManifest, ErrorKind, Secrets};
use tempfile::TempDir;

use crate::tls;

/// Where to connect, after the redirect pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    /// The tenant to ask the server to redirect to; `None` connects to `port` as it is.
    pub dbname: Option<String>,
}

/// With a tenant name, HANA redirects to the address *it* advertises, which is unreachable
/// behind Docker, NAT or an SSH tunnel, and `hdbconnect` offers no hook to change it. Pinning
/// avoids the redirect: connect to the original host on the tenant's own SQL port.
pub fn endpoint(
    host: &str,
    port: u16,
    database: Option<&str>,
    pin: bool,
    tenant_port: Option<u16>,
) -> Result<Endpoint, DriverError> {
    let database = database.map(str::trim).filter(|d| !d.is_empty());
    match (database, pin) {
        (None, _) => Ok(Endpoint {
            host: host.to_string(),
            port,
            dbname: None,
        }),
        (Some(db), false) => Ok(Endpoint {
            host: host.to_string(),
            port,
            dbname: Some(db.to_string()),
        }),
        (Some(_), true) => {
            let port = tenant_port.ok_or_else(|| {
                DriverError::new(
                    ErrorKind::Other,
                    "Pinning the redirect needs the tenant's SQL port (find it in SYSTEMDB: \
                     SELECT DATABASE_NAME, SQL_PORT FROM SYS_DATABASES.M_SERVICES \
                     WHERE SERVICE_NAME = 'indexserver')",
                )
            })?;
            Ok(Endpoint {
                host: host.to_string(),
                port,
                dbname: None,
            })
        }
    }
}

/// Parameters kept for the side connections that cancel; the CA directory they point at
/// lives as long as any clone does.
#[derive(Clone)]
pub struct Params {
    pub params: ConnectParams,
    _ca_dir: Option<Arc<TempDir>>,
}

fn flag(cfg: &ConnectionConfig, manifest: &DriverManifest, key: &str) -> bool {
    cfg.value(manifest, key) == Some("true")
}

pub(crate) fn params(
    cfg: &ConnectionConfig,
    manifest: &DriverManifest,
    secrets: &Secrets,
) -> Result<Params, DriverError> {
    // hdbconnect enables no rustls crypto backend: install one (another may be installed).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let bad = |m: String| DriverError::new(ErrorKind::Other, m);
    let text = |key: &str| {
        cfg.value(manifest, key)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let host = text("host").ok_or_else(|| bad("enter the host".into()))?;
    let port: u16 = text("port")
        .ok_or_else(|| bad("enter the port".into()))?
        .parse()
        .map_err(|_| bad("the port is not a number".into()))?;
    let tenant_port = text("tenant_port")
        .map(|p| {
            p.parse::<u16>()
                .map_err(|_| bad("the tenant port is not a number".into()))
        })
        .transpose()?;
    let user = text("user").ok_or_else(|| bad("enter the user".into()))?;
    let password = secrets.0.get("password").map_or("", String::as_str);
    let at = endpoint(
        host,
        port,
        text("database"),
        flag(cfg, manifest, "pin_redirect"),
        tenant_port,
    )?;

    let mut builder = ConnectParamsBuilder::new()
        .with_hostname(&at.host)
        .with_port(at.port)
        .with_dbuser(user)
        .with_password(password);
    if let Some(db) = &at.dbname {
        builder = builder.with_dbname(db);
    }
    let mut ca_dir = None;
    if flag(cfg, manifest, "tls") {
        builder = if flag(cfg, manifest, "tls_skip_verify") {
            builder.with_tls_without_server_verification(true)
        } else if let Some(ca) = text("tls_ca_file") {
            let dir = Arc::new(tls::pem_to_der_dir(std::path::Path::new(ca))?);
            let certs = ServerCerts::Directory(dir.path().to_string_lossy().into_owned());
            ca_dir = Some(dir);
            builder.with_tls_with(certs)
        } else {
            builder.with_tls_with(ServerCerts::RootCertificates)
        };
    }
    let params = builder
        .build()
        .map_err(|e| bad(format!("invalid connection settings: {e}")))?;
    Ok(Params {
        params,
        _ca_dir: ca_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_pinning_rewrites_host() {
        // No tenant: nothing to redirect, pinning or not.
        assert_eq!(
            endpoint("db.example", 30015, None, true, None).unwrap(),
            Endpoint {
                host: "db.example".into(),
                port: 30015,
                dbname: None
            }
        );
        // A tenant without the pin: hdbconnect follows the server's redirect.
        assert_eq!(
            endpoint("db.example", 30013, Some("HXE"), false, None).unwrap(),
            Endpoint {
                host: "db.example".into(),
                port: 30013,
                dbname: Some("HXE".into())
            }
        );
        // The pin keeps the host the user typed and goes straight to the tenant's port,
        // whatever address the server would advertise.
        assert_eq!(
            endpoint("127.0.0.1", 30013, Some("HXE"), true, Some(39041)).unwrap(),
            Endpoint {
                host: "127.0.0.1".into(),
                port: 39041,
                dbname: None
            }
        );
        // Without the tenant's port the pin cannot work, and says how to find it.
        let e = endpoint("127.0.0.1", 30013, Some("HXE"), true, None).unwrap_err();
        assert!(e.message.contains("SQL port"), "{}", e.message);
        // A blank tenant is no tenant.
        assert_eq!(
            endpoint("h", 1, Some("  "), true, None).unwrap().dbname,
            None
        );
    }
}
