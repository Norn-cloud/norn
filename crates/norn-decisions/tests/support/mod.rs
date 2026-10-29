#![allow(
    dead_code,
    reason = "each integration-test crate uses a subset of the helpers"
)]

//! Shared helpers for the `norn-decisions` integration tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use norn_decisions::{
    AdmissionError, AdmittedDecision, DecisionBinding, DecisionError, Evaluated, evaluate,
};
use norn_expr::{EvaluationBudget, Number, Value};
use norn_ir::RuleRoot;
use serde_json::Value as Json;

pub(crate) fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub(crate) fn read_json(path: &Path) -> Json {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A fixture value as an exact Norn value. Fixture numbers are short decimals,
/// which `serde_json`'s shortest round-trip text reproduces exactly.
pub(crate) fn value(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(flag) => Value::Bool(*flag),
        Json::Number(number) => Value::Number(number.to_string().parse::<Number>().unwrap()),
        Json::String(text) => Value::String(text.clone()),
        Json::Array(items) => Value::Array(items.iter().map(value).collect()),
        Json::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, item)| (key.clone(), value(item)))
                .collect(),
        ),
    }
}

pub(crate) fn num(text: &str) -> Value {
    Value::Number(text.parse::<Number>().unwrap())
}

/// A rule payload, inline or `{"ruleFile": <repo-relative envelope>}`.
pub(crate) fn rule(json: &Json) -> RuleRoot {
    let payload = match json.get("ruleFile").and_then(Json::as_str) {
        Some(file) => read_json(&repo_root().join(file))["payload"].clone(),
        None => json.clone(),
    };
    serde_json::from_value(payload).unwrap()
}

pub(crate) fn binding() -> DecisionBinding {
    DecisionBinding {
        profile_id: "norn-zen/6".to_owned(),
        environment_hash: "sha256:test".to_owned(),
    }
}

pub(crate) fn admit_fixture(fixture: &Json) -> Result<AdmittedDecision, AdmissionError> {
    let root = match fixture.get("ruleFile") {
        Some(_) => rule(fixture),
        None => rule(&fixture["rule"]),
    };
    let subs: BTreeMap<String, RuleRoot> = fixture
        .get("subDecisions")
        .and_then(Json::as_object)
        .map(|subs| {
            subs.iter()
                .map(|(key, sub)| (key.clone(), rule(sub)))
                .collect()
        })
        .unwrap_or_default();
    AdmittedDecision::admit(&root, &subs, binding())
}

pub(crate) fn run(
    decision: &AdmittedDecision,
    slice: Value,
    event: Value,
    budget: EvaluationBudget,
) -> Result<Evaluated, DecisionError> {
    let slice = decision.validate_slice(slice)?;
    let event = decision.type_event(event)?;
    evaluate(decision, &slice, &event, budget)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
pub(crate) fn error_kind(error: &DecisionError) -> String {
    match error {
        DecisionError::Strict { site, .. } => format!("strict:{site}"),
        DecisionError::BudgetExhausted { .. } => "budget_exhausted".to_owned(),
        DecisionError::OutputSchema { .. } => "output_schema".to_owned(),
        DecisionError::InputSchema { .. } => "input_schema".to_owned(),
        DecisionError::InputBinding { .. } => "input_binding".to_owned(),
        DecisionError::DepthLimitExceeded => "depth_limit".to_owned(),
        DecisionError::UnexpectedPending => "unexpected_pending".to_owned(),
        DecisionError::UnsupportedValue { .. } => "unsupported_value".to_owned(),
        DecisionError::InvalidNumber { .. } => "invalid_number".to_owned(),
        other => format!("engine: {other}"),
    }
}

pub(crate) fn admission_kind(error: &AdmissionError) -> &'static str {
    match error {
        AdmissionError::NotDecisionGraph { .. } => "not_decision_graph",
        AdmissionError::WrongContract { .. } => "wrong_contract",
        AdmissionError::NodeNotAdmitted { .. } => "node_not_admitted",
        AdmissionError::NodeSchema { .. } => "node_schema",
        AdmissionError::Import { .. } => "import",
        AdmissionError::UnresolvedSubDecision { .. } => "unresolved_sub_decision",
        AdmissionError::UnusedSubDecision { .. } => "unused_sub_decision",
        AdmissionError::SubDecisionBound { .. } => "sub_decision_bound",
        AdmissionError::InvalidSchema { .. } => "invalid_schema",
        AdmissionError::InvalidDefault { .. } => "invalid_default",
        AdmissionError::InvalidExpression { .. } => "invalid_expression",
        _ => "other",
    }
}

/// One member (`facts`, `commands`, `timers`) of an evaluation's canonical
/// output, as exact Norn values (IR numbers are `f64`s whose shortest text is
/// the decimal the evaluator emitted).
/// One member of the output, exactly (through the exact runtime value, never
/// `serde_json`'s binary numbers).
pub(crate) fn member(evaluated: &Evaluated, name: &str) -> Vec<Value> {
    let output = match norn_ir::runtime::encode(&evaluated.output).unwrap() {
        norn_ir::RuntimeValue::Object(output) => output,
        other @ (norn_ir::RuntimeValue::Null
        | norn_ir::RuntimeValue::Bool(_)
        | norn_ir::RuntimeValue::Number(_)
        | norn_ir::RuntimeValue::String(_)
        | norn_ir::RuntimeValue::Array(_)) => {
            panic!("the output is not a record: {other:?}")
        }
    };
    match norn_decisions::value_from_runtime(&output[name]).unwrap() {
        Value::Array(items) => items,
        other @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Object(_)) => panic!("{name} is not a list: {other:?}"),
    }
}

/// Runs one corpus fixture (shape: see `tests/zen_corpus.rs`), appending
/// every mismatch to `failures`; returns the number of checks it ran.
pub(crate) fn check_fixture(path: &Path, failures: &mut Vec<String>) -> usize {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let fixture = read_json(path);
    let admitted = admit_fixture(&fixture);
    if let Some(expected) = fixture.get("admissionError").and_then(Json::as_str) {
        match admitted {
            Err(error) if admission_kind(&error) == expected => {}
            Err(error) => {
                failures.push(format!("{name}: refused as {error:?}, expected {expected}"))
            }
            Ok(_) => failures.push(format!("{name}: admitted, expected {expected}")),
        }
        return 1;
    }
    let decision = match admitted {
        Ok(decision) => decision,
        Err(error) => {
            failures.push(format!("{name}: not admitted: {error}"));
            return 0;
        }
    };
    let cases = fixture["cases"].as_array().expect("cases");
    for case in cases {
        let label = format!("{name} / {}", case["name"].as_str().unwrap_or("?"));
        let budget = case
            .get("budget")
            .and_then(Json::as_u64)
            .map_or_else(EvaluationBudget::default, EvaluationBudget::new);
        let outcome = run(
            &decision,
            value(&case["slice"]),
            value(&case["event"]),
            budget,
        );
        match (
            outcome,
            case.get("expect"),
            case.get("error").and_then(Json::as_str),
        ) {
            (Ok(output), Some(expect), None) => {
                for list in ["facts", "commands", "timers"] {
                    let expected = value(&expect[list]);
                    let actual = norn_expr::Value::Array(member(&output, list));
                    if actual != expected {
                        failures.push(format!(
                            "{label}: {list} = {actual:?}, expected {expected:?}"
                        ));
                    }
                }
            }
            (Err(error), None, Some(expected)) => {
                if error_kind(&error) != expected {
                    failures.push(format!(
                        "{label}: failed with {error:?}, expected {expected}"
                    ));
                }
            }
            (Ok(output), _, Some(expected)) => {
                failures.push(format!(
                    "{label}: produced {output:?}, expected error {expected}"
                ));
            }
            (Err(error), _, _) => failures.push(format!("{label}: failed with {error}")),
            (Ok(_), None, None) => failures.push(format!("{label}: case has no expectation")),
        }
    }
    cases.len()
}
