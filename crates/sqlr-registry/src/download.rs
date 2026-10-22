//! Fetching bytes and files over https, http (tests) and `file://` (mirrors, tests).

use std::path::Path;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;

use crate::RegistryError;

/// The most an index may weigh.
const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
/// The most one artifact may weigh.
pub(crate) const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct Fetcher {
    client: reqwest::Client,
    /// Set by the redirect policy when it refused a hop to another host.
    refused: Arc<std::sync::Mutex<Option<String>>>,
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
        let refused = Arc::new(std::sync::Mutex::new(None));
        let seen = refused.clone();
        // A download may follow redirects inside its own host only: a registry that
        // bounces to another host is refused rather than trusted.
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            let from = attempt
                .previous()
                .first()
                .and_then(|u| u.host_str().map(str::to_string));
            let to = attempt.url().host_str().map(str::to_string);
            if attempt.previous().len() > 5 {
                attempt.error("too many redirects")
            } else if from == to {
                attempt.follow()
            } else {
                *seen.lock().expect("lock") = to;
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
        if let Some(to) = self.refused.lock().expect("lock").take() {
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
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| RegistryError::Fetch {
                    url: url.to_string(),
                    reason: e.to_string(),
                })?;
            if bytes.len() > MAX_INDEX_BYTES {
                return Err(RegistryError::Fetch {
                    url: url.to_string(),
                    reason: "the file is too large".into(),
                });
            }
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
            out.extend_from_slice(&chunk);
            if out.len() > MAX_INDEX_BYTES {
                return Err(RegistryError::Fetch {
                    url: url.to_string(),
                    reason: "the response is too large".into(),
                });
            }
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
            let len = tokio::fs::copy(&path, dest)
                .await
                .map_err(|e| RegistryError::Fetch {
                    url: url.to_string(),
                    reason: e.to_string(),
                })?;
            progress(len, Some(len));
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

impl Default for Fetcher {
    fn default() -> Self {
        Self::new()
    }
}

fn file_path(url: &str) -> Option<std::path::PathBuf> {
    url.strip_prefix("file://").map(std::path::PathBuf::from)
}
