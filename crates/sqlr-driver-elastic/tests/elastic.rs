//! Elasticsearch driver against 8.x and 7.17 containers. The tests take turns (one
//! Elasticsearch JVM at a time) to keep memory in check.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{
    Cell, Connection, ConnectionConfig, DocExecution, DocTarget, DocValue, DocumentConn, Driver,
    DriverError, ErrorKind, NodeKind, NodePath, Secrets, StatementClass,
};
use sqlr_driver_elastic::ElasticDriver;
use testcontainers_modules::testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, GenericImage, ImageExt};

static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const IMAGE: &str = "docker.elastic.co/elasticsearch/elasticsearch";

struct Server {
    _container: ContainerAsync<GenericImage>,
    host: String,
    port: u16,
}

async fn start(tag: &str, env: &[(&str, &str)]) -> Server {
    let mut image = GenericImage::new(IMAGE, tag)
        .with_exposed_port(9200.tcp())
        .with_wait_for(WaitFor::Nothing)
        .with_env_var("discovery.type", "single-node")
        .with_env_var("ES_JAVA_OPTS", "-Xms512m -Xmx512m")
        .with_env_var("xpack.ml.enabled", "false")
        .with_env_var("ingest.geoip.downloader.enabled", "false");
    for (k, v) in env {
        image = image.with_env_var(*k, *v);
    }
    let c = image.start().await.expect("start elasticsearch");
    let host = c.get_host().await.expect("host").to_string();
    let port = c.get_host_port_ipv4(9200).await.expect("port");
    Server {
        _container: c,
        host,
        port,
    }
}

/// Elasticsearch 8 without security (security is covered by `es_versions_and_auth`).
async fn start8(extra: &[(&str, &str)]) -> Server {
    let mut env = vec![("xpack.security.enabled", "false")];
    env.extend_from_slice(extra);
    start("8.15.5", &env).await
}

fn config(server: &Server, extra: &[(&str, &str)]) -> ConnectionConfig {
    let mut values: std::collections::BTreeMap<String, String> = [
        ("host", server.host.clone()),
        ("port", server.port.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for (k, v) in extra {
        values.insert(k.to_string(), v.to_string());
    }
    ConnectionConfig {
        id: "e".into(),
        name: "es".into(),
        driver: "elasticsearch".into(),
        group: None,
        color: None,
        values,
        profile: None,
        save_secrets: true,
    }
}

fn secrets(pairs: &[(&str, &str)]) -> Secrets {
    Secrets(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

/// Connects once the node answers (it starts in about 20 s).
async fn connect_when_up(
    server: &Server,
    extra: &[(&str, &str)],
    secrets: &Secrets,
) -> Arc<dyn Connection> {
    let driver = ElasticDriver::new();
    let cfg = config(server, extra);
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match driver.connect(&cfg, secrets).await {
            Ok(c) => {
                // Wait for the cluster to accept writes.
                let d = c.document().unwrap();
                if d.run(
                    None,
                    "GET _cluster/health?wait_for_status=yellow&timeout=60s",
                )
                .await
                .is_ok()
                {
                    return c;
                }
            }
            Err(e) if Instant::now() > deadline => panic!("elasticsearch did not start: {e:?}"),
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn connect(server: &Server, extra: &[(&str, &str)]) -> Arc<dyn Connection> {
    connect_when_up(server, extra, &Secrets::default()).await
}

fn d(conn: &Arc<dyn Connection>) -> &dyn DocumentConn {
    conn.document().expect("a document connection")
}

async fn run(conn: &Arc<dyn Connection>, text: &str) -> DocExecution {
    match d(conn).run(None, text).await {
        Ok(x) => x,
        Err(e) => panic!("{text}: {e:?}"),
    }
}

async fn run_err(conn: &Arc<dyn Connection>, text: &str) -> DriverError {
    match d(conn).run(None, text).await {
        Ok(_) => panic!("{text}: expected an error"),
        Err(e) => e,
    }
}

fn value(exec: DocExecution) -> DocValue {
    match exec {
        DocExecution::Value(v) => v,
        _ => panic!("expected a value"),
    }
}

fn written(exec: DocExecution) -> (u64, u64, u64, u64, Vec<DocValue>) {
    match exec {
        DocExecution::Written {
            matched,
            modified,
            inserted,
            deleted,
            ids,
        } => (matched, modified, inserted, deleted, ids),
        _ => panic!("expected a write result"),
    }
}

async fn rows(exec: DocExecution, page: usize) -> (Vec<String>, Vec<Vec<String>>) {
    let DocExecution::Rows(mut stream) = exec else {
        panic!("expected rows")
    };
    let cols = stream.columns().iter().map(|c| c.name.clone()).collect();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next_chunk(page).await.unwrap() {
        assert!(chunk.len() <= page);
        for r in 0..chunk.len() {
            out.push(
                (0..chunk.columns().len())
                    .map(|c| match chunk.cell(r, c) {
                        Cell::Null => "NULL".to_string(),
                        other => other.to_string(),
                    })
                    .collect(),
            );
        }
    }
    stream.close().await.unwrap();
    (cols, out)
}

fn get_str<'a>(doc: &'a DocValue, key: &str) -> &'a str {
    match doc.get(key) {
        Some(DocValue::String(s)) => s,
        other => panic!("{key}: {other:?}"),
    }
}

/// Indexes `n` documents `{n, cat}` into `index` with `_bulk`.
async fn seed(conn: &Arc<dyn Connection>, index: &str, n: usize) {
    run(
        conn,
        &format!(
            "PUT {index}\n{{\"settings\": {{\"number_of_shards\": 2, \"number_of_replicas\": 0}}, \"mappings\": {{\"properties\": {{\"n\": {{\"type\": \"long\"}}, \"cat\": {{\"type\": \"keyword\"}}}}}}}}"
        ),
    )
    .await;
    for start in (0..n).step_by(5000) {
        let mut text = format!("POST {index}/_bulk?refresh=true\n");
        for i in start..(start + 5000).min(n) {
            text.push_str(&format!(
                "{{\"index\": {{\"_id\": \"{i}\"}}}}\n{{\"n\": {i}, \"cat\": \"{}\"}}\n",
                ["a", "b", "c"][i % 3]
            ));
        }
        let (_, _, inserted, _, _) = written(run(conn, &text).await);
        assert_eq!(inserted as usize, (start + 5000).min(n) - start);
    }
}

/// Every hit of a `Docs` result, `page` at a time, with the stream's notices.
async fn hits(
    exec: DocExecution,
    page: usize,
    pause: Option<Duration>,
) -> (Vec<DocValue>, Vec<String>) {
    let DocExecution::Docs(mut stream) = exec else {
        panic!("expected documents")
    };
    let mut out = Vec::new();
    let mut notices = Vec::new();
    let mut first = true;
    while let Some(chunk) = stream.next_chunk(page).await.unwrap() {
        assert!(chunk.len() <= page);
        out.extend(chunk.docs());
        notices.extend(stream.take_notices());
        if first && let Some(p) = pause {
            tokio::time::sleep(p).await;
        }
        first = false;
    }
    notices.extend(stream.take_notices());
    stream.close().await.unwrap();
    (out, notices)
}

fn ids(docs: &[DocValue]) -> HashSet<String> {
    docs.iter().map(|d| get_str(d, "_id").to_string()).collect()
}

#[tokio::test]
async fn browse_mappings_and_document_writes() {
    sqlr_testkit::skip_container_tests!();
    let _turn = ONE_AT_A_TIME.lock().await;
    let server = start8(&[]).await;
    let conn = connect(&server, &[]).await;
    let info = conn.ping().await.unwrap();
    assert_eq!(info.product, "Elasticsearch");
    assert!(info.version.starts_with("8."), "{}", info.version);

    seed(&conn, "orders", 10).await;
    run(
        &conn,
        "PUT customers\n{\"mappings\": {\"properties\": {\"name\": {\"type\": \"text\"}}}}",
    )
    .await;
    run(
        &conn,
        "POST _aliases\n{\"actions\": [{\"add\": {\"index\": \"orders\", \"alias\": \"sales\"}}]}",
    )
    .await;
    run(&conn, "PUT _index_template/sqlr-test-events\n{\"index_patterns\": [\"app-events*\"], \"data_stream\": {}, \"priority\": 500}").await;
    run(
        &conn,
        "POST app-events/_doc\n{\"@timestamp\": \"2024-01-01T00:00:00Z\", \"msg\": \"hi\"}",
    )
    .await;

    let root = conn.browse(&NodePath::root()).await.unwrap();
    let labels: Vec<&str> = root.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(labels, ["Indices", "Aliases", "Data streams"]);
    let indices = conn.browse(&root[0].path).await.unwrap();
    let orders = indices.iter().find(|n| n.name == "orders").expect("orders");
    assert_eq!(orders.kind, NodeKind::Table);
    let detail = orders.detail.as_deref().unwrap();
    assert!(
        detail.starts_with("green") && detail.contains("10 docs"),
        "{detail}"
    );
    assert!(indices.iter().any(|n| n.name == "customers"));
    assert!(
        !indices.iter().any(|n| n.name.starts_with('.')),
        "no hidden indices"
    );
    let aliases = conn.browse(&root[1].path).await.unwrap();
    assert_eq!(aliases.len(), 1);
    assert_eq!(
        (aliases[0].name.as_str(), aliases[0].kind),
        ("sales", NodeKind::View)
    );
    assert_eq!(aliases[0].detail.as_deref(), Some("→ orders"));
    let streams = conn.browse(&root[2].path).await.unwrap();
    assert_eq!(streams.len(), 1, "{streams:?}");
    assert_eq!(streams[0].name, "app-events");

    // Mappings and settings.
    let mapping = value(run(&conn, "GET customers/_mapping").await);
    let props = mapping
        .get("customers")
        .and_then(|m| m.get("mappings"))
        .and_then(|m| m.get("properties"))
        .expect("properties");
    assert_eq!(
        props.get("name").and_then(|n| n.get("type")),
        Some(&DocValue::String("text".into()))
    );
    let target = DocTarget {
        database: None,
        collection: "customers".into(),
    };
    let ix = d(&conn).indexes(&target).await.unwrap();
    assert_eq!(ix.len(), 1);
    assert_eq!(get_str(&ix[0], "name"), "customers");
    assert!(ix[0].get("mappings").is_some() && ix[0].get("settings").is_some());

    // Index, update and delete through the console.
    let w = written(
        run(
            &conn,
            "PUT customers/_doc/c1?refresh=true\n{\"name\": \"Ada\"}",
        )
        .await,
    );
    assert_eq!((w.2, w.4.clone()), (1, vec![DocValue::String("c1".into())]));
    let w = written(
        run(
            &conn,
            "POST customers/_update/c1?refresh=true\n{\"doc\": {\"name\": \"Ada L\"}}",
        )
        .await,
    );
    assert_eq!((w.0, w.1), (1, 1));
    let w = written(run(&conn, "DELETE customers/_doc/c1?refresh=true").await);
    assert_eq!(w.3, 1);
    let e = run_err(&conn, "GET customers/_doc/c1").await;
    assert_eq!(e.kind, ErrorKind::Other, "{e:?}");

    // …and by `_id` + index through the document API.
    let doc = DocValue::Document(vec![
        ("name".into(), DocValue::String("Grace".into())),
        (
            "tags".into(),
            DocValue::Array(vec![DocValue::String("x".into())]),
        ),
    ]);
    let id = d(&conn).insert_doc(&target, &doc).await.unwrap();
    let DocValue::String(_) = &id else {
        panic!("{id:?}")
    };
    let (found, _) = hits(run(&conn, "GET customers/_search").await, 100, None).await;
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].get("_index"),
        Some(&DocValue::String("customers".into()))
    );
    assert_eq!(found[0].get("_id"), Some(&id));
    assert_eq!(
        found[0].get("name"),
        Some(&DocValue::String("Grace".into()))
    );
    let mut edited = found[0].clone();
    if let DocValue::Document(f) = &mut edited {
        f.retain(|(k, _)| k != "tags");
    }
    assert_eq!(
        d(&conn).replace_doc(&target, &id, &edited).await.unwrap(),
        1
    );
    let (found, _) = hits(run(&conn, "GET customers/_search").await, 100, None).await;
    assert_eq!(found[0].get("tags"), None, "replaced, not merged");
    let missing = DocValue::String("nope".into());
    assert_eq!(
        d(&conn)
            .replace_doc(&target, &missing, &edited)
            .await
            .unwrap(),
        0
    );
    assert_eq!(d(&conn).delete_doc(&target, &id).await.unwrap(), 1);
    assert_eq!(d(&conn).delete_doc(&target, &id).await.unwrap(), 0);
    assert_eq!(
        d(&conn).open_target_text(&target, 500),
        "GET customers/_search\n{\n  \"query\": {\"match_all\": {}}\n}"
    );

    // Classes and read-only.
    assert_eq!(d(&conn).classify("GET x/_search"), StatementClass::Read);
    assert_eq!(
        d(&conn).classify("POST x/_search\n{}"),
        StatementClass::Read
    );
    assert_eq!(d(&conn).classify("ESQL FROM x"), StatementClass::Read);
    assert_eq!(d(&conn).classify("PUT x/_doc/1\n{}"), StatementClass::Write);
    assert_eq!(d(&conn).classify("PUT x"), StatementClass::Ddl);
    assert!(matches!(
        d(&conn).classify("DELETE x"),
        StatementClass::Destructive(_)
    ));
    let ro = connect(&server, &[("read_only", "true")]).await;
    for text in [
        "PUT customers/_doc/z\n{}",
        "POST customers/_doc\n{}",
        "DELETE customers",
        "POST customers/_delete_by_query\n{\"query\": {\"match_all\": {}}}",
        "PUT newindex",
        "POST customers/_doc/_search\n{}",
        "DELETE customers/_doc/_count",
        "POST customers/_update/_mget\n{\"doc\": {}}",
    ] {
        let e = run_err(&ro, text).await;
        assert_eq!(
            (e.kind, e.message.as_str()),
            (ErrorKind::Permission, "The session is read-only"),
            "{text}"
        );
    }
    // A dot segment would resolve to another API: refused before it is sent.
    let e = run_err(&ro, "POST customers/_search/../_delete_by_query\n{}").await;
    assert_eq!(e.kind, ErrorKind::Syntax, "{e:?}");
    assert!(d(&ro).delete_doc(&target, &missing).await.is_err());
    run(&ro, "GET customers/_search").await;
    run(&ro, "SQL SELECT COUNT(*) FROM orders").await;
    assert!(
        value(run(&conn, "GET _cat/indices/customers?h=index").await)
            == DocValue::String("customers\n".into())
    );
}

#[tokio::test]
async fn search_pages_with_pit_and_tabular_results() {
    sqlr_testkit::skip_container_tests!();
    let _turn = ONE_AT_A_TIME.lock().await;
    let server = start8(&[]).await;
    let conn = connect(&server, &[]).await;
    seed(&conn, "big", 25_000).await;

    // Every hit once, page by page, through a point in time.
    let (all, notices) = hits(
        run(&conn, "GET big/_search\n{\"query\": {\"match_all\": {}}}").await,
        500,
        None,
    )
    .await;
    assert_eq!(all.len(), 25_000);
    assert_eq!(ids(&all).len(), 25_000, "no duplicates");
    assert!(notices.is_empty(), "{notices:?}");
    // Without `size`, every hit pages through; the user's sort is kept.
    let (sorted, _) = hits(
        run(&conn, "POST big/_search\n{\"query\": {\"term\": {\"cat\": \"a\"}}, \"sort\": [{\"n\": \"desc\"}]}").await,
        500,
        None,
    )
    .await;
    assert_eq!(sorted.len(), 8334);
    assert_eq!(sorted[0].get("n"), Some(&DocValue::Int32(24999)));
    assert_eq!(sorted[1].get("n"), Some(&DocValue::Int32(24996)));
    // An explicit `size` caps the hits, as in Elasticsearch…
    let (capped, _) = hits(
        run(&conn, "POST big/_search\n{\"size\": 300, \"query\": {\"term\": {\"cat\": \"a\"}}, \"sort\": [{\"n\": \"desc\"}]}").await,
        100,
        None,
    )
    .await;
    assert_eq!(capped.len(), 300);
    assert_eq!(capped[0].get("n"), Some(&DocValue::Int32(24999)));
    // …also past the 10 000-hit window, where it pages with a point in time.
    let (large, _) = hits(run(&conn, "GET big/_search?size=12000").await, 500, None).await;
    assert_eq!(large.len(), 12_000);
    assert_eq!(ids(&large).len(), 12_000);
    // `from` runs one plain request.
    let (page, _) = hits(
        run(
            &conn,
            "GET big/_search\n{\"from\": 10, \"size\": 5, \"sort\": [\"n\"]}",
        )
        .await,
        500,
        None,
    )
    .await;
    assert_eq!(page.len(), 5);
    assert_eq!(page[0].get("n"), Some(&DocValue::Int32(10)));
    // Aggregations come back as the response.
    let agg = value(run(&conn, "GET big/_search\n{\"size\": 0, \"aggs\": {\"by_cat\": {\"terms\": {\"field\": \"cat\"}}}}").await);
    assert!(agg.get("aggregations").is_some());
    assert_eq!(
        value(run(&conn, "GET big/_count").await).get("count"),
        Some(&DocValue::Int32(25_000))
    );

    // ES|QL and SQL are tabular.
    let (cols, r) = rows(
        run(
            &conn,
            "ESQL FROM big | STATS c = COUNT(*) BY cat | SORT cat",
        )
        .await,
        100,
    )
    .await;
    assert_eq!(cols, ["c", "cat"]);
    assert_eq!(r, [["8334", "a"], ["8333", "b"], ["8333", "c"]]);
    let (cols, r) = rows(
        run(
            &conn,
            "SQL SELECT n, cat FROM big WHERE n < 2500 ORDER BY n",
        )
        .await,
        400,
    )
    .await;
    assert_eq!(cols, ["n", "cat"]);
    assert_eq!(r.len(), 2500, "SQL pages through its cursor");
    assert_eq!(r[2499], ["2499", "a"]);
    let e = run_err(&conn, "SQL SELECT nope FROM big").await;
    assert_eq!(e.kind, ErrorKind::Syntax, "{e:?}");
}

#[tokio::test]
async fn es_pit_expired_restarts() {
    sqlr_testkit::skip_container_tests!();
    let _turn = ONE_AT_A_TIME.lock().await;
    // The node reaps expired search contexts every second.
    let server = start8(&[("search.keep_alive_interval", "1s")]).await;
    let conn = connect(&server, &[("pit_keep_alive", "2s")]).await;
    seed(&conn, "big", 25_000).await;
    let (all, notices) = hits(
        run(&conn, "GET big/_search\n{\"query\": {\"match_all\": {}}}").await,
        500,
        Some(Duration::from_secs(6)),
    )
    .await;
    assert_eq!(all.len(), 25_000);
    assert_eq!(ids(&all).len(), 25_000, "restarted without duplicates");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("expired"), "{}", notices[0]);
}

#[tokio::test]
async fn es_versions_and_auth() {
    sqlr_testkit::skip_container_tests!();
    let _turn = ONE_AT_A_TIME.lock().await;
    {
        // 8.x with security on (HTTP without TLS): basic auth, API keys, wrong passwords.
        let server = start(
            "8.15.5",
            &[
                ("xpack.security.enabled", "true"),
                ("xpack.security.http.ssl.enabled", "false"),
                ("ELASTIC_PASSWORD", "s3cret-pw"),
            ],
        )
        .await;
        let user = [("user", "elastic")];
        let conn = connect_when_up(&server, &user, &secrets(&[("password", "s3cret-pw")])).await;
        assert!(conn.ping().await.unwrap().version.starts_with("8."));
        let key = value(run(&conn, "POST _security/api_key\n{\"name\": \"sqlr-test\"}").await);
        let encoded = get_str(&key, "encoded").to_string();

        let driver = ElasticDriver::new();
        let e = driver
            .connect(&config(&server, &user), &secrets(&[("password", "wrong")]))
            .await
            .err()
            .expect("wrong password fails");
        assert_eq!(e.kind, ErrorKind::Auth, "{e:?}");
        assert!(e.message.contains("Authentication failed"), "{}", e.message);
        assert!(!e.message.contains("wrong"));
        let e = driver
            .connect(&config(&server, &[]), &Secrets::default())
            .await
            .err()
            .expect("no credentials fail");
        assert_eq!(e.kind, ErrorKind::Auth, "{e:?}");

        let by_key = driver
            .connect(&config(&server, &[]), &secrets(&[("api_key", &encoded)]))
            .await
            .expect("API key");
        run(&by_key, "PUT k/_doc/1?refresh=true\n{\"a\": 1}").await;
        let (cols, r) = rows(run(&by_key, "ESQL FROM k | KEEP a").await, 10).await;
        assert_eq!(
            (cols, r),
            (vec!["a".to_string()], vec![vec!["1".to_string()]])
        );
    }
    {
        // 7.17 without security: anonymous access, SQL and PIT work, ES|QL is refused.
        let server = start("7.17.25", &[("xpack.security.enabled", "false")]).await;
        let conn = connect(&server, &[]).await;
        assert!(conn.ping().await.unwrap().version.starts_with("7.17"));
        seed(&conn, "old", 1200).await;
        let e = run_err(&conn, "ESQL FROM old").await;
        assert_eq!(e.kind, ErrorKind::Unsupported);
        assert!(e.message.contains("8.11"), "{}", e.message);
        let (_, r) = rows(run(&conn, "SQL SELECT COUNT(*) FROM old").await, 10).await;
        assert_eq!(r, [["1200"]]);
        let (all, _) = hits(run(&conn, "GET old/_search").await, 500, None).await;
        assert_eq!(ids(&all).len(), 1200);
        let root = conn.browse(&NodePath::root()).await.unwrap();
        assert!(root.iter().any(|n| n.name == "Indices"));
    }
}
