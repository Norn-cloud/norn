//! Assertions and report formatting for `norn test` scenarios.

use std::fmt::Write as _;

use norn_api::actions::{ActionAnswer, ActionCallOutcome};
use norn_api::cell_definitions::{ApplyOutcome, DefinitionsApplied, DefinitionsPlan};
use norn_api::privacy::{DisclosedObjectQueryPage, DisclosedQueriedObject, serialize_disclosed};
use norn_api::refusal::Refusal;
use norn_api::routes::DefsResponse;
use serde_json::{Map, Value};

use super::format_refusal;
use super::scenario::ScenarioOutcome;

/// Optional step assertions; unknown expectation keys fail closed during parsing.
#[derive(Debug, Default)]
pub(super) struct StepExpect {
    pub(super) ok: Option<bool>,
    pub(super) refusal_code: Option<String>,
    pub(super) diagnostic_code: Option<String>,
    pub(super) min_definitions: Option<usize>,
    pub(super) version: Option<u64>,
    pub(super) unchanged: Option<bool>,
    pub(super) base_version: Option<u64>,
    pub(super) next_version: Option<u64>,
    pub(super) definitions_version: Option<u64>,
    pub(super) ripple_ids: Option<Vec<String>>,
    pub(super) assert_receipt: Option<bool>,
    pub(super) replayed: Option<bool>,
    pub(super) object_type: Option<String>,
    pub(super) object_id: Option<String>,
    pub(super) output_contains: Option<Map<String, Value>>,
    pub(super) withheld: Option<bool>,
    pub(super) fields: Option<Map<String, Value>>,
    pub(super) count: Option<usize>,
    pub(super) min_count: Option<usize>,
}

/// Exact decimal equality across JSON number tokens and exact decimal strings.
pub(super) fn exact_decimal_eq(left: &Value, right: &Value) -> bool {
    let parse_dec = |val: &Value| match val {
        Value::String(text) => norn_ir::Decimal::parse(text).ok(),
        Value::Number(num) => norn_ir::Decimal::parse(&num.to_string()).ok(),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
    };
    matches!((parse_dec(left), parse_dec(right)), (Some(a), Some(b)) if a == b)
}

/// Return true if `actual` matches `expected` (with exact-decimal equivalence
/// and recursive object-subset matching).
pub(super) fn values_match(actual: &Value, expected: &Value) -> bool {
    if actual == expected || exact_decimal_eq(actual, expected) {
        return true;
    }
    if let (Value::Object(act), Value::Object(exp)) = (actual, expected) {
        return exp
            .iter()
            .all(|(k, v)| act.get(k).is_some_and(|av| values_match(av, v)));
    }
    false
}

/// Return `None` when every `(k, v)` in `subset` matches in `actual`, else a mismatch message.
pub(super) fn object_contains(actual: &Value, subset: &Map<String, Value>) -> Option<String> {
    let Some(act_obj) = actual.as_object() else {
        return Some(format!("expected JSON object, got {actual}"));
    };
    for (key, expected) in subset {
        match act_obj.get(key) {
            None => return Some(format!("missing expected field `{key}`")),
            Some(found) if !values_match(found, expected) => {
                return Some(format!(
                    "field `{key}` mismatch: expected {expected}, got {found}"
                ));
            }
            Some(_) => {}
        }
    }
    None
}

/// Build an ordered JSON map for one step report without indexing mutation.
pub(super) fn step_report_entry(
    index: usize,
    op: &str,
    passed: bool,
    detail: Option<&str>,
) -> Value {
    let mut map = Map::new();
    map.insert("step".to_owned(), Value::from(index));
    map.insert("op".to_owned(), Value::from(op));
    map.insert("passed".to_owned(), Value::from(passed));
    if let Some(msg) = detail {
        map.insert("error".to_owned(), Value::from(msg));
    }
    Value::Object(map)
}

fn expect_ok_default(expect: &StepExpect) -> bool {
    expect
        .ok
        .unwrap_or_else(|| expect.refusal_code.is_none() && expect.diagnostic_code.is_none())
}

fn check_refusal(refusal: &Refusal, expect: &StepExpect) -> Result<(), String> {
    if expect_ok_default(expect) {
        return Err(format!("expected ok, got {}", format_refusal(refusal)));
    }
    if let Some(expected_code) = &expect.refusal_code
        && refusal.code != *expected_code
    {
        return Err(format!(
            "expected refusalCode `{expected_code}`, got `{}` ({})",
            refusal.code, refusal.message
        ));
    }
    if let Some(expected_diag) = &expect.diagnostic_code
        && !refusal
            .diagnostics
            .iter()
            .any(|diag| diag.code == *expected_diag)
    {
        let actual: Vec<&str> = refusal
            .diagnostics
            .iter()
            .map(|diag| diag.code.as_str())
            .collect();
        return Err(format!(
            "expected diagnosticCode `{expected_diag}`, got {actual:?}"
        ));
    }
    Ok(())
}

fn reject_unexpected_success(expect: &StepExpect) -> Result<(), String> {
    if !expect_ok_default(expect) {
        return Err("expected refusal, but operation succeeded".to_owned());
    }
    Ok(())
}

pub(super) fn check_defs_result(
    result: Result<DefsResponse, Refusal>,
    expect: &StepExpect,
) -> Result<(), String> {
    match result {
        Err(refusal) => check_refusal(&refusal, expect),
        Ok(defs) => {
            reject_unexpected_success(expect)?;
            if let Some(min_defs) = expect.min_definitions
                && defs.artifacts.len() < min_defs
            {
                return Err(format!(
                    "expected at least {min_defs} definitions, got {}",
                    defs.artifacts.len()
                ));
            }
            if let Some(expected_version) = expect.version
                && defs.version != expected_version
            {
                return Err(format!(
                    "expected definitions version {expected_version}, got {}",
                    defs.version
                ));
            }
            Ok(())
        }
    }
}

pub(super) fn check_plan_result(
    result: Result<DefinitionsPlan, Refusal>,
    expect: &StepExpect,
) -> Result<(), String> {
    match result {
        Err(refusal) => check_refusal(&refusal, expect),
        Ok(plan) => {
            reject_unexpected_success(expect)?;
            if let Some(unchanged) = expect.unchanged
                && plan.unchanged != unchanged
            {
                return Err(format!(
                    "expected unchanged={unchanged}, got {}",
                    plan.unchanged
                ));
            }
            if let Some(base) = expect.base_version
                && plan.base_version != base
            {
                return Err(format!(
                    "expected baseVersion={base}, got {}",
                    plan.base_version
                ));
            }
            if let Some(next) = expect.next_version
                && plan.next_version != next
            {
                return Err(format!(
                    "expected nextVersion={next}, got {}",
                    plan.next_version
                ));
            }
            if let Some(expected_ripples) = &expect.ripple_ids {
                let actual: Vec<&str> = plan
                    .ripples
                    .as_ref()
                    .map(|report| {
                        report
                            .ripples
                            .iter()
                            .map(|entry| entry.artifact_id.as_str())
                            .collect()
                    })
                    .unwrap_or_default();
                for needed in expected_ripples {
                    if !actual.contains(&needed.as_str()) {
                        return Err(format!(
                            "expected ripple id `{needed}` not found in {actual:?}"
                        ));
                    }
                }
            }
            Ok(())
        }
    }
}

pub(super) fn check_apply_result(
    result: Result<DefinitionsApplied, Refusal>,
    expect: &StepExpect,
) -> Result<(), String> {
    match result {
        Err(refusal) => check_refusal(&refusal, expect),
        Ok(applied) => {
            reject_unexpected_success(expect)?;
            let is_unchanged = matches!(applied.outcome, ApplyOutcome::Unchanged);
            if let Some(unchanged) = expect.unchanged
                && is_unchanged != unchanged
            {
                return Err(format!(
                    "expected unchanged={unchanged}, got {is_unchanged}"
                ));
            }
            if let Some(ver) = expect.definitions_version
                && applied.definitions_version != ver
            {
                return Err(format!(
                    "expected definitionsVersion={ver}, got {}",
                    applied.definitions_version
                ));
            }
            Ok(())
        }
    }
}

pub(super) fn check_action_result(
    result: Result<ActionAnswer, Refusal>,
    expect: &StepExpect,
) -> Result<(), String> {
    match result {
        Err(refusal) => check_refusal(&refusal, expect),
        Ok(answer) => {
            reject_unexpected_success(expect)?;
            let ActionAnswer::Receipt(receipt) = answer else {
                return Err("action parked behind an approval (pending)".to_owned());
            };
            if expect.assert_receipt == Some(true)
                && (receipt.receipt.receipt_hash.trim().is_empty()
                    || receipt.receipt.commit_seq == 0)
            {
                return Err("action receipt missing receiptHash or commitSeq".to_owned());
            }
            if let Some(want_replayed) = expect.replayed {
                let is_replayed = receipt.outcome == ActionCallOutcome::Replayed;
                if is_replayed != want_replayed {
                    return Err(format!(
                        "expected replayed={want_replayed}, got {is_replayed}"
                    ));
                }
            }
            if let Some(want_type) = &expect.object_type
                && receipt.object_type != *want_type
            {
                return Err(format!(
                    "expected objectType `{want_type}`, got `{}`",
                    receipt.object_type
                ));
            }
            if let Some(want_id) = &expect.object_id
                && receipt.object_id != *want_id
            {
                return Err(format!(
                    "expected objectId `{want_id}`, got `{}`",
                    receipt.object_id
                ));
            }
            if let Some(want_ver) = expect.version
                && receipt.version != want_ver
            {
                return Err(format!(
                    "expected version {want_ver}, got {}",
                    receipt.version
                ));
            }
            if let Some(subset) = &expect.output_contains {
                let wire = serde_json::to_value(&receipt).map_err(|err| err.to_string())?;
                if let Some(mismatch) = object_contains(&wire, subset) {
                    return Err(format!("outputContains failed: {mismatch}"));
                }
            }
            Ok(())
        }
    }
}

pub(super) fn check_get_object_result(
    result: Result<DisclosedQueriedObject, Refusal>,
    expect: &StepExpect,
) -> Result<(), String> {
    match result {
        Err(refusal) => check_refusal(&refusal, expect),
        Ok(record) => {
            reject_unexpected_success(expect)?;
            if let Some(withheld) = expect.withheld
                && record.withheld != withheld
            {
                return Err(format!(
                    "expected withheld={withheld}, got {}",
                    record.withheld
                ));
            }
            if let Some(ver) = expect.version
                && record.version != Some(ver)
            {
                return Err(format!(
                    "expected object version {ver}, got {:?}",
                    record.version
                ));
            }
            if let Some(fields) = &expect.fields {
                let raw = serialize_disclosed(&record).map_err(|err| format_refusal(&err))?;
                let wire: Value = serde_json::from_str(&raw).map_err(|err| err.to_string())?;
                let values = wire.get("values").unwrap_or(&Value::Null);
                if let Some(mismatch) = object_contains(values, fields) {
                    return Err(format!("fields assertion failed: {mismatch}"));
                }
            }
            Ok(())
        }
    }
}

pub(super) fn check_list_objects_result(
    result: Result<DisclosedObjectQueryPage, Refusal>,
    expect: &StepExpect,
) -> Result<(), String> {
    match result {
        Err(refusal) => check_refusal(&refusal, expect),
        Ok(page) => {
            reject_unexpected_success(expect)?;
            if let Some(count) = expect.count
                && page.records.len() != count
            {
                return Err(format!(
                    "expected {count} record(s), got {}",
                    page.records.len()
                ));
            }
            if let Some(min_count) = expect.min_count
                && page.records.len() < min_count
            {
                return Err(format!(
                    "expected at least {min_count} record(s), got {}",
                    page.records.len()
                ));
            }
            Ok(())
        }
    }
}

pub(super) fn render_json_summary(outcomes: &[ScenarioOutcome], all_passed: bool) -> String {
    let scenario_values: Vec<Value> = outcomes
        .iter()
        .map(|outcome| {
            let mut obj = Map::new();
            obj.insert("name".to_owned(), Value::from(outcome.name.as_str()));
            obj.insert("file".to_owned(), Value::from(outcome.file.as_str()));
            obj.insert("passed".to_owned(), Value::from(outcome.passed));
            if let Some(err) = &outcome.startup_error {
                obj.insert("startupError".to_owned(), Value::from(err.as_str()));
            }
            obj.insert("steps".to_owned(), Value::Array(outcome.steps.clone()));
            Value::Object(obj)
        })
        .collect();
    let mut root = Map::new();
    root.insert("ok".to_owned(), Value::from(all_passed));
    root.insert("scenarios".to_owned(), Value::Array(scenario_values));
    serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_else(|_err| "{}".to_owned())
}

pub(super) fn render_text_summary(outcomes: &[ScenarioOutcome]) -> String {
    let mut out = String::new();
    let mut passed_count = 0_usize;
    let mut failed_count = 0_usize;
    for outcome in outcomes {
        let status = if outcome.passed {
            passed_count = passed_count.saturating_add(1);
            "PASS"
        } else {
            failed_count = failed_count.saturating_add(1);
            "FAIL"
        };
        let _written = writeln!(
            out,
            "{status} {} ({} step(s))",
            outcome.name,
            outcome.steps.len()
        );
        if let Some(startup) = &outcome.startup_error {
            let _written = writeln!(out, "  startup: {startup}");
        }
        for step in &outcome.steps {
            if step.get("passed").and_then(Value::as_bool) == Some(false) {
                let idx = step.get("step").and_then(Value::as_u64).unwrap_or(0);
                let op = step.get("op").and_then(Value::as_str).unwrap_or("?");
                let err = step
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("assertion failed");
                let _written = writeln!(out, "  step {idx} ({op}): {err}");
            }
        }
    }
    let _written = write!(
        out,
        "norn test: {passed_count} passed, {failed_count} failed"
    );
    out
}
