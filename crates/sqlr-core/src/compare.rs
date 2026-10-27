//! Schema compare (spec §8.6): two snapshots of a schema become a list of per-object
//! differences and an ordered migration script. Nothing here touches a database; the
//! script only ever opens in an editor.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::driver::{DdlGen, script};
use crate::schema::{
    ColumnDef, ConstraintDef, IndexDef, ObjectDef, ObjectKind, ObjectRef, TableDef, ViewDef,
};
use crate::schema_change::{SchemaChange, diff_table};
use crate::table::TableRef;

/// Every object of one schema, loaded in bulk (not one query per object).
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaSnapshot {
    /// The driver id (`postgres`, `mysql`, `sqlite`).
    pub engine: String,
    /// The schema name.
    pub schema: String,
    /// Tables, views, sequences and routines. Indexes, triggers and constraints are parts
    /// of their table.
    pub objects: Vec<ObjectDef>,
    /// `(dependent, dependency)` pairs the server knows (views on tables and views). Where
    /// a driver leaves them out, a view's dependencies are read from its query text.
    pub dependencies: Vec<(ObjectRef, ObjectRef)>,
}

/// How an object differs between the source and the target schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    /// In the source only: the migration creates it.
    Added,
    /// In the target only: the migration drops it.
    Removed,
    /// In both with different definitions: the migration alters it.
    Changed,
}

/// One object that differs. `object` is named as it is (or would be) in the target schema.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectDiff {
    /// The object, named as in the target schema.
    pub object: ObjectRef,
    /// Whether the object is added, removed or changed.
    pub kind: DiffKind,
    /// The source object's DDL, when it exists there.
    pub source_ddl: Option<String>,
    /// The target object's DDL, when it exists there.
    pub target_ddl: Option<String>,
    /// The changes that make the target look like the source.
    pub changes: Vec<SchemaChange>,
    /// Drops, column drops and column changes that can lose data.
    pub destructive: bool,
    /// The source object (named in the target schema), for what exists there.
    pub source_def: Option<ObjectDef>,
    /// The target object, for what exists there.
    pub target_def: Option<ObjectDef>,
    /// The tables and views a view reads, by name (lower case).
    pub deps: Vec<String>,
}

/// `text` with every run of whitespace outside quotes folded into one space.
fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut quote: Option<char> = None;
    let mut space = false;
    for c in text.trim().chars() {
        match quote {
            Some(q) => {
                out.push(c);
                if c == q {
                    quote = None;
                }
            }
            None if c.is_whitespace() => space = true,
            None => {
                if space && !out.is_empty() {
                    out.push(' ');
                }
                space = false;
                if c == '\'' || c == '"' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

/// A column type with its spelling made one: lower case, `int4` as `integer`…
fn normalize_type(type_name: &str) -> String {
    let lower = collapse_ws(&type_name.to_lowercase());
    let (base, rest) = match lower.find(['(', '[']) {
        Some(i) => (lower[..i].trim_end(), &lower[i..]),
        None => (lower.as_str(), ""),
    };
    let alias = match base {
        "int" | "int4" => "integer",
        "int8" => "bigint",
        "int2" => "smallint",
        "bool" => "boolean",
        "float8" | "double" => "double precision",
        "float4" => "real",
        "varchar" => "character varying",
        "bpchar" => "character",
        "timestamptz" => "timestamp with time zone",
        "timetz" => "time with time zone",
        "decimal" => "numeric",
        other => other,
    };
    format!("{alias}{rest}")
}

fn normalize_column(c: &ColumnDef) -> ColumnDef {
    ColumnDef {
        type_name: normalize_type(&c.type_name),
        default: c.default.as_deref().map(collapse_ws),
        generated: c.generated.as_deref().map(collapse_ws),
        ..c.clone()
    }
}

fn normalize_index(i: &IndexDef) -> IndexDef {
    IndexDef {
        predicate: i.predicate.as_deref().map(collapse_ws),
        definition: i.definition.as_deref().map(collapse_ws),
        ..i.clone()
    }
}

/// `def` with the spellings a server may vary made equal (types, whitespace in expressions
/// and bodies), so that only real differences are compared.
pub fn normalize(def: &ObjectDef) -> ObjectDef {
    match def {
        ObjectDef::Table(t) => {
            let mut t = t.clone();
            t.columns = t.columns.iter().map(normalize_column).collect();
            t.indexes = t.indexes.iter().map(normalize_index).collect();
            for c in &mut t.constraints {
                match c {
                    ConstraintDef::Check { expression, .. } => {
                        *expression = collapse_ws(expression)
                    }
                    ConstraintDef::Raw { definition, .. } => *definition = collapse_ws(definition),
                    _ => {}
                }
            }
            for tr in &mut t.triggers {
                tr.body = collapse_ws(&tr.body);
                tr.condition = tr.condition.as_deref().map(collapse_ws);
            }
            ObjectDef::Table(t)
        }
        ObjectDef::View(v) => ObjectDef::View(ViewDef {
            query: collapse_ws(&v.query),
            indexes: v.indexes.iter().map(normalize_index).collect(),
            ..v.clone()
        }),
        ObjectDef::Routine(r) => {
            let mut r = r.clone();
            r.body = collapse_ws(&r.body);
            r.arguments = collapse_ws(&r.arguments);
            r.returns = r.returns.as_deref().map(collapse_ws);
            ObjectDef::Routine(r)
        }
        other => other.clone(),
    }
}

/// `def` without the objects that merely name a table (SQLite's `dependents`): they are
/// those objects' own differences, not the table's.
fn without_dependents(def: &ObjectDef) -> ObjectDef {
    match def {
        ObjectDef::Table(t) => ObjectDef::Table(TableDef {
            dependents: Vec::new(),
            ..t.clone()
        }),
        other => other.clone(),
    }
}

/// `text` with the qualifier `from.` (plain or quoted) written `to.`: `FROM a.t` becomes
/// `FROM b.t`. Text inside single quotes is data and stays; a name that merely ends like the
/// schema (`data.t`) is left alone.
fn requalify(text: &str, from: &str, to: &str) -> String {
    if from == to {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            // A string literal, copied whole (a doubled quote stays inside it) - except a
            // name cast to a `reg...` type (`'a.t_id_seq'::regclass`), which is a qualifier.
            let mut literal = String::from("'");
            i += 1;
            while i < chars.len() {
                literal.push(chars[i]);
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        literal.push('\'');
                        i += 1;
                    } else {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            let names_object = chars[i..].iter().take(5).collect::<String>() == "::reg";
            if names_object {
                let inner = &literal[1..literal.len().saturating_sub(1)];
                out.push('\'');
                out.push_str(&requalify(inner, from, to));
                out.push('\'');
            } else {
                out.push_str(&literal);
            }
            continue;
        }
        let at_start = i == 0
            || !(ident(chars[i - 1])
                || chars[i - 1] == '.'
                || chars[i - 1] == '"'
                || chars[i - 1] == '`');
        if at_start && (c == '"' || c == '`') {
            // A quoted identifier: the schema qualifier is rewritten, any other is copied
            // whole.
            if let Some(end) = chars[i + 1..].iter().position(|&x| x == c) {
                let name: String = chars[i + 1..i + 1 + end].iter().collect();
                if name == from && chars.get(i + end + 2) == Some(&'.') {
                    out.push(c);
                    out.push_str(to);
                    out.push(c);
                } else {
                    out.extend(&chars[i..i + end + 2]);
                }
                i += end + 2;
                continue;
            }
        } else if at_start && ident(c) {
            let mut j = i;
            while j < chars.len() && ident(chars[j]) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            if word.eq_ignore_ascii_case(from) && chars.get(j) == Some(&'.') {
                out.push_str(to);
            } else {
                out.push_str(&word);
            }
            i = j;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `def` moved from schema `from` to schema `to`: its own name and the references to tables
/// of `from`.
fn retarget(def: &ObjectDef, from: &str, to: &str) -> ObjectDef {
    let text = |t: &str| requalify(t, from, to);
    let table = |t: &TableRef| -> TableRef {
        if t.schema.as_deref() == Some(from) || t.schema.is_none() {
            TableRef {
                schema: Some(to.to_string()),
                ..t.clone()
            }
        } else {
            t.clone()
        }
    };
    match def {
        ObjectDef::Table(t) => {
            let mut t = t.clone();
            t.table = table(&t.table);
            for c in &mut t.columns {
                c.default = c.default.as_deref().map(&text);
                c.generated = c.generated.as_deref().map(&text);
            }
            for c in &mut t.constraints {
                match c {
                    ConstraintDef::ForeignKey { target, .. } => *target = table(target),
                    ConstraintDef::Check { expression, .. } => *expression = text(expression),
                    ConstraintDef::Raw { definition, .. } => *definition = text(definition),
                    _ => {}
                }
            }
            for i in &mut t.indexes {
                i.predicate = i.predicate.as_deref().map(&text);
                i.definition = i.definition.as_deref().map(&text);
            }
            for tr in &mut t.triggers {
                tr.table = table(&tr.table);
                tr.body = text(&tr.body);
                tr.condition = tr.condition.as_deref().map(&text);
            }
            ObjectDef::Table(t)
        }
        ObjectDef::View(v) => ObjectDef::View(ViewDef {
            view: table(&v.view),
            query: text(&v.query),
            ..v.clone()
        }),
        ObjectDef::Sequence(s) => {
            let mut s = s.clone();
            s.sequence = table(&s.sequence);
            ObjectDef::Sequence(s)
        }
        ObjectDef::Routine(r) => {
            let mut r = r.clone();
            r.schema = Some(to.to_string());
            r.body = text(&r.body);
            ObjectDef::Routine(r)
        }
        other => other.clone(),
    }
}

/// The identity two snapshots match an object by: kind, name and routine signature (not
/// the schema).
fn key_of(def: &ObjectDef) -> ObjectKey {
    let r = def.object_ref();
    (
        r.kind.keyword(),
        r.name.clone(),
        r.signature.unwrap_or_default(),
    )
}

type ObjectKey = (&'static str, String, String);

/// The names (lower case) of the objects of `known` that `query` mentions.
fn mentioned(query: &str, known: &BTreeSet<String>, own: &str) -> Vec<String> {
    let own = own.to_lowercase();
    let words: BTreeSet<String> = query
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    known
        .iter()
        .filter(|name| **name != own && words.contains(*name))
        .cloned()
        .collect()
}

fn deps_of(def: &ObjectDef, known: &BTreeSet<String>) -> Vec<String> {
    match def {
        ObjectDef::View(v) => mentioned(&v.query, known, &v.view.name),
        _ => Vec::new(),
    }
}

fn relation_names(snapshot: &SchemaSnapshot) -> BTreeSet<String> {
    snapshot
        .objects
        .iter()
        .filter(|o| matches!(o, ObjectDef::Table(_) | ObjectDef::View(_)))
        .map(|o| o.object_ref().name.to_lowercase())
        .collect()
}

/// Whether the changes can lose data: a dropped column, a changed type or a column made
/// `NOT NULL`.
fn lossy(changes: &[SchemaChange]) -> bool {
    changes.iter().any(|c| match c {
        SchemaChange::DropColumn { .. } => true,
        SchemaChange::AlterColumn { before, after, .. } => {
            before.type_name != after.type_name || (before.nullable && !after.nullable)
        }
        _ => false,
    })
}

fn order_key(d: &ObjectDiff) -> (String, &'static str, String) {
    (
        d.object.name.clone(),
        d.object.kind.keyword(),
        d.object.signature.clone().unwrap_or_default(),
    )
}

fn is_relevant(o: &&ObjectDef) -> bool {
    matches!(
        o,
        ObjectDef::Table(_) | ObjectDef::View(_) | ObjectDef::Sequence(_) | ObjectDef::Routine(_)
    )
}

/// The differences that turn `target` into `source` (both of one engine), by object. A
/// source object is renamed into the target's schema, so the schemas' names do not matter.
/// Objects are matched by name: a renamed object is a drop and a create. Differences of
/// spelling only (`int4` and `integer`, whitespace in a view) are no differences.
pub fn compare(
    source: &SchemaSnapshot,
    target: &SchemaSnapshot,
    generator: &dyn DdlGen,
) -> Vec<ObjectDiff> {
    let moved: BTreeMap<ObjectKey, ObjectDef> = source
        .objects
        .iter()
        .filter(is_relevant)
        .map(|o| (key_of(o), retarget(o, &source.schema, &target.schema)))
        .collect();
    let existing: BTreeMap<ObjectKey, &ObjectDef> = target
        .objects
        .iter()
        .filter(is_relevant)
        .map(|o| (key_of(o), o))
        .collect();
    let source_names = relation_names(source);
    let target_names = relation_names(target);
    let ddl = |def: &ObjectDef| generator.create_ddl(def).ok();
    let mut diffs = Vec::new();
    for (key, wanted_def) in &moved {
        let object = wanted_def.object_ref();
        match existing.get(key) {
            None => diffs.push(ObjectDiff {
                object,
                kind: DiffKind::Added,
                source_ddl: ddl(wanted_def),
                target_ddl: None,
                changes: vec![SchemaChange::Create(wanted_def.clone())],
                destructive: false,
                source_def: Some(wanted_def.clone()),
                target_def: None,
                deps: deps_of(wanted_def, &source_names),
            }),
            Some(have) => {
                let (n_have, n_want) = (normalize(have), normalize(wanted_def));
                if without_dependents(&n_have) == without_dependents(&n_want) {
                    continue;
                }
                let (changes, destructive) = match (&n_have, &n_want) {
                    (ObjectDef::Table(a), ObjectDef::Table(b)) => {
                        let changes = diff_table(a, b);
                        let lossy = lossy(&changes);
                        (changes, lossy)
                    }
                    (ObjectDef::View(a), ObjectDef::View(b))
                        if !a.materialized && !b.materialized =>
                    {
                        (vec![SchemaChange::ReplaceView(b.clone())], false)
                    }
                    (ObjectDef::Routine(_), ObjectDef::Routine(r)) => {
                        (vec![SchemaChange::ReplaceRoutine(r.clone())], false)
                    }
                    // Anything else is dropped and created again.
                    _ => (
                        vec![
                            SchemaChange::Drop {
                                object: have.object_ref(),
                                cascade: false,
                            },
                            SchemaChange::Create(wanted_def.clone()),
                        ],
                        true,
                    ),
                };
                diffs.push(ObjectDiff {
                    object,
                    kind: DiffKind::Changed,
                    source_ddl: ddl(wanted_def),
                    target_ddl: ddl(have),
                    changes,
                    destructive,
                    source_def: Some(wanted_def.clone()),
                    target_def: Some((*have).clone()),
                    deps: deps_of(wanted_def, &source_names),
                });
            }
        }
    }
    for (key, have) in &existing {
        if moved.contains_key(key) {
            continue;
        }
        let mut changes = Vec::new();
        // A table's foreign keys go before the table, so a cycle cannot block the drop.
        if let ObjectDef::Table(t) = have {
            for c in &t.constraints {
                if let (true, Some(name)) = (c.is_foreign_key(), c.name()) {
                    changes.push(SchemaChange::DropConstraint {
                        table: t.table.clone(),
                        name: name.to_string(),
                    });
                }
            }
        }
        changes.push(SchemaChange::Drop {
            object: have.object_ref(),
            cascade: false,
        });
        diffs.push(ObjectDiff {
            object: have.object_ref(),
            kind: DiffKind::Removed,
            source_ddl: None,
            target_ddl: ddl(have),
            changes,
            destructive: true,
            source_def: None,
            target_def: Some((*have).clone()),
            deps: deps_of(have, &target_names),
        });
    }
    diffs.sort_by_key(order_key);
    diffs
}

/// What a step of the script does to data, for the warnings at its head.
fn warnings(diffs: &[&ObjectDiff]) -> Vec<String> {
    let mut out = Vec::new();
    for d in diffs {
        let what = d.object.kind.keyword().to_lowercase();
        match d.kind {
            DiffKind::Removed => out.push(format!("drops {what} {}", d.object.name)),
            DiffKind::Changed => {
                for c in &d.changes {
                    match c {
                        SchemaChange::DropColumn { table, name } => {
                            out.push(format!("drops column {}.{name}", table.name));
                        }
                        SchemaChange::AlterColumn {
                            table,
                            before,
                            after,
                        } if before.type_name != after.type_name
                            || (before.nullable && !after.nullable) =>
                        {
                            out.push(format!("alters column {}.{}", table.name, after.name));
                        }
                        SchemaChange::Drop { object, .. } => {
                            out.push(format!(
                                "drops {} {} and creates it again",
                                object.kind.keyword().to_lowercase(),
                                object.name
                            ));
                        }
                        _ => {}
                    }
                }
            }
            DiffKind::Added => {}
        }
    }
    out
}

/// Indexes of `items` ordered so that every object comes after the objects it reads; ties
/// and cycles keep name order.
fn dependency_order(items: &[&ObjectDiff]) -> Vec<usize> {
    let names: Vec<String> = items.iter().map(|d| d.object.name.to_lowercase()).collect();
    let mut placed = vec![false; items.len()];
    let mut order = Vec::new();
    while order.len() < items.len() {
        let ready = (0..items.len()).find(|&i| {
            !placed[i]
                && items[i].deps.iter().all(|dep| {
                    names
                        .iter()
                        .position(|n| n == dep)
                        .is_none_or(|j| placed[j] || j == i)
                })
        });
        // A cycle: take the first one left.
        let next = ready
            .or_else(|| (0..items.len()).find(|&i| !placed[i]))
            .expect("the loop runs only while an object is still unplaced");
        placed[next] = true;
        order.push(next);
    }
    order
}

/// The migration script for `diffs` minus the `excluded` objects, in an order that applies:
/// sequences, tables (without foreign keys), table changes, views by dependency, routines,
/// triggers, foreign keys, then the drops in reverse dependency order. A header line and a
/// list of the destructive steps come first. The same diffs give the same text.
pub fn migration_script(
    diffs: &[ObjectDiff],
    excluded: &HashSet<ObjectRef>,
    generator: &dyn DdlGen,
    header: &str,
) -> String {
    let active: Vec<&ObjectDiff> = diffs
        .iter()
        .filter(|d| !excluded.contains(&d.object))
        .collect();
    let mut statements: Vec<String> = Vec::new();
    let mut apply = |changes: &[SchemaChange]| {
        for change in changes {
            match generator.ddl(std::slice::from_ref(change)) {
                Ok(mut lines) => statements.append(&mut lines),
                Err(e) => statements.push(format!("-- cannot generate: {}", e.message)),
            }
        }
    };
    let of_kind = |kinds: &[ObjectKind], k: DiffKind| -> Vec<&ObjectDiff> {
        active
            .iter()
            .copied()
            .filter(|d| d.kind == k && kinds.contains(&d.object.kind))
            .collect()
    };
    let views = [ObjectKind::View, ObjectKind::MaterializedView];
    let routines = [ObjectKind::Function, ObjectKind::Procedure];

    // Sequences first; a changed one is dropped and created again.
    for d in active
        .iter()
        .filter(|d| d.object.kind == ObjectKind::Sequence && d.kind != DiffKind::Removed)
    {
        apply(&d.changes);
    }
    // New tables without their foreign keys and triggers (those come later).
    let mut late_fks = Vec::new();
    let mut late_triggers = Vec::new();
    for d in of_kind(&[ObjectKind::Table], DiffKind::Added) {
        let Some(ObjectDef::Table(table)) = &d.source_def else {
            continue;
        };
        let mut bare: TableDef = table.clone();
        for c in std::mem::take(&mut bare.constraints) {
            if c.is_foreign_key() {
                late_fks.push(SchemaChange::AddConstraint {
                    table: table.table.clone(),
                    constraint: c,
                });
            } else {
                bare.constraints.push(c);
            }
        }
        for tr in std::mem::take(&mut bare.triggers) {
            late_triggers.push(SchemaChange::Create(ObjectDef::Trigger(tr)));
        }
        apply(&[SchemaChange::Create(ObjectDef::Table(bare))]);
    }
    // Views that go or change leave first, readers before what they read: a column cannot
    // be dropped under a view that reads it, nor a view's columns replaced.
    let mut leaving = of_kind(&views, DiffKind::Removed);
    leaving.extend(of_kind(&views, DiffKind::Changed));
    for i in dependency_order(&leaving).into_iter().rev() {
        apply(&[SchemaChange::Drop {
            object: leaving[i].object.clone(),
            cascade: false,
        }]);
    }
    // Changed tables: the generator's own way to turn one into the other.
    let mut rebuilt: Vec<String> = Vec::new();
    for d in of_kind(&[ObjectKind::Table], DiffKind::Changed) {
        if let (Some(have), Some(want)) = (&d.target_def, &d.source_def)
            && let (ObjectDef::Table(have), ObjectDef::Table(mut want)) =
                (normalize(have), normalize(want))
        {
            // The target is where the table is rebuilt: its dependents are the ones to
            // recreate.
            want.dependents = have.dependents.clone();
            match generator.table_ddl(&have, &want) {
                Ok(mut lines) => rebuilt.append(&mut lines),
                Err(e) => rebuilt.push(format!("-- cannot generate: {}", e.message)),
            }
        }
    }
    statements.append(&mut rebuilt);
    let mut apply = |changes: &[SchemaChange]| {
        for change in changes {
            match generator.ddl(std::slice::from_ref(change)) {
                Ok(mut lines) => statements.append(&mut lines),
                Err(e) => statements.push(format!("-- cannot generate: {}", e.message)),
            }
        }
    };
    // Views, readers after what they read.
    let mut view_diffs = of_kind(&views, DiffKind::Added);
    view_diffs.extend(of_kind(&views, DiffKind::Changed));
    for i in dependency_order(&view_diffs) {
        // A changed view was dropped above: it comes back as a create.
        match (&view_diffs[i].kind, &view_diffs[i].source_def) {
            (DiffKind::Changed, Some(def)) => apply(&[SchemaChange::Create(def.clone())]),
            _ => apply(&view_diffs[i].changes),
        }
    }
    for d in of_kind(&routines, DiffKind::Added)
        .into_iter()
        .chain(of_kind(&routines, DiffKind::Changed))
    {
        apply(&d.changes);
    }
    apply(&late_triggers);
    apply(&late_fks);
    // Drops: foreign keys, then views (readers first), routines, tables, sequences.
    let removed: Vec<&ObjectDiff> = active
        .iter()
        .copied()
        .filter(|d| d.kind == DiffKind::Removed)
        .collect();
    for d in &removed {
        let fks: Vec<SchemaChange> = d
            .changes
            .iter()
            .filter(|c| matches!(c, SchemaChange::DropConstraint { .. }))
            .cloned()
            .collect();
        apply(&fks);
    }
    for kinds in [
        &routines[..],
        &[ObjectKind::Table][..],
        &[ObjectKind::Sequence][..],
    ] {
        for d in of_kind(kinds, DiffKind::Removed) {
            apply(&d.changes[d.changes.len() - 1..]);
        }
    }

    let mut out = String::new();
    if !header.trim().is_empty() {
        out.push_str(header.trim_end());
        out.push('\n');
    }
    let warned = warnings(&active);
    if !warned.is_empty() {
        out.push_str("-- WARNING: destructive steps (review before running):\n");
        for w in warned {
            out.push_str(&format!("--   {w}\n"));
        }
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&script(&statements));
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::driver::DdlGen;
    use crate::error::DriverError;
    use crate::schema::{
        ColumnDef, ConstraintDef, FkAction, IndexDef, ObjectDef, ObjectKind, ObjectRef,
        SequenceDef, TableDef, ViewDef,
    };
    use crate::schema_change::SchemaChange;
    use crate::table::TableRef;

    /// Writes each change as one readable line, so a test can read the script's order.
    struct Lines;

    impl DdlGen for Lines {
        fn ddl(&self, changes: &[SchemaChange]) -> Result<Vec<String>, DriverError> {
            Ok(changes
                .iter()
                .map(|c| match c {
                    SchemaChange::Create(def) => {
                        let r = def.object_ref();
                        format!("CREATE {} {}", r.kind.keyword(), r.name)
                    }
                    SchemaChange::Drop { object, .. } => {
                        format!("DROP {} {}", object.kind.keyword(), object.name)
                    }
                    SchemaChange::AddColumn { table, column } => {
                        format!("ADD COLUMN {}.{}", table.name, column.name)
                    }
                    SchemaChange::DropColumn { table, name } => {
                        format!("DROP COLUMN {}.{name}", table.name)
                    }
                    SchemaChange::AlterColumn { table, after, .. } => {
                        format!(
                            "ALTER COLUMN {}.{} {}",
                            table.name, after.name, after.type_name
                        )
                    }
                    SchemaChange::AddConstraint { table, constraint } => match constraint {
                        ConstraintDef::ForeignKey { .. } => format!("ADD FK {}", table.name),
                        _ => format!("ADD CONSTRAINT {}", table.name),
                    },
                    SchemaChange::DropConstraint { table, name } => {
                        format!("DROP CONSTRAINT {}.{name}", table.name)
                    }
                    SchemaChange::ReplaceView(v) => format!("REPLACE VIEW {}", v.view.name),
                    other => format!("{other:?}"),
                })
                .collect())
        }

        fn create_ddl(&self, def: &ObjectDef) -> Result<String, DriverError> {
            let r = def.object_ref();
            Ok(format!("CREATE {} {};", r.kind.keyword(), r.name))
        }
    }

    fn t(schema: &str, name: &str) -> TableRef {
        TableRef {
            database: None,
            schema: Some(schema.into()),
            name: name.into(),
        }
    }

    fn col(name: &str, ty: &str) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            type_name: ty.into(),
            nullable: true,
            ..ColumnDef::default()
        }
    }

    fn table(schema: &str, name: &str, columns: &[(&str, &str)]) -> TableDef {
        let mut def = TableDef::new(t(schema, name));
        def.columns = columns.iter().map(|(n, ty)| col(n, ty)).collect();
        def
    }

    fn fk(name: &str, columns: &[&str], target: TableRef) -> ConstraintDef {
        ConstraintDef::ForeignKey {
            name: Some(name.into()),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            target,
            target_columns: vec!["id".into()],
            on_update: FkAction::NoAction,
            on_delete: FkAction::NoAction,
            deferrable: false,
            initially_deferred: false,
        }
    }

    fn view(schema: &str, name: &str, query: &str) -> ViewDef {
        ViewDef {
            view: t(schema, name),
            query: query.into(),
            materialized: false,
            columns: Vec::new(),
            comment: None,
            options: Vec::new(),
            indexes: Vec::new(),
        }
    }

    fn snapshot(schema: &str, objects: Vec<ObjectDef>) -> SchemaSnapshot {
        SchemaSnapshot {
            engine: "test".into(),
            schema: schema.into(),
            objects,
            dependencies: Vec::new(),
        }
    }

    fn names(diffs: &[ObjectDiff]) -> Vec<(DiffKind, String)> {
        diffs
            .iter()
            .map(|d| (d.kind, d.object.name.clone()))
            .collect()
    }

    #[test]
    fn compare_added_removed_changed() {
        let source = snapshot(
            "s",
            vec![
                ObjectDef::Table(table("s", "users", &[("id", "integer"), ("email", "text")])),
                ObjectDef::Table(table("s", "orders", &[("id", "integer")])),
                ObjectDef::Table(table("s", "same", &[("id", "integer")])),
            ],
        );
        let target = snapshot(
            "t",
            vec![
                ObjectDef::Table(table("t", "users", &[("id", "integer")])),
                ObjectDef::Table(table("t", "legacy", &[("id", "integer")])),
                ObjectDef::Table(table("t", "same", &[("id", "integer")])),
            ],
        );
        let diffs = compare(&source, &target, &Lines);
        // Schema names differ and do not matter; identical tables are not listed.
        assert_eq!(
            names(&diffs),
            [
                (DiffKind::Removed, "legacy".to_string()),
                (DiffKind::Added, "orders".to_string()),
                (DiffKind::Changed, "users".to_string()),
            ]
        );
        let users = diffs.iter().find(|d| d.object.name == "users").unwrap();
        assert_eq!(users.changes.len(), 1);
        assert!(
            matches!(&users.changes[0], SchemaChange::AddColumn { table, column }
            if table.schema.as_deref() == Some("t") && column.name == "email"),
            "the change applies on the target schema: {:?}",
            users.changes
        );
        assert!(!users.destructive);
        assert_eq!(users.source_ddl.as_deref(), Some("CREATE TABLE users;"));
        assert_eq!(users.target_ddl.as_deref(), Some("CREATE TABLE users;"));
        let legacy = diffs.iter().find(|d| d.object.name == "legacy").unwrap();
        assert!(legacy.destructive, "a drop is destructive");
        assert_eq!(legacy.source_ddl, None);
    }

    #[test]
    fn normalized_ddl_not_a_difference() {
        // The same table as two servers (or versions) print it.
        let mut a = table("s", "t", &[("id", "int4"), ("name", "varchar")]);
        a.columns[0].default = Some("nextval('t_id_seq'::regclass)".into());
        let mut b = table(
            "s",
            "t",
            &[("id", "integer"), ("name", "character varying")],
        );
        b.columns[0].default = Some("nextval('t_id_seq'::regclass)".into());
        b.columns[1].default = None;
        let va = view("s", "v", "SELECT  id,\n       name\n  FROM t");
        let vb = view("s", "v", "SELECT id, name FROM t");
        let source = snapshot("s", vec![ObjectDef::Table(a), ObjectDef::View(va)]);
        let target = snapshot("s", vec![ObjectDef::Table(b), ObjectDef::View(vb)]);
        assert_eq!(compare(&source, &target, &Lines), Vec::new());
    }

    #[test]
    fn qualifiers_follow_the_schema_rename() {
        // `a.t` in the source's view is `b.t` in the target's: the same object.
        let mut a = table("a", "t", &[("id", "integer")]);
        a.columns[0].default = Some("nextval('a.t_id_seq'::regclass)".into());
        let source = snapshot(
            "a",
            vec![
                ObjectDef::Table(a),
                ObjectDef::View(view("a", "v", "SELECT id FROM a.t WHERE note = 'a.x'")),
            ],
        );
        let mut b = table("b", "t", &[("id", "integer")]);
        b.columns[0].default = Some("nextval('b.t_id_seq'::regclass)".into());
        let target = snapshot(
            "b",
            vec![
                ObjectDef::Table(b),
                // The literal 'a.x' is data, not a qualifier: it stays.
                ObjectDef::View(view("b", "v", "SELECT id FROM b.t WHERE note = 'a.x'")),
            ],
        );
        assert_eq!(compare(&source, &target, &Lines), Vec::new());
        // A name that merely ends like the schema is left alone.
        assert_eq!(
            requalify("SELECT * FROM data.t, a.t", "a", "b"),
            "SELECT * FROM data.t, b.t"
        );
        assert_eq!(requalify("\"a\".t", "a", "b"), "\"b\".t");
        // MySQL writes every qualifier in backticks.
        assert_eq!(
            requalify("select `a`.`orders`.`id` from `a`.`orders`", "a", "b"),
            "select `b`.`orders`.`id` from `b`.`orders`"
        );
    }

    #[test]
    fn views_ordered_by_dependency() {
        // `top` reads `mid` reads the table `base`; names would sort the other way.
        let source = snapshot(
            "s",
            vec![
                ObjectDef::View(view("s", "top", "SELECT * FROM mid")),
                ObjectDef::View(view("s", "mid", "SELECT * FROM base")),
                ObjectDef::Table(table("s", "base", &[("id", "integer")])),
            ],
        );
        let target = snapshot("t", Vec::new());
        let diffs = compare(&source, &target, &Lines);
        let script = migration_script(&diffs, &HashSet::new(), &Lines, "");
        let at = |s: &str| script.find(s).unwrap_or_else(|| panic!("{s} in {script}"));
        assert!(at("CREATE TABLE base") < at("CREATE VIEW mid"), "{script}");
        assert!(at("CREATE VIEW mid") < at("CREATE VIEW top"), "{script}");

        // Dropped, the order reverses: dependents go first.
        let script = migration_script(
            &compare(&snapshot("s", Vec::new()), &source, &Lines),
            &HashSet::new(),
            &Lines,
            "",
        );
        let at = |s: &str| script.find(s).unwrap_or_else(|| panic!("{s} in {script}"));
        assert!(at("DROP VIEW top") < at("DROP VIEW mid"), "{script}");
        assert!(at("DROP VIEW mid") < at("DROP TABLE base"), "{script}");
    }

    #[test]
    fn views_leave_before_table_changes_and_return_after() {
        // `v` reads t.c, which the source drops: v must go before the column and come back
        // after it, as a create (a replace cannot drop columns of a view).
        let mut target_t = table("t", "t", &[("id", "integer"), ("c", "integer")]);
        target_t.columns[1].nullable = true;
        let target = snapshot(
            "t",
            vec![
                ObjectDef::Table(target_t),
                ObjectDef::View(view("t", "v", "SELECT id, c FROM t")),
                ObjectDef::View(view("t", "old", "SELECT id FROM t")),
            ],
        );
        let source = snapshot(
            "s",
            vec![
                ObjectDef::Table(table("s", "t", &[("id", "integer")])),
                ObjectDef::View(view("s", "v", "SELECT id FROM t")),
            ],
        );
        let script = migration_script(
            &compare(&source, &target, &Lines),
            &HashSet::new(),
            &Lines,
            "",
        );
        let at = |s: &str| script.find(s).unwrap_or_else(|| panic!("{s} in {script}"));
        assert!(at("DROP VIEW v") < at("DROP COLUMN t.c"), "{script}");
        assert!(at("DROP VIEW old") < at("DROP COLUMN t.c"), "{script}");
        assert!(at("DROP COLUMN t.c") < at("CREATE VIEW v"), "{script}");
        assert!(!script.contains("REPLACE VIEW"), "{script}");
    }

    #[test]
    fn circular_fks_ordered() {
        // a → b → a, and c → c (a self reference).
        let mut a = table("s", "a", &[("id", "integer"), ("b_id", "integer")]);
        a.constraints.push(fk("a_b", &["b_id"], t("s", "b")));
        let mut b = table("s", "b", &[("id", "integer"), ("a_id", "integer")]);
        b.constraints.push(fk("b_a", &["a_id"], t("s", "a")));
        let mut c = table("s", "c", &[("id", "integer"), ("parent", "integer")]);
        c.constraints.push(fk("c_c", &["parent"], t("s", "c")));
        let source = snapshot(
            "s",
            vec![
                ObjectDef::Table(a),
                ObjectDef::Table(b),
                ObjectDef::Table(c),
            ],
        );
        let created = migration_script(
            &compare(&source, &snapshot("t", Vec::new()), &Lines),
            &HashSet::new(),
            &Lines,
            "",
        );
        let last_create = created.rfind("CREATE TABLE").unwrap();
        let first_fk = created.find("ADD FK").unwrap();
        assert!(
            last_create < first_fk,
            "tables first, foreign keys last:\n{created}"
        );
        assert_eq!(created.matches("ADD FK").count(), 3);

        // Dropping the cycle drops its foreign keys first, then the tables.
        let dropped = migration_script(
            &compare(&snapshot("t", Vec::new()), &source, &Lines),
            &HashSet::new(),
            &Lines,
            "",
        );
        let last_fk = dropped.rfind("DROP CONSTRAINT").unwrap();
        let first_drop = dropped.find("DROP TABLE").unwrap();
        assert!(
            last_fk < first_drop,
            "constraints go before tables:\n{dropped}"
        );
        assert_eq!(dropped.matches("DROP TABLE").count(), 3);
    }

    #[test]
    fn excluded_objects_skipped() {
        let source = snapshot(
            "s",
            vec![
                ObjectDef::Table(table("s", "keep", &[("id", "integer")])),
                ObjectDef::Table(table("s", "skip", &[("id", "integer")])),
            ],
        );
        let diffs = compare(&source, &snapshot("t", Vec::new()), &Lines);
        let skip: ObjectRef = diffs
            .iter()
            .find(|d| d.object.name == "skip")
            .unwrap()
            .object
            .clone();
        let script = migration_script(&diffs, &HashSet::from([skip]), &Lines, "");
        assert!(script.contains("CREATE TABLE keep"), "{script}");
        assert!(!script.contains("CREATE TABLE skip"), "{script}");
    }

    #[test]
    fn script_header_marks_destructive() {
        let source = snapshot(
            "s",
            vec![ObjectDef::Table(table("s", "t", &[("id", "integer")]))],
        );
        let target = snapshot(
            "t",
            vec![
                ObjectDef::Table(table("t", "t", &[("id", "bigint"), ("old", "text")])),
                ObjectDef::Table(table("t", "gone", &[("id", "integer")])),
                ObjectDef::Sequence(SequenceDef {
                    sequence: t("t", "seq"),
                    data_type: None,
                    start: None,
                    increment: None,
                    min_value: None,
                    max_value: None,
                    cache: None,
                    cycle: false,
                    comment: None,
                }),
            ],
        );
        let diffs = compare(&source, &target, &Lines);
        let script = migration_script(&diffs, &HashSet::new(), &Lines, "-- migration to s");
        assert!(script.starts_with("-- migration to s\n"), "{script}");
        assert!(script.contains("-- WARNING: destructive steps"), "{script}");
        for what in [
            "drops table gone",
            "drops sequence seq",
            "drops column t.old",
        ] {
            assert!(script.contains(what), "{what} in {script}");
        }
        assert!(script.contains("alters column t.id"), "{script}");
        // The warnings come before the first statement.
        assert!(script.find("-- WARNING").unwrap() < script.find("DROP").unwrap());
    }

    #[test]
    fn the_same_snapshots_give_the_same_script() {
        let source = snapshot(
            "s",
            vec![
                ObjectDef::Table(table("s", "b", &[("id", "integer")])),
                ObjectDef::Table(table("s", "a", &[("id", "integer")])),
                ObjectDef::View(view("s", "v", "SELECT * FROM a")),
            ],
        );
        let target = snapshot("t", Vec::new());
        let one = migration_script(
            &compare(&source, &target, &Lines),
            &HashSet::new(),
            &Lines,
            "",
        );
        let two = migration_script(
            &compare(&source, &target, &Lines),
            &HashSet::new(),
            &Lines,
            "",
        );
        assert_eq!(one, two);
        assert!(one.find("CREATE TABLE a").unwrap() < one.find("CREATE TABLE b").unwrap());
        let _ = (IndexDef::default(), ObjectKind::Table);
    }
}
