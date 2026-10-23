//! DDL generation for MySQL and MariaDB. Identifiers go through [`Dialect::quote_ident`],
//! comments through [`MyDialect::comment_literal`]; expression fields are written as is.
//! Option values (engine, character set, collation) must be plain words.

use sqlr_core::{
    ColumnDef, ConstraintDef, DatabaseDef, DdlGen, Dialect, DriverError, ErrorKind, FkAction,
    IndexDef, ObjectDef, ObjectKind, ObjectRef, RoutineDef, SchemaChange, TableDef, TableRef,
    TriggerDef, ViewDef, diff_table, natural_column_order, script,
};

use crate::dialect::MyDialect;

fn unsupported(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Unsupported, message)
}

fn option<'a>(options: &'a [(String, String)], key: &str) -> Option<&'a str> {
    options
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// `value` when it is a plain word (engine, character set and collation names).
fn word(value: &str) -> Result<&str, DriverError> {
    let ok = !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(value)
    } else {
        Err(unsupported(format!(
            "`{value}` is not a valid option value"
        )))
    }
}

/// `value` when it is one of `allowed` (case-insensitive), upper-cased.
fn keyword(value: &str, allowed: &[&str]) -> Result<String, DriverError> {
    let upper = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase();
    if allowed.contains(&upper.as_str()) {
        Ok(upper)
    } else {
        Err(unsupported(format!("unknown option value `{value}`")))
    }
}

impl MyDialect {
    /// A plain quoted string, for clauses that take no introducer or hex literal (`COMMENT`,
    /// `IDENTIFIED BY`), so [`Dialect::string_literal`]'s form for text with a backslash
    /// does not apply. Quotes are doubled (safe in every SQL mode); backslashes are doubled
    /// unless the session has `NO_BACKSLASH_ESCAPES`, so the text reads back as written.
    pub(crate) fn comment_literal(&self, text: &str) -> String {
        let text = if self.no_backslash_escapes {
            text.to_string()
        } else {
            text.replace('\\', "\\\\")
        };
        format!("'{}'", text.replace('\'', "''"))
    }

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

    fn routine_name(&self, schema: &Option<String>, name: &str) -> String {
        self.qualified(&TableRef {
            database: None,
            schema: schema.clone(),
            name: name.to_string(),
        })
    }

    fn column_definition(&self, c: &ColumnDef) -> String {
        let mut out = format!("{} {}", self.q(&c.name), c.type_name);
        if let Some(collation) = &c.collation {
            out.push_str(&format!(" COLLATE {}", self.q(collation)));
        }
        if let Some(expr) = &c.generated {
            let storage = if c.generated_stored {
                "STORED"
            } else {
                "VIRTUAL"
            };
            out.push_str(&format!(" GENERATED ALWAYS AS ({expr}) {storage}"));
        } else {
            out.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
            if let Some(default) = &c.default {
                out.push_str(&format!(" DEFAULT {default}"));
            }
            if let Some(update) = &c.on_update {
                out.push_str(&format!(" ON UPDATE {update}"));
            }
        }
        if c.identity.is_some() {
            out.push_str(" AUTO_INCREMENT");
        }
        if let Some(comment) = &c.comment {
            out.push_str(&format!(" COMMENT {}", self.comment_literal(comment)));
        }
        out
    }

    fn constraint(&self, c: &ConstraintDef) -> Result<String, DriverError> {
        let named = match c.name() {
            Some(n) if !matches!(c, ConstraintDef::PrimaryKey { .. }) => {
                format!("CONSTRAINT {} ", self.q(n))
            }
            _ => String::new(),
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
                // MySQL's grammar always names the referenced columns: the "target
                // primary key" shorthand the model allows is not expressible here.
                if target_columns.is_empty() {
                    return Err(unsupported(
                        "a MySQL foreign key must name the target columns",
                    ));
                }
                let mut fk = format!(
                    "FOREIGN KEY ({}) REFERENCES {} ({})",
                    self.quoted_list(columns),
                    self.qualified(target),
                    self.quoted_list(target_columns)
                );
                if *on_delete != FkAction::NoAction {
                    fk.push_str(&format!(" ON DELETE {}", on_delete.sql()));
                }
                if *on_update != FkAction::NoAction {
                    fk.push_str(&format!(" ON UPDATE {}", on_update.sql()));
                }
                fk
            }
            ConstraintDef::Raw { definition, .. } => definition.clone(),
        };
        Ok(format!("{named}{body}"))
    }

    /// Key parts of an index: quoted columns with prefix and order, or expressions.
    fn key_parts(&self, i: &IndexDef) -> Result<String, DriverError> {
        if i.predicate.is_some() {
            return Err(unsupported("MySQL has no partial indexes"));
        }
        if !i.include.is_empty() {
            return Err(unsupported("MySQL has no INCLUDE columns"));
        }
        let parts: Vec<String> = i
            .columns
            .iter()
            .map(|c| {
                let mut part = if c.is_expression() {
                    c.expr.clone()
                } else {
                    self.q(&c.expr)
                };
                if let Some(n) = c.prefix {
                    part.push_str(&format!("({n})"));
                }
                if c.desc {
                    part.push_str(" DESC");
                }
                part
            })
            .collect();
        Ok(parts.join(", "))
    }

    /// `(kind keyword, USING clause)` of an index.
    fn index_kind(&self, i: &IndexDef) -> Result<(&'static str, String), DriverError> {
        let method = i.method.as_deref().map(str::to_ascii_uppercase);
        Ok(match method.as_deref() {
            Some("FULLTEXT") => ("FULLTEXT ", String::new()),
            Some("SPATIAL") => ("SPATIAL ", String::new()),
            Some(m @ ("BTREE" | "HASH")) => {
                (if i.unique { "UNIQUE " } else { "" }, format!(" USING {m}"))
            }
            None => (if i.unique { "UNIQUE " } else { "" }, String::new()),
            Some(other) => return Err(unsupported(format!("unknown index type `{other}`"))),
        })
    }

    fn create_index(&self, table: &TableRef, i: &IndexDef) -> Result<String, DriverError> {
        if let Some(definition) = &i.definition {
            return Ok(definition.clone());
        }
        let (kind, using) = self.index_kind(i)?;
        Ok(format!(
            "CREATE {kind}INDEX {} ON {} ({}){using}",
            self.q(&i.name),
            self.qualified(table),
            self.key_parts(i)?
        ))
    }

    fn create_trigger(&self, t: &TriggerDef) -> Result<String, DriverError> {
        let [event] = t.events.as_slice() else {
            return Err(unsupported("a MySQL trigger fires on exactly one event"));
        };
        if t.condition.is_some() {
            return Err(unsupported("MySQL triggers have no WHEN condition"));
        }
        Ok(format!(
            "CREATE TRIGGER {} {} {} ON {} FOR EACH ROW {}",
            self.routine_name(&t.table.schema, &t.name),
            keyword(&t.timing, &["BEFORE", "AFTER"])?,
            keyword(event, &["INSERT", "UPDATE", "DELETE"])?,
            self.qualified(&t.table),
            t.body
        ))
    }

    fn table_options(&self, options: &[(String, String)]) -> Result<String, DriverError> {
        let mut out = String::new();
        if let Some(engine) = option(options, "engine") {
            out.push_str(&format!(" ENGINE={}", word(engine)?));
        }
        if let Some(charset) = option(options, "charset") {
            out.push_str(&format!(" DEFAULT CHARSET={}", word(charset)?));
        }
        if let Some(collation) = option(options, "collation") {
            out.push_str(&format!(" COLLATE={}", word(collation)?));
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
        let (checks, keys): (Vec<&ConstraintDef>, Vec<&ConstraintDef>) = others
            .into_iter()
            .partition(|c| matches!(c, ConstraintDef::Check { .. }));
        for c in keys {
            lines.push(format!("    {}", self.constraint(c)?));
        }
        // An index kept as the server's statement is created after the table.
        for i in t.indexes.iter().filter(|i| i.definition.is_none()) {
            let (kind, using) = self.index_kind(i)?;
            lines.push(format!(
                "    {kind}KEY {} ({}){using}",
                self.q(&i.name),
                self.key_parts(i)?
            ));
        }
        for c in fks.into_iter().chain(checks) {
            lines.push(format!("    {}", self.constraint(c)?));
        }
        let mut create = format!(
            "CREATE TABLE {} (\n{}\n){}",
            self.qualified(&t.table),
            lines.join(",\n"),
            self.table_options(&t.options)?
        );
        if let Some(comment) = &t.comment {
            create.push_str(&format!(" COMMENT={}", self.comment_literal(comment)));
        }
        if !t.partitions.is_empty() {
            return Err(unsupported("MySQL partitions are not supported"));
        }
        let mut out = vec![create];
        for i in t.indexes.iter().filter(|i| i.definition.is_some()) {
            out.push(self.create_index(&t.table, i)?);
        }
        for trigger in &t.triggers {
            out.push(self.create_trigger(trigger)?);
        }
        Ok(out)
    }

    fn create_view(&self, v: &ViewDef, replace: bool) -> Result<String, DriverError> {
        if v.materialized {
            return Err(unsupported("MySQL has no materialized views"));
        }
        let mut out = String::from("CREATE ");
        if replace {
            out.push_str("OR REPLACE ");
        }
        if let Some(security) = option(&v.options, "security") {
            out.push_str(&format!(
                "SQL SECURITY {} ",
                keyword(security, &["DEFINER", "INVOKER"])?
            ));
        }
        out.push_str(&format!("VIEW {}", self.qualified(&v.view)));
        if !v.columns.is_empty() {
            out.push_str(&format!(" ({})", self.quoted_list(&v.columns)));
        }
        out.push_str(&format!(" AS {}", v.query));
        if let Some(check) = option(&v.options, "check_option") {
            out.push_str(&format!(
                " WITH {} CHECK OPTION",
                keyword(check, &["CASCADED", "LOCAL"])?
            ));
        }
        Ok(out)
    }

    fn create_routine(&self, r: &RoutineDef) -> Result<String, DriverError> {
        let name = self.routine_name(&r.schema, &r.name);
        let mut out = match r.kind {
            ObjectKind::Procedure => format!("CREATE PROCEDURE {name}({})", r.arguments),
            _ => {
                let returns = r
                    .returns
                    .as_deref()
                    .ok_or_else(|| unsupported("a function needs a return type"))?;
                format!("CREATE FUNCTION {name}({}) RETURNS {returns}", r.arguments)
            }
        };
        if let Some(comment) = option(&r.options, "comment") {
            out.push_str(&format!(" COMMENT {}", self.comment_literal(comment)));
        }
        if option(&r.options, "deterministic") == Some("true") {
            out.push_str(" DETERMINISTIC");
        }
        if let Some(access) = option(&r.options, "data_access") {
            out.push(' ');
            out.push_str(&keyword(
                access,
                &[
                    "CONTAINS SQL",
                    "NO SQL",
                    "READS SQL DATA",
                    "MODIFIES SQL DATA",
                ],
            )?);
        }
        if let Some(security) = option(&r.options, "security") {
            out.push_str(&format!(
                " SQL SECURITY {}",
                keyword(security, &["DEFINER", "INVOKER"])?
            ));
        }
        out.push_str(&format!("\n{}", r.body));
        Ok(out)
    }

    fn create_database(&self, d: &DatabaseDef) -> Result<String, DriverError> {
        let mut out = format!("CREATE DATABASE {}", self.q(&d.name));
        if let Some(charset) = option(&d.options, "charset") {
            out.push_str(&format!(" CHARACTER SET {}", word(charset)?));
        }
        if let Some(collation) = option(&d.options, "collation") {
            out.push_str(&format!(" COLLATE {}", word(collation)?));
        }
        Ok(out)
    }

    fn create(&self, def: &ObjectDef) -> Result<Vec<String>, DriverError> {
        Ok(match def {
            ObjectDef::Database(d) => vec![self.create_database(d)?],
            ObjectDef::Table(t) => self.create_table(t)?,
            ObjectDef::View(v) => vec![self.create_view(v, false)?],
            ObjectDef::Routine(r) => vec![self.create_routine(r)?],
            ObjectDef::Trigger(t) => vec![self.create_trigger(t)?],
            ObjectDef::Index { table, index } => vec![self.create_index(table, index)?],
            ObjectDef::Schema(_) | ObjectDef::Sequence(_) | ObjectDef::Role(_) => {
                return Err(unsupported(format!(
                    "cannot create a {} here",
                    def.kind().keyword().to_lowercase()
                )));
            }
        })
    }

    /// The table of an index (`parent`).
    fn index_table(&self, o: &ObjectRef) -> Result<TableRef, DriverError> {
        o.parent_table()
            .ok_or_else(|| unsupported("an index is named with its table"))
    }

    fn set_options(
        &self,
        object: &ObjectRef,
        options: &[(String, Option<String>)],
    ) -> Result<Vec<String>, DriverError> {
        let mut out = Vec::new();
        for (key, value) in options {
            let value = value
                .as_deref()
                .ok_or_else(|| unsupported(format!("option `{key}` cannot be reset")))?;
            let clause = match key.as_str() {
                "engine" if object.kind == ObjectKind::Table => {
                    format!("ENGINE = {}", word(value)?)
                }
                "charset" => format!("CHARACTER SET = {}", word(value)?),
                "collation" => format!("COLLATE = {}", word(value)?),
                _ => {
                    return Err(unsupported(format!(
                        "cannot change option `{key}` of a {}",
                        object.kind.keyword().to_lowercase()
                    )));
                }
            };
            out.push(match object.kind {
                ObjectKind::Database | ObjectKind::Schema => {
                    format!("ALTER DATABASE {} {clause}", self.q(&object.name))
                }
                ObjectKind::Table => {
                    let clause = if key == "charset" {
                        format!("DEFAULT {clause}")
                    } else {
                        clause
                    };
                    format!(
                        "ALTER TABLE {} {clause}",
                        self.qualified(&object.table_ref())
                    )
                }
                _ => {
                    return Err(unsupported(format!(
                        "a {} has no options",
                        object.kind.keyword().to_lowercase()
                    )));
                }
            });
        }
        Ok(out)
    }

    fn change(&self, change: &SchemaChange) -> Result<Vec<String>, DriverError> {
        Ok(match change {
            SchemaChange::Create(def) => self.create(def)?,
            SchemaChange::Drop { object, .. } => vec![match object.kind {
                ObjectKind::Database | ObjectKind::Schema => {
                    format!("DROP DATABASE {}", self.q(&object.name))
                }
                ObjectKind::Index => format!(
                    "DROP INDEX {} ON {}",
                    self.q(&object.name),
                    self.qualified(&self.index_table(object)?)
                ),
                ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::Function
                | ObjectKind::Procedure
                | ObjectKind::Trigger => format!(
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
                    "RENAME TABLE {} TO {}",
                    self.qualified(&object.table_ref()),
                    self.qualified(&TableRef {
                        name: to.clone(),
                        ..object.table_ref()
                    })
                ),
                ObjectKind::Index => format!(
                    "ALTER TABLE {} RENAME INDEX {} TO {}",
                    self.qualified(&self.index_table(object)?),
                    self.q(&object.name),
                    self.q(to)
                ),
                other => {
                    return Err(unsupported(format!(
                        "MySQL cannot rename a {}",
                        other.keyword().to_lowercase()
                    )));
                }
            }],
            SchemaChange::AddColumn { table, column } => vec![format!(
                "ALTER TABLE {} ADD COLUMN {}",
                self.qualified(table),
                self.column_definition(column)
            )],
            SchemaChange::DropColumn { table, name } => vec![format!(
                "ALTER TABLE {} DROP COLUMN {}",
                self.qualified(table),
                self.q(name)
            )],
            SchemaChange::AlterColumn {
                table,
                before,
                after,
            } => vec![format!(
                "ALTER TABLE {} CHANGE COLUMN {} {}",
                self.qualified(table),
                self.q(&before.name),
                self.column_definition(after)
            )],
            SchemaChange::AddConstraint { table, constraint } => vec![format!(
                "ALTER TABLE {} ADD {}",
                self.qualified(table),
                self.constraint(constraint)?
            )],
            SchemaChange::DropConstraint { table, name } => {
                let what = if name.is_empty() || name == "PRIMARY" {
                    "PRIMARY KEY".to_string()
                } else {
                    format!("CONSTRAINT {}", self.q(name))
                };
                vec![format!("ALTER TABLE {} DROP {what}", self.qualified(table))]
            }
            SchemaChange::AddIndex { table, index, .. } => vec![self.create_index(table, index)?],
            SchemaChange::DropIndex { table, name } => vec![format!(
                "DROP INDEX {} ON {}",
                self.q(name),
                self.qualified(table)
            )],
            SchemaChange::SetComment {
                object,
                column: None,
                comment,
            } if object.kind == ObjectKind::Table => vec![format!(
                "ALTER TABLE {} COMMENT = {}",
                self.qualified(&object.table_ref()),
                self.comment_literal(comment.as_deref().unwrap_or(""))
            )],
            SchemaChange::SetComment { .. } => {
                return Err(unsupported(
                    "MySQL comments are set on tables, or with the column definition",
                ));
            }
            SchemaChange::ReplaceView(v) => vec![self.create_view(v, true)?],
            // MariaDB replaces a routine in one statement. MySQL cannot: it drops it first,
            // a destructive statement that applying it confirms, and the two do not run as
            // one (MySQL DDL is not transactional).
            SchemaChange::ReplaceRoutine(r) if self.mariadb => {
                vec![
                    self.create_routine(r)?
                        .replacen("CREATE ", "CREATE OR REPLACE ", 1),
                ]
            }
            SchemaChange::ReplaceRoutine(r) => vec![
                format!(
                    "DROP {} IF EXISTS {}",
                    r.kind.keyword(),
                    self.routine_name(&r.schema, &r.name)
                ),
                self.create_routine(r)?,
            ],
            SchemaChange::SetOptions { object, options } => self.set_options(object, options)?,
            SchemaChange::AttachPartition { .. } | SchemaChange::DetachPartition { .. } => {
                return Err(unsupported("MySQL partitions are not supported"));
            }
            // MySQL has no TRUNCATE … CASCADE.
            SchemaChange::Truncate { table, .. } => {
                vec![format!("TRUNCATE TABLE {}", self.qualified(table))]
            }
        })
    }
}

impl DdlGen for MyDialect {
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

    /// The changes, then `MODIFY COLUMN … FIRST | AFTER …` for the columns from the first
    /// one out of place onward, when `after` reorders them.
    fn table_ddl(&self, before: &TableDef, after: &TableDef) -> Result<Vec<String>, DriverError> {
        let mut out = self.ddl(&diff_table(before, after))?;
        let natural = natural_column_order(before, after);
        let first_moved = after
            .columns
            .iter()
            .zip(&natural)
            .position(|(c, n)| c.name != *n);
        if let Some(from) = first_moved {
            for i in from..after.columns.len() {
                let place = match i {
                    0 => "FIRST".to_string(),
                    _ => format!("AFTER {}", self.q(&after.columns[i - 1].name)),
                };
                out.push(format!(
                    "ALTER TABLE {} MODIFY COLUMN {} {place}",
                    self.qualified(&after.table),
                    self.column_definition(&after.columns[i])
                ));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::{Identity, IndexColumn};

    fn t(name: &str) -> TableRef {
        TableRef {
            database: None,
            schema: Some("d`b".into()),
            name: name.into(),
        }
    }

    #[test]
    fn create_table_quotes_and_escapes() {
        let mut def = TableDef::new(t("a`b"));
        def.columns = vec![ColumnDef {
            name: "x y".into(),
            type_name: "int".into(),
            nullable: false,
            identity: Some(Identity::AutoIncrement),
            comment: Some("it's \\ here".into()),
            ..Default::default()
        }];
        def.constraints.push(ConstraintDef::PrimaryKey {
            name: None,
            columns: vec!["x y".into()],
        });
        def.indexes.push(IndexDef {
            name: "i".into(),
            columns: vec![IndexColumn {
                expr: "x y".into(),
                desc: true,
                prefix: Some(4),
            }],
            method: Some("FULLTEXT".into()),
            ..Default::default()
        });
        def.options = vec![("engine".into(), "InnoDB".into())];
        def.comment = Some("c'".into());
        assert_eq!(
            MyDialect::default()
                .create_ddl(&ObjectDef::Table(def.clone()))
                .unwrap(),
            "CREATE TABLE `d``b`.`a``b` (\n    `x y` int NOT NULL AUTO_INCREMENT COMMENT 'it''s \\\\ here',\n    \
             PRIMARY KEY (`x y`),\n    FULLTEXT KEY `i` (`x y`(4) DESC)\n) ENGINE=InnoDB COMMENT='c''';"
        );
        def.options = vec![("engine".into(), "InnoDB; DROP".into())];
        assert!(
            MyDialect::default()
                .create_ddl(&ObjectDef::Table(def))
                .is_err()
        );
    }

    #[test]
    fn replace_routine_per_server() {
        let routine = RoutineDef {
            schema: Some("d".into()),
            name: "f".into(),
            kind: ObjectKind::Function,
            arguments: String::new(),
            identity_arguments: None,
            returns: Some("int".into()),
            language: Some("SQL".into()),
            body: "RETURN 1".into(),
            options: vec![("deterministic".into(), "true".into())],
        };
        let change = [SchemaChange::ReplaceRoutine(routine)];
        // MariaDB replaces in one statement.
        let mariadb = MyDialect {
            mariadb: true,
            ..MyDialect::default()
        };
        let ddl = mariadb.ddl(&change).unwrap();
        assert_eq!(ddl.len(), 1, "{ddl:#?}");
        assert!(
            ddl[0].starts_with("CREATE OR REPLACE FUNCTION `d`.`f`()"),
            "{ddl:#?}"
        );
        // MySQL drops first: the drop is destructive, so applying it asks to confirm.
        let mysql = MyDialect::default();
        let ddl = mysql.ddl(&change).unwrap();
        assert_eq!(ddl.len(), 2, "{ddl:#?}");
        assert!(matches!(
            mysql.syntax().classify(&ddl[0]),
            sqlr_core::StatementClass::Destructive(_)
        ));
        assert!(ddl[1].starts_with("CREATE FUNCTION `d`.`f`()"), "{ddl:#?}");
    }

    #[test]
    fn every_statement_is_in_transaction_placement() {
        // MySQL applies statement by statement anyway; nothing needs to run alone.
        let d = MyDialect::default();
        for sql in [
            "CREATE DATABASE `d`",
            "DROP DATABASE `d`",
            "CREATE INDEX `i` ON `t` (`a`)",
        ] {
            assert_eq!(d.placement(sql), sqlr_core::Placement::Transaction);
        }
    }

    #[test]
    fn reordered_columns_move_after_their_predecessor() {
        let col = |name: &str| ColumnDef {
            name: name.into(),
            type_name: "int".into(),
            nullable: true,
            ..Default::default()
        };
        let mut before = TableDef::new(t("x"));
        before.columns = vec![col("a"), col("b"), col("c")];
        let mut after = before.clone();
        after.columns.swap(0, 2);
        let ddl = MyDialect::default().table_ddl(&before, &after).unwrap();
        assert_eq!(
            ddl,
            [
                "ALTER TABLE `d``b`.`x` MODIFY COLUMN `c` int NULL FIRST",
                "ALTER TABLE `d``b`.`x` MODIFY COLUMN `b` int NULL AFTER `c`",
                "ALTER TABLE `d``b`.`x` MODIFY COLUMN `a` int NULL AFTER `b`",
            ]
        );
        // Without a reorder nothing moves.
        let mut renamed = before.clone();
        renamed.columns[1].name = "bb".into();
        let ddl = MyDialect::default().table_ddl(&before, &renamed).unwrap();
        assert_eq!(ddl.len(), 1, "{ddl:#?}");
    }

    #[test]
    fn a_foreign_key_needs_the_target_columns() {
        // MySQL's REFERENCES always names the target columns (`REFERENCES t (c)`):
        // the target primary key shorthand is not part of its grammar.
        let mut def = TableDef::new(t("child"));
        def.columns = vec![ColumnDef {
            name: "parent_id".into(),
            type_name: "int".into(),
            nullable: true,
            ..Default::default()
        }];
        def.constraints = vec![ConstraintDef::ForeignKey {
            name: Some("fk".into()),
            columns: vec!["parent_id".into()],
            target: t("parent"),
            target_columns: Vec::new(),
            on_update: FkAction::NoAction,
            on_delete: FkAction::NoAction,
            deferrable: false,
            initially_deferred: false,
        }];
        let err = MyDialect::default()
            .create_ddl(&ObjectDef::Table(def.clone()))
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported);
        // The same on the ALTER TABLE path.
        let err = MyDialect::default()
            .ddl(&[SchemaChange::AddConstraint {
                table: t("child"),
                constraint: def.constraints[0].clone(),
            }])
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported);
    }

    #[test]
    fn truncate_has_no_cascade() {
        let truncate = |cascade| SchemaChange::Truncate {
            table: t("x"),
            cascade,
        };
        let plain = MyDialect::default().ddl(&[truncate(false)]).unwrap();
        assert_eq!(plain, ["TRUNCATE TABLE `d``b`.`x`"]);
        // The same statement either way: the UI offers no CASCADE.
        assert_eq!(MyDialect::default().ddl(&[truncate(true)]).unwrap(), plain);
    }
}
