//! The status bar at the bottom of a result: the pending changes with Preview, Apply and
//! Discard, "Add row", and Export on the right; or, for a result that cannot be edited, the
//! one line that says why.

use egui::{Align, Layout, Margin, RichText, Stroke};

use crate::theme::{self, Spacing, icons};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangesAction {
    Preview,
    Apply,
    Discard,
}

/// What the result bar asks of its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultBarAction {
    Changes(ChangesAction),
    /// Append a row of defaults to the pending changes.
    AddRow,
    Export,
}

/// What the result bar shows this frame.
#[derive(Clone, Copy, Debug)]
pub struct ResultBarView<'a> {
    /// The pending changes of the result.
    pub pending: usize,
    /// A changeset is being applied.
    pub applying: bool,
    /// The grid has an edit buffer, so rows can be added and changes applied.
    pub editable: bool,
    /// The result has rows to export.
    pub exportable: bool,
    /// Why the result cannot be edited, when it cannot.
    pub read_only: Option<&'a str>,
}

/// The frame of a result bar: the panel fill, with no vertical margin so the bar sits at
/// `Spacing::STATUS`; [`result_bar`] paints the hairline that closes it against the grid.
pub fn result_bar_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::new()
        .fill(theme::palette(ctx).bg_panel)
        .inner_margin(Margin {
            left: Spacing::M as i8,
            right: Spacing::M as i8,
            top: 0,
            bottom: 0,
        })
}

/// The pending changes with their actions, "Add row", and Export on the right. The buttons
/// are disabled while a changeset is being applied or when nothing is pending. A read-only
/// result shows the reason instead of the edit actions.
pub fn result_bar(ui: &mut egui::Ui, view: &ResultBarView<'_>) -> Option<ResultBarAction> {
    let palette = theme::palette(ui.ctx());
    let mut action = None;
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        if let Some(reason) = view.read_only {
            icons::paint(ui, icons::READ_ONLY, palette.text_muted);
            ui.label(RichText::new(format!("Read-only: {reason}")).color(palette.text_muted));
        } else {
            let text = match view.pending {
                0 => "No pending changes".to_string(),
                1 => "1 pending change".to_string(),
                n => format!("{n} pending changes"),
            };
            let color = if view.pending > 0 {
                palette.warning
            } else {
                palette.text_muted
            };
            let (icon, icon_color) = if view.pending > 0 {
                (icons::WARNING, palette.warning)
            } else {
                (icons::SUCCESS, palette.text_muted)
            };
            icons::paint(ui, icon, icon_color);
            ui.label(RichText::new(text).color(color));
            if view.applying {
                ui.spinner();
                ui.label(RichText::new("Applying…").color(palette.text_muted));
            }
            let enabled = view.pending > 0 && !view.applying && view.editable;
            for (icon, color, label, changes, tooltip) in [
                (
                    icons::PREVIEW,
                    None,
                    "SQL",
                    ChangesAction::Preview,
                    "Show the SQL of the pending changes",
                ),
                (
                    icons::COMMIT,
                    Some(palette.success),
                    "Apply",
                    ChangesAction::Apply,
                    "Run the pending changes in a transaction",
                ),
                (
                    icons::ROLLBACK,
                    None,
                    "Discard",
                    ChangesAction::Discard,
                    "Drop the pending changes",
                ),
            ] {
                if icons::button(ui, enabled, icon, color, label)
                    .on_hover_text(tooltip)
                    .clicked()
                {
                    action = Some(ResultBarAction::Changes(changes));
                }
            }
            if view.editable {
                icons::toolbar_divider(ui);
                let add = icons::button(
                    ui,
                    !view.applying,
                    icons::ADD,
                    Some(palette.success),
                    "Add row",
                )
                .on_hover_text("Append a row of defaults to the pending changes");
                if add.clicked() {
                    action = Some(ResultBarAction::AddRow);
                }
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = Spacing::S;
            let export = icons::button(ui, view.exportable, icons::EXPORT, None, "Export…");
            if export.clicked() {
                action = Some(ResultBarAction::Export);
            }
        });
        // Close the bar against the grid.
        let rect = ui.max_rect();
        ui.painter().hline(
            rect.x_range(),
            rect.top() + 0.5,
            Stroke::new(1.0, palette.border),
        );
    });
    action
}

/// "Read-only: <reason>", for a result drawn without the bar.
pub fn read_only_bar(ui: &mut egui::Ui, reason: &str) {
    let muted = theme::palette(ui.ctx()).text_muted;
    ui.label(RichText::new(format!("Read-only: {reason}")).color(muted));
}
