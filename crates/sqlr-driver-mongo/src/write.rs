//! Write commands: insert, update, replace, delete, findAndModify and index admin.
//!
//! Writes run as raw commands built from `DocValue`s (so documents keep their key order),
//! and their replies are checked for `writeErrors` and `writeConcernError`.

use sqlr_core::{DocExecution, DocValue, DriverError, ErrorKind, MongoOp, new_object_id};

use crate::connect::MongoConnection;
use crate::errors::kind_for_code;
use crate::query::{cmd, command, new_tag};

fn s(v: &str) -> DocValue {
    DocValue::String(v.to_string())
}

fn count(v: Option<&DocValue>) -> u64 {
    match v {
        Some(DocValue::Int32(i)) => (*i).max(0) as u64,
        Some(DocValue::Int64(i)) => (*i).max(0) as u64,
        Some(DocValue::Double(d)) => d.max(0.0) as u64,
        _ => 0,
    }
}

/// Fails on the reply's `writeErrors` or `writeConcernError`.
fn check_reply(reply: &DocValue) -> Result<(), DriverError> {
    if let Some(DocValue::Array(errors)) = reply.get("writeErrors")
        && let Some(first) = errors.first()
    {
        let code = count(first.get("code")) as i32;
        let mut message = match first.get("errmsg") {
            Some(DocValue::String(m)) => m.clone(),
            _ => format!("write error {code}"),
        };
        if errors.len() > 1 {
            message.push_str(&format!(" (and {} more write errors)", errors.len() - 1));
        }
        return Err(DriverError::new(kind_for_code(code), message));
    }
    if let Some(wce) = reply.get("writeConcernError") {
        let message = match wce.get("errmsg") {
            Some(DocValue::String(m)) => m.clone(),
            _ => "write concern error".into(),
        };
        return Err(DriverError::new(
            ErrorKind::Other,
            format!("write concern error: {message}"),
        ));
    }
    Ok(())
}

/// Document fields in order.
type Fields = Vec<(String, DocValue)>;

/// Splits `options` into the command's top-level options and the statement's own.
fn split_options(options: Option<DocValue>) -> (Fields, Fields) {
    let mut top = Vec::new();
    let mut stmt = Vec::new();
    if let Some(DocValue::Document(fields)) = options {
        for (k, v) in fields {
            match k.as_str() {
                "writeConcern" | "comment" | "let" | "bypassDocumentValidation" | "ordered" => {
                    top.push((k, v))
                }
                _ => stmt.push((k, v)),
            }
        }
    }
    (top, stmt)
}

fn with(mut base: DocValue, extra: Vec<(String, DocValue)>) -> DocValue {
    if let DocValue::Document(fields) = &mut base {
        for (k, v) in extra {
            if !fields.iter().any(|(f, _)| *f == k) {
                fields.push((k, v));
            }
        }
    }
    base
}

fn tagged(base: DocValue, top: Vec<(String, DocValue)>) -> DocValue {
    let c = with(base, top);
    if c.get("comment").is_some() {
        c
    } else {
        with(c, vec![("comment".into(), s(&new_tag()))])
    }
}

/// Inserts `docs`, giving each one without an `_id` a new ObjectId. Returns the number
/// inserted and the ids.
pub(crate) async fn insert(
    conn: &MongoConnection,
    db: &str,
    coll: &str,
    docs: Vec<DocValue>,
    options: Option<DocValue>,
) -> Result<(u64, Vec<DocValue>), DriverError> {
    let mut ids = Vec::with_capacity(docs.len());
    let docs: Vec<DocValue> = docs
        .into_iter()
        .map(|d| match d {
            DocValue::Document(mut fields) => {
                let id = match fields.iter().find(|(k, _)| k == "_id") {
                    Some((_, id)) => id.clone(),
                    None => {
                        let id = DocValue::ObjectId(new_object_id());
                        fields.insert(0, ("_id".into(), id.clone()));
                        id
                    }
                };
                ids.push(id);
                Ok(DocValue::Document(fields))
            }
            other => Err(DriverError::new(
                ErrorKind::Syntax,
                format!("expected a document, not {}", other.type_name()),
            )),
        })
        .collect::<Result<_, _>>()?;
    let (top, _) = split_options(options);
    let c = tagged(
        cmd(vec![
            ("insert", s(coll)),
            ("documents", DocValue::Array(docs)),
        ]),
        top,
    );
    let reply = command(conn, db, &c).await?;
    check_reply(&reply)?;
    Ok((count(reply.get("n")), ids))
}

/// One update statement. Returns `(matched, modified, upserted, upserted ids)`.
pub(crate) async fn update(
    conn: &MongoConnection,
    db: &str,
    coll: &str,
    filter: DocValue,
    update: DocValue,
    multi: bool,
    options: Option<DocValue>,
) -> Result<(u64, u64, u64, Vec<DocValue>), DriverError> {
    let (top, stmt) = split_options(options);
    let statement = with(
        cmd(vec![
            ("q", filter),
            ("u", update),
            ("multi", DocValue::Bool(multi)),
        ]),
        stmt,
    );
    let c = tagged(
        cmd(vec![
            ("update", s(coll)),
            ("updates", DocValue::Array(vec![statement])),
        ]),
        top,
    );
    let reply = command(conn, db, &c).await?;
    check_reply(&reply)?;
    let ids: Vec<DocValue> = match reply.get("upserted") {
        Some(DocValue::Array(up)) => up.iter().filter_map(|u| u.get("_id").cloned()).collect(),
        _ => Vec::new(),
    };
    let n = count(reply.get("n"));
    let upserted = ids.len() as u64;
    Ok((
        n.saturating_sub(upserted),
        count(reply.get("nModified")),
        upserted,
        ids,
    ))
}

/// Deletes the first (or every, with `many`) document matching `filter`.
pub(crate) async fn delete(
    conn: &MongoConnection,
    db: &str,
    coll: &str,
    filter: DocValue,
    many: bool,
) -> Result<u64, DriverError> {
    let statement = cmd(vec![
        ("q", filter),
        ("limit", DocValue::Int32(if many { 0 } else { 1 })),
    ]);
    let c = tagged(
        cmd(vec![
            ("delete", s(coll)),
            ("deletes", DocValue::Array(vec![statement])),
        ]),
        Vec::new(),
    );
    let reply = command(conn, db, &c).await?;
    check_reply(&reply)?;
    Ok(count(reply.get("n")))
}

/// The name `createIndex` gives an index: `a_1_b_-1`.
fn index_name(keys: &DocValue) -> String {
    let DocValue::Document(fields) = keys else {
        return String::new();
    };
    fields
        .iter()
        .map(|(k, v)| {
            let v = match v {
                DocValue::Int32(i) => i.to_string(),
                DocValue::Int64(i) => i.to_string(),
                DocValue::Double(d) if d.fract() == 0.0 => format!("{}", *d as i64),
                DocValue::String(t) => t.clone(),
                other => other.to_relaxed_json(),
            };
            format!("{k}_{v}")
        })
        .collect::<Vec<_>>()
        .join("_")
}

/// Refuses a replacement with top-level `$` keys: the server would apply it as an update.
pub(crate) fn check_replacement(doc: &DocValue) -> Result<(), DriverError> {
    if has_operators(doc) {
        return Err(DriverError::new(
            ErrorKind::Syntax,
            "a replacement document cannot have update operators (top-level `$` keys)",
        ));
    }
    Ok(())
}

fn has_operators(doc: &DocValue) -> bool {
    matches!(doc, DocValue::Document(f) if f.iter().any(|(k, _)| k.starts_with('$')))
}

/// Runs a write (or index) statement.
pub(crate) async fn run_write(
    conn: &MongoConnection,
    db: &str,
    op: MongoOp,
) -> Result<DocExecution, DriverError> {
    let written = |matched, modified, inserted, deleted, ids| DocExecution::Written {
        matched,
        modified,
        inserted,
        deleted,
        ids,
    };
    Ok(match op {
        MongoOp::InsertOne { coll, doc } => {
            let (n, ids) = insert(conn, db, &coll, vec![doc], None).await?;
            written(0, 0, n, 0, ids)
        }
        MongoOp::InsertMany {
            coll,
            docs,
            options,
        } => {
            let (n, ids) = insert(conn, db, &coll, docs, options).await?;
            written(0, 0, n, 0, ids)
        }
        MongoOp::UpdateOne {
            coll,
            filter,
            update: u,
            options,
        } => {
            let (m, md, up, ids) = update(conn, db, &coll, filter, u, false, options).await?;
            written(m, md, up, 0, ids)
        }
        MongoOp::UpdateMany {
            coll,
            filter,
            update: u,
            options,
        } => {
            let (m, md, up, ids) = update(conn, db, &coll, filter, u, true, options).await?;
            written(m, md, up, 0, ids)
        }
        MongoOp::ReplaceOne {
            coll,
            filter,
            replacement,
            options,
        } => {
            check_replacement(&replacement)?;
            let (m, md, up, ids) =
                update(conn, db, &coll, filter, replacement, false, options).await?;
            written(m, md, up, 0, ids)
        }
        MongoOp::DeleteOne { coll, filter } => written(
            0,
            0,
            0,
            delete(conn, db, &coll, filter, false).await?,
            Vec::new(),
        ),
        MongoOp::DeleteMany { coll, filter } => written(
            0,
            0,
            0,
            delete(conn, db, &coll, filter, true).await?,
            Vec::new(),
        ),
        MongoOp::FindOneAndUpdate {
            coll,
            filter,
            update: u,
            options,
        } => {
            let mut c = vec![
                ("findAndModify".to_string(), s(&coll)),
                ("query".into(), filter),
                ("update".into(), u),
            ];
            if let Some(DocValue::Document(opts)) = options {
                for (k, v) in opts {
                    match k.as_str() {
                        "returnDocument" => c.push(("new".into(), DocValue::Bool(v == s("after")))),
                        "returnNewDocument" => c.push(("new".into(), v)),
                        "projection" => c.push(("fields".into(), v)),
                        _ => c.push((k, v)),
                    }
                }
            }
            let reply = command(conn, db, &tagged(DocValue::Document(c), Vec::new())).await?;
            check_reply(&reply)?;
            DocExecution::Value(reply.get("value").cloned().unwrap_or(DocValue::Null))
        }
        MongoOp::CreateIndex {
            coll,
            keys,
            options,
        } => {
            let name = match options.as_ref().and_then(|o| o.get("name")) {
                Some(DocValue::String(n)) => n.clone(),
                _ => index_name(&keys),
            };
            let mut spec = vec![("key".to_string(), keys), ("name".into(), s(&name))];
            if let Some(DocValue::Document(opts)) = options {
                spec.extend(opts.into_iter().filter(|(k, _)| k != "name"));
            }
            let c = cmd(vec![
                ("createIndexes", s(&coll)),
                ("indexes", DocValue::Array(vec![DocValue::Document(spec)])),
            ]);
            command(conn, db, &c).await?;
            DocExecution::Value(s(&name))
        }
        MongoOp::DropIndex { coll, index } => DocExecution::Value(
            command(
                conn,
                db,
                &cmd(vec![("dropIndexes", s(&coll)), ("index", index)]),
            )
            .await?,
        ),
        MongoOp::Drop { coll } => {
            DocExecution::Value(command(conn, db, &cmd(vec![("drop", s(&coll))])).await?)
        }
        other => {
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                format!("not a write: {other:?}"),
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_names_like_the_shell() {
        let keys = cmd(vec![
            ("a", DocValue::Int32(1)),
            ("b", DocValue::Int32(-1)),
            ("loc", s("2dsphere")),
            ("d", DocValue::Double(1.0)),
        ]);
        assert_eq!(index_name(&keys), "a_1_b_-1_loc_2dsphere_d_1");
    }

    #[test]
    fn write_errors_fail() {
        let reply = cmd(vec![
            ("n", DocValue::Int32(0)),
            (
                "writeErrors",
                DocValue::Array(vec![cmd(vec![
                    ("index", DocValue::Int32(0)),
                    ("code", DocValue::Int32(11000)),
                    ("errmsg", s("E11000 duplicate key error")),
                ])]),
            ),
        ]);
        let e = check_reply(&reply).unwrap_err();
        assert!(e.message.contains("duplicate key"));
        assert!(check_reply(&cmd(vec![("n", DocValue::Int32(1))])).is_ok());
    }
}
