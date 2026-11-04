//! DATA-LINEAGE / LINEAGE-SUM-1: canonical contributions resolve through admitted bundles.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "source lineage conformance"
)]
#[path = "../query_support/mod.rs"]
mod query_support;
use norn_cell::compute::QueryContext;
use norn_cell::{CellStore, InstalledDefinitionInput, MAIN_WORLD, Model, SqliteCell};
use norn_ir::{ArtifactEnvelope, RuntimeValue as V};
use norn_kernel::{LogicalTime, ObservationId, Observed, Provenance};
use query_support::*;
use std::collections::{BTreeMap, BTreeSet};
pub(crate) const SCOPE: &str = "org_demo/dev";
pub(crate) const WEEK: &str = "\"2026-10-19\"";
pub(crate) type Host =
    SqliteCell<norn_kernel::ObservationEvent, norn_ir::RuntimeObject, norn_ir::RuntimeObject>;
pub(crate) fn artifacts() -> Vec<ArtifactEnvelope> {
    let mut artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(include_str!(
        "../../../../fixtures/ir/lineage/scrap-yield.artifacts.json"
    ))
    .expect("typed authoring output");
    let source = artifacts
        .iter()
        .find(|a| a.artifact_id == "demo.scrap.source")
        .expect("source");
    let pin = norn_checker::ArtifactRef::of(source).expect("exact source pin");
    let mapping = artifacts
        .iter_mut()
        .find(|a| a.artifact_id == "demo.scrap.mapping")
        .expect("mapping");
    let mut binding = serde_json::to_value(&*mapping).expect("compiled declaration");
    binding["payload"]["source"]["sha256"] = serde_json::Value::String(pin.sha256);
    *mapping = serde_json::from_value(binding).expect("bind compiled source identity");
    artifacts.push(
        serde_json::from_str(include_str!(
            "../../../../registry/core/artifacts/core.units.json"
        ))
        .expect("pinned unit catalog"),
    );
    artifacts
}
pub(crate) fn store(path: &std::path::Path) -> CellStore {
    let artifacts = artifacts();
    let context = norn_checker::EntityArtifactSetContext {
        registry: &BTreeSet::default(),
        base: &[],
        tenant: None,
    };
    let findings = norn_checker::check_artifact_set(&artifacts, &context);
    assert!(findings.is_empty(), "{findings:#?}");
    let mut objects = Vec::new();
    let mut interfaces = Vec::new();
    for artifact in &artifacts {
        let value = serde_json::to_value(artifact).expect("generated definition");
        if let Some(object) = value["payload"].get("objectType") {
            objects.push(serde_json::from_value(object.clone()).expect("object"));
        }
        if let Some(interface) = value["payload"].get("interface") {
            interfaces.push(serde_json::from_value(interface.clone()).expect("interface"));
        }
    }
    let model = Model::from_definitions_with_interfaces(&objects, &[], &interfaces).expect("model");
    let mut store =
        CellStore::open(path, model.clone(), norn_cell::StoreOptions::default()).expect("store");
    let bytes = norn_ir::canonical::canonical_json_of(&artifacts).expect("canonical definitions");
    store
        .install_definitions(
            model,
            InstalledDefinitionInput {
                canonical_set: String::from_utf8(bytes).expect("UTF8"),
                registry: vec![],
                receipt_sha256: "synthetic-lineage-admission".into(),
                admission_receipt: None,
                readmission: None,
                diff: serde_json::json!({}),
                installed_at: CLOCK - 1,
                layout: None,
            },
            None,
            SCOPE,
        )
        .expect("retained mapping definitions");
    store
}
pub(crate) fn raw(fields: &[&str], index: usize) -> Observed<V> {
    let id = fields[2];
    let number =
        |text: &str| V::from(norn_ir::Decimal::parse(&format!("{text}e-3")).expect("exact grams"));
    let payload = norn_ir::runtime_object! {
        "id"=>id, "source_row"=>format!("external-row/{index}"),
        "input"=>number(fields[4]), "recovered"=>number(fields[5]), "loss"=>number(fields[6]),
        "occurred_at"=>fields[8], "metal"=>fields[3],
        "catalog"=>norn_ir::runtime_object! {"tags"=>vec![V::from("recycled"), V::from("gold")], "label"=>"Catalog enrichment"},
    };
    let digest = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(
        &norn_ir::canonical::canonical_json_of(&payload).expect("source payload"),
    );
    let document = format!("ERP-DOC/{index}");
    Observed {
        id: ObservationId(format!("transport/{index}")),
        provenance: Provenance {
            source: "synthetic-erpnext".into(),
            source_id: document.clone(),
            mapping_version: "scrap/1".into(),
            observed_at: LogicalTime::from_millis(CLOCK),
        },
        input: norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>"synthetic-erpnext", "source_id"=>document,
        "event_id"=>index.to_string(), "source_version"=>"1", "pipeline_version"=>"scrap/1", "kind"=>"upsert",
        "extracted_at"=>CLOCK, "payload"=>payload, "payload_digest"=>digest},
    }
}
pub(crate) fn loaded() -> (
    Host,
    norn_checker::metric::compute::QuestionPlan,
    QueryContext,
    Vec<Observed<V>>,
    tempfile::TempDir,
) {
    loaded_with_extra_source_bytes(0)
}
pub(crate) fn loaded_with_extra_source_bytes(
    extra: usize,
) -> (
    Host,
    norn_checker::metric::compute::QuestionPlan,
    QueryContext,
    Vec<Observed<V>>,
    tempfile::TempDir,
) {
    let directory = tempfile::tempdir().expect("directory");
    let mut store = store(&directory.path().join("cell.sqlite"));
    let mut ids = Vec::new();
    let mut replayed = None;
    let mut raws = Vec::new();
    let input = norn_cell::mapping_bundle::MappingInput {
        mapping: "demo.scrap.mapping".into(),
        dataset: "conversions".into(),
        connector_instance: "demo".into(),
        partition: "measurements".into(),
        recorded_at: norn_ir::UtcTimestamp("2026-10-26T00:00:00Z".into()),
    };
    for (index, line) in include_str!("../../../../fixtures/demo/scrap-yield-k1.csv")
        .lines()
        .skip(1)
        .enumerate()
    {
        let fields: Vec<_> = line.split(',').collect();
        let mut raw = raw(&fields, index);
        if extra > 0 && fields[2] == "SYN-MELT-15-1" {
            let payload = raw
                .input
                .as_object_mut()
                .expect("envelope")
                .get_mut("payload")
                .expect("payload");
            payload
                .as_object_mut()
                .expect("record")
                .insert("extra_source_evidence".into(), V::from("x".repeat(extra)));
            let digest = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(
                &norn_ir::canonical::canonical_json_of(payload).expect("sealed payload"),
            );
            raw.input
                .as_object_mut()
                .expect("envelope")
                .insert("payload_digest".into(), V::from(digest));
        }
        // The cell computes both the mapping and its bundle identity before processing.
        let (id, _) = store
            .inject_mapping_bundle(&raw, &input)
            .expect("sealed capture");
        if fields[2] == "SYN-MELT-15-1" {
            let mut duplicate = raw.clone();
            duplicate.id.0 = "redelivered transport".into();
            let (same, inserted) = store
                .inject_mapping_bundle(&duplicate, &input)
                .expect("redelivery");
            assert_eq!(same, id);
            assert!(!inserted, "redelivery adds no numeric instance");
            replayed = Some(id.clone());
        }
        ids.push(id);
        raws.push(raw);
    }
    let mut cell = SqliteCell::new(store, SCOPE, |_| BTreeMap::new()).expect("host");
    for id in ids {
        assert!(
            cell.process_mapping_bundle(&id)
                .expect("mapping processing")
        );
    }
    let head = cell.store().verified_head().expect("processed head");
    assert!(
        !cell
            .process_mapping_bundle(replayed.as_deref().expect("contributing source row"))
            .expect("processed redelivery")
    );
    assert_eq!(
        cell.store().verified_head().expect("no duplicate commit"),
        head
    );
    let (_, plan, mut context) = fixture();
    context.world = MAIN_WORLD.into();
    context.scope = SCOPE.into();
    context.mapping_pin = "demo.scrap.mapping/0.1.0".into();
    (cell, plan, context, raws, directory)
}
