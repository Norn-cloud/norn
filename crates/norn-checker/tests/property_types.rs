#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! `check_property_type_defs` (T1-ONT-1a, #2511): the per-kind fixtures form
//! a valid catalog, and every diagnostic code has a negative case.

use std::collections::BTreeSet;

use norn_checker::{Diagnostic, check_property_type_defs, property_types::codes};
use norn_ir::{PropertyTypeDef, PropertyTypeName};

macro_rules! fixture {
    ($kind:literal) => {
        include_str!(concat!(
            "../../../fixtures/ir/entity-v2/property-types/",
            $kind,
            ".property-type.json"
        ))
    };
}

const FIXTURES: [&str; 11] = [
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

fn registry() -> BTreeSet<PropertyTypeName> {
    BTreeSet::from([PropertyTypeName("iso4217.currency_code".to_owned())])
}

fn defs(value: &serde_json::Value) -> Vec<PropertyTypeDef> {
    serde_json::from_str(&value.to_string()).expect("definitions decode")
}

fn findings(value: &serde_json::Value) -> Vec<(String, String)> {
    check_property_type_defs(&defs(value), &registry())
        .into_iter()
        .map(|Diagnostic { code, span, .. }| {
            (
                code.as_str().to_owned(),
                span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn the_fixture_catalog_is_valid() {
    let catalog: Vec<PropertyTypeDef> = FIXTURES
        .iter()
        .map(|source| serde_json::from_str(source).expect("fixture decodes"))
        .collect();
    assert_eq!(check_property_type_defs(&catalog, &registry()), Vec::new());
}

#[test]
fn a_ref_to_a_local_definition_inherits_its_kind_for_constraints() {
    let valid = serde_json::json!([
        { "name": "commerce.money", "type": { "kind": "decimal", "scale": 2 } },
        { "name": "commerce.unit_price", "type": { "kind": "ref", "name": "commerce.money" },
          "constraints": { "min": "0.01", "max": "99999.99" } },
        { "name": "commerce.list_price", "type": { "kind": "ref", "name": "commerce.unit_price" },
          "constraints": { "min": "0" } }
    ]);
    assert_eq!(findings(&valid), Vec::new());

    let invalid = serde_json::json!([
        { "name": "commerce.money", "type": { "kind": "decimal", "scale": 2 } },
        { "name": "commerce.unit_price", "type": { "kind": "ref", "name": "commerce.money" },
          "constraints": { "min": "0.001", "pattern": "^x$" } }
    ]);
    assert_eq!(
        findings(&invalid),
        vec![
            (
                codes::BOUND_EXCEEDS_SCALE.to_owned(),
                "/1/constraints/min".to_owned()
            ),
            (
                codes::CONSTRAINT_NOT_APPLICABLE.to_owned(),
                "/1/constraints/pattern".to_owned()
            ),
        ]
    );
}

#[test]
fn constraints_on_a_registry_ref_are_left_to_the_registry() {
    let value = serde_json::json!([
        { "name": "commerce.price_currency", "type": { "kind": "ref", "name": "iso4217.currency_code" },
          "constraints": { "nullable": true, "pattern": "^[A-Z]{3}$" } }
    ]);
    assert_eq!(findings(&value), Vec::new());
}

#[test]
fn every_code_has_a_negative_case() {
    let cases = [
        (
            codes::INVALID_NAME,
            "/0/name",
            serde_json::json!([{ "name": "Commerce.SKU", "type": { "kind": "text" } }]),
        ),
        (
            codes::DUPLICATE_NAME,
            "/1/name",
            serde_json::json!([
                { "name": "a.b", "type": { "kind": "text" } },
                { "name": "a.b", "type": { "kind": "integer" } }
            ]),
        ),
        (
            codes::DUPLICATE_NAME,
            "/0/name",
            serde_json::json!([{ "name": "iso4217.currency_code", "type": { "kind": "text" } }]),
        ),
        (
            codes::UNKNOWN_REF,
            "/0/type/name",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "ref", "name": "a.missing" } }]),
        ),
        (
            codes::SCALE_OUT_OF_RANGE,
            "/0/type/scale",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "decimal", "scale": 29 } }]),
        ),
        (
            codes::INVALID_UNIT,
            "/0/type/unit/code",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "decimal", "scale": 2,
                "unit": { "system": "ucum", "code": "k g" } } }]),
        ),
        (
            codes::EMPTY_ENUM,
            "/0/type/values",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "enum", "values": [] } }]),
        ),
        (
            codes::INVALID_ENUM_VALUE,
            "/0/type/values/0/value",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "enum", "values": [{ "value": " " }] } }]),
        ),
        (
            codes::DUPLICATE_ENUM_VALUE,
            "/0/type/values/1/value",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "enum",
                "values": [{ "value": "open" }, { "value": "open" }] } }]),
        ),
        (
            codes::CONSTRAINT_NOT_APPLICABLE,
            "/0/constraints/min",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "text" }, "constraints": { "min": "1" } }]),
        ),
        (
            codes::CONSTRAINT_NOT_APPLICABLE,
            "/0/constraints/maxLength",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "enum", "values": [{ "value": "x" }] },
                "constraints": { "maxLength": 3 } }]),
        ),
        (
            codes::INVALID_BOUND,
            "/0/constraints/max",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "decimal", "scale": 2 },
                "constraints": { "max": "1e3" } }]),
        ),
        (
            codes::BOUND_NOT_INTEGER,
            "/0/constraints/min",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "integer" }, "constraints": { "min": "1.0" } }]),
        ),
        (
            codes::BOUND_NOT_INTEGER,
            "/0/constraints/max",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "integer" },
                "constraints": { "max": "9223372036854775808" } }]),
        ),
        (
            codes::BOUND_EXCEEDS_SCALE,
            "/0/constraints/max",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "decimal", "scale": 2 },
                "constraints": { "max": "10.005" } }]),
        ),
        (
            // Trailing zeros carry the literal's scale, so they count.
            codes::BOUND_EXCEEDS_SCALE,
            "/0/constraints/min",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "decimal", "scale": 2 },
                "constraints": { "min": "1.000" } }]),
        ),
        (
            // 2^96: one past the engine's largest scale-0 decimal.
            codes::BOUND_OUT_OF_RANGE,
            "/0/constraints/max",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "decimal", "scale": 0 },
                "constraints": { "max": "79228162514264337593543950336" } }]),
        ),
        (
            codes::BOUND_OUT_OF_RANGE,
            "/0/constraints/min",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "decimal", "scale": 0 },
                "constraints": { "min": "-79228162514264337593543950336" } }]),
        ),
        (
            // Within scale 28, but the mantissa exceeds 96 bits.
            codes::BOUND_OUT_OF_RANGE,
            "/0/constraints/max",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "decimal", "scale": 28 },
                "constraints": { "max": "7.9228162514264337593543950336" } }]),
        ),
        (
            codes::INVERTED_RANGE,
            "/0/constraints/min",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "decimal", "scale": 2 },
                "constraints": { "min": "10.5", "max": "10.49" } }]),
        ),
        (
            codes::INVERTED_RANGE,
            "/0/constraints/minLength",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "text" },
                "constraints": { "minLength": 5, "maxLength": 4 } }]),
        ),
        (
            codes::INVALID_PATTERN,
            "/0/constraints/pattern",
            serde_json::json!([{ "name": "a.b", "type": { "kind": "text" }, "constraints": { "pattern": "([a-z" } }]),
        ),
    ];
    for (code, pointer, value) in cases {
        assert_eq!(
            findings(&value),
            vec![(code.to_owned(), pointer.to_owned())],
            "{value}"
        );
    }
}

#[test]
fn ref_cycles_are_reported_on_each_member_only() {
    let value = serde_json::json!([
        { "name": "a.one", "type": { "kind": "ref", "name": "a.two" } },
        { "name": "a.two", "type": { "kind": "ref", "name": "a.one" } },
        { "name": "a.self", "type": { "kind": "ref", "name": "a.self" } },
        { "name": "a.into_cycle", "type": { "kind": "ref", "name": "a.one" } }
    ]);
    assert_eq!(
        findings(&value),
        vec![
            (codes::REF_CYCLE.to_owned(), "/0/type/name".to_owned()),
            (codes::REF_CYCLE.to_owned(), "/1/type/name".to_owned()),
            (codes::REF_CYCLE.to_owned(), "/2/type/name".to_owned()),
        ]
    );
}

#[test]
fn equal_bounds_and_negative_zero_are_accepted() {
    let value = serde_json::json!([
        { "name": "a.fixed", "type": { "kind": "decimal", "scale": 2 },
          "constraints": { "min": "1.50", "max": "1.5" } },
        { "name": "a.zero", "type": { "kind": "integer" },
          "constraints": { "min": "-0", "max": "0" } },
        { "name": "a.wide", "type": { "kind": "integer" },
          "constraints": { "min": "-9223372036854775808", "max": "9223372036854775807" } },
        // The engine's exact edges: +/-(2^96 - 1) at scale 0, and scale 28.
        { "name": "a.edge", "type": { "kind": "decimal", "scale": 0 },
          "constraints": { "min": "-79228162514264337593543950335", "max": "79228162514264337593543950335" } },
        { "name": "a.fine", "type": { "kind": "decimal", "scale": 28 },
          "constraints": { "min": "0.0000000000000000000000000001", "max": "7.9228162514264337593543950335" } }
    ]);
    assert_eq!(findings(&value), Vec::new());
}

#[test]
fn record_members_are_closed_typed_and_acyclic() {
    for (members, code) in [
        (
            serde_json::json!([]),
            "property_type.validation.empty_record",
        ),
        (
            serde_json::json!([{"id":"x","type":{"kind":"integer"}},{"id":"x","type":{"kind":"text"}}]),
            "property_type.validation.duplicate_member",
        ),
        (
            serde_json::json!([{"id":"x","type":{"kind":"ref","name":"missing.type"}}]),
            codes::UNKNOWN_REF,
        ),
        (
            serde_json::json!([{"id":"x","type":{"kind":"ref","name":"test.record"}}]),
            codes::REF_CYCLE,
        ),
        (
            serde_json::json!([{"id":"x","type":{"kind":"boolean"},"constraints":{"min":"1"}}]),
            codes::CONSTRAINT_NOT_APPLICABLE,
        ),
    ] {
        let result = findings(
            &serde_json::json!([{"name":"test.record","type":{"kind":"record","members":members}}]),
        );
        assert!(result.iter().any(|(found, _)| found == code), "{result:?}");
    }
}
