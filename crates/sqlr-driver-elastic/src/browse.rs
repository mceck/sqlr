//! The explorer tree: Indices (health, documents, size), Aliases, Data streams.
//! Hidden indices and aliases (a leading `.`) are left out.

use std::collections::BTreeMap;

use serde_json::Value as Json;
use sqlr_core::{DriverError, Node, NodeKind, NodePath};

use crate::client::Body;

const FOLDERS: [(&str, &str); 3] = [
    ("indices", "Indices"),
    ("aliases", "Aliases"),
    ("data_streams", "Data streams"),
];

fn node(path: &NodePath, name: &str, kind: NodeKind, detail: Option<String>) -> Node {
    Node {
        path: path.child(name),
        name: name.to_string(),
        kind,
        has_children: false,
        detail,
    }
}

impl crate::EsConnection {
    pub(crate) async fn browse_path(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        let client = &self.client;
        let segs: Vec<&str> = path.segments().iter().map(String::as_str).collect();
        match segs.as_slice() {
            [] => Ok(FOLDERS
                .iter()
                // Data streams exist from 7.9 on.
                .filter(|(seg, _)| *seg != "data_streams" || client.at_least(7, 9))
                .map(|(seg, label)| Node {
                    path: path.child(seg),
                    name: label.to_string(),
                    kind: NodeKind::Folder,
                    has_children: true,
                    detail: None,
                })
                .collect()),
            ["indices"] => {
                let list = client
                    .ok(
                        "GET",
                        "/_cat/indices?format=json&h=index,health,docs.count,store.size&s=index&expand_wildcards=open",
                        Body::None,
                    )
                    .await?;
                Ok(list
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|ix| {
                        let name = ix["index"].as_str()?;
                        if name.starts_with('.') {
                            return None;
                        }
                        let text = |k: &str| ix[k].as_str().unwrap_or("?").to_string();
                        let detail = format!(
                            "{} · {} docs · {}",
                            text("health"),
                            text("docs.count"),
                            text("store.size")
                        );
                        Some(node(path, name, NodeKind::Table, Some(detail)))
                    })
                    .collect())
            }
            ["aliases"] => {
                let list = client
                    .ok("GET", "/_cat/aliases?format=json&h=alias,index", Body::None)
                    .await?;
                let mut by_alias: BTreeMap<String, Vec<String>> = BTreeMap::new();
                for a in list.as_array().into_iter().flatten() {
                    if let (Some(alias), Some(index)) = (a["alias"].as_str(), a["index"].as_str())
                        && !alias.starts_with('.')
                    {
                        by_alias.entry(alias.into()).or_default().push(index.into());
                    }
                }
                Ok(by_alias
                    .into_iter()
                    .map(|(alias, mut indices)| {
                        indices.sort();
                        node(
                            path,
                            &alias,
                            NodeKind::View,
                            Some(format!("→ {}", indices.join(", "))),
                        )
                    })
                    .collect())
            }
            ["data_streams"] => {
                let list = client.ok("GET", "/_data_stream", Body::None).await?;
                let mut nodes: Vec<Node> = list["data_streams"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|ds| {
                        let name = ds["name"].as_str()?;
                        if name.starts_with('.') || ds["hidden"] == Json::Bool(true) {
                            return None;
                        }
                        let n = ds["indices"].as_array().map_or(0, Vec::len);
                        Some(node(
                            path,
                            name,
                            NodeKind::Table,
                            Some(format!("{n} backing indices")),
                        ))
                    })
                    .collect();
                nodes.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(nodes)
            }
            _ => Ok(Vec::new()),
        }
    }
}
