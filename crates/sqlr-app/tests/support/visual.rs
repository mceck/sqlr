use egui_kittest::{Harness, kittest::Queryable};
use sqlr_app::SqlrApp;

pub fn set_theme(h: &mut Harness<'static, SqlrApp>, light: bool) {
    sqlr_ui::theme::install_fonts(&h.ctx, &[]);
    h.get_by_label("File").click();
    h.run_steps(3);
    h.get_by_label_contains("Settings…").click();
    h.run_steps(5);
    h.get_by_label("Appearance").click();
    h.run_steps(3);
    h.get_by_label(if light { "Light" } else { "Dark" }).click();
    h.run_steps(3);
    h.get_by_label("Save").click();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while h.query_by_label("Close Preferences").is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "preferences save timed out"
        );
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h.run_steps(3);
}

pub fn capture(h: &mut Harness<'static, SqlrApp>, name: &str, light: bool) {
    let out = std::env::var_os("SQLR_AUDIT_DIR").map_or_else(
        || {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs/media/screenshots/audit")
        },
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&out).unwrap();
    h.event(egui::Event::PointerMoved(egui::pos2(1390.0, 890.0)));
    h.run_steps(8);
    h.render()
        .expect("GPU render")
        .save(out.join(format!(
            "{name}-{}.png",
            if light { "light" } else { "dark" }
        )))
        .unwrap();
}
