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
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App as _;
    use serde_json::{Value, json};
    use sqlr_app::AppDeps;
    use sqlr_engine::{DriverRegistry, Engine};
    use sqlr_store::{ConnectionRepo, MemoryMcpTokenStore, MemorySecretStore, Settings};
    use sqlr_testkit::{FakeDriver, FakeScript};
    use std::time::Duration;

    async fn rpc(
        client: &reqwest::Client,
        url: &str,
        session: Option<&str>,
        body: Value,
    ) -> (Value, Option<String>) {
        let mut request = client.post(url).json(&body);
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        let response = request.send().await.unwrap().error_for_status().unwrap();
        let session = response
            .headers()
            .get("mcp-session-id")
            .map(|value| value.to_str().unwrap().to_string());
        let text = response.text().await.unwrap();
        let value = text
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .find_map(|data| serde_json::from_str(data.trim()).ok())
            .unwrap_or(Value::Null);
        (value, session)
    }

    #[test]
    fn hidden_window_serves_mcp_requests_and_connection_write_acknowledgements() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut drivers = DriverRegistry::default();
        drivers.register(FakeDriver::new(FakeScript::default()));
        let secrets = Arc::new(MemorySecretStore::default());
        let engine = Engine::new(drivers, Arc::new(|| {}));
        engine.set_secret_store(secrets.clone());
        let ctx = egui::Context::default();
        let app = SqlrApp::new(
            AppDeps {
                engine,
                repo: ConnectionRepo::open(dir.path(), Box::new(secrets)).unwrap(),
                settings: Settings {
                    agents_enabled: true,
                    agents_port: port,
                    ..Settings::default()
                },
                config_dir: dir.path().into(),
                data_dir: Some(dir.path().into()),
                startup_notice: None,
                storage_warning: None,
                mcp_token_store: Some(Arc::new(MemoryMcpTokenStore::with_token("hidden-test"))),
            },
            &ctx,
        );
        let mut shell = Shell {
            app,
            started: None,
            budget: None,
        };
        let mut frame = eframe::Frame::_new_kittest();
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .occluded = Some(true);
        let tick = |shell: &mut Shell, frame: &mut eframe::Frame| {
            let _ = ctx.run_logic(&input, |ctx| shell.logic(ctx, frame));
            std::thread::sleep(Duration::from_millis(5));
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shell.app.mcp_running_for_tests() {
            tick(&mut shell, &mut frame);
            assert!(
                Instant::now() < deadline,
                "MCP did not start without a UI pass"
            );
        }

        let client = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let mut headers = reqwest::header::HeaderMap::new();
                    headers.insert("Authorization", "Bearer hidden-test".parse().unwrap());
                    headers.insert("Accept", "application/json, text/event-stream".parse().unwrap());
                    let client = reqwest::Client::builder()
                        .default_headers(headers)
                        .timeout(Duration::from_secs(5))
                        .build()
                        .unwrap();
                    let url = format!("http://127.0.0.1:{port}/mcp");
                    let (init, session) = rpc(&client, &url, None, json!({
                        "jsonrpc": "2.0", "id": 1, "method": "initialize",
                        "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                            "clientInfo": {"name": "hidden-test", "version": "1"}}
                    })).await;
                    assert_eq!(init["result"]["serverInfo"]["name"], "sqlr");
                    let session = session.unwrap();
                    rpc(&client, &url, Some(&session), json!({
                        "jsonrpc": "2.0", "method": "notifications/initialized"
                    })).await;
                    let (tabs, _) = rpc(&client, &url, Some(&session), json!({
                        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                        "params": {"name": "list_query_tabs", "arguments": {}}
                    })).await;
                    assert_eq!(tabs["result"]["structuredContent"]["tabs"][0]["title"], "Query 1");
                    let (write, _) = rpc(&client, &url, Some(&session), json!({
                        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                        "params": {"name": "config_write", "arguments": {
                            "target": "connection", "payload": {"name": "Hidden write", "driver": "fake"}
                        }}
                    })).await;
                    assert_eq!(write["result"]["isError"], false, "{write}");
                    client.delete(&url).header("mcp-session-id", session)
                        .send().await.unwrap().error_for_status().unwrap();
                });
        });
        while !client.is_finished() {
            tick(&mut shell, &mut frame);
        }
        client.join().unwrap();
        let saved = std::fs::read_to_string(dir.path().join("connections.toml")).unwrap();
        assert!(saved.contains("Hidden write"));
        shell.app.set_mcp_enabled_for_tests(false);
        tick(&mut shell, &mut frame);
        assert!(shell.app.mcp_stopped_for_tests());
        assert!(!dir.path().join("mcp.json").exists());
    }
}
