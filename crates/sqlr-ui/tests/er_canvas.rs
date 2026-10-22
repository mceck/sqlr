use egui::{Event, Modifiers, PointerButton, pos2, vec2};
use egui_kittest::Harness;
use sqlr_core::{ErColumn, ErEdge, ErModel, ErTable, TableRef, er_layout};
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{ColumnsShown, ErAction, ErView, er_canvas, er_measure};

fn table(name: &str, columns: usize) -> ErTable {
    ErTable {
        table: TableRef {
            database: None,
            schema: None,
            name: name.into(),
        },
        columns: (0..columns)
            .map(|i| ErColumn {
                name: format!("c{i}"),
                type_name: "int".into(),
                pk: i == 0,
                fk: i == 1,
                nullable: true,
            })
            .collect(),
    }
}

struct App {
    model: ErModel,
    layout: sqlr_core::ErLayout,
    view: ErView,
    actions: Vec<ErAction>,
}

fn app(tables: usize) -> App {
    let model = ErModel {
        tables: (0..tables).map(|i| table(&format!("t{i:03}"), 4)).collect(),
        edges: (1..tables)
            .map(|i| ErEdge {
                from: i,
                to: i - 1,
                columns: vec![("c1".into(), "c0".into())],
                unique: false,
                nullable: true,
            })
            .collect(),
    };
    let layout = er_layout(&model, &|t| er_measure(t, ColumnsShown::All));
    App {
        model,
        layout,
        view: ErView::default(),
        actions: Vec::new(),
    }
}

fn harness(tables: usize) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(vec2(900.0, 600.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                if let Some(a) = er_canvas(ui, &app.model, &mut app.layout, &mut app.view) {
                    app.actions.push(a);
                }
            },
            app(tables),
        );
    h.run_steps(3);
    h
}

#[test]
fn er_canvas_drag_moves_table() {
    let mut h = harness(2);
    let before = h.state().layout.positions[0];
    // The canvas draws the diagram's origin at its own top-left corner plus the pan; the
    // first table's header is a little inside its box.
    let origin = h.state().view.screen_origin;
    let grab = pos2(origin.x + before.0 + 20.0, origin.y + before.1 + 8.0);
    h.event(Event::PointerMoved(grab));
    h.step();
    h.event(Event::PointerButton {
        pos: grab,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    for k in 1..=5 {
        h.event(Event::PointerMoved(
            grab + vec2(10.0 * k as f32, 6.0 * k as f32),
        ));
        h.step();
    }
    h.event(Event::PointerButton {
        pos: grab + vec2(50.0, 30.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(2);
    let moved = h
        .state()
        .actions
        .iter()
        .find_map(|a| match a {
            ErAction::Moved(0, to) => Some(*to),
            _ => None,
        })
        .expect("a Moved action for the first table");
    assert!(
        (moved.0 - (before.0 + 50.0)).abs() < 2.0,
        "{moved:?} from {before:?}"
    );
    assert!(
        (moved.1 - (before.1 + 30.0)).abs() < 2.0,
        "{moved:?} from {before:?}"
    );
    assert_eq!(h.state().layout.positions[0], moved);
}

#[test]
fn er_canvas_500_tables_frame_budget() {
    // Zoomed in on a corner, most of 500 tables are off screen and are not painted.
    let mut h = harness(500);
    h.state_mut().view.zoom = 1.0;
    h.run_steps(3);
    let painted = h.state().view.painted;
    assert!(painted > 0, "something is on screen");
    assert!(
        painted < 100,
        "{painted} of 500 tables painted: off-screen boxes are culled"
    );
    // The edges are culled alike.
    assert!(h.state().view.painted_edges < 100);
}
