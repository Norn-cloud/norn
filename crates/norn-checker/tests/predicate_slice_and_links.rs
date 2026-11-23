#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "integration-test fixtures for predicate slices and decision links"
)]

//! Admission checks for `SliceKey::Predicate` and `DecisionOutput.links` (#1586).

use std::collections::BTreeSet;

use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

fn envelope(
    kind: &str,
    artifact_id: &str,
    payload: &Value,
    dependencies: &Value,
) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": kind,
        "namespace": "schedule",
        "name": artifact_id,
        "version": "1.0.0",
        "lifecycle": "draft",
        "scope": {"orgId": "org_demo", "environmentId": "dev"},
        "dependencies": dependencies,
        "provenance": {"source": "authoring_sdk", "sourcePath": "test.ts"},
        "payload": payload
    }))
    .expect("valid envelope")
}

fn entity_artifacts() -> Vec<ArtifactEnvelope> {
    let agent = envelope(
        "entity",
        "schedule.Agent.entity",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "schedule.Agent",
                "properties": [
                    {"id": "agent_id", "type": {"kind": "text"}}
                ]
            }
        }),
        &json!([]),
    );
    let plan = envelope(
        "entity",
        "schedule.Plan.entity",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "schedule.Plan",
                "properties": [
                    {"id": "plan_id", "type": {"kind": "text"}},
                    {"id": "status", "type": {"kind": "enum", "values": [{"value": "draft"}, {"value": "committed"}]}, "default": "draft"}
                ],
                "lifecycles": [{
                    "property": "status",
                    "initial": ["draft"],
                    "terminal": ["committed"],
                    "transitions": [{"id": "commit_plan", "from": ["draft"], "to": "committed"}]
                }]
            }
        }),
        &json!([]),
    );
    let commitment = envelope(
        "entity",
        "schedule.Commitment.entity",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "schedule.Commitment",
                "properties": [
                    {"id": "commitment_id", "type": {"kind": "text"}},
                    {"id": "agent_id", "type": {"kind": "text"}},
                    {"id": "starts_at", "type": {"kind": "timestamp", "timezone": "utc"}},
                    {"id": "ends_at", "type": {"kind": "timestamp", "timezone": "utc"}},
                    {"id": "derived_label", "type": {"kind": "text"}, "computed": "self.agent_id"}
                ],
                "links": [
                    {"id": "for_agent", "target": "schedule.Agent", "cardinality": {"to": "one", "from": "many"}}
                ]
            }
        }),
        &json!([]),
    );
    vec![agent, plan, commitment]
}

fn rule_artifact(rule_links_schema: Option<Value>) -> ArtifactEnvelope {
    let mut output_props = serde_json::Map::from_iter([
        (
            "facts".to_owned(),
            json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "op": {"type": "string", "enum": ["create"]},
                        "objectType": {"type": "string", "enum": ["schedule.Commitment"]},
                        "key": {"type": "string"},
                        "values": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "commitment_id": {"type": "string"},
                                "agent_id": {"type": "string"},
                                "starts_at": {"type": "string"},
                                "ends_at": {"type": "string"}
                            },
                            "required": ["commitment_id", "agent_id", "starts_at", "ends_at"]
                        }
                    },
                    "required": ["op", "objectType", "key", "values"]
                }
            }),
        ),
        (
            "commands".to_owned(),
            json!({"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}}),
        ),
        (
            "timers".to_owned(),
            json!({"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}}),
        ),
    ]);
    let mut required = vec!["facts", "commands", "timers"];
    if let Some(links_schema) = rule_links_schema {
        output_props.insert("links".to_owned(), links_schema);
        required.push("links");
    }
    envelope(
        "rule",
        "schedule.batch.rule",
        &json!({
            "ruleId": "schedule.batch",
            "ruleVersion": "2.0",
            "contract": "decide",
            "scopeRequired": ["orgId", "environmentId"],
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "slice": {"type": "object", "additionalProperties": false, "properties": {}},
                    "event": {"type": "object", "additionalProperties": false, "properties": {}}
                },
                "required": ["slice", "event"]
            },
            "outputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": output_props,
                "required": required
            },
            "graph": {
                "contentType": "application/vnd.gorules.decision",
                "nodes": [
                    {"id": "in", "name": "input", "type": "inputNode"},
                    {"id": "decision", "name": "decision", "type": "decisionTableNode", "content": {"hitPolicy": "first", "inputs": [{"id": "condition", "name": "condition", "type": "expression"}], "outputs": [{"id": "out_facts", "name": "facts", "field": "facts"}, {"id": "out_commands", "name": "commands", "field": "commands"}, {"id": "out_timers", "name": "timers", "field": "timers"}], "rules": [{"_id": "row_0", "condition": "", "out_facts": "[]", "out_commands": "[]", "out_timers": "[]"}]}},
                    {"id": "out", "name": "output", "type": "outputNode"}
                ],
                "edges": [
                    {"id": "edge_0", "sourceId": "in", "targetId": "decision", "type": "edge"},
                    {"id": "edge_1", "sourceId": "decision", "targetId": "out", "type": "edge"}
                ]
            }
        }),
        &json!([]),
    )
}

fn base_artifacts(slice_entry: &Value, rule_links_schema: Option<Value>) -> Vec<ArtifactEnvelope> {
    let mut artifacts = entity_artifacts();
    artifacts.push(rule_artifact(rule_links_schema));
    artifacts.push(envelope(
        "action",
        "schedule.plan.commit.action",
        &json!({
            "actionId": "schedule.plan.commit",
            "title": "Commit Plan",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "plan_id": {"type": "string"},
                    "agent_id": {"type": "string"},
                    "window_start": {"type": "string"},
                    "window_end": {"type": "string"},
                    "count_limit": {"type": "integer"}
                },
                "required": ["plan_id", "agent_id", "window_start", "window_end", "count_limit"]
            },
            "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
            "slice": [
                {
                    "name": "agent",
                    "objectType": "schedule.Agent",
                    "key": {"property": {"from": "input", "property": "agent_id"}}
                },
                slice_entry
            ],
            "preconditions": ["len(slice.existing) >= 0"],
            "writes": [
                {"entityKind": "schedule.Plan", "change": "modified"},
                {"entityKind": "schedule.Commitment", "change": "created"}
            ],
            "synchronousRule": {"artifactId": "schedule.batch.rule", "version": "1.0.0"},
            "effectProgram": {
                "kind": "fire_transition",
                "objectKind": "schedule.Plan",
                "objectIdField": "plan_id",
                "entityArtifactId": "schedule.Plan.entity",
                "entityArtifactVersion": "1.0.0",
                "lifecycle": "status",
                "transition": "commit_plan"
            }
        }),
        &json!([{"artifactId": "schedule.batch.rule", "kind": "rule", "version": "1.0.0", "required": true}]),
    ));
    artifacts
}

fn valid_predicate_slice() -> Value {
    json!({
        "name": "existing",
        "objectType": "schedule.Commitment",
        "key": {
            "predicate": {
                "filter": {"agent_id": "agent_id"},
                "window": {
                    "property": "starts_at",
                    "start": "window_start",
                    "end": "window_end",
                    "startInclusive": true,
                    "endInclusive": false
                },
                "maxItems": 25
            }
        }
    })
}

fn valid_links_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "op": {"type": "string", "enum": ["add"]},
                "sourceType": {"type": "string", "enum": ["schedule.Commitment"]},
                "sourceKey": {"type": "string"},
                "link": {"type": "string", "enum": ["for_agent"]},
                "targetType": {"type": "string", "enum": ["schedule.Agent"]},
                "targetKey": {"type": "string"}
            },
            "required": ["op", "sourceType", "sourceKey", "link", "targetType", "targetKey"]
        }
    })
}

#[test]
fn admits_valid_predicate_slice_and_checked_decision_links() {
    let registry = BTreeSet::new();
    let ctx = EntityArtifactSetContext {
        base: &[],
        registry: &registry,
        tenant: None,
    };
    let artifacts = base_artifacts(&valid_predicate_slice(), Some(valid_links_schema()));
    let findings = check_artifact_set(&artifacts, &ctx);
    assert_eq!(findings, [], "{findings:?}");
}

#[test]
fn rejects_computed_property_type_mismatch_optional_and_out_of_range_predicate_slices() {
    let registry = BTreeSet::new();
    let ctx = EntityArtifactSetContext {
        base: &[],
        registry: &registry,
        tenant: None,
    };

    let computed = json!({
        "name": "existing",
        "objectType": "schedule.Commitment",
        "key": {"predicate": {"filter": {"derived_label": "agent_id"}}}
    });
    assert!(
        !check_artifact_set(&base_artifacts(&computed, None), &ctx).is_empty(),
        "computed property must fail closed"
    );

    let mismatch = json!({
        "name": "existing",
        "objectType": "schedule.Commitment",
        "key": {"predicate": {"filter": {"agent_id": "count_limit"}}}
    });
    assert!(
        !check_artifact_set(&base_artifacts(&mismatch, None), &ctx).is_empty(),
        "type mismatch must fail closed"
    );

    let optional = json!({
        "name": "existing",
        "objectType": "schedule.Commitment",
        "optional": true,
        "key": {"predicate": {"filter": {"agent_id": "agent_id"}}}
    });
    assert!(
        !check_artifact_set(&base_artifacts(&optional, None), &ctx).is_empty(),
        "optional predicate slice must fail closed"
    );

    for bad_cap in [0_u32, 1001_u32] {
        let capped = json!({
            "name": "existing",
            "objectType": "schedule.Commitment",
            "key": {"predicate": {"filter": {"agent_id": "agent_id"}, "maxItems": bad_cap}}
        });
        assert!(
            !check_artifact_set(&base_artifacts(&capped, None), &ctx).is_empty(),
            "maxItems={bad_cap} must fail closed"
        );
    }
}

#[test]
fn rejects_undeclared_or_uncaptured_decision_links() {
    let registry = BTreeSet::new();
    let ctx = EntityArtifactSetContext {
        base: &[],
        registry: &registry,
        tenant: None,
    };
    let valid_slice = valid_predicate_slice();

    let mut bad_link = valid_links_schema();
    bad_link["items"]["properties"]["link"]["enum"] = json!(["nonexistent_link"]);
    assert!(
        !check_artifact_set(&base_artifacts(&valid_slice, Some(bad_link)), &ctx).is_empty(),
        "undeclared link must fail closed"
    );

    let mut no_enum = valid_links_schema();
    no_enum["items"]["properties"]["link"] = json!({"type": "string"});
    assert!(
        !check_artifact_set(&base_artifacts(&valid_slice, Some(no_enum)), &ctx).is_empty(),
        "unbounded link schema must fail closed"
    );

    let mut remove_with_values = valid_links_schema();
    remove_with_values["items"]["properties"]["op"]["enum"] = json!(["remove"]);
    remove_with_values["items"]["properties"]["values"] =
        json!({"type": "object", "additionalProperties": false, "properties": {}});
    assert!(
        !check_artifact_set(
            &base_artifacts(&valid_slice, Some(remove_with_values)),
            &ctx
        )
        .is_empty(),
        "values on remove-only links must fail closed"
    );
}
