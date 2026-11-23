use norn_ir::{
    FactChange, KeyName, NamingSeriesIR, ObjectTypeName, PropertyId, ResourceLimitProfile,
};

use crate::cell_host::{CellHost, FactKey, FactState, JsonObject, JsonValue};
use crate::decision_commit::{DecisionRefusal, FactEvent, TypeCatalog};
use crate::envelope::TurnEnvelopeMeter;
use crate::limits::{EnvelopeLimits, SliceMeter};
use crate::naming_series::{prepare_candidate, scope_from_values};

use super::input::{check_action_record_size, validate_action_input};
use super::slice::{ActionSliceContext, check_action_preconditions, load_action_slice};
use super::types::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionCall, ActionCallRecord, ActionInputPreparation,
    ActionOutcome, ActionRefusal, EnvelopePolicy, Pending,
};
use super::upsert::upsert_stored_input;
use super::{authorize_target, commit_effect, host_failure};

const MAX_NAMING_SERIES_RETRIES: usize = 128;

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "allocation, idempotency, authorization, and CAS retry form one bounded action path"
)]
pub(super) fn commit_upsert_series<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    mut preparation: ActionInputPreparation<'_>,
    authorizer: &dyn ActionAuthorizer,
    object_id_field: &str,
    object_type: &ObjectTypeName,
    series_owner: &ObjectTypeName,
    key_name: &KeyName,
    key_properties: &[PropertyId],
    declaration: &NamingSeriesIR,
    limits: &EnvelopeLimits,
    meter: Option<&mut TurnEnvelopeMeter>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let record_key = FactKey::new(
        ACTION_CALL_TYPE,
        format!("{}/{}", call.action.action_id, call.idempotency_key),
    );
    let mut meter = meter;
    for _attempt in 0..MAX_NAMING_SERIES_RETRIES {
        // A replay is resolved before reading the counter or ledger clock. It
        // therefore retains the original generated key after time or counter
        // state advances.
        let recorded = host.load(&record_key).map_err(host_failure)?;
        if let Some(record) = recorded_generated_call(
            &recorded,
            &call.action.action_id,
            object_type,
            &call.input,
            &call.actor,
            &call.idempotency_key,
        )? {
            validate_generated_replay_input(
                host,
                catalog,
                call,
                &mut preparation,
                authorizer,
                GeneratedReplay {
                    object_id_field,
                    record: &record,
                },
                limits,
            )?;
            return Ok(ActionOutcome::Replayed { record });
        }

        let stored_input = upsert_stored_input(
            catalog,
            call.action,
            object_type,
            &preparation.effective_input,
        );
        let base_values = catalog
            .effective_object_values(object_type, &stored_input)
            .map_err(|error| {
                ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("the effective values for {}: {error}", object_type.as_str()),
                })
            })?;
        let scope = scope_from_values(declaration, &base_values)
            .map_err(|reason| ActionRefusal::Decision(DecisionRefusal::InvalidOutput { reason }))?;
        let candidate = prepare_candidate(
            host,
            catalog,
            object_type,
            series_owner,
            key_name,
            key_properties,
            declaration,
            &scope,
            call.turn.now.as_millis(),
        )
        .map_err(ActionRefusal::Decision)?;

        let mut generated_input = preparation.effective_input.clone();
        generated_input.insert(
            object_id_field.to_owned(),
            JsonValue::String(candidate.target.key.clone()),
        );
        validate_action_input::<H::Error>(call.action, &generated_input)?;
        let target = candidate.target.clone();
        let state = candidate.target_state.clone();
        let slice = load_action_slice(
            &ActionSliceContext {
                host,
                catalog,
                call,
                effective_input: &generated_input,
                authorizer,
            },
            &target,
            &state,
            SliceMeter::for_profile(&ResourceLimitProfile::norn_limits_v1()),
        )?;
        let mut receipt_evidence_bytes = preparation.receipt_evidence_bytes;
        let precondition_outcomes = check_action_preconditions(
            catalog,
            call.action,
            &generated_input,
            &slice,
            &mut preparation.remaining_fuel,
            &mut receipt_evidence_bytes,
            usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
        )?;
        let record = ActionCallRecord {
            action_id: call.action.action_id.clone(),
            action_artifact_id: Some(call.action_artifact_id.to_owned()),
            action_version: Some(call.action_version.to_owned()),
            effect_kind: Some("upsert_input_object".to_owned()),
            rule_binding: None,
            row_operations: Vec::new(),
            object: target.clone(),
            input: call.input.clone(),
            effective_input: Some(generated_input.clone()),
            derived_values: preparation.derived_values.clone(),
            precondition_outcomes,
            actor: call.actor.clone(),
            turn: call.turn.id.clone(),
            transition: String::new(),
            from: String::new(),
            to: String::new(),
        };
        check_action_record_size::<H::Error>(call.action, &record, limits)?;
        let changes = vec![FactChange::Create {
            object_type: object_type.clone(),
            key: target.key.clone(),
            values: upsert_stored_input(catalog, call.action, object_type, &generated_input),
        }];
        let pending = Pending {
            effective_input: generated_input,
            target,
            slice: slice.facts,
            reference_only: slice.reference_only,
            link_reads: slice.link_reads,
            predicate_reads: slice.predicate_reads,
            precondition_reads: slice.precondition_keys,
            record_key: record_key.clone(),
            recorded,
            record,
            series_candidate: Some(candidate),
            remaining_fuel: preparation.remaining_fuel,
            receipt: None,
        };
        let outcome = commit_effect(
            host,
            catalog,
            call,
            authorizer,
            changes,
            pending,
            EnvelopePolicy {
                limits,
                meter: meter.as_deref_mut(),
            },
        );
        match outcome {
            Err(ActionRefusal::Decision(DecisionRefusal::Stale { .. })) => {}
            Err(refusal @ ActionRefusal::Decision(DecisionRefusal::NotPending { .. })) => {
                // A consumed delivery is not retried, but an exact concurrent
                // winner may have committed this call under the same key.
                let raced = host.load(&record_key).map_err(host_failure)?;
                if let Some(record) = recorded_generated_call(
                    &raced,
                    &call.action.action_id,
                    object_type,
                    &call.input,
                    &call.actor,
                    &call.idempotency_key,
                )? {
                    validate_generated_replay_input(
                        host,
                        catalog,
                        call,
                        &mut preparation,
                        authorizer,
                        GeneratedReplay {
                            object_id_field,
                            record: &record,
                        },
                        limits,
                    )?;
                    return Ok(ActionOutcome::Replayed { record });
                }
                return Err(refusal);
            }
            result => return result,
        }
    }
    Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
        reason: "naming-series allocation exceeded its 128 compare-and-swap retries".to_owned(),
    }))
}

#[derive(Clone, Copy)]
struct GeneratedReplay<'a> {
    object_id_field: &'a str,
    record: &'a ActionCallRecord,
}

fn validate_generated_replay_input<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    preparation: &mut ActionInputPreparation<'_>,
    authorizer: &dyn ActionAuthorizer,
    replay: GeneratedReplay<'_>,
    limits: &EnvelopeLimits,
) -> Result<(), ActionRefusal<H::Error>> {
    let mut input = preparation.effective_input.clone();
    input.insert(
        replay.object_id_field.to_owned(),
        JsonValue::String(replay.record.object.key.clone()),
    );
    validate_action_input::<H::Error>(call.action, &input)?;
    let state = authorize_target(host, call, &input, authorizer, &replay.record.object)?;
    let slice = load_action_slice(
        &ActionSliceContext {
            host,
            catalog,
            call,
            effective_input: &input,
            authorizer,
        },
        &replay.record.object,
        &state,
        SliceMeter::for_profile(&ResourceLimitProfile::norn_limits_v1()),
    )?;
    check_action_preconditions(
        catalog,
        call.action,
        &input,
        &slice,
        &mut preparation.remaining_fuel,
        &mut preparation.receipt_evidence_bytes,
        usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
    )?;
    Ok(())
}

fn recorded_generated_call<X>(
    recorded: &FactState,
    action_id: &str,
    object_type: &ObjectTypeName,
    input: &JsonObject,
    actor: &str,
    key: &str,
) -> Result<Option<ActionCallRecord>, ActionRefusal<X>> {
    if recorded.version == crate::cell_host::FactVersion::ABSENT {
        return Ok(None);
    }
    let Some(values) = &recorded.values else {
        return Err(ActionRefusal::IdempotencyConflict {
            key: key.to_owned(),
        });
    };
    let Some(record) = ActionCallRecord::from_values(values) else {
        return Err(ActionRefusal::IdempotencyConflict {
            key: key.to_owned(),
        });
    };
    if record.binds_same_generated_call(action_id, object_type, input, actor) {
        Ok(Some(record))
    } else {
        Err(ActionRefusal::IdempotencyConflict {
            key: key.to_owned(),
        })
    }
}
