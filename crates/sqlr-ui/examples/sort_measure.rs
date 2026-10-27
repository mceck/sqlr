//! Deterministic CPU benchmark of the grid sorting algorithm.
//! Run with `cargo run --release -p sqlr-ui --example sort_measure`.
use sqlr_core::{Cell, ChunkBuilder, ColumnKind, ColumnMeta, DisplayHint, RowBuffer};
use std::{hint::black_box, sync::Arc, time::Instant};
// Compile the private sorter directly without widening the widget API for a benchmark.
#[expect(
    dead_code,
    reason = "the benchmark only calls part of the sorter it compiles in"
)]
#[path = "../src/grid/sort.rs"]
mod sort;
fn main() {
    for n in [100_000usize, 1_000_000] {
        for decimal in [false, true] {
            let columns: Arc<[ColumnMeta]> = vec![ColumnMeta::new(
                "v",
                if decimal { "numeric" } else { "int8" },
                if decimal {
                    ColumnKind::Text
                } else {
                    ColumnKind::Int
                },
                if decimal {
                    DisplayHint::Decimal
                } else {
                    DisplayHint::Number
                },
            )]
            .into();
            let mut rows = RowBuffer::new(columns.clone());
            let mut seed = 42u64;
            for base in (0..n).step_by(1000) {
                let mut chunk = ChunkBuilder::new(columns.clone());
                for _ in base..(base + 1000).min(n) {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let value = ((seed >> 32) % 100_000) as i64;
                    if decimal {
                        let s = format!("9007199254740992.{value:05}");
                        chunk.push_row(&[Cell::Text(&s)]).unwrap();
                    } else {
                        chunk.push_row(&[Cell::Int(value)]).unwrap();
                    }
                }
                rows.push_chunk(chunk.finish());
            }
            let mut times = vec![];
            for _ in 0..3 {
                let start = Instant::now();
                let view = sort::compute(black_box(&rows), &[(0, false)], true, &[]);
                times.push(start.elapsed().as_secs_f64() * 1000.0);
                black_box(view);
            }
            times.sort_by(f64::total_cmp);
            println!(
                "rows={n} kind={} sort_median_ms={:.2} buffer_MB={:.2}",
                if decimal { "precise_decimal" } else { "int" },
                times[1],
                rows.byte_size() as f64 / 1e6
            );
        }
    }
}
