#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! Correspondence to verification/definition-catalog-v1/SPEC.md.
use norn_checker::definition_catalog::project;
use norn_checker::resolve_artifact_set;
use norn_ir::ArtifactEnvelope;
use norn_ir::definition_catalog::{DefinitionCatalog, Member, MemberRef, WriteVia};
use serde_json::Value;
use std::collections::BTreeSet;
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
fn read_json(path: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(format!("{ROOT}/{path}"))
            .unwrap_or_else(|error| panic!("{path} should be readable: {error}")),
    )
    .unwrap_or_else(|error| panic!("{path} should parse: {error}"))
}

fn envelope(value: Value) -> ArtifactEnvelope {
    serde_json::from_value(value).expect("envelope decodes")
}

fn catalog(artifacts: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> DefinitionCatalog {
    resolve_artifact_set(artifacts, base).expect("set resolves");
    project(artifacts, base, &BTreeSet::new()).expect("catalog projects")
}

fn sales_order() -> ArtifactEnvelope {
    envelope(read_json(
        "registry/core/artifacts/commerce.SalesOrder.entity.json",
    ))
}

fn submit_invoice() -> ArtifactEnvelope {
    envelope(read_json(
        "fixtures/ir/commerce-sales-order-submit-invoice.action.json",
    ))
}

fn order_member(member: Member) -> MemberRef {
    MemberRef {
        artifact_id: "commerce.SalesOrder.entity".to_owned(),
        version: "0.1.0".to_owned(),
        member,
    }
}

fn invoice_transition() -> MemberRef {
    order_member(Member::Transition {
        type_name: "commerce.SalesOrder".to_owned(),
        lifecycle: "status".to_owned(),
        transition: "invoice".to_owned(),
    })
}

#[test]
fn an_action_effect_is_an_authored_write_of_the_pinned_transition() {
    let catalog = catalog(&[sales_order(), submit_invoice()], &[]);
    let writes: Vec<_> = catalog
        .writes
        .iter()
        .map(|row| (row.writer.artifact_id.as_str(), &row.target, row.via))
        .collect();
    assert_eq!(
        writes,
        [(
            "commerce.sales_order.submit_invoice.action",
            &invoice_transition(),
            WriteVia::FireTransition
        )]
    );
    assert!(catalog.contains(&catalog.writes[0].target));
}

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
    assert_eq!(
        serde_json::from_value::<MemberRef>(serde_json::to_value(&member).expect("encode"))
            .expect("decode"),
        member
    );
    assert!(catalog.properties.iter().all(
        |row| row.at.artifact_id == artifact.artifact_id && row.at.version == artifact.version
    ));
    assert_eq!(
        catalog,
        project(&[artifact], &[], &BTreeSet::new()).expect("repeat")
    );
}
