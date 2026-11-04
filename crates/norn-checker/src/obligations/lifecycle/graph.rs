//! PROOF-LIFECYCLE-3: complete transition-only graphs with native pointwise law filters.
use super::super::{ApprovedContract, Challenge, ChallengeError, DefinitionSource};
use super::finite::{Bounds, Row, Snapshot};
use norn_expr::{EvaluationBudget, Expression, Value};
use norn_ir::{LawModality, LawResponse, LawSeverity, PropertyTypeName, RuntimeValue};
use std::collections::{BTreeMap, BTreeSet};

const WORK: u64 = 2_000_000;
const EDGES: usize = 16_384;

/// A named declared transition between exact bounded rows.
#[derive(Debug, PartialEq, Eq)]
pub struct Edge {
    /// Index into the complete input snapshot.
    pub from: usize,
    /// Index into the same snapshot; scalar context never changes.
    pub to: usize,
    /// Effective declaration member identity.
    pub transition: String,
}
/// Conditional native relation, without evidence or discharge authority.
#[derive(Debug)]
pub struct Graph {
    /// Complete typed rows and applicable source law identities.
    pub inputs: Snapshot,
    /// Declared initial states satisfying every supported governing law.
    pub initial: Vec<usize>,
    /// Guard-enabled steps whose post-state satisfies every governing law.
    /// Unreachable pre-states remain available for local native correspondence.
    pub edges: Vec<Edge>,
}
/// Fresh challenge and pinned graph limits; unsupported profiles remain Unknown.
#[derive(Debug)]
pub struct Batch {
    /// Complete source-bound goal inventory, without modified assessments.
    pub challenge: Challenge,
    /// Canonical exact input profile plus graph resource semantics.
    pub bounds_json: String,
    /// All-or-nothing relation construction, never a partially explored graph.
    pub graph: Result<Graph, String>,
}

fn compile(
    source: &str,
    work: &mut u64,
    fields: &BTreeMap<String, norn_ir::PropertyType>,
) -> Result<Expression, String> {
    let cost = u64::try_from(source.len()).map_err(|_error| "expression source limit")?;
    *work = work
        .checked_sub(cost.saturating_add(1))
        .ok_or("graph work budget exhausted")?;
    let reads =
        norn_expr::expression_read_paths(source, "self").map_err(|error| error.to_string())?;
    if reads
        .iter()
        .any(|path| path.len() != 1 || !path.first().is_some_and(|name| fields.contains_key(name)))
    {
        return Err("lifecycle expression reads outside the frozen scalar subject".into());
    }
    for root in ["input", "actor", "event"] {
        if !norn_expr::expression_read_paths(source, root)
            .map_err(|error| error.to_string())?
            .is_empty()
        {
            return Err(format!("lifecycle expression reads unbound {root} context"));
        }
    }
    Expression::compile(source).map_err(|error| error.to_string())
}
fn boolean(expression: &Expression, context: &Value, work: &mut u64) -> Result<bool, String> {
    *work = work.checked_sub(1).ok_or("graph work budget exhausted")?;
    let attempt = expression.evaluate_metered(context, EvaluationBudget::new(*work));
    *work = work.saturating_sub(attempt.units_used);
    match attempt.result.map_err(|error| error.to_string())?.value() {
        Value::Bool(value) => Ok(*value),
        Value::Null | Value::Number(_) | Value::String(_) | Value::Array(_) | Value::Object(_) => Err("graph predicate is not Boolean".into()),
    }
}
fn key(row: &Row) -> Result<Vec<u8>, String> {
    norn_canonical::canonical_json_exact_of(row).map_err(|error| error.to_string())
}
struct Predicate<'a> {
    identity: &'a str,
    condition: Option<Expression>,
    response: Expression,
    expected: bool,
}
fn compile_laws<'a>(inputs: &'a Snapshot, work: &mut u64) -> Result<Vec<Predicate<'a>>, String> {
    if inputs.laws.len() > 128 {
        return Err("more than 128 governing laws".into());
    }
    let mut laws = Vec::new();
    for (identity, law) in &inputs.laws {
        let LawResponse::Predicate { expression } = &law.response else {
            return Err(format!(
                "unsupported lifecycle law {identity}: non-predicate response"
            ));
        };
        if law.timing.is_some()
            || !law.scope.lifecycle_states.is_empty()
            || !law.scope.commitments.is_empty()
            || law.severity != LawSeverity::Error
            || !matches!(law.modality, LawModality::Always {} | LawModality::Never {})
        {
            return Err(format!(
                "unsupported lifecycle law {identity}: timing, scope, modality or advisory severity"
            ));
        }
        let condition = law
            .condition
            .as_ref()
            .map(|condition| compile(condition.as_str(), work, &inputs.fields))
            .transpose()
            .map_err(|error| format!("law {identity}: {error}"))?;
        let response = compile(expression.as_str(), work, &inputs.fields)
            .map_err(|error| format!("law {identity}: {error}"))?;
        laws.push(Predicate { identity, condition, response, expected: matches!(law.modality, LawModality::Always {}) });
    }
    Ok(laws)
}
fn build(inputs: Snapshot, property: &str) -> Result<Graph, String> {
    let mut work = WORK;
    let laws = compile_laws(&inputs, &mut work)?;
    let chart = &inputs.machine.lifecycle;
    let transitions = chart
        .transitions
        .iter()
        .map(|transition| {
            let guards = transition
                .guards
                .iter()
                .map(|guard| {
                    compile(guard.as_str(), &mut work, &inputs.fields)
                        .map_err(|error| format!("transition {}: {error}", transition.id.as_str()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((transition, guards))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut rows = BTreeMap::new();
    let mut contexts = Vec::new();
    for (index, row) in inputs.nodes.iter().enumerate() {
        if rows.insert(key(row)?, index).is_some() {
            return Err("duplicate finite scalar valuations".into());
        }
        let value = Value::try_from(&RuntimeValue::Object(row.clone()))
            .map_err(|error| error.to_string())?;
        contexts.push(Value::object([("self", value)]));
    }
    let mut legal = Vec::new();
    for context in &contexts {
        let mut accepted = true;
        for law in &laws {
            let identity = law.identity;
            let activated = law.condition
                .as_ref()
                .map(|expression| boolean(expression, context, &mut work))
                .transpose()
                .map_err(|error| format!("law {identity}: {error}"))?
                .unwrap_or(true);
            if activated {
                accepted &= boolean(&law.response, context, &mut work)
                    .map_err(|error| format!("law {identity}: {error}"))?
                    == law.expected;
            }
        }
        legal.push(accepted);
    }
    let mut initial = Vec::new();
    let mut edges = Vec::new();
    for (from, ((row, context), allowed)) in
        inputs.nodes.iter().zip(&contexts).zip(&legal).enumerate()
    {
        let Some(RuntimeValue::String(state)) = row.get(property) else {
            return Err("missing finite lifecycle state".into());
        };
        if *allowed && chart.initial.contains(state) {
            initial.push(from);
        }
        for (transition, guards) in &transitions {
            if !transition.from.contains(state) {
                continue;
            }
            let mut enabled = true;
            for guard in guards {
                enabled &= boolean(guard, context, &mut work)
                    .map_err(|error| format!("transition {}: {error}", transition.id.as_str()))?;
            }
            if !enabled {
                continue;
            }
            let mut after = row.clone();
            after.insert(property.into(), RuntimeValue::String(transition.to.clone()));
            let to = *rows
                .get(&key(&after)?)
                .ok_or("transition leaves the declared bounds")?;
            if !*legal.get(to).ok_or("missing post-state law assessment")? {
                continue;
            }
            if edges.len() == EDGES {
                return Err("finite graph edge cap exceeded".into());
            }
            edges.push(Edge {
                from,
                to,
                transition: transition.id.as_str().into(),
            });
        }
    }
    Ok(Graph {
        inputs,
        initial,
        edges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nonlocal_reads_are_unknown_even_in_an_inactive_branch() {
        let fields = BTreeMap::from([("status".into(), norn_ir::PropertyType::Boolean {})]);
        let mut work = WORK;
        assert!(compile("self.status == true", &mut work, &fields).is_ok());
        for expression in [
            "false and actor.role == 'operator'",
            "self.supplier.balance > 0",
            "self",
        ] {
            assert!(Expression::compile(expression).is_ok(), "{expression}");
            assert!(compile(expression, &mut work, &fields).is_err());
        }
    }
}

impl ApprovedContract {
    /// Reconstruct admitted inputs before building a complete law-filtered relation.
    /// # Errors
    /// Admission, Challenge or canonical bounds encoding fails; no evidence is issued.
    pub fn lifecycle_graph(
        &self,
        source: &DefinitionSource,
        registry: &BTreeSet<PropertyTypeName>,
        bounds: &Bounds,
    ) -> Result<Batch, ChallengeError> {
        let batch = self.lifecycle_inputs(source, registry, bounds)?;
        let input: serde_json::Value =
            serde_json::from_str(&batch.bounds_json).map_err(|_error| ChallengeError::Closure)?;
        let profile = serde_json::json!({"profile":"norn.lifecycle.graph/1","inputProfile":input,
            "maxLaws":128,"maxEdges":EDGES,"workBudget":WORK,
            "workSemantics":"expression source bytes plus compilation and metered evaluation units",
            "initial":"declared initial state and all supported governing laws",
            "steps":"declared transition guards and all post-state governing laws; frozen context"});
        let bytes = norn_canonical::canonical_json_exact_of(&profile)
            .map_err(|_error| ChallengeError::Closure)?;
        Ok(Batch {
            challenge: batch.challenge,
            bounds_json: String::from_utf8(bytes).map_err(|_error| ChallengeError::LossyInput)?,
            graph: batch
                .snapshot
                .and_then(|inputs| build(inputs, &bounds.property)),
        })
    }
}
