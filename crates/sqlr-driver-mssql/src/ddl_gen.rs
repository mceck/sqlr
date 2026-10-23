//! DDL generation for SQL Server. Identifiers go through [`Dialect::quote_ident`];
//! expression fields (defaults, checks, view queries) are written as is. Tables, views,
//! indexes and schemas are covered; comments (extended properties), sequences, routines
//! and triggers are not.

use sqlr_core::{
    ColumnDef, ConstraintDef, DdlGen, Dialect, DriverError, ErrorKind, FkAction, IndexDef,
    ObjectDef, ObjectKind, ObjectRef, SchemaChange, TableDef, TableRef, ViewDef, script,
};

use crate::dialect::MsDialect;

fn unsupported(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Unsupported, message)
}

/// `text` as an `N'…'` literal.
fn nstr(text: &str) -> String {
    format!("N'{}'", text.replace('\'', "''"))
}

impl MsDialect {
    fn q(&self, ident: &str) -> String {
        self.quote_ident(ident)
    }

    fn quoted_list(&self, names: &[String]) -> String {
        names
            .iter()
            .map(|n| self.q(n))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The bracketed, dot-separated name of an object, for `sp_rename` and `OBJECT_ID`.
    fn dotted(&self, table: &TableRef, rest: &[&str]) -> String {
        let mut parts = vec![self.qualified(table)];
        parts.extend(rest.iter().map(|r| self.q(r)));
        parts.join(".")
    }

    fn column_definition(&self, c: &ColumnDef) -> String {
        if let Some(expr) = &c.generated {
            let persisted = if c.generated_stored { " PERSISTED" } else { "" };
            return format!("{} AS ({expr}){persisted}", self.q(&c.name));
        }
        let mut out = format!("{} {}", self.q(&c.name), c.type_name);
        if let Some(collation) = &c.collation {
            out.push_str(&format!(" COLLATE {collation}"));
        }
        if c.identity.is_some() {
            out.push_str(" IDENTITY(1,1)");
        }
        out.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
        if let Some(default) = &c.default {
            out.push_str(&format!(" DEFAULT {default}"));
        }
        out
    }

    fn constraint(&self, c: &ConstraintDef) -> String {
        let named = match c.name() {
            Some(n) => format!("CONSTRAINT {} ", self.q(n)),
            None => String::new(),
        };
        let body = match c {
            ConstraintDef::PrimaryKey { columns, .. } => {
                format!("PRIMARY KEY ({})", self.quoted_list(columns))
            }
            ConstraintDef::Unique { columns, .. } => {
                format!("UNIQUE ({})", self.quoted_list(columns))
            }
            ConstraintDef::Check { expression, .. } => format!("CHECK ({expression})"),
            ConstraintDef::ForeignKey {
                columns,
                target,
                target_columns,
                on_update,
                on_delete,
                ..
            } => {
                let mut fk = format!(
                    "FOREIGN KEY ({}) REFERENCES {}",
                    self.quoted_list(columns),
                    self.qualified(target)
                );
                // An empty target column list means the target's primary key.
                if !target_columns.is_empty() {
                    fk.push_str(&format!(" ({})", self.quoted_list(target_columns)));
                }
                // RESTRICT is NO ACTION here.
                for (clause, action) in [("DELETE", on_delete), ("UPDATE", on_update)] {
                    if !matches!(action, FkAction::NoAction | FkAction::Restrict) {
                        fk.push_str(&format!(" ON {clause} {}", action.sql()));
                    }
                }
                fk
            }
            ConstraintDef::Raw { definition, .. } => definition.clone(),
        };
        format!("{named}{body}")
    }

    fn create_index(&self, table: &TableRef, i: &IndexDef) -> Result<String, DriverError> {
        if let Some(definition) = &i.definition {
            return Ok(definition.clone());
        }
        let kind = match i.method.as_deref().map(str::to_ascii_uppercase).as_deref() {
            None | Some("") => "",
            Some("CLUSTERED") => "CLUSTERED ",
            Some("NONCLUSTERED") => "NONCLUSTERED ",
            Some(other) => return Err(unsupported(format!("unknown index type `{other}`"))),
        };
        let parts: Vec<String> = i
            .columns
            .iter()
            .map(|c| {
                let mut part = self.q(&c.expr);
                if c.desc {
                    part.push_str(" DESC");
                }
                part
            })
            .collect();
        let mut out = format!(
            "CREATE {}{kind}INDEX {} ON {} ({})",
            if i.unique { "UNIQUE " } else { "" },
            self.q(&i.name),
            self.qualified(table),
            parts.join(", ")
        );
        if !i.include.is_empty() {
            out.push_str(&format!(" INCLUDE ({})", self.quoted_list(&i.include)));
        }
        if let Some(predicate) = &i.predicate {
            out.push_str(&format!(" WHERE {predicate}"));
        }
        Ok(out)
    }

    fn create_table(&self, t: &TableDef) -> Result<Vec<String>, DriverError> {
        let mut lines: Vec<String> = t
            .columns
            .iter()
            .map(|c| format!("    {}", self.column_definition(c)))
            .collect();
        let (fks, others): (Vec<&ConstraintDef>, Vec<&ConstraintDef>) =
            t.constraints.iter().partition(|c| c.is_foreign_key());
        for c in others.into_iter().chain(fks) {
            lines.push(format!("    {}", self.constraint(c)));
        }
        if !t.partitions.is_empty() {
            return Err(unsupported("SQL Server partitions are not supported"));
        }
        let mut out = vec![format!(
            "CREATE TABLE {} (\n{}\n)",
            self.qualified(&t.table),
            lines.join(",\n")
        )];
        for i in &t.indexes {
            out.push(self.create_index(&t.table, i)?);
        }
        Ok(out)
    }

    fn create_view(&self, v: &ViewDef, replace: bool) -> Result<String, DriverError> {
        if v.materialized {
            return Err(unsupported("SQL Server has no materialized views"));
        }
        let mut out = format!(
            "CREATE {}VIEW {}",
            if replace { "OR ALTER " } else { "" },
            self.qualified(&v.view)
        );
        if v.query.trim().is_empty() {
            return Err(unsupported("a view needs a query"));
        }
        if !v.columns.is_empty() {
            out.push_str(&format!(" ({})", self.quoted_list(&v.columns)));
        }
        out.push_str(&format!(" AS {}", v.query));
        Ok(out)
    }

    fn create(&self, def: &ObjectDef) -> Result<Vec<String>, DriverError> {
        Ok(match def {
            ObjectDef::Table(t) => self.create_table(t)?,
            ObjectDef::View(v) => vec![self.create_view(v, false)?],
            ObjectDef::Index { table, index } => vec![self.create_index(table, index)?],
            ObjectDef::Schema(s) => vec![format!("CREATE SCHEMA {}", self.q(&s.name))],
            other => {
                return Err(unsupported(format!(
                    "cannot create a {} here",
                    other.kind().keyword().to_lowercase()
                )));
            }
        })
    }

    fn index_table(&self, o: &ObjectRef) -> Result<TableRef, DriverError> {
        o.parent_table()
            .ok_or_else(|| unsupported("an index is named with its table"))
    }

    /// Drops the default constraint of `column`, whatever name the server gave it.
    fn drop_default(&self, table: &TableRef, column: &str) -> String {
        let target = self.qualified(table);
        format!(
            "DECLARE @sqlr_default sysname = (SELECT dc.name FROM sys.default_constraints dc \
             JOIN sys.columns c ON c.default_object_id = dc.object_id \
             WHERE dc.parent_object_id = OBJECT_ID({}) AND c.name = {}); \
             IF @sqlr_default IS NOT NULL BEGIN \
             DECLARE @sqlr_drop nvarchar(max) = N'ALTER TABLE {} DROP CONSTRAINT ' + \
             QUOTENAME(@sqlr_default); EXEC(@sqlr_drop) END",
            nstr(&target),
            nstr(column),
            target.replace('\'', "''"),
        )
    }

    fn alter_column(
        &self,
        table: &TableRef,
        before: &ColumnDef,
        after: &ColumnDef,
    ) -> Result<Vec<String>, DriverError> {
        let t = self.qualified(table);
        let mut out = Vec::new();
        if before.name != after.name {
            out.push(format!(
                "EXEC sp_rename {}, {}, N'COLUMN'",
                nstr(&self.dotted(table, &[&before.name])),
                nstr(&after.name)
            ));
        }
        if before.identity != after.identity {
            return Err(unsupported(
                "SQL Server cannot add or remove IDENTITY on an existing column",
            ));
        }
        if before.generated != after.generated || before.generated_stored != after.generated_stored
        {
            return Err(unsupported(
                "SQL Server cannot change a computed column: drop and add it",
            ));
        }
        let shape = |c: &ColumnDef| (c.type_name.clone(), c.nullable, c.collation.clone());
        let reshaped = shape(before) != shape(after);
        // A default constraint depends on its column: it goes before a type change and comes
        // back after it.
        let default_changed = before.default != after.default;
        if before.default.is_some() && (reshaped || default_changed) {
            out.push(self.drop_default(table, &after.name));
        }
        if reshaped {
            let mut alter = format!(
                "ALTER TABLE {t} ALTER COLUMN {} {}",
                self.q(&after.name),
                after.type_name
            );
            if let Some(collation) = &after.collation {
                alter.push_str(&format!(" COLLATE {collation}"));
            }
            alter.push_str(if after.nullable { " NULL" } else { " NOT NULL" });
            out.push(alter);
        }
        let default_back = default_changed || (reshaped && before.default.is_some());
        if default_back && let Some(default) = &after.default {
            out.push(format!(
                "ALTER TABLE {t} ADD DEFAULT {default} FOR {}",
                self.q(&after.name)
            ));
        }
        Ok(out)
    }

    fn change(&self, change: &SchemaChange) -> Result<Vec<String>, DriverError> {
        Ok(match change {
            SchemaChange::Create(def) => self.create(def)?,
            SchemaChange::Drop { object, .. } => vec![match object.kind {
                ObjectKind::Schema => format!("DROP SCHEMA {}", self.q(&object.name)),
                ObjectKind::Index => format!(
                    "DROP INDEX {} ON {}",
                    self.q(&object.name),
                    self.qualified(&self.index_table(object)?)
                ),
                ObjectKind::Table | ObjectKind::View => format!(
                    "DROP {} {}",
                    object.kind.keyword(),
                    self.qualified(&object.table_ref())
                ),
                other => {
                    return Err(unsupported(format!(
                        "cannot drop a {}",
                        other.keyword().to_lowercase()
                    )));
                }
            }],
            SchemaChange::Rename { object, to } => vec![match object.kind {
                ObjectKind::Table | ObjectKind::View => format!(
                    "EXEC sp_rename {}, {}",
                    nstr(&self.qualified(&object.table_ref())),
                    nstr(to)
                ),
                ObjectKind::Index => format!(
                    "EXEC sp_rename {}, {}, N'INDEX'",
                    nstr(&self.dotted(&self.index_table(object)?, &[&object.name])),
                    nstr(to)
                ),
                other => {
                    return Err(unsupported(format!(
                        "SQL Server cannot rename a {}",
                        other.keyword().to_lowercase()
                    )));
                }
            }],
            SchemaChange::AddColumn { table, column } => vec![format!(
                "ALTER TABLE {} ADD {}",
                self.qualified(table),
                self.column_definition(column)
            )],
            SchemaChange::DropColumn { table, name } => vec![
                self.drop_default(table, name),
                format!(
                    "ALTER TABLE {} DROP COLUMN {}",
                    self.qualified(table),
                    self.q(name)
                ),
            ],
            SchemaChange::AlterColumn {
                table,
                before,
                after,
            } => self.alter_column(table, before, after)?,
            SchemaChange::AddConstraint { table, constraint } => vec![format!(
                "ALTER TABLE {} ADD {}",
                self.qualified(table),
                self.constraint(constraint)
            )],
            SchemaChange::DropConstraint { table, name } => vec![format!(
                "ALTER TABLE {} DROP CONSTRAINT {}",
                self.qualified(table),
                self.q(name)
            )],
            SchemaChange::AddIndex { table, index, .. } => vec![self.create_index(table, index)?],
            SchemaChange::DropIndex { table, name } => vec![format!(
                "DROP INDEX {} ON {}",
                self.q(name),
                self.qualified(table)
            )],
            SchemaChange::SetComment { .. } => {
                return Err(unsupported("SQL Server comments are not supported here"));
            }
            SchemaChange::ReplaceView(v) => vec![self.create_view(v, true)?],
            SchemaChange::ReplaceRoutine(_) => {
                return Err(unsupported("SQL Server routines are not supported here"));
            }
            SchemaChange::SetOptions { object, .. } => {
                return Err(unsupported(format!(
                    "a {} has no options here",
                    object.kind.keyword().to_lowercase()
                )));
            }
            SchemaChange::AttachPartition { .. } | SchemaChange::DetachPartition { .. } => {
                return Err(unsupported("SQL Server partitions are not supported"));
            }
            SchemaChange::Truncate { table, .. } => {
                vec![format!("TRUNCATE TABLE {}", self.qualified(table))]
            }
        })
    }
}

impl DdlGen for MsDialect {
    fn ddl(&self, changes: &[SchemaChange]) -> Result<Vec<String>, DriverError> {
        let mut out = Vec::new();
        for change in changes {
            out.extend(self.change(change)?);
        }
        Ok(out)
    }

    fn create_ddl(&self, def: &ObjectDef) -> Result<String, DriverError> {
        Ok(script(&self.create(def)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::Identity;

    fn table(name: &str) -> TableRef {
        TableRef {
            database: None,
            schema: Some("s]x".into()),
            name: name.into(),
        }
    }

    #[test]
    fn create_table_brackets_identifiers_and_puts_foreign_keys_last() {
        let mut def = TableDef::new(table("a]b"));
        def.columns = vec![
            ColumnDef {
                name: "id".into(),
                type_name: "int".into(),
                identity: Some(Identity::AutoIncrement),
                ..Default::default()
            },
            ColumnDef {
                name: "n".into(),
                type_name: "int".into(),
                nullable: true,
                default: Some("((1))".into()),
                ..Default::default()
            },
        ];
        def.constraints = vec![
            ConstraintDef::ForeignKey {
                name: Some("fk".into()),
                columns: vec!["n".into()],
                target: table("t"),
                target_columns: vec!["id".into()],
                on_update: FkAction::NoAction,
                on_delete: FkAction::Cascade,
                deferrable: false,
                initially_deferred: false,
            },
            ConstraintDef::PrimaryKey {
                name: Some("pk".into()),
                columns: vec!["id".into()],
            },
        ];
        let sql = MsDialect.create_ddl(&ObjectDef::Table(def)).unwrap();
        assert_eq!(
            sql,
            "CREATE TABLE [s]]x].[a]]b] (\n    [id] int IDENTITY(1,1) NOT NULL,\n    \
             [n] int NULL DEFAULT ((1)),\n    CONSTRAINT [pk] PRIMARY KEY ([id]),\n    \
             CONSTRAINT [fk] FOREIGN KEY ([n]) REFERENCES [s]]x].[t] ([id]) ON DELETE CASCADE\n);"
        );
    }

    #[test]
    fn a_foreign_key_without_target_columns_references_the_primary_key() {
        let mut def = TableDef::new(table("child"));
        def.columns = vec![ColumnDef {
            name: "parent_id".into(),
            type_name: "int".into(),
            nullable: true,
            ..Default::default()
        }];
        def.constraints = vec![ConstraintDef::ForeignKey {
            name: Some("fk".into()),
            columns: vec!["parent_id".into()],
            target: table("parent"),
            target_columns: Vec::new(),
            on_update: FkAction::NoAction,
            on_delete: FkAction::NoAction,
            deferrable: false,
            initially_deferred: false,
        }];
        let sql = MsDialect.create_ddl(&ObjectDef::Table(def)).unwrap();
        assert!(
            sql.contains("CONSTRAINT [fk] FOREIGN KEY ([parent_id]) REFERENCES [s]]x].[parent]\n"),
            "{sql}"
        );
        assert!(!sql.contains("()"), "{sql}");
    }

    #[test]
    fn a_view_keeps_its_explicit_column_names() {
        let view = ViewDef {
            view: table("v"),
            query: "SELECT 1 AS a, 2 AS b".into(),
            materialized: false,
            columns: vec!["a".into(), "b".into()],
            comment: None,
            options: Vec::new(),
            indexes: Vec::new(),
        };
        assert_eq!(
            MsDialect.create_ddl(&ObjectDef::View(view)).unwrap(),
            "CREATE VIEW [s]]x].[v] ([a], [b]) AS SELECT 1 AS a, 2 AS b;"
        );
    }

    #[test]
    fn renames_use_sp_rename_with_escaped_names() {
        let ddl = MsDialect
            .ddl(&[SchemaChange::Rename {
                object: ObjectRef::relation(ObjectKind::Table, &table("it's")),
                to: "x".into(),
            }])
            .unwrap();
        assert_eq!(ddl, ["EXEC sp_rename N'[s]]x].[it''s]', N'x'"]);
    }

    #[test]
    fn what_is_not_covered_is_unsupported() {
        let err = MsDialect
            .ddl(&[SchemaChange::SetComment {
                object: ObjectRef::relation(ObjectKind::Table, &table("t")),
                column: None,
                comment: Some("c".into()),
            }])
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported);
    }
}
