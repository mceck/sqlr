use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{DriverJobView, DriverManagerAction, DriverManagerState, DriverRow, driver_manager};

struct App {
    state: DriverManagerState,
    rows: Vec<DriverRow>,
    actions: Vec<DriverManagerAction>,
    height: f32,
}

fn row(id: &str, name: &str) -> DriverRow {
    DriverRow {
        id: id.into(),
        name: name.into(),
        family: "Relational".into(),
        builtin: false,
        installed: Vec::new(),
        active: None,
        manual: false,
        description: format!("{name} driver"),
        homepage: String::new(),
        license: "MIT".into(),
        download: None,
        job: None,
    }
}

fn rows() -> Vec<DriverRow> {
    vec![
        DriverRow {
            builtin: true,
            ..row("sqlite", "SQLite")
        },
        DriverRow {
            installed: vec!["1.0.0".into(), "1.1.0".into()],
            active: Some("1.1.0".into()),
            manual: true,
            ..row("hana", "SAP HANA")
        },
    ]
}

fn harness(rows: Vec<DriverRow>) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(vec2(900.0, 560.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                if let Some(a) = driver_manager(ui, &mut app.state, &app.rows) {
                    app.actions.push(a);
                }
                app.height = ui.min_rect().height();
            },
            App {
                state: DriverManagerState::default(),
                rows,
                actions: Vec::new(),
                height: 0.0,
            },
        );
    h.run_steps(2);
    h
}

/// Clicks the list entry named `name` (the first node: the details repeat the name).
fn select(h: &mut Harness<'static, App>, name: &str) {
    h.get_all_by_label(name).next().expect(name).click();
    h.run_steps(2);
}

fn click(h: &mut Harness<'static, App>, label: &str) -> DriverManagerAction {
    h.state_mut().actions.clear();
    h.get_by_label(label).click();
    h.run_steps(2);
    let actions = h.state().actions.clone();
    assert_eq!(actions.len(), 1, "{label}: {actions:?}");
    actions[0].clone()
}

#[test]
fn driver_manager_lists_bundled_and_installed_drivers() {
    let h = harness(rows());
    for name in ["SQLite", "SAP HANA"] {
        h.get_all_by_label(name).next().expect(name);
    }
    // Nothing offers a download unless the release has the driver.
    assert!(h.query_by_label("Download and install").is_none());
    assert!(h.query_by_label("Update to 1.1.0").is_none());
}

#[test]
fn versions_can_be_activated_and_removed() {
    let mut h = harness(rows());
    select(&mut h, "SAP HANA");
    // The older version can be activated (rollback) or removed; the active one cannot.
    assert_eq!(
        click(&mut h, "Activate 1.0.0"),
        DriverManagerAction::Activate {
            id: "hana".into(),
            version: "1.0.0".into()
        }
    );
    assert_eq!(
        click(&mut h, "Remove 1.0.0"),
        DriverManagerAction::Remove {
            id: "hana".into(),
            version: Some("1.0.0".into())
        }
    );
    assert!(h.query_by_label("Activate 1.1.0").is_none());
    assert_eq!(
        click(&mut h, "Remove driver"),
        DriverManagerAction::Remove {
            id: "hana".into(),
            version: None
        }
    );
}

#[test]
fn built_in_drivers_cannot_be_removed() {
    let mut h = harness(rows());
    select(&mut h, "SQLite");
    h.get_all_by_label("Built in").next().unwrap();
    assert!(h.query_by_label("Remove driver").is_none());
    assert!(h.query_by_label("Install from file…").is_some());
}

#[test]
fn search_filters_the_list() {
    let mut many = rows();
    for i in 0..4 {
        many.push(row(&format!("extra{i}"), &format!("Extra {i}")));
    }
    let mut h = harness(many);
    h.get_by_label("Search drivers").click();
    h.get_by_label("Search drivers").type_text("hana");
    h.run_steps(3);
    assert!(h.query_all_by_label("SAP HANA").next().is_some());
    assert!(h.query_all_by_label("SQLite").next().is_none());
}

#[test]
fn toolbar_offers_a_file_install_as_the_alternative() {
    let mut h = harness(rows());
    assert_eq!(
        click(&mut h, "Install from file…"),
        DriverManagerAction::InstallFromFile
    );
    h.get_by_label(
        "Bundled drivers ship with sqlr. Other drivers download from the sqlr release on \
         GitHub, or install from a local driver file.",
    );
    assert!(h.query_by_label("Refresh").is_none());
}

#[test]
fn an_available_driver_downloads_from_the_release() {
    let mut available = rows();
    available.push(DriverRow {
        download: Some("0.4.0".into()),
        ..row("duckdb", "DuckDB")
    });
    let mut h = harness(available);
    select(&mut h, "DuckDB");
    h.get_by_label("AVAILABLE");
    h.get_by_label(
        "Downloads version 0.4.0 for this computer from the sqlr release on GitHub and \
         checks its SHA-256.",
    );
    assert_eq!(
        click(&mut h, "Download and install"),
        DriverManagerAction::Download("duckdb".into())
    );
}

#[test]
fn an_installed_driver_updates_to_the_release_version() {
    let mut newer = rows();
    newer[1].download = Some("1.2.0".into());
    let mut h = harness(newer);
    select(&mut h, "SAP HANA");
    assert_eq!(
        click(&mut h, "Update to 1.2.0"),
        DriverManagerAction::Download("hana".into())
    );
}

#[test]
fn a_running_install_shows_progress_and_a_failed_one_its_message() {
    let mut running = rows();
    running[1].job = Some(DriverJobView::Running {
        done: 512,
        total: Some(1024),
    });
    let mut h = harness(running);
    select(&mut h, "SAP HANA");
    h.get_by_label("Installing… 50%");

    let mut failed = rows();
    failed[1].job = Some(DriverJobView::Failed("sha256 mismatch".into()));
    let mut h = harness(failed);
    select(&mut h, "SAP HANA");
    h.get_by_label("Install failed: sha256 mismatch");
    assert_eq!(
        click(&mut h, "Dismiss"),
        DriverManagerAction::ClearJob("hana".into())
    );
}

#[test]
fn an_installed_driver_can_be_restarted() {
    let mut h = harness(rows());
    select(&mut h, "SAP HANA");
    assert_eq!(
        click(&mut h, "Restart driver"),
        DriverManagerAction::Restart("hana".into())
    );
    // Built-in drivers run no separate process.
    select(&mut h, "SQLite");
    assert!(h.query_by_label("Restart driver").is_none());
}

#[test]
fn the_manager_does_not_grow_with_the_window() {
    // In a dialog the available height is the whole screen: the lists must not take it.
    for height in [700.0, 1600.0] {
        let mut h = Harness::builder()
            .with_size(vec2(900.0, height))
            .with_step_dt(0.01)
            .build_ui_state(
                |ui, app: &mut App| {
                    theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                    egui::Frame::NONE.show(ui, |ui| {
                        // A dialog gives its content unbounded room below.
                        ui.set_max_height(f32::INFINITY);
                        driver_manager(ui, &mut app.state, &app.rows);
                    });
                    app.height = ui.min_rect().height();
                },
                App {
                    state: DriverManagerState::default(),
                    rows: rows(),
                    actions: Vec::new(),
                    height: 0.0,
                },
            );
        h.run_steps(3);
        assert!(
            h.state().height < 640.0,
            "the manager takes {} px in a {height}-px window",
            h.state().height
        );
    }
}

#[test]
fn the_list_stacks_and_the_details_have_room() {
    let mut h = harness(rows());
    select(&mut h, "SAP HANA");
    let rect = |label: &str, nth: usize| {
        h.get_all_by_label(label)
            .nth(nth)
            .unwrap_or_else(|| panic!("{label} #{nth}"))
            .rect()
    };
    // The list is a column: each driver sits below the previous one.
    assert!(rect("SQLite", 0).top() > rect("SAP HANA", 0).bottom() - 1.0);
    // The details sit to the right of the list and are wide enough to read: a title is one
    // line, not one letter per line.
    let title = rect("SAP HANA", 1);
    assert!(title.left() > rect("SAP HANA", 0).right(), "{title:?}");
    assert!(
        title.height() < 40.0,
        "the title wraps letter by letter: {title:?}"
    );
    assert!(title.width() > 60.0, "{title:?}");
}
