use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_core::{ColumnKind, ColumnMapping, ColumnMeta, DisplayHint, TableColumn, Transform};
use sqlr_ui::mapping_table;
use sqlr_ui::theme::{self, Palette};

struct App {
    source: Vec<ColumnMeta>,
    targets: Vec<TableColumn>,
    mapping: Vec<ColumnMapping>,
    changed: usize,
}

fn source(name: &str, ty: &str) -> ColumnMeta {
    ColumnMeta::new(name, ty, ColumnKind::Mixed, DisplayHint::Plain)
}

fn target(name: &str, ty: &str) -> TableColumn {
    TableColumn {
        name: name.into(),
        type_name: ty.into(),
        nullable: true,
        default: None,
        generated: false,
    }
}

fn harness(mapping: Vec<ColumnMapping>) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(vec2(800.0, 300.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                if mapping_table(ui, &app.source, &app.targets, &mut app.mapping) {
                    app.changed += 1;
                }
            },
            App {
                source: vec![
                    source("ID", "INTEGER"),
                    source("Name", "TEXT"),
                    source("extra", "TEXT"),
                ],
                targets: vec![target("name", "text"), target("id", "integer")],
                mapping,
                changed: 0,
            },
        );
    h.run_steps(3);
    h
}

#[test]
fn mapping_auto_by_name_and_skip() {
    // An empty mapping is filled by name, ignoring case; a column nothing names is skipped.
    let mut h = harness(Vec::new());
    let targets: Vec<Option<&str>> = h
        .state()
        .mapping
        .iter()
        .map(|m| m.target.as_deref())
        .collect();
    assert_eq!(targets, [Some("id"), Some("name"), None]);

    // Unchecking a column skips it; checking it again maps it by name.
    h.get_by_label("ID").click();
    h.run_steps(2);
    assert_eq!(h.state().mapping[0].target, None);
    assert!(h.state().changed > 0);
    h.get_by_label("ID").click();
    h.run_steps(2);
    assert_eq!(h.state().mapping[0].target.as_deref(), Some("id"));

    // A mapping the caller built (a transform) is kept as it is.
    let kept = vec![
        ColumnMapping::new(0, Some("id".into())),
        ColumnMapping {
            transform: Some(Transform::Upper),
            ..ColumnMapping::new(1, Some("name".into()))
        },
        ColumnMapping::new(2, None),
    ];
    let h = harness(kept.clone());
    assert_eq!(h.state().mapping, kept);
}
