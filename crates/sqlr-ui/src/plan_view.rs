//! The visual explain plan (spec §8.6): a summary of the plan's time and cost, a hot path
//! bar and the operators as cards, with the selected node's details beside them. The raw
//! plan is one toggle away.
//!
//! The widget only reads a [`PlanTree`] and returns [`PlanAction`]s; it never calls the
//! engine.

use std::collections::HashSet;

use egui::{
    Align, Color32, CornerRadius, CursorIcon, Frame, Id, Label, Layout, Margin, Rect, RichText,
    ScrollArea, Sense, Stroke, Ui, WidgetInfo, WidgetType, pos2, vec2,
};
use sqlr_core::{PlanInsights, PlanIssue, PlanIssueLevel, PlanNode, PlanRole, PlanTree};

use crate::theme::{self, Palette, Spacing, icons};

/// The widest a node card gets.
const CARD_W: f32 = 440.0;
/// The horizontal room a child subtree adds under its parent.
const INDENT: f32 = 26.0;
/// The space between two sibling cards.
const VGAP: f32 = 8.0;
/// The width of the node inspector on the right.
const INSPECTOR_W: f32 = 292.0;
/// The height of the hot path bar.
const HOT_PATH_H: f32 = 10.0;
/// At most this many warnings are listed under the summary; the inspector shows the rest.
const SUMMARY_WARNINGS: usize = 4;
/// The room the header keeps for the chips and the toolbar beside the statement.
const HEADER_ACTIONS_W: f32 = 200.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanViewState {
    /// The selected node, as child indexes from the root.
    pub selected: Option<Vec<usize>>,
    pub show_raw: bool,
    /// Subtrees the user collapsed, by node path.
    pub collapsed: HashSet<Vec<usize>>,
    /// A node to bring into view once it is drawn (a hot path click).
    pub scroll_to: Option<Vec<usize>>,
}

impl PlanViewState {
    /// Selects `path`, opening the collapsed ancestors that hide it.
    pub fn reveal(&mut self, path: &[usize]) {
        for end in 1..path.len() {
            self.collapsed.remove(&path[..end]);
        }
        self.selected = Some(path.to_vec());
        self.scroll_to = Some(path.to_vec());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanAction {
    CopyRaw,
    Analyze,
    /// The editor text changed: explain the current statement again.
    Recompute,
}

/// What the header says around the plan.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlanChrome<'a> {
    /// The statement the plan was computed from; its first line is shown.
    pub sql: &'a str,
    /// The statement at the caret changed since this plan was computed.
    pub stale: bool,
    /// A new explain is running: the plan stays, dimmed.
    pub running: bool,
}

/// Draws the plan; returns the toolbar action the user chose.
pub fn plan_view(
    ui: &mut Ui,
    plan: &PlanTree,
    state: &mut PlanViewState,
    chrome: PlanChrome<'_>,
) -> Option<PlanAction> {
    let p = theme::palette(ui.ctx());
    let insights = plan.insights();
    let mut action = None;
    if chrome.running {
        ui.set_opacity(0.55);
    }
    action = action.or(header(ui, &p, plan, state, &insights, chrome));
    ui.add_space(Spacing::S);
    if state.show_raw {
        raw_view(ui, plan, &p);
    } else {
        nodes(ui, &p, plan, &insights, state);
    }
    if chrome.running {
        ui.set_opacity(1.0);
    }
    action
}

fn header(
    ui: &mut Ui,
    p: &Palette,
    plan: &PlanTree,
    state: &mut PlanViewState,
    insights: &PlanInsights,
    chrome: PlanChrome<'_>,
) -> Option<PlanAction> {
    let mut action = None;
    let line = chrome.sql.lines().next().unwrap_or_default().trim();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        let sql_w = (ui.available_width() - HEADER_ACTIONS_W).max(80.0);
        ui.allocate_ui_with_layout(
            vec2(sql_w, 20.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                icons::paint(ui, icons::PLAN, p.accent);
                let title = if line.is_empty() {
                    "Query plan".to_string()
                } else {
                    line.to_string()
                };
                ui.add(Label::new(RichText::new(title).monospace().color(p.text_muted)).truncate())
                    .on_hover_text(chrome.sql.trim());
            },
        );
        state_chip(ui, insights.timed);
        if chrome.stale {
            chip(
                ui,
                "Text changed",
                p.warning,
                p.warning.gamma_multiply(0.14),
            );
        }
    });

    ui.add_space(Spacing::XS);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::L;
        if let Some(execution) = plan.execution_ms {
            ui.label(RichText::new(fmt_ms(execution)).font(theme::semibold(ui.ctx(), 19.0)));
            ui.label(RichText::new("execution").small().color(p.text_muted));
        } else if let Some(time) = plan.root.time_ms {
            metric(ui, p, "root total", fmt_ms(time));
        }

        if let Some(planning) = plan.planning_ms {
            metric(ui, p, "planning", fmt_ms(planning));
        }
        if let Some((_, cost)) = plan.root.cost {
            metric(ui, p, "cost", fmt_num(cost));
        }
        if let Some(rows) = output_rows(plan) {
            metric(ui, p, "rows", fmt_num(rows));
        }
        let warnings = insights
            .issues
            .iter()
            .filter(|i| i.level == PlanIssueLevel::Warning)
            .count();
        if warnings > 0 {
            icons::paint(ui, icons::WARNING, p.warning);
            ui.label(
                RichText::new(if warnings == 1 {
                    "1 warning".to_string()
                } else {
                    format!("{warnings} warnings")
                })
                .small()
                .color(p.warning),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icons::button(ui, true, icons::COPY, None, "Copy raw plan").clicked() {
                action = Some(PlanAction::CopyRaw);
            }
            if ui.selectable_label(state.show_raw, "Raw").clicked() {
                state.show_raw = !state.show_raw;
            }
            if !state.show_raw {
                let label = if state.collapsed.is_empty() {
                    "Collapse all"
                } else {
                    "Expand all"
                };
                if ui.small_button(label).clicked() {
                    if state.collapsed.is_empty() {
                        collapse_all(plan, state);
                    } else {
                        state.collapsed.clear();
                    }
                }
            }
            if chrome.stale
                && icons::button(ui, true, icons::REFRESH, Some(p.warning), "Recompute")
                    .on_hover_text("Recompute the plan for the current statement")
                    .clicked()
            {
                action = Some(PlanAction::Recompute);
            }
        });
    });

    if !insights.timed {
        ui.horizontal(|ui| {
            theme::hint(ui, "Run Explain Analyze for real timings.");
            if ui
                .add_enabled(!chrome.running, egui::Button::new("Explain Analyze"))
                .clicked()
            {
                action = Some(PlanAction::Analyze);
            }
        });
    }

    ui.add_space(Spacing::S);
    composition(ui, p, insights);
    issues(ui, p, insights);
    ui.add_space(Spacing::S);
    hot_path(ui, p, plan, insights, state);
    ui.add_space(Spacing::S);
    theme::hairline(ui);
    action
}

fn state_chip(ui: &mut Ui, timed: bool) {
    let p = theme::palette(ui.ctx());
    if timed {
        chip(ui, "Analyzed", p.success, p.success.gamma_multiply(0.15));
    } else {
        chip(
            ui,
            "Estimated",
            p.text_muted,
            p.text_muted.gamma_multiply(0.15),
        );
    }
}

/// Composition chips: what the plan is made of, in one line.
fn composition(ui: &mut Ui, p: &Palette, insights: &PlanInsights) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        chip(
            ui,
            &format!(
                "{} {}",
                insights.nodes,
                if insights.nodes == 1 {
                    "operator"
                } else {
                    "operators"
                }
            ),
            p.text_muted,
            p.text_muted.gamma_multiply(0.12),
        );
        let parts = [
            (insights.joins, "join", "joins", PlanRole::Join),
            (insights.scans, "scan", "scans", PlanRole::Scan),
            (insights.sorts, "sort", "sorts", PlanRole::Sort),
            (
                insights.aggregates,
                "aggregate",
                "aggregates",
                PlanRole::Aggregate,
            ),
        ];
        for (count, one, many, role) in parts {
            if count == 0 {
                continue;
            }
            let (icon, color) = role_icon(role);
            let (fg, bg) = (color(p), color(p).gamma_multiply(0.14));
            Frame::new()
                .fill(bg)
                .corner_radius(CornerRadius::same(Spacing::RADIUS_S))
                .inner_margin(Margin::symmetric(5, 1))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        icons::paint(ui, icon, fg);
                        ui.label(
                            RichText::new(format!(
                                "{count} {}",
                                if count == 1 { one } else { many }
                            ))
                            .small()
                            .color(fg),
                        );
                    });
                });
        }
    });
}

fn issues(ui: &mut Ui, p: &Palette, insights: &PlanInsights) {
    let warnings: Vec<&PlanIssue> = insights
        .issues
        .iter()
        .filter(|i| i.level == PlanIssueLevel::Warning)
        .collect();
    for issue in warnings.iter().take(SUMMARY_WARNINGS) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::S;
            icons::paint(ui, icons::WARNING, p.warning);
            ui.add(
                Label::new(RichText::new(&issue.message).small().color(p.warning))
                    .truncate()
                    .selectable(false),
            )
            .on_hover_text(&issue.message);
        });
    }
    let hidden = warnings.len().saturating_sub(SUMMARY_WARNINGS);
    if hidden > 0 {
        theme::hint(ui, &format!("+{hidden} more in the node details"));
    }
    for issue in insights
        .issues
        .iter()
        .filter(|i| i.level == PlanIssueLevel::Note)
    {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::S;
            icons::paint(ui, icons::MORE, p.text_muted);
            ui.label(RichText::new(&issue.message).small().color(p.text_muted));
        });
    }
}

/// A stacked bar of the nodes' own time (or cost, without Analyze); hovering names the
/// segment and clicking selects its node.
fn hot_path(
    ui: &mut Ui,
    p: &Palette,
    plan: &PlanTree,
    insights: &PlanInsights,
    state: &mut PlanViewState,
) {
    let mut nodes = Vec::new();
    fn walk<'a>(
        node: &'a PlanNode,
        path: &mut Vec<usize>,
        nodes: &mut Vec<(Vec<usize>, f64, &'a PlanNode)>,
    ) {
        nodes.push((path.clone(), node.weight(), node));
        for (i, child) in node.children.iter().enumerate() {
            path.push(i);
            walk(child, path, nodes);
            path.pop();
        }
    }
    walk(&plan.root, &mut Vec::new(), &mut nodes);
    let total: f64 = nodes.iter().map(|(_, weight, _)| *weight).sum();
    if total <= 0.0 || nodes.len() <= 1 {
        return;
    }
    ui.label(
        RichText::new(if insights.timed {
            "SELF TIME"
        } else {
            "SELF COST"
        })
        .small()
        .color(p.text_muted),
    );
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), HOT_PATH_H), Sense::click());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(3), p.bg_subtle);
    let mut x = rect.left();
    let mut hovered: Option<(Vec<usize>, String, f64)> = None;
    let pointer = ui.ctx().pointer_hover_pos();
    for (path, weight, node) in &nodes {
        let share = (weight / total).clamp(0.0, 1.0) as f32;
        if share <= 0.0 {
            continue;
        }
        let width = (rect.width() * share).max(1.0);
        let segment = Rect::from_min_size(pos2(x, rect.top()), vec2(width, rect.height()));
        let (_, color) = role_icon(node.role());
        ui.painter()
            .rect_filled(segment.shrink2(vec2(0.5, 0.0)), 2.0, color(p));
        if pointer.is_some_and(|at| segment.contains(at)) {
            hovered = Some((path.clone(), node.title(), weight / total));
        }
        x += width;
    }
    if let Some((path, title, share)) = hovered {
        let verb = if insights.timed { "time" } else { "cost" };
        response.clone().on_hover_text(format!(
            "{title} · {} of the {verb}",
            fmt_percent(share * 100.0)
        ));
        if response.clicked() {
            state.reveal(&path);
        }
    }
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Other,
            true,
            format!("Hot path, {} nodes", nodes.len()),
        )
    });
}

fn raw_view(ui: &mut Ui, plan: &PlanTree, p: &Palette) {
    let height = ui.available_height().max(80.0);
    ScrollArea::both()
        .id_salt("plan-raw")
        .max_height(height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(Label::new(RichText::new(&plan.raw).monospace().color(p.text)).selectable(true));
        });
}

/// The diagram on the left, the selected node's details on the right.
fn nodes(
    ui: &mut Ui,
    p: &Palette,
    plan: &PlanTree,
    insights: &PlanInsights,
    state: &mut PlanViewState,
) {
    let height = ui.available_height().max(80.0);
    ui.horizontal_top(|ui| {
        let tree_w = (ui.available_width() - INSPECTOR_W - Spacing::L).max(200.0);
        ui.allocate_ui_with_layout(vec2(tree_w, height), Layout::top_down(Align::Min), |ui| {
            ui.set_min_height(height);
            ui.spacing_mut().indent = INDENT;
            ui.visuals_mut().indent_has_left_vline = false;
            let hottest = plan.hottest(3);
            let total = total_weight(plan);
            ScrollArea::both()
                .id_salt("plan-tree")
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(Spacing::S);
                    draw_node(ui, &plan.root, &mut Vec::new(), &hottest, total, state);
                    ui.add_space(Spacing::M);
                });
        });
        ui.add_space(Spacing::L);
        ui.allocate_ui_with_layout(
            vec2(INSPECTOR_W, height),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_min_height(height);
                Frame::new()
                    .fill(p.bg_raised)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(Spacing::RADIUS_M)
                    .inner_margin(Spacing::M)
                    .show(ui, |ui| inspector(ui, p, plan, insights, state));
            },
        );
    });
}

fn draw_node(
    ui: &mut Ui,
    node: &PlanNode,
    path: &mut Vec<usize>,
    hottest: &[Vec<usize>],
    total: f64,
    state: &mut PlanViewState,
) -> Rect {
    let p = theme::palette(ui.ctx());
    let rank = hottest.iter().position(|h| h == path);
    let (icon, role_color) = role_icon(node.role());
    let color = role_color(&p);
    let selected = state.selected.as_deref() == Some(path.as_slice());
    let collapsed = state.collapsed.contains(path);

    let width = (ui.available_width() - 20.0).clamp(180.0, CARD_W);
    let stroke = if selected {
        Stroke::new(1.5, p.accent)
    } else if rank == Some(0) {
        Stroke::new(1.5, p.error)
    } else if rank.is_some() {
        Stroke::new(1.0, p.warning)
    } else {
        Stroke::new(1.0, p.border)
    };
    let fill = if selected {
        p.accent_soft
    } else if rank == Some(0) {
        p.error
            .gamma_multiply(if p.is_dark() { 0.10 } else { 0.06 })
    } else {
        p.bg_panel
    };
    let card = Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(Spacing::RADIUS_M))
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.spacing_mut().interact_size.y = 16.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = Spacing::S;
                icons::paint(ui, icon, color);
                ui.add(
                    Label::new(RichText::new(node.title()).font(theme::semibold(ui.ctx(), 12.5)))
                        .truncate(),
                )
                .on_hover_text(node.title());
                if let Some(rank) = rank {
                    hot_badge(ui, &p, rank);
                }
            });
            let rows = rows_text(node);
            let loops = node.loops.unwrap_or(1.0).max(1.0);
            let share = if total > 0.0 {
                (node.weight() / total).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            if !rows.is_empty() || node.cost.is_some() || node.time_ms.is_some() || loops > 1.0 {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = Spacing::S;
                    if !rows.is_empty() {
                        ui.add(
                            Label::new(RichText::new(rows).small().color(p.text_muted)).truncate(),
                        );
                    }
                    if let Some((ratio, more)) = row_ratio(node) {
                        let text = format!(
                            "{}× {} rows than estimated",
                            fmt_num(ratio),
                            if more { "more" } else { "fewer" }
                        );
                        let badge = ui.label(RichText::new("≠").small().color(p.warning));
                        badge.widget_info(|| {
                            WidgetInfo::labeled(WidgetType::Label, true, text.clone())
                        });
                        badge.on_hover_text(text);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if loops > 1.0 {
                            ui.label(
                                RichText::new(format!("loops {}", fmt_num(loops)))
                                    .small()
                                    .color(p.text_muted),
                            );
                        }
                        let percent = fmt_percent(f64::from(share) * 100.0);
                        let metric = match (node.self_time_ms, node.time_ms, node.cost) {
                            (Some(own), Some(total), _) => format!("self {} · total {} · {percent}", fmt_ms(own), fmt_ms(total)),
                            (_, Some(total), _) => format!("total {} · {percent}", fmt_ms(total)),
                            (_, _, Some((_, cost))) => format!("cost {} · {percent}", fmt_num(cost)),
                            _ => String::new(),
                        };
                        ui.label(RichText::new(metric).small().color(if node.time_ms.is_some() { p.text } else { p.text_muted }))
                            .on_hover_text("Self: time in this operator, excluding children. Total: time including children. Both include all loops. Estimated costs are relative units, not milliseconds.");
                    });
                });
            }
            if total > 0.0 {
                let width = ui.available_width();
                    let (bar, _) = ui.allocate_exact_size(vec2(width, 3.0), Sense::hover());
                    ui.painter().rect_filled(bar, 2.0, p.bg_subtle);
                    let mut filled = bar;
                    filled.set_width(bar.width() * share);
                    ui.painter().rect_filled(filled, 2.0, color);
            }
        });
    let rect = card.response.rect;
    let response = ui.interact(rect, Id::new(("plan-card", path.clone())), Sense::click());
    response.widget_info(|| {
        WidgetInfo::labeled(WidgetType::Button, true, format!("Select {}", node.title()))
    });
    if response.clicked() {
        state.selected = Some(path.clone());
    }
    let _ = response.clone().on_hover_cursor(CursorIcon::PointingHand);
    if state.scroll_to.as_deref() == Some(path.as_slice()) {
        response.scroll_to_me(Some(Align::Center));
        state.scroll_to = None;
    }
    if !node.children.is_empty() {
        let toggle = Rect::from_center_size(
            pos2(rect.left() + INDENT / 2.0, rect.bottom() + 7.0),
            vec2(15.0, 15.0),
        );
        let toggle_response = ui.interact(
            toggle,
            Id::new(("plan-collapse", path.clone())),
            Sense::click(),
        );
        let count = node.children.len();
        let verb = if collapsed { "Expand" } else { "Collapse" };
        toggle_response.widget_info(|| {
            WidgetInfo::labeled(
                WidgetType::Button,
                true,
                format!("{verb} {count} children of {}", node.title()),
            )
        });
        let hovered = toggle_response.hovered();
        ui.painter().circle_filled(
            toggle.center(),
            7.0,
            if hovered { p.hover } else { p.bg_panel },
        );
        ui.painter().circle_stroke(
            toggle.center(),
            7.0,
            Stroke::new(1.0, if hovered { p.accent } else { p.border_input }),
        );
        ui.painter().text(
            toggle.center(),
            egui::Align2::CENTER_CENTER,
            if collapsed {
                icons::CARET_RIGHT
            } else {
                icons::CARET_DOWN
            },
            icons::font(ui, 10.0),
            if hovered { p.accent } else { p.text_muted },
        );
        if toggle_response.clicked() {
            if collapsed {
                state.collapsed.remove(path);
            } else {
                state.collapsed.insert(path.clone());
            }
        }
    }

    if collapsed || node.children.is_empty() {
        return rect;
    }

    let children = ui.indent(("plan-children", path.clone()), |ui| {
        let mut rects = Vec::new();
        for (i, child) in node.children.iter().enumerate() {
            ui.add_space(VGAP);
            path.push(i);
            rects.push(draw_node(ui, child, path, hottest, total, state));
            path.pop();
        }
        rects
    });
    let children = children.inner;
    let spine_x = rect.left() + INDENT / 2.0;
    let last = children
        .last()
        .map(|r| r.center().y)
        .unwrap_or(rect.bottom());
    ui.painter().line_segment(
        [pos2(spine_x, rect.bottom() + 15.0), pos2(spine_x, last)],
        Stroke::new(1.0, p.border_input),
    );
    let selected = state.selected.clone();
    for (i, child) in children.iter().enumerate() {
        let mut child_path = path.clone();
        child_path.push(i);
        let on_selected = selected
            .as_deref()
            .is_some_and(|s| s.starts_with(&child_path));
        let color = if on_selected {
            p.accent
        } else {
            p.border_input
        };
        let y = child.center().y;
        ui.painter().line_segment(
            [pos2(spine_x, y), pos2(child.left(), y)],
            Stroke::new(1.0, color),
        );
    }
    rect
}

fn inspector(
    ui: &mut Ui,
    p: &Palette,
    plan: &PlanTree,
    insights: &PlanInsights,
    state: &PlanViewState,
) {
    ScrollArea::vertical()
        .id_salt("plan-inspector")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let path = state.selected.clone().unwrap_or_default();
            let node = plan.node(&path).unwrap_or(&plan.root);
            ui.horizontal(|ui| {
                let (icon, color) = role_icon(node.role());
                icons::paint(ui, icon, color(p));
                ui.add(
                    Label::new(RichText::new(node.title()).font(theme::semibold(ui.ctx(), 13.0)))
                        .truncate(),
                )
                .on_hover_text(node.title());
            });
            if state.selected.is_some() && !path.is_empty() {
                let mut trail = Vec::new();
                for end in 0..=path.len() {
                    if let Some(n) = plan.node(&path[..end]) {
                        trail.push(n.title());
                    }
                }
                theme::hint(ui, &trail.join(" › "));
            } else {
                theme::hint(ui, "Select a node to inspect it");
            }
            ui.add_space(Spacing::S);
            theme::hairline(ui);
            ui.add_space(Spacing::S);
            let row = |ui: &mut Ui, name: &str, value: String| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = Spacing::S;
                    ui.label(RichText::new(name).small().color(p.text_muted));
                    ui.label(RichText::new(value).small().monospace());
                });
            };
            if let Some((start, total)) = node.cost {
                row(
                    ui,
                    "Cost",
                    format!("{} .. {}", fmt_num(start), fmt_num(total)),
                );
            }
            if let Some(rows) = node.rows_estimate {
                row(ui, "Rows (estimated)", fmt_num(rows));
            }
            if let Some(rows) = node.rows_actual {
                row(ui, "Rows (actual)", fmt_num(rows));
            }
            if let Some(loops) = node.loops {
                row(ui, "Loops", fmt_num(loops));
            }
            if let Some(time) = node.time_ms {
                row(ui, "Time (inclusive)", fmt_ms(time));
            }
            if let Some(time) = node.self_time_ms {
                row(ui, "Time (this node)", fmt_ms(time));
            }
            let node_issues: Vec<&PlanIssue> = insights
                .issues
                .iter()
                .filter(|i| i.path == path && i.level == PlanIssueLevel::Warning)
                .collect();
            if !node_issues.is_empty() {
                ui.add_space(Spacing::S);
                for issue in node_issues {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = Spacing::S;
                        icons::paint(ui, icons::WARNING, p.warning);
                        ui.label(RichText::new(&issue.message).small().color(p.warning));
                    });
                }
            }
            if !node.extra.is_empty() {
                ui.add_space(Spacing::S);
                theme::hairline(ui);
                ui.add_space(Spacing::S);
                for (name, value) in &node.extra {
                    row(ui, name, value.clone());
                }
            }
            if !node.buffers.is_empty() {
                ui.add_space(Spacing::S);
                theme::hairline(ui);
                ui.add_space(Spacing::S);
                for (name, value) in &node.buffers {
                    row(ui, name, fmt_num(*value as f64));
                }
            }
        });
}

fn hot_badge(ui: &mut Ui, p: &Palette, rank: usize) {
    let color = if rank == 0 { p.error } else { p.warning };
    Frame::new()
        .fill(color.gamma_multiply(0.18))
        .corner_radius(CornerRadius::same(3))
        .inner_margin(Margin::symmetric(4, 0))
        .show(ui, |ui| {
            ui.label(
                RichText::new(format!("HOT {}", rank + 1))
                    .small()
                    .color(color),
            );
        });
}

/// `(icon, palette color)` of an operator family.
fn role_icon(role: PlanRole) -> (&'static str, fn(&Palette) -> Color32) {
    match role {
        PlanRole::Join => (icons::DUPLICATE, |p| p.icon_view),
        PlanRole::Scan => (icons::TABLE, |p| p.icon_table),
        PlanRole::Sort => (icons::ROWS, |p| p.warning),
        PlanRole::Aggregate => (icons::FUNCTION, |p| p.icon_function),
        PlanRole::Other => (icons::MORE, |p| p.text_muted),
    }
}

fn metric(ui: &mut Ui, p: &Palette, name: &str, value: String) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.label(RichText::new(value).small().monospace());
        ui.label(RichText::new(name).small().color(p.text_muted));
    });
}

fn chip(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) {
    Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(3))
        .inner_margin(Margin::symmetric(5, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().color(fg));
        });
}

fn total_weight(plan: &PlanTree) -> f64 {
    fn walk(node: &PlanNode) -> f64 {
        node.weight() + node.children.iter().map(walk).sum::<f64>()
    }
    walk(&plan.root)
}

fn output_rows(plan: &PlanTree) -> Option<f64> {
    let root = &plan.root;
    root.rows_actual
        .map(|rows| rows * root.loops.unwrap_or(1.0).max(1.0))
        .or(root.rows_estimate)
}

fn rows_text(n: &PlanNode) -> String {
    match (n.rows_estimate, n.rows_actual) {
        (Some(e), Some(a)) => format!("rows {} est · {} act", fmt_num(e), fmt_num(a)),
        (Some(e), None) => format!("rows {} est", fmt_num(e)),
        (None, Some(a)) => format!("rows {} act", fmt_num(a)),
        _ => String::new(),
    }
}

fn row_ratio(n: &PlanNode) -> Option<(f64, bool)> {
    // Both the estimate and the actual count are per loop.
    let (estimate, actual) = (n.rows_estimate?, n.rows_actual?);
    let (estimate, actual) = (estimate.max(1.0), actual.max(1.0));
    let ratio = if actual >= estimate {
        actual / estimate
    } else {
        estimate / actual
    };
    (ratio >= 10.0).then_some((ratio, actual > estimate))
}

fn collapse_all(plan: &PlanTree, state: &mut PlanViewState) {
    fn walk(node: &PlanNode, path: &mut Vec<usize>, state: &mut PlanViewState) {
        if node.children.is_empty() {
            return;
        }
        state.collapsed.insert(path.clone());
        for (i, child) in node.children.iter().enumerate() {
            path.push(i);
            walk(child, path, state);
            path.pop();
        }
    }
    state.collapsed.clear();
    walk(&plan.root, &mut Vec::new(), state);
}

fn fmt_num(value: f64) -> String {
    let value = value.round() as i64;
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if value < 0 { format!("-{out}") } else { out }
}

fn fmt_ms(ms: f64) -> String {
    if ms >= 100.0 {
        format!("{ms:.0} ms")
    } else if ms >= 1.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{ms:.2} ms")
    }
}

fn fmt_percent(percent: f64) -> String {
    if percent > 0.0 && percent < 1.0 {
        "<1%".to_string()
    } else {
        format!("{percent:.0}%")
    }
}
