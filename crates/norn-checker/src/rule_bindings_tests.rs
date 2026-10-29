#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "unit tests")]

use norn_ir::{ArtifactEnvelope, JsonValue};
use serde_json::{Value, json};

use super::{snapshot_values_schema_error, validate_rule_bindings};
use crate::EffectiveCatalog;
use crate::envelope::{ClosedSchemaProfile, ClosedSchemaUnions, validate_closed_schema};

fn effective(properties: Value) -> crate::EffectiveObjectType {
    let mut object_type = json!({"name": "retail.Snapshot"});
    object_type["properties"] = properties;
    effective_object(object_type)
}

fn effective_object(object_type: Value) -> crate::EffectiveObjectType {
    let mut artifact: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/entity-v2/artifact-set/retail-sku.entity.json"
    ))
    .expect("entity fixture");
    artifact["payload"]["objectType"] = object_type;
    let artifact: ArtifactEnvelope = serde_json::from_value(artifact).expect("typed entity");
    EffectiveCatalog::from_artifacts(&[artifact])
        .object_type_named("retail.Snapshot")
        .expect("snapshot type")
        .expect("effective snapshot type")
        .clone()
}

fn snapshot(member: Value) -> JsonValue {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "version": {"type": "integer"},
            "values": {
                "type": "object", "properties": {"value": null},
                "required": ["value"], "additionalProperties": false
            }
        },
        "required": ["version", "values"], "additionalProperties": false
    });
    schema["properties"]["values"]["properties"]["value"] = member;
    serde_json::from_value(schema).expect("snapshot schema")
}

fn admission_findings(schema: &JsonValue) -> Vec<norn_ir::Diagnostic> {
    let mut diagnostics = Vec::new();
    validate_closed_schema(
        Some(schema),
        "snapshot",
        0,
        &mut diagnostics,
        None,
        ClosedSchemaProfile {
            code: "rule.validation.invalid_schema",
            subject: "RuleIR",
            string_enums: true,
            unions: ClosedSchemaUnions::Nullable,
        },
    );
    diagnostics
}

#[test]
fn nullable_stored_snapshot_members_pass_schema_admission_and_entity_binding() {
    let effective = effective(json!([
        {"id": "value", "type": {"kind": "text"}, "constraints": {"nullable": true}}
    ]));
    assert!(effective.properties[0].nullable);
    let schema = snapshot(json!({"type": ["string", "null"]}));
    assert!(admission_findings(&schema).is_empty());
    assert_eq!(snapshot_values_schema_error(&schema, &effective), None);
    let required_non_null = snapshot(json!({"type": "string"}));
    assert!(snapshot_values_schema_error(&required_non_null, &effective).is_some());
    let wrong_scalar = snapshot(json!({"type": ["integer", "null"]}));
    assert!(snapshot_values_schema_error(&wrong_scalar, &effective).is_some());
}

#[test]
fn enum_snapshot_schemas_cover_effective_enum_values_including_lifecycle_states() {
    let effective = effective_object(json!({
        "name": "retail.Snapshot",
        "properties": [{
            "id": "value",
            "type": {
                "kind": "enum",
                "values": [{"value": "draft"}, {"value": "submitted"}]
            },
            "default": "draft"
        }],
        "lifecycles": [{
            "property": "value",
            "initial": ["draft"],
            "terminal": ["submitted"],
            "transitions": [{"id": "submit", "from": ["draft"], "to": "submitted"}]
        }]
    }));
    let property = &effective.properties[0];
    assert_eq!(
        property.default_value().expect("enum default"),
        Some(json!("draft"))
    );
    assert_eq!(
        effective
            .lifecycle("value")
            .expect("effective lifecycle")
            .states,
        vec!["draft".to_owned(), "submitted".to_owned()]
    );

    for member in [
        json!({"type": "string", "enum": ["draft", "submitted"]}),
        json!({"type": "string", "enum": ["draft", "submitted", "cancelled"]}),
    ] {
        let schema = snapshot(member);
        assert!(admission_findings(&schema).is_empty());
        assert_eq!(snapshot_values_schema_error(&schema, &effective), None);
    }
    let broad_string = snapshot(json!({"type": "string"}));
    assert_eq!(
        snapshot_values_schema_error(&broad_string, &effective),
        None
    );
    let narrowed = snapshot(json!({"type": "string", "enum": ["draft"]}));
    assert!(snapshot_values_schema_error(&narrowed, &effective).is_some());
}

#[test]
fn nullable_enum_snapshots_use_a_broad_nullable_schema() {
    use norn_expr::Value as ExprValue;

    let effective = effective(json!([{
        "id": "value",
        "type": {
            "kind": "enum",
            "values": [{"value": "draft"}, {"value": "submitted"}]
        },
        "constraints": {"nullable": true}
    }]));
    let broad = snapshot(json!({"type": ["string", "null"]}));
    assert!(admission_findings(&broad).is_empty());
    assert_eq!(snapshot_values_schema_error(&broad, &effective), None);
    let runtime_schema = norn_expr::schema::Schema::parse(&broad, "snapshot")
        .expect("broad nullable schema is admitted at runtime");
    for value in [ExprValue::Null, ExprValue::from("draft")] {
        let input = ExprValue::object([
            ("version", ExprValue::from(1)),
            ("values", ExprValue::object([("value", value)])),
        ]);
        runtime_schema
            .check(&input, "snapshot")
            .expect("nullable enum snapshot value is accepted");
    }

    let restricted_without_null = snapshot(json!({
        "type": ["string", "null"],
        "enum": ["draft", "submitted"]
    }));
    assert!(admission_findings(&restricted_without_null).is_empty());
    assert!(snapshot_values_schema_error(&restricted_without_null, &effective).is_some());
    let restricted_with_null = snapshot(json!({
        "type": ["string", "null"],
        "enum": ["draft", "submitted", null]
    }));
    assert!(!admission_findings(&restricted_with_null).is_empty());
    assert!(snapshot_values_schema_error(&restricted_with_null, &effective).is_some());
}

#[test]
fn unbounded_text_snapshot_schema_cannot_add_an_enum_restriction() {
    let effective = effective(json!([{"id": "value", "type": {"kind": "text"}}]));
    let broad_string = snapshot(json!({"type": "string"}));
    assert_eq!(
        snapshot_values_schema_error(&broad_string, &effective),
        None
    );
    let restricted = snapshot(json!({"type": "string", "enum": ["draft"]}));
    assert!(admission_findings(&restricted).is_empty());
    assert!(snapshot_values_schema_error(&restricted, &effective).is_some());
}

#[test]
fn boolean_snapshot_values_use_broad_boolean_schema_without_enum() {
    let effective = effective(json!([{"id": "value", "type": {"kind": "boolean"}}]));
    let broad = snapshot(json!({"type": "boolean"}));
    assert!(admission_findings(&broad).is_empty());
    assert_eq!(snapshot_values_schema_error(&broad, &effective), None);
    let restricted = snapshot(json!({"type": "boolean", "enum": [true]}));
    assert!(!admission_findings(&restricted).is_empty());
    assert!(snapshot_values_schema_error(&restricted, &effective).is_some());
}

#[test]
fn admitted_nullable_snapshot_values_are_usable_in_zen_guards() {
    use norn_expr::{EvaluationBudget, Expression, Value as ExprValue};

    let schema = snapshot(json!({"type": ["string", "null"]}));
    let schema = norn_expr::schema::Schema::parse(&schema, "before").expect("runtime schema");
    for (value, expected_null, expected_text) in [
        (ExprValue::Null, true, ""),
        (ExprValue::from("Size"), false, "Size"),
    ] {
        let before = ExprValue::object([
            ("version", ExprValue::from(1)),
            ("values", ExprValue::object([("value", value)])),
        ]);
        schema.check(&before, "before").expect("typed snapshot");
        let context = ExprValue::object([("before", before)]);
        for (expression, expected) in [
            (
                "before.values.value == null",
                ExprValue::Bool(expected_null),
            ),
            ("before.values.value ?? ''", ExprValue::from(expected_text)),
        ] {
            assert_eq!(
                Expression::compile(expression)
                    .expect("ZEN expression")
                    .evaluate(&context, EvaluationBudget::default())
                    .expect("evaluate admitted snapshot")
                    .into_value(),
                expected
            );
        }
    }
}

#[test]
fn nullable_rule_schema_admission_refuses_arbitrary_unions_and_open_records() {
    for member in [
        json!({"type": ["string", "integer"]}),
        json!({"type": ["string", "null", "boolean"]}),
        json!({"type": ["null", "null"]}),
        json!({"type": ["string", "string"]}),
        json!({"type": ["unknown", "null"]}),
        json!({"type": ["object", "null"], "properties": {}, "additionalProperties": true}),
        json!({"type": ["array", "null"]}),
    ] {
        assert!(!admission_findings(&snapshot(member)).is_empty());
    }
    let root: JsonValue = serde_json::from_value(json!({
        "type": ["object", "null"], "properties": {}, "additionalProperties": false
    }))
    .expect("root schema");
    assert!(!admission_findings(&root).is_empty());
}

fn json_contract() -> Value {
    json!({
        "type": "object", "properties": {"size": {"type": "string"}},
        "required": ["size"], "additionalProperties": false
    })
}

#[test]
fn json_extension_snapshots_require_the_actual_declared_closed_contract() {
    let effective = effective(json!([
        {"id": "value", "type": {"kind": "json-ext", "schema": json_contract()}}
    ]));
    let schema = snapshot(json_contract());
    assert!(admission_findings(&schema).is_empty());
    assert_eq!(snapshot_values_schema_error(&schema, &effective), None);
    let scalar = snapshot(json!({"type": "boolean"}));
    assert!(snapshot_values_schema_error(&scalar, &effective).is_some());
    let mut wrong_nested_type = json_contract();
    wrong_nested_type["properties"]["size"] = json!({"type": "integer"});
    assert!(snapshot_values_schema_error(&snapshot(wrong_nested_type), &effective).is_some());
    let mut missing_required = json_contract();
    missing_required["required"] = json!([]);
    assert!(snapshot_values_schema_error(&snapshot(missing_required), &effective).is_some());
    let mut wrong_nullability = json_contract();
    wrong_nullability["type"] = json!(["object", "null"]);
    assert!(snapshot_values_schema_error(&snapshot(wrong_nullability), &effective).is_some());
}

#[test]
fn nullable_json_extension_snapshots_keep_the_closed_non_null_shape() {
    let effective = effective(json!([
        {"id": "value", "type": {"kind": "json-ext", "schema": json_contract()}, "constraints": {"nullable": true}}
    ]));
    assert!(effective.properties[0].nullable);
    let mut nullable = json_contract();
    nullable["type"] = json!(["null", "object"]);
    let schema = snapshot(nullable);
    assert!(admission_findings(&schema).is_empty());
    assert_eq!(snapshot_values_schema_error(&schema, &effective), None);
    assert!(snapshot_values_schema_error(&snapshot(json_contract()), &effective).is_some());
}

#[test]
fn opaque_or_unsupported_json_contracts_fail_closed_for_snapshots() {
    for property_type in [
        json!({"kind": "json-ext"}),
        json!({"kind": "json-ext", "schema": {"type": "object"}}),
        json!({"kind": "json-ext", "schema": {"type": "null"}}),
    ] {
        let effective = effective(json!([{"id": "value", "type": property_type}]));
        assert!(snapshot_values_schema_error(&snapshot(json_contract()), &effective).is_some());
    }
}

fn trigger_payload(trigger: Value) -> std::collections::BTreeMap<String, JsonValue> {
    let mut artifact: Value = serde_json::from_str(include_str!(
        "../../../fixtures/rule-ir-v2/valid/credit-hold.rule.json"
    ))
    .expect("rule fixture");
    artifact["payload"]["triggers"] = Value::Array(vec![trigger]);
    serde_json::from_value(artifact["payload"].take()).expect("lenient IR payload")
}

#[test]
fn bounded_object_triggers_survive_lenient_ir_round_trips_and_checker_admission() {
    for on in ["transitioned", "created", "deleted", "modified"] {
        for depth in [1, 8] {
            let mut trigger = json!({
                "on": on, "objectType": "commerce.SalesOrder", "maxCausalDepth": depth
            });
            if on == "transitioned" {
                trigger["transition"] = json!("submit");
            }
            let payload = trigger_payload(trigger);
            let mut diagnostics = Vec::new();
            validate_rule_bindings(&payload, &mut diagnostics, None);
            assert!(diagnostics.is_empty(), "{on}/{depth}: {diagnostics:?}");
            let root: norn_ir::rule::RuleRoot =
                serde_json::from_value(serde_json::to_value(&payload).expect("IR round trip"))
                    .expect("typed root after binary64 IR serialization");
            assert_eq!(root.triggers[0].max_causal_depth(), Some(depth));
        }
    }
}

#[test]
fn malformed_or_out_of_bounds_trigger_depths_fail_closed() {
    for depth in [
        json!(null),
        json!(0),
        json!(9),
        json!(-1),
        json!(1.5),
        json!(256),
        json!("1"),
        json!(true),
    ] {
        let payload = trigger_payload(json!({
            "on": "modified", "objectType": "commerce.SalesOrder", "maxCausalDepth": depth
        }));
        let mut diagnostics = Vec::new();
        validate_rule_bindings(&payload, &mut diagnostics, None);
        assert!(
            diagnostics
                .iter()
                .any(|finding| finding.code == super::codes::INVALID_BINDING),
            "{depth}: {diagnostics:?}"
        );
    }
}

#[test]
fn omitted_trigger_depth_remains_optional_and_null_property_filters_remain_invalid() {
    let payload = trigger_payload(json!({"on": "modified", "objectType": "commerce.SalesOrder"}));
    let mut diagnostics = Vec::new();
    validate_rule_bindings(&payload, &mut diagnostics, None);
    assert!(diagnostics.is_empty());
    let root: norn_ir::rule::RuleRoot =
        serde_json::from_value(serde_json::to_value(&payload).expect("IR round trip"))
            .expect("omitted depth");
    assert_eq!(root.triggers[0].max_causal_depth(), None);

    let payload = trigger_payload(json!({
        "on": "modified", "objectType": "commerce.SalesOrder", "changedProperties": null
    }));
    diagnostics.clear();
    validate_rule_bindings(&payload, &mut diagnostics, None);
    assert!(
        diagnostics
            .iter()
            .any(|finding| finding.code == super::codes::INVALID_BINDING)
    );
}

#[test]
fn record_snapshots_require_complete_recursive_member_contracts() {
    let effective = effective(json!([{"id":"value","type":{"kind":"record","members":[
        {"id":"amount","type":{"kind":"decimal","scale":2}},
        {"id":"details","type":{"kind":"record","members":[
            {"id":"label","type":{"kind":"text"},"constraints":{"nullable":true}}
        ]}}
    ]}}]));
    let valid = json!({"type":"object","additionalProperties":false,"required":["amount","details"],"properties":{
        "amount":{"type":"number"},
        "details":{"type":"object","additionalProperties":false,"required":["label"],"properties":{"label":{"type":["string","null"]}}}
    }});
    assert_eq!(
        snapshot_values_schema_error(&snapshot(valid.clone()), &effective),
        None
    );
    let mut missing = valid.clone();
    missing["properties"]
        .as_object_mut()
        .expect("members")
        .remove("amount");
    let mut optional = valid.clone();
    optional["required"] = json!(["details"]);
    let mut wrong = valid.clone();
    wrong["properties"]["amount"]["type"] = json!("string");
    let mut nested = valid.clone();
    nested["properties"]["details"]["properties"] = json!({});
    let mut nullability = valid;
    nullability["properties"]["details"]["properties"]["label"]["type"] = json!("string");
    for invalid in [
        missing,
        optional,
        wrong,
        nested,
        nullability,
        json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
    ] {
        assert!(snapshot_values_schema_error(&snapshot(invalid), &effective).is_some());
    }
}
