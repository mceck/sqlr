//! Reusable view of client-side operations; the app owns cancellation and retention.
#[derive(Clone, Debug)]
pub struct OperationRow {
    pub id: u64,
    pub label: String,
    pub kind: String,
    pub state: String,
    pub elapsed_ms: u64,
    pub rows: u64,
    pub bytes: u64,
    pub cancelable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationsAction {
    Cancel(u64),
    CancelAll,
    Forget(u64),
}
pub fn operations_panel(ui: &mut egui::Ui, rows: &[OperationRow]) -> Vec<OperationsAction> {
    let mut out = Vec::new();
    ui.horizontal(|ui| {
        ui.heading("Operations");
        if rows.iter().any(|r| r.cancelable) && ui.button("Cancel all…").clicked() {
            out.push(OperationsAction::CancelAll);
        }
    });
    if rows.is_empty() {
        ui.label("No operations");
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for row in rows {
            ui.push_id(row.id, |ui| {
                ui.horizontal(|ui| {
                    if row.cancelable {
                        ui.spinner();
                    }
                    ui.label(&row.kind);
                    ui.label(&row.label);
                    ui.label(&row.state);
                    ui.label(format!(
                        "{:.1} s · {} rows · {} bytes",
                        row.elapsed_ms as f64 / 1000.0,
                        row.rows,
                        row.bytes
                    ));
                    if row.cancelable {
                        if ui.button("Cancel").clicked() {
                            out.push(OperationsAction::Cancel(row.id));
                        }
                    } else if ui.small_button("Dismiss").clicked() {
                        out.push(OperationsAction::Forget(row.id));
                    }
                });
            });
        }
    });
    out
}
