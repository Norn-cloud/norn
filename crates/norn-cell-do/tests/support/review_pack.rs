//! PRINCIPAL-INPUT-1 (#1091): a synthetic review pack, shared by the hosted
//! cell tests and the `norn dev` tests. A document is recorded, then approved
//! by an action whose pinned decision writes `confirmed = principal.isAdmin`.
use serde_json::{Value, json};

pub(crate) const DOCUMENT: &str = "review.Document";
pub(crate) const APPROVE: &str = "review.document.approve";

pub(crate) fn review_artifact(
    kind: &str,
    id: &str,
    version: &str,
    dependencies: &Value,
    payload: &Value,
) -> Value {
    json!({ "artifactId": id, "dependencies": dependencies, "irVersion": "0.1", "kind": kind,
        "lifecycle": "active", "name": id, "namespace": "review", "payload": payload,
        "provenance": { "source": "system", "sourcePath": "tests/support/review_pack.rs" },
        "scope": {}, "version": version })
}

/// The document type and the access policy, with `catalog` as its principal
/// attribute catalog, and the action that records a document.
pub(crate) fn review_base(catalog: &Value) -> Vec<Value> {
    let entity = json!({ "entityVersion": "2.0", "objectType": {
        "identity": [{ "name": "primary", "properties": ["id"] }],
        "name": DOCUMENT,
        "properties": [
            { "id": "id", "type": { "kind": "text" } },
            { "id": "owner", "type": { "kind": "text" } },
            { "id": "confirmed", "type": { "kind": "boolean" }, "default": false },
            { "id": "status", "type": { "kind": "enum",
                "values": [{ "value": "draft" }, { "value": "approved" }] }, "default": "draft" }
        ],
        "lifecycles": [{ "property": "status", "initial": ["draft"], "terminal": ["approved"],
            "transitions": [{ "id": "approve", "from": ["draft"], "to": "approved" }] }] } });
    let policy = json!({ "accessVersion": "0.1", "policyId": "review.access",
        "rules": [{ "ruleId": "staff", "actions": ["review.document.record", APPROVE],
            "resourceType": DOCUMENT, "scope": { "dimensions": ["org", "environment"] },
            "allOf": [{ "role": { "anyOf": ["member", "admin", "owner"] } }] }],
        "principalCatalog": catalog });
    let record = json!({ "actionId": "review.document.record", "title": "Record a document",
        "description": "Record a document for review.",
        "scopeRequired": ["orgId", "environmentId"], "idempotent": true, "ledgered": true,
        "access": { "artifactId": "review.access.policy", "version": "0.1.0" },
        "inputSchema": { "type": "object", "additionalProperties": false,
            "required": ["id", "owner"],
            "properties": { "id": { "type": "string", "title": "Document number" },
                "owner": { "type": "string", "title": "Owner" } } },
        "outputSchema": { "type": "object", "additionalProperties": false, "required": ["id"],
            "properties": { "id": { "type": "string" } } },
        "effectProgram": { "kind": "upsert_input_object", "entityArtifactId": "review.Document.entity",
            "entityArtifactVersion": "0.1.0", "objectKind": DOCUMENT, "objectIdField": "id" },
        "writes": [{ "change": "created", "entityKind": DOCUMENT },
            { "change": "modified", "entityKind": DOCUMENT }] });
    vec![
        review_artifact(
            "entity",
            "review.Document.entity",
            "0.1.0",
            &json!([]),
            &entity,
        ),
        review_artifact(
            "policy",
            "review.access.policy",
            "0.1.0",
            &json!([]),
            &policy,
        ),
        review_artifact(
            "action",
            "review.document.record.action",
            "0.1.0",
            &json!([]),
            &record,
        ),
    ]
}

pub(crate) fn admin_role() -> Value {
    json!([{ "name": "admin", "type": "bool", "source": { "kind": "role" } }])
}

/// The approval action at `version`, pinning the decision at `rule` if any.
pub(crate) fn approve_action(version: &str, rule: Option<&str>) -> Value {
    let mut payload = json!({ "actionId": APPROVE, "title": "Approve a document",
        "description": "Approve a recorded document.",
        "scopeRequired": ["orgId", "environmentId"], "idempotent": true, "ledgered": true,
        "access": { "artifactId": "review.access.policy", "version": "0.1.0" },
        "inputSchema": { "type": "object", "additionalProperties": false, "required": ["id"],
            "properties": { "id": { "type": "string", "title": "Document number" } } },
        "outputSchema": { "type": "object", "additionalProperties": false, "required": ["id"],
            "properties": { "id": { "type": "string" } } },
        "effectProgram": { "kind": "fire_transition", "entityArtifactId": "review.Document.entity",
            "entityArtifactVersion": "0.1.0", "objectKind": DOCUMENT, "objectIdField": "id",
            "lifecycle": "status", "transition": "approve" },
        "writes": [{ "change": "modified", "entityKind": DOCUMENT }] });
    let mut dependencies = json!([]);
    if let Some(rule) = rule {
        payload["synchronousRule"] =
            json!({ "artifactId": "review.approval.decide.rule", "version": rule });
        dependencies = json!([{ "artifactId": "review.approval.decide.rule", "kind": "rule",
            "required": true, "version": rule }]);
    }
    review_artifact(
        "action",
        "review.document.approve.action",
        version,
        &dependencies,
        &payload,
    )
}

/// The approval decision at `version`: with principal inputs it writes
/// `confirmed = principal.isAdmin`; without, it declares and reads nothing
/// about the principal and writes `confirmed = false`.
pub(crate) fn approval_rule(version: &str, principal_inputs: bool) -> Value {
    let empty = json!({ "type": "array", "items": { "type": "object",
        "additionalProperties": false, "properties": {} } });
    let event = json!({ "type": "object", "additionalProperties": false,
        "required": ["input", "actor", "operationId"],
        "properties": { "actor": { "type": "string" }, "operationId": { "type": "string" },
            "input": { "type": "object", "additionalProperties": false, "required": ["id"],
                "properties": { "id": { "type": "string" } } } } });
    let value = if principal_inputs {
        "principal.isAdmin"
    } else {
        "false"
    };
    let mut payload = json!({ "ruleVersion": "2.0", "ruleId": "review.approval.decide",
        "contract": "decide", "scopeRequired": ["orgId", "environmentId"],
        "inputSchema": { "type": "object", "additionalProperties": false,
            "required": ["slice", "event"],
            "properties": { "event": event,
                "slice": { "type": "object", "additionalProperties": false, "properties": {} } } },
        "outputSchema": { "type": "object", "additionalProperties": false,
            "required": ["facts", "commands", "timers"],
            "properties": { "commands": empty, "timers": empty,
                "facts": { "type": "array", "items": { "type": "object",
                    "additionalProperties": false,
                    "required": ["op", "objectType", "key", "property", "value"],
                    "properties": { "op": { "type": "string", "enum": ["set"] },
                        "objectType": { "type": "string", "enum": [DOCUMENT] },
                        "key": { "type": "string" },
                        "property": { "type": "string", "enum": ["confirmed"] },
                        "value": { "type": "boolean" } } } } } },
        "graph": { "contentType": "application/vnd.gorules.decision",
            "nodes": [
                { "id": "in", "type": "inputNode", "name": "input" },
                { "id": "decision", "type": "decisionTableNode", "name": "decision", "content": {
                    "hitPolicy": "first",
                    "inputs": [{ "id": "condition", "name": "condition", "type": "expression" }],
                    "outputs": [
                        { "field": "facts", "id": "out_facts", "name": "facts" },
                        { "field": "commands", "id": "out_commands", "name": "commands" },
                        { "field": "timers", "id": "out_timers", "name": "timers" }],
                    "rules": [{ "_id": "row_0", "condition": "",
                        "out_facts": format!("[{{op: 'set', objectType: 'review.Document', key: event.input.id, property: 'confirmed', value: {value}}}]"),
                        "out_commands": "[]", "out_timers": "[]" }] } },
                { "id": "out", "type": "outputNode", "name": "output" }],
            "edges": [
                { "id": "edge_0", "sourceId": "in", "targetId": "decision", "type": "edge" },
                { "id": "edge_1", "sourceId": "decision", "targetId": "out", "type": "edge" }] } });
    if principal_inputs {
        payload["principalInputs"] = json!([{ "name": "isAdmin",
            "kind": { "holdsRole": { "role": "admin", "at": "resource" } } }]);
        payload["inputSchema"]["required"] = json!(["slice", "event", "principal"]);
        payload["inputSchema"]["properties"]["principal"] = json!({ "type": "object",
            "additionalProperties": false, "required": ["isAdmin"],
            "properties": { "isAdmin": { "type": "boolean" } } });
    }
    review_artifact(
        "rule",
        "review.approval.decide.rule",
        version,
        &json!([]),
        &payload,
    )
}
