//! Closed JSON Schema subset validation shared by action and rule payloads.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{Diagnostic, JsonValue};

use super::action::{X_NORN, validate_input_groups, validate_input_labels};
use super::helpers::{invalid_diagnostic, reject_unknown_object_fields};

/// A closed schema's string `enum`: a non-empty list of distinct strings.
fn validate_string_enum(
    values: &JsonValue,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let Some(values) = values.as_array().filter(|values| !values.is_empty()) else {
        diagnostics.push(invalid_diagnostic(
            code,
            "enum must be a non-empty array of distinct strings",
            path,
            artifact_id,
        ));
        return;
    };
    let mut seen = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        match value.as_string() {
            Some(value) if seen.insert(value) => {}
            Some(value) => diagnostics.push(invalid_diagnostic(
                code,
                format!("enum value '{value}' is duplicated"),
                format!("{path}[{index}]"),
                artifact_id,
            )),
            None => diagnostics.push(invalid_diagnostic(
                code,
                "enum values of a string schema must be strings",
                format!("{path}[{index}]"),
                artifact_id,
            )),
        }
    }
}

/// The closed JSON Schema subset one IR kind admits.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClosedSchemaProfile {
    /// The diagnostic code of a violation.
    pub(crate) code: &'static str,
    /// The IR named in messages (`ActionIR`).
    pub(crate) subject: &'static str,
    /// Whether a `string` schema may carry `enum`: a non-empty list of
    /// distinct strings. `RuleIR` v2 only (T3-LIFE-2, #2673): a decide
    /// graph's fact changes enumerate the ops, object types, properties, and
    /// transitions they emit, and the evaluator validates output against the
    /// schema (T3-DEC-2). `ActionIR` inputs keep the enum-free subset the
    /// engine's input validation implements.
    pub(crate) string_enums: bool,
    /// Which precise type-array forms this profile admits.
    pub(crate) unions: ClosedSchemaUnions,
}

/// Precisely admitted closed-schema type-array forms.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ClosedSchemaUnions {
    /// One explicit type only.
    None,
    /// The declared closed non-null type or null.
    Nullable,
    /// Existing `ActionIR` input leaves: nullable primitives or bounded JSON.
    /// ACTION-ACTOR-1 also reserves actor/approved property names at every depth.
    ActionInput,
}

pub(crate) fn validate_closed_schema(
    value: Option<&JsonValue>,
    path: &str,
    depth: usize,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    profile: ClosedSchemaProfile,
) {
    let ClosedSchemaProfile { code, subject, .. } = profile;
    if depth > 32 {
        diagnostics.push(invalid_diagnostic(
            code,
            format!("closed {subject} schema nesting exceeds 32 levels"),
            path,
            artifact_id,
        ));
        return;
    }
    let Some(schema) = value.and_then(JsonValue::as_object) else {
        diagnostics.push(invalid_diagnostic(
            code,
            format!("closed {subject} schema must be an object"),
            path,
            artifact_id,
        ));
        return;
    };
    let Some(type_value) = schema.get("type") else {
        diagnostics.push(invalid_diagnostic(
            code,
            format!("closed {subject} schema requires one explicit type"),
            format!("{path}.type"),
            artifact_id,
        ));
        return;
    };
    if let Some(type_members) = type_value.as_array() {
        validate_closed_schema_type_array(
            type_members,
            schema,
            path,
            depth,
            diagnostics,
            artifact_id,
            profile,
        );
        return;
    }
    let Some(schema_type) = type_value.as_string() else {
        diagnostics.push(invalid_diagnostic(
            code,
            format!("closed {subject} schema requires one explicit type"),
            format!("{path}.type"),
            artifact_id,
        ));
        return;
    };
    if !matches!(
        schema_type,
        "object" | "array" | "string" | "integer" | "number" | "boolean" | "null"
    ) {
        diagnostics.push(invalid_diagnostic(
            code,
            format!("unsupported closed {subject} schema type '{schema_type}'"),
            format!("{path}.type"),
            artifact_id,
        ));
        return;
    }

    if let Some(schema_value) = value {
        validate_closed_schema_keywords(
            schema_value,
            schema_type,
            path,
            depth,
            diagnostics,
            artifact_id,
            profile,
        );
    }

    match schema_type {
        "object" => {
            validate_closed_object_schema(schema, path, depth, diagnostics, artifact_id, profile);
        }
        "array" => validate_closed_schema(
            schema.get("items"),
            &format!("{path}.items"),
            depth.saturating_add(1),
            diagnostics,
            artifact_id,
            profile,
        ),
        "string" => {
            if let Some(values) = schema.get("enum") {
                validate_string_enum(
                    values,
                    &format!("{path}.enum"),
                    diagnostics,
                    artifact_id,
                    code,
                );
            }
        }
        _ => {}
    }
}

fn validate_closed_schema_type_array(
    type_members: &[JsonValue],
    schema: &BTreeMap<String, JsonValue>,
    path: &str,
    depth: usize,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    profile: ClosedSchemaProfile,
) {
    match profile.unions {
        ClosedSchemaUnions::ActionInput => validate_action_type_union(
            type_members,
            schema,
            path,
            depth,
            diagnostics,
            artifact_id,
            profile,
        ),
        ClosedSchemaUnions::Nullable => validate_nullable_schema(
            type_members,
            schema,
            path,
            depth,
            diagnostics,
            artifact_id,
            profile,
        ),
        ClosedSchemaUnions::None => diagnostics.push(invalid_diagnostic(
            profile.code,
            format!(
                "closed {} schema requires one explicit type",
                profile.subject
            ),
            format!("{path}.type"),
            artifact_id,
        )),
    }
}

fn validate_closed_schema_keywords(
    value: &JsonValue,
    schema_type: &str,
    path: &str,
    depth: usize,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    profile: ClosedSchemaProfile,
) {
    let Some(schema) = value.as_object() else {
        return;
    };
    let code = profile.code;
    if let Err(error) = norn_expr::schema::decimal_refinement(value, path) {
        diagnostics.push(invalid_diagnostic(code, error.reason, path, artifact_id));
    }
    let action_input = matches!(profile.unions, ClosedSchemaUnions::ActionInput);
    let mut allowed: Vec<&str> = match schema_type {
        "number" => vec!["type", "nornType"],
        "object" => vec!["type", "properties", "required", "additionalProperties"],
        "array" => vec!["type", "items"],
        // An ActionIR input may declare a string enum; the artifact-set check
        // requires it to be a subset of the enum property it writes.
        "string"
            if profile.string_enums
                || action_input
                || profile.subject == "ActionIR"
                    && norn_expr::schema::fixed_currency_schema(value) =>
        {
            vec!["type", "enum"]
        }
        _ => vec!["type"],
    };
    if action_input {
        // Display-only labels: they change no admission semantics.
        allowed.extend(["title", "description"]);
        if schema_type == "object" && depth == 0 {
            allowed.push(X_NORN);
        }
        validate_input_labels(schema, path, diagnostics, artifact_id);
        if schema_type == "object" && depth == 0 {
            validate_input_groups(schema, path, diagnostics, artifact_id);
        }
    }
    reject_unknown_object_fields(schema, &allowed, path, diagnostics, artifact_id, code);
    if schema
        .get("description")
        .is_some_and(|description| description.as_string().is_none())
    {
        diagnostics.push(invalid_diagnostic(
            code,
            "description must be a string",
            format!("{path}.description"),
            artifact_id,
        ));
    }
}

fn validate_closed_object_schema(
    schema: &BTreeMap<String, JsonValue>,
    path: &str,
    depth: usize,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    profile: ClosedSchemaProfile,
) {
    let code = profile.code;
    let Some(properties) = schema.get("properties").and_then(JsonValue::as_object) else {
        diagnostics.push(invalid_diagnostic(
            code,
            "closed object schemas require an explicit properties object",
            format!("{path}.properties"),
            artifact_id,
        ));
        return;
    };
    if schema.get("additionalProperties") != Some(&JsonValue::Bool(false)) {
        diagnostics.push(invalid_diagnostic(
            code,
            "closed object schemas require additionalProperties=false",
            format!("{path}.additionalProperties"),
            artifact_id,
        ));
    }
    if let Some(required) = schema.get("required") {
        let Some(required) = required.as_array() else {
            diagnostics.push(invalid_diagnostic(
                code,
                "required must be an array of unique declared field names",
                format!("{path}.required"),
                artifact_id,
            ));
            return;
        };
        let mut seen = BTreeSet::new();
        for (index, field) in required.iter().enumerate() {
            let Some(field) = field.as_string().filter(|field| !field.trim().is_empty()) else {
                diagnostics.push(invalid_diagnostic(
                    code,
                    "required must contain non-empty strings",
                    format!("{path}.required[{index}]"),
                    artifact_id,
                ));
                continue;
            };
            if !seen.insert(field) {
                diagnostics.push(invalid_diagnostic(
                    code,
                    format!("required field '{field}' is duplicated"),
                    format!("{path}.required[{index}]"),
                    artifact_id,
                ));
            }
            if !properties.contains_key(field) {
                diagnostics.push(invalid_diagnostic(
                    code,
                    format!("required field '{field}' is not declared in properties"),
                    format!("{path}.required[{index}]"),
                    artifact_id,
                ));
            }
        }
    }
    for (field, field_schema) in properties {
        if matches!(profile.unions, ClosedSchemaUnions::ActionInput)
            && matches!(field.as_str(), "actor" | "approved" | "clock")
        {
            diagnostics.push(invalid_diagnostic(
                code,
                "ActionIR input reserves actor, approved, and clock for authenticated context",
                format!("{path}.properties.{field}"),
                artifact_id,
            ));
        }
        validate_closed_schema(
            Some(field_schema),
            &format!("{path}.properties.{field}"),
            depth.saturating_add(1),
            diagnostics,
            artifact_id,
            profile,
        );
    }
}

/// A nullable `RuleIR` value retains the same closed non-null shape. Arbitrary
/// unions and the `ActionIR` JSON escape leaf remain outside this profile.
fn validate_nullable_schema(
    members: &[JsonValue],
    schema: &BTreeMap<String, JsonValue>,
    path: &str,
    depth: usize,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    profile: ClosedSchemaProfile,
) {
    let names: BTreeSet<&str> = members.iter().filter_map(JsonValue::as_string).collect();
    if depth == 0 || members.len() != 2 || names.len() != 2 || !names.contains("null") {
        diagnostics.push(invalid_diagnostic(
            profile.code,
            "nullable closed schemas require one non-null type and null below the root record",
            format!("{path}.type"),
            artifact_id,
        ));
        return;
    }
    let Some(non_null) = names.into_iter().find(|name| *name != "null") else {
        return;
    };
    let mut inner = schema.clone();
    inner.insert("type".to_owned(), JsonValue::String(non_null.to_owned()));
    validate_closed_schema(
        Some(&JsonValue::Object(inner)),
        path,
        depth,
        diagnostics,
        artifact_id,
        profile,
    );
}

/// Admit only the two standard JSON Schema type arrays of `ActionIR` inputs: the
/// full `RuntimeValue` JSON domain (generated `MappingIR` inputs), which accepts
/// no sibling keyword, or a nullable primitive scalar, which is validated as its
/// non-null scalar leaf and so may carry what that leaf may: a string `enum`, a
/// decimal `nornType`, and display labels. Null is admitted besides the enum.
fn validate_action_type_union(
    members: &[JsonValue],
    schema: &BTreeMap<String, JsonValue>,
    path: &str,
    depth: usize,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    profile: ClosedSchemaProfile,
) {
    let code = profile.code;
    let names: Option<Vec<&str>> = members.iter().map(JsonValue::as_string).collect();
    let Some(names) = names else {
        diagnostics.push(invalid_diagnostic(
            code,
            "ActionIR type arrays must contain only admitted type names",
            format!("{path}.type"),
            artifact_id,
        ));
        return;
    };
    let unique: BTreeSet<&str> = names.iter().copied().collect();
    let json_domain: BTreeSet<&str> = ["null", "boolean", "number", "string", "array", "object"]
        .into_iter()
        .collect();
    let scalar = (names.len() == 2 && unique.len() == 2 && unique.contains("null"))
        .then(|| {
            unique
                .iter()
                .copied()
                .find(|name| matches!(*name, "string" | "integer" | "number" | "boolean"))
        })
        .flatten();
    if let Some(scalar) = scalar {
        let mut inner = schema.clone();
        inner.insert("type".to_owned(), JsonValue::String(scalar.to_owned()));
        validate_closed_schema(
            Some(&JsonValue::Object(inner)),
            path,
            depth,
            diagnostics,
            artifact_id,
            profile,
        );
        return;
    }
    reject_unknown_object_fields(schema, &["type"], path, diagnostics, artifact_id, code);
    if !(names.len() == json_domain.len() && unique == json_domain) {
        diagnostics.push(invalid_diagnostic(
            code,
            "ActionIR type array must be a nullable scalar or the complete JSON value domain",
            format!("{path}.type"),
            artifact_id,
        ));
    }
}
