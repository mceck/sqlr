//! A visual review of the explain plan widget on a synthetic PostgreSQL plan.
//! `SQLR_PLAN_PNG=/tmp/plan.png cargo test -p sqlr-app --test integration plan_visual -- --ignored`

use egui::vec2;
use egui_kittest::Harness;
use sqlr_core::{PlanTree, parse_pg_json};
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{PlanChrome, PlanViewState, plan_view};

const PLAN: &str = r#"[{"Plan": {"Node Type": "Hash Join", "Startup Cost": 12.0, "Total Cost": 1840.5,
  "Plan Rows": 1200, "Actual Rows": 1180, "Actual Loops": 1, "Actual Total Time": 42.8,
  "Hash Cond": "(o.customer_id = c.id)", "Plans": [
    {"Node Type": "Seq Scan", "Relation Name": "orders", "Startup Cost": 0.0, "Total Cost": 1240.0,
     "Plan Rows": 800, "Actual Rows": 150000, "Actual Loops": 1, "Actual Total Time": 31.2,
     "Filter": "(status = 'paid')"},
    {"Node Type": "Hash", "Startup Cost": 8.0, "Total Cost": 8.0, "Plan Rows": 400,
     "Actual Rows": 400, "Actual Loops": 1, "Actual Total Time": 1.1, "Plans": [
       {"Node Type": "Sort", "Startup Cost": 3.0, "Total Cost": 8.0, "Plan Rows": 400,
        "Actual Rows": 400, "Actual Loops": 1, "Actual Total Time": 1.0,
        "Sort Method": "external merge  Disk: 24kB", "Plans": [
          {"Node Type": "Index Scan", "Relation Name": "customers", "Startup Cost": 0.1,
           "Total Cost": 6.0, "Plan Rows": 400, "Actual Rows": 400, "Actual Loops": 1,
           "Actual Total Time": 0.8, "Index Name": "customers_pkey"}]}]}]},
  "Planning Time": 0.4, "Execution Time": 44.1}]"#;

struct App {
    plan: PlanTree,
    state: PlanViewState,
}

#[test]
#[ignore = "renders the plan view; requires a GPU adapter"]
fn plan_visual() {
    let out = std::env::var_os("SQLR_PLAN_PNG").map_or_else(
        || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/plan-view.png"),
        std::path::PathBuf::from,
    );
    for (palette, suffix) in [(Palette::dark(), "dark"), (Palette::light(), "light")] {
        let plan = parse_pg_json(PLAN).unwrap();
        let state = PlanViewState {
            selected: Some(vec![0]),
            ..PlanViewState::default()
        };
        let mut h = Harness::builder()
            .with_size(vec2(1100.0, 700.0))
            .with_step_dt(0.01)
            .build_ui_state(
                |ui, app: &mut App| {
                    theme::apply(ui.ctx(), &palette, 13.0);
                    plan_view(
                        ui,
                        &app.plan,
                        &mut app.state,
                        PlanChrome {
                            sql: "SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.status = 'paid'",
                            stale: false,
                            running: false,
                        },
                    );
                },
                App { plan, state },
            );
        sqlr_ui::theme::install_fonts(&h.ctx, &[]);
        h.run_steps(4);
        let path = out.with_file_name(format!(
            "{}-{suffix}.png",
            out.file_stem().unwrap().to_string_lossy()
        ));
        h.render().expect("GPU render").save(&path).unwrap();
        eprintln!("wrote {}", path.display());
    }
}
