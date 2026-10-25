use egui_kittest::{Harness, kittest::Queryable};
use sqlr_ui::operations_panel::{OperationRow, OperationsAction, operations_panel};

#[test]
fn long_operation_keeps_its_cancel_action_inside_a_narrow_panel() {
    let rows = vec![OperationRow {
        id: 1,
        label: "Export orders_with_a_very_long_table_name from production to archive.csv".into(),
        kind: "Export".into(),
        state: "Running".into(),
        elapsed_ms: 12000,
        rows: 120000,
        bytes: 102400000,
        cancelable: true,
    }];
    let mut h = Harness::builder().with_size((360.0, 280.0)).build_ui(|ui| {
        operations_panel(ui, &rows);
    });
    h.run_steps(3);
    let cancel = h.get_by_label("Cancel");
    assert!(
        cancel.rect().right() <= 360.0,
        "Cancel is outside the panel: {:?}",
        cancel.rect()
    );
    assert!(cancel.rect().bottom() <= 280.0);
}
#[test]
fn operations_panel_lists_and_cancels() {
    let rows = vec![OperationRow {
        id: 7,
        label: "Connect test".into(),
        kind: "Connect".into(),
        state: "Running".into(),
        elapsed_ms: 120,
        rows: 5,
        bytes: 12,
        cancelable: true,
    }];
    let mut h = Harness::builder().build_ui_state(
        |ui, actions: &mut Vec<OperationsAction>| *actions = operations_panel(ui, &rows),
        vec![],
    );
    h.get_by_label("Connect test");
    h.get_by_label("Cancel").click();
    h.run_steps(1);
    assert_eq!(h.state(), &vec![OperationsAction::Cancel(7)]);
}
#[test]
fn operations_panel_shows_progress_elapsed_and_empty_state() {
    let mut h = Harness::new_ui(|ui| {
        operations_panel(ui, &[]);
    });
    h.run_steps(1);
    h.get_by_label("No operations");
}

#[test]
fn completed_operations_can_be_cleared_together() {
    let rows = vec![OperationRow {
        id: 9,
        label: "Connect archive".into(),
        kind: "Connect".into(),
        state: "Done".into(),
        elapsed_ms: 100,
        rows: 0,
        bytes: 0,
        cancelable: false,
    }];
    let mut h = Harness::builder().build_ui_state(
        |ui, actions: &mut Vec<OperationsAction>| *actions = operations_panel(ui, &rows),
        vec![],
    );
    h.get_by_label("Clear completed").click();
    h.run_steps(1);
    assert_eq!(h.state(), &vec![OperationsAction::Forget(9)]);
}
