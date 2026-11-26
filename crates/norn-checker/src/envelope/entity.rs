//! Entity artifact payload and mutation-authority validation.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::identifier_limits::MAX_OBJECT_TYPE_NAME_BYTES;
use norn_ir::{Diagnostic, JsonValue};

use super::helpers::{
    expect_object, identifier_too_long, is_exact_semantic_identity, kind_invalid,
    reject_unknown_object_fields, require_boolean_field, require_literal_field,
    require_non_empty_field, validate_string_array,
};

pub(super) fn validate_entity_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "entity.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };
    // `entityVersion` selects the EntityIR v2 form (T1-IR-1b); without it the
    // payload is EntityIR v1, checked below exactly as before.
    if crate::typed_roots::is_entity_ir_v2_payload(payload) {
        crate::typed_roots::validate_entity_ir_v2_payload(payload, diagnostics, artifact_id);
        return;
    }
    reject_unknown_object_fields(
        payload,
        &[
            "objectKind",
            "fields",
            "mutationAuthority",
            "overlaySources",
        ],
        "payload",
        diagnostics,
        artifact_id,
        CODE,
    );

    if let Some(object_kind) = require_non_empty_field(
        payload,
        "objectKind",
        "payload.objectKind",
        diagnostics,
        artifact_id,
        CODE,
    ) && !is_exact_semantic_identity(&object_kind)
    {
        diagnostics.push(kind_invalid(
            "payload.objectKind must be an exact identity without whitespace, control characters, or wildcards",
            "payload.objectKind",
            artifact_id,
            CODE,
        ));
    }
    if let Some(JsonValue::String(object_kind)) = payload.get("objectKind")
        && object_kind.len() > MAX_OBJECT_TYPE_NAME_BYTES
    {
        diagnostics.push(identifier_too_long(
            "object type name",
            object_kind.len(),
            MAX_OBJECT_TYPE_NAME_BYTES,
            "payload.objectKind",
            artifact_id,
        ));
    }

    if let Some(authority) = payload.get("mutationAuthority") {
        validate_entity_mutation_authority(authority, diagnostics, artifact_id);
    }

    // `overlaySources` is engine-written provenance (`bac.rs` `add_field`):
    // the customization artifact ids applied, possibly none (T1-IR-2).
    if let Some(overlay_sources) = payload.get("overlaySources") {
        validate_string_array(
            overlay_sources,
            "payload.overlaySources",
            diagnostics,
            artifact_id,
            CODE,
        );
    }

    // `fields` is required and must be an object (field-name → field-def map).
    match payload.get("fields") {
        None => {
            diagnostics.push(kind_invalid(
                "payload.fields is required for entity artifacts",
                "payload.fields",
                artifact_id,
                CODE,
            ));
        }
        Some(JsonValue::Object(fields)) => {
            validate_entity_fields(fields, diagnostics, artifact_id, CODE);
        }
        Some(_) => {
            diagnostics.push(kind_invalid(
                "payload.fields must be an object",
                "payload.fields",
                artifact_id,
                CODE,
            ));
        }
    }
}

fn validate_entity_fields(
    fields: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    for (field_name, field_definition) in fields {
        let field_path = format!("payload.fields.{field_name}");
        if field_name.trim().is_empty() {
            diagnostics.push(kind_invalid(
                "Entity field names must be non-empty",
                &field_path,
                artifact_id,
                code,
            ));
        }
        let Some(field_definition) = expect_object(
            field_definition,
            &field_path,
            diagnostics,
            artifact_id,
            code,
        ) else {
            continue;
        };
        let field_type = require_literal_field(
            field_definition,
            "type",
            &[
                "text", "string", "integer", "number", "boolean", "object", "array", "enum",
            ],
            format!("{field_path}.type"),
            diagnostics,
            artifact_id,
            code,
        );
        let allowed_fields = if field_type.as_deref() == Some("enum") {
            &["type", "required", "values"][..]
        } else {
            &["type", "required"][..]
        };
        reject_unknown_object_fields(
            field_definition,
            allowed_fields,
            &field_path,
            diagnostics,
            artifact_id,
            code,
        );
        if field_definition.contains_key("required") {
            require_boolean_field(
                field_definition,
                "required",
                format!("{field_path}.required"),
                diagnostics,
                artifact_id,
                code,
            );
        }
        if field_type.as_deref() == Some("enum") {
            validate_entity_enum_values(
                field_definition.get("values"),
                &field_path,
                diagnostics,
                artifact_id,
                code,
            );
        }
    }
}

fn validate_entity_enum_values(
    values: Option<&JsonValue>,
    field_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    match values {
        Some(JsonValue::Array(values)) if !values.is_empty() => {
            let mut seen = BTreeSet::new();
            for (index, value) in values.iter().enumerate() {
                let value_path = format!("{field_path}.values[{index}]");
                match value {
                    JsonValue::String(value) if !value.trim().is_empty() => {
                        if !seen.insert(value.as_str()) {
                            diagnostics.push(kind_invalid(
                                "enum values must be unique",
                                &value_path,
                                artifact_id,
                                code,
                            ));
                        }
                    }
                    JsonValue::Null
                    | JsonValue::Bool(_)
                    | JsonValue::Number(_)
                    | JsonValue::String(_)
                    | JsonValue::Array(_)
                    | JsonValue::Object(_) => diagnostics.push(kind_invalid(
                        "enum values must be non-empty strings",
                        &value_path,
                        artifact_id,
                        code,
                    )),
                }
            }
        }
        _ => diagnostics.push(kind_invalid(
            "enum fields require a non-empty values array",
            &format!("{field_path}.values"),
            artifact_id,
            code,
        )),
    }
}

pub(crate) fn validate_entity_mutation_authority(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "entity.validation.invalid_mutation_authority";
    let Some(authority) = expect_object(
        value,
        "payload.mutationAuthority",
        diagnostics,
        artifact_id,
        CODE,
    ) else {
        return;
    };
    let kind = require_literal_field(
        authority,
        "kind",
        &["generic_action", "domain_engine"],
        "payload.mutationAuthority.kind",
        diagnostics,
        artifact_id,
        CODE,
    );
    let allowed = if kind.as_deref() == Some("domain_engine") {
        &["kind", "domain"][..]
    } else {
        &["kind"][..]
    };
    reject_unknown_object_fields(
        authority,
        allowed,
        "payload.mutationAuthority",
        diagnostics,
        artifact_id,
        CODE,
    );
    if kind.as_deref() == Some("domain_engine") {
        require_literal_field(
            authority,
            "domain",
            &["inventory", "accounting", "compliance", "settlement", "tax"],
            "payload.mutationAuthority.domain",
            diagnostics,
            artifact_id,
            CODE,
        );
    }
}
