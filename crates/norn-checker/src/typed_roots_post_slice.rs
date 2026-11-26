//! Admission type-checking for post-slice action derivations (`ACTION-SLICE-DERIVE-1`).

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{EnvOptions, Expression, Type, ZenTypeEnv, check_expression_with_root_bounds};
use norn_ir::{
    ActionInputDerivation, ActionRoot, Diagnostic, JsonValue, ResourceLimitProfile, SliceKey,
};

use crate::envelope::invalid_diagnostic;
use crate::typed_roots::{
    ACTION_CODE, action_derivation_target_type, action_expression_root_bounds,
    action_input_field_type, action_post_derivation_fields_prefix, bind_flat_input_aliases, codes,
    refined_derivation_type,
};

fn post_slice_key_cycle_findings(
    root: &ActionRoot,
    split: usize,
    artifact_id: Option<&str>,
) -> Vec<Diagnostic> {
    let mut pre_slice_inputs = BTreeSet::new();
    if let Some(target) = root.effect_program.as_ref().map(|p| p.target()) {
        pre_slice_inputs.insert(target.object_id_field.to_owned());
    }
    for entry in &root.slice {
        match &entry.key {
            SliceKey::Property { from, property } if from == "input" => {
                pre_slice_inputs.insert(property.as_str().to_owned());
            }
            SliceKey::Predicate { filter, window, .. } => {
                pre_slice_inputs.extend(filter.values().cloned());
                if let Some(window) = window {
                    pre_slice_inputs.insert(window.start.clone());
                    pre_slice_inputs.insert(window.end.clone());
                }
            }
            SliceKey::Parameter { effective_at, .. } => {
                pre_slice_inputs.insert(effective_at.as_str().to_owned());
            }
            SliceKey::Property { .. }
            | SliceKey::Link { .. }
            | SliceKey::TriggerObject
            | SliceKey::TriggerBefore
            | SliceKey::TriggerAfter
            | SliceKey::TriggerChanges => {}
        }
    }
    for aggregate in &root.aggregates {
        pre_slice_inputs.extend(aggregate.filter.values().cloned());
        if let Some(window) = &aggregate.window {
            pre_slice_inputs.insert(window.start.clone());
            pre_slice_inputs.insert(window.end.clone());
        }
    }
    let mut diagnostics = Vec::new();
    for (offset, step) in root
        .derive
        .get(split..)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let index = split.saturating_add(offset);
        if pre_slice_inputs.contains(&step.field) {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!(
                    "post-slice derivation cannot write input field {:?}, which is used to resolve slice keys, aggregates, or target identity",
                    step.field
                ),
                format!("payload.derive[{index}].field"),
                artifact_id,
            ));
        }
    }
    diagnostics
}

pub(crate) fn local_derivation_limit(
    payload: &JsonObject,
    derive: &[ActionInputDerivation],
) -> usize {
    let has_slice = payload
        .get("slice")
        .and_then(JsonValue::as_array)
        .is_some_and(|entries| !entries.is_empty());
    let has_aggregates = payload
        .get("aggregates")
        .and_then(JsonValue::as_array)
        .is_some_and(|entries| !entries.is_empty());
    norn_expr::schema::first_slice_derivation_index_for(derive, has_slice, has_aggregates)
        .unwrap_or(derive.len())
}

/// ACTION-SLICE-DERIVE-1: type-check post-slice action derivations (steps from
/// the first slice/aggregate reader onward) against `input + actor + clock +
/// slice + aggregates`, and refuse any cycle back into slice keys or target id.
pub(crate) fn check_action_slice_derivations(
    root: &ActionRoot,
    records: &BTreeMap<String, BTreeMap<String, Type>>,
    slice_fields: &BTreeMap<String, Type>,
    aggregate_fields: &BTreeMap<String, Type>,
    artifact_id: Option<&str>,
) -> Vec<Diagnostic> {
    let Some(split) = norn_expr::schema::first_slice_derivation_index(root) else {
        return Vec::new();
    };
    let schema = JsonValue::Object(root.input_schema.clone());
    let mut diagnostics = post_slice_key_cycle_findings(root, split, artifact_id);
    let Some((mut fields, mut total_cost)) =
        action_post_derivation_fields_prefix(&schema, &root.derive, split)
    else {
        return diagnostics;
    };
    let declares_aggregates = !root.aggregates.is_empty();
    for (offset, derivation) in root
        .derive
        .get(split..)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let index = split.saturating_add(offset);
        if action_input_field_type(Some(&schema), &derivation.field).is_none()
            || derivation.expression.as_str().trim().is_empty()
        {
            continue;
        }
        let Some(expected) = action_derivation_target_type(Some(&schema), &derivation.field) else {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!(
                    "post-slice derivation target field {:?} has an unsupported opaque JSON schema type",
                    derivation.field
                ),
                format!("payload.derive[{index}].field"),
                artifact_id,
            ));
            continue;
        };
        let Some(environment) = action_slice_derivation_env(
            records,
            slice_fields,
            aggregate_fields,
            declares_aggregates,
            fields.clone(),
        ) else {
            diagnostics.push(invalid_diagnostic(
                codes::ACTION_COMMAND_ENVIRONMENT_UNDERIVABLE,
                "post-slice derivations require every declared slice object and aggregate to have a resolved, closed type",
                "payload.slice",
                artifact_id,
            ));
            return diagnostics;
        };
        if let Some((refined, cost)) = check_single_post_slice_step(
            derivation,
            index,
            &environment,
            &expected,
            artifact_id,
            &mut diagnostics,
        ) {
            total_cost = total_cost.saturating_add(cost);
            fields.insert(derivation.field.clone(), refined);
        }
    }
    let turn_fuel = ResourceLimitProfile::norn_limits_v1().turn.fuel_units;
    if total_cost > turn_fuel {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            format!(
                "the aggregate static cost of action derivations is {total_cost}, above the turn fuel cap of {turn_fuel}"
            ),
            "payload.derive",
            artifact_id,
        ));
    }
    diagnostics
}

fn check_single_post_slice_step(
    derivation: &ActionInputDerivation,
    index: usize,
    environment: &ZenTypeEnv,
    expected: &Type,
    artifact_id: Option<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(Type, u64)> {
    let path = format!("payload.derive[{index}].expression");
    let expression = match Expression::compile(derivation.expression.as_str()) {
        Ok(expression) => expression,
        Err(error) => {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!(
                    "derivation expression {:?} does not compile: {error}",
                    derivation.expression.as_str()
                ),
                path,
                artifact_id,
            ));
            return None;
        }
    };
    match check_expression_with_root_bounds(
        &expression,
        environment,
        expected,
        &ResourceLimitProfile::norn_limits_v1(),
        &action_expression_root_bounds(environment),
    ) {
        Ok(checked) => Some((
            refined_derivation_type(expected.clone(), checked.inferred()),
            checked.cost().units(),
        )),
        Err(findings) => {
            for finding in findings {
                diagnostics.push(invalid_diagnostic(
                    ACTION_CODE,
                    format!(
                        "derivation expression {:?} is invalid: {} ({})",
                        derivation.expression.as_str(),
                        finding.message,
                        finding.code
                    ),
                    path.clone(),
                    artifact_id,
                ));
            }
            None
        }
    }
}

pub(crate) fn action_slice_derivation_env(
    records: &BTreeMap<String, BTreeMap<String, Type>>,
    slice_fields: &BTreeMap<String, Type>,
    aggregate_fields: &BTreeMap<String, Type>,
    declares_aggregates: bool,
    fields: BTreeMap<String, Type>,
) -> Option<ZenTypeEnv> {
    let mut env_records = records.clone();
    let slice_record = "norn:ActionSlice".to_owned();
    env_records.insert(slice_record.clone(), slice_fields.clone());
    let aggregate_record = "norn:ActionAggregates".to_owned();
    if declares_aggregates {
        env_records.insert(aggregate_record.clone(), aggregate_fields.clone());
    }
    let mut environment = ZenTypeEnv::action_derivation_with_records(
        env_records,
        fields,
        EnvOptions::from_limit_profile(&ResourceLimitProfile::norn_limits_v1()),
    )
    .ok()?;
    environment = environment
        .bind_operand("slice", Type::Record(slice_record))
        .ok()?;
    if declares_aggregates {
        environment = environment
            .bind_operand("aggregates", Type::Record(aggregate_record))
            .ok()?;
    }
    bind_flat_input_aliases(environment)
}

pub(crate) fn post_slice_fields_and_cost(
    input_schema: &JsonValue,
    root: &ActionRoot,
    records: &BTreeMap<String, BTreeMap<String, Type>>,
    slice_fields: &BTreeMap<String, Type>,
    aggregate_fields: &BTreeMap<String, Type>,
) -> Option<(BTreeMap<String, Type>, u64)> {
    let split = norn_expr::schema::first_slice_derivation_index(root).unwrap_or(root.derive.len());
    let (mut fields, mut total_cost) =
        action_post_derivation_fields_prefix(input_schema, &root.derive, split)?;
    let declares_aggregates = !root.aggregates.is_empty();
    for step in root.derive.get(split..).unwrap_or_default() {
        let environment = action_slice_derivation_env(
            records,
            slice_fields,
            aggregate_fields,
            declares_aggregates,
            fields.clone(),
        )?;
        let expected = action_input_field_type(Some(input_schema), &step.field)?;
        let expression = Expression::compile(step.expression.as_str()).ok()?;
        let checked = check_expression_with_root_bounds(
            &expression,
            &environment,
            &expected,
            &ResourceLimitProfile::norn_limits_v1(),
            &action_expression_root_bounds(&environment),
        )
        .ok()?;
        total_cost = total_cost.saturating_add(checked.cost().units());
        fields.insert(
            step.field.clone(),
            refined_derivation_type(expected, checked.inferred()),
        );
    }
    Some((fields, total_cost))
}

pub(crate) fn action_input_fields_with_post_slice(
    payload: &BTreeMap<String, JsonValue>,
    schema: &JsonValue,
    derivations: &[ActionInputDerivation],
) -> Option<BTreeMap<String, Type>> {
    let has_slice = payload
        .get("slice")
        .and_then(JsonValue::as_array)
        .is_some_and(|entries| !entries.is_empty());
    let has_aggregates = payload
        .get("aggregates")
        .and_then(JsonValue::as_array)
        .is_some_and(|entries| !entries.is_empty());
    let split =
        norn_expr::schema::first_slice_derivation_index_for(derivations, has_slice, has_aggregates)
            .unwrap_or(derivations.len());
    let (mut fields, _) = action_post_derivation_fields_prefix(schema, derivations, split)?;
    for step in derivations.get(split..).unwrap_or_default() {
        let expected = action_input_field_type(Some(schema), &step.field)?;
        fields.insert(step.field.clone(), expected);
    }
    Some(fields)
}
