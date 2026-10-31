#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! `EntityIR` v2 object types (T1-ONT-1b, #2512): identity (single and
//! composite keys, identity rules), defaults, and computed properties. Wire
//! round-trip of the fixtures, the null-vs-omitted discipline, closed shapes,
//! and the emitted JSON Schema.

use norn_ir::{
    DefaultValue, JsonValue, KeySpec, Normalization, ObjectTypeDef, PropertyId,
    object_type_def_schema,
};

macro_rules! fixture {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!(
                "../../../fixtures/ir/entity-v2/object-types/",
                $name,
                ".object-type.json"
            )),
        )
    };
}

const FIXTURES: [(&str, &str); 3] = [
    fixture!("sku"),
    fixture!("sales-order-line"),
    fixture!("party"),
];

fn canonical(source: &str) -> String {
    let value: JsonValue = serde_json::from_str(source).expect("fixture is JSON");
    serde_json::to_string(&value).expect("JSON encodes")
}

fn decode(value: &serde_json::Value) -> Result<ObjectTypeDef, serde_json::Error> {
    serde_json::from_str(&value.to_string())
}

fn fixture(name: &str) -> ObjectTypeDef {
    let (_, source) = FIXTURES
        .iter()
        .find(|(fixture, _)| *fixture == name)
        .expect("fixture exists");
    serde_json::from_str(source).expect("fixture decodes")
}

fn ids(key: &KeySpec) -> Vec<&str> {
    key.properties.iter().map(PropertyId::as_str).collect()
}

#[test]
fn every_fixture_round_trips() {
    for (name, source) in FIXTURES {
        let def: ObjectTypeDef = serde_json::from_str(source)
            .unwrap_or_else(|error| panic!("{name} fixture should decode: {error}"));
        let encoded = serde_json::to_string(&def).expect("definition encodes");
        assert_eq!(canonical(&encoded), canonical(source), "{name} round-trip");
    }
}

#[test]
fn fixtures_cover_single_composite_and_ruled_keys() {
    let sku = fixture("sku");
    assert_eq!(ids(&sku.identity[0]), ["sku_code"], "single key");
    let rule = sku.identity[0].rule.as_ref().expect("sku key has a rule");
    assert_eq!(
        rule.normalize[0].steps,
        [Normalization::Trim, Normalization::Uppercase]
    );

    let line = fixture("sales-order-line");
    assert_eq!(
        ids(&line.identity[0]),
        ["order_number", "line_number"],
        "composite key keeps its declared order"
    );

    let party = fixture("party");
    assert_eq!(party.identity.len(), 2, "two keys");
    assert_eq!(
        party.identity[1]
            .rule
            .as_ref()
            .and_then(|rule| rule.match_policy.as_ref())
            .map(norn_ir::ResolutionPolicyName::as_str),
        Some("crm.party_email_exact")
    );
}

#[test]
fn defaults_are_literals_and_computed_properties_are_read_only() {
    let sku = fixture("sku");
    let property = |id: &str| {
        sku.properties
            .iter()
            .find(|property| property.id.as_str() == id)
            .expect("property exists")
    };
    assert_eq!(
        property("net_weight").default,
        Some(DefaultValue(JsonValue::String("0.000".to_owned()))),
        "decimal defaults are exact strings that keep their scale"
    );
    assert_eq!(
        property("is_discontinued").default,
        Some(DefaultValue(JsonValue::Bool(false)))
    );
    assert!(property("reorder_weight").is_read_only());
    assert!(!property("net_weight").is_read_only());
}

#[test]
fn optional_members_may_be_omitted_but_not_null() {
    let base = serde_json::json!({
        "name": "retail.Sku",
        "properties": [{ "id": "code", "type": { "kind": "text" } }],
        "identity": [{ "name": "primary", "properties": ["code"] }]
    });
    decode(&base).expect("optional members omitted");
    decode(&serde_json::json!({ "name": "retail.Marker" })).expect("no properties, no keys");

    let nulls = [
        serde_json::json!({ "name": "retail.Sku", "description": null }),
        serde_json::json!({ "name": "retail.Sku", "properties": null }),
        serde_json::json!({ "name": "retail.Sku", "identity": null }),
        serde_json::json!({ "name": "retail.Sku",
            "properties": [{ "id": "code", "type": { "kind": "text" }, "description": null }] }),
        serde_json::json!({ "name": "retail.Sku",
            "properties": [{ "id": "code", "type": { "kind": "text" }, "constraints": null }] }),
        serde_json::json!({ "name": "retail.Sku",
            "properties": [{ "id": "code", "type": { "kind": "text" }, "default": null }] }),
        serde_json::json!({ "name": "retail.Sku",
            "properties": [{ "id": "code", "type": { "kind": "text" }, "computed": null }] }),
        serde_json::json!({ "name": "retail.Sku",
            "identity": [{ "name": "primary", "properties": ["code"], "namingSeries": null }] }),
        serde_json::json!({ "name": "retail.Sku",
            "identity": [{ "name": "primary", "properties": ["code"], "rule": null }] }),
        serde_json::json!({ "name": "retail.Sku",
            "identity": [{ "name": "primary", "properties": ["code"], "rule": { "matchPolicy": null } }] }),
        serde_json::json!({ "name": "retail.Sku",
            "identity": [{ "name": "primary", "properties": ["code"], "rule": { "normalize": null } }] }),
    ];
    for value in nulls {
        assert!(
            decode(&value).is_err(),
            "explicit null in {value} is rejected"
        );
    }
}

#[test]
fn naming_series_key_round_trips_without_nullable_wire_members() {
    let plain = serde_json::json!({ "name": "primary", "properties": ["code"] });
    let key: KeySpec = serde_json::from_value(plain.clone()).expect("omitted series");
    assert_eq!(serde_json::to_value(key).expect("plain key"), plain);
    let numbered = serde_json::json!({
        "name": "primary", "properties": ["code"],
        "namingSeries": {
            "pattern": "SKU-{#####}", "timezone": "UTC",
            "resetPolicy": "never", "gapPolicy": "gapless_within_commit"
        }
    });
    let key: KeySpec = serde_json::from_value(numbered.clone()).expect("declared series");
    assert_eq!(serde_json::to_value(key).expect("numbered key"), numbered);
}

#[test]
fn closed_shapes_reject_unknown_members() {
    let rejected = [
        serde_json::json!({ "name": "retail.Sku", "keys": [] }),
        serde_json::json!({ "name": "retail.Sku",
            "properties": [{ "id": "code", "type": { "kind": "text" }, "readOnly": true }] }),
        serde_json::json!({ "name": "retail.Sku",
            "identity": [{ "name": "primary", "properties": ["code"], "unique": true }] }),
        // A key lists property ids, not objects.
        serde_json::json!({ "name": "retail.Sku",
            "identity": [{ "name": "primary", "properties": [{ "property": "code" }] }] }),
        serde_json::json!({ "name": "retail.Sku",
            "identity": [{ "name": "primary", "properties": ["code"],
                "rule": { "normalize": [{ "property": "code", "steps": ["soundex"] }] } }] }),
        serde_json::json!({ "name": "retail.Sku",
            "identity": [{ "name": "primary", "properties": ["code"],
                "rule": { "normalize": [{ "property": "code", "steps": ["trim"], "locale": "tr" }] } }] }),
        // `properties` is required on a key.
        serde_json::json!({ "name": "retail.Sku", "identity": [{ "name": "primary" }] }),
    ];
    for value in rejected {
        assert!(decode(&value).is_err(), "{value} is rejected");
    }
}

#[test]
fn canonical_form_omits_empty_members() {
    let explicit = serde_json::json!({
        "name": "retail.Sku",
        "properties": [{ "id": "code", "type": { "kind": "text" }, "constraints": {} }],
        "identity": [{ "name": "primary", "properties": ["code"], "rule": { "normalize": [], "matchPolicy": "a.b" } }]
    });
    let def = decode(&explicit).expect("decodes");
    assert_eq!(
        serde_json::to_value(&def).expect("encodes"),
        serde_json::json!({
            "name": "retail.Sku",
            "properties": [{ "id": "code", "type": { "kind": "text" } }],
            "identity": [{ "name": "primary", "properties": ["code"], "rule": { "matchPolicy": "a.b" } }]
        })
    );
    let empty =
        decode(&serde_json::json!({ "name": "retail.Marker", "properties": [], "identity": [] }))
            .expect("decodes");
    assert_eq!(
        serde_json::to_value(&empty).expect("encodes"),
        serde_json::json!({ "name": "retail.Marker" })
    );
}

#[test]
fn a_numeric_default_decodes_the_same_under_every_number_profile() {
    // The checker rejects JSON-number defaults for integer and decimal
    // properties; decoding them must still be profile-independent.
    let def = decode(&serde_json::json!({
        "name": "retail.Sku",
        "properties": [{ "id": "extra", "type": { "kind": "json-ext" }, "default": { "n": 9_007_199_254_740_993_u64 } }]
    }))
    .expect("decodes");
    let default = def.properties[0].default.as_ref().expect("default");
    let JsonValue::Object(members) = &default.0 else {
        panic!("default is an object");
    };
    assert!(matches!(members.get("n"), Some(JsonValue::Number(_))));
}

#[test]
fn schema_publishes_closed_members() {
    let schema = serde_json::to_value(object_type_def_schema()).expect("schema encodes");
    assert_eq!(schema["required"], serde_json::json!(["name"]));
    assert_eq!(schema["additionalProperties"], false);
    for definition in [
        "Property",
        "KeySpec",
        "IdentityRule",
        "ComponentNormalization",
    ] {
        assert_eq!(
            schema["$defs"][definition]["additionalProperties"], false,
            "{definition} is closed"
        );
    }
    assert_eq!(
        schema["$defs"]["KeySpec"]["required"],
        serde_json::json!(["name", "properties"])
    );
    assert_eq!(
        schema["$defs"]["Property"]["required"],
        serde_json::json!(["id", "type"])
    );
}

#[test]
fn omitted_write_authority_flags_preserve_wire_defaults() {
    let wire = serde_json::json!({"name":"test.Item","properties":[{"id":"count","type":{"kind":"integer"}}]});
    let object: ObjectTypeDef = serde_json::from_value(wire.clone()).expect("old definition");
    assert!(!object.immutable);
    assert!(!object.properties[0].protected);
    assert_eq!(serde_json::to_value(&object).expect("unchanged wire"), wire);
}
