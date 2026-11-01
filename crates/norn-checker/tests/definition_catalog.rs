#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! Correspondence to verification/definition-catalog-v1/SPEC.md.
use norn_checker::{EntityArtifactSetContext, check_artifact_set, definition_catalog::project};
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn artifact(id: &str, payload: &Value) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": id, "kind": "entity", "namespace": "test", "name": id,
        "version": "1.0.0", "lifecycle": "draft", "scope": {}, "dependencies": [],
        "provenance": { "source": "system" }, "payload": payload
    }))
    .expect("fixture")
}

fn check_ref_lifecycle(hops: usize) {
    let values = json!({ "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] });
    let types: Vec<_> = (0..hops).map(|i| json!({ "name": format!("test.state{i}"),
        "type": if i == 0 { values.clone() } else { json!({ "kind": "ref", "name": format!("test.state{}", i.saturating_sub(1)) }) } })).collect();
    let artifacts = [
        artifact(
            "test.types.entity",
            &json!({ "entityVersion": "2.0", "propertyTypes": types }),
        ),
        artifact(
            "test.Order.entity",
            &json!({ "entityVersion": "2.0", "objectType": { "name": "test.Order",
            "properties": [{ "id": "status", "type": { "kind": "ref", "name": format!("test.state{}", hops.saturating_sub(1)) } }],
            "lifecycles": [{ "property": "status", "initial": ["open"], "terminal": ["closed"],
                "transitions": [{ "id": "finish", "from": ["open"], "to": "closed" }] }] } }),
        ),
    ];
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let findings = check_artifact_set(&artifacts, &context);
    assert!(findings.is_empty(), "{findings:?}");
    let catalog = project(&artifacts, &[], &registry).expect("projection");
    assert_eq!(
        json!(catalog.properties),
        json!([{
            "at": { "artifactId": "test.Order.entity", "version": "1.0.0",
                "member": { "kind": "property", "typeName": "test.Order", "property": "status" } },
            "computed": false,
            "lifecycle": { "states": ["open", "closed"], "initial": ["open"], "terminal": ["closed"] }
        }])
    );
    assert_eq!(
        catalog,
        project(&artifacts, &[], &registry).expect("repeat")
    );
}

#[test]
fn admitted_ref_enum_lifecycle_preserves_metadata_and_declaration_identity() {
    check_ref_lifecycle(1);
}

proptest::proptest! {
    #[test]
    fn enum_alias_chains_preserve_lifecycle_metadata(hops in 1usize..8) { check_ref_lifecycle(hops); }
}
