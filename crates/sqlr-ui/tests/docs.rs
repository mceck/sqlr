//! The document widgets: the Table view (the grid over documents), the tree and the JSON
//! view.

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_core::{Cell, DocChunk, DocValue};
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{
    DocAction, DocTableState, DocTreeState, JsonViewState, doc_table, doc_tree, json_view,
};

fn doc(fields: Vec<(&str, DocValue)>) -> DocValue {
    DocValue::Document(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
}

fn sample() -> Vec<DocChunk> {
    vec![
        DocChunk::from_docs(&[doc(vec![
            ("_id", DocValue::Int32(1)),
            ("name", DocValue::String("ann".into())),
            (
                "address",
                doc(vec![
                    ("city", DocValue::String("Rome".into())),
                    ("geo", doc(vec![("lat", DocValue::Double(41.9))])),
                ]),
            ),
        ])]),
        DocChunk::from_docs(&[doc(vec![
            ("email", DocValue::String("b@x.io".into())),
            ("_id", DocValue::Int32(2)),
        ])]),
    ]
}

#[test]
fn doc_table_union_of_keys() {
    let chunks = sample();
    let mut state = DocTableState::default();
    let rows = state.rows(&chunks, 500);
    let names: Vec<&str> = rows.columns().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        ["_id", "name", "address", "email"],
        "first-seen order"
    );
    assert_eq!(rows.row_count(), 2);
    assert_eq!(
        rows.cell(0, 2),
        Some(Cell::Text("{\"city\": \"Rome\", \"geo\": {\"lat\": 41.9}}"))
    );
    assert_eq!(rows.cell(1, 1), Some(Cell::Null), "a missing key is NULL");
    assert_eq!(rows.cell(1, 0), Some(Cell::Int(2)));
    // A later page appends rows under the same columns.
    let mut more = chunks.clone();
    more.push(DocChunk::from_docs(&[doc(vec![
        ("_id", DocValue::Int32(3)),
        ("extra", DocValue::Bool(true)),
    ])]));
    let rows = state.rows(&more, 500);
    assert_eq!(rows.row_count(), 3);
    assert_eq!(rows.columns().len(), 4, "only the first page names columns");

    // Drawn, a click on a row selects its document.
    struct App {
        state: DocTableState,
        chunks: Vec<DocChunk>,
        actions: Vec<DocAction>,
    }
    let mut h = Harness::builder()
        .with_size(vec2(800.0, 400.0))
        .build_ui_state(
            |ui, app: &mut App| {
                if let Some(a) = doc_table(ui, &mut app.state, &app.chunks, 500, false) {
                    app.actions.push(a);
                }
            },
            App {
                state: DocTableState::default(),
                chunks: sample(),
                actions: Vec::new(),
            },
        );
    h.run_steps(2);
    h.state_mut().state.grid.selection = sqlr_ui::Selection::cell(1, 0);
    h.state_mut().state.grid.selected = Some((1, 0));
    h.run_steps(2);
    assert!(h.state().actions.contains(&DocAction::Select(1)));
}

struct Tree {
    state: DocTreeState,
    chunks: Vec<DocChunk>,
    actions: Vec<DocAction>,
}

#[test]
fn doc_tree_lazy_expand() {
    let mut h = Harness::builder()
        .with_size(vec2(700.0, 500.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut Tree| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                if let Some(a) = doc_tree(ui, &mut app.state, &app.chunks, true) {
                    app.actions.push(a);
                }
            },
            Tree {
                state: DocTreeState::default(),
                chunks: sample(),
                actions: Vec::new(),
            },
        );
    h.run_steps(2);
    // Documents only: nothing nested is decoded or shown before it is expanded.
    h.get_by_label("Expand document 1");
    h.get_by_label("Expand document 2");
    assert!(h.query_by_label("city").is_none());
    assert_eq!(h.state().state.decoded(), 0);
    h.get_by_label("Expand document 1").click();
    h.run_steps(2);
    h.get_by_label("name");
    h.get_by_label("address");
    assert!(
        h.query_by_label("city").is_none(),
        "address stays collapsed"
    );
    assert_eq!(h.state().state.decoded(), 1);
    h.get_by_label("Expand address").click();
    h.run_steps(2);
    h.get_by_label("city");
    assert!(h.query_by_label("lat").is_none(), "one level at a time");
    // Selecting a field names it for the value panel.
    h.get_by_label("city").click();
    h.run_steps(2);
    let (path, value) = h.state().state.selected_field().unwrap();
    assert_eq!(path, "address.city");
    assert_eq!(value, DocValue::String("Rome".into()));
    assert!(h.state().actions.contains(&DocAction::Select(0)));
    h.get_by_label("Collapse document 1").click();
    h.run_steps(2);
    assert!(h.query_by_label("city").is_none());
}

struct Json {
    state: JsonViewState,
    doc: DocValue,
    actions: Vec<DocAction>,
}

fn json_harness(doc: DocValue) -> Harness<'static, Json> {
    let mut h = Harness::builder()
        .with_size(vec2(700.0, 500.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut Json| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                if let Some(a) = json_view(ui, &mut app.state, 0, &app.doc, true) {
                    app.actions.push(a);
                }
            },
            Json {
                state: JsonViewState::default(),
                doc,
                actions: Vec::new(),
            },
        );
    h.run_steps(2);
    h
}

#[test]
fn json_view_validates_before_save() {
    let original = doc(vec![
        ("_id", DocValue::ObjectId([7; 12])),
        ("n", DocValue::Int64(5)),
    ]);
    let mut h = json_harness(original);
    h.get_by_label("Edit document").click();
    h.run_steps(2);
    assert!(h.state().state.editing());
    assert!(
        h.state().state.edit_text().unwrap().contains("$oid"),
        "the editor starts from the relaxed JSON"
    );
    h.state_mut()
        .state
        .set_edit_text("{\"_id\": 1,\n  \"n\": }");
    h.run_steps(1);
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert!(h.state().actions.is_empty(), "invalid JSON is never saved");
    let error = h.state().state.error().unwrap().to_string();
    assert!(error.contains("line 2"), "{error}");
    let fixed = "{\"_id\": {\"$oid\": \"070707070707070707070707\"}, \"n\": NumberLong(6)}";
    h.state_mut().state.set_edit_text(fixed);
    h.run_steps(1);
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        [DocAction::Save(
            0,
            doc(vec![
                ("_id", DocValue::ObjectId([7; 12])),
                ("n", DocValue::Int64(6)),
            ])
        )]
    );
    // The editor stays open until the caller knows the save worked (a failed save keeps
    // the text).
    assert!(h.state().state.editing());
    h.state_mut().state.cancel();
    h.run_steps(1);
    assert!(!h.state().state.editing());
}

#[test]
fn json_view_refuses_to_edit_unmodelled_types() {
    let code = doc(vec![
        ("_id", DocValue::Int32(1)),
        ("f", doc(vec![("$code", DocValue::String("x".into()))])),
    ]);
    let mut h = json_harness(code);
    h.get_by_label("Edit document").click();
    h.run_steps(2);
    assert!(!h.state().state.editing());
    h.get_by_label_contains("JavaScript code");
}

#[test]
fn documents_without_id_offer_no_edit_or_delete() {
    let chunks = vec![DocChunk::from_docs(&[
        doc(vec![("_id", DocValue::Int32(1))]),
        doc(vec![("n", DocValue::Int32(2))]),
    ])];
    let mut h = Harness::builder()
        .with_size(vec2(700.0, 500.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut Tree| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                if let Some(a) = doc_tree(ui, &mut app.state, &app.chunks, true) {
                    app.actions.push(a);
                }
            },
            Tree {
                state: DocTreeState::default(),
                chunks,
                actions: Vec::new(),
            },
        );
    h.run_steps(2);
    h.get_by_label_contains("(1)").click_secondary();
    h.run_steps(3);
    h.get_by_label("Edit document");
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    h.get_by_label_contains("(2)").click_secondary();
    h.run_steps(3);
    h.get_by_label("Copy JSON");
    assert!(h.query_by_label("Edit document").is_none());
    assert!(h.query_by_label("Delete document…").is_none());
}

#[test]
fn the_table_copy_of_documents_has_a_budget() {
    let chunks: Vec<DocChunk> = (0..10)
        .map(|c| {
            DocChunk::from_docs(
                &(0..100)
                    .map(|i| {
                        doc(vec![
                            ("_id", DocValue::Int32(c * 100 + i)),
                            ("text", DocValue::String("x".repeat(1_000))),
                        ])
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let mut state = DocTableState::with_budget(300_000);
    let rows = state.rows(&chunks, 500).row_count();
    assert!(rows < 1_000, "{rows} rows copied");
    assert!(rows >= 100, "at least the first page");
    assert!(state.truncated());
    let mut all = DocTableState::default();
    assert_eq!(all.rows(&chunks, 500).row_count(), 1_000);
    assert!(!all.truncated());
}
