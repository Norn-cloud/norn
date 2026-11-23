//! BENCH-MAP-1: admit one candidate mapping, then run sample rows through the real cell.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stdout,
    clippy::too_many_lines,
    clippy::unwrap_in_result,
    missing_docs,
    reason = "benchmark driver"
)]
use norn_cell::mapping_bundle::{MappingBundle, MappingInput};
use norn_cell::{CellStore, InstalledDefinitionInput, Model, SqliteCell, StoreOptions};
use norn_checker::{ArtifactRef, EntityArtifactSetContext};
use norn_ir::{ArtifactEnvelope, ResourceLimitProfile, RuntimeValue as Value};
use norn_kernel::{LogicalTime, ObservationId, Observed, Provenance};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};

fn registry(path: &str) -> Json {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry");
    serde_json::from_str(&std::fs::read_to_string(root.join(path)).expect("registry artifact"))
        .expect("artifact JSON")
}

fn report(value: &Json) {
    println!("BENCH-MAP {value}");
}

fn envelopes(candidate: &Json) -> Result<Vec<ArtifactEnvelope>, String> {
    let entity_name = candidate["entity"].as_str().expect("entity name");
    let (contract, contract_path) = if entity_name == "canon.Item" {
        (
            "canon.Catalog.interface",
            "packs/canon/artifacts/canon.Catalog.interface.json",
        )
    } else {
        (
            "core.Agent.interface",
            "core/artifacts/core.Agent.interface.json",
        )
    };
    let entity_id = format!("{entity_name}.entity");
    let entity = registry(&format!("packs/canon/artifacts/{entity_id}.json"));
    let interface = registry(contract_path);
    let scope = json!({"environmentId":"dev","orgId":"org_demo"});
    let provenance =
        json!({"source":"system","sourcePath":"crates/norn-cell/tests/bench_mapping.rs"});
    let fields: Vec<Json> = candidate["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .enumerate()
        .map(|(ordinal, field)| {
            json!({"dataset":"Rows","name":field["name"],"nativeType":"text","ordinal":ordinal,
                "required":field["required"],"typeCandidate":{"origin":"declared","type":field["type"]}})
        })
        .collect();
    let source: ArtifactEnvelope = serde_json::from_value(json!({
        "artifactId":"bench.trial.source","dependencies":[],"irVersion":"0.1","kind":"source_schema",
        "lifecycle":"draft","name":"Trial source","namespace":"bench","provenance":provenance,
        "scope":scope,"version":"1.0.0","payload":{"adapter":{"name":"bench.jsonl","version":"1.0.0"},
        "datasets":[{"id":"Rows","kind":"table","nativeName":"Rows"}],"fields":fields,
        "format":"norn.source-schema/1","observedAt":"2026-11-05T00:00:00Z",
        "rawMetadata":{"uri":"https://github.com/Norn-cloud/norn/blob/main/scripts/bench_mapping_trials.py",
            "sha256":"0000000000000000000000000000000000000000000000000000000000000000"},
        "source":{"system":"Trial","instance":"public"}}
    }))
    .expect("source schema");
    let pin = ArtifactRef::of(&source).expect("source pin").sha256;
    let dependency = |id: &str, version: &Json, kind: &str| json!({"artifactId":id,"version":version,"kind":kind,"required":true});
    let mapping: ArtifactEnvelope = serde_json::from_value(json!({
        "artifactId":"bench.trial.mapping","irVersion":"0.1","kind":"mapping","lifecycle":"draft",
        "name":"Trial mapping","namespace":"bench","provenance":provenance,"scope":scope,"version":"1.0.0",
        "dependencies":[dependency("bench.trial.source",&json!("1.0.0"),"source_schema"),
            dependency(&entity_id,&entity["version"],"entity"),
            dependency(contract,&interface["version"],"entity")],
        "payload":{"extensions":{"norn.mapping.generic/1.0.0":{"parity":"unqualified"}},
            "format":"norn.mapping/1","interfaces":[contract],"maps":candidate["maps"],
            "source":{"schemaId":"bench.trial.source","version":"1.0.0","sha256":pin}}
    }))
    .map_err(|error| format!("envelope: {error}"))?;
    let mut set: Vec<ArtifactEnvelope> = [entity, interface]
        .into_iter()
        .map(|v| serde_json::from_value(v).expect("artifact"))
        .collect();
    set.extend([source, mapping]);
    Ok(set)
}

#[test]
#[ignore = "requires a candidate file outside the checkout"]
fn candidate_mapping_admission_and_rows() {
    let path = std::env::var("NORN_BENCH_CANDIDATE").expect("candidate path");
    let candidate: Json =
        serde_json::from_str(&std::fs::read_to_string(path).expect("candidate")).expect("JSON");
    let artifacts = match envelopes(&candidate) {
        Ok(set) => set,
        Err(reason) => {
            report(
                &json!({"admitted":false,"diagnostics":[{"code":"harness.envelope","message":reason}]}),
            );
            return;
        }
    };
    let base: Vec<ArtifactEnvelope> = vec![
        serde_json::from_value(registry("core/artifacts/core.value_types.entity.json"))
            .expect("types"),
    ];
    let wire = |set: &[ArtifactEnvelope]| -> Vec<Value> {
        set.iter()
            .map(|a| Value::from_json(serde_json::to_value(a).expect("wire")).expect("source"))
            .collect()
    };
    let registry_names = BTreeSet::new();
    let checked = norn_checker::check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&wire(&artifacts), &wire(&base)).expect("sources"),
        &EntityArtifactSetContext {
            registry: &registry_names,
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("receipt");
    if !checked.receipt.admitted {
        let diagnostics: Vec<Json> = checked
            .diagnostics
            .iter()
            .map(|d| json!({"code":d.code.to_string(),"message":d.message}))
            .collect();
        report(&json!({"admitted":false,"diagnostics":diagnostics}));
        return;
    }
    let receipt = String::from_utf8(
        norn_ir::canonical::serialize(&checked.receipt.to_canonical_value().expect("value"))
            .expect("canonical"),
    )
    .expect("UTF-8");
    let values = serde_json::to_value(&artifacts).expect("definitions");
    let object =
        serde_json::from_value(values[0]["payload"]["objectType"].clone()).expect("object");
    let interface =
        serde_json::from_value(values[1]["payload"]["interface"].clone()).expect("interface");
    let types: Vec<_> = serde_json::from_value(
        serde_json::to_value(&base).expect("base")[0]["payload"]["propertyTypes"].clone(),
    )
    .expect("value types");
    let model =
        Model::from_definitions_with_interfaces(&[object], &types, &[interface]).expect("model");
    let directory = tempfile::tempdir().expect("temporary cell");
    let database = directory.path().join("trial.sqlite");
    let mut store =
        CellStore::open(&database, model.clone(), StoreOptions::default()).expect("SQLite");
    store
        .install_definitions(
            model.clone(),
            InstalledDefinitionInput {
                canonical_set: serde_json::to_string(
                    &artifacts.iter().chain(&base).collect::<Vec<_>>(),
                )
                .expect("canonical set"),
                registry: Vec::new(),
                receipt_sha256: norn_checker::admission_receipt_sha256(&receipt).expect("hash"),
                admission_receipt: Some(receipt),
                readmission: None,
                diff: json!({"benchmark":"candidate mapping"}),
                installed_at: 1,
                layout: None,
            },
            Some(0),
            "org_demo/dev",
        )
        .expect("install admitted mapping");
    let input = MappingInput {
        mapping: "bench.trial.mapping".into(),
        dataset: "Rows".into(),
        connector_instance: "public".into(),
        partition: "trial".into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-05T00:00:00Z".into()),
    };
    let mut cell = SqliteCell::<
        norn_kernel::ObservationEvent,
        BTreeMap<String, Value>,
        BTreeMap<String, Value>,
    >::new(
        CellStore::open(&database, model, StoreOptions::default()).expect("processing connection"),
        "org_demo/dev",
        |_| BTreeMap::new(),
    )
    .expect("cell");
    let mut rows = Vec::new();
    for (index, row) in candidate["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .enumerate()
    {
        let text = serde_json::to_string(row).expect("row text");
        let payload = Value::from_json_text(&text).expect("row value");
        let hash = norn_ir::canonical::HashProfile::Norn1
            .hash_json(&text)
            .expect("digest");
        let source_id = format!("row-{index}");
        let observed = Observed {
            id: ObservationId(source_id.clone()),
            provenance: Provenance {
                source: "Trial".into(),
                source_id: source_id.clone(),
                mapping_version: "pipeline/1".into(),
                observed_at: LogicalTime::from_millis(1_793_750_400_000),
            },
            input: norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>"Trial",
            "source_id"=>source_id.clone(), "event_id"=>source_id, "source_version"=>"1",
            "pipeline_version"=>"pipeline/1", "kind"=>"upsert", "extracted_at"=>1_793_750_400_000u64,
            "payload"=>payload, "payload_digest"=>hash},
        };
        let id = match store.inject_mapping_bundle(&observed, &input) {
            Ok((id, _)) => id,
            Err(error) => {
                rows.push(json!({"row":index,"capture_error":error.to_string()}));
                continue;
            }
        };
        let pending = store.read().pending_observations().expect("inbox");
        let bundle = pending.iter().find(|p| p.observation_id == id).map(|p| {
            let observed: Observed<Value> =
                norn_ir::runtime::decode(&p.observed).expect("observed");
            norn_ir::runtime::decode::<MappingBundle>(&observed.input).expect("bundle")
        });
        let processed = cell
            .process_mapping_bundle(&id)
            .map_err(|error| error.to_string());
        rows.push(json!({"row":index,"processed":processed.as_ref().ok(),"process_error":processed.as_ref().err(),
            "records":bundle.as_ref().map(|b| serde_json::to_value(&b.records).expect("records")),
            "diagnostics":bundle.as_ref().map(|b| serde_json::to_value(&b.diagnostics).expect("diagnostics"))}));
    }
    report(&json!({"admitted":true,"rows":rows}));
}
