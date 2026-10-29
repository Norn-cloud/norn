#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! `EntityIR` v2 shared property types (T1-ONT-1a, #2511): one fixture per
//! kind, wire round-trip, the null-vs-omitted discipline, and the emitted
//! JSON Schema.

use std::collections::BTreeSet;

use norn_ir::{
    DecimalLiteral, JsonValue, PropertyConstraints, PropertyKind, PropertyType, PropertyTypeDef,
    TimezonePolicy, UnitSystem, property_type_def_schema,
};

macro_rules! fixture {
    ($kind:literal) => {
        (
            $kind,
            include_str!(concat!(
                "../../../fixtures/ir/entity-v2/property-types/",
                $kind,
                ".property-type.json"
            )),
        )
    };
}

const FIXTURES: [(&str, &str); 11] = [
    fixture!("text"),
    fixture!("integer"),
    fixture!("decimal"),
    fixture!("boolean"),
    fixture!("timestamp"),
    fixture!("date"),
    fixture!("duration"),
    fixture!("enum"),
    fixture!("json-ext"),
    fixture!("ref"),
    fixture!("record"),
];

fn canonical(source: &str) -> String {
    let value: JsonValue = serde_json::from_str(source).expect("fixture is JSON");
    serde_json::to_string(&value).expect("JSON encodes")
}

fn decode(value: &serde_json::Value) -> Result<PropertyTypeDef, serde_json::Error> {
    serde_json::from_str(&value.to_string())
}

#[test]
fn every_kind_has_a_fixture_that_round_trips() {
    let kinds: BTreeSet<&str> = PropertyKind::ALL.iter().map(|kind| kind.as_str()).collect();
    let covered: BTreeSet<&str> = FIXTURES.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(kinds, covered, "one fixture per property kind");

    for (kind, source) in FIXTURES {
        let def: PropertyTypeDef = serde_json::from_str(source)
            .unwrap_or_else(|error| panic!("{kind} fixture should decode: {error}"));
        assert_eq!(
            def.property_type.kind().as_str(),
            kind,
            "{kind} fixture kind"
        );
        let encoded = serde_json::to_string(&def).expect("definition encodes");
        assert_eq!(canonical(&encoded), canonical(source), "{kind} round-trip");
    }
}

#[test]
fn fixtures_carry_the_declared_members() {
    let decimal: PropertyTypeDef =
        serde_json::from_str(FIXTURES[2].1).expect("decimal fixture decodes");
    let PropertyType::Decimal { scale, unit, .. } = decimal.property_type else {
        panic!("decimal fixture is a decimal");
    };
    assert_eq!(scale.0, 3);
    let unit = unit.expect("decimal fixture declares a unit");
    assert_eq!((unit.system, unit.code.as_str()), (UnitSystem::Ucum, "kg"));
    assert_eq!(
        decimal.constraints.min,
        Some(DecimalLiteral("0.000".to_owned())),
        "bounds keep their literal scale"
    );

    let timestamp: PropertyTypeDef =
        serde_json::from_str(FIXTURES[4].1).expect("timestamp fixture decodes");
    assert_eq!(
        timestamp.property_type,
        PropertyType::Timestamp {
            timezone: TimezonePolicy::Utc
        }
    );
}

#[test]
fn optional_members_may_be_omitted_but_not_null() {
    let base = serde_json::json!({
        "name": "commerce.weight",
        "type": { "kind": "decimal", "scale": 2 }
    });
    decode(&base).expect("optional members omitted");

    let nulls = [
        ("/description", serde_json::json!({ "description": null })),
        ("/constraints", serde_json::json!({ "constraints": null })),
        (
            "/constraints/min",
            serde_json::json!({ "constraints": { "min": null } }),
        ),
        (
            "/constraints/maxLength",
            serde_json::json!({ "constraints": { "maxLength": null } }),
        ),
        (
            "/constraints/pattern",
            serde_json::json!({ "constraints": { "pattern": null } }),
        ),
        (
            "/constraints/nullable",
            serde_json::json!({ "constraints": { "nullable": null } }),
        ),
        (
            "/type/unit",
            serde_json::json!({ "type": { "kind": "decimal", "scale": 2, "unit": null } }),
        ),
        (
            "/type/format",
            serde_json::json!({ "type": { "kind": "text", "format": null } }),
        ),
        (
            "/type/schema",
            serde_json::json!({ "type": { "kind": "json-ext", "schema": null } }),
        ),
    ];
    for (member, patch) in nulls {
        let mut value = base.clone();
        for (key, member_value) in patch.as_object().expect("patch is an object") {
            value[key] = member_value.clone();
        }
        assert!(
            decode(&value).is_err(),
            "explicit null at {member} is rejected"
        );
    }
}

#[test]
fn closed_shapes_reject_unknown_members_and_kinds() {
    let rejected = [
        serde_json::json!({ "name": "a.b", "type": { "kind": "boolean" }, "extra": 1 }),
        serde_json::json!({ "name": "a.b", "type": { "kind": "boolean", "format": "email" } }),
        serde_json::json!({ "name": "a.b", "type": { "kind": "integer", "scale": 0 } }),
        serde_json::json!({ "name": "a.b", "type": { "kind": "float" } }),
        serde_json::json!({ "name": "a.b", "type": { "kind": "text" }, "constraints": { "regex": "x" } }),
        // A timestamp must name its timezone policy: no ambient timezone.
        serde_json::json!({ "name": "a.b", "type": { "kind": "timestamp" } }),
        serde_json::json!({ "name": "a.b", "type": { "kind": "timestamp", "timezone": "local" } }),
        // Bounds are exact decimal strings, never JSON numbers.
        serde_json::json!({ "name": "a.b", "type": { "kind": "integer" }, "constraints": { "min": 1 } }),
        serde_json::json!({ "name": "a.b", "type": { "kind": "decimal", "scale": 2, "unit": { "system": "si", "code": "kg" } } }),
        serde_json::json!({ "name": "a.b", "type": { "kind": "decimal", "scale": -1 } }),
        serde_json::json!({ "name": "a.b", "type": { "kind": "decimal", "scale": 1.5 } }),
        serde_json::json!({ "name": "a.b", "type": { "kind": "enum", "values": ["draft"] } }),
    ];
    for value in rejected {
        assert!(decode(&value).is_err(), "{value} is rejected");
    }
}

#[test]
fn canonical_form_omits_default_constraints() {
    let explicit = serde_json::json!({
        "name": "a.b",
        "type": { "kind": "boolean" },
        "constraints": { "nullable": false }
    });
    let def = decode(&explicit).expect("explicit default decodes");
    assert_eq!(def.constraints, PropertyConstraints::default());
    assert_eq!(
        serde_json::to_value(&def).expect("encodes"),
        serde_json::json!({ "name": "a.b", "type": { "kind": "boolean" } })
    );
}

#[test]
fn schema_publishes_every_kind_and_closed_members() {
    let schema = serde_json::to_value(property_type_def_schema()).expect("schema encodes");
    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("required list")
        .iter()
        .map(|value| value.as_str().expect("string"))
        .collect();
    assert_eq!(required, ["name", "type"]);
    assert_eq!(schema["additionalProperties"], false);

    let branches = schema["$defs"]["PropertyType"]["oneOf"]
        .as_array()
        .expect("PropertyType is a oneOf");
    let kinds: BTreeSet<&str> = branches
        .iter()
        .map(|branch| {
            branch["properties"]["kind"]["const"]
                .as_str()
                .expect("each branch pins its kind")
        })
        .collect();
    let expected: BTreeSet<&str> = PropertyKind::ALL.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(kinds, expected);
    for branch in branches {
        assert_eq!(branch["additionalProperties"], false, "{branch} is closed");
    }
    assert_eq!(
        schema["$defs"]["DecimalLiteral"]["type"], "string",
        "bounds are strings"
    );
}
