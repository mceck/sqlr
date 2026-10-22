use crate::app::SqlrApp;
use sqlr_engine::{OperationId, OperationState};
use sqlr_ui::operations_panel::{OperationRow, OperationsAction, operations_panel};
impl SqlrApp {
    pub(crate) fn operations_ui(&mut self, ui: &mut egui::Ui) {
        self.engine.set_operations_retention(
            self.layered
                .get("operations.keep_finished_s", None)
                .0
                .as_i64()
                .max(0) as u64,
        );
        let operations = self.engine.operations();
        let running = operations.iter().any(|o| {
            matches!(
                o.state,
                OperationState::Running | OperationState::Cancelling
            )
        });
        if running {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        if self.show_operations {
            egui::Panel::bottom("operations")
                .resizable(true)
                .default_size(200.0)
                .show(ui, |ui| {
                    let rows = operations
                        .iter()
                        .map(|o| OperationRow {
                            id: o.id.0,
                            label: o.label.clone(),
                            kind: format!("{:?}", o.kind),
                            state: match &o.state {
                                OperationState::Done { ok: true } => "Done".into(),
                                OperationState::Cancelled => "Cancelled".into(),
                                s => format!("{s:?}"),
                            },
                            elapsed_ms: o.elapsed.as_millis() as u64,
                            rows: o.progress.rows,
                            bytes: o.progress.bytes,
                            cancelable: o.cancelable,
                        })
                        .collect::<Vec<_>>();
                    for action in operations_panel(ui, &rows) {
                        match action {
                            OperationsAction::Cancel(id) => {
                                self.engine.cancel_operation(OperationId(id));
                            }
                            OperationsAction::Forget(id) => {
                                self.engine.forget_operation(OperationId(id))
                            }
                            OperationsAction::CancelAll => self.cancel_all_operations = true,
                        }
                    }
                });
            // Expire finished rows even when nothing else repaints the window.
            if !operations.is_empty() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs(1));
            }
        }
        if self.cancel_all_operations {
            egui::Window::new("Cancel all operations?")
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label("Interrupt all running client operations?");
                    ui.horizontal(|ui| {
                        if ui.button("Keep running").clicked() {
                            self.cancel_all_operations = false;
                        }
                        if ui.button("Cancel all").clicked() {
                            for op in self.engine.operations() {
                                self.engine.cancel_operation(op.id);
                            }
                            self.cancel_all_operations = false;
                        }
                    });
                });
        }
    }
}
