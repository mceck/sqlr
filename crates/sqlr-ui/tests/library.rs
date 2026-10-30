use egui::{Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sqlr_core::{HistoryEntry, HistoryOutcome, HistoryQuery, Snippet};
use sqlr_ui::{
    DraftResult, HistoryAction, HistoryPanelState, SnippetAction, SnippetDraft, SnippetsState,
    format_duration, format_when, history_panel, snippet_dialog, snippets_panel,
};

fn double_click_at<S>(h: &mut Harness<'static, S>, pos: Pos2) {
    h.run_steps(100);
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        h.step();
    }
}

fn click_at<S>(h: &mut Harness<'static, S>, pos: Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(2);
}

fn conns() -> Vec<(String, String)> {
    vec![
        ("c1".to_string(), "local".to_string()),
        ("c2".to_string(), "prod".to_string()),
    ]
}

fn entry(id: i64, sql: &str, outcome: HistoryOutcome) -> HistoryEntry {
    HistoryEntry {
        id,
        conn_id: "c1".into(),
        database: None,
        sql: sql.into(),
        started_at_ms: 1_700_000_000_000,
        duration_ms: 850,
        outcome,
    }
}

struct HistoryApp {
    state: HistoryPanelState,
    conns: Vec<(String, String)>,
    entries: Option<Vec<HistoryEntry>>,
    actions: Vec<HistoryAction>,
}

fn history_harness(entries: Option<Vec<HistoryEntry>>) -> Harness<'static, HistoryApp> {
    let app = HistoryApp {
        state: HistoryPanelState::default(),
        conns: conns(),
        entries,
        actions: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(900.0, 400.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut HistoryApp| {
                let a = history_panel(ui, &mut app.state, &app.conns, app.entries.as_deref());
                app.actions.extend(a);
            },
            app,
        );
    h.run_steps(2);
    h
}

fn searches(h: &Harness<'static, HistoryApp>) -> Vec<HistoryQuery> {
    h.state()
        .actions
        .iter()
        .filter_map(|a| match a {
            HistoryAction::Search(q) => Some(q.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn history_emits_search_on_first_frame_and_on_typing() {
    let mut h = history_harness(None);
    assert_eq!(
        searches(&h),
        vec![HistoryQuery {
            text: String::new(),
            conn_id: None,
            limit: 200,
        }]
    );
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_steps(2);
    h.event(egui::Event::Text("ord".into()));
    h.run_steps(2);
    let last = searches(&h).pop().unwrap();
    assert_eq!(last.text, "ord");
    assert_eq!(last.limit, 200);
    let count = searches(&h).len();
    h.run_steps(5);
    assert_eq!(searches(&h).len(), count, "no search without a change");

    h.state_mut().state.conn_filter = Some("c2".into());
    h.run_steps(2);
    let last = searches(&h).pop().unwrap();
    assert_eq!(last.conn_id.as_deref(), Some("c2"));
    assert_eq!(last.text, "ord");
}

#[test]
fn history_shows_loading_and_entries() {
    let h = history_harness(None);
    h.get_by_label("Loading…");
    let h = history_harness(Some(vec![entry(1, "select 1", HistoryOutcome::Rows(1))]));
    h.get_by_label("select 1");
    h.get_by_label("local");
    h.get_by_label("850 ms");
    h.get_by_label(&format_when(1_700_000_000_000));
}

#[test]
fn history_double_click_opens() {
    let e = entry(7, "select *\nfrom orders", HistoryOutcome::Rows(500));
    let mut h = history_harness(Some(vec![e.clone()]));
    let pos = h.get_by_label("select *").rect().center();
    double_click_at(&mut h, pos);
    assert!(h.state().actions.contains(&HistoryAction::Open(e)));
}

#[test]
fn history_context_menu_inserts() {
    let e = entry(7, "select 42", HistoryOutcome::Affected(3));
    let mut h = history_harness(Some(vec![e.clone()]));
    h.get_by_label("select 42").click_secondary();
    h.run_steps(2);
    h.get_by_label("Open in new tab");
    h.get_by_label("Copy SQL");
    h.get_by_label("Insert into editor").click();
    h.run_steps(2);
    assert!(h.state().actions.contains(&HistoryAction::Insert(e)));
}

#[test]
fn history_entry_actions() {
    let e = entry(7, "select 42", HistoryOutcome::Affected(3));
    for (item, expected) in [
        ("Open in new tab", HistoryAction::Open(e.clone())),
        ("Insert into editor", HistoryAction::Insert(e.clone())),
        ("Copy SQL", HistoryAction::Copy(e.clone())),
    ] {
        let mut h = history_harness(Some(vec![e.clone()]));
        h.get_by_label("select 42").click_secondary();
        h.run_steps(2);
        h.get_by_label(item).click();
        h.run_steps(2);
        assert!(
            h.state().actions.contains(&expected),
            "{item}: {:?}",
            h.state().actions
        );
    }
}

#[test]
fn history_shows_outcome_badges() {
    let h = history_harness(Some(vec![
        entry(1, "select a", HistoryOutcome::Rows(500)),
        entry(2, "update b", HistoryOutcome::Affected(3)),
        entry(3, "select c", HistoryOutcome::Failed("boom".into())),
        entry(4, "select d", HistoryOutcome::Cancelled),
    ]));
    h.get_by_label("500 rows");
    h.get_by_label("3 affected");
    h.get_by_label("failed");
    h.get_by_label("cancelled");
}

#[test]
fn format_duration_values() {
    assert_eq!(format_duration(850), "850 ms");
    assert_eq!(format_duration(1200), "1.2 s");
    assert_eq!(format_duration(125_000), "2 m 05 s");
}

#[test]
fn format_when_is_local_seconds() {
    let s = format_when(1_700_000_000_000);
    assert_eq!(s.len(), "YYYY-MM-DD HH:MM:SS".len(), "{s}");
    assert!(s.starts_with("2023-11-1"), "{s}");
}

fn snippet(id: &str, name: &str, folder: &str, conn: Option<&str>) -> Snippet {
    Snippet {
        id: id.into(),
        name: name.into(),
        folder: folder.into(),
        conn_id: conn.map(str::to_string),
        sql: format!("select '{name}'"),
        updated_at_ms: 0,
    }
}

struct SnippetsApp {
    state: SnippetsState,
    snippets: Vec<Snippet>,
    actions: Vec<SnippetAction>,
}

fn snippets_harness(snippets: Vec<Snippet>) -> Harness<'static, SnippetsApp> {
    let app = SnippetsApp {
        state: SnippetsState::default(),
        snippets,
        actions: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(400.0, 500.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut SnippetsApp| {
                let a = snippets_panel(ui, &mut app.state, &app.snippets, Some("c1"));
                app.actions.extend(a);
            },
            app,
        );
    h.run_steps(2);
    h
}

fn scoped_snippets() -> Vec<Snippet> {
    vec![
        snippet("s1", "one", "a/b", None),
        snippet("s2", "two", "", Some("c1")),
        snippet("s3", "three", "", Some("c2")),
    ]
}

#[test]
fn snippets_scope_and_folders() {
    let mut h = snippets_harness(scoped_snippets());
    h.get_by_label("two");
    assert!(h.query_by_label("three").is_none());
    assert!(h.query_by_label("one").is_none(), "folders start collapsed");
    h.get_by_label("a").click();
    h.run_steps(2);
    h.get_by_label("b").click();
    h.run_steps(2);
    h.get_by_label("one");
    assert!(h.query_by_label("three").is_none());
}

#[test]
fn snippets_filter_by_name() {
    let mut h = snippets_harness(scoped_snippets());
    h.state_mut().state.filter = "ON".into();
    h.run_steps(2);
    // A filter opens the folders holding a match.
    h.get_by_label("one");
    assert!(h.query_by_label("two").is_none());
}

#[test]
fn same_named_folders_under_different_parents_toggle_independently() {
    let snippets = vec![
        snippet("s1", "one", "a/reports", None),
        snippet("s2", "two", "b/reports", None),
    ];
    let mut h = snippets_harness(snippets);
    h.get_by_label("a").click();
    h.run_steps(2);
    h.get_by_label("b").click();
    h.run_steps(2);
    let first = h
        .get_all_by_label("reports")
        .next()
        .expect("a reports folder")
        .rect()
        .center();
    click_at(&mut h, first);
    h.get_by_label("one");
    // The two folders are separate rows: opening the second leaves the first open.
    let second = h
        .get_all_by_label("reports")
        .nth(1)
        .expect("the second reports folder")
        .rect()
        .center();
    click_at(&mut h, second);
    h.get_by_label("two");
    // Collapsing the first reports folder leaves the second one open.
    let first = h
        .get_all_by_label("reports")
        .next()
        .expect("a reports folder")
        .rect()
        .center();
    click_at(&mut h, first);
    assert!(h.query_by_label("one").is_none());
    h.get_by_label("two");
}

#[test]
fn snippet_double_click_inserts() {
    let mut h = snippets_harness(scoped_snippets());
    let pos = h.get_by_label("two").rect().center();
    double_click_at(&mut h, pos);
    assert!(
        h.state()
            .actions
            .contains(&SnippetAction::Insert("select 'two'".into()))
    );
}

#[test]
fn snippet_context_menu_and_new() {
    let all = scoped_snippets();
    let mut h = snippets_harness(all);
    h.get_by_label("two").click_secondary();
    h.run_steps(2);
    h.get_by_label("Insert");
    h.get_by_label("Open in new tab");
    h.get_by_label("Delete");
    h.get_by_label("Edit…").click();
    h.run_steps(2);
    assert!(
        h.state()
            .actions
            .contains(&SnippetAction::Edit(SnippetDraft {
                id: Some("s2".into()),
                name: "two".into(),
                folder: String::new(),
                conn_id: Some("c1".into()),
                sql: "select 'two'".into(),
            }))
    );
    h.get_by_label("New snippet").click();
    h.run_steps(2);
    assert!(
        h.state()
            .actions
            .contains(&SnippetAction::Edit(SnippetDraft::default()))
    );
}

struct DialogApp {
    draft: SnippetDraft,
    results: Vec<DraftResult>,
}

fn dialog_harness(draft: SnippetDraft) -> Harness<'static, DialogApp> {
    let app = DialogApp {
        draft,
        results: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(800.0, 600.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut DialogApp| {
                let ctx = ui.ctx().clone();
                let r = snippet_dialog(&ctx, &mut app.draft, &conns());
                app.results.push(r);
            },
            app,
        );
    h.run_steps(2);
    h
}

#[test]
fn snippet_dialog_save_disabled_without_name() {
    let mut h = dialog_harness(SnippetDraft {
        sql: "select 1".into(),
        ..Default::default()
    });
    assert!(h.get_by_label("Save").accesskit_node().is_disabled());
    assert_eq!(h.state().results.last(), Some(&DraftResult::Open));
    h.state_mut().draft.name = "mine".into();
    h.run_steps(2);
    assert!(!h.get_by_label("Save").accesskit_node().is_disabled());
    h.get_by_label("Save").click();
    h.run_steps(1);
    assert!(h.state().results.contains(&DraftResult::Save));
}

#[test]
fn snippet_dialog_cancel() {
    let mut h = dialog_harness(SnippetDraft::default());
    h.get_by_label("Cancel").click();
    h.run_steps(1);
    assert!(h.state().results.contains(&DraftResult::Cancel));
}

#[test]
fn snippet_dialog_focuses_name() {
    let mut h = dialog_harness(SnippetDraft::default());
    h.event(egui::Event::Text("mine".into()));
    h.run_steps(2);
    assert_eq!(h.state().draft.name, "mine");
}

#[test]
fn snippet_dialog_selects_a_suggested_name() {
    let mut h = dialog_harness(SnippetDraft {
        name: "select 42".into(),
        sql: "select 42".into(),
        ..Default::default()
    });
    h.event(egui::Event::Text("mine".into()));
    h.run_steps(2);
    assert_eq!(h.state().draft.name, "mine");
}

#[test]
fn empty_snippets_can_be_created_from_the_empty_state() {
    let mut h = snippets_harness(Vec::new());
    h.get_by_label("New snippet").click();
    h.run_steps(2);
    assert!(
        h.state()
            .actions
            .contains(&SnippetAction::Edit(SnippetDraft::default()))
    );
}

#[test]
fn empty_history_does_not_offer_a_destructive_action() {
    let h = history_harness(Some(Vec::new()));
    assert!(h.query_by_label("Clear history").is_none());
}
