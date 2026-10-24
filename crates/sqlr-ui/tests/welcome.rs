use egui::{Color32, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{DriverCard, RecentItem, WelcomeAction, welcome};

struct App {
    cards: Vec<DriverCard>,
    recent: Vec<RecentItem>,
    actions: Vec<WelcomeAction>,
}

fn cards() -> Vec<DriverCard> {
    [
        ("postgresql", "PostgreSQL", "TCP server", "postgres"),
        ("mysql", "MySQL / MariaDB", "TCP server", "mysql"),
        ("sqlite", "SQLite", "File", "sqlite"),
    ]
    .iter()
    .map(|(id, name, detail, icon)| DriverCard {
        id: id.to_string(),
        name: name.to_string(),
        detail: detail.to_string(),
        icon,
    })
    .collect()
}

fn harness(recent: Vec<RecentItem>, size: egui::Vec2) -> Harness<'static, App> {
    let app = App {
        cards: cards(),
        recent,
        actions: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(size)
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                if let Some(a) = welcome(ui, &app.cards, &app.recent) {
                    app.actions.push(a);
                }
            },
            app,
        );
    h.run_steps(2);
    h
}

fn click(h: &mut Harness<'static, App>, label: &str) -> WelcomeAction {
    h.state_mut().actions.clear();
    h.get_by_label(label).click();
    h.run_steps(2);
    let actions = &h.state().actions;
    assert_eq!(actions.len(), 1, "{label}: {actions:?}");
    actions[0].clone()
}

#[test]
fn welcome_actions() {
    let recent = vec![
        RecentItem {
            id: "c1".into(),
            name: "Production orders".into(),
            detail: "PostgreSQL · db.example".into(),
            color: Some(Color32::from_rgb(200, 60, 60)),
            when: "5 min ago".into(),
        },
        RecentItem {
            id: "c2".into(),
            name: "Local".into(),
            detail: "SQLite · app.db".into(),
            color: None,
            when: "yesterday".into(),
        },
    ];
    let mut h = harness(recent, vec2(1000.0, 800.0));
    h.get_by_label("sqlr");
    h.get_by_label("A fast, native database client");
    assert_eq!(click(&mut h, "Open demo database"), WelcomeAction::OpenDemo);
    assert_eq!(
        click(&mut h, "Import connections…"),
        WelcomeAction::ImportConnections
    );
    assert_eq!(
        click(&mut h, "Production orders"),
        WelcomeAction::OpenRecent("c1".into())
    );
    assert_eq!(
        click(&mut h, "Local"),
        WelcomeAction::OpenRecent("c2".into())
    );
    assert_eq!(
        click(&mut h, "PostgreSQL"),
        WelcomeAction::NewConnection(Some("postgresql".into()))
    );
    assert_eq!(
        click(&mut h, "More databases…"),
        WelcomeAction::MoreDatabases
    );
    assert_eq!(
        click(&mut h, "New connection…"),
        WelcomeAction::NewConnection(None)
    );
}

#[test]
fn welcome_without_recent_and_in_a_small_window() {
    let mut h = harness(Vec::new(), vec2(520.0, 420.0));
    h.get_by_label("No recent connections yet");
    // Every card stays inside the window.
    for label in ["PostgreSQL", "MySQL / MariaDB", "SQLite", "More databases…"] {
        let rect = h.get_by_label(label).rect();
        assert!(rect.right() <= 520.5, "{label}: {rect:?}");
    }
    assert_eq!(
        click(&mut h, "SQLite"),
        WelcomeAction::NewConnection(Some("sqlite".into()))
    );
}
