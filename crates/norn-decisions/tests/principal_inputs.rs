//! PRINCIPAL-INPUT-1 (#1091): a decide rule's declared principal inputs at
//! the evaluator: admitted with their closed `principal` record, bound by the
//! host, and never defaulted.

use std::collections::BTreeMap;

use norn_decisions::{
    AdmissionError, AdmittedDecision, DecisionBinding, DecisionError, TypedEvent, ValidatedSlice,
    evaluate,
};
use norn_expr::{EvaluationBudget, Value};
use norn_ir::{FactChange, RuleRoot, RuntimeValue};
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn empty_list() -> serde_json::Value {
    json!({"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}})
}

/// A decision that writes `reviewed = principal.isReviewer`.
fn rule_json() -> serde_json::Value {
    json!({
        "ruleVersion": "2.0",
        "ruleId": "document.review.decide",
        "contract": "decide",
        "scopeRequired": ["orgId"],
        "principalInputs": [
            {"name": "isReviewer", "kind": {"holdsRole": {"role": "reviewer", "at": "resource"}}}
        ],
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "required": ["slice", "event", "principal"],
            "properties": {
                "slice": {"type": "object", "additionalProperties": false, "properties": {}},
                "event": {
                    "type": "object", "additionalProperties": false, "required": ["document"],
                    "properties": {"document": {"type": "string"}}
                },
                "principal": {
                    "type": "object", "additionalProperties": false, "required": ["isReviewer"],
                    "properties": {"isReviewer": {"type": "boolean"}}
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
                        "objectType": {"type": "string", "enum": ["document.Document"]},
                        "key": {"type": "string"},
                        "property": {"type": "string", "enum": ["reviewed"]},
                        "value": {"type": "boolean"}
                    }
                }}
            }
        },
        "graph": {
            "contentType": "application/vnd.gorules.decision",
            "nodes": [
                {"id": "in", "type": "inputNode", "name": "input"},
                {"id": "decision", "type": "decisionTableNode", "name": "decision", "content": {
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
                        "out_facts": "[{op: 'set', objectType: 'document.Document', key: event.document, property: 'reviewed', value: principal.isReviewer}]",
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

fn admit(rule: serde_json::Value) -> TestResult<Result<AdmittedDecision, AdmissionError>> {
    let root: RuleRoot = serde_json::from_value(rule)?;
    Ok(AdmittedDecision::admit(
        &root,
        &BTreeMap::new(),
        DecisionBinding {
            profile_id: "norn-zen/7".to_owned(),
            environment_hash: "principal-inputs".to_owned(),
        },
    ))
}

fn typed(decision: &AdmittedDecision) -> TestResult<(ValidatedSlice, TypedEvent)> {
    let slice = decision.validate_slice(Value::object::<&str>([]))?;
    let event = decision.type_event(Value::object([("document", Value::from("DOC-1"))]))?;
    Ok((slice, event))
}

#[test]
fn bound_principal_inputs_are_the_typed_principal_columns() -> TestResult {
    let decision = admit(rule_json())??;
    assert_eq!(decision.principal_inputs().len(), 1);
    for held in [true, false] {
        let (slice, event) = typed(&decision)?;
        let principal = Value::object([("isReviewer", Value::Bool(held))]);
        let event = decision.bind_principal(event, principal)?;
        let output = evaluate(&decision, &slice, &event, EvaluationBudget::default())?.output;
        let [FactChange::Set { value, .. }] = output.facts.as_slice() else {
            panic!("one set fact");
        };
        assert_eq!(*value, RuntimeValue::Bool(held));
    }
    Ok(())
}

#[test]
fn declared_inputs_that_are_not_bound_produce_no_effects_and_no_default() -> TestResult {
    let decision = admit(rule_json())??;
    let (slice, event) = typed(&decision)?;
    let error = evaluate(&decision, &slice, &event, EvaluationBudget::default())
        .expect_err("nothing was bound");
    assert!(
        matches!(error, DecisionError::PrincipalInputUnavailable { .. }),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .starts_with("decision.principal_input_unavailable")
    );
    Ok(())
}

#[test]
fn a_bound_value_of_another_type_or_name_is_refused_without_repeating_it() -> TestResult {
    let decision = admit(rule_json())??;
    for principal in [
        Value::object([("isReviewer", Value::from("secret-role-list"))]),
        Value::object([
            ("isReviewer", Value::Bool(true)),
            ("ownerOf", Value::from("secret-role-list")),
        ]),
        Value::object::<&str>([]),
    ] {
        let (_slice, event) = typed(&decision)?;
        let error = decision
            .bind_principal(event, principal)
            .expect_err("does not conform");
        assert!(
            matches!(&error, DecisionError::PrincipalInputType { path } if path.starts_with("principal")),
            "{error}"
        );
        assert!(!error.to_string().contains("secret"), "{error}");
    }
    Ok(())
}

#[test]
fn declarations_and_the_principal_record_are_admitted_only_together() -> TestResult {
    let mut undeclared = rule_json();
    undeclared
        .as_object_mut()
        .ok_or("payload")?
        .remove("principalInputs");
    let mut no_record = rule_json();
    let input = no_record
        .pointer_mut("/inputSchema")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("input schema")?;
    input.insert("required".to_owned(), json!(["slice", "event"]));
    input
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("properties")?
        .remove("principal");
    let mut other_column = rule_json();
    *other_column
        .pointer_mut("/principalInputs/0/name")
        .ok_or("name")? = json!("isOwner");
    for rule in [undeclared, no_record, other_column] {
        let refused = admit(rule)?.expect_err("the pair does not match");
        assert!(
            matches!(&refused, AdmissionError::InvalidSchema { path, .. } if path.contains("principal")),
            "{refused}"
        );
    }
    Ok(())
}

#[test]
fn a_decision_that_declares_nothing_accepts_no_principal() -> TestResult {
    let mut rule = rule_json();
    rule.as_object_mut()
        .ok_or("payload")?
        .remove("principalInputs");
    let input = rule
        .pointer_mut("/inputSchema")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("input schema")?;
    input.insert("required".to_owned(), json!(["slice", "event"]));
    input
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("properties")?
        .remove("principal");
    *rule
        .pointer_mut("/graph/nodes/1/content/rules/0/out_facts")
        .ok_or("cell")? = json!("[]");
    let decision = admit(rule)??;
    assert!(decision.principal_inputs().is_empty());
    let (slice, event) = typed(&decision)?;
    let refused = decision
        .bind_principal(
            event.clone(),
            Value::object([("isReviewer", Value::Bool(true))]),
        )
        .expect_err("nothing is declared");
    assert!(matches!(refused, DecisionError::PrincipalInputType { .. }));
    let output = evaluate(&decision, &slice, &event, EvaluationBudget::default())?.output;
    assert!(output.facts.is_empty());
    Ok(())
}
