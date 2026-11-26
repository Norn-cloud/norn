//! Admission checks for `TRANSITION-PATCH-1` (`fire_transition` `setFields`),
//! post-slice derivation effect compatibility, and owned-row origin parity.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::Type;
use norn_ir::{
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, JsonValue, PropertyId, ReceiptAcceptance,
    TransitionId,
};

use crate::Diagnostic;
use crate::entity_instance::{EffectiveObjectType, EffectiveProperty};

use super::{Model, codes, finding, input_schema_field_required};

pub(super) fn post_slice_effect_findings(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    model: &Model<'_>,
) -> Vec<Diagnostic> {
    if norn_expr::schema::first_slice_derivation_index(root).is_none() {
        return Vec::new();
    }
    let unsupported_post_slice = match &root.effect_program {
        Some(ActionEffectProgram::MutateParentWithRows { .. }) => {
            Some("mutate_parent_with_rows does not support post-slice derive steps")
        }
        Some(ActionEffectProgram::UpsertInputObject { object_kind, .. })
            if model
                .effective_object_type_named(object_kind)
                .is_some_and(|v| v.keys.iter().any(|key| key.naming_series.is_some())) =>
        {
            Some("naming-series upserts do not support post-slice derive steps")
        }
        Some(
            ActionEffectProgram::UpsertInputObject { .. }
            | ActionEffectProgram::FireTransition { .. },
        )
        | None => None,
    };
    unsupported_post_slice
        .map(|reason| {
            finding(
                codes::TARGET_NOT_WRITABLE,
                reason.to_owned(),
                artifact,
                "/payload/derive",
            )
        })
        .into_iter()
        .collect()
}

fn transition_guarded_fields(
    view: Option<&EffectiveObjectType>,
    lifecycle: &PropertyId,
    transition: &TransitionId,
) -> (BTreeSet<String>, bool) {
    let mut guarded_fields = BTreeSet::new();
    let mut guard_reads_all = false;
    if let Some(tr) = view
        .and_then(|view| view.lifecycle(lifecycle.as_str()))
        .and_then(|lc| {
            lc.lifecycle
                .transitions
                .iter()
                .find(|tr| tr.id == *transition)
        })
    {
        for guard in &tr.guards {
            match norn_expr::expression_reads(guard.as_str(), "self") {
                Ok(reads) => {
                    guard_reads_all |= reads.whole_root;
                    guarded_fields.extend(reads.fields);
                }
                Err(_) => guard_reads_all = true,
            }
        }
    }
    (guarded_fields, guard_reads_all)
}

pub(super) fn fire_transition_set_fields_findings(
    action: &ArtifactEnvelope,
    root: &ActionRoot,
    view: Option<&EffectiveObjectType>,
    lifecycle: &PropertyId,
    transition: &TransitionId,
    accept_receipt: Option<&ReceiptAcceptance>,
    set_fields: &BTreeMap<PropertyId, String>,
) -> Vec<Diagnostic> {
    if set_fields.is_empty() {
        return Vec::new();
    }
    let mut findings = Vec::new();
    let input_properties = root
        .input_schema
        .get("properties")
        .and_then(JsonValue::as_object);
    let (guarded_fields, guard_reads_all) = transition_guarded_fields(view, lifecycle, transition);
    let trusted_sources: BTreeSet<String> = root
        .actor_stamped_fields()
        .into_iter()
        .chain(root.clock_stamped_fields())
        .chain(norn_expr::schema::trusted_server_derived_fields(root))
        .collect();
    let writable = |id: &PropertyId| {
        !id.as_str().starts_with("_norn")
            && view.is_some_and(|view| {
                view.property(id.as_str())
                    .is_some_and(|property| !property.computed && !property.protected())
                    && view.lifecycle(id.as_str()).is_none()
                    && !view.keys.iter().any(|key| key.components.contains(id))
            })
    };
    for (target_prop, source_field) in set_fields {
        let receipt_overlap = accept_receipt.as_ref().is_some_and(|receipt| {
            &receipt.digest_property == target_prop
                || receipt.result_bindings.contains_key(target_prop)
        });
        let source_schema = input_properties.and_then(|props| props.get(source_field.as_str()));
        let target_effective = view.and_then(|view| view.property(target_prop.as_str()));
        let type_compatible =
            source_schema
                .zip(target_effective)
                .is_some_and(|(schema, target)| {
                    let source_required =
                        input_schema_field_required(&root.input_schema, source_field)
                            || root.derive.iter().any(|step| step.field == *source_field);
                    crate::typed_roots::action_schema_type(schema).is_some_and(|source_ty| {
                        set_field_type_compatible(&source_ty, source_required, target)
                    })
                });
        if !writable(target_prop) || receipt_overlap || source_schema.is_none() {
            findings.push(finding(
                codes::TARGET_NOT_WRITABLE,
                format!(
                    "fire_transition setFields {:?} -> {:?} requires a writable non-lifecycle, non-identity target property and a declared input field",
                    target_prop.as_str(),
                    source_field
                ),
                action,
                "/payload/effectProgram/setFields",
            ));
        } else if !type_compatible {
            findings.push(finding(
                codes::TARGET_NOT_WRITABLE,
                format!(
                    "fire_transition setFields {:?} -> {:?} has an input schema type or optionality incompatible with the target property",
                    target_prop.as_str(),
                    source_field
                ),
                action,
                "/payload/effectProgram/setFields",
            ));
        } else if (guard_reads_all || guarded_fields.contains(target_prop.as_str()))
            && !trusted_sources.contains(source_field)
        {
            findings.push(finding(
                codes::TARGET_NOT_WRITABLE,
                format!(
                    "fire_transition setFields {:?} -> {:?} cannot feed a caller-controlled input into a property read by transition {:?}'s guard",
                    target_prop.as_str(),
                    source_field,
                    transition.as_str()
                ),
                action,
                "/payload/effectProgram/setFields",
            ));
        }
    }
    findings
}

pub(super) fn is_owned_row_property_restricted(property: &EffectiveProperty) -> bool {
    property.computed
        || property.protected()
        || property.immutable_after_create()
        || property.origin().is_some_and(|origin| {
            !matches!(
                origin,
                norn_ir::PropertyOrigin::Caller | norn_ir::PropertyOrigin::Derived
            )
        })
}

pub(super) fn check_input_enums(
    action: &ArtifactEnvelope,
    program: &ActionEffectProgram,
    view: Option<&EffectiveObjectType>,
    enums: Vec<(String, bool, Vec<String>, String)>,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    for (field, top_level, values, at) in enums {
        let target_props = enum_target_properties(program, &field, top_level);
        if target_props.is_empty() {
            findings.push(finding(
                codes::INPUT_ENUM_UNBOUND,
                format!("input {field:?} declares an enum but writes no enum property"),
                action,
                &at,
            ));
            continue;
        }
        for target_prop in target_props {
            let target = view
                .and_then(|view| view.property(&target_prop))
                .and_then(|property| {
                    if let crate::EffectiveKind::Base(norn_ir::PropertyType::Enum { values }) =
                        &property.kind
                    {
                        Some(values)
                    } else {
                        None
                    }
                });
            let Some(declared) = target else {
                findings.push(finding(
                    codes::INPUT_ENUM_UNBOUND,
                    format!("input {field:?} declares an enum but writes no enum property"),
                    action,
                    &at,
                ));
                continue;
            };
            let missing: Vec<&String> = values
                .iter()
                .filter(|value| !declared.iter().any(|declared| &declared.value == *value))
                .collect();
            if !missing.is_empty() {
                findings.push(finding(
                    codes::INPUT_ENUM_NOT_SUBSET,
                    format!(
                        "input {field:?} admits {missing:?}, which property {target_prop:?} does not declare"
                    ),
                    action,
                    &at,
                ));
            }
        }
    }
    findings
}

fn enum_target_properties(
    program: &ActionEffectProgram,
    field: &str,
    top_level: bool,
) -> Vec<String> {
    match program {
        ActionEffectProgram::UpsertInputObject { .. } if top_level => vec![field.to_owned()],
        ActionEffectProgram::FireTransition { set_fields, .. } if top_level => set_fields
            .iter()
            .filter_map(|(target_prop, source_field)| {
                (source_field == field).then(|| target_prop.as_str().to_owned())
            })
            .collect(),
        ActionEffectProgram::UpsertInputObject { .. }
        | ActionEffectProgram::FireTransition { .. }
        | ActionEffectProgram::MutateParentWithRows { .. } => Vec::new(),
    }
}

fn set_field_type_compatible(
    source_ty: &Type,
    source_required: bool,
    target: &EffectiveProperty,
) -> bool {
    let source_nullable = matches!(source_ty, Type::Optional(_));
    if (!source_required || source_nullable) && !target.nullable {
        return false;
    }
    let unwrapped = if let Type::Optional(inner) = source_ty {
        inner.as_ref()
    } else {
        source_ty
    };
    let crate::EffectiveKind::Base(property_type) = &target.kind else {
        return false;
    };
    match property_type {
        norn_ir::PropertyType::Text { .. } => {
            matches!(unwrapped, Type::Text { .. } | Type::Enum { .. })
        }
        norn_ir::PropertyType::Timestamp { .. } => {
            matches!(unwrapped, Type::Timestamp { .. } | Type::Text { .. })
        }
        norn_ir::PropertyType::Date {} => matches!(unwrapped, Type::Date | Type::Text { .. }),
        norn_ir::PropertyType::Duration {} => {
            matches!(unwrapped, Type::Duration | Type::Text { .. })
        }
        norn_ir::PropertyType::Enum { .. } => {
            matches!(unwrapped, Type::Enum { .. } | Type::Text { .. })
        }
        norn_ir::PropertyType::Integer { .. } => matches!(unwrapped, Type::Integer),
        norn_ir::PropertyType::Decimal { .. } => {
            matches!(
                unwrapped,
                Type::Decimal { .. } | Type::Number | Type::Integer
            )
        }
        norn_ir::PropertyType::Boolean {} => matches!(unwrapped, Type::Bool),
        norn_ir::PropertyType::Record { .. } => matches!(unwrapped, Type::Record(_) | Type::Json),
        norn_ir::PropertyType::JsonExt { .. } => true,
        norn_ir::PropertyType::Ref { .. } | norn_ir::PropertyType::DecimalBinding { .. } => false,
    }
}
