use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{EvaluationBudget, Expression, Value as ExprValue};
use norn_ir::{
    ActionRoot, Decimal, DecisionOutput, FactChange, ObjectTypeName, OwnedChildCollection,
    OwnedCollectionAction, PropertyId, ResourceLimitProfile,
};

use crate::cell_host::{
    CellCommit, CellHost, CommitError, FactExpectation, FactKey, FactMutation, FactState,
    FactVersion, FactWrite, JsonObject, JsonValue, LinkKey, LinkMutation, LinkWrite,
    OwnedRowsCommitProof,
};
use crate::decision_commit::{
    DecisionRefusal, FactEvent, TypeCatalog, expr_value, fill_lifecycle_defaults,
    prepare_owned_rows, widen_effect,
};
use crate::envelope::TurnEnvelopeMeter;
use crate::limits::{EnvelopeLimits, SliceMeter};
use crate::naming_series::type_series;

use super::input::{build_action_commands, check_action_record_size, expression_value_to_runtime};
use super::slice::{
    ActionSliceContext, check_action_preconditions, load_action_slice,
    reconcile_action_target_snapshot,
};
use super::types::{
    ACTION_CALL_TYPE, ACTION_EFFECT_MUTATE_PARENT_WITH_ROWS, ActionAuthorizer, ActionAuthzRequest,
    ActionCall, ActionCallRecord, ActionDenial, ActionInputPreparation, ActionOutcome,
    ActionRefusal, owned_rows_manifest_key,
};
use super::{
    authorize_current_target, authorize_identity_collision, check_action_envelope, check_kind,
    host_failure, ledger_action_invariant_refusal, precondition_read_refusal, replay,
};

#[derive(Debug, Clone)]
enum ParsedOwnedRowOp {
    Add {
        collection: String,
        row_id: String,
        expected: FactVersion,
        values: JsonObject,
    },
    Update {
        collection: String,
        row_id: String,
        expected: FactVersion,
        values: JsonObject,
    },
    Remove {
        collection: String,
        row_id: String,
        expected: FactVersion,
    },
    Reorder {
        collection: String,
        rows: Vec<(String, FactVersion)>,
    },
}

impl ParsedOwnedRowOp {
    fn collection(&self) -> &str {
        match self {
            Self::Add { collection, .. }
            | Self::Update { collection, .. }
            | Self::Remove { collection, .. }
            | Self::Reorder { collection, .. } => collection,
        }
    }
}

fn input_version<X>(input: &JsonObject, field: &str) -> Result<FactVersion, ActionRefusal<X>> {
    let Some(JsonValue::String(text)) = input.get(field) else {
        return Err(ActionRefusal::InvalidInput {
            field: field.to_owned(),
        });
    };
    parse_version(text).ok_or_else(|| ActionRefusal::InvalidInput {
        field: field.to_owned(),
    })
}

fn parse_version(text: &str) -> Option<FactVersion> {
    let version = text.parse::<u64>().ok()?;
    (version.to_string() == text).then_some(FactVersion(version))
}

fn event_limit_as_usize(limits: &EnvelopeLimits) -> usize {
    usize::try_from(limits.max_events).unwrap_or(usize::MAX)
}

fn invalid_row_ops<X>() -> ActionRefusal<X> {
    ActionRefusal::InvalidInput {
        field: "rowOps".to_owned(),
    }
}

fn parse_owned_row_op<X>(
    operation: &JsonValue,
    declared: &BTreeMap<String, OwnedChildCollection>,
) -> Result<ParsedOwnedRowOp, ActionRefusal<X>> {
    let Some(object) = operation.as_object() else {
        return Err(invalid_row_ops());
    };
    let (Some(JsonValue::String(collection)), Some(JsonValue::String(kind))) =
        (object.get("collection"), object.get("op"))
    else {
        return Err(invalid_row_ops());
    };
    let Some(collection_definition) = declared.get(collection) else {
        return Err(invalid_row_ops());
    };
    match kind.as_str() {
        "add" | "update" => parse_owned_row_value_op(collection, kind, object),
        "remove" => parse_owned_row_remove(collection, object),
        "reorder" => {
            let slice_limit = usize::try_from(
                crate::limits::ResourceLimitProfile::norn_limits_v1()
                    .slice
                    .max_records,
            )
            .unwrap_or(usize::MAX);
            let collection_limit =
                usize::try_from(collection_definition.max_rows.0).unwrap_or(usize::MAX);
            parse_owned_row_reorder(collection, object, collection_limit.min(slice_limit))
        }
        _ => Err(invalid_row_ops()),
    }
}

fn parse_owned_row_value_op<X>(
    collection: &str,
    kind: &str,
    object: &JsonObject,
) -> Result<ParsedOwnedRowOp, ActionRefusal<X>> {
    let (
        Some(JsonValue::String(row_id)),
        Some(JsonValue::String(version)),
        Some(JsonValue::Object(values)),
    ) = (
        object.get("rowId"),
        object.get("expectedVersion"),
        object.get("values"),
    )
    else {
        return Err(invalid_row_ops());
    };
    if object.len() != 5 {
        return Err(invalid_row_ops());
    }
    let expected = parse_version(version).ok_or_else(invalid_row_ops)?;
    let collection = collection.to_owned();
    let row_id = row_id.clone();
    let values = values.clone();
    Ok(if kind == "add" {
        ParsedOwnedRowOp::Add {
            collection,
            row_id,
            expected,
            values,
        }
    } else {
        ParsedOwnedRowOp::Update {
            collection,
            row_id,
            expected,
            values,
        }
    })
}

fn parse_owned_row_remove<X>(
    collection: &str,
    object: &JsonObject,
) -> Result<ParsedOwnedRowOp, ActionRefusal<X>> {
    let (Some(JsonValue::String(row_id)), Some(JsonValue::String(version))) =
        (object.get("rowId"), object.get("expectedVersion"))
    else {
        return Err(invalid_row_ops());
    };
    if object.len() != 4 {
        return Err(invalid_row_ops());
    }
    Ok(ParsedOwnedRowOp::Remove {
        collection: collection.to_owned(),
        row_id: row_id.clone(),
        expected: parse_version(version).ok_or_else(invalid_row_ops)?,
    })
}

fn parse_owned_row_reorder<X>(
    collection: &str,
    object: &JsonObject,
    max_rows: usize,
) -> Result<ParsedOwnedRowOp, ActionRefusal<X>> {
    let Some(JsonValue::Array(rows)) = object.get("rowIds") else {
        return Err(invalid_row_ops());
    };
    if object.len() != 3 || rows.len() > max_rows {
        return Err(invalid_row_ops());
    }
    let mut parsed_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(row) = row.as_object() else {
            return Err(invalid_row_ops());
        };
        let (Some(JsonValue::String(row_id)), Some(JsonValue::String(version))) =
            (row.get("rowId"), row.get("expectedVersion"))
        else {
            return Err(invalid_row_ops());
        };
        if row.len() != 2 {
            return Err(invalid_row_ops());
        }
        parsed_rows.push((
            row_id.clone(),
            parse_version(version).ok_or_else(invalid_row_ops)?,
        ));
    }
    Ok(ParsedOwnedRowOp::Reorder {
        collection: collection.to_owned(),
        rows: parsed_rows,
    })
}

fn parse_owned_row_ops<X>(
    raw: &[JsonValue],
    limits: &EnvelopeLimits,
    declared: &BTreeMap<String, OwnedChildCollection>,
) -> Result<Vec<ParsedOwnedRowOp>, ActionRefusal<X>> {
    if raw.len() > event_limit_as_usize(limits) {
        return Err(ActionRefusal::Decision(DecisionRefusal::EnvelopeExceeded {
            kind: crate::decider::EnvelopeKind::Events,
            limit: limits.max_events,
            actual: raw.len(),
        }));
    }
    let mut parsed = Vec::with_capacity(raw.len());
    for operation in raw {
        parsed.push(parse_owned_row_op(operation, declared)?);
    }
    Ok(parsed)
}

fn read_manifest_ids<X>(
    manifest: &FactState,
    parent: &FactKey,
    collection_id: &str,
    collection: &OwnedChildCollection,
) -> Result<Vec<String>, ActionRefusal<X>> {
    let Some(values) = manifest.values.as_ref() else {
        return Ok(Vec::new());
    };
    let valid_identity = values.get("parentType")
        == Some(&JsonValue::String(parent.object_type.as_str().to_owned()))
        && values.get("parentKey") == Some(&JsonValue::String(parent.key.clone()))
        && values.get("collectionId") == Some(&JsonValue::String(collection_id.to_owned()));
    let Some(JsonValue::Array(rows)) = values.get("rowIds") else {
        return Err(manifest_corrupt::<X>(parent, collection_id));
    };
    if !valid_identity || rows.len() > usize::try_from(collection.max_rows.0).unwrap_or(100) {
        return Err(manifest_corrupt::<X>(parent, collection_id));
    }
    let mut ids = Vec::with_capacity(rows.len());
    let mut seen = BTreeSet::new();
    for row in rows {
        let Some(row_id) = row.as_str() else {
            return Err(manifest_corrupt::<X>(parent, collection_id));
        };
        if !seen.insert(row_id.to_owned()) {
            return Err(manifest_corrupt::<X>(parent, collection_id));
        }
        ids.push(row_id.to_owned());
    }
    Ok(ids)
}

fn manifest_corrupt<X>(parent: &FactKey, collection_id: &str) -> ActionRefusal<X> {
    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
        reason: format!(
            "owned-row manifest for {parent} collection {collection_id:?} is malformed"
        ),
    })
}

fn integer_equals(value: Option<&JsonValue>, expected: usize) -> bool {
    value
        .and_then(JsonValue::as_i64)
        .and_then(|integer| usize::try_from(integer).ok())
        == Some(expected)
}

fn decimal_value(value: usize) -> Result<JsonValue, String> {
    let text = value.to_string();
    Decimal::parse(&text)
        .map(JsonValue::Number)
        .map_err(|error| error.to_string())
}

fn owned_manifest_key<X>(
    parent: &FactKey,
    collection_id: &str,
) -> Result<FactKey, ActionRefusal<X>> {
    owned_rows_manifest_key(parent, collection_id).map_err(|error| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("owned-row manifest key could not be derived: {error}"),
        })
    })
}

fn manifest_values(parent: &FactKey, collection_id: &str, row_ids: &[String]) -> JsonObject {
    JsonObject::from([
        (
            "parentType".to_owned(),
            JsonValue::String(parent.object_type.as_str().to_owned()),
        ),
        (
            "parentKey".to_owned(),
            JsonValue::String(parent.key.clone()),
        ),
        (
            "collectionId".to_owned(),
            JsonValue::String(collection_id.to_owned()),
        ),
        (
            "rowIds".to_owned(),
            JsonValue::Array(row_ids.iter().cloned().map(JsonValue::String).collect()),
        ),
    ])
}

fn validate_row_values<X>(
    catalog: &TypeCatalog<'_>,
    values: &JsonObject,
    collection: &OwnedChildCollection,
    config: &OwnedCollectionAction,
    field: &str,
) -> Result<(), ActionRefusal<X>> {
    if values.contains_key(collection.row_id_property.as_str())
        || values.contains_key(collection.ordering_key.as_str())
        || catalog
            .lifecycle_properties(&collection.child_type)
            .iter()
            .any(|property| values.contains_key(property.as_str()))
        || config
            .derive
            .keys()
            .any(|property| values.contains_key(property.as_str()))
    {
        return Err(ActionRefusal::InvalidInput {
            field: field.to_owned(),
        });
    }
    validate_stored_patch::<X>(catalog, &collection.child_type, values, field)?;
    Ok(())
}

fn validate_stored_patch<X>(
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeName,
    values: &JsonObject,
    field: &str,
) -> Result<(), ActionRefusal<X>> {
    let Some(effective) = catalog.effective_type(object_type) else {
        return Err(ActionRefusal::InvalidInput {
            field: field.to_owned(),
        });
    };
    for property in values.keys() {
        let Some(declared) = effective
            .properties
            .iter()
            .find(|candidate| candidate.id.as_str() == property)
        else {
            return Err(ActionRefusal::InvalidInput {
                field: field.to_owned(),
            });
        };
        if declared.computed
            || declared.protected()
            || type_series(catalog, object_type)
                .is_some_and(|series| series.key_properties.contains(&declared.id))
        {
            return Err(ActionRefusal::InvalidInput {
                field: field.to_owned(),
            });
        }
    }
    Ok(())
}

fn row_expression_context<X>(
    input: &JsonObject,
    proposed: &JsonObject,
) -> Result<ExprValue, ActionRefusal<X>> {
    let input = expr_value(&JsonValue::Object(input.clone())).map_err(|reason| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("row action input could not be evaluated: {reason}"),
        })
    })?;
    let proposed = expr_value(&JsonValue::Object(proposed.clone())).map_err(|reason| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("proposed row could not be evaluated: {reason}"),
        })
    })?;
    Ok(ExprValue::object([("input", input), ("self", proposed)]))
}

fn derive_row_values<X>(
    action: &ActionRoot,
    catalog: &TypeCatalog<'_>,
    collection: &OwnedChildCollection,
    config: &OwnedCollectionAction,
    input: &JsonObject,
    mut proposed: JsonObject,
    remaining_fuel: &mut u64,
) -> Result<JsonObject, ActionRefusal<X>> {
    if catalog.effective_type(&collection.child_type).is_none() {
        return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!(
                "owned child type {:?} has no effective schema",
                collection.child_type.as_str()
            ),
        }));
    }
    let base_env = catalog
        .base_expression_environment()
        .map_err(|reason| ActionRefusal::Decision(DecisionRefusal::InvalidOutput { reason }))?;
    let excluded = config
        .derive
        .keys()
        .map(|property| property.as_str().to_owned())
        .collect();
    let base_env = base_env
        .bind_self_to_stored_fields(&collection.child_type, &excluded)
        .map_err(|error| {
            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: error.to_string(),
            })
        })?;
    let env =
        norn_expr::schema::action_environment(action, Some(&base_env), action.derive.len())
            .map_err(|reason| ActionRefusal::Decision(DecisionRefusal::InvalidOutput { reason }))?;
    for (property, expression) in &config.derive {
        let context = row_expression_context::<X>(input, &proposed)?;
        let invalid = |reason| {
            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: format!(
                    "row derivation for {:?} failed: {reason}",
                    property.as_str()
                ),
            })
        };
        let expected = catalog
            .expression_environment(&collection.child_type)
            .ok()
            .and_then(|env| {
                env.record(collection.child_type.as_str())
                    .and_then(|record| record.fields.get(property.as_str()))
                    .map(|field| field.ty.clone())
            })
            .ok_or_else(|| invalid("unknown derived property type".into()))?;
        let evaluated = evaluate_row_expression(
            expression.as_str(),
            &context,
            remaining_fuel,
            &env,
            &expected,
            invalid,
        )?;
        let value = expression_value_to_runtime(&evaluated).map_err(invalid)?;
        proposed.insert(property.as_str().to_owned(), value);
    }
    Ok(proposed)
}

fn check_row_preconditions<X>(
    action: &ActionRoot,
    catalog: &TypeCatalog<'_>,
    child_type: &ObjectTypeName,
    config: &OwnedCollectionAction,
    input: &JsonObject,
    proposed: &JsonObject,
    remaining_fuel: &mut u64,
) -> Result<(), ActionRefusal<X>> {
    let context = row_expression_context::<X>(input, proposed)?;
    let base_env = catalog
        .base_expression_environment()
        .and_then(|env| {
            env.bind_self_to_stored_fields(child_type, &BTreeSet::new())
                .map_err(|e| e.to_string())
        })
        .map_err(|reason| ActionRefusal::Decision(DecisionRefusal::InvalidOutput { reason }))?;
    let env =
        norn_expr::schema::action_environment(action, Some(&base_env), action.derive.len())
            .map_err(|reason| ActionRefusal::Decision(DecisionRefusal::InvalidOutput { reason }))?;
    for precondition in &config.preconditions {
        let invalid = |reason| ActionRefusal::PreconditionFailed {
            precondition: precondition.as_str().to_owned(),
            reason: Some(reason),
        };
        match evaluate_row_expression(
            precondition.as_str(),
            &context,
            remaining_fuel,
            &env,
            &norn_expr::Type::Bool,
            invalid,
        )? {
            ExprValue::Bool(true) => {}
            ExprValue::Bool(false) => {
                return Err(ActionRefusal::PreconditionFailed {
                    precondition: precondition.as_str().to_owned(),
                    reason: None,
                });
            }
            ExprValue::Null
            | ExprValue::Number(_)
            | ExprValue::String(_)
            | ExprValue::Array(_)
            | ExprValue::Object(_) => {
                return Err(invalid(
                    "the expression did not evaluate to a boolean".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn evaluate_row_expression<X>(
    source: &str,
    context: &ExprValue,
    remaining_fuel: &mut u64,
    env: &norn_expr::ZenTypeEnv,
    expected: &norn_expr::Type,
    invalid: impl Fn(String) -> ActionRefusal<X>,
) -> Result<ExprValue, ActionRefusal<X>> {
    let expression = Expression::compile(source).map_err(|error| invalid(error.to_string()))?;
    let attempt = expression.evaluate_admitted_metered(
        context,
        EvaluationBudget::new(*remaining_fuel),
        env,
        expected,
    );
    *remaining_fuel = remaining_fuel.saturating_sub(attempt.units_used);
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
    )]
    let evaluated = attempt.result.map_err(|error| match error {
        norn_expr::ExprError::BudgetExhausted { .. } => ActionRefusal::ExpressionBudgetExceeded {
            limit: ResourceLimitProfile::norn_limits_v1().turn.fuel_units,
        },
        _ => invalid(error.to_string()),
    })?;
    Ok(evaluated.into_value())
}

fn stale_action<X>(key: FactKey, expected: FactVersion, current: FactVersion) -> ActionRefusal<X> {
    ActionRefusal::Decision(DecisionRefusal::Stale {
        key,
        expected,
        current,
    })
}

struct OwnedProposalAuthorization<'a, 'catalog, 'call> {
    catalog: &'a TypeCatalog<'catalog>,
    call: &'a ActionCall<'call>,
    effective_input: &'a JsonObject,
    authorizer: &'a dyn ActionAuthorizer,
    parent: &'a FactKey,
    authorize_parent: bool,
    proposed_rows: &'a BTreeSet<(String, String)>,
    declared: &'a BTreeMap<String, OwnedChildCollection>,
}

fn authorize_owned_proposals<X>(
    context: &OwnedProposalAuthorization<'_, '_, '_>,
    writes: &mut [FactWrite],
) -> Result<(), ActionRefusal<X>> {
    let mut targets = Vec::new();
    if context.authorize_parent {
        targets.push(context.parent.clone());
    }
    for (collection_id, row_id) in context.proposed_rows {
        let Some(collection) = context.declared.get(collection_id) else {
            return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: format!("unknown owned collection {collection_id:?} in proposed row set"),
            }));
        };
        targets.push(FactKey::new(collection.child_type.as_str(), row_id.clone()));
    }
    for target in targets {
        let Some(write) = writes.iter_mut().find(|write| write.key == target) else {
            // An exact no-op update/reorder has no proposed write. Its
            // committed snapshot was already authorized above.
            continue;
        };
        let FactMutation::Put { values } = &mut write.mutation else {
            continue;
        };
        let canonical = context
            .catalog
            .effective_object_values(&target.object_type, values)
            .map_err(|error| {
                ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("the effective values for {target}: {error}"),
                })
            })?;
        *values = canonical;
        match context.authorizer.authorize(&ActionAuthzRequest {
            action: context.call.action,
            scope: &context.call.scope,
            actor: &context.call.actor,
            input: context.effective_input,
            target: &target,
            target_values: Some(values),
        }) {
            Ok(()) => {}
            Err(ActionDenial::Invisible) => return Err(ActionRefusal::TargetNotFound),
            Err(ActionDenial::Forbidden { reason }) => {
                return Err(ActionRefusal::PermissionDenied { reason });
            }
        }
    }
    Ok(())
}

fn invalid_snapshot<X>(reason: impl Into<String>) -> ActionRefusal<X> {
    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
        reason: reason.into(),
    })
}

fn snapshot_collection<'a, X>(
    rows: &'a BTreeMap<String, Vec<String>>,
    collection_id: &str,
) -> Result<&'a Vec<String>, ActionRefusal<X>> {
    rows.get(collection_id).ok_or_else(|| {
        invalid_snapshot(format!(
            "owned-row snapshot is missing collection {collection_id:?}"
        ))
    })
}

fn snapshot_row<'a, X>(
    rows: &'a BTreeMap<(String, String), FactState>,
    collection_id: &str,
    row_id: &str,
) -> Result<&'a FactState, ActionRefusal<X>> {
    rows.get(&(collection_id.to_owned(), row_id.to_owned()))
        .ok_or_else(|| {
            invalid_snapshot(format!(
                "owned-row snapshot is missing row {row_id:?} in collection {collection_id:?}"
            ))
        })
}

fn snapshot_config<'a, X>(
    configs: &'a BTreeMap<String, &OwnedCollectionAction>,
    collection_id: &str,
) -> Result<&'a OwnedCollectionAction, ActionRefusal<X>> {
    configs.get(collection_id).copied().ok_or_else(|| {
        invalid_snapshot(format!(
            "owned-row action is missing collection {collection_id:?}"
        ))
    })
}

fn snapshot_manifest<'a, X>(
    manifests: &'a BTreeMap<String, FactState>,
    collection_id: &str,
) -> Result<&'a FactState, ActionRefusal<X>> {
    manifests.get(collection_id).ok_or_else(|| {
        invalid_snapshot(format!(
            "owned-row snapshot is missing manifest for collection {collection_id:?}"
        ))
    })
}

fn snapshot_declared<'a, X>(
    declared: &'a BTreeMap<String, OwnedChildCollection>,
    collection_id: &str,
) -> Result<&'a OwnedChildCollection, ActionRefusal<X>> {
    declared.get(collection_id).ok_or_else(|| {
        invalid_snapshot(format!(
            "owned-row declaration is missing collection {collection_id:?}"
        ))
    })
}

fn normalized_row_operations<X>(
    raw: &[JsonValue],
    delete_parent: bool,
    declared: &BTreeMap<String, OwnedChildCollection>,
    ordered_rows: &BTreeMap<String, Vec<String>>,
    row_states: &BTreeMap<(String, String), FactState>,
) -> Result<Vec<JsonValue>, ActionRefusal<X>> {
    let mut operations = raw.to_vec();
    if delete_parent {
        for collection_id in declared.keys() {
            for row_id in snapshot_collection(ordered_rows, collection_id)? {
                let version = snapshot_row(row_states, collection_id, row_id)?.version;
                operations.push(JsonValue::Object(JsonObject::from([
                    (
                        "collection".to_owned(),
                        JsonValue::String(collection_id.clone()),
                    ),
                    ("op".to_owned(), JsonValue::String("remove".to_owned())),
                    ("rowId".to_owned(), JsonValue::String(row_id.clone())),
                    (
                        "expectedVersion".to_owned(),
                        JsonValue::String(version.0.to_string()),
                    ),
                    ("cascade".to_owned(), JsonValue::Bool(true)),
                ])));
            }
        }
    }
    Ok(operations)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the admitted parent-and-row effect is one atomic authorization and commit boundary"
)]
pub(super) fn commit_parent_rows<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    mut preparation: ActionInputPreparation<'_>,
    authorizer: &dyn ActionAuthorizer,
    object_id_field: &str,
    object_kind: &str,
    parent_values_field: &str,
    row_ops_field: &str,
    parent_version_field: &str,
    parent_delete_field: Option<&str>,
    collection_actions: &[OwnedCollectionAction],
    limits: &EnvelopeLimits,
    meter: Option<&mut TurnEnvelopeMeter>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
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
    let target = FactKey::new(object_type.as_str(), object_id.clone());
    crate::check_fact_key_identity(&target)
        .map_err(|error| ActionRefusal::Decision(DecisionRefusal::from_commit(error)))?;
    let parent_state = host.load(&target).map_err(host_failure)?;
    if parent_state.exists() {
        authorize_current_target(
            call,
            &preparation.effective_input,
            authorizer,
            &target,
            &parent_state,
        )?;
    }
    check_kind(catalog, &object_type, object_kind)?;

    let expected_parent =
        input_version::<H::Error>(&preparation.effective_input, parent_version_field)?;
    let parent_values = match preparation.effective_input.get(parent_values_field) {
        Some(JsonValue::Object(values)) => values.clone(),
        _ => {
            return Err(ActionRefusal::InvalidInput {
                field: parent_values_field.to_owned(),
            });
        }
    };
    let delete_parent = match parent_delete_field {
        None => false,
        Some(field) => match preparation.effective_input.get(field) {
            None => false,
            Some(JsonValue::Bool(delete)) => *delete,
            _ => {
                return Err(ActionRefusal::InvalidInput {
                    field: field.to_owned(),
                });
            }
        },
    };
    if delete_parent && !parent_values.is_empty() {
        return Err(ActionRefusal::InvalidInput {
            field: parent_values_field.to_owned(),
        });
    }
    let Some(JsonValue::Array(raw_row_ops)) = preparation.effective_input.get(row_ops_field) else {
        return Err(ActionRefusal::InvalidInput {
            field: row_ops_field.to_owned(),
        });
    };
    if delete_parent && !raw_row_ops.is_empty() {
        return Err(ActionRefusal::InvalidInput {
            field: row_ops_field.to_owned(),
        });
    }

    let Some(effective_type) = catalog.effective_type(&object_type) else {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: call.action.action_id.clone(),
        });
    };
    validate_stored_patch::<H::Error>(catalog, &object_type, &parent_values, parent_values_field)?;
    let mut declared = effective_type
        .owned_collections
        .iter()
        .map(|collection| (collection.id.as_str().to_owned(), collection.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut configs = BTreeMap::new();
    for config in collection_actions {
        if configs
            .insert(config.id.as_str().to_owned(), config)
            .is_some()
        {
            return Err(ActionRefusal::UnsupportedEffect {
                action_id: call.action.action_id.clone(),
            });
        }
    }
    let Some(action_type) = catalog.effective_type(&ObjectTypeName(object_kind.to_owned())) else {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: call.action.action_id.clone(),
        });
    };
    // Coverage belongs to the action's pinned type, not subtype-added collections.
    if action_type.owned_collections.is_empty()
        || action_type.owned_collections.len() != configs.len()
        || action_type
            .owned_collections
            .iter()
            .any(|collection| !configs.contains_key(collection.id.as_str()))
        || configs.keys().any(|id| !declared.contains_key(id))
        || (delete_parent && declared.keys().any(|id| !configs.contains_key(id)))
    {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: call.action.action_id.clone(),
        });
    }
    declared.retain(|id, _| configs.contains_key(id));
    if declared
        .values()
        .any(|collection| collection.max_rows.0 == 0 || collection.max_rows.0 > 100)
        || declared.iter().any(|(id, collection)| {
            let Some(child) = catalog.effective_type(&collection.child_type) else {
                return true;
            };
            if [&collection.row_id_property, &collection.ordering_key]
                .into_iter()
                .any(|property| {
                    child
                        .property(property.as_str())
                        .is_some_and(norn_checker::EffectiveProperty::protected)
                })
            {
                return true;
            }
            configs.get(id.as_str()).is_none_or(|config| {
                config.derive.keys().any(|property| {
                    property == &collection.row_id_property
                        || property == &collection.ordering_key
                        || type_series(catalog, &collection.child_type)
                            .is_some_and(|series| series.key_properties.contains(property))
                        || child
                            .properties
                            .iter()
                            .find(|candidate| candidate.id == *property)
                            .is_none_or(|candidate| candidate.computed || candidate.protected())
                })
            })
        })
    {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: call.action.action_id.clone(),
        });
    }
    let parsed = parse_owned_row_ops::<H::Error>(raw_row_ops, limits, &declared)?;
    if parsed
        .iter()
        .any(|op| !declared.contains_key(op.collection()))
    {
        return Err(ActionRefusal::InvalidInput {
            field: row_ops_field.to_owned(),
        });
    }

    let mut manifest_states = BTreeMap::<String, FactState>::new();
    let mut ordered_rows = BTreeMap::<String, Vec<String>>::new();
    let mut row_states = BTreeMap::<(String, String), FactState>::new();
    let mut row_facts = BTreeSet::new();
    let mut manifest_bytes = 0usize;
    let slice_limits = crate::limits::ResourceLimitProfile::norn_limits_v1().slice;
    let max_snapshot_rows = usize::try_from(slice_limits.max_records).unwrap_or(usize::MAX);
    if declared.len() > max_snapshot_rows {
        return Err(ActionRefusal::SliceUnavailable {
            entry: "$owned_rows".to_owned(),
            reason: "owned-row manifests exceed the bounded slice records".to_owned(),
        });
    }
    let mut collection_rows = 0usize;
    for (collection_id, collection) in &declared {
        let manifest_key = owned_manifest_key::<H::Error>(&target, collection_id)?;
        let manifest = host.load(&manifest_key).map_err(host_failure)?;
        let ids = read_manifest_ids::<H::Error>(&manifest, &target, collection_id, collection)?;
        if let Some(values) = &manifest.values {
            let bytes = norn_ir::canonical::canonical_json_of(values)
                .map_err(|error| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!("owned-row manifest could not be measured: {error}"),
                    })
                })?
                .len();
            manifest_bytes = manifest_bytes.saturating_add(bytes);
        }
        for row_id in &ids {
            let row_key = FactKey::new(collection.child_type.as_str(), row_id.clone());
            crate::check_fact_key_identity(&row_key)
                .map_err(|error| ActionRefusal::Decision(DecisionRefusal::from_commit(error)))?;
        }
        collection_rows = collection_rows.saturating_add(ids.len());
        manifest_states.insert(collection_id.clone(), manifest);
        ordered_rows.insert(collection_id.clone(), ids);
    }
    let snapshot_cost = collection_rows
        .saturating_add(raw_row_ops.len())
        .saturating_add(declared.len());
    if snapshot_cost > max_snapshot_rows {
        return Err(ActionRefusal::SliceUnavailable {
            entry: "$owned_rows".to_owned(),
            reason: "owned-row state and operations exceed the bounded slice records".to_owned(),
        });
    }
    let input_bytes = norn_ir::canonical::canonical_json_of(&preparation.effective_input)
        .map_err(|error| ActionRefusal::InvalidInput {
            field: format!("$input ({error})"),
        })?
        .len();
    let max_snapshot_bytes =
        usize::try_from(slice_limits.max_canonical_bytes).unwrap_or(usize::MAX);
    let mut snapshot_bytes = manifest_bytes.saturating_add(input_bytes);
    if let Some(values) = &parent_state.values {
        let bytes = norn_ir::canonical::canonical_json_of(values)
            .map_err(|error| {
                ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("parent value snapshot could not be measured: {error}"),
                })
            })?
            .len();
        snapshot_bytes = snapshot_bytes.saturating_add(bytes);
    }
    if snapshot_bytes > max_snapshot_bytes {
        return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: "owned-row state, manifests, and action input snapshot exceed the bounded slice bytes"
                .to_owned(),
        }));
    }
    for (collection_id, collection) in &declared {
        let ids = snapshot_collection(&ordered_rows, collection_id)?;
        for (index, row_id) in ids.iter().enumerate() {
            let row_key = FactKey::new(collection.child_type.as_str(), row_id.clone());
            crate::check_fact_key_identity(&row_key)
                .map_err(|error| ActionRefusal::Decision(DecisionRefusal::from_commit(error)))?;
            if !row_facts.insert(row_key.clone()) {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("row {row_key} appears in more than one owned collection"),
                }));
            }
            let row_state = host.load(&row_key).map_err(host_failure)?;
            authorize_current_target(
                call,
                &preparation.effective_input,
                authorizer,
                &row_key,
                &row_state,
            )?;
            let Some(values) = row_state.values.as_ref() else {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("owned row {row_key} is missing from storage"),
                }));
            };
            let bytes = norn_ir::canonical::canonical_json_of(values)
                .map_err(|error| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!("owned row value snapshot could not be measured: {error}"),
                    })
                })?
                .len();
            snapshot_bytes = snapshot_bytes.saturating_add(bytes);
            if snapshot_bytes > max_snapshot_bytes {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: "owned-row state, manifests, and action input snapshot exceed the bounded slice bytes"
                        .to_owned(),
                }));
            }
            if values.get(collection.row_id_property.as_str())
                != Some(&JsonValue::String(row_id.clone()))
                || !integer_equals(
                    values.get(collection.ordering_key.as_str()),
                    index
                        .checked_add(1)
                        .ok_or_else(|| ActionRefusal::Decision(DecisionRefusal::Overflow))?,
                )
            {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!(
                        "owned row {row_key} has an inconsistent stable id or ordering key"
                    ),
                }));
            }
            let link_key = LinkKey::new(target.clone(), collection_id.clone(), row_key.clone());
            let link_state = host.load_link(&link_key).map_err(host_failure)?;
            let Some(link_values) = link_state.values.as_ref() else {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("owned row {row_key} is missing its declared parent link"),
                }));
            };
            let bytes = norn_ir::canonical::canonical_json_of(link_values)
                .map_err(|error| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!(
                            "owned row link value snapshot could not be measured: {error}"
                        ),
                    })
                })?
                .len();
            snapshot_bytes = snapshot_bytes.saturating_add(bytes);
            if snapshot_bytes > max_snapshot_bytes {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: "owned-row state, links, manifests, and action input snapshot exceed the bounded slice bytes"
                        .to_owned(),
                }));
            }
            row_states.insert((collection_id.clone(), row_id.clone()), row_state);
        }
    }

    let record_key = FactKey::new(
        ACTION_CALL_TYPE,
        format!("{}/{}", call.action.action_id, call.idempotency_key),
    );
    let record = ActionCallRecord {
        action_id: call.action.action_id.clone(),
        action_artifact_id: Some(call.action_artifact_id.to_owned()),
        action_version: Some(call.action_version.to_owned()),
        effect_kind: Some(ACTION_EFFECT_MUTATE_PARENT_WITH_ROWS.to_owned()),
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
        row_operations: normalized_row_operations(
            raw_row_ops,
            delete_parent,
            &declared,
            &ordered_rows,
            &row_states,
        )?,
        trace: call.trace.clone(),
    };
    let recorded = host.load(&record_key).map_err(host_failure)?;
    if !parent_state.exists() && recorded.exists() {
        // A deleted parent cannot be used to reveal a formerly successful
        // mutation through its idempotency record. Create retries still
        // replay because their current parent exists and was authorized.
        authorize_current_target(
            call,
            &preparation.effective_input,
            authorizer,
            &target,
            &parent_state,
        )?;
    }
    if let Some(outcome) = replay(&recorded, &record, &call.idempotency_key)? {
        return Ok(outcome);
    }

    if !parent_state.exists() && type_series(catalog, &object_type).is_some() {
        return Err(ActionRefusal::InvalidInput {
            field: object_id_field.to_owned(),
        });
    }
    if parsed.iter().any(|op| {
        matches!(op, ParsedOwnedRowOp::Add { collection, .. }
            if declared.get(collection).is_some_and(|owned| type_series(catalog, &owned.child_type).is_some()))
    }) {
        return Err(ActionRefusal::InvalidInput {
            field: row_ops_field.to_owned(),
        });
    }

    // Reserve the bounded owned-state/input work before loading declared dependencies.
    let mut slice_meter = SliceMeter::new(slice_limits);
    for record_index in 0..snapshot_cost {
        let bytes = if record_index == 0 {
            u64::try_from(snapshot_bytes).unwrap_or(u64::MAX)
        } else {
            0
        };
        slice_meter
            .admit_record(bytes, 0)
            .map_err(|error| ActionRefusal::SliceUnavailable {
                entry: "$owned_rows".to_owned(),
                reason: error.to_string(),
            })?;
    }
    let action_slice = load_action_slice(
        &ActionSliceContext {
            host,
            catalog,
            call,
            effective_input: &preparation.effective_input,
            authorizer,
        },
        &target,
        &parent_state,
        slice_meter,
    )?;
    preparation.precondition_outcomes = check_action_preconditions(
        catalog,
        call.action,
        &preparation.effective_input,
        &action_slice,
        &mut preparation.remaining_fuel,
        &mut preparation.receipt_evidence_bytes,
        usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
    )?;
    let mut record = record;
    record
        .precondition_outcomes
        .clone_from(&preparation.precondition_outcomes);
    record
        .parameter_pins
        .clone_from(&action_slice.parameter_pins);
    check_action_record_size::<H::Error>(call.action, &record, limits)?;

    // Resolve replays before CAS validation; current authorization already ran above.
    if delete_parent && !parent_state.exists() {
        return Err(ActionRefusal::TargetNotFound);
    }
    if delete_parent
        && declared
            .values()
            .any(|collection| !collection.cascade_on_parent_delete)
    {
        return Err(ActionRefusal::WriteModeFailed {
            mode: "parent_delete".to_owned(),
            reason: "every owned collection must declare cascadeOnParentDelete".to_owned(),
        });
    }

    let mut changes = Vec::<FactChange>::new();
    if delete_parent {
        changes.push(FactChange::Delete {
            object_type: object_type.clone(),
            key: object_id.clone(),
        });
    } else if !parent_state.exists() {
        changes.push(FactChange::Create {
            object_type: object_type.clone(),
            key: object_id.clone(),
            values: parent_values,
        });
    } else {
        changes.extend(
            parent_values
                .iter()
                .map(|(property, value)| FactChange::Set {
                    object_type: object_type.clone(),
                    key: object_id.clone(),
                    property: PropertyId(property.clone()),
                    value: value.clone(),
                }),
        );
        if parent_values.is_empty() && !parsed.is_empty() {
            let current_values = parent_state.values.as_ref().ok_or_else(|| {
                ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: "existing parent has no committed value snapshot".to_owned(),
                })
            })?;
            let lifecycle_properties = catalog.lifecycle_properties(&object_type);
            let touch = effective_type.properties.iter().find_map(|property| {
                if property.computed
                    || property.protected()
                    || lifecycle_properties.contains(&property.id)
                {
                    None
                } else {
                    current_values
                        .get(property.id.as_str())
                        .map(|value| (property.id.clone(), value.clone()))
                }
            });
            let Some((property, value)) = touch else {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: "a row-only mutation requires a writable parent property to advance the parent version".to_owned(),
                }));
            };
            changes.push(FactChange::Set {
                object_type: object_type.clone(),
                key: object_id.clone(),
                property,
                value,
            });
        }
    }

    let mut proposed_rows = BTreeSet::<(String, String)>::new();
    let mut row_values = BTreeMap::<(String, String), JsonObject>::new();
    let mut guarded_rows = BTreeMap::<(String, String), JsonObject>::new();
    let mut link_writes = Vec::<LinkWrite>::new();
    let mut manifest_writes = Vec::<FactWrite>::new();
    if delete_parent {
        for (collection_id, collection) in &declared {
            for row_id in snapshot_collection(&ordered_rows, collection_id)? {
                let row_key = FactKey::new(collection.child_type.as_str(), row_id.clone());
                changes.push(FactChange::Delete {
                    object_type: collection.child_type.clone(),
                    key: row_id.clone(),
                });
                let row_state = snapshot_row(&row_states, collection_id, row_id)?;
                let values = row_state.values.as_ref().ok_or_else(|| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!(
                            "owned row {row_id:?} disappeared from the loaded snapshot"
                        ),
                    })
                })?;
                guarded_rows.insert((collection_id.clone(), row_id.clone()), values.clone());
                let link_key = LinkKey::new(target.clone(), collection_id.clone(), row_key);
                let link_state = host.load_link(&link_key).map_err(host_failure)?;
                link_writes.push(LinkWrite {
                    key: link_key,
                    expected: link_state.version,
                    mutation: LinkMutation::Remove,
                });
            }
            let manifest_key = owned_manifest_key::<H::Error>(&target, collection_id)?;
            let manifest = snapshot_manifest(&manifest_states, collection_id)?;
            if manifest.exists() {
                manifest_writes.push(FactWrite {
                    key: manifest_key,
                    expected: manifest.version,
                    mutation: FactMutation::Delete,
                });
            }
        }
    } else {
        for (collection_id, collection) in &declared {
            let collection_ops = parsed
                .iter()
                .filter(|op| op.collection() == collection_id)
                .collect::<Vec<_>>();
            let initial = snapshot_collection(&ordered_rows, collection_id)?;
            let mut additions = Vec::new();
            let mut removals = BTreeSet::new();
            let mut replacements = BTreeMap::<String, JsonObject>::new();
            let mut touched = BTreeSet::<String>::new();
            let mut reorder = None;
            for op in &collection_ops {
                match op {
                    ParsedOwnedRowOp::Add {
                        row_id,
                        expected,
                        values,
                        ..
                    } => {
                        if *expected != FactVersion::ABSENT || initial.contains(row_id) {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        if !touched.insert(row_id.clone()) {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        let row_key = FactKey::new(collection.child_type.as_str(), row_id.clone());
                        crate::check_fact_key_identity(&row_key).map_err(|error| {
                            ActionRefusal::Decision(DecisionRefusal::from_commit(error))
                        })?;
                        if row_facts.contains(&row_key) {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        let current = host.load(&row_key).map_err(host_failure)?;
                        if current.exists() || current.version != FactVersion::ABSENT {
                            if current.exists() {
                                authorize_current_target(
                                    call,
                                    &preparation.effective_input,
                                    authorizer,
                                    &row_key,
                                    &current,
                                )?;
                            }
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        let config = snapshot_config(&configs, collection_id)?;
                        validate_row_values::<H::Error>(
                            catalog,
                            values,
                            collection,
                            config,
                            row_ops_field,
                        )?;
                        let mut proposal = values.clone();
                        proposal.insert(
                            collection.row_id_property.as_str().to_owned(),
                            JsonValue::String(row_id.clone()),
                        );
                        additions.push(row_id.clone());
                        replacements.insert(row_id.clone(), proposal);
                        proposed_rows.insert((collection_id.clone(), row_id.clone()));
                    }
                    ParsedOwnedRowOp::Update {
                        row_id,
                        expected,
                        values,
                        ..
                    } => {
                        if !initial.contains(row_id) || !touched.insert(row_id.clone()) {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        let current = snapshot_row(&row_states, collection_id, row_id)?;
                        if current.version != *expected {
                            return Err(stale_action(
                                FactKey::new(collection.child_type.as_str(), row_id.clone()),
                                *expected,
                                current.version,
                            ));
                        }
                        let config = snapshot_config(&configs, collection_id)?;
                        validate_row_values::<H::Error>(
                            catalog,
                            values,
                            collection,
                            config,
                            row_ops_field,
                        )?;
                        let current_values = current.values.as_ref().ok_or_else(|| {
                            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                                reason: format!(
                                    "owned row {row_id:?} disappeared from the loaded snapshot"
                                ),
                            })
                        })?;
                        let mut proposal = current_values.clone();
                        proposal.extend(values.clone());
                        replacements.insert(row_id.clone(), proposal);
                        proposed_rows.insert((collection_id.clone(), row_id.clone()));
                    }
                    ParsedOwnedRowOp::Remove {
                        row_id, expected, ..
                    } => {
                        if !initial.contains(row_id) || !touched.insert(row_id.clone()) {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        let current = snapshot_row(&row_states, collection_id, row_id)?;
                        if current.version != *expected {
                            return Err(stale_action(
                                FactKey::new(collection.child_type.as_str(), row_id.clone()),
                                *expected,
                                current.version,
                            ));
                        }
                        let current_values = current.values.as_ref().ok_or_else(|| {
                            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                                reason: format!(
                                    "owned row {row_id:?} disappeared from the loaded snapshot"
                                ),
                            })
                        })?;
                        guarded_rows.insert(
                            (collection_id.clone(), row_id.clone()),
                            current_values.clone(),
                        );
                        removals.insert(row_id.clone());
                    }
                    ParsedOwnedRowOp::Reorder { rows, .. } => {
                        if reorder.is_some() {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        for (row_id, expected) in rows {
                            let state = row_states.get(&(collection_id.clone(), row_id.clone()));
                            let actual = state.map_or(FactVersion::ABSENT, |state| state.version);
                            if actual != *expected {
                                return Err(stale_action(
                                    FactKey::new(collection.child_type.as_str(), row_id.clone()),
                                    *expected,
                                    actual,
                                ));
                            }
                        }
                        reorder = Some(rows.clone());
                    }
                }
            }
            let mut final_order = initial
                .iter()
                .filter(|row_id| !removals.contains(*row_id))
                .cloned()
                .collect::<Vec<_>>();
            final_order.extend(additions.iter().cloned());
            if let Some(reorder) = reorder {
                let requested = reorder
                    .iter()
                    .map(|(row_id, _)| row_id.clone())
                    .collect::<Vec<_>>();
                let expected_members = final_order.iter().cloned().collect::<BTreeSet<_>>();
                let requested_members = requested.iter().cloned().collect::<BTreeSet<_>>();
                if requested.len() != requested_members.len()
                    || requested_members != expected_members
                {
                    return Err(ActionRefusal::InvalidInput {
                        field: row_ops_field.to_owned(),
                    });
                }
                final_order = requested;
            }
            if final_order.len() > usize::try_from(collection.max_rows.0).unwrap_or(100) {
                return Err(ActionRefusal::InvalidInput {
                    field: row_ops_field.to_owned(),
                });
            }
            for row_id in &final_order {
                let row_key = (collection_id.clone(), row_id.clone());
                if let Some(values) = replacements.get(row_id) {
                    row_values.insert(row_key, values.clone());
                } else if let Some(values) = row_states
                    .get(&(collection_id.clone(), row_id.clone()))
                    .and_then(|state| state.values.clone())
                {
                    row_values.insert(row_key, values);
                }
            }
            for (index, row_id) in final_order.iter().enumerate() {
                let order = decimal_value(
                    index
                        .checked_add(1)
                        .ok_or_else(|| ActionRefusal::Decision(DecisionRefusal::Overflow))?,
                )
                .map_err(|reason| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput { reason })
                })?;
                let key = (collection_id.clone(), row_id.clone());
                let mut values = row_values.get(&key).cloned().ok_or_else(|| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!("owned row {row_id:?} has no final value snapshot"),
                    })
                })?;
                let old_order = row_states
                    .get(&key)
                    .and_then(|state| state.values.as_ref())
                    .and_then(|values| values.get(collection.ordering_key.as_str()));
                let order_changed = old_order != Some(&order);
                values.insert(collection.ordering_key.as_str().to_owned(), order);
                values = catalog
                    .materialize_object_defaults(&collection.child_type, &values)
                    .map_err(|error| {
                        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                            reason: format!(
                                "owned row defaults could not be materialized: {error}"
                            ),
                        })
                    })?;
                values = fill_lifecycle_defaults::<H::Error>(
                    catalog,
                    &FactKey::new(collection.child_type.as_str(), row_id.clone()),
                    &values,
                )?;
                let recompute_derived = additions.contains(row_id)
                    || replacements.contains_key(row_id)
                    || order_changed;
                if recompute_derived {
                    values = derive_row_values::<H::Error>(
                        call.action,
                        catalog,
                        collection,
                        snapshot_config(&configs, collection_id)?,
                        &preparation.effective_input,
                        values,
                        &mut preparation.remaining_fuel,
                    )?;
                    guarded_rows.insert(key.clone(), values.clone());
                }
                row_values.insert(key.clone(), values.clone());
                if additions.contains(row_id) {
                    changes.push(FactChange::Create {
                        object_type: collection.child_type.clone(),
                        key: row_id.clone(),
                        values: values.clone(),
                    });
                    let link_key = LinkKey::new(
                        target.clone(),
                        collection_id.clone(),
                        FactKey::new(collection.child_type.as_str(), row_id.clone()),
                    );
                    let link_state = host.load_link(&link_key).map_err(host_failure)?;
                    if link_state.exists() {
                        return Err(ActionRefusal::InvalidInput {
                            field: row_ops_field.to_owned(),
                        });
                    }
                    link_writes.push(LinkWrite {
                        key: link_key,
                        expected: link_state.version,
                        mutation: LinkMutation::Add {
                            values: JsonObject::new(),
                        },
                    });
                } else {
                    let old = row_states
                        .get(&key)
                        .and_then(|state| state.values.as_ref())
                        .ok_or_else(|| {
                            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                                reason: format!(
                                    "owned row {row_id:?} disappeared from the loaded snapshot"
                                ),
                            })
                        })?;
                    for (property, value) in &values {
                        if property == collection.row_id_property.as_str() {
                            continue;
                        }
                        if old.get(property) != Some(value)
                            && (recompute_derived
                                || (property == collection.ordering_key.as_str() && order_changed))
                        {
                            changes.push(FactChange::Set {
                                object_type: collection.child_type.clone(),
                                key: row_id.clone(),
                                property: PropertyId(property.clone()),
                                value: value.clone(),
                            });
                        }
                    }
                }
                if proposed_rows.contains(&key) || order_changed {
                    proposed_rows.insert(key);
                }
            }
            for row_id in &removals {
                changes.push(FactChange::Delete {
                    object_type: collection.child_type.clone(),
                    key: row_id.clone(),
                });
                let link_key = LinkKey::new(
                    target.clone(),
                    collection_id.clone(),
                    FactKey::new(collection.child_type.as_str(), row_id.clone()),
                );
                let link_state = host.load_link(&link_key).map_err(host_failure)?;
                if !link_state.exists() {
                    return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!("owned row {row_id:?} is missing its declared parent link"),
                    }));
                }
                link_writes.push(LinkWrite {
                    key: link_key,
                    expected: link_state.version,
                    mutation: LinkMutation::Remove,
                });
            }
            if !collection_ops.is_empty() {
                let manifest_key = owned_manifest_key::<H::Error>(&target, collection_id)?;
                let manifest = snapshot_manifest(&manifest_states, collection_id)?;
                manifest_writes.push(FactWrite {
                    key: manifest_key,
                    expected: manifest.version,
                    mutation: FactMutation::Put {
                        values: manifest_values(&target, collection_id, &final_order),
                    },
                });
            }
        }
    }

    // Materialize link defaults before sealing so host canonicalization is unchanged.
    for write in &mut link_writes {
        if let LinkMutation::Add { values } = &mut write.mutation {
            *values = catalog
                .effective_link_values(&target.object_type, &write.key.link, values)
                .map_err(|error| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!("owned collection link values are invalid: {error}"),
                    })
                })?;
        }
    }

    let mut slice = action_slice.facts;
    for ((collection_id, row_id), state) in &row_states {
        let row_type = &snapshot_declared(&declared, collection_id)?.child_type;
        reconcile_action_target_snapshot::<H::Error>(
            catalog,
            &mut slice,
            &FactKey::new(row_type.as_str(), row_id.clone()),
            state,
        )?;
    }
    for op in &parsed {
        if let ParsedOwnedRowOp::Add {
            collection, row_id, ..
        } = op
        {
            reconcile_action_target_snapshot::<H::Error>(
                catalog,
                &mut slice,
                &FactKey::new(
                    snapshot_declared(&declared, collection)?
                        .child_type
                        .as_str(),
                    row_id.clone(),
                ),
                &FactState::default(),
            )?;
        }
    }
    let commands = build_action_commands::<H::Error>(
        call.action,
        &preparation.effective_input,
        &format!("{}/{}", call.action.action_id, call.idempotency_key),
        &mut preparation.remaining_fuel,
    )?;
    let output = DecisionOutput {
        facts: changes,
        commands,
        timers: Vec::new(),
    };
    let mut prepared =
        prepare_owned_rows::<H::Error>(catalog, &slice, &call.turn, &output, limits)?;
    for (collection_id, manifest) in &manifest_states {
        let key = owned_manifest_key::<H::Error>(&target, collection_id)?;
        if !manifest_writes.iter().any(|write| write.key == key) {
            prepared.reads.push(FactExpectation {
                key,
                expected: manifest.version,
            });
        }
    }
    let counts =
        check_action_envelope::<H::Error>(limits, prepared.events.len(), output.commands.len())?;
    authorize_owned_proposals(
        &OwnedProposalAuthorization {
            catalog,
            call,
            effective_input: &preparation.effective_input,
            authorizer,
            parent: &target,
            authorize_parent: !delete_parent,
            proposed_rows: &proposed_rows,
            declared: &declared,
        },
        &mut prepared.writes,
    )?;
    // Check row preconditions only after every proposed write passes authorization.
    for ((collection_id, _), values) in &guarded_rows {
        let child_type = &declared
            .get(collection_id)
            .ok_or_else(|| {
                ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: "unknown owned collection".into(),
                })
            })?
            .child_type;
        check_row_preconditions::<H::Error>(
            call.action,
            catalog,
            child_type,
            snapshot_config(&configs, collection_id)?,
            &preparation.effective_input,
            values,
            &mut preparation.remaining_fuel,
        )?;
    }
    if delete_parent && !parent_state.exists() {
        return Err(ActionRefusal::TargetNotFound);
    }
    if expected_parent != parent_state.version {
        return Err(stale_action(target, expected_parent, parent_state.version));
    }
    let mut events = std::mem::take(&mut prepared.events);
    let action_call = format!("{}/{}", call.action.action_id, call.idempotency_key);
    for effect in &prepared.effects {
        if let crate::effect::Effect::Perform(command) = effect {
            events.push(FactEvent::CommandEnqueued {
                command_id: command.id.0.clone(),
                generation: command.generation.0,
                idempotency_key: command.idempotency_key.0.clone(),
                action_call: action_call.clone(),
                payload: command.payload.clone(),
            });
        }
    }
    let mut writes = prepared.writes;
    writes.extend(manifest_writes);
    let target_before = parent_state.version;
    super::receipt::ensure_target_read(&target, target_before, &writes, &mut prepared.reads);
    writes.push(FactWrite {
        key: record_key.clone(),
        expected: recorded.version,
        mutation: FactMutation::Put {
            values: record.to_values(),
        },
    });
    let commit = CellCommit {
        consumes: prepared.consumes,
        reads: prepared.reads,
        writes,
        events: events.into_iter().map(Into::into).collect(),
        effects: prepared.effects.into_iter().map(widen_effect).collect(),
        links: link_writes,
        link_reads: action_slice.link_reads,
        predicate_reads: action_slice.predicate_reads,
    };
    if let Some(meter) = meter.as_deref() {
        meter.check(counts).map_err(|exceeded| {
            ActionRefusal::Decision(DecisionRefusal::TurnEnvelopeExceeded {
                kind: exceeded.kind,
                limit: exceeded.limit,
                actual: exceeded.actual,
            })
        })?;
    }
    let quantity =
        crate::action_quantity::prepare_quantity_action(call, &preparation.effective_input)?;
    let proof = OwnedRowsCommitProof::new(&commit, limits.max_events, quantity);
    match host.commit_owned_rows(commit, proof) {
        Ok(receipt) => {
            if let Some(meter) = meter {
                meter.charge(counts);
            }
            let target_version =
                super::receipt::committed_target_version(&receipt, &target, target_before);
            Ok(ActionOutcome::Committed {
                receipt,
                record,
                target_version,
            })
        }
        Err(error) if error.is_invariant_refusal() => Err(ActionRefusal::Decision(
            ledger_action_invariant_refusal(host, call, error),
        )),
        Err(error) => {
            authorize_identity_collision(
                host,
                catalog,
                call,
                &preparation.effective_input,
                authorizer,
                &target,
                &error,
            )?;
            if matches!(
                &error,
                CommitError::Conflict { .. }
                    | CommitError::LinkConflict { .. }
                    | CommitError::LinkReadConflict { .. }
                    | CommitError::UnsupportedLinkRead { .. }
                    | CommitError::PredicateReadConflict { .. }
                    | CommitError::NotPending { .. }
            ) && let Some(outcome) = replay_parent_rows_after_race(
                host,
                call,
                &preparation.effective_input,
                authorizer,
                &target,
                &record_key,
                &record,
                &declared,
            )? {
                return Ok(outcome);
            }
            Err(
                precondition_read_refusal(&error, &action_slice.precondition_keys).unwrap_or_else(
                    || ActionRefusal::Decision(DecisionRefusal::from_commit(error)),
                ),
            )
        }
    }
}

/// Rechecks the winner's committed parent and descendants before exposing an
/// idempotent replay after a compare-and-swap race.
#[allow(
    clippy::too_many_arguments,
    reason = "replay reauthorizes the parent and descendants with the prepared effective input"
)]
fn replay_parent_rows_after_race<H>(
    host: &H,
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    parent: &FactKey,
    record_key: &FactKey,
    record: &ActionCallRecord,
    declared: &BTreeMap<String, OwnedChildCollection>,
) -> Result<Option<ActionOutcome>, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
{
    let recorded = host.load(record_key).map_err(host_failure)?;
    if !recorded.exists() {
        return Ok(None);
    }
    let parent_state = host.load(parent).map_err(host_failure)?;
    authorize_current_target(call, effective_input, authorizer, parent, &parent_state)?;
    for (collection_id, collection) in declared {
        let key = owned_manifest_key::<H::Error>(parent, collection_id)?;
        let manifest = host.load(&key).map_err(host_failure)?;
        for row_id in read_manifest_ids::<H::Error>(&manifest, parent, collection_id, collection)? {
            let row = FactKey::new(collection.child_type.as_str(), row_id);
            let state = host.load(&row).map_err(host_failure)?;
            authorize_current_target(call, effective_input, authorizer, &row, &state)?;
        }
    }
    replay(&recorded, record, &call.idempotency_key)
}
