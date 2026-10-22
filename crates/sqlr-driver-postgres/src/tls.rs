//! TLS through rustls on the `ring` provider: `ssl_mode` with libpq's meaning, a custom root
//! certificate and an optional client certificate.

use std::sync::{Arc, Once};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore,
    SignatureScheme,
};
use sqlr_core::{DriverError, ErrorKind};
use tokio_postgres::config::SslMode as WireSslMode;
use tokio_postgres_rustls::MakeRustlsConnect;

/// The `ssl_mode` connection field, as libpq's `sslmode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SslMode {
    Disable,
    /// TLS when the server offers it, plain otherwise.
    Prefer,
    /// TLS without checking the certificate (unless a root certificate is given).
    Require,
    /// The certificate chains to a trusted root; its host name is not checked.
    VerifyCa,
    /// The certificate chains to a trusted root and names the host.
    VerifyFull,
}

impl SslMode {
    pub(crate) fn parse(value: &str) -> Result<Self, DriverError> {
        Ok(match value {
            "disable" => Self::Disable,
            "prefer" | "" => Self::Prefer,
            "require" => Self::Require,
            "verify-ca" => Self::VerifyCa,
            "verify-full" => Self::VerifyFull,
            other => {
                return Err(DriverError::new(
                    ErrorKind::Other,
                    format!("unknown SSL mode `{other}`"),
                ));
            }
        })
    }

    /// What tokio-postgres negotiates: it refuses a plain connection only under `Require`.
    pub(crate) fn wire(self) -> WireSslMode {
        match self {
            Self::Disable => WireSslMode::Disable,
            Self::Prefer => WireSslMode::Prefer,
            Self::Require | Self::VerifyCa | Self::VerifyFull => WireSslMode::Require,
        }
    }
}

/// Paths of the PEM files in the TLS group.
#[derive(Clone, Debug, Default)]
pub(crate) struct TlsFiles {
    pub(crate) root_cert: Option<String>,
    pub(crate) cert: Option<String>,
    pub(crate) key: Option<String>,
}

fn provider() -> Arc<CryptoProvider> {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        // Another crate may have installed it first; either way ring is the default.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    Arc::new(rustls::crypto::ring::default_provider())
}

fn tls_error(message: String) -> DriverError {
    DriverError::new(ErrorKind::Other, message)
}

/// The connector for `mode`. Reads the certificate files and, for the verifying modes
/// without a root certificate, the system roots: call it off the async threads.
pub(crate) fn connector(mode: SslMode, files: &TlsFiles) -> Result<MakeRustlsConnect, DriverError> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| tls_error(format!("TLS setup failed: {e}")))?;

    let verifier: Arc<dyn ServerCertVerifier> = match (mode, &files.root_cert) {
        (SslMode::Disable, _) | (SslMode::Prefer | SslMode::Require, None) => {
            Arc::new(AcceptAnyCertificate(provider.clone()))
        }
        (SslMode::Prefer | SslMode::Require | SslMode::VerifyCa, root) => Arc::new(ChainOnly(
            webpki_verifier(root.as_deref(), provider.clone())?,
        )),
        (SslMode::VerifyFull, root) => webpki_verifier(root.as_deref(), provider.clone())?,
    };
    let builder = builder
        .dangerous()
        .with_custom_certificate_verifier(verifier);

    let config = match (&files.cert, &files.key) {
        (None, None) => builder.with_no_client_auth(),
        (Some(cert), Some(key)) => {
            let chain = read_certificates(cert, "client certificate")?;
            let key = PrivateKeyDer::from_pem_file(key)
                .map_err(|e| tls_error(format!("cannot read TLS client key `{key}`: {e}")))?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|e| tls_error(format!("invalid TLS client certificate: {e}")))?
        }
        _ => {
            return Err(tls_error(
                "the TLS client certificate and key must be given together".into(),
            ));
        }
    };
    Ok(MakeRustlsConnect::new(config))
}

fn read_certificates(path: &str, what: &str) -> Result<Vec<CertificateDer<'static>>, DriverError> {
    let unreadable = |e: rustls::pki_types::pem::Error| {
        tls_error(format!("cannot read TLS {what} `{path}`: {e}"))
    };
    let certs = CertificateDer::pem_file_iter(path)
        .map_err(unreadable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unreadable)?;
    if certs.is_empty() {
        return Err(tls_error(format!(
            "TLS {what} `{path}` holds no certificate"
        )));
    }
    Ok(certs)
}

/// Roots from `root_cert`, or the system's.
fn webpki_verifier(
    root_cert: Option<&str>,
    provider: Arc<CryptoProvider>,
) -> Result<Arc<WebPkiServerVerifier>, DriverError> {
    let mut roots = RootCertStore::empty();
    match root_cert {
        Some(path) => {
            let (added, _) =
                roots.add_parsable_certificates(read_certificates(path, "root certificate")?);
            if added == 0 {
                return Err(tls_error(format!(
                    "TLS root certificate `{path}` holds no usable certificate"
                )));
            }
        }
        None => {
            let native = rustls_native_certs::load_native_certs();
            for e in &native.errors {
                tracing::warn!("system root certificates: {e}");
            }
            roots.add_parsable_certificates(native.certs);
        }
    }
    WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
        .build()
        .map_err(|e| tls_error(format!("TLS setup failed: {e}")))
}

/// Encryption without authentication (`prefer`/`require` without a root certificate). The
/// handshake signatures are still checked against the presented certificate.
#[derive(Debug)]
struct AcceptAnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
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
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
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

/// `verify-ca`: the chain must reach a trusted root; the host name is not compared. The
/// webpki verifier checks the chain before the name, so a name mismatch means the chain
/// passed.
#[derive(Debug)]
struct ChainOnly(Arc<WebPkiServerVerifier>);

impl ServerCertVerifier for ChainOnly {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            Err(TlsError::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssl_modes_parse_and_map_to_the_wire() {
        assert_eq!(SslMode::parse("").unwrap(), SslMode::Prefer);
        assert_eq!(
            SslMode::parse("verify-ca").unwrap().wire(),
            WireSslMode::Require
        );
        assert_eq!(
            SslMode::parse("prefer").unwrap().wire(),
            WireSslMode::Prefer
        );
        assert_eq!(
            SslMode::parse("disable").unwrap().wire(),
            WireSslMode::Disable
        );
        let err = SslMode::parse("allow").unwrap_err();
        assert!(err.message.contains("allow"), "{err:?}");
    }

    #[test]
    fn client_certificate_needs_its_key() {
        let files = TlsFiles {
            cert: Some("/x.pem".into()),
            ..Default::default()
        };
        let err = connector(SslMode::Require, &files).err().unwrap();
        assert!(err.message.contains("together"), "{err:?}");
        assert!(connector(SslMode::Require, &TlsFiles::default()).is_ok());
    }
}
