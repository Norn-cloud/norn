#![cfg(all(feature = "native", target_os = "linux"))]
#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Explicit large-ledger regression: a fresh process must reopen history larger
//! than its entire address-space limit. Run on Boat with --ignored --nocapture.

use norn_cell::{CellStore, CommitBatch, MAIN_WORLD, Model, StoreOptions};
use norn_ir::runtime_object;

fn model() -> Model {
    Model::from_definitions(&[], &[]).expect("empty model")
}

const ROWS: u64 = 600;

#[test]
#[ignore = "writes a 1.2 GiB synthetic ledger; run explicitly on Boat before the capped verifier"]
fn seed_large_startup_ledger() {
    let path = std::env::var_os("NORN_STARTUP_MEMORY_CELL").expect("isolated fixture path");
    let mut store = CellStore::open(
        std::path::Path::new(&path),
        model(),
        StoreOptions::default(),
    )
    .expect("seed store");
    let payload = "x".repeat(2 * 1024 * 1024);
    for index in 0..ROWS {
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "memory-test/dev".to_owned(),
                logical_time: index,
                idempotency_key: Some(format!("seed-{index}")),
                events: vec![
                    runtime_object! { "type" => "synthetic.payload", "body" => payload.clone() },
                ],
                ..CommitBatch::default()
            })
            .expect("seed commit");
    }
    println!("seeded 1200 MiB of ledger payload");
}

#[test]
#[ignore = "run the seeded fixture under a 1 GiB address-space cap on Boat"]
fn startup_verification_reopens_large_ledger_under_one_gib() {
    let path = std::env::var_os("NORN_STARTUP_MEMORY_CELL").expect("seeded fixture path");
    let store = CellStore::open(
        std::path::Path::new(&path),
        model(),
        StoreOptions::default(),
    )
    .expect("large history reopens under 1 GiB");
    assert_eq!(
        store.read().verify_ledger().expect("verified head").seq,
        ROWS
    );
    let status = std::fs::read_to_string("/proc/self/status").expect("memory status");
    for line in status.lines().filter(|line| line.starts_with("VmHWM:")) {
        println!("startup verification {line}");
    }
}
