//! A Document fixture exercising the SDK with a typed value and explorer targets.
use sqlr_driver_sdk::*;
use sqlr_testkit::{FakeDocDriver, FakeDocScript};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let mut script = FakeDocScript {
        docs: vec![DocValue::Document(vec![
            ("_id".into(), DocValue::ObjectId([7; 12])),
            ("secret".into(), DocValue::String("private document".into())),
        ])],
        tree: vec![Node {
            path: NodePath(vec!["db".into(), "collections".into(), "a.b".into()]),
            name: "a.b".into(),
            kind: NodeKind::Table,
            has_children: false,
            detail: None,
        }],
        views: vec!["view".into()],
        ..Default::default()
    };
    if std::env::var_os("DOC_LARGE_PAGE").is_some() {
        script.docs = (0..4)
            .map(|_| DocValue::String("x".repeat(17 * 1024 * 1024)))
            .collect();
    }
    let driver = FakeDocDriver::new(script);
    serve_io(
        driver,
        std::env::var("DOC_PROTOCOL").unwrap_or_else(|_| PROTOCOL.into()),
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await
}
