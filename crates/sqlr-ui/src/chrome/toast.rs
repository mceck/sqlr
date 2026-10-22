//! Short-lived notices stacked at the bottom right of the window.

use std::collections::VecDeque;

use egui::{Align2, CornerRadius, Id, Margin, Order, Rect, RichText, Sense, vec2};

use crate::theme::{self, Spacing, icons};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

impl ToastKind {
    /// Seconds a toast stays, its fade included.
    fn life(self) -> f64 {
        match self {
            Self::Info | Self::Success => 4.0,
            Self::Error => 8.0,
        }
    }
}

/// The most toasts shown at once; later ones wait for a free slot.
const MAX_TOASTS: usize = 3;
/// Seconds over which a toast fades out at the end of its life.
const FADE: f64 = 0.6;

struct Toast {
    kind: ToastKind,
    text: String,
    /// The `input.time` it was first shown at.
    born: Option<f64>,
    id: u64,
}

/// The toast queue: [`Toasts::push`] from anywhere, [`Toasts::show`] once per frame.
#[derive(Default)]
pub struct Toasts {
    items: VecDeque<Toast>,
    next_id: u64,
}

impl Toasts {
    pub fn push(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.next_id += 1;
        self.items.push_back(Toast {
            kind,
            text: text.into(),
            born: None,
            id: self.next_id,
        });
    }

    /// The texts of the toasts shown and waiting, oldest first.
    pub fn texts(&self) -> Vec<String> {
        self.items.iter().map(|t| t.text.clone()).collect()
    }

    /// Drops expired toasts and paints the oldest three above the status bar, each with a
    /// 3 px bar in its kind's color and a Dismiss button, fading out over its last 0.6 s.
    /// A toast's time starts when it is first shown; a hovered error stays.
    pub fn show(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.items
            .retain(|t| t.born.is_none_or(|born| now - born < t.kind.life()));
        for t in self.items.iter_mut().take(MAX_TOASTS) {
            t.born.get_or_insert(now);
        }
        if self.items.is_empty() {
            return;
        }
        let p = theme::palette(ctx);
        let offset = vec2(-Spacing::XL, -(Spacing::STATUS + Spacing::M));
        let mut dismissed = Vec::new();
        egui::Area::new(Id::new("sqlr_toasts"))
            .anchor(Align2::RIGHT_BOTTOM, offset)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.set_width(Spacing::TOAST_WIDTH);
                ui.spacing_mut().item_spacing.y = Spacing::S;
                for t in self.items.iter_mut().take(MAX_TOASTS) {
                    let born = t.born.unwrap_or(now);
                    let left = t.kind.life() - (now - born);
                    let bar = match t.kind {
                        ToastKind::Info => p.accent,
                        ToastKind::Success => p.success,
                        ToastKind::Error => p.error,
                    };
                    let rect = ui
                        .scope(|ui| {
                            ui.set_opacity((left / FADE).clamp(0.0, 1.0) as f32);
                            egui::Frame::new()
                                .fill(p.bg_raised)
                                .stroke(egui::Stroke::new(1.0, p.border_input))
                                .corner_radius(CornerRadius::same(Spacing::RADIUS_M))
                                .shadow(ui.visuals().popup_shadow)
                                .inner_margin(Margin {
                                    left: (Spacing::L + Spacing::TOAST_BAR) as i8,
                                    right: Spacing::L as i8,
                                    top: Spacing::M as i8,
                                    bottom: Spacing::M as i8,
                                })
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal_top(|ui| {
                                        let text = ui.available_width() - 20.0 - Spacing::S;
                                        ui.scope(|ui| {
                                            ui.set_width(text);
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&t.text).color(p.text),
                                                )
                                                .wrap(),
                                            );
                                        });
                                        if icons::row_button(ui, icons::CLOSE, "Dismiss").clicked()
                                        {
                                            dismissed.push(t.id);
                                        }
                                    });
                                })
                                .response
                                .rect
                        })
                        .inner;
                    let stripe = Rect::from_min_size(
                        rect.min + vec2(1.0, 1.0),
                        vec2(Spacing::TOAST_BAR, rect.height() - 2.0),
                    );
                    ui.painter().rect_filled(
                        stripe,
                        CornerRadius {
                            nw: Spacing::RADIUS_M,
                            sw: Spacing::RADIUS_M,
                            ne: 0,
                            se: 0,
                        },
                        bar.gamma_multiply((left / FADE).clamp(0.0, 1.0) as f32),
                    );
                    let hovered = ui
                        .interact(rect, Id::new(("sqlr_toast", t.id)), Sense::hover())
                        .contains_pointer();
                    if hovered && t.kind == ToastKind::Error {
                        // Keep at least a second before the fade starts.
                        t.born = Some(born.max(now - (t.kind.life() - FADE - 1.0)));
                    }
                }
            });
        self.items.retain(|t| !dismissed.contains(&t.id));
        // Wake up for the next fade step, or when the next toast starts fading.
        let next = self
            .items
            .iter()
            .take(MAX_TOASTS)
            .map(|t| t.kind.life() - (now - t.born.unwrap_or(now)) - FADE)
            .fold(f64::INFINITY, f64::min);
        // No toast left: nothing to wake up for (infinity is not a Duration).
        if next.is_finite() {
            let wait = if next <= 0.0 { 1.0 / 30.0 } else { next };
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
        }
    }
}
