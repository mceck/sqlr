//! Reusable view of client-side operations; the app owns cancellation and retention.
use egui::{Align, Layout, RichText};

use crate::theme::{self, Spacing, icons};

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
    Close,
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
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icons::icon_button(ui, icons::CLOSE, Some(p.text_muted), "Close Operations")
                .clicked()
            {
                out.push(OperationsAction::Close);
            }
            let completed: Vec<_> = rows
                .iter()
                .filter(|r| {
                    r.state == "Done" || r.state == "Cancelled" || r.state.starts_with("Failed")
                })
                .collect();
            if !completed.is_empty() && ui.button("Clear completed").clicked() {
                out.extend(completed.iter().map(|r| OperationsAction::Forget(r.id)));
            }
            if running > 0 && ui.button("Cancel all…").clicked() {
                out.push(OperationsAction::CancelAll);
            }
        });
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
                theme::list_row(
                    ui,
                    ui.id().with(row.id),
                    false,
                    32.0,
                    egui::Sense::hover(),
                    |ui| {
                        let color = if row.state == "Done" {
                            p.success
                        } else if row.state.starts_with("Failed") {
                            p.error
                        } else {
                            p.text_muted
                        };
                        if row.cancelable {
                            ui.spinner();
                        } else {
                            icons::paint(
                                ui,
                                if row.state == "Done" {
                                    icons::SUCCESS
                                } else {
                                    icons::HISTORY
                                },
                                color,
                            );
                        }
                        let action_width = 82.0;
                        let width = (ui.available_width() - action_width).max(20.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(width, 24.0),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.add(egui::Label::new(&row.label).truncate())
                                    .on_hover_text(&row.label);
                                ui.label(RichText::new(&row.state).small().color(color))
                                    .on_hover_text(&row.state);
                                let mut metrics = crate::history::format_duration(row.elapsed_ms);
                                if row.rows > 0 {
                                    metrics.push_str(&format!(" · {} rows", row.rows));
                                }
                                if row.bytes > 0 {
                                    metrics.push_str(&format!(
                                        " · {}",
                                        crate::human_bytes(row.bytes.min(i64::MAX as u64) as i64)
                                    ));
                                }
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(metrics).small().color(p.text_muted),
                                    )
                                    .truncate(),
                                );
                            },
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if row.cancelable {
                                if ui.button("Cancel").clicked() {
                                    out.push(OperationsAction::Cancel(row.id));
                                }
                            } else if ui.button("Dismiss").clicked() {
                                out.push(OperationsAction::Forget(row.id));
                            }
                        });
                    },
                );
            }
        });
    out
}
