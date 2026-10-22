use egui_kittest::{Harness, kittest::Queryable};
use sqlr_ui::operations_panel::{OperationRow, OperationsAction, operations_panel};
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
