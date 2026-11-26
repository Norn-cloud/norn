use std::collections::BTreeSet;

use norn_checker::entity_instance::InstanceError;
use norn_ir::{ActionRoot, FactChange, ObjectTypeName, ResourceLimitProfile, UpsertWriteMode};

use crate::cell_host::{CellHost, FactKey, JsonObject, JsonValue};
use crate::creator_stamp::CreatorStamp;
use crate::decision_commit::{DecisionRefusal, FactEvent, TypeCatalog};
use crate::limits::SliceMeter;
use crate::naming_series::type_series;

use super::input::check_action_record_size;
use super::series::commit_upsert_series;
use super::slice::{ActionSliceContext, check_action_preconditions, load_action_slice};
use super::transition::{WriterTarget, writer_output};
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

/// ACTOR-STAMP-1: the creator stamp of a governed create: each protected
/// property of `target` whose value is the action's server stamp of the
/// verified principal ([`ActionRoot::actor_stamped_fields`]). Any other
/// protected value stays refused when the create is prepared (FA-PROTECT-1).
pub(super) fn creator_stamp(
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    target: &FactKey,
    values: &JsonObject,
) -> Option<CreatorStamp> {
    let effective = catalog.effective_type(&target.object_type)?;
    let stamped = call.action.actor_stamped_fields();
    let properties: BTreeSet<String> = values
        .iter()
        .filter(|(property, value)| {
            stamped.contains(property.as_str())
                && value.as_str() == Some(call.actor.as_str())
                && effective
                    .property(property.as_str())
                    .is_some_and(norn_checker::EffectiveProperty::protected)
        })
        .map(|(property, _)| property.clone())
        .collect();
    (!properties.is_empty()).then(|| CreatorStamp {
        key: target.clone(),
        properties,
        principal: call.actor.clone(),
    })
}

/// CREATE-WRITER-1: fold the writer's `set`s of ordinary properties of the
/// created target into its create, so the commit records one `created` event
/// with the object's final values; the store proves every event of a commit
/// against that object's one committed snapshot. Each folded value is checked
/// against the property's effective contract first, and an invalid one
/// refuses before preparation, so no later `set` can mask it and nothing
/// downstream (an approval gate, a dry run) ever sees it. A `set` of a
/// protected, lifecycle or undeclared property stays a separate change,
/// which preparation refuses as it refuses any such write.
///
/// # Errors
///
/// [`DecisionRefusal::InvalidOutput`] for a writer value its property refuses.
fn fold_into_create<X>(
    catalog: &TypeCatalog<'_>,
    target: &FactKey,
    changes: &mut Vec<FactChange>,
    facts: Vec<FactChange>,
) -> Result<(), ActionRefusal<X>> {
    let effective = catalog.effective_type(&target.object_type);
    let lifecycles = catalog.lifecycle_properties(&target.object_type);
    for fact in facts {
        if let FactChange::Set {
            object_type,
            key,
            property,
            value,
        } = &fact
            && *object_type == target.object_type
            && *key == target.key
            && !lifecycles.contains(property)
            && let Some(effective) = effective
            && effective
                .property(property.as_str())
                .is_some_and(|declared| !declared.protected())
            && let Some(FactChange::Create { values, .. }) = changes.first_mut()
        {
            effective
                .check_stored_property_value(property.as_str(), value)
                .map_err(|error| {
                    let detail = match error {
                        InstanceError::Unsupported(feature) => {
                            format!("uses unsupported {feature}")
                        }
                        InstanceError::TypeMismatch(expected) => format!("expects {expected}"),
                        InstanceError::ConstraintViolation(reason) => reason,
                    };
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!(
                            "the effective values for {target}: property `{}` {detail}",
                            property.as_str()
                        ),
                    })
                })?;
            values.insert(property.as_str().to_owned(), value.clone());
            continue;
        }
        changes.push(fact);
    }
    Ok(())
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
    rule: Option<&crate::synchronous_rule::SynchronousRule>,
    mut preparation: ActionInputPreparation<'_>,
    authorizer: &dyn ActionAuthorizer,
    object_id_field: &str,
    object_kind: &str,
    write_mode: &UpsertWriteMode,
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
    let object_type = call
        .object_type
        .clone()
        .unwrap_or_else(|| ObjectTypeName(object_kind.to_owned()));
    check_kind(catalog, &object_type, object_kind)?;
    let naming_series = type_series(catalog, &object_type);
    if matches!(write_mode, UpsertWriteMode::CreateOnly)
        && let Some(series) = naming_series.as_ref()
    {
        if rule.is_some() {
            // A generated key's allocation retries are not qualified with a writer.
            return Err(ActionRefusal::UnsupportedEffect {
                action_id: action.action_id.clone(),
            });
        }
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
            envelope,
        );
        // No honest target exists for some allocator failures. Conceal all
        // refusals alike unless diagnostic access was separately granted, so
        // denied calls cannot distinguish a healthy counter from a bad one.
        // An approval's binding of the allocated commit is past the allocator
        // and says nothing about the counter (#618).
        return outcome.map_err(|refusal| {
            if disclose || matches!(refusal, ActionRefusal::ApprovedIntentChanged) {
                refusal
            } else {
                ActionRefusal::TargetNotFound
            }
        });
    }
    let object_id =
        super::required_object_id::<H::Error>(&preparation.effective_input, object_id_field)?;

    let target = FactKey {
        object_type: object_type.clone(),
        key: object_id.to_owned(),
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
        rule_binding: rule.map(|rule| rule.binding(call)).transpose()?,
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
        principal_inputs: None,
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
    let mut record = record;
    record
        .precondition_outcomes
        .clone_from(&preparation.precondition_outcomes);
    record.parameter_pins.clone_from(&slice.parameter_pins);

    let mut business_input =
        upsert_stored_input(catalog, action, &object_type, &preparation.effective_input);
    if let Some(quantity) = &action.quantity_effect {
        business_input.remove(&quantity.balance_versions_field);
    }
    // ACTOR-STAMP-1: only a create-only create stamps protected creator properties.
    let stamp = if matches!(write_mode, UpsertWriteMode::CreateOnly) && !state.exists() {
        creator_stamp(catalog, call, &target, &business_input)
    } else {
        None
    };
    let mut changes = match write_mode {
        UpsertWriteMode::Upsert | UpsertWriteMode::CreateOnly if !state.exists() => {
            vec![FactChange::Create {
                object_type: object_type.clone(),
                key: object_id.to_owned(),
                values: business_input.clone(),
            }]
        }
        UpsertWriteMode::UpdateOnly if !state.exists() => {
            return Err(ActionRefusal::TargetNotFound);
        }
        UpsertWriteMode::Upsert | UpsertWriteMode::UpdateOnly => {
            set_changes(&object_type, object_id, &business_input)
        }
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
            set_changes(&object_type, object_id, &business_input)
        }
    };
    // PRINCIPAL-INPUT-1: resolved once, after replay and write-mode checks,
    // and recorded, exactly as a transition's are. The subject is the object
    // the call writes, which a create only now brings into being.
    if let Some(rule) = rule {
        record.principal_inputs =
            crate::principal_inputs::resolve::<H::Error>(rule, call, authorizer, &target)?;
    }
    check_action_record_size::<H::Error>(call.action, &record, limits)?;
    // CREATE-WRITER-1 (#1101): the writer computes the created object's
    // remaining fields and its declared side effects from the input and the
    // captured slice; they commit with the create as one receipt.
    let decision_links = match rule {
        Some(rule) => {
            let (facts, links) = writer_output(
                host,
                catalog,
                call,
                rule,
                &mut preparation,
                authorizer,
                &mut slice,
                &target,
                WriterTarget::Created,
                record.principal_inputs.as_ref(),
                limits,
            )?;
            fold_into_create::<H::Error>(catalog, &target, &mut changes, facts)?;
            links
        }
        None => Vec::new(),
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
            remaining_fuel: &mut preparation.remaining_fuel,
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
            receipt: None,
            creator_stamp: stamp,
        },
        envelope,
    )
}

fn set_changes(
    object_type: &ObjectTypeName,
    object_id: &str,
    business_input: &JsonObject,
) -> Vec<FactChange> {
    business_input
        .iter()
        .map(|(property, value)| FactChange::Set {
            object_type: object_type.clone(),
            key: object_id.to_owned(),
            property: norn_ir::PropertyId(property.clone()),
            value: value.clone(),
        })
        .collect()
}
