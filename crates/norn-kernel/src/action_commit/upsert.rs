use norn_ir::{ActionRoot, FactChange, ObjectTypeName, ResourceLimitProfile, UpsertWriteMode};

use crate::cell_host::{CellHost, FactKey, JsonObject, JsonValue};
use crate::decision_commit::{FactEvent, TypeCatalog};
use crate::envelope::TurnEnvelopeMeter;
use crate::limits::{EnvelopeLimits, SliceMeter};
use crate::naming_series::type_series;

use super::input::check_action_record_size;
use super::series::commit_upsert_series;
use super::slice::{ActionSliceContext, check_action_preconditions, load_action_slice};
use super::types::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionCall, ActionCallRecord, ActionInputPreparation,
    ActionOutcome, ActionRefusal, EnvelopePolicy, NamingSeriesInspectionRequest, Pending,
};
use super::{
    authorize_current_target, authorize_upsert_target, check_kind, commit_effect, host_failure,
    replay,
};

pub(super) fn upsert_stored_input(
    catalog: &TypeCatalog<'_>,
    action: &ActionRoot,
    object_type: &ObjectTypeName,
    input: &JsonObject,
) -> JsonObject {
    let mut values = input.clone();
    for binding in &action.create_links {
        if catalog
            .effective_type(object_type)
            .is_some_and(|definition| definition.property(&binding.target_id_field).is_none())
        {
            values.remove(&binding.target_id_field);
        }
    }
    values
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the admitted action effect, host, and turn policy are explicit commit inputs"
)]
pub(super) fn commit_upsert<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    mut preparation: ActionInputPreparation<'_>,
    authorizer: &dyn ActionAuthorizer,
    object_id_field: &str,
    object_kind: &str,
    write_mode: &UpsertWriteMode,
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
    let object_type = call
        .object_type
        .clone()
        .unwrap_or_else(|| ObjectTypeName(object_kind.to_owned()));
    check_kind(catalog, &object_type, object_kind)?;
    let naming_series = type_series(catalog, &object_type);
    if matches!(write_mode, UpsertWriteMode::CreateOnly)
        && let Some(series) = naming_series.as_ref()
    {
        if series.key_properties != [norn_ir::PropertyId(object_id_field.to_owned())]
            || preparation.effective_input.contains_key(object_id_field)
        {
            return Err(ActionRefusal::InvalidInput {
                field: object_id_field.to_owned(),
            });
        }
        let disclose = authorizer.can_inspect_naming_series(&NamingSeriesInspectionRequest {
            action,
            scope: &call.scope,
            actor: &call.actor,
            object_type: &object_type,
            series_owner: series.owner,
            key_name: series.key_name,
        });
        let outcome = commit_upsert_series(
            host,
            catalog,
            call,
            preparation,
            authorizer,
            object_id_field,
            &object_type,
            series.owner,
            series.key_name,
            series.key_properties,
            series.declaration,
            limits,
            meter,
        );
        // No honest target exists for some allocator failures. Conceal all
        // refusals alike unless diagnostic access was separately granted, so
        // denied calls cannot distinguish a healthy counter from a bad one.
        return if disclose {
            outcome
        } else {
            outcome.map_err(|_concealed_refusal| ActionRefusal::TargetNotFound)
        };
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

    let target = FactKey {
        object_type: object_type.clone(),
        key: object_id.clone(),
    };
    let state = authorize_upsert_target(
        host,
        call,
        &preparation.effective_input,
        authorizer,
        &target,
    )?;
    if naming_series.is_some() && matches!(write_mode, UpsertWriteMode::Upsert) && !state.exists() {
        return Err(ActionRefusal::WriteModeFailed {
            mode: "upsert".to_owned(),
            reason: "a naming-series object cannot be created with a caller-supplied key"
                .to_owned(),
        });
    }

    let record_key = FactKey::new(
        ACTION_CALL_TYPE,
        format!("{}/{}", action.action_id, call.idempotency_key),
    );
    let record = ActionCallRecord {
        action_id: action.action_id.clone(),
        action_artifact_id: Some(call.action_artifact_id.to_owned()),
        action_version: Some(call.action_version.to_owned()),
        effect_kind: Some("upsert_input_object".to_owned()),
        rule_binding: None,
        object: target.clone(),
        input: call.input.clone(),
        effective_input: preparation.recorded_effective_input(),
        derived_values: preparation.derived_values.clone(),
        precondition_outcomes: Vec::new(),
        parameter_pins: Vec::new(),
        actor: call.actor.clone(),
        turn: call.turn.id.clone(),
        transition: String::new(),
        from: String::new(),
        to: String::new(),
        row_operations: Vec::new(),
        trace: call.trace.clone(),
    };
    let recorded = host.load(&record_key).map_err(host_failure)?;
    if !state.exists() && recorded.exists() {
        // An absent target with an existing idempotency record is not a new
        // create. Recheck current state so a replay cannot bypass current
        // authorization, and preserve the not-found response for the absent
        // target regardless of the policy verdict.
        authorize_current_target(
            call,
            &preparation.effective_input,
            authorizer,
            &target,
            &state,
        )?;
    }
    if let Some(outcome) = replay(&recorded, &record, &call.idempotency_key)? {
        return Ok(outcome);
    }

    let slice = load_action_slice(
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
    let mut record = record;
    record
        .precondition_outcomes
        .clone_from(&preparation.precondition_outcomes);
    record.parameter_pins.clone_from(&slice.parameter_pins);
    check_action_record_size::<H::Error>(call.action, &record, limits)?;

    let mut business_input =
        upsert_stored_input(catalog, action, &object_type, &preparation.effective_input);
    if let Some(quantity) = &action.quantity_effect {
        business_input.remove(&quantity.balance_versions_field);
    }
    let changes = match write_mode {
        UpsertWriteMode::Upsert | UpsertWriteMode::CreateOnly if !state.exists() => {
            vec![FactChange::Create {
                object_type: object_type.clone(),
                key: object_id.clone(),
                values: business_input.clone(),
            }]
        }
        UpsertWriteMode::Upsert => business_input
            .iter()
            .map(|(property, value)| FactChange::Set {
                object_type: object_type.clone(),
                key: object_id.clone(),
                property: norn_ir::PropertyId(property.clone()),
                value: value.clone(),
            })
            .collect(),
        UpsertWriteMode::CreateOnly => {
            return Err(ActionRefusal::WriteModeFailed {
                mode: "create_only".to_owned(),
                reason: "the target already exists".to_owned(),
            });
        }
        UpsertWriteMode::DraftOnly {
            lifecycle,
            state: draft,
        } => {
            if !state.exists() {
                return Err(ActionRefusal::TargetNotFound);
            }
            let current = state
                .values
                .as_ref()
                .and_then(|values| values.get(lifecycle.as_str()))
                .and_then(JsonValue::as_str);
            if current != Some(draft.as_str()) {
                return Err(ActionRefusal::WriteModeFailed {
                    mode: "draft_only".to_owned(),
                    reason: format!(
                        "the target's {} state is {:?}, expected {:?}",
                        lifecycle.as_str(),
                        current,
                        draft
                    ),
                });
            }
            business_input
                .iter()
                .map(|(property, value)| FactChange::Set {
                    object_type: object_type.clone(),
                    key: object_id.clone(),
                    property: norn_ir::PropertyId(property.clone()),
                    value: value.clone(),
                })
                .collect()
        }
    };
    commit_effect(
        host,
        catalog,
        call,
        authorizer,
        changes,
        Pending {
            effective_input: record
                .effective_input
                .clone()
                .unwrap_or_else(|| preparation.effective_input.clone()),
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
            receipt: None,
        },
        EnvelopePolicy { limits, meter },
    )
}
