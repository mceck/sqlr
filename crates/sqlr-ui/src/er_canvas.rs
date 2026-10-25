//! The ER diagram canvas (spec §8.6): boxes dragged with the mouse, pan and zoom, edges
//! with crow's feet. Boxes and edges outside the view are not painted, so a schema of
//! hundreds of tables stays smooth.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use sqlr_core::{ErEdge, ErLayout, ErModel, ErTable};

use crate::theme::{self, Spacing};

const HEADER: f32 = 26.0;
const LINE: f32 = 18.0;
/// Columns shown before "+N more".
const COLLAPSE_AFTER: usize = 25;

/// Which columns a box lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnsShown {
    #[default]
    All,
    /// Primary and foreign keys only.
    Keys,
    None,
}

/// The size of a table's box for `mode`: wide enough for its longest line.
pub fn er_measure(t: &ErTable, mode: ColumnsShown) -> (f32, f32) {
    let shown = visible_columns(t, mode).len();
    let extra =
        usize::from(visible_columns(t, mode).len() < t.columns.len() && mode == ColumnsShown::All);
    let widest = t
        .columns
        .iter()
        .map(|c| c.name.len() + c.type_name.len() + 6)
        .chain(std::iter::once(t.table.name.len() + 4))
        .max()
        .unwrap_or(10);
    (
        (widest as f32 * 7.2 + 70.0).max(140.0),
        HEADER + LINE * (shown + extra) as f32 + 6.0,
    )
}

fn visible_columns(t: &ErTable, mode: ColumnsShown) -> Vec<usize> {
    match mode {
        ColumnsShown::None => Vec::new(),
        ColumnsShown::Keys => (0..t.columns.len())
            .filter(|&i| t.columns[i].pk || t.columns[i].fk)
            .collect(),
        ColumnsShown::All => (0..t.columns.len().min(COLLAPSE_AFTER)).collect(),
    }
}

/// Pan, zoom and selection of one diagram.
#[derive(Clone, Debug, PartialEq)]
pub struct ErView {
    pub pan: Vec2,
    pub zoom: f32,
    pub selected: Option<usize>,
    pub columns: ColumnsShown,
    /// Where the diagram's origin is on screen, as of the last frame.
    pub screen_origin: Pos2,
    /// Boxes and edges painted in the last frame.
    pub painted: usize,
    pub painted_edges: usize,
    /// The table being dragged.
    dragging: Option<usize>,
    /// Fit the whole diagram into the canvas on the next frame.
    pub fit: bool,
}

impl Default for ErView {
    fn default() -> Self {
        Self {
            pan: vec2(20.0, 20.0),
            zoom: 1.0,
            selected: None,
            columns: ColumnsShown::All,
            screen_origin: Pos2::ZERO,
            painted: 0,
            painted_edges: 0,
            dragging: None,
            fit: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ErAction {
    /// A table was dragged to this position (it is already in the layout).
    Moved(usize, (f32, f32)),
    /// A table was double-clicked.
    OpenTable(usize),
    ExportPng,
    ExportSvg,
    Relayout,
}

fn box_rect(layout: &ErLayout, i: usize) -> Rect {
    let (x, y) = layout.positions[i];
    let (w, h) = layout.sizes[i];
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

/// Draws the diagram; returns what the user did. `layout` is moved in place by dragging.
pub fn er_canvas(
    ui: &mut egui::Ui,
    model: &ErModel,
    layout: &mut ErLayout,
    view: &mut ErView,
) -> Option<ErAction> {
    let p = theme::palette(ui.ctx());
    let mut action = None;
    ui.horizontal(|ui| {
        if ui.button("Relayout").clicked() {
            action = Some(ErAction::Relayout);
        }
        if ui.button("Fit").clicked() {
            view.fit = true;
        }
        ui.label(format!("{:.0}%", view.zoom * 100.0));
        for (mode, label) in [
            (ColumnsShown::All, "All columns"),
            (ColumnsShown::Keys, "Keys only"),
            (ColumnsShown::None, "No columns"),
        ] {
            if ui.selectable_label(view.columns == mode, label).clicked() && view.columns != mode {
                view.columns = mode;
                action = Some(ErAction::Relayout);
            }
        }
        if ui.button("Export SVG").clicked() {
            action = Some(ErAction::ExportSvg);
        }
        if ui.button("Export PNG").clicked() {
            action = Some(ErAction::ExportPng);
        }
    });
    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let canvas = response.rect;
    // The canvas is a framed surface, so it separates from the page in both themes.
    painter.rect_filled(canvas, Spacing::RADIUS_M, p.bg_editor);
    painter.rect_stroke(
        canvas,
        Spacing::RADIUS_M,
        Stroke::new(1.0, p.border_input),
        egui::StrokeKind::Inside,
    );
    if view.fit && !layout.positions.is_empty() {
        view.fit = false;
        let (mut w, mut h) = (1f32, 1f32);
        for i in 0..layout.positions.len() {
            let r = box_rect(layout, i);
            w = w.max(r.max.x);
            h = h.max(r.max.y);
        }
        view.zoom = ((canvas.width() - 40.0) / w)
            .min((canvas.height() - 40.0) / h)
            .clamp(0.1, 1.5);
        view.pan = vec2(20.0, 20.0);
    }
    // Zoom with the scroll wheel while hovering the canvas, around the pointer.
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let old = view.zoom;
            view.zoom = (view.zoom * (1.0 + scroll * 0.002)).clamp(0.1, 3.0);
            if let Some(pointer) = response.hover_pos() {
                let rel = pointer - canvas.min - view.pan;
                view.pan -= rel * (view.zoom / old - 1.0);
            }
        }
    }
    let origin = canvas.min + view.pan;
    view.screen_origin = origin;
    let zoom = view.zoom;
    let to_screen = |x: f32, y: f32| pos2(origin.x + x * zoom, origin.y + y * zoom);

    // Dragging: a table under the pointer moves, the background pans.
    if response.drag_started()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        view.dragging = (0..layout.positions.len()).rev().find(|&i| {
            let r = box_rect(layout, i);
            Rect::from_min_max(to_screen(r.min.x, r.min.y), to_screen(r.max.x, r.max.y))
                .contains(pointer)
        });
        view.selected = view.dragging;
    }
    if response.dragged() {
        let delta = response.drag_delta();
        match view.dragging {
            Some(i) => {
                let (x, y) = layout.positions[i];
                layout.positions[i] = (x + delta.x / zoom, y + delta.y / zoom);
            }
            None => view.pan += delta,
        }
    }
    if response.drag_stopped()
        && let Some(i) = view.dragging.take()
    {
        action = Some(ErAction::Moved(i, layout.positions[i]));
    }
    if response.double_clicked()
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(i) = (0..layout.positions.len()).rev().find(|&i| {
            let r = box_rect(layout, i);
            Rect::from_min_max(to_screen(r.min.x, r.min.y), to_screen(r.max.x, r.max.y))
                .contains(pointer)
        })
    {
        action = Some(ErAction::OpenTable(i));
    }

    let visible = canvas.expand(40.0);
    let on_screen = |r: Rect| {
        visible.intersects(Rect::from_min_max(
            to_screen(r.min.x, r.min.y),
            to_screen(r.max.x, r.max.y),
        ))
    };
    // Edges first, under the boxes.
    let stroke = Stroke::new(1.2, p.accent);
    view.painted_edges = 0;
    for e in &model.edges {
        let (Some(_), Some(_)) = (layout.positions.get(e.from), layout.positions.get(e.to)) else {
            continue;
        };
        let (a, b) = (box_rect(layout, e.from), box_rect(layout, e.to));
        if !on_screen(a.union(b)) {
            continue;
        }
        view.painted_edges += 1;
        paint_edge(&painter, e, a, b, &to_screen, zoom, stroke);
    }
    view.painted = 0;
    let detail = zoom >= 0.45;
    for (i, t) in model.tables.iter().enumerate() {
        let r = box_rect(layout, i);
        if !on_screen(r) {
            continue;
        }
        view.painted += 1;
        let screen = Rect::from_min_max(to_screen(r.min.x, r.min.y), to_screen(r.max.x, r.max.y));
        let selected = view.selected == Some(i);
        painter.rect_filled(screen, 4.0, p.bg_panel);
        painter.rect_stroke(
            screen,
            4.0,
            Stroke::new(
                if selected { 2.0 } else { 1.0 },
                if selected { p.accent } else { p.border },
            ),
            egui::StrokeKind::Inside,
        );
        let header = Rect::from_min_size(screen.min, vec2(screen.width(), HEADER * zoom));
        painter.rect_filled(header, 4.0, p.bg_subtle);
        if detail {
            let font = FontId::proportional((13.0 * zoom).max(8.0));
            painter.text(
                header.left_center() + vec2(8.0 * zoom, 0.0),
                Align2::LEFT_CENTER,
                &t.table.name,
                font.clone(),
                p.text,
            );
            let small = FontId::monospace((11.0 * zoom).max(7.0));
            let shown = visible_columns(t, view.columns);
            for (k, &ci) in shown.iter().enumerate() {
                let c = &t.columns[ci];
                let y = screen.min.y
                    + (HEADER + LINE * k as f32 + LINE / 2.0) * zoom
                    + HEADER * zoom * 0.0
                    + 3.0 * zoom;
                let mark = match (c.pk, c.fk) {
                    (true, true) => "PK FK",
                    (true, false) => "PK",
                    (false, true) => "FK",
                    _ => "",
                };
                let colour = if c.pk || c.fk { p.icon_key } else { p.text };
                painter.text(
                    pos2(screen.min.x + 6.0 * zoom, y),
                    Align2::LEFT_CENTER,
                    mark,
                    small.clone(),
                    p.text_muted,
                );
                painter.text(
                    pos2(screen.min.x + 44.0 * zoom, y),
                    Align2::LEFT_CENTER,
                    &c.name,
                    small.clone(),
                    colour,
                );
                painter.text(
                    pos2(screen.max.x - 6.0 * zoom, y),
                    Align2::RIGHT_CENTER,
                    &c.type_name,
                    small.clone(),
                    p.text_muted,
                );
            }
            if view.columns == ColumnsShown::All && t.columns.len() > shown.len() {
                let y = screen.min.y
                    + (HEADER + LINE * shown.len() as f32 + LINE / 2.0) * zoom
                    + 3.0 * zoom;
                painter.text(
                    pos2(screen.min.x + 6.0 * zoom, y),
                    Align2::LEFT_CENTER,
                    format!("+{} more", t.columns.len() - shown.len()),
                    small,
                    p.text_muted,
                );
            }
        }
    }
    let _ = Color32::TRANSPARENT;
    action
}

fn paint_edge(
    painter: &egui::Painter,
    e: &ErEdge,
    a: Rect,
    b: Rect,
    to_screen: &dyn Fn(f32, f32) -> Pos2,
    zoom: f32,
    stroke: Stroke,
) {
    if e.from == e.to {
        let start = to_screen(a.max.x, a.min.y + HEADER / 2.0);
        let out = vec2(20.0 * zoom, 0.0);
        let down = vec2(0.0, 24.0 * zoom);
        painter.line_segment([start, start + out], stroke);
        painter.line_segment([start + out, start + out + down], stroke);
        painter.line_segment([start + out + down, start + down], stroke);
        return;
    }
    let (x1, x2) = if a.min.x <= b.min.x {
        (a.max.x, b.min.x)
    } else {
        (a.min.x, b.max.x)
    };
    let (y1, y2) = (a.center().y, b.center().y);
    let mid = (x1 + x2) / 2.0;
    let pts = [
        to_screen(x1, y1),
        to_screen(mid, y1),
        to_screen(mid, y2),
        to_screen(x2, y2),
    ];
    for w in pts.windows(2) {
        painter.line_segment([w[0], w[1]], stroke);
    }
    let dir = if x2 > x1 { 1.0 } else { -1.0 };
    // The referencing side: a crow's foot (many) whose prongs open onto the table, or a
    // single bar when the key is unique (one).
    let edge = pts[0];
    let joint = edge + vec2(dir * 12.0 * zoom, 0.0);
    if e.unique {
        let bar = edge + vec2(dir * 6.0 * zoom, 0.0);
        painter.line_segment(
            [bar + vec2(0.0, -6.0 * zoom), bar + vec2(0.0, 6.0 * zoom)],
            stroke,
        );
    } else {
        for spread in [-6.0, 0.0, 6.0] {
            painter.line_segment([joint, edge + vec2(0.0, spread * zoom)], stroke);
        }
    }
    // The referenced side: one row, a bar across the line.
    let end = pts[3];
    let bar = end - vec2(dir * 6.0 * zoom, 0.0);
    painter.line_segment(
        [bar + vec2(0.0, -6.0 * zoom), bar + vec2(0.0, 6.0 * zoom)],
        stroke,
    );
}
