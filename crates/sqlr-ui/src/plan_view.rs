//! The visual explain plan (spec §8.6): the plan as an indented tree with cost, rows
//! (estimated against actual) and time per node, the heaviest nodes highlighted, a details
//! pane for the selected node and the raw plan one click away.

use egui::{Color32, RichText, WidgetInfo, WidgetType};
use sqlr_core::{PlanNode, PlanTree};

use crate::theme::{self, Spacing};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanViewState {
    /// The selected node, as child indexes from the root.
    pub selected: Option<Vec<usize>>,
    pub show_raw: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanAction {
    CopyRaw,
}

/// The factor by which actual rows differ from the estimate, when it is 10× or more.
fn row_ratio(n: &PlanNode) -> Option<(f64, bool)> {
    let (est, act) = (
        n.rows_estimate?,
        n.rows_actual? * n.loops.unwrap_or(1.0).max(1.0),
    );
    let (est, act) = (est.max(1.0), act.max(1.0));
    let ratio = if act >= est { act / est } else { est / act };
    (ratio >= 10.0).then_some((ratio, act > est))
}

fn fmt(v: f64) -> String {
    if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 1.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// Draws `plan`; returns the toolbar action the user chose.
pub fn plan_view(
    ui: &mut egui::Ui,
    plan: &PlanTree,
    state: &mut PlanViewState,
) -> Option<PlanAction> {
    let p = theme::palette(ui.ctx());
    let mut action = None;
    ui.horizontal(|ui| {
        if ui.selectable_label(state.show_raw, "Raw").clicked() {
            state.show_raw = !state.show_raw;
        }
        if ui.button("Copy raw plan").clicked() {
            action = Some(PlanAction::CopyRaw);
        }
        let mut times = Vec::new();
        if let Some(t) = plan.planning_ms {
            times.push(format!("planning {} ms", fmt(t)));
        }
        if let Some(t) = plan.execution_ms {
            times.push(format!("execution {} ms", fmt(t)));
        }
        if !times.is_empty() {
            ui.label(RichText::new(times.join(" · ")).small().color(p.text_muted));
        }
    });
    ui.add_space(Spacing::S);
    if state.show_raw {
        egui::ScrollArea::both().show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(&plan.raw).monospace()).selectable(true));
        });
        return action;
    }
    let hottest = plan.hottest(3);
    let max_weight = plan
        .hottest(1)
        .first()
        .and_then(|path| plan.node(path))
        .map_or(0.0, PlanNode::weight);
    egui::ScrollArea::vertical()
        .id_salt("plan-tree")
        .max_height((ui.available_height() * 0.6).max(160.0))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let mut path = Vec::new();
            draw_node(ui, &plan.root, &mut path, 0, &hottest, max_weight, state);
        });
    if let Some(node) = state.selected.as_deref().and_then(|s| plan.node(s)) {
        ui.add_space(Spacing::M);
        theme::hairline(ui);
        ui.label(RichText::new(&node.label).strong());
        let mut line = |name: &str, value: String| {
            ui.label(RichText::new(format!("{name}: {value}")).monospace());
        };
        if let Some((s, t)) = node.cost {
            line("Cost", format!("{} .. {}", fmt(s), fmt(t)));
        }
        if let Some(r) = node.rows_estimate {
            line("Rows (estimated)", fmt(r));
        }
        if let Some(r) = node.rows_actual {
            line("Rows (actual)", fmt(r));
        }
        if let Some(l) = node.loops {
            line("Loops", fmt(l));
        }
        if let Some(t) = node.time_ms {
            line("Time (inclusive)", format!("{} ms", fmt(t)));
        }
        if let Some(t) = node.self_time_ms {
            line("Time (this node)", format!("{} ms", fmt(t)));
        }
        for (k, v) in &node.buffers {
            line(k, v.to_string());
        }
        for (k, v) in &node.extra {
            line(k, v.clone());
        }
    }
    action
}

fn draw_node(
    ui: &mut egui::Ui,
    node: &PlanNode,
    path: &mut Vec<usize>,
    depth: usize,
    hottest: &[Vec<usize>],
    max_weight: f64,
    state: &mut PlanViewState,
) {
    let p = theme::palette(ui.ctx());
    let rank = hottest.iter().position(|h| h == path);
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * Spacing::XL);
        let tint = match rank {
            Some(0) => p.error,
            Some(_) => p.warning,
            None => Color32::TRANSPARENT,
        };
        let selected = state.selected.as_deref() == Some(path.as_slice());
        let label = ui.selectable_label(selected, &node.label);
        if label.clicked() {
            state.selected = Some(path.clone());
        }
        if let Some(rank) = rank {
            let text = format!("Hottest {}: {}", rank + 1, node.label);
            let badge = ui.label(RichText::new(format!("▲{}", rank + 1)).color(tint));
            badge.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text.clone()));
        }
        if let Some(rel) = &node.relation {
            ui.label(RichText::new(rel).color(p.icon_table));
        }
        let rows = match (node.rows_estimate, node.rows_actual) {
            (Some(e), Some(a)) => format!("rows {} est · {} act", fmt(e), fmt(a)),
            (Some(e), None) => format!("rows {} est", fmt(e)),
            _ => String::new(),
        };
        if !rows.is_empty() {
            ui.label(RichText::new(rows).small().color(p.text_muted));
        }
        if let Some((ratio, more)) = row_ratio(node) {
            let text = format!(
                "{}× {} rows than estimated",
                ratio.round(),
                if more { "more" } else { "fewer" }
            );
            let badge = ui.label(RichText::new("≠").color(p.warning));
            badge.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text.clone()));
        }
        if let Some(t) = node.self_time_ms.or(node.time_ms) {
            let share = if max_weight > 0.0 {
                (node.weight() / max_weight).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(60.0, 8.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, p.bg_subtle);
            let mut fill = rect;
            fill.set_width(rect.width() * share);
            ui.painter()
                .rect_filled(fill, 2.0, if rank == Some(0) { p.error } else { p.accent });
            ui.label(
                RichText::new(format!("{} ms", fmt(t)))
                    .small()
                    .color(p.text_muted),
            );
        }
        if let Some((_, total)) = node.cost {
            ui.label(
                RichText::new(format!("cost {}", fmt(total)))
                    .small()
                    .color(p.text_muted),
            );
        }
    });
    for (i, child) in node.children.iter().enumerate() {
        path.push(i);
        draw_node(ui, child, path, depth + 1, hottest, max_weight, state);
        path.pop();
    }
}
