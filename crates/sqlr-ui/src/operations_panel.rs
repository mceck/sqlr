//! Reusable view of client-side operations; the app owns cancellation and retention.
use egui::{Align, Frame, Layout, Margin, RichText, Stroke};

use crate::theme::{self, Spacing};

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
    let p = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.heading("Operations");
        let running = rows.iter().filter(|r| r.cancelable).count();
        if running > 0 {
            ui.label(
                RichText::new(format!("{running} active"))
                    .small()
                    .color(p.text_muted),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Cancel all…").clicked() {
                    out.push(OperationsAction::CancelAll);
                }
            });
        }
    });
    theme::hairline(ui);
    ui.add_space(Spacing::S);
    if rows.is_empty() {
        theme::empty_panel(
            ui,
            theme::icons::HISTORY,
            "No operations",
            "Running queries, imports and exports appear here. You can follow their progress or cancel them.",
        );
    }
    egui::ScrollArea::vertical()
        .id_salt("operation_rows")
        .show(ui, |ui| {
            for row in rows {
                ui.push_id(row.id, |ui| {
                    Frame::new()
                        .fill(p.bg_editor)
                        .stroke(Stroke::new(1.0, p.border))
                        .corner_radius(Spacing::RADIUS_M)
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal_top(|ui| {
                                // Reserve the action before laying out a potentially very long label.
                                let details_width = (ui.available_width() - 78.0).max(20.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(details_width, 0.0),
                                    Layout::top_down(Align::Min),
                                    |ui| {
                                        ui.label(RichText::new(&row.label).strong());
                                        ui.horizontal_wrapped(|ui| {
                                            if row.cancelable {
                                                ui.spinner();
                                            }
                                            let color = if row.state == "Done" {
                                                p.success
                                            } else if row.state.starts_with("Failed") {
                                                p.error
                                            } else {
                                                p.text_muted
                                            };
                                            ui.label(RichText::new(&row.state).color(color));
                                            ui.label(
                                                RichText::new(&row.kind)
                                                    .small()
                                                    .color(p.text_muted),
                                            );
                                        });
                                        ui.label(
                                            RichText::new(format!(
                                                "{:.1} s · {} rows · {}",
                                                row.elapsed_ms as f64 / 1000.0,
                                                row.rows,
                                                crate::human_bytes(
                                                    row.bytes.min(i64::MAX as u64) as i64
                                                )
                                            ))
                                            .small()
                                            .color(p.text_muted),
                                        );
                                    },
                                );
                                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                    if row.cancelable {
                                        if ui.button("Cancel").clicked() {
                                            out.push(OperationsAction::Cancel(row.id));
                                        }
                                    } else if ui.button("Dismiss").clicked() {
                                        out.push(OperationsAction::Forget(row.id));
                                    }
                                });
                            });
                        });
                    ui.add_space(Spacing::S);
                });
            }
        });
    out
}
