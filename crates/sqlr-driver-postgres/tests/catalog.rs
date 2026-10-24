mod support;

use sqlr_core::{Driver, RelationKind};
use sqlr_driver_postgres::{PG_KEYWORDS, PG_RESERVED, PostgresDriver};
use support::pg;

async fn connect() -> (
    impl Sized,
    std::sync::Arc<dyn sqlr_core::Connection>,
    tokio_postgres::Client,
) {
    let (c, cfg, sec) = pg().await;
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    let url = format!(
        "host={} port={} user=postgres password=postgres dbname={}",
        cfg.values["host"], cfg.values["port"], cfg.values["database"]
    );
    let (client, io) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(io);
    (c, conn, client)
}

#[tokio::test]
async fn catalog_lists_relations_and_columns() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let (_c, conn, admin) = connect().await;
    admin
        .batch_execute(
            r#"CREATE SCHEMA "s p";
               CREATE TABLE "s p"."T 1"(id int, "Name" text);
               CREATE VIEW "s p".v AS SELECT id FROM "s p"."T 1";"#,
        )
        .await
        .unwrap();
    let cat = conn.relational().unwrap().catalog("s p").await.unwrap();
    assert_eq!(cat.schema, "s p");
    assert_eq!(cat.relations.len(), 2);
    let t = &cat.relations[0];
    assert_eq!((t.name.as_str(), t.kind), ("T 1", RelationKind::Table));
    let cols: Vec<_> = t
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_name.as_str()))
        .collect();
    assert_eq!(cols, [("id", "integer"), ("Name", "text")]);
    let v = &cat.relations[1];
    assert_eq!((v.name.as_str(), v.kind), ("v", RelationKind::View));
    assert_eq!(v.columns.len(), 1);
    assert_eq!(v.columns[0].name, "id");
}

#[tokio::test]
async fn catalog_of_missing_schema_is_empty() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let (_c, conn, _admin) = connect().await;
    let cat = conn.relational().unwrap().catalog("nope").await.unwrap();
    assert!(cat.relations.is_empty());
}

#[tokio::test]
async fn pg_reserved_keywords_are_known() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let (_c, _conn, admin) = connect().await;
    let rows = admin
        .query(
            "SELECT upper(word), catcode::text FROM pg_get_keywords()",
            &[],
        )
        .await
        .unwrap();
    assert!(!rows.is_empty());
    for row in rows {
        let word: String = row.get(0);
        let catcode: String = row.get(1);
        assert!(
            PG_KEYWORDS.contains(&word.as_str()),
            "{word} missing from PG_KEYWORDS"
        );
        if catcode == "R" || catcode == "T" {
            assert!(
                PG_RESERVED.contains(&word.as_str()),
                "{word} missing from PG_RESERVED"
            );
        }
    }
}
