//! RUN-LEDGER-1: the first turn of a flow run.
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
    let binding = DecisionBinding {
        profile_id: norn_expr::check::CHECKER_PROFILE.to_owned(),
        environment_hash: environment_hash.to_owned(),
    };
    let decision = AdmittedDecision::admit(rule, &BTreeMap::new(), binding)
        .map_err(|error| FlowStartError::Inadmissible(error.to_string()))?;
    let refused = |error: norn_decisions::DecisionError| FlowStartError::Refused(error.to_string());
    // An absent run: `slice.run` evaluates to null.
    let slice = RuntimeValue::Object(BTreeMap::new());
    let event = RuntimeValue::Object(BTreeMap::from([
        ("kind".to_owned(), RuntimeValue::String("start".to_owned())),
        ("runId".to_owned(), RuntimeValue::String(run_id.to_owned())),
    ]));
    let slice = decision
        .validate_slice(value_from_runtime(&slice).map_err(refused)?)
        .map_err(refused)?;
    let event = decision
        .type_event(value_from_runtime(&event).map_err(refused)?)
        .map_err(refused)?;
    evaluate(&decision, &slice, &event, EvaluationBudget::default())
        .map(|evaluated| evaluated.output)
        .map_err(refused)
}
