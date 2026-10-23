//! Object introspection, DDL generation and the table rebuild against SQLite files
//!.

use std::sync::Arc;

use sqlr_core::{
    ColumnDef, Connection, ConnectionConfig, ConstraintDef, DdlGen, Driver, Execution, FkAction,
    Identity, IndexColumn, IndexDef, Introspect, ObjectDef, ObjectKind, ObjectRef, Placement,
    SchemaChange, Secrets, TableDef, TableRef, TriggerDef, Value, ViewDef,
};
use sqlr_driver_sqlite::SqliteDriver;

struct Env {
    _dir: tempfile::TempDir,
    conn: Arc<dyn Connection>,
}

impl Env {
    async fn new(setup: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ConnectionConfig {
            id: "t".into(),
            name: "test".into(),
            driver: "sqlite".into(),
            group: None,
            color: None,
            values: [
                ("path", dir.path().join("t.db").display().to_string()),
                ("create_if_missing", "true".into()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
            profile: None,
            save_secrets: true,
        };
        let conn = SqliteDriver::new()
            .connect(&cfg, &Secrets::default())
            .await
            .unwrap();
        let env = Self { _dir: dir, conn };
        for statement in setup.split(";\n").filter(|s| !s.trim().is_empty()) {
            env.run(&[statement.to_string()]).await;
        }
        env
    }

    async fn run(&self, statements: &[String]) {
        let rel = self.conn.relational().unwrap();
        for sql in statements {
            match rel.execute(sql).await {
                Ok(Execution::Rows(mut s)) => {
                    while s.next_chunk(1000).await.unwrap().is_some() {}
                    s.close().await.unwrap();
                }
                Ok(Execution::Affected(_)) => {}
                Err(e) => panic!("{sql}\n{e:?}"),
            }
        }
    }

    async fn query(&self, sql: &str) -> Vec<Vec<Value>> {
        let rel = self.conn.relational().unwrap();
        let Execution::Rows(mut s) = rel.execute(sql).await.unwrap() else {
            panic!("{sql}: rows expected");
        };
        let mut out = Vec::new();
        while let Some(chunk) = s.next_chunk(1000).await.unwrap() {
            for r in 0..chunk.len() {
                out.push(
                    (0..chunk.columns().len())
                        .map(|c| Value::from_cell(chunk.cell(r, c)))
                        .collect(),
                );
            }
        }
        s.close().await.unwrap();
        out
    }

    fn introspect(&self) -> &dyn Introspect {
        self.conn.relational().unwrap().introspect().unwrap()
    }

    fn generator(&self) -> &dyn DdlGen {
        self.conn.relational().unwrap().dialect().ddl_gen().unwrap()
    }

    async fn load(&self, object: &ObjectRef) -> ObjectDef {
        self.introspect()
            .load_object(object)
            .await
            .unwrap_or_else(|e| panic!("{object:?}: {e:?}"))
    }

    async fn table(&self, name: &str) -> TableDef {
        match self.load(&obj(ObjectKind::Table, name)).await {
            ObjectDef::Table(t) => t,
            other => panic!("{other:?}"),
        }
    }

    /// Applies `before` → `after` in one transaction, as the engine does: a leading or
    /// trailing `PRAGMA foreign_keys` runs outside it.
    async fn alter(&self, before: &TableDef, after: &TableDef) -> Vec<String> {
        let statements = self.generator().table_ddl(before, after).unwrap();
        self.apply_placed(&statements)
            .await
            .unwrap_or_else(|e| panic!("{e:?}\n{statements:#?}"));
        statements
    }

    /// Applies generated statements by the rule on `DdlGen`: runs of
    /// `Placement::Transaction` statements in one transaction, `Alone` ones by themselves;
    /// after a failure (rolled back) only `Cleanup` statements still run.
    async fn apply_placed(&self, statements: &[String]) -> Result<(), sqlr_core::DriverError> {
        let rel = self.conn.relational().unwrap();
        let mut open = false;
        let mut failure = None;
        for s in statements {
            let placement = self.generator().placement(s);
            if failure.is_some() {
                if placement == Placement::Cleanup {
                    self.run(std::slice::from_ref(s)).await;
                }
                continue;
            }
            let in_transaction = placement == Placement::Transaction;
            if in_transaction != open {
                self.run(&[if open { "COMMIT" } else { "BEGIN" }.to_string()])
                    .await;
                open = in_transaction;
            }
            if let Err(e) = rel.execute(s).await {
                if open {
                    self.run(&["ROLLBACK".to_string()]).await;
                    open = false;
                }
                failure = Some(e);
            }
        }
        if open {
            self.run(&["COMMIT".to_string()]).await;
        }
        failure.map_or(Ok(()), Err)
    }

    async fn apply(&self, changes: &[SchemaChange]) {
        let statements = self.generator().ddl(changes).unwrap();
        self.run(&statements).await;
    }
}

fn t(name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some("main".into()),
        name: name.into(),
    }
}

fn obj(kind: ObjectKind, name: &str) -> ObjectRef {
    ObjectRef::relation(kind, &t(name))
}

fn col(name: &str, ty: &str, nullable: bool) -> ColumnDef {
    ColumnDef {
        name: name.into(),
        type_name: ty.into(),
        nullable,
        ..Default::default()
    }
}

const OBJECTS: &str = "CREATE TABLE parent (id INTEGER PRIMARY KEY, name TEXT);
CREATE TABLE log (msg TEXT);
CREATE TABLE items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_id INTEGER NOT NULL REFERENCES parent (id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    -- a comment inside the definition
    name TEXT COLLATE NOCASE DEFAULT 'it''s' CHECK (length(name) > 0),
    qty NUMERIC(10, 2) DEFAULT (1 + 1),
    created TEXT DEFAULT CURRENT_TIMESTAMP,
    code TEXT UNIQUE,
    shout TEXT GENERATED ALWAYS AS (upper(name)) VIRTUAL,
    CONSTRAINT qty_positive CHECK (qty >= 0)
);
CREATE INDEX items_name ON items (lower(name), qty DESC) WHERE qty > 0;
CREATE UNIQUE INDEX items_pair ON items (parent_id, code);
CREATE VIEW big_items (item, amount) AS SELECT id, qty FROM items WHERE qty > 10;
CREATE TRIGGER items_log AFTER UPDATE OF qty, name ON items FOR EACH ROW WHEN new.qty > 5
BEGIN INSERT INTO log VALUES ('big ' || new.id); END;
CREATE TABLE kv (k TEXT PRIMARY KEY, v BLOB) WITHOUT ROWID, STRICT";

#[tokio::test]
async fn round_trip_every_creatable_kind() {
    let env = Env::new(OBJECTS).await;
    let editing = SqliteDriver::new().manifest().editing.clone().unwrap();
    assert!(!editing.comments);
    assert!(editing.transactional_ddl);
    let items = t("items");
    let cases = [
        (
            ObjectRef::child(ObjectKind::Trigger, &items, "items_log"),
            "DROP TRIGGER items_log",
        ),
        (
            ObjectRef::child(ObjectKind::Index, &items, "items_name"),
            "DROP INDEX items_name",
        ),
        (obj(ObjectKind::View, "big_items"), "DROP VIEW big_items"),
        (obj(ObjectKind::Table, "items"), "DROP TABLE items"),
        (obj(ObjectKind::Table, "kv"), "DROP TABLE kv"),
    ];
    let mut covered = Vec::new();
    for (object, drop) in &cases {
        let before = env.load(object).await;
        let statements = env
            .generator()
            .ddl(&[SchemaChange::Create(before.clone())])
            .unwrap();
        env.run(&[drop.to_string()]).await;
        env.run(&statements).await;
        let after = env.load(object).await;
        assert_eq!(after, before, "{object:?}\n{statements:#?}");
        covered.push(object.kind);
    }
    for kind in &editing.create {
        assert!(covered.contains(kind), "{kind:?} has no round trip");
    }

    // The parsed definition, column constraints normalized to table constraints.
    let items = env.table("items").await;
    let id = items.column("id").unwrap();
    assert_eq!(id.identity, Some(Identity::AutoIncrement));
    let name = items.column("name").unwrap();
    assert_eq!(name.collation.as_deref(), Some("NOCASE"));
    assert_eq!(name.default.as_deref(), Some("'it''s'"));
    assert_eq!(items.column("qty").unwrap().type_name, "NUMERIC(10, 2)");
    assert_eq!(
        items.column("qty").unwrap().default.as_deref(),
        Some("(1 + 1)")
    );
    let shout = items.column("shout").unwrap();
    assert_eq!(shout.generated.as_deref(), Some("upper(name)"));
    assert!(!shout.generated_stored);
    assert!(items.constraints.contains(&ConstraintDef::ForeignKey {
        name: None,
        columns: vec!["parent_id".into()],
        target: t("parent"),
        target_columns: vec!["id".into()],
        on_update: FkAction::NoAction,
        on_delete: FkAction::Cascade,
        deferrable: true,
        initially_deferred: true,
    }));
    assert!(items.constraints.contains(&ConstraintDef::Check {
        name: Some("qty_positive".into()),
        expression: "qty >= 0".into(),
    }));
    assert!(items.constraints.contains(&ConstraintDef::Unique {
        name: None,
        columns: vec!["code".into()],
    }));
    let index = items
        .indexes
        .iter()
        .find(|i| i.name == "items_name")
        .unwrap();
    assert_eq!(
        index.columns,
        [
            IndexColumn {
                expr: "(lower(name))".into(),
                desc: false,
                prefix: None
            },
            IndexColumn {
                expr: "qty".into(),
                desc: true,
                prefix: None
            },
        ]
    );
    assert_eq!(index.predicate.as_deref(), Some("qty > 0"));
    assert_eq!(
        items.triggers,
        [TriggerDef {
            name: "items_log".into(),
            table: t("items"),
            timing: "AFTER".into(),
            events: vec!["UPDATE OF qty, name".into()],
            for_each_row: true,
            condition: Some("new.qty > 5".into()),
            body: "INSERT INTO log VALUES ('big ' || new.id);".into(),
        }]
    );
    match env.load(&obj(ObjectKind::View, "big_items")).await {
        ObjectDef::View(v) => assert_eq!(
            v,
            ViewDef {
                view: t("big_items"),
                query: "SELECT id, qty FROM items WHERE qty > 10".into(),
                materialized: false,
                columns: vec!["item".into(), "amount".into()],
                comment: None,
                options: Vec::new(),
                indexes: Vec::new(),
            }
        ),
        other => panic!("{other:?}"),
    }
    let kv = env.table("kv").await;
    assert_eq!(
        kv.options,
        [
            ("without_rowid".to_string(), "true".to_string()),
            ("strict".to_string(), "true".to_string())
        ]
    );
}

#[tokio::test]
async fn ddl_awkward_names_per_dialect() {
    let env = Env::new("CREATE TABLE \"Párent\" (\"ID\" INTEGER PRIMARY KEY)").await;
    let table = t("Wé \"ird\" 'name'");
    let mut def = TableDef::new(table.clone());
    def.columns = vec![
        col("select", "INTEGER", false),
        ColumnDef {
            default: Some("'it''s'".into()),
            ..col("MixedCase", "TEXT", true)
        },
        col("ünï code", "INTEGER", true),
        col("a\"b", "INTEGER", true),
    ];
    def.constraints = vec![
        ConstraintDef::PrimaryKey {
            name: Some("pk \"x\"".into()),
            columns: vec!["select".into()],
        },
        ConstraintDef::Unique {
            name: Some("uq 'u'".into()),
            columns: vec!["ünï code".into(), "a\"b".into()],
        },
        ConstraintDef::ForeignKey {
            name: Some("fk;drop".into()),
            columns: vec!["a\"b".into()],
            target: t("Párent"),
            target_columns: vec!["ID".into()],
            on_update: FkAction::NoAction,
            on_delete: FkAction::SetNull,
            deferrable: false,
            initially_deferred: false,
        },
        ConstraintDef::Check {
            name: Some("chk 'c'".into()),
            expression: "\"select\" > 0".into(),
        },
    ];
    def.indexes = vec![IndexDef {
        name: "idx \"x\"".into(),
        columns: vec![IndexColumn {
            desc: true,
            ..IndexColumn::column("MixedCase")
        }],
        ..Default::default()
    }];
    env.apply(&[SchemaChange::Create(ObjectDef::Table(def.clone()))])
        .await;
    assert_eq!(
        env.load(&ObjectRef::relation(ObjectKind::Table, &table))
            .await,
        ObjectDef::Table(def.clone())
    );
    // A rename in place (native) and a drop with the same names.
    let mut renamed = def.clone();
    renamed.columns[1].name = "Other \"one\"".into();
    renamed.indexes[0].columns[0].expr = "Other \"one\"".into();
    let statements = env.alter(&def, &renamed).await;
    assert_eq!(statements.len(), 1, "{statements:#?}");
    assert_eq!(
        env.load(&ObjectRef::relation(ObjectKind::Table, &table))
            .await,
        ObjectDef::Table(renamed)
    );
    env.apply(&[SchemaChange::Drop {
        object: ObjectRef::relation(ObjectKind::Table, &table),
        cascade: false,
    }])
    .await;
    assert!(
        env.introspect()
            .load_object(&ObjectRef::relation(ObjectKind::Table, &table))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn alter_column_type_nullability_default() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b INTEGER NOT NULL, c INTEGER DEFAULT 7);
INSERT INTO t VALUES (1, '12', 2, 3)",
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    after.columns[1].type_name = "INTEGER".into();
    after.columns[1].nullable = false;
    after.columns[2].nullable = true;
    after.columns[2].default = Some("42".into());
    after.columns[3].default = None;
    env.alter(&before, &after).await;
    assert_eq!(env.table("t").await, after);
    assert_eq!(
        env.query("SELECT a, typeof(a), b, c FROM t").await,
        [vec![
            Value::Int(12),
            Value::Text("integer".into()),
            Value::Int(2),
            Value::Int(3)
        ]]
    );
    env.alter(&after, &before).await;
    assert_eq!(env.table("t").await, before);
}

#[tokio::test]
async fn add_drop_fk_check_unique_index() {
    let env = Env::new(
        "CREATE TABLE p (id INTEGER PRIMARY KEY);
CREATE TABLE c (id INTEGER PRIMARY KEY, p_id INTEGER, qty INTEGER, code TEXT);
INSERT INTO p VALUES (1);
INSERT INTO c VALUES (1, 1, 5, 'a')",
    )
    .await;
    let before = env.table("c").await;
    let mut after = before.clone();
    after.constraints.push(ConstraintDef::Unique {
        name: Some("c_code_key".into()),
        columns: vec!["code".into()],
    });
    after.constraints.push(ConstraintDef::ForeignKey {
        name: Some("c_p_fk".into()),
        columns: vec!["p_id".into()],
        target: t("p"),
        target_columns: vec!["id".into()],
        on_update: FkAction::Cascade,
        on_delete: FkAction::Restrict,
        deferrable: true,
        initially_deferred: false,
    });
    after.constraints.push(ConstraintDef::Check {
        name: Some("c_qty_check".into()),
        expression: "qty > 0".into(),
    });
    after.indexes.push(IndexDef {
        name: "c_qty".into(),
        columns: vec![IndexColumn::column("qty")],
        ..Default::default()
    });
    env.alter(&before, &after).await;
    assert_eq!(env.table("c").await, after);
    assert!(env.query("PRAGMA foreign_key_check").await.is_empty());
    env.alter(&after, &before).await;
    assert_eq!(env.table("c").await, before);
    assert_eq!(
        env.query("SELECT count(*) FROM c").await,
        [vec![Value::Int(1)]]
    );
}

#[tokio::test]
async fn rename_table_column_index() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a INTEGER);
CREATE INDEX t_a ON t (a)",
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    after.table.name = "t2".into();
    after.columns[1].name = "b".into();
    after.indexes[0].name = "t2_b".into();
    after.indexes[0].columns[0].expr = "b".into();
    let statements = env.alter(&before, &after).await;
    // Native ALTER TABLE: no rebuild.
    assert!(
        statements.iter().all(|s| !s.starts_with("CREATE TABLE")),
        "{statements:#?}"
    );
    assert_eq!(env.table("t2").await, after);
}

#[tokio::test]
async fn sqlite_rebuild_preserves_everything() {
    let env = Env::new(
        "CREATE TABLE parent (id INTEGER PRIMARY KEY, name TEXT);
CREATE TABLE log (msg TEXT);
CREATE TABLE t (id INTEGER NOT NULL, code TEXT NOT NULL, qty TEXT, legacy TEXT, owner INTEGER, PRIMARY KEY (id));
CREATE INDEX t_legacy ON t (legacy);
CREATE INDEX t_code ON t (code DESC);
CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES ('added ' || new.id); END;
CREATE VIEW v AS SELECT id, code, qty FROM t;
CREATE TABLE child (id INTEGER PRIMARY KEY, t_id INTEGER REFERENCES t (id));
INSERT INTO parent VALUES (1, 'p1'), (2, 'p2');
INSERT INTO t VALUES (1, 'a', '10', 'x', 1), (2, 'b', '20', 'y', 2), (3, 'c', NULL, 'z', NULL);
INSERT INTO child VALUES (1, 1), (2, 3)",
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    // Drop a column used by an index (and the index), change a type, add a foreign key,
    // change the primary key (the old key stays unique for `child`).
    after.columns.retain(|c| c.name != "legacy");
    after.indexes.retain(|i| i.name != "t_legacy");
    after.columns[2].type_name = "INTEGER".into();
    after.constraints = vec![
        ConstraintDef::PrimaryKey {
            name: None,
            columns: vec!["id".into(), "code".into()],
        },
        ConstraintDef::Unique {
            name: None,
            columns: vec!["id".into()],
        },
        ConstraintDef::ForeignKey {
            name: Some("t_owner".into()),
            columns: vec!["owner".into()],
            target: t("parent"),
            target_columns: vec!["id".into()],
            on_update: FkAction::NoAction,
            on_delete: FkAction::NoAction,
            deferrable: false,
            initially_deferred: false,
        },
    ];
    // Run inside one transaction with foreign keys still on, the rebuild refuses to drop
    // the table (its ON DELETE actions would run) and the transaction rolls back.
    let statements = env.generator().table_ddl(&before, &after).unwrap();
    let rel = env.conn.relational().unwrap();
    env.run(&["BEGIN".to_string()]).await;
    let mut failure = None;
    for s in &statements {
        if let Err(e) = rel.execute(s).await {
            failure = Some(e);
            break;
        }
    }
    env.run(&["ROLLBACK".to_string()]).await;
    let failure = failure.expect("the guard must fail");
    assert!(
        failure
            .message
            .contains("foreign keys must be off to rebuild a table"),
        "{failure:?}"
    );
    assert_eq!(env.table("t").await, before);

    let statements = env.alter(&before, &after).await;
    assert!(
        statements.iter().any(|s| s.starts_with("CREATE TABLE")),
        "a rebuild: {statements:#?}"
    );
    assert_eq!(
        env.query("PRAGMA foreign_keys").await,
        [vec![Value::Int(1)]]
    );
    assert_eq!(env.table("t").await, after);
    assert_eq!(
        env.query("SELECT id, code, qty, typeof(qty), owner FROM t ORDER BY id")
            .await,
        [
            vec![
                Value::Int(1),
                Value::Text("a".into()),
                Value::Int(10),
                Value::Text("integer".into()),
                Value::Int(1)
            ],
            vec![
                Value::Int(2),
                Value::Text("b".into()),
                Value::Int(20),
                Value::Text("integer".into()),
                Value::Int(2)
            ],
            vec![
                Value::Int(3),
                Value::Text("c".into()),
                Value::Null,
                Value::Text("null".into()),
                Value::Null
            ],
        ]
    );
    // Indexes, triggers and views survive; foreign keys hold.
    let indexes = env
        .query("SELECT name FROM sqlite_schema WHERE type = 'index' AND tbl_name = 't' AND sql IS NOT NULL ORDER BY name")
        .await;
    assert_eq!(indexes, [vec![Value::Text("t_code".into())]]);
    env.run(&["INSERT INTO t VALUES (4, 'd', 5, 2)".to_string()])
        .await;
    // The setup's inserts logged rows 1–3; copying the rows fired nothing, the recreated
    // trigger fires for row 4.
    let log: Vec<Vec<Value>> = ["added 1", "added 2", "added 3", "added 4"]
        .iter()
        .map(|m| vec![Value::Text(m.to_string())])
        .collect();
    assert_eq!(env.query("SELECT msg FROM log ORDER BY rowid").await, log);
    assert_eq!(
        env.query("SELECT count(*) FROM v").await,
        [vec![Value::Int(4)]]
    );
    assert!(env.query("PRAGMA foreign_key_check").await.is_empty());
    assert_eq!(
        env.query("PRAGMA integrity_check").await,
        [vec![Value::Text("ok".into())]]
    );
    assert!(
        env.query("SELECT name FROM sqlite_schema WHERE name LIKE 'sqlr%'")
            .await
            .is_empty()
    );
}

/// Dropping an unnamed constraint (no `DropConstraint` names it) still rebuilds the table.
#[tokio::test]
async fn dropping_an_unnamed_constraint_rebuilds() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, code TEXT, UNIQUE (code), CHECK (id > 0));
INSERT INTO t VALUES (1, 'a')",
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    after
        .constraints
        .retain(|c| matches!(c, ConstraintDef::PrimaryKey { .. }));
    env.alter(&before, &after).await;
    assert_eq!(env.table("t").await, after);
    env.run(&["INSERT INTO t VALUES (2, 'a')".to_string()])
        .await;
}

/// The foreign keys a rebuild adds are checked before it commits.
#[tokio::test]
async fn rebuild_refuses_violated_foreign_keys() {
    let env = Env::new(
        "CREATE TABLE p (id INTEGER PRIMARY KEY);
CREATE TABLE c (id INTEGER PRIMARY KEY, p_id INTEGER);
INSERT INTO p VALUES (1);
INSERT INTO c VALUES (1, 1), (2, 99)",
    )
    .await;
    let before = env.table("c").await;
    let mut after = before.clone();
    after.constraints.push(ConstraintDef::ForeignKey {
        name: None,
        columns: vec!["p_id".into()],
        target: t("p"),
        target_columns: vec!["id".into()],
        on_update: FkAction::NoAction,
        on_delete: FkAction::NoAction,
        deferrable: false,
        initially_deferred: false,
    });
    let statements = env.generator().table_ddl(&before, &after).unwrap();
    let err = env.apply_placed(&statements).await.unwrap_err();
    assert!(err.message.contains("foreign key"), "{err:?}");
    assert_eq!(env.table("c").await, before);
    assert_eq!(
        env.query("SELECT count(*) FROM c").await,
        [vec![Value::Int(2)]]
    );
    // The cleanup turned enforcement back on.
    assert_eq!(
        env.query("PRAGMA foreign_keys").await,
        [vec![Value::Int(1)]]
    );
}

/// Views naming the rebuilt table are created again and must still work: a rebuild that
/// would break one fails.
#[tokio::test]
async fn rebuild_refuses_to_break_a_view() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b TEXT);
CREATE VIEW v AS SELECT id, b FROM t;
INSERT INTO t VALUES (1, 'x', 'y')",
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    after.columns.retain(|c| c.name != "b");
    after.columns[1].type_name = "INTEGER".into();
    let statements = env.generator().table_ddl(&before, &after).unwrap();
    assert!(statements.iter().any(|s| s.starts_with("CREATE TABLE")));
    let err = env.apply_placed(&statements).await.unwrap_err();
    assert!(err.message.contains('b'), "{err:?}");
    assert_eq!(env.table("t").await, before);
    assert_eq!(
        env.query("SELECT * FROM v").await,
        [vec![Value::Int(1), Value::Text("y".into())]]
    );
}

/// A trigger on another table that names the rebuilt table is recreated from its old SQL;
/// if a column it uses was dropped, the transaction must not commit a trigger that fails
/// only when it fires.
#[tokio::test]
async fn rebuild_does_not_commit_a_dependent_trigger_that_names_a_dropped_column() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b TEXT);
CREATE TABLE audit (id INTEGER PRIMARY KEY);
CREATE TRIGGER audit_t AFTER INSERT ON audit BEGIN INSERT INTO t (b) VALUES ((SELECT a FROM t WHERE id = new.id)); END;
INSERT INTO t VALUES (1, 'x', 'y')",
    )
    .await;
    let before = env.table("t").await;
    assert!(
        before.dependents.iter().any(|d| d.name == "audit_t"),
        "{:#?}",
        before.dependents
    );
    let mut after = before.clone();
    after.columns.retain(|c| c.name != "a");
    // Force the rebuild path instead of a native DROP COLUMN.
    after.columns[1].type_name = "INTEGER".into();
    let statements = env.generator().table_ddl(&before, &after).unwrap();
    assert!(statements.iter().any(|s| s.starts_with("CREATE TABLE")));
    let applied = env.apply_placed(&statements).await;
    if applied.is_ok() {
        // The rebuilt schema claims success: the dependent trigger must still work.
        env.run(&["INSERT INTO audit (id) VALUES (2)".to_string()])
            .await;
    } else {
        // The rollback must leave the original table and its trigger working.
        assert_eq!(env.table("t").await, before);
        env.run(&["INSERT INTO audit (id) VALUES (2)".to_string()])
            .await;
    }
}

/// `AUTOINCREMENT` never reuses an id, even after every row was deleted. A rebuild must
/// carry the sequence high-water mark, or new rows reuse the ids of deleted ones.
#[tokio::test]
async fn rebuild_preserves_the_autoincrement_high_water_mark() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, a TEXT, b TEXT);
INSERT INTO t (a, b) VALUES ('one', 'x'), ('two', 'y'), ('three', 'z');
DELETE FROM t",
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    // An empty table still rebuilds when a column type changes.
    after.columns[1].type_name = "INTEGER".into();
    env.alter(&before, &after).await;
    env.run(&["INSERT INTO t (a, b) VALUES ('four', 4)".to_string()])
        .await;
    assert_eq!(env.query("SELECT id FROM t").await, [vec![Value::Int(4)]]);
}

/// The refusal names the broken trigger, and the rollback keeps the old table and trigger.
#[tokio::test]
async fn rebuild_refuses_a_dependent_trigger_that_names_a_dropped_column() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b TEXT);
CREATE TABLE audit (id INTEGER PRIMARY KEY);
CREATE TRIGGER audit_t AFTER INSERT ON audit BEGIN UPDATE t SET b = a WHERE id = new.id; END;
INSERT INTO t VALUES (1, 'x', 'y')",
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    after.columns.retain(|c| c.name != "a");
    after.columns[1].type_name = "INTEGER".into();
    let statements = env.generator().table_ddl(&before, &after).unwrap();
    let err = env.apply_placed(&statements).await.unwrap_err();
    assert!(
        err.message.contains("audit_t") && err.message.contains("no such column: a"),
        "{err:?}"
    );
    assert_eq!(env.table("t").await, before);
    env.run(&["INSERT INTO audit (id) VALUES (1)".to_string()])
        .await;
    assert_eq!(
        env.query("SELECT b FROM t").await,
        [vec![Value::Text("x".into())]]
    );
}

/// A dependent trigger that only uses kept columns is recreated and still fires.
#[tokio::test]
async fn rebuild_keeps_a_dependent_trigger_that_still_works() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b TEXT);
CREATE TABLE audit (id INTEGER PRIMARY KEY);
CREATE TRIGGER audit_t AFTER INSERT ON audit BEGIN UPDATE t SET b = 'seen' WHERE id = new.id; END;
INSERT INTO t VALUES (1, 'x', 'y')",
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    after.columns.retain(|c| c.name != "a");
    after.columns[1].type_name = "INTEGER".into();
    env.alter(&before, &after).await;
    env.run(&["INSERT INTO audit (id) VALUES (1)".to_string()])
        .await;
    assert_eq!(
        env.query("SELECT b FROM t").await,
        [vec![Value::Text("seen".into())]]
    );
    assert!(
        env.query("SELECT name FROM sqlite_schema WHERE name LIKE 'sqlr%'")
            .await
            .is_empty()
    );
}

/// The table's own triggers are created from the new definition: one that names a
/// dropped column is refused too.
#[tokio::test]
async fn rebuild_refuses_an_own_trigger_that_names_a_dropped_column() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b TEXT);
CREATE TABLE log (msg TEXT);
CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (new.a); END;
INSERT INTO t VALUES (1, 'x', 'y')",
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    after.columns.retain(|c| c.name != "a");
    after.columns[1].type_name = "INTEGER".into();
    let statements = env.generator().table_ddl(&before, &after).unwrap();
    let err = env.apply_placed(&statements).await.unwrap_err();
    assert!(err.message.contains("t_ai"), "{err:?}");
    assert_eq!(env.table("t").await, before);
}

/// A non-empty `AUTOINCREMENT` table keeps a high-water mark above its largest id.
#[tokio::test]
async fn rebuild_preserves_the_autoincrement_mark_of_a_non_empty_table() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, a TEXT, b TEXT);
INSERT INTO t (a, b) VALUES ('one', 'x'), ('two', 'y'), ('three', 'z'), ('four', 'w');
DELETE FROM t WHERE id > 2",
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    after.columns[1].type_name = "INTEGER".into();
    after.table.name = "renamed".into();
    env.alter(&before, &after).await;
    env.run(&["INSERT INTO renamed (a, b) VALUES (5, 'v')".to_string()])
        .await;
    assert_eq!(
        env.query("SELECT id FROM renamed ORDER BY id").await,
        [
            vec![Value::Int(1)],
            vec![Value::Int(2)],
            vec![Value::Int(5)]
        ]
    );
    assert_eq!(
        env.query("SELECT name, seq FROM sqlite_sequence").await,
        [vec![Value::Text("renamed".into()), Value::Int(5)]]
    );
}

/// A rebuild combined with a table rename, on awkward names: views and other tables'
/// triggers follow the new name.
#[tokio::test]
async fn rebuild_with_rename_and_awkward_names() {
    let env = Env::new(
        "CREATE TABLE \"Wé \"\"ird\"\" 'n'\" (\"ID\" INTEGER PRIMARY KEY, \"ünï code\" TEXT, \"a\"\"b\" TEXT);
CREATE INDEX \"idx \"\"x\"\"\" ON \"Wé \"\"ird\"\" 'n'\" (\"ünï code\");
CREATE VIEW \"v \"\"1\"\"\" AS SELECT \"ID\", \"ünï code\" FROM \"Wé \"\"ird\"\" 'n'\";
CREATE TABLE log (msg TEXT);
CREATE TRIGGER log_ai AFTER INSERT ON log BEGIN UPDATE \"Wé \"\"ird\"\" 'n'\" SET \"a\"\"b\" = new.msg; END;
INSERT INTO \"Wé \"\"ird\"\" 'n'\" VALUES (1, '7', 'x')",
    )
    .await;
    let before = env.table("Wé \"ird\" 'n'").await;
    let mut after = before.clone();
    after.table.name = "Nëw \"name\"".into();
    after.columns[1].type_name = "INTEGER".into();
    after.columns[1].nullable = false;
    let statements = env.alter(&before, &after).await;
    assert!(statements.iter().any(|s| s.starts_with("CREATE TABLE")));
    let loaded = env.table("Nëw \"name\"").await;
    // The view and the other table's trigger now name the table by its new name.
    assert_eq!(loaded.dependents.len(), 2, "{:#?}", loaded.dependents);
    for dependent in &loaded.dependents {
        assert!(
            dependent.sql.contains("\"Nëw \"\"name\"\"\""),
            "{}",
            dependent.sql
        );
    }
    assert_eq!(
        TableDef {
            dependents: Vec::new(),
            ..loaded
        },
        TableDef {
            dependents: Vec::new(),
            ..after
        }
    );
    assert_eq!(
        env.query("SELECT * FROM \"v \"\"1\"\"\"").await,
        [vec![Value::Int(1), Value::Int(7)]]
    );
    env.run(&["INSERT INTO log VALUES ('z')".to_string()]).await;
    assert_eq!(
        env.query("SELECT \"a\"\"b\" FROM \"Nëw \"\"name\"\"\"")
            .await,
        [vec![Value::Text("z".into())]]
    );
    assert!(env.query("PRAGMA foreign_key_check").await.is_empty());
    assert_eq!(
        env.query("PRAGMA integrity_check").await,
        [vec![Value::Text("ok".into())]]
    );
}

#[tokio::test]
async fn view_and_trigger_changes() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);
INSERT INTO t VALUES (1, 'x');
CREATE VIEW tv AS SELECT id FROM t",
    )
    .await;
    let view = match env.load(&obj(ObjectKind::View, "tv")).await {
        ObjectDef::View(v) => v,
        other => panic!("{other:?}"),
    };
    let mut replaced = view.clone();
    replaced.query = "SELECT id, v FROM t".into();
    replaced.columns = vec!["id".into(), "v".into()];
    env.apply(&[SchemaChange::ReplaceView(replaced.clone())])
        .await;
    assert_eq!(
        env.load(&obj(ObjectKind::View, "tv")).await,
        ObjectDef::View(replaced)
    );
    let trigger = TriggerDef {
        name: "t_bd".into(),
        table: t("t"),
        timing: "BEFORE".into(),
        events: vec!["DELETE".into()],
        for_each_row: true,
        condition: None,
        body: "SELECT RAISE(ABORT, 'no deletes');".into(),
    };
    env.apply(&[SchemaChange::Create(ObjectDef::Trigger(trigger.clone()))])
        .await;
    assert_eq!(
        env.load(&ObjectRef::child(ObjectKind::Trigger, &t("t"), "t_bd"))
            .await,
        ObjectDef::Trigger(trigger)
    );
    let err = env
        .conn
        .relational()
        .unwrap()
        .execute("DELETE FROM t")
        .await
        .err()
        .unwrap();
    assert!(err.message.contains("no deletes"), "{err:?}");
}

/// A column stops being generated: the rebuild copies its stored values instead of
/// losing them (a plain column cannot be recomputed).
#[tokio::test]
async fn rebuild_keeps_the_values_of_a_column_that_stops_being_generated() {
    let env = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a INTEGER, twice INTEGER GENERATED ALWAYS AS (a * 2) STORED);
INSERT INTO t (id, a) VALUES (1, 10), (2, 21)",
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    let twice = after
        .columns
        .iter_mut()
        .find(|c| c.name == "twice")
        .expect("the generated column");
    twice.generated = None;
    twice.generated_stored = false;
    env.alter(&before, &after).await;
    assert_eq!(
        env.query("SELECT id, twice FROM t ORDER BY id").await,
        [
            vec![Value::Int(1), Value::Int(20)],
            vec![Value::Int(2), Value::Int(42)],
        ]
    );
}
