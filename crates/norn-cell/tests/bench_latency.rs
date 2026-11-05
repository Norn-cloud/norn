//! BENCH-LATENCY-1: durable governed-commit cost as history grows.
//!
//! The `law_check_control` samples are a control, not a finding: they evaluate one
//! object's values already in memory and read no history, so they are flat in
//! history size by construction and say nothing about laws that read history.
#![cfg(feature = "native")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::similar_names,
    clippy::too_many_lines,
    missing_docs,
    reason = "benchmark driver"
)]
#[path = "stress_purchase.rs"]
mod stress;
use norn_cell::{CellStore, CommitBatch, FactWriteRow, Model, StoreOptions};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, Decimal, EntityRootV2, ObjectTypeDef, RuntimeValue as V,
};
use norn_kernel::{ACTION_CALL_TYPE, ActionOutcome, FactKey, TypeCatalog, governing_laws};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

fn audit_type() -> ObjectTypeDef {
    let properties: Vec<Value> = [
        "actionId",
        "actionArtifactId",
        "actionVersion",
        "effectKind",
        "object",
        "input",
        "effectiveInput",
        "derivedValues",
        "preconditionOutcomes",
        "actor",
        "turn",
        "transition",
        "from",
        "to",
        "rowOperations",
    ]
    .into_iter()
    .map(|id| json!({"id":id,"type":{"kind":"json-ext"},"constraints":{"nullable":true}}))
    .collect();
    serde_json::from_value(json!({"name":ACTION_CALL_TYPE,"properties":properties})).expect("audit")
}

fn percentile(sorted: &[u128], p: usize) -> u128 {
    sorted[(sorted.len() * p).div_ceil(100).saturating_sub(1)]
}

fn summary(mut samples: Vec<u128>) -> Value {
    samples.sort_unstable();
    json!({"n":samples.len(),"p50_ns":percentile(&samples,50),"p95_ns":percentile(&samples,95),"p99_ns":percentile(&samples,99)})
}

#[test]
#[ignore = "long-running history growth measurement"]
fn governed_commit_and_law_check_latency_by_history_size() {
    let maximum: u64 = std::env::var("NORN_BENCH_MAX")
        .unwrap_or_else(|_| "1000".into())
        .parse()
        .expect("record count");
    let window = 300_usize;
    let laws = std::env::var("NORN_BENCH_LAWS").map_or(true, |v| v != "0");
    let mut artifacts = stress::artifacts();
    if !laws {
        artifacts.retain(|a| !a.artifact_id.as_str().ends_with(".law"));
    }
    let artifacts: Vec<ArtifactEnvelope> = artifacts;
    let mut definitions: Vec<ObjectTypeDef> = artifacts
        .iter()
        .filter_map(|a| {
            if let ArtifactPayload::Entity(payload) = &a.payload {
                Some(
                    payload
                        .to_json_value()
                        .decode::<EntityRootV2>()
                        .expect("entity")
                        .object_type,
                )
            } else {
                None
            }
        })
        .collect();
    definitions.push(audit_type());
    let interface: ArtifactEnvelope = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.Process.interface.json"
    ))
    .expect("core Process");
    let payload = serde_json::to_value(interface.payload.to_json_value()).expect("payload");
    let interfaces = [serde_json::from_value(payload["interface"].clone()).expect("interface")];
    let model = Model::from_definitions_with_interfaces(&definitions, &[], &interfaces)
        .expect("model")
        .with_governing_laws(&artifacts)
        .expect("laws");
    // Default to the system temp dir, never the crate directory under test.
    let directory = std::env::var_os("NORN_BENCH_DIR")
        .map_or_else(tempfile::tempdir, tempfile::tempdir_in)
        .expect("directory");
    let path = directory.path().join("history.sqlite");
    let laws_only: Vec<ArtifactEnvelope> = artifacts
        .iter()
        .filter(|a| a.artifact_id.as_str().ends_with(".law"))
        .cloned()
        .collect();
    let catalog = TypeCatalog::new(definitions.iter())
        .with_interfaces(&interfaces)
        .with_governing_laws(&laws_only);
    let started = Instant::now();
    let (mut seeded, mut history, mut measured) = (0_u64, 1_000_u64, 0_u64);
    while history <= maximum {
        let mut store =
            CellStore::open(&path, model.clone(), StoreOptions::default()).expect("SQLite");
        while seeded < history {
            let end = history.min(seeded + 2_000);
            let writes = (seeded..end)
                .map(|record| FactWriteRow::Put {
                    object_type: "stress.Purchase".into(),
                    key: format!("h{record}"),
                    values: BTreeMap::from([
                        ("id".into(), V::from(format!("h{record}"))),
                        ("status".into(), V::from("draft")),
                        (
                            "ordered".into(),
                            V::from(Decimal::parse("7").expect("quantity")),
                        ),
                        (
                            "received".into(),
                            V::from(Decimal::parse("0").expect("quantity")),
                        ),
                    ]),
                })
                .collect();
            store
                .commit(&CommitBatch {
                    world: "main".into(),
                    scope: "org_demo/dev".into(),
                    logical_time: 1 + seeded,
                    writes,
                    ..CommitBatch::default()
                })
                .expect("history seed");
            seeded = end;
        }
        let mut cell =
            stress::Cell::new(store, "org_demo/dev", |c| c.payload.clone()).expect("cell");
        let mut commits = Vec::with_capacity(window);
        for _ in 0..window {
            measured += 1;
            let id = format!("p{measured}");
            let at = Instant::now();
            let outcome = stress::run(
                "stress.purchase.create.action",
                json!({"id":id,"ordered":1 + measured % 97}),
                &mut cell,
                &catalog,
                &artifacts,
                &id,
            );
            commits.push(at.elapsed().as_nanos());
            assert!(
                matches!(outcome, Ok(ActionOutcome::Committed { .. })),
                "commit {measured}"
            );
        }
        let key = FactKey::new("stress.Purchase", "p1");
        let values = cell
            .store()
            .read()
            .object("main", "stress.Purchase", "p1")
            .expect("read")
            .expect("object")
            .values;
        let mut law_only = Vec::with_capacity(5_000);
        for _ in 0..5_000 {
            let at = Instant::now();
            let mut fuel = 1_000_000;
            let failures =
                governing_laws::evaluate(&catalog, &key, &values, &mut fuel).expect("law check");
            law_only.push(at.elapsed().as_nanos());
            assert!(failures.is_empty());
        }
        let size = std::fs::metadata(&path).expect("size").len();
        println!(
            "BENCH-LATENCY {}",
            json!({"laws":laws,"history":history,"records":seeded + measured,"commit":summary(commits),
            "law_check_control":summary(law_only),"db_bytes":size,"elapsed_s":started.elapsed().as_secs()})
        );
        drop(cell);
        history = history.saturating_mul(10);
    }
}
