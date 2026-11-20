//! INGEST-RATE-1: the rate a normal record import lands at. `#[ignore]`d;
//! run in release on a quiet machine:
//! `cargo test --release -p norn-cell-do --test ingest_rate -- --ignored --nocapture`.
//! `NORN_INGEST_ROWS` sets the order-line count (default 2,000) and
//! `NORN_INGEST_ROUNDS` the rounds (default 3, the median is reported);
//! `NORN_INGEST_STORE` is `file`, `memory` or both (default, alternating).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stdout,
    clippy::cast_precision_loss,
    reason = "a bench"
)]
use norn_api::host::DevHost;
use norn_cell::engine::NativeEngine;
use norn_cell_do::seed;
use serde_json::{Value, json};
use std::time::Instant;

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// The ERPNext synthetic export with `lines` distinct sales-order lines.
fn export(lines: usize) -> Value {
    let export: Value = serde_json::from_str(include_str!(
        "../../../registry/packs/erpnext/synthetic.json"
    ))
    .unwrap();
    let rows = export["snapshot"].as_array().unwrap();
    let mut snapshot: Vec<Value> = rows
        .iter()
        .filter(|row| row["dataset"] != "Sales Order Item")
        .cloned()
        .collect();
    let order = rows
        .iter()
        .find(|row| row["dataset"] == "Sales Order Item")
        .unwrap();
    for n in 1..=lines {
        let mut line = order.clone();
        line["record"]["name"] = json!(format!("SYN-ERPNEXT-order-{n:06}"));
        line["record"]["custom_norn_ref"] = json!(format!("demo/order/{n:06}/line/1"));
        snapshot.push(line);
    }
    json!({ "profile": export["profile"], "snapshot": snapshot })
}

fn round(lines: usize, file: bool) -> (f64, usize, usize, usize) {
    let scope = seed::demo_scope();
    let dir = std::env::temp_dir().join(format!(
        "norn-ingest-rate-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let engine = if file {
        std::fs::create_dir_all(&dir).unwrap();
        NativeEngine::open(&dir.join("cell.sqlite"), norn_cell::StoreOptions::default()).unwrap()
    } else {
        NativeEngine::open_in_memory().unwrap()
    };
    let store = seed::provision(engine, &scope).unwrap();
    let host = DevHost::from_store(store, scope, |command| command.payload.clone()).unwrap();
    let recorded = norn_cell_do::recorded::Recorded::parse(&export(lines)).unwrap();
    let rows = recorded.observations().unwrap();
    let started = Instant::now();
    let report = host.ingest_within(&rows, None).unwrap();
    let elapsed = started.elapsed().as_secs_f64();
    let landed = report
        .outcomes
        .iter()
        .filter(|row| row.outcome == norn_cell::MappingOutcome::Landed)
        .count();
    drop(host);
    let _ = std::fs::remove_dir_all(&dir);
    (rows.len() as f64 / elapsed, landed, report.pages, report.commits)
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

#[test]
#[ignore = "a release bench"]
fn ingest_rate() {
    let lines = env("NORN_INGEST_ROWS", 2_000);
    let rounds = env("NORN_INGEST_ROUNDS", 3);
    let (mut on_file, mut in_memory) = (Vec::new(), Vec::new());
    for at in 0..rounds {
        // Alternate the two stores so drift hits both alike.
        let stores = match std::env::var("NORN_INGEST_STORE").as_deref() {
            Ok("file") => vec![true],
            Ok("memory") => vec![false],
            _ => vec![at % 2 == 0, at % 2 != 0],
        };
        for file in stores {
            let (rate, landed, pages, commits) = round(lines, file);
            println!(
                "round {at} {}: {rate:.1} rows/s, landed {landed}, pages {pages}, commits {commits}",
                if file { "file" } else { "memory" }
            );
            if file {
                on_file.push(rate);
            } else {
                in_memory.push(rate);
            }
        }
    }
    for (name, rates) in [("file", on_file), ("memory", in_memory)] {
        if !rates.is_empty() {
            println!("INGEST {lines} lines, {name}: median {:.1} rows/s", median(rates));
        }
    }
}
