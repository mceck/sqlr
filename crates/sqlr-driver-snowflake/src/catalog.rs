use super::*;
pub(super) fn text(rows: &[Vec<Value>], r: usize, c: usize) -> Result<&str, DriverError> {
    match rows.get(r).and_then(|row| row.get(c)) {
        Some(Value::Text(s)) => Ok(s),
        _ => Err(protocol::invalid()),
    }
}
fn relation_kind(table_type: &str) -> RelationKind {
    match table_type {
        "VIEW" => RelationKind::View,
        "MATERIALIZED VIEW" => RelationKind::MaterializedView,
        _ => RelationKind::Table,
    }
}
fn node_kind(table_type: &str) -> NodeKind {
    match relation_kind(table_type) {
        RelationKind::Table => NodeKind::Table,
        RelationKind::View => NodeKind::View,
        RelationKind::MaterializedView => NodeKind::MaterializedView,
    }
}
impl SnowflakeConnection {
    pub(super) async fn rows(
        &self,
        sql: &str,
        params: &[Value],
    ) -> Result<Vec<Vec<Value>>, DriverError> {
        let Execution::Rows(mut stream) = self.execute_with(sql, params).await? else {
            return Err(protocol::invalid());
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
                    .rows(
                        "SELECT SCHEMA_NAME FROM INFORMATION_SCHEMA.SCHEMATA ORDER BY SCHEMA_NAME",
                        &[],
                    )
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
            [schema] => {
                let rows = self.relations(schema).await?;
                for r in 0..rows.len() {
                    let name = text(&rows, r, 0)?.to_owned();
                    let ty = text(&rows, r, 1)?;
                    nodes.push(Node {
                        path: path.child(&name),
                        name,
                        kind: node_kind(ty),
                        has_children: true,
                        detail: Some(ty.into()),
                    });
                }
            }
            [schema, table] => {
                let info = self
                    .describe_impl(&TableRef {
                        database: None,
                        schema: Some(schema.clone()),
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
    async fn relations(&self, schema: &str) -> Result<Vec<Vec<Value>>, DriverError> {
        self.rows("SELECT TABLE_NAME, TABLE_TYPE FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",&[Value::Text(schema.into())]).await
    }
    pub(super) async fn catalog_impl(&self, schema: &str) -> Result<SchemaCatalog, DriverError> {
        let tables = self.relations(schema).await?;
        let columns = self.rows("SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, ORDINAL_POSITION",&[Value::Text(schema.into())]).await?;
        let mut by_table: std::collections::HashMap<&str, Vec<CatalogColumn>> =
            std::collections::HashMap::new();
        for c in 0..columns.len() {
            by_table
                .entry(text(&columns, c, 0)?)
                .or_default()
                .push(CatalogColumn {
                    name: text(&columns, c, 1)?.into(),
                    type_name: text(&columns, c, 2)?.into(),
                });
        }
        let mut relations = Vec::new();
        for r in 0..tables.len() {
            let name = text(&tables, r, 0)?.to_owned();
            let kind = relation_kind(text(&tables, r, 1)?);
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
        if table
            .database
            .as_deref()
            .is_some_and(|db| self.database_name() != Some(db))
        {
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                "Snowflake browsing is limited to the configured database",
            ));
        }
        let schema = table
            .schema
            .as_deref()
            .or_else(|| self.api.context.get("schema").and_then(Json::as_str))
            .unwrap_or("PUBLIC");
        let rows = self.rows("SELECT COLUMN_NAME, DATA_TYPE, IS_NULLABLE, COLUMN_DEFAULT, IS_IDENTITY FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",&[Value::Text(schema.into()),Value::Text(table.name.clone())]).await?;
        let mut columns = Vec::new();
        for r in 0..rows.len() {
            let default = match rows[r].get(3) {
                Some(Value::Null) => None,
                Some(Value::Text(s)) => Some(s.clone()),
                _ => return Err(protocol::invalid()),
            };
            columns.push(TableColumn {
                name: text(&rows, r, 0)?.into(),
                type_name: text(&rows, r, 1)?.into(),
                nullable: text(&rows, r, 2)? == "YES",
                default,
                generated: false,
                automatic: text(&rows, r, 4)? == "YES",
            });
        }
        Ok(TableInfo {
            table: table.clone(),
            columns,
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialized_views_keep_their_kind() {
        assert_eq!(relation_kind("VIEW"), RelationKind::View);
        assert_eq!(
            relation_kind("MATERIALIZED VIEW"),
            RelationKind::MaterializedView
        );
        assert_eq!(relation_kind("BASE TABLE"), RelationKind::Table);
        assert_eq!(node_kind("MATERIALIZED VIEW"), NodeKind::MaterializedView);
    }
}
