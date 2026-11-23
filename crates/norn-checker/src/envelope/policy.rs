//! Policy artifact payload validation (modern and legacy forms).

use std::collections::BTreeMap;

use norn_ir::{Diagnostic, JsonValue};

use super::helpers::{
    expect_object, invalid_diagnostic, require_literal_field, require_non_empty_field,
    validate_non_empty_string_array_field, validate_string_array_field,
};

pub(super) fn validate_policy_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "policy.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    if is_access_policy_payload(payload) {
        // AccessIR (#2879): its shape is checked by the strict typed root
        // (`typed_roots`); compilation to Cedar against the projected schema
        // happens at admission, in `norn-authz`.
    } else if is_modern_policy_payload(payload) {
        validate_modern_policy_payload(payload, diagnostics, artifact_id);
    } else {
        validate_legacy_policy_payload(payload, diagnostics, artifact_id);
    }
}

/// The `AccessIR` form of a `policy` payload (#2879), selected by
/// `accessVersion`.
pub(crate) fn is_access_policy_payload(payload: &BTreeMap<String, JsonValue>) -> bool {
    payload.contains_key("accessVersion")
}

pub(crate) fn is_modern_policy_payload(payload: &BTreeMap<String, JsonValue>) -> bool {
    payload.contains_key("policyVersion")
        || payload.contains_key("obligations")
        || payload.contains_key("scopeRequired")
}

fn validate_modern_policy_payload(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "policy.validation.invalid_payload";
    require_literal_field(
        payload,
        "policyVersion",
        &["0.1"],
        "payload.policyVersion",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_non_empty_field(
        payload,
        "policyId",
        "payload.policyId",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_non_empty_string_array_field(
        payload,
        "scopeRequired",
        "payload.scopeRequired",
        diagnostics,
        artifact_id,
        CODE,
    );

    match payload.get("obligations") {
        Some(JsonValue::Array(obligations)) if !obligations.is_empty() => {
            validate_policy_obligations(obligations, diagnostics, artifact_id, CODE);
        }
        Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "obligations must be a non-empty array",
            "payload.obligations",
            artifact_id,
        )),
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "obligations must be an array",
            "payload.obligations",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "obligations is required",
            "payload.obligations",
            artifact_id,
        )),
    }

    // The approval-form members are optional here but keep their approval-form
    // types when present (T1-IR-2).
    for field in ["appliesTo", "reviewRequiredWhen"] {
        if payload.contains_key(field) {
            require_non_empty_field(
                payload,
                field,
                format!("payload.{field}"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
    }
    if payload.contains_key("approvalRoles") {
        validate_non_empty_string_array_field(
            payload,
            "approvalRoles",
            "payload.approvalRoles",
            diagnostics,
            artifact_id,
            CODE,
        );
    }
}

fn validate_policy_obligations(
    obligations: &[JsonValue],
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    for (index, obligation) in obligations.iter().enumerate() {
        let path = format!("payload.obligations[{index}]");
        let Some(obligation) =
            expect_object(obligation, path.clone(), diagnostics, artifact_id, code)
        else {
            continue;
        };
        require_non_empty_field(
            obligation,
            "obligationId",
            format!("{path}.obligationId"),
            diagnostics,
            artifact_id,
            code,
        );
        require_non_empty_field(
            obligation,
            "appliesTo",
            format!("{path}.appliesTo"),
            diagnostics,
            artifact_id,
            code,
        );
        validate_string_array_field(
            obligation,
            "evidenceRequired",
            format!("{path}.evidenceRequired"),
            diagnostics,
            artifact_id,
            code,
        );
        validate_string_array_field(
            obligation,
            "reviewRoles",
            format!("{path}.reviewRoles"),
            diagnostics,
            artifact_id,
            code,
        );
    }
}

fn validate_legacy_policy_payload(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "policy.validation.invalid_payload";
    require_non_empty_field(
        payload,
        "policyId",
        "payload.policyId",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_non_empty_field(
        payload,
        "appliesTo",
        "payload.appliesTo",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_non_empty_field(
        payload,
        "reviewRequiredWhen",
        "payload.reviewRequiredWhen",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_non_empty_string_array_field(
        payload,
        "approvalRoles",
        "payload.approvalRoles",
        diagnostics,
        artifact_id,
        CODE,
    );
}
