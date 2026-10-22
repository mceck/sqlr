//! Object editor widgets.

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use sqlr_core::{
    ColumnDef, ConstraintDef, ObjectDef, ObjectEditing, ObjectKind, SchemaChange, SqlSyntax,
    TableDef, TableRef,
};
use sqlr_ui::{
    ObjectEditorAction, ObjectEditorContext, ObjectEditorState, ObjectSubTab, object_editor,
};

fn t(name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some("public".into()),
        name: name.into(),
    }
}

fn column(name: &str) -> ColumnDef {
    ColumnDef {
        name: name.into(),
        type_name: "integer".into(),
        nullable: true,
        ..Default::default()
    }
}

fn orders(columns: &[&str]) -> TableDef {
    let mut def = TableDef::new(t("orders"));
    def.columns = columns.iter().map(|c| column(c)).collect();
    def.constraints.push(ConstraintDef::PrimaryKey {
        name: Some("orders_pkey".into()),
        columns: vec!["id".into()],
    });
    def
}

fn editing(column_order: bool) -> ObjectEditing {
    ObjectEditing {
        create: vec![ObjectKind::Table],
        alter: vec![ObjectKind::Table],
        drop: vec![ObjectKind::Table],
        rename: vec![ObjectKind::Table],
        transactional_ddl: true,
        column_types: vec!["integer".into(), "text".into()],
        comments: true,
        column_order,
    }
}

struct App {
    state: ObjectEditorState,
    editing: ObjectEditing,
    tables: Vec<(TableRef, Vec<String>)>,
    actions: Vec<ObjectEditorAction>,
}

fn harness(def: TableDef, editing: ObjectEditing) -> Harness<'static, App> {
    let app = App {
        state: ObjectEditorState::new(ObjectDef::Table(def)),
        editing,
        tables: vec![
            (t("customers"), vec!["id".into(), "name".into()]),
            (t("orders"), vec!["id".into(), "customer".into()]),
        ],
        actions: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(1300.0, 800.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                let ctx = ObjectEditorContext {
                    tables: &app.tables,
                    syntax: SqlSyntax::ansi(),
                    current_ddl: Some("CREATE TABLE orders ();"),
                    pending_ddl: None,
                    busy: None,
                    apply_blocked: None,
                    message: None,
                    stats: None,
                };
                if let Some(a) = object_editor(ui, &mut app.state, &app.editing, &ctx) {
                    app.actions.push(a);
                }
            },
            app,
        );
    h.run_steps(2);
    h
}

/// The text field showing `value`.
fn field(value: &str) -> By<'_> {
    By::new().role(Role::TextInput).value(value)
}

/// Clicks the text field showing `value`, selects its text and types `text` over it.
fn retype(h: &mut Harness<'static, App>, value: &str, text: &str) {
    h.get(field(value)).click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text(text.into()));
    h.run_steps(2);
}

fn pending(h: &mut Harness<'static, App>) -> usize {
    h.state_mut().state.pending()
}

#[test]
fn column_edit_produces_alter_change() {
    let mut h = harness(orders(&["id", "total"]), editing(true));
    assert_eq!(pending(&mut h), 0);
    h.get_by_label("No pending changes");
    retype(&mut h, "total", "amount");
    assert_eq!(pending(&mut h), 1);
    let changes = h.state_mut().state.changes().to_vec();
    assert!(
        matches!(&changes[..], [SchemaChange::AlterColumn { before, after, .. }]
            if before.name == "total" && after.name == "amount"),
        "{changes:?}"
    );
    h.run_steps(2);
    h.get_by_label("1 pending change");
    // Typing the old name back leaves nothing pending.
    retype(&mut h, "amount", "total");
    assert_eq!(pending(&mut h), 0);
    h.run_steps(2);
    h.get_by_label("No pending changes");
    assert!(h.get_by_label("Apply").accesskit_node().is_disabled());
}

#[test]
fn add_fk_with_picker() {
    let mut h = harness(orders(&["id", "customer"]), editing(true));
    h.get_by_label("Foreign keys").click();
    h.run_steps(2);
    h.get_by_label("Add foreign key").click();
    h.run_steps(2);
    // A new key starts from the first column outside the primary key, without a target.
    h.get(By::new().role(Role::ComboBox).value("Choose a table…"))
        .click();
    h.run_steps(2);
    h.get_by_label("customers").click();
    h.run_steps(3);
    let ObjectDef::Table(after) = &h.state().state.after else {
        unreachable!()
    };
    let fk = after
        .constraints
        .iter()
        .find(|c| c.is_foreign_key())
        .expect("a foreign key");
    match fk {
        ConstraintDef::ForeignKey {
            columns,
            target,
            target_columns,
            ..
        } => {
            assert_eq!(columns, &["customer"]);
            assert_eq!(target, &t("customers"));
            assert_eq!(target_columns, &["id"]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(pending(&mut h), 1);
    assert!(
        h.state_mut().state.problems(&[]).iter().all(|p| !p.error),
        "{:?}",
        h.state_mut().state.problems(&[])
    );
}

#[test]
fn column_editor_thousand_columns() {
    let names: Vec<String> = (0..1000).map(|i| format!("c{i}")).collect();
    let refs: Vec<&str> = std::iter::once("id")
        .chain(names.iter().map(String::as_str))
        .collect();
    let mut h = harness(orders(&refs), editing(true));
    h.get_by_label("1001 columns");
    // Only the visible rows are built.
    let built = h.query_all(field("c10")).count() + h.query_all(field("c900")).count();
    assert_eq!(built, 1, "c10 is visible, c900 is not");
    h.state_mut().state.scroll_to_column = Some(901);
    h.run_steps(3);
    assert!(h.query(field("c10")).is_none());
    retype(&mut h, "c900", "renamed");
    let ObjectDef::Table(after) = &h.state().state.after else {
        unreachable!()
    };
    assert_eq!(after.columns[901].name, "renamed");
    assert_eq!(pending(&mut h), 1);
}

#[test]
fn pg_disables_column_reorder() {
    let mut h = harness(orders(&["id", "a", "b"]), editing(false));
    // Select row 2 through its number.
    h.get_by_label("2").click();
    h.run_steps(2);
    assert!(h.get_by_label("Move up").accesskit_node().is_disabled());
    assert!(h.get_by_label("Move down").accesskit_node().is_disabled());
    // Where the database keeps an order, the row moves and the reorder is pending.
    let mut h = harness(orders(&["id", "a", "b"]), editing(true));
    h.get_by_label("2").click();
    h.run_steps(2);
    assert!(!h.get_by_label("Move down").accesskit_node().is_disabled());
    h.get_by_label("Move down").click();
    h.run_steps(2);
    let ObjectDef::Table(after) = &h.state().state.after else {
        unreachable!()
    };
    let names: Vec<&str> = after.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "b", "a"]);
    assert_eq!(pending(&mut h), 1);
}

#[test]
fn new_object_counts_its_creation_and_validates() {
    let mut def = TableDef::new(t("fresh"));
    def.columns.clear();
    let mut state = ObjectEditorState::new_object(ObjectDef::Table(def));
    assert!(state.is_new());
    assert_eq!(state.pending(), 1);
    assert!(matches!(state.changes(), [SchemaChange::Create(_)]));
    let problems = state.problems(&[]);
    assert!(
        problems
            .iter()
            .any(|p| p.error && p.text.contains("no column"))
    );
    assert_eq!(state.sub_tabs()[0], ObjectSubTab::Columns);
    assert!(!state.sub_tabs().contains(&ObjectSubTab::Data));
    // Once created and loaded again, nothing is pending (the count follows `before` too).
    let created = state.after.clone();
    state.reload(created);
    assert!(!state.is_new());
    assert_eq!(state.pending(), 0);
}

#[test]
fn toolbar_actions() {
    let mut h = harness(orders(&["id", "total"]), editing(true));
    retype(&mut h, "total", "amount");
    h.get_by_label("Preview DDL").click();
    h.run_steps(2);
    h.get_by_label("Apply").click();
    h.run_steps(2);
    h.get_by_label("Revert").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        [
            ObjectEditorAction::Preview,
            ObjectEditorAction::Apply,
            ObjectEditorAction::Revert
        ]
    );
    assert_eq!(pending(&mut h), 0);
}

#[test]
fn data_is_asked_for_once() {
    let mut h = harness(orders(&["id", "total"]), editing(true));
    h.get_by_label("Data").click();
    h.run_steps(5);
    let asked = |h: &Harness<'static, App>| {
        h.state()
            .actions
            .iter()
            .filter(|a| **a == ObjectEditorAction::LoadData)
            .count()
    };
    assert_eq!(asked(&h), 1);
    // The caller draws the page below the editor (the result tab's grid and changes bar).
    assert!(h.state().state.data_shown());
    assert!(h.query_by_label("Loading…").is_none());
    // A reload (Refresh, an apply) asks again.
    let def = h.state().state.after.clone();
    h.state_mut().state.reload(def);
    h.run_steps(3);
    assert_eq!(asked(&h), 2);
}

#[test]
fn problems_of_many_columns_are_cheap() {
    let mut def = TableDef::new(t("wide"));
    def.columns = (0..20_000).map(|i| column(&format!("c{i}"))).collect();
    def.columns.push(column("c7"));
    let mut state = ObjectEditorState::new(ObjectDef::Table(def));
    let started = std::time::Instant::now();
    for _ in 0..10 {
        let problems = state.problems(&[]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].text.contains("\"c7\" appears twice"));
    }
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
}
