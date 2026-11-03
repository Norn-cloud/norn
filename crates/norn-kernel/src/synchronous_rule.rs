//! Pure, pinned synchronous writers for the governed action transaction.

use crate::{ActionCall, ActionRefusal, FactKey, TypeCatalog, cell_host::JsonObject};
use norn_decisions::{AdmittedDecision, DecisionBinding};
use norn_ir::{ActionRuleRef, DecisionOutput, RuleRoot, RuntimeValue};
use std::collections::BTreeMap;

/// A checked `RuleIR` graph and its exact helper closure, sealed against mutation.
#[derive(Debug)]
pub struct SynchronousRule {
    reference: ActionRuleRef,
    decision: AdmittedDecision,
    digest: String,
    scopes: Vec<String>,
}

/// A synchronous writer could not be loaded or fingerprinted.
#[derive(Debug, thiserror::Error)]
pub enum SynchronousRuleError {
    /// The writer reference is empty.
    #[error("the writer requires a nonempty exact artifact id and version")]
    InvalidReference,
    /// The pure graph/profile refused admission.
    #[error(transparent)]
    Admission(#[from] norn_decisions::AdmissionError),
    /// The exact closure could not be canonically identified.
    #[error(transparent)]
    Canonical(#[from] norn_ir::canonical::CanonicalJsonError),
}

impl SynchronousRule {
    /// Load a writer from an admitted artifact set; no callbacks or IO survive.
    ///
    /// # Errors
    /// The same pure evaluator admission or canonical-identity refusal.
    pub fn admit(
        reference: ActionRuleRef,
        root: &RuleRoot,
        helpers: &BTreeMap<String, RuleRoot>,
        binding: DecisionBinding,
    ) -> Result<Self, SynchronousRuleError> {
        if reference.artifact_id.trim().is_empty() || reference.version.trim().is_empty() {
            return Err(SynchronousRuleError::InvalidReference);
        }
        let digest = norn_ir::canonical::domain_hash_hex(
            b"norn.synchronous-rule.v1\0",
            &(
                &reference,
                root,
                helpers,
                &binding.profile_id,
                &binding.environment_hash,
            ),
        )?;
        Ok(Self {
            reference,
            decision: AdmittedDecision::admit(root, helpers, binding)?,
            digest,
            scopes: root.scope_required.clone(),
        })
    }

    pub(crate) fn binding<X>(&self, call: &ActionCall<'_>) -> Result<String, ActionRefusal<X>> {
        norn_ir::canonical::domain_hash_hex(
            b"norn.action-rule-call.v1\0",
            &(
                call.action,
                call.action_artifact_id,
                call.action_version,
                &call.scope,
                &call.turn.world,
                &self.digest,
            ),
        )
        .map_err(|error| refused(error.to_string()))
    }

    /// TXN-RULE-1: evaluate only the captured action slice and trusted actor.
    pub(crate) fn evaluate<X>(
        &self,
        call: &ActionCall<'_>,
        input: &JsonObject,
        slice: &JsonObject,
        fuel: &mut u64,
    ) -> Result<DecisionOutput, ActionRefusal<X>> {
        if self.scopes.iter().any(|scope| {
            call.scope
                .get(scope)
                .is_none_or(|value| value.trim().is_empty())
        }) {
            return Err(refused("the rule requires uncovered scope"));
        }
        let event = RuntimeValue::Object(JsonObject::from([
            ("input".to_owned(), RuntimeValue::Object(input.clone())),
            ("actor".to_owned(), RuntimeValue::String(call.actor.clone())),
            (
                "operationId".to_owned(),
                RuntimeValue::String(call.idempotency_key.clone()),
            ),
        ]));
        let values = crate::decision_commit::expr_value(&RuntimeValue::Object(slice.clone()))
            .map_err(refused)?;
        let event = crate::decision_commit::expr_value(&event).map_err(refused)?;
        let slice = self
            .decision
            .validate_slice(values)
            .map_err(ActionRefusal::from)?;
        let event = self
            .decision
            .type_event(event)
            .map_err(ActionRefusal::from)?;
        let evaluated = norn_decisions::evaluate(
            &self.decision,
            &slice,
            &event,
            norn_expr::EvaluationBudget::new(*fuel),
        )
        .map_err(ActionRefusal::from)?;
        *fuel = fuel
            .checked_sub(evaluated.units_used)
            .ok_or_else(|| refused("the rule exceeded the action fuel"))?;
        if !evaluated.output.commands.is_empty() || !evaluated.output.timers.is_empty() {
            return Err(refused(
                "synchronous rules are facts-only; use declared action commands",
            ));
        }
        Ok(evaluated.output)
    }
}

impl<X> From<norn_decisions::DecisionError> for ActionRefusal<X> {
    fn from(error: norn_decisions::DecisionError) -> Self {
        if let norn_decisions::DecisionError::BudgetExhausted { .. } = error {
            return Self::ExpressionBudgetExceeded {
                limit: crate::limits::ResourceLimitProfile::norn_limits_v1()
                    .turn
                    .fuel_units,
            };
        }
        refused(error.to_string())
    }
}

pub(crate) fn refused<X>(reason: impl Into<String>) -> ActionRefusal<X> {
    ActionRefusal::UncheckedWriter {
        reason: reason.into(),
    }
}

/// TXN-RULE-1: absence and ambiguity never fall back to a primary-only effect.
pub(crate) fn resolve<'a, X>(
    catalog: &TypeCatalog<'a>,
    call: &ActionCall<'_>,
) -> Result<Option<&'a SynchronousRule>, ActionRefusal<X>> {
    // TXN-CLOSED-1: bound the fixed link closure before any host access.
    if call.action.create_links.len() > 16 {
        return Err(refused("createLinks exceeds the admitted bound of sixteen"));
    }
    let Some(pin) = &call.action.synchronous_rule else {
        return Ok(None);
    };
    if !matches!(
        call.action.effect_program,
        Some(norn_ir::ActionEffectProgram::FireTransition { .. })
    ) {
        return Err(refused(
            "only transition actions admit a synchronous writer",
        ));
    }
    let mut matching = catalog
        .action_rules
        .iter()
        .filter(|rule| rule.reference == *pin);
    let result = matching
        .next()
        .ok_or_else(|| ActionRefusal::UnsupportedEffect {
            action_id: call.action.action_id.clone(),
        })?;
    if matching.next().is_some() {
        return Err(refused("the exact writer pin is ambiguous"));
    }
    Ok(Some(result))
}

/// TXN-CLOSED-1: an emitted operation must have an explicit declared writer.
pub(crate) fn writer_target<X>(
    change: &norn_ir::FactChange,
) -> Result<(FactKey, norn_ir::WriteChange), ActionRefusal<X>> {
    use norn_ir::{FactChange, WriteChange};
    let operation = match change {
        FactChange::Create { .. } => WriteChange::Created,
        FactChange::Set { .. } | FactChange::Transition { .. } => WriteChange::Modified,
        FactChange::Delete { .. } => {
            return Err(refused("synchronous action rules cannot delete facts"));
        }
    };
    Ok((crate::decision_commit::fact_key(change), operation))
}
