//! Redis driver against the fixed `redis:7` services (and a scripted fake server for clusters).

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlr_core::{
    Connection, ConnectionConfig, Driver, DriverError, ErrorKind, KeyInfo, KeyType, KeyValueConn,
    KvCursor, KvEdit, KvNodePath, KvPage, NodeKind, NodePath, RespValue, Secrets, StatementClass,
};
use sqlr_driver_redis::RedisDriver;
use sqlr_testkit::{TestLock, service};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

fn config(host: &str, port: u16, extra: &[(&str, &str)]) -> ConnectionConfig {
    let mut values: BTreeMap<String, String> = [
        ("host", host.to_string()),
        ("port", port.to_string()),
        ("database", "0".to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for (k, v) in extra {
        values.insert(k.to_string(), v.to_string());
    }
    ConnectionConfig {
        id: "r".into(),
        name: "redis".into(),
        driver: "redis".into(),
        group: None,
        color: None,
        values,
        profile: None,
        save_secrets: true,
    }
}

struct Server {
    _lock: Option<TestLock>,
    host: String,
    port: u16,
}

async fn start() -> Server {
    let (host, port) = service("REDIS");
    let guard = TestLock::acquire("redis");
    reset(&host, port).await;
    Server {
        _lock: Some(guard),
        host,
        port,
    }
}

async fn start_tls() -> Server {
    let (host, port) = service("REDIS_TLS");
    Server {
        _lock: None,
        host,
        port,
    }
}

/// Every test starts from an empty server; the lock keeps nextest's processes apart.
async fn reset(host: &str, port: u16) {
    let client = redis::Client::open(format!("redis://{host}:{port}/")).unwrap();
    for attempt in 0..3 {
        let mut con = match client.get_multiplexed_async_connection().await {
            Ok(con) => con,
            Err(_) if attempt < 2 => {
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            Err(e) => panic!("reset redis: {e}"),
        };
        if redis::cmd("FLUSHALL")
            .arg("ASYNC")
            .query_async::<redis::Value>(&mut con)
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("reset redis: timed out");
}

async fn connect(server: &Server, extra: &[(&str, &str)]) -> Arc<dyn Connection> {
    let driver = RedisDriver::new();
    let cfg = config(&server.host, server.port, extra);
    for attempt in 0..3 {
        match driver.connect(&cfg, &Secrets::default()).await {
            Ok(conn) => return conn,
            Err(_) if attempt < 2 => {
                // `CLIENT KILL` tests can leave the port proxy reconnecting for a moment.
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => panic!("connect: {e:?}"),
        }
    }
    unreachable!()
}

fn kv(conn: &Arc<dyn Connection>) -> &dyn KeyValueConn {
    conn.key_value().expect("a key-value connection")
}

/// A plain `redis` client for seeding and inspecting the server.
async fn raw(server: &Server) -> redis::aio::MultiplexedConnection {
    let client = redis::Client::open(format!("redis://{}:{}/", server.host, server.port)).unwrap();
    for attempt in 0..3 {
        match client.get_multiplexed_async_connection().await {
            Ok(con) => return con,
            Err(_) if attempt < 2 => tokio::time::sleep(Duration::from_millis(250)).await,
            Err(e) => panic!("raw redis: {e}"),
        }
    }
    unreachable!()
}

async fn run(con: &mut redis::aio::MultiplexedConnection, args: &[&str]) -> redis::Value {
    let mut cmd = redis::cmd(args[0]);
    for a in &args[1..] {
        cmd.arg(*a);
    }
    cmd.query_async(con).await.unwrap()
}

/// Every key matching `pattern`, scanning from the start.
async fn scan_all(kv: &dyn KeyValueConn, pattern: &str) -> Vec<KeyInfo> {
    let mut out = Vec::new();
    let mut cursor = 0;
    loop {
        let (keys, next) = kv.scan(0, pattern.as_bytes(), cursor, 500).await.unwrap();
        out.extend(keys);
        if next == 0 {
            return out;
        }
        cursor = next;
    }
}

fn args(list: &[&str]) -> Vec<Vec<u8>> {
    list.iter().map(|a| a.as_bytes().to_vec()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn scan_never_uses_keys() {
    sqlr_testkit::skip_container_tests!();
    let server = start().await;
    let mut con = raw(&server).await;
    for i in 0..300 {
        run(&mut con, &["SET", &format!("user:{i}"), "x"]).await;
    }
    // MONITOR on a side connection records every command the server runs.
    let mut monitor = TcpStream::connect((server.host.as_str(), server.port))
        .await
        .unwrap();
    monitor.write_all(b"MONITOR\r\n").await.unwrap();
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = log.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(monitor).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            sink.lock().unwrap().push(line);
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let conn = connect(&server, &[]).await;
    let keys = scan_all(kv(&conn), "*").await;
    assert_eq!(keys.len(), 300);
    scan_all(kv(&conn), "user:1*").await;
    let root = conn.browse(&NodePath::root()).await.unwrap();
    let db0 = conn.browse(&root[0].path).await.unwrap();
    conn.browse(&db0[0].path).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let log = log.lock().unwrap().clone();
    let commands: Vec<String> = log
        .iter()
        .filter_map(|l| l.split("] \"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_ascii_uppercase)
        .collect();
    assert!(commands.iter().any(|c| c == "SCAN"), "{log:?}");
    assert!(!commands.iter().any(|c| c == "KEYS"), "{log:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn scan_groups_and_types() {
    sqlr_testkit::skip_container_tests!();
    let server = start().await;
    let mut con = raw(&server).await;
    run(&mut con, &["SET", "user:1:name", "ann"]).await;
    run(&mut con, &["HSET", "user:1:profile", "age", "30"]).await;
    run(&mut con, &["RPUSH", "queue:jobs", "a", "b"]).await;
    run(&mut con, &["SADD", "tags", "x"]).await;
    run(&mut con, &["ZADD", "board", "1", "m"]).await;
    run(&mut con, &["XADD", "events", "*", "k", "v"]).await;
    run(&mut con, &["SET", "session", "s", "PX", "600000"]).await;
    run(&mut con, &["SELECT", "2"]).await;
    run(&mut con, &["SET", "other", "1"]).await;

    let conn = connect(&server, &[]).await;
    let kv = kv(&conn);
    assert_eq!(kv.databases(), 16);
    let keys: BTreeMap<Vec<u8>, KeyInfo> = scan_all(kv, "*")
        .await
        .into_iter()
        .map(|k| (k.name.clone(), k))
        .collect();
    let ty = |name: &str| keys[name.as_bytes()].ty.clone();
    assert_eq!(ty("user:1:name"), KeyType::String);
    assert_eq!(ty("user:1:profile"), KeyType::Hash);
    assert_eq!(ty("queue:jobs"), KeyType::List);
    assert_eq!(ty("tags"), KeyType::Set);
    assert_eq!(ty("board"), KeyType::ZSet);
    assert_eq!(ty("events"), KeyType::Stream);
    let ttl = keys[b"session".as_slice()].ttl_ms.expect("a TTL");
    assert!(ttl > 590_000 && ttl <= 600_000, "{ttl}");
    assert_eq!(keys[b"tags".as_slice()].ttl_ms, None);
    let users: Vec<Vec<u8>> = scan_all(kv, "user:*")
        .await
        .into_iter()
        .map(|k| k.name)
        .collect();
    assert_eq!(users.len(), 2);

    // The explorer: non-empty databases (and the configured one), then prefix groups.
    let root = conn.browse(&NodePath::root()).await.unwrap();
    let names: Vec<&str> = root.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, ["db0", "db2"]);
    assert_eq!(root[0].kind, NodeKind::Database);
    assert_eq!(root[0].detail.as_deref(), Some("7 keys"));
    let db0 = conn.browse(&root[0].path).await.unwrap();
    let shown: Vec<(&str, NodeKind)> = db0.iter().map(|n| (n.name.as_str(), n.kind)).collect();
    assert_eq!(
        shown,
        [
            ("queue", NodeKind::Folder),
            ("user", NodeKind::Folder),
            ("board", NodeKind::Table),
            ("events", NodeKind::Table),
            ("session", NodeKind::Table),
            ("tags", NodeKind::Table),
        ]
    );
    assert_eq!(db0[1].detail.as_deref(), Some("2 keys"));
    assert_eq!(db0[2].detail.as_deref(), Some("zset"));
    let user = conn.browse(&db0[1].path).await.unwrap();
    assert_eq!(user.len(), 1);
    assert_eq!(user[0].name, "1");
    assert_eq!(user[0].kind, NodeKind::Folder);
    let one = conn.browse(&user[0].path).await.unwrap();
    let leaves: Vec<&str> = one.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(leaves, ["name", "profile"]);
    assert_eq!(
        KvNodePath::key_of(&one[1].path),
        Some(b"user:1:profile".to_vec())
    );
    assert_eq!(KvNodePath::db_of(&one[1].path), Some(0));
}

/// Pages a value from the start until it is complete, appending pages.
async fn load_all(kv: &dyn KeyValueConn, key: &str, count: usize) -> (KvPage, usize) {
    let mut page = kv
        .page(0, key.as_bytes(), KvCursor::Start, count)
        .await
        .unwrap();
    let mut pages = 1;
    while let Some(next) = page.next() {
        let more = kv.page(0, key.as_bytes(), next, count).await.unwrap();
        // `HSCAN`/`SSCAN` collect at least `COUNT` elements and may overshoot by a bucket;
        // a string page is counted in bytes.
        if !matches!(more, KvPage::String { .. }) {
            assert!(
                more.len() <= count + 16,
                "a page holds about `count` elements, not {}",
                more.len()
            );
        }
        page.append(more);
        pages += 1;
        assert!(pages < 1000, "paging never ends");
    }
    (page, pages)
}

#[tokio::test(flavor = "multi_thread")]
async fn values_every_type_paged() {
    sqlr_testkit::skip_container_tests!();
    let server = start().await;
    let mut con = raw(&server).await;
    let mut pipe = redis::pipe();
    for i in 0..450 {
        pipe.cmd("RPUSH")
            .arg("list")
            .arg(format!("item{i}"))
            .ignore();
        pipe.cmd("HSET")
            .arg("hash")
            .arg(format!("f{i}"))
            .arg(i)
            .ignore();
        pipe.cmd("SADD").arg("set").arg(format!("m{i}")).ignore();
        pipe.cmd("ZADD")
            .arg("zset")
            .arg(i)
            .arg(format!("z{i}"))
            .ignore();
        pipe.cmd("XADD")
            .arg("stream")
            .arg(format!("{}-0", i + 1))
            .arg("n")
            .arg(i)
            .ignore();
    }
    pipe.query_async::<()>(&mut con).await.unwrap();
    let big = "x".repeat(3 * 1024 * 1024 + 5);
    redis::cmd("SET")
        .arg("big")
        .arg(&big)
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let conn = connect(&server, &[]).await;
    let kv = kv(&conn);

    let first = kv.page(0, b"list", KvCursor::Start, 200).await.unwrap();
    assert_eq!(first.len(), 200);
    assert_eq!(first.next(), Some(KvCursor::Index(200)));
    let (list, pages) = load_all(kv, "list", 200).await;
    assert_eq!(pages, 3);
    let KvPage::List { items, total, .. } = &list else {
        panic!("{list:?}")
    };
    assert_eq!(*total, 450);
    assert_eq!(items[449], b"item449");

    let (hash, _) = load_all(kv, "hash", 200).await;
    let KvPage::Hash { fields, .. } = &hash else {
        panic!("{hash:?}")
    };
    let names: HashSet<&[u8]> = fields.iter().map(|(f, _)| f.as_slice()).collect();
    assert_eq!(names.len(), 450);
    assert!(fields.contains(&(b"f7".to_vec(), b"7".to_vec())));

    let (set, _) = load_all(kv, "set", 200).await;
    assert_eq!(set.len(), 450);

    let (zset, pages) = load_all(kv, "zset", 200).await;
    assert_eq!(pages, 3);
    let KvPage::ZSet { members, total, .. } = &zset else {
        panic!("{zset:?}")
    };
    assert_eq!(*total, 450);
    assert_eq!(members[0], (b"z0".to_vec(), 0.0));
    assert_eq!(members[449], (b"z449".to_vec(), 449.0));

    let first = kv.page(0, b"stream", KvCursor::Start, 200).await.unwrap();
    assert_eq!(first.next(), Some(KvCursor::After("200-0".into())));
    let (stream, pages) = load_all(kv, "stream", 200).await;
    assert_eq!(pages, 3);
    let KvPage::Stream { entries, total, .. } = &stream else {
        panic!("{stream:?}")
    };
    assert_eq!(*total, 450);
    assert_eq!(entries.len(), 450);
    assert_eq!(
        entries[0],
        ("1-0".to_string(), vec![(b"n".to_vec(), b"0".to_vec())])
    );

    // A string preview is capped at 1 MiB, with more to load.
    let first = kv.page(0, b"big", KvCursor::Start, 200).await.unwrap();
    let KvPage::String { bytes, total_len } = &first else {
        panic!("not a string")
    };
    assert_eq!(bytes.len(), 1 << 20);
    assert_eq!(*total_len, big.len() as u64);
    assert_eq!(first.next(), Some(KvCursor::Index(1 << 20)));
    let (all, pages) = load_all(kv, "big", 200).await;
    assert_eq!(pages, 4);
    assert_eq!(all.len(), big.len());

    // Size and length of a key.
    let info = kv.key_info(0, b"hash").await.unwrap().expect("exists");
    assert_eq!(info.ty, KeyType::Hash);
    assert_eq!(info.length, Some(450));
    assert!(info.size.is_some_and(|s| s > 0));
    assert_eq!(kv.key_info(0, b"missing").await.unwrap(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn huge_values_paged() {
    sqlr_testkit::skip_container_tests!();
    const FIELDS: usize = 5_000_000;
    let server = start().await;
    let mut con = raw(&server).await;
    // Pipelined HSETs of 1000 fields each.
    for batch in 0..FIELDS / 50_000 {
        let mut pipe = redis::pipe();
        for chunk in 0..50 {
            let mut cmd = redis::cmd("HSET");
            cmd.arg("huge");
            let base = batch * 50_000 + chunk * 1000;
            for i in base..base + 1000 {
                cmd.arg(i).arg(1);
            }
            pipe.add_command(cmd).ignore();
        }
        pipe.query_async::<()>(&mut con).await.unwrap();
    }
    let conn = connect(&server, &[]).await;
    let kv = kv(&conn);
    let info = kv.key_info(0, b"huge").await.unwrap().expect("exists");
    assert_eq!(info.length, Some(FIELDS as u64));
    // Warm the connection, then time a first page.
    kv.page(0, b"huge", KvCursor::Start, 200).await.unwrap();
    let started = Instant::now();
    let page = kv.page(0, b"huge", KvCursor::Start, 200).await.unwrap();
    let took = started.elapsed();
    assert!(took < Duration::from_millis(100), "{took:?}");
    let KvPage::Hash { fields, cursor } = &page else {
        panic!("not a hash")
    };
    // `HSCAN`'s COUNT is a hint: a page ends at a bucket boundary, so it may hold a few
    // more fields than asked for, never the whole hash.
    assert!(
        !fields.is_empty() && fields.len() <= 200 + 16,
        "{}",
        fields.len()
    );
    assert_ne!(*cursor, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_every_type() {
    sqlr_testkit::skip_container_tests!();
    let server = start().await;
    let conn = connect(&server, &[]).await;
    let kv = kv(&conn);
    let page = |key: &'static str| async move {
        kv.page(0, key.as_bytes(), KvCursor::Start, 200)
            .await
            .unwrap()
    };

    kv.edit(0, b"s", KvEdit::SetString(b"one".to_vec()))
        .await
        .unwrap();
    assert_eq!(
        page("s").await,
        KvPage::String {
            bytes: b"one".to_vec(),
            total_len: 3
        }
    );

    kv.edit(0, b"h", KvEdit::HashSet(b"a".to_vec(), b"1".to_vec()))
        .await
        .unwrap();
    kv.edit(0, b"h", KvEdit::HashSet(b"b".to_vec(), b"2".to_vec()))
        .await
        .unwrap();
    kv.edit(0, b"h", KvEdit::HashSet(b"a".to_vec(), b"3".to_vec()))
        .await
        .unwrap();
    kv.edit(0, b"h", KvEdit::HashDel(b"b".to_vec()))
        .await
        .unwrap();
    let KvPage::Hash { fields, .. } = page("h").await else {
        panic!()
    };
    assert_eq!(fields, vec![(b"a".to_vec(), b"3".to_vec())]);

    for (left, v) in [(false, "b"), (false, "c"), (true, "a")] {
        kv.edit(
            0,
            b"l",
            KvEdit::ListPush {
                left,
                value: v.into(),
            },
        )
        .await
        .unwrap();
    }
    kv.edit(0, b"l", KvEdit::ListSet(1, b"B".to_vec()))
        .await
        .unwrap();
    kv.edit(
        0,
        b"l",
        KvEdit::ListRem {
            index: 2,
            value: b"c".to_vec(),
        },
    )
    .await
    .unwrap();
    let KvPage::List { items, total, .. } = page("l").await else {
        panic!()
    };
    assert_eq!(items, vec![b"a".to_vec(), b"B".to_vec()]);
    assert_eq!(total, 2);

    kv.edit(0, b"set", KvEdit::SetAdd(b"x".to_vec()))
        .await
        .unwrap();
    kv.edit(0, b"set", KvEdit::SetAdd(b"y".to_vec()))
        .await
        .unwrap();
    kv.edit(0, b"set", KvEdit::SetRem(b"x".to_vec()))
        .await
        .unwrap();
    let KvPage::Set { members, .. } = page("set").await else {
        panic!()
    };
    assert_eq!(members, vec![b"y".to_vec()]);

    kv.edit(0, b"z", KvEdit::ZAdd(b"m".to_vec(), 2.5))
        .await
        .unwrap();
    kv.edit(0, b"z", KvEdit::ZAdd(b"n".to_vec(), 1.0))
        .await
        .unwrap();
    kv.edit(0, b"z", KvEdit::ZAdd(b"m".to_vec(), 0.5))
        .await
        .unwrap();
    kv.edit(0, b"z", KvEdit::ZRem(b"n".to_vec())).await.unwrap();
    let KvPage::ZSet { members, .. } = page("z").await else {
        panic!()
    };
    assert_eq!(members, vec![(b"m".to_vec(), 0.5)]);

    kv.edit(0, b"x", KvEdit::XAdd(vec![(b"k".to_vec(), b"v".to_vec())]))
        .await
        .unwrap();
    kv.edit(0, b"x", KvEdit::XAdd(vec![(b"k".to_vec(), b"w".to_vec())]))
        .await
        .unwrap();
    let KvPage::Stream { entries, .. } = page("x").await else {
        panic!()
    };
    assert_eq!(entries.len(), 2);
    kv.edit(0, b"x", KvEdit::XDel(entries[0].0.clone()))
        .await
        .unwrap();
    let KvPage::Stream { entries, total, .. } = page("x").await else {
        panic!()
    };
    assert_eq!(total, 1);
    assert_eq!(entries[0].1, vec![(b"k".to_vec(), b"w".to_vec())]);

    // Rename refuses to overwrite an existing key.
    kv.edit(0, b"s", KvEdit::Rename(b"s2".to_vec()))
        .await
        .unwrap();
    assert_eq!(kv.key_info(0, b"s").await.unwrap(), None);
    assert!(kv.key_info(0, b"s2").await.unwrap().is_some());
    let e = kv
        .edit(0, b"s2", KvEdit::Rename(b"h".to_vec()))
        .await
        .unwrap_err();
    assert!(e.message.contains("already exists"), "{e:?}");

    // Binary-safe names and values.
    let name = b"bin:\xff\x00".to_vec();
    kv.edit(0, &name, KvEdit::SetString(vec![0, 159, 146, 150]))
        .await
        .unwrap();
    assert_eq!(
        kv.page(0, &name, KvCursor::Start, 200).await.unwrap(),
        KvPage::String {
            bytes: vec![0, 159, 146, 150],
            total_len: 4
        }
    );
    let found = scan_all(kv, "bin:*").await;
    assert_eq!(found[0].name, name);

    kv.edit(0, b"h", KvEdit::Delete).await.unwrap();
    assert_eq!(kv.key_info(0, b"h").await.unwrap(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn ttl_and_persist() {
    sqlr_testkit::skip_container_tests!();
    let server = start().await;
    let conn = connect(&server, &[]).await;
    let kv = kv(&conn);
    kv.edit(0, b"k", KvEdit::SetString(b"v".to_vec()))
        .await
        .unwrap();
    assert_eq!(kv.key_info(0, b"k").await.unwrap().unwrap().ttl_ms, None);
    kv.edit(0, b"k", KvEdit::Expire(Some(60_000)))
        .await
        .unwrap();
    let ttl = kv.key_info(0, b"k").await.unwrap().unwrap().ttl_ms.unwrap();
    assert!(ttl > 55_000 && ttl <= 60_000, "{ttl}");
    // Setting the string keeps its TTL.
    kv.edit(0, b"k", KvEdit::SetString(b"w".to_vec()))
        .await
        .unwrap();
    assert!(
        kv.key_info(0, b"k")
            .await
            .unwrap()
            .unwrap()
            .ttl_ms
            .is_some()
    );
    kv.edit(0, b"k", KvEdit::Expire(None)).await.unwrap();
    assert_eq!(kv.key_info(0, b"k").await.unwrap().unwrap().ttl_ms, None);
    // An edit of a key that does not exist fails clearly.
    let e = kv
        .edit(0, b"nope", KvEdit::Expire(Some(10)))
        .await
        .unwrap_err();
    assert!(e.message.contains("does not exist"), "{e:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn console_resp_types_and_multi_exec() {
    sqlr_testkit::skip_container_tests!();
    let server = start().await;
    let conn = connect(&server, &[]).await;
    let kv = kv(&conn);
    let c = |line: &[&str]| kv.command(0, args(line));
    assert_eq!(
        c(&["PING"]).await.unwrap(),
        RespValue::Simple("PONG".into())
    );
    assert_eq!(
        c(&["SET", "a", "1"]).await.unwrap(),
        RespValue::Simple("OK".into())
    );
    assert_eq!(
        c(&["GET", "a"]).await.unwrap(),
        RespValue::Bulk(b"1".to_vec())
    );
    assert_eq!(c(&["GET", "missing"]).await.unwrap(), RespValue::Nil);
    assert_eq!(c(&["INCR", "a"]).await.unwrap(), RespValue::Int(2));
    c(&["HSET", "h", "f", "v"]).await.unwrap();
    assert_eq!(
        c(&["HGETALL", "h"]).await.unwrap(),
        RespValue::Map(vec![(
            RespValue::Bulk(b"f".to_vec()),
            RespValue::Bulk(b"v".to_vec())
        )])
    );
    c(&["ZADD", "z", "1.5", "m"]).await.unwrap();
    assert_eq!(
        c(&["ZSCORE", "z", "m"]).await.unwrap(),
        RespValue::Double(1.5)
    );
    assert!(matches!(
        c(&["RPUSH", "l", "x", "y"]).await.unwrap(),
        RespValue::Int(2)
    ));
    assert_eq!(
        c(&["LRANGE", "l", "0", "-1"]).await.unwrap(),
        RespValue::Array(vec![
            RespValue::Bulk(b"x".to_vec()),
            RespValue::Bulk(b"y".to_vec())
        ])
    );
    let RespValue::Verbatim(info) = c(&["INFO", "server"]).await.unwrap() else {
        panic!("INFO is a verbatim string in RESP3")
    };
    assert!(info.contains("redis_version"));
    let RespValue::Error(e) = c(&["HGET", "a", "f"]).await.unwrap() else {
        panic!("a server error is a reply")
    };
    assert!(e.starts_with("WRONGTYPE"), "{e}");
    // Commands that would break the shared connection are refused.
    for refused in [
        &["SUBSCRIBE", "ch"][..],
        &["SELECT", "1"],
        &["MONITOR"],
        &["QUIT"],
    ] {
        assert!(
            matches!(c(refused).await.unwrap(), RespValue::Error(_)),
            "{refused:?}"
        );
    }
    // The connection still works afterwards.
    assert_eq!(
        c(&["GET", "a"]).await.unwrap(),
        RespValue::Bulk(b"2".to_vec())
    );

    let reply = kv
        .exec_block(
            0,
            vec![
                args(&["SET", "t", "1"]),
                args(&["INCR", "t"]),
                args(&["LPUSH", "t", "x"]),
            ],
        )
        .await
        .unwrap();
    let RespValue::Array(items) = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(items[0], RespValue::Simple("OK".into()));
    assert_eq!(items[1], RespValue::Int(2));
    assert!(matches!(&items[2], RespValue::Error(e) if e.starts_with("WRONGTYPE")));

    // Another database.
    kv.command(3, args(&["SET", "in3", "y"])).await.unwrap();
    assert_eq!(c(&["EXISTS", "in3"]).await.unwrap(), RespValue::Int(0));
    assert_eq!(
        kv.command(3, args(&["GET", "in3"])).await.unwrap(),
        RespValue::Bulk(b"y".to_vec())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_refuses_writes() {
    sqlr_testkit::skip_container_tests!();
    let server = start().await;
    let mut con = raw(&server).await;
    run(&mut con, &["SET", "k", "v"]).await;
    let conn = connect(&server, &[("read_only", "true")]).await;
    let kv = kv(&conn);
    assert_eq!(kv.classify(&args(&["GET", "k"])), StatementClass::Read);
    assert_eq!(
        kv.classify(&args(&["set", "k", "v"])),
        StatementClass::Write
    );
    assert!(matches!(
        kv.classify(&args(&["FLUSHDB"])),
        StatementClass::Destructive(_)
    ));
    assert!(matches!(
        kv.classify(&args(&["DEL", "a", "b"])),
        StatementClass::Destructive(_)
    ));
    assert_eq!(kv.classify(&args(&["DEL", "a"])), StatementClass::Write);
    assert_eq!(
        kv.classify(&args(&["CONFIG", "GET", "x"])),
        StatementClass::Read
    );
    assert_eq!(
        kv.classify(&args(&["CONFIG", "SET", "x", "y"])),
        StatementClass::Other
    );
    assert_eq!(kv.classify(&args(&["MYMODULE.DO"])), StatementClass::Other);

    // Reads work.
    assert_eq!(scan_all(kv, "*").await.len(), 1);
    assert_eq!(
        kv.command(0, args(&["GET", "k"])).await.unwrap(),
        RespValue::Bulk(b"v".to_vec())
    );
    // Writes are refused before they reach the server.
    let e = kv
        .edit(0, b"k", KvEdit::SetString(b"w".to_vec()))
        .await
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permission);
    for line in [
        &["SET", "k", "w"][..],
        &["FLUSHALL"],
        &["EVAL", "return 1", "0"],
    ] {
        let e = kv.command(0, args(line)).await.unwrap_err();
        assert_eq!(e.kind, ErrorKind::Permission, "{line:?}");
    }
    let e = kv
        .exec_block(0, vec![args(&["GET", "k"]), args(&["DEL", "k"])])
        .await
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permission);
    assert_eq!(
        run(&mut con, &["GET", "k"]).await,
        redis::Value::BulkString(b"v".to_vec())
    );

    kv.set_read_only(false);
    kv.edit(0, b"k", KvEdit::SetString(b"w".to_vec()))
        .await
        .unwrap();
    assert_eq!(
        run(&mut con, &["GET", "k"]).await,
        redis::Value::BulkString(b"w".to_vec())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_and_acl_auth() {
    sqlr_testkit::skip_container_tests!();
    let server = start_tls().await;
    let port = server.port;
    let ca_path = sqlr_testkit::ca_path();
    let ca_path = ca_path.to_str().unwrap().to_string();
    let driver = RedisDriver::new();
    let secrets = |pw: &str| Secrets([("password".to_string(), pw.to_string())].into());
    let tls = [
        ("tls", "required"),
        ("tls_ca", ca_path.as_str()),
        ("user", "alice"),
    ];

    let conn = driver
        .connect(&config("localhost", port, &tls), &secrets("s3cret"))
        .await
        .expect("TLS with the CA and the ACL user");
    let info = conn.ping().await.unwrap();
    assert_eq!(info.product, "Redis");
    assert!(info.version.starts_with('7'), "{info:?}");
    let kv = kv(&conn);
    kv.edit(0, b"k", KvEdit::SetString(b"v".to_vec()))
        .await
        .unwrap();
    assert_eq!(scan_all(kv, "*").await.len(), 1);

    let e = driver
        .connect(&config("localhost", port, &tls), &secrets("wrong"))
        .await
        .err()
        .expect("a wrong password fails");
    assert_eq!(e.kind, ErrorKind::Auth, "{e:?}");
    assert!(
        !e.message.contains("wrong"),
        "the password never shows: {e:?}"
    );

    let skip = [("tls", "allow_invalid"), ("user", "alice")];
    driver
        .connect(&config("localhost", port, &skip), &secrets("s3cret"))
        .await
        .expect("TLS without verification");

    // Without the CA the server's certificate is not trusted.
    let untrusted = [("tls", "required"), ("user", "alice")];
    let e = driver
        .connect(&config("localhost", port, &untrusted), &secrets("s3cret"))
        .await
        .err()
        .expect("an unknown CA fails");
    assert_eq!(e.kind, ErrorKind::Connection, "{e:?}");

    // Plain TCP against a TLS port fails within the timeout instead of hanging.
    let started = Instant::now();
    let e = driver
        .connect(
            &config("localhost", port, &[("user", "alice")]),
            &secrets("s3cret"),
        )
        .await
        .err()
        .expect("no TLS against a TLS port fails");
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(
        matches!(e.kind, ErrorKind::Connection | ErrorKind::Timeout),
        "{e:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scan_resumes_after_reconnect() {
    sqlr_testkit::skip_container_tests!();
    let server = start().await;
    let mut con = raw(&server).await;
    let mut pipe = redis::pipe();
    for i in 0..3000 {
        pipe.cmd("SET").arg(format!("k{i}")).arg(i).ignore();
    }
    pipe.query_async::<()>(&mut con).await.unwrap();
    let conn = connect(&server, &[]).await;
    let kv = kv(&conn);
    let (first, cursor) = kv.scan(0, b"*", 0, 500).await.unwrap();
    assert_ne!(cursor, 0);
    let mut names: Vec<Vec<u8>> = first.into_iter().map(|k| k.name).collect();
    // The server drops every other client mid-scan.
    let killed = run(
        &mut con,
        &["CLIENT", "KILL", "TYPE", "normal", "SKIPME", "yes"],
    )
    .await;
    assert_ne!(killed, redis::Value::Int(0));
    let mut cursor = cursor;
    while cursor != 0 {
        let (keys, next) = kv.scan(0, b"*", cursor, 500).await.expect("resumes");
        names.extend(keys.into_iter().map(|k| k.name));
        cursor = next;
    }
    let unique: HashSet<&Vec<u8>> = names.iter().collect();
    assert_eq!(unique.len(), 3000);
    assert_eq!(names.len(), 3000, "no key comes back twice");
}

/// A scripted server that answers like a Redis Cluster node: `INFO` reports
/// `cluster_enabled:<info_cluster>` and every key command is redirected with `MOVED`.
async fn fake_cluster(info_cluster: bool) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(serve_fake(socket, info_cluster));
        }
    });
    port
}

async fn serve_fake(socket: TcpStream, info_cluster: bool) {
    let mut reader = BufReader::new(socket);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let Some(n) = line
            .trim()
            .strip_prefix('*')
            .and_then(|n| n.parse::<usize>().ok())
        else {
            return;
        };
        let mut parts = Vec::new();
        for _ in 0..n {
            let mut len = String::new();
            reader.read_line(&mut len).await.unwrap();
            let len: usize = len.trim().trim_start_matches('$').parse().unwrap();
            let mut buf = vec![0; len + 2];
            reader.read_exact(&mut buf).await.unwrap();
            buf.truncate(len);
            parts.push(String::from_utf8_lossy(&buf).to_ascii_uppercase());
        }
        let reply: Vec<u8> = match parts.first().map(String::as_str) {
            Some("HELLO") => b"%2\r\n+server\r\n+redis\r\n+proto\r\n:3\r\n".to_vec(),
            Some("CLIENT" | "SELECT" | "PING") => b"+OK\r\n".to_vec(),
            Some("INFO") => {
                let body = format!(
                    "# Cluster\r\ncluster_enabled:{}\r\n",
                    u8::from(info_cluster)
                );
                format!("${}\r\n{body}\r\n", body.len()).into_bytes()
            }
            Some("CONFIG") => b"*2\r\n$9\r\ndatabases\r\n$2\r\n16\r\n".to_vec(),
            _ => b"-MOVED 3999 127.0.0.1:6381\r\n".to_vec(),
        };
        if reader.get_mut().write_all(&reply).await.is_err() {
            return;
        }
    }
}

fn is_cluster_refusal(e: &DriverError) -> bool {
    e.kind == ErrorKind::Unsupported && e.message.contains("Redis Cluster")
}

#[tokio::test(flavor = "multi_thread")]
async fn cluster_refused() {
    sqlr_testkit::skip_container_tests!();
    let driver = RedisDriver::new();
    let port = fake_cluster(true).await;
    let e = driver
        .connect(&config("127.0.0.1", port, &[]), &Secrets::default())
        .await
        .err()
        .expect("a cluster endpoint is refused");
    assert!(is_cluster_refusal(&e), "{e:?}");

    // A node that hides `cluster_enabled` is refused at its first redirect.
    let port = fake_cluster(false).await;
    let conn = driver
        .connect(&config("127.0.0.1", port, &[]), &Secrets::default())
        .await
        .expect("connects");
    let e = kv(&conn).scan(0, b"*", 0, 500).await.unwrap_err();
    assert!(is_cluster_refusal(&e), "{e:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn create_never_overwrites_an_existing_key() {
    sqlr_testkit::skip_container_tests!();
    let server = start().await;
    let conn = connect(&server, &[]).await;
    let kv = kv(&conn);
    let create = |edit: KvEdit| KvEdit::Create(Box::new(edit));
    kv.edit(0, b"h", KvEdit::HashSet(b"f".to_vec(), b"v".to_vec()))
        .await
        .unwrap();
    // A string over a hash, and the other types over the same key, are refused.
    for edit in [
        KvEdit::SetString(b"x".to_vec()),
        KvEdit::HashSet(b"g".to_vec(), b"w".to_vec()),
        KvEdit::ListPush {
            left: false,
            value: b"x".to_vec(),
        },
        KvEdit::SetAdd(b"x".to_vec()),
        KvEdit::ZAdd(b"x".to_vec(), 1.0),
        KvEdit::XAdd(vec![(b"a".to_vec(), b"b".to_vec())]),
    ] {
        let e = kv.edit(0, b"h", create(edit.clone())).await.unwrap_err();
        assert!(e.message.contains("already exists"), "{edit:?}: {e:?}");
    }
    // The hash is intact.
    let KvPage::Hash { fields, .. } = kv.page(0, b"h", KvCursor::Start, 200).await.unwrap() else {
        panic!("still a hash")
    };
    assert_eq!(fields, vec![(b"f".to_vec(), b"v".to_vec())]);
    // Creating a new key works for every type.
    for (name, edit) in [
        ("s", KvEdit::SetString(b"x".to_vec())),
        ("h2", KvEdit::HashSet(b"g".to_vec(), b"w".to_vec())),
        (
            "l",
            KvEdit::ListPush {
                left: false,
                value: b"x".to_vec(),
            },
        ),
        ("set", KvEdit::SetAdd(b"x".to_vec())),
        ("z", KvEdit::ZAdd(b"x".to_vec(), 1.0)),
        ("x", KvEdit::XAdd(vec![(b"a".to_vec(), b"b".to_vec())])),
    ] {
        kv.edit(0, name.as_bytes(), create(edit)).await.unwrap();
        assert!(
            kv.key_info(0, name.as_bytes()).await.unwrap().is_some(),
            "{name}"
        );
    }
    // Other edits cannot be wrapped.
    let e = kv.edit(0, b"s", create(KvEdit::Delete)).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Syntax);
}

#[tokio::test(flavor = "multi_thread")]
async fn list_remove_checks_the_index_and_the_value() {
    sqlr_testkit::skip_container_tests!();
    let server = start().await;
    let mut con = raw(&server).await;
    run(&mut con, &["RPUSH", "l", "x", "a", "x"]).await;
    let conn = connect(&server, &[]).await;
    let kv = kv(&conn);
    let items = |page: KvPage| match page {
        KvPage::List { items, .. } => items,
        other => panic!("{other:?}"),
    };
    // Removing index 2 takes the last `x`, not the first.
    kv.edit(
        0,
        b"l",
        KvEdit::ListRem {
            index: 2,
            value: b"x".to_vec(),
        },
    )
    .await
    .unwrap();
    let after = items(kv.page(0, b"l", KvCursor::Start, 200).await.unwrap());
    assert_eq!(after, vec![b"x".to_vec(), b"a".to_vec()]);
    // The list changed since it was shown: refused, nothing removed.
    let e = kv
        .edit(
            0,
            b"l",
            KvEdit::ListRem {
                index: 1,
                value: b"x".to_vec(),
            },
        )
        .await
        .unwrap_err();
    assert!(e.message.contains("changed"), "{e:?}");
    let e = kv
        .edit(
            0,
            b"l",
            KvEdit::ListRem {
                index: 9,
                value: b"x".to_vec(),
            },
        )
        .await
        .unwrap_err();
    assert!(e.message.contains("changed"), "{e:?}");
    let after = items(kv.page(0, b"l", KvCursor::Start, 200).await.unwrap());
    assert_eq!(after, vec![b"x".to_vec(), b"a".to_vec()]);
    // The key is not left with a tombstone.
    assert!(!after.iter().any(|i| i.starts_with(b"\0sqlr")));
}
