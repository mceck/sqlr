//! The REST client: TLS on rustls with the ring provider, basic or API-key auth, the
//! server version, and request tagging for cancellation.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use serde_json::Value;
use sqlr_core::{DriverError, ErrorKind};

use crate::errors::{http_error, status_error};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Accepts any server certificate ("skip verification"); signatures are still checked.
#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_config(ca: Option<&str>, skip_verify: bool) -> Result<rustls::ClientConfig, DriverError> {
    let tls_err = |m: String| DriverError::new(ErrorKind::Connection, m);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| tls_err(format!("TLS setup failed: {e}")))?;
    if skip_verify {
        return Ok(builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
            .with_no_client_auth());
    }
    let mut roots = RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    if let Some(path) = ca {
        let certs = CertificateDer::pem_file_iter(path)
            .map_err(|e| tls_err(format!("cannot read the CA certificate {path}: {e}")))?;
        for cert in certs {
            let cert = cert.map_err(|e| tls_err(format!("invalid CA certificate {path}: {e}")))?;
            roots
                .add(cert)
                .map_err(|e| tls_err(format!("invalid CA certificate {path}: {e}")))?;
        }
    }
    Ok(builder.with_root_certificates(roots).with_no_client_auth())
}

#[derive(Clone)]
pub(crate) enum Auth {
    None,
    Basic { user: String, password: String },
    ApiKey(String),
}

/// A request body: none, one JSON value, or NDJSON lines (`_bulk`, `_msearch`).
pub(crate) enum Body {
    None,
    Json(Value),
    NdJson(Vec<Value>),
}

pub(crate) struct Response {
    pub status: u16,
    /// The body as JSON, `Null` when it is not JSON.
    pub json: Value,
    pub text: String,
}

impl Response {
    pub(crate) fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub(crate) fn into_result(self) -> Result<Value, DriverError> {
        if self.is_success() {
            Ok(self.json)
        } else {
            Err(status_error(self.status, &self.json, &self.text))
        }
    }
}

pub(crate) struct EsClient {
    http: reqwest::Client,
    base: String,
    auth: Auth,
    /// `(major, minor)` of the server.
    pub(crate) version: (u32, u32),
    pub(crate) version_text: String,
    /// The `X-Opaque-Id` of the request in flight, for cancellation.
    pub(crate) running: Arc<Mutex<Option<String>>>,
}

impl EsClient {
    /// Builds the client and reads the server version from `GET /`.
    pub(crate) async fn open(
        scheme: &str,
        host: &str,
        port: u16,
        auth: Auth,
        ca: Option<&str>,
        skip_verify: bool,
    ) -> Result<Self, DriverError> {
        // reqwest sets up rustls even for plain HTTP and needs a process-wide provider;
        // another crate may have installed it already.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut builder = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent("sqlr");
        if scheme == "https" {
            builder = builder.tls_backend_preconfigured(tls_config(ca, skip_verify)?);
        }
        let http = builder
            .build()
            .map_err(|e| DriverError::new(ErrorKind::Connection, e.to_string()))?;
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        let mut client = Self {
            http,
            base: format!("{scheme}://{host}:{port}"),
            auth,
            version: (0, 0),
            version_text: String::new(),
            running: Arc::new(Mutex::new(None)),
        };
        let root = client
            .send("GET", "/", Body::None, None)
            .await?
            .into_result()?;
        let number = root["version"]["number"]
            .as_str()
            .unwrap_or("0.0")
            .to_string();
        if root["version"]["distribution"].as_str() == Some("opensearch") {
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                format!("this is OpenSearch {number}, not Elasticsearch"),
            ));
        }
        let mut parts = number.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        client.version = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
        client.version_text = number;
        Ok(client)
    }

    pub(crate) fn at_least(&self, major: u32, minor: u32) -> bool {
        self.version >= (major, minor)
    }

    /// Sends one request; `tag` becomes its `X-Opaque-Id`.
    pub(crate) async fn send(
        &self,
        method: &str,
        path: &str,
        body: Body,
        tag: Option<&str>,
    ) -> Result<Response, DriverError> {
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| DriverError::new(ErrorKind::Syntax, format!("invalid method {method}")))?;
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        req = match &self.auth {
            Auth::None => req,
            Auth::Basic { user, password } => req.basic_auth(user, Some(password)),
            Auth::ApiKey(key) => req.header("Authorization", format!("ApiKey {key}")),
        };
        if let Some(tag) = tag {
            req = req.header("X-Opaque-Id", tag);
        }
        req = match body {
            Body::None => req,
            Body::Json(v) => req
                .header("Content-Type", "application/json")
                .body(serde_json::to_vec(&v).expect("JSON serializes")),
            Body::NdJson(lines) => {
                let mut out = Vec::new();
                for line in lines {
                    serde_json::to_writer(&mut out, &line).expect("JSON serializes");
                    out.push(b'\n');
                }
                req.header("Content-Type", "application/x-ndjson").body(out)
            }
        };
        let resp = req.send().await.map_err(http_error)?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(http_error)?;
        let json = serde_json::from_str(&text).unwrap_or(Value::Null);
        Ok(Response { status, json, text })
    }

    /// Sends a request and fails on a non-2xx status.
    pub(crate) async fn ok(
        &self,
        method: &str,
        path: &str,
        body: Body,
    ) -> Result<Value, DriverError> {
        self.send(method, path, body, None).await?.into_result()
    }
}

/// Percent-encodes one path segment (index names, document ids).
pub(crate) fn segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_encoded() {
        assert_eq!(segment("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(segment("logs-2024.01"), "logs-2024.01");
        assert_eq!(segment("è"), "%C3%A8");
    }

    #[test]
    fn tls_configs_build_with_ring() {
        assert!(tls_config(None, true).is_ok());
        assert!(tls_config(None, false).is_ok());
        assert!(tls_config(Some("/no/such/ca.pem"), false).is_err());
    }
}
