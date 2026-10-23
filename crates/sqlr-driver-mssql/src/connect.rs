//! Connection setup for the SQL Server driver.

use std::sync::Arc;

use sqlr_core::{
    CancelHandle, Connection, ConnectionConfig, Dialect, Driver, DriverError, DriverManifest,
    ErrorKind, Execution, Introspect, Node, NodeKind, NodePath, ObjectDef, ObjectRef,
    RelationalConn, Secrets, ServerInfo, TableInfo, TableRef, Value,
};
use tiberius::{AuthMethod, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::TokioAsyncWriteCompatExt;

use crate::browse::MsClient;
use crate::dialect::MsDialect;
use crate::{browse, catalog, describe, execute, introspect, manifest, map_error};

pub struct SqlServerDriver {
    manifest: DriverManifest,
}

impl SqlServerDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            manifest: manifest::manifest(),
        })
    }
}

pub struct MsConnection {
    client: Arc<Mutex<MsClient>>,
    /// The server address, for the side connection a cancel opens.
    config: Config,
    dial_host: String,
    port: u16,
    dialect: MsDialect,
    database: String,
    /// `@@SPID`, the session `KILL` stops.
    spid: Option<i32>,
}

impl MsConnection {
    fn encryption(value: &str) -> Result<EncryptionLevel, DriverError> {
        match value {
            "off" => Ok(EncryptionLevel::Off),
            "on" => Ok(EncryptionLevel::On),
            "required" | "" => Ok(EncryptionLevel::Required),
            "not_supported" => Ok(EncryptionLevel::NotSupported),
            other => Err(DriverError::new(
                ErrorKind::Other,
                format!("invalid encryption mode `{other}`"),
            )),
        }
    }

    async fn open(config: Config, dial_host: &str, port: u16) -> Result<MsClient, DriverError> {
        let tcp = TcpStream::connect((dial_host, port))
            .await
            .map_err(map_error)?;
        tcp.set_nodelay(true).map_err(map_error)?;
        tiberius::Client::connect(config, tcp.compat_write())
            .await
            .map_err(map_error)
    }
}

#[async_trait::async_trait]
impl Driver for SqlServerDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    fn dialect(&self) -> Option<Arc<dyn Dialect>> {
        Some(Arc::new(MsDialect))
    }

    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        sqlr_core::validate_driver_properties(&self.manifest.fields, &cfg.values)?;
        let m = &self.manifest;
        let text = |key: &str| {
            cfg.value(m, key).ok_or_else(|| {
                DriverError::new(
                    ErrorKind::Other,
                    format!("missing connection field `{key}`"),
                )
            })
        };
        let port: u16 = text("port")?
            .parse()
            .map_err(|_| DriverError::new(ErrorKind::Other, "invalid port"))?;
        let dial_host = text("host")?.to_string();
        let server_name = cfg.tls_server_name().unwrap_or(&dial_host).to_string();
        let database = text("database")?.to_string();
        let user = text("user")?.to_string();
        let password = secrets.0.get("password").cloned().unwrap_or_default();

        let mut config = Config::new();
        config.host(&server_name);
        config.port(port);
        config.database(&database);
        config.authentication(AuthMethod::sql_server(user, password));
        config.application_name(text("application_name").unwrap_or("sqlr"));
        config.encryption(MsConnection::encryption(
            text("encryption").unwrap_or("required"),
        )?);
        config.command_timeout(None);
        if cfg
            .value(m, "trust_server_certificate")
            .is_some_and(|v| v == "true")
        {
            config.trust_cert();
        }
        let mut client = MsConnection::open(config.clone(), &dial_host, port).await?;
        let spid = match client.simple_query("SELECT @@SPID").await {
            Ok(stream) => match stream.into_row().await {
                Ok(Some(row)) => row.get::<i32, _>(0),
                _ => None,
            },
            Err(_) => None,
        };
        Ok(Arc::new(MsConnection {
            client: Arc::new(Mutex::new(client)),
            config,
            dial_host,
            port,
            dialect: MsDialect,
            database,
            spid,
        }))
    }
}

#[async_trait::async_trait]
impl Connection for MsConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let mut client = self.client.lock().await;
        let stream = client
            .simple_query("SELECT @@VERSION")
            .await
            .map_err(map_error)?;
        let row = stream
            .into_row()
            .await
            .map_err(map_error)?
            .ok_or_else(|| DriverError::new(ErrorKind::Protocol, "no version row"))?;
        let version = row
            .get::<&str, _>(0)
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        Ok(ServerInfo {
            product: "SQL Server".into(),
            version,
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        let mut client = self.client.lock().await;
        browse::browse(&mut client, path).await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(MsCancel {
            config: self.config.clone(),
            dial_host: self.dial_host.clone(),
            port: self.port,
            spid: self.spid,
        })
    }

    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }

    fn database_name(&self) -> Option<&str> {
        Some(&self.database)
    }
}

#[async_trait::async_trait]
impl RelationalConn for MsConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        let mut client = self.client.lock().await;
        execute::execute(&mut client, sql, &[]).await
    }

    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        let mut client = self.client.lock().await;
        execute::execute(&mut client, sql, params).await
    }

    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        let mut client = self.client.lock().await;
        describe::describe(&mut client, table).await
    }

    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }

    async fn explain(&self, sql: &str, analyze: bool) -> Result<sqlr_core::PlanTree, DriverError> {
        let statement = sqlr_core::explain_check(&self.dialect.syntax(), sql)?;
        let spec = self.dialect.explain_spec(&statement, analyze)?;
        let mut client = self.client.lock().await;
        let rows = execute::explain_rows(&mut client, &spec).await?;
        spec.parser.parse(&rows)
    }

    /// Paths are `[schema]`, `[schema, folder, name]` and
    /// `[schema, "tables", table, "indexes", index]`.
    fn object_ref(&self, node: &Node) -> Option<ObjectRef> {
        let segs: Vec<&str> = node.path.segments().iter().map(String::as_str).collect();
        let object = |kind, schema: &str, name: &str, parent: Option<&str>| ObjectRef {
            kind,
            database: None,
            schema: Some(schema.to_string()),
            name: name.to_string(),
            parent: parent.map(String::from),
            signature: None,
        };
        match (node.kind, segs.as_slice()) {
            (NodeKind::Schema, [schema]) => {
                Some(object(sqlr_core::ObjectKind::Schema, schema, schema, None))
            }
            (NodeKind::Index, [schema, "tables", table, "indexes", index]) => Some(object(
                sqlr_core::ObjectKind::Index,
                schema,
                index,
                Some(table),
            )),
            (NodeKind::Table, [schema, "tables", name]) => {
                Some(object(sqlr_core::ObjectKind::Table, schema, name, None))
            }
            (NodeKind::View, [schema, "views", name]) => {
                Some(object(sqlr_core::ObjectKind::View, schema, name, None))
            }
            _ => None,
        }
    }

    fn introspect(&self) -> Option<&dyn Introspect> {
        Some(self)
    }

    async fn catalog(&self, schema: &str) -> Result<sqlr_core::SchemaCatalog, DriverError> {
        let mut client = self.client.lock().await;
        catalog::catalog(&mut client, schema).await
    }
}

#[async_trait::async_trait]
impl Introspect for MsConnection {
    async fn load_object(&self, object: &ObjectRef) -> Result<ObjectDef, DriverError> {
        let mut client = self.client.lock().await;
        introspect::load_object(&mut client, &self.database, object).await
    }

    async fn list_roles(&self) -> Result<Vec<String>, DriverError> {
        let mut client = self.client.lock().await;
        introspect::roles(&mut client).await
    }

    /// The connection's own database only: SQL Server has no cross-database connection here.
    async fn load_schema(
        &self,
        database: Option<&str>,
        schema: &str,
    ) -> Result<sqlr_core::SchemaSnapshot, DriverError> {
        if database.is_some_and(|d| d != self.database) {
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                "multiple databases are not supported",
            ));
        }
        let mut client = self.client.lock().await;
        introspect::load_schema(&mut client, schema).await
    }
}

struct MsCancel {
    config: Config,
    dial_host: String,
    port: u16,
    spid: Option<i32>,
}

#[async_trait::async_trait]
impl CancelHandle for MsCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        let spid = self.spid.ok_or_else(|| {
            DriverError::new(ErrorKind::Unsupported, "the server session id is unknown")
        })?;
        let mut client =
            MsConnection::open(self.config.clone(), &self.dial_host, self.port).await?;
        client
            .simple_query(format!("KILL {spid}"))
            .await
            .map_err(map_error)?
            .into_results()
            .await
            .map_err(map_error)?;
        Ok(())
    }
}
