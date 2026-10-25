//! The column table of the test-data generator (spec §8.6): per column, the generator, its
//! options, how often it is NULL and whether values must be unique.

use egui::{ComboBox, DragValue, RichText, TextEdit, WidgetInfo, WidgetType};
use sqlr_core::{ColumnGen, Generator};

use crate::theme::{self, Spacing};

/// The generator kinds a column can switch to, by label.
/// Builds the generator a menu entry stands for.
type MakeGenerator = fn() -> Generator;

const CHOICES: [(&str, MakeGenerator); 20] = [
    ("Sequence", || Generator::Sequence { start: 1, step: 1 }),
    ("Integer range", || Generator::IntRange {
        min: 1,
        max: 100_000,
    }),
    ("Float range", || Generator::FloatRange {
        min: 0.0,
        max: 1000.0,
        decimals: 2,
    }),
    ("Decimal", || Generator::Decimal {
        precision: 10,
        scale: 2,
    }),
    ("Boolean", || Generator::Bool { true_ratio: 0.5 }),
    ("Date", || Generator::DateRange {
        from: 18_000,
        to: 20_500,
    }),
    ("Timestamp", || Generator::TimestampRange {
        from: 1_550_000_000,
        to: 1_750_000_000,
    }),
    ("UUID", || Generator::Uuid),
    ("First name", || Generator::FirstName),
    ("Last name", || Generator::LastName),
    ("Full name", || Generator::FullName),
    ("Email", || Generator::Email),
    ("Phone", || Generator::Phone),
    ("City", || Generator::City),
    ("Country", || Generator::Country),
    ("Company", || Generator::Company),
    ("Lorem", || Generator::Lorem { words: 3 }),
    ("Pattern", || Generator::Pattern("##-??".into())),
    ("One of", || Generator::OneOf(vec!["a".into(), "b".into()])),
    ("Constant", || Generator::Constant(String::new())),
];

/// The label of a generator.
pub fn generator_label(g: &Generator) -> &'static str {
    match g {
        Generator::Sequence { .. } => "Sequence",
        Generator::IntRange { .. } => "Integer range",
        Generator::FloatRange { .. } => "Float range",
        Generator::Decimal { .. } => "Decimal",
        Generator::Bool { .. } => "Boolean",
        Generator::DateRange { .. } => "Date",
        Generator::TimestampRange { .. } => "Timestamp",
        Generator::Uuid => "UUID",
        Generator::FirstName => "First name",
        Generator::LastName => "Last name",
        Generator::FullName => "Full name",
        Generator::Email => "Email",
        Generator::Phone => "Phone",
        Generator::City => "City",
        Generator::Country => "Country",
        Generator::Company => "Company",
        Generator::Lorem { .. } => "Lorem",
        Generator::Pattern(_) => "Pattern",
        Generator::OneOf(_) => "One of",
        Generator::Constant(_) => "Constant",
        Generator::ForeignKey { .. } => "Foreign key",
        Generator::Default => "Database default",
    }
}

/// Draws one row per column of `columns` (`types`: each column's name and type, for display);
/// returns `true` when the user changed something.
pub fn datagen_table(
    ui: &mut egui::Ui,
    columns: &mut [ColumnGen],
    types: &[(String, String)],
) -> bool {
    let mut changed = false;
    let muted = theme::palette(ui.ctx()).text_muted;
    egui::Grid::new("datagen_columns")
        .num_columns(6)
        .spacing([Spacing::L, Spacing::S])
        .striped(true)
        .show(ui, |ui| {
            for heading in [
                "Include",
                "Type",
                "Generator",
                "Options",
                "NULL %",
                "Unique",
            ] {
                ui.label(RichText::new(heading).strong());
            }
            ui.end_row();
            for (i, c) in columns.iter_mut().enumerate() {
                let name = c.column.clone();
                let mut write = c.generator != Generator::Default;
                let check = ui.checkbox(&mut write, name.as_str());
                check.widget_info(|| {
                    WidgetInfo::selected(WidgetType::Checkbox, true, write, format!("Write {name}"))
                });
                if check.clicked() {
                    changed = true;
                    c.generator = if write {
                        Generator::Sequence { start: 1, step: 1 }
                    } else {
                        Generator::Default
                    };
                }
                let ty = types
                    .iter()
                    .find(|(n, _)| *n == c.column)
                    .map_or("", |(_, t)| t.as_str());
                ui.label(RichText::new(ty).small().color(muted));
                if matches!(
                    c.generator,
                    Generator::ForeignKey { .. } | Generator::Default
                ) {
                    ui.add_enabled_ui(false, |ui| {
                        ComboBox::from_id_salt(("fixed_generator", i))
                            .selected_text(generator_label(&c.generator))
                            .show_ui(ui, |_| {});
                    });
                } else {
                    ComboBox::from_id_salt(("datagen-kind", i))
                        .selected_text(generator_label(&c.generator))
                        .show_ui(ui, |ui| {
                            for (label, make) in CHOICES {
                                let current = generator_label(&c.generator) == label;
                                if ui.selectable_label(current, label).clicked() && !current {
                                    c.generator = make();
                                    changed = true;
                                }
                            }
                        });
                }
                changed |= options(ui, &mut c.generator);
                let mut percent = c.null_ratio * 100.0;
                if ui
                    .add(
                        DragValue::new(&mut percent)
                            .range(0.0..=100.0)
                            .speed(1.0)
                            .suffix(" %")
                            .fixed_decimals(0),
                    )
                    .changed()
                {
                    c.null_ratio = percent / 100.0;
                    changed = true;
                }
                let unique = ui.checkbox(&mut c.unique, "");
                let label = c.column.clone();
                let on = c.unique;
                unique.widget_info(|| {
                    WidgetInfo::selected(WidgetType::Checkbox, true, on, format!("Unique {label}"))
                });
                changed |= unique.changed();
                ui.end_row();
            }
        });
    changed
}

fn options(ui: &mut egui::Ui, g: &mut Generator) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| match g {
        Generator::Sequence { start, step } => {
            changed |= ui.add(DragValue::new(start).prefix("from ")).changed();
            changed |= ui.add(DragValue::new(step).prefix("step ")).changed();
        }
        Generator::IntRange { min, max } => {
            changed |= ui.add(DragValue::new(min).prefix("min ")).changed();
            changed |= ui.add(DragValue::new(max).prefix("max ")).changed();
        }
        Generator::FloatRange { min, max, decimals } => {
            changed |= ui.add(DragValue::new(min).prefix("min ")).changed();
            changed |= ui.add(DragValue::new(max).prefix("max ")).changed();
            changed |= ui
                .add(DragValue::new(decimals).range(0..=10).prefix("dec "))
                .changed();
        }
        Generator::Decimal { precision, scale } => {
            changed |= ui
                .add(DragValue::new(precision).range(1..=30).prefix("p "))
                .changed();
            changed |= ui
                .add(DragValue::new(scale).range(0..=15).prefix("s "))
                .changed();
        }
        Generator::Bool { true_ratio } => {
            changed |= ui
                .add(
                    DragValue::new(true_ratio)
                        .range(0.0..=1.0)
                        .speed(0.01)
                        .prefix("true "),
                )
                .changed();
        }
        Generator::DateRange { from, to } => {
            changed |= ui.add(DragValue::new(from).prefix("day ")).changed();
            changed |= ui.add(DragValue::new(to).prefix("to ")).changed();
        }
        Generator::TimestampRange { from, to } => {
            changed |= ui.add(DragValue::new(from).prefix("s ")).changed();
            changed |= ui.add(DragValue::new(to).prefix("to ")).changed();
        }
        Generator::Lorem { words } => {
            changed |= ui
                .add(DragValue::new(words).range(1..=200).suffix(" words"))
                .changed();
        }
        Generator::Pattern(p) => {
            changed |= ui
                .add(
                    TextEdit::singleline(p)
                        .desired_width(110.0)
                        .hint_text("# digit ? letter * either"),
                )
                .changed();
        }
        Generator::OneOf(options) => {
            let mut text = options.join(", ");
            if ui
                .add(
                    TextEdit::singleline(&mut text)
                        .desired_width(140.0)
                        .hint_text("a, b, c"),
                )
                .changed()
            {
                *options = text
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                changed = true;
            }
        }
        Generator::Constant(text) => {
            changed |= ui
                .add(TextEdit::singleline(text).desired_width(110.0))
                .changed();
        }
        Generator::ForeignKey { table, column } => {
            ui.label(format!("{}.{column}", table.name));
        }
        _ => {}
    });
    changed
}
