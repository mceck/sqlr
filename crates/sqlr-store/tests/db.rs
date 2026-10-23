use sqlr_core::library::*;
use sqlr_core::{CatalogColumn, CatalogRelation, RelationKind, SchemaCatalog};
use sqlr_store::{Db, MAX_HISTORY_SQL_BYTES, Settings, StoreError};

fn entry(conn: &str, sql: &str, at: i64, outcome: HistoryOutcome) -> HistoryEntry {
    HistoryEntry {
        id: 0,
        conn_id: conn.into(),
        database: None,
        sql: sql.into(),
        started_at_ms: at,
        duration_ms: 7,
        outcome,
    }
}

fn snippet(id: &str, name: &str, folder: &str) -> Snippet {
    Snippet {
        id: id.into(),
        name: name.into(),
        folder: folder.into(),
        conn_id: None,
        sql: "select 1".into(),
        updated_at_ms: 1,
    }
}

#[test]
fn history_round_trip_newest_first() {
    let db = Db::open_in_memory().unwrap();
    db.add_history(&entry("a", "q1", 1, HistoryOutcome::Failed("boom".into())))
        .unwrap();
    db.add_history(&entry("a", "q2", 2, HistoryOutcome::Affected(3)))
        .unwrap();
    db.add_history(&entry("a", "q3", 3, HistoryOutcome::Rows(500)))
        .unwrap();
    let got = db.search_history(&HistoryQuery::default()).unwrap();
    let sqls: Vec<_> = got.iter().map(|e| e.sql.as_str()).collect();
    assert_eq!(sqls, ["q3", "q2", "q1"]);
    assert!(got.iter().all(|e| e.id > 0));
    assert_eq!(got[0].outcome, HistoryOutcome::Rows(500));
    assert_eq!(got[1].outcome, HistoryOutcome::Affected(3));
    assert_eq!(got[2].outcome, HistoryOutcome::Failed("boom".into()));
    assert_eq!(got[0].duration_ms, 7);
}

#[test]
fn history_filters_by_text_and_connection() {
    let db = Db::open_in_memory().unwrap();
    db.add_history(&entry(
        "a",
        "select * from orders",
        1,
        HistoryOutcome::Rows(1),
    ))
    .unwrap();
    db.add_history(&entry(
        "b",
        "select * from users",
        2,
        HistoryOutcome::Cancelled,
    ))
    .unwrap();
    let q = HistoryQuery {
        text: "ORDERS".into(),
        ..Default::default()
    };
    let got = db.search_history(&q).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].sql, "select * from orders");
    let q = HistoryQuery {
        conn_id: Some("b".into()),
        ..Default::default()
    };
    let got = db.search_history(&q).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].conn_id, "b");
    assert_eq!(got[0].outcome, HistoryOutcome::Cancelled);
}

#[test]
fn search_escapes_like_wildcards() {
    let db = Db::open_in_memory().unwrap();
    db.add_history(&entry("a", "select '50%'", 1, HistoryOutcome::Rows(1)))
        .unwrap();
    db.add_history(&entry("a", "select 500", 2, HistoryOutcome::Rows(1)))
        .unwrap();
    db.add_history(&entry("a", "select a_b", 3, HistoryOutcome::Rows(1)))
        .unwrap();
    let q = HistoryQuery {
        text: "50%".into(),
        ..Default::default()
    };
    let got = db.search_history(&q).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].sql, "select '50%'");
    let q = HistoryQuery {
        text: "_".into(),
        ..Default::default()
    };
    let got = db.search_history(&q).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].sql, "select a_b");
}

#[test]
fn history_sql_truncated_at_char_boundary() {
    let db = Db::open_in_memory().unwrap();
    let sql = "é".repeat(1 << 20); // 2 MiB
    db.add_history(&entry("a", &sql, 1, HistoryOutcome::Rows(1)))
        .unwrap();
    let got = db.search_history(&HistoryQuery::default()).unwrap();
    assert!(got[0].sql.len() <= MAX_HISTORY_SQL_BYTES);
    assert!(got[0].sql.chars().all(|c| c == 'é'));
}

#[test]
fn history_trimmed_on_request_not_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");
    let db = Db::open_with_keep(&path, 5).unwrap();
    for i in 0..8 {
        db.add_history(&entry("a", &format!("q{i}"), i, HistoryOutcome::Rows(1)))
            .unwrap();
    }
    drop(db);
    // Opening is cheap: the trim is a separate step the engine runs off the UI thread.
    let db = Db::open_with_keep(&path, 5).unwrap();
    assert_eq!(
        db.search_history(&HistoryQuery::default()).unwrap().len(),
        8
    );
    db.trim_history().unwrap();
    let got = db.search_history(&HistoryQuery::default()).unwrap();
    let sqls: Vec<_> = got.iter().map(|e| e.sql.as_str()).collect();
    assert_eq!(sqls, ["q7", "q6", "q5", "q4", "q3"]);
}

#[cfg(unix)]
#[test]
fn the_database_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");
    let _db = Db::open_with_keep(&path, 5).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn old_password_rows_are_scrubbed_on_trim() {
    let db = Db::open_in_memory().unwrap();
    db.add_history(&entry(
        "a",
        "CREATE USER app IDENTIFIED BY 'S3cret'",
        1,
        HistoryOutcome::Affected(1),
    ))
    .unwrap();
    db.add_history(&entry("a", "select 1", 2, HistoryOutcome::Rows(1)))
        .unwrap();
    let mut secret = snippet("s", "secret", "");
    secret.sql = "ALTER ROLE app WITH PASSWORD 'S3cret'".into();
    db.save_snippet(&secret).unwrap();
    db.save_snippet(&snippet("ok", "ok", "")).unwrap();
    db.trim_history().unwrap();
    let got = db.search_history(&HistoryQuery::default()).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].sql, "select 1");
    let snippets = db.list_snippets().unwrap();
    assert!(snippets.iter().all(|s| s.id == "ok"), "{snippets:?}");
}

#[test]
fn snippets_upsert_and_delete() {
    let db = Db::open_in_memory().unwrap();
    db.save_snippet(&snippet("1", "zeta", "b")).unwrap();
    db.save_snippet(&snippet("2", "alpha", "b")).unwrap();
    db.save_snippet(&snippet("3", "mid", "")).unwrap();
    db.save_snippet(&snippet("1", "renamed", "b")).unwrap();
    let got = db.list_snippets().unwrap();
    let names: Vec<_> = got.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["mid", "alpha", "renamed"]);
    db.delete_snippet("1").unwrap();
    db.delete_snippet("2").unwrap();
    db.delete_snippet("3").unwrap();
    assert!(db.list_snippets().unwrap().is_empty());
}

#[test]
fn catalog_cache_round_trip() {
    let db = Db::open_in_memory().unwrap();
    let cat = SchemaCatalog {
        schema: "public".into(),
        relations: vec![CatalogRelation {
            name: "t".into(),
            kind: RelationKind::Table,
            columns: vec![CatalogColumn {
                name: "id".into(),
                type_name: "int4".into(),
            }],
        }],
    };
    db.save_catalog_schemas("c1", &["public".into(), "s".into()])
        .unwrap();
    db.save_schema_catalog("c1", &cat).unwrap();
    let got = db.load_catalogs().unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].conn_id, "c1");
    assert_eq!(
        got[0].schemas,
        Some(vec!["public".to_string(), "s".to_string()])
    );
    assert_eq!(got[0].schema, vec![cat]);
    db.delete_catalog("c1").unwrap();
    assert!(db.load_catalogs().unwrap().is_empty());
}

#[test]
fn persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("sqlr.db");
    let db = Db::open(&path).unwrap();
    db.add_history(&entry("a", "q", 1, HistoryOutcome::Rows(1)))
        .unwrap();
    db.save_snippet(&snippet("1", "n", "")).unwrap();
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(
        db.search_history(&HistoryQuery::default()).unwrap().len(),
        1
    );
    assert_eq!(db.list_snippets().unwrap().len(), 1);
}

#[test]
fn open_garbage_file_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");
    std::fs::write(&path, vec![0xABu8; 4096]).unwrap();
    assert!(matches!(Db::open(&path), Err(StoreError::Db(_))));
}

#[test]
fn two_handles_share_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");
    let a = Db::open(&path).unwrap();
    let b = Db::open(&path).unwrap();
    a.add_history(&entry("a", "from a", 1, HistoryOutcome::Rows(1)))
        .unwrap();
    b.add_history(&entry("a", "from b", 2, HistoryOutcome::Rows(1)))
        .unwrap();
    assert_eq!(a.search_history(&HistoryQuery::default()).unwrap().len(), 2);
    assert_eq!(b.search_history(&HistoryQuery::default()).unwrap().len(), 2);
}

#[test]
fn read_only_handle_fails_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");
    drop(Db::open(&path).unwrap());
    let db = Db::open_read_only(&path).unwrap();
    assert!(matches!(
        db.add_history(&entry("a", "q", 1, HistoryOutcome::Rows(1))),
        Err(StoreError::Db(_))
    ));
}

#[test]
fn settings_old_file_gets_defaults() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("settings.toml"), "page_size = 100\n").unwrap();
    let s = Settings::load(dir.path()).unwrap();
    assert_eq!(s.page_size, 100);
    assert!(s.stop_script_on_error);
}

#[test]
fn recent_round_trip_and_order() {
    let db = Db::open_in_memory().unwrap();
    assert!(db.recent(8).unwrap().is_empty());
    db.touch_recent("a", 1).unwrap();
    db.touch_recent("b", 2).unwrap();
    db.touch_recent("c", 3).unwrap();
    // Touching again moves a connection to the front, once.
    db.touch_recent("a", 4).unwrap();
    assert_eq!(
        db.recent(8).unwrap(),
        [("a".into(), 4), ("c".into(), 3), ("b".into(), 2)]
    );
    assert_eq!(db.recent(2).unwrap(), [("a".into(), 4), ("c".into(), 3)]);
    db.forget_recent("c").unwrap();
    db.forget_recent("missing").unwrap();
    assert_eq!(db.recent(8).unwrap(), [("a".into(), 4), ("b".into(), 2)]);
}

#[test]
fn recent_survives_reopening_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");
    Db::open(&path).unwrap().touch_recent("a", 7).unwrap();
    assert_eq!(
        Db::open(&path).unwrap().recent(8).unwrap(),
        [("a".into(), 7)]
    );
}

#[test]
fn history_database_column_migrates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");
    // A version 1 database: the history table without the `database` column.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE history (id INTEGER PRIMARY KEY, conn_id TEXT NOT NULL, sql TEXT NOT NULL,
               started_at INTEGER NOT NULL, duration_ms INTEGER NOT NULL, outcome TEXT NOT NULL,
               count INTEGER, error TEXT);
             INSERT INTO history (conn_id, sql, started_at, duration_ms, outcome, count)
               VALUES ('a', 'old', 1, 1, 'rows', 1);
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let db = Db::open(&path).unwrap();
    let old = db.search_history(&HistoryQuery::default()).unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].database, None, "old rows have no database");
    let mut new = entry("a", "new", 2, HistoryOutcome::Rows(1));
    new.database = Some("analytics".into());
    db.add_history(&new).unwrap();
    let all = db.search_history(&HistoryQuery::default()).unwrap();
    assert_eq!(all[0].database.as_deref(), Some("analytics"));
    assert_eq!(all[1].database, None);
}
