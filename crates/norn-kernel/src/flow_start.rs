//! RUN-LEDGER-1 / FLOW-TURN-1: the start and result turns of a flow run.
//!
//! A flow lowers to a Run entity and one `decide` rule. A run starts when
//! that rule decides `{kind: "start", runId}` over an absent run
//! (`slice.run == null`): its output creates the run object in the flow's
//! initial state and dispatches the first steps. This module only evaluates
//! that turn. The host commits the output through its run-start verb, which
//! records the run in the same commit.

use std::collections::BTreeMap;

use norn_decisions::{AdmittedDecision, DecisionBinding, evaluate, value_from_runtime};
use norn_expr::EvaluationBudget;
use norn_ir::{DecisionOutput, RuleRoot, RuntimeValue};

/// Why a flow run cannot start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlowStartError {
    /// The rule is not an admissible `decide` graph for this profile.
    #[error("run.flow_inadmissible: {0}")]
    Inadmissible(String),
    /// The rule refused or failed the start turn.
    #[error("run.start_refused: {0}")]
    Refused(String),
}

impl FlowStartError {
    /// Stable refusal code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Inadmissible(_) => "run.flow_inadmissible",
            Self::Refused(_) => "run.start_refused",
        }
    }
}

/// Evaluate the start turn of `rule` for `run_id`. `environment_hash` is the
/// admitted definition set's receipt digest the rule was checked under.
///
/// # Errors
/// [`FlowStartError`]; nothing is committed either way.
pub fn flow_start(
    rule: &RuleRoot,
    environment_hash: &str,
    run_id: &str,
) -> Result<DecisionOutput, FlowStartError> {
    // An absent run: `slice.run` evaluates to null.
    let event = BTreeMap::from([
        ("kind".to_owned(), RuntimeValue::String("start".to_owned())),
        ("runId".to_owned(), RuntimeValue::String(run_id.to_owned())),
    ]);
    flow_turn(rule, environment_hash, BTreeMap::new(), event)
}

/// One perform step's result, as the flow rule receives it.
#[derive(Debug, Clone, PartialEq)]
pub struct StepResult<'a> {
    /// The run.
    pub run_id: &'a str,
    /// The perform step that produced it.
    pub step: &'a str,
    /// The attempt it answers.
    pub attempt: RuntimeValue,
    /// The Function result's summary.
    pub result: norn_ir::RuntimeObject,
}

/// FLOW-TURN-1: evaluate the result turn of `rule` for one perform step of a
/// live run. `run` is the run object's current values; the event is
/// `{kind: "result", runId, step, attempt, results: {<step>: result}}`.
///
/// # Errors
/// [`FlowStartError`]; nothing is committed either way.
pub fn flow_result(
    rule: &RuleRoot,
    environment_hash: &str,
    run: norn_ir::RuntimeObject,
    result: StepResult<'_>,
) -> Result<DecisionOutput, FlowStartError> {
    let event = BTreeMap::from([
        ("kind".to_owned(), RuntimeValue::String("result".to_owned())),
        (
            "runId".to_owned(),
            RuntimeValue::String(result.run_id.to_owned()),
        ),
        (
            "step".to_owned(),
            RuntimeValue::String(result.step.to_owned()),
        ),
        ("attempt".to_owned(), result.attempt),
        (
            "results".to_owned(),
            RuntimeValue::Object(BTreeMap::from([(
                result.step.to_owned(),
                RuntimeValue::Object(result.result),
            )])),
        ),
    ]);
    let slice = BTreeMap::from([("run".to_owned(), RuntimeValue::Object(run))]);
    flow_turn(rule, environment_hash, slice, event)
}

fn flow_turn(
    rule: &RuleRoot,
    environment_hash: &str,
    slice: norn_ir::RuntimeObject,
    event: norn_ir::RuntimeObject,
) -> Result<DecisionOutput, FlowStartError> {
    let binding = DecisionBinding {
        profile_id: norn_expr::check::CHECKER_PROFILE.to_owned(),
        environment_hash: environment_hash.to_owned(),
    };
    let decision = AdmittedDecision::admit(rule, &BTreeMap::new(), binding)
        .map_err(|error| FlowStartError::Inadmissible(error.to_string()))?;
    let refused = |error: norn_decisions::DecisionError| FlowStartError::Refused(error.to_string());
    let slice = decision
        .validate_slice(value_from_runtime(&RuntimeValue::Object(slice)).map_err(refused)?)
        .map_err(refused)?;
    let event = decision
        .type_event(value_from_runtime(&RuntimeValue::Object(event)).map_err(refused)?)
        .map_err(refused)?;
    evaluate(&decision, &slice, &event, EvaluationBudget::default())
        .map(|evaluated| evaluated.output)
        .map_err(refused)
}
