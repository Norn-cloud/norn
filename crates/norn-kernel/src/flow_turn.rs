//! RUN-ROUTES-1 / FLOW-TURN-1: the start and result turns of a flow run.
//!
//! A flow lowers to a Run entity and one `decide` rule (`lowerFlow`). A run
//! starts when that rule decides `{kind: "start", runId}` over an absent run
//! (`slice.run` is null): its output creates the run object in the flow's
//! initial state and dispatches the first steps. A perform step's result
//! comes back as `{kind: "result", runId, step, attempt, results: {<step>:
//! output}}` over the run's current values. This module only evaluates those
//! turns; the host commits the output (the start through its run-start verb,
//! which records the run in the same commit).

use std::collections::BTreeMap;

use norn_decisions::{AdmittedDecision, DecisionBinding, evaluate, value_from_runtime};
use norn_expr::EvaluationBudget;
use norn_ir::{DecisionOutput, RuleRoot, RuntimeObject, RuntimeValue};

/// Why a flow turn produced no output.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlowTurnError {
    /// The rule is not an admissible `decide` graph for this profile.
    #[error("run.flow_inadmissible: {0}")]
    Inadmissible(String),
    /// The rule refused or failed the start turn.
    #[error("run.start_refused: {0}")]
    StartRefused(String),
    /// The rule refused or failed a result turn.
    #[error("run.turn_refused: {0}")]
    TurnRefused(String),
}

impl FlowTurnError {
    /// Stable refusal code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Inadmissible(_) => "run.flow_inadmissible",
            Self::StartRefused(_) => "run.start_refused",
            Self::TurnRefused(_) => "run.turn_refused",
        }
    }
}

fn text(value: &str) -> RuntimeValue {
    RuntimeValue::String(value.to_owned())
}

/// Evaluate the start turn of `rule` for `run_id`. `environment_hash` is the
/// admitted definition set's receipt digest the rule was checked under.
///
/// # Errors
/// [`FlowTurnError`]; nothing is committed either way.
pub fn start(
    rule: &RuleRoot,
    environment_hash: &str,
    run_id: &str,
) -> Result<DecisionOutput, FlowTurnError> {
    let event = BTreeMap::from([
        ("kind".to_owned(), text("start")),
        ("runId".to_owned(), text(run_id)),
    ]);
    // An absent run: `slice.run` evaluates to null.
    turn(
        rule,
        environment_hash,
        BTreeMap::new(),
        event,
        FlowTurnError::StartRefused,
    )
}

/// One perform step's result, as the flow rule receives it.
#[derive(Debug, Clone, PartialEq)]
pub struct StepResult<'a> {
    /// The run.
    pub run_id: &'a str,
    /// The perform step that produced it.
    pub step: &'a str,
    /// The attempt it answers, as the command carried it.
    pub attempt: RuntimeValue,
    /// The Function's output.
    pub output: RuntimeObject,
}

/// Evaluate the result turn of `rule` for one perform step of a live run.
/// `run` is the run object's current values.
///
/// # Errors
/// [`FlowTurnError`]; nothing is committed either way.
pub fn result(
    rule: &RuleRoot,
    environment_hash: &str,
    run: RuntimeObject,
    step: StepResult<'_>,
) -> Result<DecisionOutput, FlowTurnError> {
    let event = BTreeMap::from([
        ("kind".to_owned(), text("result")),
        ("runId".to_owned(), text(step.run_id)),
        ("step".to_owned(), text(step.step)),
        ("attempt".to_owned(), step.attempt),
        (
            "results".to_owned(),
            RuntimeValue::Object(BTreeMap::from([(
                step.step.to_owned(),
                RuntimeValue::Object(step.output),
            )])),
        ),
    ]);
    // An unset property is absent from the slice, as the closed slice schema
    // expects; the stored object carries it as null.
    let run = run
        .into_iter()
        .filter(|(_, value)| !matches!(value, RuntimeValue::Null))
        .collect();
    let slice = BTreeMap::from([("run".to_owned(), RuntimeValue::Object(run))]);
    turn(
        rule,
        environment_hash,
        slice,
        event,
        FlowTurnError::TurnRefused,
    )
}

fn turn(
    rule: &RuleRoot,
    environment_hash: &str,
    slice: RuntimeObject,
    event: RuntimeObject,
    refusal: fn(String) -> FlowTurnError,
) -> Result<DecisionOutput, FlowTurnError> {
    let binding = DecisionBinding {
        profile_id: norn_expr::check::CHECKER_PROFILE.to_owned(),
        environment_hash: environment_hash.to_owned(),
    };
    let decision = AdmittedDecision::admit(rule, &BTreeMap::new(), binding)
        .map_err(|error| FlowTurnError::Inadmissible(error.to_string()))?;
    let refused = |error: norn_decisions::DecisionError| refusal(error.to_string());
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
