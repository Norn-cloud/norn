//! Artifact-envelope and graph-load checks, moved verbatim from `norn-ir`
//! (T1-CHK-1, #2524). Diagnostic codes, messages, and paths are unchanged.

mod action;
mod closed_schema;
mod connector;
mod customization;
mod derived_view;
mod entity;
mod graph_load;
/// Shared JSON-shape, field, and diagnostic helpers for envelope validators.
pub mod helpers;
mod pack;
mod policy;
mod relation;
mod rule;
mod setting;

use norn_ir::identifier_limits::MAX_ACTION_ID_BYTES;
use norn_ir::{
    ArtifactDependency, ArtifactEnvelope, ArtifactKind, ArtifactPayload, ArtifactProvenance,
    Diagnostic, DiagnosticRemediation, DiagnosticSeverity, JsonValue, SUPPORTED_IR_VERSION,
    ScopeRef,
};

pub use action::input_codes;
pub(crate) use closed_schema::{ClosedSchemaProfile, ClosedSchemaUnions, validate_closed_schema};
pub(crate) use entity::validate_entity_mutation_authority;
pub use graph_load::plan_graph_load;
pub(crate) use helpers::{
    invalid_diagnostic, require_literal_field, require_non_empty_field, validate_json_object_field,
    validate_non_empty_string_array_field,
};
pub(crate) use policy::{is_access_policy_payload, is_modern_policy_payload};
pub(crate) use rule::reject_rule_side_effect_fields;

use action::validate_action_payload;
use connector::validate_connector_payload;
use customization::validate_customization_payload;
use derived_view::validate_derived_payload;
use entity::validate_entity_payload;
use helpers::{identifier_too_long, require_non_empty_string, validate_optional_non_empty};
use pack::validate_business_pack_payload;
use policy::validate_policy_payload;
use relation::validate_relation_payload;
use rule::validate_rule_payload;
use setting::validate_setting_payload;

/// Stable diagnostic for identifier lengths at new admission boundaries.
pub const IDENTIFIER_TOO_LONG: &str = "identifier.validation.too_long";

/// Method-call sugar for [`validate_artifact`], so `artifact.validate()` keeps
/// working for callers after the validator moved out of `norn-ir`.
pub trait ArtifactValidation {
    /// Validate the envelope and its kind payload; see [`validate_artifact`].
    ///
    /// # Errors
    ///
    /// Returns every diagnostic found when the envelope is invalid.
    fn validate(&self) -> Result<(), Vec<Diagnostic>>;
}

impl ArtifactValidation for ArtifactEnvelope {
    fn validate(&self) -> Result<(), Vec<Diagnostic>> {
        validate_artifact(self)
    }
}
/// Validate one artifact envelope and its kind payload.
///
/// Moved verbatim from `norn_ir::ArtifactEnvelope::validate` (T1-CHK-1); the
/// diagnostics (codes, messages, paths) are unchanged for Ring-0 kinds.
/// `ArtifactPayload::Widget` fails closed here because widget validation lives
/// in Ring-1 `norn-ui-checker`.
///
/// # Errors
///
/// Returns every diagnostic found when the envelope is invalid.
pub fn validate_artifact(artifact: &ArtifactEnvelope) -> Result<(), Vec<Diagnostic>> {
    validate_artifact_inner(artifact, None)
}

/// Validate one artifact envelope and its kind payload, running `ui`'s
/// strictly additive widget payload validator on `ArtifactPayload::Widget`
/// before typed-root agreement.
///
/// # Errors
///
/// Returns every diagnostic found when the envelope is invalid.
pub fn validate_artifact_with_ui(
    artifact: &ArtifactEnvelope,
    ui: &dyn crate::UiValidator,
) -> Result<(), Vec<Diagnostic>> {
    validate_artifact_inner(artifact, Some(ui))
}

fn validate_artifact_inner(
    artifact: &ArtifactEnvelope,
    ui: Option<&dyn crate::UiValidator>,
) -> Result<(), Vec<Diagnostic>> {
    let (normalized, layout_findings) = match crate::layout::normalize_artifact(artifact) {
        Ok((normalized, _layout)) => (normalized, Vec::new()),
        Err(findings) => (artifact.clone(), findings),
    };
    let mut diagnostics = collect_artifact_diagnostics_inner(&normalized, ui);
    for diagnostic in layout_findings.into_iter().map(layout_finding_to_legacy) {
        if !diagnostics.iter().any(|existing| {
            existing.code == diagnostic.code
                && existing.artifact_id == diagnostic.artifact_id
                && existing.path == diagnostic.path
        }) {
            diagnostics.push(diagnostic);
        }
    }
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    {
        Err(diagnostics)
    } else {
        Ok(())
    }
}

/// Collect legacy envelope diagnostics for one normalized artifact, running
/// optional strictly-additive Ring-1 widget validation before typed-root agreement.
#[must_use]
pub(crate) fn collect_artifact_diagnostics_inner(
    artifact: &ArtifactEnvelope,
    ui: Option<&dyn crate::UiValidator>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    if artifact.ir_version != SUPPORTED_IR_VERSION {
        diagnostics.push(Diagnostic {
            code: "ir.validation.unsupported_version".to_string(),
            severity: DiagnosticSeverity::Error,
            message: format!("unsupported IR version '{}'", artifact.ir_version),
            artifact_id: Some(artifact.artifact_id.clone()),
            path: Some("irVersion".to_string()),
            remediation: Some(DiagnosticRemediation::UpgradeIr),
        });
    }

    if artifact.artifact_id.trim().is_empty() {
        diagnostics.push(required_field("artifactId"));
    }

    if artifact.namespace.trim().is_empty() {
        diagnostics.push(required_field("namespace"));
    }

    if artifact.name.trim().is_empty() {
        diagnostics.push(required_field("name"));
    }
    if artifact.kind() == ArtifactKind::Action && artifact.name.len() > MAX_ACTION_ID_BYTES {
        diagnostics.push(identifier_too_long(
            "action name",
            artifact.name.len(),
            MAX_ACTION_ID_BYTES,
            "name",
            Some(&artifact.artifact_id),
        ));
    }

    if artifact.version.trim().is_empty() {
        diagnostics.push(required_field("version"));
    }

    validate_scope_ref(
        &artifact.scope,
        &mut diagnostics,
        Some(artifact.artifact_id.as_str()),
    );
    validate_dependencies(
        &artifact.dependencies,
        &mut diagnostics,
        artifact.artifact_id.as_str(),
        artifact.version.as_str(),
    );
    validate_provenance(
        &artifact.provenance,
        &mut diagnostics,
        Some(artifact.artifact_id.as_str()),
    );
    let payload = artifact.payload.to_json_value();
    validate_json_value(
        &payload,
        "payload",
        &mut diagnostics,
        Some(artifact.artifact_id.as_str()),
    );
    validate_kind_payload(artifact, &mut diagnostics, ui);

    diagnostics
}

fn layout_finding_to_legacy(diagnostic: crate::Diagnostic) -> Diagnostic {
    let path = diagnostic
        .span
        .map(|span| pointer_to_legacy_path(&span.pointer));
    Diagnostic {
        code: diagnostic.code.as_str().to_owned(),
        severity: diagnostic.severity,
        message: diagnostic.message,
        artifact_id: diagnostic.artifact_id,
        path,
        remediation: diagnostic.remediation,
    }
}

fn pointer_to_legacy_path(pointer: &str) -> String {
    let mut path = String::new();
    for segment in pointer.trim_start_matches('/').split('/') {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        if segment.parse::<usize>().is_ok() {
            path.push('[');
            path.push_str(&segment);
            path.push(']');
        } else {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(&segment);
        }
    }
    path
}

fn required_field(path: &str) -> Diagnostic {
    Diagnostic {
        code: "ir.validation.required_field".to_string(),
        severity: DiagnosticSeverity::Error,
        message: format!("{path} is required"),
        artifact_id: None,
        path: Some(path.to_string()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

fn validate_kind_payload(
    artifact: &ArtifactEnvelope,
    diagnostics: &mut Vec<Diagnostic>,
    ui: Option<&dyn crate::UiValidator>,
) {
    let artifact_id = Some(artifact.artifact_id.as_str());
    let before = diagnostics.len();
    let code = artifact_payload_validation_code(&artifact.kind());
    validate_root_shape(&artifact.payload, diagnostics, artifact_id, code);
    let payload = validation_json_value(&artifact.payload);
    match &artifact.payload {
        ArtifactPayload::Entity(_) => validate_entity_payload(&payload, diagnostics, artifact_id),
        ArtifactPayload::Relation(relation) => {
            validate_relation_payload(&payload, diagnostics, artifact_id, &artifact.name);
            // There is no RelationIR v2: an EntityIR v2 relation is a link
            // declared on its source object type (T1-IR-1b). `entityVersion`
            // is not a relation member, so it is also an unknown field.
            if relation.unknown_fields().contains("entityVersion") {
                diagnostics.push(invalid_diagnostic(
                    crate::typed_roots::codes::RELATION_V2_IS_LINK,
                    "RelationIR has no v2 form: declare an EntityIR v2 relation as a link in the source entity's payload.objectType.links",
                    "payload.entityVersion",
                    artifact_id,
                ));
            }
        }
        ArtifactPayload::Derived(_) => validate_derived_payload(&payload, diagnostics, artifact_id),
        ArtifactPayload::Action(_) => validate_action_payload(&payload, diagnostics, artifact_id),
        ArtifactPayload::Rule(_) => {
            validate_rule_payload(&payload, &artifact.dependencies, diagnostics, artifact_id);
        }
        ArtifactPayload::Policy(_) => validate_policy_payload(&payload, diagnostics, artifact_id),
        ArtifactPayload::Connector(_) => {
            validate_connector_payload(&artifact.payload.to_json_value(), diagnostics, artifact_id);
        }
        ArtifactPayload::Customization(_) => {
            validate_customization_payload(&payload, diagnostics, artifact_id);
        }
        ArtifactPayload::Setting(_) => validate_setting_payload(&payload, diagnostics, artifact_id),
        ArtifactPayload::BusinessPack(_) => {
            validate_business_pack_payload(&payload, diagnostics, artifact_id);
        }
        // Widget payload validation lives in Ring-1 `norn-ui-checker`. Ring-0
        // fails closed when no Ring-1 UI validator is supplied; when one is
        // supplied, its returned diagnostics are strictly appended so Ring-0
        // findings can never be mutated or suppressed.
        ArtifactPayload::Widget(_) => {
            if let Some(validator) = ui {
                diagnostics.extend(validator.validate_widget_payload(artifact, &payload));
            } else {
                diagnostics.push(invalid_diagnostic(
                    code,
                    "widget artifacts require Ring-1 norn-ui-checker validation and cannot be admitted by Ring-0 norn-checker alone",
                    "payload",
                    artifact_id,
                ));
            }
        }
        // The remaining strict-serde roots are checked by their dedicated modules.
        ArtifactPayload::SourceSchema(_)
        | ArtifactPayload::Mapping(_)
        | ArtifactPayload::Law(_)
        | ArtifactPayload::DefinedConcept(_)
        | ArtifactPayload::Labels(_)
        | ArtifactPayload::Model(_) => {}
    }
    if matches!(artifact.payload, ArtifactPayload::Law(_)) {
        diagnostics.extend(
            crate::verification::check_law_local(artifact)
                .into_iter()
                .map(|d| norn_ir::Diagnostic {
                    code: d.code.as_str().to_owned(),
                    severity: d.severity,
                    message: d.message,
                    artifact_id: d.artifact_id,
                    path: d
                        .span
                        .map(|span| span.pointer.trim_start_matches('/').replace('/', ".")),
                    remediation: d.remediation,
                }),
        );
    }
    // Typed-root agreement: a payload the kind validator accepts must decode
    // into its strict typed root. Run only on otherwise clean payloads so a
    // malformed member is reported once, by the validator.
    if diagnostics.len() == before {
        crate::typed_roots::check_typed_root(&artifact.payload, diagnostics, artifact_id);
    }
}

/// The payload object the kind validators walk: the bare payload without the
/// reserved `extensions` slots (moved from the `norn-ir` payload roots).
fn validation_json_value(payload: &ArtifactPayload) -> JsonValue {
    let mut value = payload.to_json_value();
    if let JsonValue::Object(object) = &mut value {
        object.remove("extensions");
    }
    value
}

/// Root-shape diagnostics recorded while decoding a closed payload root (moved
/// from the `norn-ir` payload roots).
fn validate_root_shape(
    payload: &ArtifactPayload,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    for field in payload.unknown_fields() {
        diagnostics.push(invalid_diagnostic(
            code,
            format!("unknown field '{field}' is not allowed"),
            format!("payload.{field}"),
            artifact_id,
        ));
    }
    if payload.has_invalid_extensions() {
        diagnostics.push(invalid_diagnostic(
            code,
            "extensions must be an object map",
            "payload.extensions",
            artifact_id,
        ));
    }
}

const fn artifact_payload_validation_code(kind: &ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Entity => "entity.validation.invalid_payload",
        ArtifactKind::Relation => "relation.validation.invalid_payload",
        ArtifactKind::Action => "action.validation.invalid_payload",
        ArtifactKind::Rule => "rule.validation.invalid_payload",
        ArtifactKind::Connector => "connector.validation.invalid_payload",
        ArtifactKind::Widget => "widget.validation.invalid_payload",
        ArtifactKind::Customization => "customization.validation.invalid_payload",
        ArtifactKind::Setting => "setting.validation.invalid_payload",
        ArtifactKind::BusinessPack => "business_pack.validation.invalid_payload",
        ArtifactKind::Policy => "policy.validation.invalid_payload",
        ArtifactKind::Derived => "derived.validation.invalid_payload",
        ArtifactKind::SourceSchema => "source_schema.validation.invalid_document",
        ArtifactKind::Mapping => "mapping.validation.invalid_document",
        ArtifactKind::Law => crate::verification::codes::INVALID_DOCUMENT,
        ArtifactKind::DefinedConcept => "defined_concept.validation.invalid_payload",
        ArtifactKind::Labels => "labels.validation.invalid_payload",
        ArtifactKind::Model => "model.validation.invalid_payload",
    }
}
fn validate_scope_ref(
    scope: &ScopeRef,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    validate_optional_non_empty(
        scope.org_id.as_deref(),
        "scope.orgId",
        diagnostics,
        artifact_id,
    );
    validate_optional_non_empty(
        scope.tenant_id.as_deref(),
        "scope.tenantId",
        diagnostics,
        artifact_id,
    );
    validate_optional_non_empty(
        scope.workspace_id.as_deref(),
        "scope.workspaceId",
        diagnostics,
        artifact_id,
    );
    validate_optional_non_empty(
        scope.environment_id.as_deref(),
        "scope.environmentId",
        diagnostics,
        artifact_id,
    );
    validate_optional_non_empty(
        scope.business_unit_id.as_deref(),
        "scope.businessUnitId",
        diagnostics,
        artifact_id,
    );
}

fn validate_dependencies(
    dependencies: &[ArtifactDependency],
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: &str,
    artifact_version: &str,
) {
    for (index, dependency) in dependencies.iter().enumerate() {
        require_non_empty_string(
            &dependency.artifact_id,
            format!("dependencies[{index}].artifactId"),
            diagnostics,
            Some(artifact_id),
            "ir.validation.invalid_artifact",
        );

        let dependency_version_path = format!("dependencies[{index}].version");
        let Some(dependency_version) = dependency.version.as_deref() else {
            diagnostics.push(dependency_diagnostic(
                "dependency version must be explicit",
                dependency_version_path,
                artifact_id,
            ));
            continue;
        };

        if dependency_version.trim().is_empty() {
            diagnostics.push(dependency_diagnostic(
                "dependency version must be a non-empty string",
                dependency_version_path,
                artifact_id,
            ));
            continue;
        }

        if dependency.artifact_id == artifact_id && dependency_version == artifact_version {
            diagnostics.push(dependency_diagnostic(
                "artifact cannot depend on itself",
                format!("dependencies[{index}]"),
                artifact_id,
            ));
        }
    }
}

fn dependency_diagnostic(
    message: impl Into<String>,
    path: impl Into<String>,
    artifact_id: &str,
) -> Diagnostic {
    Diagnostic {
        code: "ir.validation.invalid_dependency".to_string(),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        artifact_id: Some(artifact_id.to_string()),
        path: Some(path.into()),
        remediation: Some(DiagnosticRemediation::ResolveDependency),
    }
}

fn validate_provenance(
    provenance: &ArtifactProvenance,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    validate_optional_non_empty(
        provenance.source_path.as_deref(),
        "provenance.sourcePath",
        diagnostics,
        artifact_id,
    );
    validate_optional_non_empty(
        provenance.actor_id.as_deref(),
        "provenance.actorId",
        diagnostics,
        artifact_id,
    );
    validate_optional_non_empty(
        provenance.created_at.as_deref(),
        "provenance.createdAt",
        diagnostics,
        artifact_id,
    );
}

fn validate_json_value(
    value: &JsonValue,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let path = path.into();
    match value {
        JsonValue::Number(value) if !value.is_finite() => diagnostics.push(invalid_diagnostic(
            "ir.validation.invalid_artifact",
            "number must be finite",
            path,
            artifact_id,
        )),
        JsonValue::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                validate_json_value(item, format!("{path}[{index}]"), diagnostics, artifact_id);
            }
        }
        JsonValue::Object(object) => {
            for (key, item) in object {
                validate_json_value(item, format!("{path}.{key}"), diagnostics, artifact_id);
            }
        }
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::String(_) => {}
    }
}

#[cfg(test)]
mod tests;
