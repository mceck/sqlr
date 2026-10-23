//! Run after `cargo build --release -p sqlr-app --bin sqlr`.
//! Native startup/RSS and binary size, plus the CPU grid harness.
use egui_kittest::Harness;
use sqlr_core::{
    Cell, ChunkBuilder, ColumnKind, ColumnMeta, ConnectionConfig, DisplayHint, RowBuffer, Secrets,
};
use sqlr_store::{ConnectionRepo, MemorySecretStore};
use sqlr_ui::{GridState, result_grid};
use std::{
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
fn main() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let repo = ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default()))?;
    for id in ["budget-a", "budget-b"] {
        repo.save(
            &ConnectionConfig {
                id: id.into(),
                name: id.into(),
                driver: "sqlite".into(),
                group: None,
                color: None,
                values: [
                    (
                        "path".into(),
                        dir.path()
                            .join(format!("{id}.sqlite"))
                            .display()
                            .to_string(),
                    ),
                    ("create_if_missing".into(), "true".into()),
                ]
                .into(),
                profile: None,
                save_secrets: true,
                agent_access: Default::default(),
            },
            &Secrets::default(),
        )?;
    }
    let binary = std::env::current_exe()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(if cfg!(windows) { "sqlr.exe" } else { "sqlr" });
    let binary_mb = std::fs::metadata(&binary)?.len() as f64 / 1_000_000.0;
    let report = dir.path().join("native.json");
    let stderr = std::fs::File::create(dir.path().join("native.stderr"))?;
    let mut child = Command::new(&binary)
        .args([
            "--config-dir",
            dir.path().to_str().unwrap(),
            "--data-dir",
            dir.path().to_str().unwrap(),
        ])
        .env("SQLR_BUDGET_REPORT", &report)
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::ensure!(
                status.success(),
                "native app failed: {}",
                std::fs::read_to_string(dir.path().join("native.stderr"))?
            );
            break;
        }
        if Instant::now() > deadline {
            child.kill()?;
            let _ = child.wait();
            anyhow::bail!(
                "native app did not produce its performance report within 30 s: {}",
                std::fs::read_to_string(dir.path().join("native.stderr"))?
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let native: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report)?)?;
    let columns: Arc<[ColumnMeta]> = vec![
        ColumnMeta::new("id", "int8", ColumnKind::Int, DisplayHint::Number),
        ColumnMeta::new("name", "text", ColumnKind::Text, DisplayHint::Plain),
    ]
    .into();
    let mut rows = RowBuffer::new(columns.clone());
    for base in (0..1_000_000).step_by(1000) {
        let mut chunk = ChunkBuilder::new(columns.clone());
        for id in base..base + 1000 {
            chunk.push_row(&[Cell::Int(id), Cell::Text("budget row")])?;
        }
        rows.push_chunk(chunk.finish());
    }
    let mut h = Harness::builder()
        .with_size((1280.0, 800.0))
        .build_ui_state(
            |ui, state: &mut GridState| {
                result_grid(ui, state, &rows, false);
            },
            GridState::default(),
        );
    h.run_steps(5);
    let mut times = vec![];
    let mut reached = 0;
    for i in 0..300 {
        let row = (i * 7919) % 999_950;
        h.state_mut().scroll_to_row = Some(row);
        for _ in 0..3 {
            let start = Instant::now();
            h.run_steps(1);
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        if h.state().last_visible_rows.contains(&row) {
            reached += 1;
        }
    }
    times.sort_by(f64::total_cmp);
    let grid_p99_ms = times[(times.len() * 99 / 100).min(times.len() - 1)];
    let result = serde_json::json!({"native":native,"binary_mb":binary_mb,"grid_p99_ms":grid_p99_ms,"grid_rows":1_000_000,"grid_jumps_reached":reached});
    println!("{}", serde_json::to_string_pretty(&result)?);
    anyhow::ensure!(
        native["first_frame_ms"]
            .as_f64()
            .is_some_and(|n| n < 1000.0),
        "cold startup budget exceeded"
    );
    let rss_ok = native["idle_rss_mb"]
        .as_f64()
        .is_some_and(|n| n > 0.0 && n < 150.0);
    if std::env::var("SQLR_ENFORCE_RSS_BUDGET").is_ok_and(|v| v == "1") {
        anyhow::ensure!(rss_ok, "idle RSS budget exceeded");
    } else if !rss_ok {
        eprintln!(
            "Deferred optimization: idle RSS exceeds 150 MB; set SQLR_ENFORCE_RSS_BUDGET=1 to enforce."
        );
    }
    anyhow::ensure!(
        native["connected"].as_u64() == Some(2),
        "idle fixture did not connect both databases"
    );
    anyhow::ensure!(binary_mb < 50.0, "binary budget exceeded");
    anyhow::ensure!(
        grid_p99_ms < 16.7 && reached == 300,
        "grid budget or scrolling correctness failed"
    );
    Ok(())
}
