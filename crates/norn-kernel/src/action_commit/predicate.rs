//! Bounded, fenced predicate-keyed slice reads (`SliceKey::Predicate`, #1586).
//!
//! Resolves a predicate slice entry to a [`crate::PredicateQuery`], enforces
//! type-level and per-record read authorization before exposing values or
//! revealing match-count / byte bounds, bounds record count and canonical bytes
//! strictly (refusing on overflow, never truncating), records exact fact
//! versions in the slice, and pins the predicate snapshot into
//! `CellCommit.predicate_reads` (`TXN-PHANTOM`).

use norn_ir::{ActionAggregateInput, ObjectTypeName, RuntimeValue, SliceDeclaration, SliceKey};

use crate::cell_host::{CellHost, FactState, PredicateActionRead, PredicateObject};
use crate::{PredicateQuery, SliceLimits, SliceMeter};

use super::host_failure;
use super::slice::{
    ActionSliceContext, ActionSliceState, admit_action_fact_record, authorize_slice_type,
};
use super::types::{ActionReadAuthzRequest, ActionRefusal};

/// Load one [`SliceKey::Predicate`] entry into the action slice state.
pub(super) fn load_action_slice_predicate<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    state: &mut ActionSliceState,
    entry: &SliceDeclaration,
) -> Result<(), ActionRefusal<H::Error>> {
    let SliceKey::Predicate {
        filter,
        window,
        max_items,
    } = &entry.key
    else {
        return Ok(());
    };
    let ActionSliceContext {
        host,
        catalog,
        call,
        effective_input,
        authorizer,
    } = *context;
    let unavailable = |reason: String| ActionRefusal::SliceUnavailable {
        entry: entry.name.clone(),
        reason,
    };
    if state.read_types.insert(entry.object_type.clone()) {
        authorize_slice_type(call, effective_input, authorizer, &entry.object_type)?;
    }
    let predicate = crate::action_aggregate::build_predicate_query(
        &entry.object_type,
        filter,
        window.as_ref(),
        effective_input,
        catalog,
    )
    .map_err(unavailable)?;

    let max_records = usize::try_from(state.limits.max_records).unwrap_or(usize::MAX);
    let remaining_records =
        max_records.saturating_sub(usize::try_from(state.observed_records).unwrap_or(usize::MAX));
    let entry_cap = max_items.map_or(max_records, |cap| {
        usize::try_from(cap).unwrap_or(usize::MAX)
    });
    let effective_limit = entry_cap.min(remaining_records);

    let objects = if let Some((_, existing)) = state
        .predicate_snapshots
        .iter()
        .find(|(existing, _)| *existing == predicate)
    {
        let objects = existing.clone();
        // Fail closed on any unauthorized member before revealing match-count
        // or byte bounds, matching `load_action_fact`.
        authorize_predicate_matches(context, &entry.object_type, &objects)?;
        if objects.len() > effective_limit {
            return Err(unavailable(format!(
                "predicate match count exceeds the admitted bound of {effective_limit}"
            )));
        }
        objects
    } else {
        let remaining_bytes = state
            .limits
            .max_canonical_bytes
            .saturating_sub(state.meter.bytes());
        // Authorize every matched row BEFORE surfacing host-side disputes,
        // match-count caps, or byte bounds so hidden rows are indistinguishable
        // from `TargetNotFound`.
        let objects = match host
            .read_predicate_for_action(
                &predicate,
                effective_limit,
                remaining_bytes,
                &mut |object| authorize_predicate_object(context, &entry.object_type, object),
            )
            .map_err(host_failure)?
        {
            PredicateActionRead::Admitted(objects) => objects,
            PredicateActionRead::Concealed => return Err(ActionRefusal::TargetNotFound),
        };
        if objects.len() > effective_limit {
            return Err(unavailable(format!(
                "predicate match count exceeds the admitted bound of {effective_limit}"
            )));
        }
        state.predicate_snapshots.push((predicate, objects.clone()));
        objects
    };

    admit_predicate_matches(context, state, entry, &objects)
}

/// Load or reuse the predicate snapshot for one declared [`ActionAggregateInput`],
/// enforcing per-record `authorize_read` BEFORE surfacing source disputes,
/// `AggregateBoundExceeded`, or `AggregateBytesExceeded`, and concealing
/// unsupported/over-bound scans as [`ActionRefusal::TargetNotFound`].
pub(super) fn load_aggregate_predicate_objects<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    aggregate: &ActionAggregateInput,
    predicate: &PredicateQuery,
    max_items: usize,
    meter: &mut SliceMeter,
    limits: &SliceLimits,
    snapshots: &mut Vec<(PredicateQuery, Vec<PredicateObject>)>,
) -> Result<Vec<PredicateObject>, ActionRefusal<H::Error>> {
    let unavailable = |reason: String| ActionRefusal::AggregateUnavailable {
        aggregate: aggregate.name.clone(),
        reason,
    };
    let fresh = !snapshots.iter().any(|(read, _)| *read == *predicate);
    let objects = if fresh {
        let remaining = limits.max_canonical_bytes.saturating_sub(meter.bytes());
        let objects = match context
            .host
            .read_predicate_for_action(predicate, max_items, remaining, &mut |object| {
                authorize_predicate_object(context, &aggregate.object_type, object)
            })
            .map_err(host_failure)?
        {
            PredicateActionRead::Admitted(objects) => objects,
            PredicateActionRead::Concealed => return Err(ActionRefusal::TargetNotFound),
        };
        if objects.len() > max_items {
            return Err(ActionRefusal::AggregateBoundExceeded {
                aggregate: aggregate.name.clone(),
                max_items: aggregate.max_items,
            });
        }
        for object in &objects {
            let bytes = norn_ir::canonical::canonical_json_of(&object.values)
                .map_err(|error| unavailable(error.to_string()))?
                .len();
            meter
                .admit_record(u64::try_from(bytes).unwrap_or(u64::MAX), 0)
                .map_err(|exceeded| match exceeded.kind {
                    crate::IngressLimit::SliceBytes => ActionRefusal::AggregateBytesExceeded {
                        aggregate: aggregate.name.clone(),
                        max_bytes: limits.max_canonical_bytes,
                    },
                    crate::IngressLimit::SliceRecords
                    | crate::IngressLimit::SliceRelationDepth
                    | crate::IngressLimit::EnvelopeBytes => unavailable(exceeded.to_string()),
                })?;
        }
        snapshots.push((predicate.clone(), objects.clone()));
        objects
    } else {
        let Some((_, existing)) = snapshots.iter().find(|(read, _)| *read == *predicate) else {
            return Err(unavailable(
                "the predicate snapshot was not retained".to_owned(),
            ));
        };
        let objects = existing.clone();
        authorize_predicate_matches(context, &aggregate.object_type, &objects)?;
        if objects.len() > max_items {
            return Err(ActionRefusal::AggregateBoundExceeded {
                aggregate: aggregate.name.clone(),
                max_items: aggregate.max_items,
            });
        }
        objects
    };
    Ok(objects)
}

fn authorize_predicate_object<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    declared_type: &ObjectTypeName,
    object: &PredicateObject,
) -> bool {
    context
        .authorizer
        .authorize_read(&ActionReadAuthzRequest {
            action: context.call.action,
            scope: &context.call.scope,
            actor: &context.call.actor,
            input: context.effective_input,
            declared_type,
            target: &object.key,
            target_values: Some(&object.values),
        })
        .is_ok()
}

fn authorize_predicate_matches<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    declared_type: &ObjectTypeName,
    objects: &[PredicateObject],
) -> Result<(), ActionRefusal<H::Error>> {
    for object in objects {
        if !authorize_predicate_object(context, declared_type, object) {
            return Err(ActionRefusal::TargetNotFound);
        }
    }
    Ok(())
}

fn admit_predicate_matches<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    state: &mut ActionSliceState,
    entry: &SliceDeclaration,
    objects: &[PredicateObject],
) -> Result<(), ActionRefusal<H::Error>> {
    let ActionSliceContext { catalog, .. } = *context;
    let model = crate::CommitModel::new(catalog);
    let mut items = Vec::with_capacity(objects.len());
    let mut item_keys = Vec::with_capacity(objects.len());
    for object in objects {
        if !catalog
            .lineage(&object.key.object_type)
            .contains(&entry.object_type)
        {
            return Err(ActionRefusal::SliceUnavailable {
                entry: entry.name.clone(),
                reason: "a predicate match has a type outside the admitted entry type".to_owned(),
            });
        }

        admit_action_fact_record::<H::Error>(
            Some(&object.values),
            &mut state.meter,
            &mut state.observed_records,
            0,
            &state.limits,
            &entry.name,
        )?;

        let canonical = model.canonical(&object.key);
        let concrete_key = if let Some((existing_key, existing_state)) = state
            .facts
            .iter()
            .find(|(key, _)| model.canonical(key) == canonical)
        {
            if existing_state.version != object.version
                || existing_state.values.as_ref() != Some(&object.values)
            {
                return Err(ActionRefusal::StalePreconditionRead {
                    key: object.key.clone(),
                    link: None,
                });
            }
            existing_key.clone()
        } else {
            state.facts.insert(
                object.key.clone(),
                FactState {
                    version: object.version,
                    values: Some(object.values.clone()),
                },
            );
            object.key.clone()
        };
        state.precondition_keys.insert(concrete_key.clone());
        items.push(RuntimeValue::Object(object.values.clone()));
        item_keys.push(concrete_key);
    }

    state
        .values
        .insert(entry.name.clone(), RuntimeValue::Array(items));
    state.entry_keys.insert(entry.name.clone(), None);
    state.entry_list_keys.insert(entry.name.clone(), item_keys);
    state.entry_depths.insert(entry.name.clone(), 0);
    Ok(())
}

/// Convert deduplicated predicate snapshots into commit-time read fences.
pub(super) fn predicate_read_expectations(
    snapshots: Vec<(crate::PredicateQuery, Vec<PredicateObject>)>,
) -> Vec<crate::PredicateReadExpectation> {
    snapshots
        .into_iter()
        .map(|(predicate, objects)| crate::PredicateReadExpectation {
            predicate,
            members: objects.iter().map(PredicateObject::member).collect(),
        })
        .collect()
}
