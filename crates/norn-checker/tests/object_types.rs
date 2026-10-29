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

//! `check_object_type_defs` (T1-ONT-1b, #2512): the object-type fixtures are
//! valid against the property-type fixture catalog, and every diagnostic
//! code has a negative case.

use std::collections::BTreeSet;

use norn_checker::{
    Diagnostic, check_object_type_defs, check_property_type_defs, object_types::codes,
    property_types::codes as property_codes,
};
use norn_ir::identifier_limits::MAX_OBJECT_KEY_BYTES;
use norn_ir::{ObjectTypeDef, PropertyTypeDef, PropertyTypeName};

#[path = "support/model_codes.rs"]
mod model_codes;
use model_codes::MODEL_CODES;

macro_rules! property_type {
    ($kind:literal) => {
        include_str!(concat!(
            "../../../fixtures/ir/entity-v2/property-types/",
            $kind,
            ".property-type.json"
        ))
    };
}

macro_rules! object_type {
    ($name:literal) => {
        include_str!(concat!(
            "../../../fixtures/ir/entity-v2/object-types/",
            $name,
            ".object-type.json"
        ))
    };
}

const CATALOG: [&str; 10] = [
    property_type!("text"),
    property_type!("integer"),
    property_type!("decimal"),
    property_type!("boolean"),
    property_type!("timestamp"),
    property_type!("date"),
    property_type!("duration"),
    property_type!("enum"),
    property_type!("json-ext"),
    property_type!("ref"),
];

const OBJECT_TYPES: [&str; 3] = [
    object_type!("sku"),
    object_type!("sales-order-line"),
    object_type!("party"),
];

fn registry() -> BTreeSet<PropertyTypeName> {
    BTreeSet::from([PropertyTypeName("iso4217.currency_code".to_owned())])
}

fn catalog() -> Vec<PropertyTypeDef> {
    CATALOG
        .iter()
        .map(|source| serde_json::from_str(source).expect("property type fixture decodes"))
        .collect()
}

fn findings(value: &serde_json::Value) -> Vec<(String, String)> {
    let defs: Vec<ObjectTypeDef> =
        serde_json::from_str(&value.to_string()).expect("object types decode");
    check_object_type_defs(&defs, &catalog(), &registry())
        .into_iter()
        .map(|Diagnostic { code, span, .. }| {
            (
                code.as_str().to_owned(),
                span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect()
}

/// One object type `test.Thing` with these properties and keys.
fn thing(properties: &serde_json::Value, identity: &serde_json::Value) -> serde_json::Value {
    serde_json::json!([{ "name": "test.Thing", "properties": properties, "identity": identity }])
}

fn naming_series_thing(series: serde_json::Value) -> serde_json::Value {
    let mut value = thing(
        &serde_json::json!([
            { "id": "number", "type": { "kind": "text" } },
            { "id": "company", "type": { "kind": "text" } }
        ]),
        &serde_json::json!([{ "name": "primary", "properties": ["number"] }]),
    );
    value[0]["identity"][0]["namingSeries"] = series;
    value
}

/// One property of `test.Thing` with this type, extra members merged in.
fn with_property(
    property_type: &serde_json::Value,
    extra: &serde_json::Value,
) -> serde_json::Value {
    let mut property = serde_json::json!({ "id": "value", "type": property_type });
    for (key, value) in extra.as_object().expect("extra is an object") {
        property[key] = value.clone();
    }
    thing(&serde_json::json!([property]), &serde_json::json!([]))
}

#[test]
fn the_fixtures_are_valid_against_the_fixture_catalog() {
    let catalog = catalog();
    assert_eq!(check_property_type_defs(&catalog, &registry()), Vec::new());
    let defs: Vec<ObjectTypeDef> = OBJECT_TYPES
        .iter()
        .map(|source| serde_json::from_str(source).expect("object type fixture decodes"))
        .collect();
    assert_eq!(
        check_object_type_defs(&defs, &catalog, &registry()),
        Vec::new()
    );
    // The checker parses computed expressions (T1-ONT-2); pin that the
    // fixtures' expressions compile on their own too. Typing is T1-EXPR-5.
    for def in &defs {
        for property in &def.properties {
            if let Some(computed) = &property.computed {
                norn_expr::Expression::compile(computed.as_str())
                    .unwrap_or_else(|error| panic!("{} compiles: {error}", computed.as_str()));
            }
        }
    }
}

#[test]
fn every_code_has_a_negative_case() {
    let text = serde_json::json!({ "kind": "text" });
    let invalid_series_timezone = naming_series_thing(serde_json::json!({
        "pattern": "PO-{company}-{YYYY}{MM}{DD}-{#####}",
        "timezone": "Mars/Phobos",
        "scopeFields": ["company"],
        "resetPolicy": "day",
        "gapPolicy": "gapless_within_commit"
    }));
    let cases = [
        (
            codes::INVALID_NAME,
            "/0/name",
            serde_json::json!([{ "name": "test.thing" }]),
        ),
        (
            codes::DUPLICATE_NAME,
            "/1/name",
            serde_json::json!([{ "name": "test.Thing" }, { "name": "test.Thing" }]),
        ),
        (
            codes::INVALID_PROPERTY_ID,
            "/0/properties/0/id",
            thing(
                &serde_json::json!([{ "id": "Value", "type": text }]),
                &serde_json::json!([]),
            ),
        ),
        (
            codes::DUPLICATE_PROPERTY_ID,
            "/0/properties/1/id",
            thing(
                &serde_json::json!([{ "id": "value", "type": text }, { "id": "value", "type": text }]),
                &serde_json::json!([]),
            ),
        ),
        (
            codes::INVALID_KEY_NAME,
            "/0/identity/0/name",
            thing(
                &serde_json::json!([{ "id": "value", "type": text }]),
                &serde_json::json!([{ "name": "Primary", "properties": ["value"] }]),
            ),
        ),
        (
            codes::DUPLICATE_KEY_NAME,
            "/0/identity/1/name",
            thing(
                &serde_json::json!([{ "id": "a", "type": text }, { "id": "b", "type": text }]),
                &serde_json::json!([
                    { "name": "primary", "properties": ["a"] },
                    { "name": "primary", "properties": ["b"] }
                ]),
            ),
        ),
        (
            codes::EMPTY_KEY,
            "/0/identity/0/properties",
            thing(
                &serde_json::json!([]),
                &serde_json::json!([{ "name": "primary", "properties": [] }]),
            ),
        ),
        (
            codes::UNKNOWN_KEY_PROPERTY,
            "/0/identity/0/properties/1",
            thing(
                &serde_json::json!([{ "id": "a", "type": text }]),
                &serde_json::json!([{ "name": "primary", "properties": ["a", "b"] }]),
            ),
        ),
        (
            codes::DUPLICATE_KEY_COMPONENT,
            "/0/identity/0/properties/1",
            thing(
                &serde_json::json!([{ "id": "a", "type": text }]),
                &serde_json::json!([{ "name": "primary", "properties": ["a", "a"] }]),
            ),
        ),
        (
            codes::INADMISSIBLE_KEY_KIND,
            "/0/identity/0/properties/0",
            thing(
                &serde_json::json!([{ "id": "a", "type": { "kind": "json-ext" } }]),
                &serde_json::json!([{ "name": "primary", "properties": ["a"] }]),
            ),
        ),
        (
            codes::NULLABLE_KEY_COMPONENT,
            "/0/identity/0/properties/0",
            thing(
                &serde_json::json!([{ "id": "a", "type": text, "constraints": { "nullable": true } }]),
                &serde_json::json!([{ "name": "primary", "properties": ["a"] }]),
            ),
        ),
        (
            codes::COMPUTED_KEY_COMPONENT,
            "/0/identity/0/properties/0",
            thing(
                &serde_json::json!([
                    { "id": "a", "type": text },
                    { "id": "b", "type": text, "computed": "self.a + \"-x\"" }
                ]),
                &serde_json::json!([{ "name": "primary", "properties": ["b"] }]),
            ),
        ),
        (
            codes::REDUNDANT_KEY,
            "/0/identity/1/properties",
            thing(
                &serde_json::json!([{ "id": "a", "type": text }, { "id": "b", "type": text }]),
                &serde_json::json!([
                    { "name": "primary", "properties": ["a", "b"] },
                    { "name": "reversed", "properties": ["b", "a"] }
                ]),
            ),
        ),
        (
            codes::EMPTY_IDENTITY_RULE,
            "/0/identity/0/rule",
            thing(
                &serde_json::json!([{ "id": "a", "type": text }]),
                &serde_json::json!([{ "name": "primary", "properties": ["a"], "rule": {} }]),
            ),
        ),
        (
            codes::NORMALIZATION_NOT_IN_KEY,
            "/0/identity/0/rule/normalize/0/property",
            thing(
                &serde_json::json!([{ "id": "a", "type": text }, { "id": "b", "type": text }]),
                &serde_json::json!([{ "name": "primary", "properties": ["a"],
                    "rule": { "normalize": [{ "property": "b", "steps": ["trim"] }] } }]),
            ),
        ),
        (
            codes::DUPLICATE_NORMALIZATION,
            "/0/identity/0/rule/normalize/1/property",
            thing(
                &serde_json::json!([{ "id": "a", "type": text }]),
                &serde_json::json!([{ "name": "primary", "properties": ["a"],
                    "rule": { "normalize": [
                        { "property": "a", "steps": ["trim"] },
                        { "property": "a", "steps": ["lowercase"] }
                    ] } }]),
            ),
        ),
        (
            codes::EMPTY_NORMALIZATION,
            "/0/identity/0/rule/normalize/0/steps",
            thing(
                &serde_json::json!([{ "id": "a", "type": text }]),
                &serde_json::json!([{ "name": "primary", "properties": ["a"],
                    "rule": { "normalize": [{ "property": "a", "steps": [] }] } }]),
            ),
        ),
        (
            codes::NORMALIZATION_NOT_APPLICABLE,
            "/0/identity/0/rule/normalize/0/property",
            thing(
                &serde_json::json!([{ "id": "a", "type": { "kind": "ref", "name": "inventory.reorder_quantity" } }]),
                &serde_json::json!([{ "name": "primary", "properties": ["a"],
                    "rule": { "normalize": [{ "property": "a", "steps": ["trim"] }] } }]),
            ),
        ),
        (
            codes::INVALID_MATCH_POLICY,
            "/0/identity/0/rule/matchPolicy",
            thing(
                &serde_json::json!([{ "id": "a", "type": text }]),
                &serde_json::json!([{ "name": "primary", "properties": ["a"],
                    "rule": { "matchPolicy": "Party Email" } }]),
            ),
        ),
        (
            codes::INVALID_NAMING_SERIES,
            "/0/identity/0/namingSeries/timezone",
            invalid_series_timezone,
        ),
        (
            codes::DEFAULT_TYPE_MISMATCH,
            "/0/properties/0/default",
            with_property(
                &serde_json::json!({ "kind": "decimal", "scale": 2 }),
                &serde_json::json!({ "default": 1.5 }),
            ),
        ),
        (
            codes::DEFAULT_OUT_OF_RANGE,
            "/0/properties/0/default",
            with_property(
                &serde_json::json!({ "kind": "integer" }),
                &serde_json::json!({ "default": "9223372036854775808" }),
            ),
        ),
        (
            codes::DEFAULT_VIOLATES_CONSTRAINT,
            "/0/properties/0/default",
            with_property(
                &serde_json::json!({ "kind": "ref", "name": "inventory.reorder_quantity" }),
                &serde_json::json!({ "default": "0" }),
            ),
        ),
        (
            codes::DEFAULT_UNCHECKED,
            "/0/properties/0/default",
            with_property(
                &serde_json::json!({ "kind": "ref", "name": "commerce.price_currency" }),
                &serde_json::json!({ "default": "EGP" }),
            ),
        ),
        (
            codes::COMPUTED_WITH_DEFAULT,
            "/0/properties/0/default",
            with_property(
                &serde_json::json!({ "kind": "integer" }),
                &serde_json::json!({ "computed": "1 + 1", "default": "2" }),
            ),
        ),
        (
            codes::EMPTY_COMPUTED,
            "/0/properties/0/computed",
            with_property(
                &serde_json::json!({ "kind": "integer" }),
                &serde_json::json!({ "computed": "  " }),
            ),
        ),
        (
            codes::INVALID_COMPUTED,
            "/0/properties/0/computed",
            with_property(
                &serde_json::json!({ "kind": "integer" }),
                &serde_json::json!({ "computed": "self.quantity *" }),
            ),
        ),
        // Inline types and property constraints reuse the property-type codes.
        (
            property_codes::UNKNOWN_REF,
            "/0/properties/0/type/name",
            with_property(
                &serde_json::json!({ "kind": "ref", "name": "commerce.missing" }),
                &serde_json::json!({}),
            ),
        ),
        (
            property_codes::SCALE_OUT_OF_RANGE,
            "/0/properties/0/type/scale",
            with_property(
                &serde_json::json!({ "kind": "decimal", "scale": 29 }),
                &serde_json::json!({}),
            ),
        ),
        (
            property_codes::CONSTRAINT_NOT_APPLICABLE,
            "/0/properties/0/constraints/pattern",
            with_property(
                &serde_json::json!({ "kind": "ref", "name": "inventory.net_weight" }),
                &serde_json::json!({ "constraints": { "pattern": "^x$" } }),
            ),
        ),
    ];
    let covered: BTreeSet<&str> = cases.iter().map(|(code, _, _)| *code).collect();
    let registered: BTreeSet<&str> = norn_checker::codes::REGISTRY
        .iter()
        .map(|entry| entry.code)
        .filter(|code| code.starts_with("object_type.") && !MODEL_CODES.contains(code))
        .collect();
    assert!(
        registered.is_subset(&covered),
        "uncovered codes: {:?}",
        registered.difference(&covered).collect::<Vec<_>>()
    );
    for (code, pointer, value) in cases {
        assert_eq!(
            findings(&value),
            vec![(code.to_owned(), pointer.to_owned())],
            "{value}"
        );
    }
}

fn default_findings(property_type: &serde_json::Value, default: &serde_json::Value) -> Vec<String> {
    findings(&with_property(
        property_type,
        &serde_json::json!({ "default": default }),
    ))
    .into_iter()
    .map(|(code, _)| code)
    .collect()
}

#[test]
fn defaults_are_type_checked_per_kind() {
    let ok: Vec<String> = Vec::new();
    let mismatch = vec![codes::DEFAULT_TYPE_MISMATCH.to_owned()];
    let out_of_range = vec![codes::DEFAULT_OUT_OF_RANGE.to_owned()];
    let violates = vec![codes::DEFAULT_VIOLATES_CONSTRAINT.to_owned()];
    let unchecked = vec![codes::DEFAULT_UNCHECKED.to_owned()];
    let decimal = serde_json::json!({ "kind": "decimal", "scale": 2 });
    let utc = serde_json::json!({ "kind": "timestamp", "timezone": "utc" });
    let offset = serde_json::json!({ "kind": "timestamp", "timezone": "offset" });
    let status = serde_json::json!({ "kind": "ref", "name": "commerce.order_status" });
    let cases = [
        // Exact decimals: strings only, at most `scale` fraction digits,
        // trailing zeros count, exponents and JSON numbers rejected.
        (decimal.clone(), serde_json::json!("12.50"), &ok),
        (decimal.clone(), serde_json::json!("-0.01"), &ok),
        (decimal.clone(), serde_json::json!("12.500"), &mismatch),
        (decimal.clone(), serde_json::json!("1e2"), &mismatch),
        (decimal, serde_json::json!(12), &mismatch),
        (
            serde_json::json!({ "kind": "decimal", "scale": 0 }),
            serde_json::json!("79228162514264337593543950335"),
            &ok,
        ),
        (
            serde_json::json!({ "kind": "decimal", "scale": 0 }),
            serde_json::json!("79228162514264337593543950336"),
            &out_of_range,
        ),
        // Integers: no fraction, i64 range.
        (
            serde_json::json!({ "kind": "integer" }),
            serde_json::json!("-9223372036854775808"),
            &ok,
        ),
        (
            serde_json::json!({ "kind": "integer" }),
            serde_json::json!("1.0"),
            &mismatch,
        ),
        // Bounds of the property and of every definition on its ref chain.
        (
            serde_json::json!({ "kind": "ref", "name": "inventory.net_weight" }),
            serde_json::json!("25000.000"),
            &ok,
        ),
        (
            serde_json::json!({ "kind": "ref", "name": "inventory.net_weight" }),
            serde_json::json!("25000.001"),
            &violates,
        ),
        (
            serde_json::json!({ "kind": "ref", "name": "inventory.net_weight" }),
            serde_json::json!("-1"),
            &violates,
        ),
        // Text: length, pattern (via the ref chain), formats.
        (
            serde_json::json!({ "kind": "ref", "name": "commerce.sku_code" }),
            serde_json::json!("AB-12"),
            &ok,
        ),
        (
            serde_json::json!({ "kind": "ref", "name": "commerce.sku_code" }),
            serde_json::json!("ab-12"),
            &violates,
        ),
        (
            serde_json::json!({ "kind": "ref", "name": "commerce.sku_code" }),
            serde_json::json!("AB"),
            &violates,
        ),
        (
            serde_json::json!({ "kind": "text", "format": "uuid" }),
            serde_json::json!("0190f1c4-7d1e-7c3a-9b5e-3f2a1d4c5b6e"),
            &ok,
        ),
        (
            serde_json::json!({ "kind": "text", "format": "uuid" }),
            serde_json::json!("not-a-uuid"),
            &mismatch,
        ),
        (
            serde_json::json!({ "kind": "text", "format": "email" }),
            serde_json::json!("ops@example.com"),
            &unchecked,
        ),
        (
            serde_json::json!({ "kind": "text" }),
            serde_json::json!(5),
            &mismatch,
        ),
        // Booleans.
        (
            serde_json::json!({ "kind": "boolean" }),
            serde_json::json!("true"),
            &mismatch,
        ),
        // Both timestamp policies accept explicit `Z` or numeric offsets;
        // the runtime stores their canonical UTC instant.
        (utc.clone(), serde_json::json!("2026-03-01T12:00:00Z"), &ok),
        (
            utc.clone(),
            serde_json::json!("2026-03-01T14:00:00+02:00"),
            &ok,
        ),
        (utc, serde_json::json!("2026-03-01T12:00:00"), &mismatch),
        (
            offset.clone(),
            serde_json::json!("2026-03-01T14:00:00+02:00"),
            &ok,
        ),
        (
            offset.clone(),
            serde_json::json!("2026-03-01T12:00:00Z"),
            &ok,
        ),
        (
            offset.clone(),
            serde_json::json!("2026-03-01T14:00:00-00:00"),
            &mismatch,
        ),
        (offset, serde_json::json!("2026-02-30T00:00:00Z"), &mismatch),
        // Dates.
        (
            serde_json::json!({ "kind": "date" }),
            serde_json::json!("2028-02-29"),
            &ok,
        ),
        (
            serde_json::json!({ "kind": "date" }),
            serde_json::json!("2026-02-29"),
            &mismatch,
        ),
        // Enums (through a ref).
        (status.clone(), serde_json::json!("placed"), &ok),
        (status, serde_json::json!("shipped"), &mismatch),
        // Opaque JSON.
        (
            serde_json::json!({ "kind": "json-ext" }),
            serde_json::json!({ "a": [1, "b"] }),
            &ok,
        ),
        // Not checkable here: fail closed.
        (
            serde_json::json!({ "kind": "duration" }),
            serde_json::json!("PT1H"),
            &unchecked,
        ),
    ];
    for (property_type, default, expected) in cases {
        assert_eq!(
            &default_findings(&property_type, &default),
            expected,
            "{property_type} default {default}"
        );
    }
}

#[test]
fn keys_admit_registry_and_utc_components_but_not_offset_timestamps() {
    let valid = thing(
        &serde_json::json!([
            { "id": "currency", "type": { "kind": "ref", "name": "commerce.price_currency" } },
            { "id": "at", "type": { "kind": "timestamp", "timezone": "utc" } },
            { "id": "day", "type": { "kind": "date" } },
            { "id": "open", "type": { "kind": "boolean" } }
        ]),
        &serde_json::json!([{ "name": "primary", "properties": ["currency", "at", "day", "open"] }]),
    );
    assert_eq!(findings(&valid), Vec::new());

    let invalid = thing(
        &serde_json::json!([
            { "id": "at", "type": { "kind": "timestamp", "timezone": "offset" } },
            // Nullability through the ref chain.
            { "id": "expires", "type": { "kind": "ref", "name": "commerce.expiry_date" } }
        ]),
        &serde_json::json!([{ "name": "primary", "properties": ["at", "expires"] }]),
    );
    assert_eq!(
        findings(&invalid),
        vec![
            (
                codes::INADMISSIBLE_KEY_KIND.to_owned(),
                "/0/identity/0/properties/0".to_owned()
            ),
            (
                codes::NULLABLE_KEY_COMPONENT.to_owned(),
                "/0/identity/0/properties/1".to_owned()
            ),
        ]
    );
}

#[test]
fn naming_series_contracts_validate_patterns_scopes_timezone_and_reset_periods() {
    let valid = naming_series_thing(serde_json::json!({
        "pattern": "PO-{company}-{YYYY}{MM}{DD}-{#####}",
        "timezone": "America/New_York",
        "scopeFields": ["company"],
        "resetPolicy": "day",
        "gapPolicy": "gapless_within_commit"
    }));
    assert_eq!(findings(&valid), Vec::new());

    let invalid_scope_boundary = serde_json::json!({
        "pattern": "PO-{company}_{#####}",
        "timezone": "UTC",
        "scopeFields": ["company"],
        "resetPolicy": "never",
        "gapPolicy": "allow_gaps"
    });
    let mut missing_reset_token = invalid_scope_boundary.clone();
    missing_reset_token["pattern"] = serde_json::json!("PO-{company}-{MM}-{#####}");
    missing_reset_token["resetPolicy"] = serde_json::json!("month");
    let mut missing_scope_token = invalid_scope_boundary.clone();
    missing_scope_token["pattern"] = serde_json::json!("PO-{YYYY}-{#####}");

    for invalid in [
        naming_series_thing(invalid_scope_boundary),
        naming_series_thing(missing_reset_token),
        naming_series_thing(missing_scope_token),
    ] {
        assert!(
            findings(&invalid)
                .iter()
                .any(|(code, _)| code == codes::INVALID_NAMING_SERIES),
            "invalid naming-series contract must be rejected"
        );
    }
}

#[test]
fn inherited_naming_series_properties_remain_plain_text() {
    let inherited_generated_key = serde_json::json!([
        {
            "name": "test.Base",
            "properties": [
                { "id": "number", "type": { "kind": "text" } }
            ],
            "identity": [{
                "name": "primary",
                "properties": ["number"],
                "namingSeries": {
                    "pattern": "PO-{YYYY}-{#####}",
                    "timezone": "UTC",
                    "resetPolicy": "year",
                    "gapPolicy": "gapless_within_commit"
                }
            }]
        },
        {
            "name": "test.Child",
            "extends": "test.Base",
            "properties": [
                { "id": "number", "type": { "kind": "text", "format": "uuid" } }
            ]
        }
    ]);
    assert!(findings(&inherited_generated_key).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/1/extends".to_owned(),
    )));

    let inherited_scope = serde_json::json!([
        {
            "name": "test.Base",
            "properties": [
                { "id": "number", "type": { "kind": "text" } },
                { "id": "company", "type": { "kind": "text" } }
            ],
            "identity": [{
                "name": "primary",
                "properties": ["number"],
                "namingSeries": {
                    "pattern": "PO-{company}-{YYYY}-{#####}",
                    "timezone": "UTC",
                    "scopeFields": ["company"],
                    "resetPolicy": "year",
                    "gapPolicy": "gapless_within_commit"
                }
            }]
        },
        {
            "name": "test.Child",
            "extends": "test.Base",
            "properties": [
                { "id": "company", "type": { "kind": "text", "format": "uuid" } }
            ]
        }
    ]);
    assert!(findings(&inherited_scope).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/1/extends".to_owned(),
    )));
}

#[test]
fn naming_series_minimum_rendered_key_size_obeys_object_key_byte_limit() {
    let pattern = |extra_bytes: usize| {
        format!(
            "{}{{YYYY}}-{{company}}-{{#####}}",
            "x".repeat(MAX_OBJECT_KEY_BYTES - 12 + extra_bytes)
        )
    };
    let at_limit = naming_series_thing(serde_json::json!({
        "pattern": pattern(0),
        "timezone": "UTC",
        "scopeFields": ["company"],
        "resetPolicy": "year",
        "gapPolicy": "allow_gaps"
    }));
    assert_eq!(findings(&at_limit), Vec::new());

    let over_limit = naming_series_thing(serde_json::json!({
        "pattern": pattern(1),
        "timezone": "UTC",
        "scopeFields": ["company"],
        "resetPolicy": "year",
        "gapPolicy": "allow_gaps"
    }));
    assert!(findings(&over_limit).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/0/identity/0/namingSeries/pattern".to_owned(),
    )));
}

#[test]
fn naming_series_respects_generated_text_length_constraints() {
    let series = serde_json::json!({
        "pattern": "PO-{YYYY}-{#####}",
        "timezone": "UTC",
        "resetPolicy": "year",
        "gapPolicy": "allow_gaps"
    });
    let mut too_short_max = naming_series_thing(series.clone());
    too_short_max[0]["properties"][0]["constraints"] = serde_json::json!({ "maxLength": 12 });
    assert!(findings(&too_short_max).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/0/identity/0/namingSeries/pattern".to_owned(),
    )));

    let mut too_long_min = naming_series_thing(series.clone());
    too_long_min[0]["properties"][0]["constraints"] = serde_json::json!({ "minLength": 14 });
    assert!(findings(&too_long_min).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/0/identity/0/namingSeries/pattern".to_owned(),
    )));

    let mut exact_bounds = naming_series_thing(series);
    exact_bounds[0]["properties"][0]["constraints"] =
        serde_json::json!({ "minLength": 13, "maxLength": 13 });
    assert_eq!(findings(&exact_bounds), Vec::new());

    let inherited_max = serde_json::json!([
        {
            "name": "test.Base",
            "properties": [
                { "id": "number", "type": { "kind": "text" } }
            ],
            "identity": [{
                "name": "primary",
                "properties": ["number"],
                "namingSeries": {
                    "pattern": "PO-{YYYY}-{#####}",
                    "timezone": "UTC",
                    "resetPolicy": "year",
                    "gapPolicy": "allow_gaps"
                }
            }]
        },
        {
            "name": "test.Child",
            "extends": "test.Base",
            "properties": [{
                "id": "number",
                "type": { "kind": "text" },
                "constraints": { "maxLength": 12 }
            }]
        }
    ]);
    assert!(findings(&inherited_max).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/1/extends".to_owned(),
    )));
}

#[test]
fn naming_series_scope_minimum_length_constrains_generated_key_length() {
    let mut declared = naming_series_thing(serde_json::json!({
        "pattern": "{company}-{#}",
        "timezone": "UTC",
        "scopeFields": ["company"],
        "resetPolicy": "never",
        "gapPolicy": "allow_gaps"
    }));
    declared[0]["properties"][0]["constraints"] = serde_json::json!({ "maxLength": 10 });
    declared[0]["properties"][1]["constraints"] = serde_json::json!({ "minLength": 20 });
    assert!(findings(&declared).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/0/identity/0/namingSeries/pattern".to_owned(),
    )));

    let mut valid = naming_series_thing(serde_json::json!({
        "pattern": "{company}-{#}",
        "timezone": "UTC",
        "scopeFields": ["company"],
        "resetPolicy": "never",
        "gapPolicy": "allow_gaps"
    }));
    valid[0]["properties"][0]["constraints"] = serde_json::json!({ "maxLength": 4 });
    valid[0]["properties"][1]["constraints"] = serde_json::json!({ "minLength": 2 });
    assert_eq!(findings(&valid), Vec::new());

    let inherited_scope_minimum = serde_json::json!([
        {
            "name": "test.Base",
            "properties": [
                { "id": "number", "type": { "kind": "text" } },
                { "id": "company", "type": { "kind": "text" } }
            ],
            "identity": [{
                "name": "primary",
                "properties": ["number"],
                "namingSeries": {
                    "pattern": "{company}-{#}",
                    "timezone": "UTC",
                    "scopeFields": ["company"],
                    "resetPolicy": "never",
                    "gapPolicy": "allow_gaps"
                }
            }]
        },
        {
            "name": "test.Child",
            "extends": "test.Base",
            "properties": [
                {
                    "id": "number",
                    "type": { "kind": "text" },
                    "constraints": { "maxLength": 10 }
                },
                {
                    "id": "company",
                    "type": { "kind": "text" },
                    "constraints": { "minLength": 20 }
                }
            ]
        }
    ]);
    assert!(findings(&inherited_scope_minimum).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/1/extends".to_owned(),
    )));
}

#[test]
fn naming_series_scope_maximum_length_bounds_generated_minimum() {
    let mut rejected = naming_series_thing(serde_json::json!({
        "pattern": "{company}-{#}",
        "timezone": "UTC",
        "scopeFields": ["company"],
        "resetPolicy": "never",
        "gapPolicy": "allow_gaps"
    }));
    rejected[0]["properties"][0]["constraints"] = serde_json::json!({ "minLength": 5 });
    rejected[0]["properties"][1]["constraints"] = serde_json::json!({ "maxLength": 2 });
    assert!(findings(&rejected).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/0/identity/0/namingSeries/pattern".to_owned(),
    )));

    let mut exact_boundary = naming_series_thing(serde_json::json!({
        "pattern": "{company}-{#}",
        "timezone": "UTC",
        "scopeFields": ["company"],
        "resetPolicy": "never",
        "gapPolicy": "allow_gaps"
    }));
    exact_boundary[0]["properties"][0]["constraints"] = serde_json::json!({ "minLength": 4 });
    exact_boundary[0]["properties"][1]["constraints"] = serde_json::json!({ "maxLength": 2 });
    assert_eq!(findings(&exact_boundary), Vec::new());

    let inherited = |scope_maximum| {
        serde_json::json!([
            {
                "name": "test.Base",
                "properties": [
                    { "id": "number", "type": { "kind": "text" } },
                    { "id": "company", "type": { "kind": "text" } }
                ],
                "identity": [{
                    "name": "primary",
                    "properties": ["number"],
                    "namingSeries": {
                        "pattern": "{company}-{#}",
                        "timezone": "UTC",
                        "scopeFields": ["company"],
                        "resetPolicy": "never",
                        "gapPolicy": "allow_gaps"
                    }
                }]
            },
            {
                "name": "test.Child",
                "extends": "test.Base",
                "properties": [
                    {
                        "id": "number",
                        "type": { "kind": "text" },
                        "constraints": { "minLength": 5 }
                    },
                    {
                        "id": "company",
                        "type": { "kind": "text" },
                        "constraints": { "maxLength": scope_maximum }
                    }
                ]
            }
        ])
    };
    assert!(findings(&inherited(2)).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/1/extends".to_owned(),
    )));
}

#[test]
fn naming_series_rejects_empty_or_unchecked_regex_scope_domains() {
    let mut declared = naming_series_thing(serde_json::json!({
        "pattern": "{company}-{#}",
        "timezone": "UTC",
        "scopeFields": ["company"],
        "resetPolicy": "never",
        "gapPolicy": "allow_gaps"
    }));
    declared[0]["properties"][1]["constraints"] =
        serde_json::json!({ "maxLength": 0, "pattern": "^X$" });
    let declared_findings = findings(&declared);
    assert!(declared_findings.contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/0/identity/0/namingSeries/scopeFields/0".to_owned(),
    )));

    let inherited = serde_json::json!([
        {
            "name": "test.Base",
            "properties": [
                { "id": "number", "type": { "kind": "text" } },
                { "id": "company", "type": { "kind": "text" } }
            ],
            "identity": [{
                "name": "primary",
                "properties": ["number"],
                "namingSeries": {
                    "pattern": "{company}-{#}",
                    "timezone": "UTC",
                    "scopeFields": ["company"],
                    "resetPolicy": "never",
                    "gapPolicy": "allow_gaps"
                }
            }]
        },
        {
            "name": "test.Child",
            "extends": "test.Base",
            "properties": [{
                "id": "company",
                "type": { "kind": "text" },
                "constraints": { "maxLength": 0, "pattern": "^X$" }
            }]
        }
    ]);
    assert!(findings(&inherited).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/1/extends".to_owned(),
    )));
}

#[test]
fn inherited_naming_series_minimum_bytes_obey_object_key_limit() {
    let inherited = |scope_minimum| {
        serde_json::json!([
            {
                "name": "test.Base",
                "properties": [
                    { "id": "number", "type": { "kind": "text" } },
                    { "id": "company", "type": { "kind": "text" } }
                ],
                "identity": [{
                    "name": "primary",
                    "properties": ["number"],
                    "namingSeries": {
                        "pattern": "{company}-{#}",
                        "timezone": "UTC",
                        "scopeFields": ["company"],
                        "resetPolicy": "never",
                        "gapPolicy": "allow_gaps"
                    }
                }]
            },
            {
                "name": "test.Child",
                "extends": "test.Base",
                "properties": [{
                    "id": "company",
                    "type": { "kind": "text" },
                    "constraints": { "minLength": scope_minimum }
                }]
            }
        ])
    };
    assert_eq!(findings(&inherited(MAX_OBJECT_KEY_BYTES - 2)), Vec::new());
    assert!(findings(&inherited(MAX_OBJECT_KEY_BYTES)).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/1/extends".to_owned(),
    )));
}

#[test]
fn generated_minimum_length_obeys_key_byte_limit_with_unbounded_scope() {
    let series = serde_json::json!({
        "pattern": "{company}-{#}",
        "timezone": "UTC",
        "scopeFields": ["company"],
        "resetPolicy": "never",
        "gapPolicy": "allow_gaps"
    });
    let mut at_limit = naming_series_thing(series.clone());
    at_limit[0]["properties"][0]["constraints"] =
        serde_json::json!({ "minLength": MAX_OBJECT_KEY_BYTES });
    assert_eq!(findings(&at_limit), Vec::new());

    let mut over_limit = naming_series_thing(series);
    over_limit[0]["properties"][0]["constraints"] =
        serde_json::json!({ "minLength": MAX_OBJECT_KEY_BYTES + 1 });
    assert!(findings(&over_limit).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/0/identity/0/namingSeries/pattern".to_owned(),
    )));

    let inherited = serde_json::json!([
        {
            "name": "test.Base",
            "properties": [
                { "id": "number", "type": { "kind": "text" } },
                { "id": "company", "type": { "kind": "text" } }
            ],
            "identity": [{
                "name": "primary",
                "properties": ["number"],
                "namingSeries": {
                    "pattern": "{company}-{#}",
                    "timezone": "UTC",
                    "scopeFields": ["company"],
                    "resetPolicy": "never",
                    "gapPolicy": "allow_gaps"
                }
            }]
        },
        {
            "name": "test.Child",
            "extends": "test.Base",
            "properties": [{
                "id": "number",
                "type": { "kind": "text" },
                "constraints": { "minLength": MAX_OBJECT_KEY_BYTES + 1 }
            }]
        }
    ]);
    assert!(findings(&inherited).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/1/extends".to_owned(),
    )));
}

#[test]
fn naming_series_text_length_counts_unicode_scalars_not_key_bytes() {
    let pattern = format!(
        "{}{{YYYY}}-{{#####}}",
        "é".repeat((MAX_OBJECT_KEY_BYTES - 10) / 2)
    );
    let value = thing(
        &serde_json::json!([{
            "id": "number",
            "type": { "kind": "text" },
            "constraints": { "maxLength": 2053 }
        }]),
        &serde_json::json!([{
            "name": "primary",
            "properties": ["number"],
            "namingSeries": {
                "pattern": pattern,
                "timezone": "UTC",
                "resetPolicy": "year",
                "gapPolicy": "allow_gaps"
            }
        }]),
    );
    assert_eq!(findings(&value), Vec::new());
}

#[test]
fn naming_series_rejects_effective_regex_constraints_on_generated_properties() {
    let mut declared = naming_series_thing(serde_json::json!({
        "pattern": "INV-{#####}",
        "timezone": "UTC",
        "resetPolicy": "never",
        "gapPolicy": "allow_gaps"
    }));
    declared[0]["properties"][0]["constraints"] = serde_json::json!({ "pattern": "^X$" });
    assert!(findings(&declared).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/0/identity/0/namingSeries/pattern".to_owned(),
    )));

    let inherited = serde_json::json!([
        {
            "name": "test.Base",
            "properties": [
                { "id": "number", "type": { "kind": "text" } }
            ],
            "identity": [{
                "name": "primary",
                "properties": ["number"],
                "namingSeries": {
                    "pattern": "INV-{#####}",
                    "timezone": "UTC",
                    "resetPolicy": "never",
                    "gapPolicy": "allow_gaps"
                }
            }]
        },
        {
            "name": "test.Child",
            "extends": "test.Base",
            "properties": [{
                "id": "number",
                "type": { "kind": "text" },
                "constraints": { "pattern": "^X$" }
            }]
        }
    ]);
    assert!(findings(&inherited).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/1/extends".to_owned(),
    )));

    let shared_type = thing(
        &serde_json::json!([{
            "id": "number",
            "type": { "kind": "ref", "name": "commerce.sku_code" }
        }]),
        &serde_json::json!([{
            "name": "primary",
            "properties": ["number"],
            "namingSeries": {
                "pattern": "inv-{#####}",
                "timezone": "UTC",
                "resetPolicy": "never",
                "gapPolicy": "allow_gaps"
            }
        }]),
    );
    assert!(findings(&shared_type).contains(&(
        codes::INVALID_NAMING_SERIES.to_owned(),
        "/0/identity/0/namingSeries/pattern".to_owned(),
    )));
}

#[test]
fn an_effective_type_cannot_declare_a_second_naming_series_key() {
    let value = serde_json::json!([
        {
            "name": "test.Base",
            "properties": [
                { "id": "base_number", "type": { "kind": "text" } }
            ],
            "identity": [{
                "name": "primary",
                "properties": ["base_number"],
                "namingSeries": {
                    "pattern": "BASE-{YYYY}-{#####}",
                    "timezone": "UTC",
                    "resetPolicy": "year",
                    "gapPolicy": "gapless_within_commit"
                }
            }]
        },
        {
            "name": "test.Child",
            "extends": "test.Base",
            "properties": [
                { "id": "child_number", "type": { "kind": "text" } }
            ],
            "identity": [{
                "name": "child_number_key",
                "properties": ["child_number"],
                "namingSeries": {
                    "pattern": "CHILD-{YYYY}-{#####}",
                    "timezone": "UTC",
                    "resetPolicy": "year",
                    "gapPolicy": "gapless_within_commit"
                }
            }]
        }
    ]);

    assert!(
        findings(&value).contains(&(
            codes::INVALID_NAMING_SERIES.to_owned(),
            "/1/identity/0/namingSeries".to_owned(),
        )),
        "a child series must be rejected when the inherited type already declares one"
    );
}

#[test]
fn keys_with_the_same_components_but_different_rules_are_distinct() {
    let text = serde_json::json!({ "kind": "text" });
    let value = thing(
        &serde_json::json!([{ "id": "email", "type": text }, { "id": "region", "type": text }]),
        &serde_json::json!([
            { "name": "raw", "properties": ["email", "region"] },
            { "name": "folded", "properties": ["region", "email"],
              "rule": { "normalize": [{ "property": "email", "steps": ["lowercase"] }] } },
            { "name": "trimmed_folded", "properties": ["email", "region"],
              "rule": { "normalize": [{ "property": "email", "steps": ["trim", "lowercase"] }] } },
            // Step order is significant.
            { "name": "folded_trimmed", "properties": ["email", "region"],
              "rule": { "normalize": [{ "property": "email", "steps": ["lowercase", "trim"] }] } },
            { "name": "matched", "properties": ["email", "region"],
              "rule": { "matchPolicy": "crm.party_email_exact" } }
        ]),
    );
    assert_eq!(findings(&value), Vec::new());

    // Identical effective rules, permuted components: redundant.
    let redundant = thing(
        &serde_json::json!([{ "id": "email", "type": text }, { "id": "region", "type": text }]),
        &serde_json::json!([
            { "name": "folded", "properties": ["email", "region"],
              "rule": { "normalize": [{ "property": "email", "steps": ["lowercase"] }],
                        "matchPolicy": "crm.party_email_exact" } },
            { "name": "folded_again", "properties": ["region", "email"],
              "rule": { "matchPolicy": "crm.party_email_exact",
                        "normalize": [{ "property": "email", "steps": ["lowercase"] }] } }
        ]),
    );
    assert_eq!(
        findings(&redundant),
        vec![(
            codes::REDUNDANT_KEY.to_owned(),
            "/0/identity/1/properties".to_owned()
        )]
    );
}

#[test]
fn key_order_is_significant_but_not_for_redundancy() {
    // Distinct component sets are distinct keys, overlapping is fine.
    let value = thing(
        &serde_json::json!([
            { "id": "a", "type": { "kind": "text" } },
            { "id": "b", "type": { "kind": "text" } }
        ]),
        &serde_json::json!([
            { "name": "primary", "properties": ["a", "b"] },
            { "name": "by_a", "properties": ["a"] }
        ]),
    );
    assert_eq!(findings(&value), Vec::new());
}
