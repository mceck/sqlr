use std::path::PathBuf;

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sqlr_core::{
    DumpOptions, Endpoint, SavedTask, TableRef, TaskRun, TaskSpec, TransferOptions, TransferSpec,
};
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{RunningTask, TasksAction, tasks_panel};

struct App {
    tasks: Vec<SavedTask>,
    running: Vec<RunningTask>,
    runs: Vec<TaskRun>,
    selected: Option<String>,
    actions: Vec<TasksAction>,
}

fn transfer(id: &str, name: &str) -> SavedTask {
    SavedTask {
        id: id.into(),
        name: name.into(),
        spec: TaskSpec::Transfer(TransferSpec {
            source: Endpoint::Table {
                conn: "a".into(),
                table: TableRef {
                    database: None,
                    schema: None,
                    name: "t".into(),
                },
            },
            target: Endpoint::Parquet {
                path: PathBuf::from("/tmp/t.parquet"),
            },
            mapping: Vec::new(),
            options: TransferOptions::default(),
        }),
        created_ms: 1,
        updated_ms: 1,
    }
}

fn dump(id: &str, name: &str) -> SavedTask {
    SavedTask {
        id: id.into(),
        name: name.into(),
        spec: TaskSpec::Dump {
            conn: "a".into(),
            database: "app".into(),
            options: DumpOptions::default(),
            out: PathBuf::from("/tmp/app.sql"),
        },
        created_ms: 1,
        updated_ms: 1,
    }
}

fn run(id: i64, outcome: &str, summary: &str) -> TaskRun {
    TaskRun {
        id,
        task_id: "t1".into(),
        started_ms: 1_700_000_000_000,
        ended_ms: Some(1_700_000_002_500),
        outcome: outcome.into(),
        summary: summary.into(),
        log: String::new(),
    }
}

fn harness(running: Vec<RunningTask>) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(vec2(420.0, 620.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                if let Some(a) =
                    tasks_panel(ui, &app.tasks, &app.running, &app.runs, &mut app.selected)
                {
                    app.actions.push(a);
                }
            },
            App {
                tasks: vec![transfer("t1", "Copy users"), dump("t2", "Nightly dump")],
                running,
                runs: vec![
                    run(2, "failed", "no such table"),
                    run(1, "ok", "Read 3 rows, wrote 3, rejected 0"),
                ],
                selected: None,
                actions: Vec::new(),
            },
        );
    h.run_steps(3);
    h
}

fn click(h: &mut Harness<'static, App>, label: &str) -> Vec<TasksAction> {
    h.state_mut().actions.clear();
    h.get_by_label(label).click();
    h.run_steps(2);
    h.state().actions.clone()
}

#[test]
fn tasks_panel_actions() {
    let mut h = harness(Vec::new());
    // Both tasks are listed under their kinds.
    h.get_by_label("Copy users");
    h.get_by_label("Nightly dump");
    // Nothing happens until a task is selected: the buttons wait.
    assert!(h.get_by_label("Run task").accesskit_node().is_disabled());

    h.get_by_label("Copy users").click();
    h.run_steps(2);
    assert_eq!(h.state().selected.as_deref(), Some("t1"));
    assert_eq!(
        click(&mut h, "Run task"),
        vec![TasksAction::Run("t1".into())]
    );
    assert_eq!(click(&mut h, "Edit"), vec![TasksAction::Edit("t1".into())]);
    assert_eq!(
        click(&mut h, "Duplicate"),
        vec![TasksAction::Duplicate("t1".into())]
    );
    assert_eq!(
        click(&mut h, "Delete"),
        vec![TasksAction::Delete("t1".into())]
    );

    // The runs of the selected task show their outcome and summary; one opens its log.
    h.get_by_label_contains("no such table");
    assert_eq!(
        click(&mut h, "Read 3 rows, wrote 3, rejected 0"),
        vec![TasksAction::ShowRun(1)]
    );
}

#[test]
fn a_running_task_shows_progress_and_can_be_cancelled() {
    let mut h = harness(vec![RunningTask {
        task_id: "t1".into(),
        job: 42,
        progress: "1500 rows".into(),
    }]);
    h.get_by_label("Copy users (running)").click();
    h.run_steps(2);
    h.get_by_label_contains("1500 rows");
    // A running task cannot be started again.
    assert!(h.get_by_label("Run task").accesskit_node().is_disabled());
    assert_eq!(click(&mut h, "Cancel"), vec![TasksAction::Cancel(42)]);
}

#[test]
fn task_actions_remain_reachable_in_a_narrow_sidebar() {
    let tasks = vec![transfer("t1", "Copy users")];
    let running = vec![RunningTask {
        task_id: "t1".into(),
        job: 42,
        progress: "1500 rows".into(),
    }];
    let mut selected = Some("t1".into());
    let mut h = Harness::builder()
        .with_size((260.0, 600.0))
        .build_ui(move |ui| {
            theme::apply(ui.ctx(), &Palette::dark(), 13.0);
            tasks_panel(ui, &tasks, &running, &[], &mut selected);
        });
    h.run_steps(3);
    for label in ["Run task", "Edit", "Duplicate", "Delete", "Cancel"] {
        let rect = h.get_by_label(label).rect();
        assert!(
            rect.right() <= 260.0,
            "{label} is outside the sidebar: {rect:?}"
        );
    }
}
