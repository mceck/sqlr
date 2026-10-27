use egui::{Color32, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_core::{DatabaseTools, Node, NodeKind, NodePath, ObjectKind, SqlTemplate};
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{
    Children, ConnBadge, NodeMenu, SchemaSource, TreeAction, TreeConn, TreeDrop, TreeState,
    schema_tree,
};

fn node(parent: &NodePath, name: &str, kind: NodeKind, has_children: bool) -> Node {
    Node {
        path: parent.child(name),
        name: name.into(),
        kind,
        has_children,
        detail: None,
    }
}

struct StubSource {
    prod_root: Vec<Node>,
    prod_public: Vec<Node>,
    local_failed: bool,
    public_loaded: bool,
    /// Folder names in display order.
    folders: Vec<String>,
    /// Offer object entries in context menus.
    objects: bool,
    /// The connections are key-value stores.
    key_value: bool,
    /// PostgreSQL offers native dump and restore.
    database_tools: bool,
    /// The "local" connection's state.
    local_badge: ConnBadge,
    /// The "local" connection's agent access.
    local_access: sqlr_core::AgentAccess,
    /// Objects have DDL ("Open DDL").
    ddl: bool,
    /// "public" also holds a "views" folder with three loaded views.
    views: Option<Vec<Node>>,
}

impl StubSource {
    fn new() -> Self {
        let root = NodePath::root();
        let public = node(&root, "public", NodeKind::Schema, true);
        let prod_public = vec![
            node(&public.path, "orders", NodeKind::Table, true),
            node(&public.path, "users", NodeKind::Table, true),
        ];
        Self {
            prod_root: vec![node(&root, "other", NodeKind::Database, true), public],
            prod_public,
            local_failed: false,
            public_loaded: true,
            folders: vec!["prod".into()],
            objects: false,
            key_value: false,
            database_tools: false,
            local_badge: ConnBadge::Disconnected,
            local_access: sqlr_core::AgentAccess::None,
            ddl: false,
            views: None,
        }
    }

    /// With a "views" folder under "public" whose three views are loaded.
    fn with_views() -> Self {
        let mut source = Self::new();
        let public = NodePath::root().child("public");
        let folder = node(&public, "views", NodeKind::Folder, true);
        source.views = Some(
            ["v1", "v2", "v3"]
                .iter()
                .map(|v| node(&folder.path, v, NodeKind::View, true))
                .collect(),
        );
        source.prod_public.push(folder);
        source
    }
}

impl SchemaSource for StubSource {
    fn connections(&self) -> Vec<TreeConn<'_>> {
        vec![
            TreeConn {
                id: "local",
                name: "local",
                driver: "sqlite",
                detail: Some("app.db"),
                group: None,
                color: None,
                badge: self.local_badge.clone(),
                tint: None,
                open_transaction: false,
                agent_access: self.local_access,
            },
            TreeConn {
                id: "prod",
                name: "prod-db",
                driver: "postgres",
                detail: Some("PostgreSQL"),
                group: Some("prod"),
                color: Some([200, 40, 40]),
                badge: ConnBadge::Connected,
                tint: Some([40, 200, 40]),
                open_transaction: true,
                agent_access: Default::default(),
            },
        ]
    }

    fn children(&self, conn: &str, path: &NodePath) -> Children<'_> {
        match (conn, path.segments()) {
            ("local", []) if self.local_failed => Children::Failed("boom"),
            ("prod", []) => Children::Ready(&self.prod_root),
            ("prod", [s]) if s == "public" && self.public_loaded => {
                Children::Ready(&self.prod_public)
            }
            ("prod", [s, f]) if s == "public" && f == "views" => match &self.views {
                Some(views) => Children::Ready(views),
                None => Children::NotLoaded,
            },
            _ => Children::NotLoaded,
        }
    }

    fn folders(&self) -> Vec<&str> {
        self.folders.iter().map(String::as_str).collect()
    }

    fn node_menu(&self, _conn: &str, node: &Node) -> NodeMenu {
        match node.kind {
            NodeKind::Table if self.objects => NodeMenu {
                open: true,
                new: vec![ObjectKind::Index],
                drop: true,
                rename: true,
                truncate: true,
                generate: vec![SqlTemplate::Select, SqlTemplate::Create],
            },
            NodeKind::Schema if self.objects => NodeMenu {
                new: vec![ObjectKind::Table, ObjectKind::View],
                ..Default::default()
            },
            _ => NodeMenu::default(),
        }
    }

    fn key_value(&self, _conn: &str) -> bool {
        self.key_value
    }

    fn ddl(&self, _conn: &str, node: &Node) -> bool {
        self.ddl && node.kind != NodeKind::Folder && node.kind != NodeKind::Column
    }

    fn database_tools(&self, conn: &str) -> DatabaseTools {
        if conn == "prod" && self.database_tools {
            DatabaseTools::both()
        } else {
            DatabaseTools::default()
        }
    }

    fn connection_database(&self, conn: &str) -> Option<String> {
        (conn == "prod" && self.database_tools).then(|| "app".to_string())
    }

    fn conn_new(&self, _conn: &str) -> Vec<ObjectKind> {
        if self.objects {
            vec![ObjectKind::Database]
        } else {
            Vec::new()
        }
    }
}

struct App {
    state: TreeState,
    source: StubSource,
    actions: Vec<TreeAction>,
}

fn harness(source: StubSource) -> Harness<'static, App> {
    let app = App {
        state: TreeState::default(),
        source,
        actions: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(400.0, 500.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                let a = schema_tree(ui, &mut app.state, &app.source);
                app.actions.extend(a);
            },
            app,
        );
    h.run_steps(2);
    h
}

/// Opens the `prod` folder and connection, and its `public` schema.
fn open_prod(h: &mut Harness<'static, App>) {
    h.state_mut().state.expand_folder("prod");
    h.state_mut()
        .state
        .set_expanded("prod", &NodePath::root(), true);
    h.state_mut()
        .state
        .set_expanded("prod", &NodePath::root().child("public"), true);
    h.run_steps(2);
}

/// Opens one folder level (`prod` for top-level folders).
fn open_folder(h: &mut Harness<'static, App>, folder: &str) {
    h.state_mut().state.expand_folder(folder);
    h.run_steps(2);
}

fn click_at(h: &mut Harness<'static, App>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(2);
}

#[test]
fn shows_connections_by_label() {
    let mut h = harness(StubSource::new());
    h.get_by_label("local");
    open_folder(&mut h, "prod");
    h.get_by_label("prod-db");
}

#[test]
fn clicking_the_driver_icon_selects_the_connection() {
    let mut h = harness(StubSource::new());
    open_folder(&mut h, "prod");
    // The driver icon sits immediately before the name button.
    let name = h.get_by_label("prod-db").rect();
    click_at(&mut h, egui::pos2(name.left() - 10.0, name.center().y));
    assert_eq!(
        h.state().state.selected,
        Some(("prod".to_string(), NodePath::root()))
    );
}

#[test]
fn clicking_tree_icons_selects_their_rows() {
    let mut h = harness(StubSource::new());
    open_prod(&mut h);

    let folder = h.get_by_label("prod").rect();
    click_at(&mut h, egui::pos2(folder.left() - 10.0, folder.center().y));
    assert_eq!(h.state().state.selected_folder.as_deref(), Some("prod"));
    assert!(h.state().state.selected.is_none());

    let table = h.get_by_label("orders").rect();
    click_at(&mut h, egui::pos2(table.left() - 10.0, table.center().y));
    assert_eq!(
        h.state().state.selected,
        Some((
            "prod".to_string(),
            NodePath::root().child("public").child("orders")
        ))
    );
    assert!(h.state().state.selected_folder.is_none());
}

#[test]
fn open_transaction_marks_the_connection() {
    let mut h = harness(StubSource::new());
    open_folder(&mut h, "prod");
    // Only "prod-db" has an open transaction.
    assert_eq!(h.get_all_by_label("●").count(), 1);
}

#[test]
fn expanding_disconnected_emits_connect() {
    let mut h = harness(StubSource::new());
    h.get_by_label("Expand local").click();
    h.run_steps(2);
    let actions = &h.state().actions;
    assert!(actions.contains(&TreeAction::Connect("local".into())));
    assert!(actions.contains(&TreeAction::Expand {
        conn: "local".into(),
        path: NodePath::root()
    }));
    assert!(h.state().state.is_expanded("local", &NodePath::root()));
}

#[test]
fn expanding_unloaded_node_emits_expand() {
    let mut source = StubSource::new();
    source.public_loaded = false;
    let mut h = harness(source);
    open_folder(&mut h, "prod");
    h.state_mut()
        .state
        .set_expanded("prod", &NodePath::root(), true);
    h.run_steps(2);
    h.get_by_label("Expand public").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![TreeAction::Expand {
            conn: "prod".into(),
            path: NodePath::root().child("public")
        }]
    );
    assert!(
        h.state()
            .state
            .is_expanded("prod", &NodePath::root().child("public"))
    );
}

#[test]
fn double_click_table_emits_open_data() {
    let mut h = harness(StubSource::new());
    open_prod(&mut h);
    let pos = h.get_by_label("orders").rect().center();
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        h.step();
    }
    assert!(h.state().actions.iter().any(|a| matches!(
        a,
        TreeAction::OpenData { conn, node } if conn == "prod" && node.name == "orders"
    )));
}

#[test]
fn database_context_menu_opens_query() {
    let mut h = harness(StubSource::new());
    open_folder(&mut h, "prod");
    h.state_mut()
        .state
        .set_expanded("prod", &NodePath::root(), true);
    h.run_steps(2);
    h.run_steps(100);
    h.get_by_label("other").click_secondary();
    h.run_steps(2);
    h.get_by_label("New query").click();
    h.run_steps(2);
    assert!(h.state().actions.contains(&TreeAction::NewQuery {
        conn: "prod".into(),
        database: "other".into()
    }));
}

#[test]
fn database_tools_appear_on_database_nodes_and_live_connections() {
    let mut h = harness(StubSource {
        database_tools: true,
        ..StubSource::new()
    });
    open_folder(&mut h, "prod");
    h.state_mut()
        .state
        .set_expanded("prod", &NodePath::root(), true);
    h.run_steps(100);

    // A database node dumps that database.
    h.get_by_label("other").click_secondary();
    h.run_steps(2);
    h.get_by_label("Dump database…").click();
    h.run_steps(2);
    assert!(h.state().actions.contains(&TreeAction::DumpDatabase {
        conn: "prod".into(),
        database: "other".into()
    }));

    // The connection's own root uses its configured database (important when the explorer
    // does not list database nodes).
    h.get_by_label("prod-db").click_secondary();
    h.run_steps(2);
    h.get_by_label("Restore database…").click();
    h.run_steps(2);
    assert!(h.state().actions.contains(&TreeAction::RestoreDatabase {
        conn: "prod".into(),
        database: "app".into()
    }));
}

#[test]
fn a_key_value_database_opens_the_key_browser_whatever_its_driver_id() {
    // The manifest decides (the stub's driver id is "postgres"), not the id "redis".
    let mut h = harness(StubSource {
        key_value: true,
        ..StubSource::new()
    });
    open_folder(&mut h, "prod");
    h.state_mut()
        .state
        .set_expanded("prod", &NodePath::root(), true);
    h.run_steps(100);
    h.get_by_label("other").click_secondary();
    h.run_steps(2);
    assert!(h.query_by_label("New query").is_none());
    h.get_by_label("Open key browser").click();
    h.run_steps(2);
    assert!(h.state().actions.contains(&TreeAction::NewQuery {
        conn: "prod".into(),
        database: "other".into()
    }));
}

#[test]
fn transfer_entries_follow_the_source() {
    for key_value in [false, true] {
        let mut h = harness(StubSource {
            key_value,
            ..StubSource::new()
        });
        open_prod(&mut h);
        h.run_steps(100);
        h.get_by_label("orders").click_secondary();
        h.run_steps(2);
        h.get_by_label("Open data");
        h.get_by_label("Import/Export ⏵").hover();
        h.run_steps(30);
        h.get_by_label("Export data…");
        assert_eq!(h.query_by_label("Import data…").is_some(), !key_value);
    }
}

#[test]
fn filter_hides_non_matching() {
    let mut h = harness(StubSource::new());
    open_prod(&mut h);
    h.get_by_label("users");
    h.state_mut().state.filter = "ORD".into();
    h.run_steps(2);
    h.get_by_label("orders");
    assert!(h.query_by_label("users").is_none());
    // Ancestors of a match and connections stay.
    h.get_by_label("public");
    h.get_by_label("local");
}

#[test]
fn failed_children_offer_retry() {
    let mut source = StubSource::new();
    source.local_failed = true;
    let mut h = harness(source);
    h.state_mut()
        .state
        .set_expanded("local", &NodePath::root(), true);
    h.run_steps(2);
    h.get_by_label("Retry").click();
    h.run_steps(2);
    assert!(h.state().actions.contains(&TreeAction::Refresh {
        conn: "local".into(),
        path: NodePath::root()
    }));
}

#[test]
fn plus_button_emits_new_connection() {
    let mut h = harness(StubSource::new());
    h.get_by_label("New connection").click();
    h.run_steps(2);
    assert!(h.state().actions.contains(&TreeAction::NewConnection));
}

#[test]
fn folders_show_empty_ones_and_start_closed() {
    let mut source = StubSource::new();
    source.folders.push("empty".into());
    let mut h = harness(source);
    h.get_by_label("empty");
    h.get_by_label("prod");
    assert!(
        h.query_by_label("prod-db").is_none(),
        "folders start closed"
    );
    h.get_by_label("Expand prod").click();
    h.run_steps(2);
    h.get_by_label("prod-db");
    h.get_by_label("Collapse prod").click();
    h.run_steps(2);
    assert!(
        h.query_by_label("prod-db").is_none(),
        "a closed folder hides its connections"
    );
    h.get_by_label("Expand prod").click();
    h.run_steps(2);
    h.get_by_label("prod-db");
}

/// Drags `from`'s row onto `to` and releases there.
fn drag_onto(h: &mut Harness<'static, App>, from: &str, to: &str) {
    drag_onto_at(h, from, to, 0.5);
}

/// Drags `from`'s row onto `to`, releasing at `y_fraction` of the target row's height
/// (0.0 top, 0.5 middle, 1.0 bottom).
fn drag_onto_at(h: &mut Harness<'static, App>, from: &str, to: &str, y_fraction: f32) {
    h.run_steps(100);
    let start = h.get_by_label(from).rect().center();
    let target = h.get_by_label(to).rect();
    let end = egui::pos2(
        target.center().x,
        target.top() + target.height() * y_fraction,
    );
    h.event(egui::Event::PointerMoved(start));
    h.step();
    h.event(egui::Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    let steps = 12;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let pos = egui::pos2(
            start.x + (end.x - start.x) * t,
            start.y + (end.y - start.y) * t,
        );
        h.event(egui::Event::PointerMoved(pos));
        h.step();
    }
    h.event(egui::Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

#[test]
fn dragging_a_connection_onto_a_folder_moves_it() {
    let mut h = harness(StubSource::new());
    drag_onto(&mut h, "local", "prod");
    assert!(
        h.state().actions.iter().any(|a| matches!(
            a,
            TreeAction::MoveConnection {
                conn,
                folder: Some(folder),
                target: TreeDrop::End,
            } if conn == "local" && folder == "prod"
        )),
        "{:?}",
        h.state().actions
    );
    assert!(
        h.state().state.is_folder_expanded("prod"),
        "the destination folder opens"
    );
}

#[test]
fn dragging_a_folder_reorders_it() {
    let mut source = StubSource::new();
    source.folders.push("empty".into());
    let mut h = harness(source);
    drag_onto_at(&mut h, "empty", "prod", 0.85);
    assert!(
        h.state().actions.iter().any(|a| matches!(
            a,
            TreeAction::MoveFolder { folder, target: TreeDrop::After(_) } if folder == "empty"
        )),
        "{:?}",
        h.state().actions
    );
}

#[test]
fn dragging_a_folder_onto_the_middle_nests_it() {
    let mut source = StubSource::new();
    source.folders.push("empty".into());
    let mut h = harness(source);
    drag_onto_at(&mut h, "empty", "prod", 0.5);
    assert!(
        h.state().actions.iter().any(|a| matches!(
            a,
            TreeAction::MoveFolder { folder, target: TreeDrop::Into(dest) }
                if folder == "empty" && dest == "prod"
        )),
        "{:?}",
        h.state().actions
    );
    assert!(
        h.state().state.is_folder_expanded("prod"),
        "the destination folder opens"
    );
}

#[test]
fn folder_context_menu_creates_and_deletes() {
    let mut h = harness(StubSource::new());
    h.run_steps(100);
    h.get_by_label("prod").click_secondary();
    h.run_steps(2);
    // The header's "+" carries the same label; the menu entry is the last one.
    h.get_all_by_label("New connection")
        .last()
        .expect("the folder menu entry")
        .click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![TreeAction::NewConnectionIn("prod".into())]
    );

    let mut h = harness(StubSource::new());
    menu(&mut h, "prod", None, "Delete folder");
    assert_eq!(
        h.state().actions,
        vec![TreeAction::DeleteFolder("prod".into())]
    );
}

#[test]
fn f2_renames_a_folder_inline() {
    let mut h = harness(StubSource::new());
    h.get_by_label("prod").click();
    h.run_steps(2);
    h.key_press(egui::Key::F2);
    h.run_steps(3);
    let field = h.get(
        egui_kittest::kittest::By::new()
            .role(egui::accesskit::Role::TextInput)
            .value("prod"),
    );
    field.focus();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("production".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert!(
        h.state().actions.iter().any(|a| matches!(
            a,
            TreeAction::RenameFolder { from, to } if from == "prod" && to == "production"
        )),
        "{:?}",
        h.state().actions
    );
    assert!(h.state().state.renaming_folder.is_none());
}

#[test]
fn connection_menu_duplicates_the_connection() {
    let mut h = harness(StubSource::new());
    menu(&mut h, "local", None, "Duplicate");
    assert!(
        h.state()
            .actions
            .contains(&TreeAction::Duplicate("local".into())),
        "{:?}",
        h.state().actions
    );
}

#[test]
fn connection_menu_moves_it_into_a_folder() {
    let mut h = harness(StubSource::new());
    menu(&mut h, "local", Some("Move to folder"), "prod");
    let actions = &h.state().actions;
    assert!(
        actions.iter().any(|a| matches!(
            a,
            TreeAction::MoveConnection {
                conn,
                folder: Some(folder),
                target: TreeDrop::End,
            } if conn == "local" && folder == "prod"
        )),
        "{actions:?}"
    );
}

#[test]
fn folder_button_opens_an_inline_row_and_creates() {
    let mut h = harness(StubSource::new());
    h.get_by_label("New folder").click();
    h.run_steps(3);
    let field = h
        .get_all(
            egui_kittest::kittest::By::new()
                .role(egui::accesskit::Role::TextInput)
                .value(""),
        )
        .last()
        .expect("the new folder field");
    field.focus();
    h.run_steps(1);
    h.event(egui::Event::Text("Local".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert!(
        h.state()
            .actions
            .contains(&TreeAction::CreateFolder("Local".into())),
        "{:?}",
        h.state().actions
    );
    assert!(h.state().state.new_folder.is_none());
}

#[test]
fn the_header_button_creates_inside_the_selected_folder() {
    let mut h = harness(StubSource::new());
    h.get_by_label("prod").click();
    h.run_steps(2);
    h.get_by_label("New folder").click();
    h.run_steps(3);
    assert_eq!(h.state().state.new_folder_parent.as_deref(), Some("prod"));
    assert!(
        h.state().state.is_folder_expanded("prod"),
        "the folder opens to show the inline row"
    );
}

#[test]
fn nested_folders_start_closed_and_open_with_their_parent() {
    let mut source = StubSource::new();
    source.folders.push("prod/team".into());
    let mut h = harness(source);
    assert!(
        h.query_by_label("team").is_none(),
        "a nested folder is hidden while its parent is closed"
    );
    h.get_by_label("Expand prod").click();
    h.run_steps(2);
    h.get_by_label("team");
    h.get_by_label("Expand team");
}

#[test]
fn a_folder_menu_creates_a_nested_folder() {
    let mut h = harness(StubSource::new());
    h.get_by_label("prod").click_secondary();
    h.run_steps(2);
    // The header's button carries the same label; the menu entry is the last one.
    h.get_all_by_label("New folder")
        .last()
        .expect("the folder menu entry")
        .click();
    h.run_steps(3);
    assert_eq!(
        h.state().state.new_folder_parent.as_deref(),
        Some("prod"),
        "the inline row belongs to the folder it was opened from"
    );
    let field = h
        .get_all(
            egui_kittest::kittest::By::new()
                .role(egui::accesskit::Role::TextInput)
                .value(""),
        )
        .last()
        .expect("the new folder field");
    field.focus();
    h.run_steps(1);
    h.event(egui::Event::Text("Team".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert!(
        h.state()
            .actions
            .contains(&TreeAction::CreateFolder("prod/Team".into())),
        "{:?}",
        h.state().actions
    );
    assert!(h.state().state.new_folder.is_none());
    assert!(h.state().state.new_folder_parent.is_none());
}

#[test]
fn f2_renames_a_nested_folder_inside_its_parent() {
    let mut source = StubSource::new();
    source.folders.push("prod/team".into());
    let mut h = harness(source);
    open_folder(&mut h, "prod");
    h.get_by_label("team").click();
    h.run_steps(2);
    h.key_press(egui::Key::F2);
    h.run_steps(3);
    let field = h.get(
        egui_kittest::kittest::By::new()
            .role(egui::accesskit::Role::TextInput)
            .value("team"),
    );
    field.focus();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("squad".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert!(
        h.state().actions.iter().any(|a| matches!(
            a,
            TreeAction::RenameFolder { from, to } if from == "prod/team" && to == "prod/squad"
        )),
        "{:?}",
        h.state().actions
    );
}

#[test]
fn expanding_loaded_node_emits_nothing() {
    let mut h = harness(StubSource::new());
    open_folder(&mut h, "prod");
    h.state_mut()
        .state
        .set_expanded("prod", &NodePath::root(), true);
    h.run_steps(2);
    h.get_by_label("Expand public").click();
    h.run_steps(2);
    assert!(h.state().actions.is_empty());
    h.get_by_label("orders");
}

#[test]
fn connection_context_menu_refreshes_connected_only() {
    let mut h = harness(StubSource::new());
    open_folder(&mut h, "prod");
    h.get_by_label("local").click_secondary();
    h.run_steps(2);
    assert!(
        h.query_by_label("Refresh").is_none(),
        "a disconnected connection has nothing to refresh"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(100);
    h.get_by_label("prod-db").click_secondary();
    h.run_steps(2);
    h.get_by_label("Refresh").click();
    h.run_steps(2);
    assert!(h.state().actions.contains(&TreeAction::Refresh {
        conn: "prod".into(),
        path: NodePath::root(),
    }));
}

fn with_objects() -> StubSource {
    StubSource {
        objects: true,
        ..StubSource::new()
    }
}

/// Opens `label`'s context menu and clicks `item` (in a submenu after `submenu`, if any).
fn menu(h: &mut Harness<'static, App>, label: &str, submenu: Option<&str>, item: &str) {
    h.run_steps(100);
    h.get_by_label(label).click_secondary();
    h.run_steps(2);
    if let Some(sub) = submenu {
        // A submenu button reads "<label> ⏵".
        h.get_by_label(&format!("{sub} ⏵")).hover();
        h.run_steps(30);
    }
    h.get_all_by_label(item)
        .last()
        .unwrap_or_else(|| panic!("menu entry {item}"))
        .click();
    h.run_steps(2);
}

#[test]
fn object_menu_entries_follow_the_source() {
    let mut h = harness(StubSource::new());
    open_prod(&mut h);
    h.run_steps(100);
    h.get_by_label("orders").click_secondary();
    h.run_steps(2);
    assert!(
        h.query_by_label("Drop…").is_none(),
        "no object entries by default"
    );

    let mut h = harness(with_objects());
    open_prod(&mut h);
    menu(&mut h, "orders", None, "Drop…");
    menu(&mut h, "orders", Some("Generate SQL"), "SELECT");
    menu(&mut h, "orders", None, "Open editor");
    menu(&mut h, "public", Some("New"), "View");
    let actions = &h.state().actions;
    assert!(matches!(&actions[0], TreeAction::DropObject { node, .. } if node.name == "orders"));
    assert!(matches!(
        &actions[1],
        TreeAction::GenerateSql {
            template: SqlTemplate::Select,
            ..
        }
    ));
    assert!(matches!(&actions[2], TreeAction::OpenObject { .. }));
    assert!(matches!(
        &actions[3],
        TreeAction::NewObject { node: Some(n), kind: ObjectKind::View, .. } if n.name == "public"
    ));
}

#[test]
fn f2_renames_inline() {
    let mut h = harness(with_objects());
    open_prod(&mut h);
    h.get_by_label("orders").click();
    h.run_steps(2);
    h.key_press(egui::Key::F2);
    h.run_steps(3);
    let field = h.get(
        egui_kittest::kittest::By::new()
            .role(egui::accesskit::Role::TextInput)
            .value("orders"),
    );
    field.focus();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("purchases".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert!(
        h.state().actions.iter().any(|a| matches!(
            a,
            TreeAction::RenameObject { node, to, .. } if node.name == "orders" && to == "purchases"
        )),
        "{:?}",
        h.state().actions
    );
    assert!(h.state().state.renaming.is_none());
}

#[test]
fn only_live_connections_offer_reconnect() {
    let mut h = harness(StubSource::new());
    open_folder(&mut h, "prod");
    h.get_by_label("prod-db").hover();
    h.run_steps(2);
    // "local" is disconnected; only "prod-db" has a reconnect button.
    assert_eq!(h.get_all_by_label("Reconnect").count(), 1);
    h.get_by_label("Reconnect").click();
    h.run_steps(2);
    assert!(
        h.state()
            .actions
            .contains(&TreeAction::Reconnect("prod".into()))
    );
}

#[test]
fn only_live_connections_offer_disconnect() {
    let mut h = harness(StubSource::new());
    open_folder(&mut h, "prod");
    h.get_by_label("prod-db").hover();
    h.run_steps(2);
    // "local" is disconnected; only "prod-db" can be disconnected.
    assert_eq!(h.get_all_by_label("Disconnect connection").count(), 1);
    h.get_by_label("Disconnect connection").click();
    h.run_steps(2);
    assert!(
        h.state()
            .actions
            .contains(&TreeAction::Disconnect("prod".into()))
    );
}

#[test]
fn connection_state_is_an_icon_without_text() {
    let mut h = harness(StubSource::new());
    h.state_mut().state.expand_folder("prod");
    h.run_steps(2);
    let painted: Vec<String> = h
        .output()
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_string()),
            _ => None,
        })
        .collect();
    assert!(
        !painted
            .iter()
            .any(|text| matches!(text.as_str(), "connected" | "disconnected")),
        "the state is written out: {painted:?}"
    );
    h.get_by_label("connected");
    h.get_by_label("disconnected");
}

#[test]
fn connection_actions_appear_on_hover_and_hide_at_rest() {
    let mut h = harness(StubSource::new());
    open_folder(&mut h, "prod");
    assert!(h.query_by_label("Disconnect connection").is_none());
    h.get_by_label("prod-db").hover();
    h.run_steps(2);
    h.get_by_label("Disconnect connection");
    h.get_by_label("local").hover();
    h.run_steps(2);
    assert!(h.query_by_label("Disconnect connection").is_none());
}

#[test]
fn connection_rows_do_not_show_the_agent_access_eye() {
    let mut source = StubSource::new();
    source.local_access = sqlr_core::AgentAccess::Read;
    let h = harness(source);
    assert!(
        h.query_by_label(theme::icons::VIEW).is_none(),
        "the connection row still shows the agent access eye"
    );
}

#[test]
fn connection_actions_sit_at_the_right_edge() {
    let mut h = harness(StubSource::new());
    open_folder(&mut h, "prod");
    h.get_by_label("prod-db").hover();
    h.run_steps(2);
    let disconnect = h.get_by_label("Disconnect connection").rect();
    let reconnect = h.get_by_label("Reconnect").rect();
    let name = h.get_by_label("prod-db").rect();
    assert!(
        disconnect.right() > 350.0,
        "the actions are not anchored to the right edge: {disconnect:?}"
    );
    assert!(disconnect.left() > name.right());
    assert!(reconnect.left() > name.right());
}

#[test]
fn connection_color_tints_its_root_row() {
    let mut h = harness(StubSource::new());
    open_folder(&mut h, "prod");
    let fill = Color32::from_rgba_unmultiplied(200, 40, 40, 40);
    assert!(
        h.output().shapes.iter().any(|shape| matches!(
            &shape.shape,
            egui::Shape::Rect(rect) if rect.fill == fill
        )),
        "the connection's own color is not used as its root row background"
    );
}

#[test]
fn first_row_hover_background_is_not_clipped() {
    let mut h = harness(StubSource::new());
    h.get_by_label("local").hover();
    h.run_steps(2);
    let palette = theme::palette(&h.ctx);
    let (background, clip) = h
        .output()
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect) if rect.fill == palette.hover => {
                Some((rect.rect, shape.clip_rect))
            }
            _ => None,
        })
        .expect("the hovered row's background");
    assert!(
        background.top() >= clip.top(),
        "the first row's hover background is clipped at the top: {background:?} in {clip:?}"
    );
    assert!(
        background.bottom() <= clip.bottom(),
        "the hover background is clipped at the bottom: {background:?} in {clip:?}"
    );
}

#[test]
fn tree_folder_shows_loaded_count() {
    let mut h = harness(StubSource::with_views());
    open_prod(&mut h);
    // The folder's children are loaded: their count shows, open or closed.
    h.get_by_label("views");
    h.get_by_label("3");
    h.state_mut().state.set_expanded(
        "prod",
        &NodePath::root().child("public").child("views"),
        true,
    );
    h.run_steps(2);
    h.get_by_label("v1");
    h.get_by_label("3");

    // Not loaded yet: no count (and no query to get one).
    let mut source = StubSource::with_views();
    source.views = None;
    let mut h = harness(source);
    open_prod(&mut h);
    h.get_by_label("views");
    assert!(h.query_by_label("3").is_none());
    assert!(h.state().actions.is_empty(), "{:?}", h.state().actions);
}

#[test]
fn tree_hides_loaded_empty_folders_unless_asked() {
    let mut source = StubSource::with_views();
    source.views = Some(Vec::new());
    let mut h = harness(source);
    open_prod(&mut h);
    assert!(h.query_by_label("views").is_none());

    h.state_mut().state.show_empty_folders = true;
    h.run_steps(2);
    h.get_by_label("views");
    h.get_by_label("0");

    // A folder that is not loaded yet stays: nothing says it is empty.
    let mut source = StubSource::with_views();
    source.views = None;
    let mut h = harness(source);
    open_prod(&mut h);
    h.get_by_label("views");
}

#[test]
fn connection_rows_show_their_detail() {
    let mut h = harness(StubSource::new());
    h.get_by_label("app.db");
    open_folder(&mut h, "prod");
    h.get_by_label("PostgreSQL");
}

#[test]
fn tree_failed_connection_offers_retry() {
    let mut source = StubSource::new();
    source.local_badge = ConnBadge::Failed("connection refused\n  at 127.0.0.1:5432".into());
    let mut h = harness(source);
    h.get_by_label("Could not connect: connection refused");
    h.get_by_label("Retry").click();
    h.run_steps(2);
    assert_eq!(h.state().actions, [TreeAction::Retry("local".into())]);
    h.state_mut().actions.clear();
    h.get_by_label("Edit").click();
    h.run_steps(2);
    assert_eq!(h.state().actions, [TreeAction::Edit("local".into())]);
}

#[test]
fn table_context_menu_entries() {
    let source = StubSource {
        ddl: true,
        ..StubSource::new()
    };
    let mut h = harness(source);
    open_prod(&mut h);
    h.run_steps(100);
    h.get_by_label("orders").click_secondary();
    h.run_steps(2);
    for entry in [
        "Open data",
        "Open DDL",
        "Import/Export ⏵",
        "Copy name",
        "Refresh",
    ] {
        h.get_by_label(entry);
    }
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    let orders = node(
        &NodePath::root().child("public"),
        "orders",
        NodeKind::Table,
        true,
    );
    for (entry, action) in [
        (
            "Open DDL",
            TreeAction::OpenDdl {
                conn: "prod".into(),
                node: orders.clone(),
            },
        ),
        (
            "Copy name",
            TreeAction::CopyName {
                conn: "prod".into(),
                node: orders.clone(),
                qualified: false,
            },
        ),
    ] {
        h.state_mut().actions.clear();
        menu(&mut h, "orders", None, entry);
        assert_eq!(h.state().actions, [action], "{entry}");
    }
    h.state_mut().actions.clear();
    menu(&mut h, "orders", Some("Import/Export"), "Export data…");
    assert_eq!(
        h.state().actions,
        [TreeAction::ExportData {
            conn: "prod".into(),
            node: orders.clone(),
        }]
    );

    // A schema has no data and, without DDL support, no "Open DDL".
    h.get_by_label("public").click_secondary();
    h.run_steps(2);
    assert!(h.query_by_label("Import/Export ⏵").is_none());
    let mut h = harness(StubSource::new());
    open_prod(&mut h);
    h.run_steps(100);
    h.get_by_label("orders").click_secondary();
    h.run_steps(2);
    assert!(h.query_by_label("Open DDL").is_none());
}

#[test]
fn table_menu_opens_the_editor_first() {
    let mut h = harness(StubSource {
        ddl: true,
        ..with_objects()
    });
    open_prod(&mut h);
    h.run_steps(100);
    h.get_by_label("orders").click_secondary();
    h.run_steps(2);
    let open = h.get_by_label("Open editor").rect().top();
    for entry in [
        "Open data",
        "Open DDL",
        "Import/Export ⏵",
        "New ⏵",
        "Generate SQL ⏵",
        "Rename",
        "Truncate…",
        "Drop…",
        "Refresh",
        "Copy name",
    ] {
        assert!(
            open < h.get_by_label(entry).rect().top(),
            "{entry} is above Open editor"
        );
    }
}

#[test]
fn connection_context_menu_entries() {
    let mut h = harness(StubSource::new());
    open_folder(&mut h, "prod");
    for (entry, action) in [
        ("New editor tab", TreeAction::NewEditorTab("prod".into())),
        ("Copy connection URL", TreeAction::CopyUrl("prod".into())),
    ] {
        h.state_mut().actions.clear();
        menu(&mut h, "prod-db", None, entry);
        assert_eq!(h.state().actions, [action], "{entry}");
    }
}

#[test]
fn header_refresh_refreshes_the_selection() {
    let mut h = harness(StubSource::new());
    h.get_by_label("Refresh selected").click();
    h.run_steps(2);
    assert!(h.state().actions.is_empty());
    open_prod(&mut h);
    h.get_by_label("public").click();
    h.run_steps(2);
    h.get_by_label("Refresh selected").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        [TreeAction::Refresh {
            conn: "prod".into(),
            path: NodePath::root().child("public"),
        }]
    );
}

/// Many connections with long names.
struct ManySource {
    names: Vec<String>,
}

impl SchemaSource for ManySource {
    fn connections(&self) -> Vec<TreeConn<'_>> {
        self.names
            .iter()
            .map(|name| TreeConn {
                id: name,
                name,
                driver: "postgres",
                detail: Some("PostgreSQL"),
                group: None,
                color: None,
                badge: ConnBadge::Connected,
                tint: None,
                open_transaction: false,
                agent_access: Default::default(),
            })
            .collect()
    }

    fn children(&self, _conn: &str, _path: &NodePath) -> Children<'_> {
        Children::NotLoaded
    }
}

#[test]
fn explorer_many_long_connections() {
    let names: Vec<String> = (0..50)
        .map(|i| {
            let stem = format!("connection {i:02} 🚀 analytics warehouse replica ");
            stem.repeat(3).chars().take(80).collect()
        })
        .collect();
    let width = 300.0;
    let mut h = Harness::builder()
        .with_size(vec2(width, 500.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut (TreeState, ManySource, Vec<TreeAction>)| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                let a = schema_tree(ui, &mut app.0, &app.1);
                app.2.extend(a);
            },
            (
                TreeState::default(),
                ManySource {
                    names: names.clone(),
                },
                Vec::new(),
            ),
        );
    h.run_steps(3);
    // The visible rows are drawn, truncated inside the panel; the rest scroll.
    let first = h.get_by_label(&names[0]);
    assert!(
        first.rect().right() <= width + 0.5,
        "the name runs past the panel: {:?}",
        first.rect()
    );
    assert!(
        h.query_by_label(&names[49]).is_none(),
        "every row was drawn"
    );
    // Hover exposes only this row's actions, inside the panel.
    first.hover();
    h.run_steps(2);
    for button in h.get_all_by_label("Disconnect connection") {
        assert!(button.rect().right() <= width + 0.5, "{:?}", button.rect());
    }
}

/// The rectangles painted in `fill`, in paint order.
fn rects_filled(h: &Harness<'static, App>, fill: Color32) -> Vec<egui::Rect> {
    h.output()
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect) if rect.fill == fill => Some(rect.rect),
            _ => None,
        })
        .collect()
}

/// The color the text `text` was painted in, if it was painted.
fn text_color(h: &Harness<'static, App>, text: &str) -> Option<Color32> {
    h.output()
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(t) if t.galley.text() == text => Some(
                t.galley
                    .job
                    .sections
                    .first()
                    .map_or(t.fallback_color, |s| s.format.color),
            )
            .map(|c| {
                if c == Color32::PLACEHOLDER {
                    t.fallback_color
                } else {
                    c
                }
            }),
            _ => None,
        })
}

#[test]
fn hover_and_selection_cover_the_whole_row() {
    let mut h = harness(StubSource::new());
    let palette = Palette::dark();
    let name = h.get_by_label("local").rect();
    // Hovering the empty space at the right of the row highlights the full row.
    let blank = egui::pos2(380.0, name.center().y);
    h.event(egui::Event::PointerMoved(blank));
    h.run_steps(2);
    let hover = rects_filled(&h, palette.hover);
    assert!(
        hover
            .iter()
            .any(|r| r.left() < name.left() - 20.0 && r.right() > 390.0 && r.contains(blank)),
        "no full-width hover fill: {hover:?}"
    );
    // A click there selects the row, and the selection spans the row as well.
    click_at(&mut h, blank);
    assert_eq!(
        h.state().state.selected,
        Some(("local".to_string(), NodePath::root()))
    );
    let selected = rects_filled(&h, palette.selection);
    assert!(
        selected
            .iter()
            .any(|r| r.left() < name.left() - 20.0 && r.right() > 390.0),
        "no full-width selection fill: {selected:?}"
    );
}

#[test]
fn connection_menu_ends_with_a_separated_danger_delete() {
    let mut h = harness(StubSource::new());
    let row = h.get_by_label("local").rect();
    h.get_by_label("local").click_secondary();
    h.run_steps(3);
    let palette = Palette::dark();
    // The menu opens below the row, so the row it acts on stays visible, and the row keeps
    // its highlight while the menu is open.
    let edit = h.get_by_label("Edit").rect();
    assert!(
        edit.top() >= row.bottom(),
        "menu {edit:?} covers row {row:?}"
    );
    assert!(
        rects_filled(&h, palette.hover)
            .iter()
            .any(|r| r.contains(row.center())),
        "the row is not highlighted while its menu is open"
    );
    // Delete is last, after a separator, in the error color.
    let delete = h.get_by_label("Delete").rect();
    let previous = h.get_by_label("Agent access ⏵").rect();
    assert!(delete.top() > previous.bottom());
    let separator = h.output().shapes.iter().any(|shape| match &shape.shape {
        egui::Shape::LineSegment { points, .. } => {
            points[0].y == points[1].y
                && points[0].y > previous.bottom()
                && points[0].y < delete.top()
        }
        _ => false,
    });
    assert!(separator, "no separator between Agent access and Delete");
    assert_eq!(text_color(&h, "Delete"), Some(palette.error));
    // Entries keep their own icons: Duplicate and Copy connection URL differ.
    assert_ne!(theme::icons::DUPLICATE, theme::icons::LINK);
}

#[test]
fn menu_labels_align_with_and_without_icons() {
    let mut h = harness(StubSource::new());
    h.get_by_label("local").click_secondary();
    h.run_steps(3);
    // "Agent access" has an icon now and "Edit" always had one; their labels share a column.
    let label_x = |h: &Harness<'static, App>, text: &str| {
        h.output()
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(t) if t.galley.text() == text => Some(t.pos.x),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{text} was not painted"))
    };
    let edit = label_x(&h, "Edit");
    for other in ["Agent access", "Delete", "New editor tab"] {
        assert!(
            (label_x(&h, other) - edit).abs() < 0.5,
            "{other} is not aligned with Edit"
        );
    }
}

#[test]
fn an_expanded_empty_connection_says_it_has_no_objects() {
    let mut source = StubSource::new();
    source.prod_root.clear();
    let mut h = harness(source);
    h.state_mut().state.expand_folder("prod");
    h.state_mut()
        .state
        .set_expanded("prod", &NodePath::root(), true);
    h.run_steps(2);
    h.get_by_label("No objects");
}
