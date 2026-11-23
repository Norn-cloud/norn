//! Rule artifact payload validation (modern decision graphs and legacy tables).

use std::collections::BTreeMap;

use norn_ir::{ArtifactDependency, Diagnostic, JsonValue};

use super::helpers::{
    expect_object, invalid_diagnostic, require_literal_field, require_non_empty_field,
    validate_json_object_field, validate_non_empty_string_array_field,
};

pub(super) fn validate_rule_payload(
    value: &JsonValue,
    dependencies: &[ArtifactDependency],
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "rule.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    if crate::typed_roots::is_rule_ir_v2_payload(payload) {
        crate::typed_roots::validate_rule_ir_v2_payload(
            payload,
            dependencies,
            diagnostics,
            artifact_id,
        );
    } else if is_modern_rule_payload(payload) {
        validate_modern_rule_payload(payload, diagnostics, artifact_id);
        crate::typed_roots::reject_rule_ir_v2_members_in_v1(payload, diagnostics, artifact_id);
    } else {
        validate_legacy_rule_payload(payload, diagnostics, artifact_id);
        crate::typed_roots::reject_rule_ir_v2_members_in_v1(payload, diagnostics, artifact_id);
    }
}

fn is_modern_rule_payload(payload: &BTreeMap<String, JsonValue>) -> bool {
    payload.contains_key("ruleVersion")
        || payload.contains_key("inputSchema")
        || payload.contains_key("outputSchema")
        || payload.contains_key("decision")
}

fn validate_modern_rule_payload(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "rule.validation.invalid_payload";
    require_literal_field(
        payload,
        "ruleVersion",
        &["0.1"],
        "payload.ruleVersion",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_non_empty_field(
        payload,
        "ruleId",
        "payload.ruleId",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_json_object_field(
        payload,
        "inputSchema",
        "payload.inputSchema",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_json_object_field(
        payload,
        "outputSchema",
        "payload.outputSchema",
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
    validate_rule_decision(payload.get("decision"), diagnostics, artifact_id);
    reject_rule_side_effect_fields(payload, diagnostics, artifact_id);
}

fn validate_legacy_rule_payload(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "rule.validation.invalid_payload";
    require_non_empty_field(
        payload,
        "ruleId",
        "payload.ruleId",
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
        "expression",
        "payload.expression",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_literal_field(
        payload,
        "severity",
        &["info", "warning", "error"],
        "payload.severity",
        diagnostics,
        artifact_id,
        CODE,
    );
    reject_rule_side_effect_fields(payload, diagnostics, artifact_id);
}

fn validate_rule_decision(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "rule.validation.invalid_payload";
    let Some(value) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "decision is required",
            "payload.decision",
            artifact_id,
        ));
        return;
    };
    let Some(decision) = expect_object(value, "payload.decision", diagnostics, artifact_id, CODE)
    else {
        return;
    };

    let Some(kind) = require_literal_field(
        decision,
        "kind",
        &["literal", "decision_table"],
        "payload.decision.kind",
        diagnostics,
        artifact_id,
        CODE,
    ) else {
        return;
    };

    match kind.as_str() {
        "literal" => {
            if !decision.contains_key("value") {
                diagnostics.push(invalid_diagnostic(
                    CODE,
                    "value is required",
                    "payload.decision.value",
                    artifact_id,
                ));
            }
        }
        "decision_table" => validate_rule_decision_table(decision, diagnostics, artifact_id),
        _ => {}
    }
}

fn validate_rule_decision_table(
    decision: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "rule.validation.invalid_payload";
    require_literal_field(
        decision,
        "hitPolicy",
        &["first", "unique"],
        "payload.decision.hitPolicy",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_rule_inputs(decision.get("inputs"), diagnostics, artifact_id);
    match decision.get("rows") {
        Some(JsonValue::Array(rows)) if !rows.is_empty() => {
            for (index, row) in rows.iter().enumerate() {
                let path = format!("payload.decision.rows[{index}]");
                let Some(row) = expect_object(row, path.clone(), diagnostics, artifact_id, CODE)
                else {
                    continue;
                };
                require_non_empty_field(
                    row,
                    "rowId",
                    format!("{path}.rowId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_rule_conditions(
                    row.get("when"),
                    format!("{path}.when"),
                    diagnostics,
                    artifact_id,
                );
                if !row.contains_key("output") {
                    diagnostics.push(invalid_diagnostic(
                        CODE,
                        "output is required",
                        format!("{path}.output"),
                        artifact_id,
                    ));
                }
            }
        }
        Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "rows must be a non-empty array",
            "payload.decision.rows",
            artifact_id,
        )),
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "rows must be an array",
            "payload.decision.rows",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "rows is required",
            "payload.decision.rows",
            artifact_id,
        )),
    }
}

fn validate_rule_inputs(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "rule.validation.invalid_payload";
    match value {
        Some(JsonValue::Array(inputs)) if !inputs.is_empty() => {
            for (index, input) in inputs.iter().enumerate() {
                let path = format!("payload.decision.inputs[{index}]");
                let Some(input) =
                    expect_object(input, path.clone(), diagnostics, artifact_id, CODE)
                else {
                    continue;
                };
                require_non_empty_field(
                    input,
                    "field",
                    format!("{path}.field"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_literal_field(
                    input,
                    "type",
                    &["string", "number", "boolean", "json"],
                    format!("{path}.type"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
        }
        Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "inputs must be a non-empty array",
            "payload.decision.inputs",
            artifact_id,
        )),
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "inputs must be an array",
            "payload.decision.inputs",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "inputs is required",
            "payload.decision.inputs",
            artifact_id,
        )),
    }
}

fn validate_rule_conditions(
    value: Option<&JsonValue>,
    path: String,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "rule.validation.invalid_payload";
    match value {
        Some(JsonValue::Array(conditions)) => {
            for (index, condition) in conditions.iter().enumerate() {
                let condition_path = format!("{path}[{index}]");
                let Some(condition) = expect_object(
                    condition,
                    condition_path.clone(),
                    diagnostics,
                    artifact_id,
                    CODE,
                ) else {
                    continue;
                };
                require_non_empty_field(
                    condition,
                    "field",
                    format!("{condition_path}.field"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                let op = require_literal_field(
                    condition,
                    "op",
                    &[
                        "exists", "eq", "neq", "lt", "lte", "gt", "gte", "in", "between",
                    ],
                    format!("{condition_path}.op"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                match op.as_deref() {
                    Some("eq" | "neq" | "lt" | "lte" | "gt" | "gte")
                        if !condition.contains_key("value") =>
                    {
                        diagnostics.push(invalid_diagnostic(
                            CODE,
                            "value is required",
                            format!("{condition_path}.value"),
                            artifact_id,
                        ));
                    }
                    Some("in") => match condition.get("values") {
                        Some(JsonValue::Array(_)) => {}
                        _ => diagnostics.push(invalid_diagnostic(
                            CODE,
                            "values must be an array",
                            format!("{condition_path}.values"),
                            artifact_id,
                        )),
                    },
                    Some("between") => {
                        let min = match condition.get("min") {
                            Some(JsonValue::Number(value)) => Some(*value),
                            _ => None,
                        };
                        let max = match condition.get("max") {
                            Some(JsonValue::Number(value)) => Some(*value),
                            _ => None,
                        };
                        if match (min, max) {
                            (Some(min), Some(max)) => min > max,
                            _ => true,
                        } {
                            diagnostics.push(invalid_diagnostic(
                                CODE,
                                "min and max must be ordered numbers",
                                format!("{condition_path}.min"),
                                artifact_id,
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "when must be an array",
            path,
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "when is required",
            path,
            artifact_id,
        )),
    }
}

pub(crate) fn reject_rule_side_effect_fields(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "rule.validation.invalid_payload";
    for field in [
        "sideEffects",
        "directDatabaseAccess",
        "ambientCredentials",
        "networkAccess",
        "storageAccess",
    ] {
        // `sideEffects` is a declared member typed as the literal `false`
        // (T1-IR-2); the others are unknown fields, rejected here when set.
        let rejected = match payload.get(field) {
            None => false,
            Some(value) if field == "sideEffects" => !matches!(value, JsonValue::Bool(false)),
            Some(value) => matches!(value, JsonValue::Bool(true)),
        };
        if rejected {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("{field} is not allowed in deterministic RuleIR"),
                format!("payload.{field}"),
                artifact_id,
            ));
        }
    }

    // `requiredServices` is typed as the empty list (T1-IR-2).
    if payload.get("requiredServices").is_some_and(
        |services| !matches!(services, JsonValue::Array(services) if services.is_empty()),
    ) {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "requiredServices are not allowed in deterministic RuleIR",
            "payload.requiredServices",
            artifact_id,
        ));
    }
}
