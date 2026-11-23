//! Customization artifact payload validation.

use std::collections::BTreeMap;

use norn_ir::{Diagnostic, JsonValue};

use super::helpers::{
    expect_object, invalid_diagnostic, reject_truthy_boolean_field, require_boolean_field,
    require_literal_field, require_non_empty_field, validate_json_object_field,
    validate_non_empty_string_array_field, validate_optional_non_empty_json_field,
    validate_string_array_field,
};

pub(super) fn validate_customization_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "customization.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    require_literal_field(
        payload,
        "customizationVersion",
        &["0.1"],
        "payload.customizationVersion",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_non_empty_field(
        payload,
        "customizationId",
        "payload.customizationId",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_customization_intent(payload.get("intent"), diagnostics, artifact_id);
    validate_customization_target(payload.get("targetArtifact"), diagnostics, artifact_id);
    let requires_executable =
        validate_customization_overlay(payload.get("overlay"), diagnostics, artifact_id);
    validate_customization_approval(payload.get("approval"), diagnostics, artifact_id);
    validate_non_empty_string_array_field(
        payload,
        "scopeRequired",
        "payload.scopeRequired",
        diagnostics,
        artifact_id,
        CODE,
    );

    match payload.get("executable") {
        Some(executable) => {
            validate_customization_executable(executable, diagnostics, artifact_id);
        }
        None if requires_executable => diagnostics.push(invalid_diagnostic(
            CODE,
            "custom hook overlays require an executable boundary declaration",
            "payload.executable",
            artifact_id,
        )),
        None => {}
    }
}

fn validate_customization_intent(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "customization.validation.invalid_payload";
    let Some(value) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "intent is required",
            "payload.intent",
            artifact_id,
        ));
        return;
    };
    let Some(intent) = expect_object(value, "payload.intent", diagnostics, artifact_id, CODE)
    else {
        return;
    };
    require_non_empty_field(
        intent,
        "summary",
        "payload.intent.summary",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_literal_field(
        intent,
        "requestedBy",
        &["user", "agent"],
        "payload.intent.requestedBy",
        diagnostics,
        artifact_id,
        CODE,
    );
}

fn validate_customization_target(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "customization.validation.invalid_payload";
    let Some(value) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "targetArtifact is required",
            "payload.targetArtifact",
            artifact_id,
        ));
        return;
    };
    let Some(target) = expect_object(
        value,
        "payload.targetArtifact",
        diagnostics,
        artifact_id,
        CODE,
    ) else {
        return;
    };
    require_non_empty_field(
        target,
        "artifactId",
        "payload.targetArtifact.artifactId",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_literal_field(
        target,
        "kind",
        &[
            "entity",
            "relation",
            "action",
            "rule",
            "connector",
            "widget",
            "customization",
            "business_pack",
            "policy",
        ],
        "payload.targetArtifact.kind",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_non_empty_field(
        target,
        "version",
        "payload.targetArtifact.version",
        diagnostics,
        artifact_id,
        CODE,
    );
}

fn validate_customization_overlay(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> bool {
    const CODE: &str = "customization.validation.invalid_payload";
    let Some(value) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "overlay is required",
            "payload.overlay",
            artifact_id,
        ));
        return false;
    };
    let Some(overlay) = expect_object(value, "payload.overlay", diagnostics, artifact_id, CODE)
    else {
        return false;
    };
    require_literal_field(
        overlay,
        "kind",
        &[
            "field",
            "validation",
            "widget",
            "connector",
            "rule",
            "policy",
            "pack",
        ],
        "payload.overlay.kind",
        diagnostics,
        artifact_id,
        CODE,
    );

    let mut requires_executable = false;
    match overlay.get("operations") {
        Some(JsonValue::Array(operations)) if !operations.is_empty() => {
            for (index, operation) in operations.iter().enumerate() {
                if validate_customization_operation(
                    operation,
                    index,
                    diagnostics,
                    artifact_id,
                    CODE,
                ) {
                    requires_executable = true;
                }
            }
        }
        Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "overlay operations must be a non-empty array",
            "payload.overlay.operations",
            artifact_id,
        )),
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "overlay operations must be an array",
            "payload.overlay.operations",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "operations is required",
            "payload.overlay.operations",
            artifact_id,
        )),
    }

    requires_executable
}

fn validate_customization_operation(
    operation: &JsonValue,
    index: usize,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) -> bool {
    let path = format!("payload.overlay.operations[{index}]");
    let Some(operation) = expect_object(operation, path.clone(), diagnostics, artifact_id, code)
    else {
        return false;
    };
    let operation_kind = require_literal_field(
        operation,
        "op",
        &[
            "add_field",
            "update_field",
            "remove_field",
            "add_validation",
            "update_validation",
            "add_widget_binding",
            "custom_hook",
        ],
        format!("{path}.op"),
        diagnostics,
        artifact_id,
        code,
    );
    match operation_kind.as_deref() {
        Some("add_field" | "update_field") => {
            require_non_empty_field(
                operation,
                "field",
                format!("{path}.field"),
                diagnostics,
                artifact_id,
                code,
            );
            validate_json_object_field(
                operation,
                "definition",
                format!("{path}.definition"),
                diagnostics,
                artifact_id,
                code,
            );
            false
        }
        Some("custom_hook") => true,
        _ => false,
    }
}

fn validate_customization_approval(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "customization.validation.invalid_payload";
    let Some(value) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "approval is required",
            "payload.approval",
            artifact_id,
        ));
        return;
    };
    let Some(approval) = expect_object(value, "payload.approval", diagnostics, artifact_id, CODE)
    else {
        return;
    };
    require_literal_field(
        approval,
        "state",
        &["draft", "proposed", "approved", "active", "rejected"],
        "payload.approval.state",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_boolean_field(
        approval,
        "required",
        "payload.approval.required",
        diagnostics,
        artifact_id,
        CODE,
    );
}

/// Executable boundaries retired by the rulings: there is no server-side
/// custom TypeScript runtime (2026-09-25 §11), and inline custom execution is
/// rejected in v1 (2026-09-27 compute boundary §1/§2, D33). Custom code is a
/// pool Function invoked through `perform` (T1-FN-1, #2776).
const RETIRED_EXECUTABLE_BOUNDARIES: &[&str] = &["effectful_ts", "wasm"];

fn validate_customization_executable(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "customization.validation.invalid_payload";
    let Some(executable) =
        expect_object(value, "payload.executable", diagnostics, artifact_id, CODE)
    else {
        return;
    };
    validate_customization_boundary(executable, diagnostics, artifact_id, CODE);
    for field in ["inputSchema", "outputSchema", "errorSchema"] {
        validate_json_object_field(
            executable,
            field,
            format!("payload.executable.{field}"),
            diagnostics,
            artifact_id,
            CODE,
        );
    }
    validate_non_empty_string_array_field(
        executable,
        "scopeCapabilities",
        "payload.executable.scopeCapabilities",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_string_array_field(
        executable,
        "requiredServices",
        "payload.executable.requiredServices",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_customization_resource_limits(
        executable.get("resourceLimits"),
        diagnostics,
        artifact_id,
    );
    for field in ["ambientCredentials", "directDatabaseAccess"] {
        reject_truthy_boolean_field(
            executable,
            field,
            format!("payload.executable.{field}"),
            diagnostics,
            artifact_id,
            CODE,
        );
    }
    validate_optional_non_empty_json_field(
        executable,
        "sourceRef",
        "payload.executable.sourceRef",
        diagnostics,
        artifact_id,
        CODE,
    );
}

fn validate_customization_boundary(
    executable: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    const RETIRED_BOUNDARY_CODE: &str = "customization.validation.retired_boundary";
    match executable.get("boundary") {
        Some(JsonValue::String(boundary))
            if RETIRED_EXECUTABLE_BOUNDARIES.contains(&boundary.as_str()) =>
        {
            diagnostics.push(invalid_diagnostic(
                RETIRED_BOUNDARY_CODE,
                format!(
                    "executable boundary \"{boundary}\" is retired: inline custom execution is \
                     not admitted; move the code into a pool Function invoked through perform"
                ),
                "payload.executable.boundary",
                artifact_id,
            ));
        }
        _ => {
            require_literal_field(
                executable,
                "boundary",
                &["trusted_rust", "capability_rpc"],
                "payload.executable.boundary",
                diagnostics,
                artifact_id,
                code,
            );
        }
    }
}

fn validate_customization_resource_limits(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "customization.validation.invalid_payload";
    let Some(value) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "resourceLimits is required",
            "payload.executable.resourceLimits",
            artifact_id,
        ));
        return;
    };
    let Some(limits) = expect_object(
        value,
        "payload.executable.resourceLimits",
        diagnostics,
        artifact_id,
        CODE,
    ) else {
        return;
    };
    validate_integer_field(
        limits,
        "maxWallTimeMs",
        1.0,
        "payload.executable.resourceLimits.maxWallTimeMs",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_integer_field(
        limits,
        "maxMemoryMb",
        1.0,
        "payload.executable.resourceLimits.maxMemoryMb",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_integer_field(
        limits,
        "maxNetworkCalls",
        0.0,
        "payload.executable.resourceLimits.maxNetworkCalls",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_integer_field(
        limits,
        "maxStorageBytes",
        0.0,
        "payload.executable.resourceLimits.maxStorageBytes",
        diagnostics,
        artifact_id,
        CODE,
    );
}

fn validate_integer_field(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    minimum: f64,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::Number(value)) if value.fract() == 0.0 && *value >= minimum => {}
        Some(_) => diagnostics.push(invalid_diagnostic(
            code,
            format!("{path} must be an integer greater than or equal to {minimum:.0}"),
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
