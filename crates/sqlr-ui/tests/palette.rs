use egui::{Key, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_ui::{
    PaletteGroup, PaletteItem, PaletteMode, PaletteOutput, PaletteState, fuzzy_rank, palette,
};

fn item(label: &str, group: PaletteGroup, fuzzy: bool) -> PaletteItem {
    PaletteItem {
        label: label.into(),
        detail: "conn · table".into(),
        group,
        fuzzy,
        icon: None,
        shortcut: None,
    }
}

fn objects() -> Vec<PaletteItem> {
    ["public.orders", "public.order_items", "sales.customers"]
        .iter()
        .map(|l| item(l, PaletteGroup::Object, true))
        .collect()
}

struct App {
    state: PaletteState,
    items: Vec<PaletteItem>,
    outputs: Vec<PaletteOutput>,
}

fn harness(items: Vec<PaletteItem>) -> Harness<'static, App> {
    let app = App {
        state: PaletteState::default(),
        items,
        outputs: Vec::new(),
    };
    Harness::builder()
        .with_size(vec2(900.0, 600.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                ui.label("background");
                let ctx = ui.ctx().clone();
                let out = palette(&ctx, &mut app.state, &app.items, 1);
                if out != PaletteOutput::None {
                    app.outputs.push(out);
                }
            },
            app,
        )
}

fn open(h: &mut Harness<'static, App>, mode: PaletteMode) {
    h.state_mut().state.open(mode);
    h.run_steps(3);
}

fn type_text(h: &mut Harness<'static, App>, text: &str) {
    h.event(egui::Event::Text(text.into()));
    h.run_steps(3);
}

#[test]
fn fuzzy_rank_orders_best_first() {
    let items = objects();
    let ranked = fuzzy_rank("ordit", &items, 50);
    assert_eq!(items[ranked[0]].label, "public.order_items");
    assert_eq!(fuzzy_rank("", &items, 50), vec![0, 1, 2]);
    assert_eq!(fuzzy_rank("", &items, 2), vec![0, 1]);
}

#[test]
fn opens_types_and_chooses() {
    let mut h = harness(objects());
    open(&mut h, PaletteMode::Objects);
    type_text(&mut h, "cust");
    assert_eq!(h.state().state.query, "cust");
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert!(h.state().outputs.contains(&PaletteOutput::QueryChanged));
    assert_eq!(h.state().outputs.last(), Some(&PaletteOutput::Chosen(2)));
}

#[test]
fn arrows_move_the_selection() {
    let mut h = harness(objects());
    open(&mut h, PaletteMode::Objects);
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().outputs.last(), Some(&PaletteOutput::Chosen(1)));
}

#[test]
fn non_fuzzy_items_follow() {
    let mut items = objects();
    items.insert(
        0,
        item("History: delete from logs", PaletteGroup::History, false),
    );
    let mut h = harness(items);
    open(&mut h, PaletteMode::Commands);
    type_text(&mut h, "cust");
    h.get_by_label("sales.customers");
    let history = h.get_by_label("History: delete from logs").rect();
    let customers = h.get_by_label("sales.customers").rect();
    assert!(
        history.top() > customers.top(),
        "the history item is listed last"
    );
    assert!(h.query_by_label("public.orders").is_none());
    // Choosing the second row picks the non-fuzzy item by its index in `items`.
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().outputs.last(), Some(&PaletteOutput::Chosen(0)));
}

#[test]
fn escape_closes() {
    let mut h = harness(objects());
    open(&mut h, PaletteMode::Objects);
    h.get_by_label("public.orders");
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().outputs.last(), Some(&PaletteOutput::Closed));
    assert_eq!(h.state().state.mode, None);
    assert!(h.query_by_label("public.orders").is_none());
}

#[test]
fn click_outside_closes() {
    let mut h = harness(objects());
    open(&mut h, PaletteMode::Objects);
    h.get_by_label("background").click();
    h.run_steps(2);
    assert_eq!(h.state().outputs.last(), Some(&PaletteOutput::Closed));
    assert_eq!(h.state().state.mode, None);
}

#[test]
fn clicking_a_row_chooses_it() {
    let mut h = harness(objects());
    open(&mut h, PaletteMode::Objects);
    h.get_by_label("public.order_items").click();
    h.run_steps(2);
    assert_eq!(h.state().outputs.last(), Some(&PaletteOutput::Chosen(1)));
}

#[test]
fn rows_follow_the_query_in_the_same_frame() {
    let mut h = harness(objects());
    open(&mut h, PaletteMode::Objects);
    h.get_by_label("public.orders");
    h.event(egui::Event::Text("cust".into()));
    h.step();
    assert_eq!(h.state().state.query, "cust");
    assert!(
        h.query_by_label("public.orders").is_none(),
        "the frame that typed the query still listed the old rows"
    );
    h.get_by_label("sales.customers");
}

#[test]
fn shortcuts_sit_in_a_right_aligned_column() {
    let mut items: Vec<PaletteItem> = ["Run statement", "New connection with a long label"]
        .iter()
        .map(|l| item(l, PaletteGroup::Command, true))
        .collect();
    items[0].shortcut = Some("⌘↵".into());
    items[1].shortcut = Some("⇧⌘N".into());
    let mut h = harness(items);
    open(&mut h, PaletteMode::Commands);
    let run = h.get_by_label("⌘↵").rect();
    let other = h.get_by_label("⇧⌘N").rect();
    assert!(
        (run.right() - other.right()).abs() < 1.0,
        "{run:?} vs {other:?}"
    );
    assert!(run.left() > h.get_by_label("Run statement").rect().right() + 100.0);
}
