//! Shared JSON-shape, field, and diagnostic helpers for envelope validators.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{Diagnostic, DiagnosticRemediation, DiagnosticSeverity, JsonValue};

pub(super) fn kind_invalid(
    message: &str,
    path: &str,
    artifact_id: Option<&str>,
    code: &str,
) -> Diagnostic {
    Diagnostic {
        code: code.to_string(),
        severity: DiagnosticSeverity::Error,
        message: message.to_string(),
        artifact_id: artifact_id.map(str::to_string),
        path: Some(path.to_string()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

pub(super) fn identifier_too_long(
    member: &str,
    actual_bytes: usize,
    max_bytes: usize,
    path: impl Into<String>,
    artifact_id: Option<&str>,
) -> Diagnostic {
    invalid_diagnostic(
        crate::codes::IDENTIFIER_TOO_LONG,
        format!(
            "{member} is {actual_bytes} UTF-8 bytes; the admission limit is {max_bytes}. Rename it before installing updated definitions"
        ),
        path,
        artifact_id,
    )
}
pub(super) fn validate_proposal_metadata(
    value: &JsonValue,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let path = path.into();
    let Some(proposal) = expect_object(value, path.clone(), diagnostics, artifact_id, code) else {
        return;
    };
    require_literal_field(
        proposal,
        "mode",
        &["preview"],
        format!("{path}.mode"),
        diagnostics,
        artifact_id,
        code,
    );
    require_literal_field(
        proposal,
        "proposedBy",
        &["agent", "user"],
        format!("{path}.proposedBy"),
        diagnostics,
        artifact_id,
        code,
    );
    validate_optional_non_empty_json_field(
        proposal,
        "rationale",
        format!("{path}.rationale"),
        diagnostics,
        artifact_id,
        code,
    );
    validate_optional_non_empty_json_field(
        proposal,
        "sourceArtifactId",
        format!("{path}.sourceArtifactId"),
        diagnostics,
        artifact_id,
        code,
    );
}
pub(super) fn expect_object<'a>(
    value: &'a JsonValue,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) -> Option<&'a BTreeMap<String, JsonValue>> {
    match value {
        JsonValue::Object(object) => Some(object),
        JsonValue::Null
        | JsonValue::Bool(_)
        | JsonValue::Number(_)
        | JsonValue::String(_)
        | JsonValue::Array(_) => {
            diagnostics.push(invalid_diagnostic(
                code,
                "value must be an object",
                path,
                artifact_id,
            ));
            None
        }
    }
}

pub(crate) fn require_literal_field(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    allowed: &[&str],
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) -> Option<String> {
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::String(value)) if allowed.iter().any(|allowed| value == allowed) => {
            Some(value.clone())
        }
        Some(_) => {
            diagnostics.push(invalid_diagnostic(
                code,
                format!("{path} must be one of: {}", allowed.join(", ")),
                path,
                artifact_id,
            ));
            None
        }
        None => {
            diagnostics.push(invalid_diagnostic(
                code,
                format!("{field} is required"),
                path,
                artifact_id,
            ));
            None
        }
    }
}

pub(crate) fn require_non_empty_field(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) -> Option<String> {
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        Some(_) => {
            diagnostics.push(invalid_diagnostic(
                code,
                format!("{path} must be a non-empty string"),
                path,
                artifact_id,
            ));
            None
        }
        None => {
            diagnostics.push(invalid_diagnostic(
                code,
                format!("{field} is required"),
                path,
                artifact_id,
            ));
            None
        }
    }
}

pub(super) fn is_exact_semantic_identity(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(|character| {
            character.is_whitespace() || character.is_control() || character == '*'
        })
}

pub(super) fn require_boolean_field(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::Bool(_)) => {}
        Some(_) => diagnostics.push(invalid_diagnostic(
            code,
            format!("{path} must be a boolean"),
            path,
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            code,
            format!("{field} is required"),
            path,
            artifact_id,
        )),
    }
}

pub(crate) fn validate_json_object_field(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::Object(_)) => {}
        Some(_) => diagnostics.push(invalid_diagnostic(
            code,
            format!("{path} must be an object"),
            path,
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            code,
            format!("{field} is required"),
            path,
            artifact_id,
        )),
    }
}

pub(super) fn validate_optional_non_empty_json_field(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    if !object.contains_key(field) {
        return;
    }
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::String(value)) if !value.trim().is_empty() => {}
        _ => diagnostics.push(invalid_diagnostic(
            code,
            format!("{path} must be a non-empty string"),
            path,
            artifact_id,
        )),
    }
}

pub(super) fn validate_string_array_field(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let path = path.into();
    match object.get(field) {
        Some(value) => validate_string_array(value, path, diagnostics, artifact_id, code),
        None => diagnostics.push(invalid_diagnostic(
            code,
            format!("{field} is required"),
            path,
            artifact_id,
        )),
    }
}

pub(crate) fn validate_non_empty_string_array_field(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::Array(items)) if items.is_empty() => diagnostics.push(invalid_diagnostic(
            code,
            format!("{path} must be a non-empty array"),
            path,
            artifact_id,
        )),
        Some(value) => validate_string_array(value, path, diagnostics, artifact_id, code),
        None => diagnostics.push(invalid_diagnostic(
            code,
            format!("{field} is required"),
            path,
            artifact_id,
        )),
    }
}

pub(super) fn reject_truthy_boolean_field(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    if !object.contains_key(field) {
        return;
    }
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::Bool(false)) => {}
        Some(JsonValue::Bool(true)) => diagnostics.push(invalid_diagnostic(
            code,
            format!("{path} is not allowed for customization executable logic"),
            path,
            artifact_id,
        )),
        _ => diagnostics.push(invalid_diagnostic(
            code,
            format!("{path} must be a boolean"),
            path,
            artifact_id,
        )),
    }
}

pub(super) fn validate_string_array(
    value: &JsonValue,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let path = path.into();
    match value {
        JsonValue::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                match item {
                    JsonValue::String(value) if !value.trim().is_empty() => {}
                    JsonValue::Null
                    | JsonValue::Bool(_)
                    | JsonValue::Number(_)
                    | JsonValue::String(_)
                    | JsonValue::Array(_)
                    | JsonValue::Object(_) => diagnostics.push(invalid_diagnostic(
                        code,
                        format!("{path}[{index}] must be a non-empty string"),
                        format!("{path}[{index}]"),
                        artifact_id,
                    )),
                }
            }
        }
        JsonValue::Null
        | JsonValue::Bool(_)
        | JsonValue::Number(_)
        | JsonValue::String(_)
        | JsonValue::Object(_) => diagnostics.push(invalid_diagnostic(
            code,
            format!("{path} must be an array"),
            path,
            artifact_id,
        )),
    }
}

pub(super) fn validate_unique_string_array_field(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::Array(items)) => {
            let mut seen = BTreeSet::new();
            for (index, item) in items.iter().enumerate() {
                match item {
                    JsonValue::String(value) if !value.trim().is_empty() => {
                        if !seen.insert(value) {
                            diagnostics.push(invalid_diagnostic(
                                code,
                                format!("{path}[{index}] duplicates an earlier value"),
                                format!("{path}[{index}]"),
                                artifact_id,
                            ));
                        }
                    }
                    JsonValue::Null
                    | JsonValue::Bool(_)
                    | JsonValue::Number(_)
                    | JsonValue::String(_)
                    | JsonValue::Array(_)
                    | JsonValue::Object(_) => diagnostics.push(invalid_diagnostic(
                        code,
                        format!("{path}[{index}] must be a non-empty string"),
                        format!("{path}[{index}]"),
                        artifact_id,
                    )),
                }
            }
        }
        Some(_) => diagnostics.push(invalid_diagnostic(
            code,
            format!("{path} must be an array"),
            path,
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            code,
            format!("{field} is required"),
            path,
            artifact_id,
        )),
    }
}

pub(super) fn validate_optional_non_empty(
    value: Option<&str>,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if let Some(value) = value {
        require_non_empty_string(
            value,
            path,
            diagnostics,
            artifact_id,
            "ir.validation.invalid_artifact",
        );
    }
}

pub(super) fn require_non_empty_string(
    value: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let path = path.into();
    if value.trim().is_empty() {
        diagnostics.push(invalid_diagnostic(
            code,
            format!("{path} must be a non-empty string"),
            path,
            artifact_id,
        ));
    }
}

pub(super) fn reject_unknown_object_fields(
    object: &BTreeMap<String, JsonValue>,
    allowed: &[&str],
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    for key in object.keys() {
        if !allowed.iter().any(|allowed| key == allowed) {
            diagnostics.push(invalid_diagnostic(
                code,
                format!("unknown field '{key}' is not allowed"),
                format!("{path}.{key}"),
                artifact_id,
            ));
        }
    }
}

pub(crate) fn invalid_diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    path: impl Into<String>,
    artifact_id: Option<&str>,
) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        artifact_id: artifact_id.map(str::to_string),
        path: Some(path.into()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}
