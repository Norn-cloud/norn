//! Setting artifact payload validation.

use norn_ir::{Diagnostic, JsonValue};

use super::helpers::{
    expect_object, invalid_diagnostic, require_literal_field, require_non_empty_field,
    validate_non_empty_string_array_field,
};
use super::validate_json_value;

pub(super) fn validate_setting_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "setting.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    require_literal_field(
        payload,
        "settingVersion",
        &["0.1"],
        "payload.settingVersion",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_non_empty_field(
        payload,
        "settingKey",
        "payload.settingKey",
        diagnostics,
        artifact_id,
        CODE,
    );
    let value_type = require_literal_field(
        payload,
        "valueType",
        &["text", "number", "boolean", "json"],
        "payload.valueType",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_setting_typed_value(
        payload.get("value"),
        value_type.as_deref(),
        diagnostics,
        artifact_id,
    );
    let effective_from = require_non_empty_field(
        payload,
        "effectiveFrom",
        "payload.effectiveFrom",
        diagnostics,
        artifact_id,
        CODE,
    );
    if let Some(effective_to) = payload.get("effectiveTo") {
        match effective_to {
            JsonValue::String(value) if !value.trim().is_empty() => {
                if let Some(effective_from) = effective_from.as_deref()
                    && value.as_str() <= effective_from
                {
                    diagnostics.push(invalid_diagnostic(
                        CODE,
                        "payload.effectiveTo must be after payload.effectiveFrom",
                        "payload.effectiveTo",
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
                CODE,
                "payload.effectiveTo must be a non-empty string",
                "payload.effectiveTo",
                artifact_id,
            )),
        }
    }
    validate_setting_supersedes(payload.get("supersedes"), diagnostics, artifact_id);
    validate_setting_review(payload.get("review"), diagnostics, artifact_id);
    validate_non_empty_string_array_field(
        payload,
        "scopeRequired",
        "payload.scopeRequired",
        diagnostics,
        artifact_id,
        CODE,
    );
}

fn validate_setting_typed_value(
    value: Option<&JsonValue>,
    value_type: Option<&str>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "setting.validation.invalid_payload";
    match (value_type, value) {
        (Some("text"), Some(JsonValue::String(_)))
        | (Some("boolean"), Some(JsonValue::Bool(_))) => {}
        (Some("number"), Some(JsonValue::String(value))) => {
            if let Err(error) = norn_ir::SettingValue::decimal(value) {
                diagnostics.push(invalid_diagnostic(
                    "core.values.inexact",
                    error.to_string(),
                    "payload.value",
                    artifact_id,
                ));
            }
        }
        (Some("json"), Some(value)) => {
            validate_json_value(value, "payload.value", diagnostics, artifact_id);
        }
        (Some("text"), Some(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "payload.value must be a string for text settings",
            "payload.value",
            artifact_id,
        )),
        (Some("number"), Some(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "payload.value must be an exact decimal string for number settings",
            "payload.value",
            artifact_id,
        )),
        (Some("boolean"), Some(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "payload.value must be a boolean for boolean settings",
            "payload.value",
            artifact_id,
        )),
        (_, None) => diagnostics.push(invalid_diagnostic(
            CODE,
            "value is required",
            "payload.value",
            artifact_id,
        )),
        _ => {}
    }
}

fn validate_setting_supersedes(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "setting.validation.invalid_payload";
    let Some(value) = value else {
        return;
    };
    let Some(object) = expect_object(value, "payload.supersedes", diagnostics, artifact_id, CODE)
    else {
        return;
    };
    require_non_empty_field(
        object,
        "artifactId",
        "payload.supersedes.artifactId",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_non_empty_field(
        object,
        "version",
        "payload.supersedes.version",
        diagnostics,
        artifact_id,
        CODE,
    );
}

fn validate_setting_review(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "setting.validation.invalid_payload";
    let Some(value) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "review is required",
            "payload.review",
            artifact_id,
        ));
        return;
    };
    let Some(object) = expect_object(value, "payload.review", diagnostics, artifact_id, CODE)
    else {
        return;
    };
    require_literal_field(
        object,
        "state",
        &[
            "draft",
            "proposed",
            "approved",
            "active",
            "rejected",
            "superseded",
        ],
        "payload.review.state",
        diagnostics,
        artifact_id,
        CODE,
    );
    for field in ["reviewedBy", "reviewedAt", "reason"] {
        if object.contains_key(field) {
            require_non_empty_field(
                object,
                field,
                format!("payload.review.{field}"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
    }
}
