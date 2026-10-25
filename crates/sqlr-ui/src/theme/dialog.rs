//! The one dialog every screen uses: a centered modal over a dimmed backdrop, a header
//! (icon badge, title, optional subtitle, close button), an inset body that scrolls with a
//! visible cue when it overflows, and a footer with secondary actions on the left and
//! Cancel + the primary action on the right.
//!
//! ```ignore
//! let response = Dialog::new("export", icons::EXPORT, "Export orders")
//!     .subtitle("Choose the format of the file")
//!     .show(ctx, |d| {
//!         d.body(|ui| { /* the form */ });
//!         d.footer(|f| {
//!             // The right group fills from the right edge: the primary first, then Cancel.
//!             if f.primary("Export", None) { /* … */ }
//!             f.cancel();
//!         });
//!     });
//! if response.closed { /* ✕, Escape or Cancel */ }
//! ```
//!
//! Keyboard: Escape (or ✕, or Cancel) closes; Enter presses the enabled primary when no
//! widget holds the keyboard focus (a single-line field gives it up on Enter). A dialog with
//! a [`DialogFooter::danger`] primary focuses Cancel when it opens.

use std::hash::Hash;

use egui::{
    Align, Align2, Color32, CornerRadius, Id, Key, Layout, Margin, Modifiers, RichText, Sense, Ui,
    UiBuilder, accesskit, vec2,
};

use super::{Spacing, hairline, icons, palette, semibold};

/// Room kept between a dialog and the window edges.
const SCREEN_MARGIN: f32 = 24.0;
/// The body's inset.
const BODY_INSET: i8 = 16;
/// The header and footer heights assumed before the first frame measures them.
const CHROME_ESTIMATE: f32 = 128.0;
/// The height of the fade that marks more content above or below a scrolled body.
const FADE: f32 = 22.0;

/// A dialog under construction; see the module docs.
#[must_use = "call `show` to draw the dialog"]
pub struct Dialog<'a> {
    id: Id,
    icon: &'a str,
    title: &'a str,
    subtitle: Option<&'a str>,
    width: f32,
    min_body_height: f32,
    body_height: Option<f32>,
    body_margin: Margin,
    modal: bool,
    dismiss_on_backdrop: bool,
    danger: bool,
}

/// What a dialog did this frame.
pub struct DialogResponse<R> {
    /// The value the contents returned.
    pub inner: R,
    /// The user dismissed the dialog: ✕, Escape, the Cancel button, or a click on the
    /// backdrop when [`Dialog::dismiss_on_backdrop`] is set.
    pub closed: bool,
}

impl<'a> Dialog<'a> {
    /// A dialog titled `title` ("Export orders", "Delete connection") with `icon` in its
    /// header badge. `id_salt` must be stable while the dialog is open.
    pub fn new(id_salt: impl Hash + std::fmt::Debug, icon: &'a str, title: &'a str) -> Self {
        Self {
            id: Id::new(("sqlr_dialog", id_salt)),
            icon,
            title,
            subtitle: None,
            width: 480.0,
            min_body_height: 0.0,
            body_height: None,
            body_margin: Margin::same(BODY_INSET),
            modal: true,
            dismiss_on_backdrop: false,
            danger: false,
        }
    }

    /// One line under the title.
    pub fn subtitle(mut self, subtitle: &'a str) -> Self {
        self.subtitle = Some(subtitle);
        self
    }

    /// The preferred width; a smaller window narrows it.
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// The body never gets shorter than this (unless the window is): a scrolled body keeps
    /// room for a few rows.
    pub fn min_body_height(mut self, height: f32) -> Self {
        self.min_body_height = height;
        self
    }

    /// A body of exactly `height` (bounded by the window) that lays out its own panels and
    /// scrolling: list/detail dialogs such as Preferences. Without it the body takes its
    /// content's height and scrolls past the window's.
    pub fn body_height(mut self, height: f32) -> Self {
        self.body_height = Some(height);
        self
    }

    /// The body's inset; the default is 16 px. List/detail dialogs whose panels reach the
    /// edges use [`Margin::ZERO`].
    pub fn body_margin(mut self, margin: Margin) -> Self {
        self.body_margin = margin;
        self
    }

    /// A dialog that leaves the window usable behind it: no backdrop, movable, and Escape
    /// does not close it. For viewers that sit next to the work (diagnostics).
    pub fn non_modal(mut self) -> Self {
        self.modal = false;
        self
    }

    /// A click on the backdrop dismisses the dialog: for confirmations that hold no input.
    pub fn dismiss_on_backdrop(mut self) -> Self {
        self.dismiss_on_backdrop = true;
        self
    }

    /// Tints the header badge with the error color: the dialog confirms a destructive action.
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// Draws the dialog. `add_contents` calls [`DialogUi::body`] and then, optionally,
    /// [`DialogUi::footer`].
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        add_contents: impl FnOnce(&mut DialogUi<'_>) -> R,
    ) -> DialogResponse<R> {
        let screen = ctx.content_rect();
        let width = self
            .width
            .min(screen.width() - 2.0 * SCREEN_MARGIN)
            .max(200.0_f32.min(screen.width()));
        let frame_nr = ctx.cumulative_frame_nr();
        let seen_id = self.id.with("seen");
        let last_seen: Option<u64> = ctx.data(|d| d.get_temp(seen_id));
        let first_frame = last_seen.is_none_or(|n| n + 1 < frame_nr);
        ctx.data_mut(|d| d.insert_temp(seen_id, frame_nr));
        let chrome_id = self.id.with("chrome");
        let chrome: f32 = ctx
            .data(|d| d.get_temp(chrome_id))
            .unwrap_or(CHROME_ESTIMATE);
        let max_body = (screen.height() - 2.0 * SCREEN_MARGIN - chrome).max(60.0);

        let p = palette(ctx);
        let frame = egui::Frame::window(&ctx.global_style())
            .fill(p.bg_raised)
            .inner_margin(Margin::ZERO);
        let modal = self.modal;
        let dismiss_on_backdrop = self.dismiss_on_backdrop;
        let id = self.id;
        let contents = |ui: &mut Ui, is_top: bool| {
            ui.set_width(width);
            ui.set_max_width(width);
            let top = ui.cursor().top();
            let close = header_ui(ui, &self, self.danger);
            let mut d = DialogUi {
                ui,
                id,
                width,
                max_body,
                min_body: self.min_body_height,
                body_height: self.body_height,
                body_margin: self.body_margin,
                closed: close,
                keyboard: is_top && !first_frame,
                first_frame,
                body_h: 0.0,
            };
            let inner = add_contents(&mut d);
            // Everything but the body: header, footer and any band between them.
            let measured = d.ui.min_rect().bottom() - top - d.body_h;
            label_dialog(d.ui, self.title);
            if (measured - chrome).abs() > 0.5 {
                ctx.data_mut(|data| data.insert_temp(chrome_id, measured));
                ctx.request_repaint();
            }
            (inner, d.closed)
        };

        if modal {
            let response = egui::Modal::new(id)
                .frame(frame)
                .backdrop_color(Color32::from_black_alpha(if p.is_dark() {
                    120
                } else {
                    80
                }))
                .show(ctx, |ui| {
                    let is_top = ui.ctx().memory(|m| m.top_modal_layer()) == Some(ui.layer_id());
                    contents(ui, is_top)
                });
            let (inner, mut closed) = response.inner;
            let escape = response.is_top_modal
                && !response.any_popup_open
                && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            if escape || (dismiss_on_backdrop && response.backdrop_response.clicked()) {
                closed = true;
            }
            DialogResponse { inner, closed }
        } else {
            let response = egui::Window::new(self.title)
                .id(id)
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .pivot(Align2::CENTER_CENTER)
                .default_pos(screen.center())
                .constrain_to(screen.shrink(8.0))
                .frame(frame)
                .show(ctx, |ui| contents(ui, false));
            match response.and_then(|r| r.inner) {
                Some((inner, closed)) => DialogResponse { inner, closed },
                // A collapsed window does not run its contents; ours never collapses.
                None => unreachable!("a dialog window always shows its contents"),
            }
        }
    }
}

/// Names the dialog's container for screen readers and tests.
fn label_dialog(ui: &Ui, title: &str) {
    let rect = ui.min_rect();
    ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
        node.set_role(accesskit::Role::Dialog);
        node.set_label(format!("{title} dialog"));
        node.set_bounds(accesskit::Rect {
            x0: rect.min.x.into(),
            y0: rect.min.y.into(),
            x1: rect.max.x.into(),
            y1: rect.max.y.into(),
        });
    });
}

/// The header: badge, title, subtitle and ✕. `true` when ✕ was clicked.
fn header_ui(ui: &mut Ui, dialog: &Dialog<'_>, danger: bool) -> bool {
    let p = palette(ui.ctx());
    let (badge, badge_fill) = if danger {
        (p.error, p.bg_raised.lerp_to_gamma(p.error, 0.18))
    } else {
        (p.accent, p.accent_soft)
    };
    let mut close = false;
    egui::Frame::new()
        .fill(p.bg_panel)
        .corner_radius(CornerRadius {
            nw: Spacing::RADIUS_M,
            ne: Spacing::RADIUS_M,
            sw: 0,
            se: 0,
        })
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = Spacing::M;
                let (rect, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 16.0, badge_fill);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    dialog.icon,
                    icons::font(ui, 16.0),
                    badge,
                );
                // The close button takes its room first, so a long title wraps before it.
                let close_w = 28.0;
                let text_w = (ui.available_width() - close_w - Spacing::M).max(40.0);
                ui.allocate_ui_with_layout(
                    vec2(text_w, 32.0),
                    Layout::top_down(Align::Min).with_main_align(Align::Center),
                    |ui| {
                        ui.set_min_height(32.0);
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.add(
                            egui::Label::new(
                                RichText::new(dialog.title).font(semibold(ui.ctx(), 15.0)),
                            )
                            .wrap(),
                        );
                        if let Some(subtitle) = dialog.subtitle {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(subtitle).small().color(p.text_muted),
                                )
                                .wrap(),
                            );
                        }
                    },
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = format!("Close {}", dialog.title);
                    if icons::icon_button(ui, icons::CLOSE, Some(p.text_muted), &label)
                        .on_hover_text("Close (Esc)")
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
        });
    hairline(ui);
    close
}

/// The inside of a dialog: its body, then its footer.
pub struct DialogUi<'u> {
    ui: &'u mut Ui,
    id: Id,
    width: f32,
    max_body: f32,
    min_body: f32,
    body_height: Option<f32>,
    body_margin: Margin,
    closed: bool,
    /// Enter may press the primary: the dialog is on top and was already open.
    keyboard: bool,
    first_frame: bool,
    /// The body's measured height, margins included.
    body_h: f32,
}

impl DialogUi<'_> {
    /// A band under the header that never scrolls (a wizard's step strip), closed by a
    /// hairline. Call it before [`DialogUi::body`].
    pub fn top<R>(&mut self, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
        let inner_w = self.width - 2.0 * f32::from(BODY_INSET);
        let inner = egui::Frame::new()
            .inner_margin(Margin::symmetric(BODY_INSET, 10))
            .show(self.ui, |ui| {
                ui.set_width(inner_w);
                add_contents(ui)
            })
            .inner;
        hairline(self.ui);
        inner
    }

    /// The body: inset, and scrolled with a fade cue when taller than the window allows.
    pub fn body<R>(&mut self, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
        let margin = self.body_margin;
        let inner_w = self.width - margin.sum().x;
        let max_h = (self.max_body - margin.sum().y).max(40.0);
        let fixed = self.body_height;
        let min_h = self.min_body.min(max_h);
        let id = self.id;
        let response = egui::Frame::new().inner_margin(margin).show(self.ui, |ui| {
            ui.set_width(inner_w);
            match fixed {
                Some(h) => {
                    let h = (h - margin.sum().y).min(max_h).max(min_h);
                    ui.allocate_ui_with_layout(
                        vec2(inner_w, h),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_min_size(vec2(inner_w, h));
                            ui.set_max_height(h);
                            add_contents(ui)
                        },
                    )
                    .inner
                }
                None => scroll_body(ui, id.with("body"), min_h, max_h, add_contents),
            }
        });
        self.body_h = response.response.rect.height();
        response.inner
    }

    /// The footer: a hairline and one row of actions. See [`DialogFooter`].
    pub fn footer<R>(&mut self, add_contents: impl FnOnce(&mut DialogFooter) -> R) -> R {
        let p = palette(self.ui.ctx());
        hairline(self.ui);
        let keyboard = self.keyboard;
        let first_frame = self.first_frame;
        let (inner, closed) = egui::Frame::new()
            .fill(p.bg_panel)
            .corner_radius(CornerRadius {
                nw: 0,
                ne: 0,
                sw: Spacing::RADIUS_M,
                se: Spacing::RADIUS_M,
            })
            .inner_margin(Margin::symmetric(16, 12))
            .show(self.ui, |ui| {
                ui.spacing_mut().item_spacing.x = Spacing::M;
                let row_h = ui.spacing().interact_size.y.max(26.0);
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                let left = ui.new_child(
                    UiBuilder::new()
                        .id_salt("dialog_footer_left")
                        .max_rect(rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                let right = ui.new_child(
                    UiBuilder::new()
                        .id_salt("dialog_footer_right")
                        .max_rect(rect)
                        .layout(Layout::right_to_left(Align::Center)),
                );
                let enter = keyboard
                    && ui.ctx().memory(|m| m.focused().is_none())
                    && !egui::Popup::is_any_open(ui.ctx())
                    && ui.input(|i| i.key_pressed(Key::Enter) && i.modifiers.is_none());
                let mut footer = DialogFooter {
                    left,
                    right,
                    enter,
                    reason: None,
                    danger: false,
                    cancel: None,
                    focus_cancel: false,
                    closed: false,
                };
                let inner = add_contents(&mut footer);
                footer.finish(first_frame);
                // Keep the row tall enough for what the groups drew.
                let used = footer.left.min_rect().union(footer.right.min_rect());
                ui.advance_cursor_after_rect(used);
                (inner, footer.closed)
            })
            .inner;
        self.closed |= closed;
        inner
    }

    /// Asks the dialog to close, as Cancel would (a "Close" button inside the body).
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// The dialog's own `Ui`, for content between the body and the footer.
    pub fn ui(&mut self) -> &mut Ui {
        self.ui
    }
}

/// The footer's action row. The left group (filled left to right) holds secondary actions
/// and status; the right group fills from the right edge, so add the primary first, then
/// Cancel, then any other button (Back). The reason a primary is disabled shows as muted
/// text to the left of the right group, and as the button's hover text.
pub struct DialogFooter {
    left: Ui,
    right: Ui,
    enter: bool,
    reason: Option<String>,
    danger: bool,
    cancel: Option<Id>,
    focus_cancel: bool,
    closed: bool,
}

impl DialogFooter {
    /// Enter does not press the primary here: for approvals that must be clicked.
    pub fn without_enter(&mut self) {
        self.enter = false;
    }

    /// Moves the keyboard focus to the dismiss button this frame (the safe answer of a new
    /// request shown in an open dialog).
    pub fn focus_cancel(&mut self) {
        self.focus_cancel = true;
    }

    /// The primary action, in the accent color. `blocked` disables it and says why
    /// ("Choose a file to import"); an empty reason disables it silently (work in progress).
    /// `true` when clicked, or when Enter was pressed with nothing focused.
    pub fn primary(&mut self, label: &str, blocked: Option<&str>) -> bool {
        self.main_action(label, blocked, false)
    }

    /// A destructive primary ("Delete", "Stop all"): an error-color fill, and Cancel gets
    /// the focus when the dialog opens.
    pub fn danger(&mut self, label: &str, blocked: Option<&str>) -> bool {
        self.danger = true;
        self.main_action(label, blocked, true)
    }

    fn main_action(&mut self, label: &str, blocked: Option<&str>, danger: bool) -> bool {
        let ctx = self.right.ctx().clone();
        let p = palette(&ctx);
        let enabled = blocked.is_none();
        let button = match (enabled, danger) {
            (true, false) => super::primary_button(&ctx, label),
            (true, true) => {
                egui::Button::new(RichText::new(label.to_string()).color(Color32::WHITE))
                    .fill(p.error)
            }
            (false, _) => super::primary_button_state(&ctx, false, label),
        };
        let mut response = self.right.add_enabled(enabled, button);
        if let Some(reason) = blocked.filter(|r| !r.is_empty()) {
            response = response.on_disabled_hover_text(reason);
            self.reason = Some(reason.to_string());
        }
        let mut clicked = response.clicked();
        // Enter never confirms a destructive action: Cancel holds the focus instead.
        if enabled && !danger && self.enter {
            self.enter = false;
            ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
            clicked = true;
        }
        clicked
    }

    /// The Cancel button; it closes the dialog.
    pub fn cancel(&mut self) -> bool {
        self.cancel_as("Cancel")
    }

    /// A dismiss button with another label ("Close" once a job is done, "Keep running").
    pub fn cancel_as(&mut self, label: &str) -> bool {
        let response = self.right.button(label);
        self.cancel = Some(response.id);
        let clicked = response.clicked();
        self.closed |= clicked;
        clicked
    }

    /// A plain button in the right group, left of those added before it (Back, Apply).
    pub fn button(&mut self, label: &str, enabled: bool) -> bool {
        self.right
            .add_enabled(enabled, egui::Button::new(label))
            .clicked()
    }

    /// A secondary action on the left ("Test connection", "Save as task", "Reset").
    pub fn secondary(&mut self, label: &str, enabled: bool) -> bool {
        self.left
            .add_enabled(enabled, egui::Button::new(label))
            .clicked()
    }

    /// The left group, for status next to the secondary actions (a spinner, progress).
    pub fn left(&mut self) -> &mut Ui {
        &mut self.left
    }

    /// The right group, for widgets beside the buttons (a spinner while waiting).
    pub fn right(&mut self) -> &mut Ui {
        &mut self.right
    }

    fn finish(&mut self, first_frame: bool) {
        if (self.focus_cancel || self.danger && first_frame)
            && let Some(cancel) = self.cancel
        {
            self.right.ctx().memory_mut(|m| m.request_focus(cancel));
        }
        if let Some(reason) = self.reason.take() {
            let room = self.right.min_rect().left() - self.left.min_rect().right();
            let room = if self.left.min_rect().width() > 0.0 {
                room - 2.0 * Spacing::M
            } else {
                self.right.available_width() - Spacing::S
            };
            if room > 40.0 {
                let p = palette(self.right.ctx());
                self.right.add_space(Spacing::XS);
                self.right.allocate_ui_with_layout(
                    vec2(room, self.right.available_height()),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(reason).color(p.text_muted)).truncate(),
                        );
                    },
                );
            }
        }
    }
}

/// A destructive confirmation in `dialog`: `body`, then Cancel (focused) and the red
/// `action`. `Some(true)` once confirmed, `Some(false)` once dismissed (Cancel, ✕, Escape or
/// a click outside), `None` while open.
pub fn confirm(
    ctx: &egui::Context,
    dialog: Dialog<'_>,
    action: &str,
    body: impl FnOnce(&mut Ui),
) -> Option<bool> {
    let response = dialog.danger().dismiss_on_backdrop().show(ctx, |d| {
        d.body(body);
        d.footer(|f| {
            let confirmed = f.danger(action, None);
            f.cancel();
            confirmed
        })
    });
    if response.inner {
        Some(true)
    } else if response.closed {
        Some(false)
    } else {
        None
    }
}

/// A vertical scroll area for a dialog body or a dialog pane: it shrinks to its content,
/// stops at `max_height`, keeps its scroll bar visible while it overflows and fades the
/// edge where more content hides.
pub fn scroll_body<R>(
    ui: &mut Ui,
    id: impl Hash + std::fmt::Debug,
    min_height: f32,
    max_height: f32,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> R {
    let p = palette(ui.ctx());
    let output = ui
        .scope(|ui| {
            let scroll = &mut ui.spacing_mut().scroll;
            scroll.dormant_handle_opacity = 0.55;
            scroll.dormant_background_opacity = 0.0;
            egui::ScrollArea::vertical()
                .id_salt(id)
                .min_scrolled_height(min_height)
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, add_contents)
        })
        .inner;
    let view = output.inner_rect;
    let offset = output.state.offset.y;
    let hidden_below = output.content_size.y - offset - view.height();
    let fade_rect = view.with_max_x(view.right() - Spacing::M);
    if hidden_below > 1.0 {
        let rect = fade_rect.with_min_y(view.bottom() - FADE);
        fade(ui, rect, p.bg_raised, false);
    }
    if offset > 1.0 {
        let rect = fade_rect.with_max_y(view.top() + FADE);
        fade(ui, rect, p.bg_raised, true);
    }
    output.inner
}

/// A vertical gradient from transparent to `color` (downwards, or upwards when `up`).
fn fade(ui: &Ui, rect: egui::Rect, color: Color32, up: bool) {
    let clear = color.gamma_multiply(0.0);
    let (top, bottom) = if up { (color, clear) } else { (clear, color) };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}
