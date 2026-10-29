#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! `EntityIR` v2 inheritance, interfaces, status, and namespaces (T1-ONT-1c,
//! #2513): wire round-trip of the fixtures, the null-vs-omitted discipline,
//! closed shapes, the canonical status form, and the emitted JSON Schemas.

use norn_ir::{
    InterfaceDef, JsonValue, ObjectTypeDef, ObjectTypeExtension, ObjectTypeName, ResourceStatus,
    interface_def_schema, object_type_def_schema, object_type_extension_schema,
};

macro_rules! fixture {
    ($dir:literal, $name:literal) => {
        (
            $name,
            include_str!(concat!("../../../fixtures/ir/entity-v2/", $dir, "/", $name)),
        )
    };
}

const OBJECT_TYPES: [(&str, &str); 4] = [
    fixture!("object-types", "customer.object-type.json"),
    fixture!("object-types", "employee.object-type.json"),
    fixture!("object-types", "key-account.object-type.json"),
    fixture!("object-types", "acme-wholesale-customer.object-type.json"),
];
const INTERFACES: [(&str, &str); 1] = [fixture!("interfaces", "party.interface.json")];
const EXTENSIONS: [(&str, &str); 1] = [fixture!("extensions", "acme-customer.extension.json")];

fn canonical(source: &str) -> String {
    let value: JsonValue = serde_json::from_str(source).expect("fixture is JSON");
    serde_json::to_string(&value).expect("JSON encodes")
}

fn round_trips<T>(fixtures: &[(&str, &str)])
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    for (name, source) in fixtures {
        let def: T = serde_json::from_str(source)
            .unwrap_or_else(|error| panic!("{name} should decode: {error}"));
        let encoded = serde_json::to_string(&def).expect("definition encodes");
        assert_eq!(canonical(&encoded), canonical(source), "{name} round-trip");
    }
}

fn decodes<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> bool {
    serde_json::from_str::<T>(&value.to_string()).is_ok()
}

#[test]
fn every_fixture_round_trips() {
    round_trips::<ObjectTypeDef>(&OBJECT_TYPES);
    round_trips::<InterfaceDef>(&INTERFACES);
    round_trips::<ObjectTypeExtension>(&EXTENSIONS);
}

#[test]
fn fixtures_cover_inheritance_interfaces_status_and_extensions() {
    let decode = |index: usize| -> ObjectTypeDef {
        serde_json::from_str(OBJECT_TYPES[index].1).expect("decodes")
    };
    let (customer, employee, key_account, wholesale) = (decode(0), decode(1), decode(2), decode(3));
    for implementor in [&customer, &employee] {
        assert_eq!(implementor.implements[0].as_str(), "party.Party");
    }
    assert_eq!(
        key_account.extends.as_ref().map(ObjectTypeName::as_str),
        Some("crm.Customer")
    );
    assert!(matches!(
        &key_account.status,
        ResourceStatus::Deprecated { replaced_by: Some(name), .. } if name.as_str() == "crm.Customer"
    ));
    assert!(matches!(wholesale.status, ResourceStatus::Experimental {}));
    assert!(customer.status.is_active(), "omitted status is active");

    // The name's namespace is the domain namespace.
    assert_eq!(customer.name.domain(), Some("crm"));
    assert_eq!(customer.name.local_name(), "Customer");
    assert_eq!(wholesale.name.domain(), Some("acme"));

    let extension: ObjectTypeExtension = serde_json::from_str(EXTENSIONS[0].1).expect("decodes");
    assert_eq!(extension.namespace.as_str(), "acme");
    assert!(
        extension
            .properties
            .iter()
            .all(|property| property.id.as_str().starts_with("acme__"))
    );
}

#[test]
fn optional_members_may_be_omitted_but_not_null() {
    let nulls = [
        serde_json::json!({ "name": "retail.Sku", "status": null }),
        serde_json::json!({ "name": "retail.Sku", "extends": null }),
        serde_json::json!({ "name": "retail.Sku", "implements": null }),
        serde_json::json!({ "name": "retail.Sku",
            "status": { "state": "deprecated", "since": "2026-09-30", "replacedBy": null } }),
    ];
    for value in &nulls {
        assert!(!decodes::<ObjectTypeDef>(value), "{value} is rejected");
    }
    assert!(!decodes::<InterfaceDef>(
        &serde_json::json!({ "name": "party.Party", "status": null })
    ));
    assert!(!decodes::<InterfaceDef>(
        &serde_json::json!({ "name": "party.Party", "properties": null })
    ));
    assert!(!decodes::<ObjectTypeExtension>(
        &serde_json::json!({ "namespace": "acme", "target": "crm.Customer", "properties": null })
    ));
    assert!(!decodes::<ObjectTypeExtension>(
        &serde_json::json!({ "namespace": "acme", "target": "crm.Customer", "description": null })
    ));
    assert!(decodes::<ObjectTypeExtension>(
        &serde_json::json!({ "namespace": "acme", "target": "crm.Customer" })
    ));
}

#[test]
fn closed_shapes_reject_unknown_members() {
    let rejected_types = [
        // One supertype, never a list.
        serde_json::json!({ "name": "retail.Sku", "extends": ["a.B"] }),
        serde_json::json!({ "name": "retail.Sku", "domain": "retail" }),
        serde_json::json!({ "name": "retail.Sku", "status": "active" }),
        serde_json::json!({ "name": "retail.Sku", "status": { "state": "retired" } }),
        serde_json::json!({ "name": "retail.Sku", "status": { "state": "active", "since": "2026-09-30" } }),
        // `since` is required when deprecated.
        serde_json::json!({ "name": "retail.Sku", "status": { "state": "deprecated" } }),
    ];
    for value in &rejected_types {
        assert!(!decodes::<ObjectTypeDef>(value), "{value} is rejected");
    }
    // A requirement has no default and is never computed.
    for member in [
        serde_json::json!({ "default": "x" }),
        serde_json::json!({ "computed": "x" }),
    ] {
        let mut property = serde_json::json!({ "id": "name", "type": { "kind": "text" } });
        for (key, value) in member.as_object().expect("object") {
            property[key] = value.clone();
        }
        let value = serde_json::json!({ "name": "party.Party", "properties": [property] });
        assert!(!decodes::<InterfaceDef>(&value), "{value} is rejected");
    }
    // An extension cannot declare keys or a supertype.
    for member in ["identity", "extends"] {
        let mut value = serde_json::json!({ "namespace": "acme", "target": "crm.Customer" });
        value[member] = serde_json::json!([]);
        assert!(
            !decodes::<ObjectTypeExtension>(&value),
            "{value} is rejected"
        );
    }
}

#[test]
fn canonical_form_omits_active_status_and_empty_lists() {
    let explicit = serde_json::json!({
        "name": "retail.Sku", "status": { "state": "active" }, "implements": []
    });
    let def: ObjectTypeDef = serde_json::from_value(explicit).expect("decodes");
    assert_eq!(
        serde_json::to_value(&def).expect("encodes"),
        serde_json::json!({ "name": "retail.Sku" })
    );
    let interface: InterfaceDef =
        serde_json::from_value(serde_json::json!({ "name": "party.Party", "properties": [] }))
            .expect("decodes");
    assert_eq!(
        serde_json::to_value(&interface).expect("encodes"),
        serde_json::json!({ "name": "party.Party" })
    );
}

#[test]
fn legacy_interfaces_decode_unchanged_and_empty_invariants_are_omitted() {
    let legacy = serde_json::json!({
        "name": "party.Party",
        "properties": [{ "id": "name", "type": { "kind": "text" } }]
    });
    let interface: InterfaceDef = serde_json::from_value(legacy.clone()).expect("decodes");
    assert!(interface.invariants.is_empty());
    assert_eq!(serde_json::to_value(&interface).expect("encodes"), legacy);
}

#[test]
fn schemas_publish_closed_members() {
    let object = serde_json::to_value(object_type_def_schema()).expect("schema encodes");
    assert_eq!(object["required"], serde_json::json!(["name"]));
    for member in ["status", "extends", "implements"] {
        assert!(
            object["properties"][member].is_object(),
            "{member} is published"
        );
    }
    let interface = serde_json::to_value(interface_def_schema()).expect("schema encodes");
    assert_eq!(interface["additionalProperties"], false);
    assert_eq!(interface["required"], serde_json::json!(["name"]));
    assert!(interface["properties"]["invariants"].is_object());
    assert_eq!(
        interface["$defs"]["InterfaceProperty"]["additionalProperties"],
        false
    );
    let extension = serde_json::to_value(object_type_extension_schema()).expect("schema encodes");
    assert_eq!(extension["additionalProperties"], false);
    assert_eq!(
        extension["required"],
        serde_json::json!(["namespace", "target"])
    );
    assert_eq!(
        extension["$defs"]["ExtensionNamespace"]["pattern"],
        norn_ir::EXTENSION_NAMESPACE_PATTERN
    );
}
