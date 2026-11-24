//! PRINCIPAL-INPUT-1 (#1091): the admission matrix of a `decide` rule's
//! declared principal inputs and the principal attribute catalog.

use std::collections::BTreeSet;

use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const INVALID: &str = "rule.validation.principal_input_invalid";
const UNDECLARED: &str = "rule.validation.principal_input_undeclared";
const UNKNOWN: &str = "artifact_set.decision.principal_input_unknown";
const TYPE: &str = "artifact_set.decision.principal_input_type";
const CONFLICT: &str = "artifact_set.policy.principal_catalog_conflict";
const POLICY: &str = "policy.validation.invalid_payload";

fn envelope(kind: &str, artifact_id: &str, payload: &Value) -> TestResult<ArtifactEnvelope> {
    Ok(serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": kind,
        "namespace": "expense",
        "name": artifact_id,
        "version": "1.0.0",
        "lifecycle": "draft",
        "scope": {"orgId": "org_demo", "environmentId": "dev"},
        "dependencies": [],
        "provenance": {"source": "authoring_sdk", "sourcePath": "test.ts"},
        "payload": payload
    }))?)
}

fn entity() -> TestResult<ArtifactEnvelope> {
    envelope(
        "entity",
        "expense.Expense.entity",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "expense.Expense",
                "properties": [
                    {"id": "expense_id", "type": {"kind": "text"}},
                    {"id": "approver_confirmed", "type": {"kind": "boolean"}, "default": false}
                ]
            }
        }),
    )
}

fn catalog_entries() -> Value {
    json!([
        {"name": "approver", "type": "bool", "source": {"kind": "role"}},
        {"name": "finance", "type": "stringSet",
         "source": {"kind": "roleProfile", "roles": ["approver", "auditor"]}},
        {"name": "id", "type": "string", "source": {"kind": "attribute"}},
        {"name": "level", "type": "long", "source": {"kind": "attribute"}}
    ])
}

fn policy(artifact_id: &str, entries: &Value) -> TestResult<ArtifactEnvelope> {
    envelope(
        "policy",
        artifact_id,
        &json!({
            "accessVersion": "0.1",
            "policyId": artifact_id,
            "rules": [],
            "principalCatalog": entries
        }),
    )
}

fn empty_list() -> Value {
    json!({"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}})
}

/// A decide rule that reads `principal.isApprover` and declares three inputs.
fn rule_payload() -> Value {
    json!({
        "ruleVersion": "2.0",
        "ruleId": "expense.approval.decide",
        "contract": "decide",
        "scopeRequired": ["orgId", "environmentId"],
        "principalInputs": [
            {"name": "isApprover", "kind": {"holdsRole": {"role": "approver", "at": "resource"}}},
            {"name": "financeRoles", "kind": {"roleProfile": {"profile": "finance"}}},
            {"name": "actorId", "kind": {"attribute": {"name": "id"}}}
        ],
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "required": ["slice", "event", "principal"],
            "properties": {
                "slice": {"type": "object", "additionalProperties": false, "properties": {}},
                "event": {
                    "type": "object", "additionalProperties": false, "required": ["expense_id"],
                    "properties": {"expense_id": {"type": "string"}}
                },
                "principal": {
                    "type": "object", "additionalProperties": false,
                    "required": ["isApprover", "financeRoles", "actorId"],
                    "properties": {
                        "isApprover": {"type": "boolean"},
                        "financeRoles": {"type": "array", "items": {"type": "string"}},
                        "actorId": {"type": "string"}
                    }
                }
            }
        },
        "outputSchema": {
            "type": "object", "additionalProperties": false,
            "required": ["facts", "commands", "timers"],
            "properties": {
                "commands": empty_list(),
                "timers": empty_list(),
                "facts": {"type": "array", "items": {
                    "type": "object", "additionalProperties": false,
                    "required": ["op", "objectType", "key", "property", "value"],
                    "properties": {
                        "op": {"type": "string", "enum": ["set"]},
                        "objectType": {"type": "string", "enum": ["expense.Expense"]},
                        "key": {"type": "string"},
                        "property": {"type": "string", "enum": ["approver_confirmed"]},
                        "value": {"type": "boolean"}
                    }
                }}
            }
        },
        "graph": {
            "contentType": "application/vnd.gorules.decision",
            "nodes": [
                {"id": "in", "type": "inputNode", "name": "input"},
                {"id": "decision", "type": "decisionTableNode", "name": "principal check", "content": {
                    "hitPolicy": "first",
                    "inputs": [{"id": "condition", "name": "condition", "type": "expression"}],
                    "outputs": [
                        {"field": "facts", "id": "out_facts", "name": "facts"},
                        {"field": "commands", "id": "out_commands", "name": "commands"},
                        {"field": "timers", "id": "out_timers", "name": "timers"}
                    ],
                    "rules": [{
                        "_id": "row_0",
                        "condition": "",
                        "out_facts": "[{op: 'set', objectType: 'expense.Expense', key: event.expense_id, property: 'approver_confirmed', value: principal.isApprover}]",
                        "out_commands": "[]",
                        "out_timers": "[]"
                    }]
                }},
                {"id": "out", "type": "outputNode", "name": "output"}
            ],
            "edges": [
                {"id": "edge_0", "sourceId": "in", "targetId": "decision", "type": "edge"},
                {"id": "edge_1", "sourceId": "decision", "targetId": "out", "type": "edge"}
            ]
        }
    })
}

const FACTS_CELL: &str = "/graph/nodes/1/content/rules/0/out_facts";
const COLUMNS: &str = "/inputSchema/properties/principal/properties";

/// `rule_payload` with the member at each JSON pointer replaced.
fn rule_with(changes: &[(&str, Value)]) -> TestResult<Value> {
    let mut payload = rule_payload();
    for (pointer, value) in changes {
        *payload
            .pointer_mut(pointer)
            .ok_or_else(|| format!("no member at {pointer}"))? = value.clone();
    }
    Ok(payload)
}

/// The error codes of checking the entity, `policies` and one rule together.
fn codes(rule: &Value, policies: &[ArtifactEnvelope]) -> TestResult<BTreeSet<String>> {
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        base: &[],
        registry: &registry,
        tenant: None,
    };
    let mut artifacts = vec![entity()?];
    artifacts.extend_from_slice(policies);
    artifacts.push(envelope("rule", "expense.approval.decide.rule", rule)?);
    Ok(check_artifact_set(&artifacts, &context)
        .into_iter()
        .filter(norn_checker::Diagnostic::is_error)
        .map(|finding| finding.code.as_str().to_owned())
        .collect())
}

fn only(code: &str) -> BTreeSet<String> {
    BTreeSet::from([code.to_owned()])
}

fn catalog() -> TestResult<Vec<ArtifactEnvelope>> {
    Ok(vec![policy("expense.access.policy", &catalog_entries())?])
}

fn facts(value: &str) -> Value {
    json!(format!(
        "[{{op: 'set', objectType: 'expense.Expense', key: event.expense_id, property: 'approver_confirmed', value: {value}}}]"
    ))
}

#[test]
fn a_held_role_a_role_profile_and_a_scoped_attribute_are_admitted() -> TestResult {
    assert_eq!(codes(&rule_payload(), &catalog()?)?, BTreeSet::new());
    // A role at listed scope dimensions and an integer attribute are admitted too.
    let listed = rule_with(&[
        (
            "/principalInputs/0/kind/holdsRole/at",
            json!(["org", "environment"]),
        ),
        ("/principalInputs/2/kind/attribute/name", json!("level")),
        (
            "/inputSchema/properties/principal/properties/actorId",
            json!({"type": "integer"}),
        ),
    ])?;
    assert_eq!(codes(&listed, &catalog()?)?, BTreeSet::new());
    Ok(())
}

#[test]
fn reading_an_undeclared_principal_name_is_refused() -> TestResult {
    for read in [
        // A name the rule does not declare.
        "principal.isOwner",
        // The record as a whole, and a computed name.
        "principal != null",
        "principal[event.expense_id] == true",
    ] {
        let rule = rule_with(&[(FACTS_CELL, facts(read))])?;
        assert_eq!(codes(&rule, &catalog()?)?, only(UNDECLARED), "{read}");
    }
    // A column the rule does not declare.
    let mut extra = rule_payload();
    *extra
        .pointer_mut(COLUMNS)
        .and_then(Value::as_object_mut)
        .ok_or("columns")?
        .entry("isOwner")
        .or_insert(Value::Null) = json!({"type": "boolean"});
    assert!(codes(&extra, &catalog()?)?.contains(UNDECLARED));
    Ok(())
}

#[test]
fn a_rule_that_declares_nothing_reads_nothing_under_principal() -> TestResult {
    let mut rule = rule_payload();
    let payload = rule.as_object_mut().ok_or("payload")?;
    payload.remove("principalInputs");
    // The read and the reserved input member both remain: each is refused.
    assert_eq!(
        codes(&rule, &catalog()?)?,
        BTreeSet::from([INVALID.to_owned(), UNDECLARED.to_owned()])
    );
    Ok(())
}

#[test]
fn an_unknown_catalog_role_profile_or_attribute_is_refused() -> TestResult {
    for (pointer, name) in [
        ("/principalInputs/0/kind/holdsRole/role", "treasurer"),
        ("/principalInputs/1/kind/roleProfile/profile", "nobody"),
        ("/principalInputs/2/kind/attribute/name", "department"),
        // A name of another source: a role is not a scoped attribute.
        ("/principalInputs/2/kind/attribute/name", "approver"),
        ("/principalInputs/0/kind/holdsRole/role", "finance"),
    ] {
        let rule = rule_with(&[(pointer, json!(name))])?;
        assert_eq!(
            codes(&rule, &catalog()?)?,
            only(UNKNOWN),
            "{pointer}={name}"
        );
    }
    // With no catalog at all, every input is unknown.
    assert_eq!(codes(&rule_payload(), &[])?, only(UNKNOWN));
    Ok(())
}

#[test]
fn a_column_of_another_type_than_its_catalog_entry_is_refused() -> TestResult {
    for (column, schema) in [
        ("isApprover", json!({"type": "string"})),
        ("financeRoles", json!({"type": "string"})),
        (
            "financeRoles",
            json!({"type": "array", "items": {"type": "integer"}}),
        ),
        ("actorId", json!({"type": "boolean"})),
    ] {
        let pointer = format!("{COLUMNS}/{column}");
        let mut changes = vec![(pointer.as_str(), schema)];
        if column == "isApprover" {
            // Keep the fact value boolean so only the column type differs.
            changes.push((FACTS_CELL, facts("true")));
        }
        let rule = rule_with(&changes)?;
        assert_eq!(codes(&rule, &catalog()?)?, only(TYPE), "{column}");
    }
    Ok(())
}

#[test]
fn malformed_declarations_and_writes_under_principal_are_refused() -> TestResult {
    let duplicate = json!({"name": "isApprover", "kind": {"attribute": {"name": "id"}}});
    for changes in [
        vec![("/principalInputs/2", duplicate)],
        vec![("/principalInputs/0/name", json!("is-approver"))],
        vec![(
            "/principalInputs/0/kind/holdsRole/at",
            json!(["environment"]),
        )],
        vec![(
            "/principalInputs/0/kind/holdsRole/at",
            json!(["org", "org"]),
        )],
        vec![("/principalInputs/0/kind", json!({"ownsEverything": {}}))],
        vec![(
            "/inputSchema/properties/principal/required",
            json!(["isApprover", "financeRoles"]),
        )],
        vec![(
            "/graph/nodes/1/content/outputs/0/field",
            json!("principal.isApprover"),
        )],
    ] {
        let rule = rule_with(&changes)?;
        let found = codes(&rule, &catalog()?)?;
        assert!(found.contains(INVALID), "{changes:?}: {found:?}");
    }
    Ok(())
}

#[test]
fn the_catalog_has_one_definition_per_name() -> TestResult {
    let other = json!([{"name": "approver", "type": "stringSet",
        "source": {"kind": "roleProfile", "roles": ["approver"]}}]);
    let conflicting = vec![
        policy("expense.access.policy", &catalog_entries())?,
        policy("expense.other.policy", &other)?,
    ];
    assert!(codes(&rule_payload(), &conflicting)?.contains(CONFLICT));
    // The same definition twice is one definition.
    let repeated = vec![
        policy("expense.access.policy", &catalog_entries())?,
        policy("expense.other.policy", &catalog_entries())?,
    ];
    assert_eq!(codes(&rule_payload(), &repeated)?, BTreeSet::new());
    // A role is a boolean and a role profile bundles at least one role.
    for entry in [
        json!({"name": "approver", "type": "string", "source": {"kind": "role"}}),
        json!({"name": "finance", "type": "stringSet", "source": {"kind": "roleProfile", "roles": []}}),
        json!({"name": "", "type": "string", "source": {"kind": "attribute"}}),
    ] {
        let invalid = vec![policy("expense.access.policy", &json!([entry]))?];
        assert!(
            codes(&rule_payload(), &invalid)?.contains(POLICY),
            "{entry}"
        );
    }
    Ok(())
}
