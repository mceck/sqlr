//! Schema changes: what an object editor applies. The pending list of a table editor is
//! derived with [`diff_table`] from the loaded definition and the edited copy, never
//! recorded by hand, so undoing an edit removes its change.

use std::collections::HashMap;

use crate::schema::{
    ColumnDef, ConstraintDef, IndexDef, ObjectDef, ObjectKind, ObjectRef, PartitionDef, RoutineDef,
    TableDef, ViewDef,
};
use crate::table::TableRef;

/// One change an object editor applies to the database. A driver's schema DDL turns it into statements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaChange {
    /// Creates an object.
    Create(ObjectDef),
    /// Drops an object.
    Drop {
        /// The object to drop.
        object: ObjectRef,
        /// Also drop what depends on it, where the dialect has it.
        cascade: bool,
    },
    /// Renames an object.
    Rename {
        /// The object to rename.
        object: ObjectRef,
        /// The new name.
        to: String,
    },
    /// Adds a column to a table.
    AddColumn {
        /// The table.
        table: TableRef,
        /// The new column.
        column: ColumnDef,
    },
    /// Drops a column.
    DropColumn {
        /// The table.
        table: TableRef,
        /// The column's name.
        name: String,
    },
    /// Any change of a column, its name included (`before.name` is the current name).
    AlterColumn {
        /// The table.
        table: TableRef,
        /// The column as it is.
        before: ColumnDef,
        /// The column as it should be.
        after: ColumnDef,
    },
    /// Adds a constraint to a table.
    AddConstraint {
        /// The table.
        table: TableRef,
        /// The new constraint.
        constraint: ConstraintDef,
    },
    /// An empty `name` means the table's unnamed primary key.
    DropConstraint {
        /// The table.
        table: TableRef,
        /// The constraint's name.
        name: String,
    },
    /// Adds an index to a table.
    AddIndex {
        /// The table.
        table: TableRef,
        /// The new index.
        index: IndexDef,
        /// PostgreSQL `CONCURRENTLY`: applied outside a transaction.
        concurrently: bool,
    },
    /// Drops an index.
    DropIndex {
        /// The table the index belongs to.
        table: TableRef,
        /// The index's name.
        name: String,
    },
    /// `comment: None` removes the comment.
    SetComment {
        /// The object that carries the comment.
        object: ObjectRef,
        /// The column, to comment a column of the object rather than the object.
        column: Option<String>,
        /// The new comment.
        comment: Option<String>,
    },
    /// Replaces the query of a view, keeping its name.
    ReplaceView(ViewDef),
    /// Replaces a function or procedure with a new definition.
    ReplaceRoutine(RoutineDef),
    /// `None` resets an option to its default.
    SetOptions {
        /// The object to alter.
        object: ObjectRef,
        /// Option names with their new value.
        options: Vec<(String, Option<String>)>,
    },
    /// PostgreSQL: attaches an existing table as a partition of `table`.
    AttachPartition {
        /// The partitioned table.
        table: TableRef,
        /// The partition to attach.
        partition: PartitionDef,
    },
    /// PostgreSQL: detaches a partition; it stays as a standalone table.
    DetachPartition {
        /// The partitioned table.
        table: TableRef,
        /// The partition to detach.
        partition: TableRef,
    },
    /// Deletes every row of `table` (`TRUNCATE`, SQLite `DELETE FROM`); `cascade` also
    /// empties the tables that reference it, where the dialect has it.
    Truncate {
        /// The table to empty.
        table: TableRef,
        /// Also empty the tables that reference it.
        cascade: bool,
    },
}

/// Columns of `before` and `after` paired up: by name, then an unmatched column of `after`
/// with the unmatched column of `before` at the same position (a rename in place).
pub fn pair_columns<'a>(
    before: &'a TableDef,
    after: &'a TableDef,
) -> Vec<(Option<&'a ColumnDef>, Option<&'a ColumnDef>)> {
    let in_after = |name: &str| after.columns.iter().any(|c| c.name == name);
    let in_before = |name: &str| before.columns.iter().any(|c| c.name == name);
    let mut used = vec![false; before.columns.len()];
    let mut pairs = Vec::new();
    for (i, a) in after.columns.iter().enumerate() {
        let source = before
            .columns
            .iter()
            .position(|b| b.name == a.name)
            .or_else(|| {
                (!in_before(&a.name))
                    .then_some(i)
                    .filter(|&i| i < before.columns.len())
                    .filter(|&i| !in_after(&before.columns[i].name))
            });
        match source {
            Some(j) if !used[j] => {
                used[j] = true;
                pairs.push((Some(&before.columns[j]), Some(a)));
            }
            _ => pairs.push((None, Some(a))),
        }
    }
    for (j, b) in before.columns.iter().enumerate() {
        if !used[j] {
            pairs.push((Some(b), None));
        }
    }
    pairs
}

fn rename_all(names: &[String], renames: &HashMap<&str, &str>) -> Vec<String> {
    names
        .iter()
        .map(|n| renames.get(n.as_str()).map_or(n.clone(), |r| r.to_string()))
        .collect()
}

/// `c` with its own-table column names renamed.
fn renamed_constraint(c: &ConstraintDef, renames: &HashMap<&str, &str>) -> ConstraintDef {
    let mut c = c.clone();
    match &mut c {
        ConstraintDef::PrimaryKey { columns, .. }
        | ConstraintDef::Unique { columns, .. }
        | ConstraintDef::ForeignKey { columns, .. } => *columns = rename_all(columns, renames),
        ConstraintDef::Check { .. } | ConstraintDef::Raw { .. } => {}
    }
    c
}

fn renamed_index(i: &IndexDef, renames: &HashMap<&str, &str>) -> IndexDef {
    let mut i = i.clone();
    for c in &mut i.columns {
        // A key part that is SQL is not a column name, even when both read the same.
        if !c.is_expression()
            && let Some(r) = renames.get(c.expr.as_str())
        {
            c.expr = r.to_string();
        }
    }
    i.include = rename_all(&i.include, renames);
    i
}

/// The changes that turn `before` into `after`, in this order: rename the table, drop
/// foreign keys, drop other constraints, indexes and triggers, drop columns, add and alter
/// columns, add constraints, add indexes, add foreign keys, create triggers, partitions,
/// options, the table comment. Column comments travel with their column.
pub fn diff_table(before: &TableDef, after: &TableDef) -> Vec<SchemaChange> {
    let table = &after.table;
    let mut out = Vec::new();
    if before.table != after.table {
        out.push(SchemaChange::Rename {
            object: ObjectRef::relation(ObjectKind::Table, &before.table),
            to: after.table.name.clone(),
        });
    }

    let pairs = pair_columns(before, after);
    let renames: HashMap<&str, &str> = pairs
        .iter()
        .filter_map(|p| match p {
            (Some(b), Some(a)) if b.name != a.name => Some((b.name.as_str(), a.name.as_str())),
            _ => None,
        })
        .collect();

    // Constraints and indexes of `before`, as they read once the columns are renamed.
    let old_constraints: Vec<ConstraintDef> = before
        .constraints
        .iter()
        .map(|c| renamed_constraint(c, &renames))
        .collect();
    let old_indexes: Vec<IndexDef> = before
        .indexes
        .iter()
        .map(|i| renamed_index(i, &renames))
        .collect();
    let dropped_constraints: Vec<&ConstraintDef> = old_constraints
        .iter()
        .filter(|c| !after.constraints.contains(c))
        .collect();
    let added_constraints: Vec<&ConstraintDef> = after
        .constraints
        .iter()
        .filter(|c| !old_constraints.contains(c))
        .collect();

    // Indexes: unchanged, renamed (same definition, new name), dropped or added.
    let same_but_name = |a: &IndexDef, b: &IndexDef| {
        IndexDef {
            name: b.name.clone(),
            ..a.clone()
        } == *b
    };
    let mut index_renames: Vec<(&IndexDef, &IndexDef)> = Vec::new();
    let mut dropped_indexes: Vec<&IndexDef> = Vec::new();
    for old in old_indexes.iter().filter(|i| !after.indexes.contains(i)) {
        let target = after.indexes.iter().find(|new| {
            !old_indexes.iter().any(|o| o.name == new.name)
                && same_but_name(old, new)
                && !index_renames.iter().any(|(_, n)| n.name == new.name)
        });
        match target {
            Some(new) => index_renames.push((old, new)),
            None => dropped_indexes.push(old),
        }
    }
    let added_indexes: Vec<&IndexDef> = after
        .indexes
        .iter()
        .filter(|i| !old_indexes.contains(i))
        .filter(|i| !index_renames.iter().any(|(_, n)| n.name == i.name))
        .collect();

    // An unnamed primary key is dropped as "the primary key" (an empty name, MySQL's
    // unnamed key); any other unnamed constraint has no name to drop it by and yields no
    // change (introspection names them; SQLite, which keeps them unnamed, rebuilds).
    let drop_constraint = |c: &ConstraintDef| {
        match (c, c.name()) {
            (_, Some(name)) => Some(name.to_string()),
            (ConstraintDef::PrimaryKey { .. }, None) => Some(String::new()),
            _ => None,
        }
        .map(|name| SchemaChange::DropConstraint {
            table: table.clone(),
            name,
        })
    };
    for c in dropped_constraints.iter().filter(|c| c.is_foreign_key()) {
        out.extend(drop_constraint(c));
    }
    for c in dropped_constraints.iter().filter(|c| !c.is_foreign_key()) {
        out.extend(drop_constraint(c));
    }
    for i in &dropped_indexes {
        out.push(SchemaChange::DropIndex {
            table: table.clone(),
            name: i.name.clone(),
        });
    }
    for (old, new) in &index_renames {
        out.push(SchemaChange::Rename {
            object: ObjectRef::child(ObjectKind::Index, table, &old.name),
            to: new.name.clone(),
        });
    }
    // A trigger follows its table's rename, and an editor may leave the trigger's `table`
    // reference stale while it changes only the table's own name: compare every other field.
    let same_trigger = |a: &crate::schema::TriggerDef, b: &crate::schema::TriggerDef| {
        a.name == b.name
            && a.timing == b.timing
            && a.events == b.events
            && a.for_each_row == b.for_each_row
            && a.condition == b.condition
            && a.body == b.body
    };
    for t in before
        .triggers
        .iter()
        .filter(|t| !after.triggers.iter().any(|a| same_trigger(t, a)))
    {
        out.push(SchemaChange::Drop {
            object: ObjectRef::child(ObjectKind::Trigger, table, &t.name),
            cascade: false,
        });
    }

    for pair in &pairs {
        if let (Some(b), None) = pair {
            out.push(SchemaChange::DropColumn {
                table: table.clone(),
                name: b.name.clone(),
            });
        }
    }
    for pair in &pairs {
        match pair {
            (None, Some(a)) => out.push(SchemaChange::AddColumn {
                table: table.clone(),
                column: (*a).clone(),
            }),
            (Some(b), Some(a)) if b != a => out.push(SchemaChange::AlterColumn {
                table: table.clone(),
                before: (*b).clone(),
                after: (*a).clone(),
            }),
            _ => {}
        }
    }

    let add_constraint = |c: &ConstraintDef| SchemaChange::AddConstraint {
        table: table.clone(),
        constraint: c.clone(),
    };
    for c in added_constraints.iter().filter(|c| !c.is_foreign_key()) {
        out.push(add_constraint(c));
    }
    for i in &added_indexes {
        out.push(SchemaChange::AddIndex {
            table: table.clone(),
            index: (*i).clone(),
            concurrently: false,
        });
    }
    for c in added_constraints.iter().filter(|c| c.is_foreign_key()) {
        out.push(add_constraint(c));
    }
    for t in after
        .triggers
        .iter()
        .filter(|t| !before.triggers.iter().any(|b| same_trigger(b, t)))
    {
        out.push(SchemaChange::Create(ObjectDef::Trigger(t.clone())));
    }

    for p in before
        .partitions
        .iter()
        .filter(|p| !after.partitions.contains(p))
    {
        out.push(SchemaChange::DetachPartition {
            table: table.clone(),
            partition: p.table.clone(),
        });
    }
    for p in after
        .partitions
        .iter()
        .filter(|p| !before.partitions.contains(p))
    {
        out.push(SchemaChange::AttachPartition {
            table: table.clone(),
            partition: p.clone(),
        });
    }

    let mut options: Vec<(String, Option<String>)> = Vec::new();
    for (k, v) in &after.options {
        if before.option(k) != Some(v.as_str()) {
            options.push((k.clone(), Some(v.clone())));
        }
    }
    for (k, _) in &before.options {
        if after.option(k).is_none() {
            options.push((k.clone(), None));
        }
    }
    if !options.is_empty() {
        out.push(SchemaChange::SetOptions {
            object: ObjectRef::relation(ObjectKind::Table, table),
            options,
        });
    }
    if before.comment != after.comment {
        out.push(SchemaChange::SetComment {
            object: ObjectRef::relation(ObjectKind::Table, table),
            column: None,
            comment: after.comment.clone(),
        });
    }
    out
}

/// The column names of `after` in the order the server keeps them once `diff_table`'s
/// changes are applied without any reordering: `before`'s columns (renamed, the dropped ones
/// left out), then the added columns at the end.
pub fn natural_column_order<'a>(before: &'a TableDef, after: &'a TableDef) -> Vec<&'a str> {
    let pairs = pair_columns(before, after);
    let mut kept: Vec<(usize, &str)> = pairs
        .iter()
        .filter_map(|p| match p {
            (Some(b), Some(a)) => {
                let at = before.columns.iter().position(|c| c.name == b.name)?;
                Some((at, a.name.as_str()))
            }
            _ => None,
        })
        .collect();
    kept.sort_by_key(|(at, _)| *at);
    let mut out: Vec<&str> = kept.into_iter().map(|(_, name)| name).collect();
    out.extend(pairs.iter().filter_map(|p| match p {
        (None, Some(a)) => Some(a.name.as_str()),
        _ => None,
    }));
    out
}

/// `after` lists its columns in another order than the server would keep them (see
/// [`natural_column_order`]): applying it needs a reorder.
pub fn column_order_changed(before: &TableDef, after: &TableDef) -> bool {
    let natural = natural_column_order(before, after);
    !natural
        .iter()
        .copied()
        .eq(after.columns.iter().map(|c| c.name.as_str()))
}

/// The changes that turn `before` into `after`, two definitions of one object of the same
/// kind. Tables follow [`diff_table`]. Views and routines are replaced (a routine whose
/// arguments change is dropped and created, as it becomes another routine); indexes and
/// triggers are dropped and created unless only their name changed; schemas and databases
/// change their owner, options and comment. A rename comes first.
pub fn diff_object(before: &ObjectDef, after: &ObjectDef) -> Vec<SchemaChange> {
    let mut out = Vec::new();
    let old = before.object_ref();
    let renamed = |new: &str| {
        (old.name != new).then(|| SchemaChange::Rename {
            object: old.clone(),
            to: new.to_string(),
        })
    };
    let comment = |b: &Option<String>, a: &Option<String>| {
        (b != a).then(|| SchemaChange::SetComment {
            object: after.object_ref(),
            column: None,
            comment: a.clone(),
        })
    };
    match (before, after) {
        (ObjectDef::Table(b), ObjectDef::Table(a)) => return diff_table(b, a),
        (ObjectDef::View(b), ObjectDef::View(a)) => {
            out.extend(renamed(&a.view.name));
            let body_changed = b.query != a.query
                || b.columns != a.columns
                || b.options != a.options
                || b.indexes != a.indexes
                || b.materialized != a.materialized;
            if body_changed {
                out.push(SchemaChange::ReplaceView(a.clone()));
            }
            out.extend(comment(&b.comment, &a.comment));
        }
        (ObjectDef::Routine(b), ObjectDef::Routine(a)) => {
            if b.arguments != a.arguments || b.kind != a.kind {
                out.push(SchemaChange::Drop {
                    object: old,
                    cascade: false,
                });
                out.push(SchemaChange::Create(after.clone()));
                return out;
            }
            out.extend(renamed(&a.name));
            let renamed_only = RoutineDef {
                name: a.name.clone(),
                ..b.clone()
            };
            if renamed_only != *a {
                out.push(SchemaChange::ReplaceRoutine(a.clone()));
            }
        }
        (
            ObjectDef::Index {
                table: bt,
                index: b,
            },
            ObjectDef::Index {
                table: at,
                index: a,
            },
        ) => {
            let same = IndexDef {
                name: a.name.clone(),
                ..b.clone()
            } == *a;
            if same && bt == at {
                out.extend(renamed(&a.name));
            } else if before != after {
                out.push(SchemaChange::DropIndex {
                    table: bt.clone(),
                    name: b.name.clone(),
                });
                out.push(SchemaChange::AddIndex {
                    table: at.clone(),
                    index: a.clone(),
                    concurrently: false,
                });
            }
        }
        (ObjectDef::Trigger(b), ObjectDef::Trigger(a)) => {
            let same = crate::schema::TriggerDef {
                name: a.name.clone(),
                ..b.clone()
            } == *a;
            if same {
                out.extend(renamed(&a.name));
            } else {
                out.push(SchemaChange::Drop {
                    object: old,
                    cascade: false,
                });
                out.push(SchemaChange::Create(after.clone()));
            }
        }
        (ObjectDef::Sequence(b), ObjectDef::Sequence(a)) => {
            out.extend(renamed(&a.sequence.name));
            out.extend(comment(&b.comment, &a.comment));
        }
        (ObjectDef::Schema(b), ObjectDef::Schema(a)) => {
            out.extend(renamed(&a.name));
            if b.owner != a.owner
                && let Some(owner) = &a.owner
            {
                out.push(SchemaChange::SetOptions {
                    object: after.object_ref(),
                    options: vec![("owner".into(), Some(owner.clone()))],
                });
            }
            out.extend(comment(&b.comment, &a.comment));
        }
        (ObjectDef::Database(b), ObjectDef::Database(a)) => {
            out.extend(renamed(&a.name));
            let find = |opts: &[(String, String)], k: &str| {
                opts.iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v.clone())
            };
            let mut options: Vec<(String, Option<String>)> = a
                .options
                .iter()
                .filter(|(k, v)| find(&b.options, k).as_ref() != Some(v))
                .map(|(k, v)| (k.clone(), Some(v.clone())))
                .collect();
            options.extend(
                b.options
                    .iter()
                    .filter(|(k, _)| find(&a.options, k).is_none())
                    .map(|(k, _)| (k.clone(), None)),
            );
            if !options.is_empty() {
                out.push(SchemaChange::SetOptions {
                    object: after.object_ref(),
                    options,
                });
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{FkAction, IndexColumn, TriggerDef};

    #[test]
    fn column_renames_do_not_rewrite_index_expressions_with_the_same_text() {
        let index = IndexDef {
            columns: vec![
                IndexColumn::column("(1)"),
                IndexColumn {
                    expr: "(1)".into(),
                    expression: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let renamed = renamed_index(&index, &[("(1)", "new_name")].into());
        assert_eq!(renamed.columns[0].expr, "new_name");
        assert_eq!(renamed.columns[1].expr, "(1)");
    }

    fn t(name: &str) -> TableRef {
        TableRef {
            database: None,
            schema: Some("public".into()),
            name: name.into(),
        }
    }

    fn col(name: &str) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            type_name: "integer".into(),
            nullable: true,
            ..Default::default()
        }
    }

    fn base() -> TableDef {
        let mut def = TableDef::new(t("orders"));
        def.columns = vec![col("id"), col("customer"), col("total")];
        def.constraints.push(ConstraintDef::PrimaryKey {
            name: Some("orders_pkey".into()),
            columns: vec!["id".into()],
        });
        def.indexes.push(IndexDef {
            name: "orders_total".into(),
            columns: vec![IndexColumn::column("total")],
            ..Default::default()
        });
        def
    }

    fn fk() -> ConstraintDef {
        ConstraintDef::ForeignKey {
            name: Some("orders_customer_fk".into()),
            columns: vec!["customer".into()],
            target: t("customers"),
            target_columns: vec!["id".into()],
            on_update: FkAction::NoAction,
            on_delete: FkAction::Cascade,
            deferrable: false,
            initially_deferred: false,
        }
    }

    #[test]
    fn diff_identical_is_empty() {
        let def = base();
        assert_eq!(diff_table(&def, &def.clone()), []);
    }

    #[test]
    fn diff_table_orders_changes() {
        let before = {
            let mut d = base();
            d.constraints.push(ConstraintDef::ForeignKey {
                name: Some("old_fk".into()),
                columns: vec!["customer".into()],
                target: t("people"),
                target_columns: vec!["id".into()],
                on_update: FkAction::NoAction,
                on_delete: FkAction::NoAction,
                deferrable: false,
                initially_deferred: false,
            });
            d.constraints.push(ConstraintDef::Check {
                name: Some("positive".into()),
                expression: "total > 0".into(),
            });
            d
        };
        let mut after = base();
        after.indexes.clear();
        after.columns.push(ColumnDef {
            comment: Some("note".into()),
            ..col("note")
        });
        after.columns[2].nullable = false;
        after.constraints.push(fk());
        after.constraints.push(ConstraintDef::Unique {
            name: Some("orders_note_key".into()),
            columns: vec!["note".into()],
        });
        after.indexes.push(IndexDef {
            name: "orders_note".into(),
            columns: vec![IndexColumn::column("note")],
            ..Default::default()
        });
        after.comment = Some("All orders".into());
        let changes = diff_table(&before, &after);
        let table = t("orders");
        assert_eq!(
            changes,
            [
                SchemaChange::DropConstraint {
                    table: table.clone(),
                    name: "old_fk".into()
                },
                SchemaChange::DropConstraint {
                    table: table.clone(),
                    name: "positive".into()
                },
                SchemaChange::DropIndex {
                    table: table.clone(),
                    name: "orders_total".into()
                },
                SchemaChange::AlterColumn {
                    table: table.clone(),
                    before: col("total"),
                    after: after.columns[2].clone()
                },
                SchemaChange::AddColumn {
                    table: table.clone(),
                    column: after.columns[3].clone()
                },
                SchemaChange::AddConstraint {
                    table: table.clone(),
                    constraint: after.constraints[2].clone()
                },
                SchemaChange::AddIndex {
                    table: table.clone(),
                    index: after.indexes[0].clone(),
                    concurrently: false
                },
                SchemaChange::AddConstraint {
                    table: table.clone(),
                    constraint: fk()
                },
                SchemaChange::SetComment {
                    object: ObjectRef::relation(ObjectKind::Table, &table),
                    column: None,
                    comment: Some("All orders".into())
                },
            ]
        );
    }

    #[test]
    fn rename_in_place_is_one_alter_and_keeps_constraints() {
        let before = base();
        let mut after = base();
        after.columns[0].name = "order_id".into();
        after.constraints[0] = ConstraintDef::PrimaryKey {
            name: Some("orders_pkey".into()),
            columns: vec!["order_id".into()],
        };
        assert_eq!(
            diff_table(&before, &after),
            [SchemaChange::AlterColumn {
                table: t("orders"),
                before: col("id"),
                after: col("order_id"),
            }]
        );
        // Reverting the edit removes the change.
        after.columns[0].name = "id".into();
        after.constraints[0] = before.constraints[0].clone();
        assert_eq!(diff_table(&before, &after), []);
    }

    #[test]
    fn renames_tables_and_indexes_and_moves_triggers() {
        let mut before = base();
        before.triggers.push(TriggerDef {
            name: "trg".into(),
            table: t("orders"),
            timing: "BEFORE".into(),
            events: vec!["INSERT".into()],
            for_each_row: true,
            condition: None,
            body: "EXECUTE FUNCTION f()".into(),
        });
        let mut after = before.clone();
        after.table.name = "orders2".into();
        after.triggers[0].table.name = "orders2".into();
        after.indexes[0].name = "by_total".into();
        after.options.push(("tablespace".into(), "fast".into()));
        let changes = diff_table(&before, &after);
        assert_eq!(
            changes,
            [
                SchemaChange::Rename {
                    object: ObjectRef::relation(ObjectKind::Table, &t("orders")),
                    to: "orders2".into()
                },
                SchemaChange::Rename {
                    object: ObjectRef::child(ObjectKind::Index, &t("orders2"), "orders_total"),
                    to: "by_total".into()
                },
                SchemaChange::SetOptions {
                    object: ObjectRef::relation(ObjectKind::Table, &t("orders2")),
                    options: vec![("tablespace".into(), Some("fast".into()))]
                },
            ]
        );
    }

    #[test]
    fn a_table_rename_keeps_triggers_with_a_stale_table_reference() {
        let mut before = base();
        before.triggers.push(TriggerDef {
            name: "trg".into(),
            table: t("orders"),
            timing: "BEFORE".into(),
            events: vec!["INSERT".into()],
            for_each_row: true,
            condition: None,
            body: "EXECUTE FUNCTION f()".into(),
        });
        let mut after = before.clone();
        after.table.name = "orders2".into();
        // The editor renames the table but leaves `after.triggers[0].table` as it was.
        assert_eq!(
            diff_table(&before, &after),
            [SchemaChange::Rename {
                object: ObjectRef::relation(ObjectKind::Table, &t("orders")),
                to: "orders2".into()
            }]
        );

        // A real trigger change still drops and creates it.
        after.triggers[0].body = "EXECUTE FUNCTION g()".into();
        assert!(diff_table(&before, &after).iter().any(|c| matches!(
            c,
            SchemaChange::Drop {
                object,
                ..
            } if object.kind == ObjectKind::Trigger
        )));
        assert!(diff_table(&before, &after).iter().any(|c| matches!(
            c,
            SchemaChange::Create(ObjectDef::Trigger(t)) if t.body == "EXECUTE FUNCTION g()"
        )));
    }

    #[test]
    fn unnamed_dropped_constraints() {
        let mut before = base();
        before.constraints = vec![
            ConstraintDef::PrimaryKey {
                name: None,
                columns: vec!["id".into()],
            },
            ConstraintDef::Unique {
                name: None,
                columns: vec!["total".into()],
            },
        ];
        let mut after = before.clone();
        after.constraints.clear();
        // The unnamed primary key is dropped as "the primary key" (empty name); another
        // unnamed constraint has no name to drop by, so it yields no change (drivers that
        // keep unnamed constraints, like SQLite, compare constraints themselves).
        assert_eq!(
            diff_table(&before, &after),
            [SchemaChange::DropConstraint {
                table: t("orders"),
                name: String::new(),
            }]
        );
    }

    #[test]
    fn drop_and_add_columns_and_partitions() {
        let before = base();
        let mut after = base();
        after.columns.remove(1);
        after.columns.push(col("extra"));
        after.partitions.push(PartitionDef {
            table: t("orders_2024"),
            bound: "FOR VALUES FROM (1) TO (10)".into(),
        });
        let changes = diff_table(&before, &after);
        assert_eq!(
            changes,
            [
                SchemaChange::DropColumn {
                    table: t("orders"),
                    name: "customer".into()
                },
                SchemaChange::AddColumn {
                    table: t("orders"),
                    column: col("extra")
                },
                SchemaChange::AttachPartition {
                    table: t("orders"),
                    partition: after.partitions[0].clone()
                },
            ]
        );
    }

    fn view(name: &str, query: &str) -> crate::schema::ViewDef {
        crate::schema::ViewDef {
            view: t(name),
            query: query.into(),
            materialized: false,
            columns: Vec::new(),
            comment: None,
            options: Vec::new(),
            indexes: Vec::new(),
        }
    }

    #[test]
    fn diff_object_views_and_routines() {
        let before = ObjectDef::View(view("v", "SELECT 1"));
        assert_eq!(diff_object(&before, &before.clone()), []);
        let mut v = view("w", "SELECT 2");
        v.comment = Some("c".into());
        let after = ObjectDef::View(v.clone());
        assert_eq!(
            diff_object(&before, &after),
            [
                SchemaChange::Rename {
                    object: before.object_ref(),
                    to: "w".into()
                },
                SchemaChange::ReplaceView(v),
                SchemaChange::SetComment {
                    object: after.object_ref(),
                    column: None,
                    comment: Some("c".into())
                },
            ]
        );

        let routine = |args: &str, body: &str| {
            ObjectDef::Routine(RoutineDef {
                schema: Some("public".into()),
                name: "f".into(),
                kind: ObjectKind::Function,
                arguments: args.into(),
                identity_arguments: Some(args.into()),
                returns: Some("integer".into()),
                language: Some("sql".into()),
                body: body.into(),
                options: Vec::new(),
            })
        };
        let before = routine("a integer", "SELECT 1");
        let body = routine("a integer", "SELECT 2");
        let ObjectDef::Routine(r) = &body else {
            unreachable!()
        };
        assert_eq!(
            diff_object(&before, &body),
            [SchemaChange::ReplaceRoutine(r.clone())]
        );
        // New arguments make another routine: the old one is dropped.
        let args = routine("a integer, b text", "SELECT 1");
        assert_eq!(
            diff_object(&before, &args),
            [
                SchemaChange::Drop {
                    object: before.object_ref(),
                    cascade: false
                },
                SchemaChange::Create(args.clone()),
            ]
        );
    }

    #[test]
    fn diff_object_indexes_schemas_databases() {
        let index = |name: &str, unique: bool| ObjectDef::Index {
            table: t("orders"),
            index: IndexDef {
                name: name.into(),
                columns: vec![IndexColumn::column("total")],
                unique,
                ..Default::default()
            },
        };
        let before = index("i", false);
        assert_eq!(
            diff_object(&before, &index("j", false)),
            [SchemaChange::Rename {
                object: before.object_ref(),
                to: "j".into()
            }]
        );
        let ObjectDef::Index { index: unique, .. } = index("i", true) else {
            unreachable!()
        };
        assert_eq!(
            diff_object(&before, &index("i", true)),
            [
                SchemaChange::DropIndex {
                    table: t("orders"),
                    name: "i".into()
                },
                SchemaChange::AddIndex {
                    table: t("orders"),
                    index: unique,
                    concurrently: false
                },
            ]
        );

        let schema = |owner: &str| {
            ObjectDef::Schema(crate::schema::SchemaDef {
                name: "s".into(),
                owner: Some(owner.into()),
                comment: None,
            })
        };
        assert_eq!(
            diff_object(&schema("a"), &schema("b")),
            [SchemaChange::SetOptions {
                object: schema("a").object_ref(),
                options: vec![("owner".into(), Some("b".into()))]
            }]
        );
        let db = |options: &[(&str, &str)]| {
            ObjectDef::Database(crate::schema::DatabaseDef {
                name: "d".into(),
                options: options
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            })
        };
        assert_eq!(
            diff_object(
                &db(&[("charset", "latin1")]),
                &db(&[("charset", "utf8mb4")])
            ),
            [SchemaChange::SetOptions {
                object: db(&[]).object_ref(),
                options: vec![("charset".into(), Some("utf8mb4".into()))]
            }]
        );
    }

    #[test]
    fn column_order_follows_the_server() {
        let before = base();
        let mut after = base();
        assert!(!column_order_changed(&before, &after));
        // Renamed and appended columns keep the server's natural order.
        after.columns[1].name = "client".into();
        after.columns.push(col("extra"));
        assert!(!column_order_changed(&before, &after));
        after.columns.swap(0, 2);
        assert!(column_order_changed(&before, &after));
        let mut added_first = base();
        added_first.columns.insert(0, col("first"));
        assert!(column_order_changed(&before, &added_first));
    }
}
