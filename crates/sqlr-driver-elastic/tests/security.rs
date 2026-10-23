//! Security regressions use a local HTTP peer; no Elasticsearch or Docker is needed.

use sqlr_core::{ConnectionConfig, Driver, ErrorKind, Secrets};
use sqlr_driver_elastic::ElasticDriver;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn ambiguous_console_paths_never_reach_the_server() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        loop {
            let (mut socket, _) = tokio::select! {
                _ = &mut stopped => break,
                accepted = listener.accept() => accepted.unwrap(),
            };
            let mut bytes = Vec::new();
            loop {
                assert_ne!(socket.read_buf(&mut bytes).await.unwrap(), 0);
                if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .next()
                    .unwrap()
                    .to_owned(),
            );
            let body = r#"{"version":{"number":"8.15.0"}}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        requests
    });
    let config = ConnectionConfig {
        id: "security-test".into(),
        name: "Local security peer".into(),
        driver: "elasticsearch".into(),
        group: None,
        color: None,
        values: [
            ("host".into(), "127.0.0.1".into()),
            ("port".into(), port.to_string()),
            ("read_only".into(), "true".into()),
        ]
        .into(),
        profile: None,
        save_secrets: false,
    };
    let connection = ElasticDriver::new()
        .connect(&config, &Secrets::default())
        .await
        .unwrap();
    let documents = connection.document().unwrap();
    for read_only in [true, false] {
        documents.set_read_only(read_only);
        for sql in [
            "DELETE /victim#/_search",
            "POST /idx/_search/..\\_delete_by_query\n{}",
            "POST /idx/_search?query=x#fragment\n{}",
        ] {
            let error = match documents.run(None, sql).await {
                Err(error) => error,
                Ok(_) => panic!("ambiguous path was sent: {sql}"),
            };
            assert_eq!(error.kind, ErrorKind::Syntax, "{sql}: {error}");
        }
    }
    stop.send(()).unwrap();
    assert_eq!(server.await.unwrap(), ["GET / HTTP/1.1"]);
}
