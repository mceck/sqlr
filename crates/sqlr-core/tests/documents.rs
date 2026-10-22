//! The document model: Extended JSON, chunks, the ES console and the per-language lexers.

use sqlr_core::{
    DocChunk, DocValue, EsRequest, LexState, QueryLanguage, Token, TokenKind, json_error,
    parse_es_console,
};

fn doc(pairs: Vec<(&str, DocValue)>) -> DocValue {
    DocValue::Document(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// One value of every type, with the awkward cases the review asks for.
fn edge_cases() -> DocValue {
    let mut deep = DocValue::Int32(1);
    for _ in 0..100 {
        deep = DocValue::Array(vec![deep]);
    }
    doc(vec![
        (
            "_id",
            DocValue::ObjectId(*b"\x65\x0a\xbc\xde\xf0\x12\x34\x56\x78\x9a\xbc\xde"),
        ),
        ("null", DocValue::Null),
        ("t", DocValue::Bool(true)),
        ("i32", DocValue::Int32(-7)),
        ("i32max", DocValue::Int32(i32::MAX)),
        ("small_long", DocValue::Int64(5)),
        ("big_long", DocValue::Int64(i64::MIN)),
        ("dbl", DocValue::Double(1.0)),
        ("dbl_frac", DocValue::Double(-0.1)),
        ("dbl_big", DocValue::Double(1e300)),
        ("dbl_tiny", DocValue::Double(5e-324)),
        ("nan", DocValue::Double(f64::NAN)),
        ("inf", DocValue::Double(f64::INFINITY)),
        ("ninf", DocValue::Double(f64::NEG_INFINITY)),
        (
            "dec",
            DocValue::Decimal128("1234567890.123456789012345678901234".into()),
        ),
        ("dec_trailing", DocValue::Decimal128("1.10".into())),
        (
            "str",
            DocValue::String("è😀 \"quoted\" \\ \n\t\u{1}".into()),
        ),
        ("date", DocValue::DateTime(1_700_000_000_123)),
        ("date_epoch", DocValue::DateTime(0)),
        (
            "date_pre_1970",
            DocValue::DateTime(-86_400_000 * 365 * 100 - 1),
        ),
        (
            "date_after_9999",
            DocValue::DateTime(253_402_300_800_000 + 5),
        ),
        (
            "bin_generic",
            DocValue::Binary {
                subtype: 0,
                bytes: vec![0, 1, 2, 255],
            },
        ),
        (
            "bin_uuid",
            DocValue::Binary {
                subtype: 4,
                bytes: (0u8..16).collect(),
            },
        ),
        (
            "bin_user",
            DocValue::Binary {
                subtype: 0x80,
                bytes: Vec::new(),
            },
        ),
        (
            "re",
            DocValue::Regex {
                pattern: "^a/[b/]\\d+$".into(),
                options: "im".into(),
            },
        ),
        ("ts", DocValue::Timestamp { t: u32::MAX, i: 1 }),
        ("min", DocValue::MinKey),
        ("max", DocValue::MaxKey),
        ("undef", DocValue::Undefined),
        ("a.b", DocValue::String("dotted key".into())),
        ("$weird", DocValue::String("dollar key".into())),
        ("", DocValue::String("empty key".into())),
        ("deep", deep),
        ("nested", doc(vec![("x", DocValue::Array(vec![]))])),
        ("dup", DocValue::Int32(1)),
        ("dup", DocValue::Int32(2)),
    ])
}

/// `NaN != NaN`, so compare through the canonical text.
fn same(a: &DocValue, b: &DocValue) -> bool {
    a.to_canonical_ejson() == b.to_canonical_ejson() && a.type_name() == b.type_name()
}

#[test]
fn ejson_round_trip_edge_cases() {
    let original = edge_cases();

    // Relaxed text keeps every type, order and duplicate key.
    let text = original.to_relaxed_json();
    let back = DocValue::from_relaxed_json(&text).expect("relaxed text parses");
    let DocValue::Document(fields) = &back else {
        panic!("not a document")
    };
    let DocValue::Document(orig_fields) = &original else {
        unreachable!()
    };
    assert_eq!(fields.len(), orig_fields.len(), "{text}");
    for ((ka, va), (kb, vb)) in fields.iter().zip(orig_fields) {
        assert_eq!(ka, kb);
        assert!(same(va, vb), "{ka}: {va:?} != {vb:?}");
    }
    let pretty = original.to_relaxed_json_pretty();
    assert!(
        pretty.contains("\n  \"_id\": {\"$oid\": \"650abcdef0123456789abcde\"}"),
        "{pretty}"
    );
    assert!(same(&DocValue::from_relaxed_json(&pretty).unwrap(), &back));

    // Relaxed forms of the awkward values.
    assert!(
        text.contains(r#""small_long": {"$numberLong": "5"}"#),
        "{text}"
    );
    assert!(
        text.contains(r#""big_long": -9223372036854775808"#),
        "{text}"
    );
    assert!(text.contains(r#""dbl": 1.0"#), "{text}");
    assert!(
        text.contains(r#""nan": {"$numberDouble": "NaN"}"#),
        "{text}"
    );
    assert!(
        text.contains(r#""date": {"$date": "2023-11-14T22:13:20.123Z"}"#),
        "{text}"
    );
    assert!(
        text.contains(r#""date_epoch": {"$date": "1970-01-01T00:00:00Z"}"#),
        "{text}"
    );
    assert!(
        text.contains(r#""date_pre_1970": {"$date": {"$numberLong": "-3153600000001"}}"#),
        "{text}"
    );
    assert!(
        text.contains(r#""date_after_9999": {"$date": {"$numberLong": "253402300800005"}}"#),
        "{text}"
    );
    assert!(
        text.contains(r#""dec": {"$numberDecimal": "1234567890.123456789012345678901234"}"#),
        "{text}"
    );
    assert!(
        text.contains(r#""bin_user": {"$binary": {"base64": "", "subType": "80"}}"#),
        "{text}"
    );

    // Canonical Extended JSON through serde_json keeps types and order (not duplicates).
    let mut no_dup = original.clone();
    if let DocValue::Document(f) = &mut no_dup {
        f.pop();
    }
    let canonical = no_dup.to_canonical_ejson();
    assert_eq!(canonical["i32"], serde_json::json!({"$numberInt": "-7"}));
    assert_eq!(
        canonical["dbl"],
        serde_json::json!({"$numberDouble": "1.0"})
    );
    assert_eq!(
        canonical["date"],
        serde_json::json!({"$date": {"$numberLong": "1700000000123"}})
    );
    let keys: Vec<&String> = canonical.as_object().unwrap().keys().collect();
    assert_eq!(keys[..3], ["_id", "null", "t"]);
    let from_canonical = DocValue::from_ejson(&canonical).unwrap();
    assert!(same(&from_canonical, &no_dup));

    // Reading Extended JSON written by other tools: `$oid` in upper case, legacy `$binary`,
    // `$uuid`, a relaxed `$date` string with an offset, and a plain number date.
    let foreign = serde_json::json!({
        "a": {"$oid": "650ABCDEF0123456789ABCDE"},
        "b": {"$binary": "AAEC", "$type": "05"},
        "c": {"$uuid": "00010203-0405-0607-0809-0a0b0c0d0e0f"},
        "d": {"$date": "2020-01-01T01:00:00+01:00"},
        "e": {"$date": 86400000},
        "f": 3000000000_i64,
        "g": 2.5,
        "h": {"$regex": "x", "$options": "i"},
    });
    let parsed = DocValue::from_ejson(&foreign).unwrap();
    let DocValue::Document(f) = parsed else {
        panic!()
    };
    assert_eq!(
        f[0].1,
        DocValue::ObjectId(*b"\x65\x0a\xbc\xde\xf0\x12\x34\x56\x78\x9a\xbc\xde")
    );
    assert_eq!(
        f[1].1,
        DocValue::Binary {
            subtype: 5,
            bytes: vec![0, 1, 2]
        }
    );
    assert_eq!(
        f[2].1,
        DocValue::Binary {
            subtype: 4,
            bytes: (0u8..16).collect()
        }
    );
    assert_eq!(f[3].1, DocValue::DateTime(1_577_836_800_000));
    assert_eq!(f[4].1, DocValue::DateTime(86_400_000));
    assert_eq!(f[5].1, DocValue::Int64(3_000_000_000));
    assert_eq!(f[6].1, DocValue::Double(2.5));
    // `$regex` is a query operator, not a wrapper: it stays a document.
    assert_eq!(f[7].1.type_name(), "object");

    // Malformed wrappers are errors, not silently plain documents.
    for bad in [
        serde_json::json!({"$oid": "xyz"}),
        serde_json::json!({"$numberLong": "1.5"}),
        serde_json::json!({"$date": "yesterday"}),
        serde_json::json!({"$binary": {"base64": "!!", "subType": "00"}}),
    ] {
        assert!(DocValue::from_ejson(&bad).is_err(), "{bad}");
    }

    let names: Vec<&str> = [
        DocValue::Null,
        DocValue::Int64(1),
        DocValue::ObjectId([0; 12]),
        DocValue::DateTime(0),
        DocValue::Decimal128("1".into()),
        DocValue::Array(vec![]),
    ]
    .iter()
    .map(DocValue::type_name)
    .collect();
    assert_eq!(
        names,
        ["null", "long", "objectId", "date", "decimal", "array"]
    );
}

#[test]
fn docchunk_round_trip() {
    let docs: Vec<DocValue> = (0..300)
        .map(|i| {
            if i == 0 {
                edge_cases()
            } else {
                doc(vec![
                    ("_id", DocValue::Int32(i)),
                    ("name", DocValue::String(format!("item {i}"))),
                    ("tags", DocValue::Array(vec![DocValue::String("a".into())])),
                ])
            }
        })
        .collect();
    let chunk = DocChunk::from_docs(&docs);
    assert_eq!(chunk.len(), 300);
    assert!(!chunk.is_empty());
    for (i, d) in docs.iter().enumerate() {
        assert!(same(&chunk.doc(i), d), "doc {i}");
    }
    // Compact: well under the size of the relaxed JSON text.
    let json: usize = docs.iter().map(|d| d.to_relaxed_json().len()).sum();
    assert!(chunk.byte_size() < json, "{} >= {json}", chunk.byte_size());
    assert_eq!(DocChunk::from_docs(&[]).len(), 0);
}

#[test]
fn es_console_parsing() {
    let text = "# a comment\nGET my-index/_search?size=5\n{\n  \"query\": {\"match_all\": {}},\n  // inline\n  \"sort\": [\"_doc\"]\n}\n\nPUT /orders/_doc/1\n{\"a\": 1}\nDELETE orders\n\nPOST _bulk\n{\"index\": {\"_index\": \"o\"}}\n{\"a\": 2}\n\nESQL FROM orders\n| LIMIT 5\n\nsql SELECT 1\nPOST _query\n{\"query\": \"\"\"\nFROM x | WHERE a == \"b\"\n\"\"\"}\n";
    let reqs = parse_es_console(text).expect("parses");
    let got: Vec<&EsRequest> = reqs.iter().map(|(_, r)| r).collect();
    assert_eq!(
        got,
        [
            &EsRequest::Http {
                method: "GET".into(),
                path: "/my-index/_search?size=5".into(),
                body: Some(serde_json::json!({"query": {"match_all": {}}, "sort": ["_doc"]})),
            },
            &EsRequest::Http {
                method: "PUT".into(),
                path: "/orders/_doc/1".into(),
                body: Some(serde_json::json!({"a": 1})),
            },
            &EsRequest::Http {
                method: "DELETE".into(),
                path: "/orders".into(),
                body: None,
            },
            &EsRequest::Http {
                method: "POST".into(),
                path: "/_bulk".into(),
                body: Some(serde_json::json!([{"index": {"_index": "o"}}, {"a": 2}])),
            },
            &EsRequest::Esql("FROM orders\n| LIMIT 5".into()),
            &EsRequest::Sql("SELECT 1".into()),
            &EsRequest::Http {
                method: "POST".into(),
                path: "/_query".into(),
                body: Some(serde_json::json!({"query": "\nFROM x | WHERE a == \"b\"\n"})),
            },
        ]
    );
    // Ranges cover each request from its first line to the end of its body.
    let (r0, _) = &reqs[0];
    assert!(text[r0.clone()].starts_with("GET my-index"));
    assert!(text[r0.clone()].ends_with('}'));
    assert_eq!(&text[reqs[2].0.clone()], "DELETE orders");
    assert_eq!(&text[reqs[4].0.clone()], "ESQL FROM orders\n| LIMIT 5");
    assert_eq!(QueryLanguage::EsConsole.split(text).len(), 7);

    // Errors name the problem and point at it.
    let err = parse_es_console("GET x/_search\n{\"a\": }").unwrap_err();
    assert!(err.message.contains("JSON"), "{}", err.message);
    assert_eq!(err.position, "GET x/_search\n{\"a\": ".len());
    let err = parse_es_console("hello world").unwrap_err();
    assert!(err.message.contains("GET"), "{}", err.message);
    assert_eq!(err.position, 0);
    assert!(
        parse_es_console("  \n# only a comment\n")
            .unwrap()
            .is_empty()
    );
}

fn tokens(lang: QueryLanguage, text: &str) -> Vec<(String, TokenKind)> {
    let mut out: Vec<Token> = Vec::new();
    lang.lex(text, LexState::Normal, &mut out);
    out.iter()
        .map(|t| (text[t.start..t.end].to_string(), t.kind))
        .collect()
}

fn kind_of(toks: &[(String, TokenKind)], text: &str) -> TokenKind {
    toks.iter()
        .find(|(t, _)| t == text)
        .unwrap_or_else(|| panic!("no token {text:?} in {toks:?}"))
        .1
}

#[test]
fn lexers_for_each_language() {
    use TokenKind::*;
    assert_eq!(
        QueryLanguage::for_dialect("mongodb"),
        QueryLanguage::MongoShell
    );
    assert_eq!(
        QueryLanguage::for_dialect("elasticsearch"),
        QueryLanguage::EsConsole
    );
    assert_eq!(QueryLanguage::for_dialect("redis"), QueryLanguage::Redis);
    assert_eq!(QueryLanguage::for_dialect("json"), QueryLanguage::Json);
    assert_eq!(QueryLanguage::for_dialect("postgres"), QueryLanguage::Sql);

    // Mongo shell and JSON.
    let m = "db.users.find({name: 'a', \"age\": {$gt: -1.5e3}, _id: ObjectId(\"65\"), re: /a[/]b/i}) // c\n/* b */;";
    let t = tokens(QueryLanguage::MongoShell, m);
    assert_eq!(kind_of(&t, "db"), Keyword);
    assert_eq!(kind_of(&t, "find"), Function);
    assert_eq!(kind_of(&t, "ObjectId"), Function);
    assert_eq!(kind_of(&t, "name"), Ident);
    assert_eq!(kind_of(&t, "\"age\""), QuotedIdent, "a quoted key");
    assert_eq!(kind_of(&t, "'a'"), String);
    assert_eq!(kind_of(&t, "\"65\""), String);
    assert_eq!(kind_of(&t, "-1.5e3"), Number);
    assert_eq!(kind_of(&t, "/a[/]b/i"), String, "a regex literal");
    assert_eq!(kind_of(&t, "// c"), Comment);
    assert_eq!(kind_of(&t, "/* b */"), Comment);
    assert_eq!(kind_of(&t, ";"), Semicolon);
    assert_eq!(kind_of(&t, "{"), Punct);

    // Resumable: an open block comment or string carries into the next line.
    let mut out = Vec::new();
    let end = QueryLanguage::MongoShell.lex("{a: /* open", LexState::Normal, &mut out);
    assert_eq!(end.state, LexState::BlockComment { depth: 1 });
    let end = QueryLanguage::MongoShell.lex("still */ 'x", end.state, &mut out);
    assert_eq!(end.state, LexState::Str { escapes: true });

    // Mongo statements split at `;` and at newlines between complete statements, not
    // before a chained `.method()`.
    let script = "db.a.find()\n  .sort({x: 1})\ndb.b.drop(); show dbs\n\nuse x";
    let parts: Vec<&str> = QueryLanguage::MongoShell
        .split(script)
        .into_iter()
        .map(|r| &script[r])
        .collect();
    assert_eq!(
        parts,
        [
            "db.a.find()\n  .sort({x: 1})",
            "db.b.drop()",
            "show dbs",
            "use x"
        ]
    );

    // Elasticsearch console: method, path, JSON body, comments, ES|QL.
    let e = "GET orders/_search?q=1\n{\"query\": {\"match\": {\"a\": true}}}\n# note\nESQL FROM orders | LIMIT 1";
    let t = tokens(QueryLanguage::EsConsole, e);
    assert_eq!(kind_of(&t, "GET"), Keyword);
    assert_eq!(kind_of(&t, "orders/_search?q=1"), Ident);
    assert_eq!(kind_of(&t, "\"query\""), QuotedIdent);
    assert_eq!(kind_of(&t, "true"), Keyword);
    assert_eq!(kind_of(&t, "# note"), Comment);
    assert_eq!(kind_of(&t, "ESQL"), Keyword);
    assert_eq!(kind_of(&t, "FROM"), Keyword);
    let mut out = Vec::new();
    let end = QueryLanguage::EsConsole.lex("{\"q\": \"\"\"FROM", LexState::Normal, &mut out);
    assert_ne!(end.state, LexState::Normal, "inside a triple-quoted string");
    let end = QueryLanguage::EsConsole.lex("x \"\"\"}", end.state, &mut out);
    assert_eq!(end.state, LexState::Normal);

    // Redis: the command, its arguments and quoted strings, one command per line.
    let r = "SET user:1 \"a b\\\" c\" EX 10\nget 'k k'";
    let t = tokens(QueryLanguage::Redis, r);
    assert_eq!(kind_of(&t, "SET"), Keyword);
    assert_eq!(kind_of(&t, "user:1"), Ident);
    assert_eq!(kind_of(&t, "\"a b\\\" c\""), String);
    assert_eq!(kind_of(&t, "EX"), Ident);
    assert_eq!(kind_of(&t, "10"), Number);
    assert_eq!(kind_of(&t, "get"), Keyword);
    assert_eq!(kind_of(&t, "'k k'"), String);
    let lines: Vec<&str> = QueryLanguage::Redis
        .split("PING\n\n  GET a  \n")
        .into_iter()
        .map(|r| &"PING\n\n  GET a  \n"[r])
        .collect();
    assert_eq!(lines, ["PING", "GET a"]);
}

#[test]
fn json_lexes_and_splits_one_document() {
    use TokenKind::*;
    let doc = "{\"name\": \"a\", \"n\": 1, \"ok\": true}";
    let t = tokens(QueryLanguage::Json, doc);
    assert_eq!(kind_of(&t, "\"name\""), QuotedIdent, "a quoted key");
    assert_eq!(kind_of(&t, "\"a\""), String);
    assert_eq!(kind_of(&t, "1"), Number);
    assert_eq!(kind_of(&t, "true"), Keyword);
    assert_eq!(kind_of(&t, ":"), Punct);
    assert_eq!(QueryLanguage::Json.line_comment(), "");

    let padded = format!("  {doc}\n");
    let parts: Vec<&str> = QueryLanguage::Json
        .split(&padded)
        .into_iter()
        .map(|r| &padded[r])
        .collect();
    assert_eq!(parts, [doc], "a JSON value is one statement");
    assert!(QueryLanguage::Json.split(" \n\t ").is_empty());

    // An open string resumes on the next line.
    let mut out = Vec::new();
    let end = QueryLanguage::Json.lex("{\"a\": \"x", LexState::Normal, &mut out);
    assert!(
        matches!(end.state, LexState::DoubleStr { .. }),
        "{:?}",
        end.state
    );
    assert_eq!(out.last().map(|t| t.kind), Some(String));
}

#[test]
fn json_errors_carry_a_char_offset() {
    assert!(json_error("{\"a\": 1}").is_none());
    assert!(json_error("[1, 2, \"x\"]").is_none());

    let text = "{\n  \"a\": 1,\n}";
    let err = json_error(text).expect("a trailing comma");
    assert!(!err.message.is_empty());
    assert_eq!(
        err.at,
        text.chars().position(|c| c == '}').unwrap(),
        "the error points at the offending brace"
    );

    // serde_json's column counts bytes; the offset counts chars.
    let text = "{\"é\": 1,}";
    let err = json_error(text).expect("a trailing comma");
    assert_eq!(err.at, text.chars().position(|c| c == '}').unwrap());

    // An error never points past the end of the text.
    let text = "{\"a\": ";
    let err = json_error(text).expect("a truncated value");
    assert!(err.at <= text.chars().count(), "{}", err.at);
}
