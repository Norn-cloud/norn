//! Studio's Data Explorer object documents are the cell's exact reads of the
//! synthetic three-systems world (Drop 9), projected by `norn_api::objects`,
//! retained as bytes and checked against the generated contract.
//! `NORN_BLESS=1` rewrites them; review the diff.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "retained engine explorer documents"
)]
use norn_api::objects::{NornLinkPage, NornObjectPage, NornObjectRecord};
use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
use norn_cell::read::LinkDirection;
use norn_cell::{CellStore, InstalledDefinitionInput, MAIN_WORLD, Model};
use norn_checker::{
    EntityArtifactSetContext,
    receipt::{ReceiptSources, check_artifact_set_with_receipt},
};
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

// The recorded catalog leaves the Odoo pack out for now (see three-systems.definitions.json).
const SYSTEMS: [&str; 2] = ["erpnext", "shopify"];
const RECORDED_AT: &str = "2026-11-02T12:00:00Z";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn directory() -> PathBuf {
    root().join("fixtures/demo/explorer/objects")
}

fn retained(name: &str, document: &impl serde::Serialize) -> Value {
    let path = directory().join(name);
    let mut text = serde_json::to_string_pretty(document).expect("exact JSON text");
    text.push('\n');
    if std::env::var("NORN_BLESS").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("objects directory");
        std::fs::write(&path, &text).expect("bless retained document");
    }
    let retained = std::fs::read_to_string(&path).expect("retained document");
    assert!(
        retained == text,
        "{name} differs from the cell's output; review it and rerun with NORN_BLESS=1"
    );
    serde_json::from_str(&text).expect("document JSON")
}

/// Install the retained three-systems catalog, admitting the packs over the core layer.
fn install() -> CellStore {
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(
        &std::fs::read_to_string(
            root().join("fixtures/demo/explorer/three-systems.definitions.json"),
        )
        .expect("retained catalog"),
    )
    .expect("artifacts");
    let (base, candidate): (Vec<_>, Vec<_>) = artifacts
        .iter()
        .cloned()
        .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
    let values = |set: &[ArtifactEnvelope]| -> Vec<RuntimeValue> {
        set.iter()
            .map(|a| {
                RuntimeValue::from_json(serde_json::to_value(a).expect("wire")).expect("value")
            })
            .collect()
    };
    let sources = ReceiptSources::new(&values(&candidate), &values(&base)).expect("sources");
    let checked = check_artifact_set_with_receipt(
        &sources,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("checked");
    assert!(checked.receipt.admitted, "{:#?}", checked.diagnostics);
    let (mut objects, mut interfaces, mut properties) = (Vec::new(), Vec::new(), Vec::new());
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Entity)
    {
        let root: EntityRoot = serde_json::from_value(
            serde_json::to_value(artifact.payload.to_json_value()).expect("payload"),
        )
        .expect("entity root");
        match root {
            EntityRoot::V2(root) => objects.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
            EntityRoot::V2Markings(_) | EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {
                panic!(
                    "{} is outside the three-systems profile",
                    artifact.artifact_id
                )
            }
        }
    }
    let model = Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
        .expect("model")
        .with_governing_laws(&artifacts)
        .expect("governing laws");
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(
            RuntimeValue::from_json(serde_json::to_value(&artifacts).expect("set"))
                .expect("value")
                .canonical_json()
                .expect("canonical"),
        )
        .expect("UTF-8"),
        registry: vec![],
        receipt_sha256: checked.receipt.content_sha256().expect("receipt digest"),
        admission_receipt: Some(
            String::from_utf8(checked.receipt.canonical_json().expect("receipt")).expect("UTF-8"),
        ),
        readmission: None,
        diff: json!({}),
        installed_at: 1,
        layout: None,
    };
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    store
        .install_definitions(model, input, Some(0), "org_demo/dev")
        .expect("installed");
    store
}

/// One synthetic source row as the sealed raw envelope a connector delivers.
fn raw(system: &str, record: &Value) -> norn_kernel::Observed<RuntimeValue> {
    let source = format!("synthetic-{system}");
    let id = record
        .get("id")
        .or_else(|| record.get("name"))
        .and_then(Value::as_str)
        .expect("source record identity")
        .to_owned();
    let payload = RuntimeValue::from_json_text(record.to_string()).expect("payload");
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&payload).expect("payload text"))
        .expect("payload digest");
    norn_kernel::Observed {
        id: norn_kernel::ObservationId(format!("{source}/{id}")),
        provenance: norn_kernel::Provenance {
            source: source.clone(),
            source_id: id.clone(),
            mapping_version: format!("{system}.three.mapping"),
            observed_at: norn_kernel::LogicalTime::from_millis(1_793_620_800_000),
        },
        input: norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>source, "source_id"=>id, "event_id"=>"snapshot", "source_version"=>"1", "pipeline_version"=>format!("{system}.three.mapping"), "kind"=>"upsert", "extracted_at"=>1_793_620_800_000u64, "payload"=>payload, "payload_digest"=>digest},
    }
}

/// Each continuation page the engine returns after `after`, until it returns no cursor.
fn retain_continuations(
    reader: &norn_cell::StoreReader<'_>,
    object_type: &str,
    mut after: Option<String>,
) {
    while let Some(cursor) = after {
        let next = NornObjectPage::new(
            object_type,
            reader
                .authoritative_page(MAIN_WORLD, object_type, Some(&cursor), 100)
                .expect("continuation page"),
        );
        retained(
            &format!("{object_type}.after.{}.page.json", file_name(&cursor)),
            &next,
        );
        after = next.scanned_through;
    }
}

fn file_name(id: &str) -> String {
    id.replace('/', "~")
}

#[test]
fn three_systems_object_reads_are_the_cells_exact_documents() {
    let mut store = install();
    let mut captured = Vec::new();
    for system in SYSTEMS {
        let fixture: Value = serde_json::from_str(
            &std::fs::read_to_string(
                root().join(format!("registry/packs/{system}/synthetic.json")),
            )
            .expect("synthetic fixture"),
        )
        .expect("fixture JSON");
        for row in fixture["snapshot"].as_array().expect("snapshot rows") {
            let input = norn_cell::mapping_bundle::MappingInput {
                mapping: format!("{system}.three.mapping"),
                dataset: row["dataset"].as_str().expect("dataset").to_owned(),
                connector_instance: "demo".into(),
                partition: "snapshot".into(),
                recorded_at: norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
            };
            let (id, _) = store
                .inject_mapping_bundle(&raw(system, &row["record"]), &input)
                .expect("captured as evidence");
            captured.push(id);
        }
    }
    let mut cell =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
    // Mapped records with links refuse until the processing profile admits relations (#823).
    let processed = captured
        .iter()
        .filter(|id| cell.process_mapping_bundle(id).is_ok())
        .count();
    assert!(processed > 0, "no three-systems record became an object");

    let reader = cell.store().read();
    let mut index = BTreeMap::new();
    for object_type in [
        "canon.Delivery",
        "canon.Holding",
        "canon.Item",
        "canon.OrderLine",
        "canon.Party",
        "canon.PartyRole",
        "canon.Posting",
        "canon.SettlementObservation",
    ] {
        let page = reader
            .authoritative_page(MAIN_WORLD, object_type, None, 100)
            .expect("authoritative page");
        let page = NornObjectPage::new(object_type, page);
        for record in &page.records {
            let name = file_name(&record.id);
            // Each record is read and retained under its concrete type, as Studio links it.
            let concrete = record.object_type.as_str();
            let object = reader
                .authoritative_object(MAIN_WORLD, concrete, &record.id)
                .expect("object read")
                .expect("present");
            retained(
                &format!("{concrete}/{name}.object.json"),
                &NornObjectRecord::from(object),
            );
            let links = reader
                .links_touching_page(
                    MAIN_WORLD,
                    concrete,
                    &record.id,
                    LinkDirection::Both,
                    None,
                    None,
                    100,
                )
                .expect("link page");
            retained(
                &format!("{concrete}/{name}.links.json"),
                &NornLinkPage::new(concrete, &record.id, links),
            );
            let mut budget = DecodeBudget::new(&ReactiveLimits {
                max_scan: 100,
                max_candidate_bytes_per_commit: 10_000_000,
                ..ReactiveLimits::from_profile(&ResourceLimitProfile::norn_limits_v1())
            });
            let fact = norn_ir::FactRef::new(concrete, &record.id, record.version);
            let lineage = reader
                .instance_source_provenance(MAIN_WORLD, &fact, &mut budget, |_| Ok(true))
                .expect("source lineage")
                .expect("a mapped object retains its source bundle");
            retained(&format!("{concrete}/{name}.source.json"), &lineage);
        }
        index.insert(object_type, page.records.len());
        let after = page.scanned_through.clone();
        retained(&format!("{object_type}.page.json"), &page);
        retain_continuations(&reader, object_type, after);
    }
    assert_eq!(
        index["canon.Item"], 1,
        "three systems describe one gold item"
    );
}

#[test]
fn retained_object_documents_validate_against_their_projected_schemas() {
    let contract: Value =
        serde_json::from_str(&norn_api::openapi::openapi_json().expect("contract"))
            .expect("contract JSON");
    let validate = |root: &str, path: &std::path::Path| {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$ref": format!("#/components/schemas/{root}"),
            "components": contract["components"],
        });
        let validator = jsonschema::validator_for(&schema).expect("contract schema compiles");
        let document: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("document")).expect("JSON");
        let errors: Vec<String> = validator
            .iter_errors(&document)
            .map(|e| e.to_string())
            .collect();
        assert!(
            errors.is_empty(),
            "{} does not satisfy {root}: {errors:#?}",
            path.display()
        );
    };
    let mut seen = 0;
    for entry in walk(&directory()) {
        let name = entry
            .file_name()
            .and_then(|n| n.to_str())
            .expect("file name")
            .to_owned();
        let root = [
            (".page.json", "NornObjectPage"),
            (".object.json", "NornObjectRecord"),
            (".links.json", "NornLinkPage"),
            (".source.json", "NornInstanceSourceProvenance"),
        ]
        .into_iter()
        .find_map(|(suffix, root)| name.ends_with(suffix).then_some(root))
        .unwrap_or_else(|| panic!("unexpected document {name}"));
        validate(root, &entry);
        seen += 1;
    }
    assert!(seen > 8, "every concept page plus at least one object");
}

fn walk(directory: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory).expect("directory") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}
