mod support;

use std::time::Duration;

use sqlr_core::{
    Cell, ConnectionConfig, Driver, ErrorKind, Execution, FieldType, Node, NodeKind, NodePath,
    Secrets, TableRef,
};
use sqlr_driver_postgres::PostgresDriver;
use support::{config, pg, pg_isolated, secrets};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

fn path(segs: &[&str]) -> NodePath {
    NodePath(segs.iter().map(|s| s.to_string()).collect())
}

fn names(nodes: &[Node]) -> Vec<&str> {
    nodes.iter().map(|n| n.name.as_str()).collect()
}

#[tokio::test]
async fn connect_and_ping() {
    sqlr_testkit::skip_container_tests!();
    let (_c, cfg, sec) = pg().await;
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    let info = conn.ping().await.unwrap();
    assert_eq!(info.product, "PostgreSQL");
    assert!(!info.version.is_empty());
}

#[tokio::test]
async fn wrong_password_is_auth_error() {
    sqlr_testkit::skip_container_tests!();
    let (_c, cfg, _) = pg().await;
    let err = PostgresDriver::new()
        .connect(&cfg, &secrets("nope"))
        .await
        .err()
        .expect("must fail");
    assert_eq!(err.kind, ErrorKind::Auth, "{err:?}");
}

#[tokio::test]
async fn unreachable_host_is_connection_error() {
    sqlr_testkit::skip_container_tests!();
    let cfg = config("127.0.0.1", 1);
    let err = PostgresDriver::new()
        .connect(&cfg, &secrets("x"))
        .await
        .err()
        .expect("must fail");
    assert_eq!(err.kind, ErrorKind::Connection, "{err:?}");
}

#[tokio::test]
async fn unknown_path_unsupported() {
    sqlr_testkit::skip_container_tests!();
    let (_c, cfg, sec) = pg().await;
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    for p in [
        path(&["public", "nope"]),
        path(&["public", "tables", "t", "weird"]),
    ] {
        let err = conn.browse(&p).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported, "{p:?}");
    }
}

async fn raw(
    cfg: &sqlr_core::ConnectionConfig,
    sec: &sqlr_core::Secrets,
    database: &str,
) -> tokio_postgres::Client {
    let (client, conn) = tokio_postgres::Config::new()
        .host(cfg.values["host"].as_str())
        .port(cfg.values["port"].parse().unwrap())
        .user("postgres")
        .password(&sec.0["password"])
        .dbname(database)
        .connect(tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    client
}

async fn setup(cfg: &sqlr_core::ConnectionConfig, sec: &sqlr_core::Secrets) {
    // Setup goes through a raw client: execute() belongs to a later task.
    raw(cfg, sec, &cfg.values["database"])
        .await
        .batch_execute(
            r#"CREATE SCHEMA "select";
               CREATE TABLE "select"."Mixed ""Case"" T"(id int primary key, note text not null);
               CREATE VIEW "select"."V" AS SELECT id, note FROM "select"."Mixed ""Case"" T";
               CREATE MATERIALIZED VIEW "select"."M" AS SELECT id FROM "select"."Mixed ""Case"" T";
               CREATE SEQUENCE "select"."seq";
               CREATE FUNCTION "select".f(a int, b text) RETURNS int LANGUAGE sql AS 'SELECT 1';"#,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn browse_tree() {
    sqlr_testkit::skip_container_tests!();
    let (_c, cfg, sec) = pg().await;
    setup(&cfg, &sec).await;
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();

    let root = conn.browse(&NodePath::root()).await.unwrap();
    let n = names(&root);
    assert!(n.contains(&"select") && n.contains(&"public"), "{n:?}");
    assert_eq!(n[n.len() - 2..], ["information_schema", "pg_catalog"]);
    assert!(
        root.iter()
            .all(|x| x.kind == NodeKind::Schema && x.has_children)
    );
    assert!(
        !n.iter()
            .any(|s| s.starts_with("pg_toast") || s.starts_with("pg_temp_"))
    );

    let folders = conn.browse(&path(&["select"])).await.unwrap();
    assert_eq!(
        names(&folders),
        [
            "tables",
            "views",
            "materialized views",
            "functions",
            "sequences"
        ]
    );
    assert!(folders.iter().all(|x| x.kind == NodeKind::Folder));

    let tables = conn.browse(&path(&["select", "tables"])).await.unwrap();
    assert_eq!(names(&tables), ["Mixed \"Case\" T"]);
    assert_eq!(tables[0].kind, NodeKind::Table);
    assert!(tables[0].has_children);
    assert_eq!(
        tables[0].path,
        path(&["select", "tables", "Mixed \"Case\" T"])
    );

    let t = &tables[0].path;
    let sub = conn.browse(t).await.unwrap();
    assert_eq!(names(&sub), ["columns", "indexes"]);

    let cols = conn.browse(&t.child("columns")).await.unwrap();
    assert_eq!(names(&cols), ["id", "note"]);
    assert_eq!(cols[0].detail.as_deref(), Some("integer NOT NULL"));
    assert_eq!(cols[1].detail.as_deref(), Some("text NOT NULL"));
    assert!(
        cols.iter()
            .all(|c| c.kind == NodeKind::Column && !c.has_children)
    );

    let idx = conn.browse(&t.child("indexes")).await.unwrap();
    assert_eq!(idx.len(), 1);
    assert_eq!(idx[0].kind, NodeKind::Index);
    assert!(
        idx[0]
            .detail
            .as_deref()
            .unwrap()
            .starts_with("CREATE UNIQUE INDEX")
    );

    let views = conn.browse(&path(&["select", "views"])).await.unwrap();
    assert_eq!(names(&views), ["V"]);
    assert_eq!(views[0].kind, NodeKind::View);
    let vcols = conn.browse(&views[0].path).await.unwrap();
    assert_eq!(names(&vcols), ["id", "note"]);

    let mv = conn
        .browse(&path(&["select", "materialized views"]))
        .await
        .unwrap();
    assert_eq!(names(&mv), ["M"]);
    assert_eq!(mv[0].kind, NodeKind::MaterializedView);
    let mcols = conn.browse(&mv[0].path).await.unwrap();
    assert_eq!(names(&mcols), ["id"]);

    let seqs = conn.browse(&path(&["select", "sequences"])).await.unwrap();
    assert_eq!(names(&seqs), ["seq"]);
    assert_eq!(seqs[0].kind, NodeKind::Sequence);
    assert!(!seqs[0].has_children);

    let funcs = conn.browse(&path(&["select", "functions"])).await.unwrap();
    assert_eq!(names(&funcs), ["f(a integer, b text)"]);
    assert_eq!(funcs[0].kind, NodeKind::Function);
}

#[tokio::test]
async fn show_all_databases_switches_the_explorer_root() {
    sqlr_testkit::skip_container_tests!();
    let (_c, mut cfg, sec) = pg_isolated().await;
    raw(&cfg, &sec, "postgres")
        .await
        .batch_execute(r#"CREATE DATABASE "sqlr_other""#)
        .await
        .unwrap();
    raw(&cfg, &sec, "postgres")
        .await
        .batch_execute("CREATE TABLE public.current_table(id int)")
        .await
        .unwrap();
    raw(&cfg, &sec, "sqlr_other")
        .await
        .batch_execute("CREATE TABLE widgets(id int)")
        .await
        .unwrap();

    cfg.values
        .insert("show_all_databases".into(), "true".into());
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    assert_eq!(conn.database_name(), Some("postgres"));
    assert_eq!(conn.schema_path(), path(&["postgres"]));
    assert_eq!(
        conn.database_for_path(&path(&["sqlr_other", "public", "tables", "widgets"]))
            .as_deref(),
        Some("sqlr_other")
    );
    assert_eq!(
        conn.database_for_path(&path(&["postgres", "public", "tables", "widgets"])),
        None
    );

    let root = conn.browse(&NodePath::root()).await.unwrap();
    assert!(
        root.iter()
            .all(|n| n.kind == NodeKind::Database && n.has_children)
    );
    assert_eq!(root[0].name, "postgres", "the current database comes first");
    assert_eq!(root[0].detail.as_deref(), Some("current"));
    assert!(root.iter().any(|n| n.name == "sqlr_other"));
    assert!(
        !root.iter().any(|n| n.name == "template0"),
        "databases that do not accept connections are hidden"
    );

    // Children keep the database prefix in their paths, so expanding the node the
    // explorer shows browses the same database again.
    let other_db = root.iter().find(|n| n.name == "sqlr_other").unwrap();
    assert_eq!(other_db.path, path(&["sqlr_other"]));
    let schemas = conn.browse(&other_db.path).await.unwrap();
    let public = schemas.iter().find(|n| n.name == "public").unwrap();
    assert_eq!(public.path, path(&["sqlr_other", "public"]));
    let folders = conn.browse(&public.path).await.unwrap();
    let tables = folders.iter().find(|n| n.name == "tables").unwrap();
    assert_eq!(tables.path, path(&["sqlr_other", "public", "tables"]));
    let relations = conn.browse(&tables.path).await.unwrap();
    assert_eq!(names(&relations), ["widgets"]);
    assert_eq!(
        relations[0].path,
        path(&["sqlr_other", "public", "tables", "widgets"])
    );
    let folder = conn.browse(&relations[0].path).await.unwrap();
    assert_eq!(names(&folder), ["columns", "indexes"]);
    assert_eq!(
        folder[0].path,
        path(&["sqlr_other", "public", "tables", "widgets", "columns"])
    );

    // The connection's own database keeps the prefix too.
    let current = root.iter().find(|n| n.name == "postgres").unwrap();
    let schemas = conn.browse(&current.path).await.unwrap();
    let public = schemas.iter().find(|n| n.name == "public").unwrap();
    assert_eq!(public.path, path(&["postgres", "public"]));
    let tables = conn
        .browse(&path(&["postgres", "public", "tables"]))
        .await
        .unwrap();
    assert_eq!(names(&tables), ["current_table"]);

    let other = conn.connect_database("sqlr_other").await.unwrap();
    assert_eq!(other.database_name(), Some("sqlr_other"));
    let mut stream = match other
        .relational()
        .unwrap()
        .execute("SELECT current_database()")
        .await
        .unwrap()
    {
        Execution::Rows(stream) => stream,
        Execution::Affected(n) => panic!("expected rows, got {n} affected"),
    };
    let chunk = stream.next_chunk(10).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Text("sqlr_other"));
    stream.close().await.unwrap();
}

#[test]
fn manifest_declares_tls_group_and_capabilities() {
    let driver = PostgresDriver::new();
    let m = driver.manifest();
    let caps = m.capabilities;
    assert!(caps.transactions && caps.editable_results && caps.ssh);
    assert!(caps.server_cancel && caps.schemas);
    let tls: Vec<(&str, &FieldType)> = m
        .fields
        .iter()
        .filter(|f| f.group == "TLS")
        .map(|f| (f.key.as_str(), &f.ty))
        .collect();
    let keys: Vec<&str> = tls.iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, ["ssl_mode", "ssl_root_cert", "ssl_cert", "ssl_key"]);
    let FieldType::Choice(modes) = tls[0].1 else {
        panic!("ssl_mode is a choice")
    };
    let values: Vec<&str> = modes.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(
        values,
        ["disable", "prefer", "require", "verify-ca", "verify-full"]
    );
    let ssl_mode = m.fields.iter().find(|f| f.key == "ssl_mode").unwrap();
    assert_eq!(ssl_mode.default.as_deref(), Some("prefer"));
    assert!(
        tls[1..]
            .iter()
            .all(|(_, ty)| matches!(ty, FieldType::File { save: false, .. }))
    );
}

fn object(segs: &[&str], kind: NodeKind) -> Node {
    Node {
        path: path(segs),
        name: segs.last().unwrap().to_string(),
        kind,
        has_children: false,
        detail: None,
    }
}

fn table_ref(schema: Option<&str>, name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: schema.map(String::from),
        name: name.into(),
    }
}

#[tokio::test]
async fn describe_keys() {
    sqlr_testkit::skip_container_tests!();
    let (_c, cfg, sec) = pg().await;
    raw(&cfg, &sec, &cfg.values["database"])
        .await
        .batch_execute(
            r#"CREATE TABLE public.k(
                   a int, "B" text, email text UNIQUE, code text NOT NULL UNIQUE,
                   ident int GENERATED ALWAYS AS IDENTITY,
                   g int GENERATED ALWAYS AS (a * 2) STORED,
                   d text DEFAULT 'x',
                   PRIMARY KEY ("B", a));
               CREATE UNIQUE INDEX k_partial ON k(d) WHERE d IS NOT NULL;
               CREATE UNIQUE INDEX k_expr ON k(lower(d));
               CREATE TABLE public.nopk(email text UNIQUE, code text NOT NULL,
                   CONSTRAINT nopk_code UNIQUE (code));
               CREATE UNIQUE INDEX nopk_covering ON nopk(code) INCLUDE (email);
               CREATE VIEW kv AS SELECT a, d FROM k;"#,
        )
        .await
        .unwrap();
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    let rel = conn.relational().unwrap();

    let info = rel.describe(&table_ref(Some("public"), "k")).await.unwrap();
    assert_eq!(info.table, table_ref(Some("public"), "k"));
    assert_eq!(info.primary_key, ["B", "a"]);
    assert_eq!(info.unique_keys, [vec!["code"], vec!["email"]]);
    assert_eq!(
        info.row_key(),
        Some(&["B".to_string(), "a".to_string()][..])
    );
    let cols: Vec<(&str, &str, bool, Option<&str>, bool)> = info
        .columns
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                c.type_name.as_str(),
                c.nullable,
                c.default.as_deref(),
                c.generated,
            )
        })
        .collect();
    assert_eq!(
        cols,
        [
            ("a", "integer", false, None, false),
            ("B", "text", false, None, false),
            ("email", "text", true, None, false),
            ("code", "text", false, None, false),
            ("ident", "integer", false, None, true),
            ("g", "integer", true, None, true),
            ("d", "text", true, Some("'x'::text"), false),
        ]
    );

    // Unqualified names resolve through the search path.
    let info = rel.describe(&table_ref(None, "nopk")).await.unwrap();
    assert_eq!(info.table, table_ref(Some("public"), "nopk"));
    assert!(info.primary_key.is_empty());
    assert_eq!(
        info.unique_keys,
        [vec!["code"], vec!["code"], vec!["email"]]
    );
    assert_eq!(info.row_key(), Some(&["code".to_string()][..]));

    let view = rel
        .describe(&table_ref(Some("public"), "kv"))
        .await
        .unwrap();
    assert_eq!(view.columns.len(), 2);
    assert!(view.primary_key.is_empty() && view.unique_keys.is_empty());
    assert_eq!(view.row_key(), None);

    let err = rel
        .describe(&table_ref(Some("public"), "missing"))
        .await
        .unwrap_err();
    assert!(err.message.contains("missing"), "{err:?}");
}

#[tokio::test]
async fn ddl_of_table_view_function() {
    sqlr_testkit::skip_container_tests!();
    let (_c, cfg, sec) = pg().await;
    let client = raw(&cfg, &sec, &cfg.values["database"]).await;
    client
        .batch_execute(
            r#"CREATE SCHEMA "s q";
               CREATE TABLE "s q".customers(id int PRIMARY KEY);
               CREATE TABLE "s q"."Orders"(
                   id int GENERATED ALWAYS AS IDENTITY,
                   customer int NOT NULL REFERENCES "s q".customers(id),
                   total numeric(10,2) NOT NULL DEFAULT 0 CHECK (total >= 0),
                   doubled numeric GENERATED ALWAYS AS (total * 2) STORED,
                   "Note" text COLLATE "C",
                   PRIMARY KEY (id),
                   UNIQUE (customer, "Note"));
               CREATE INDEX orders_note ON "s q"."Orders"("Note") WHERE "Note" IS NOT NULL;
               COMMENT ON TABLE "s q"."Orders" IS 'All the orders';
               COMMENT ON COLUMN "s q"."Orders"."Note" IS 'it''s free text';
               CREATE VIEW "s q".v AS SELECT id, total FROM "s q"."Orders";
               CREATE MATERIALIZED VIEW "s q".m AS SELECT id FROM "s q"."Orders";
               CREATE SEQUENCE "s q".seq INCREMENT BY 5 MINVALUE 10 START 20 CACHE 3 CYCLE;
               CREATE FUNCTION "s q".f(a int, b text) RETURNS int LANGUAGE sql AS 'SELECT a';"#,
        )
        .await
        .unwrap();
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    let rel = conn.relational().unwrap();

    let table = object(&["s q", "tables", "Orders"], NodeKind::Table);
    let ddl = rel.ddl(&table).await.unwrap();
    for fragment in [
        r#"CREATE TABLE "s q"."Orders" ("#,
        r#"    "id" integer GENERATED ALWAYS AS IDENTITY NOT NULL,"#,
        r#"    "customer" integer NOT NULL,"#,
        r#"    "total" numeric(10,2) DEFAULT 0 NOT NULL,"#,
        r#"    "doubled" numeric GENERATED ALWAYS AS ((total * (2)::numeric)) STORED,"#,
        r#"    "Note" text COLLATE "C","#,
        r#"PRIMARY KEY ("id")"#,
        r#"FOREIGN KEY ("customer") REFERENCES "s q"."customers" ("id")"#,
        "CHECK ((total >= (0)::numeric))",
        r#"UNIQUE ("customer", "Note")"#,
        r#"CREATE INDEX "orders_note" ON "s q"."Orders" USING "btree" ("Note") WHERE ("Note" IS NOT NULL);"#,
        r#"COMMENT ON TABLE "s q"."Orders" IS 'All the orders';"#,
        r#"COMMENT ON COLUMN "s q"."Orders"."Note" IS 'it''s free text';"#,
    ] {
        assert!(ddl.contains(fragment), "{fragment}\n---\n{ddl}");
    }
    // The constraints' indexes are not repeated as CREATE INDEX statements.
    assert_eq!(ddl.matches("CREATE INDEX").count(), 1, "{ddl}");

    // The DDL recreates the same table.
    client
        .batch_execute(r#"DROP TABLE "s q"."Orders" CASCADE"#)
        .await
        .unwrap();
    client.batch_execute(&ddl).await.unwrap();
    assert_eq!(rel.ddl(&table).await.unwrap(), ddl);

    client
        .batch_execute(
            r#"CREATE VIEW "s q".v AS SELECT id, total FROM "s q"."Orders";
               CREATE MATERIALIZED VIEW "s q".m AS SELECT id FROM "s q"."Orders";
               CREATE UNIQUE INDEX m_id ON "s q".m(id);"#,
        )
        .await
        .unwrap();
    let view = rel
        .ddl(&object(&["s q", "views", "v"], NodeKind::View))
        .await
        .unwrap();
    assert!(
        view.starts_with("CREATE OR REPLACE VIEW \"s q\".\"v\" (\"id\", \"total\") AS\n"),
        "{view}"
    );
    assert!(view.contains("FROM \"s q\".\"Orders\""), "{view}");
    assert!(view.ends_with(';'), "{view}");
    client.batch_execute(&view).await.unwrap();

    let matview = rel
        .ddl(&object(
            &["s q", "materialized views", "m"],
            NodeKind::MaterializedView,
        ))
        .await
        .unwrap();
    assert!(
        matview.starts_with("CREATE MATERIALIZED VIEW \"s q\".\"m\" (\"id\") AS\n"),
        "{matview}"
    );
    assert!(
        matview.contains("CREATE UNIQUE INDEX \"m_id\" ON \"s q\".\"m\" USING \"btree\" (\"id\");"),
        "{matview}"
    );

    let function = rel
        .ddl(&object(
            &["s q", "functions", "f(a integer, b text)"],
            NodeKind::Function,
        ))
        .await
        .unwrap();
    assert!(
        function.starts_with("CREATE OR REPLACE FUNCTION \"s q\".\"f\"(a integer, b text)"),
        "{function}"
    );
    assert!(function.ends_with(';'), "{function}");
    client.batch_execute(&function).await.unwrap();

    let sequence = rel
        .ddl(&object(&["s q", "sequences", "seq"], NodeKind::Sequence))
        .await
        .unwrap();
    assert_eq!(
        sequence,
        "CREATE SEQUENCE \"s q\".\"seq\" AS bigint INCREMENT BY 5 MINVALUE 10 \
         MAXVALUE 9223372036854775807 START WITH 20 CACHE 3 CYCLE;"
    );

    let index = rel
        .ddl(&object(
            &["s q", "tables", "Orders", "indexes", "orders_note"],
            NodeKind::Index,
        ))
        .await
        .unwrap();
    assert_eq!(
        index,
        r#"CREATE INDEX "orders_note" ON "s q"."Orders" USING "btree" ("Note") WHERE ("Note" IS NOT NULL);"#
    );

    let err = rel
        .ddl(&object(&["s q", "tables", "nope"], NodeKind::Table))
        .await
        .unwrap_err();
    assert!(err.message.contains("nope"), "{err:?}");
    let err = rel
        .ddl(&object(
            &["s q", "tables", "Orders", "columns", "id"],
            NodeKind::Column,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported, "{err:?}");
}

#[tokio::test]
async fn ddl_in_another_database() {
    sqlr_testkit::skip_container_tests!();
    let (_c, mut cfg, sec) = pg_isolated().await;
    raw(&cfg, &sec, "postgres")
        .await
        .batch_execute("CREATE DATABASE other")
        .await
        .unwrap();
    raw(&cfg, &sec, "other")
        .await
        .batch_execute("CREATE TABLE widgets(id int PRIMARY KEY)")
        .await
        .unwrap();
    cfg.values
        .insert("show_all_databases".into(), "true".into());
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    let ddl = conn
        .relational()
        .unwrap()
        .ddl(&object(
            &["other", "public", "tables", "widgets"],
            NodeKind::Table,
        ))
        .await
        .unwrap();
    assert!(
        ddl.starts_with("CREATE TABLE \"public\".\"widgets\" ("),
        "{ddl}"
    );
}

fn with_values(cfg: &ConnectionConfig, values: &[(&str, &str)]) -> ConnectionConfig {
    let mut cfg = cfg.clone();
    for (k, v) in values {
        cfg.values.insert(k.to_string(), v.to_string());
    }
    cfg
}

async fn connect_within(
    cfg: &ConnectionConfig,
    sec: &Secrets,
) -> Result<std::sync::Arc<dyn sqlr_core::Connection>, sqlr_core::DriverError> {
    tokio::time::timeout(
        Duration::from_secs(15),
        PostgresDriver::new().connect(cfg, sec),
    )
    .await
    .expect("connect must not hang")
}

#[tokio::test]
async fn tls_required_but_unavailable() {
    sqlr_testkit::skip_container_tests!();
    let (_c, cfg, sec) = pg().await;
    for mode in ["require", "verify-ca", "verify-full"] {
        let err = connect_within(&with_values(&cfg, &[("ssl_mode", mode)]), &sec)
            .await
            .err()
            .unwrap_or_else(|| panic!("{mode}: must fail"));
        assert_eq!(err.kind, ErrorKind::Connection, "{mode}: {err:?}");
        assert!(
            err.message.contains("server does not support TLS"),
            "{mode}: {err:?}"
        );
    }
    for mode in ["prefer", "disable"] {
        let conn = connect_within(&with_values(&cfg, &[("ssl_mode", mode)]), &sec)
            .await
            .unwrap_or_else(|e| panic!("{mode}: {e:?}"));
        conn.ping().await.unwrap();
    }
    let err = connect_within(&with_values(&cfg, &[("ssl_mode", "sometimes")]), &sec)
        .await
        .err()
        .expect("unknown mode");
    assert!(err.message.contains("sometimes"), "{err:?}");
}

/// A PostgreSQL container serving TLS with a certificate for `localhost` signed by a
/// throwaway CA. Returns the CA certificate's PEM.
async fn pg_with_tls() -> (ContainerAsync<Postgres>, ConnectionConfig, Secrets, String) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "sqlr test CA");
    let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).unwrap();
    let server_key = rcgen::KeyPair::generate().unwrap();
    let server = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .signed_by(&server_key, &ca)
        .unwrap();
    // Init scripts run as the postgres user, so the key ends up owned by it with 0600.
    let init = "cp /certs/server.crt /certs/server.key \"$PGDATA\"/\n\
                chmod 600 \"$PGDATA\"/server.key\n\
                echo 'ssl = on' >> \"$PGDATA\"/postgresql.conf\n";
    let container = Postgres::default()
        .with_tag("17-alpine")
        .with_copy_to("/certs/server.crt", server.pem().into_bytes())
        .with_copy_to("/certs/server.key", server_key.serialize_pem().into_bytes())
        .with_copy_to(
            "/docker-entrypoint-initdb.d/tls.sh",
            init.as_bytes().to_vec(),
        )
        .start()
        .await
        .expect("start postgres container");
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let (cfg, sec) = (config("localhost", port), secrets("postgres"));
    support::wait_ready(&cfg, &sec).await;
    (container, cfg, sec, ca.pem())
}

#[tokio::test]
async fn tls_modes_against_a_tls_server() {
    sqlr_testkit::skip_container_tests!();
    let (_c, cfg, sec, ca_pem) = pg_with_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let ca_path = dir.path().join("ca.pem");
    std::fs::write(&ca_path, ca_pem).unwrap();
    let ca = ca_path.to_str().unwrap();

    async fn ssl_in_use(conn: &std::sync::Arc<dyn sqlr_core::Connection>) -> Cell<'static> {
        let Execution::Rows(mut s) = conn
            .relational()
            .unwrap()
            .execute("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .await
            .unwrap()
        else {
            panic!("expected rows")
        };
        let chunk = s.next_chunk(1).await.unwrap().unwrap();
        let cell = match chunk.cell(0, 0) {
            Cell::Bool(b) => Cell::Bool(b),
            other => panic!("{other:?}"),
        };
        s.close().await.unwrap();
        cell
    }

    let ok = [
        vec![("ssl_mode", "prefer")],
        vec![("ssl_mode", "require")],
        vec![("ssl_mode", "verify-full"), ("ssl_root_cert", ca)],
        vec![("ssl_mode", "verify-ca"), ("ssl_root_cert", ca)],
    ];
    for values in &ok {
        let conn = connect_within(&with_values(&cfg, values), &sec)
            .await
            .unwrap_or_else(|e| panic!("{values:?}: {e:?}"));
        assert_eq!(ssl_in_use(&conn).await, Cell::Bool(true), "{values:?}");
        // Cancel requests go over TLS too.
        conn.cancel_handle().cancel().await.unwrap();
    }

    // verify-ca checks the chain only: the certificate names `localhost`, not 127.0.0.1.
    let by_ip = with_values(&cfg, &[("host", "127.0.0.1"), ("ssl_root_cert", ca)]);
    let conn = connect_within(&with_values(&by_ip, &[("ssl_mode", "verify-ca")]), &sec)
        .await
        .unwrap();
    assert_eq!(ssl_in_use(&conn).await, Cell::Bool(true));
    let err = connect_within(&with_values(&by_ip, &[("ssl_mode", "verify-full")]), &sec)
        .await
        .err()
        .expect("verify-full checks the host name");
    assert_eq!(err.kind, ErrorKind::Connection, "{err:?}");
    assert!(err.message.contains("TLS"), "{err:?}");
    // Through an SSH tunnel the driver dials 127.0.0.1 but verifies the original host.
    let tunneled = with_values(
        &by_ip,
        &[
            ("ssl_mode", "verify-full"),
            (sqlr_core::TLS_SERVER_NAME, "localhost"),
        ],
    );
    let conn = connect_within(&tunneled, &sec)
        .await
        .unwrap_or_else(|e| panic!("verify-full by the original host: {e:?}"));
    assert_eq!(ssl_in_use(&conn).await, Cell::Bool(true));

    // Without the CA the system roots do not trust the certificate.
    let err = connect_within(&with_values(&cfg, &[("ssl_mode", "verify-full")]), &sec)
        .await
        .err()
        .expect("unknown issuer");
    assert!(err.message.contains("TLS"), "{err:?}");

    let missing = with_values(
        &cfg,
        &[
            ("ssl_mode", "verify-full"),
            ("ssl_root_cert", "/nonexistent/ca.pem"),
        ],
    );
    let err = connect_within(&missing, &sec)
        .await
        .err()
        .expect("missing CA file");
    assert!(err.message.contains("/nonexistent/ca.pem"), "{err:?}");
}
