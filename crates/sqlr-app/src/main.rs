use std::sync::Arc;
use std::time::Instant;

use sqlr_app::cli::{CliCommand, USAGE, parse_args, version_line};
use sqlr_app::{SqlrApp, init_logging, production_deps_in};

/// Wraps the app in a frameless central panel and logs the time to the first frame.
struct Shell {
    app: SqlrApp,
    started: Option<Instant>,
    budget: Option<(std::path::PathBuf, Instant, Option<f64>)>,
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::no_frame().show(ui, |ui| self.app.ui(ui));
        if let Some(started) = self.started.take() {
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            tracing::info!("first frame after {} ms", elapsed);
            if let Some((_, _, first)) = &mut self.budget {
                *first = Some(elapsed);
            }
        }
        if self
            .budget
            .as_ref()
            .is_some_and(|(_, started, _)| started.elapsed() > std::time::Duration::from_secs(2))
        {
            let (path, _, first) = self.budget.take().unwrap();
            let connected = self.app.connected_connection_count();
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let output = std::process::Command::new("ps")
                    .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                    .output()
                    .expect("measure resident memory");
                let rss = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse::<f64>()
                    .expect("RSS in KiB")
                    * 1024.0
                    / 1_000_000.0;
                let result = serde_json::json!({"first_frame_ms":first,"idle_rss_mb":rss,"connected":connected});
                std::fs::write(path, result.to_string()).expect("write performance report");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                ctx.request_repaint();
            });
        } else if self.budget.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

fn main() -> eframe::Result {
    let started = Instant::now();
    let (data_dir, config_dir) = match parse_args(std::env::args().skip(1)) {
        CliCommand::Run {
            data_dir,
            config_dir,
        } => (data_dir, config_dir),
        CliCommand::Version => {
            println!("{}", version_line());
            return Ok(());
        }
        CliCommand::Help => {
            print!("{USAGE}");
            return Ok(());
        }
        CliCommand::Error(message) => {
            eprintln!("sqlr: {message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let log_dir = data_dir
        .clone()
        .or_else(sqlr_store::default_data_dir)
        .map(|d| d.join("logs"));
    let _log_guard = init_logging(log_dir.as_deref());

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("sqlr")
        .with_inner_size([1280.0, 800.0]);
    if let Some(icon) = sqlr_app::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "sqlr",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let repaint = Arc::new(move || ctx.request_repaint());
            let deps = production_deps_in(repaint, config_dir, data_dir)?;
            let mut app = SqlrApp::new(deps, &cc.egui_ctx);
            let budget = std::env::var_os("SQLR_BUDGET_REPORT").map(|path| {
                app.connect_for_tests("budget-a");
                app.connect_for_tests("budget-b");
                (std::path::PathBuf::from(path), Instant::now(), None)
            });
            // macOS puts the app's menus in the system menu bar. Tests and other platforms
            // keep the in-window menus.
            #[cfg(target_os = "macos")]
            app.install_native_menu(&cc.egui_ctx);
            Ok(Box::new(Shell {
                app,
                started: Some(started),
                budget,
            }))
        }),
    )
}
