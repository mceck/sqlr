//! Running console requests: searches paged with a point in time, ES|QL and SQL, and
//! plain requests; plus statement classification.
//!
//! `_search` (GET or POST, no `scroll`) without a `size` pages through every hit with a
//! point in time and `search_after`, a page (the engine's page size) at a time. An explicit
//! `size` caps the hits as in Elasticsearch: up to 10 000 it is one plain request, beyond
//! that the point in time pages until the cap. A body with `from`, `search_after` or `pit`,
//! or a server older than 7.12, runs one plain request. A body with aggregations or
//! `size: 0` returns the response itself. Hits come back as `{_index, _id, …_source}`.

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value as Json, json};
use sqlr_core::{
    DocChunk, DocExecution, DocStream, DocValue, DriverError, ErrorKind, EsRequest, StatementClass,
    new_object_id, parse_es_console, read_only_error,
};

use crate::client::{Body, EsClient};
use crate::tabular::EsRows;
use crate::write::written_of;

/// Plain JSON as a document value (no Extended JSON decoding: `$` keys are just keys).
pub(crate) fn json_to_doc(v: &Json) -> DocValue {
    match v {
        Json::Null => DocValue::Null,
        Json::Bool(b) => DocValue::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => match i32::try_from(i) {
                Ok(i) => DocValue::Int32(i),
                Err(_) => DocValue::Int64(i),
            },
            None => DocValue::Double(n.as_f64().unwrap_or(f64::NAN)),
        },
        Json::String(s) => DocValue::String(s.clone()),
        Json::Array(items) => DocValue::Array(items.iter().map(json_to_doc).collect()),
        Json::Object(map) => DocValue::Document(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_doc(v)))
                .collect(),
        ),
    }
}

/// A document value as plain JSON for a request body. Types JSON lacks become strings
/// (dates as ISO 8601, ObjectIds as hex, binary as base64).
pub(crate) fn doc_to_json(v: &DocValue) -> Json {
    v.to_plain_json()
}

/// A search hit as a document: `_index`, `_id`, then the `_source` fields.
fn hit_doc(hit: &Json) -> DocValue {
    let mut fields = vec![
        ("_index".to_string(), json_to_doc(&hit["_index"])),
        ("_id".to_string(), json_to_doc(&hit["_id"])),
    ];
    match &hit["_source"] {
        Json::Object(src) => fields.extend(src.iter().map(|(k, v)| (k.clone(), json_to_doc(v)))),
        _ => {
            if let Json::Object(f) = &hit["fields"] {
                fields.extend(f.iter().map(|(k, v)| (k.clone(), json_to_doc(v))));
            }
        }
    }
    DocValue::Document(fields)
}

fn hits_of(resp: &Json) -> &[Json] {
    resp["hits"]["hits"].as_array().map_or(&[], Vec::as_slice)
}

/// A fresh `X-Opaque-Id` that tags one request for cancellation.
pub(crate) fn new_tag() -> String {
    let hex: String = new_object_id().iter().map(|b| format!("{b:02x}")).collect();
    format!("sqlr:{hex}")
}

/// Marks a tagged request as in flight until dropped.
pub(crate) struct Running(Arc<Mutex<Option<String>>>);

impl Running {
    pub(crate) fn start(slot: &Arc<Mutex<Option<String>>>, tag: &str) -> Self {
        *slot.lock().unwrap() = Some(tag.to_string());
        Self(slot.clone())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = None;
    }
}

/// Splits `path` into its path and query parameters.
fn split_path(path: &str) -> (&str, Vec<(String, String)>) {
    let (p, q) = path.split_once('?').unwrap_or((path, ""));
    let params = q
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k.to_string(), v.to_string())
        })
        .collect();
    (p, params)
}

fn segments(path: &str) -> Vec<&str> {
    path.trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect()
}

/// The index part of a `_search` path (`""` for `/_search`).
fn search_index(path: &str) -> Option<String> {
    let segs = segments(path);
    match segs.as_slice() {
        ["_search"] => Some(String::new()),
        [index, "_search"] => Some(index.to_string()),
        _ => None,
    }
}

/// Elasticsearch's default `index.max_result_window`: larger sizes page with a point in time.
const MAX_RESULT_WINDOW: u64 = 10_000;

/// Hits from one plain request.
struct HitsOnce(Option<Vec<DocValue>>);

#[async_trait::async_trait]
impl DocStream for HitsOnce {
    async fn next_chunk(&mut self, max: usize) -> Result<Option<DocChunk>, DriverError> {
        let Some(docs) = self.0.as_mut() else {
            return Ok(None);
        };
        if docs.is_empty() {
            self.0 = None;
            return Ok(None);
        }
        let n = max.max(1).min(docs.len());
        let rest = docs.split_off(n);
        let chunk = DocChunk::from_docs(docs);
        *docs = rest;
        Ok(Some(chunk))
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        self.0 = None;
        Ok(())
    }
}

/// Pages through every hit with a point in time and `search_after`, reopening an
/// expired point in time and skipping the hits already returned.
struct PitSearch {
    client: Arc<EsClient>,
    index: String,
    query: String,
    body: Map<String, Json>,
    sort: Vec<Json>,
    /// The total number of hits to return (an explicit `size`); `None` for all.
    limit: Option<u64>,
    keep_alive: String,
    pit_id: Option<String>,
    search_after: Option<Json>,
    returned: u64,
    skip: u64,
    done: bool,
    notices: Vec<String>,
    tag: String,
}

fn is_expired(resp: &crate::client::Response) -> bool {
    !resp.is_success()
        && (resp.text.contains("search_context_missing_exception")
            || resp.text.contains("No search context found"))
}

impl PitSearch {
    async fn open_pit(&mut self) -> Result<(), DriverError> {
        let index = if self.index.is_empty() {
            "*"
        } else {
            &self.index
        };
        let resp = self
            .client
            .ok(
                "POST",
                &format!("/{index}/_pit?keep_alive={}", self.keep_alive),
                Body::None,
            )
            .await?;
        self.pit_id = resp["id"].as_str().map(String::from);
        if self.pit_id.is_none() {
            return Err(DriverError::new(ErrorKind::Protocol, "no point in time id"));
        }
        Ok(())
    }

    async fn close_pit(&mut self) {
        if let Some(id) = self.pit_id.take() {
            // The point in time expires anyway; a failure to close it loses nothing.
            let _ = self
                .client
                .send("DELETE", "/_pit", Body::Json(json!({ "id": id })), None)
                .await;
        }
    }
}

#[async_trait::async_trait]
impl DocStream for PitSearch {
    async fn next_chunk(&mut self, max: usize) -> Result<Option<DocChunk>, DriverError> {
        let _running = Running::start(&self.client.running, &self.tag);
        let mut restarts = 0;
        while !self.done {
            if self.pit_id.is_none() {
                self.open_pit().await?;
            }
            let remaining = self
                .limit
                .map_or(u64::MAX, |l| l.saturating_sub(self.returned));
            if remaining == 0 {
                self.done = true;
                break;
            }
            let want = if self.skip > 0 {
                self.skip.min(1000) as usize
            } else {
                (max.max(1) as u64).min(remaining) as usize
            };
            let mut body = self.body.clone();
            body.insert("size".into(), json!(want));
            body.insert("sort".into(), Json::Array(self.sort.clone()));
            body.insert(
                "pit".into(),
                json!({ "id": self.pit_id, "keep_alive": self.keep_alive }),
            );
            if let Some(after) = &self.search_after {
                body.insert("search_after".into(), after.clone());
            }
            body.entry("track_total_hits").or_insert(Json::Bool(false));
            let path = format!("/_search{}", self.query);
            let resp = self
                .client
                .send(
                    "POST",
                    &path,
                    Body::Json(Json::Object(body)),
                    Some(&self.tag),
                )
                .await?;
            if is_expired(&resp) && restarts == 0 {
                restarts += 1;
                self.notices.push(format!(
                    "The point in time of this search expired (keep-alive {}); the search \
                     restarted on a new snapshot and skipped the {} hits already shown. \
                     Documents changed in between may be missed or shown twice.",
                    self.keep_alive, self.returned
                ));
                self.pit_id = None;
                self.search_after = None;
                self.skip = self.returned;
                continue;
            }
            let resp = resp.into_result()?;
            if let Some(id) = resp["pit_id"].as_str() {
                self.pit_id = Some(id.to_string());
            }
            let hits = hits_of(&resp);
            if let Some(last) = hits.last() {
                self.search_after = Some(last["sort"].clone());
            }
            if self.skip > 0 {
                self.skip = self.skip.saturating_sub(hits.len() as u64);
                if hits.is_empty() {
                    // Fewer hits than before: nothing left to show.
                    self.skip = 0;
                    self.done = true;
                }
                continue;
            }
            if hits.len() < want {
                self.done = true;
                self.close_pit().await;
            }
            if hits.is_empty() {
                return Ok(None);
            }
            self.returned += hits.len() as u64;
            let docs: Vec<DocValue> = hits.iter().map(hit_doc).collect();
            return Ok(Some(DocChunk::from_docs(&docs)));
        }
        self.close_pit().await;
        Ok(None)
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        self.done = true;
        self.close_pit().await;
        Ok(())
    }

    fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }
}

fn syntax_error(text: &str, e: sqlr_core::ShellError) -> DriverError {
    let chars = text[..e.position.min(text.len())].chars().count();
    DriverError::new(ErrorKind::Syntax, e.message).with_position(chars)
}

/// The single request of `text`.
pub(crate) fn parse_one(text: &str) -> Result<EsRequest, DriverError> {
    let mut reqs = parse_es_console(text).map_err(|e| syntax_error(text, e))?;
    match reqs.len() {
        1 => Ok(reqs.pop().unwrap().1),
        0 => Err(DriverError::new(ErrorKind::Syntax, "nothing to run")),
        n => Err(DriverError::new(
            ErrorKind::Syntax,
            format!("the text holds {n} requests: run them one at a time or as a script"),
        )),
    }
}

/// APIs that only read (for POST and DELETE; GET and HEAD always read).
const READ_APIS: &[&str] = &[
    "_search",
    "_count",
    "_msearch",
    "_mget",
    "_sql",
    "_query",
    "_field_caps",
    "_explain",
    "_validate",
    "_termvectors",
    "_mtermvectors",
    "_search_shards",
    "_rank_eval",
    "_eql",
    "_async_search",
    "_pit",
    "_resolve",
];

/// APIs that write documents.
const WRITE_APIS: &[&str] = &[
    "_doc",
    "_create",
    "_update",
    "_bulk",
    "_update_by_query",
    "_reindex",
];

const DDL_APIS: &[&str] = &[
    "_mapping",
    "_settings",
    "_alias",
    "_aliases",
    "_index_template",
    "_template",
    "_component_template",
    "_ingest",
    "_ilm",
    "_close",
    "_open",
    "_rollover",
    "_shrink",
    "_split",
    "_clone",
    "_data_stream",
];

/// Refuses paths the HTTP URL parser could resolve into a different API than the one
/// classified here, including fragments (not sent) and backslashes (path separators).
pub(crate) fn check_path(path: &str) -> Result<(), DriverError> {
    let (p, _) = split_path(path);
    if path.contains('#')
        || path.chars().any(|c| c.is_ascii_control())
        || p.contains(['%', '\\'])
        || p.split('/').any(|s| s == "." || s == "..")
    {
        return Err(DriverError::new(
            ErrorKind::Syntax,
            "URL fragments, control characters, backslashes, `.`/`..` segments and \
             percent-escapes are not supported in request paths",
        ));
    }
    Ok(())
}

fn matches_all(body: Option<&Json>) -> bool {
    match body.map(|b| &b["query"]) {
        None | Some(Json::Null) => true,
        Some(q) => q.get("match_all").is_some(),
    }
}

/// The statement class of a request. GET and HEAD read; other methods are classified by
/// the API, the first `_` segment (after an index expression such as `_all`), checking
/// writes before reads; anything unknown counts as a write.
pub(crate) fn classify(req: &EsRequest) -> StatementClass {
    use StatementClass::*;
    let (method, path, body) = match req {
        EsRequest::Esql(_) | EsRequest::Sql(_) => return Read,
        EsRequest::Http { method, path, body } => (method.as_str(), path, body.as_ref()),
    };
    if check_path(path).is_err() {
        return Other;
    }
    if matches!(method, "GET" | "HEAD") {
        return Read;
    }
    let (p, _) = split_path(path);
    let segs = segments(p);
    let index_part = segs
        .first()
        .is_some_and(|s| !s.starts_with('_') || *s == "_all");
    let api = segs
        .iter()
        .skip(usize::from(index_part))
        .find(|s| s.starts_with('_'))
        .copied();
    match (method, api) {
        ("DELETE", None) if index_part && segs.len() == 1 => {
            Destructive(format!("DELETE drops the index {}", segs[0]))
        }
        ("PUT", None) if index_part && segs.len() == 1 => Ddl,
        (_, None) => Other,
        (_, Some(a)) if WRITE_APIS.contains(&a) => Write,
        (_, Some("_delete_by_query")) => {
            if matches_all(body) {
                Destructive("_delete_by_query without a query deletes every document".into())
            } else {
                Write
            }
        }
        ("DELETE", Some("_data_stream")) => Destructive(format!(
            "DELETE drops the data stream {}",
            segs.last().copied().unwrap_or_default()
        )),
        (_, Some(a)) if READ_APIS.contains(&a) => Read,
        (_, Some(a)) if DDL_APIS.contains(&a) => Ddl,
        _ => Other,
    }
}

impl crate::EsConnection {
    pub(crate) async fn run_request(&self, req: EsRequest) -> Result<DocExecution, DriverError> {
        if let EsRequest::Http { path, .. } = &req {
            check_path(path)?;
        }
        if self.is_read_only() && classify(&req).modifies() {
            return Err(read_only_error());
        }
        let client = &self.client;
        match req {
            EsRequest::Esql(q) => {
                if !client.at_least(8, 11) {
                    return Err(DriverError::new(
                        ErrorKind::Unsupported,
                        format!(
                            "ES|QL needs Elasticsearch 8.11 or later; this server is {}",
                            client.version_text
                        ),
                    ));
                }
                let tag = new_tag();
                let _running = Running::start(&client.running, &tag);
                let body = client
                    .send(
                        "POST",
                        "/_query?format=json",
                        Body::Json(json!({ "query": q })),
                        Some(&tag),
                    )
                    .await?
                    .into_result()?;
                Ok(DocExecution::Rows(Box::new(EsRows::esql(
                    client.clone(),
                    &body,
                ))))
            }
            EsRequest::Sql(q) => {
                let tag = new_tag();
                let _running = Running::start(&client.running, &tag);
                let body = client
                    .send(
                        "POST",
                        "/_sql?format=json",
                        Body::Json(json!({ "query": q, "fetch_size": 1000 })),
                        Some(&tag),
                    )
                    .await?
                    .into_result()?;
                Ok(DocExecution::Rows(Box::new(EsRows::sql(
                    client.clone(),
                    &body,
                ))))
            }
            EsRequest::Http { method, path, body } => {
                let (p, params) = split_path(&path);
                if matches!(method.as_str(), "GET" | "POST")
                    && !params.iter().any(|(k, _)| k == "scroll")
                    && let Some(index) = search_index(p)
                {
                    return self.search(index, &path, params, body).await;
                }
                let ndjson = EsRequest::Http {
                    method: method.clone(),
                    path: path.clone(),
                    body: body.clone(),
                }
                .is_ndjson();
                let body = match body {
                    None => Body::None,
                    Some(Json::Array(lines)) if ndjson => Body::NdJson(lines),
                    Some(v) => Body::Json(v),
                };
                let tag = new_tag();
                let _running = Running::start(&client.running, &tag);
                let resp = client.send(&method, &path, body, Some(&tag)).await?;
                if method == "HEAD" {
                    return Ok(DocExecution::Value(DocValue::Bool(resp.is_success())));
                }
                let text = resp.text.clone();
                let json = resp.into_result()?;
                if json.is_null() && !text.trim().is_empty() && text.trim() != "null" {
                    return Ok(DocExecution::Value(DocValue::String(text)));
                }
                match written_of(&json)? {
                    Some(w) => Ok(w),
                    None => Ok(DocExecution::Value(json_to_doc(&json))),
                }
            }
        }
    }

    async fn search(
        &self,
        index: String,
        path: &str,
        params: Vec<(String, String)>,
        body: Option<Json>,
    ) -> Result<DocExecution, DriverError> {
        let client = &self.client;
        let mut map = match body {
            Some(Json::Object(m)) => m,
            None => Map::new(),
            Some(_) => {
                return Err(DriverError::new(
                    ErrorKind::Syntax,
                    "the _search body must be a JSON object",
                ));
            }
        };
        let param = |k: &str| params.iter().find(|(p, _)| p == k).map(|(_, v)| v.clone());
        let size = map
            .get("size")
            .and_then(Json::as_u64)
            .or_else(|| param("size").and_then(|s| s.parse().ok()));
        let aggregates = map.contains_key("aggs") || map.contains_key("aggregations");
        let plain = ["from", "search_after", "pit"]
            .iter()
            .any(|k| map.contains_key(*k))
            || param("from").is_some()
            || !client.at_least(7, 12)
            || size.is_some_and(|n| n <= MAX_RESULT_WINDOW);
        if aggregates || size == Some(0) || plain {
            let tag = new_tag();
            let _running = Running::start(&client.running, &tag);
            let resp = client
                .send("POST", path, Body::Json(Json::Object(map)), Some(&tag))
                .await?
                .into_result()?;
            if aggregates || size == Some(0) {
                return Ok(DocExecution::Value(json_to_doc(&resp)));
            }
            let docs = hits_of(&resp).iter().map(hit_doc).collect();
            return Ok(DocExecution::Docs(Box::new(HitsOnce(Some(docs)))));
        }
        map.remove("size");
        let mut sort = match map.remove("sort") {
            Some(Json::Array(items)) => items,
            Some(one) => vec![one],
            None => Vec::new(),
        };
        if !sort.iter().any(|s| s.to_string().contains("_shard_doc")) {
            sort.push(json!({ "_shard_doc": "asc" }));
        }
        // Path parameters other than size/from go to every page (`q`, `routing`, …).
        let rest: Vec<String> = params
            .iter()
            .filter(|(k, _)| k != "size" && k != "from")
            .map(|(k, v)| {
                if v.is_empty() {
                    k.clone()
                } else {
                    format!("{k}={v}")
                }
            })
            .collect();
        let query = if rest.is_empty() {
            String::new()
        } else {
            format!("?{}", rest.join("&"))
        };
        Ok(DocExecution::Docs(Box::new(PitSearch {
            client: client.clone(),
            index,
            query,
            body: map,
            sort,
            limit: size,
            keep_alive: self.keep_alive.clone(),
            pit_id: None,
            search_after: None,
            returned: 0,
            skip: 0,
            done: false,
            notices: Vec::new(),
            tag: new_tag(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(text: &str) -> StatementClass {
        classify(&parse_one(text).unwrap())
    }

    #[test]
    fn classes() {
        use StatementClass::*;
        assert_eq!(class("GET x/_doc/1"), Read);
        assert_eq!(class("POST x/_search\n{}"), Read);
        assert_eq!(class("POST _sql/close\n{}"), Read);
        assert_eq!(class("DELETE _pit\n{\"id\": \"a\"}"), Read);
        assert_eq!(class("SQL SELECT 1"), Read);
        assert_eq!(class("PUT x/_doc/1\n{}"), Write);
        assert_eq!(class("POST x/_bulk\n{}\n{}"), Write);
        assert_eq!(class("DELETE x/_doc/1"), Write);
        assert_eq!(class("PUT x"), Ddl);
        assert_eq!(class("PUT x/_mapping\n{}"), Ddl);
        assert!(matches!(class("DELETE x"), Destructive(_)));
        assert!(matches!(class("DELETE _all"), Destructive(_)));
        assert!(matches!(
            class("POST x/_delete_by_query\n{}"),
            Destructive(_)
        ));
        assert_eq!(
            class("POST x/_delete_by_query\n{\"query\": {\"term\": {\"a\": 1}}}"),
            Write
        );
        assert_eq!(class("POST _cluster/reroute"), Other);
        // The API is the first `_` segment: later segments cannot disguise a write.
        assert!(class("POST idx/_search/../_delete_by_query\n{}").modifies());
        assert_eq!(class("POST idx/_doc/_search\n{}"), Write);
        assert_eq!(class("DELETE idx/_doc/_count"), Write);
        assert_eq!(class("POST idx/_update/_mget\n{}"), Write);
        assert!(class("POST idx/%5Fsearch\n{}").modifies());
        assert!(class("POST ./_search\n{}").modifies());
        assert!(class("POST idx/_unknown_api").modifies());
        assert_eq!(class("POST idx/_count"), Read);
        assert_eq!(class("POST _search/scroll\n{}"), Read);
    }

    #[test]
    fn dot_segments_and_escapes_are_refused() {
        for path in [
            "/idx/_search/../_delete_by_query",
            "/./_search",
            "/idx/%5Fsearch",
            "/idx/..",
        ] {
            assert!(check_path(path).is_err(), "{path}");
        }
        assert!(check_path("/.hidden-index/_search?q=a.b").is_ok());
        assert!(check_path("/idx/_doc/1").is_ok());
    }

    #[test]
    fn fragments_cannot_disguise_index_deletion_as_a_read() {
        let path = "/victim#/_search";
        let target = reqwest::Url::parse(&format!("http://localhost:9200{path}")).unwrap();
        assert_eq!(target.path(), "/victim");
        // The HTTP client drops the fragment: this would send DELETE /victim.
        assert!(class(&format!("DELETE {path}")).modifies());
        assert!(check_path(path).is_err());
    }

    #[test]
    fn backslash_traversal_cannot_disguise_delete_by_query_as_a_read() {
        let path = r"/idx/_search/..\_delete_by_query";
        let target = reqwest::Url::parse(&format!("http://localhost:9200{path}")).unwrap();
        assert_eq!(target.path(), "/idx/_delete_by_query");
        assert!(class(&format!("POST {path}\n{{}}")).modifies());
        assert!(check_path(path).is_err());
    }

    #[test]
    fn hits_flatten_metadata_and_source() {
        let hit = json!({"_index": "i", "_id": "1", "_score": null, "_source": {"a": 1, "b": {"c": "x"}}});
        let d = hit_doc(&hit);
        assert_eq!(d.get("_index"), Some(&DocValue::String("i".into())));
        assert_eq!(d.get("a"), Some(&DocValue::Int32(1)));
        assert_eq!(doc_to_json(&d)["b"]["c"], json!("x"));
        assert_eq!(search_index("/a,b/_search"), Some("a,b".into()));
        assert_eq!(search_index("/_search"), Some(String::new()));
        assert_eq!(search_index("/a/_count"), None);
    }
}
