//! Derived / `ViewIR` v0 artifact payload validation.

use std::collections::BTreeSet;

use norn_ir::{
    Diagnostic, DiagnosticRemediation, DiagnosticSeverity, JsonValue, TemporalScopeMode,
};

use super::helpers::{
    expect_object, kind_invalid, require_non_empty_field, validate_non_empty_string_array_field,
};

pub(super) fn validate_derived_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "derived.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    match payload.get("viewVersion") {
        None => {
            diagnostics.push(kind_invalid(
                "payload.viewVersion is required",
                "payload.viewVersion",
                artifact_id,
                CODE,
            ));
        }
        Some(JsonValue::String(v)) if v == "0.1" => {}
        _ => {
            diagnostics.push(kind_invalid(
                "payload.viewVersion must be \"0.1\"",
                "payload.viewVersion",
                artifact_id,
                CODE,
            ));
        }
    }

    require_non_empty_field(
        payload,
        "viewId",
        "payload.viewId",
        diagnostics,
        artifact_id,
        CODE,
    );

    validate_view_inputs(payload.get("inputs"), diagnostics, artifact_id, CODE);

    validate_view_calc(payload.get("calc"), diagnostics, artifact_id, CODE);

    match payload.get("outputSchema") {
        None => {
            diagnostics.push(kind_invalid(
                "payload.outputSchema is required",
                "payload.outputSchema",
                artifact_id,
                CODE,
            ));
        }
        Some(JsonValue::Object(_)) => {}
        Some(_) => {
            diagnostics.push(kind_invalid(
                "payload.outputSchema must be an object",
                "payload.outputSchema",
                artifact_id,
                CODE,
            ));
        }
    }

    // maintenance: required; "recompute_on_read", or "materialized" for a
    // relational calc (VIEW-MAT-1); "incremental" is rejected.
    let relational = payload
        .get("calc")
        .and_then(|calc| calc.as_object())
        .and_then(|calc| calc.get("kind"))
        .and_then(|kind| kind.as_string())
        .is_some_and(|kind| kind == "relational");
    validate_view_maintenance(
        payload.get("maintenance"),
        relational,
        diagnostics,
        artifact_id,
        CODE,
    );

    // scopeRequired: optional; when present, the same fail-closed scope
    // dimensions as every other kind (T1-IR-2).
    if payload.contains_key("scopeRequired") {
        validate_non_empty_string_array_field(
            payload,
            "scopeRequired",
            "payload.scopeRequired",
            diagnostics,
            artifact_id,
            CODE,
        );
    }

    // temporal_scope: optional; if present must be a valid mode; as_of_param requires "as_of" input.
    let temporal_mode =
        validate_view_temporal_scope(payload.get("temporalScope"), diagnostics, artifact_id, CODE);
    if matches!(temporal_mode, Some(TemporalScopeMode::AsOfParam)) {
        // The read-set inputs must include an alias named "as_of".
        let has_as_of = payload
            .get("inputs")
            .and_then(JsonValue::as_array)
            .is_some_and(|arr| {
                arr.iter().any(|inp| {
                    inp.as_object()
                        .and_then(|o| o.get("alias"))
                        .and_then(JsonValue::as_string)
                        .is_some_and(|s| s == "as_of")
                })
            });
        if !has_as_of {
            diagnostics.push(kind_invalid(
                "temporal_scope.mode \"as_of_param\" requires an input with alias \"as_of\"",
                "payload.inputs",
                artifact_id,
                CODE,
            ));
        }
    }
}

fn validate_view_inputs(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    match value {
        None => {
            diagnostics.push(kind_invalid(
                "payload.inputs is required (a view with no inputs must be a setting, not a derived view)",
                "payload.inputs",
                artifact_id,
                code,
            ));
        }
        Some(JsonValue::Array(inputs)) if inputs.is_empty() => {
            diagnostics.push(kind_invalid(
                "payload.inputs must be non-empty",
                "payload.inputs",
                artifact_id,
                code,
            ));
        }
        Some(JsonValue::Array(inputs)) => {
            let mut aliases = BTreeSet::new();
            for (i, input) in inputs.iter().enumerate() {
                let path = format!("payload.inputs[{i}]");
                let Some(inp) = input.as_object() else {
                    diagnostics.push(kind_invalid(
                        &format!("{path} must be an object"),
                        &path,
                        artifact_id,
                        code,
                    ));
                    continue;
                };

                match inp.get("artifactId") {
                    Some(JsonValue::String(s)) if !s.trim().is_empty() => {}
                    _ => {
                        diagnostics.push(kind_invalid(
                            &format!("{path}.artifactId must be a non-empty string"),
                            &format!("{path}.artifactId"),
                            artifact_id,
                            code,
                        ));
                    }
                }

                match inp.get("kind") {
                    Some(JsonValue::String(k))
                        if k == "entity" || k == "relation" || k == "derived" => {}
                    _ => {
                        diagnostics.push(kind_invalid(
                            &format!(
                                "{path}.kind must be \"entity\", \"relation\", or \"derived\""
                            ),
                            &format!("{path}.kind"),
                            artifact_id,
                            code,
                        ));
                    }
                }

                match inp.get("version") {
                    Some(JsonValue::String(v)) if !v.trim().is_empty() => {}
                    _ => {
                        diagnostics.push(kind_invalid(
                            &format!(
                                "{path}.version must be a non-empty string (exact version required)"
                            ),
                            &format!("{path}.version"),
                            artifact_id,
                            code,
                        ));
                    }
                }

                match inp.get("alias") {
                    Some(JsonValue::String(alias)) if !alias.trim().is_empty() => {
                        if !aliases.insert(alias.as_str()) {
                            diagnostics.push(kind_invalid(
                                &format!("duplicate input alias \"{alias}\""),
                                &format!("{path}.alias"),
                                artifact_id,
                                code,
                            ));
                        }
                    }
                    _ => {
                        diagnostics.push(kind_invalid(
                            &format!("{path}.alias must be a non-empty string"),
                            &format!("{path}.alias"),
                            artifact_id,
                            code,
                        ));
                    }
                }
            }
        }
        Some(_) => {
            diagnostics.push(kind_invalid(
                "payload.inputs must be an array",
                "payload.inputs",
                artifact_id,
                code,
            ));
        }
    }
}

fn validate_view_calc(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    match value {
        None => {
            diagnostics.push(kind_invalid(
                "payload.calc is required",
                "payload.calc",
                artifact_id,
                code,
            ));
        }
        Some(JsonValue::Object(calc)) => {
            match calc.get("kind") {
                Some(JsonValue::String(k)) if k == "effectful_ts" => {
                    // IO inside a Derived view is rejected — fail closed.
                    diagnostics.push(Diagnostic {
                        code: "derived.validation.io_in_view".to_string(),
                        severity: DiagnosticSeverity::Error,
                        message: "calc kind \"effectful_ts\" is not allowed inside a Derived view \
                                  (IO must be in an Effect artifact)"
                            .to_string(),
                        artifact_id: artifact_id.map(str::to_string),
                        path: Some("payload.calc.kind".to_string()),
                        remediation: Some(DiagnosticRemediation::FixDefinition),
                    });
                }
                Some(JsonValue::String(k)) if k == "raw_promise" => {
                    diagnostics.push(Diagnostic {
                        code: "derived.validation.io_in_view".to_string(),
                        severity: DiagnosticSeverity::Error,
                        message: "calc kind \"raw_promise\" is never allowed; use an Effect artifact for IO"
                            .to_string(),
                        artifact_id: artifact_id.map(str::to_string),
                        path: Some("payload.calc.kind".to_string()),
                        remediation: Some(DiagnosticRemediation::FixDefinition),
                    });
                }
                Some(JsonValue::String(k)) if k == "field_projection" => match calc.get("fields") {
                    Some(JsonValue::Array(fields)) if !fields.is_empty() => {
                        for (i, f) in fields.iter().enumerate() {
                            if !matches!(f, JsonValue::String(s) if !s.trim().is_empty()) {
                                diagnostics.push(kind_invalid(
                                    &format!("payload.calc.fields[{i}] must be a non-empty string"),
                                    &format!("payload.calc.fields[{i}]"),
                                    artifact_id,
                                    code,
                                ));
                            }
                        }
                    }
                    _ => {
                        diagnostics.push(kind_invalid(
                                "payload.calc.fields must be a non-empty array for kind \"field_projection\"",
                                "payload.calc.fields",
                                artifact_id,
                                code,
                            ));
                    }
                },
                // ViewIR v1: the strict typed root reports a malformed plan
                // (`derived.validation.invalid_payload`), and the plan's
                // operators are typed against the whole artifact set
                // (`transform::check_transform_set`).
                Some(JsonValue::String(k)) if k == "relational" => {}
                _ => {
                    diagnostics.push(kind_invalid(
                        "payload.calc.kind must be \"field_projection\" or \"relational\"",
                        "payload.calc.kind",
                        artifact_id,
                        code,
                    ));
                }
            }
        }
        Some(_) => {
            diagnostics.push(kind_invalid(
                "payload.calc must be an object",
                "payload.calc",
                artifact_id,
                code,
            ));
        }
    }
}

fn validate_view_maintenance(
    value: Option<&JsonValue>,
    relational: bool,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    match value {
        None => {
            diagnostics.push(kind_invalid(
                "payload.maintenance is required",
                "payload.maintenance",
                artifact_id,
                code,
            ));
        }
        Some(JsonValue::String(m)) if m == "recompute_on_read" => {}
        Some(JsonValue::String(m)) if m == "materialized" && relational => {}
        Some(JsonValue::String(m)) if m == "incremental" => {
            diagnostics.push(Diagnostic {
                code: "derived.validation.incremental_not_supported".to_string(),
                severity: DiagnosticSeverity::Error,
                message: "maintenance \"incremental\" is reserved for a future IVM phase and is \
                          not yet supported; use \"recompute_on_read\""
                    .to_string(),
                artifact_id: artifact_id.map(str::to_string),
                path: Some("payload.maintenance".to_string()),
                remediation: Some(DiagnosticRemediation::UpgradeIr),
            });
        }
        _ => {
            diagnostics.push(kind_invalid(
                "payload.maintenance must be \"recompute_on_read\", or \"materialized\" for a relational calc",
                "payload.maintenance",
                artifact_id,
                code,
            ));
        }
    }
}

/// Returns the parsed `TemporalScopeMode` if the field is present and valid.
fn validate_view_temporal_scope(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) -> Option<TemporalScopeMode> {
    let scope = match value {
        None => return None,
        Some(JsonValue::Object(o)) => o,
        Some(_) => {
            diagnostics.push(kind_invalid(
                "payload.temporalScope must be an object",
                "payload.temporalScope",
                artifact_id,
                code,
            ));
            return None;
        }
    };

    match scope.get("mode") {
        Some(JsonValue::String(m)) if m == "current" => Some(TemporalScopeMode::Current),
        Some(JsonValue::String(m)) if m == "as_of_param" => Some(TemporalScopeMode::AsOfParam),
        _ => {
            diagnostics.push(kind_invalid(
                "payload.temporalScope.mode must be \"current\" or \"as_of_param\"",
                "payload.temporalScope.mode",
                artifact_id,
                code,
            ));
            None
        }
    }
}
