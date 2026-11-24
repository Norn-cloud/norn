use norn_ir::{FactChange, ObjectTypeName, ResourceLimitProfile};

use crate::cell_host::{CellHost, FactKey, JsonObject, JsonValue};
use crate::decision_commit::{FactEvent, TypeCatalog};
use crate::limits::SliceMeter;

use super::input::check_action_record_size;
use super::receipt::resolve_receipt_effect;
use super::slice::{ActionSliceContext, check_action_preconditions, load_action_slice};
use super::types::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionCall, ActionCallRecord, ActionInputPreparation,
    ActionOutcome, ActionRefusal, EnvelopePolicy, Pending,
};
use super::{
    authorize_current_target, authorize_target, authorize_upsert_target, check_action_envelope,
    check_kind, commit_effect, host_failure, replay,
};

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the pinned writer and primary transition share authorization, captured fences and one commit"
)]
pub(super) fn commit_transition<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    mut preparation: ActionInputPreparation<'_>,
    authorizer: &dyn ActionAuthorizer,
    object_id_field: &str,
    object_kind: &str,
    lifecycle: &norn_ir::PropertyId,
    transition: &norn_ir::TransitionId,
    accept_receipt: Option<&norn_ir::ReceiptAcceptance>,
    envelope: EnvelopePolicy<'_>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let limits = envelope.limits;
    let action = call.action;
    if accept_receipt.is_some()
        && (action.synchronous_rule.is_some() || call.turn.consumes.is_some())
    {
        // One commit consumes at most one delivery, and a rule writer's
        // closure has not been qualified against receipt-bound values.
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: action.action_id.clone(),
        });
    }
    let Some(JsonValue::String(object_id)) = preparation.effective_input.get(object_id_field)
    else {
        return Err(ActionRefusal::InvalidInput {
            field: object_id_field.to_owned(),
        });
    };
    if object_id.trim().is_empty() {
        return Err(ActionRefusal::InvalidInput {
            field: object_id_field.to_owned(),
        });
    }
    let object_type = call
        .object_type
        .clone()
        .unwrap_or_else(|| ObjectTypeName(object_kind.to_owned()));

    let target = FactKey {
        object_type: object_type.clone(),
        key: object_id.clone(),
    };
    let state = authorize_target(
        host,
        call,
        &preparation.effective_input,
        authorizer,
        &target,
    )?;
    check_kind(catalog, &object_type, object_kind)?;
    let record_key = FactKey::new(
        ACTION_CALL_TYPE,
        format!("{}/{}", action.action_id, call.idempotency_key),
    );
    let rule = crate::synchronous_rule::resolve::<H::Error>(catalog, call)?;
    let mut record = ActionCallRecord::for_transition(call, &preparation, &target, transition);
    record.rule_binding = rule.map(|rule| rule.binding(call)).transpose()?;
    let recorded = host.load(&record_key).map_err(host_failure)?;
    if let Some(outcome) = replay(&recorded, &record, &call.idempotency_key)? {
        return Ok(outcome);
    }
    let mut slice = load_action_slice(
        &ActionSliceContext {
            host,
            catalog,
            call,
            effective_input: &preparation.effective_input,
            authorizer,
        },
        &target,
        &state,
        SliceMeter::for_profile(&ResourceLimitProfile::norn_limits_v1()),
    )?;
    preparation.precondition_outcomes = check_action_preconditions(
        catalog,
        action,
        &preparation.effective_input,
        &slice,
        &mut preparation.remaining_fuel,
        &mut preparation.receipt_evidence_bytes,
        usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
    )?;
    record
        .precondition_outcomes
        .clone_from(&preparation.precondition_outcomes);
    record.parameter_pins.clone_from(&slice.parameter_pins);
    check_action_record_size::<H::Error>(call.action, &record, limits)?;
    let (mut changes, decision_links) = if let Some(rule) = rule {
        let output = rule.evaluate(
            call,
            &preparation.effective_input,
            &slice.values,
            &mut preparation.remaining_fuel,
        )?;
        check_action_envelope::<H::Error>(
            limits,
            output.facts.len().saturating_add(1),
            action.commands.len(),
        )?;
        for change in &output.facts {
            let (key, operation) = crate::synchronous_rule::writer_target(change)?;
            if key.object_type.as_str().starts_with("norn.")
                || !action.writes.iter().any(|write| {
                    write.entity_kind == key.object_type.as_str() && write.change == operation
                })
                || (key == target && matches!(change, FactChange::Transition { .. }))
            {
                return Err(crate::synchronous_rule::refused(
                    "the rule wrote outside the declared writer contract",
                ));
            }
            if operation == norn_ir::WriteChange::Created {
                if let std::collections::btree_map::Entry::Vacant(entry) = slice.facts.entry(key) {
                    let state = authorize_upsert_target(
                        host,
                        call,
                        &preparation.effective_input,
                        authorizer,
                        entry.key(),
                    )?;
                    entry.insert(state);
                }
            } else {
                let state = slice.facts.get(&key).ok_or_else(|| {
                    crate::synchronous_rule::refused(
                        "a modified object is outside the captured slice",
                    )
                })?;
                authorize_current_target(
                    call,
                    &preparation.effective_input,
                    authorizer,
                    &key,
                    state,
                )?;
            }
        }
        (output.facts, output.links)
    } else {
        (Vec::new(), Vec::new())
    };
    let receipt = match accept_receipt {
        Some(declaration) => {
            let accepted =
                resolve_receipt_effect(host, &preparation.effective_input, declaration, &target)?;
            changes.extend(accepted.changes);
            Some(accepted.acceptance)
        }
        None => None,
    };
    let change = FactChange::Transition {
        object_type,
        key: object_id.clone(),
        lifecycle: lifecycle.clone(),
        transition: transition.clone(),
    };
    changes.push(change);
    commit_effect(
        host,
        catalog,
        call,
        authorizer,
        changes,
        Pending {
            effective_input: preparation.effective_input.clone(),
            remaining_fuel: preparation.remaining_fuel,
            target,
            slice: slice.facts,
            reference_only: slice.reference_only,
            link_reads: slice.link_reads,
            predicate_reads: slice.predicate_reads,
            precondition_reads: slice.precondition_keys,
            record_key,
            recorded,
            record,
            series_candidate: None,
            decision_links,
            receipt,
        },
        envelope,
    )
}
