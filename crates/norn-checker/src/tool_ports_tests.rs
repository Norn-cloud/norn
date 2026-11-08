use serde_json::json;

use super::*;

fn port(schema: &serde_json::Value) -> PortType {
    ports_from_schema(schema).expect("supported schema")
}

fn refused(schema: &serde_json::Value) -> Unsupported {
    ports_from_schema(schema).expect_err("unsupported schema")
}

#[test]
fn a_typical_tool_input_lowers_to_a_record() {
    let input = port(&json!({
        "type": "object",
        "properties": {
            "repo": {"type": "string"},
            "state": {"enum": ["open", "closed"]},
            "limit": {"type": "integer"},
            "since": {"type": "string", "format": "date-time"},
            "labels": {"type": "array", "items": {"type": "string"}},
            "assignee": {"type": ["string", "null"]}
        },
        "required": ["repo"],
        "additionalProperties": false
    }));
    assert_eq!(
        input.to_json(),
        json!({"kind": "record", "open": false, "fields": [
            {"name": "assignee", "type": {"kind": "nullable", "inner": {"kind": "string"}}, "required": false},
            {"name": "labels", "type": {"kind": "list", "item": {"kind": "string"}}, "required": false},
            {"name": "limit", "type": {"kind": "integer"}, "required": false},
            {"name": "repo", "type": {"kind": "string"}, "required": true},
            {"name": "since", "type": {"kind": "datetime"}, "required": false},
            {"name": "state", "type": {"kind": "enum", "values": ["closed", "open"]}, "required": false}
        ]})
    );
}

#[test]
fn local_references_resolve_and_nullable_unions_collapse() {
    let schema = json!({
        "type": "object",
        "properties": {"owner": {"anyOf": [{"$ref": "#/$defs/user"}, {"type": "null"}]}},
        "$defs": {"user": {"type": "object", "properties": {"login": {"type": "string"}}}}
    });
    let PortType::Record { fields, .. } = port(&schema) else {
        panic!("record")
    };
    assert!(matches!(fields["owner"].port, PortType::Nullable(_)));
}

#[test]
fn typed_maps_lower_and_free_objects_are_refused() {
    assert_eq!(
        port(&json!({"type": "object", "additionalProperties": {"type": "number"}})),
        PortType::Map(Box::new(PortType::Number))
    );
    assert_eq!(refused(&json!({"type": "object"})).reason, Reason::Untyped);
    assert_eq!(refused(&json!({})).reason, Reason::Untyped);
}

#[test]
fn refusals_name_the_subschema_and_the_reason() {
    let unsupported = refused(&json!({
        "type": "object",
        "properties": {"a/b": {"oneOf": [{"type": "string"}, {"type": "integer"}]}}
    }));
    assert_eq!(unsupported.path, "/properties/a~1b/oneOf");
    assert_eq!(unsupported.reason.code(), "tool_port.union");
    assert_eq!(
        refused(&json!({"type": "array"})).reason,
        Reason::UntypedItems
    );
    assert_eq!(
        refused(&json!({"enum": [1, 2]})).reason,
        Reason::NonStringEnum
    );
    assert_eq!(
        refused(&json!({"allOf": []})).reason,
        Reason::Combinator("allOf".to_owned())
    );
    assert_eq!(
        refused(&json!({"$ref": "https://example.com/schema"})).reason,
        Reason::Reference("https://example.com/schema".to_owned())
    );
}

#[test]
fn recursive_references_are_refused() {
    let schema = json!({
        "$ref": "#/$defs/node",
        "$defs": {"node": {"type": "object", "properties": {"next": {"$ref": "#/$defs/node"}}}}
    });
    assert_eq!(
        refused(&schema).reason,
        Reason::Recursive("#/$defs/node".to_owned())
    );
}

#[test]
fn edges_widen_but_never_narrow() {
    use PortType::{Datetime, Integer, Number, String};
    assert_eq!(edge_refusal(&Integer, &Number), None);
    assert_eq!(edge_refusal(&Datetime, &String), None);
    let refusal = edge_refusal(&Number, &Integer).expect("narrowing");
    assert_eq!(
        (
            refusal.code,
            refusal.expected.as_str(),
            refusal.found.as_str()
        ),
        ("edge.type_mismatch", "integer", "number")
    );
    let nullable = PortType::Nullable(Box::new(String));
    assert_eq!(edge_refusal(&String, &nullable), None);
    assert_eq!(
        edge_refusal(&nullable, &String).expect("nullable").code,
        "edge.nullable"
    );
    let small = PortType::Enum(vec!["a".into()]);
    let big = PortType::Enum(vec!["a".into(), "b".into()]);
    assert_eq!(edge_refusal(&small, &big), None);
    assert!(edge_refusal(&big, &small).is_some());
}

#[test]
fn record_edges_check_required_fields_by_path() {
    let target = port(&json!({
        "type": "object",
        "properties": {"issue": {"type": "object", "properties": {"title": {"type": "string"}}, "required": ["title"]}},
        "required": ["issue"]
    }));
    let optional_title = port(&json!({
        "type": "object",
        "properties": {"issue": {"type": "object", "properties": {"title": {"type": "string"}}}},
        "required": ["issue"]
    }));
    let refusal = edge_refusal(&optional_title, &target).expect("optional title");
    assert_eq!(
        (refusal.path.as_str(), refusal.code),
        ("issue.title", "edge.missing_field")
    );
    let empty = port(&json!({"type": "object", "properties": {"other": {"type": "string"}}}));
    assert_eq!(
        edge_refusal(&empty, &target).expect("absent").found,
        "absent"
    );
    assert_eq!(edge_refusal(&target, &target), None);
}

#[test]
fn a_null_enum_member_counts_only_where_the_type_admits_null() {
    assert_eq!(
        port(&json!({"type": "string", "enum": ["a", null]})),
        PortType::Enum(vec!["a".to_owned()])
    );
    assert_eq!(
        port(&json!({"type": ["string", "null"], "enum": ["a", null]})),
        PortType::Nullable(Box::new(PortType::Enum(vec!["a".to_owned()])))
    );
    assert_eq!(
        port(&json!({"enum": ["a", null]})),
        PortType::Nullable(Box::new(PortType::Enum(vec!["a".to_owned()])))
    );
}

#[test]
fn reference_fan_out_is_bounded_by_the_node_budget() {
    // Four references per level, sixteen levels: 4^16 leaves within the depth limit.
    let mut defs = serde_json::Map::new();
    for level in 1_u32..16 {
        let next = format!("#/$defs/{}", level.saturating_add(1));
        let properties: serde_json::Map<String, serde_json::Value> = ["a", "b", "c", "d"]
            .iter()
            .map(|name| ((*name).to_owned(), json!({ "$ref": next })))
            .collect();
        defs.insert(
            level.to_string(),
            json!({"type": "object", "properties": properties}),
        );
    }
    defs.insert("16".to_owned(), json!({"type": "string"}));
    let schema = json!({"$ref": "#/$defs/1", "$defs": defs});
    assert_eq!(refused(&schema).reason, Reason::TooLarge);
}
