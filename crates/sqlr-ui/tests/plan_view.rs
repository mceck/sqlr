use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_core::{PlanTree, parse_pg_json};
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{PlanAction, PlanChrome, PlanViewState, plan_view};

const PLAN: &str = r#"[{"Plan": {"Node Type": "Nested Loop", "Startup Cost": 0.0, "Total Cost": 30.0,
  "Plan Rows": 10, "Actual Rows": 8, "Actual Loops": 1, "Actual Total Time": 5.0,
  "Plans": [
    {"Node Type": "Seq Scan", "Relation Name": "big", "Startup Cost": 0.0, "Total Cost": 20.0,
     "Plan Rows": 100, "Actual Rows": 15000, "Actual Loops": 1, "Actual Total Time": 3.0,
     "Filter": "(x > 1)"},
    {"Node Type": "Index Scan", "Relation Name": "small", "Startup Cost": 0.1, "Total Cost": 0.5,
     "Plan Rows": 1, "Actual Rows": 1, "Actual Loops": 4, "Actual Total Time": 0.25}]},
  "Planning Time": 0.3, "Execution Time": 5.5}]"#;

const ESTIMATED: &str = r#"[{"Plan": {"Node Type": "Seq Scan", "Relation Name": "t",
  "Startup Cost": 0.0, "Total Cost": 12.0, "Plan Rows": 500}}]"#;

struct App {
    plan: PlanTree,
    state: PlanViewState,
    actions: Vec<PlanAction>,
    sql: &'static str,
    stale: bool,
    running: bool,
}

fn harness_with(
    plan: PlanTree,
    sql: &'static str,
    stale: bool,
    running: bool,
) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(vec2(1000.0, 620.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                if let Some(a) = plan_view(
                    ui,
                    &app.plan,
                    &mut app.state,
                    PlanChrome {
                        sql: app.sql,
                        stale: app.stale,
                        running: app.running,
                    },
                ) {
                    app.actions.push(a);
                }
            },
            App {
                plan,
                state: PlanViewState::default(),
                actions: Vec::new(),
                sql,
                stale,
                running,
            },
        );
    h.run_steps(4);
    h
}

fn harness() -> Harness<'static, App> {
    harness_with(
        parse_pg_json(PLAN).unwrap(),
        "SELECT * FROM big JOIN small ON true",
        false,
        false,
    )
}

#[test]
fn cards_are_compact_and_show_node_times_in_milliseconds() {
    let h = harness();
    let card = h.get_by_label("Select Nested Loop").rect();
    assert!(card.height() <= 58.0, "card is too tall: {card:?}");
    assert!(card.width() <= 470.0, "card is too wide: {card:?}");
    h.get_by_label("self 1.0 ms · total 5.0 ms · 20%");
    h.get_by_label("self 3.0 ms · total 3.0 ms · 60%");
    // The index scan takes 0.25 ms per loop, four loops: display their total once.
    h.get_by_label("self 1.0 ms · total 1.0 ms · 20%");
}

#[test]
fn estimated_plans_explain_how_to_get_measured_times() {
    let h = harness_with(
        parse_pg_json(ESTIMATED).unwrap(),
        "SELECT * FROM t",
        false,
        false,
    );
    h.get_by_label("Run Explain Analyze for real timings.");
    assert!(h.query_by_label_contains("self 0").is_none());
}

#[test]
fn node_timings_are_visible_even_without_row_counts_or_estimated_costs() {
    let h = harness_with(
        PlanTree {
            root: sqlr_core::PlanNode {
                label: "Remote query".into(),
                time_ms: Some(4.0),
                self_time_ms: Some(4.0),
                ..Default::default()
            },
            execution_ms: Some(4.0),
            ..Default::default()
        },
        "SELECT 1",
        false,
        false,
    );
    h.get_by_label("self 4.0 ms · total 4.0 ms · 100%");
}

#[test]
fn plan_view_summarizes_the_plan_and_marks_the_hottest_nodes() {
    let h = harness();
    h.get_by_label("Analyzed");
    h.get_by_label("1 join");
    h.get_by_label("2 scans");
    h.get_by_label("3 operators");
    h.get_by_label("HOT 1");
    h.get_by_label("SELF TIME");
    h.get_by_label_contains("Hot path, 3 nodes");
    // The warnings are in the summary, in plain words.
    h.get_by_label_contains("100 rows estimated, 15,000 read");
    h.get_by_label_contains("Seq Scan on big reads 15,000 rows (full scan)");
}

#[test]
fn selecting_a_node_shows_its_details_and_the_ratio_badge_names_it() {
    let mut h = harness();
    h.get_by_label_contains("150× more rows than estimated");
    h.get_by_label("Select Seq Scan on big").click();
    h.run_steps(2);
    assert_eq!(h.state().state.selected, Some(vec![0]));
    h.get_by_label("(x > 1)");
    h.get_by_label("Filter");
    h.get_by_label("Rows (actual)");
}

#[test]
fn a_subtree_collapses_and_expands() {
    let mut h = harness();
    h.get_by_label("Select Seq Scan on big");
    h.get_by_label("Collapse 2 children of Nested Loop").click();
    h.run_steps(2);
    assert!(h.query_by_label("Select Seq Scan on big").is_none());
    h.get_by_label("Expand 2 children of Nested Loop").click();
    h.run_steps(2);
    h.get_by_label("Select Seq Scan on big");
    // "Collapse all" hides every subtree; "Expand all" brings them back.
    h.get_by_label("Collapse all").click();
    h.run_steps(2);
    assert!(h.query_by_label("Select Seq Scan on big").is_none());
    h.get_by_label("Expand all").click();
    h.run_steps(2);
    h.get_by_label("Select Seq Scan on big");
}

#[test]
fn the_raw_plan_is_one_toggle_away_and_can_be_copied() {
    let mut h = harness();
    h.get_by_label("Raw").click();
    h.run_steps(2);
    h.get_by_label_contains("\"Node Type\"");
    h.state_mut().actions.clear();
    h.get_by_label("Copy raw plan").click();
    h.run_steps(2);
    assert_eq!(h.state().actions, vec![PlanAction::CopyRaw]);
}

#[test]
fn a_changed_statement_offers_a_recompute() {
    let mut h = harness_with(parse_pg_json(PLAN).unwrap(), "SELECT 1", true, false);
    h.get_by_label("Text changed");
    h.get_by_label("Recompute").click();
    h.run_steps(2);
    assert_eq!(h.state().actions, vec![PlanAction::Recompute]);
}

#[test]
fn an_estimated_plan_says_so_and_lists_the_note() {
    let h = harness_with(
        parse_pg_json(ESTIMATED).unwrap(),
        "SELECT * FROM t",
        false,
        false,
    );
    h.get_by_label("Estimated");
    h.get_by_label_contains("Estimated plan: row counts and costs are estimates");
    h.get_by_label("1 operator");
    // Without timings there is no hot path worth drawing for a single node.
    assert!(h.query_by_label("SELF COST").is_none());
}

#[test]
fn estimated_plan_offers_a_measured_run() {
    let mut h = harness_with(
        parse_pg_json(ESTIMATED).unwrap(),
        "SELECT * FROM t",
        false,
        false,
    );
    h.get_by_label("Explain Analyze").click();
    h.run_steps(2);
    assert_eq!(
        format!("{:?}", h.state().actions.last().unwrap()),
        "Analyze"
    );
}
