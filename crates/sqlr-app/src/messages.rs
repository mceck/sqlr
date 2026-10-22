//! The Messages tab of a run: each statement's outcome and timing, server notices and
//! transaction notes, with their time and level, and a button that copies them.

use egui::RichText;
use sqlr_engine::{MessageLevel, RunMessage};
use sqlr_ui::format_when;
use sqlr_ui::theme::{self, Spacing};

fn level_name(level: MessageLevel) -> &'static str {
    match level {
        MessageLevel::Info => "info",
        MessageLevel::Notice => "notice",
        MessageLevel::Warning => "warning",
        MessageLevel::Error => "error",
    }
}

/// `HH:MM:SS` of Unix milliseconds, in local time.
fn clock(ms: i64) -> String {
    let when = format_when(ms);
    when.get(11..).unwrap_or(&when).to_string()
}

/// The messages as text, one per line: time, level and text.
pub(crate) fn messages_text(messages: &[RunMessage]) -> String {
    messages
        .iter()
        .map(|m| {
            format!(
                "{}  {:<7}  {}\n",
                clock(m.at_ms),
                level_name(m.level),
                m.text
            )
        })
        .collect()
}

pub(crate) fn messages_view(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    messages: &[RunMessage],
) {
    let palette = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        let n = messages.len();
        let noun = if n == 1 { "message" } else { "messages" };
        ui.label(RichText::new(format!("{n} {noun}")).color(palette.text_muted));
        if ui.add_enabled(n > 0, egui::Button::new("Copy")).clicked() {
            ui.ctx().copy_text(messages_text(messages));
        }
    });
    let row_height = ui.spacing().interact_size.y;
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink(false)
        .show_rows(ui, row_height, messages.len(), |ui, range| {
            for m in &messages[range] {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = Spacing::M;
                    ui.label(
                        RichText::new(clock(m.at_ms))
                            .monospace()
                            .color(palette.text_muted),
                    );
                    let color = match m.level {
                        MessageLevel::Info => palette.text,
                        MessageLevel::Notice => palette.accent,
                        MessageLevel::Warning => palette.warning,
                        MessageLevel::Error => palette.error,
                    };
                    ui.label(RichText::new(&m.text).color(color));
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copied_text_has_one_line_per_message() {
        let messages = vec![
            RunMessage::new(MessageLevel::Info, "select 1 · 1 rows · 2 ms"),
            RunMessage::new(MessageLevel::Error, "boom"),
        ];
        let text = messages_text(&messages);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].ends_with("info     select 1 · 1 rows · 2 ms"),
            "{}",
            lines[0]
        );
        assert!(lines[1].ends_with("error    boom"));
    }
}
