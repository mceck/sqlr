//! Huge inputs stay streamed: peak memory does not follow the size of the file. Its own
//! test binary, so the process's peak RSS is not shared with other tests.
#![cfg(unix)]

#[allow(dead_code)]
mod support;

use std::io::{BufRead, BufWriter, Read, Write};
use std::sync::Mutex;

use sqlr_core::{CsvOptions, Endpoint, TextFormat, TransferOptions, TransferSpec, read_rows};
use sqlr_engine::{DriverRegistry, Engine, JobOutcome};
use support::{TIMEOUT, no_repaint, wait_until};

/// One test at a time, so a test's peak is its own.
static SERIAL: Mutex<()> = Mutex::new(());

/// The process's peak resident set size in bytes.
fn peak_rss() -> u64 {
    let mut usage = unsafe { std::mem::zeroed::<libc::rusage>() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let max = usage.ru_maxrss as u64;
    // macOS reports bytes, Linux kilobytes.
    if cfg!(target_os = "macos") {
        max
    } else {
        max * 1024
    }
}

/// A JSON array of objects of about `bytes` bytes, produced as it is read.
struct JsonArray {
    remaining: usize,
    next: u64,
    started: bool,
    buf: Vec<u8>,
    pos: usize,
}

impl JsonArray {
    fn new(bytes: usize) -> Self {
        Self {
            remaining: bytes,
            next: 0,
            started: false,
            buf: Vec::new(),
            pos: 0,
        }
    }
}

impl Read for JsonArray {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let available = self.fill_buf()?;
        let n = available.len().min(out.len());
        out[..n].copy_from_slice(&available[..n]);
        self.consume(n);
        Ok(n)
    }
}

impl BufRead for JsonArray {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.pos >= self.buf.len() {
            self.buf.clear();
            self.pos = 0;
            if !self.started {
                self.buf.push(b'[');
                self.started = true;
            } else if self.remaining == 0 && self.next != u64::MAX {
                self.buf.push(b']');
                self.next = u64::MAX;
            }
            while self.remaining > 0 && self.buf.len() < 64 * 1024 {
                let sep = if self.next == 0 { "" } else { "," };
                let row = format!(
                    "{sep}{{\"id\":{},\"name\":\"row {}\",\"pad\":\"{}\"}}",
                    self.next,
                    self.next,
                    "x".repeat(100)
                );
                self.remaining = self.remaining.saturating_sub(row.len());
                self.next += 1;
                self.buf.extend_from_slice(row.as_bytes());
            }
        }
        Ok(&self.buf[self.pos..])
    }

    fn consume(&mut self, amt: usize) {
        self.pos += amt;
    }
}

#[test]
fn json_array_streamed() {
    let _guard = SERIAL.lock().unwrap();
    let before = peak_rss();
    let mut rows = 0u64;
    read_rows(&TextFormat::Json, JsonArray::new(128 << 20), |_, _, _| {
        rows += 1;
        true
    })
    .unwrap();
    let growth = peak_rss().saturating_sub(before);
    assert!(rows > 500_000, "{rows} rows read");
    assert!(
        growth < 100 << 20,
        "reading a 128 MB array grew the peak by {} MB",
        growth >> 20
    );
}

#[test]
fn transfer_memory_bounded() {
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("big.csv");
    {
        let mut out = BufWriter::new(std::fs::File::create(&source).unwrap());
        writeln!(out, "id,name,note").unwrap();
        for i in 0..1_500_000u64 {
            writeln!(out, "{i},name {i},{}", "n".repeat(30)).unwrap();
        }
        out.flush().unwrap();
    }
    let engine = Engine::new(DriverRegistry::default(), no_repaint());
    let target = dir.path().join("out.jsonl");
    let before = peak_rss();
    let job = engine.transfer(TransferSpec {
        source: Endpoint::File {
            path: source,
            format: TextFormat::Csv(CsvOptions::default()),
        },
        target: Endpoint::File {
            path: target.clone(),
            format: TextFormat::JsonLines,
        },
        mapping: Vec::new(),
        options: TransferOptions::default(),
    });
    assert!(wait_until(TIMEOUT * 12, || engine
        .job(job)
        .is_some_and(|j| j.outcome.is_some())));
    match engine.job(job).unwrap().outcome.unwrap() {
        JobOutcome::Exported { rows, .. } => assert_eq!(rows, 1_500_000),
        other => panic!("{other:?}"),
    }
    let growth = peak_rss().saturating_sub(before);
    assert!(
        growth < 100 << 20,
        "a 1.5 M-row transfer grew the peak by {} MB",
        growth >> 20
    );
    assert!(target.metadata().unwrap().len() > 100 << 20 >> 2);
}

#[test]
fn generator_memory_bounded() {
    use sqlr_core::{ColumnGen, DataGenSpec, Generator, TableRef};
    let _guard = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(DriverRegistry::default(), no_repaint());
    let target = dir.path().join("gen.jsonl");
    let col = |name: &str, generator| ColumnGen {
        column: name.into(),
        generator,
        null_ratio: 0.0,
        unique: false,
    };
    let before = peak_rss();
    let job = engine.transfer(TransferSpec {
        source: Endpoint::Generator(DataGenSpec {
            table: TableRef {
                database: None,
                schema: None,
                name: "t".into(),
            },
            rows: 1_000_000,
            seed: 1,
            columns: vec![
                col("id", Generator::Sequence { start: 1, step: 1 }),
                col("who", Generator::FullName),
                col("note", Generator::Lorem { words: 8 }),
            ],
        }),
        target: Endpoint::File {
            path: target,
            format: TextFormat::JsonLines,
        },
        mapping: Vec::new(),
        options: TransferOptions::default(),
    });
    assert!(wait_until(TIMEOUT * 24, || engine
        .job(job)
        .is_some_and(|j| j.outcome.is_some())));
    match engine.job(job).unwrap().outcome.unwrap() {
        JobOutcome::Exported { rows, .. } => assert_eq!(rows, 1_000_000),
        other => panic!("{other:?}"),
    }
    let growth = peak_rss().saturating_sub(before);
    assert!(
        growth < 100 << 20,
        "1 M generated rows grew the peak by {} MB",
        growth >> 20
    );
}
