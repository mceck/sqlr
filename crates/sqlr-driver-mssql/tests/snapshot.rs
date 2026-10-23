//! A whole schema read from SQL Server 2025: tables, keys, indexes, checks and views.

mod support;

use sqlr_core::{ConstraintDef, FkAction, ObjectDef, ObjectKind, ObjectRef};
use support::server;

const SETUP: &[&str] = &[
    "CREATE SCHEMA shop",
    "CREATE TABLE shop.customers (id int IDENTITY(1,1) NOT NULL, email nvarchar(120) NOT NULL, \
     age int NULL CONSTRAINT df_age DEFAULT 18, \
     CONSTRAINT pk_customers PRIMARY KEY (id), CONSTRAINT uq_email UNIQUE (email), \
     CONSTRAINT ck_age CHECK (age >= 0))",
    "CREATE TABLE shop.orders (id int NOT NULL PRIMARY KEY, customer_id int NOT NULL, \
     total decimal(10,2) NOT NULL, note varchar(max) NULL, \
     CONSTRAINT fk_orders_customer FOREIGN KEY (customer_id) REFERENCES shop.customers (id) \
     ON DELETE CASCADE)",
    "CREATE INDEX ix_orders_total ON shop.orders (total DESC) INCLUDE (note)",
    "CREATE VIEW shop.big_orders AS SELECT id, total FROM shop.orders WHERE total > 100",
];

#[tokio::test]
async fn a_schema_loads_with_its_keys_indexes_and_views() {
    let (_c, conn) = server().await;
    let rel = conn.relational().unwrap();
    for sql in SETUP {
        rel.execute(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
    }
    let snapshot = rel
        .introspect()
        .expect("introspection")
        .load_schema(None, "shop")
        .await
        .unwrap();
    let table = |name: &str| {
        snapshot
            .objects
            .iter()
            .find_map(|o| match o {
                ObjectDef::Table(t) if t.table.name == name => Some(t.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("table {name}"))
    };

    let customers = table("customers");
    let names: Vec<_> = customers.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "email", "age"]);
    assert_eq!(customers.columns[1].type_name, "nvarchar(120)");
    assert!(customers.columns[0].identity.is_some() && !customers.columns[0].nullable);
    assert!(customers.columns[2].nullable);
    assert_eq!(customers.columns[2].default.as_deref(), Some("((18))"));
    assert_eq!(customers.primary_key(), Some(&["id".to_string()][..]));
    assert!(customers.constraints.iter().any(|c| matches!(
        c, ConstraintDef::Unique { columns, .. } if columns == &["email".to_string()])));
    assert!(customers.constraints.iter().any(|c| matches!(
        c, ConstraintDef::Check { name, .. } if name.as_deref() == Some("ck_age"))));

    let orders = table("orders");
    let fk = orders
        .constraints
        .iter()
        .find_map(|c| match c {
            ConstraintDef::ForeignKey {
                columns,
                target,
                target_columns,
                on_delete,
                ..
            } => Some((columns, target, target_columns, *on_delete)),
            _ => None,
        })
        .expect("foreign key");
    assert_eq!(fk.0, &["customer_id".to_string()]);
    assert_eq!(fk.1.name, "customers");
    assert_eq!(fk.1.schema.as_deref(), Some("shop"));
    assert_eq!(fk.2, &["id".to_string()]);
    assert_eq!(fk.3, FkAction::Cascade);
    assert_eq!(orders.columns[2].type_name, "decimal(10,2)");
    assert_eq!(orders.columns[3].type_name, "varchar(max)");
    let ix = orders
        .indexes
        .iter()
        .find(|i| i.name == "ix_orders_total")
        .expect("index");
    assert!(ix.columns[0].desc && ix.columns[0].expr == "total");
    assert_eq!(ix.include, ["note"]);
    // The primary key is a constraint, not an index.
    assert!(orders.indexes.iter().all(|i| !i.name.starts_with("PK_")));

    let view = snapshot
        .objects
        .iter()
        .find_map(|o| match o {
            ObjectDef::View(v) if v.view.name == "big_orders" => Some(v),
            _ => None,
        })
        .expect("view");
    assert!(view.query.contains("total > 100"), "{}", view.query);
    assert_eq!(view.columns, ["id", "total"]);

    // One object by reference, and a schema nobody has.
    let object = rel
        .introspect()
        .unwrap()
        .load_object(&ObjectRef {
            kind: ObjectKind::Table,
            database: None,
            schema: Some("shop".into()),
            name: "orders".into(),
            parent: None,
            signature: None,
        })
        .await
        .unwrap();
    assert!(matches!(object, ObjectDef::Table(t) if t.table.name == "orders"));
    let empty = rel
        .introspect()
        .unwrap()
        .load_schema(None, "nope")
        .await
        .unwrap();
    assert!(empty.objects.is_empty());
}
