use super::*;
pub(super) fn text(rows: &[Vec<Value>], r: usize, c: usize) -> Result<&str, DriverError> {
    match rows.get(r).and_then(|row| row.get(c)) {
        Some(Value::Text(s)) => Ok(s),
        _ => Err(invalid()),
    }
}
impl ClickHouseConnection {
    pub(super) async fn rows(
        &self,
        sql: &str,
        params: &[Value],
    ) -> Result<Vec<Vec<Value>>, DriverError> {
        let Execution::Rows(mut stream) = self.execute_with(sql, params).await? else {
            return Err(invalid());
        };
        let mut rows = Vec::new();
        while let Some(chunk) = stream.next_chunk(500).await? {
            for r in 0..chunk.len() {
                rows.push(
                    (0..chunk.columns().len())
                        .map(|c| Value::from_cell(chunk.cell(r, c)))
                        .collect(),
                );
            }
        }
        stream.close().await?;
        Ok(rows)
    }
    pub(super) async fn browse_impl(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        let mut nodes = Vec::new();
        match path.segments() {
            [] => {
                let rows = self
                    .rows("SELECT name FROM system.databases ORDER BY name", &[])
                    .await?;
                for r in 0..rows.len() {
                    let name = text(&rows, r, 0)?.to_owned();
                    nodes.push(Node {
                        path: path.child(&name),
                        name,
                        kind: NodeKind::Schema,
                        has_children: true,
                        detail: None,
                    });
                }
            }
            [database] => {
                let rows = self.relations(database).await?;
                for r in 0..rows.len() {
                    let name = text(&rows, r, 0)?.to_owned();
                    let engine = text(&rows, r, 1)?;
                    let kind = match engine {
                        "View" => NodeKind::View,
                        "MaterializedView" => NodeKind::MaterializedView,
                        _ => NodeKind::Table,
                    };
                    nodes.push(Node {
                        path: path.child(&name),
                        name,
                        kind,
                        has_children: true,
                        detail: Some(engine.into()),
                    });
                }
            }
            [database, table] => {
                let info = self
                    .describe_impl(&TableRef {
                        database: None,
                        schema: Some(database.clone()),
                        name: table.clone(),
                    })
                    .await?;
                for column in info.columns {
                    nodes.push(Node {
                        path: path.child(&column.name),
                        name: column.name,
                        kind: NodeKind::Column,
                        has_children: false,
                        detail: Some(column.type_name),
                    });
                }
            }
            _ => {}
        }
        Ok(nodes)
    }
    async fn relations(&self, database: &str) -> Result<Vec<Vec<Value>>, DriverError> {
        self.rows("SELECT name, engine FROM system.tables WHERE database = ? AND NOT startsWith(name, '.inner') ORDER BY name", &[Value::Text(database.into())]).await
    }
    pub(super) async fn catalog_impl(&self, schema: &str) -> Result<SchemaCatalog, DriverError> {
        let tables = self.relations(schema).await?;
        let columns = self.rows("SELECT table, name, type FROM system.columns WHERE database = ? ORDER BY table, position", &[Value::Text(schema.into())]).await?;
        let mut by_table: std::collections::HashMap<&str, Vec<CatalogColumn>> =
            std::collections::HashMap::new();
        for r in 0..columns.len() {
            by_table
                .entry(text(&columns, r, 0)?)
                .or_default()
                .push(CatalogColumn {
                    name: text(&columns, r, 1)?.into(),
                    type_name: text(&columns, r, 2)?.into(),
                });
        }
        let mut relations = Vec::new();
        for r in 0..tables.len() {
            let name = text(&tables, r, 0)?.to_owned();
            let kind = match text(&tables, r, 1)? {
                "View" => RelationKind::View,
                "MaterializedView" => RelationKind::MaterializedView,
                _ => RelationKind::Table,
            };
            relations.push(CatalogRelation {
                columns: by_table.remove(name.as_str()).unwrap_or_default(),
                name,
                kind,
            });
        }
        Ok(SchemaCatalog {
            schema: schema.into(),
            relations,
        })
    }
    pub(super) async fn describe_impl(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        let database = table
            .schema
            .as_deref()
            .or(table.database.as_deref())
            .unwrap_or(&self.dialect.database);
        let rows = self.rows("SELECT name, type, default_kind, default_expression FROM system.columns WHERE database = ? AND table = ? ORDER BY position", &[Value::Text(database.into()), Value::Text(table.name.clone())]).await?;
        let mut columns = Vec::new();
        for r in 0..rows.len() {
            let ty = text(&rows, r, 1)?;
            let default_kind = text(&rows, r, 2)?;
            let expression = text(&rows, r, 3)?;
            columns.push(TableColumn {
                name: text(&rows, r, 0)?.into(),
                type_name: ty.into(),
                nullable: ty.starts_with("Nullable(") || ty.starts_with("LowCardinality(Nullable("),
                default: (!expression.is_empty()).then(|| expression.into()),
                generated: matches!(default_kind, "MATERIALIZED" | "ALIAS"),
                automatic: false,
            });
        }
        // A MergeTree primary key is a sparse sorting index, not a uniqueness constraint.
        Ok(TableInfo {
            table: table.clone(),
            columns,
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        })
    }
}
