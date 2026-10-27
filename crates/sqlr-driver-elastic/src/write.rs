//! Write responses as counts, and document edits by `_index` + `_id`.

use serde_json::{Value as Json, json};
use sqlr_core::{DocExecution, DocTarget, DocValue, DriverError, ErrorKind};

use crate::client::{Body, segment};
use crate::errors::error_parts;
use crate::query::{doc_to_json, json_to_doc};

/// Search-hit metadata that is not part of a document's source.
const META: &[&str] = &[
    "_index",
    "_id",
    "_score",
    "_routing",
    "_version",
    "_seq_no",
    "_primary_term",
    "_ignored",
];

#[derive(Default)]
struct Counts {
    matched: u64,
    modified: u64,
    inserted: u64,
    deleted: u64,
    ids: Vec<DocValue>,
}

impl Counts {
    /// Counts one `result` (`created`, `updated`, `deleted`, `noop`, `not_found`).
    fn add(&mut self, result: &str, id: &Json) {
        match result {
            "created" => self.inserted += 1,
            "updated" => {
                self.matched += 1;
                self.modified += 1;
            }
            "deleted" => self.deleted += 1,
            "noop" => self.matched += 1,
            _ => return,
        }
        if !id.is_null() {
            self.ids.push(json_to_doc(id));
        }
    }

    fn done(self) -> DocExecution {
        DocExecution::Written {
            matched: self.matched,
            modified: self.modified,
            inserted: self.inserted,
            deleted: self.deleted,
            ids: self.ids,
        }
    }
}

/// The counts of a document, `_bulk` or `_…_by_query` response; `None` for other responses.
pub(crate) fn written_of(resp: &Json) -> Result<Option<DocExecution>, DriverError> {
    let mut c = Counts::default();
    if let (Some(result), Some(_)) = (resp["result"].as_str(), resp.get("_id")) {
        c.add(result, &resp["_id"]);
        return Ok(Some(c.done()));
    }
    if let (Some(items), Some(errors)) = (resp["items"].as_array(), resp["errors"].as_bool()) {
        if errors {
            let failed: Vec<&Json> = items
                .iter()
                .filter_map(|i| i.as_object()?.values().next())
                .filter(|op| op.get("error").is_some())
                .collect();
            let Some(first) = failed.first() else {
                return Err(DriverError::new(
                    ErrorKind::Protocol,
                    "the bulk reply reports errors but no item failed; check the index",
                ));
            };
            let (ty, reason) = error_parts(first);
            return Err(DriverError::new(
                ErrorKind::Other,
                format!(
                    "{} of {} bulk items failed; the first: {}: {}",
                    failed.len(),
                    items.len(),
                    ty.unwrap_or_default(),
                    reason.unwrap_or_default()
                ),
            ));
        }
        for item in items {
            if let Some(op) = item.as_object().and_then(|o| o.values().next()) {
                c.add(op["result"].as_str().unwrap_or(""), &op["_id"]);
            }
        }
        return Ok(Some(c.done()));
    }
    if resp.get("batches").is_some() && resp.get("total").is_some() {
        let n = |k: &str| resp[k].as_u64().unwrap_or(0);
        return Ok(Some(DocExecution::Written {
            matched: n("total"),
            modified: n("updated"),
            inserted: n("created"),
            deleted: n("deleted"),
            ids: Vec::new(),
        }));
    }
    Ok(None)
}

/// A document id as the string Elasticsearch uses.
fn id_text(id: &DocValue) -> Result<String, DriverError> {
    match id {
        DocValue::String(s) => Ok(s.clone()),
        DocValue::Int32(i) => Ok(i.to_string()),
        DocValue::Int64(i) => Ok(i.to_string()),
        other => Err(DriverError::new(
            ErrorKind::Syntax,
            format!("a document id must be a string, not {}", other.type_name()),
        )),
    }
}

/// The source of `doc`: its fields without the hit metadata.
fn source(doc: &DocValue) -> Result<Json, DriverError> {
    let DocValue::Document(fields) = doc else {
        return Err(DriverError::new(
            ErrorKind::Syntax,
            format!("expected a document, not {}", doc.type_name()),
        ));
    };
    let kept: Vec<(String, DocValue)> = fields
        .iter()
        .filter(|(k, _)| !META.contains(&k.as_str()))
        .cloned()
        .collect();
    Ok(doc_to_json(&DocValue::Document(kept)))
}

impl crate::EsConnection {
    /// Replaces document `id` of `target`, only when it exists; the number replaced.
    pub(crate) async fn replace(
        &self,
        target: &DocTarget,
        id: &DocValue,
        doc: &DocValue,
    ) -> Result<u64, DriverError> {
        let path = format!(
            "/{}/_doc/{}",
            segment(&target.collection),
            segment(&id_text(id)?)
        );
        let exists = self.client.send("HEAD", &path, Body::None, None).await?;
        if exists.status == 404 {
            return Ok(0);
        }
        exists.into_result()?;
        let resp = self
            .client
            .ok(
                "PUT",
                &format!("{path}?refresh=wait_for"),
                Body::Json(source(doc)?),
            )
            .await?;
        Ok(u64::from(matches!(
            resp["result"].as_str(),
            Some("updated" | "created" | "noop")
        )))
    }

    /// Indexes `doc` (under its `_id` when it has one); returns the `_id`.
    pub(crate) async fn insert(
        &self,
        target: &DocTarget,
        doc: &DocValue,
    ) -> Result<DocValue, DriverError> {
        let index = segment(&target.collection);
        let path = match doc.get("_id") {
            Some(id) => format!(
                "/{index}/_create/{}?refresh=wait_for",
                segment(&id_text(id)?)
            ),
            None => format!("/{index}/_doc?refresh=wait_for"),
        };
        let resp = self
            .client
            .ok(
                if doc.get("_id").is_some() {
                    "PUT"
                } else {
                    "POST"
                },
                &path,
                Body::Json(source(doc)?),
            )
            .await?;
        Ok(json_to_doc(&resp["_id"]))
    }

    /// Deletes document `id`; the number deleted.
    pub(crate) async fn delete(
        &self,
        target: &DocTarget,
        id: &DocValue,
    ) -> Result<u64, DriverError> {
        let path = format!(
            "/{}/_doc/{}?refresh=wait_for",
            segment(&target.collection),
            segment(&id_text(id)?)
        );
        let resp = self.client.send("DELETE", &path, Body::None, None).await?;
        if resp.status == 404 {
            return Ok(0);
        }
        let json = resp.into_result()?;
        Ok(u64::from(json["result"].as_str() == Some("deleted")))
    }

    /// The mappings and settings of each index `target` names.
    pub(crate) async fn mappings(&self, target: &DocTarget) -> Result<Vec<DocValue>, DriverError> {
        let index = segment(&target.collection);
        let mappings = self
            .client
            .ok("GET", &format!("/{index}/_mapping"), Body::None)
            .await?;
        let settings = self
            .client
            .ok("GET", &format!("/{index}/_settings"), Body::None)
            .await?;
        let Json::Object(by_index) = &mappings else {
            return Ok(Vec::new());
        };
        Ok(by_index
            .iter()
            .map(|(name, m)| {
                json_to_doc(&json!({
                    "name": name,
                    "mappings": m["mappings"],
                    "settings": settings[name]["settings"],
                }))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(resp: &Json) -> (u64, u64, u64, u64, Vec<DocValue>) {
        match written_of(resp).unwrap() {
            Some(DocExecution::Written {
                matched,
                modified,
                inserted,
                deleted,
                ids,
            }) => (matched, modified, inserted, deleted, ids),
            _ => panic!("no counts"),
        }
    }

    #[test]
    fn write_responses() {
        assert_eq!(
            counts(&json!({"_index": "i", "_id": "1", "result": "created"})),
            (0, 0, 1, 0, vec![DocValue::String("1".into())])
        );
        let bulk = json!({"errors": false, "items": [
            {"index": {"_id": "1", "result": "created"}},
            {"update": {"_id": "2", "result": "updated"}},
            {"delete": {"_id": "3", "result": "deleted"}}]});
        let c = counts(&bulk);
        assert_eq!((c.0, c.1, c.2, c.3), (1, 1, 1, 1));
        let bad = json!({"errors": true, "items": [{"index": {"_id": "1", "error": {"type": "mapper_parsing_exception", "reason": "bad"}}}]});
        assert!(written_of(&bad).err().unwrap().message.contains("1 of 1"));
        let dbq =
            json!({"took": 1, "total": 5, "deleted": 5, "batches": 1, "updated": 0, "created": 0});
        assert_eq!(counts(&dbq).3, 5);
        assert!(
            written_of(&json!({"acknowledged": true}))
                .unwrap()
                .is_none()
        );
        // `errors: true` without a failed item is inconsistent: an error, not a panic.
        let odd = json!({"errors": true, "items": [{"index": {"_id": "1", "result": "created"}}]});
        let e = written_of(&odd).err().expect("an error");
        assert!(e.message.contains("bulk"), "{}", e.message);
        assert!(written_of(&json!({"errors": true, "items": []})).is_err());
    }

    #[test]
    fn source_drops_metadata() {
        let d = DocValue::Document(vec![
            ("_index".into(), DocValue::String("i".into())),
            ("_id".into(), DocValue::String("1".into())),
            ("a".into(), DocValue::Int32(1)),
        ]);
        assert_eq!(source(&d).unwrap(), json!({"a": 1}));
    }
}
