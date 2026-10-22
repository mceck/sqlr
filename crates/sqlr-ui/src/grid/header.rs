//! The sticky header (column names, types and sort arrows) and the row-number gutter.

use egui::{Align2, FontId, Pos2, Rect, Stroke, pos2, vec2};
use sqlr_core::ColumnMeta;

use crate::theme::Palette;

pub(crate) const HEADER_HEIGHT: f32 = 40.0;

/// Where a pointer position in the grid's content falls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hit {
    /// The corner above the row numbers.
    Corner,
    Header(usize),
    /// A row number, by view row.
    Gutter(usize),
    Cell(usize, usize),
}

/// The grid's geometry for one frame, in content coordinates.
pub(crate) struct Geometry<'a> {
    pub origin: Pos2,
    pub viewport: Rect,
    /// Width of the row-number gutter.
    pub gutter: f32,
    /// `col_x[i]` is the left edge of column `i` after the gutter; the last entry is the
    /// total width of the columns.
    pub col_x: &'a [f32],
    pub row_height: f32,
    pub view_rows: usize,
}

impl Geometry<'_> {
    pub fn columns(&self) -> usize {
        self.col_x.len().saturating_sub(1)
    }

    pub fn hit(&self, pos: Pos2) -> Option<Hit> {
        let rel = pos - self.origin;
        if rel.x < 0.0 || rel.y < 0.0 {
            return None;
        }
        let in_header = rel.y < self.viewport.min.y + HEADER_HEIGHT;
        let in_gutter = rel.x < self.viewport.min.x + self.gutter;
        let col = (!in_gutter)
            .then(|| self.col_x.partition_point(|&x| x <= rel.x - self.gutter))
            .and_then(|p| p.checked_sub(1))
            .filter(|&c| c < self.columns());
        let row = (!in_header)
            .then(|| ((rel.y - HEADER_HEIGHT) / self.row_height) as usize)
            .filter(|&r| r < self.view_rows);
        match (in_header, in_gutter) {
            (true, true) => Some(Hit::Corner),
            (true, false) => col.map(Hit::Header),
            (false, true) => row.map(Hit::Gutter),
            (false, false) => Some(Hit::Cell(row?, col?)),
        }
    }

    /// The screen rect of a cell, by view row.
    pub fn cell_rect(&self, row: usize, col: usize) -> Rect {
        let y = self.origin.y + HEADER_HEIGHT + row as f32 * self.row_height;
        let x = self.origin.x + self.gutter;
        Rect::from_min_max(
            pos2(x + self.col_x[col], y),
            pos2(x + self.col_x[col + 1], y + self.row_height),
        )
    }

    /// The screen rect of a row number (pinned to the viewport's left edge).
    pub fn gutter_rect(&self, row: usize) -> Rect {
        let y = self.origin.y + HEADER_HEIGHT + row as f32 * self.row_height;
        let x = self.origin.x + self.viewport.min.x;
        Rect::from_min_size(pos2(x, y), vec2(self.gutter, self.row_height))
    }

    /// The header strip, pinned to the viewport's top.
    pub fn header_rect(&self) -> Rect {
        let top = self.origin.y + self.viewport.min.y;
        Rect::from_min_max(
            pos2(self.origin.x + self.viewport.min.x, top),
            pos2(self.origin.x + self.viewport.max.x, top + HEADER_HEIGHT),
        )
    }
}

/// Paints the header of columns `cols` with the sort arrows of `sort`, and gives each its
/// accessibility node (the column name).
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint(
    ui: &egui::Ui,
    id: egui::Id,
    geo: &Geometry<'_>,
    columns: &[ColumnMeta],
    cols: std::ops::Range<usize>,
    sort: &[(usize, bool)],
    palette: &Palette,
    fonts: (&FontId, &FontId),
) {
    let painter = ui.painter();
    let header = geo.header_rect();
    painter.rect_filled(header, 0.0, palette.bg_panel);
    painter.hline(
        header.x_range(),
        header.bottom(),
        Stroke::new(1.0, palette.border),
    );
    let (name_font, type_font) = fonts;
    let pinned_left = header.left() + geo.gutter;
    for col in cols {
        let meta = &columns[col];
        let left = geo.origin.x + geo.gutter + geo.col_x[col];
        let width = geo.col_x[col + 1] - geo.col_x[col];
        let cell = Rect::from_min_size(pos2(left, header.top()), vec2(width, HEADER_HEIGHT));
        let visible = cell.intersect(Rect::from_min_max(
            pos2(pinned_left, header.top()),
            header.max,
        ));
        if visible.width() <= 0.0 {
            continue;
        }
        let clipped = painter.with_clip_rect(visible);
        let arrow = sort.iter().position(|(c, _)| *c == col).map(|i| {
            let (_, desc) = sort[i];
            if desc { "▼" } else { "▲" }
        });
        let name = match arrow {
            Some(a) => format!("{} {a}", meta.name),
            None => meta.name.clone(),
        };
        clipped.text(
            pos2(cell.left() + super::CELL_PADDING, cell.top() + 12.0),
            Align2::LEFT_CENTER,
            name,
            name_font.clone(),
            palette.text,
        );
        clipped.text(
            pos2(cell.left() + super::CELL_PADDING, cell.top() + 28.0),
            Align2::LEFT_CENTER,
            &meta.type_name,
            type_font.clone(),
            palette.text_muted,
        );
        painter.vline(
            cell.right(),
            header.y_range(),
            Stroke::new(1.0, palette.border),
        );
        ui.ctx()
            .accesskit_node_builder(id.with(("header", col)), |node| {
                node.set_role(egui::accesskit::Role::ColumnHeader);
                node.set_label(meta.name.as_str());
                node.set_bounds(bounds(visible));
            });
    }
    // The corner above the row numbers.
    let corner = Rect::from_min_size(header.min, vec2(geo.gutter, HEADER_HEIGHT));
    painter.rect_filled(corner, 0.0, palette.bg_panel);
    painter.vline(
        corner.right(),
        header.y_range(),
        Stroke::new(1.0, palette.border),
    );
}

pub(crate) fn bounds(rect: Rect) -> egui::accesskit::Rect {
    egui::accesskit::Rect {
        x0: rect.min.x.into(),
        y0: rect.min.y.into(),
        x1: rect.max.x.into(),
        y1: rect.max.y.into(),
    }
}
