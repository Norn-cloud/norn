use std::collections::BTreeMap;

use norn_expr::{Expression, TurnFuel, Value as ExprValue};
use norn_ir::{
    ActionCommand, ActionCommandOperation, ActionEffectProgram, ActionRoot, Decimal,
    ResourceLimitProfile, RuntimeValue,
};

use crate::cell_host::JsonObject;
use crate::decision_commit::{DecisionRefusal, TypeCatalog, expr_value};
use crate::limits::EnvelopeLimits;
use crate::observations::LogicalTime;

use super::slice::LoadedActionSlice;
use super::types::{
    ActionCall, ActionCallRecord, ActionDerivedValue, ActionDerivedValueRef,
    ActionInputPreparation, ActionRefusal,
};

/// Evaluates the declared outbound commands over the action input. The
/// expressions remain inert IR until this admitted action is actually
/// committed; the returned records are then converted by the shared decision
/// path into `perform` effects in the same commit as the transition.
struct ActionExpressionContext {
    value: ExprValue,
    environment: norn_expr::ZenTypeEnv,
}

pub(super) fn build_action_commands<X>(
    action: &ActionRoot,
    input: &JsonObject,
    action_call: &str,
    remaining_fuel: &mut TurnFuel,
) -> Result<Vec<JsonObject>, ActionRefusal<X>> {
    if action.commands.is_empty() {
        return Ok(Vec::new());
    }
    let invalid = |reason: String| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("action command: {reason}"),
        })
    };
    let input = input
        .iter()
        .map(|(name, value)| expr_value(value).map(|value| (name.clone(), value)))
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(invalid)?;
    let context = ActionExpressionContext {
        value: ExprValue::object([("input", ExprValue::Object(input))]),
        environment: norn_expr::schema::action_environment(action, None, action.derive.len())
            .map_err(&invalid)?,
    };
    action
        .commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let mut record = JsonObject::new();
            record.insert(
                "type".to_owned(),
                RuntimeValue::String("norn.connector.command/v1".to_owned()),
            );
            record.insert(
                "connector".to_owned(),
                RuntimeValue::String(command.connector.clone()),
            );
            let operation = action_command_operation_name(command.operation);
            record.insert(
                "operation".to_owned(),
                RuntimeValue::String(operation.to_owned()),
            );
            record.insert(
                "doctype".to_owned(),
                RuntimeValue::String(command.doctype.clone()),
            );
            if let Some(name) = &command.name {
                let value = evaluate_action_expression(name.as_str(), &context, remaining_fuel)
                    .map_err(&invalid)?;
                if !matches!(value, RuntimeValue::String(_)) {
                    return Err(invalid(format!(
                        "command {index} name must evaluate to a string"
                    )));
                }
                record.insert("name".to_owned(), value);
            } else if matches!(
                &command.operation,
                ActionCommandOperation::Submit
                    | ActionCommandOperation::Update
                    | ActionCommandOperation::Cancel
            ) {
                return Err(invalid(format!(
                    "command {index} requires a name expression"
                )));
            }
            if let Some(value) = evaluate_action_expected_modified(
                index,
                command
                    .expected_modified
                    .as_ref()
                    .map(norn_ir::ZenExpression::as_str),
                &context,
                remaining_fuel,
            )? {
                record.insert("expectedModified".to_owned(), value);
            }
            let mut fields = JsonObject::new();
            for (field, expression) in &command.fields {
                fields.insert(
                    field.clone(),
                    evaluate_action_expression(expression.as_str(), &context, remaining_fuel)
                        .map_err(&invalid)?,
                );
            }
            record.insert("fields".to_owned(), RuntimeValue::Object(fields));
            if let Some(child_merges) =
                build_action_child_merges(index, command, &context, remaining_fuel)?
            {
                record.insert("childMerges".to_owned(), child_merges);
            }
            record.insert(
                "actionId".to_owned(),
                RuntimeValue::String(action.action_id.clone()),
            );
            record.insert(
                "actionCall".to_owned(),
                RuntimeValue::String(action_call.to_owned()),
            );
            // The normal decision commit derives the command id and external
            // idempotency key from the action turn and this stable index.
            Ok(record)
        })
        .collect()
}

fn action_command_operation_name(operation: ActionCommandOperation) -> &'static str {
    match operation {
        ActionCommandOperation::Submit => "submit",
        ActionCommandOperation::Create => "create",
        ActionCommandOperation::Update => "update",
        ActionCommandOperation::Cancel => "cancel",
    }
}

/// Evaluates a command's guarded source version, when present.
fn evaluate_action_expected_modified<X>(
    command_index: usize,
    source: Option<&str>,
    context: &ActionExpressionContext,
    remaining_fuel: &mut TurnFuel,
) -> Result<Option<RuntimeValue>, ActionRefusal<X>> {
    let Some(source) = source else {
        return Ok(None);
    };
    let invalid = |reason: String| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("action command: {reason}"),
        })
    };
    let value = evaluate_action_expression(source, context, remaining_fuel).map_err(&invalid)?;
    if !matches!(&value, RuntimeValue::String(version) if !version.trim().is_empty()) {
        return Err(invalid(format!(
            "command {command_index} expectedModified must evaluate to non-empty text"
        )));
    }
    Ok(Some(value))
}

/// Evaluates and encodes the child-table merge declarations for one command.
fn build_action_child_merges<X>(
    command_index: usize,
    command: &ActionCommand,
    context: &ActionExpressionContext,
    remaining_fuel: &mut TurnFuel,
) -> Result<Option<RuntimeValue>, ActionRefusal<X>> {
    if command.child_merges.is_empty() {
        return Ok(None);
    }
    let invalid = |reason: String| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("action command: {reason}"),
        })
    };
    let mut child_merges = Vec::with_capacity(command.child_merges.len());
    for merge in &command.child_merges {
        let ownership_value =
            evaluate_action_expression(merge.ownership_value.as_str(), context, remaining_fuel)
                .map_err(&invalid)?;
        if !matches!(
            &ownership_value,
            RuntimeValue::String(owner) if !owner.trim().is_empty()
        ) {
            return Err(invalid(format!(
                "command {command_index} child merge {} ownershipValue must be non-empty text",
                merge.field
            )));
        }
        let rows = evaluate_action_expression(merge.rows.as_str(), context, remaining_fuel)
            .map_err(&invalid)?;
        let RuntimeValue::Array(rows) = rows else {
            return Err(invalid(format!(
                "command {command_index} child merge {} rows must evaluate to an array",
                merge.field
            )));
        };
        if rows
            .iter()
            .any(|row| !matches!(row, RuntimeValue::Object(_)))
        {
            return Err(invalid(format!(
                "command {command_index} child merge {} rows must contain only objects",
                merge.field
            )));
        }
        child_merges.push(RuntimeValue::Object(JsonObject::from([
            (
                "field".to_owned(),
                RuntimeValue::String(merge.field.clone()),
            ),
            (
                "ownershipField".to_owned(),
                RuntimeValue::String(merge.ownership_field.clone()),
            ),
            ("ownershipValue".to_owned(), ownership_value),
            (
                "rowKeyField".to_owned(),
                RuntimeValue::String(merge.row_key_field.clone()),
            ),
            ("rows".to_owned(), RuntimeValue::Array(rows)),
        ])));
    }
    Ok(Some(RuntimeValue::Array(child_merges)))
}

/// Evaluates one action command expression with the input under `input`.
fn evaluate_action_expression(
    source: &str,
    context: &ActionExpressionContext,
    remaining_fuel: &mut TurnFuel,
) -> Result<RuntimeValue, String> {
    let expression = Expression::compile(source).map_err(|error| error.to_string())?;
    let attempt = remaining_fuel.spend(|budget| {
        expression.evaluate_admitted_metered(
            &context.value,
            budget,
            &context.environment,
            &norn_expr::Type::Json,
        )
    });
    let evaluated = attempt.result.map_err(|error| error.to_string())?;
    expression_value_to_runtime(evaluated.value())
}

/// Checks the action input against the closed schema admitted on its action
/// artifact. The public action route is one caller, but this check stays in
/// the kernel so direct `commit_action` callers cannot smuggle undeclared
/// properties into an upsert.
pub(super) fn validate_action_input<X>(
    action: &ActionRoot,
    input: &JsonObject,
) -> Result<(), ActionRefusal<X>> {
    let schema = norn_ir::JsonValue::Object(action.input_schema.clone());
    let value = norn_expr::Value::try_from(&RuntimeValue::Object(input.clone())).map_err(
        |_conversion| ActionRefusal::InvalidInput {
            field: "$input".to_owned(),
        },
    )?;
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

/// Runs declarative input derivations in order, then validates the exact
/// effective input that authorization, preconditions, and writes will use.
/// This validation-only token is not bound to an action artifact version and
/// is refused by [`commit_action_with_preparation`](super::commit_action_with_preparation). The caller's object
/// remains untouched for idempotency and delegation binding.
///
/// # Errors
///
/// Refuses missing required input, invalid derived values or final input,
/// expression failures, and input or receipt size limits.
pub fn prepare_action_input<'a, X>(
    action: &'a ActionRoot,
    raw_input: &'a JsonObject,
) -> Result<ActionInputPreparation<'a>, ActionRefusal<X>> {
    prepare_action_input_inner(action, raw_input, None, None, None, None)
}

/// Prepares one action call's input, binding the opaque result to the action
/// artifact version and exact raw input that the kernel will commit.
/// ACTION-ACTOR-1: `actor.subjectId` is this authenticated host principal;
/// caller identity fields refuse and a preparation cannot move to another actor.
///
/// # Errors
///
/// Refuses missing required input, invalid derived values or final input,
/// expression failures, and input or receipt size limits.
pub fn prepare_action_input_for_call<'a, X>(
    action: &'a ActionRoot,
    raw_input: &'a JsonObject,
    action_artifact_id: &'a str,
    action_version: &'a str,
    actor: &str,
) -> Result<ActionInputPreparation<'a>, ActionRefusal<X>> {
    prepare_action_input_inner(
        action,
        raw_input,
        Some(action_artifact_id),
        Some(action_version),
        Some(actor),
        None,
    )
}

/// ACTION-CLOCK-1: prepares one action call's pre-slice input with both the
/// authenticated principal (`actor.subjectId`) and the server commit clock
/// (`clock.nowMillis`, `clock.nowIso`).
///
/// # Errors
///
/// Refuses missing required input, spoofed actor/clock fields, invalid derived
/// values or final input, expression failures, and size limits.
pub(super) fn prepare_action_input_for_call_at<'a, X>(
    action: &'a ActionRoot,
    raw_input: &'a JsonObject,
    action_artifact_id: &'a str,
    action_version: &'a str,
    actor: &str,
    now: LogicalTime,
) -> Result<ActionInputPreparation<'a>, ActionRefusal<X>> {
    prepare_action_input_inner(
        action,
        raw_input,
        Some(action_artifact_id),
        Some(action_version),
        Some(actor),
        Some(now),
    )
}

/// ACTION-SLICE-DERIVE-1: index of the first derivation step that reads
/// `slice` or `aggregates` (when declared on the action) and therefore runs
/// after `load_action_slice`.
pub(super) fn first_slice_derivation_index(action: &ActionRoot) -> Option<usize> {
    if action.slice.is_empty() && action.aggregates.is_empty() {
        return None;
    }
    action.derive.iter().position(|step| {
        (!action.slice.is_empty()
            && norn_expr::expression_reads(step.expression.as_str(), "slice")
                .map_or(true, |reads| reads.whole_root || !reads.fields.is_empty()))
            || (!action.aggregates.is_empty()
                && norn_expr::expression_reads(step.expression.as_str(), "aggregates")
                    .map_or(true, |reads| reads.whole_root || !reads.fields.is_empty()))
    })
}

pub(super) fn clock_derived_fields(action: &ActionRoot) -> Vec<String> {
    let mut fields = Vec::new();
    for step in &action.derive {
        if !step.input_required
            && norn_expr::expression_reads(step.expression.as_str(), norn_expr::CLOCK_VARIABLE)
                .map_or(true, |reads| reads.whole_root || !reads.fields.is_empty())
        {
            fields.push(step.field.clone());
        }
    }
    fields
}

fn normalize_action_input<X>(
    action: &ActionRoot,
    raw_input: &JsonObject,
) -> Result<JsonObject, ActionRefusal<X>> {
    let schema = norn_expr::schema::Schema::parse_action(
        &norn_ir::JsonValue::Object(action.input_schema.clone()),
        "$input",
    )
    .map_err(|violation| ActionRefusal::InvalidInput {
        field: violation.path,
    })?;
    // ACTION-ACTOR-1 / ACTION-CLOCK-1: server-stamped identity and clock fields
    // are never caller-supplied, not even as `null` or a value that derivation
    // would overwrite.
    if let Some(field) = actor_derived_fields(action)
        .into_iter()
        .chain(clock_derived_fields(action))
        .find(|field| raw_input.contains_key(field))
    {
        return Err(ActionRefusal::InvalidInput { field });
    }
    let mut supplied = raw_input.clone();
    for step in action.derive.iter().filter(|step| !step.input_required) {
        if supplied.get(&step.field) == Some(&RuntimeValue::Null) {
            supplied.remove(&step.field);
        }
    }
    let normalized = schema
        .decode_action_input(RuntimeValue::Object(supplied), "$input")
        .map_err(|violation| ActionRefusal::InvalidInput {
            field: violation.path,
        })?;
    let RuntimeValue::Object(normalized) = normalized else {
        return Err(ActionRefusal::InvalidInput {
            field: "$input".to_owned(),
        });
    };
    Ok(normalized)
}

fn prepare_action_input_inner<'a, X>(
    action: &'a ActionRoot,
    raw_input: &'a JsonObject,
    action_artifact_id: Option<&'a str>,
    action_version: Option<&'a str>,
    actor: Option<&str>,
    now: Option<LogicalTime>,
) -> Result<ActionInputPreparation<'a>, ActionRefusal<X>> {
    let profile = ResourceLimitProfile::norn_limits_v1();
    let max_input_bytes = usize::try_from(profile.slice.max_canonical_bytes).unwrap_or(usize::MAX);
    // Rejects injected identity fields and, for prepared actions, refuses
    // oversized raw input by its canonical size before any clone, decode or
    // normalization work.
    check_action_identity_input::<X>(action, raw_input, actor, max_input_bytes)?;
    let has_preparation = has_action_preparation_features(action);
    let normalized = normalize_action_input::<X>(action, raw_input)?;
    if !receipt_input_bound_applies(action, raw_input, &normalized) {
        validate_action_input(action, &normalized)?;
        return Ok(ActionInputPreparation {
            action,
            raw_input,
            action_artifact_id,
            action_version,
            actor: actor.map(str::to_owned),
            effective_input: normalized,
            derived_values: Vec::new(),
            remaining_fuel: TurnFuel::turn(&profile),
            receipt_evidence_bytes: 0,
            precondition_outcomes: Vec::new(),
        });
    }
    let max_receipt_bytes =
        usize::try_from(profile.envelope.max_output_bytes).unwrap_or(usize::MAX);
    if !has_preparation {
        check_canonical_object_bound::<X>("caller input", raw_input, max_input_bytes)?;
    }
    let raw_bytes = check_receipt_input_bound::<X>(raw_input)?;
    let mut effective_input = normalized;
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
    check_canonical_object_bound::<X>("effective input", &effective_input, max_input_bytes)?;
    let effective_bytes = if action.derive.is_empty() && effective_input == *raw_input {
        0 // Raw input already describes the exact execution value.
    } else {
        canonical_object_bytes::<X>(&effective_input)?
    };
    let mut receipt_evidence_bytes = raw_bytes
        .saturating_add(effective_bytes)
        .saturating_add(128);
    if receipt_evidence_bytes > max_receipt_bytes {
        return Err(ActionRefusal::InputBoundExceeded {
            subject: "action call receipt evidence".to_owned(),
            actual_bytes: receipt_evidence_bytes,
            max_bytes: max_receipt_bytes,
        });
    }
    let mut derived_values = Vec::with_capacity(action.derive.len());
    let mut remaining_fuel = TurnFuel::turn(&profile);
    let pre_slice_limit = first_slice_derivation_index(action).unwrap_or(action.derive.len());

    for (index, derivation) in action.derive.iter().take(pre_slice_limit).enumerate() {
        let (value, next_receipt_bytes) = prepare_action_derivation(
            action,
            index,
            derivation,
            &effective_input,
            actor,
            now,
            None,
            None,
            &mut remaining_fuel,
            receipt_evidence_bytes,
        )?;
        receipt_evidence_bytes = next_receipt_bytes;
        if receipt_evidence_bytes > max_receipt_bytes {
            return Err(ActionRefusal::InputBoundExceeded {
                subject: "action call receipt evidence".to_owned(),
                actual_bytes: receipt_evidence_bytes,
                max_bytes: max_receipt_bytes,
            });
        }
        effective_input.insert(derivation.field.clone(), value.clone());
        derived_values.push(ActionDerivedValue {
            field: derivation.field.clone(),
            value,
        });
        check_canonical_object_bound::<X>("effective input", &effective_input, max_input_bytes)?;
    }

    if pre_slice_limit == action.derive.len() {
        validate_action_input(action, &effective_input)?;
    }
    Ok(ActionInputPreparation {
        action,
        raw_input,
        action_artifact_id,
        action_version,
        actor: actor.map(str::to_owned),
        effective_input,
        derived_values,
        remaining_fuel,
        receipt_evidence_bytes,
        precondition_outcomes: Vec::new(),
    })
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
    let Some(split) = first_slice_derivation_index(action) else {
        return Ok(());
    };
    let profile = ResourceLimitProfile::norn_limits_v1();
    let max_input_bytes = usize::try_from(profile.slice.max_canonical_bytes).unwrap_or(usize::MAX);
    let Some(first_step) = action.derive.get(split) else {
        return Ok(());
    };
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
            action,
            index,
            derivation,
            &preparation.effective_input,
            Some(call.actor.as_str()),
            Some(call.turn.now),
            Some(slice),
            catalog_env.as_ref(),
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
    validate_action_input(action, &preparation.effective_input)?;
    Ok(())
}

/// ACTION-ACTOR-1: caller JSON cannot supply identity or approval claims.
pub(super) fn check_action_identity_input<X>(
    action: &ActionRoot,
    raw_input: &JsonObject,
    actor: Option<&str>,
    max_bytes: usize,
) -> Result<(), ActionRefusal<X>> {
    // Validate the principal before any traversal of caller input.
    if actor.is_some_and(|a| {
        a.len() > max_bytes || a.trim().is_empty() || norn_ir::canonical::nfc(a) != a
    }) {
        return Err(ActionRefusal::InvalidInput {
            field: "actor".into(),
        });
    }
    if has_action_preparation_features(action)
        || matches!(
            action.effect_program,
            Some(ActionEffectProgram::MutateParentWithRows { .. })
        )
    {
        check_canonical_object_bound::<X>("caller input", raw_input, max_bytes)?;
    }
    if let Some((field, _)) = reserved_action_identity(raw_input) {
        return Err(ActionRefusal::InvalidInput {
            field: field.into(),
        });
    }
    Ok(())
}

pub(super) fn rfc6901_token(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

pub(super) fn reserved_action_identity(raw_input: &JsonObject) -> Option<(&'static str, String)> {
    if let Some(field) = reserved_identity_member(raw_input) {
        return Some((field, format!("/{field}")));
    }
    let mut pending: Vec<_> = raw_input
        .iter()
        .rev()
        .filter(|(_, value)| matches!(value, RuntimeValue::Object(_) | RuntimeValue::Array(_)))
        .map(|(field, value)| (value, format!("/{}", rfc6901_token(field))))
        .collect();
    while let Some((value, path)) = pending.pop() {
        match value {
            RuntimeValue::Object(object) => {
                if let Some(field) = reserved_identity_member(object) {
                    return Some((field, format!("{path}/{field}")));
                }
                pending.extend(
                    object
                        .iter()
                        .rev()
                        .filter(|(_, value)| {
                            matches!(value, RuntimeValue::Object(_) | RuntimeValue::Array(_))
                        })
                        .map(|(field, value)| (value, format!("{path}/{}", rfc6901_token(field)))),
                );
            }
            RuntimeValue::Array(items) => pending.extend(
                items
                    .iter()
                    .enumerate()
                    .rev()
                    .filter(|(_, value)| {
                        matches!(value, RuntimeValue::Object(_) | RuntimeValue::Array(_))
                    })
                    .map(|(index, value)| (value, format!("{path}/{index}"))),
            ),
            RuntimeValue::Null
            | RuntimeValue::Bool(_)
            | RuntimeValue::Number(_)
            | RuntimeValue::String(_) => {}
        }
    }
    None
}

fn reserved_identity_member(input: &JsonObject) -> Option<&'static str> {
    ["actor", "approved", "clock"]
        .into_iter()
        .find(|field| input.contains_key(*field))
}

fn action_derivation_destination(
    action: &ActionRoot,
    derivation: &norn_ir::ActionInputDerivation,
    env: &norn_expr::ZenTypeEnv,
) -> norn_expr::Type {
    let explicit_nullable = action
        .input_schema
        .get("properties")
        .and_then(norn_ir::JsonValue::as_object)
        .and_then(|fields| fields.get(&derivation.field))
        .and_then(|schema| norn_expr::schema::Schema::parse(schema, "target").ok())
        .is_some_and(|schema| matches!(schema, norn_expr::schema::Schema::Nullable(_)));
    let declared = env
        .record(norn_expr::env::INPUT_RECORD)
        .and_then(|record| record.fields.get(&derivation.field))
        .map_or(norn_expr::Type::Json, |field| field.ty.clone());
    if !explicit_nullable && let norn_expr::Type::Optional(inner) = declared {
        return *inner;
    }
    declared
}

/// ACTION-ACTOR-1 / ACTION-CLOCK-1: bind actor and commit clock data only from
/// the authenticated call context, plus loaded slice/aggregates when post-slice.
fn action_derivation_context<X>(
    action: &ActionRoot,
    effective_input: &JsonObject,
    actor: Option<&str>,
    now: Option<LogicalTime>,
    slice: Option<&LoadedActionSlice>,
    derivation: &norn_ir::ActionInputDerivation,
) -> Result<ExprValue, ActionRefusal<X>> {
    let empty_slice = JsonObject::new();
    let slice_values = slice.map_or(&empty_slice, |loaded| &loaded.values);
    let aggregates = slice
        .filter(|_| !action.aggregates.is_empty())
        .map(|loaded| &loaded.aggregates);
    let mut context = action_expression_context(effective_input, slice_values, aggregates)
        .map_err(|reason| ActionRefusal::DerivationFailed {
            field: derivation.field.clone(),
            expression: derivation.expression.as_str().to_owned(),
            reason,
        })?;
    if let ExprValue::Object(fields) = &mut context {
        if let Some(principal) = actor {
            fields.insert(
                "actor".into(),
                ExprValue::Object(BTreeMap::from([(
                    "subjectId".into(),
                    ExprValue::String(principal.into()),
                )])),
            );
        }
        if let Some(instant) = now {
            let millis = instant.as_millis();
            let now_iso = norn_expr::temporal::format_utc_millis(millis).map_err(|error| {
                ActionRefusal::DerivationFailed {
                    field: derivation.field.clone(),
                    expression: derivation.expression.as_str().to_owned(),
                    reason: format!("commit clock out of range: {error}"),
                }
            })?;
            let signed_millis =
                i64::try_from(millis).map_err(|_error| ActionRefusal::DerivationFailed {
                    field: derivation.field.clone(),
                    expression: derivation.expression.as_str().to_owned(),
                    reason: "commit clock millis out of i64 range".to_owned(),
                })?;
            let now_millis = expr_value(&RuntimeValue::from(signed_millis)).map_err(|reason| {
                ActionRefusal::DerivationFailed {
                    field: derivation.field.clone(),
                    expression: derivation.expression.as_str().to_owned(),
                    reason: format!("commit clock millis invalid: {reason}"),
                }
            })?;
            fields.insert(
                "clock".into(),
                ExprValue::Object(BTreeMap::from([
                    ("nowMillis".into(), now_millis),
                    ("nowIso".into(), ExprValue::String(now_iso)),
                ])),
            );
        }
    }
    Ok(context)
}

#[allow(
    clippy::too_many_arguments,
    reason = "pre-slice and post-slice derivations share one metered evaluator"
)]
fn prepare_action_derivation<X>(
    action: &ActionRoot,
    completed: usize,
    derivation: &norn_ir::ActionInputDerivation,
    effective_input: &JsonObject,
    actor: Option<&str>,
    now: Option<LogicalTime>,
    slice: Option<&LoadedActionSlice>,
    catalog_env: Option<&norn_expr::ZenTypeEnv>,
    remaining_fuel: &mut TurnFuel,
    receipt_evidence_bytes: usize,
) -> Result<(RuntimeValue, usize), ActionRefusal<X>> {
    let context =
        action_derivation_context(action, effective_input, actor, now, slice, derivation)?;
    let expression = Expression::compile(derivation.expression.as_str()).map_err(|error| {
        ActionRefusal::DerivationFailed {
            field: derivation.field.clone(),
            expression: derivation.expression.as_str().to_owned(),
            reason: error.to_string(),
        }
    })?;
    let env = if slice.is_some() {
        norn_expr::schema::action_slice_derivation_environment(action, catalog_env, completed)
    } else {
        norn_expr::schema::action_derivation_environment(action, completed)
    }
    .map_err(|reason| ActionRefusal::DerivationFailed {
        field: derivation.field.clone(),
        expression: derivation.expression.as_str().to_owned(),
        reason,
    })?;
    let expected = action_derivation_destination(action, derivation, &env);
    let attempt = remaining_fuel
        .spend(|budget| expression.evaluate_admitted_metered(&context, budget, &env, &expected));
    let evaluated = attempt
        .result
        .map_err(|error| ActionRefusal::DerivationFailed {
            field: derivation.field.clone(),
            expression: derivation.expression.as_str().to_owned(),
            reason: error.to_string(),
        })?;
    let value = expression_value_to_runtime(evaluated.value()).map_err(|reason| {
        ActionRefusal::DerivationFailed {
            field: derivation.field.clone(),
            expression: derivation.expression.as_str().to_owned(),
            reason,
        }
    })?;
    let encoded = norn_ir::canonical::canonical_json_of(&ActionDerivedValueRef {
        field: &derivation.field,
        value: &value,
    })
    .map_err(|error| ActionRefusal::DerivationFailed {
        field: derivation.field.clone(),
        expression: derivation.expression.as_str().to_owned(),
        reason: error.to_string(),
    })?;
    let value_bytes = norn_ir::canonical::canonical_json_of(&value)
        .map_err(|error| ActionRefusal::DerivationFailed {
            field: derivation.field.clone(),
            expression: derivation.expression.as_str().to_owned(),
            reason: error.to_string(),
        })?
        .len();
    let effective_growth = if let Some(previous) = effective_input.get(&derivation.field) {
        let previous_bytes = norn_ir::canonical::canonical_json_of(previous)
            .map_err(|error| ActionRefusal::DerivationFailed {
                field: derivation.field.clone(),
                expression: derivation.expression.as_str().to_owned(),
                reason: error.to_string(),
            })?
            .len();
        (
            value_bytes >= previous_bytes,
            value_bytes.abs_diff(previous_bytes),
        )
    } else {
        let field_bytes = norn_ir::canonical::canonical_json_of(&derivation.field)
            .map_err(|error| ActionRefusal::DerivationFailed {
                field: derivation.field.clone(),
                expression: derivation.expression.as_str().to_owned(),
                reason: error.to_string(),
            })?
            .len();
        (
            true,
            field_bytes
                .saturating_add(1)
                .saturating_add(value_bytes)
                .saturating_add(usize::from(!effective_input.is_empty())),
        )
    };
    let receipt_evidence_bytes = if effective_growth.0 {
        receipt_evidence_bytes.saturating_add(effective_growth.1)
    } else {
        receipt_evidence_bytes.saturating_sub(effective_growth.1)
    }
    .saturating_add(encoded.len())
    .saturating_add(1);
    Ok((value, receipt_evidence_bytes))
}

fn canonical_object_bytes<X>(object: &JsonObject) -> Result<usize, ActionRefusal<X>> {
    norn_ir::canonical::canonical_json_of(object)
        .map(|bytes| bytes.len())
        .map_err(|error| ActionRefusal::DerivationFailed {
            field: "$input".to_owned(),
            expression: String::new(),
            reason: error.to_string(),
        })
}

pub(super) fn check_canonical_object_bound<X>(
    subject: &str,
    object: &JsonObject,
    max_bytes: usize,
) -> Result<(), ActionRefusal<X>> {
    let actual_bytes = canonical_object_bytes::<X>(object)?;
    if actual_bytes > max_bytes {
        return Err(ActionRefusal::InputBoundExceeded {
            subject: subject.to_owned(),
            actual_bytes,
            max_bytes,
        });
    }
    Ok(())
}

pub(super) fn action_expression_context(
    input: &JsonObject,
    slice: &JsonObject,
    aggregates: Option<&JsonObject>,
) -> Result<ExprValue, String> {
    let mut context = expression_object(input)?
        .into_iter()
        .filter(|(name, _)| name != "input" && name != "slice")
        .collect::<BTreeMap<_, _>>();
    let input = expression_object(input)?;
    let slice = expression_object(slice)?;
    context.insert("input".to_owned(), ExprValue::Object(input));
    context.insert("slice".to_owned(), ExprValue::Object(slice));
    // A declared `aggregates` operand shadows an input field of that name,
    // which stays reachable as `input.aggregates` (as the checker types it).
    if let Some(aggregates) = aggregates {
        context.insert(
            "aggregates".to_owned(),
            ExprValue::Object(expression_object(aggregates)?),
        );
    }
    Ok(ExprValue::Object(context))
}

fn expression_object(values: &JsonObject) -> Result<BTreeMap<String, ExprValue>, String> {
    values
        .iter()
        .map(|(name, value)| expr_value(value).map(|value| (name.clone(), value)))
        .collect()
}

/// Whether a call's raw input is held to the receipt input bound
/// ([`check_receipt_input_bound`]): a call to an action without preparation
/// features whose input normalizes to itself keeps its admitted fast path.
fn receipt_input_bound_applies(
    action: &ActionRoot,
    raw_input: &JsonObject,
    normalized: &JsonObject,
) -> bool {
    has_action_preparation_features(action) || normalized != raw_input
}

/// The action call receipt input bound: the raw input's canonical bytes must
/// fit the envelope's `max_output_bytes`. Returns those bytes.
fn check_receipt_input_bound<X>(raw_input: &JsonObject) -> Result<usize, ActionRefusal<X>> {
    let max_receipt_bytes = usize::try_from(
        ResourceLimitProfile::norn_limits_v1()
            .envelope
            .max_output_bytes,
    )
    .unwrap_or(usize::MAX);
    let raw_bytes = canonical_object_bytes::<X>(raw_input)?;
    if raw_bytes > max_receipt_bytes {
        return Err(ActionRefusal::InputBoundExceeded {
            subject: "action call receipt input".to_owned(),
            actual_bytes: raw_bytes,
            max_bytes: max_receipt_bytes,
        });
    }
    Ok(raw_bytes)
}

/// AUTOMATION-1 (#1704): apply the receipt input bound a call to `action`
/// with `raw_input` would meet at runtime, under the same condition
/// ([`receipt_input_bound_applies`]). Admission passes the static input with
/// the runtime-injected target id, so an input refused here is refused for
/// every matched record.
pub(super) fn check_call_receipt_input<X>(
    action: &ActionRoot,
    raw_input: &JsonObject,
) -> Result<(), ActionRefusal<X>> {
    if !has_action_preparation_features(action) {
        let normalized = normalize_action_input::<X>(action, raw_input)?;
        if !receipt_input_bound_applies(action, raw_input, &normalized) {
            return Ok(());
        }
    }
    check_receipt_input_bound::<X>(raw_input).map(drop)
}

fn has_action_preparation_features(action: &ActionRoot) -> bool {
    action.synchronous_rule.is_some()
        || !action.derive.is_empty()
        || !action.slice.is_empty()
        || !action.preconditions.is_empty()
}

pub(super) fn check_action_record_size<X>(
    action: &ActionRoot,
    record: &ActionCallRecord,
    limits: &EnvelopeLimits,
) -> Result<(), ActionRefusal<X>> {
    // Existing actions without the new preparation phases retain their
    // admitted input/receipt behavior, including command-only actions.
    if !has_action_preparation_features(action)
        && !record.effective_input.as_ref().is_some_and(|effective| {
            record
                .input
                .iter()
                .any(|(key, value)| effective.get(key).is_some_and(|current| current != value))
        })
        && !matches!(
            action.effect_program,
            Some(ActionEffectProgram::MutateParentWithRows { .. })
        )
    {
        return Ok(());
    }
    let values = RuntimeValue::Object(record.to_values());
    let actual_bytes = norn_ir::canonical::canonical_json_of(&values)
        .map_err(|_error| ActionRefusal::InputBoundExceeded {
            subject: "action call receipt".to_owned(),
            actual_bytes: usize::MAX,
            max_bytes: usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
        })?
        .len();
    let max_bytes = usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX);
    if actual_bytes > max_bytes {
        return Err(ActionRefusal::InputBoundExceeded {
            subject: "action call receipt".to_owned(),
            actual_bytes,
            max_bytes,
        });
    }
    Ok(())
}

/// Converts an expression value without routing exact decimals through a
/// binary JSON number.
pub(super) fn expression_value_to_runtime(value: &ExprValue) -> Result<RuntimeValue, String> {
    Ok(match value {
        ExprValue::Null => RuntimeValue::Null,
        ExprValue::Bool(flag) => RuntimeValue::Bool(*flag),
        ExprValue::Number(number) => RuntimeValue::Number(
            Decimal::parse(number.as_str()).map_err(|error| error.to_string())?,
        ),
        ExprValue::String(text) => RuntimeValue::String(text.clone()),
        ExprValue::Array(values) => RuntimeValue::Array(
            values
                .iter()
                .map(expression_value_to_runtime)
                .collect::<Result<_, _>>()?,
        ),
        ExprValue::Object(values) => RuntimeValue::Object(
            values
                .iter()
                .map(|(name, value)| {
                    expression_value_to_runtime(value).map(|value| (name.clone(), value))
                })
                .collect::<Result<_, _>>()?,
        ),
    })
}

/// ACTION-ACTOR-1: the input fields an action derives from the verified actor
/// without the caller: a derivation with `inputRequired: false` that reads
/// `actor`. A caller never supplies them (a supplied value refuses with
/// `action.invalid_input`), and a published caller input schema omits them.
/// A derivation that does not parse counts as one (fail closed).
#[must_use]
pub fn actor_derived_fields(action: &ActionRoot) -> std::collections::BTreeSet<String> {
    action
        .derive
        .iter()
        .filter(|step| !step.input_required)
        .filter(|step| {
            norn_expr::expression_read_paths(step.expression.as_str(), norn_expr::ACTOR_VARIABLE)
                .map_or(true, |paths| !paths.is_empty())
        })
        .map(|step| step.field.clone())
        .collect()
}

pub(super) fn action_link_key(value: &RuntimeValue) -> Option<&str> {
    value.as_str().filter(|key| !key.trim().is_empty())
}
