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
                                OperationState::Done { ok: false } => "Failed".into(),
                                OperationState::Failed(message) => format!("Failed: {message}"),
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
                            OperationsAction::Close => self.show_operations = false,
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
            let names: Vec<String> = operations
                .iter()
                .filter(|o| matches!(o.state, OperationState::Running) && o.cancelable)
                .map(|o| o.label.clone())
                .collect();
            let prompt = stop_all_prompt(&names);
            let spec = crate::confirm::ConfirmSpec::new(
                egui::Id::new("cancel_all_operations"),
                sqlr_ui::theme::icons::STOP,
                "Stop running operations",
                "Stop all",
            )
            .dismiss("Keep running");
            let answer = crate::confirm::confirm_modal(
                ui.ctx(),
                spec,
                |ui| {
                    ui.label(prompt);
                },
                |_| {},
            );
            match answer {
                Some(true) => {
                    for op in self.engine.operations() {
                        self.engine.cancel_operation(op.id);
                    }
                    self.cancel_all_operations = false;
                }
                Some(false) => self.cancel_all_operations = false,
                None => {}
            }
        }
    }
}

/// "Stop 3 running operations: Dump Northwind, Export orders and 1 more?"
fn stop_all_prompt(names: &[String]) -> String {
    const LISTED: usize = 3;
    match names {
        [] => "Stop every running operation?".to_string(),
        [one] => format!("Stop the running operation \"{one}\"?"),
        _ => {
            let listed = names
                .iter()
                .take(LISTED)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            let more = names.len().saturating_sub(LISTED);
            let tail = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            format!("Stop {} running operations: {listed}{tail}?", names.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stop_all_prompt;

    #[test]
    fn the_stop_all_prompt_names_the_operations() {
        assert_eq!(stop_all_prompt(&[]), "Stop every running operation?");
        assert_eq!(
            stop_all_prompt(&["Dump Northwind".into()]),
            "Stop the running operation \"Dump Northwind\"?"
        );
        let names: Vec<String> = ["Dump Northwind", "Export orders", "Connect", "Import"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            stop_all_prompt(&names),
            "Stop 4 running operations: Dump Northwind, Export orders, Connect and 1 more?"
        );
    }
}
