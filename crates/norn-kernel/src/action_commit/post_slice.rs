//! `ACTION-CLOCK-1` and `ACTION-SLICE-DERIVE-1` runtime helpers for `action_commit`.

use std::collections::BTreeSet;

use norn_ir::{ActionRoot, PropertyOrigin, ResourceLimitProfile, RuntimeValue};

use crate::cell_host::JsonObject;
use crate::decision_commit::TypeCatalog;

use super::input::{
    DerivationEvalEnv, canonical_object_bytes, check_canonical_object_bound,
    prepare_action_derivation, validate_action_input,
};
use super::slice::LoadedActionSlice;
use super::types::{ActionCall, ActionDerivedValue, ActionInputPreparation, ActionRefusal};

fn expr_reads_clock_fail_closed(expression: &str) -> bool {
    match norn_expr::expression_reads(expression, norn_expr::CLOCK_VARIABLE) {
        Ok(reads) => reads.whole_root || !reads.fields.is_empty(),
        Err(_) => true,
    }
}

/// ACTION-CLOCK-1: the input fields an action derives from the server commit
/// clock without the caller: a derivation with `inputRequired: false` that
/// reads `clock`. A caller never supplies them (a supplied value refuses with
/// `action.invalid_input`), and a published caller input schema omits them.
/// A derivation that does not parse counts as one (fail closed).
#[must_use]
pub fn clock_derived_fields(action: &ActionRoot) -> BTreeSet<String> {
    action
        .derive
        .iter()
        .filter(|step| !step.input_required)
        .filter(|step| expr_reads_clock_fail_closed(step.expression.as_str()))
        .map(|step| step.field.clone())
        .collect()
}

/// ACTION-CLOCK-1: returns `true` when ANY derivation step on `action` (including
/// `inputRequired: true` or unparseable expressions, failing closed) reads `clock`.
#[must_use]
pub fn action_reads_clock(action: &ActionRoot) -> bool {
    action
        .derive
        .iter()
        .any(|step| expr_reads_clock_fail_closed(step.expression.as_str()))
}

pub(super) fn seed_derived_slots_and_receipt_bytes<X>(
    action: &ActionRoot,
    raw_input: &JsonObject,
    effective_input: &mut JsonObject,
    raw_bytes: usize,
    max_input_bytes: usize,
    max_receipt_bytes: usize,
) -> Result<usize, ActionRefusal<X>> {
    for derivation in &action.derive {
        if derivation.input_required && !raw_input.contains_key(&derivation.field) {
            return Err(ActionRefusal::InvalidInput {
                field: derivation.field.clone(),
            });
        }
        if !derivation.input_required && !effective_input.contains_key(&derivation.field) {
            effective_input.insert(derivation.field.clone(), RuntimeValue::Null);
        }
    }
    check_canonical_object_bound::<X>("effective input", effective_input, max_input_bytes)?;
    let effective_bytes = if action.derive.is_empty() && *effective_input == *raw_input {
        0 // Raw input already describes the exact execution value.
    } else {
        canonical_object_bytes::<X>(effective_input)?
    };
    let receipt_evidence_bytes = raw_bytes
        .saturating_add(effective_bytes)
        .saturating_add(128);
    if receipt_evidence_bytes > max_receipt_bytes {
        return Err(ActionRefusal::InputBoundExceeded {
            subject: "action call receipt evidence".to_owned(),
            actual_bytes: receipt_evidence_bytes,
            max_bytes: max_receipt_bytes,
        });
    }
    Ok(receipt_evidence_bytes)
}

pub(super) fn validate_action_input_excluding<X>(
    action: &ActionRoot,
    input: &JsonObject,
    deferred_fields: &BTreeSet<String>,
) -> Result<(), ActionRefusal<X>> {
    if deferred_fields.is_empty() {
        return validate_action_input(action, input);
    }
    let mut schema_obj = action.input_schema.clone();
    if let Some(norn_ir::JsonValue::Array(required)) = schema_obj.get_mut("required") {
        required.retain(|value| {
            value
                .as_string()
                .is_none_or(|name| !deferred_fields.contains(name))
        });
    }
    let mut filtered = input.clone();
    for field in deferred_fields {
        filtered.remove(field);
    }
    let schema = norn_ir::JsonValue::Object(schema_obj);
    let value =
        norn_expr::Value::try_from(&RuntimeValue::Object(filtered)).map_err(|_conversion| {
            ActionRefusal::InvalidInput {
                field: "$input".to_owned(),
            }
        })?;
    let parsed =
        norn_expr::schema::Schema::parse_action(&schema, "$input").map_err(|violation| {
            ActionRefusal::InvalidInput {
                field: violation.path,
            }
        })?;
    parsed
        .check(&value, "$input")
        .map_err(|violation| ActionRefusal::InvalidInput {
            field: violation.path,
        })
}

pub(super) fn validate_pre_slice_partial<X>(
    action: &ActionRoot,
    pre_slice_limit: usize,
    effective_input: &mut JsonObject,
) -> Result<(), ActionRefusal<X>> {
    let pre_slice_produced: BTreeSet<&str> = action
        .derive
        .iter()
        .take(pre_slice_limit)
        .map(|step| step.field.as_str())
        .collect();
    let deferred: BTreeSet<String> = action
        .derive
        .get(pre_slice_limit..)
        .unwrap_or_default()
        .iter()
        .filter(|step| {
            !step.input_required
                && !pre_slice_produced.contains(step.field.as_str())
                && effective_input
                    .get(&step.field)
                    .is_none_or(RuntimeValue::is_null)
        })
        .map(|step| step.field.clone())
        .collect();
    for field in &deferred {
        if effective_input
            .get(field)
            .is_some_and(RuntimeValue::is_null)
        {
            effective_input.remove(field);
        }
    }
    validate_action_input_excluding(action, effective_input, &deferred)
}

/// ACTION-SLICE-DERIVE-1: runs deferred post-slice derivations right after
/// `load_action_slice` and validates the final effective input before
/// preconditions and writes.
pub(super) fn apply_post_slice_derivations<X>(
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    preparation: &mut ActionInputPreparation<'_>,
    slice: &LoadedActionSlice,
    max_receipt_bytes: usize,
) -> Result<(), ActionRefusal<X>> {
    let action = call.action;
    let Some(split) = norn_expr::schema::first_slice_derivation_index(action) else {
        return Ok(());
    };
    let profile = ResourceLimitProfile::norn_limits_v1();
    let max_input_bytes = usize::try_from(profile.slice.max_canonical_bytes).unwrap_or(usize::MAX);
    let Some(first_step) = action.derive.get(split) else {
        return Ok(());
    };
    for step in action.derive.get(split..).unwrap_or_default() {
        if !step.input_required && !preparation.effective_input.contains_key(&step.field) {
            preparation
                .effective_input
                .insert(step.field.clone(), RuntimeValue::Null);
        }
    }
    let catalog_env = action
        .slice
        .first()
        .map(|entry| &entry.object_type)
        .or_else(|| {
            action
                .aggregates
                .first()
                .map(|aggregate| &aggregate.object_type)
        })
        .map(|object_type| catalog.expression_environment(object_type))
        .transpose()
        .map_err(|reason| ActionRefusal::DerivationFailed {
            field: first_step.field.clone(),
            expression: first_step.expression.as_str().to_owned(),
            reason,
        })?;
    for (offset, derivation) in action
        .derive
        .get(split..)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let index = split.saturating_add(offset);
        let (value, next_receipt_bytes) = prepare_action_derivation(
            &DerivationEvalEnv {
                action,
                effective_input: &preparation.effective_input,
                actor: Some(call.actor.as_str()),
                now: Some(call.turn.now),
                slice: Some(slice),
                catalog_env: catalog_env.as_ref(),
            },
            index,
            derivation,
            &mut preparation.remaining_fuel,
            preparation.receipt_evidence_bytes,
        )?;
        preparation.receipt_evidence_bytes = next_receipt_bytes;
        if preparation.receipt_evidence_bytes > max_receipt_bytes {
            return Err(ActionRefusal::InputBoundExceeded {
                subject: "action call receipt evidence".to_owned(),
                actual_bytes: preparation.receipt_evidence_bytes,
                max_bytes: max_receipt_bytes,
            });
        }
        preparation
            .effective_input
            .insert(derivation.field.clone(), value.clone());
        preparation.derived_values.push(ActionDerivedValue {
            field: derivation.field.clone(),
            value,
        });
        check_canonical_object_bound::<X>(
            "effective input",
            &preparation.effective_input,
            max_input_bytes,
        )?;
    }
    validate_action_input(action, &preparation.effective_input)
}

pub(super) fn is_owned_row_property_restricted(
    prop: &norn_checker::entity_instance::EffectiveProperty,
) -> bool {
    prop.computed
        || prop.protected()
        || prop.immutable_after_create()
        || prop.origin().is_some_and(|origin| {
            !matches!(origin, PropertyOrigin::Caller | PropertyOrigin::Derived)
        })
}

pub(super) fn is_stored_patch_origin_restricted(
    prop: &norn_checker::entity_instance::EffectiveProperty,
) -> bool {
    prop.computed
        || prop.protected()
        || prop
            .origin()
            .is_some_and(|origin| origin != PropertyOrigin::Caller)
}
