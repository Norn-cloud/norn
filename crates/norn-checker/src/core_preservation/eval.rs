//! Metered ZEN expression evaluation, schema finite-domain extraction,
//! derivation/precondition evaluation, and assignment expansion shared by
//! core-law preservation checks.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{EvaluationBudget, ExprError, Expression, Value};
use norn_ir::{
    ActionRoot, ArtifactEnvelope, FactChangeOp, InterfaceDef, InterfaceName, JsonValue, LawIR,
    RuntimeValue,
};

use super::{GoverningCoreRule, codes, finding, governing_core_rules, is_core_name};
use crate::lifecycle_writes::{LiteralFactWrite, extract_literal_fact_writes};
use crate::{Diagnostic, DiagnosticWitness, EffectiveCatalog, EffectiveObjectType};

pub(super) const MAX_COMBINATIONS: usize = 256;
pub(super) const MAX_FINITE_ENUM: usize = 8;
pub(super) const WORK_BUDGET: u64 = 250_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EvalStop {
    Budget,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PreconditionVerdict {
    Admitted,
    Refused,
    Unknown(String),
}

pub(super) fn expr_value_to_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.as_str().to_owned(),
        Value::String(text) => format!("{text:?}"),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(expr_value_to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(entries) => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(k, v)| format!("{k}: {}", expr_value_to_string(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub(super) fn json_value_to_expr(value: &JsonValue) -> Option<Value> {
    let serde_val = serde_json::to_value(value).ok()?;
    serde_value_to_expr(serde_val)
}

pub(super) fn serde_value_to_expr(value: serde_json::Value) -> Option<Value> {
    let runtime = RuntimeValue::from_json(value).ok()?;
    Value::try_from(&runtime).ok()
}

pub(super) fn eval_bool(
    expr: &Expression,
    context: &Value,
    work: &mut u64,
) -> Result<bool, EvalStop> {
    let value = eval_value(expr, context, work)?;
    match value {
        Value::Bool(flag) => Ok(flag),
        Value::Null | Value::Number(_) | Value::String(_) | Value::Array(_) | Value::Object(_) => {
            Err(EvalStop::Other)
        }
    }
}

pub(super) fn eval_value(
    expr: &Expression,
    context: &Value,
    work: &mut u64,
) -> Result<Value, EvalStop> {
    *work = work.checked_sub(1).ok_or(EvalStop::Budget)?;
    let attempt = expr.evaluate_metered(context, EvaluationBudget::new(*work));
    *work = work.saturating_sub(attempt.units_used);
    match attempt.result {
        Ok(evaluated) => Ok(evaluated.value().clone()),
        Err(error) => {
            if matches!(error, ExprError::BudgetExhausted { .. }) {
                Err(EvalStop::Budget)
            } else {
                Err(EvalStop::Other)
            }
        }
    }
}

/// Extract single-hop `self.<field>` reads for a governing core rule.
/// Returns `None` if the rule traverses links (`self.a.b`) or reads non-`self`
/// roots (`input`, `actor`, `event`, `slice`).
pub(super) fn rule_single_hop_fields(rule: &GoverningCoreRule) -> Option<BTreeSet<String>> {
    let mut fields = BTreeSet::new();
    let mut sources = vec![rule.expression.as_str()];
    if let Some(cond) = &rule.condition {
        sources.push(cond.as_str());
    }
    for source in sources {
        let self_reads = norn_expr::expression_read_paths(source, "self").ok()?;
        for path in self_reads {
            if path.len() != 1 {
                return None;
            }
            if let Some(head) = path.first() {
                fields.insert(head.clone());
            }
        }
        for root in ["input", "actor", "event", "slice", "aggregates", "clock"] {
            if !norn_expr::expression_read_paths(source, root)
                .ok()?
                .is_empty()
            {
                return None;
            }
        }
    }
    Some(fields)
}

pub(super) fn evaluate_core_rule(
    rule: &GoverningCoreRule,
    self_state: &BTreeMap<String, Value>,
    work: &mut u64,
) -> Result<bool, EvalStop> {
    let context = Value::object([("self", Value::Object(self_state.clone()))]);
    if let Some(cond_src) = &rule.condition {
        let cond = Expression::compile(cond_src).map_err(|_error| EvalStop::Other)?;
        if !eval_bool(&cond, &context, work)? {
            return Ok(true);
        }
    }
    let response = Expression::compile(&rule.expression).map_err(|_error| EvalStop::Other)?;
    let actual = eval_bool(&response, &context, work)?;
    Ok(actual == rule.expected)
}

pub(super) fn budget_unknown_finding(
    artifact: &ArtifactEnvelope,
    action_id: &str,
    core_law_id: &str,
) -> Diagnostic {
    finding(
        codes::PRESERVATION_UNKNOWN,
        artifact,
        "/payload/effectProgram",
        format!(
            "bounded core-law preservation check for action {action_id:?} against core law {core_law_id:?} exhausted its work budget"
        ),
        None,
    )
}

pub(super) fn extract_fixed_schema_value(schema: &JsonValue) -> Option<Value> {
    let obj = schema.as_object()?;
    if let Some(const_val) = obj.get("const") {
        return json_value_to_expr(const_val);
    }
    if let Some(JsonValue::Array(items)) = obj.get("enum")
        && items.len() == 1
        && let Some(only) = items.first()
    {
        return json_value_to_expr(only);
    }
    None
}

pub(super) fn extract_finite_schema_domain(schema: &JsonValue) -> Option<Vec<Value>> {
    let obj = schema.as_object()?;
    if let Some(JsonValue::Array(items)) = obj.get("enum")
        && (2..=MAX_FINITE_ENUM).contains(&items.len())
    {
        let values: Option<Vec<Value>> = items.iter().map(json_value_to_expr).collect();
        return values;
    }
    if obj.get("type").and_then(JsonValue::as_string) == Some("boolean") {
        return Some(vec![Value::Bool(false), Value::Bool(true)]);
    }
    None
}

pub(super) fn slice_constrained_input_fields(root: &ActionRoot) -> BTreeSet<String> {
    let mut constrained = BTreeSet::new();
    for pre in &root.preconditions {
        let src = pre.as_str();
        let reads_external = ["slice", "aggregates", "actor", "clock", "event"]
            .iter()
            .any(|root_name| {
                norn_expr::expression_read_paths(src, root_name)
                    .map_or(true, |paths| !paths.is_empty())
            });
        if reads_external && let Ok(inputs) = norn_expr::expression_read_paths(src, "input") {
            for path in inputs {
                if let Some(head) = path.first() {
                    constrained.insert(head.clone());
                }
            }
        }
    }
    constrained
}

/// For each derived field, `Some(deps)` if every derivation step writing it
/// reads only `input.*` (no `slice`/`actor`/`clock`), or `None` otherwise.
pub(super) fn derive_input_dependencies(
    root: &ActionRoot,
) -> BTreeMap<String, Option<BTreeSet<String>>> {
    let mut deps_by_field: BTreeMap<String, Option<BTreeSet<String>>> = BTreeMap::new();
    for step in &root.derive {
        let src = step.expression.as_str();
        let reads_external = ["slice", "aggregates", "actor", "clock", "event", "self"]
            .iter()
            .any(|root_name| {
                norn_expr::expression_read_paths(src, root_name)
                    .map_or(true, |paths| !paths.is_empty())
            });
        if reads_external {
            deps_by_field.insert(step.field.clone(), None);
            continue;
        }
        let Ok(inputs) = norn_expr::expression_read_paths(src, "input") else {
            deps_by_field.insert(step.field.clone(), None);
            continue;
        };
        let mut step_deps = BTreeSet::new();
        let mut ok = true;
        for path in inputs {
            if path.len() != 1 {
                ok = false;
                break;
            }
            if let Some(head) = path.first() {
                if let Some(prev) = deps_by_field.get(head) {
                    match prev {
                        Some(prev_deps) => step_deps.extend(prev_deps.iter().cloned()),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                } else {
                    step_deps.insert(head.clone());
                }
            }
        }
        deps_by_field.insert(step.field.clone(), ok.then_some(step_deps));
    }
    deps_by_field
}

pub(super) fn apply_derivations(
    root: &ActionRoot,
    input_map: &mut BTreeMap<String, Value>,
    state_map: &mut BTreeMap<String, Value>,
    prop_names: &BTreeSet<&str>,
    work: &mut u64,
) -> Result<(), EvalStop> {
    for step in &root.derive {
        let src = step.expression.as_str();
        let reads_external = ["slice", "aggregates", "actor", "clock", "event", "self"]
            .iter()
            .any(|root_name| {
                norn_expr::expression_read_paths(src, root_name)
                    .map_or(true, |paths| !paths.is_empty())
            });
        if reads_external {
            continue;
        }
        let Ok(inputs) = norn_expr::expression_read_paths(src, "input") else {
            continue;
        };
        if !inputs.iter().all(|path| {
            path.len() == 1
                && path
                    .first()
                    .is_some_and(|head| input_map.contains_key(head))
        }) {
            continue;
        }
        let Ok(expr) = Expression::compile(src) else {
            continue;
        };
        let context = Value::object([("input", Value::Object(input_map.clone()))]);
        let value = eval_value(&expr, &context, work)?;
        input_map.insert(step.field.clone(), value.clone());
        if prop_names.contains(step.field.as_str()) {
            state_map.insert(step.field.clone(), value);
        }
    }
    Ok(())
}

pub(super) fn preconditions_admit(
    root: &ActionRoot,
    input_map: &BTreeMap<String, Value>,
    work: &mut u64,
) -> Result<PreconditionVerdict, EvalStop> {
    let mut unknown_reason: Option<String> = None;
    for (index, pre) in root.preconditions.iter().enumerate() {
        let src = pre.as_str();
        let reads_external = ["slice", "aggregates", "actor", "clock", "event", "self"]
            .iter()
            .any(|root_name| {
                norn_expr::expression_read_paths(src, root_name)
                    .map_or(true, |paths| !paths.is_empty())
            });
        if reads_external {
            unknown_reason.get_or_insert_with(|| {
                format!("precondition #{index} ({src:?}) reads external state outside action input")
            });
            continue;
        }
        let Ok(inputs) = norn_expr::expression_read_paths(src, "input") else {
            unknown_reason.get_or_insert_with(|| {
                format!("precondition #{index} ({src:?}) input reads could not be analyzed")
            });
            continue;
        };
        if !inputs.iter().all(|path| {
            path.len() == 1
                && path
                    .first()
                    .is_some_and(|head| input_map.contains_key(head))
        }) {
            unknown_reason.get_or_insert_with(|| {
                format!(
                    "precondition #{index} ({src:?}) references unassigned or nested input fields"
                )
            });
            continue;
        }
        let Ok(expr) = Expression::compile(src) else {
            unknown_reason.get_or_insert_with(|| {
                format!("precondition #{index} ({src:?}) failed to compile")
            });
            continue;
        };
        let context = Value::object([("input", Value::Object(input_map.clone()))]);
        match eval_bool(&expr, &context, work) {
            Ok(true) => {}
            Ok(false) => return Ok(PreconditionVerdict::Refused),
            Err(EvalStop::Budget) => return Err(EvalStop::Budget),
            Err(EvalStop::Other) => {
                unknown_reason.get_or_insert_with(|| {
                    format!("precondition #{index} ({src:?}) could not be evaluated statically")
                });
            }
        }
    }
    Ok(match unknown_reason {
        Some(reason) => PreconditionVerdict::Unknown(reason),
        None => PreconditionVerdict::Admitted,
    })
}

pub(super) fn expand_assignments(
    fields: &[String],
    domains: &BTreeMap<String, Vec<Value>>,
) -> Option<Vec<BTreeMap<String, Value>>> {
    let mut rows = vec![BTreeMap::new()];
    for field in fields {
        let values = domains.get(field)?;
        let next_len = rows.len().checked_mul(values.len())?;
        if next_len > MAX_COMBINATIONS {
            return None;
        }
        let mut next = Vec::with_capacity(next_len);
        for row in &rows {
            for val in values {
                let mut cloned = row.clone();
                cloned.insert(field.clone(), val.clone());
                next.push(cloned);
            }
        }
        rows = next;
    }
    Some(rows)
}

#[allow(
    clippy::too_many_arguments,
    reason = "synchronous rule literal write verification needs owner artifact, rule, catalog, and core laws"
)]
pub(super) fn check_rule_literal_writes(
    owner_artifact: &ArtifactEnvelope,
    action_id: &str,
    rule_artifact: &ArtifactEnvelope,
    catalog: &EffectiveCatalog,
    interfaces: &BTreeMap<InterfaceName, InterfaceDef>,
    base_laws: &[(&ArtifactEnvelope, LawIR)],
    candidate_laws: &[(&ArtifactEnvelope, LawIR)],
    findings: &mut Vec<Diagnostic>,
) {
    let writes = extract_literal_fact_writes(rule_artifact);
    if writes.is_empty() {
        return;
    }
    let mut work = WORK_BUDGET;
    for write in &writes {
        let Some(object_type) = write.object_type.as_deref() else {
            continue;
        };
        let Some(Ok(effective)) = catalog.object_type_named(object_type) else {
            continue;
        };
        if !effective
            .interfaces
            .iter()
            .any(|iface| is_core_name(iface.as_str()))
            && !is_core_name(object_type)
        {
            continue;
        }
        let rules = governing_core_rules(effective, catalog, interfaces, base_laws, candidate_laws);
        if rules.is_empty() {
            continue;
        }
        let Some((op_label, state_map)) = literal_write_state(write, effective) else {
            continue;
        };
        for rule in &rules {
            let Some(needed) = rule_single_hop_fields(rule) else {
                continue;
            };
            if !needed.iter().all(|f| state_map.contains_key(f)) {
                continue;
            }
            match evaluate_core_rule(rule, &state_map, &mut work) {
                Ok(true) => {}
                Ok(false) => {
                    let state_assignment = state_map
                        .iter()
                        .filter(|(k, _)| needed.contains(*k))
                        .map(|(k, v)| (k.clone(), expr_value_to_string(v)))
                        .collect();
                    let detail = format!(
                        "synchronous rule {:?} emits literal {op_label} on {:?} violating core law {:?}",
                        rule_artifact.artifact_id, object_type, rule.id
                    );
                    let trace = vec![format!(
                        "rule {} ({op_label}) -> {}",
                        rule_artifact.artifact_id, object_type
                    )];
                    findings.push(finding(
                        codes::PRESERVATION_VIOLATED,
                        owner_artifact,
                        &write.pointer,
                        detail.as_str(),
                        Some(DiagnosticWitness::core_law_violation(
                            Vec::new(),
                            action_id,
                            rule_artifact.artifact_id.as_str(),
                            object_type,
                            rule.id.as_str(),
                            state_assignment,
                            BTreeMap::new(),
                            trace,
                            detail,
                        )),
                    ));
                }
                Err(EvalStop::Other) => {
                    findings.push(finding(
                        codes::PRESERVATION_UNKNOWN,
                        owner_artifact,
                        &write.pointer,
                        format!(
                            "synchronous rule {:?} literal {op_label} on {:?} could not be statically evaluated against core law {:?}",
                            rule_artifact.artifact_id, object_type, rule.id
                        ),
                        None,
                    ));
                }
                Err(EvalStop::Budget) => {
                    findings.push(budget_unknown_finding(owner_artifact, action_id, &rule.id));
                    break;
                }
            }
        }
    }
}

fn literal_write_state(
    write: &LiteralFactWrite,
    effective: &EffectiveObjectType,
) -> Option<(&'static str, BTreeMap<String, Value>)> {
    let mut state_map: BTreeMap<String, Value> = BTreeMap::new();
    let op_label = match write.op {
        Some(FactChangeOp::Create) => {
            for prop in &effective.properties {
                if let Ok(Some(default_val)) = prop.default_value()
                    && let Some(expr_val) = serde_value_to_expr(default_val)
                {
                    state_map.insert(prop.id.as_str().to_owned(), expr_val);
                }
            }
            for (k, v) in &write.values {
                if let Ok(expr_val) = Value::try_from(v) {
                    state_map.insert(k.clone(), expr_val);
                }
            }
            "create"
        }
        Some(FactChangeOp::Set) => {
            if let (Some(prop), Some(val)) = (&write.property, &write.value)
                && let Ok(expr_val) = Value::try_from(val)
            {
                state_map.insert(prop.clone(), expr_val);
            }
            "set"
        }
        Some(FactChangeOp::Patch) | None => {
            for (k, v) in &write.patch {
                if let Ok(expr_val) = Value::try_from(v) {
                    state_map.insert(k.clone(), expr_val);
                }
            }
            if let (Some(prop), Some(val)) = (&write.property, &write.value)
                && let Ok(expr_val) = Value::try_from(val)
            {
                state_map.insert(prop.clone(), expr_val);
            }
            "patch"
        }
        Some(FactChangeOp::Transition | FactChangeOp::Delete) => return None,
    };
    (!state_map.is_empty()).then_some((op_label, state_map))
}
