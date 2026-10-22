use egui::Color32;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_ui::{
    ChipAction, ChipItem, ChipState, Crumb, ToastKind, Toasts, breadcrumb, connection_chip,
    empty_state, status_segment,
};

fn item(id: &str, name: &str, state: ChipState) -> ChipItem {
    ChipItem {
        id: id.into(),
        name: name.into(),
        detail: "PostgreSQL".into(),
        driver: "postgres".into(),
        color: Some(Color32::from_rgb(200, 60, 60)),
        state,
    }
}

#[derive(Default)]
struct ChipTest {
    items: Vec<ChipItem>,
    current: Option<usize>,
    actions: Vec<ChipAction>,
}

#[test]
fn chip_lists_connections_and_actions() {
    let state = ChipTest {
        items: vec![
            item("a", "prod-db", ChipState::Connected),
            item("b", "staging", ChipState::Offline),
        ],
        current: Some(0),
        actions: Vec::new(),
    };
    let mut h = Harness::builder().with_size((600.0, 400.0)).build_ui_state(
        |ui, s: &mut ChipTest| {
            let current = s.current.map(|i| s.items[i].clone());
            if let Some(a) = connection_chip(ui, current.as_ref(), || s.items.clone()) {
                s.actions.push(a);
            }
        },
        state,
    );
    h.run();
    // Closed: only the chip names the current connection.
    assert!(h.query_by_label("staging").is_none());
    h.get_by_label("prod-db").click();
    h.run();
    h.get_by_label("CONNECTIONS");
    h.get_by_label("staging").click();
    h.run();
    assert_eq!(h.state().actions, vec![ChipAction::Select("b".into())]);

    h.get_by_label("prod-db").click();
    h.run();
    h.get_by_label("Disconnect").click();
    h.run();
    assert_eq!(h.state().actions.last(), Some(&ChipAction::Disconnect));

    h.get_by_label("prod-db").click();
    h.run();
    h.get_by_label("New connection…").click();
    h.run();
    assert_eq!(h.state().actions.last(), Some(&ChipAction::New));
}

#[test]
fn chip_without_connection_offers_only_new() {
    let state = ChipTest {
        items: vec![item("a", "prod-db", ChipState::Offline)],
        current: None,
        actions: Vec::new(),
    };
    let mut h = Harness::builder().with_size((600.0, 400.0)).build_ui_state(
        |ui, s: &mut ChipTest| {
            let current = s.current.map(|i| s.items[i].clone());
            if let Some(a) = connection_chip(ui, current.as_ref(), || s.items.clone()) {
                s.actions.push(a);
            }
        },
        state,
    );
    h.run();
    h.get_by_label("No connection").click();
    h.run();
    h.get_by_label("Disconnect").click();
    h.run();
    if h.query_by_label("Edit connection…").is_none() {
        h.get_by_label("No connection").click();
        h.run();
    }
    h.get_by_label("Edit connection…").click();
    h.run();
    assert!(h.state().actions.is_empty(), "{:?}", h.state().actions);
}

#[test]
fn breadcrumb_click_returns_index() {
    let crumbs = vec![
        Crumb {
            label: "prod-db".into(),
            icon: None,
        },
        Crumb {
            label: "sales".into(),
            icon: None,
        },
        Crumb {
            label: "Query 1".into(),
            icon: None,
        },
    ];
    let mut h = Harness::builder().with_size((600.0, 60.0)).build_ui_state(
        move |ui, clicked: &mut Vec<usize>| {
            if let Some(i) = breadcrumb(ui, &crumbs, "Auto · ⌘↵ run") {
                clicked.push(i);
            }
        },
        Vec::new(),
    );
    h.run();
    h.get_by_label("Auto · ⌘↵ run");
    h.get_by_label("sales").click();
    h.run();
    h.get_by_label("prod-db").click();
    h.run();
    assert_eq!(h.state(), &vec![1, 0]);
}

#[test]
fn toasts_expire_and_cap_at_three() {
    let mut toasts = Toasts::default();
    toasts.push(ToastKind::Info, "one");
    toasts.push(ToastKind::Success, "two");
    toasts.push(ToastKind::Error, "three");
    toasts.push(ToastKind::Info, "four");
    // None is dropped: the fourth waits for a free slot.
    assert_eq!(toasts.texts(), vec!["one", "two", "three", "four"]);
    let mut h = Harness::builder()
        .with_size((800.0, 600.0))
        .with_step_dt(0.5)
        .build_ui_state(|ui, t: &mut Toasts| t.show(ui.ctx()), toasts);
    h.run_steps(2);
    h.get_by_label("one");
    h.get_by_label("three");
    assert!(h.query_by_label("four").is_none(), "only three at once");
    // Info and success go after 4 s; "four" then shows for its own 4 s.
    h.run_steps(8);
    assert!(h.query_by_label("one").is_none());
    h.get_by_label("four");
    h.get_by_label("three");
    h.run_steps(10);
    assert!(h.state().texts().is_empty(), "{:?}", h.state().texts());
    assert!(h.query_by_label("three").is_none());
}

#[test]
fn dismissing_the_last_toast_does_not_panic() {
    // With nothing left to fade, the wake-up time is "never": it must not become a Duration.
    let mut toasts = Toasts::default();
    toasts.push(ToastKind::Info, "Installing the hana driver…");
    let mut h = Harness::builder()
        .with_size((800.0, 600.0))
        .build_ui_state(|ui, t: &mut Toasts| t.show(ui.ctx()), toasts);
    h.run_steps(2);
    h.get_by_label("Dismiss").click();
    h.run_steps(3);
    assert!(h.state().texts().is_empty());
}

#[test]
fn toast_close_button_dismisses_it() {
    let mut toasts = Toasts::default();
    toasts.push(ToastKind::Error, "it broke");
    toasts.push(ToastKind::Info, "fyi");
    let mut h = Harness::builder()
        .with_size((800.0, 600.0))
        .build_ui_state(|ui, t: &mut Toasts| t.show(ui.ctx()), toasts);
    h.run_steps(2);
    assert_eq!(h.get_all_by_label("Dismiss").count(), 2);
    h.get_all_by_label("Dismiss").next().unwrap().click();
    h.run_steps(2);
    assert_eq!(h.state().texts(), vec!["fyi"]);
}

#[test]
fn hovered_error_toast_stays() {
    let mut toasts = Toasts::default();
    toasts.push(ToastKind::Error, "it broke");
    let mut h = Harness::builder()
        .with_size((800.0, 600.0))
        .with_step_dt(0.5)
        .build_ui_state(|ui, t: &mut Toasts| t.show(ui.ctx()), toasts);
    h.run_steps(2);
    h.get_by_label("it broke").hover();
    h.run_steps(30);
    assert_eq!(h.state().texts(), vec!["it broke"]);
}

#[test]
fn status_segment_and_empty_state() {
    let mut h = Harness::builder().with_size((600.0, 400.0)).build_ui_state(
        |ui, clicks: &mut usize| {
            status_segment(ui, "Ln 2, Col 5", None);
            if empty_state(ui, "No tabs", "Open an editor", Some("New tab")) {
                *clicks += 1;
            }
            empty_state(ui, "Nothing", "No action here", None);
        },
        0,
    );
    h.run();
    h.get_by_label("Ln 2, Col 5");
    h.get_by_label("No tabs");
    h.get_by_label("Open an editor");
    h.get_by_label("New tab").click();
    h.run();
    assert_eq!(*h.state(), 1);
}
