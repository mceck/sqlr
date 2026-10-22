use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_core::{ColumnGen, Generator};
use sqlr_ui::datagen_table;
use sqlr_ui::theme::{self, Palette};

struct App {
    columns: Vec<ColumnGen>,
    types: Vec<(String, String)>,
    changed: usize,
}

fn colgen(name: &str, generator: Generator) -> ColumnGen {
    ColumnGen {
        column: name.into(),
        generator,
        null_ratio: 0.0,
        unique: false,
    }
}

fn harness() -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(vec2(900.0, 300.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                if datagen_table(ui, &mut app.columns, &app.types) {
                    app.changed += 1;
                }
            },
            App {
                columns: vec![
                    colgen("id", Generator::Sequence { start: 1, step: 1 }),
                    colgen("name", Generator::FullName),
                    colgen("code", Generator::Pattern("C-##".into())),
                ],
                types: vec![
                    ("id".into(), "integer".into()),
                    ("name".into(), "text".into()),
                    ("code".into(), "text".into()),
                ],
                changed: 0,
            },
        );
    h.run_steps(3);
    h
}

#[test]
fn datagen_column_table_edits() {
    let mut h = harness();
    // Each column lists its name and type, and the generator chosen for it.
    for name in ["Write id", "Write name", "Write code"] {
        h.get_by_label(name);
    }
    assert_eq!(h.get_all_by_role(Role::ComboBox).count(), 3);

    // Unchecking "Unique" on a column marks it unique in the spec.
    h.get_by_label("Unique code").click();
    h.run_steps(2);
    assert!(h.state().columns[2].unique);
    assert!(h.state().changed > 0);

    // A column the database fills is switched to Default and left out.
    h.get_by_label("Write id").click();
    h.run_steps(2);
    assert_eq!(h.state().columns[0].generator, Generator::Default);
}
