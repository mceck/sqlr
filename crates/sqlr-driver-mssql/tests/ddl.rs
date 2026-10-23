//! DDL written by the SQL Server generator, applied to SQL Server 2025.

mod support;

use sqlr_core::{
    ColumnDef, Connection, ConstraintDef, DdlGen, Dialect, IndexColumn, IndexDef, ObjectDef,
    ObjectKind, ObjectRef, SchemaChange, TableDef, TableRef,
};
use sqlr_driver_mssql::MsDialect;
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

fn shop(name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some("shop".into()),
        name: name.into(),
    }
}

async fn run(conn: &dyn Connection, statements: &[String]) {
    let rel = conn.relational().unwrap();
    for sql in statements {
        rel.execute(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}\n{e:?}"));
    }
}

async fn table(conn: &dyn Connection, name: &str) -> TableDef {
    let snapshot = conn
        .relational()
        .unwrap()
        .introspect()
        .unwrap()
        .load_schema(None, "shop")
        .await
        .unwrap();
    snapshot
        .objects
        .into_iter()
        .find_map(|o| match o {
            ObjectDef::Table(t) if t.table.name == name => Some(t),
            _ => None,
        })
        .unwrap_or_else(|| panic!("table {name}"))
}

fn column(name: &str, type_name: &str, nullable: bool) -> ColumnDef {
    ColumnDef {
        name: name.into(),
        type_name: type_name.into(),
        nullable,
        ..Default::default()
    }
}

/// A copy of `t` named `{name}_copy`, its constraints and indexes renamed to match.
fn copy_of(t: &TableDef) -> TableDef {
    let rename = |n: &Option<String>| n.as_ref().map(|n| format!("{n}_copy"));
    let mut copy = t.clone();
    copy.table.name = format!("{}_copy", t.table.name);
    for c in &mut copy.constraints {
        match c {
            ConstraintDef::PrimaryKey { name, .. }
            | ConstraintDef::Unique { name, .. }
            | ConstraintDef::Check { name, .. } => *name = rename(name),
            ConstraintDef::ForeignKey { name, target, .. } => {
                *name = rename(name);
                target.name = format!("{}_copy", target.name);
            }
            ConstraintDef::Raw { .. } => {}
        }
    }
    for i in &mut copy.indexes {
        i.name = format!("{}_copy", i.name);
    }
    copy
}

#[tokio::test]
async fn created_tables_and_views_read_back_as_they_were() {
    let (_c, conn) = server().await;
    run(
        &*conn,
        &SETUP.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )
    .await;
    let gen_ = MsDialect;
    let customers = table(&*conn, "customers").await;
    let orders = table(&*conn, "orders").await;
    // Parents first, as a migration would.
    for t in [&customers, &orders] {
        let sql = gen_.create_ddl(&ObjectDef::Table(copy_of(t))).unwrap();
        run(&*conn, &[sql]).await;
    }
    let views = conn
        .relational()
        .unwrap()
        .introspect()
        .unwrap()
        .load_schema(None, "shop")
        .await
        .unwrap();
    let ObjectDef::View(view) = views
        .objects
        .iter()
        .find(|o| matches!(o, ObjectDef::View(v) if v.view.name == "big_orders"))
        .unwrap()
        .clone()
    else {
        unreachable!()
    };
    let mut copy = view.clone();
    copy.view.name = "big_orders_copy".into();
    copy.query = copy.query.replace("shop.orders", "shop.orders_copy");
    run(&*conn, &[gen_.create_ddl(&ObjectDef::View(copy)).unwrap()]).await;

    for original in [&customers, &orders] {
        let copied = table(&*conn, &format!("{}_copy", original.table.name)).await;
        assert_eq!(copied, copy_of(original), "{}", original.table.name);
    }
    let again = conn
        .relational()
        .unwrap()
        .introspect()
        .unwrap()
        .load_schema(None, "shop")
        .await
        .unwrap();
    assert!(again.objects.iter().any(
        |o| matches!(o, ObjectDef::View(v) if v.view.name == "big_orders_copy"
            && v.query.contains("total > 100"))
    ));
}

#[tokio::test]
async fn table_changes_apply() {
    let (_c, conn) = server().await;
    run(
        &*conn,
        &SETUP.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )
    .await;
    let g = MsDialect;
    let customers = shop("customers");
    let before = table(&*conn, "customers").await;
    let age = before.column("age").unwrap().clone();

    // SQL Server refuses a type change under a check constraint: it goes first.
    let changes = vec![
        SchemaChange::DropConstraint {
            table: customers.clone(),
            name: "ck_age".into(),
        },
        SchemaChange::AddColumn {
            table: customers.clone(),
            column: ColumnDef {
                default: Some("(N'new')".into()),
                ..column("status", "nvarchar(10)", false)
            },
        },
        // The type changes under a default constraint; the default is replaced too.
        SchemaChange::AlterColumn {
            table: customers.clone(),
            before: age.clone(),
            after: ColumnDef {
                type_name: "bigint".into(),
                default: Some("((21))".into()),
                ..age.clone()
            },
        },
        SchemaChange::AlterColumn {
            table: customers.clone(),
            before: column("email", "nvarchar(120)", false),
            after: column("mail", "nvarchar(120)", false),
        },
        SchemaChange::AddIndex {
            table: customers.clone(),
            index: IndexDef {
                name: "ix_age".into(),
                columns: vec![IndexColumn::column("age")],
                ..Default::default()
            },
            concurrently: false,
        },
    ];
    run(&*conn, &g.ddl(&changes).unwrap()).await;
    let after = table(&*conn, "customers").await;
    let names: Vec<_> = after.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "mail", "age", "status"]);
    assert_eq!(after.column("age").unwrap().type_name, "bigint");
    assert_eq!(
        after.column("age").unwrap().default.as_deref(),
        Some("((21))")
    );
    assert_eq!(
        after.column("status").unwrap().default.as_deref(),
        Some("(N'new')")
    );
    assert!(after.indexes.iter().any(|i| i.name == "ix_age"));
    assert!(!after.constraints.iter().any(|c| c.name() == Some("ck_age")));

    // Dropping a column with a default, renaming a table and an index, dropping an index.
    let changes = vec![
        SchemaChange::DropColumn {
            table: customers.clone(),
            name: "status".into(),
        },
        SchemaChange::Rename {
            object: ObjectRef::child(ObjectKind::Index, &customers, "ix_age"),
            to: "ix_age2".into(),
        },
        SchemaChange::Rename {
            object: ObjectRef::relation(ObjectKind::Table, &customers),
            to: "clients".into(),
        },
    ];
    run(&*conn, &g.ddl(&changes).unwrap()).await;
    let clients = table(&*conn, "clients").await;
    assert!(clients.column("status").is_none());
    assert!(clients.indexes.iter().any(|i| i.name == "ix_age2"));
    run(
        &*conn,
        &g.ddl(&[SchemaChange::DropIndex {
            table: shop("clients"),
            name: "ix_age2".into(),
        }])
        .unwrap(),
    )
    .await;
    assert!(table(&*conn, "clients").await.indexes.is_empty());

    // Views are replaced in place, and an empty table can be truncated and dropped.
    let view_changes = vec![
        SchemaChange::ReplaceView(sqlr_core::ViewDef {
            view: shop("big_orders"),
            query: "SELECT id FROM shop.orders".into(),
            materialized: false,
            columns: vec![],
            comment: None,
            options: vec![],
            indexes: vec![],
        }),
        SchemaChange::Truncate {
            table: shop("orders"),
            cascade: false,
        },
        SchemaChange::Drop {
            object: ObjectRef::relation(ObjectKind::View, &shop("big_orders")),
            cascade: false,
        },
    ];
    run(&*conn, &g.ddl(&view_changes).unwrap()).await;
    let _ = g.quote_ident("x");
}
