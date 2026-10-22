use std::collections::HashSet;

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_core::{DiffKind, ObjectDiff, ObjectKind, ObjectRef};
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{DiffRowKind, diff_rows, diff_tree, text_diff};

fn diff(name: &str, kind: DiffKind, object_kind: ObjectKind, destructive: bool) -> ObjectDiff {
    ObjectDiff {
        object: ObjectRef {
            kind: object_kind,
            database: None,
            schema: Some("main".into()),
            name: name.into(),
            parent: None,
            signature: None,
        },
        kind,
        source_ddl: None,
        target_ddl: None,
        changes: Vec::new(),
        destructive,
        source_def: None,
        target_def: None,
        deps: Vec::new(),
    }
}

struct App {
    diffs: Vec<ObjectDiff>,
    excluded: HashSet<ObjectRef>,
    selected: Option<usize>,
}

fn harness() -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(vec2(500.0, 400.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                diff_tree(ui, &app.diffs, &mut app.excluded, &mut app.selected);
            },
            App {
                diffs: vec![
                    diff("fresh", DiffKind::Added, ObjectKind::Table, false),
                    diff("legacy", DiffKind::Removed, ObjectKind::Table, true),
                    diff("users", DiffKind::Changed, ObjectKind::Table, false),
                    diff("v_big", DiffKind::Added, ObjectKind::View, false),
                ],
                excluded: HashSet::new(),
                selected: None,
            },
        );
    h.run_steps(3);
    h
}

#[test]
fn diff_tree_filters_and_excludes() {
    let mut h = harness();
    for name in ["fresh", "legacy", "users", "v_big"] {
        h.get_by_label(name);
    }
    // Clicking a name selects its diff; clicking its checkbox excludes the object.
    h.get_by_label("users").click();
    h.run_steps(2);
    assert_eq!(h.state().selected, Some(2));
    h.get_by_label("Include fresh").click();
    h.run_steps(2);
    assert_eq!(h.state().excluded.len(), 1);
    assert!(h.state().excluded.iter().any(|o| o.name == "fresh"));
    h.get_by_label("Include fresh").click();
    h.run_steps(2);
    assert!(h.state().excluded.is_empty());

    // A destructive step is flagged.
    h.get_by_label("Destructive: legacy");

    // The status filter narrows the list.
    h.get_by_label("Removed").click();
    h.run_steps(2);
    assert!(h.query_by_label("fresh").is_none());
    h.get_by_label("legacy");
    h.get_by_label("All").click();
    h.run_steps(2);
    h.get_by_label("fresh");

    // The text filter matches names.
    h.get_by_label("Views").click();
    h.run_steps(2);
    assert!(h.query_by_label("users").is_none());
    h.get_by_label("v_big");
}

#[test]
fn text_diff_marks_changed_lines() {
    let left = "CREATE TABLE t (\n  id int,\n  name text\n);\n";
    let right = "CREATE TABLE t (\n  id bigint,\n  name text,\n  extra int\n);\n";
    let rows = diff_rows(left, right);
    let kinds: Vec<DiffRowKind> = rows.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        [
            DiffRowKind::Same,    // CREATE TABLE t (
            DiffRowKind::Changed, // id int -> id bigint
            DiffRowKind::Changed, // name text -> name text,
            DiffRowKind::Added,   // extra int
            DiffRowKind::Same,    // );
        ]
    );
    assert_eq!(
        rows[1].left.as_ref().unwrap().0,
        2,
        "line numbers start at 1"
    );
    assert_eq!(rows[1].right.as_ref().unwrap().1, "  id bigint,");
    assert!(rows[3].left.is_none() && rows[3].right.is_some());

    // The widget draws both sides.
    let mut h = Harness::builder()
        .with_size(vec2(700.0, 300.0))
        .build_ui(|ui| {
            theme::apply(ui.ctx(), &Palette::dark(), 13.0);
            text_diff(ui, left, right);
        });
    h.run_steps(2);
    h.get_by_label_contains("extra int");
    h.get_by_label_contains("id int");
}
