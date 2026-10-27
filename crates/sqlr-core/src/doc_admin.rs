//! Statements sqlr writes for document databases: index administration and batch inserts.
//! They are ordinary statements in the connection's language (the Mongo shell subset, the
//! Elasticsearch console), run through [`crate::DocumentConn::run`] like typed ones, so the
//! drivers' parsing, classification and read-only checks apply to them too.

use crate::doc::DocValue;
use crate::document::DocTarget;
use crate::lang::QueryLanguage;
use crate::mongo_shell::ShellError;

/// Why an index or batch-insert statement for a document database cannot be written.
#[derive(Debug, thiserror::Error)]
pub enum DocAdminError {
    /// A MongoDB index without keys.
    #[error("An index needs at least one key")]
    NoIndexKeys,
    /// A TTL index must have exactly one key.
    #[error("A TTL index has exactly one key (a date field)")]
    TtlNeedsOneKey,
    /// A negative TTL.
    #[error("The TTL must be zero or more seconds")]
    NegativeTtl,
    /// The partial filter is not valid relaxed Extended JSON.
    #[error("Partial filter: {}", .0.message)]
    PartialFilter(#[source] ShellError),
    /// The partial filter is valid JSON but not a document.
    #[error("The partial filter must be a document")]
    PartialFilterNotDocument,
    /// An Elasticsearch index name that Elasticsearch would reject.
    #[error(
        "\"{index}\" is not a valid index name: use lowercase letters, digits, '-', '_' or '.'"
    )]
    InvalidIndexName {
        /// The rejected name.
        index: String,
    },
    /// The settings or mappings text is not JSON.
    #[error("{section}: {source}")]
    InvalidBody {
        /// `settings` or `mappings`.
        section: &'static str,
        /// The parser's complaint.
        source: serde_json::Error,
    },
    /// The settings or mappings JSON is not an object.
    #[error("{section} must be a JSON object")]
    BodyNotObject {
        /// `settings` or `mappings`.
        section: &'static str,
    },
    /// The request body could not be serialized.
    #[error("{0}")]
    Serialize(#[from] serde_json::Error),
    /// A name that is not an Elasticsearch index name (index deletion).
    #[error("\"{index}\" is not an index name")]
    NotAnIndexName {
        /// The rejected name.
        index: String,
    },
    /// The target index of a bulk insert is not an index name.
    #[error("\"{index}\" is not an index name to write into")]
    NotWritableIndex {
        /// The rejected name.
        index: String,
    },
    /// A value to insert is not a document.
    #[error("expected a document, not {type_name}")]
    NotADocument {
        /// The value's type name.
        type_name: &'static str,
    },
}

/// A key's direction or kind in a MongoDB index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexDirection {
    /// Ascending order.
    Asc,
    /// Descending order.
    Desc,
    /// Full-text index.
    Text,
    /// Hashed index.
    Hashed,
    /// Geospatial `2dsphere` index.
    Sphere2d,
}

impl IndexDirection {
    /// Every direction, in menu order.
    pub const ALL: [IndexDirection; 5] = [
        IndexDirection::Asc,
        IndexDirection::Desc,
        IndexDirection::Text,
        IndexDirection::Hashed,
        IndexDirection::Sphere2d,
    ];

    /// The name shown in the index editor.
    pub fn label(self) -> &'static str {
        match self {
            IndexDirection::Asc => "ascending",
            IndexDirection::Desc => "descending",
            IndexDirection::Text => "text",
            IndexDirection::Hashed => "hashed",
            IndexDirection::Sphere2d => "2dsphere",
        }
    }

    fn value(self) -> DocValue {
        match self {
            IndexDirection::Asc => DocValue::Int32(1),
            IndexDirection::Desc => DocValue::Int32(-1),
            IndexDirection::Text => DocValue::String("text".into()),
            IndexDirection::Hashed => DocValue::String("hashed".into()),
            IndexDirection::Sphere2d => DocValue::String("2dsphere".into()),
        }
    }
}

/// A MongoDB index to create.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MongoIndexSpec {
    /// The indexed fields with their directions, in key order.
    pub keys: Vec<(String, IndexDirection)>,
    /// The server names it from its keys when empty.
    pub name: String,
    /// Whether the index rejects duplicate keys.
    pub unique: bool,
    /// Documents expire this many seconds after the date in the (single) key.
    pub ttl_seconds: Option<i64>,
    /// Relaxed JSON of a filter document; empty for none.
    pub partial_filter: String,
}

/// A string as a quoted JSON string.
fn quoted(s: &str) -> String {
    DocValue::String(s.to_string()).to_relaxed_json()
}

fn collection_expr(target: &DocTarget) -> String {
    format!("db.getCollection({})", quoted(&target.collection))
}

/// `db.getCollection("…").createIndex({…}, {…})` for `spec`.
pub fn mongo_create_index(
    target: &DocTarget,
    spec: &MongoIndexSpec,
) -> Result<String, DocAdminError> {
    let keys: Vec<(String, DocValue)> = spec
        .keys
        .iter()
        .filter(|(k, _)| !k.trim().is_empty())
        .map(|(k, d)| (k.trim().to_string(), d.value()))
        .collect();
    if keys.is_empty() {
        return Err(DocAdminError::NoIndexKeys);
    }
    let mut options = Vec::new();
    if !spec.name.trim().is_empty() {
        options.push((
            "name".to_string(),
            DocValue::String(spec.name.trim().into()),
        ));
    }
    if spec.unique {
        options.push(("unique".to_string(), DocValue::Bool(true)));
    }
    if let Some(ttl) = spec.ttl_seconds {
        if keys.len() != 1 {
            return Err(DocAdminError::TtlNeedsOneKey);
        }
        if ttl < 0 {
            return Err(DocAdminError::NegativeTtl);
        }
        options.push(("expireAfterSeconds".to_string(), DocValue::Int64(ttl)));
    }
    if !spec.partial_filter.trim().is_empty() {
        let filter = DocValue::from_relaxed_json(&spec.partial_filter)
            .map_err(DocAdminError::PartialFilter)?;
        if !matches!(filter, DocValue::Document(_)) {
            return Err(DocAdminError::PartialFilterNotDocument);
        }
        options.push(("partialFilterExpression".to_string(), filter));
    }
    let keys = DocValue::Document(keys).to_relaxed_json();
    let text = if options.is_empty() {
        format!("{}.createIndex({keys})", collection_expr(target))
    } else {
        let options = DocValue::Document(options).to_relaxed_json();
        format!("{}.createIndex({keys}, {options})", collection_expr(target))
    };
    Ok(text)
}

/// `db.getCollection("…").dropIndex("name")`.
pub fn mongo_drop_index(target: &DocTarget, name: &str) -> String {
    format!("{}.dropIndex({})", collection_expr(target), quoted(name))
}

/// Whether `name` is a valid Elasticsearch index name: lowercase, without `\ / * ? " < > |`,
/// spaces, `,` or `#`, not starting with `-`, `_` or `+`, not `.` or `..`, at most 255 bytes.
pub fn es_index_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.starts_with(['-', '_', '+'])
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

/// `PUT /<index>` with a body of `settings` and `mappings` (JSON text each; empty for
/// none).
pub fn es_create_index(
    index: &str,
    settings: &str,
    mappings: &str,
) -> Result<String, DocAdminError> {
    if !es_index_name_ok(index) {
        return Err(DocAdminError::InvalidIndexName {
            index: index.to_string(),
        });
    }
    let mut body = serde_json::Map::new();
    for (key, text) in [("settings", settings), ("mappings", mappings)] {
        if text.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|source| DocAdminError::InvalidBody {
                section: key,
                source,
            })?;
        if !value.is_object() {
            return Err(DocAdminError::BodyNotObject { section: key });
        }
        body.insert(key.to_string(), value);
    }
    if body.is_empty() {
        return Ok(format!("PUT /{index}"));
    }
    let body = serde_json::to_string_pretty(&serde_json::Value::Object(body))?;
    Ok(format!("PUT /{index}\n{body}"))
}

/// `DELETE /<index>`.
pub fn es_drop_index(index: &str) -> Result<String, DocAdminError> {
    if !es_index_name_ok(index) {
        return Err(DocAdminError::NotAnIndexName {
            index: index.to_string(),
        });
    }
    Ok(format!("DELETE /{index}"))
}

/// The statement inserting `docs` into `target` in one request: `insertMany` (MongoDB,
/// documents in relaxed Extended JSON, so every type survives) or a `_bulk` request
/// (Elasticsearch, plain JSON; a document's `_id` becomes the action's id and its `_index`
/// is dropped: the target index applies).
pub fn insert_many_text(
    language: QueryLanguage,
    target: &DocTarget,
    docs: &[DocValue],
) -> Result<String, DocAdminError> {
    match language {
        QueryLanguage::EsConsole => {
            if !es_index_name_ok(&target.collection) {
                return Err(DocAdminError::NotWritableIndex {
                    index: target.collection.clone(),
                });
            }
            let mut text = format!("POST /{}/_bulk?refresh=wait_for\n", target.collection);
            for doc in docs {
                let DocValue::Document(fields) = doc else {
                    return Err(DocAdminError::NotADocument {
                        type_name: doc.type_name(),
                    });
                };
                let mut source = serde_json::Map::new();
                let mut action = serde_json::Map::new();
                for (k, v) in fields {
                    match k.as_str() {
                        "_id" => {
                            action.insert("_id".into(), v.to_plain_json());
                        }
                        "_index" => {}
                        _ => {
                            source.insert(k.clone(), v.to_plain_json());
                        }
                    }
                }
                let action = serde_json::json!({ "index": action });
                text.push_str(&action.to_string());
                text.push('\n');
                text.push_str(&serde_json::Value::Object(source).to_string());
                text.push('\n');
            }
            Ok(text)
        }
        _ => {
            let mut text = format!("{}.insertMany([", collection_expr(target));
            for (i, doc) in docs.iter().enumerate() {
                if i > 0 {
                    text.push_str(",\n");
                }
                text.push_str(&doc.to_relaxed_json());
            }
            text.push_str("])");
            Ok(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mongo_shell::{MongoOp, parse_mongo_shell};

    fn users() -> DocTarget {
        DocTarget {
            database: None,
            collection: "my \"users\"".into(),
        }
    }

    #[test]
    fn mongo_index_statements_parse_back() {
        let spec = MongoIndexSpec {
            keys: vec![
                ("email".into(), IndexDirection::Asc),
                ("at".into(), IndexDirection::Desc),
            ],
            name: "by_email".into(),
            unique: true,
            ttl_seconds: None,
            partial_filter: "{active: true}".into(),
        };
        let text = mongo_create_index(&users(), &spec).unwrap();
        let ops = parse_mongo_shell(&text).unwrap();
        let MongoOp::CreateIndex {
            coll,
            keys,
            options,
        } = &ops[0].1
        else {
            panic!("{ops:?}")
        };
        assert_eq!(coll, "my \"users\"");
        assert_eq!(keys.get("at"), Some(&DocValue::Int32(-1)));
        let options = options.as_ref().unwrap();
        assert_eq!(options.get("unique"), Some(&DocValue::Bool(true)));
        assert!(options.get("partialFilterExpression").is_some());
        let drop = mongo_drop_index(&users(), "by_email");
        assert!(matches!(
            &parse_mongo_shell(&drop).unwrap()[0].1,
            MongoOp::DropIndex { .. }
        ));
        let ttl = MongoIndexSpec {
            ttl_seconds: Some(60),
            ..spec.clone()
        };
        assert_eq!(
            mongo_create_index(&users(), &ttl).unwrap_err().to_string(),
            "A TTL index has exactly one key (a date field)"
        );
        assert_eq!(
            mongo_create_index(&users(), &MongoIndexSpec::default())
                .unwrap_err()
                .to_string(),
            "An index needs at least one key"
        );
        let bad = MongoIndexSpec {
            partial_filter: "{a:".into(),
            ..spec
        };
        assert!(
            mongo_create_index(&users(), &bad)
                .unwrap_err()
                .to_string()
                .starts_with("Partial filter: ")
        );
    }

    #[test]
    fn es_index_statements_validate_names_and_bodies() {
        assert_eq!(
            es_create_index("logs-1", "{\"number_of_shards\": 1}", "").unwrap(),
            "PUT /logs-1\n{\n  \"settings\": {\n    \"number_of_shards\": 1\n  }\n}"
        );
        assert_eq!(es_create_index("logs", "", "").unwrap(), "PUT /logs");
        assert_eq!(
            es_create_index("Logs", "", "").unwrap_err().to_string(),
            "\"Logs\" is not a valid index name: use lowercase letters, digits, '-', '_' or '.'"
        );
        assert!(es_create_index("a/../b", "", "").is_err());
        assert_eq!(
            es_create_index("ok", "[1]", "").unwrap_err().to_string(),
            "settings must be a JSON object"
        );
        assert_eq!(es_drop_index("logs").unwrap(), "DELETE /logs");
        assert_eq!(
            es_drop_index("logs*").unwrap_err().to_string(),
            "\"logs*\" is not an index name"
        );
    }

    #[test]
    fn batch_inserts_in_each_language() {
        let docs = [DocValue::Document(vec![
            ("_id".into(), DocValue::String("1".into())),
            ("_index".into(), DocValue::String("old".into())),
            ("n".into(), DocValue::Int64(5)),
        ])];
        let es = DocTarget {
            database: None,
            collection: "logs".into(),
        };
        let text = insert_many_text(QueryLanguage::EsConsole, &es, &docs).unwrap();
        assert_eq!(
            text,
            "POST /logs/_bulk?refresh=wait_for\n{\"index\":{\"_id\":\"1\"}}\n{\"n\":5}\n"
        );
        let mongo = insert_many_text(QueryLanguage::MongoShell, &users(), &docs).unwrap();
        let ops = parse_mongo_shell(&mongo).unwrap();
        let MongoOp::InsertMany { docs: parsed, .. } = &ops[0].1 else {
            panic!("{ops:?}")
        };
        assert_eq!(parsed, &docs, "every type survives");
    }
}
