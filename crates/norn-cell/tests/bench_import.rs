//! BENCH-ITEM-1: opt-in observed import of the link-free canonical Item slice.
#![cfg(feature = "native")]
#![allow(clippy::expect_used, clippy::panic, missing_docs, reason = "benchmark driver")]
use norn_cell::{CellStore, InstalledDefinitionInput, Model, SqliteCell, StoreOptions};
use norn_checker::{ArtifactRef, EntityArtifactSetContext};
use norn_ir::{ArtifactEnvelope, ResourceLimitProfile, RuntimeValue as Value};
use norn_kernel::{LogicalTime, ObservationId, Observed, Provenance};
use serde_json::json;
use std::{collections::{BTreeMap, BTreeSet}, io::BufRead, time::Instant};

fn artifacts() -> Vec<ArtifactEnvelope> {
    let mut values: Vec<serde_json::Value> = serde_json::from_str(include_str!("../../../fixtures/bench/olist-item.json")).expect("mapping fixture");
    let source: ArtifactEnvelope = serde_json::from_value(values.get(2).expect("source").clone()).expect("source envelope");
    values.get_mut(3).expect("mapping")["payload"]["source"]["sha256"] = json!(ArtifactRef::of(&source).expect("source pin").sha256);
    values.into_iter().map(|v| serde_json::from_value(v).expect("artifact")).collect()
}

#[test]
#[ignore = "requires a test-time Olist product_id JSONL extract outside the checkout"]
fn real_olist_items_through_retained_mapping_and_cell() {
    let path = std::env::var("NORN_BENCH_ROWS").expect("test-time rows path");
    let limit: usize = std::env::var("NORN_BENCH_LIMIT").unwrap_or_else(|_| "1000".into()).parse().expect("positive limit");
    assert!(limit > 0);
    let artifacts = artifacts();
    let sources: Vec<Value> = artifacts.iter().map(|a| Value::from_json(serde_json::to_value(a).expect("wire")).expect("source")).collect();
    let registry = BTreeSet::new();
    let checked = norn_checker::check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&sources, &[]).expect("receipt sources"),
        &EntityArtifactSetContext { registry: &registry, base: &[], tenant: None },
        &ResourceLimitProfile::norn_limits_v1(),
    ).expect("receipt");
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    let receipt = serde_json::to_string(&checked.receipt).expect("receipt wire");
    let values = serde_json::to_value(&artifacts).expect("definitions");
    let object = serde_json::from_value(values.get(0).expect("item")["payload"]["objectType"].clone()).expect("item model");
    let interface = serde_json::from_value(values.get(1).expect("interface")["payload"]["interface"].clone()).expect("interface model");
    let model = Model::from_definitions_with_interfaces(&[object], &[], &[interface]).expect("model");
    let directory = tempfile::tempdir().expect("temporary cell");
    let database = directory.path().join("items.sqlite");
    let mut store = CellStore::open(&database, model.clone(), StoreOptions::default()).expect("SQLite");
    store.install_definitions(model.clone(), InstalledDefinitionInput {
        canonical_set: serde_json::to_string(&artifacts).expect("canonical set"), registry: Vec::new(),
        receipt_sha256: norn_checker::admission_receipt_sha256(&receipt).expect("receipt hash"),
        admission_receipt: Some(receipt), readmission: None, diff: json!({"benchmark":"link-free Item only"}), installed_at: 1, layout: None,
    }, Some(0), "org_demo/dev").expect("install checked mapping");
    let input = norn_cell::mapping_bundle::MappingInput { mapping:"bench.olist.items".into(), dataset:"Rows".into(), connector_instance:"public-v2".into(), partition:"products".into(), recorded_at:norn_ir::provenance::UtcTimestamp("2026-11-04T00:00:00Z".into()) };
    let mut cell = SqliteCell::<norn_kernel::ObservationEvent, BTreeMap<String, Value>, BTreeMap<String, Value>>::new(CellStore::open(&database, model, StoreOptions::default()).expect("processing connection"), "org_demo/dev", |_| BTreeMap::new()).expect("cell");
    let file = std::io::BufReader::new(std::fs::File::open(path).expect("rows"));
    let mut durations = Vec::new();
    let mut ids = BTreeSet::new();
    let started = Instant::now();
    for (index, line) in file.lines().take(limit).enumerate() {
        let text = line.expect("source line");
        let payload = Value::from_json_text(&text).expect("source JSON");
        let id = payload.get("product_id").and_then(Value::as_str).expect("source key").to_owned();
        assert!(ids.insert(id.clone()), "source IDs must be unique");
        let hash = norn_ir::canonical::HashProfile::Norn1.hash_json(&text).expect("payload digest");
        let observed = Observed {id:ObservationId(format!("row-{index}")), provenance:Provenance {source:"Olist".into(), source_id:id.clone(), mapping_version:"pipeline/1".into(), observed_at:LogicalTime::from_millis(1_793_750_400_000)}, input:norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>"Olist", "source_id"=>id.clone(), "event_id"=>format!("row-{index}"), "source_version"=>"2", "pipeline_version"=>"pipeline/1", "kind"=>"upsert", "extracted_at"=>1_793_750_400_000u64, "payload"=>payload, "payload_digest"=>hash}};
        let at = Instant::now();
        let (bundle, _) = store.inject_mapping_bundle(&observed, &input).expect("capture mapped evidence");
        assert!(cell.process_mapping_bundle(&bundle).expect("link-free import"));
        durations.push(at.elapsed().as_micros());
        let actual = cell.store().read().object("main", "canon.Item", &id).expect("read").expect("mapped item");
        for property in ["id", "code", "label"] { assert_eq!(actual.values.get(property).and_then(Value::as_str), Some(id.as_str())); }
    }
    assert_eq!(ids.len(), limit, "extract shorter than the declared sample");
    let elapsed = started.elapsed().as_secs_f64();
    durations.sort_unstable();
    let percentile = |p: usize| durations.get((durations.len() * p).div_ceil(100).saturating_sub(1)).expect("sample");
    println!("BENCH-ITEM-1 {}", json!({"rows":limit,"seconds":elapsed,"rows_per_second":f64::from(u32::try_from(limit).expect("bounded row count"))/elapsed,"end_to_end_us":{"p50":percentile(50),"p95":percentile(95),"p99":percentile(99)},"law_check_latency":"not isolated; no domain laws installed","correctness":"all imported id/code/label equal independent product_id reference; label is deliberately an opaque ID"}));
}
