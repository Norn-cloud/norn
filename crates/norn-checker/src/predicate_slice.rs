//! Admission checks for bounded predicate-keyed action slices (`SliceKey::Predicate`, #1586).
//!
//! Predicate slices share the exact indexed-scalar property rule with bounded
//! action aggregates (`crate::predicate_property`), ensuring hosts can fence
//! and verify every matching object without a type scan or silent truncation.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{FieldSource, RecordType, Type, ZenTypeEnv};
use norn_ir::{
    ActionAggregateInput, ActionRoot, ArtifactEnvelope, Diagnostic, DiagnosticRemediation,
    DiagnosticSeverity, JsonValue, PropertyId, ResourceLimitProfile, SliceDeclaration, SliceKey,
};

use crate::action_set::codes as action_codes;
use crate::entity_instance::EffectiveObjectType;
use crate::envelope::invalid_diagnostic;
use crate::predicate_property::{
    EQUALITY, ScalarClass, WINDOW, indexed_scalar_property, scalar_class,
};
use crate::typed_roots::{action_input_field_type, is_action_member_name};
use crate::{Diagnostic as CheckerDiagnostic, DiagnosticCode, Span};

/// Maximum filter + window terms permitted in one predicate query.
const MAX_PREDICATE_TERMS: usize = 32;

/// T2 local validation of a [`SliceKey::Predicate`] entry on an action.
pub(crate) fn validate_action_predicate_slice_key(
    payload: &std::collections::BTreeMap<String, JsonValue>,
    entry: &SliceDeclaration,
    path: &str,
    is_guaranteed: &dyn Fn(&str) -> bool,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let SliceKey::Predicate {
        filter,
        window,
        max_items,
    } = &entry.key
    else {
        return;
    };
    let code = "action.validation.invalid_payload";
    let limits = ResourceLimitProfile::norn_limits_v1().slice;
    let terms = filter.len().saturating_add(usize::from(window.is_some()));
    if terms > MAX_PREDICATE_TERMS {
        diagnostics.push(invalid_diagnostic(
            code,
            format!(
                "predicate slice {:?} has {terms} terms, above the cap of {MAX_PREDICATE_TERMS}",
                entry.name
            ),
            format!("{path}.key"),
            artifact_id,
        ));
    }
    if let Some(cap) = *max_items {
        if cap == 0 {
            diagnostics.push(invalid_diagnostic(
                code,
                "predicate slice maxItems must be at least 1",
                format!("{path}.key.maxItems"),
                artifact_id,
            ));
        } else if cap > limits.max_records {
            diagnostics.push(invalid_diagnostic(
                code,
                format!(
                    "predicate slice maxItems {cap} exceeds the profile slice record cap of {}",
                    limits.max_records
                ),
                format!("{path}.key.maxItems"),
                artifact_id,
            ));
        }
    }
    let mut check_input = |field: &str, field_path: String| {
        if action_input_field_type(payload.get("inputSchema"), field).is_none() {
            diagnostics.push(invalid_diagnostic(
                code,
                format!("predicate slice input field {field:?} is not declared in inputSchema"),
                field_path,
                artifact_id,
            ));
        } else if !is_guaranteed(field) {
            diagnostics.push(invalid_diagnostic(
                code,
                format!(
                    "predicate slice input field {field:?} may be absent; require its final input value"
                ),
                field_path,
                artifact_id,
            ));
        }
    };
    for (property, field) in filter {
        check_input(field, format!("{path}.key.filter.{}", property.as_str()));
    }
    if let Some(window) = window {
        check_input(&window.start, format!("{path}.key.window.start"));
        check_input(&window.end, format!("{path}.key.window.end"));
    }
}

/// T3 artifact-set validation of a [`SliceKey::Predicate`] entry against the
/// target type's effective stored properties and action input schema types.
pub(crate) fn action_predicate_slice_findings(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    entry: &SliceDeclaration,
    path: &str,
    effective: &EffectiveObjectType,
    record: &RecordType,
) -> Vec<CheckerDiagnostic> {
    let SliceKey::Predicate { filter, window, .. } = &entry.key else {
        return Vec::new();
    };
    let input_schema = JsonValue::Object(root.input_schema.clone());
    let mut findings = Vec::new();
    let mut compare = |suffix: String,
                       property: &PropertyId,
                       input: &str,
                       allowed: &[ScalarClass]| {
        let class = match indexed_scalar_property(record, effective, property.as_str(), allowed) {
            Ok((class, _)) => class,
            Err(error) => {
                findings.push(slice_finding(
                    artifact,
                    &format!("{path}{suffix}"),
                    format!(
                        "predicate slice {:?}: property {:?} {}",
                        entry.name,
                        property.as_str(),
                        error.reason()
                    ),
                ));
                return;
            }
        };
        let field_declared = root
            .input_schema
            .get("properties")
            .and_then(JsonValue::as_object)
            .is_some_and(|properties| properties.contains_key(input));
        if field_declared {
            let input_class = action_input_field_type(Some(&input_schema), input)
                .as_ref()
                .and_then(scalar_class);
            if !input_class.is_some_and(|input_class| input_comparable(input_class, class)) {
                findings.push(slice_finding(
                    artifact,
                    &format!("{path}{suffix}"),
                    format!(
                        "predicate slice {:?}: input field {input:?} cannot be compared with property {:?}",
                        entry.name,
                        property.as_str()
                    ),
                ));
            }
        }
    };
    for (property, input) in filter {
        compare(
            format!("/key/filter/{}", property.as_str()),
            property,
            input,
            EQUALITY,
        );
    }
    if let Some(window) = window {
        compare(
            "/key/window/start".to_owned(),
            &window.property,
            &window.start,
            WINDOW,
        );
        compare(
            "/key/window/end".to_owned(),
            &window.property,
            &window.end,
            WINDOW,
        );
    }
    findings
}

fn input_comparable(input: ScalarClass, property: ScalarClass) -> bool {
    match property {
        ScalarClass::Date | ScalarClass::Timestamp => input == ScalarClass::Text,
        ScalarClass::Decimal => matches!(input, ScalarClass::Decimal | ScalarClass::Integer),
        ScalarClass::Text | ScalarClass::Integer | ScalarClass::Bool => input == property,
    }
}

fn slice_finding(artifact: &ArtifactEnvelope, pointer: &str, message: String) -> CheckerDiagnostic {
    CheckerDiagnostic {
        witness: None,
        code: DiagnosticCode::new(action_codes::INVALID_SLICE),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

/// T2 validation of decoded action aggregates (#1105).
pub(crate) fn validate_action_aggregates_decoded(
    payload: &BTreeMap<String, JsonValue>,
    aggregates: &[ActionAggregateInput],
    is_guaranteed: &dyn Fn(&str) -> bool,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let code = "action.validation.invalid_payload";
    let limits = ResourceLimitProfile::norn_limits_v1().slice;
    let total_items = aggregates.iter().fold(0_u64, |total, aggregate| {
        total.saturating_add(u64::from(aggregate.max_items))
    });
    if total_items > u64::from(limits.max_records) {
        diagnostics.push(invalid_diagnostic(
            code,
            format!(
                "aggregates may match {total_items} objects in total, above the profile's slice record cap of {}",
                limits.max_records
            ),
            "payload.aggregates",
            artifact_id,
        ));
    }
    let slice_names: BTreeSet<&str> = payload
        .get("slice")
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry.as_object()?.get("name")?.as_string())
        .collect();
    let mut names = BTreeSet::new();
    for (index, aggregate) in aggregates.iter().enumerate() {
        let path = format!("payload.aggregates[{index}]");
        if !is_action_member_name(&aggregate.name)
            || slice_names.contains(aggregate.name.as_str())
            || !names.insert(aggregate.name.as_str())
        {
            diagnostics.push(invalid_diagnostic(
                code,
                format!(
                    "aggregate name {:?} is empty, repeated, or also a slice entry",
                    aggregate.name
                ),
                format!("{path}.name"),
                artifact_id,
            ));
        }
        if aggregate.max_items == 0 {
            diagnostics.push(invalid_diagnostic(
                code,
                "aggregate maxItems must be at least 1",
                format!("{path}.maxItems"),
                artifact_id,
            ));
        }
        for field in aggregate.input_fields() {
            if action_input_field_type(payload.get("inputSchema"), field).is_none() {
                diagnostics.push(invalid_diagnostic(
                    code,
                    format!("aggregate input field {field:?} is not declared in inputSchema"),
                    path.clone(),
                    artifact_id,
                ));
            } else if !is_guaranteed(field) {
                diagnostics.push(invalid_diagnostic(
                    code,
                    format!(
                        "aggregate input field {field:?} may be absent; require its final input value"
                    ),
                    path.clone(),
                    artifact_id,
                ));
            }
        }
    }
}

/// T3 validation of a property-keyed action slice referencing an earlier slice entry.
pub(crate) fn action_property_slice_findings(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    index: usize,
    entry: &SliceDeclaration,
    path: &str,
    environment: &ZenTypeEnv,
) -> Vec<CheckerDiagnostic> {
    let SliceKey::Property { from, property } = &entry.key else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    let Some(source) = root
        .slice
        .iter()
        .take(index)
        .find(|prior| prior.name == *from)
    else {
        return findings;
    };
    let pointer = format!("{path}/key/property");
    match environment
        .record(source.object_type.as_str())
        .and_then(|record| record.fields.get(property.as_str()))
        .filter(|field| matches!(field.source, FieldSource::Property { .. }))
    {
        None => findings.push(slice_finding(
            artifact,
            &pointer,
            format!(
                "slice key property {:?} is not declared on source type {:?}",
                property.as_str(),
                source.object_type.as_str()
            ),
        )),
        Some(field) if !is_action_string_key_type(&field.ty) => {
            findings.push(slice_finding(
                artifact,
                &pointer,
                format!(
                    "slice key property {:?} must be a string or enum",
                    property.as_str()
                ),
            ));
        }
        Some(field)
            if (source.optional || matches!(field.ty, Type::Optional(_))) && !entry.optional =>
        {
            findings.push(slice_finding(
                artifact,
                &pointer,
                format!(
                    "slice key property {:?} of entry {from:?} may be absent or null; mark the slice entry optional",
                    property.as_str(),
                ),
            ));
        }
        Some(_) => {}
    }
    findings
}

fn is_action_string_key_type(ty: &Type) -> bool {
    if let Type::Optional(inner) = ty {
        is_action_string_key_type(inner)
    } else {
        matches!(ty, Type::Text { .. } | Type::Enum { .. })
    }
}
