#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! What `norn-kernel` enforces at commit (T2-KER-2b) is what the checker
//! validates: the lifecycle refusal codes (`norn_ir::lifecycle_codes`) are
//! exactly the checker's static codes, and the effective lifecycles
//! (`norn_ir::effective_lifecycles`) equal the checker's
//! `EffectiveLifecycle`s, including inherited lifecycles narrowed by a
//! subtype's enum without a redeclaration.

use std::collections::BTreeMap;
use std::path::Path;

use norn_checker::{EffectiveCatalog, lifecycle_writes, lifecycles};
use norn_ir::{
    ArtifactEnvelope, FiringRefusal, ObjectTypeDef, ObjectTypeExtension,
    declared_lifecycle_properties, effective_lifecycles, lifecycle_codes,
};
use serde_json::{Value, json};

fn object_type(path: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/ir/entity-v2/object-types")
        .join(path);
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
}

fn envelope(object_type: &Value) -> ArtifactEnvelope {
    let name = object_type["name"].as_str().expect("name");
    serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": format!("{name}.entity"),
        "kind": "entity",
        "namespace": "commerce",
        "name": name,
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk", "sourcePath": "tests/shared_lifecycle_semantics.rs" },
        "payload": { "entityVersion": "2.0", "objectType": object_type }
    }))
    .expect("envelope decodes")
}

#[test]
fn effective_lifecycles_equal_the_checker_machines() {
    // A subtype that only narrows the inherited status enum (no `rejected`)
    // without redeclaring the lifecycle: `reject` and `revise` drop out.
    let narrowed = json!({
        "name": "commerce.NoRejectPurchaseOrder",
        "extends": "commerce.PurchaseOrder",
        "properties": [{
            "id": "status",
            "type": { "kind": "enum", "values": [
                { "value": "draft" }, { "value": "submitted" }, { "value": "approved" },
                { "value": "received" }, { "value": "cancelled" }
            ] }
        }]
    });
    let types = [
        object_type("purchase-order.object-type.json"),
        object_type("rush-purchase-order.object-type.json"),
        narrowed,
    ];
    let artifacts: Vec<ArtifactEnvelope> = types.iter().map(envelope).collect();
    let defs: Vec<ObjectTypeDef> = types
        .iter()
        .map(|value| serde_json::from_value(value.clone()).expect("object type decodes"))
        .collect();
    let by_name: BTreeMap<&str, &ObjectTypeDef> =
        defs.iter().map(|def| (def.name.as_str(), def)).collect();
    let lookup = |name: &str| by_name.get(name).copied();
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    for def in &defs {
        let name = def.name.as_str();
        let checker = catalog
            .object_type(&format!("{name}.entity"))
            .expect("defined")
            .unwrap_or_else(|finding| panic!("{name} unavailable: {finding:?}"));
        let expected: Vec<_> = checker
            .lifecycles
            .iter()
            .map(|effective| effective.lifecycle.clone())
            .collect();
        let shared: Vec<_> = effective_lifecycles(&lookup, &[], name)
            .into_values()
            .collect();
        assert_eq!(shared, expected, "{name}");
    }
    let narrowed = effective_lifecycles(&lookup, &[], "commerce.NoRejectPurchaseOrder");
    let status = narrowed
        .values()
        .find(|l| l.property.as_str() == "status")
        .expect("status");
    assert!(status.fire("reject", Some("submitted")).is_err());
    assert!(status.fire("approve", Some("submitted")).is_ok());
}

#[test]
fn immutable_states_are_inherited_through_lifecycle_redeclaration() {
    let mut base = object_type("purchase-order.object-type.json");
    base["lifecycles"][0]["immutable"] = json!(["submitted"]);
    let child = object_type("rush-purchase-order.object-type.json");
    let artifacts = [envelope(&base), envelope(&child)];
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let child = catalog
        .object_type("commerce.RushPurchaseOrder.entity")
        .expect("child is defined")
        .expect("child is available");
    let status = child.lifecycle("status").expect("status lifecycle");
    assert_eq!(status.lifecycle.immutable, ["submitted"]);

    let defs: Vec<ObjectTypeDef> = [&base, &object_type("rush-purchase-order.object-type.json")]
        .iter()
        .map(|value| serde_json::from_value((*value).clone()).expect("object type decodes"))
        .collect();
    let by_name: BTreeMap<&str, &ObjectTypeDef> =
        defs.iter().map(|def| (def.name.as_str(), def)).collect();
    let lookup = |name: &str| by_name.get(name).copied();
    assert_eq!(
        effective_lifecycles(&lookup, &[], "commerce.RushPurchaseOrder")
            [&norn_ir::PropertyId("status".to_owned())]
            .immutable,
        ["submitted"]
    );
}

#[test]
fn kernel_lifecycle_codes_equal_checker_codes() {
    assert_eq!(
        lifecycle_codes::UNDECLARED_WRITE,
        lifecycles::codes::UNDECLARED_WRITE
    );
    assert_eq!(
        lifecycle_codes::DIRECT_WRITE,
        lifecycle_writes::codes::DIRECT_WRITE
    );
    assert_eq!(
        lifecycle_codes::NOT_A_LIFECYCLE,
        lifecycle_writes::codes::NOT_A_LIFECYCLE
    );
    assert_eq!(
        lifecycle_codes::UNKNOWN_TRANSITION,
        lifecycle_writes::codes::UNKNOWN_TRANSITION
    );
    assert_eq!(
        FiringRefusal::NotFromState.code(),
        lifecycles::codes::UNDECLARED_WRITE
    );
    assert_eq!(
        FiringRefusal::UnknownTransition.code(),
        lifecycle_writes::codes::UNKNOWN_TRANSITION
    );
}

/// A lifecycle a subtype declares over a property an extension adds to its
/// supertype: the checker enforces it, so the shared helper must resolve it
/// (never drop it, which would let a commit write the state directly).
#[test]
fn lifecycle_over_an_extension_property_is_effective() {
    let base = json!({
        "name": "commerce.Ticket",
        "properties": [{ "id": "title", "type": { "kind": "text" } }]
    });
    let extension = json!({
        "namespace": "acme",
        "target": "commerce.Ticket",
        "properties": [{
            "id": "acme__stage",
            "type": { "kind": "enum", "values": [
                { "value": "open" }, { "value": "triaged" }, { "value": "closed" }
            ] },
            "default": "open"
        }]
    });
    let sub = json!({
        "name": "commerce.AcmeTicket",
        "extends": "commerce.Ticket",
        "lifecycles": [{
            "property": "acme__stage",
            "initial": ["open"],
            "terminal": ["closed"],
            "transitions": [
                { "id": "triage", "from": ["open"], "to": "triaged" },
                { "id": "close", "from": ["open", "triaged"], "to": "closed", "guards": ["self.title != \"\""] }
            ]
        }]
    });
    let mut artifacts = vec![envelope(&base), envelope(&sub)];
    artifacts.push(
        serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": "acme.commerce.Ticket.extension",
            "kind": "entity",
            "namespace": "acme",
            "name": "Ticket extension",
            "version": "0.1.0",
            "lifecycle": "draft",
            "scope": { "orgId": "org_demo", "environmentId": "dev" },
            "dependencies": [],
            "provenance": { "source": "authoring_sdk", "sourcePath": "tests/shared_lifecycle_semantics.rs" },
            "payload": { "entityVersion": "2.0", "objectTypeExtension": extension }
        }))
        .expect("extension envelope decodes"),
    );
    let defs: Vec<ObjectTypeDef> = [&base, &sub]
        .iter()
        .map(|value| serde_json::from_value((*value).clone()).expect("object type decodes"))
        .collect();
    let extension: ObjectTypeExtension =
        serde_json::from_value(extension).expect("extension decodes");
    let by_name: BTreeMap<&str, &ObjectTypeDef> =
        defs.iter().map(|def| (def.name.as_str(), def)).collect();
    let lookup = |name: &str| by_name.get(name).copied();

    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let checker = catalog
        .object_type("commerce.AcmeTicket.entity")
        .expect("defined")
        .unwrap_or_else(|finding| panic!("AcmeTicket unavailable: {finding:?}"));
    let expected: Vec<_> = checker
        .lifecycles
        .iter()
        .map(|effective| effective.lifecycle.clone())
        .collect();
    assert_eq!(
        expected.len(),
        1,
        "the checker enforces the extension lifecycle"
    );
    let shared: Vec<_> = effective_lifecycles(&lookup, &[&extension], "commerce.AcmeTicket")
        .into_values()
        .collect();
    assert_eq!(shared, expected);
    // Without the extension the machine does not resolve, but the property
    // is still known to carry a declared lifecycle, so enforcers fail closed.
    assert!(effective_lifecycles(&lookup, &[], "commerce.AcmeTicket").is_empty());
    assert!(
        declared_lifecycle_properties(&lookup, "commerce.AcmeTicket")
            .iter()
            .any(|property| property.as_str() == "acme__stage")
    );
}
