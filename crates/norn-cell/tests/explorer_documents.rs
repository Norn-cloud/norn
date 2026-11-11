//! Studio's Data Explorer documents are the cell's exact output for the
//! synthetic three-systems world (Drop 9), retained as bytes.
//! `NORN_BLESS=1` rewrites them; review the diff.
#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "retained engine explorer documents"
)]
use norn_cell::catalog::CatalogSnapshot;
use norn_cell::{CellStore, InstalledDefinitionInput, Model};
use norn_checker::{
    EntityArtifactSetContext,
    receipt::{ReceiptSources, check_artifact_set_with_receipt},
};
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
use std::collections::BTreeSet;
use std::path::PathBuf;

const DIRECTORY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/demo/explorer");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn retained(name: &str, text: &str) {
    let path = format!("{DIRECTORY}/{name}");
    if std::env::var("NORN_BLESS").as_deref() == Ok("1") {
        std::fs::create_dir_all(DIRECTORY).expect("explorer fixtures directory");
        std::fs::write(&path, text).expect("bless retained document");
    }
    let retained = std::fs::read_to_string(&path).expect("retained document");
    assert!(
        retained == text,
        "{name} differs from the cell's output; review it and rerun with NORN_BLESS=1"
    );
}

/// The admitted three-systems set, as `crates/norn-checker/tests/three_systems.rs` selects it,
/// without the Odoo pack, which the demo leaves out for now.
fn artifacts() -> Vec<ArtifactEnvelope> {
    let excluded = [
        "commerce.SalesOrder.entity",
        "core.Order.interface",
        "core.Entitlement.entity",
        "identity.BusinessUnit.entity",
        "location.Region.entity",
        "location.Space.entity",
        "party.Brand.entity",
    ];
    let mut paths: Vec<PathBuf> = ["core", "packs/canon", "packs/erpnext", "packs/shopify"]
        .into_iter()
        .flat_map(|dir| {
            std::fs::read_dir(root().join(format!("registry/{dir}/artifacts")))
                .expect("artifact directory")
                .map(|entry| entry.expect("entry").path())
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            serde_json::from_str(&std::fs::read_to_string(path).expect("artifact"))
                .expect("envelope")
        })
        .filter(|a: &ArtifactEnvelope| !excluded.contains(&a.artifact_id.as_str()))
        .collect()
}

/// Admit the packs over the core layer through the shared checker, as
/// `mvp_map_three_artifact_set_is_admitted` does, then install the whole set.
fn install(artifacts: &[ArtifactEnvelope]) -> CellStore {
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
        .with_governing_laws(artifacts)
        .expect("governing laws");
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(
            RuntimeValue::from_json(serde_json::to_value(artifacts).expect("set"))
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
        diff: serde_json::json!({}),
        installed_at: 1,
        layout: None,
    };
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    store
        .install_definitions(model, input, Some(0), "org_demo/dev")
        .expect("installed");
    store
}

#[test]
fn three_systems_catalog_is_the_cells_installed_definition_set() {
    let store = install(&artifacts());
    CatalogSnapshot::read_active(&store, &BTreeSet::new()).expect("catalog reads project it");
    let installed = store
        .read()
        .installed_definitions(None)
        .expect("definitions")
        .expect("active");
    retained("three-systems.definitions.json", &installed.canonical_set);
}
