//! Catalog-backed suggestions for schema and table names, with a typed fallback.
use egui::TextEdit;
use sqlr_core::{CatalogNeed, NodeKind, NodePath};
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

/// The databases a connection lists above its schemas, when its explorer does (PostgreSQL
/// with "Show all databases"). Empty when the root lists schemas, which name databases
/// themselves on MySQL-like Drivers.
pub(crate) fn database_names(engine: &Engine, conn: &str) -> Vec<String> {
    let root = NodePath::root();
    let nodes = match engine.children(conn, &root) {
        Some(LoadState::Ready(nodes)) => nodes,
        Some(_) => return Vec::new(),
        None => {
            engine.browse(conn, root);
            return Vec::new();
        }
    };
    nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Database)
        .map(|n| n.name.clone())
        .collect()
}

pub(crate) fn table_names(
    engine: &Engine,
    conn: &str,
    database: Option<&str>,
    schema: &str,
) -> Vec<String> {
    let catalog = engine.catalog_in(conn, database);
    if catalog
        .as_ref()
        .is_none_or(|c| !c.schema.contains_key(schema))
    {
        engine.load_catalog_in(conn, database, CatalogNeed::Schema(schema.into()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use sqlr_core::{ConnectionConfig, ConnectionProfile, Node, Secrets};
    use sqlr_engine::{DriverRegistry, SessionStatus};
    use sqlr_testkit::{FakeDriver, FakeScript};

    fn node(name: &str, kind: NodeKind) -> Node {
        Node {
            path: NodePath::root().child(name),
            name: name.into(),
            kind,
            has_children: true,
            detail: None,
        }
    }

    fn engine_with(tree: Vec<Node>) -> Engine {
        let fake = FakeDriver::new(FakeScript {
            tree,
            own_database: Some("app".into()),
            ..Default::default()
        });
        let mut drivers = DriverRegistry::default();
        drivers.register(fake);
        let engine = Engine::new(drivers, Arc::new(|| {}));
        engine.connect(
            ConnectionConfig {
                id: "c".into(),
                name: "c".into(),
                driver: "fake".into(),
                group: None,
                color: None,
                values: Default::default(),
                profile: None,
                save_secrets: true,
                agent_access: Default::default(),
            },
            Secrets::default(),
            ConnectionProfile::development(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(
            engine.session_status("c"),
            Some(SessionStatus::Connected(_))
        ) {
            assert!(
                Instant::now() < deadline,
                "the fake session never connected"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        engine
    }

    #[test]
    fn database_names_lists_databases_the_explorer_shows() {
        let engine = engine_with(vec![
            node("app", NodeKind::Database),
            node("other", NodeKind::Database),
        ]);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut names = Vec::new();
        while names.is_empty() {
            names = database_names(&engine, "c");
            assert!(Instant::now() < deadline, "the root never loaded");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(names, ["app", "other"]);
    }

    #[test]
    fn database_names_is_empty_when_the_root_lists_schemas() {
        let engine = engine_with(vec![
            node("app", NodeKind::Schema),
            node("other", NodeKind::Schema),
        ]);
        database_names(&engine, "c");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(
            engine.children("c", &NodePath::root()),
            Some(LoadState::Ready(_))
        ) {
            assert!(Instant::now() < deadline, "the root never loaded");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(database_names(&engine, "c").is_empty());
    }
}
