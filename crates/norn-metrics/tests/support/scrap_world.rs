//! DATA-FORECAST: the pinned numeric recipe, with store-authenticated knowledge.
#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic, missing_docs, reason = "synthetic source fixture")]
use std::collections::BTreeMap;
use norn_cell::{CellStore, CommitBatch, FactVersionEvidence, FactWriteRow, Model};
use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
use norn_ir::{ArtifactEnvelope, RuntimeObject, RuntimeValue as Value};
use norn_metrics::temporal::{TemporalPlan, TemporalRow, TemporalValue};

pub const CSV: &str = include_str!("../../../../fixtures/demo/scrap-yield-k1.csv");
pub const TABLE_SHA: &str = "22cee20298906d6f1ecb8bd3eabc3d405409d7c3091c275cbfe0a8932694c28d";
pub const BULK_AT: &str = "2026-10-26T00:00:00Z";
pub const SPLIT: &str = "2026-09-28T00:00:00Z";
pub const SCOPE: &str = "org_demo/dev";
pub fn millis(time: &str) -> u64 {
    u64::try_from(chrono::DateTime::parse_from_rfc3339(time).expect("UTC").timestamp_millis()).expect("post-epoch")
}
pub fn micros(time: &str) -> i64 { i64::try_from(millis(time)).expect("bounded clock").checked_mul(1000).expect("microseconds") }
pub fn sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub fn artifacts() -> Vec<ArtifactEnvelope> {
    let root = std::path::PathBuf::from(std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR").expect("fixtures"));
    serde_json::from_slice(&std::fs::read(root.join("scrap-publication.json")).expect("compiled typed declaration")).expect("artifacts")
}
pub fn model(artifacts: &[ArtifactEnvelope]) -> Model {
    let mut objects = Vec::new(); let mut interfaces = Vec::new();
    for artifact in artifacts {
        let payload = serde_json::to_value(&artifact.payload).expect("payload");
        if let Some(value) = payload.get("objectType") { objects.push(serde_json::from_value(value.clone()).expect("type")); }
        if let Some(value) = payload.get("interface") { interfaces.push(serde_json::from_value(value.clone()).expect("interface")); }
    }
    objects.push(serde_json::from_value(serde_json::json!({"name":"norn.FunctionCommand","properties":[{"id":"state","type":{"kind":"json-ext"}}]})).expect("generic command oracle"));
    let fields = ["actionId","actionArtifactId","actionVersion","effectKind","object","input","effectiveInput","derivedValues","preconditionOutcomes","actor","turn","transition","from","to","rowOperations"];
    objects.push(serde_json::from_value(serde_json::json!({"name":"norn.ActionCall","properties":fields.map(|id| serde_json::json!({"id":id,"type":{"kind":"json-ext"},"constraints":{"nullable":true}}))})).expect("generic action receipt oracle"));
    Model::from_definitions_with_interfaces(&objects, &[], &interfaces).expect("declared model")
}
pub struct SourceWorld {
    pub store: CellStore,
    pub rows: Vec<TemporalRow>,
    pub evidence: Vec<FactVersionEvidence>,
    pub head: u64,
    pub plan: TemporalPlan,
}
pub fn world(bulk: bool, proposal: Option<RuntimeObject>) -> SourceWorld {
    assert_eq!(sha(CSV.as_bytes()), TABLE_SHA, "integrator recipe must not drift");
    let artifacts = artifacts();
    let catalog = norn_checker::EffectiveCatalog::from_artifacts(&artifacts);
    let plan = TemporalPlan::compile(&catalog, "demo.ScrapForecast", &["input".into()], "recovered", 0).expect("ontology units");
    let mut store = CellStore::open_in_memory(model(&artifacts)).expect("cell");
    if let Some(values) = proposal {
        store.commit(&CommitBatch { world:"main".into(), scope:SCOPE.into(), writes:vec![FactWriteRow::Put { object_type:"core.analytics.Publication".into(), key:"forecast".into(), values }], ..CommitBatch::default() }).expect("preregister policy");
    }
    let mut events = Vec::new();
    for line in CSV.lines().skip(1) {
        let row: Vec<_> = line.split(',').collect();
        for (role, mass, recorded) in [("input",4,7),("output",5,9),("loss",6,9)] {
            let effective = if role == "input" {row[7]} else {row[8]};
            events.push((millis(if bulk { BULK_AT } else {row[recorded]}), format!("{}/{role}",row[2]), BTreeMap::from([
                ("mass_mg".into(),Value::from(row[mass].parse::<i64>().expect("mass"))),
                ("effective_at".into(),Value::from(effective)), ("source_document".into(),Value::from(row[2])),
                ("source_sha256".into(),Value::from(sha(line.as_bytes()))), ("table_sha256".into(),Value::from(TABLE_SHA)),
            ])));
        }
    }
    events.sort_by(|a,b| (&a.0,&a.1).cmp(&(&b.0,&b.1)));
    for (logical_time,key,values) in events {
        store.commit(&CommitBatch { world:"main".into(), scope:SCOPE.into(), logical_time, writes:vec![FactWriteRow::Put { object_type:"demo.Measurement".into(),key,values }],..CommitBatch::default() }).expect("record source incarnation");
    }
    let limits = ReactiveLimits { max_candidate_bytes_per_commit: 2_000_000, ..ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1()) };
    let mut budget = DecodeBudget::new(&limits); let mut rows = Vec::new(); let mut evidence = Vec::new();
    for line in CSV.lines().skip(1) {
        let row: Vec<_> = line.split(',').collect(); if row[3] != "gold" { continue; }
        let mut values = Vec::new();
        for role in ["input","output"] {
            let proof = store.read().fact_version_evidence_with_budget("main","demo.Measurement", &format!("{}/{role}",row[2]),1,&mut budget).expect("authenticated historical read").expect("evidence");
            assert_eq!(proof.snapshot().scope,SCOPE); assert_eq!(proof.snapshot().world,"main");
            let snapshot = proof.snapshot().values.as_ref().expect("live source");
            assert_eq!(snapshot["source_sha256"],Value::from(sha(line.as_bytes())));
            let mass = snapshot["mass_mg"].as_i64().expect("integer milligrams");
            values.push(TemporalValue {
                value:Value::from_json_text(format!("{}.{:03}",mass/1000,mass%1000)).expect("exact grams"),
                valid_at:micros(snapshot["effective_at"].as_str().expect("effective time")),
                recorded_at:i64::try_from(proof.receipt().body.logical_time).expect("recorded clock").checked_mul(1000).expect("microseconds"),
                source:format!("{}:{}:{}",proof.snapshot().key,proof.snapshot_sha256(),proof.receipt().hash),
            });
            evidence.push(proof);
        }
        rows.push(TemporalRow { key:row[2].into(),at:micros(row[7]),features:vec![values[0].clone()],outcome:values[1].clone() });
    }
    let head = store.verified_head().expect("frontier").commits;
    SourceWorld { store,rows,evidence,head,plan }
}
