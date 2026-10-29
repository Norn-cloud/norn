//! The synchronous `decide(slice, event) -> { facts, commands, timers }`
//! evaluation.

use std::error::Error as StdError;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use norn_expr::{EvaluationBudget, Value};
use zen_engine::meter::Meter;
use zen_engine::{
    DecisionGraphTrace, EvaluationError, EvaluationOptions, EvaluationTrace, StrictErrorSite,
    StrictEvaluationError, Variable,
};

use crate::admit::ENGINE_MAX_DEPTH;
use norn_ir::{DecisionOutput, FactChange, RuntimeObject, RuntimeValue};

use crate::convert::{from_variable, to_variable, value_to_runtime};
use crate::schema::Schema;
use crate::{AdmittedDecision, DecisionError, StrictSite};

/// A decision slice validated against one decision's input schema
/// ([`AdmittedDecision::validate_slice`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedSlice {
    pub(crate) rule_id: String,
    pub(crate) schema: Arc<Schema>,
    pub(crate) value: Value,
}

impl ValidatedSlice {
    /// The slice.
    #[must_use]
    pub const fn value(&self) -> &Value {
        &self.value
    }
}

/// A triggering event validated against one decision's input schema
/// ([`AdmittedDecision::type_event`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedEvent {
    pub(crate) rule_id: String,
    pub(crate) schema: Arc<Schema>,
    pub(crate) value: Value,
}

impl TypedEvent {
    /// The event.
    #[must_use]
    pub const fn value(&self) -> &Value {
        &self.value
    }
}

/// One successful evaluation: the decision's candidate effects as the
/// canonical [`DecisionOutput`] (fact changes, command records, timer records,
/// in emitted order, validated against the declared output schema), and the
/// operation units it used. Committing the output (atomically, with lifecycle
/// checks) is the kernel's `commit_decision`.
///
/// `units_used` is deterministic (it depends only on the decision and its
/// input) and reported for accounting; it is not part of the canonical
/// decision result. `matched_rows` are the decision-table rows that matched
/// (only from [`evaluate_explained`]; empty from [`evaluate`]), read from the
/// engine's node trace, whose timing is discarded; they are deterministic.
#[derive(Clone, Debug, PartialEq)]
pub struct Evaluated {
    /// The decision's output.
    pub output: DecisionOutput,
    /// Operation units the evaluation used.
    pub units_used: u64,
    /// The decision-table rows that matched, in node execution order.
    pub matched_rows: Vec<MatchedRow>,
}

/// One decision-table row that matched during an evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchedRow {
    /// The table node's id.
    pub node_id: String,
    /// The table node's name.
    pub node_name: String,
    /// The row's 0-based position in the table.
    pub index: u64,
    /// The row's `_id`, when it has one.
    pub row_id: Option<String>,
}

/// Evaluates `decision` on `slice` and `event` within `budget`.
///
/// Synchronous, deterministic, and sans-IO: the engine future is polled
/// exactly once (every admitted node is synchronous), with a fresh operation
/// meter of `budget` units shared by the whole sub-decision closure. Any
/// failure returns no effects at all.
///
/// # Errors
///
/// A [`DecisionError`]: input validated for another decision, a strict
/// table/switch error, budget exhaustion, engine suspension, an engine
/// failure, or output that violates the declared output schema.
pub fn evaluate(
    decision: &AdmittedDecision,
    slice: &ValidatedSlice,
    event: &TypedEvent,
    budget: EvaluationBudget,
) -> Result<Evaluated, DecisionError> {
    evaluate_with(decision, slice, event, budget, false, &mut |_| Ok(()))
}

/// [`evaluate`] that also reports the decision-table rows that matched
/// ([`Evaluated::matched_rows`]), for explanation. The output is the same;
/// tracing costs more operation units (the engine evaluates every table row
/// instead of an index), so a host that explains its decisions budgets for
/// this variant. The engine's per-node timing is discarded.
///
/// # Errors
///
/// As [`evaluate`].
pub fn evaluate_explained(
    decision: &AdmittedDecision,
    slice: &ValidatedSlice,
    event: &TypedEvent,
    budget: EvaluationBudget,
) -> Result<Evaluated, DecisionError> {
    evaluate_with(decision, slice, event, budget, true, &mut |_| Ok(()))
}

/// Evaluate with explanations and a host's effective fact-contract check.
/// The check sees each exact fact record after output-schema validation and
/// before patch lowering. A failed check returns no effects.
///
/// # Errors
///
/// As [`evaluate_explained`], or [`DecisionError::FactContract`] when the
/// host refuses a fact against its effective stored-property contracts.
pub fn evaluate_explained_with_fact_check(
    decision: &AdmittedDecision,
    slice: &ValidatedSlice,
    event: &TypedEvent,
    budget: EvaluationBudget,
    mut check_fact: impl FnMut(&RuntimeValue) -> Result<(), String>,
) -> Result<Evaluated, DecisionError> {
    evaluate_with(decision, slice, event, budget, true, &mut check_fact)
}

fn evaluate_with(
    decision: &AdmittedDecision,
    slice: &ValidatedSlice,
    event: &TypedEvent,
    budget: EvaluationBudget,
    trace: bool,
    check_fact: &mut dyn FnMut(&RuntimeValue) -> Result<(), String>,
) -> Result<Evaluated, DecisionError> {
    if !decision.accepts(slice, event) {
        let validated_for = if slice.rule_id == decision.rule_id() {
            event.rule_id.clone()
        } else {
            slice.rule_id.clone()
        };
        return Err(DecisionError::InputBinding {
            decision: decision.rule_id().to_owned(),
            validated_for,
        });
    }
    let input = to_variable(&Value::object([
        ("event", event.value.clone()),
        ("slice", slice.value.clone()),
    ]))?;
    let meter = Meter::new(budget.units());
    let options = EvaluationOptions {
        trace,
        max_depth: ENGINE_MAX_DEPTH,
    };
    let outcome = poll_once(
        decision
            .decision()
            .evaluate_metered(input, options, meter.clone()),
    )?;
    // Exhaustion wins over whatever the engine made of it.
    if let Some(exhausted) = meter.exhausted() {
        return Err(DecisionError::BudgetExhausted {
            limit: exhausted.limit,
            used: exhausted.used,
        });
    }
    let response = outcome.map_err(|error| map_engine_error(&error))?;
    let result = from_variable(&response.result)?;
    let mut evaluated = output(decision, result, meter.used(), check_fact)?;
    evaluated.matched_rows = matched_rows(response.trace.as_ref());
    Ok(evaluated)
}

/// One node's trace: execution order, id, name, and trace data.
type NodeTrace = (u64, String, String, Option<Variable>);

/// The matched decision-table rows of an engine trace, in node execution
/// order. A table node's trace data is one row record (first hit) or a list
/// of them (collect), each with its `index` and its `rule` (with `_id`). A
/// sub-decision node's trace data is its child graph's node traces, which
/// are walked recursively in their execution order.
fn matched_rows(trace: Option<&EvaluationTrace>) -> Vec<MatchedRow> {
    let Some(graph) = trace.and_then(EvaluationTrace::as_graph) else {
        return Vec::new();
    };
    let nodes: Vec<NodeTrace> = graph
        .values()
        .map(|node: &DecisionGraphTrace| {
            (
                u64::from(node.order),
                node.id.to_string(),
                node.name.to_string(),
                node.trace_data.clone(),
            )
        })
        .collect();
    let mut rows = Vec::new();
    collect_rows(nodes, &mut rows, 0);
    rows
}

/// The deepest sub-decision nesting walked (the engine bounds depth too).
const MAX_TRACE_DEPTH: usize = 8;

fn collect_rows(mut nodes: Vec<NodeTrace>, rows: &mut Vec<MatchedRow>, depth: usize) {
    if depth > MAX_TRACE_DEPTH {
        return;
    }
    nodes.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (_, node_id, node_name, data) in nodes {
        let records: Vec<Variable> = match &data {
            Some(Variable::Array(items)) => items.borrow().iter().cloned().collect(),
            Some(record @ Variable::Object(_)) => vec![record.clone()],
            _ => continue,
        };
        for record in records {
            let Variable::Object(fields) = &record else {
                continue;
            };
            let fields = fields.borrow();
            if let Some(index) = match fields.get_str("index") {
                Some(Variable::Number(index)) => index.to_string().parse::<u64>().ok(),
                _ => None,
            } {
                let row_id = match fields.get_str("rule") {
                    Some(Variable::Object(rule)) => match rule.borrow().get_str("_id") {
                        Some(Variable::String(id)) => Some(id.to_string()),
                        _ => None,
                    },
                    _ => None,
                };
                rows.push(MatchedRow {
                    node_id: node_id.clone(),
                    node_name: node_name.clone(),
                    index,
                    row_id,
                });
                continue;
            }
            // A sub-decision: the child graph's node traces, keyed by id.
            let children: Vec<NodeTrace> = fields
                .values()
                .filter_map(|child| {
                    let Variable::Object(child) = child else {
                        return None;
                    };
                    let child = child.borrow();
                    let text = |name: &str| match child.get_str(name) {
                        Some(Variable::String(value)) => Some(value.to_string()),
                        _ => None,
                    };
                    let order = match child.get_str("order") {
                        Some(Variable::Number(order)) => order.to_string().parse::<u64>().ok(),
                        _ => None,
                    }?;
                    Some((
                        order,
                        text("id")?,
                        text("name").unwrap_or_default(),
                        child.get_str("traceData").cloned(),
                    ))
                })
                .collect();
            collect_rows(children, rows, depth.saturating_add(1));
        }
    }
}

/// Applies output defaults, validates against the output schema, and splits
/// the record into its three effect lists.
fn output(
    decision: &AdmittedDecision,
    result: Value,
    units_used: u64,
    check_fact: &mut dyn FnMut(&RuntimeValue) -> Result<(), String>,
) -> Result<Evaluated, DecisionError> {
    let Value::Object(mut members) = result else {
        return Err(DecisionError::OutputSchema {
            path: "output".to_owned(),
            reason: "the decision output must be a record".to_owned(),
        });
    };
    for (member, default) in decision.defaults() {
        members
            .entry(member.clone())
            .or_insert_with(|| default.clone());
    }
    let record = Value::Object(members);
    decision
        .output_schema()
        .check(&record, "output")
        .map_err(|violation| DecisionError::OutputSchema {
            path: violation.path,
            reason: violation.reason,
        })?;
    let Value::Object(mut members) = record else {
        return Err(DecisionError::OutputSchema {
            path: "output".to_owned(),
            reason: "the decision output must be a record".to_owned(),
        });
    };
    let mut list = |name: &str| match members.remove(name) {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    };
    let (facts, commands, timers) = (list("facts"), list("commands"), list("timers"));
    let mut changes = Vec::with_capacity(facts.len());
    for (index, fact) in facts.iter().enumerate() {
        changes.extend(fact_changes(fact, index, check_fact)?);
    }
    let output = DecisionOutput {
        facts: changes,
        commands: records(&commands, "commands")?,
        timers: records(&timers, "timers")?,
    };
    Ok(Evaluated {
        output,
        units_used,
        matched_rows: Vec::new(),
    })
}

/// A validated fact item as canonical [`FactChange`]s, exactly: one, or one
/// `set` per property of a `patch` (#3080, [`lower_fact_record`]).
fn fact_changes(
    fact: &Value,
    index: usize,
    check_fact: &mut dyn FnMut(&RuntimeValue) -> Result<(), String>,
) -> Result<Vec<FactChange>, DecisionError> {
    let path = format!("output.facts[{index}]");
    let record = value_to_runtime(fact)?;
    check_fact(&record).map_err(|reason| DecisionError::FactContract {
        path: path.clone(),
        reason,
    })?;
    lower_fact_record(&record).map_err(|error| DecisionError::OutputSchema {
        path,
        reason: format!("not a canonical fact change: {error}"),
    })
}

/// Validated command or timer item records as exact runtime records.
fn records(items: &[Value], member: &str) -> Result<Vec<RuntimeObject>, DecisionError> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let path = format!("output.{member}[{index}]");
            match value_to_runtime(item)? {
                RuntimeValue::Object(record) => Ok(record),
                RuntimeValue::Null
                | RuntimeValue::Bool(_)
                | RuntimeValue::Number(_)
                | RuntimeValue::String(_)
                | RuntimeValue::Array(_) => Err(DecisionError::OutputSchema {
                    path,
                    reason: "must be a record".to_owned(),
                }),
            }
        })
        .collect()
}

/// Polls `future` once. The engine never suspends on admitted graphs (no IO,
/// no timers, an immediately-ready loader), so `Pending` is a typed error,
/// never a spin.
pub(crate) fn poll_once<F: Future>(future: F) -> Result<F::Output, DecisionError> {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => Ok(output),
        Poll::Pending => Err(DecisionError::UnexpectedPending),
    }
}

/// Maps an engine error to a Norn error. A strict error is found anywhere in
/// the source chain (a sub-decision's error is the decision node's source).
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
fn map_engine_error(error: &EvaluationError) -> DecisionError {
    let node_id = match error {
        EvaluationError::NodeError { node_id, .. } => node_id.to_string(),
        _ => String::new(),
    };
    let mut cause: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(current) = cause {
        if let Some(strict) = current.downcast_ref::<StrictEvaluationError>() {
            return DecisionError::Strict {
                site: match strict.site {
                    StrictErrorSite::DecisionTableInput => StrictSite::DecisionTableInput,
                    StrictErrorSite::DecisionTableOutput => StrictSite::DecisionTableOutput,
                    StrictErrorSite::SwitchCondition => StrictSite::SwitchCondition,
                },
                node_id,
                id: strict.id.to_string(),
                expression: strict.expression.to_string(),
                message: strict.message.clone(),
            };
        }
        if let Some(EvaluationError::DepthLimitExceeded) = current.downcast_ref::<EvaluationError>()
        {
            return DecisionError::DepthLimitExceeded;
        }
        if let Some(EvaluationError::BudgetExhausted(exhausted)) =
            current.downcast_ref::<EvaluationError>()
        {
            return DecisionError::BudgetExhausted {
                limit: exhausted.limit,
                used: exhausted.used,
            };
        }
        cause = current.source();
    }
    DecisionError::Engine {
        node_id,
        message: error.to_string(),
    }
}

/// The members of a `patch` record.
const PATCH_MEMBERS: [&str; 4] = ["op", "objectType", "key", "patch"];

/// The canonical fact changes one decide-output `facts` record stands for
/// (#3080): a `patch` record lowers to one [`FactChange::Set`] per property
/// of its `patch` record, in property-id order; any other record decodes as
/// one [`FactChange`].
///
/// # Errors
///
/// The record is not a fact change: an unknown op, a member its op does not
/// carry, a missing or mistyped member, or an empty patch.
pub fn lower_fact_record(record: &RuntimeValue) -> Result<Vec<FactChange>, String> {
    let Some(object) = record.as_object() else {
        return Err("a fact change is a record".to_owned());
    };
    if object.get("op").and_then(RuntimeValue::as_str)
        != Some(norn_ir::FactChangeOp::Patch.as_str())
    {
        return norn_ir::runtime::decode::<FactChange>(record).map(|change| vec![change]);
    }
    if let Some(member) = object
        .keys()
        .find(|member| !PATCH_MEMBERS.contains(&member.as_str()))
    {
        return Err(format!("a patch carries {PATCH_MEMBERS:?}, not {member:?}"));
    }
    let text = |name: &str| {
        object
            .get(name)
            .and_then(RuntimeValue::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| format!("a patch's {name} is a string"))
    };
    let object_type = norn_ir::ObjectTypeName(text("objectType")?);
    let key = text("key")?;
    let Some(RuntimeValue::Object(patch)) = object.get("patch") else {
        return Err("a patch's patch is a record of property values".to_owned());
    };
    if patch.is_empty() {
        return Err("a patch sets at least one property".to_owned());
    }
    Ok(patch
        .iter()
        .map(|(property, value)| FactChange::Set {
            object_type: object_type.clone(),
            key: key.clone(),
            property: norn_ir::PropertyId(property.clone()),
            value: value.clone(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::poll_once;
    use crate::DecisionError;

    #[test]
    fn ready_future_completes_in_one_poll() {
        assert_eq!(poll_once(std::future::ready(7)), Ok(7));
    }

    #[test]
    fn suspension_is_a_typed_error_not_a_spin() {
        assert_eq!(
            poll_once(std::future::pending::<()>()),
            Err(DecisionError::UnexpectedPending)
        );
    }
}
