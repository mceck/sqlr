//! Elasticsearch driver for sqlr: REST through `reqwest` on rustls with the ring provider.
//!
//! The query language is the Kibana console (`sqlr_core::parse_es_console`): HTTP requests
//! with JSON bodies, `ESQL …` (8.11+) and `SQL …`. Searches page with a point in time and
//! `search_after`; ES|QL and SQL results are tabular.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod browse;
mod client;
mod errors;
mod manifest;
mod query;
mod tabular;
mod write;

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value as Json;
use sqlr_core::{
    CancelHandle, Connection, ConnectionConfig, DocExecution, DocTarget, DocValue, DocumentConn,
    Driver, DriverError, DriverManifest, ErrorKind, Node, NodePath, QueryLanguage, Secrets,
    ServerInfo, StatementClass, read_only_error,
};

use crate::client::{Auth, Body, EsClient};

pub struct ElasticDriver {
    manifest: DriverManifest,
}

impl ElasticDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            manifest: manifest::manifest(),
        })
    }
}

#[async_trait::async_trait]
impl Driver for ElasticDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let m = &self.manifest;
        let value = |key: &str| cfg.value(m, key).map(str::trim).filter(|v| !v.is_empty());
        let secret = |key: &str| secrets.0.get(key).filter(|v| !v.is_empty()).cloned();
        let invalid = |message: String| DriverError::new(ErrorKind::Other, message);
        let scheme = value("scheme").unwrap_or("http");
        if !matches!(scheme, "http" | "https") {
            return Err(invalid(format!("unknown protocol `{scheme}`")));
        }
        let host =
            value("host").ok_or_else(|| invalid("missing connection field `host`".into()))?;
        let port: u16 = value("port")
            .unwrap_or("9200")
            .parse()
            .map_err(|_| invalid("invalid port".into()))?;
        let auth = match (secret("api_key"), value("user")) {
            (Some(key), _) => Auth::ApiKey(key),
            (None, Some(user)) => Auth::Basic {
                user: user.to_string(),
                password: secret("password").unwrap_or_default(),
            },
            (None, None) => Auth::None,
        };
        let client = EsClient::open(
            scheme,
            host,
            port,
            auth,
            value("tls_ca"),
            value("tls_skip_verify") == Some("true"),
        )
        .await?;
        Ok(Arc::new(EsConnection {
            client: Arc::new(client),
            read_only: AtomicBool::new(value("read_only") == Some("true")),
            keep_alive: value("pit_keep_alive").unwrap_or("5m").to_string(),
        }))
    }
}

pub struct EsConnection {
    client: Arc<EsClient>,
    read_only: AtomicBool,
    /// The point-in-time keep-alive of searches (`5m`).
    keep_alive: String,
}

impl EsConnection {
    fn is_read_only(&self) -> bool {
        self.read_only.load(Ordering::SeqCst)
    }

    fn check_writable(&self) -> Result<(), DriverError> {
        if self.is_read_only() {
            Err(read_only_error())
        } else {
            Ok(())
        }
    }
}

/// Cancels the tagged request in flight through the task management API.
struct EsCancel(Arc<EsClient>);

#[async_trait::async_trait]
impl CancelHandle for EsCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        let Some(tag) = self.0.running.lock().clone() else {
            return Ok(());
        };
        let tasks = self
            .0
            .ok("GET", "/_tasks?detailed=true&group_by=none", Body::None)
            .await?;
        for task in tasks["tasks"].as_array().into_iter().flatten() {
            let ours = task["headers"]["X-Opaque-Id"].as_str() == Some(tag.as_str());
            if ours && task["cancellable"] == Json::Bool(true) {
                let id = format!(
                    "{}:{}",
                    task["node"].as_str().unwrap_or_default(),
                    task["id"]
                );
                self.0
                    .ok("POST", &format!("/_tasks/{id}/_cancel"), Body::None)
                    .await?;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Connection for EsConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let root = self.client.ok("GET", "/", Body::None).await?;
        Ok(ServerInfo {
            product: "Elasticsearch".into(),
            version: root["version"]["number"]
                .as_str()
                .unwrap_or("?")
                .to_string(),
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        self.browse_path(path).await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(EsCancel(self.client.clone()))
    }

    fn document(&self) -> Option<&dyn DocumentConn> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl DocumentConn for EsConnection {
    fn language(&self) -> QueryLanguage {
        QueryLanguage::EsConsole
    }

    async fn run(&self, _database: Option<&str>, text: &str) -> Result<DocExecution, DriverError> {
        let req = query::parse_one(text)?;
        self.run_request(req).await
    }

    fn split(&self, text: &str) -> Vec<Range<usize>> {
        QueryLanguage::EsConsole.split(text)
    }

    async fn replace_doc(
        &self,
        target: &DocTarget,
        id: &DocValue,
        doc: &DocValue,
    ) -> Result<u64, DriverError> {
        self.check_writable()?;
        self.replace(target, id, doc).await
    }

    async fn insert_doc(
        &self,
        target: &DocTarget,
        doc: &DocValue,
    ) -> Result<DocValue, DriverError> {
        self.check_writable()?;
        self.insert(target, doc).await
    }

    async fn delete_doc(&self, target: &DocTarget, id: &DocValue) -> Result<u64, DriverError> {
        self.check_writable()?;
        self.delete(target, id).await
    }

    async fn indexes(&self, target: &DocTarget) -> Result<Vec<DocValue>, DriverError> {
        self.mappings(target).await
    }

    /// Without a `size`: the search pages through every hit, `page` at a time as the
    /// engine fetches.
    fn open_target_text(&self, target: &DocTarget, _page: usize) -> String {
        format!(
            "GET {}/_search\n{{\n  \"query\": {{\"match_all\": {{}}}}\n}}",
            target.collection
        )
    }

    /// `[indices|aliases|data_streams, name]`.
    fn node_target(&self, path: &sqlr_core::NodePath) -> Option<DocTarget> {
        match path.segments() {
            [_, name] => Some(DocTarget {
                database: None,
                collection: name.clone(),
            }),
            _ => None,
        }
    }

    fn classify(&self, text: &str) -> StatementClass {
        query::parse_one(text).map_or(StatementClass::Other, |r| query::classify(&r))
    }

    fn set_read_only(&self, on: bool) {
        self.read_only.store(on, Ordering::SeqCst);
    }
}
