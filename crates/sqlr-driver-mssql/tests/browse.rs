//! The explorer tree, table descriptions, the autocomplete catalog and value decoding,
//! all through the `Driver`/`Connection` traits against SQL Server 2025.

mod support;

use std::sync::Arc;

use sqlr_core::{
    Connection, Execution, Node, NodeKind, NodePath, ObjectKind, RowStream, TableRef, Value,
};
use support::server;

const SETUP: &[&str] = &[
    "CREATE SCHEMA shop",
    "CREATE TABLE shop.customers (id int IDENTITY(1,1) NOT NULL, email nvarchar(120) NOT NULL, \
     age int NULL CONSTRAINT df_age DEFAULT 18, full_name AS (email + N'!'), \
     CONSTRAINT pk_customers PRIMARY KEY (id), CONSTRAINT uq_email UNIQUE (email))",
    "CREATE TABLE shop.orders (id int NOT NULL PRIMARY KEY, customer_id int NOT NULL, \
     total decimal(10,2) NOT NULL, note varchar(max) NULL, code char(4) NULL, \
     raw varbinary(16) NULL, tag nchar(3) NULL)",
    "CREATE INDEX ix_orders_total ON shop.orders (total)",
    "CREATE VIEW shop.big_orders AS SELECT id, total FROM shop.orders WHERE total > 100",
    "CREATE SEQUENCE shop.seq_a START WITH 1",
    "CREATE FUNCTION shop.f_one() RETURNS int AS BEGIN RETURN 1 END",
    "CREATE PROCEDURE shop.p_noop AS BEGIN SET NOCOUNT ON END",
];

async fn collect(mut stream: Box<dyn RowStream>) -> Vec<Vec<Value>> {
    let mut rows = Vec::new();
    while let Some(chunk) = stream.next_chunk(1000).await.unwrap() {
        for row in 0..chunk.len() {
            rows.push(
                (0..chunk.columns().len())
                    .map(|col| Value::from_cell(chunk.cell(row, col)))
                    .collect(),
            );
        }
    }
    stream.close().await.unwrap();
    rows
}

async fn rows(conn: &Arc<dyn Connection>, sql: &str) -> Vec<Vec<Value>> {
    match conn.relational().unwrap().execute(sql).await.unwrap() {
        Execution::Rows(s) => collect(s).await,
        Execution::Affected(n) => panic!("{sql}: affected {n}"),
    }
}

fn path(segs: &[&str]) -> NodePath {
    segs.iter().fold(NodePath::root(), |p, s| p.child(s))
}

async fn names(conn: &Arc<dyn Connection>, segs: &[&str]) -> Vec<Node> {
    conn.browse(&path(segs)).await.unwrap()
}

fn text(v: &Value) -> String {
    match v {
        Value::Text(t) => t.clone(),
        other => panic!("not text: {other:?}"),
    }
}

#[tokio::test]
async fn the_explorer_tree_walks_schemas_down_to_columns() {
    let (_c, conn) = server().await;
    let rel = conn.relational().unwrap();
    for sql in SETUP {
        rel.execute(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
    }
    rel.execute("CREATE TRIGGER shop.trg_orders ON shop.orders AFTER INSERT AS SET NOCOUNT ON")
        .await
        .unwrap();
    rel.execute("DISABLE TRIGGER shop.trg_orders ON shop.orders")
        .await
        .unwrap();
    rel.execute("INSERT INTO shop.customers (email) VALUES (N'a@x'), (N'b@x')")
        .await
        .unwrap();

    let schemas = names(&conn, &[]).await;
    assert_eq!(schemas[0].name, "dbo");
    assert!(schemas.iter().any(|n| n.name == "shop"));
    assert!(schemas.iter().all(|n| n.kind == NodeKind::Schema));

    let folders = names(&conn, &["shop"]).await;
    let f: Vec<_> = folders.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        f,
        ["tables", "views", "functions", "procedures", "sequences"]
    );

    let tables = names(&conn, &["shop", "tables"]).await;
    let t: Vec<_> = tables.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(t, ["customers", "orders"]);
    assert_eq!(tables[0].detail.as_deref(), Some("2 rows"));
    assert_eq!(tables[1].detail.as_deref(), Some("0 rows"));

    let views = names(&conn, &["shop", "views"]).await;
    assert_eq!(views[0].name, "big_orders");
    assert_eq!(views[0].kind, NodeKind::View);
    let view_cols = names(&conn, &["shop", "views", "big_orders"]).await;
    assert_eq!(view_cols.len(), 2);

    let funcs = names(&conn, &["shop", "functions"]).await;
    assert_eq!(funcs[0].name, "f_one");
    assert_eq!(funcs[0].kind, NodeKind::Function);
    let procs = names(&conn, &["shop", "procedures"]).await;
    assert_eq!(procs[0].name, "p_noop");
    assert_eq!(procs[0].kind, NodeKind::Procedure);
    let seqs = names(&conn, &["shop", "sequences"]).await;
    assert_eq!(seqs[0].name, "seq_a");
    assert_eq!(seqs[0].kind, NodeKind::Sequence);

    let sub = names(&conn, &["shop", "tables", "orders"]).await;
    let s: Vec<_> = sub.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(s, ["columns", "indexes", "triggers"]);

    let cols = names(&conn, &["shop", "tables", "customers", "columns"]).await;
    let details: Vec<_> = cols
        .iter()
        .map(|n| (n.name.as_str(), n.detail.clone().unwrap()))
        .collect();
    assert_eq!(details[0], ("id", "int NOT NULL".to_string()));
    assert_eq!(details[1], ("email", "nvarchar(120) NOT NULL".to_string()));
    assert_eq!(details[2], ("age", "int".to_string()));

    let idx = names(&conn, &["shop", "tables", "orders", "indexes"]).await;
    assert_eq!(
        idx[0].detail.as_deref(),
        Some("CLUSTERED UNIQUE PRIMARY KEY")
    );
    assert_eq!(idx[1].name, "ix_orders_total");
    assert_eq!(idx[1].detail.as_deref(), Some("NONCLUSTERED"));
    let trg = names(&conn, &["shop", "tables", "orders", "triggers"]).await;
    assert_eq!(trg[0].name, "trg_orders");
    assert_eq!(trg[0].detail.as_deref(), Some("disabled"));
    rel.execute("ENABLE TRIGGER shop.trg_orders ON shop.orders")
        .await
        .unwrap();
    let trg = names(&conn, &["shop", "tables", "orders", "triggers"]).await;
    assert_eq!(trg[0].detail, None);

    let err = conn
        .browse(&path(&["shop", "nonsense", "x", "y"]))
        .await
        .unwrap_err();
    assert_eq!(err.kind, sqlr_core::ErrorKind::Unsupported);

    // Object references for the nodes the tree offers.
    let object = |node: &Node| rel.object_ref(node);
    assert_eq!(object(&schemas[0]).unwrap().kind, ObjectKind::Schema);
    assert_eq!(object(&tables[0]).unwrap().kind, ObjectKind::Table);
    assert_eq!(object(&views[0]).unwrap().kind, ObjectKind::View);
    let ix = object(&idx[1]).unwrap();
    assert_eq!(ix.kind, ObjectKind::Index);
    assert_eq!(ix.parent.as_deref(), Some("orders"));
    assert!(object(&procs[0]).is_none());
}

#[tokio::test]
async fn describe_and_catalog_report_columns_and_keys() {
    let (_c, conn) = server().await;
    let rel = conn.relational().unwrap();
    for sql in SETUP {
        rel.execute(sql).await.unwrap();
    }
    let table = |schema: Option<&str>, name: &str| TableRef {
        database: None,
        schema: schema.map(String::from),
        name: name.into(),
    };

    let info = rel
        .describe(&table(Some("shop"), "customers"))
        .await
        .unwrap();
    assert_eq!(info.primary_key, ["id"]);
    assert_eq!(info.unique_keys, [vec!["email".to_string()]]);
    assert!(info.columns[0].generated, "identity is not writable");
    assert!(info.columns[3].generated, "computed is not writable");
    assert!(!info.columns[1].generated);
    assert_eq!(info.columns[2].default.as_deref(), Some("((18))"));

    let orders = rel.describe(&table(Some("shop"), "orders")).await.unwrap();
    let types: Vec<_> = orders
        .columns
        .iter()
        .map(|c| c.type_name.as_str())
        .collect();
    assert_eq!(
        types,
        [
            "int",
            "int",
            "decimal(10,2)",
            "varchar(max)",
            "char(4)",
            "varbinary(16)",
            "nchar(3)"
        ]
    );

    // No schema means dbo.
    rel.execute("CREATE TABLE dbo.up (id int)").await.unwrap();
    let up = rel.describe(&table(None, "up")).await.unwrap();
    assert_eq!(up.table.schema.as_deref(), Some("dbo"));
    assert!(up.primary_key.is_empty() && up.unique_keys.is_empty());
    let missing = rel
        .describe(&table(Some("shop"), "ghost"))
        .await
        .unwrap_err();
    assert!(missing.message.contains("not found"), "{missing:?}");

    let catalog = rel.catalog("shop").await.unwrap();
    assert_eq!(catalog.schema, "shop");
    let names: Vec<_> = catalog.relations.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["big_orders", "customers", "orders"]);
    assert_eq!(catalog.relations[0].kind, sqlr_core::RelationKind::View);
    assert_eq!(catalog.relations[1].kind, sqlr_core::RelationKind::Table);
    let o = &catalog.relations[2];
    assert_eq!(o.columns.len(), 7);
    assert_eq!(o.columns[2].type_name, "decimal(10,2)");
    assert!(rel.catalog("nope").await.unwrap().relations.is_empty());
}

#[tokio::test]
async fn values_of_every_type_decode_to_cells() {
    let (_c, conn) = server().await;
    let r = rows(
        &conn,
        "SELECT CAST(1 AS bit), CAST(2 AS tinyint), CAST(3 AS smallint), CAST(4 AS int), \
         CAST(5 AS bigint), CAST(1.5 AS real), CAST(2.5 AS float), CAST(12.340 AS decimal(10,3)), \
         CAST(-0.05 AS numeric(5,2)), CAST(1.25 AS money), CAST(0.5 AS smallmoney), \
         CAST('00000000-0000-0000-0000-000000000001' AS uniqueidentifier), \
         CAST(0xCAFE AS varbinary(8)), CAST(0xCAFE AS binary(2)), \
         CAST('abc' AS varchar(10)), CAST('ab' AS char(4)), N'néw', CAST(N'x' AS nchar(2)), \
         CAST(N'<a/>' AS xml), CAST('2024-01-02 03:04:05.678' AS datetime), \
         CAST('2024-01-02 03:04' AS smalldatetime), CAST('2024-01-02' AS date), \
         CAST('03:04:05.1234567' AS time), \
         CAST('2024-01-02 03:04:05.1234567' AS datetime2), \
         CAST('2024-01-02 03:04:05.5 -05:30' AS datetimeoffset(1)), \
         CAST(NULL AS int), CAST(7 AS sql_variant)",
    )
    .await;
    let row = &r[0];
    assert_eq!(row[0], Value::Bool(true));
    assert_eq!(row[9], Value::Float(1.25));
    assert_eq!(row[10], Value::Float(0.5));
    assert_eq!(row[1], Value::Int(2));
    assert_eq!(row[2], Value::Int(3));
    assert_eq!(row[3], Value::Int(4));
    assert_eq!(row[4], Value::Int(5));
    assert_eq!(row[5], Value::Float(1.5));
    assert_eq!(row[6], Value::Float(2.5));
    assert_eq!(text(&row[7]), "12.340");
    assert_eq!(text(&row[8]), "-0.05");
    assert_eq!(
        row[11],
        Value::Text("00000000-0000-0000-0000-000000000001".into())
    );
    assert_eq!(row[12], Value::Bytes(vec![0xCA, 0xFE]));
    assert_eq!(row[13], Value::Bytes(vec![0xCA, 0xFE]));
    assert_eq!(text(&row[14]), "abc");
    assert_eq!(text(&row[15]), "ab  ");
    assert_eq!(text(&row[16]), "néw");
    assert_eq!(text(&row[18]), "<a/>");
    assert_eq!(text(&row[19]), "2024-01-02 03:04:05.677");
    assert_eq!(text(&row[20]), "2024-01-02 03:04:00");
    assert_eq!(text(&row[21]), "2024-01-02");
    assert_eq!(text(&row[22]), "03:04:05.1234567");
    assert_eq!(text(&row[23]), "2024-01-02 03:04:05.1234567");
    assert_eq!(text(&row[24]), "2024-01-02 03:04:05.5-05:30");
    assert_eq!(row[25], Value::Null);
    assert_eq!(row[26], Value::Text("7".into()));
}

#[tokio::test]
async fn column_metadata_names_every_tds_type() {
    let (_c, conn) = server().await;
    let Execution::Rows(s) = conn
        .relational()
        .unwrap()
        .execute(
            "SELECT CAST(1 AS bit) a, CAST(1 AS tinyint) b, CAST(1 AS smallint) c, 1 d, \
             CAST(1 AS bigint) e, CAST(1 AS real) f, CAST(1 AS float) g, CAST(1 AS money) h, \
             CAST(1 AS decimal(5,2)) i, NEWID() j, CAST(0x01 AS varbinary(2)) k, \
             CAST('a' AS varchar(2)) l, CAST('a' AS char(2)) m, N'a' n, CAST(N'a' AS nchar(2)) o, \
             CAST(N'a' AS xml) p, CAST(1 AS sql_variant) q, GETDATE() r, \
             CAST(GETDATE() AS smalldatetime) s, SYSDATETIME() t, SYSDATETIMEOFFSET() u, \
             CAST(GETDATE() AS date) v, CAST(GETDATE() AS time) w, \
             CAST('a' AS text) x, CAST(N'a' AS ntext) y, CAST(0x01 AS image) z, \
             CAST(1 AS numeric(5,2)) aa, CAST(1 AS smallmoney) ab, CAST(1 AS binary(2)) ac",
        )
        .await
        .unwrap()
    else {
        panic!("rows")
    };
    let kinds: Vec<_> = s.columns().iter().map(|c| c.type_name.clone()).collect();
    for expected in [
        "bit",
        "tinyint",
        "smallint",
        "int",
        "bigint",
        "real",
        "float",
        "money",
        "decimal",
        "uniqueidentifier",
        "varbinary",
        "varchar",
        "char",
        "nvarchar",
        "nchar",
        "xml",
        "sql_variant",
        "datetime",
        "datetime2",
        "datetimeoffset",
        "date",
        "time",
        "text",
        "ntext",
        "numeric",
    ] {
        assert!(
            kinds.iter().any(|k| k == expected),
            "{expected} in {kinds:?}"
        );
    }
}

#[tokio::test]
async fn bound_parameters_round_trip_and_the_connection_reports_itself() {
    let (_c, conn) = server().await;
    let rel = conn.relational().unwrap();
    let Execution::Rows(s) = rel
        .execute_with(
            "SELECT @P1, @P2, @P3, @P4, @P5, @P6",
            &[
                Value::Null,
                Value::Bool(true),
                Value::Int(9),
                Value::Float(0.5),
                Value::Text("hi".into()),
                Value::Bytes(vec![1, 2]),
            ],
        )
        .await
        .unwrap()
    else {
        panic!("rows")
    };
    let r = collect(s).await;
    assert_eq!(
        r[0],
        [
            Value::Null,
            Value::Bool(true),
            Value::Int(9),
            Value::Float(0.5),
            Value::Text("hi".into()),
            Value::Bytes(vec![1, 2])
        ]
    );
    let info = conn.ping().await.unwrap();
    assert_eq!(info.product, "SQL Server");
    assert!(info.version.contains("SQL Server"));
    assert_eq!(conn.database_name(), Some("master"));
    // Cancel kills a long wait from a side connection.
    let handle = conn.cancel_handle();
    let waiting = rel.execute("WAITFOR DELAY '00:00:20'");
    let (res, cancelled) = tokio::join!(waiting, async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        handle.cancel().await
    });
    cancelled.unwrap();
    assert!(res.is_err());
}
