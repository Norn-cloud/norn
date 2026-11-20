//! A cell with one object type, a source schema and mapping that read it
//! field by field, and the given laws: the per-row path (observations
//! mapped and committed one at a time) and the dataset path over the same
//! definitions.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    dead_code,
    unreachable_pub,
    reason = "test support shared by the dataset tests and the benchmark"
)]

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::{CellStore, InstalledDefinitionInput, Model, SqliteCell, StoreOptions};
use norn_checker::{ArtifactRef, EntityArtifactSetContext};
use norn_ir::{ArtifactEnvelope, ResourceLimitProfile, RuntimeValue as Value};
use serde_json::json;

/// One property and the source field of the same name it is mapped from.
pub struct Field {
    /// The property and field id.
    pub id: &'static str,
    /// The property type (`{"kind": ...}`).
    pub kind: serde_json::Value,
    /// Whether the field is required; an optional one is a nullable property.
    pub required: bool,
}

/// The per-row cell.
pub type Cell =
    SqliteCell<norn_kernel::ObservationEvent, BTreeMap<String, Value>, BTreeMap<String, Value>>;

fn registry(path: &str) -> serde_json::Value {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry");
    serde_json::from_str(&std::fs::read_to_string(root.join(path)).expect("registry artifact"))
        .expect("artifact JSON")
}

/// The type `name` with `fields` (the first is its identity), its source and
/// mapping (`bench.dataset.mapping`), and `laws` (`(id, severity, expression)`).
pub fn artifacts(
    name: &str,
    fields: &[Field],
    laws: &[(&str, &str, &str)],
) -> (Vec<ArtifactEnvelope>, Vec<ArtifactEnvelope>) {
    let provenance =
        json!({"source":"system","sourcePath":"crates/norn-cell/tests/support/dataset_cell.rs"});
    let scope = json!({"environmentId":"dev","orgId":"org_demo"});
    let wrap = |id: &str,
                kind: &str,
                payload: serde_json::Value,
                dependencies: serde_json::Value| {
        json!({"artifactId":id,"dependencies":dependencies,"irVersion":"0.1","kind":kind,"lifecycle":"draft",
            "name":id,"namespace":"bench","payload":payload,"version":"1.0.0",
            "provenance":provenance,"scope":scope})
    };
    let identity = fields.first().expect("an identity field").id;
    let properties: Vec<_> = fields
        .iter()
        .map(|field| {
            let mut property = json!({"id":field.id,"type":field.kind});
            if !field.required {
                property["constraints"] = json!({"nullable":true});
            }
            property
        })
        .collect();
    let entity_id = format!("{name}.entity");
    let interface_name = format!("{name}Source");
    let interface_id = format!("{interface_name}.interface");
    let entity = wrap(
        &entity_id,
        "entity",
        json!({"entityVersion":"2.0","objectType":{"name":name,"implements":[interface_name],
            "identity":[{"name":"primary","properties":[identity]}],"properties":properties}}),
        json!([{"artifactId":"core.units","kind":"entity","required":true,"version":"1.0.0"}]),
    );
    let interface = wrap(
        &interface_id,
        "entity",
        json!({"entityVersion":"2.0","interface":{"name":interface_name,"properties":[{"id":identity,"type":{"kind":"text"}}]}}),
        json!([]),
    );
    let laws = laws.iter().map(|(id, severity, expression)| {
        wrap(
            &format!("bench.{id}.law"),
            "law",
            json!({"id":id,"lawVersion":"0.1","modality":{"kind":"always"},"owner":"bench",
                "provenance":provenance,"rationale":id,
                "response":{"kind":"predicate","expression":expression},
                "scope":{"objectTypes":[name],"root":{"kind":"entity","id":name}},
                "severity":severity,"subject":name}),
            json!([{"artifactId":entity_id,"kind":"entity","required":true,"version":"1.0.0"}]),
        )
    });
    let source_fields: Vec<_> = fields
        .iter()
        .enumerate()
        .map(|(ordinal, field)| {
            let kind = match field.kind["kind"].as_str() {
                Some("decimal") => "decimal",
                _ => "string",
            };
            json!({"dataset":"Rows","name":field.id,"nativeType":"text","ordinal":ordinal,
                "required":field.required,"typeCandidate":{"origin":"declared","type":kind}})
        })
        .collect();
    let source: ArtifactEnvelope = serde_json::from_value(json!({
        "artifactId":"bench.dataset.source","dependencies":[],"irVersion":"0.1","kind":"source_schema",
        "lifecycle":"draft","name":"Dataset extract","namespace":"bench","provenance":provenance,
        "scope":scope,"version":"1.0.0","payload":{"adapter":{"name":"bench.jsonl","version":"1.0.0"},
        "datasets":[{"id":"Rows","kind":"table","nativeName":"Rows"}],"fields":source_fields,
        "format":"norn.source-schema/1","observedAt":"2026-11-04T00:00:00Z",
        "rawMetadata":{"uri":"https://github.com/Norn-cloud/norn/blob/main/crates/norn-cell/tests/support/dataset_cell.rs","sha256":"0000000000000000000000000000000000000000000000000000000000000000"},
        "source":{"system":"Bench","instance":"public"}}
    }))
    .expect("source schema");
    let pin = ArtifactRef::of(&source).expect("source pin").sha256;
    let mapped: Vec<_> = fields
        .iter()
        .map(|field| {
            let value = if field.kind["kind"] == "timestamp" {
                format!("timestamp(input.{})", field.id)
            } else {
                format!("input.{}", field.id)
            };
            json!({"property":field.id,"value":value})
        })
        .collect();
    let dependency = |id: &str, kind: &str| json!({"artifactId":id,"version":"1.0.0","kind":kind,"required":true});
    let subject = format!("input.{identity}");
    let mapping: ArtifactEnvelope = serde_json::from_value(json!({
        "artifactId":"bench.dataset.mapping","irVersion":"0.1","kind":"mapping","lifecycle":"draft",
        "name":"Dataset mapping","namespace":"bench","provenance":provenance,"scope":scope,"version":"1.0.0",
        "dependencies":[dependency("bench.dataset.source","source_schema"),
            dependency(&entity_id,"entity"), dependency(&interface_id,"entity")],
        "payload":{"extensions":{"norn.mapping.generic/1.0.0":{"parity":"unqualified"}},
            "format":"norn.mapping/1","interfaces":[interface_id],
            "maps":[{"id":"row","source":{"dataset":"Rows"},"subject":subject,"sourceRecord":subject,
                "target":{"entity":entity_id,"interface":interface_id},"properties":mapped}],
            "source":{"schemaId":"bench.dataset.source","version":"1.0.0","sha256":pin}}
    }))
    .expect("mapping");
    let candidate = [entity, interface]
        .into_iter()
        .chain(laws)
        .map(|v| serde_json::from_value(v).expect("artifact"))
        .chain([source, mapping])
        .collect();
    let base = vec![
        serde_json::from_value(registry("core/artifacts/core.value_types.entity.json"))
            .expect("core types"),
        serde_json::from_value(registry("core/artifacts/core.units.json")).expect("core units"),
    ];
    (candidate, base)
}

/// A cell with [`artifacts`] admitted by the checker and installed.
pub fn cell(
    name: &str,
    fields: &[Field],
    laws: &[(&str, &str, &str)],
) -> (tempfile::TempDir, Cell) {
    let (artifacts, base) = artifacts(name, fields, laws);
    let wire = |set: &[ArtifactEnvelope]| -> Vec<Value> {
        set.iter()
            .map(|a| Value::from_json(serde_json::to_value(a).expect("wire")).expect("value"))
            .collect()
    };
    let (sources, base_sources) = (wire(&artifacts), wire(&base));
    let registry = BTreeSet::new();
    let checked = norn_checker::check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&sources, &base_sources).expect("receipt sources"),
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("receipt");
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    let receipt = String::from_utf8(
        norn_ir::canonical::serialize(&checked.receipt.to_canonical_value().expect("receipt"))
            .expect("receipt canonicalizes"),
    )
    .expect("UTF-8 receipt");
    let values = serde_json::to_value(&artifacts).expect("definitions");
    let object =
        serde_json::from_value(values[0]["payload"]["objectType"].clone()).expect("object");
    let interface =
        serde_json::from_value(values[1]["payload"]["interface"].clone()).expect("interface");
    let types: Vec<_> = serde_json::from_value(
        serde_json::to_value(&base).expect("base")[0]["payload"]["propertyTypes"].clone(),
    )
    .expect("value types");
    let model = Model::from_definitions_with_interfaces(&[object], &types, &[interface])
        .expect("model")
        .with_governing_laws(&artifacts)
        .expect("governing laws");
    let directory = tempfile::tempdir().expect("temporary cell");
    let database = directory.path().join("rows.sqlite");
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
                diff: json!({"test":"dataset cell"}),
                installed_at: 1,
                layout: None,
            },
            Some(0),
            "org_demo/dev",
        )
        .expect("install");
    drop(store);
    let cell = Cell::new(
        CellStore::open(&database, model, StoreOptions::default()).expect("cell connection"),
        "org_demo/dev",
        |_| BTreeMap::new(),
    )
    .expect("cell");
    (directory, cell)
}
