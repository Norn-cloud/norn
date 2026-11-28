//! The registry's core, canon and connector packs installed in a cell, for
//! tests that map the packs' synthetic source rows (OBS-LINK, OBS-PAGE,
//! OBS-LINK-DISPUTE-1).
#![allow(
    dead_code,
    clippy::expect_used,
    reason = "shared test fixture; each user takes what it needs"
)]

use norn_cell::{CellStore, InstalledDefinitionInput, Model, StoreOptions};
use norn_ir::{ArtifactEnvelope, InterfaceDef, ObjectTypeDef, PropertyTypeDef};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

/// The repository root.
pub(crate) fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The core, canon, erpnext, shopify and odoo artifacts these tests install.
pub(crate) fn fixture_artifacts() -> Vec<ArtifactEnvelope> {
    let mut artifacts: Vec<ArtifactEnvelope> = Vec::new();
    for dir in [
        "core",
        "packs/canon",
        "packs/erpnext",
        "packs/shopify",
        "packs/odoo",
    ] {
        for file in
            fs::read_dir(root().join(format!("registry/{dir}/artifacts"))).expect("artifacts")
        {
            let artifact: ArtifactEnvelope = serde_json::from_str(
                &fs::read_to_string(file.expect("file").path()).expect("read"),
            )
            .expect("artifact");
            if ![
                "commerce.SalesOrder.entity",
                "core.Order.interface",
                "core.Entitlement.entity",
                "identity.BusinessUnit.entity",
                "identity.Directory.interface",
                "location.Region.entity",
                "location.Space.entity",
                "party.Brand.entity",
                "party.Organization.entity",
            ]
            .contains(&artifact.artifact_id.as_str())
            {
                artifacts.push(artifact);
            }
        }
    }
    artifacts
}

fn observed_tombstone_type() -> ObjectTypeDef {
    serde_json::from_value(json!({
        "name": norn_kernel::snapshot_observe::OBSERVED_TOMBSTONE_TYPE,
        "properties": [
            { "id": "objectType", "type": { "kind": "text" } },
            { "id": "key", "type": { "kind": "text" } },
            { "id": "at", "type": { "kind": "text" } },
            { "id": "source", "type": { "kind": "text" } },
            { "id": "mappingVersion", "type": { "kind": "text" } },
            { "id": "provenance", "type": { "kind": "json-ext" } }
        ]
    }))
    .expect("tombstone type")
}

/// A store at `path` with the artifacts installed after `edit`, its scope bound.
pub(crate) fn store(path: &Path, edit: impl FnOnce(&mut Vec<ArtifactEnvelope>)) -> CellStore {
    let mut artifacts = fixture_artifacts();
    edit(&mut artifacts);
    artifacts.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
    let (base, candidate): (Vec<_>, Vec<_>) = artifacts
        .iter()
        .cloned()
        .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
    let diagnostics = norn_checker::check_artifact_set(
        &candidate,
        &norn_checker::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    );
    assert!(
        diagnostics
            .iter()
            .all(|d| d.severity != norn_ir::DiagnosticSeverity::Error),
        "{diagnostics:#?}"
    );
    let mut types: Vec<ObjectTypeDef> = vec![];
    let mut interfaces: Vec<InterfaceDef> = vec![];
    let mut properties: Vec<PropertyTypeDef> = vec![];
    for artifact in &artifacts {
        let payload = serde_json::to_value(artifact).expect("artifact")["payload"].clone();
        if let Some(value) = payload.get("objectType") {
            types.push(serde_json::from_value(value.clone()).expect("object"));
        }
        if let Some(value) = payload.get("interface") {
            interfaces.push(serde_json::from_value(value.clone()).expect("interface"));
        }
        if let Some(value) = payload.get("propertyTypes") {
            properties.extend(
                serde_json::from_value::<Vec<PropertyTypeDef>>(value.clone()).expect("properties"),
            );
        }
    }
    if !types
        .iter()
        .any(|t| t.name.as_str() == norn_kernel::snapshot_observe::OBSERVED_TOMBSTONE_TYPE)
    {
        types.push(observed_tombstone_type());
    }
    let model = Model::from_definitions_with_interfaces(&types, &properties, &interfaces)
        .expect("model")
        .with_governing_laws(&artifacts)
        .expect("laws");
    let mut store = CellStore::open(path, model.clone(), StoreOptions::default()).expect("store");
    // Bound before the first commit, so every exported record carries it.
    store.bind("scope", "org_demo/dev").expect("scope");
    store
        .install_definitions(
            model,
            InstalledDefinitionInput {
                canonical_set: String::from_utf8(
                    norn_ir::canonical::canonical_json_of(&artifacts).expect("set"),
                )
                .expect("UTF8"),
                registry: vec![],
                // A well-formed digest, so the cell's history exports.
                receipt_sha256: norn_ir::canonical::domain_hash_hex(
                    b"norn.test.receipt\0",
                    &"synthetic-mapped-links",
                )
                .expect("receipt digest"),
                admission_receipt: None,
                readmission: None,
                diff: json!({}),
                installed_at: 1_793_702_400_000,
                layout: None,
            },
            None,
            "org_demo/dev",
        )
        .expect("install");
    store
}
