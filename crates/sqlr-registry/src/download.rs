//! Fetching bytes and files over https, http (tests) and `file://` (mirrors, tests).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::RegistryError;

/// The most an index may weigh.
const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
/// The most one artifact may weigh.
pub(crate) const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct Fetcher {
    client: reqwest::Client,
    /// Set by the redirect policy when it refused a hop to another host, keyed by the
    /// request's original URL so concurrent downloads do not report each other's refusal.
    refused: Arc<parking_lot::Mutex<HashMap<String, String>>>,
}

fn tls_config() -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

impl Fetcher {
    pub fn new() -> Self {
        // reqwest needs a process-wide provider even for plain HTTP; another crate may have
        // installed it already.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let refused = Arc::new(parking_lot::Mutex::new(HashMap::new()));
        let seen = refused.clone();
        // A download may follow redirects inside its own host only (and from GitHub to its
        // release file storage): a registry that bounces elsewhere is refused, not trusted.
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            let from = attempt.previous().first().cloned();
            if attempt.previous().len() > 5 {
                attempt.error("too many redirects")
            } else if from
                .as_ref()
                .is_some_and(|from| redirect_allowed(from, attempt.url()))
            {
                attempt.follow()
            } else {
                if let Some(from) = from {
                    seen.lock().insert(
                        from.to_string(),
                        attempt.url().host_str().unwrap_or("").to_string(),
                    );
                }
                attempt.error("redirect to another host")
            }
        });
        let client = reqwest::Client::builder()
            .user_agent(concat!("sqlr/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(15))
            .redirect(policy)
            .tls_backend_preconfigured(tls_config())
            .build()
            .expect("an http client builds");
        Self { client, refused }
    }

    fn error(&self, url: &str, e: impl std::fmt::Display) -> RegistryError {
        if let Some(to) = self.refused.lock().remove(url) {
            return RegistryError::RedirectToOtherHost {
                url: url.to_string(),
                to,
            };
        }
        RegistryError::Fetch {
            url: url.to_string(),
            reason: e.to_string(),
        }
    }

    /// A small text or binary resource (the index and its signature).
    pub async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, RegistryError> {
        if let Some(path) = file_path(url) {
            let bytes = read_bounded_file(&path, MAX_INDEX_BYTES as u64)
                .await
                .map_err(|e| RegistryError::Fetch {
                    url: url.to_string(),
                    reason: e.to_string(),
                })?;
            return Ok(bytes);
        }
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| self.error(url, e))?
            .error_for_status()
            .map_err(|e| self.error(url, e))?;
        let mut out = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| self.error(url, e))? {
            if chunk.len() > MAX_INDEX_BYTES - out.len() {
                return Err(RegistryError::Fetch {
                    url: url.to_string(),
                    reason: "the response is too large".into(),
                });
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    /// Downloads `url` to `dest`, calling `progress(done, total)`. A body shorter than its
    /// announced length is an error; `dest` is left for the caller to delete.
    pub async fn download_to(
        &self,
        url: &str,
        dest: &Path,
        progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
    ) -> Result<(), RegistryError> {
        if let Some(path) = file_path(url) {
            let error = |e: std::io::Error| RegistryError::Fetch {
                url: url.to_string(),
                reason: e.to_string(),
            };
            let mut source = tokio::fs::File::open(&path).await.map_err(error)?;
            let total = source.metadata().await.map_err(error)?.len();
            if total > MAX_ARTIFACT_BYTES {
                return Err(error(size_limit_error()));
            }
            let mut file = tokio::fs::File::create(dest).await.map_err(error)?;
            let mut done = 0u64;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let read = source.read(&mut buffer).await.map_err(error)?;
                if read == 0 {
                    break;
                }
                if read as u64 > MAX_ARTIFACT_BYTES - done {
                    return Err(error(size_limit_error()));
                }
                file.write_all(&buffer[..read]).await.map_err(error)?;
                done += read as u64;
                progress(done, Some(total));
            }
            file.flush().await.map_err(error)?;
            if done != total {
                return Err(RegistryError::Fetch {
                    url: url.to_string(),
                    reason: "the local file changed size during the download".into(),
                });
            }
            return Ok(());
        }
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| self.error(url, e))?
            .error_for_status()
            .map_err(|e| self.error(url, e))?;
        let total = response.content_length();
        if total.is_some_and(|t| t > MAX_ARTIFACT_BYTES) {
            return Err(RegistryError::Fetch {
                url: url.to_string(),
                reason: "the file is larger than any driver".into(),
            });
        }
        let mut file = tokio::fs::File::create(dest).await?;
        let mut done = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(|e| self.error(url, e))? {
            done += chunk.len() as u64;
            if done > MAX_ARTIFACT_BYTES {
                return Err(RegistryError::Fetch {
                    url: url.to_string(),
                    reason: "the download passed the size limit".into(),
                });
            }
            file.write_all(&chunk).await?;
            progress(done, total);
        }
        file.flush().await?;
        if let Some(total) = total.filter(|&t| t != done) {
            return Err(RegistryError::Fetch {
                url: url.to_string(),
                reason: format!("the download stopped after {done} of {total} bytes"),
            });
        }
        Ok(())
    }
}

fn size_limit_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "the file exceeds the size limit",
    )
}

/// Checks the opened file, then also bounds the read in case it grows or its length is
/// unavailable. The extra byte distinguishes a file at the limit from an oversized one.
pub(crate) async fn read_bounded_file(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let file = tokio::fs::File::open(path).await?;
    if file.metadata().await?.len() > limit {
        return Err(size_limit_error());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > limit {
        return Err(size_limit_error());
    }
    Ok(bytes)
}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a download that started at `from` may continue at `to`: the same host, or a
/// GitHub release file (github.com) served by GitHub's file storage over https.
fn redirect_allowed(from: &reqwest::Url, to: &reqwest::Url) -> bool {
    // Same host, scheme and port: never allow a redirect to downgrade HTTPS to HTTP or to
    // move the download to another service on the same host.
    if from.scheme() == to.scheme()
        && from.host_str() == to.host_str()
        && from.port_or_known_default() == to.port_or_known_default()
    {
        return true;
    }
    from.scheme() == "https"
        && from.host_str() == Some("github.com")
        && to.scheme() == "https"
        && to
            .host_str()
            .is_some_and(|host| host.ends_with(".githubusercontent.com"))
}

fn file_path(url: &str) -> Option<std::path::PathBuf> {
    url.strip_prefix("file://").map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(from: &str, to: &str) -> bool {
        redirect_allowed(&from.parse().unwrap(), &to.parse().unwrap())
    }

    #[test]
    fn redirects_stay_on_their_host_or_github_release_storage() {
        assert!(allowed("http://127.0.0.1:1/a", "http://127.0.0.1:1/b"));
        assert!(!allowed("http://127.0.0.1:1/a", "http://localhost:1/b"));
        // Never downgrade HTTPS or move to another port on the same host.
        assert!(!allowed("https://example.com/a", "http://example.com/b"));
        assert!(!allowed(
            "https://example.com/a",
            "https://example.com:8443/b"
        ));
        let release = "https://github.com/mceck/sqlr/releases/download/v1.0.0/x.tar.gz";
        assert!(allowed(
            release,
            "https://release-assets.githubusercontent.com/github-production-release-asset/1"
        ));
        assert!(allowed(release, "https://objects.githubusercontent.com/x"));
        assert!(!allowed(release, "http://objects.githubusercontent.com/x"));
        assert!(!allowed(
            release,
            "https://githubusercontent.com.evil.example/x"
        ));
        assert!(!allowed(release, "https://example.com/x"));
        assert!(!allowed(
            "https://example.com/x",
            "https://objects.githubusercontent.com/x"
        ));
    }

    #[test]
    fn a_refused_redirect_is_attributed_to_its_own_url() {
        let fetcher = Fetcher::new();
        fetcher
            .refused
            .lock()
            .insert("https://a.example/x".into(), "evil.example".into());
        assert!(matches!(
            fetcher.error("https://b.example/x", "500"),
            RegistryError::Fetch { .. }
        ));
        assert!(matches!(
            fetcher.error("https://a.example/x", "500"),
            RegistryError::RedirectToOtherHost { .. }
        ));
    }
}
