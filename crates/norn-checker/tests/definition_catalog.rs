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
use norn_ir::definition_catalog::{
    DefinitionCatalog, LawBinding, MapRule, Member, MemberRef, WriteVia,
};
use norn_ir::mapping::GapReason;
use serde_json::{Value, json};
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
fn a_mapping_observes_members_and_declares_gaps() {
    let mapping = envelope(json!({
        "irVersion": "0.1",
        "artifactId": "erpnext.sales_order.mapping",
        "kind": "mapping",
        "namespace": "erpnext",
        "name": "Sales orders",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "format": "norn.mapping/1",
            "source": {
                "schemaId": "erpnext.schema",
                "version": "0.1.0",
                "sha256": "a".repeat(64)
            },
            "interfaces": ["core.Order.interface"],
            "maps": [{
                "id": "sales_order",
                "source": { "dataset": "Sales Order" },
                "target": {
                    "entity": "commerce.SalesOrder.entity",
                    "interface": "core.Order.interface"
                },
                "subject": "input.name",
                "sourceRecord": "input.name",
                "properties": [{ "property": "total_amount", "value": "input.grand_total" }],
                "links": [{ "link": "counterparty", "key": "input.customer" }],
                "lifecycles": [{ "property": "status", "state": "input.status" }],
                "gaps": [{ "member": "amended_from", "reason": "deferred" }]
            }]
        }
    }));
    let catalog = catalog(&[sales_order(), mapping], &[]);
    let mut rows: Vec<(String, MapRule)> = catalog
        .mappings
        .iter()
        .map(|row| {
            assert_eq!(row.map.member.id(), "map:sales_order");
            assert_eq!(row.dataset, "Sales Order");
            assert_eq!(row.target.artifact_id, "commerce.SalesOrder.entity");
            (row.target.member.id(), row.rule.clone())
        })
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        [
            (
                "link:commerce.SalesOrder#amended_from".to_owned(),
                MapRule::Gap {
                    reason: GapReason::Deferred
                }
            ),
            (
                "link:commerce.SalesOrder#counterparty".to_owned(),
                MapRule::Link {}
            ),
            (
                "property:commerce.SalesOrder#status".to_owned(),
                MapRule::Lifecycle {}
            ),
            (
                "property:commerce.SalesOrder#total_amount".to_owned(),
                MapRule::Property {}
            ),
            ("type:commerce.SalesOrder".to_owned(), MapRule::Object {}),
        ]
    );
    // Observation is not authorship: a mapping adds no write.
    assert!(catalog.writes.is_empty());
}

#[test]
fn laws_bind_their_subject_and_response() {
    let mut artifacts: Vec<ArtifactEnvelope> = Vec::new();
    for path in ["fixtures/law-ir/entities.json", "fixtures/law-ir/laws.json"] {
        let Value::Array(values) = read_json(path) else {
            panic!("{path} is a list of artifacts");
        };
        artifacts.extend(values.into_iter().map(envelope));
    }
    let catalog = catalog(&artifacts, &[]);
    assert_eq!(catalog.laws.len(), 5);
    let identity = catalog
        .laws
        .iter()
        .find(|row| row.at.artifact_id == "erpnext.invariant_identity.law")
        .expect("the invariant law is a row");
    let purchase_order = MemberRef {
        artifact_id: "erpnext.PurchaseOrder.entity".to_owned(),
        version: "0.1.0".to_owned(),
        member: Member::Type {
            type_name: "erpnext.PurchaseOrder".to_owned(),
        },
    };
    assert_eq!(identity.subject, purchase_order);
    assert!(catalog.contains(&identity.subject));
    assert_eq!(
        identity.binds,
        Some(LawBinding::Invariant {
            owner: purchase_order,
            invariant: "amount_frozen".to_owned(),
        })
    );
    assert!(
        catalog
            .laws
            .iter()
            .filter(|row| row.at.artifact_id != "erpnext.invariant_identity.law")
            .all(|row| row.binds.is_none())
    );
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

#[test]
fn local_lifecycle_states_keep_inherited_property_ownership() {
    let mut parent: serde_json::Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/commerce.SalesOrder.entity.json"
    ))
    .expect("fixture");
    parent["payload"]["objectType"]["implements"] = serde_json::json!([]);
    parent["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .retain(|p| p["id"] == "status");
    parent["payload"]["objectType"]["identity"] = serde_json::json!([]);
    parent["payload"]["objectType"]["links"] = serde_json::json!([]);
    let mut child = parent.clone();
    child["artifactId"] = "commerce.Child.entity".into();
    child["payload"]["objectType"]["name"] = "commerce.Child".into();
    child["payload"]["objectType"]["extends"] = serde_json::json!("commerce.SalesOrder");
    child["payload"]["objectType"]["properties"] = serde_json::json!([]);
    child["payload"]["objectType"]["links"] = serde_json::json!([]);
    child["payload"]["objectType"]["identity"] = serde_json::json!([]);
    parent["payload"]["objectType"]["lifecycles"] = serde_json::json!([]);
    let artifacts = [parent, child].map(|value| serde_json::from_value(value).expect("envelope"));
    let registry = BTreeSet::new();
    let context = norn_checker::EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let findings = norn_checker::check_artifact_set(&artifacts, &context);
    assert!(findings.is_empty(), "{findings:?}");
    let catalog = project(&artifacts, &[], &BTreeSet::new()).expect("projection");
    let state = MemberRef {
        artifact_id: "commerce.Child.entity".into(),
        version: "0.1.0".into(),
        member: Member::State {
            type_name: "commerce.Child".into(),
            lifecycle: "status".into(),
            state: "draft".into(),
        },
    };
    assert!(catalog.contains(&state));
    assert!(
        !catalog
            .properties
            .iter()
            .any(|row| row.at.artifact_id == state.artifact_id)
    );
    assert!(
        catalog
            .properties
            .iter()
            .any(|row| row.at.member.id() == "property:commerce.SalesOrder#status")
    );
}
