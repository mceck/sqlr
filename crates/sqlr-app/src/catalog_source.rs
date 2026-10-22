//! Feeds a tab's editor completion from the engine's catalog cache.

use std::sync::Arc;

use sqlr_core::{CatalogNeed, Dialect, SchemaCatalog};
use sqlr_engine::{ConnCatalog, Engine, LoadState, SessionStatus};
use sqlr_ui::CompletionSource;

/// One frame's view of a tab connection's catalog, for the database its statements run on.
pub struct TabCatalog {
    catalog: Option<Arc<ConnCatalog>>,
    default_schemas: Vec<String>,
    /// The session is connecting or connected, so missing pieces can be loaded.
    available: bool,
}

impl TabCatalog {
    /// `database` is the database the tab's statements run on, when it is not the
    /// connection's own (`None` for the connection's own database).
    pub fn new(
        engine: &Engine,
        conn: Option<&str>,
        database: Option<&str>,
        dialect: Option<&dyn Dialect>,
    ) -> Self {
        let Some(conn) = conn else {
            return Self {
                catalog: None,
                default_schemas: Vec::new(),
                available: false,
            };
        };
        let available = matches!(
            engine.session_status(conn),
            Some(SessionStatus::Connecting | SessionStatus::Connected(_))
        );
        // A database is also a schema on servers like MySQL, and then it is the schema an
        // unqualified name resolves in.
        let default_schemas = match (database, dialect) {
            (Some(database), Some(dialect)) if dialect.database_is_schema() => {
                vec![database.to_string()]
            }
            (_, Some(dialect)) => dialect.default_schemas(),
            _ => Vec::new(),
        };
        Self {
            catalog: engine.catalog_in(conn, database),
            default_schemas,
            available,
        }
    }

    /// Asks for the pieces completion needs to know the database: its schema list and the
    /// catalogs of the schemas an unqualified name resolves in. Cheap once loaded.
    pub fn prefetch(&self, engine: &Engine, conn: &str, database: Option<&str>) {
        if !self.available {
            return;
        }
        engine.load_catalog_in(conn, database, CatalogNeed::Schemas);
        for schema in &self.default_schemas {
            engine.load_catalog_in(conn, database, CatalogNeed::Schema(schema.clone()));
        }
    }
}

impl CompletionSource for TabCatalog {
    fn available(&self) -> bool {
        self.available
    }

    fn schemas(&self) -> Option<&[String]> {
        match self.catalog.as_ref()?.schemas.as_ref()? {
            LoadState::Ready(schemas) => Some(schemas),
            _ => None,
        }
    }

    fn schema(&self, name: &str) -> Option<&SchemaCatalog> {
        match self.catalog.as_ref()?.schema.get(name)? {
            LoadState::Ready(catalog) => Some(catalog),
            _ => None,
        }
    }

    fn default_schemas(&self) -> &[String] {
        &self.default_schemas
    }
}
