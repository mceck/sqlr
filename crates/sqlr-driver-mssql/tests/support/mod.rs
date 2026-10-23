//! SQL Server 2025 for the integration tests: one fixed server, one test at a time.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{Connection, ConnectionConfig, Driver, Secrets};
use sqlr_driver_mssql::SqlServerDriver;
use sqlr_testkit::{TestLock, service};

const PASSWORD: &str = "yourStrong(!)Password";

pub async fn server() -> (TestLock, Arc<dyn Connection>) {
    let (host, port) = service("MSSQL");
    let lock = TestLock::acquire("mssql");
    let cfg = ConnectionConfig {
        id: "t".into(),
        name: "test".into(),
        driver: "sqlserver".into(),
        group: None,
        color: None,
        values: [
            ("host", host),
            ("port", port.to_string()),
            ("database", "master".into()),
            ("user", "sa".into()),
            ("encryption", "not_supported".into()),
            ("trust_server_certificate", "true".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    let secrets = Secrets([("password".to_string(), PASSWORD.to_string())].into());
    let deadline = Instant::now() + Duration::from_secs(180);
    let conn = loop {
        match SqlServerDriver::new().connect(&cfg, &secrets).await {
            Ok(conn) => break conn,
            Err(e) if Instant::now() > deadline => panic!("sql server not ready: {e:?}"),
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    };
    cleanup(&conn).await;
    (lock, conn)
}

/// Drops the schemas and tables the two integration files create. SQL Server has no
/// `DROP SCHEMA ... CASCADE`, so foreign keys and views go first.
async fn cleanup(conn: &Arc<dyn Connection>) {
    conn.relational()
        .expect("relational")
        .execute(
            r#"
IF OBJECT_ID('dbo.up', 'U') IS NOT NULL DROP TABLE dbo.up;
IF SCHEMA_ID('shop') IS NOT NULL
BEGIN
    DECLARE @sql nvarchar(max) = N'';
    SELECT @sql += N'ALTER TABLE ' + QUOTENAME(OBJECT_SCHEMA_NAME(parent_object_id)) + N'.' +
        QUOTENAME(OBJECT_NAME(parent_object_id)) + N' DROP CONSTRAINT ' + QUOTENAME(name) + N';'
    FROM sys.foreign_keys
    WHERE OBJECT_SCHEMA_NAME(parent_object_id) = 'shop';
    SELECT @sql += N'DROP VIEW ' + QUOTENAME(OBJECT_SCHEMA_NAME(object_id)) + N'.' +
        QUOTENAME(name) + N';'
    FROM sys.views WHERE OBJECT_SCHEMA_NAME(object_id) = 'shop';
    SELECT @sql += N'DROP PROCEDURE ' + QUOTENAME(OBJECT_SCHEMA_NAME(object_id)) + N'.' +
        QUOTENAME(name) + N';'
    FROM sys.procedures WHERE OBJECT_SCHEMA_NAME(object_id) = 'shop';
    SELECT @sql += N'DROP FUNCTION ' + QUOTENAME(OBJECT_SCHEMA_NAME(object_id)) + N'.' +
        QUOTENAME(name) + N';'
    FROM sys.objects WHERE OBJECT_SCHEMA_NAME(object_id) = 'shop' AND type IN ('FN', 'IF', 'TF');
    SELECT @sql += N'DROP TABLE ' + QUOTENAME(OBJECT_SCHEMA_NAME(object_id)) + N'.' +
        QUOTENAME(name) + N';'
    FROM sys.tables WHERE OBJECT_SCHEMA_NAME(object_id) = 'shop';
    SELECT @sql += N'DROP SEQUENCE ' + QUOTENAME(OBJECT_SCHEMA_NAME(object_id)) + N'.' +
        QUOTENAME(name) + N';'
    FROM sys.sequences WHERE OBJECT_SCHEMA_NAME(object_id) = 'shop';
    EXEC sp_executesql @sql;
    DROP SCHEMA shop;
END
"#,
        )
        .await
        .expect("reset sql server");
}
