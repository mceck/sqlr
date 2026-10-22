use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_core::{PlanTree, parse_pg_json};
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{PlanAction, PlanViewState, plan_view};

const PLAN: &str = r#"[{"Plan": {"Node Type": "Nested Loop", "Startup Cost": 0.0, "Total Cost": 30.0,
  "Plan Rows": 10, "Actual Rows": 8, "Actual Loops": 1, "Actual Total Time": 5.0,
  "Plans": [
    {"Node Type": "Seq Scan", "Relation Name": "big", "Startup Cost": 0.0, "Total Cost": 20.0,
     "Plan Rows": 100, "Actual Rows": 1500, "Actual Loops": 1, "Actual Total Time": 3.0,
     "Filter": "(x > 1)"},
    {"Node Type": "Index Scan", "Relation Name": "small", "Startup Cost": 0.1, "Total Cost": 0.5,
     "Plan Rows": 1, "Actual Rows": 1, "Actual Loops": 4, "Actual Total Time": 0.25}]},
  "Planning Time": 0.3, "Execution Time": 5.5}]"#;

struct App {
    plan: PlanTree,
    state: PlanViewState,
    actions: Vec<PlanAction>,
}

fn harness() -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(vec2(800.0, 500.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                if let Some(a) = plan_view(ui, &app.plan, &mut app.state) {
                    app.actions.push(a);
                }
            },
            App {
                plan: parse_pg_json(PLAN).unwrap(),
                state: PlanViewState::default(),
                actions: Vec::new(),
            },
        );
    h.run_steps(3);
    h
}

#[test]
fn plan_view_highlights_hottest() {
    let mut h = harness();
    // All three nodes are among the top three: each is marked, the hottest first.
    h.get_by_label("Hottest 1: Seq Scan");
    h.get_by_label_contains("Hottest 2");
    h.get_by_label_contains("Hottest 3");
    // The scan read 1500 rows where 100 were expected: the ratio badge says so.
    h.get_by_label_contains("15× more rows than estimated");
    // Selecting a node shows its details.
    h.get_by_label("Seq Scan").click();
    h.run_steps(2);
    assert_eq!(h.state().state.selected, Some(vec![0]));
    h.get_by_label_contains("Filter: (x > 1)");
}

#[test]
fn plan_view_shows_and_copies_the_raw_plan() {
    let mut h = harness();
    h.get_by_label("Raw").click();
    h.run_steps(2);
    h.get_by_label_contains("\"Node Type\"");
    h.state_mut().actions.clear();
    h.get_by_label("Copy raw plan").click();
    h.run_steps(2);
    assert_eq!(h.state().actions, vec![PlanAction::CopyRaw]);
}
