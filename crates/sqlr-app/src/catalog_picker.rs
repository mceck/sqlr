//! Catalog-backed suggestions for schema and table names, with a typed fallback.
use egui::TextEdit;
use sqlr_core::CatalogNeed;
use sqlr_engine::{Engine, LoadState};

pub(crate) fn schema_names(engine: &Engine, conn: &str, database: Option<&str>) -> Vec<String> {
    let catalog = engine.catalog_in(conn, database);
    if catalog.as_ref().is_none_or(|c| c.schemas.is_none()) {
        engine.load_catalog_in(conn, database, CatalogNeed::Schemas);
    }
    match catalog.and_then(|c| c.schemas.clone()) {
        Some(LoadState::Ready(names)) => names,
        _ => vec![],
    }
}

pub(crate) fn table_names(engine: &Engine, conn: &str, schema: &str) -> Vec<String> {
    let catalog = engine.catalog(conn);
    if catalog
        .as_ref()
        .is_none_or(|c| !c.schema.contains_key(schema))
    {
        engine.load_catalog(conn, CatalogNeed::Schema(schema.into()));
    }
    match catalog.and_then(|c| c.schema.get(schema).cloned()) {
        Some(LoadState::Ready(schema)) => schema.relations.iter().map(|r| r.name.clone()).collect(),
        _ => vec![],
    }
}

pub(crate) fn name(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut String,
    names: &[String],
    hint: &str,
    width: f32,
) -> bool {
    let mut changed = false;
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            changed |= sqlr_ui::text_edit(
                ui,
                TextEdit::singleline(value)
                    .hint_text(hint)
                    .desired_width((width - 32.0).max(60.0)),
            )
            .changed();
            let filter = value.to_lowercase();
            let exact = names.iter().any(|n| n.eq_ignore_ascii_case(value));
            // The field's own drop-down: a caret beside it lists the catalog's names.
            let browse = sqlr_ui::theme::icons::icon_button(
                ui,
                sqlr_ui::theme::icons::CARET_DOWN,
                None,
                "Browse catalog names",
            );
            egui::Popup::menu(&browse)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
                .show(|ui| {
                    ui.set_min_width(width.min(320.0));
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .show(ui, |ui| {
                            let mut found = false;
                            for item in names.iter().filter(|n| {
                                exact || filter.is_empty() || n.to_lowercase().contains(&filter)
                            }) {
                                found = true;
                                if ui.selectable_label(item == value, item).clicked() {
                                    *value = item.clone();
                                    changed = true;
                                }
                            }
                            if !found {
                                sqlr_ui::theme::hint(
                                    ui,
                                    if names.is_empty() {
                                        "Loading names… or type one in the field"
                                    } else {
                                        "No matches: type a name in the field"
                                    },
                                );
                            }
                        });
                });
        });
    });
    changed
}
