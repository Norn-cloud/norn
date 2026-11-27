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

use norn_checker::entity_scopes::ScopedView;
use norn_checker::{lifecycle_writes, lifecycles};
use norn_ir::{
    ArtifactEnvelope, FiringRefusal, InterfaceDef, ObjectTypeDef, ObjectTypeExtension,
    declared_lifecycle_properties, declared_lifecycle_properties_with_interfaces,
    effective_lifecycles, effective_lifecycles_with_interfaces, lifecycle_codes,
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
    let catalog =
        ScopedView::uniform(&artifacts, &std::collections::BTreeSet::new()).into_catalog();
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
    let catalog =
        ScopedView::uniform(&artifacts, &std::collections::BTreeSet::new()).into_catalog();
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

    let catalog =
        ScopedView::uniform(&artifacts, &std::collections::BTreeSet::new()).into_catalog();
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
    assert_eq!(
        effective_lifecycles(&lookup, &[], "commerce.AcmeTicket").len(),
        0
    );
    assert!(
        declared_lifecycle_properties(&lookup, "commerce.AcmeTicket")
            .iter()
            .any(|property| property.as_str() == "acme__stage")
    );
}

fn interface_envelope(interface: &Value) -> ArtifactEnvelope {
    let name = interface["name"].as_str().expect("name");
    serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": format!("{name}.interface"),
        "kind": "entity",
        "namespace": "commerce",
        "name": name,
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk", "sourcePath": "tests/shared_lifecycle_semantics.rs" },
        "payload": { "entityVersion": "2.0", "interface": interface }
    }))
    .expect("interface envelope decodes")
}

fn tracked_interface_fixtures() -> [serde_json::Value; 3] {
    let base_iface = json!({
        "name": "commerce.Tracked",
        "properties": [
            { "id": "code", "type": { "kind": "text" } },
            {
                "id": "status",
                "type": {
                    "kind": "enum",
                    "values": [
                        { "value": "draft" }, { "value": "active" },
                        { "value": "fulfilled" }, { "value": "cancelled" }
                    ]
                }
            }
        ],
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["fulfilled", "cancelled"],
            "immutable": ["fulfilled"],
            "endings": [
                { "state": "fulfilled", "outcome": "completed" },
                { "state": "cancelled", "outcome": "cancelled" }
            ],
            "pathProperties": [{ "target": "fulfilled", "via": "active" }],
            "transitions": [
                { "id": "activate", "from": ["draft"], "to": "active", "guards": ["self.code != \"\""] },
                { "id": "fulfill", "from": ["active"], "to": "fulfilled", "mode": "fact" },
                { "id": "cancel", "from": ["draft", "active"], "to": "cancelled" }
            ]
        }]
    });
    let left_iface = json!({
        "name": "commerce.LeftTracked",
        "extends": ["commerce.Tracked"],
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["fulfilled", "cancelled"],
            "transitions": [
                { "id": "activate", "from": ["draft"], "to": "active", "guards": ["self.code != \"\"", "len(self.code) > 1"] },
                { "id": "fulfill", "from": ["active"], "to": "fulfilled", "mode": "fact" },
                { "id": "cancel", "from": ["draft", "active"], "to": "cancelled" }
            ]
        }]
    });
    let right_iface = json!({
        "name": "commerce.RightTracked",
        "extends": ["commerce.Tracked"],
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["fulfilled", "cancelled"],
            "immutable": ["active"],
            "transitions": [
                { "id": "activate", "from": ["draft"], "to": "active", "guards": ["self.code != \"\"", "len(self.code) < 50"] },
                { "id": "fulfill", "from": ["active"], "to": "fulfilled", "mode": "fact" },
                { "id": "cancel", "from": ["draft", "active"], "to": "cancelled" }
            ]
        }]
    });
    [base_iface, left_iface, right_iface]
}

fn tracked_object_fixtures() -> [serde_json::Value; 4] {
    let direct_obj = json!({
        "name": "commerce.DirectCommitment",
        "implements": ["commerce.Tracked"],
        "properties": [
            { "id": "code", "type": { "kind": "text" } },
            {
                "id": "status",
                "type": {
                    "kind": "enum",
                    "values": [
                        { "value": "draft" }, { "value": "active" },
                        { "value": "fulfilled" }, { "value": "cancelled" }
                    ]
                },
                "default": "draft"
            }
        ]
    });
    let narrowed_enum_obj = json!({
        "name": "commerce.NoCancelCommitment",
        "implements": ["commerce.Tracked"],
        "properties": [
            { "id": "code", "type": { "kind": "text" } },
            {
                "id": "status",
                "type": {
                    "kind": "enum",
                    "values": [
                        { "value": "draft" }, { "value": "active" }, { "value": "fulfilled" }
                    ]
                },
                "default": "draft"
            }
        ]
    });
    let diamond_obj = json!({
        "name": "commerce.DiamondCommitment",
        "implements": ["commerce.LeftTracked", "commerce.RightTracked"],
        "properties": [
            { "id": "code", "type": { "kind": "text" } },
            {
                "id": "status",
                "type": {
                    "kind": "enum",
                    "values": [
                        { "value": "draft" }, { "value": "active" },
                        { "value": "fulfilled" }, { "value": "cancelled" }
                    ]
                },
                "default": "draft"
            }
        ]
    });
    let redeclared_sub = json!({
        "name": "commerce.SubCommitment",
        "extends": "commerce.DirectCommitment",
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["fulfilled", "cancelled"],
            "transitions": [
                { "id": "activate", "from": ["draft"], "to": "active", "guards": ["self.code != \"\"", "len(self.code) > 3"] },
                { "id": "fulfill", "from": ["active"], "to": "fulfilled", "mode": "fact" },
                { "id": "cancel", "from": ["draft", "active"], "to": "cancelled" }
            ]
        }]
    });
    [direct_obj, narrowed_enum_obj, diamond_obj, redeclared_sub]
}

fn assert_conflicting_interface_fails_closed<'a>(
    mut by_type: BTreeMap<&'a str, &'a ObjectTypeDef>,
    mut by_iface: BTreeMap<&'a str, &'a InterfaceDef>,
) {
    // Conflicting interfaces omit the machine from effective_lifecycles_with_interfaces
    // while keeping the property in declared_lifecycle_properties_with_interfaces (fail closed).
    let conflicting: InterfaceDef = serde_json::from_value(json!({
        "name": "commerce.Conflicting",
        "properties": [{
            "id": "status",
            "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "active" }, { "value": "fulfilled" }, { "value": "cancelled" }] }
        }],
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["fulfilled", "cancelled"],
            "transitions": [
                { "id": "activate", "from": ["draft"], "to": "cancelled" }
            ]
        }]
    }))
    .expect("conflicting decodes");
    let conflicted_obj: ObjectTypeDef = serde_json::from_value(json!({
        "name": "commerce.Conflicted",
        "implements": ["commerce.Tracked", "commerce.Conflicting"],
        "properties": [
            { "id": "code", "type": { "kind": "text" } },
            { "id": "status", "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "active" }, { "value": "fulfilled" }, { "value": "cancelled" }] } }
        ]
    }))
    .expect("conflicted obj decodes");
    by_type.insert(conflicted_obj.name.as_str(), &conflicted_obj);
    by_iface.insert(conflicting.name.as_str(), &conflicting);
    let lookup = |name: &str| by_type.get(name).copied();
    let lookup_iface = |name: &str| by_iface.get(name).copied();
    assert!(
        effective_lifecycles_with_interfaces(&lookup, &lookup_iface, &[], "commerce.Conflicted")
            .is_empty()
    );
    assert!(
        declared_lifecycle_properties_with_interfaces(
            &lookup,
            &lookup_iface,
            "commerce.Conflicted"
        )
        .iter()
        .any(|p| p.as_str() == "status")
    );
}

#[test]
fn interface_effective_lifecycles_equal_checker_machines() {
    let ifaces = tracked_interface_fixtures();
    let objs = tracked_object_fixtures();
    let mut artifacts: Vec<ArtifactEnvelope> = ifaces.iter().map(interface_envelope).collect();
    artifacts.extend(objs.iter().map(envelope));

    let iface_defs: Vec<InterfaceDef> = ifaces
        .iter()
        .map(|v| serde_json::from_value(v.clone()).expect("iface decodes"))
        .collect();
    let obj_defs: Vec<ObjectTypeDef> = objs
        .iter()
        .map(|v| serde_json::from_value(v.clone()).expect("obj decodes"))
        .collect();
    let by_type: BTreeMap<&str, &ObjectTypeDef> =
        obj_defs.iter().map(|d| (d.name.as_str(), d)).collect();
    let by_iface: BTreeMap<&str, &InterfaceDef> =
        iface_defs.iter().map(|d| (d.name.as_str(), d)).collect();
    let lookup = |name: &str| by_type.get(name).copied();
    let lookup_iface = |name: &str| by_iface.get(name).copied();

    let catalog =
        ScopedView::uniform(&artifacts, &std::collections::BTreeSet::new()).into_catalog();
    for def in &obj_defs {
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
        let shared: Vec<_> =
            effective_lifecycles_with_interfaces(&lookup, &lookup_iface, &[], name)
                .into_values()
                .collect();
        assert_eq!(shared, expected, "{name}");
        assert!(
            declared_lifecycle_properties_with_interfaces(&lookup, &lookup_iface, name)
                .iter()
                .any(|p| p.as_str() == "status"),
            "{name} declares status lifecycle via interface"
        );
    }

    // Verify diamond meet conjoined guards and immutable states from both branches.
    let diamond = effective_lifecycles_with_interfaces(
        &lookup,
        &lookup_iface,
        &[],
        "commerce.DiamondCommitment",
    );
    let status = &diamond[&norn_ir::PropertyId("status".to_owned())];
    assert_eq!(status.immutable, ["active", "fulfilled"]);
    let activate = status
        .transitions
        .iter()
        .find(|t| t.id.as_str() == "activate")
        .expect("activate");
    assert_eq!(
        activate
            .guards
            .iter()
            .map(norn_ir::ZenExpression::as_str)
            .collect::<Vec<_>>(),
        [
            "self.code != \"\"",
            "len(self.code) > 1",
            "len(self.code) < 50"
        ]
    );
    assert_conflicting_interface_fails_closed(by_type, by_iface);
}
