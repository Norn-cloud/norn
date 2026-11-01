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
    let artifact: ArtifactEnvelope = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/commerce.SalesOrder.entity.json"
    ))
    .expect("fixture");
    let catalog =
        project(std::slice::from_ref(&artifact), &[], &BTreeSet::new()).expect("projection");
    let member = MemberRef {
        artifact_id: artifact.artifact_id.clone(),
        version: artifact.version.clone(),
        member: Member::Type {
            type_name: "commerce.SalesOrder".into(),
        },
    };
    assert!(catalog.contains(&member));
    assert_eq!(member.member.id(), "type:commerce.SalesOrder");
    assert!(catalog.properties.iter().all(
        |row| row.at.artifact_id == artifact.artifact_id && row.at.version == artifact.version
    ));
    assert_eq!(
        catalog,
        project(&[artifact], &[], &BTreeSet::new()).expect("repeat")
    );
}

