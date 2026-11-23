//! Widget artifact payload and dependency-binding validation.

use std::collections::BTreeSet;

use norn_ir::{
    ArtifactEnvelope, ArtifactKind, Diagnostic, DiagnosticRemediation, DiagnosticSeverity,
    JsonValue,
};

use norn_checker::envelope::helpers::{
    expect_object, invalid_diagnostic, require_literal_field, require_non_empty_field,
    validate_optional_non_empty_json_field, validate_proposal_metadata,
    validate_string_array_field,
};

/// Payload-validator hook for [`norn_checker::check_artifact_with`] and
/// [`norn_checker::validate_artifact_with`]: validates `ArtifactPayload::Widget`
/// envelopes before typed-root agreement.
pub fn validate_widget_extra(
    artifact: &ArtifactEnvelope,
    payload: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if matches!(artifact.payload, norn_ir::ArtifactPayload::Widget(_)) {
        validate_widget_payload(payload, diagnostics, Some(artifact.artifact_id.as_str()));
        validate_widget_binding_dependencies(artifact, diagnostics);
    }
}

/// Validate a widget artifact's JSON payload fields.
pub fn validate_widget_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "widget.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    require_literal_field(
        payload,
        "widgetVersion",
        &["0.1"],
        "payload.widgetVersion",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_non_empty_field(
        payload,
        "widgetId",
        "payload.widgetId",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_widget_data_dependencies(payload.get("dataDependencies"), diagnostics, artifact_id);
    validate_widget_view_states(payload.get("viewStates"), diagnostics, artifact_id);
    validate_widget_action_bindings(payload.get("actionBindings"), diagnostics, artifact_id);
    if payload.contains_key("ux") {
        for field in ["dataDependencies", "actionBindings"] {
            match payload.get(field) {
                Some(JsonValue::Array(items)) if items.is_empty() => {}
                Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
                    CODE,
                    format!("{field} must be empty when the typed ux contract is present"),
                    format!("payload.{field}"),
                    artifact_id,
                )),
                _ => {}
            }
        }
    }

    validate_widget_permissions(payload.get("permissions"), diagnostics, artifact_id);

    if let Some(hints) = payload.get("projectionHints") {
        validate_widget_projection_hints(hints, diagnostics, artifact_id);
    }
    if let Some(proposal) = payload.get("proposal") {
        validate_proposal_metadata(proposal, "payload.proposal", diagnostics, artifact_id, CODE);
    }
}

/// Validate that every legacy widget action binding declares a matching envelope dependency.
pub fn validate_widget_binding_dependencies(
    artifact: &ArtifactEnvelope,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let payload_value = artifact.payload.to_json_value();
    let JsonValue::Object(payload) = &payload_value else {
        return;
    };

    let dependency_keys: BTreeSet<String> = artifact
        .dependencies
        .iter()
        .filter_map(|dependency| {
            dependency
                .version
                .as_deref()
                .filter(|version| !version.trim().is_empty())
                .map(|version| {
                    format!(
                        "{}:{}:{}",
                        artifact_kind_key(&dependency.kind),
                        dependency.artifact_id,
                        version
                    )
                })
        })
        .collect();

    if let Some(JsonValue::Array(bindings)) = payload.get("actionBindings") {
        for (index, binding) in bindings.iter().enumerate() {
            let JsonValue::Object(binding) = binding else {
                continue;
            };
            let Some(JsonValue::String(action_id)) = binding.get("actionId") else {
                continue;
            };
            let version = optional_binding_version(binding.get("actionVersion"));
            if !dependency_keys.contains(&format!("action:{action_id}:{version}")) {
                diagnostics.push(widget_dependency_diagnostic(
                    format!(
                        "widget action binding '{action_id}' must declare a matching action dependency"
                    ),
                    format!("payload.actionBindings[{index}].actionId"),
                    artifact.artifact_id.as_str(),
                ));
            }
        }
    }
}

fn optional_binding_version(value: Option<&JsonValue>) -> &str {
    match value {
        Some(JsonValue::String(version)) if !version.trim().is_empty() => version.as_str(),
        _ => "0.1.0",
    }
}

fn artifact_kind_key(kind: &ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Entity => "entity",
        ArtifactKind::Relation => "relation",
        ArtifactKind::Derived => "derived",
        ArtifactKind::Action => "action",
        ArtifactKind::Rule => "rule",
        ArtifactKind::Connector => "connector",
        ArtifactKind::Widget => "widget",
        ArtifactKind::Customization => "customization",
        ArtifactKind::Setting => "setting",
        ArtifactKind::BusinessPack => "business_pack",
        ArtifactKind::Policy => "policy",
        ArtifactKind::SourceSchema => "source_schema",
        ArtifactKind::Mapping => "mapping",
        ArtifactKind::Law => "law",
        ArtifactKind::DefinedConcept => "defined_concept",
        ArtifactKind::Labels => "labels",
        ArtifactKind::Model => "model",
    }
}
fn widget_dependency_diagnostic(
    message: impl Into<String>,
    path: impl Into<String>,
    artifact_id: &str,
) -> Diagnostic {
    Diagnostic {
        code: "widget.validation.missing_dependency".to_string(),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        artifact_id: Some(artifact_id.to_string()),
        path: Some(path.into()),
        remediation: Some(DiagnosticRemediation::ResolveDependency),
    }
}

fn validate_widget_data_dependencies(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "widget.validation.invalid_payload";
    match value {
        Some(JsonValue::Array(dependencies)) => {
            for (index, dependency) in dependencies.iter().enumerate() {
                let path = format!("payload.dataDependencies[{index}]");
                let Some(dependency) =
                    expect_object(dependency, path.clone(), diagnostics, artifact_id, CODE)
                else {
                    continue;
                };
                require_non_empty_field(
                    dependency,
                    "dependencyId",
                    format!("{path}.dependencyId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    dependency,
                    "objectKind",
                    format!("{path}.objectKind"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                let Some(query) = dependency.get("query") else {
                    diagnostics.push(invalid_diagnostic(
                        CODE,
                        "query is required",
                        format!("{path}.query"),
                        artifact_id,
                    ));
                    continue;
                };
                let Some(query) = expect_object(
                    query,
                    format!("{path}.query"),
                    diagnostics,
                    artifact_id,
                    CODE,
                ) else {
                    continue;
                };
                require_literal_field(
                    query,
                    "kind",
                    &["object_list", "object_detail", "relation_traversal"],
                    format!("{path}.query.kind"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                if matches!(
                    query.get("kind"),
                    Some(JsonValue::String(kind)) if kind == "relation_traversal"
                ) {
                    require_non_empty_field(
                        query,
                        "relationKind",
                        format!("{path}.query.relationKind"),
                        diagnostics,
                        artifact_id,
                        CODE,
                    );
                }
            }
        }
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "dataDependencies must be an array",
            "payload.dataDependencies",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "dataDependencies is required",
            "payload.dataDependencies",
            artifact_id,
        )),
    }
}

fn validate_widget_view_states(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "widget.validation.invalid_payload";
    match value {
        Some(JsonValue::Array(states)) if !states.is_empty() => {
            for (index, state) in states.iter().enumerate() {
                let path = format!("payload.viewStates[{index}]");
                let Some(state) =
                    expect_object(state, path.clone(), diagnostics, artifact_id, CODE)
                else {
                    continue;
                };
                require_non_empty_field(
                    state,
                    "stateId",
                    format!("{path}.stateId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_literal_field(
                    state,
                    "kind",
                    &["empty", "loading", "ready", "error", "preview"],
                    format!("{path}.kind"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
        }
        Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "viewStates must be a non-empty array",
            "payload.viewStates",
            artifact_id,
        )),
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "viewStates must be an array",
            "payload.viewStates",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "viewStates is required",
            "payload.viewStates",
            artifact_id,
        )),
    }
}

fn validate_widget_action_bindings(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "widget.validation.invalid_payload";
    match value {
        Some(JsonValue::Array(bindings)) => {
            for (index, binding) in bindings.iter().enumerate() {
                let path = format!("payload.actionBindings[{index}]");
                let Some(binding) =
                    expect_object(binding, path.clone(), diagnostics, artifact_id, CODE)
                else {
                    continue;
                };
                if matches!(
                    binding.get("unsafeRawMutation"),
                    Some(JsonValue::Bool(true))
                ) {
                    diagnostics.push(invalid_diagnostic(
                        "widget.validation.unsafe_action_binding",
                        "action binding must target a Norn action or process command, not a raw mutation",
                        path.clone(),
                        artifact_id,
                    ));
                }
                require_non_empty_field(
                    binding,
                    "bindingId",
                    format!("{path}.bindingId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    binding,
                    "actionId",
                    format!("{path}.actionId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_optional_non_empty_json_field(
                    binding,
                    "actionVersion",
                    format!("{path}.actionVersion"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    binding,
                    "label",
                    format!("{path}.label"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_string_array_field(
                    binding,
                    "requiredPermissions",
                    format!("{path}.requiredPermissions"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
        }
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "actionBindings must be an array",
            "payload.actionBindings",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "actionBindings is required",
            "payload.actionBindings",
            artifact_id,
        )),
    }
}

fn validate_widget_permissions(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "widget.validation.invalid_payload";
    match value {
        Some(JsonValue::Array(permissions)) => {
            for (index, permission) in permissions.iter().enumerate() {
                let path = format!("payload.permissions[{index}]");
                let Some(permission) =
                    expect_object(permission, path.clone(), diagnostics, artifact_id, CODE)
                else {
                    continue;
                };
                require_non_empty_field(
                    permission,
                    "permission",
                    format!("{path}.permission"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    permission,
                    "reason",
                    format!("{path}.reason"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
        }
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "permissions must be an array",
            "payload.permissions",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "permissions is required",
            "payload.permissions",
            artifact_id,
        )),
    }
}

fn validate_widget_projection_hints(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "widget.validation.invalid_payload";
    let Some(hints) = expect_object(
        value,
        "payload.projectionHints",
        diagnostics,
        artifact_id,
        CODE,
    ) else {
        return;
    };
    if hints.contains_key("renderer") {
        require_literal_field(
            hints,
            "renderer",
            &["react", "openui", "shadcn", "native"],
            "payload.projectionHints.renderer",
            diagnostics,
            artifact_id,
            CODE,
        );
    }
    if hints.contains_key("layout") {
        require_literal_field(
            hints,
            "layout",
            &["table", "detail", "kanban", "form", "dashboard"],
            "payload.projectionHints.layout",
            diagnostics,
            artifact_id,
            CODE,
        );
    }
}
