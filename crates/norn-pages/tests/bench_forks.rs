//! BENCH-FORK-1: fork latency, storage overhead and read latency at 20, 100
//! and 1,000 forks, copy-on-write against the copying reference engine.
//!
//! Run in release: `cargo test --release -p norn-pages --test bench_forks --
//! --ignored --nocapture`. Everything is in memory: this measures the CPU
//! path of the in-memory tier, not durable or cold reads. Each value is also
//! printed as a `METRIC` JSON line for the metrics ledger's nightly probe.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stdout,
    clippy::too_many_lines,
    missing_docs,
    reason = "benchmark driver"
)]

use std::time::{Duration, Instant};

use norn_cell::engine::{NativeEngine, SqlEngine};
use norn_cell::fork::ForkEngine;
use norn_cell::{CellStore, Model, SqliteCell};
use norn_kernel::cell_host::{JsonObject, JsonValue, ObjectTypeDef};
use norn_kernel::{CellCommit, CellHost, Command, FactKey, FactMutation, FactVersion, FactWrite};
use norn_pages::{CowEngine, PAGE_SIZE, PageStore, TenantKey};
use serde_json::json;

type Cell<E> = SqliteCell<u8, u8, u8, E>;

const SEED_FACTS: u32 = 5_000;
const FORK_COMMITS: u32 = 5;
const READS: usize = 2_000;

fn model() -> Model {
    let types: Vec<ObjectTypeDef> = vec![
        serde_json::from_value(json!({
            "name": "bench.Thing",
            "properties": [
                { "id": "v", "type": { "kind": "integer" } },
                { "id": "label", "type": { "kind": "text" } }
            ]
        }))
        .expect("decodes"),
    ];
    Model::from_definitions(&types, &[]).expect("projects")
}

fn serve<E: SqlEngine>(store: CellStore<E>) -> Cell<E> {
    SqliteCell::new(store, "bench", |command: &Command<u8>| command.payload).expect("serves")
}

fn put<E: SqlEngine>(cell: &mut Cell<E>, n: u32, value: i64) {
    let key = FactKey::new("bench.Thing", format!("t{n}"));
    let expected = cell.load(&key).expect("loads").version;
    let values = JsonObject::from([
        ("v".to_owned(), JsonValue::from(value)),
        (
            "label".to_owned(),
            JsonValue::from(format!("thing number {n}")),
        ),
    ]);
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key,
            expected,
            mutation: FactMutation::Put { values },
        }],
        ..CellCommit::empty()
    })
    .expect("commits");
}

fn seed<E: SqlEngine>(cell: &mut Cell<E>) {
    for n in 0..SEED_FACTS {
        let key = FactKey::new("bench.Thing", format!("t{n}"));
        let values = JsonObject::from([
            ("v".to_owned(), JsonValue::from(i64::from(n))),
            (
                "label".to_owned(),
                JsonValue::from(format!("thing number {n}")),
            ),
        ]);
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key,
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put { values },
            }],
            ..CellCommit::empty()
        })
        .expect("seeds");
    }
}

/// One machine-readable line per value, for the metrics ledger's probe:
/// `METRIC {"metric", "value", "unit", "statistic"}`.
fn metric(name: &str, value: f64, unit: &str, statistic: &str) {
    println!(
        "METRIC {}",
        json!({ "metric": name, "value": value, "unit": unit, "statistic": statistic })
    );
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn us(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000.0
}

/// The p50 and p99 of one measured pair, under `prefix`.
fn percentile_metrics(prefix: &str, (p50, p99): (Duration, Duration), unit: &str) {
    let convert = if unit == "us" { us } else { ms };
    metric(&format!("{prefix}.p50"), convert(p50), unit, "p50");
    metric(&format!("{prefix}.p99"), convert(p99), unit, "p99");
}

fn percentiles(mut samples: Vec<Duration>) -> (Duration, Duration) {
    samples.sort();
    let at = |q: f64| samples[((samples.len() as f64 - 1.0) * q).round() as usize];
    (at(0.5), at(0.99))
}

fn reads<E: SqlEngine>(cell: &Cell<E>) -> Vec<Duration> {
    (0..READS)
        .map(|i| {
            let key = FactKey::new(
                "bench.Thing",
                format!("t{}", (i * 7919) as u32 % SEED_FACTS),
            );
            let start = Instant::now();
            let state = cell.load(&key).expect("loads");
            let elapsed = start.elapsed();
            assert!(state.values.is_some());
            elapsed
        })
        .collect()
}

struct Run {
    fork: (Duration, Duration),
    main_read: (Duration, Duration),
    fork_read: (Duration, Duration),
}

fn run<E: SqlEngine + ForkEngine>(main: &mut Cell<E>, forks: usize) -> (Run, Vec<Cell<E>>) {
    let base = main.store().verified_head().expect("head");
    let mut fork_samples = Vec::with_capacity(forks);
    let mut cells = Vec::with_capacity(forks);
    for slot in 0..forks {
        let start = Instant::now();
        let store = main
            .store()
            .fork(&format!("f{slot}"), &base)
            .expect("forks");
        fork_samples.push(start.elapsed());
        cells.push(serve(store));
    }
    for (slot, cell) in cells.iter_mut().enumerate() {
        for step in 0..FORK_COMMITS {
            put(cell, step, (slot as i64) * 1_000 + i64::from(step));
        }
    }
    let mut fork_reads = Vec::new();
    for cell in cells.iter().take(20) {
        fork_reads.extend(reads(cell));
    }
    let run = Run {
        fork: percentiles(fork_samples),
        main_read: percentiles(reads(main)),
        fork_read: percentiles(fork_reads),
    };
    (run, cells)
}

#[test]
#[ignore = "benchmark: run in release with --ignored --nocapture"]
fn fork_benchmark() {
    let store = PageStore::new(TenantKey::derive(b"bench-tenant"));
    let mut cow = serve(
        CellStore::from_engine(CowEngine::create(&store).expect("creates"), model())
            .expect("opens"),
    );
    let mut copy = serve(
        CellStore::from_engine(NativeEngine::open_in_memory().expect("opens"), model())
            .expect("opens"),
    );
    seed(&mut cow);
    seed(&mut copy);
    store.collect_garbage().expect("collects");
    let base_pages = store.stats().expect("stats").pages;
    println!(
        "base cell: {SEED_FACTS} facts, {base_pages} unique pages ({:.1} MiB)",
        (base_pages * PAGE_SIZE) as f64 / 1_048_576.0
    );
    println!(
        "forks | engine | fork p50/p99 | main read p50/p99 | fork read p50/p99 | pages per fork | growth vs full copies"
    );
    for forks in [20, 100, 1_000] {
        let (cow_run, cow_cells) = run(&mut cow, forks);
        let gc_start = Instant::now();
        store.collect_garbage().expect("collects");
        let gc = gc_start.elapsed();
        let grown = store.stats().expect("stats").pages - base_pages;
        println!(
            "{forks} | cow | {:?}/{:?} | {:?}/{:?} | {:?}/{:?} | {:.1} | {:.3}%",
            cow_run.fork.0,
            cow_run.fork.1,
            cow_run.main_read.0,
            cow_run.main_read.1,
            cow_run.fork_read.0,
            cow_run.fork_read.1,
            grown as f64 / forks as f64,
            grown as f64 * 100.0 / (base_pages * forks) as f64,
        );
        percentile_metrics(&format!("fork.cow.create.f{forks}"), cow_run.fork, "ms");
        percentile_metrics(
            &format!("fork.cow.read.fork.f{forks}"),
            cow_run.fork_read,
            "us",
        );
        percentile_metrics(
            &format!("fork.cow.read.main.f{forks}"),
            cow_run.main_read,
            "us",
        );
        metric(
            &format!("fork.cow.pages_per_fork.f{forks}"),
            grown as f64 / forks as f64,
            "count",
            "mean",
        );
        metric(
            &format!("fork.cow.storage_vs_full.f{forks}"),
            grown as f64 * 100.0 / (base_pages * forks) as f64,
            "percent",
            "single",
        );
        drop(cow_cells);
        let discard_start = Instant::now();
        let report = store.collect_garbage().expect("collects");
        let discard = discard_start.elapsed();
        println!(
            "{forks} | cow | collection with forks live {gc:?}; discard {forks} forks and collect {discard:?}, freed {} pages",
            report.pages_freed
        );
        metric(
            &format!("fork.cow.gc.collect_live.f{forks}"),
            ms(gc),
            "ms",
            "single",
        );
        metric(
            &format!("fork.cow.gc.discard_collect.f{forks}"),
            ms(discard),
            "ms",
            "single",
        );
        metric(
            &format!("fork.cow.gc.pages_freed.f{forks}"),
            report.pages_freed as f64,
            "count",
            "single",
        );
        if forks <= 100 {
            let (copy_run, copy_cells) = run(&mut copy, forks);
            println!(
                "{forks} | copy | {:?}/{:?} | {:?}/{:?} | {:?}/{:?} | full copy | 100%",
                copy_run.fork.0,
                copy_run.fork.1,
                copy_run.main_read.0,
                copy_run.main_read.1,
                copy_run.fork_read.0,
                copy_run.fork_read.1,
            );
            percentile_metrics(
                &format!("fork.full_copy.create.f{forks}"),
                copy_run.fork,
                "ms",
            );
            drop(copy_cells);
        }
    }
}
