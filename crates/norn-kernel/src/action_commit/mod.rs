//! The action commit path (T2-KER-2c, #2895): authorized calls commit through
//! [`crate::decision_commit`] in one [`CellHost::commit`] (fact writes, ledger
//! events, and idempotency record).
//!
//! Checks (each refusing with nothing committed):
//! 1. **Command & scope** (fail-closed): [`crate::action::check_action_command`] and `scopeRequired`.
//! 2. **Effect**: `fire_transition`, `upsert_input_object`, or owned-rows mutation.
//! 3. **Input preparation**: ZEN derivations against caller input; raw input binds idempotency.
//! 4. **Authorization**: [`ActionAuthorizer`] on committed values (deferred for create upserts).
//! 5. **Preconditions**: ZEN boolean expressions over authorized slice, CAS-verified at commit.
//! 6. **Object kind**: target type matches effect's `objectKind` or subtype.
//! 7. **Idempotency**: exact binding replays; conflicts refuse. Existing targets are authorized
//!    against current state before replay. An absent upsert replay is checked
//!    as an absent current target and remains not-found, rather than becoming
//!    a fresh create.
//! 8. **Effect preparation and proposed-state authorization**: a transition
//!    uses one `transition` fact change; an upsert uses one `create` or the
//!    input's `set` changes. Both go through [`crate::prepare_decision`]. The
//!    target's prepared `put` is canonicalized with the shared effective-value
//!    builder, then authorized as the proposed state.
//!    **Receipt acceptance** (RECEIPT-EFFECT-1 / RECEIPT-CONSUME-1, Core 07):
//!    a `fire_transition` declaring `acceptReceipt` resolves its input's
//!    receipt digest through [`CellHost::resolve_accepted_receipt`], checks the
//!    receipt is bound to the target run and its current generation, and adds
//!    the digest and bound result `set`s plus the acceptance's delivery and
//!    pins to the same commit. The host's pending check refuses a second
//!    consumption; no transition commits without its consumption.
//! 9. **Commit**: the prepared writes and call record commit atomically. A
//!    concurrent call that commits first makes this one stale; if that call
//!    had the same idempotency key and binding, this one replays after current
//!    state authorization.

mod automation_input;
mod input;
mod owned_rows;
mod parameter;
mod predicate;
mod receipt;
mod series;
mod slice;
mod transition;
mod types;
mod upsert;

#[cfg(test)]
#[path = "tests.rs"]
mod reference_lineage_tests;

use std::collections::BTreeSet;

use norn_ir::{ActionEffectProgram, FactChange, ObjectTypeName};

use crate::action::{ActionCommandRejection, ActionCommandShape, check_action_command};
use crate::cell_host::{
    CellCommit, CellHost, CommitError, FactKey, FactMutation, FactState, FactWrite, JsonObject,
};
use crate::decision_commit::{
    DecisionRefusal, FactEvent, TypeCatalog, prepare_action_decision, widen_effect,
};
use crate::envelope::{EnvelopeCounts, TurnEnvelopeMeter, check_decision_envelope};
use crate::limits::EnvelopeLimits;

pub use automation_input::{AutomationInputViolation, validate_automation_static_input};
pub use input::{actor_derived_fields, prepare_action_input, prepare_action_input_for_call};
pub use types::{
    ACTION_CALL_TYPE, ACTION_EFFECT_MUTATE_PARENT_WITH_ROWS, ActionAuthorizer, ActionAuthzRequest,
    ActionCall, ActionCallRecord, ActionDenial, ActionDerivedValue, ActionInputPreparation,
    ActionOutcome, ActionParameterPin, ActionPreconditionOutcome, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest, ActionReferenceReads, ActionRefusal, NamingSeriesInspectionRequest,
    OWNED_ROWS_TYPE, owned_rows_manifest_key,
};

use input::{build_action_commands, check_action_record_size};
use owned_rows::commit_parent_rows;
use receipt::consume_receipt;
use slice::{authorize_slice_type, reference_read_proof};
use transition::commit_transition;
use types::{EnvelopePolicy, Pending};
use upsert::commit_upsert;

/// Commits one authorized `fire_transition` action call (see the module
/// docs for the order of checks).
///
/// # Errors
///
/// An [`ActionRefusal`]; nothing was committed.
pub fn commit_action<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    commit_action_with_limits(host, catalog, call, authorizer, &EnvelopeLimits::V1)
}

/// Commits one action call using an input preparation already computed for
/// this exact action definition, artifact identity/version, and raw input.
///
/// The opaque, non-cloneable token must come from
/// [`prepare_action_input_for_call`] and is consumed here. This lets an API
/// perform authorization with the effective input without evaluating
/// declared derivations a second time.
///
/// # Errors
///
/// An [`ActionRefusal`]; nothing was committed.
pub fn commit_action_with_preparation<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    preparation: ActionInputPreparation<'_>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    commit_action_inner(
        host,
        catalog,
        call,
        authorizer,
        Some(preparation),
        &EnvelopeLimits::V1,
        None,
    )
}

/// Commits one action call under explicit per-decision envelope limits.
///
/// # Errors
///
/// An [`ActionRefusal`]; nothing was committed.
pub fn commit_action_with_limits<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    limits: &EnvelopeLimits,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    commit_action_inner(host, catalog, call, authorizer, None, limits, None)
}

/// Commits one action call under the meter's per-decision and aggregate turn
/// limits.
///
/// A replay and every refusal leave the meter unchanged. A new transition is
/// charged only after its host commit succeeds.
///
/// # Errors
///
/// An [`ActionRefusal`]; nothing was committed and the meter is unchanged.
pub fn commit_action_with_meter<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    meter: &mut TurnEnvelopeMeter,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let limits = meter.decision_limits();
    commit_action_inner(host, catalog, call, authorizer, None, &limits, Some(meter))
}

#[allow(
    clippy::too_many_lines,
    reason = "one dispatch resolves closure, scope, input and effect program before any commit"
)]
fn commit_action_inner<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    preparation: Option<ActionInputPreparation<'_>>,
    limits: &EnvelopeLimits,
    meter: Option<&mut TurnEnvelopeMeter>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let action = call.action;
    // TXN-RULE-1: a declared closure never degrades to its primary effect.
    crate::synchronous_rule::resolve::<H::Error>(catalog, call)?;
    if !action.create_links.is_empty()
        && matches!(
            action.effect_program,
            Some(ActionEffectProgram::MutateParentWithRows { .. })
        )
    {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: action.action_id.clone(),
        });
    }
    // ACTOR-INDEPENDENT-1: only a transition reads a recorded prior actor; a
    // declared check never silently drops on another effect.
    if !action.independent_of.is_empty()
        && !matches!(
            action.effect_program,
            Some(ActionEffectProgram::FireTransition { .. })
        )
    {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: action.action_id.clone(),
        });
    }
    check_scope(call)?;
    let prepared_input = match preparation {
        Some(preparation) if preparation.matches_call(call) => preparation,
        Some(_) => {
            return Err(ActionRefusal::InvalidInput {
                field: "$prepared_input".to_owned(),
            });
        }
        None => prepare_action_input_for_call::<H::Error>(
            action,
            &call.input,
            call.action_artifact_id,
            call.action_version,
            &call.actor,
        )?,
    };
    match action.effect_program.as_ref() {
        None => Err(ActionRefusal::UnsupportedEffect {
            action_id: action.action_id.clone(),
        }),
        // RECEIPT-EFFECT-1: an accepted receipt is consumed in the transition's commit.
        Some(ActionEffectProgram::FireTransition {
            object_id_field,
            object_kind,
            lifecycle,
            transition,
            accept_receipt,
            ..
        }) => commit_transition(
            host,
            catalog,
            call,
            prepared_input,
            authorizer,
            object_id_field,
            object_kind,
            lifecycle,
            transition,
            accept_receipt.as_ref(),
            limits,
            meter,
        ),
        Some(ActionEffectProgram::UpsertInputObject {
            object_id_field,
            object_kind,
            write_mode,
            ..
        }) => commit_upsert(
            host,
            catalog,
            call,
            prepared_input,
            authorizer,
            object_id_field,
            object_kind,
            write_mode,
            limits,
            meter,
        ),
        Some(ActionEffectProgram::MutateParentWithRows {
            object_id_field,
            object_kind,
            parent_values_field,
            row_ops_field,
            parent_version_field,
            parent_delete_field,
            collections,
            ..
        }) => commit_parent_rows(
            host,
            catalog,
            call,
            prepared_input,
            authorizer,
            object_id_field,
            object_kind,
            parent_values_field,
            row_ops_field,
            parent_version_field,
            parent_delete_field.as_deref(),
            collections,
            limits,
            meter,
        ),
    }
}

fn check_kind<X>(
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeName,
    object_kind: &str,
) -> Result<(), ActionRefusal<X>> {
    if catalog
        .lineage(object_type)
        .iter()
        .any(|ancestor| ancestor.as_str() == object_kind)
    {
        Ok(())
    } else {
        Err(ActionRefusal::KindMismatch {
            expected: object_kind.to_owned(),
            actual: object_type.as_str().to_owned(),
        })
    }
}

fn host_failure<X>(error: X) -> ActionRefusal<X> {
    ActionRefusal::Decision(DecisionRefusal::Host(error))
}

/// Loads the target and authorizes the call; existence never leaks.
fn authorize_target<H: CellHost + ?Sized>(
    host: &H,
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    target: &FactKey,
) -> Result<FactState, ActionRefusal<H::Error>> {
    crate::check_fact_key_identity(target)
        .map_err(|error| ActionRefusal::Decision(DecisionRefusal::from_commit(error)))?;
    let state = host.load(target).map_err(host_failure)?;
    authorize_current_target(call, effective_input, authorizer, target, &state)?;
    Ok(state)
}

/// Authorizes one snapshot of the committed target, preserving absence
/// concealment for current-state checks.
fn authorize_current_target<X>(
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    target: &FactKey,
    state: &FactState,
) -> Result<(), ActionRefusal<X>> {
    let verdict = authorizer.authorize(&ActionAuthzRequest {
        action: call.action,
        scope: &call.scope,
        actor: &call.actor,
        input: effective_input,
        target,
        target_values: state.values.as_ref(),
    });
    match (verdict, state.exists()) {
        (Ok(()), true) => Ok(()),
        (Err(ActionDenial::Forbidden { reason }), true) => {
            Err(ActionRefusal::PermissionDenied { reason })
        }
        (Err(ActionDenial::Invisible), _)
        | (Ok(()) | Err(ActionDenial::Forbidden { .. }), false) => {
            Err(ActionRefusal::TargetNotFound)
        }
    }
}

/// Loads an upsert target and authorizes its current state when it exists.
/// A new create is authorized after its prepared values are available.
fn authorize_upsert_target<H: CellHost + ?Sized>(
    host: &H,
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    target: &FactKey,
) -> Result<FactState, ActionRefusal<H::Error>> {
    crate::check_fact_key_identity(target)
        .map_err(|error| ActionRefusal::Decision(DecisionRefusal::from_commit(error)))?;
    let state = host.load(target).map_err(host_failure)?;
    // A create has no resource state to inspect yet. Its identity/scope and
    // proposed-resource authorization is checked after the prepared write is
    // available, without sending an empty resource to the policy engine.
    if !state.exists() {
        return Ok(state);
    }
    authorize_current_target(call, effective_input, authorizer, target, &state)?;
    Ok(state)
}

/// Prepares an action's changes and commits them with the call record.
#[allow(
    clippy::too_many_lines,
    reason = "business changes, counter state, receipt, and race replay share one commit boundary"
)]
fn commit_effect<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    changes: Vec<FactChange>,
    pending: Pending,
    envelope: EnvelopePolicy<'_>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let EnvelopePolicy { limits, meter } = envelope;
    let Pending {
        effective_input,
        mut remaining_fuel,
        target,
        slice,
        reference_only,
        link_reads,
        predicate_reads,
        precondition_reads,
        record_key,
        recorded,
        record,
        series_candidate,
        decision_links,
        receipt,
        creator_stamp,
    } = pending;
    let generated_key = series_candidate.is_some();
    let additional_events = usize::from(generated_key);
    let fact_event_count = changes
        .len()
        .checked_add(additional_events)
        .ok_or_else(|| {
            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: "the action output event count overflowed".to_owned(),
            })
        })?;

    let mut counts =
        check_action_envelope::<H::Error>(limits, fact_event_count, call.action.commands.len())?;
    let commands = build_action_commands::<H::Error>(
        call.action,
        &effective_input,
        &format!("{}/{}", call.action.action_id, call.idempotency_key),
        &mut remaining_fuel,
    )?;
    let output = norn_ir::DecisionOutput {
        facts: changes,
        links: decision_links.clone(),
        commands,
        timers: Vec::new(),
    };
    let mut prepared = prepare_action_decision(
        catalog,
        &slice,
        &call.turn,
        &output,
        limits,
        creator_stamp.as_ref(),
    )?;
    for write in &mut prepared.writes {
        let key = write.key.clone();
        authorize_proposed_target(
            catalog,
            call,
            &effective_input,
            authorizer,
            &key,
            std::slice::from_mut(write),
        )?;
    }
    let created_links = crate::decision_links::prepare_action_commit_links(
        host,
        &crate::decision_links::ActionCommitLinks {
            catalog,
            call,
            effective_input: &effective_input,
            authorizer,
            target: &target,
            slice: &slice,
            prepared_writes: &prepared.writes,
            decision_links: &decision_links,
            limits,
        },
    )?;
    let record = action_record_with_transition(record, &prepared.events);
    // Transition metadata is added after the earlier precondition/evidence
    // checks, so include the finalized receipt in the aggregate output bound.
    check_action_record_size::<H::Error>(call.action, &record, limits)?;
    let mut events = prepared.events;
    if let Some(candidate) = &series_candidate {
        events.push(candidate.event.clone());
    }
    let action_call = format!("{}/{}", call.action.action_id, call.idempotency_key);
    append_command_events(&mut events, &prepared.effects, &action_call);
    counts.events = events.len().saturating_add(created_links.len());
    check_decision_envelope(limits, counts).map_err(|exceeded| {
        ActionRefusal::Decision(DecisionRefusal::EnvelopeExceeded {
            kind: exceeded.kind,
            limit: exceeded.limit,
            actual: exceeded.actual,
        })
    })?;
    let mut writes = prepared.writes;
    let mut reads = prepared.reads;
    if let Some(candidate) = series_candidate {
        writes.push(candidate.counter_write);
        reads.extend(candidate.skipped_reads);
    }
    let target_before = receipt::target_before_version(catalog, &slice, &target);
    receipt::ensure_target_read(catalog, &target, target_before, &writes, &mut reads);
    writes.push(FactWrite {
        key: record_key.clone(),
        expected: recorded.version,
        mutation: FactMutation::Put {
            values: record.to_values(),
        },
    });
    let mut consumes = prepared.consumes;
    let receipt_delivery = match receipt {
        Some(acceptance) => {
            let delivery = consume_receipt::<H::Error>(
                catalog,
                consumes.is_some(),
                &acceptance,
                &writes,
                &mut reads,
            )?;
            consumes = Some(delivery.clone());
            Some(delivery)
        }
        None => None,
    };
    let commit = CellCommit {
        consumes,
        reads,
        writes,
        events: events.into_iter().map(Into::into).collect(),
        effects: prepared.effects.into_iter().map(widen_effect).collect(),
        links: created_links,
        link_reads,
        predicate_reads,
    };
    check_action_turn_meter::<H::Error>(meter.as_deref(), counts)?;
    let quantity = crate::action_quantity::prepare_quantity_action(call, &effective_input)?;
    // Precision granted after full snapshots were loaded cannot erase their
    // value-dependent admission. Revocation refuses before any write rather
    // than using empty values under newly dependent authorization.
    if reference_only && !authorizer.supports_action_reference_lineage(call.action) {
        return Err(ActionRefusal::TargetNotFound);
    }
    let reference_reads = (reference_only && !generated_key)
        .then(|| reference_read_proof(catalog, call.action, authorizer, &commit, &target))
        .flatten();
    // ACTOR-STAMP-1: the proof binds this exact transaction; only it admits the stamp.
    let stamp =
        creator_stamp.map(|stamp| crate::CreatorStampProof::new(stamp, &commit, limits.max_events));
    let result = if let Some(proof) = stamp {
        let quantity = quantity
            .as_ref()
            .map(|(request, key)| (request, key.as_str()));
        host.commit_stamped(commit, proof, quantity)
    } else if let Some((request, key)) = quantity {
        if let Some(proof) = reference_reads {
            host.commit_action_references(commit, proof, Some((&request, &key)))
        } else {
            host.commit_action_quantity(commit, std::slice::from_ref(&request), &key)
        }
    } else if let Some(proof) = reference_reads {
        host.commit_action_references(commit, proof, None)
    } else {
        host.commit(commit)
    };
    match result {
        Ok(receipt) => {
            if let Some(meter) = meter {
                meter.charge(counts);
            }
            let target_version =
                receipt::committed_target_version(catalog, &receipt, &target, target_before);
            Ok(ActionOutcome::Committed {
                receipt,
                record,
                target_version,
            })
        }
        // RECEIPT-CONSUME-1: the host refused the result inside the commit
        // (revoked command authority or a passed deadline); nothing applied.
        Err(CommitError::ReceiptNotAccepted { consumed })
            if receipt_delivery.as_ref() == Some(&consumed) =>
        {
            Err(ActionRefusal::ReceiptRefused {
                reason: crate::receipt_binding::ReceiptRefusal::NotAccepted,
            })
        }
        Err(error) if error.is_invariant_refusal() => Err(ActionRefusal::Decision(
            ledger_action_invariant_refusal(host, call, error),
        )),
        Err(error) => action_commit_conflict(
            host,
            catalog,
            call,
            authorizer,
            error,
            &ActionReplayState {
                effective_input: &effective_input,
                record_key: &record_key,
                record: &record,
                precondition_reads: &precondition_reads,
                generated_key,
            },
        )
        .map_err(|refusal| {
            // RECEIPT-EFFECT-1: the receipt's delivery was already consumed
            // or is not the run's current generation; nothing committed.
            if let ActionRefusal::Decision(DecisionRefusal::NotPending { consumed }) = &refusal
                && receipt_delivery.as_ref() == Some(consumed)
            {
                ActionRefusal::ReceiptRefused {
                    reason: crate::receipt_binding::ReceiptRefusal::NotPending,
                }
            } else {
                refusal
            }
        }),
    }
}

fn action_record_with_transition(
    record: ActionCallRecord,
    events: &[FactEvent],
) -> ActionCallRecord {
    events
        .iter()
        .find_map(|event| match event {
            FactEvent::Transitioned { key, from, to, .. } if *key == record.object => {
                Some(ActionCallRecord {
                    from: from.clone(),
                    to: to.clone(),
                    ..record.clone()
                })
            }
            FactEvent::Transitioned { .. }
            | FactEvent::CommandEnqueued { .. }
            | FactEvent::NamingSeriesAllocated { .. }
            | FactEvent::NamingSeriesCounterSeeded { .. }
            | FactEvent::Created { .. }
            | FactEvent::Set { .. }
            | FactEvent::Modified { .. }
            | FactEvent::Deleted { .. }
            | FactEvent::InvariantRefused { .. } => None,
        })
        .unwrap_or(record)
}

fn check_action_turn_meter<X>(
    meter: Option<&TurnEnvelopeMeter>,
    counts: EnvelopeCounts,
) -> Result<(), ActionRefusal<X>> {
    if let Some(meter) = meter {
        meter.check(counts).map_err(|exceeded| {
            ActionRefusal::Decision(DecisionRefusal::TurnEnvelopeExceeded {
                kind: exceeded.kind,
                limit: exceeded.limit,
                actual: exceeded.actual,
            })
        })?;
    }
    Ok(())
}

struct ActionReplayState<'a> {
    effective_input: &'a JsonObject,
    record_key: &'a FactKey,
    record: &'a ActionCallRecord,
    precondition_reads: &'a BTreeSet<FactKey>,
    generated_key: bool,
}

fn action_commit_conflict<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    error: CommitError<H::Error>,
    state: &ActionReplayState<'_>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>> {
    let ActionReplayState {
        effective_input,
        record_key,
        record,
        precondition_reads,
        generated_key,
    } = *state;
    authorize_identity_collision(
        host,
        catalog,
        call,
        effective_input,
        authorizer,
        &record.object,
        &error,
    )?;
    if generated_key {
        // A generated candidate may lose the counter CAS to a different key.
        // The allocation loop reloads the call record and revalidates replay
        // against the winner's exact key before trying a new candidate.
        return Err(precondition_read_refusal(&error, precondition_reads)
            .unwrap_or_else(|| ActionRefusal::Decision(DecisionRefusal::from_commit(error))));
    }
    if matches!(
        &error,
        CommitError::Conflict { .. }
            | CommitError::LinkReadConflict { .. }
            | CommitError::UnsupportedLinkRead { .. }
            | CommitError::PredicateReadConflict { .. }
            | CommitError::NotPending { .. }
    ) {
        // A concurrent call committed first (it changed the target,
        // or consumed the same delivery): replay it if it was this
        // same call, else report the original refusal.
        let raced = host.load(record_key).map_err(host_failure)?;
        if let Some(outcome) = replay(&raced, record, &call.idempotency_key)? {
            // The winner may have changed authorization-relevant
            // values. A replay must use current authorization too.
            authorize_target(host, call, effective_input, authorizer, &record.object)?;
            return Ok(outcome);
        }
    }
    Err(precondition_read_refusal(&error, precondition_reads)
        .unwrap_or_else(|| ActionRefusal::Decision(DecisionRefusal::from_commit(error))))
}

/// ID-UNIQUE-1: conflict details require ordinary collection/object read authority.
fn authorize_identity_collision<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    target: &FactKey,
    error: &CommitError<H::Error>,
) -> Result<(), ActionRefusal<H::Error>> {
    let CommitError::IdentityCollision { conflicts, .. } = error else {
        return Ok(());
    };
    for key in conflicts {
        authorize_slice_type(call, input, authorizer, &key.object_type)?;
        let bounded = host
            .load_bounded(
                key,
                norn_ir::ResourceLimitProfile::norn_limits_v1()
                    .slice
                    .max_canonical_bytes,
            )
            .map_err(host_failure)?
            .ok_or(ActionRefusal::TargetNotFound)?;
        if bounded.state.values.is_none() {
            if key.key == target.key
                && catalog
                    .lineage(&target.object_type)
                    .contains(&key.object_type)
            {
                continue;
            }
            return Err(ActionRefusal::TargetNotFound);
        }
        let concrete = bounded.concrete_type.as_ref().map_or_else(
            || key.clone(),
            |kind| FactKey::new(kind.as_str(), key.key.clone()),
        );
        if authorizer
            .authorize_read(&ActionReadAuthzRequest {
                action: call.action,
                scope: &call.scope,
                actor: &call.actor,
                input,
                declared_type: &key.object_type,
                target: &concrete,
                target_values: bounded.state.values.as_ref(),
            })
            .is_err()
        {
            return Err(ActionRefusal::TargetNotFound);
        }
    }
    Ok(())
}

fn precondition_read_refusal<X>(
    error: &CommitError<X>,
    precondition_reads: &BTreeSet<FactKey>,
) -> Option<ActionRefusal<X>> {
    if let CommitError::PredicateReadConflict { object_type } = error {
        return Some(ActionRefusal::StaleAggregateRead {
            object_type: object_type.clone(),
        });
    }
    let stale_fact = match error {
        CommitError::Conflict { key, .. } if precondition_reads.contains(key) => Some(key.clone()),
        CommitError::Conflict { key, .. } => precondition_reads
            .iter()
            .find(|read| crate::snapshot_observe::tombstone_key(read) == *key)
            .cloned(),
        CommitError::IdentityReuse
        | CommitError::IdentityCollision { .. }
        | CommitError::AtomicBoundary { .. }
        | CommitError::AtomicDeliveryBoundary { .. }
        | CommitError::ImmutableObject { .. }
        | CommitError::OwnedRowsProtected { .. }
        | CommitError::DuplicateKey { .. }
        | CommitError::NotPending { .. }
        | CommitError::ReceiptNotAccepted { .. }
        | CommitError::DuplicateLink { .. }
        | CommitError::IdentifierTooLong { .. }
        | CommitError::LinkConflict { .. }
        | CommitError::LinkReadConflict { .. }
        | CommitError::UnsupportedLinkRead { .. }
        | CommitError::DuplicateLinkRead { .. }
        | CommitError::LinkReadTooLarge { .. }
        | CommitError::InvalidLinkRead { .. }
        | CommitError::PredicateReadConflict { .. }
        | CommitError::UnsupportedPredicateRead { .. }
        | CommitError::InvalidPredicateRead { .. }
        | CommitError::DanglingLink { .. }
        | CommitError::LinkTarget { .. }
        | CommitError::LinkCardinality { .. }
        | CommitError::LinkRequired { .. }
        | CommitError::TypeMismatch { .. }
        | CommitError::EffectiveValues { .. }
        | CommitError::TraversalLimitExceeded { .. }
        | CommitError::AccountingPosting { .. }
        | CommitError::InvariantViolation { .. }
        | CommitError::InvariantBudgetExceeded { .. }
        | CommitError::Law { .. }
        | CommitError::InvariantModel { .. }
        | CommitError::ObservationAuthorization
        | CommitError::EventLimitExceeded { .. }
        | CommitError::Overflow
        | CommitError::Host(_) => None,
    };
    let stale_link = match error {
        CommitError::LinkReadConflict { source_key, link }
        | CommitError::UnsupportedLinkRead { source_key, link } => {
            Some((source_key.clone(), link.clone()))
        }
        CommitError::IdentityReuse
        | CommitError::IdentityCollision { .. }
        | CommitError::AtomicBoundary { .. }
        | CommitError::AtomicDeliveryBoundary { .. }
        | CommitError::ImmutableObject { .. }
        | CommitError::OwnedRowsProtected { .. }
        | CommitError::DuplicateKey { .. }
        | CommitError::NotPending { .. }
        | CommitError::ReceiptNotAccepted { .. }
        | CommitError::Conflict { .. }
        | CommitError::DuplicateLink { .. }
        | CommitError::IdentifierTooLong { .. }
        | CommitError::LinkConflict { .. }
        | CommitError::DuplicateLinkRead { .. }
        | CommitError::LinkReadTooLarge { .. }
        | CommitError::InvalidLinkRead { .. }
        | CommitError::PredicateReadConflict { .. }
        | CommitError::UnsupportedPredicateRead { .. }
        | CommitError::InvalidPredicateRead { .. }
        | CommitError::DanglingLink { .. }
        | CommitError::LinkTarget { .. }
        | CommitError::LinkCardinality { .. }
        | CommitError::LinkRequired { .. }
        | CommitError::TypeMismatch { .. }
        | CommitError::EffectiveValues { .. }
        | CommitError::TraversalLimitExceeded { .. }
        | CommitError::AccountingPosting { .. }
        | CommitError::InvariantViolation { .. }
        | CommitError::InvariantBudgetExceeded { .. }
        | CommitError::Law { .. }
        | CommitError::InvariantModel { .. }
        | CommitError::ObservationAuthorization
        | CommitError::EventLimitExceeded { .. }
        | CommitError::Overflow
        | CommitError::Host(_) => None,
    };
    if let Some(key) = stale_fact {
        return Some(ActionRefusal::StalePreconditionRead { key, link: None });
    }
    stale_link.map(|(key, link)| ActionRefusal::StalePreconditionRead {
        key,
        link: Some(link),
    })
}

fn append_command_events(
    events: &mut Vec<FactEvent>,
    effects: &[crate::effect::Effect<JsonObject, JsonObject>],
    action_call: &str,
) {
    for effect in effects {
        if let crate::effect::Effect::Perform(command) = effect {
            events.push(FactEvent::CommandEnqueued {
                command_id: command.id.0.clone(),
                generation: command.generation.0,
                idempotency_key: command.idempotency_key.0.clone(),
                action_call: action_call.to_owned(),
                payload: command.payload.clone(),
            });
        }
    }
}

/// Canonicalizes and authorizes the target's proposed post-commit values.
/// The same canonical value map is retained in the write that will be
/// committed, so policy and storage see an identical resource.
fn authorize_proposed_target<X>(
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    target: &FactKey,
    writes: &mut [FactWrite],
) -> Result<(), ActionRefusal<X>> {
    let Some(write) = writes.iter_mut().find(|write| write.key == *target) else {
        return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("the prepared action does not write its target {target}"),
        }));
    };
    let FactMutation::Put { values } = &mut write.mutation else {
        return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("the prepared action does not put its target {target}"),
        }));
    };
    let canonical = catalog
        .effective_object_values(&target.object_type, values)
        .map_err(|error| {
            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: format!("the effective values for {target}: {error}"),
            })
        })?;
    *values = canonical;
    match authorizer.authorize(&ActionAuthzRequest {
        action: call.action,
        scope: &call.scope,
        actor: &call.actor,
        input: effective_input,
        target,
        target_values: Some(values),
    }) {
        Ok(()) => Ok(()),
        // Invisibility continues to conceal the target, even for a create.
        // A policy denial is a real authorization refusal because the
        // proposed resource values are now available to decide against.
        Err(ActionDenial::Invisible) => Err(ActionRefusal::TargetNotFound),
        Err(ActionDenial::Forbidden { reason }) => Err(ActionRefusal::PermissionDenied { reason }),
    }
}

fn ledger_action_invariant_refusal<H>(
    host: &mut H,
    call: &ActionCall<'_>,
    error: CommitError<H::Error>,
) -> DecisionRefusal<H::Error>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
{
    let attempt_id = format!("{}/{}", call.action.action_id, call.idempotency_key);
    let Some(event) = error.invariant_refusal_event(attempt_id) else {
        return DecisionRefusal::from_commit(error);
    };
    let refusal = DecisionRefusal::from_commit(error);
    let mut refusal_commit = CellCommit::empty();
    refusal_commit.consumes.clone_from(&call.turn.consumes);
    refusal_commit.events.push(event.into());
    match host.commit(refusal_commit) {
        Ok(_) => refusal,
        Err(error) => DecisionRefusal::from_commit(error),
    }
}

/// Checks the output envelope, including one event per committed command.
fn check_action_envelope<X>(
    limits: &EnvelopeLimits,
    fact_change_count: usize,
    command_count: usize,
) -> Result<EnvelopeCounts, ActionRefusal<X>> {
    let event_count = fact_change_count
        .checked_add(command_count)
        .ok_or_else(|| {
            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: "the action output event count overflowed".to_owned(),
            })
        })?;
    let counts = EnvelopeCounts::new(event_count, command_count, 0);
    check_decision_envelope(limits, counts).map_err(|exceeded| {
        ActionRefusal::Decision(DecisionRefusal::EnvelopeExceeded {
            kind: exceeded.kind,
            limit: exceeded.limit,
            actual: exceeded.actual,
        })
    })?;
    Ok(counts)
}

/// Command identity and scope fail-closed, then every required scope key.
fn check_scope<X>(call: &ActionCall<'_>) -> Result<(), ActionRefusal<X>> {
    check_action_command(&ActionCommandShape {
        org_id: call.scope.get("orgId").map(String::as_str),
        action_id: &call.action.action_id,
        action_version: Some(call.action_version),
        idempotency_key: &call.idempotency_key,
    })
    .map_err(ActionRefusal::Command)?;
    if call.action_artifact_id.trim().is_empty() {
        return Err(ActionRefusal::Command(ActionCommandRejection::BlankField));
    }
    for key in &call.action.scope_required {
        if call
            .scope
            .get(key)
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(ActionRefusal::MissingScope { key: key.clone() });
        }
    }
    if let Some(trace) = &call.trace {
        trace
            .validate()
            .map_err(|error| ActionRefusal::InvalidInput {
                field: error.field().to_owned(),
            })?;
    }
    Ok(())
}

/// The replay of an already committed call under the same key, or `None`
/// when no call was committed under it. The decision is
/// [`crate::replay_decision::decide_replay`]; this only decodes the record.
fn replay<X>(
    recorded: &FactState,
    call: &ActionCallRecord,
    key: &str,
) -> Result<Option<ActionOutcome>, ActionRefusal<X>> {
    use crate::replay_decision::{Recorded, ReplayDecision, decide_replay};
    let stored = match &recorded.values {
        None => Recorded::Empty,
        Some(values) => {
            ActionCallRecord::from_values(values).map_or(Recorded::Unreadable, Recorded::Record)
        }
    };
    match decide_replay(stored, call) {
        ReplayDecision::Fresh => Ok(None),
        ReplayDecision::Replay(record) => Ok(Some(ActionOutcome::Replayed { record })),
        ReplayDecision::Conflict => Err(ActionRefusal::IdempotencyConflict {
            key: key.to_owned(),
        }),
    }
}
