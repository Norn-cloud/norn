#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! T1-IR-1b (#2507): checks for the typed Entity (`EntityIR` v1 or v2) and
//! Relation (v1) roots.

use norn_checker::validate_artifact;
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

const V1_ENTITY: &str = include_str!("../../../fixtures/ir/retail-sku.entity.json");
const V2_ENTITY: &str = include_str!("../../../fixtures/ir/retail-sku-v2.entity.json");
const RELATION: &str = include_str!("../../../fixtures/ir/retail-sku-supplier.relation.json");

const ENTITY: &str = "entity.validation.invalid_payload";
const AUTHORITY: &str = "entity.validation.invalid_mutation_authority";
const RELATION_CODE: &str = "relation.validation.invalid_payload";

fn fixture(source: &str) -> Value {
    serde_json::from_str(source).expect("fixture should parse")
}

/// `(code, path)` of every diagnostic for an envelope JSON value.
fn findings(envelope: Value) -> Vec<(String, String)> {
    let envelope: ArtifactEnvelope =
        serde_json::from_value(envelope).expect("envelope should decode leniently");
    validate_artifact(&envelope)
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|diagnostic| (diagnostic.code, diagnostic.path.unwrap_or_default()))
        .collect()
}

fn one(found: &[(String, String)], code: &str, path: &str) {
    assert_eq!(
        found,
        [(code.to_owned(), path.to_owned())],
        "expected exactly {code} at {path}"
    );
}

#[test]
fn entity_and_relation_fixtures_are_valid() {
    for source in [V1_ENTITY, V2_ENTITY, RELATION] {
        assert_eq!(findings(fixture(source)), []);
    }
    let mut v2 = fixture(V2_ENTITY);
    v2["payload"]["mutationAuthority"] = json!({ "kind": "domain_engine", "domain": "inventory" });
    v2["payload"]["extensions"] = json!({ "example.com/audit/1.0.0": { "enabled": true } });
    assert_eq!(findings(v2), []);
}

#[test]
fn v2_version_and_members_are_checked() {
    let mut artifact = fixture(V2_ENTITY);
    artifact["payload"]["entityVersion"] = json!("1.0");
    one(&findings(artifact), ENTITY, "payload.entityVersion");

    let mut artifact = fixture(V2_ENTITY);
    artifact["payload"]["entityVersion"] = Value::Null;
    one(&findings(artifact), ENTITY, "payload.entityVersion");

    for member in ["objectKind", "fields", "overlaySources"] {
        let mut artifact = fixture(V2_ENTITY);
        artifact["payload"][member] = json!("x");
        one(&findings(artifact), ENTITY, &format!("payload.{member}"));
    }

    let mut artifact = fixture(V2_ENTITY);
    artifact["payload"]
        .as_object_mut()
        .expect("payload object")
        .remove("objectType");
    one(&findings(artifact), ENTITY, "payload");

    let mut artifact = fixture(V2_ENTITY);
    artifact["payload"]["objectType"] = json!([]);
    one(&findings(artifact), ENTITY, "payload.objectType");

    let mut artifact = fixture(V2_ENTITY);
    artifact["payload"]["unknown"] = json!(true);
    one(&findings(artifact), ENTITY, "payload.unknown");
}

#[test]
fn v2_mutation_authority_is_the_v1_descriptor() {
    for (authority, path) in [
        (Value::Null, "payload.mutationAuthority"),
        (
            json!({ "kind": "domain_engine", "domain": "payroll" }),
            "payload.mutationAuthority.domain",
        ),
        (
            json!({ "kind": "generic_action", "domain": "tax" }),
            "payload.mutationAuthority.domain",
        ),
    ] {
        let mut artifact = fixture(V2_ENTITY);
        artifact["payload"]["mutationAuthority"] = authority;
        one(&findings(artifact), AUTHORITY, path);
    }
}

#[test]
fn object_type_decode_failures_are_located() {
    let cases: [(&[&str], Value, &str); 6] = [
        (&["name"], json!(7), "payload.objectType.name"),
        (
            &["description"],
            Value::Null,
            "payload.objectType.description",
        ),
        (&["extends"], Value::Null, "payload.objectType.extends"),
        (
            &["properties", "1", "type"],
            json!({ "kind": "float" }),
            "payload.objectType.properties[1]",
        ),
        (
            &["links", "0", "cardinality"],
            json!({ "to": "one" }),
            "payload.objectType.links[0]",
        ),
        (&["unknown"], json!(1), "payload.objectType"),
    ];
    for (path, value, expected) in cases {
        let mut artifact = fixture(V2_ENTITY);
        let mut target = &mut artifact["payload"]["objectType"];
        for segment in path {
            target = match segment.parse::<usize>() {
                Ok(index) => &mut target[index],
                Err(_) => &mut target[*segment],
            };
        }
        *target = value;
        one(&findings(artifact), ENTITY, expected);
    }
}

#[test]
fn object_type_name_is_the_object_kind_and_follows_the_grammar() {
    for name in ["retail.sku", "Sku", "retail.Sku Item", "retail.*"] {
        let mut artifact = fixture(V2_ENTITY);
        artifact["payload"]["objectType"]["name"] = json!(name);
        one(
            &findings(artifact),
            "object_type.validation.invalid_name",
            "payload.objectType.name",
        );
    }
}

#[test]
fn v2_members_without_entity_version_are_v1_unknown_fields() {
    let mut artifact = fixture(V1_ENTITY);
    artifact["payload"]["objectType"] = fixture(V2_ENTITY)["payload"]["objectType"].clone();
    one(&findings(artifact), ENTITY, "payload.objectType");
}

#[test]
fn relation_root_is_v1_only_and_checked() {
    let mut artifact = fixture(RELATION);
    artifact["payload"]["entityVersion"] = json!("2.0");
    let found = findings(artifact);
    assert!(
        found.contains(&(
            "relation.validation.v2_relation_is_link".to_owned(),
            "payload.entityVersion".to_owned()
        )),
        "{found:?}"
    );

    let mut artifact = fixture(RELATION);
    artifact["payload"]["cardinality"] = Value::Null;
    one(&findings(artifact), RELATION_CODE, "payload.cardinality");

    let mut artifact = fixture(RELATION);
    artifact["payload"]["cardinality"] = json!("some_to_some");
    one(&findings(artifact), RELATION_CODE, "payload.cardinality");
}

/// Every `EntityIR` v2 fixture object type is valid in its full model, so the
/// per-definition subset must not report anything on it alone: missing
/// supertypes, interfaces, link targets, property types, and layers are not
/// findings here.
#[test]
fn local_checks_report_nothing_on_valid_fixture_types() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/ir/entity-v2/object-types");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).expect("fixture directory should be readable") {
        let path = entry.expect("directory entry").path();
        let source = std::fs::read_to_string(&path).expect("fixture should be readable");
        let def: norn_ir::ObjectTypeDef =
            serde_json::from_str(&source).expect("fixture should decode");
        assert_eq!(
            norn_checker::check_object_type_local(&def),
            [],
            "{}",
            path.display()
        );
        let mut artifact = fixture(V2_ENTITY);
        artifact["payload"]["objectType"] = fixture(&source);
        assert_eq!(findings(artifact), [], "{} as an artifact", path.display());
        checked += 1;
    }
    assert!(checked >= 10, "expected the EntityIR v2 fixture set");
}

/// `(name, mutation of payload.objectType, expected code, expected path)`.
type Case = (&'static str, fn(&mut Value), &'static str, &'static str);

/// Single-definition errors fail per-artifact validation (T1-IR-1b review).
#[test]
fn single_definition_errors_fail_validate_artifact() {
    let cases: [Case; 8] = [
        (
            "duplicate property id",
            |object_type| {
                let copy = object_type["properties"][0].clone();
                object_type["properties"]
                    .as_array_mut()
                    .expect("array")
                    .push(copy);
            },
            "object_type.validation.duplicate_property_id",
            "payload.objectType.properties[8].id",
        ),
        (
            "invalid property id",
            |object_type| object_type["properties"][1]["id"] = json!("Title"),
            "object_type.validation.invalid_property_id",
            "payload.objectType.properties[1].id",
        ),
        (
            "key names no property",
            |object_type| object_type["identity"][0]["properties"] = json!(["missing"]),
            "object_type.validation.unknown_key_property",
            "payload.objectType.identity[0].properties[0]",
        ),
        (
            "duplicate key name",
            |object_type| {
                let copy = object_type["identity"][0].clone();
                object_type["identity"]
                    .as_array_mut()
                    .expect("array")
                    .push(copy);
            },
            "object_type.validation.duplicate_key_name",
            "payload.objectType.identity[1].name",
        ),
        (
            "default of the wrong type",
            |object_type| object_type["properties"][2]["default"] = json!(3),
            "object_type.validation.default_type_mismatch",
            "payload.objectType.properties[2].default",
        ),
        (
            "computed with a default",
            |object_type| object_type["properties"][6]["default"] = json!("1.000"),
            "object_type.validation.computed_with_default",
            "payload.objectType.properties[6].default",
        ),
        (
            "duplicate link id",
            |object_type| {
                let copy = object_type["links"][0].clone();
                object_type["links"]
                    .as_array_mut()
                    .expect("array")
                    .push(copy);
            },
            "object_type.validation.duplicate_link_id",
            "payload.objectType.links[1].id",
        ),
        (
            "inverted length bounds",
            |object_type| {
                object_type["properties"][1]["constraints"] =
                    json!({ "minLength": 10, "maxLength": 2 });
            },
            "property_type.validation.inverted_range",
            "payload.objectType.properties[1].constraints.minLength",
        ),
    ];
    for (name, mutate, code, path) in cases {
        let mut artifact = fixture(V2_ENTITY);
        mutate(&mut artifact["payload"]["objectType"]);
        let found = findings(artifact);
        assert!(
            found.contains(&(code.to_owned(), path.to_owned())),
            "{name}: expected {code} at {path}, got {found:?}"
        );
    }
}

/// A subtype's key may name an inherited property, which one definition
/// cannot see; the whole-model check owns that case.
#[test]
fn keys_on_inherited_properties_are_left_to_the_model_check() {
    let mut artifact = fixture(V2_ENTITY);
    artifact["payload"]["objectType"]["extends"] = json!("commerce.Product");
    artifact["payload"]["objectType"]["identity"][0]["properties"] = json!(["product_code"]);
    artifact["payload"]["objectType"]["identity"][0]
        .as_object_mut()
        .expect("key object")
        .remove("rule");
    assert_eq!(findings(artifact), []);
}
