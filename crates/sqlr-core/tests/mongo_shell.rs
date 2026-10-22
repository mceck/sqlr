//! The Mongo shell subset corpus: each `fixtures/mongosh/*.js` parses to the operations (or
//! the error) in the `.expected` file next to it. Run with `SQLR_BLESS=1` to rewrite the
//! expected files after a deliberate change, then review the diff.

use std::fmt::Write as _;
use std::path::Path;

use sqlr_core::parse_mongo_shell;

fn render(text: &str) -> String {
    match parse_mongo_shell(text) {
        Ok(ops) => {
            let mut out = String::new();
            for (range, op) in ops {
                writeln!(out, "// {}", text[range].replace('\n', "\n// ")).unwrap();
                writeln!(out, "{op:#?}").unwrap();
            }
            out
        }
        Err(e) => format!("error at {}: {}\n", e.position, e.message),
    }
}

#[test]
fn shell_subset_corpus() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mongosh");
    let bless = std::env::var_os("SQLR_BLESS").is_some();
    let mut cases: Vec<_> = std::fs::read_dir(&dir)
        .expect("fixtures dir")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "js"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 15, "corpus too small: {}", cases.len());
    let mut failures = Vec::new();
    for js in cases {
        let text = std::fs::read_to_string(&js).unwrap();
        let got = render(&text);
        let expected_path = js.with_extension("expected");
        if bless {
            std::fs::write(&expected_path, &got).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&expected_path)
            .unwrap_or_else(|_| panic!("missing {}", expected_path.display()));
        if got != expected {
            failures.push(format!(
                "{}:\n--- expected\n{expected}\n--- got\n{got}",
                js.display()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every prefix (on a char boundary) of every fixture parses or fails cleanly: no panic.
#[test]
fn every_prefix_parses_or_errors_without_panicking() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mongosh");
    let mut texts: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "js"))
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();
    texts.push("{\"a\": \"è😀\", b: [1, 2.5e3, ObjectId(\"650abcdef0123456789abcde\")]}".into());
    for text in &texts {
        for (i, _) in text.char_indices().chain([(text.len(), ' ')]) {
            let prefix = &text[..i];
            let _ = parse_mongo_shell(prefix);
            let _ = sqlr_core::DocValue::from_relaxed_json(prefix);
            let _ = sqlr_core::parse_es_console(prefix);
        }
    }
}

#[test]
fn unterminated_strings_are_errors() {
    for (text, at) in [("\"abc", 0), ("{a: 'è", 4), ("[1, \"x\\\"", 4)] {
        let e = sqlr_core::DocValue::from_relaxed_json(text).unwrap_err();
        assert_eq!(e.position, at, "{text}");
        assert!(e.message.contains("unterminated string"), "{}", e.message);
    }
}

#[test]
fn errors_point_at_the_construct() {
    let text = "db.c.find().forEach(function (d) { print(d) })";
    let e = parse_mongo_shell(text).unwrap_err();
    assert_eq!(e.position, text.find("forEach").unwrap());
    assert!(e.message.contains("forEach"), "{}", e.message);
    assert!(e.message.contains("runCommand"), "{}", e.message);
}
