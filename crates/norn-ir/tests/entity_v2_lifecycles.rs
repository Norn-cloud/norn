#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! `EntityIR` v2 declared lifecycles (T3-LIFE-1, #2672): wire round-trip of
//! the positive fixtures, the invalid-transition fixtures decode (their
//! faults are `norn-checker` diagnostics, not decode errors), the
//! null-vs-omitted discipline, closed shapes, and the emitted JSON Schema.
//! Every test name contains `lifecycle` (`cargo test -p norn-ir lifecycle`).

use norn_ir::{JsonValue, ObjectTypeDef, ObjectTypeExtension, object_type_def_schema};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/ir/entity-v2");

const POSITIVE: [&str; 2] = [
    "object-types/purchase-order.object-type.json",
    "object-types/rush-purchase-order.object-type.json",
];

fn read(path: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{path}"))
        .unwrap_or_else(|error| panic!("{path} should be readable: {error}"))
}

fn canonical(source: &str) -> String {
    let value: JsonValue = serde_json::from_str(source).expect("fixture is JSON");
    serde_json::to_string(&value).expect("JSON encodes")
}

fn decodes<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> bool {
    serde_json::from_str::<T>(&value.to_string()).is_ok()
}

#[test]
fn lifecycle_fixtures_round_trip() {
    for path in POSITIVE {
        let source = read(path);
        let def: ObjectTypeDef = serde_json::from_str(&source)
            .unwrap_or_else(|error| panic!("{path} should decode: {error}"));
        let encoded = serde_json::to_string(&def).expect("definition encodes");
        assert_eq!(canonical(&encoded), canonical(&source), "{path} round-trip");
    }
    let order: ObjectTypeDef = serde_json::from_str(&read(POSITIVE[0])).expect("decodes");
    let [status, payment] = order.lifecycles.as_slice() else {
        panic!("two lifecycles");
    };
    assert_eq!(
        (status.property.as_str(), payment.property.as_str()),
        ("status", "payment_status")
    );
    assert!(
        status.allows("approved", "cancelled"),
        "cancel leaves three states"
    );
    assert!(status.allows("rejected", "draft"));
    assert!(!status.allows("draft", "approved"), "no skipped step");
    assert!(
        !status.allows("received", "draft"),
        "terminal states are absorbing"
    );
    assert_eq!(status.transitions[0].guards.len(), 1);
}

#[test]
fn invalid_lifecycle_fixtures_decode() {
    let manifest: serde_json::Value =
        serde_json::from_str(&read("lifecycles/invalid/manifest.json")).expect("manifest");
    let fixtures = manifest["fixtures"].as_array().expect("fixture list");
    assert!(!fixtures.is_empty());
    for fixture in fixtures {
        let file = fixture["file"].as_str().expect("file");
        let document: serde_json::Value =
            serde_json::from_str(&read(&format!("lifecycles/invalid/{file}"))).expect("JSON");
        let types: Vec<ObjectTypeDef> = serde_json::from_value(document["objectTypes"].clone())
            .unwrap_or_else(|error| panic!("{file} should decode: {error}"));
        assert!(!types.is_empty(), "{file}");
    }
}

#[test]
fn lifecycle_optional_members_may_be_omitted_but_not_null() {
    let with = |lifecycle: serde_json::Value| serde_json::json!({ "name": "test.Thing", "lifecycles": [lifecycle] });
    let minimal = serde_json::json!({ "property": "stage", "initial": ["open"] });
    assert!(
        decodes::<ObjectTypeDef>(&with(minimal)),
        "terminal and transitions may be omitted"
    );
    let transition = |extra: serde_json::Value| {
        let mut transition = serde_json::json!({ "id": "close", "from": ["open"], "to": "closed" });
        transition
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        with(
            serde_json::json!({ "property": "stage", "initial": ["open"], "transitions": [transition] }),
        )
    };
    assert!(decodes::<ObjectTypeDef>(&transition(serde_json::json!({}))));
    let rejected = [
        serde_json::json!({ "name": "test.Thing", "lifecycles": null }),
        with(serde_json::json!({ "property": "stage", "initial": ["open"], "description": null })),
        with(serde_json::json!({ "property": "stage", "initial": ["open"], "terminal": null })),
        with(serde_json::json!({ "property": "stage", "initial": ["open"], "transitions": null })),
        // `initial` is required; `property` is required.
        with(serde_json::json!({ "property": "stage" })),
        with(serde_json::json!({ "initial": ["open"] })),
        // Unknown members, including a `states` list: the states are the
        // property's enum values.
        with(serde_json::json!({ "property": "stage", "initial": ["open"], "states": ["open"] })),
        transition(serde_json::json!({ "guards": null })),
        // `guards` is a list; the single-guard form is not the wire.
        transition(serde_json::json!({ "guard": "true" })),
        transition(serde_json::json!({ "description": null })),
        transition(serde_json::json!({ "action": "close_order" })),
        // `from` is a list.
        with(
            serde_json::json!({ "property": "stage", "initial": ["open"],
            "transitions": [{ "id": "close", "from": "open", "to": "closed" }] }),
        ),
    ];
    for value in &rejected {
        assert!(!decodes::<ObjectTypeDef>(value), "{value} is rejected");
    }
}

#[test]
fn an_extension_cannot_carry_a_lifecycle() {
    let extension = serde_json::json!({
        "namespace": "acme",
        "target": "commerce.PurchaseOrder",
        "lifecycles": [{ "property": "status", "initial": ["draft"] }]
    });
    assert!(
        !decodes::<ObjectTypeExtension>(&extension),
        "a tenant never widens a base lifecycle through an extension"
    );
}

#[test]
fn lifecycle_schema_publishes_closed_members() {
    let schema = serde_json::to_value(object_type_def_schema()).expect("schema encodes");
    assert!(schema["properties"]["lifecycles"].is_object());
    for definition in ["Lifecycle", "Transition"] {
        assert_eq!(
            schema["$defs"][definition]["additionalProperties"], false,
            "{definition} is closed"
        );
    }
    assert_eq!(
        schema["$defs"]["Lifecycle"]["required"],
        serde_json::json!(["property", "initial"])
    );
    assert_eq!(
        schema["$defs"]["Transition"]["required"],
        serde_json::json!(["id", "from", "to"])
    );
}
