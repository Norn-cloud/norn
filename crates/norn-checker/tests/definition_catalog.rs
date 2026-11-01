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
fn a_decide_graph_writes_only_the_declared_members_its_schema_admits() {
    let order = envelope(read_json(
        "fixtures/ir/entity-v2/artifact-set/commerce-purchase-order.entity.json",
    ));
    let rule = envelope(read_json(
        "fixtures/rule-ir-v2/valid/purchase-order-approval.rule.json",
    ));
    let catalog = catalog(&[order, rule], &[]);
    let mut writes: Vec<(String, WriteVia)> = catalog
        .writes
        .iter()
        .map(|row| (row.target.member.id(), row.via))
        .collect();
    writes.sort();
    // `create` writes the object; `transition` writes `submit` and `approve`
    // of `status`, the only lifecycle the schema names.
    assert_eq!(
        writes,
        [
            (
                "transition:commerce.PurchaseOrder#status/approve".to_owned(),
                WriteVia::FactTransition
            ),
            (
                "transition:commerce.PurchaseOrder#status/submit".to_owned(),
                WriteVia::FactTransition
            ),
            (
                "type:commerce.PurchaseOrder".to_owned(),
                WriteVia::FactObject
            ),
        ]
    );
}

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
