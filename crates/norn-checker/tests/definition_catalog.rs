#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! Correspondence to verification/definition-catalog-v1/SPEC.md.
use norn_checker::definition_catalog::project;
use norn_ir::ArtifactEnvelope;
use norn_ir::definition_catalog::{Member, MemberRef};
use std::collections::BTreeSet;
#[test]
fn entity_projection_preserves_identity_declarations_and_order() {
    let artifact: ArtifactEnvelope = serde_json::from_value(serde_json::json!({
        "irVersion": "0.1", "artifactId": "commerce.SalesOrder.entity", "kind": "entity",
        "namespace": "commerce", "name": "SalesOrder", "version": "0.1.0", "lifecycle": "draft",
        "scope": {}, "dependencies": [], "provenance": { "source": "system" },
        "payload": { "entityVersion": "2.0", "objectType": { "name": "commerce.SalesOrder",
            "properties": [{ "id": "order_number", "type": { "kind": "text" } }] } }
    }))
    .expect("fixture");
    let context = norn_checker::EntityArtifactSetContext {
        registry: &BTreeSet::new(),
        base: &[],
        tenant: None,
    };
    assert!(norn_checker::check_artifact_set(std::slice::from_ref(&artifact), &context).is_empty());
    let catalog =
        project(std::slice::from_ref(&artifact), &[], &BTreeSet::new()).expect("projection");
    let at = |member| MemberRef {
        artifact_id: artifact.artifact_id.clone(),
        version: artifact.version.clone(),
        member,
    };
    assert!(catalog.contains(&at(Member::Type {
        type_name: "commerce.SalesOrder".into()
    })));
    assert_eq!(
        catalog
            .properties
            .iter()
            .map(|row| row.at.clone())
            .collect::<Vec<_>>(),
        [at(Member::Property {
            type_name: "commerce.SalesOrder".into(),
            property: "order_number".into()
        })]
    );
    assert_eq!(
        catalog,
        project(&[artifact], &[], &BTreeSet::new()).expect("repeat")
    );
}
