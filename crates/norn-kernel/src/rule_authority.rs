//! Correspondence boundary for `field_authority.ruleAuthorityIsKernelSealed`.

use std::collections::{BTreeMap, BTreeSet};

use norn_decisions::{
    AdmittedDecision, DecisionBinding, DecisionError, TypedEvent, ValidatedSlice,
};
use norn_expr::EvaluationBudget;
use norn_ir::{DecisionOutput, RuleRoot, RuntimeObject};

use crate::cell_host::{CommitReceipt, JsonObject};
use crate::decision_commit::{
    AdmissionOrigin, commit_prepared_decision, prepare_decision_with_limits_for,
};
use crate::envelope::EnvelopeCounts;
use crate::{CellHost, FactEvent};
use crate::{
    DecisionRefusal, DecisionTurn, EnvelopeLimits, FactKey, FactSlice, PreparedDecision,
    TypeCatalog,
};

/// A rule and exact closure admitted for a cell's pinned definition set.
#[derive(Debug, Clone)]
pub struct AdmittedRule {
    decision: AdmittedDecision,
    pin: String,
}

/// Rule loading or canonical definition commitment failed.
#[derive(Debug, thiserror::Error)]
pub enum RuleAdmissionError {
    /// The evaluator refused the rule or its closure.
    #[error(transparent)]
    Admission(#[from] norn_decisions::AdmissionError),
    /// The exact definition could not be committed.
    #[error(transparent)]
    Canonical(#[from] norn_ir::canonical::CanonicalJsonError),
}

impl AdmittedRule {
    /// Admit and pin the root, closure, exact defaults and checker binding.
    ///
    /// # Errors
    /// Refuses invalid rules and non-canonical definitions.
    pub fn admit(
        root: &RuleRoot,
        closure: &BTreeMap<String, RuleRoot>,
        binding: DecisionBinding,
        exact_defaults: Option<&RuntimeObject>,
    ) -> Result<Self, RuleAdmissionError> {
        let pin = norn_ir::canonical::domain_hash_hex(
            b"norn.rule-authority.v1\0",
            &(
                root,
                closure,
                &binding.profile_id,
                &binding.environment_hash,
                exact_defaults,
            ),
        )?;
        let decision =
            AdmittedDecision::admit_with_defaults(root, closure, binding, exact_defaults)?;
        Ok(Self { decision, pin })
    }

    /// The admitted evaluator, for typed input validation.
    #[must_use]
    pub const fn decision(&self) -> &AdmittedDecision {
        &self.decision
    }
}

/// Exact evaluated effects and their kernel-sealed rule identity and pin.
/// No constructor, deserializer or mutable output accessor is exposed.
///
/// ```compile_fail
/// let forged = norn_kernel::RuleDecision {
///     rule_id: "declared.rule".to_owned(), pin: "claimed-pin".to_owned(),
///     output: norn_ir::DecisionOutput::default(),
/// };
/// ```
#[derive(Debug)]
pub struct RuleDecision {
    rule_id: String,
    pin: String,
    output: DecisionOutput,
}

/// Evaluate an admitted rule; only this path mints rule authority.
///
/// # Errors
/// Returns the evaluator's typed input, budget or output refusal.
pub fn evaluate_rule(
    rule: &AdmittedRule,
    slice: &ValidatedSlice,
    event: &TypedEvent,
    budget: EvaluationBudget,
) -> Result<RuleDecision, DecisionError> {
    let evaluated = norn_decisions::evaluate(rule.decision(), slice, event, budget)?;
    Ok(RuleDecision {
        rule_id: rule.decision().rule_id().to_owned(),
        pin: rule.pin.clone(),
        output: evaluated.output,
    })
}

/// Prepare sealed effects only under the exact admitted rule definition.
///
/// # Errors
/// `decision.invalid_output` for absent or changed rule authority, otherwise
/// the shared decision admission refusal. Caller-supplied `DecisionOutput`
/// continues through `prepare_decision` without rule authority.
pub fn prepare_rule_decision<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    decision: &RuleDecision,
    source_owned_targets: &BTreeSet<FactKey>,
) -> Result<PreparedDecision, DecisionRefusal<X>> {
    if catalog
        .rules
        .get(decision.rule_id.as_str())
        .is_none_or(|rule| rule.pin != decision.pin)
    {
        return Err(DecisionRefusal::InvalidOutput {
            reason: "rule result does not match the admitted rule identity and definition pin"
                .to_owned(),
        });
    }
    prepare_decision_with_limits_for(
        catalog,
        slice,
        turn,
        &decision.output,
        &EnvelopeLimits::V1,
        AdmissionOrigin::Rule,
        source_owned_targets,
    )
}

/// Atomically commit a sealed rule result through the shared commit boundary.
/// Source-owned targets come from the same trusted host resolution as preparation.
///
/// # Errors
/// Returns rule binding, admission or host commit refusals without applying effects.
pub fn commit_rule_decision<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    decision: &RuleDecision,
    source_owned_targets: &BTreeSet<FactKey>,
) -> Result<CommitReceipt, DecisionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let prepared = prepare_rule_decision(catalog, slice, turn, decision, source_owned_targets)?;
    let counts = EnvelopeCounts::new(
        prepared.events.len(),
        decision.output.commands.len(),
        decision.output.timers.len(),
    );
    commit_prepared_decision(host, turn, prepared, counts, None)
}
