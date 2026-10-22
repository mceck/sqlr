//! A starting point for an sqlr driver. It answers `SELECT 1` and nothing else: replace
//! `MyConnection::execute` with calls to your database, fill in the manifest, and run the
//! conformance suite (see README.md).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use sqlr_driver_sdk::*;

pub struct MyDriver {
    manifest: DriverManifest,
}

impl MyDriver {
    pub fn new() -> Self {
        Self {
            manifest: DriverManifest {
                // The id is what saved connections name: choose it once.
                id: "{{project-name}}".into(),
                name: "My database".into(),
                family: Family::Relational,
                capabilities: Capabilities {
                    server_cancel: true,
                    ..Capabilities::default()
                },
                // The connection form is generated from these fields. Fields marked
                // `secret` are kept in the OS keychain and reach `connect` as `secrets`.
                fields: vec![
                    FieldSpec {
        free_form: false,
                        key: "host".into(),
                        label: "Host".into(),
                        ty: FieldType::Text,
                        default: Some("localhost".into()),
                        required: true,
                        secret: false,
                        group: "Main".into(),
                    },
                    FieldSpec {
        free_form: false,
                        key: "password".into(),
                        label: "Password".into(),
                        ty: FieldType::Password,
                        default: None,
                        required: false,
                        secret: true,
                        group: "Main".into(),
                    },
                ],
                dialect: "ansi".into(),
                url_schemes: Vec::new(),
                default_port: None,
                editing: None,
            },
        }
    }
}

impl Default for MyDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for MyDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    async fn connect(
        &self,
        _cfg: &ConnectionConfig,
        _secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        // Open your database connection here.
        Ok(Arc::new(MyConnection::default()))
    }
}

#[derive(Default)]
struct MyConnection {
    cancelled: Arc<AtomicBool>,
    dialect: MyDialect,
}

#[derive(Default)]
struct MyDialect;

impl Dialect for MyDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    fn select_all(&self, node: &Node, limit: Option<usize>) -> Option<String> {
        node.kind.opens_data().then(|| {
            let mut sql = format!("SELECT * FROM {}", self.quote_ident(&node.name));
            if let Some(n) = limit {
                sql.push_str(&format!(" LIMIT {n}"));
            }
            sql
        })
    }
}

struct Cancel(Arc<AtomicBool>);

#[async_trait]
impl CancelHandle for Cancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        // Ask your database to stop the running statement.
        self.0.store(true, Ordering::Release);
        Ok(())
    }
}

#[async_trait]
impl Connection for MyConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        Ok(ServerInfo {
            product: "My database".into(),
            version: "0".into(),
        })
    }

    async fn browse(&self, _path: &NodePath) -> Result<Vec<Node>, DriverError> {
        Ok(Vec::new())
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(Cancel(self.cancelled.clone()))
    }

    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }
}

#[async_trait]
impl RelationalConn for MyConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        if sql.trim().eq_ignore_ascii_case("select 1") {
            let columns: Arc<[ColumnMeta]> = Arc::from(vec![ColumnMeta::new(
                "1",
                "integer",
                ColumnKind::Int,
                DisplayHint::Number,
            )]);
            let mut builder = ChunkBuilder::new(columns.clone());
            builder.push_row(&[Cell::Int(1)]).expect("one int column");
            return Ok(Execution::Rows(Box::new(OneChunk {
                columns,
                chunk: Some(builder.finish()),
            })));
        }
        Err(DriverError::new(ErrorKind::Syntax, "only SELECT 1 is implemented"))
    }

    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }
}

struct OneChunk {
    columns: Arc<[ColumnMeta]>,
    chunk: Option<ResultChunk>,
}

#[async_trait]
impl RowStream for OneChunk {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }

    async fn next_chunk(&mut self, _max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        Ok(self.chunk.take())
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        self.chunk = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn select_one_works() {
        let driver = MyDriver::new();
        let conn = driver
            .connect(&ConnectionConfig {
                id: "t".into(),
                name: "t".into(),
                driver: driver.manifest().id.clone(),
                group: None,
                color: None,
                values: Default::default(),
                profile: None,
                save_secrets: true,
            }, &Secrets::default())
            .await
            .unwrap();
        let Execution::Rows(mut rows) = conn.relational().unwrap().execute("SELECT 1").await.unwrap()
        else {
            panic!("rows expected");
        };
        let chunk = rows.next_chunk(10).await.unwrap().unwrap();
        assert_eq!(chunk.cell(0, 0), Cell::Int(1));
    }

    // Once the driver talks to a real database, run the shared conformance suite:
    //
    //     let report = sqlr_driver_tests::relational_suite(&target).await;
    //     assert!(report.failed.is_empty(), "{report}");
}
