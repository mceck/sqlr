//! Object introspection and DDL generation against MySQL 8.4 and MariaDB 11.4
//!. Every check runs on both servers.

mod support;

use std::sync::Arc;

use sqlr_core::{
    ColumnDef, Connection, ConstraintDef, DdlGen, Driver, Execution, FkAction, Identity,
    IndexColumn, IndexDef, Introspect, ObjectDef, ObjectKind, ObjectRef, SchemaChange, TableDef,
    TableRef, diff_table,
};
use sqlr_driver_mysql::MysqlDriver;
use support::{Container, Server, start_isolated};

macro_rules! suite {
    ($($name:ident),* $(,)?) => {
        mod mysql84 {
            $(#[tokio::test(flavor = "multi_thread")]
            async fn $name() {
                sqlr_testkit::skip_container_tests!();
                super::$name(super::Server::MySql).await
            })*
        }
        mod mariadb114 {
            $(#[tokio::test(flavor = "multi_thread")]
            async fn $name() {
                sqlr_testkit::skip_container_tests!();
                super::$name(super::Server::MariaDb).await
            })*
        }
    };
}

suite!(
    round_trip_every_creatable_kind,
    ddl_awkward_names_per_dialect,
    alter_column_type_nullability_default,
    add_drop_fk_check_unique_index,
    rename_table_column_index,
    comments_round_trip,
    mysql_options_engine_charset,
    functions_procedures_triggers_round_trip,
    on_update_without_default_round_trips,
);

struct Env {
    _c: Container,
    server: Server,
    conn: Arc<dyn Connection>,
}

impl Env {
    async fn new(server: Server, setup: &[&str]) -> Self {
        let (c, cfg, sec) = start_isolated(server).await;
        let conn = MysqlDriver::new()
            .connect(&cfg, &sec)
            .await
            .unwrap_or_else(|e| panic!("connect: {e:?}"));
        let env = Self {
            _c: c,
            server,
            conn,
        };
        env.run(setup).await;
        env
    }

    async fn run<S: AsRef<str>>(&self, statements: &[S]) {
        let rel = self.conn.relational().unwrap();
        for sql in statements {
            let sql = sql.as_ref();
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
        match self.load(&obj(ObjectKind::Table, "shop", name)).await {
            ObjectDef::Table(t) => t,
            other => panic!("{other:?}"),
        }
    }

    async fn apply(&self, changes: &[SchemaChange]) -> Vec<String> {
        let statements = self.generator().ddl(changes).unwrap();
        self.run(&statements).await;
        statements
    }

    /// The type MariaDB prints with a display width (`int(11)`).
    fn int(&self, ty: &str) -> String {
        match (self.server, ty) {
            (Server::MariaDb, "int") => "int(11)".into(),
            (Server::MariaDb, "bigint") => "bigint(20)".into(),
            _ => ty.into(),
        }
    }
}

fn t(name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some("shop".into()),
        name: name.into(),
    }
}

fn obj(kind: ObjectKind, schema: &str, name: &str) -> ObjectRef {
    ObjectRef {
        kind,
        database: None,
        schema: Some(schema.into()),
        name: name.into(),
        parent: None,
        signature: None,
    }
}

const OBJECTS: &[&str] = &[
    "CREATE DATABASE shop CHARACTER SET utf8mb4",
    "CREATE DATABASE other_db CHARACTER SET latin1 COLLATE latin1_bin",
    "CREATE TABLE shop.customers (id INT PRIMARY KEY, email VARCHAR(100) NOT NULL UNIQUE)",
    "CREATE TABLE shop.orders (
        id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
        customer INT NOT NULL,
        total DECIMAL(10,2) NOT NULL DEFAULT 0 COMMENT 'it''s money',
        note VARCHAR(50) COLLATE utf8mb4_bin DEFAULT 'n/a',
        path VARCHAR(20) DEFAULT 'C:\\\\tmp',
        doubled DECIMAL(12,2) GENERATED ALWAYS AS (total * 2) STORED,
        created DATETIME DEFAULT CURRENT_TIMESTAMP,
        updated TIMESTAMP NULL DEFAULT NULL ON UPDATE CURRENT_TIMESTAMP,
        CONSTRAINT orders_customer FOREIGN KEY (customer) REFERENCES shop.customers (id) ON DELETE CASCADE,
        CONSTRAINT orders_total CHECK (total >= 0),
        KEY orders_note (note(10), total DESC)
     ) ENGINE=InnoDB COMMENT='All orders'",
    "CREATE INDEX orders_created ON shop.orders (created)",
    "CREATE VIEW shop.big_orders AS SELECT id, total FROM shop.orders WHERE total > 100",
    "CREATE FUNCTION shop.add_one(a INT) RETURNS INT DETERMINISTIC COMMENT 'adds' RETURN a + 1",
    "CREATE PROCEDURE shop.reset(IN x INT, OUT n INT) MODIFIES SQL DATA
     BEGIN
         DELETE FROM shop.orders WHERE id = x;
         SET n = ROW_COUNT();
     END",
    "CREATE TRIGGER shop.orders_bi BEFORE INSERT ON shop.orders FOR EACH ROW
     BEGIN
         IF NEW.note IS NULL THEN SET NEW.note = 'none'; END IF;
     END",
];

/// The Round-trip rule for every kind the manifest says it can create.
async fn round_trip_every_creatable_kind(server: Server) {
    let env = Env::new(server, OBJECTS).await;
    let editing = MysqlDriver::new().manifest().editing.clone().unwrap();
    let orders = t("orders");
    let database = |name: &str| ObjectRef {
        schema: None,
        ..obj(ObjectKind::Database, "", name)
    };
    let cases: Vec<(ObjectRef, &str)> = vec![
        (
            obj(ObjectKind::Trigger, "shop", "orders_bi"),
            "DROP TRIGGER shop.orders_bi",
        ),
        (
            ObjectRef::child(ObjectKind::Index, &orders, "orders_created"),
            "DROP INDEX orders_created ON shop.orders",
        ),
        (
            obj(ObjectKind::View, "shop", "big_orders"),
            "DROP VIEW shop.big_orders",
        ),
        (
            obj(ObjectKind::Function, "shop", "add_one"),
            "DROP FUNCTION shop.add_one",
        ),
        (
            obj(ObjectKind::Procedure, "shop", "reset"),
            "DROP PROCEDURE shop.reset",
        ),
        (
            obj(ObjectKind::Table, "shop", "orders"),
            "DROP TABLE shop.orders",
        ),
        (database("other_db"), "DROP DATABASE other_db"),
    ];
    let mut covered = Vec::new();
    for (object, drop) in &cases {
        let before = env.load(object).await;
        let statements = env
            .generator()
            .ddl(&[SchemaChange::Create(before.clone())])
            .unwrap();
        env.run(&[*drop]).await;
        env.run(&statements).await;
        let after = env.load(object).await;
        assert_eq!(after, before, "{object:?}\n{statements:#?}");
        covered.push(object.kind);
    }
    for kind in &editing.create {
        assert!(covered.contains(kind), "{kind:?} has no round trip");
    }
    assert!(!editing.transactional_ddl);

    // The table brought back its indexes, trigger and options.
    let orders = env.table("orders").await;
    assert_eq!(orders.triggers.len(), 1);
    assert_eq!(orders.option("engine"), Some("InnoDB"));
    let note = orders.column("note").unwrap();
    assert_eq!(note.collation.as_deref(), Some("utf8mb4_bin"));
    let id = orders.column("id").unwrap();
    assert_eq!(id.identity, Some(Identity::AutoIncrement));
    let index = orders
        .indexes
        .iter()
        .find(|i| i.name == "orders_note")
        .unwrap();
    assert_eq!(index.columns[0].prefix, Some(10));
    assert!(index.columns[1].desc);
}

async fn ddl_awkward_names_per_dialect(server: Server) {
    let env = Env::new(server, &["CREATE DATABASE `Sch \"q\" ü`"]).await;
    let schema = "Sch \"q\" ü";
    let table = TableRef {
        database: None,
        schema: Some(schema.into()),
        name: "Wé \"ird\" 'name' `bt`".into(),
    };
    let parent_ref = TableRef {
        name: "Párent".into(),
        ..table.clone()
    };
    let int = env.int("int");
    let col = |name: &str, nullable| ColumnDef {
        name: name.into(),
        type_name: int.clone(),
        nullable,
        ..Default::default()
    };
    let mut parent = TableDef::new(parent_ref.clone());
    parent.columns = vec![col("ID", false)];
    parent.constraints.push(ConstraintDef::PrimaryKey {
        name: None,
        columns: vec!["ID".into()],
    });
    let mut def = TableDef::new(table.clone());
    def.columns = vec![
        col("select", false),
        ColumnDef {
            type_name: "varchar(30)".into(),
            comment: Some("it's\nmulti-line \\ \"quoted\"".into()),
            default: Some("'it''s'".into()),
            ..col("MixedCase", true)
        },
        col("ünï code", true),
        col("a`b", true),
    ];
    def.constraints = vec![
        ConstraintDef::PrimaryKey {
            name: None,
            columns: vec!["select".into()],
        },
        ConstraintDef::Unique {
            name: Some("uq 'u'".into()),
            columns: vec!["ünï code".into(), "a`b".into()],
        },
        ConstraintDef::ForeignKey {
            name: Some("fk;drop".into()),
            columns: vec!["a`b".into()],
            target: parent_ref.clone(),
            target_columns: vec!["ID".into()],
            on_update: FkAction::Restrict,
            on_delete: FkAction::SetNull,
            deferrable: false,
            initially_deferred: false,
        },
        ConstraintDef::Check {
            name: Some("chk 'c'".into()),
            expression: "`select` > 0".into(),
        },
    ];
    def.indexes = vec![
        IndexDef {
            name: "fk;drop".into(),
            columns: vec![IndexColumn::column("a`b")],
            ..Default::default()
        },
        IndexDef {
            name: "idx \"x\"".into(),
            columns: vec![IndexColumn {
                desc: true,
                ..IndexColumn::column("MixedCase")
            }],
            ..Default::default()
        },
    ];
    def.comment = Some("O'Reilly;\n-- not a comment \\".into());
    env.apply(&[
        SchemaChange::Create(ObjectDef::Table(parent)),
        SchemaChange::Create(ObjectDef::Table(def.clone())),
    ])
    .await;
    let loaded = match env
        .load(&ObjectRef::relation(ObjectKind::Table, &table))
        .await
    {
        ObjectDef::Table(t) => t,
        other => panic!("{other:?}"),
    };
    // The server normalizes check expressions; everything else comes back as written.
    let check = loaded
        .constraints
        .iter()
        .find_map(|c| match c {
            ConstraintDef::Check { expression, .. } => Some(expression.clone()),
            _ => None,
        })
        .unwrap();
    assert!(check.contains("`select` > 0"), "{check}");
    let mut expected = def.clone();
    expected.constraints[3] = ConstraintDef::Check {
        name: Some("chk 'c'".into()),
        expression: check,
    };
    expected.options = loaded.options.clone();
    assert_eq!(loaded, expected);

    // Rename a column and drop the table with the same names.
    let mut renamed = loaded.clone();
    renamed.columns[1].name = "Other `one`".into();
    renamed.indexes[1].columns[0].expr = "Other `one`".into();
    env.apply(&diff_table(&loaded, &renamed)).await;
    let reloaded = env
        .load(&ObjectRef::relation(ObjectKind::Table, &table))
        .await;
    assert_eq!(reloaded, ObjectDef::Table(renamed));
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

async fn alter_column_type_nullability_default(server: Server) {
    let env = Env::new(
        server,
        &[
            "CREATE DATABASE shop",
            "CREATE TABLE shop.t (id INT PRIMARY KEY, a TEXT, b INT NOT NULL, c INT DEFAULT 7)",
            "INSERT INTO shop.t VALUES (1, 'x', 2, 3)",
        ],
    )
    .await;
    let before = env.table("t").await;
    assert_eq!(before.columns[3].default.as_deref(), Some("7"));
    let mut after = before.clone();
    after.columns[1].type_name = "varchar(20)".into();
    after.columns[1].nullable = false;
    after.columns[2].nullable = true;
    after.columns[2].default = Some("42".into());
    after.columns[3].default = None;
    after.columns[3].type_name = env.int("bigint");
    env.apply(&diff_table(&before, &after)).await;
    assert_eq!(env.table("t").await, after);
    env.apply(&diff_table(&after, &before)).await;
    assert_eq!(env.table("t").await, before);
}

async fn add_drop_fk_check_unique_index(server: Server) {
    let env = Env::new(
        server,
        &[
            "CREATE DATABASE shop",
            "CREATE TABLE shop.p (id INT PRIMARY KEY)",
            "CREATE TABLE shop.c (id INT PRIMARY KEY, p_id INT, qty INT, code VARCHAR(10))",
        ],
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
        deferrable: false,
        initially_deferred: false,
    });
    after.constraints.push(ConstraintDef::Check {
        name: Some("c_qty_check".into()),
        expression: "`qty` > 0".into(),
    });
    after.indexes.push(IndexDef {
        name: "c_p_fk".into(),
        columns: vec![IndexColumn::column("p_id")],
        ..Default::default()
    });
    after.indexes.push(IndexDef {
        name: "c_qty".into(),
        columns: vec![IndexColumn::column("qty")],
        ..Default::default()
    });
    env.apply(&diff_table(&before, &after)).await;
    let loaded = env.table("c").await;
    let mut expected = after.clone();
    // The server writes the check back in its own form.
    expected.constraints[3] = loaded.constraints[3].clone();
    assert!(
        matches!(&loaded.constraints[3], ConstraintDef::Check { expression, .. } if expression.contains("`qty` > 0"))
    );
    assert_eq!(loaded, expected);
    env.apply(&diff_table(&loaded, &before)).await;
    assert_eq!(env.table("c").await, before);
}

async fn rename_table_column_index(server: Server) {
    let env = Env::new(
        server,
        &[
            "CREATE DATABASE shop",
            "CREATE TABLE shop.t (id INT PRIMARY KEY, a INT, KEY t_a (a))",
        ],
    )
    .await;
    let before = env.table("t").await;
    let mut after = before.clone();
    after.table.name = "t2".into();
    after.columns[1].name = "b".into();
    after.indexes[0].name = "t2_b".into();
    after.indexes[0].columns[0].expr = "b".into();
    let changes = diff_table(&before, &after);
    assert_eq!(changes.len(), 3, "{changes:#?}");
    env.apply(&changes).await;
    assert_eq!(env.table("t2").await, after);
}

async fn comments_round_trip(server: Server) {
    let env = Env::new(
        server,
        &[
            "CREATE DATABASE shop",
            "CREATE TABLE shop.t (id INT, a INT COMMENT 'old')",
        ],
    )
    .await;
    assert!(
        MysqlDriver::new()
            .manifest()
            .editing
            .as_ref()
            .unwrap()
            .comments
    );
    let before = env.table("t").await;
    assert_eq!(before.columns[1].comment.as_deref(), Some("old"));
    let mut after = before.clone();
    after.comment = Some("table's\ncomment \\".into());
    after.columns[0].comment = Some("the id".into());
    after.columns[1].comment = None;
    env.apply(&diff_table(&before, &after)).await;
    assert_eq!(env.table("t").await, after);
    env.apply(&diff_table(&after, &before)).await;
    assert_eq!(env.table("t").await, before);
}

async fn mysql_options_engine_charset(server: Server) {
    let env = Env::new(
        server,
        &[
            "CREATE DATABASE shop CHARACTER SET latin1 COLLATE latin1_swedish_ci",
            "CREATE TABLE shop.t (id INT PRIMARY KEY) ENGINE=MyISAM DEFAULT CHARSET=latin1",
        ],
    )
    .await;
    let before = env.table("t").await;
    assert_eq!(before.option("engine"), Some("MyISAM"));
    assert_eq!(before.option("charset"), Some("latin1"));
    assert_eq!(before.option("collation"), Some("latin1_swedish_ci"));
    let mut after = before.clone();
    after.options = vec![
        ("engine".into(), "InnoDB".into()),
        ("charset".into(), "utf8mb4".into()),
        ("collation".into(), "utf8mb4_bin".into()),
    ];
    env.apply(&diff_table(&before, &after)).await;
    assert_eq!(env.table("t").await, after);

    let shop = ObjectRef {
        schema: None,
        ..obj(ObjectKind::Database, "", "shop")
    };
    match env.load(&shop).await {
        ObjectDef::Database(d) => assert_eq!(
            d.options,
            [
                ("charset".to_string(), "latin1".to_string()),
                ("collation".to_string(), "latin1_swedish_ci".to_string())
            ]
        ),
        other => panic!("{other:?}"),
    }
    env.apply(&[SchemaChange::SetOptions {
        object: shop.clone(),
        options: vec![
            ("charset".into(), Some("utf8mb4".into())),
            ("collation".into(), Some("utf8mb4_bin".into())),
        ],
    }])
    .await;
    match env.load(&shop).await {
        ObjectDef::Database(d) => assert_eq!(
            d.options,
            [
                ("charset".to_string(), "utf8mb4".to_string()),
                ("collation".to_string(), "utf8mb4_bin".to_string())
            ]
        ),
        other => panic!("{other:?}"),
    }
}

async fn functions_procedures_triggers_round_trip(server: Server) {
    let env = Env::new(server, OBJECTS).await;
    let function_ref = obj(ObjectKind::Function, "shop", "add_one");
    let function = match env.load(&function_ref).await {
        ObjectDef::Routine(r) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(function.kind, ObjectKind::Function);
    assert_eq!(function.arguments, format!("`a` {}", env.int("int")));
    assert_eq!(function.returns.as_deref(), Some(env.int("int").as_str()));
    assert_eq!(function.body, "RETURN a + 1");
    assert!(
        function
            .options
            .contains(&("deterministic".to_string(), "true".to_string()))
    );
    assert!(
        function
            .options
            .contains(&("comment".to_string(), "adds".to_string()))
    );
    let procedure_ref = obj(ObjectKind::Procedure, "shop", "reset");
    let procedure = match env.load(&procedure_ref).await {
        ObjectDef::Routine(r) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(procedure.kind, ObjectKind::Procedure);
    assert_eq!(
        procedure.arguments,
        format!("IN `x` {0}, OUT `n` {0}", env.int("int"))
    );
    assert_eq!(procedure.returns, None);
    assert!(procedure.body.contains("SET n = ROW_COUNT();"));
    let trigger_ref = obj(ObjectKind::Trigger, "shop", "orders_bi");
    let trigger = match env.load(&trigger_ref).await {
        ObjectDef::Trigger(t) => t,
        other => panic!("{other:?}"),
    };
    assert_eq!(trigger.table, t("orders"));
    assert_eq!(trigger.timing, "BEFORE");
    assert_eq!(trigger.events, ["INSERT"]);
    assert!(trigger.for_each_row);
    assert!(trigger.body.starts_with("BEGIN"), "{}", trigger.body);

    // Replace the function body.
    let mut replaced = function.clone();
    replaced.body = "RETURN a + 2".into();
    env.apply(&[SchemaChange::ReplaceRoutine(replaced.clone())])
        .await;
    assert_eq!(env.load(&function_ref).await, ObjectDef::Routine(replaced));

    // Drop and recreate all three from their definitions.
    env.run(&[
        "DROP FUNCTION shop.add_one",
        "DROP PROCEDURE shop.reset",
        "DROP TRIGGER shop.orders_bi",
    ])
    .await;
    let defs = [
        ObjectDef::Routine(function),
        ObjectDef::Routine(procedure),
        ObjectDef::Trigger(trigger),
    ];
    let changes: Vec<SchemaChange> = defs.iter().cloned().map(SchemaChange::Create).collect();
    env.apply(&changes).await;
    assert_eq!(env.load(&function_ref).await, defs[0]);
    assert_eq!(env.load(&procedure_ref).await, defs[1]);
    assert_eq!(env.load(&trigger_ref).await, defs[2]);
    env.run(&[
        "INSERT INTO shop.customers VALUES (1, 'a')",
        "INSERT INTO shop.orders (customer, total, note) VALUES (1, 5, NULL)",
    ])
    .await;
    let rel = env.conn.relational().unwrap();
    let Execution::Rows(mut rows) = rel.execute("SELECT note FROM shop.orders").await.unwrap()
    else {
        panic!("rows expected");
    };
    let chunk = rows.next_chunk(10).await.unwrap().unwrap();
    assert_eq!(
        sqlr_core::Value::from_cell(chunk.cell(0, 0)),
        sqlr_core::Value::Text("none".into())
    );
    rows.close().await.unwrap();
}

/// A NOT NULL column with ON UPDATE and no default comes back as it was.
async fn on_update_without_default_round_trips(server: Server) {
    let env = Env::new(
        server,
        &[
            "CREATE DATABASE shop",
            "CREATE TABLE shop.t (id INT PRIMARY KEY, \
             touched TIMESTAMP NOT NULL ON UPDATE CURRENT_TIMESTAMP, \
             seen DATETIME NULL DEFAULT NULL ON UPDATE CURRENT_TIMESTAMP)",
        ],
    )
    .await;
    let before = env.table("t").await;
    let statements = env
        .generator()
        .ddl(&[SchemaChange::Create(ObjectDef::Table(before.clone()))])
        .unwrap();
    env.run(&["DROP TABLE shop.t"]).await;
    env.run(&statements).await;
    assert_eq!(env.table("t").await, before, "{statements:#?}");
}
