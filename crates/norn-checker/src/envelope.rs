//! Artifact-envelope and graph-load checks, moved verbatim from `norn-ir`
//! (T1-CHK-1, #2524). Diagnostic codes, messages, and paths are unchanged.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::identifier_limits::{
    MAX_ACTION_ID_BYTES, MAX_LINK_NAME_BYTES, MAX_OBJECT_TYPE_NAME_BYTES,
};

#[allow(
    clippy::wildcard_imports,
    reason = "moved verbatim from norn-ir's crate root, where every IR type was in scope"
)]
use norn_ir::*;

use crate::connector_path::ConnectorValuePath;
use crate::ontology;

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

pub fn plan_graph_load(request: &GraphLoadRequest) -> GraphLoadPlan {
    let mut diagnostics = Vec::new();

    validate_graph_scope(request, &mut diagnostics);
    validate_graph_shape(&request.shape, &mut diagnostics);

    let mut relation_batches = Vec::new();
    build_relation_batches(
        request,
        &request.shape.root_object_kind,
        &request.shape.root_ids,
        &request.shape.relations,
        1,
        &mut relation_batches,
        &mut diagnostics,
    );

    let depth = graph_max_depth(&request.shape.relations);
    let relation_shape_count = graph_relation_count(&request.shape.relations);
    let estimated_rows = estimate_graph_rows(
        u64::try_from(request.shape.root_ids.len()).unwrap_or(u64::MAX),
        &request.shape.relations,
    );

    if u32::try_from(request.shape.root_ids.len())
        .ok()
        .is_none_or(|count| count > request.budget.max_root_objects)
    {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.over_budget",
            format!(
                "root object count exceeds budget {}",
                request.budget.max_root_objects
            ),
            "shape.rootIds",
        ));
    }
    if depth > request.budget.max_depth {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.excessive_depth",
            format!(
                "graph depth {depth} exceeds budget {}",
                request.budget.max_depth
            ),
            "shape.relations",
        ));
    }
    if relation_shape_count > request.budget.max_relation_shapes {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.over_budget",
            format!(
                "relation shape count {relation_shape_count} exceeds budget {}",
                request.budget.max_relation_shapes
            ),
            "shape.relations",
        ));
    }
    if u32::try_from(relation_batches.len())
        .ok()
        .is_none_or(|count| count > request.budget.max_relation_batches)
    {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.over_budget",
            format!(
                "relation batch count {} exceeds budget {}",
                relation_batches.len(),
                request.budget.max_relation_batches
            ),
            "shape.relations",
        ));
    }
    if estimated_rows > request.budget.max_rows
        && request.budget.allow_higher_risk_policy != Some(true)
    {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.over_budget",
            format!(
                "estimated row count {estimated_rows} exceeds budget {}",
                request.budget.max_rows
            ),
            "budget.maxRows",
        ));
    }

    let evidence = GraphLoadEvidence {
        correlation_id: request.correlation_id.clone(),
        depth,
        relation_shape_count,
        relation_batch_count: u32::try_from(relation_batches.len()).unwrap_or(u32::MAX),
        estimated_rows,
        budget: request.budget.clone(),
    };

    GraphLoadPlan {
        request_id: request.request_id.clone(),
        correlation_id: request.correlation_id.clone(),
        root_identity_keys: request
            .shape
            .root_ids
            .iter()
            .map(|root_id| graph_identity_key(&request.shape.root_object_kind, root_id))
            .collect(),
        relation_batches,
        evidence,
        can_execute: diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != DiagnosticSeverity::Error),
        diagnostics,
    }
}

/// Validate one artifact envelope and its kind payload.
///
/// Moved verbatim from `norn_ir::ArtifactEnvelope::validate` (T1-CHK-1); the
/// diagnostics (codes, messages, paths) are unchanged.
///
/// # Errors
///
/// Returns every diagnostic found when the envelope is invalid.
pub fn validate_artifact(artifact: &ArtifactEnvelope) -> Result<(), Vec<Diagnostic>> {
    let (normalized, layout_findings) = match crate::layout::normalize_artifact(artifact) {
        Ok((normalized, _layout)) => (normalized, Vec::new()),
        Err(findings) => (artifact.clone(), findings),
    };
    let mut diagnostics = validate_artifact_unchecked(&normalized)
        .err()
        .unwrap_or_default();
    for diagnostic in layout_findings.into_iter().map(layout_finding_to_legacy) {
        if !diagnostics.iter().any(|existing| {
            existing.code == diagnostic.code
                && existing.artifact_id == diagnostic.artifact_id
                && existing.path == diagnostic.path
        }) {
            diagnostics.push(diagnostic);
        }
    }
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

pub(crate) fn validate_artifact_unchecked(
    artifact: &ArtifactEnvelope,
) -> Result<(), Vec<Diagnostic>> {
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
    validate_kind_payload(artifact, &mut diagnostics);

    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
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

fn validate_kind_payload(artifact: &ArtifactEnvelope, diagnostics: &mut Vec<Diagnostic>) {
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
        ArtifactPayload::Widget(_) => {
            validate_widget_payload(&payload, diagnostics, artifact_id);
            validate_widget_binding_dependencies(artifact, diagnostics);
        }
        // Both roots are strict serde types: the typed-root check below
        // decodes them, and their semantic checks are `crate::source_schema`
        // and `crate::mapping` (run per document and over the artifact set).
        ArtifactPayload::SourceSchema(_)
        | ArtifactPayload::Mapping(_)
        | ArtifactPayload::Law(_) => {}
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
    }
}

// ---------------------------------------------------------------------------
// Entity / Relation payload validation
// ---------------------------------------------------------------------------

fn validate_entity_payload(
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
            for (field_name, field_definition) in fields {
                let field_path = format!("payload.fields.{field_name}");
                if field_name.trim().is_empty() {
                    diagnostics.push(kind_invalid(
                        "Entity field names must be non-empty",
                        &field_path,
                        artifact_id,
                        CODE,
                    ));
                }
                let Some(field_definition) = expect_object(
                    field_definition,
                    &field_path,
                    diagnostics,
                    artifact_id,
                    CODE,
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
                    CODE,
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
                    CODE,
                );
                if field_definition.contains_key("required") {
                    require_boolean_field(
                        field_definition,
                        "required",
                        format!("{field_path}.required"),
                        diagnostics,
                        artifact_id,
                        CODE,
                    );
                }
                if field_type.as_deref() == Some("enum") {
                    match field_definition.get("values") {
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
                                                CODE,
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
                                        CODE,
                                    )),
                                }
                            }
                        }
                        _ => diagnostics.push(kind_invalid(
                            "enum fields require a non-empty values array",
                            &format!("{field_path}.values"),
                            artifact_id,
                            CODE,
                        )),
                    }
                }
            }
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

fn validate_relation_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    relation_name: &str,
) {
    const CODE: &str = "relation.validation.invalid_payload";
    if relation_name.len() > MAX_LINK_NAME_BYTES {
        diagnostics.push(identifier_too_long(
            "relation name",
            relation_name.len(),
            MAX_LINK_NAME_BYTES,
            "name",
            artifact_id,
        ));
    }
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };
    reject_unknown_object_fields(
        payload,
        &["from", "to", "cardinality"],
        "payload",
        diagnostics,
        artifact_id,
        CODE,
    );

    // Canonical RelationIR v0: `from`/`to` name the source and target entity kinds
    // (entity-kind references such as "retail.Item"), and both are required. The relation's
    // kind is its artifact identity (id/name), not a payload field — mirrors the reset-era
    // retail pack, the only concrete RelationIR producer.
    if let Some(source) = require_non_empty_field(
        payload,
        "from",
        "payload.from",
        diagnostics,
        artifact_id,
        CODE,
    ) && source.len() > MAX_OBJECT_TYPE_NAME_BYTES
    {
        diagnostics.push(identifier_too_long(
            "source object type",
            source.len(),
            MAX_OBJECT_TYPE_NAME_BYTES,
            "payload.from",
            artifact_id,
        ));
    }
    if let Some(target) =
        require_non_empty_field(payload, "to", "payload.to", diagnostics, artifact_id, CODE)
        && target.len() > MAX_OBJECT_TYPE_NAME_BYTES
    {
        diagnostics.push(identifier_too_long(
            "target object type",
            target.len(),
            MAX_OBJECT_TYPE_NAME_BYTES,
            "payload.to",
            artifact_id,
        ));
    }

    // Optional `cardinality`: when present it must name a known relation cardinality.
    if let Some(cardinality) = payload.get("cardinality") {
        match cardinality.as_string() {
            Some("one_to_one" | "one_to_many" | "many_to_one" | "many_to_many") => {}
            _ => {
                diagnostics.push(kind_invalid(
                    "payload.cardinality must be one of one_to_one, one_to_many, many_to_one, many_to_many",
                    "payload.cardinality",
                    artifact_id,
                    CODE,
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Derived / ViewIR v0 payload validation
// ---------------------------------------------------------------------------

fn validate_derived_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "derived.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    // viewVersion must be "0.1"
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

    // viewId is required
    require_non_empty_field(
        payload,
        "viewId",
        "payload.viewId",
        diagnostics,
        artifact_id,
        CODE,
    );

    // inputs: required, non-empty array of view input refs
    validate_view_inputs(payload.get("inputs"), diagnostics, artifact_id, CODE);

    // calc: required, serializable, must not be effectful_ts (IO-in-view rejection)
    validate_view_calc(payload.get("calc"), diagnostics, artifact_id, CODE);

    // outputSchema: required object
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

    // maintenance: required; only "recompute_on_read" is supported; "incremental" is rejected.
    validate_view_maintenance(payload.get("maintenance"), diagnostics, artifact_id, CODE);

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
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|inp| {
                    inp.as_object()
                        .and_then(|o| o.get("alias"))
                        .and_then(|a| a.as_string())
                        .map(|s| s == "as_of")
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
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
            let mut aliases: BTreeSet<String> = BTreeSet::new();
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

                // artifactId
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

                // kind — only entity, relation, or derived are valid inputs
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

                // version — must be exact (non-empty)
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

                // alias — must be a non-empty string, unique across inputs
                match inp.get("alias") {
                    Some(JsonValue::String(a)) if !a.trim().is_empty() => {
                        let alias = a.clone();
                        if !aliases.insert(alias.clone()) {
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
                _ => {
                    diagnostics.push(kind_invalid(
                        "payload.calc.kind must be \"field_projection\"",
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
                "payload.maintenance must be \"recompute_on_read\"",
                "payload.maintenance",
                artifact_id,
                code,
            ));
        }
    }
}

/// Returns the parsed TemporalScopeMode if the field is present and valid.
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

fn kind_invalid(message: &str, path: &str, artifact_id: Option<&str>, code: &str) -> Diagnostic {
    Diagnostic {
        code: code.to_string(),
        severity: DiagnosticSeverity::Error,
        message: message.to_string(),
        artifact_id: artifact_id.map(str::to_string),
        path: Some(path.to_string()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

fn identifier_too_long(
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

// ---------------------------------------------------------------------------
// End Derived / ViewIR v0 validation
// ---------------------------------------------------------------------------

fn validate_action_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "action.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    reject_unknown_object_fields(
        payload,
        &[
            "actionId",
            "scopeRequired",
            "idempotent",
            "ledgered",
            "inputSchema",
            "outputSchema",
            "effectProgram",
            "title",
            "description",
            "ledgerCategory",
            "preconditions",
            "slice",
            "derive",
            "commands",
            "writes",
            "compensation",
            "access",
            "uiEffects",
            "quantityEffect",
            "createLinks",
        ],
        "payload",
        diagnostics,
        artifact_id,
        CODE,
    );

    require_non_empty_field(
        payload,
        "actionId",
        "payload.actionId",
        diagnostics,
        artifact_id,
        CODE,
    );
    if let Some(JsonValue::String(action_id)) = payload.get("actionId")
        && action_id.len() > MAX_ACTION_ID_BYTES
    {
        diagnostics.push(identifier_too_long(
            "action id",
            action_id.len(),
            MAX_ACTION_ID_BYTES,
            "payload.actionId",
            artifact_id,
        ));
    }
    if let Some(JsonValue::Object(effect)) = payload.get("effectProgram")
        && let Some(JsonValue::String(object_kind)) = effect.get("objectKind")
        && object_kind.len() > MAX_OBJECT_TYPE_NAME_BYTES
    {
        diagnostics.push(identifier_too_long(
            "effect object type",
            object_kind.len(),
            MAX_OBJECT_TYPE_NAME_BYTES,
            "payload.effectProgram.objectKind",
            artifact_id,
        ));
    }
    if let Some(JsonValue::Array(writes)) = payload.get("writes") {
        for (index, write) in writes.iter().enumerate() {
            if let JsonValue::Object(write) = write
                && let Some(JsonValue::String(object_kind)) = write.get("entityKind")
                && object_kind.len() > MAX_OBJECT_TYPE_NAME_BYTES
            {
                diagnostics.push(identifier_too_long(
                    "write object type",
                    object_kind.len(),
                    MAX_OBJECT_TYPE_NAME_BYTES,
                    format!("payload.writes[{index}].entityKind"),
                    artifact_id,
                ));
            }
        }
    }
    if let Some(JsonValue::Object(compensation)) = payload.get("compensation")
        && let Some(JsonValue::String(action_id)) = compensation.get("actionId")
        && action_id.len() > MAX_ACTION_ID_BYTES
    {
        diagnostics.push(identifier_too_long(
            "compensation action id",
            action_id.len(),
            MAX_ACTION_ID_BYTES,
            "payload.compensation.actionId",
            artifact_id,
        ));
    }
    validate_action_scope_required(payload.get("scopeRequired"), diagnostics, artifact_id);
    require_boolean_field(
        payload,
        "idempotent",
        "payload.idempotent",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_boolean_field(
        payload,
        "ledgered",
        "payload.ledgered",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_closed_action_schema(
        payload.get("inputSchema"),
        "payload.inputSchema",
        0,
        true,
        diagnostics,
        artifact_id,
    );
    validate_closed_action_schema(
        payload.get("outputSchema"),
        "payload.outputSchema",
        0,
        false,
        diagnostics,
        artifact_id,
    );
    if let Some(effect_program) = payload.get("effectProgram") {
        validate_action_effect_program(
            effect_program,
            payload.get("inputSchema"),
            diagnostics,
            artifact_id,
        );
    }
    if let Some(ui_effects) = payload.get("uiEffects") {
        validate_action_ui_effects(ui_effects, diagnostics, artifact_id);
    }
    crate::typed_roots::validate_action_typed_members(payload, diagnostics, artifact_id);
}

fn validate_action_ui_effects(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "action.validation.invalid_payload";
    let Some(ui_effects) = value.as_object() else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "payload.uiEffects must be an object",
            "payload.uiEffects",
            artifact_id,
        ));
        return;
    };

    reject_unknown_object_fields(
        ui_effects,
        &[
            "effectsVersion",
            "irreversible",
            "destructive",
            "money",
            "errorCodes",
            "resultCodes",
        ],
        "payload.uiEffects",
        diagnostics,
        artifact_id,
        CODE,
    );

    match ui_effects.get("effectsVersion") {
        Some(JsonValue::String(version)) if version == "0.1" => {}
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "payload.uiEffects.effectsVersion must be \"0.1\"",
            "payload.uiEffects.effectsVersion",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "payload.uiEffects.effectsVersion is required",
            "payload.uiEffects.effectsVersion",
            artifact_id,
        )),
    }

    for field in ["irreversible", "destructive"] {
        if let Some(value) = ui_effects.get(field)
            && !matches!(value, JsonValue::Bool(_))
        {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("payload.uiEffects.{field} must be a boolean"),
                format!("payload.uiEffects.{field}"),
                artifact_id,
            ));
        }
    }

    for field in ["errorCodes", "resultCodes"] {
        if let Some(value) = ui_effects.get(field) {
            let path = format!("payload.uiEffects.{field}");
            match value {
                JsonValue::Array(codes) => {
                    let mut seen = BTreeSet::new();
                    for (index, code) in codes.iter().enumerate() {
                        let item_path = format!("{path}[{index}]");
                        match code {
                            JsonValue::String(code) if is_exact_semantic_identity(code) => {
                                if !seen.insert(code) {
                                    diagnostics.push(invalid_diagnostic(
                                        CODE,
                                        format!("{item_path} duplicates an earlier value"),
                                        item_path,
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
                                format!("{item_path} must be a non-empty exact code"),
                                item_path,
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
                    CODE,
                    format!("{path} must be an array"),
                    path,
                    artifact_id,
                )),
            }
        }
    }

    if let Some(value) = ui_effects.get("money") {
        let path = "payload.uiEffects.money";
        match value {
            JsonValue::Array(currencies) => {
                let mut seen = BTreeSet::new();
                for (index, currency) in currencies.iter().enumerate() {
                    let item_path = format!("{path}[{index}]");
                    match currency {
                        JsonValue::String(currency_ref)
                            if is_exact_semantic_identity(currency_ref) =>
                        {
                            if !seen.insert(currency_ref) {
                                diagnostics.push(invalid_diagnostic(
                                    CODE,
                                    format!("{item_path} duplicates an earlier value"),
                                    item_path,
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
                            format!("{item_path} must be a non-empty exact currency reference"),
                            item_path,
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
                CODE,
                format!("{path} must be an array"),
                path,
                artifact_id,
            )),
        }
    }
}

fn validate_closed_action_schema(
    value: Option<&JsonValue>,
    path: &str,
    depth: usize,
    action_input: bool,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if action_input
        && value
            .and_then(JsonValue::as_object)
            .and_then(|schema| schema.get("type"))
            .is_some_and(|schema_type| schema_type.as_array().is_some())
    {
        diagnostics.push(invalid_diagnostic(
            "action.validation.invalid_schema",
            "ActionIR type arrays are admitted only as input leaf schemas",
            format!("{path}.type"),
            artifact_id,
        ));
        return;
    }
    validate_closed_schema(
        value,
        path,
        depth,
        diagnostics,
        artifact_id,
        ClosedSchemaProfile {
            code: "action.validation.invalid_schema",
            subject: "ActionIR",
            string_enums: false,
            unions: if action_input {
                ClosedSchemaUnions::ActionInput
            } else {
                ClosedSchemaUnions::None
            },
        },
    );
}

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
    /// Existing ActionIR input leaves: nullable primitives or bounded JSON.
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
        match profile.unions {
            ClosedSchemaUnions::ActionInput => validate_action_type_union(
                type_members,
                schema,
                path,
                diagnostics,
                artifact_id,
                code,
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
                code,
                format!("closed {subject} schema requires one explicit type"),
                format!("{path}.type"),
                artifact_id,
            )),
        }
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

    let allowed: &[&str] = match schema_type {
        "object" => &["type", "properties", "required", "additionalProperties"],
        "array" => &["type", "items"],
        "string" if profile.string_enums => &["type", "enum"],
        _ => &["type"],
    };
    reject_unknown_object_fields(schema, allowed, path, diagnostics, artifact_id, code);

    match schema_type {
        "object" => {
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
                    let Some(field) = field.as_string().filter(|field| !field.trim().is_empty())
                    else {
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

/// A nullable RuleIR value retains the same closed non-null shape. Arbitrary
/// unions and the ActionIR JSON escape leaf remain outside this profile.
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

/// Admit only the two standard JSON Schema type arrays used by generated
/// `MappingIR` action inputs: the full `RuntimeValue` JSON domain, or a nullable
/// primitive scalar. A union is a leaf and accepts no sibling keywords.
fn validate_action_type_union(
    members: &[JsonValue],
    schema: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    reject_unknown_object_fields(schema, &["type"], path, diagnostics, artifact_id, code);

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
    let nullable_scalar = names.len() == 2
        && unique.len() == 2
        && unique.contains("null")
        && unique
            .iter()
            .any(|name| matches!(*name, "string" | "integer" | "number" | "boolean"));
    let json_value = names.len() == json_domain.len() && unique == json_domain;
    if !nullable_scalar && !json_value {
        diagnostics.push(invalid_diagnostic(
            code,
            "ActionIR type array must be a nullable scalar or the complete JSON value domain",
            format!("{path}.type"),
            artifact_id,
        ));
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the closed effect whitelist validates common target members and the three admitted variant shapes together"
)]
fn validate_action_effect_program(
    value: &JsonValue,
    input_schema: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "action.validation.invalid_effect_program";
    let Some(program) = expect_object(
        value,
        "payload.effectProgram",
        diagnostics,
        artifact_id,
        CODE,
    ) else {
        return;
    };
    // The members every effect carries; `fire_transition` adds the
    // transition it fires (T3-LIFE-3, #2867).
    const TARGET_MEMBERS: [&str; 5] = [
        "kind",
        "objectIdField",
        "objectKind",
        "entityArtifactId",
        "entityArtifactVersion",
    ];
    const TRANSITION_MEMBERS: [&str; 2] = ["lifecycle", "transition"];
    const ROW_MEMBERS: [&str; 5] = [
        "parentValuesField",
        "rowOpsField",
        "parentVersionField",
        "parentDeleteField",
        "collections",
    ];
    let fires_transition =
        program.get("kind").and_then(JsonValue::as_string) == Some("fire_transition");
    let mutates_rows =
        program.get("kind").and_then(JsonValue::as_string) == Some("mutate_parent_with_rows");
    let allowed: Vec<&str> = if fires_transition {
        TARGET_MEMBERS
            .into_iter()
            .chain(TRANSITION_MEMBERS)
            .collect()
    } else if mutates_rows {
        TARGET_MEMBERS.into_iter().chain(ROW_MEMBERS).collect()
    } else if program.get("kind").and_then(JsonValue::as_string) == Some("upsert_input_object") {
        TARGET_MEMBERS.into_iter().chain(["writeMode"]).collect()
    } else {
        TARGET_MEMBERS.to_vec()
    };
    reject_unknown_object_fields(
        program,
        &allowed,
        "payload.effectProgram",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_literal_field(
        program,
        "kind",
        &[
            "upsert_input_object",
            "fire_transition",
            "mutate_parent_with_rows",
        ],
        "payload.effectProgram.kind",
        diagnostics,
        artifact_id,
        CODE,
    );
    if fires_transition {
        for member in TRANSITION_MEMBERS {
            let path = format!("payload.effectProgram.{member}");
            if let Some(id) =
                require_non_empty_field(program, member, &path, diagnostics, artifact_id, CODE)
                && !crate::property_types::matches_grammar(
                    &crate::object_types::MEMBER_ID_GRAMMAR,
                    &id,
                )
            {
                diagnostics.push(kind_invalid(
                    &format!("{path} must match [a-z][a-z0-9_]*"),
                    &path,
                    artifact_id,
                    CODE,
                ));
            }
        }
    } else if mutates_rows {
        for member in ["parentValuesField", "rowOpsField", "parentVersionField"]
            .into_iter()
            .chain(
                program
                    .contains_key("parentDeleteField")
                    .then_some("parentDeleteField"),
            )
        {
            let path = format!("payload.effectProgram.{member}");
            require_non_empty_field(program, member, &path, diagnostics, artifact_id, CODE);
        }
        // Use the canonical closed enum for nested collection declarations,
        // including their row guard and derivation shapes.
        if let Err(error) =
            serde_json::to_value(value).and_then(serde_json::from_value::<ActionEffectProgram>)
        {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("payload.effectProgram is invalid: {error}"),
                "payload.effectProgram",
                artifact_id,
            ));
        }
    } else if program.get("kind").and_then(JsonValue::as_string) == Some("upsert_input_object") {
        validate_upsert_write_mode(program, diagnostics, artifact_id, CODE);
    }
    for (field, path) in [
        ("objectIdField", "payload.effectProgram.objectIdField"),
        ("entityArtifactId", "payload.effectProgram.entityArtifactId"),
        (
            "entityArtifactVersion",
            "payload.effectProgram.entityArtifactVersion",
        ),
    ] {
        require_non_empty_field(program, field, path, diagnostics, artifact_id, CODE);
    }
    if let Some(object_kind) = require_non_empty_field(
        program,
        "objectKind",
        "payload.effectProgram.objectKind",
        diagnostics,
        artifact_id,
        CODE,
    ) && !is_exact_semantic_identity(&object_kind)
    {
        diagnostics.push(kind_invalid(
            "payload.effectProgram.objectKind must be an exact identity without whitespace, control characters, or wildcards",
            "payload.effectProgram.objectKind",
            artifact_id,
            CODE,
        ));
    }

    let Some(object_id_field) = program.get("objectIdField").and_then(JsonValue::as_string) else {
        return;
    };
    let Some(input_schema) = input_schema.and_then(JsonValue::as_object) else {
        return;
    };
    let required = input_schema
        .get("required")
        .and_then(JsonValue::as_array)
        .is_some_and(|fields| {
            fields
                .iter()
                .any(|field| field.as_string() == Some(object_id_field))
        });
    let create_only_generated_id = program.get("kind").and_then(JsonValue::as_string)
        == Some("upsert_input_object")
        && program
            .get("writeMode")
            .and_then(JsonValue::as_object)
            .and_then(|mode| mode.get("kind"))
            .and_then(JsonValue::as_string)
            == Some("create_only");
    let string_field = input_schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .and_then(|properties| properties.get(object_id_field))
        .and_then(JsonValue::as_object)
        .and_then(|field| field.get("type"))
        .and_then(JsonValue::as_string)
        == Some("string");
    if (!required && !create_only_generated_id) || !string_field {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "objectIdField must name a string input field; it may be optional only for create_only upserts",
            "payload.effectProgram.objectIdField",
            artifact_id,
        ));
    }
}

/// Check the closed tagged shape of an optional upsert write mode (T2).
fn validate_upsert_write_mode(
    program: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let Some(value) = program.get("writeMode") else {
        return;
    };
    let Some(mode) = expect_object(
        value,
        "payload.effectProgram.writeMode",
        diagnostics,
        artifact_id,
        code,
    ) else {
        return;
    };
    let kind = mode.get("kind").and_then(JsonValue::as_string);
    let allowed: &[&str] = match kind {
        Some("upsert" | "create_only") => &["kind"],
        Some("draft_only") => &["kind", "lifecycle", "state"],
        _ => {
            diagnostics.push(invalid_diagnostic(
                code,
                "writeMode.kind must be upsert, create_only, or draft_only",
                "payload.effectProgram.writeMode.kind",
                artifact_id,
            ));
            return;
        }
    };
    reject_unknown_object_fields(
        mode,
        allowed,
        "payload.effectProgram.writeMode",
        diagnostics,
        artifact_id,
        code,
    );
    require_literal_field(
        mode,
        "kind",
        &["upsert", "create_only", "draft_only"],
        "payload.effectProgram.writeMode.kind",
        diagnostics,
        artifact_id,
        code,
    );
    if kind == Some("draft_only") {
        let lifecycle = require_non_empty_field(
            mode,
            "lifecycle",
            "payload.effectProgram.writeMode.lifecycle",
            diagnostics,
            artifact_id,
            code,
        );
        if let Some(lifecycle) = lifecycle
            && !crate::property_types::matches_grammar(
                &crate::object_types::MEMBER_ID_GRAMMAR,
                &lifecycle,
            )
        {
            diagnostics.push(kind_invalid(
                "payload.effectProgram.writeMode.lifecycle must match [a-z][a-z0-9_]*",
                "payload.effectProgram.writeMode.lifecycle",
                artifact_id,
                code,
            ));
        }
        require_non_empty_field(
            mode,
            "state",
            "payload.effectProgram.writeMode.state",
            diagnostics,
            artifact_id,
            code,
        );
    }
}

fn validate_setting_payload(
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
        (Some("text"), Some(JsonValue::String(_))) => {}
        (Some("number"), Some(JsonValue::Number(value))) if value.is_finite() => {}
        (Some("boolean"), Some(JsonValue::Bool(_))) => {}
        (Some("json"), Some(value)) => {
            validate_json_value(value, "payload.value", diagnostics, artifact_id)
        }
        (Some("text"), Some(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "payload.value must be a string for text settings",
            "payload.value",
            artifact_id,
        )),
        (Some("number"), Some(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "payload.value must be a finite number for number settings",
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

fn validate_action_scope_required(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "action.validation.invalid_payload";
    match value {
        Some(JsonValue::Array(fields)) if !fields.is_empty() => {
            let mut seen = BTreeSet::new();
            for (index, field) in fields.iter().enumerate() {
                match field {
                    JsonValue::String(value) if !value.trim().is_empty() => {
                        if !seen.insert(value.clone()) {
                            diagnostics.push(invalid_diagnostic(
                                CODE,
                                format!("duplicate required scope field '{value}'"),
                                format!("payload.scopeRequired[{index}]"),
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
                        format!("payload.scopeRequired[{index}] must be a non-empty string"),
                        format!("payload.scopeRequired[{index}]"),
                        artifact_id,
                    )),
                }
            }
        }
        Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "scopeRequired must be a non-empty array",
            "payload.scopeRequired",
            artifact_id,
        )),
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "scopeRequired must be an array",
            "payload.scopeRequired",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "scopeRequired is required",
            "payload.scopeRequired",
            artifact_id,
        )),
    }
}

fn validate_rule_payload(
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

fn validate_policy_payload(
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
            for (index, obligation) in obligations.iter().enumerate() {
                let path = format!("payload.obligations[{index}]");
                let Some(obligation) =
                    expect_object(obligation, path.clone(), diagnostics, artifact_id, CODE)
                else {
                    continue;
                };
                require_non_empty_field(
                    obligation,
                    "obligationId",
                    format!("{path}.obligationId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    obligation,
                    "appliesTo",
                    format!("{path}.appliesTo"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_string_array_field(
                    obligation,
                    "evidenceRequired",
                    format!("{path}.evidenceRequired"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_string_array_field(
                    obligation,
                    "reviewRoles",
                    format!("{path}.reviewRoles"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
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

fn validate_business_pack_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "business_pack.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    require_non_empty_field(
        payload,
        "packId",
        "payload.packId",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_non_empty_field(
        payload,
        "packVersion",
        "payload.packVersion",
        diagnostics,
        artifact_id,
        CODE,
    );

    // IR 0.1 composition packs predate the discriminator. Preserve their wire compatibility;
    // ontology packs remain explicit and all newly authored packs should emit `packKind`.
    let pack_kind = if payload.contains_key("packKind") {
        require_literal_field(
            payload,
            "packKind",
            &["composition", "ontology", "link"],
            "payload.packKind",
            diagnostics,
            artifact_id,
            CODE,
        )
    } else {
        Some("composition".to_string())
    };

    match pack_kind.as_deref() {
        Some("composition") => {
            reject_unknown_object_fields(
                payload,
                &["packId", "packVersion", "packKind", "provides", "artifacts"],
                "payload",
                diagnostics,
                artifact_id,
                CODE,
            );
            validate_composition_pack_payload(payload, diagnostics, artifact_id);
        }
        Some("ontology") => {
            ontology::validate_ontology_pack_payload(value, diagnostics, artifact_id);
        }
        Some("link") => ontology::validate_link_payload(value, diagnostics, artifact_id),
        _ => {}
    }
}

fn validate_composition_pack_payload(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "business_pack.validation.invalid_payload";

    if let Some(provides) = payload.get("provides") {
        validate_string_array(provides, "payload.provides", diagnostics, artifact_id, CODE);
    }

    match payload.get("artifacts") {
        Some(JsonValue::Array(artifacts)) if !artifacts.is_empty() => {
            for (index, artifact_ref) in artifacts.iter().enumerate() {
                let path = format!("payload.artifacts[{index}]");
                let Some(artifact_ref) =
                    expect_object(artifact_ref, &path, diagnostics, artifact_id, CODE)
                else {
                    continue;
                };
                require_non_empty_field(
                    artifact_ref,
                    "artifactId",
                    format!("{path}.artifactId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_literal_field(
                    artifact_ref,
                    "kind",
                    &[
                        "entity",
                        "relation",
                        "action",
                        "rule",
                        "connector",
                        "widget",
                        "customization",
                        "setting",
                        "business_pack",
                        "policy",
                        "source_schema",
                        "mapping",
                    ],
                    format!("{path}.kind"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    artifact_ref,
                    "version",
                    format!("{path}.version"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
        }
        Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "artifacts must be a non-empty array",
            "payload.artifacts",
            artifact_id,
        )),
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "artifacts must be an array",
            "payload.artifacts",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "artifacts is required",
            "payload.artifacts",
            artifact_id,
        )),
    }
}

fn validate_customization_payload(
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
                let path = format!("payload.overlay.operations[{index}]");
                let Some(operation) =
                    expect_object(operation, path.clone(), diagnostics, artifact_id, CODE)
                else {
                    continue;
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
                    CODE,
                );
                match operation_kind.as_deref() {
                    Some("add_field" | "update_field") => {
                        require_non_empty_field(
                            operation,
                            "field",
                            format!("{path}.field"),
                            diagnostics,
                            artifact_id,
                            CODE,
                        );
                        validate_json_object_field(
                            operation,
                            "definition",
                            format!("{path}.definition"),
                            diagnostics,
                            artifact_id,
                            CODE,
                        );
                    }
                    Some("custom_hook") => {
                        requires_executable = true;
                    }
                    _ => {}
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
    const RETIRED_BOUNDARY_CODE: &str = "customization.validation.retired_boundary";
    let Some(executable) =
        expect_object(value, "payload.executable", diagnostics, artifact_id, CODE)
    else {
        return;
    };
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
                CODE,
            );
        }
    }
    validate_json_object_field(
        executable,
        "inputSchema",
        "payload.executable.inputSchema",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_json_object_field(
        executable,
        "outputSchema",
        "payload.executable.outputSchema",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_json_object_field(
        executable,
        "errorSchema",
        "payload.executable.errorSchema",
        diagnostics,
        artifact_id,
        CODE,
    );
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

    reject_truthy_boolean_field(
        executable,
        "ambientCredentials",
        "payload.executable.ambientCredentials",
        diagnostics,
        artifact_id,
        CODE,
    );
    reject_truthy_boolean_field(
        executable,
        "directDatabaseAccess",
        "payload.executable.directDatabaseAccess",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_optional_non_empty_json_field(
        executable,
        "sourceRef",
        "payload.executable.sourceRef",
        diagnostics,
        artifact_id,
        CODE,
    );
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

fn validate_scope_ref(
    scope: &ScopeRef,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    validate_optional_non_empty(&scope.org_id, "scope.orgId", diagnostics, artifact_id);
    validate_optional_non_empty(&scope.tenant_id, "scope.tenantId", diagnostics, artifact_id);
    validate_optional_non_empty(
        &scope.workspace_id,
        "scope.workspaceId",
        diagnostics,
        artifact_id,
    );
    validate_optional_non_empty(
        &scope.environment_id,
        "scope.environmentId",
        diagnostics,
        artifact_id,
    );
    validate_optional_non_empty(
        &scope.business_unit_id,
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
        &provenance.source_path,
        "provenance.sourcePath",
        diagnostics,
        artifact_id,
    );
    validate_optional_non_empty(
        &provenance.actor_id,
        "provenance.actorId",
        diagnostics,
        artifact_id,
    );
    validate_optional_non_empty(
        &provenance.created_at,
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

fn validate_connector_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    require_literal_field(
        payload,
        "connectorVersion",
        &["0.1"],
        "payload.connectorVersion",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_connector_auth(payload.get("auth"), diagnostics, artifact_id);
    crate::source_commands::validate_connector_profile(payload, diagnostics, artifact_id);
    if let Some(commands) = payload.get("commands") {
        validate_connector_commands(commands, diagnostics, artifact_id);
    }

    let mut resource_ids = BTreeSet::new();
    match payload.get("resources") {
        Some(JsonValue::Array(resources))
            if !resources.is_empty()
                || payload
                    .get("commands")
                    .and_then(JsonValue::as_array)
                    .is_some_and(|c| !c.is_empty()) =>
        {
            for (index, resource) in resources.iter().enumerate() {
                validate_connector_resource(
                    resource,
                    index,
                    diagnostics,
                    artifact_id,
                    &mut resource_ids,
                );
            }
        }
        Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "resources must be a non-empty array",
            "payload.resources",
            artifact_id,
        )),
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "resources must be an array",
            "payload.resources",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "resources is required",
            "payload.resources",
            artifact_id,
        )),
    }

    validate_connector_mappings(
        payload.get("mappings"),
        diagnostics,
        artifact_id,
        &resource_ids,
    );
    validate_catalog_connector_extensions(payload.get("extensions"), diagnostics, artifact_id);
    validate_connector_poll_profile(
        payload.get("extensions"),
        payload.get("resources"),
        diagnostics,
        artifact_id,
    );

    if let Some(cursor) = payload.get("cursor") {
        validate_connector_cursor_descriptor(cursor, diagnostics, artifact_id, &resource_ids);
    }
    if let Some(policy) = payload.get("deadLetterPolicy") {
        validate_dead_letter_policy(policy, diagnostics, artifact_id);
    }
    if let Some(hook_ids) = payload.get("customStepHookIds") {
        validate_string_array(
            hook_ids,
            "payload.customStepHookIds",
            diagnostics,
            artifact_id,
            CODE,
        );
    }
    if let Some(hooks) = payload.get("customStepHooks") {
        validate_connector_custom_step_hooks(hooks, diagnostics, artifact_id);
    }
    if let Some(proposal) = payload.get("proposal") {
        validate_proposal_metadata(proposal, "payload.proposal", diagnostics, artifact_id, CODE);
    }
}

fn validate_connector_poll_profile(
    extensions: Option<&JsonValue>,
    resources: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(JsonValue::Object(extensions)) = extensions else {
        return;
    };
    let Some(value) = extensions.get(norn_ir::connector::CONNECTOR_POLL_EXTENSION_KEY) else {
        return;
    };
    let path = format!(
        "payload.extensions.{}",
        norn_ir::connector::CONNECTOR_POLL_EXTENSION_KEY
    );
    let profile = serde_json::to_string(value).ok().and_then(|text| {
        serde_json::from_str::<norn_ir::connector::ConnectorPollProfile>(&text).ok()
    });
    let Some(profile) = profile else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "the norn.poll/1 extension must be an ordered feed profile",
            path,
            artifact_id,
        ));
        return;
    };
    if profile.feeds.is_empty() || profile.feeds.len() > 32 {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "the poll profile must contain between 1 and 32 feeds",
            format!("{path}.feeds"),
            artifact_id,
        ));
        return;
    }

    let mut resources_by_id = BTreeMap::new();
    if let Some(JsonValue::Array(resources)) = resources {
        for resource in resources {
            let JsonValue::Object(resource) = resource else {
                continue;
            };
            let (Some(JsonValue::String(resource_id)), Some(JsonValue::String(kind))) =
                (resource.get("resourceId"), resource.get("kind"))
            else {
                continue;
            };
            let endpoint = resource.get("endpoint").and_then(JsonValue::as_string);
            resources_by_id.insert(
                resource_id.clone(),
                (kind.as_str(), endpoint, resource.get("queryParams")),
            );
        }
    }

    let mut seen_resources = BTreeSet::new();
    for (index, feed) in profile.feeds.iter().enumerate() {
        let feed_path = format!("{path}.feeds[{index}]");
        let valid_id = !feed.resource_id.trim().is_empty()
            && feed.resource_id.len() <= 256
            && !feed.resource_id.chars().any(char::is_control)
            && seen_resources.insert(feed.resource_id.as_str());
        if !valid_id {
            diagnostics.push(invalid_diagnostic(
                CODE,
                "poll feed resource ids must be non-empty and unique",
                format!("{feed_path}.resourceId"),
                artifact_id,
            ));
            continue;
        }
        match resources_by_id.get(feed.resource_id.as_str()) {
            Some(("cursor" | "paginated" | "delta", _, _)) => {}
            _ => {
                diagnostics.push(invalid_diagnostic(
                    CODE,
                    "poll feeds must reference a declared readable resource",
                    format!("{feed_path}.resourceId"),
                    artifact_id,
                ));
            }
        }
        if let Some(record_key_path) = &feed.record_key_path
            && let Some(error) = poll_value_path_error(record_key_path)
        {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("recordKeyPath is invalid: {error}"),
                format!("{feed_path}.recordKeyPath"),
                artifact_id,
            ));
        }
        if let Some(fanout) = &feed.fanout {
            if !profile.feeds.iter().take(index).any(|parent| {
                parent.resource_id == fanout.parent_resource_id && parent.fanout.is_none()
            }) {
                diagnostics.push(invalid_diagnostic(
                    CODE,
                    "a fanout parent must be an earlier feed in the profile",
                    format!("{feed_path}.fanout.parentResourceId"),
                    artifact_id,
                ));
            }
            let binding = fanout.path_binding.trim();
            let request_has_binding = resources_by_id.get(feed.resource_id.as_str()).is_some_and(
                |(_, endpoint, query)| {
                    let placeholder = format!("{{{binding}}}");
                    endpoint.is_some_and(|endpoint| endpoint.contains(&placeholder))
                        || query.and_then(JsonValue::as_object).is_some_and(|query| {
                            query
                                .values()
                                .filter_map(JsonValue::as_string)
                                .any(|value| value.contains(&placeholder))
                        })
                },
            );
            if binding != fanout.path_binding
                || binding.is_empty()
                || binding.len() > 128
                || binding.chars().any(char::is_control)
                || !request_has_binding
            {
                diagnostics.push(invalid_diagnostic(
                    CODE,
                    "fanout requires a valid binding name and a matching child request binding",
                    format!("{feed_path}.fanout"),
                    artifact_id,
                ));
            }
            if let Some(error) = poll_value_path_error(&fanout.key_path) {
                diagnostics.push(invalid_diagnostic(
                    CODE,
                    format!("fanout keyPath is invalid: {error}"),
                    format!("{feed_path}.fanout.keyPath"),
                    artifact_id,
                ));
            }
        }
    }
}

fn poll_value_path_error(path: &str) -> Option<String> {
    if path.is_empty() {
        return Some("path must not be empty".to_owned());
    }
    if path.len() > 1_024 {
        return Some("path exceeds 1,024 bytes".to_owned());
    }
    ConnectorValuePath::parse(path)
        .err()
        .map(|error| error.to_string())
}

fn validate_connector_custom_step_hooks(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    match value {
        JsonValue::Array(hooks) => {
            for (index, hook) in hooks.iter().enumerate() {
                let path = format!("payload.customStepHooks[{index}]");
                let Some(hook) = expect_object(hook, &path, diagnostics, artifact_id, CODE) else {
                    continue;
                };

                require_non_empty_field(
                    hook,
                    "hookId",
                    format!("{path}.hookId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    hook,
                    "installId",
                    format!("{path}.installId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    hook,
                    "extensionVersion",
                    format!("{path}.extensionVersion"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_string_array_field(
                    hook,
                    "requiredServices",
                    format!("{path}.requiredServices"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_string_array_field(
                    hook,
                    "scopeCapabilities",
                    format!("{path}.scopeCapabilities"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_json_object_field(
                    hook,
                    "inputSchema",
                    format!("{path}.inputSchema"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_json_object_field(
                    hook,
                    "outputSchema",
                    format!("{path}.outputSchema"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_json_object_field(
                    hook,
                    "errorSchema",
                    format!("{path}.errorSchema"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
        }
        JsonValue::Null
        | JsonValue::Bool(_)
        | JsonValue::Number(_)
        | JsonValue::String(_)
        | JsonValue::Object(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "customStepHooks must be an array",
            "payload.customStepHooks",
            artifact_id,
        )),
    }
}

fn validate_connector_auth(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let auth_path = "payload.auth";
    let Some(auth) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "auth is required",
            "payload.auth",
            artifact_id,
        ));
        return;
    };
    let Some(auth) = expect_object(auth, "payload.auth", diagnostics, artifact_id, CODE) else {
        return;
    };
    let kind = require_literal_field(
        auth,
        "kind",
        &["none", "oauth2", "api_key"],
        "payload.auth.kind",
        diagnostics,
        artifact_id,
        CODE,
    );

    match kind.as_deref() {
        Some("oauth2") => {
            require_non_empty_field(
                auth,
                "authId",
                "payload.auth.authId",
                diagnostics,
                artifact_id,
                CODE,
            );
            require_non_empty_field(
                auth,
                "authorizationUrl",
                "payload.auth.authorizationUrl",
                diagnostics,
                artifact_id,
                CODE,
            );
            require_non_empty_field(
                auth,
                "tokenUrl",
                "payload.auth.tokenUrl",
                diagnostics,
                artifact_id,
                CODE,
            );
            validate_string_array_field(
                auth,
                "scopes",
                "payload.auth.scopes",
                diagnostics,
                artifact_id,
                CODE,
            );
            validate_optional_non_empty_json_field(
                auth,
                "clientSecretRef",
                "payload.auth.clientSecretRef",
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        Some("api_key") => {
            require_non_empty_field(
                auth,
                "authId",
                "payload.auth.authId",
                diagnostics,
                artifact_id,
                CODE,
            );
            require_literal_field(
                auth,
                "placement",
                &["header", "query"],
                "payload.auth.placement",
                diagnostics,
                artifact_id,
                CODE,
            );
            require_non_empty_field(
                auth,
                "name",
                "payload.auth.name",
                diagnostics,
                artifact_id,
                CODE,
            );
            require_non_empty_field(
                auth,
                "secretRef",
                "payload.auth.secretRef",
                diagnostics,
                artifact_id,
                CODE,
            );
            validate_optional_non_empty_json_field(
                auth,
                "prefix",
                format!("{auth_path}.prefix"),
                diagnostics,
                artifact_id,
                CODE,
            );
            if let Some(format) = auth.get("format") {
                let format_path = format!("{auth_path}.format");
                if let Some(format) =
                    expect_object(format, format_path.clone(), diagnostics, artifact_id, CODE)
                {
                    match require_literal_field(
                        format,
                        "kind",
                        &["raw", "pair"],
                        format!("{format_path}.kind"),
                        diagnostics,
                        artifact_id,
                        CODE,
                    )
                    .as_deref()
                    {
                        Some("pair") => {
                            require_non_empty_field(
                                format,
                                "separator",
                                format!("{format_path}.separator"),
                                diagnostics,
                                artifact_id,
                                CODE,
                            );
                        }
                        Some("raw") => {}
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}

fn validate_connector_resource(
    value: &JsonValue,
    index: usize,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    resource_ids: &mut BTreeSet<String>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let path = format!("payload.resources[{index}]");
    let Some(resource) = expect_object(value, path.clone(), diagnostics, artifact_id, CODE) else {
        return;
    };
    validate_optional_non_empty_json_field(
        resource,
        "commandKeyHeader",
        format!("{path}.commandKeyHeader"),
        diagnostics,
        artifact_id,
        CODE,
    );
    if let Some(value) = resource.get("missingItemsIsEmpty")
        && !matches!(value, JsonValue::Bool(_))
    {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "missingItemsIsEmpty must be a boolean",
            format!("{path}.missingItemsIsEmpty"),
            artifact_id,
        ));
    }
    let kind = require_literal_field(
        resource,
        "kind",
        &["paginated", "cursor", "delta", "webhook", "csv_import"],
        format!("{path}.kind"),
        diagnostics,
        artifact_id,
        CODE,
    );
    if let Some(resource_id) = require_non_empty_field(
        resource,
        "resourceId",
        format!("{path}.resourceId"),
        diagnostics,
        artifact_id,
        CODE,
    ) && !resource_ids.insert(resource_id)
    {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "resource ids must be unique",
            format!("{path}.resourceId"),
            artifact_id,
        ));
    }
    if resource.contains_key("queryParams") {
        validate_connector_query_params(
            resource,
            "queryParams",
            format!("{path}.queryParams"),
            diagnostics,
            artifact_id,
        );
    }

    match kind.as_deref() {
        Some("paginated") => {
            require_non_empty_field(
                resource,
                "endpoint",
                format!("{path}.endpoint"),
                diagnostics,
                artifact_id,
                CODE,
            );
            require_literal_field(
                resource,
                "method",
                &["GET", "POST"],
                format!("{path}.method"),
                diagnostics,
                artifact_id,
                CODE,
            );
            require_non_empty_field(
                resource,
                "pageParam",
                format!("{path}.pageParam"),
                diagnostics,
                artifact_id,
                CODE,
            );
            validate_optional_non_empty_json_field(
                resource,
                "pageSizeParam",
                format!("{path}.pageSizeParam"),
                diagnostics,
                artifact_id,
                CODE,
            );
            require_non_empty_field(
                resource,
                "itemsPath",
                format!("{path}.itemsPath"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        Some("cursor") => {
            for field in ["endpoint", "cursorParam", "nextCursorPath", "itemsPath"] {
                require_non_empty_field(
                    resource,
                    field,
                    format!("{path}.{field}"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
        }
        Some("delta") => {
            for field in ["endpoint", "sinceParam", "updatedAtPath", "itemsPath"] {
                require_non_empty_field(
                    resource,
                    field,
                    format!("{path}.{field}"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
            if let Some(keyset) = resource.get("keyset") {
                validate_connector_keyset_profile(keyset, &path, diagnostics, artifact_id);
            }
        }
        Some("webhook") => {
            require_non_empty_field(
                resource,
                "eventType",
                format!("{path}.eventType"),
                diagnostics,
                artifact_id,
                CODE,
            );
            require_non_empty_field(
                resource,
                "payloadPath",
                format!("{path}.payloadPath"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        Some("csv_import") => {
            require_literal_field(
                resource,
                "delimiter",
                &[",", ";", "\t"],
                format!("{path}.delimiter"),
                diagnostics,
                artifact_id,
                CODE,
            );
            validate_string_array_field(
                resource,
                "columns",
                format!("{path}.columns"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        _ => {}
    }
}

fn validate_connector_query_params(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: String,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(JsonValue::Object(parameters)) = object.get(field) else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            format!("{path} must be an object of string values"),
            path,
            artifact_id,
        ));
        return;
    };
    for (key, value) in parameters {
        if key.trim().is_empty() || !matches!(value, JsonValue::String(_)) {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("{path} keys and values must be non-empty strings"),
                path,
                artifact_id,
            ));
            return;
        }
    }
}

fn validate_connector_keyset_profile(
    value: &JsonValue,
    resource_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let path = format!("{resource_path}.keyset");
    let Some(profile) = expect_object(value, path.clone(), diagnostics, artifact_id, CODE) else {
        return;
    };
    for field in [
        "filtersParam",
        "orFilterParam",
        "fieldsParam",
        "orderParam",
        "pageParam",
        "sizeParam",
        "watermarkField",
        "tieBreakerField",
    ] {
        require_non_empty_field(
            profile,
            field,
            format!("{path}.{field}"),
            diagnostics,
            artifact_id,
            CODE,
        );
    }
    validate_unique_string_array_field(
        profile,
        "fields",
        format!("{path}.fields"),
        diagnostics,
        artifact_id,
        CODE,
    );
    match profile.get("pageSize") {
        Some(JsonValue::Number(size))
            if size.fract() == 0.0 && *size > 0.0 && *size <= f64::from(u32::MAX) => {}
        _ => diagnostics.push(invalid_diagnostic(
            CODE,
            "pageSize must be a positive 32-bit integer",
            format!("{path}.pageSize"),
            artifact_id,
        )),
    }
}

fn validate_connector_commands(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let JsonValue::Array(commands) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "commands must be an array",
            "payload.commands",
            artifact_id,
        ));
        return;
    };
    let mut identities: BTreeSet<(String, String, Vec<String>)> = BTreeSet::new();
    for (index, command_value) in commands.iter().enumerate() {
        let path = format!("payload.commands[{index}]");
        let Some(command) =
            expect_object(command_value, path.clone(), diagnostics, artifact_id, CODE)
        else {
            continue;
        };
        let operation = require_literal_field(
            command,
            "operation",
            &["create", "submit", "update", "cancel"],
            format!("{path}.operation"),
            diagnostics,
            artifact_id,
            CODE,
        );
        let action_id = command
            .get("actionId")
            .and_then(JsonValue::as_string)
            .unwrap_or("")
            .to_owned();
        let doctypes = match command.get("doctypes") {
            Some(JsonValue::Array(doctypes)) => doctypes
                .iter()
                .filter_map(JsonValue::as_string)
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        };
        let identity = (
            operation.as_deref().unwrap_or("").to_owned(),
            action_id,
            doctypes,
        );
        if !identities.insert(identity) {
            diagnostics.push(invalid_diagnostic(
                CODE,
                "command profiles must be unique by operation, actionId, and doctypes",
                format!("{path}.operation"),
                artifact_id,
            ));
        }
        let endpoint = require_non_empty_field(
            command,
            "endpoint",
            format!("{path}.endpoint"),
            diagnostics,
            artifact_id,
            CODE,
        );
        if endpoint
            .as_deref()
            .is_some_and(|endpoint| endpoint.starts_with("//") || endpoint.contains("://"))
        {
            diagnostics.push(invalid_diagnostic(
                CODE,
                "endpoint must be relative to the host-bound connector URL",
                format!("{path}.endpoint"),
                artifact_id,
            ));
        }
        let method = require_literal_field(
            command,
            "method",
            &["GET", "POST", "PUT", "DELETE"],
            format!("{path}.method"),
            diagnostics,
            artifact_id,
            CODE,
        );
        if method.as_deref() == Some("GET") {
            diagnostics.push(invalid_diagnostic(
                CODE,
                "connector command mutations cannot use GET",
                format!("{path}.method"),
                artifact_id,
            ));
        }
        if command.contains_key("actionId") {
            validate_optional_non_empty_json_field(
                command,
                "actionId",
                format!("{path}.actionId"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        if command.contains_key("doctypes") {
            validate_unique_string_array_field(
                command,
                "doctypes",
                format!("{path}.doctypes"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        if command.contains_key("bodyFields") {
            validate_unique_string_array_field(
                command,
                "bodyFields",
                format!("{path}.bodyFields"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        if command.contains_key("fixedBody") {
            validate_json_object_field(
                command,
                "fixedBody",
                format!("{path}.fixedBody"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        if let Some(idempotency) = command.get("idempotency") {
            validate_connector_idempotency(idempotency, &path, diagnostics, artifact_id);
        }
        if let Some(lifecycle) = command.get("lifecycle") {
            validate_connector_lifecycle(lifecycle, &path, diagnostics, artifact_id);
        }
        if let Some(conflicts) = command.get("conflicts") {
            validate_connector_conflicts(conflicts, &path, diagnostics, artifact_id);
        }
        validate_connector_body_allowlist(command, &path, diagnostics, artifact_id);
    }
}

fn validate_connector_body_allowlist(
    command: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let mut reserved = BTreeSet::new();
    if let Some(idempotency) = command.get("idempotency").and_then(JsonValue::as_object) {
        if let Some(field) = idempotency.get("keyField").and_then(JsonValue::as_string) {
            reserved.insert(field.to_owned());
        }
        if let Some(field) = idempotency
            .get("receipt")
            .and_then(JsonValue::as_object)
            .and_then(|receipt| receipt.get("tableField"))
            .and_then(JsonValue::as_string)
        {
            reserved.insert(field.to_owned());
        }
    }
    if let Some(lifecycle) = command.get("lifecycle").and_then(JsonValue::as_object) {
        for field in ["versionField", "statusField"] {
            if let Some(value) = lifecycle.get(field).and_then(JsonValue::as_string) {
                reserved.insert(value.to_owned());
            }
        }
    }
    let fixed = command
        .get("fixedBody")
        .and_then(JsonValue::as_object)
        .map(|fields| fields.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    if let Some(JsonValue::Array(fields)) = command.get("bodyFields") {
        for (index, value) in fields.iter().enumerate() {
            let Some(field) = value.as_string() else {
                continue;
            };
            if reserved.contains(field) || fixed.contains(field) {
                diagnostics.push(invalid_diagnostic(
                    CODE,
                    "bodyFields cannot override fixed or reserved connector fields",
                    format!("{path}.bodyFields[{index}]"),
                    artifact_id,
                ));
            }
        }
    }
}

fn validate_connector_idempotency(
    value: &JsonValue,
    command_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let path = format!("{command_path}.idempotency");
    let Some(profile) = expect_object(value, path.clone(), diagnostics, artifact_id, CODE) else {
        return;
    };
    require_non_empty_field(
        profile,
        "keyField",
        format!("{path}.keyField"),
        diagnostics,
        artifact_id,
        CODE,
    );
    for field in ["headerName", "headerPrefix"] {
        if profile.contains_key(field) {
            validate_optional_non_empty_json_field(
                profile,
                field,
                format!("{path}.{field}"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
    }
    let receipt_path = format!("{path}.receipt");
    if let Some(receipt) = profile.get("receipt")
        && let Some(receipt) = expect_object(
            receipt,
            receipt_path.clone(),
            diagnostics,
            artifact_id,
            CODE,
        )
    {
        for field in [
            "doctype",
            "tableField",
            "keyField",
            "operationField",
            "parentField",
            "parentTypeField",
            "parentTableField",
        ] {
            require_non_empty_field(
                receipt,
                field,
                format!("{receipt_path}.{field}"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        for field in [
            "identityField",
            "actionIdField",
            "doctypeField",
            "targetNameField",
        ] {
            if receipt.contains_key(field) {
                validate_optional_non_empty_json_field(
                    receipt,
                    field,
                    format!("{receipt_path}.{field}"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
        }
        if receipt.contains_key("timestampField") {
            validate_optional_non_empty_json_field(
                receipt,
                "timestampField",
                format!("{receipt_path}.timestampField"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
    } else if !profile.contains_key("receipt") {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "receipt is required",
            receipt_path,
            artifact_id,
        ));
    }
    for lookup_name in ["createLookup", "receiptLookup"] {
        let lookup_path = format!("{path}.{lookup_name}");
        match profile.get(lookup_name) {
            Some(lookup) => {
                validate_connector_lookup(lookup, &lookup_path, diagnostics, artifact_id)
            }
            None => diagnostics.push(invalid_diagnostic(
                CODE,
                format!("{lookup_name} is required"),
                lookup_path,
                artifact_id,
            )),
        }
    }
}

fn validate_connector_lookup(
    value: &JsonValue,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(lookup) = expect_object(value, path, diagnostics, artifact_id, CODE) else {
        return;
    };
    for field in [
        "endpoint",
        "itemsPath",
        "filtersParam",
        "fieldsParam",
        "limitParam",
    ] {
        require_non_empty_field(
            lookup,
            field,
            format!("{path}.{field}"),
            diagnostics,
            artifact_id,
            CODE,
        );
    }
    validate_unique_string_array_field(
        lookup,
        "fields",
        format!("{path}.fields"),
        diagnostics,
        artifact_id,
        CODE,
    );
    // Request N+1 results so either idempotency lookup can expose a duplicate.
    match lookup.get("maxResults") {
        Some(JsonValue::Number(max))
            if max.fract() == 0.0 && *max >= 2.0 && *max <= f64::from(u16::MAX) => {}
        _ => diagnostics.push(invalid_diagnostic(
            CODE,
            format!(
                "{path}.maxResults must be an integer from 2 through {} to detect duplicate matches",
                u16::MAX
            ),
            format!("{path}.maxResults"),
            artifact_id,
        )),
    }
    match lookup.get("filters") {
        None => {}
        Some(JsonValue::Array(filters)) => {
            for (index, filter_value) in filters.iter().enumerate() {
                let filter_path = format!("{path}.filters[{index}]");
                let Some(filter) = expect_object(
                    filter_value,
                    filter_path.clone(),
                    diagnostics,
                    artifact_id,
                    CODE,
                ) else {
                    continue;
                };
                if filter.contains_key("entity") {
                    validate_optional_non_empty_json_field(
                        filter,
                        "entity",
                        format!("{filter_path}.entity"),
                        diagnostics,
                        artifact_id,
                        CODE,
                    );
                }
                require_non_empty_field(
                    filter,
                    "field",
                    format!("{filter_path}.field"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                if filter.contains_key("operator") {
                    validate_optional_non_empty_json_field(
                        filter,
                        "operator",
                        format!("{filter_path}.operator"),
                        diagnostics,
                        artifact_id,
                        CODE,
                    );
                }
                require_literal_field(
                    filter,
                    "value",
                    &[
                        "idempotency_key",
                        "operation",
                        "action_id",
                        "document_type",
                        "target_name",
                        "parent_type",
                        "parent_field",
                    ],
                    format!("{filter_path}.value"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
        }
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "filters must be an array",
            format!("{path}.filters"),
            artifact_id,
        )),
    }
}

fn validate_connector_lifecycle(
    value: &JsonValue,
    command_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let path = format!("{command_path}.lifecycle");
    let Some(lifecycle) = expect_object(value, path.clone(), diagnostics, artifact_id, CODE) else {
        return;
    };
    for field in [
        "documentPath",
        "identityField",
        "versionField",
        "statusField",
    ] {
        require_non_empty_field(
            lifecycle,
            field,
            format!("{path}.{field}"),
            diagnostics,
            artifact_id,
            CODE,
        );
    }
    for field in ["initialStatus", "submittedStatus", "cancelledStatus"] {
        if !matches!(lifecycle.get(field), Some(JsonValue::Number(value)) if value.fract() == 0.0) {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("{path}.{field} must be an integer"),
                format!("{path}.{field}"),
                artifact_id,
            ));
        }
    }
}

fn validate_connector_conflicts(
    value: &JsonValue,
    command_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let path = format!("{command_path}.conflicts");
    let JsonValue::Array(conflicts) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "conflicts must be an array",
            path,
            artifact_id,
        ));
        return;
    };
    for (index, conflict_value) in conflicts.iter().enumerate() {
        let conflict_path = format!("{path}[{index}]");
        let Some(conflict) = expect_object(
            conflict_value,
            conflict_path.clone(),
            diagnostics,
            artifact_id,
            CODE,
        ) else {
            continue;
        };
        match conflict.get("status") {
            Some(JsonValue::Number(status))
                if status.fract() == 0.0 && (400.0..=599.0).contains(status) => {}
            _ => diagnostics.push(invalid_diagnostic(
                CODE,
                "status must be an HTTP error status",
                format!("{conflict_path}.status"),
                artifact_id,
            )),
        }
        if conflict.contains_key("errorType") {
            validate_optional_non_empty_json_field(
                conflict,
                "errorType",
                format!("{conflict_path}.errorType"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        require_literal_field(
            conflict,
            "category",
            &["duplicate", "unique", "version"],
            format!("{conflict_path}.category"),
            diagnostics,
            artifact_id,
            CODE,
        );
    }
}

fn validate_connector_mappings(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    resource_ids: &BTreeSet<String>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    match value {
        Some(JsonValue::Array(mappings)) => {
            for (index, mapping) in mappings.iter().enumerate() {
                let path = format!("payload.mappings[{index}]");
                let Some(mapping) =
                    expect_object(mapping, path.clone(), diagnostics, artifact_id, CODE)
                else {
                    continue;
                };
                let kind = require_literal_field(
                    mapping,
                    "kind",
                    &["field", "external_id"],
                    format!("{path}.kind"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                if let Some(resource_id) = require_non_empty_field(
                    mapping,
                    "resourceId",
                    format!("{path}.resourceId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                ) {
                    validate_resource_reference(
                        &resource_id,
                        format!("{path}.resourceId"),
                        diagnostics,
                        artifact_id,
                        resource_ids,
                    );
                }
                require_non_empty_field(
                    mapping,
                    "sourcePath",
                    format!("{path}.sourcePath"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    mapping,
                    "targetObjectKind",
                    format!("{path}.targetObjectKind"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );

                match kind.as_deref() {
                    Some("field") => {
                        require_non_empty_field(
                            mapping,
                            "targetField",
                            format!("{path}.targetField"),
                            diagnostics,
                            artifact_id,
                            CODE,
                        );
                    }
                    Some("external_id") => {
                        require_non_empty_field(
                            mapping,
                            "externalSource",
                            format!("{path}.externalSource"),
                            diagnostics,
                            artifact_id,
                            CODE,
                        );
                    }
                    _ => {}
                }
            }
        }
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "mappings must be an array",
            "payload.mappings",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "mappings is required",
            "payload.mappings",
            artifact_id,
        )),
    }
}

fn validate_connector_cursor_descriptor(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    resource_ids: &BTreeSet<String>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(cursor) = expect_object(value, "payload.cursor", diagnostics, artifact_id, CODE)
    else {
        return;
    };
    require_non_empty_field(
        cursor,
        "cursorId",
        "payload.cursor.cursorId",
        diagnostics,
        artifact_id,
        CODE,
    );
    if let Some(resource_id) = require_non_empty_field(
        cursor,
        "resourceId",
        "payload.cursor.resourceId",
        diagnostics,
        artifact_id,
        CODE,
    ) {
        validate_resource_reference(
            &resource_id,
            "payload.cursor.resourceId",
            diagnostics,
            artifact_id,
            resource_ids,
        );
    }
    require_non_empty_field(
        cursor,
        "storageKey",
        "payload.cursor.storageKey",
        diagnostics,
        artifact_id,
        CODE,
    );
}

fn validate_dead_letter_policy(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(policy) = expect_object(
        value,
        "payload.deadLetterPolicy",
        diagnostics,
        artifact_id,
        CODE,
    ) else {
        return;
    };
    match policy.get("maxFailures") {
        Some(JsonValue::Number(value)) if value.fract() == 0.0 && *value >= 1.0 => {}
        _ => diagnostics.push(invalid_diagnostic(
            CODE,
            "deadLetterPolicy.maxFailures must be a positive integer",
            "payload.deadLetterPolicy.maxFailures",
            artifact_id,
        )),
    }
    require_literal_field(
        policy,
        "route",
        &["ledger", "queue"],
        "payload.deadLetterPolicy.route",
        diagnostics,
        artifact_id,
        CODE,
    );
}

fn validate_widget_payload(
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

fn validate_widget_binding_dependencies(
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
    }
}

fn validate_graph_scope(request: &GraphLoadRequest, diagnostics: &mut Vec<Diagnostic>) {
    for key in &request.scope_required {
        if !graph_scope_has(&request.scope, key) {
            diagnostics.push(graph_diagnostic(
                "graph_load.validation.missing_scope",
                format!("required scope '{key}' is missing"),
                format!("scope.{key}"),
            ));
        }
    }
}

fn graph_scope_has(scope: &ScopeRef, key: &str) -> bool {
    match key {
        "orgId" | "org_id" => scope
            .org_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        "tenantId" | "tenant_id" => scope
            .tenant_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        "workspaceId" | "workspace_id" => scope
            .workspace_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        "environmentId" | "environment_id" => scope
            .environment_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        "businessUnitId" | "business_unit_id" => scope
            .business_unit_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        _ => false,
    }
}

fn validate_graph_shape(shape: &GraphReadShape, diagnostics: &mut Vec<Diagnostic>) {
    if shape.root_object_kind.trim().is_empty() {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.invalid_shape",
            "rootObjectKind must be present",
            "shape.rootObjectKind",
        ));
    }
    if shape.root_ids.is_empty() {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.invalid_shape",
            "rootIds must not be empty",
            "shape.rootIds",
        ));
    }

    for (index, relation) in shape.relations.iter().enumerate() {
        validate_graph_relation_shape(relation, &format!("shape.relations[{index}]"), diagnostics);
    }
}

fn validate_graph_relation_shape(
    relation: &GraphRelationShape,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if relation.relation_kind.trim().is_empty() {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.missing_relation_kind",
            "relationKind must be present",
            format!("{path}.relationKind"),
        ));
    }
    if relation.target_object_kind.trim().is_empty() {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.invalid_shape",
            "targetObjectKind must be present",
            format!("{path}.targetObjectKind"),
        ));
    }
    if relation.limit == 0 {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.over_budget",
            "relation limits must be positive integers",
            format!("{path}.limit"),
        ));
    }
    for (index, child) in relation.relations.iter().enumerate() {
        validate_graph_relation_shape(child, &format!("{path}.relations[{index}]"), diagnostics);
    }
}

fn build_relation_batches(
    request: &GraphLoadRequest,
    source_object_kind: &str,
    source_ids: &[String],
    relations: &[GraphRelationShape],
    depth: u32,
    batches: &mut Vec<RelationBatchPlan>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for relation in relations {
        if source_ids.len() > 1 && !relation.relations.is_empty() && relation.limit > 500 {
            diagnostics.push(graph_diagnostic(
                "graph_load.validation.n_plus_one_risk",
                format!(
                    "relation batch '{}:{}' risks unbounded fan-out",
                    source_object_kind, relation.relation_kind
                ),
                "shape.relations",
            ));
        }

        batches.push(RelationBatchPlan {
            batch_id: format!("batch_{}", batches.len().saturating_add(1)),
            relation_kind: relation.relation_kind.clone(),
            direction: relation.direction.clone(),
            source_object_kind: source_object_kind.to_string(),
            target_object_kind: relation.target_object_kind.clone(),
            depth,
            limit: relation.limit,
            source_identity_keys: source_ids
                .iter()
                .map(|source_id| graph_identity_key(source_object_kind, source_id))
                .collect(),
            scope_pushdown: request.scope.clone(),
            include_relation_metadata: relation.include_relation_metadata.unwrap_or(false),
        });

        build_relation_batches(
            request,
            &relation.target_object_kind,
            &[],
            &relation.relations,
            depth.saturating_add(1),
            batches,
            diagnostics,
        );
    }
}

fn graph_max_depth(relations: &[GraphRelationShape]) -> u32 {
    relations
        .iter()
        .map(|relation| graph_max_depth(&relation.relations).saturating_add(1))
        .max()
        .unwrap_or(0)
}

fn graph_relation_count(relations: &[GraphRelationShape]) -> u32 {
    relations
        .iter()
        .map(|relation| graph_relation_count(&relation.relations).saturating_add(1))
        .fold(0_u32, u32::saturating_add)
}

fn estimate_graph_rows(root_count: u64, relations: &[GraphRelationShape]) -> u64 {
    let mut rows = root_count;
    for relation in relations {
        let direct_rows = root_count.max(1).saturating_mul(u64::from(relation.limit));
        rows = rows.saturating_add(
            direct_rows.saturating_add(estimate_graph_rows(direct_rows, &relation.relations)),
        );
    }
    rows
}

fn graph_identity_key(object_kind: &str, object_id: &str) -> String {
    format!("{object_kind}:{object_id}")
}

fn graph_diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    path: impl Into<String>,
) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        artifact_id: None,
        path: Some(path.into()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
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

fn validate_proposal_metadata(
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

fn validate_resource_reference(
    resource_id: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    resource_ids: &BTreeSet<String>,
) {
    if resource_ids.contains(resource_id) {
        return;
    }
    diagnostics.push(invalid_diagnostic(
        "connector.validation.unknown_resource",
        format!("resourceId '{resource_id}' does not match a declared connector resource"),
        path,
        artifact_id,
    ));
}

fn expect_object<'a>(
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

fn is_exact_semantic_identity(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(|character| {
            character.is_whitespace() || character.is_control() || character == '*'
        })
}

fn require_boolean_field(
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

fn validate_optional_non_empty_json_field(
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

fn validate_string_array_field(
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

fn reject_truthy_boolean_field(
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

fn validate_string_array(
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

fn validate_unique_string_array_field(
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

fn validate_optional_non_empty(
    value: &Option<String>,
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

fn require_non_empty_string(
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

fn reject_unknown_object_fields(
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provenance() -> ArtifactProvenance {
        ArtifactProvenance {
            source: ProvenanceSource::AuthoringSdk,
            source_path: None,
            actor_id: None,
            created_at: None,
        }
    }

    fn entity_payload() -> JsonValue {
        // Minimal valid entity payload: `fields` is required and must be an object.
        let mut fields_map: BTreeMap<String, JsonValue> = BTreeMap::new();
        let mut field_def: BTreeMap<String, JsonValue> = BTreeMap::new();
        field_def.insert("type".to_string(), JsonValue::String("text".to_string()));
        field_def.insert("required".to_string(), JsonValue::Bool(true));
        fields_map.insert("id".to_string(), JsonValue::Object(field_def));
        let mut payload: BTreeMap<String, JsonValue> = BTreeMap::new();
        payload.insert(
            "objectKind".to_string(),
            JsonValue::String("commerce.Product".to_string()),
        );
        payload.insert("fields".to_string(), JsonValue::Object(fields_map));
        JsonValue::Object(payload)
    }

    fn entity_artifact() -> ArtifactEnvelope {
        ArtifactEnvelope {
            ir_version: SUPPORTED_IR_VERSION.to_string(),
            artifact_id: "commerce.entity.product".to_string(),
            namespace: "commerce".to_string(),
            name: "Product".to_string(),
            version: "0.1.0".to_string(),
            lifecycle: ArtifactLifecycle::Draft,
            scope: ScopeRef::default(),
            dependencies: Vec::new(),
            provenance: provenance(),
            payload: ArtifactPayload::from_kind_json(&ArtifactKind::Entity, entity_payload())
                .expect("entity payload is an object"),
        }
    }

    #[test]
    fn rejects_unknown_top_level_payload_fields_with_a_stable_diagnostic() {
        let mut source =
            serde_json::to_value(entity_artifact()).expect("entity artifact should serialize");
        source["payload"]["unknownTopLevel"] = serde_json::json!(true);
        let artifact: ArtifactEnvelope =
            serde_json::from_value(source).expect("unknown field should reach validation");

        let diagnostics = artifact
            .validate()
            .expect_err("unknown payload fields must fail closed");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "entity.validation.invalid_payload"
                && diagnostic.message == "unknown field 'unknownTopLevel' is not allowed"
                && diagnostic.path.as_deref() == Some("payload.unknownTopLevel")
        }));
    }

    #[test]
    fn rejects_non_object_extensions_with_a_validation_diagnostic() {
        let mut source =
            serde_json::to_value(entity_artifact()).expect("entity artifact should serialize");
        source["payload"]["extensions"] = serde_json::json!("not-an-object-map");
        let artifact: ArtifactEnvelope =
            serde_json::from_value(source).expect("invalid extensions should reach validation");

        let diagnostics = artifact
            .validate()
            .expect_err("non-object extensions must fail validation");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "entity.validation.invalid_payload"
                && diagnostic.message == "extensions must be an object map"
                && diagnostic.path.as_deref() == Some("payload.extensions")
        }));
    }

    #[test]
    fn validates_supported_envelope() {
        let artifact = ArtifactEnvelope {
            ir_version: SUPPORTED_IR_VERSION.to_string(),
            artifact_id: "commerce.entity.product".to_string(),
            namespace: "commerce".to_string(),
            name: "Product".to_string(),
            version: "0.1.0".to_string(),
            lifecycle: ArtifactLifecycle::Draft,
            scope: ScopeRef::default(),
            dependencies: Vec::new(),
            provenance: provenance(),
            payload: ArtifactPayload::from_kind_json(&ArtifactKind::Entity, entity_payload())
                .expect("entity payload is an object"),
        };

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_unsupported_ir_version() {
        let artifact = ArtifactEnvelope {
            ir_version: "9.9".to_string(),
            artifact_id: "commerce.entity.product".to_string(),
            namespace: "commerce".to_string(),
            name: "Product".to_string(),
            version: "0.1.0".to_string(),
            lifecycle: ArtifactLifecycle::Draft,
            scope: ScopeRef::default(),
            dependencies: Vec::new(),
            provenance: provenance(),
            payload: ArtifactPayload::from_kind_json(
                &ArtifactKind::Entity,
                JsonValue::Object(BTreeMap::new()),
            )
            .expect("entity payload is an object"),
        };

        let diagnostics = artifact
            .validate()
            .expect_err("unsupported versions should fail");
        assert_eq!(diagnostics[0].code, "ir.validation.unsupported_version");
    }

    #[test]
    fn rejects_dependency_without_explicit_version() {
        let mut artifact = entity_artifact();
        artifact.dependencies = vec![ArtifactDependency {
            artifact_id: "commerce.entity.category".to_string(),
            kind: ArtifactKind::Entity,
            version: None,
            required: true,
        }];

        let diagnostics = artifact
            .validate()
            .expect_err("versionless dependency should fail");

        assert_eq!(diagnostics[0].code, "ir.validation.invalid_dependency");
        assert_eq!(
            diagnostics[0].remediation,
            Some(DiagnosticRemediation::ResolveDependency)
        );
    }

    #[test]
    fn rejects_self_dependency() {
        let mut artifact = entity_artifact();
        artifact.dependencies = vec![ArtifactDependency {
            artifact_id: "commerce.entity.product".to_string(),
            kind: ArtifactKind::Entity,
            version: Some("0.1.0".to_string()),
            required: true,
        }];

        let diagnostics = artifact
            .validate()
            .expect_err("self dependency should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "ir.validation.invalid_dependency")
        );
    }

    #[test]
    fn validates_shared_connector_fixture() {
        let artifact = artifact_fixture("retail-inventory.connector.json");

        assert_eq!(artifact.kind(), ArtifactKind::Connector);
        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn validates_typed_connector_command_and_keyset_profiles() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Connector,
            json!({
                "connectorVersion": "0.1",
                "auth": { "kind": "none" },
                "resources": [{
                    "kind": "delta",
                    "resourceId": "sales_orders",
                    "endpoint": "/api/resource/{doctype}",
                    "sinceParam": "modified",
                    "updatedAtPath": "$.modified",
                    "itemsPath": "$.data",
                    "queryParams": { "fields": "[\"name\"]" },
                    "keyset": {
                        "filtersParam": "filters",
                        "orFilterParam": "or_filters",
                        "fieldsParam": "fields",
                        "orderParam": "order_by",
                        "pageParam": "limit_start",
                        "sizeParam": "limit_page_length",
                        "watermarkField": "modified",
                        "tieBreakerField": "name",
                        "fields": ["name", "modified"],
                        "pageSize": 100
                    }
                }],
                "mappings": [],
                "commands": [{
                    "actionId": "cancel_sales_order",
                    "operation": "cancel",
                    "doctypes": ["Sales Order"],
                    "endpoint": "/api/resource/{doctype}/{name}",
                    "method": "PUT",
                    "bodyFields": ["reason"],
                    "fixedBody": { "docstatus": 2 },
                    "idempotency": {
                        "keyField": "norn_idempotency_key",
                        "headerName": "X-Norn-Idempotency-Key",
                        "receipt": {
                            "doctype": "Norn Command Receipt",
                            "tableField": "norn_command_receipts",
                            "keyField": "idempotency_key",
                            "operationField": "operation",
                            "parentField": "parent",
                            "parentTypeField": "parenttype",
                            "parentTableField": "parentfield",
                            "timestampField": "at"
                        },
                        "createLookup": {
                            "endpoint": "/api/resource/{doctype}",
                            "itemsPath": "$.data",
                            "fields": ["name", "modified"],
                            "filtersParam": "filters",
                            "fieldsParam": "fields",
                            "limitParam": "limit_page_length",
                            "maxResults": 2,
                            "filters": [{ "field": "norn_idempotency_key", "value": "idempotency_key" }]
                        },
                        "receiptLookup": {
                            "endpoint": "/api/resource/{doctype}",
                            "itemsPath": "$.data",
                            "fields": ["name", "modified"],
                            "filtersParam": "filters",
                            "fieldsParam": "fields",
                            "limitParam": "limit_page_length",
                            "maxResults": 2,
                            "filters": [{ "entity": "Norn Command Receipt", "field": "parenttype", "value": "parent_type" }]
                        }
                    },
                    "lifecycle": {
                        "documentPath": "$.data",
                        "identityField": "name",
                        "versionField": "modified",
                        "statusField": "docstatus",
                        "initialStatus": 0,
                        "submittedStatus": 1,
                        "cancelledStatus": 2
                    },
                    "conflicts": [{ "status": 409, "errorType": "TimestampMismatchError", "category": "version" }]
                }]
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_whitespace_padded_poll_fanout_bindings() {
        let canonical =
            include_str!("../../../fixtures/connectors/google-directory.connector.json");
        let source: serde_json::Value =
            serde_json::from_str(canonical).expect("Google connector artifact");
        let artifact: ArtifactEnvelope =
            serde_json::from_value(source.clone()).expect("connector envelope");
        artifact
            .validate()
            .expect("canonical poll profile is admitted");

        let mut invalid = source;
        invalid["payload"]["extensions"]["norn.poll/1"]["feeds"][3]["fanout"]["pathBinding"] =
            json!(" groupId ");
        let artifact: ArtifactEnvelope =
            serde_json::from_value(invalid).expect("mutated connector envelope");
        let diagnostics = artifact
            .validate()
            .expect_err("binding whitespace changes the declared endpoint key");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "connector.validation.invalid_payload"
                && diagnostic.path.as_deref()
                    == Some("payload.extensions.norn.poll/1.feeds[3].fanout")
        }));
    }

    #[test]
    fn poll_fanout_accepts_declared_query_binding_and_refuses_unbound_name() {
        let canonical =
            include_str!("../../../fixtures/connectors/google-directory.connector.json");
        let mut source: serde_json::Value =
            serde_json::from_str(canonical).expect("connector fixture");
        let resources = source["payload"]["resources"]
            .as_array_mut()
            .expect("resources");
        let resource = resources
            .iter_mut()
            .find(|resource| {
                resource["endpoint"]
                    .as_str()
                    .is_some_and(|endpoint| endpoint.contains("{groupId}"))
            })
            .expect("child resource");
        resource["endpoint"] = json!("/members");
        resource["queryParams"] = json!({"group": "{groupId}"});
        let artifact: ArtifactEnvelope =
            serde_json::from_value(source.clone()).expect("query connector");
        artifact.validate().expect("query-bound fanout admitted");
        let resources = source["payload"]["resources"]
            .as_array_mut()
            .expect("resources");
        let resource = resources
            .iter_mut()
            .find(|resource| resource["endpoint"] == "/members")
            .expect("child");
        resource["queryParams"] = json!({"group": "literal"});
        let artifact: ArtifactEnvelope = serde_json::from_value(source).expect("unbound connector");
        assert!(artifact.validate().is_err());
    }

    #[test]
    fn poll_connector_paths_accept_canonical_erpnext_and_google_packs() {
        let fixtures = [
            (
                "ERPNext",
                include_str!("../../../fixtures/connectors/frappe.connector.json"),
            ),
            (
                "Google Directory",
                include_str!("../../../fixtures/connectors/google-directory.connector.json"),
            ),
        ];

        for (name, source) in fixtures {
            let value: serde_json::Value = serde_json::from_str(source).expect("provider artifact");
            let artifact: ArtifactEnvelope =
                serde_json::from_value(value).expect("provider connector envelope");
            assert!(artifact.validate().is_ok(), "{name} paths remain admitted");
        }
    }

    #[test]
    fn poll_connector_paths_report_each_malformed_key_at_its_field() {
        let canonical =
            include_str!("../../../fixtures/connectors/google-directory.connector.json");
        let source: serde_json::Value = serde_json::from_str(canonical).expect("Google artifact");
        let mut malformed_paths = vec![
            "".to_owned(),
            " /id".to_owned(),
            "/id ".to_owned(),
            "a..b".to_owned(),
            "a[no]".to_owned(),
            "a[1".to_owned(),
            "a[1][2]".to_owned(),
            "a[1]junk".to_owned(),
            "a[999999999999999999999999999999999999999]".to_owned(),
            "/id~2".to_owned(),
            "/id\u{1}".to_owned(),
        ];
        malformed_paths.push(format!("/{}", "a".repeat(1_024)));

        for path in malformed_paths {
            let mut invalid_record_key = source.clone();
            invalid_record_key["payload"]["extensions"]["norn.poll/1"]["feeds"][1]["recordKeyPath"] =
                json!(path.clone());
            let artifact: ArtifactEnvelope =
                serde_json::from_value(invalid_record_key).expect("record key fixture");
            let diagnostics = artifact
                .validate()
                .expect_err("malformed recordKeyPath must be rejected");
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "connector.validation.invalid_payload"
                    && diagnostic.message.contains("recordKeyPath is invalid:")
                    && diagnostic.path.as_deref()
                        == Some("payload.extensions.norn.poll/1.feeds[1].recordKeyPath")
            }));

            let mut invalid_fanout_key = source.clone();
            invalid_fanout_key["payload"]["extensions"]["norn.poll/1"]["feeds"][3]["fanout"]["keyPath"] =
                json!(path.clone());
            let artifact: ArtifactEnvelope =
                serde_json::from_value(invalid_fanout_key).expect("fanout key fixture");
            let diagnostics = artifact
                .validate()
                .expect_err("malformed fanout keyPath must be rejected");
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "connector.validation.invalid_payload"
                    && diagnostic.message.contains("fanout keyPath is invalid:")
                    && diagnostic.path.as_deref()
                        == Some("payload.extensions.norn.poll/1.feeds[3].fanout.keyPath")
            }));
        }
    }

    #[test]
    fn rejects_duplicate_poll_resource_ids() {
        let canonical =
            include_str!("../../../fixtures/connectors/google-directory.connector.json");
        let mut source: serde_json::Value =
            serde_json::from_str(canonical).expect("Google connector artifact");
        let duplicate = source["payload"]["resources"][0].clone();
        source["payload"]["resources"]
            .as_array_mut()
            .expect("resource array")
            .push(duplicate);
        let artifact: ArtifactEnvelope =
            serde_json::from_value(source).expect("mutated connector envelope");
        let diagnostics = artifact
            .validate()
            .expect_err("resource ids must be unique");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "connector.validation.invalid_payload"
                && diagnostic.path.as_deref() == Some("payload.resources[4].resourceId")
        }));
    }

    #[test]
    fn rejects_idempotency_lookups_that_cannot_detect_duplicate_matches() {
        let canonical = include_str!("../../../fixtures/connectors/frappe.connector.json");
        let source: serde_json::Value =
            serde_json::from_str(canonical).expect("canonical ERPNext connector artifact");
        let admitted: ArtifactEnvelope =
            serde_json::from_value(source.clone()).expect("canonical connector envelope");
        admitted
            .validate()
            .expect("canonical connector lookup limits are admitted");

        for lookup_name in ["createLookup", "receiptLookup"] {
            let mut invalid = source.clone();
            invalid["payload"]["commands"][0]["idempotency"][lookup_name]["maxResults"] = json!(1);
            let artifact: ArtifactEnvelope =
                serde_json::from_value(invalid).expect("mutated connector artifact");

            let expected_path = format!("payload.commands[0].idempotency.{lookup_name}.maxResults");
            let diagnostics = artifact
                .validate()
                .expect_err("one result cannot establish lookup uniqueness");
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "connector.validation.invalid_payload"
                    && diagnostic.path.as_deref() == Some(expected_path.as_str())
                    && diagnostic.message.contains("from 2 through")
            }));
        }
    }

    #[test]
    fn validates_optional_connector_command_key_header_with_resource_path() {
        let payload = |command_key_header: &serde_json::Value| {
            json!({
                "connectorVersion": "0.1",
                "auth": { "kind": "none" },
                "resources": [{
                    "kind": "csv_import",
                    "resourceId": "records",
                    "delimiter": ",",
                    "columns": ["name"],
                    "commandKeyHeader": command_key_header
                }],
                "mappings": []
            })
        };
        assert!(
            artifact_with_payload(
                &ArtifactKind::Connector,
                payload(&json!("X-Norn-Command-Key")),
            )
            .validate()
            .is_ok()
        );

        for invalid in [json!(" "), json!(42), json!(null)] {
            let diagnostics = artifact_with_payload(&ArtifactKind::Connector, payload(&invalid))
                .validate()
                .expect_err("command key header must be a non-empty string");
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "connector.validation.invalid_payload"
                    && diagnostic.path.as_deref() == Some("payload.resources[0].commandKeyHeader")
            }));
        }
    }

    #[test]
    fn rejects_connector_body_fields_that_override_reserved_contract_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Connector,
            json!({
                "connectorVersion": "0.1",
                "auth": { "kind": "none" },
                "resources": [{
                    "kind": "csv_import",
                    "resourceId": "records",
                    "delimiter": ",",
                    "columns": ["name"]
                }],
                "mappings": [],
                "commands": [{
                    "operation": "update",
                    "endpoint": "/api/resource/{doctype}/{name}",
                    "method": "PUT",
                    "bodyFields": ["modified"],
                    "idempotency": {
                        "keyField": "norn_idempotency_key",
                        "receipt": {
                            "doctype": "Receipt",
                            "tableField": "receipts",
                            "keyField": "key",
                            "operationField": "operation",
                            "parentField": "parent",
                            "parentTypeField": "parenttype",
                            "parentTableField": "parentfield"
                        },
                        "createLookup": {
                            "endpoint": "/lookup/{doctype}", "itemsPath": "$.data", "fields": ["name"],
                            "filtersParam": "filters", "fieldsParam": "fields", "limitParam": "limit", "maxResults": 2,
                            "filters": [{ "field": "key", "value": "idempotency_key" }]
                        },
                        "receiptLookup": {
                            "endpoint": "/lookup/{doctype}", "itemsPath": "$.data", "fields": ["name"],
                            "filtersParam": "filters", "fieldsParam": "fields", "limitParam": "limit", "maxResults": 2,
                            "filters": [{ "field": "key", "value": "idempotency_key" }]
                        }
                    },
                    "lifecycle": {
                        "documentPath": "$.data", "identityField": "name", "versionField": "modified",
                        "statusField": "docstatus", "initialStatus": 0, "submittedStatus": 1, "cancelledStatus": 2
                    }
                }]
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("reserved body field should fail");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some("payload.commands[0].bodyFields[0]")
        }));
    }

    #[test]
    fn validates_shared_widget_fixture() {
        let artifact = artifact_fixture("retail-replenishment.widget.json");

        assert_eq!(artifact.kind(), ArtifactKind::Widget);
        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_connector_mapping_to_unknown_resource() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Connector,
            json!({
                "connectorVersion": "0.1",
                "auth": { "kind": "none" },
                "resources": [{
                    "kind": "csv_import",
                    "resourceId": "inventory",
                    "delimiter": ",",
                    "columns": ["sku"]
                }],
                "mappings": [{
                    "kind": "external_id",
                    "resourceId": "missing",
                    "sourcePath": "$.sku",
                    "targetObjectKind": "retail.StockItem",
                    "externalSource": "demo_inventory"
                }]
            }),
        );

        let diagnostics = artifact.validate().expect_err("mapping should fail");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "connector.validation.unknown_resource")
        );
    }

    #[test]
    fn validates_connector_custom_step_hook_schemas() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Connector,
            json!({
                "connectorVersion": "0.1",
                "auth": { "kind": "none" },
                "resources": [{
                    "kind": "csv_import",
                    "resourceId": "inventory",
                    "delimiter": ",",
                    "columns": ["sku"]
                }],
                "mappings": [],
                "customStepHooks": [{
                    "hookId": "retail.connector.inventory.fetch_page",
                    "installId": "install_inventory_fetch_page",
                    "extensionVersion": "0.1.0",
                    "requiredServices": ["provider_client"],
                    "scopeCapabilities": ["connector.run"],
                    "inputSchema": { "type": "object" },
                    "outputSchema": { "type": "object" },
                    "errorSchema": { "type": "object" }
                }]
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_connector_custom_step_hook_without_schema_objects() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Connector,
            json!({
                "connectorVersion": "0.1",
                "auth": { "kind": "none" },
                "resources": [{
                    "kind": "csv_import",
                    "resourceId": "inventory",
                    "delimiter": ",",
                    "columns": ["sku"]
                }],
                "mappings": [],
                "customStepHooks": [{
                    "hookId": "retail.connector.inventory.fetch_page",
                    "installId": "install_inventory_fetch_page",
                    "extensionVersion": "0.1.0",
                    "requiredServices": ["provider_client"],
                    "scopeCapabilities": ["connector.run"],
                    "inputSchema": "object",
                    "outputSchema": { "type": "object" },
                    "errorSchema": { "type": "object" }
                }]
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("custom step schema should fail");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path == Some("payload.customStepHooks[0].inputSchema".to_string())
        }));
    }

    #[test]
    fn rejects_unsafe_widget_action_binding() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Widget,
            json!({
                "widgetVersion": "0.1",
                "widgetId": "retail.replenishment.proposal",
                "dataDependencies": [],
                "viewStates": [{ "stateId": "preview", "kind": "preview" }],
                "actionBindings": [{
                    "bindingId": "approve",
                    "actionId": "retail.replenishment.approve",
                    "label": "Approve",
                    "inputMapping": {},
                    "requiredPermissions": ["retail.replenishment.approve"],
                    "unsafeRawMutation": true
                }],
                "permissions": []
            }),
        );

        let diagnostics = artifact.validate().expect_err("unsafe widget should fail");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "widget.validation.unsafe_action_binding")
        );
    }

    #[test]
    fn rejects_relation_traversal_widget_dependency_without_relation_kind() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Widget,
            json!({
                "widgetVersion": "0.1",
                "widgetId": "retail.stock.overview",
                "dataDependencies": [{
                    "dependencyId": "locationEdge",
                    "objectKind": "retail.StockPosition",
                    "query": { "kind": "relation_traversal" }
                }],
                "viewStates": [{ "stateId": "ready", "kind": "ready" }],
                "actionBindings": [],
                "permissions": [{
                    "permission": "retail.inventory.read",
                    "reason": "show scoped inventory data"
                }]
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("relation traversal must declare relation kind");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "widget.validation.invalid_payload"
                && diagnostic.path.as_deref()
                    == Some("payload.dataDependencies[0].query.relationKind")
        }));
    }

    #[test]
    fn rejects_widget_action_binding_without_dependency() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Widget,
            json!({
                "widgetVersion": "0.1",
                "widgetId": "retail.replenishment.proposal",
                "dataDependencies": [],
                "viewStates": [{ "stateId": "preview", "kind": "preview" }],
                "actionBindings": [{
                    "bindingId": "approve",
                    "actionId": "retail.replenishment.approve",
                    "label": "Approve",
                    "inputMapping": {},
                    "requiredPermissions": ["retail.replenishment.approve"]
                }],
                "permissions": []
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("widget action binding should require graph dependency");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "widget.validation.missing_dependency")
        );
    }

    #[test]
    fn validates_customization_field_overlay_payload() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Customization,
            json!({
                "customizationVersion": "0.1",
                "customizationId": "crm.customer.loyalty_tier",
                "intent": {
                    "summary": "Add loyalty tier tracking to customers",
                    "requestedBy": "agent"
                },
                "targetArtifact": {
                    "artifactId": "crm.entity.customer",
                    "kind": "entity",
                    "version": "0.1.0"
                },
                "overlay": {
                    "kind": "field",
                    "operations": [{
                        "op": "add_field",
                        "field": "loyaltyTier",
                        "definition": {
                            "type": "text",
                            "required": false,
                            "validation": { "enum": ["bronze", "silver", "gold"] }
                        }
                    }]
                },
                "approval": { "state": "proposed", "required": true },
                "scopeRequired": ["orgId", "workspaceId", "environmentId"]
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_customization_custom_hook_without_executable_boundary() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Customization,
            json!({
                "customizationVersion": "0.1",
                "customizationId": "crm.customer.live_validation",
                "intent": {
                    "summary": "Validate customer credit state before approval",
                    "requestedBy": "agent"
                },
                "targetArtifact": {
                    "artifactId": "crm.entity.customer",
                    "kind": "entity",
                    "version": "0.1.0"
                },
                "overlay": {
                    "kind": "validation",
                    "operations": [{ "op": "custom_hook" }]
                },
                "approval": { "state": "proposed", "required": true },
                "scopeRequired": ["orgId", "workspaceId", "environmentId"]
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("custom hook must declare executable contract");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "customization.validation.invalid_payload"
                && diagnostic.path.as_deref() == Some("payload.executable")
        }));
    }

    #[test]
    fn rejects_customization_executable_with_ambient_database_access() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Customization,
            json!({
                "customizationVersion": "0.1",
                "customizationId": "crm.customer.live_validation",
                "intent": {
                    "summary": "Validate customer credit state before approval",
                    "requestedBy": "agent"
                },
                "targetArtifact": {
                    "artifactId": "crm.entity.customer",
                    "kind": "entity",
                    "version": "0.1.0"
                },
                "overlay": {
                    "kind": "validation",
                    "operations": [{ "op": "custom_hook" }]
                },
                "approval": { "state": "proposed", "required": true },
                "scopeRequired": ["orgId", "workspaceId", "environmentId"],
                "executable": {
                    "boundary": "capability_rpc",
                    "inputSchema": { "type": "object" },
                    "outputSchema": { "type": "object" },
                    "errorSchema": { "type": "object" },
                    "requiredServices": ["customer_state"],
                    "scopeCapabilities": ["crm.customer.read"],
                    "resourceLimits": {
                        "maxWallTimeMs": 1000,
                        "maxMemoryMb": 128,
                        "maxNetworkCalls": 2,
                        "maxStorageBytes": 0
                    },
                    "directDatabaseAccess": true
                }
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("direct DB access must fail closed");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "customization.validation.invalid_payload"
                && diagnostic.path.as_deref() == Some("payload.executable.directDatabaseAccess")
        }));
    }

    #[test]
    fn validates_action_payload_shape() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Action,
            json!({
                "actionId": "retail.replenishment.approve",
                "scopeRequired": ["orgId", "workspaceId", "environmentId"],
                "idempotent": true,
                "ledgered": true,
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                },
                "outputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_action_payload_missing_versioned_contract_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Action,
            json!({
                "actionId": "retail.replenishment.approve",
                "ledgered": true
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("ActionIR contract fields should be required");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "action.validation.invalid_payload")
        );
    }

    #[test]
    fn validates_rule_payload_shape() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Rule,
            json!({
                "ruleId": "retail.invariant.stock_nonnegative",
                "appliesTo": "retail.StockPosition",
                "expression": "quantity >= 0",
                "severity": "error"
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn validates_modern_rule_payload_shape() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Rule,
            json!({
                "ruleVersion": "0.1",
                "ruleId": "retail.rule.replenishment_review",
                "inputSchema": { "type": "object", "required": ["projectedValue"] },
                "outputSchema": { "type": "object", "required": ["reviewRequired"] },
                "scopeRequired": ["orgId", "workspaceId", "environmentId"],
                "decision": {
                    "kind": "decision_table",
                    "hitPolicy": "first",
                    "inputs": [{ "field": "projectedValue", "type": "number" }],
                    "rows": [
                        {
                            "rowId": "high_value",
                            "when": [{ "field": "projectedValue", "op": "gte", "value": 1000 }],
                            "output": { "reviewRequired": true, "reason": "high_value" }
                        },
                        {
                            "rowId": "low_value",
                            "when": [],
                            "output": { "reviewRequired": false, "reason": "low_value" }
                        }
                    ]
                }
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_modern_rule_payload_with_required_services() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Rule,
            json!({
                "ruleVersion": "0.1",
                "ruleId": "retail.rule.live_credit_check",
                "inputSchema": { "type": "object" },
                "outputSchema": { "type": "object" },
                "scopeRequired": ["orgId"],
                "requiredServices": ["credit_provider"],
                "decision": { "kind": "literal", "value": { "allowed": true } }
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("deterministic RuleIR must reject live services");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "rule.validation.invalid_payload"
                && diagnostic.path.as_deref() == Some("payload.requiredServices")
        }));
    }

    #[test]
    fn rejects_rule_payload_missing_invariant_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Rule,
            json!({
                "ruleId": "retail.invariant.stock_nonnegative",
                "severity": "fatal"
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("RuleIR contract fields should be required");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "rule.validation.invalid_payload")
        );
    }

    #[test]
    fn validates_modern_policy_payload_shape() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Policy,
            json!({
                "policyVersion": "0.1",
                "policyId": "retail.policy.replenishment_review",
                "scopeRequired": ["orgId", "workspaceId", "environmentId"],
                "obligations": [{
                    "obligationId": "high_value_review",
                    "appliesTo": "retail.ReplenishmentProposal",
                    "ruleId": "retail.rule.replenishment_review",
                    "evidenceRequired": ["proposal", "policy_trace"],
                    "reviewRoles": ["inventory_manager"]
                }]
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_policy_payload_without_obligations() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Policy,
            json!({
                "policyVersion": "0.1",
                "policyId": "retail.policy.replenishment_review",
                "scopeRequired": ["orgId"],
                "obligations": []
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("PolicyIR must require obligations");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "policy.validation.invalid_payload"
                && diagnostic.path.as_deref() == Some("payload.obligations")
        }));
    }

    #[test]
    fn validates_business_pack_payload_shape() {
        let artifact = artifact_with_payload(
            &ArtifactKind::BusinessPack,
            json!({
                "packId": "retail.pack.core",
                "packVersion": "0.1",
                "packKind": "composition",
                "artifacts": [
                    { "artifactId": "retail.StockPosition.entity", "kind": "entity", "version": "0.1.0" }
                ]
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn accepts_legacy_ir_v01_composition_pack_without_discriminator() {
        let artifact = artifact_with_payload(
            &ArtifactKind::BusinessPack,
            json!({
                "packId": "retail.pack.legacy",
                "packVersion": "0.1",
                "artifacts": [
                    { "artifactId": "retail.StockPosition.entity", "kind": "entity", "version": "0.1.0" }
                ]
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_business_pack_payload_without_artifacts() {
        let artifact = artifact_with_payload(
            &ArtifactKind::BusinessPack,
            json!({
                "packId": "retail.pack.core",
                "packVersion": "0.1",
                "packKind": "composition",
                "artifacts": []
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("BusinessPackIR contract fields should be required");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "business_pack.validation.invalid_payload")
        );
    }

    #[test]
    fn plans_bounded_graph_load_requests() {
        let request = GraphLoadRequest {
            request_id: "graph_req_1".to_string(),
            correlation_id: "corr_1".to_string(),
            scope: ScopeRef {
                org_id: Some("org_demo".to_string()),
                environment_id: Some("dev".to_string()),
                ..ScopeRef::default()
            },
            scope_required: vec!["orgId".to_string(), "environmentId".to_string()],
            shape: GraphReadShape {
                root_object_kind: "customer".to_string(),
                root_ids: vec!["customer_1".to_string(), "customer_2".to_string()],
                fields: vec!["name".to_string(), "email".to_string()],
                relations: vec![GraphRelationShape {
                    relation_kind: "customer_has_order".to_string(),
                    direction: GraphTraversalDirection::Outbound,
                    target_object_kind: "order".to_string(),
                    fields: vec!["orderNumber".to_string(), "total".to_string()],
                    include_relation_metadata: Some(true),
                    limit: 20,
                    relations: vec![GraphRelationShape {
                        relation_kind: "order_has_line_item".to_string(),
                        direction: GraphTraversalDirection::Outbound,
                        target_object_kind: "line_item".to_string(),
                        fields: vec!["sku".to_string(), "quantity".to_string()],
                        include_relation_metadata: None,
                        limit: 50,
                        relations: Vec::new(),
                    }],
                }],
            },
            budget: GraphQueryBudget {
                max_depth: 3,
                max_root_objects: 100,
                max_relation_shapes: 20,
                max_relation_batches: 20,
                max_rows: 10_000,
                allow_higher_risk_policy: None,
            },
        };

        let plan = plan_graph_load(&request);

        assert!(plan.can_execute);
        assert_eq!(
            plan.root_identity_keys,
            vec!["customer:customer_1", "customer:customer_2"]
        );
        assert_eq!(plan.evidence.depth, 2);
        assert_eq!(plan.evidence.relation_batch_count, 2);
        assert!(plan.relation_batches[0].include_relation_metadata);
    }

    #[test]
    fn reports_graph_load_scope_and_budget_diagnostics() {
        let request = GraphLoadRequest {
            request_id: "graph_req_2".to_string(),
            correlation_id: "corr_2".to_string(),
            scope: ScopeRef {
                environment_id: Some("dev".to_string()),
                ..ScopeRef::default()
            },
            scope_required: vec!["orgId".to_string(), "environmentId".to_string()],
            shape: GraphReadShape {
                root_object_kind: "customer".to_string(),
                root_ids: vec![
                    "customer_1".to_string(),
                    "customer_2".to_string(),
                    "customer_3".to_string(),
                ],
                fields: vec!["name".to_string()],
                relations: vec![GraphRelationShape {
                    relation_kind: "customer_has_order".to_string(),
                    direction: GraphTraversalDirection::Outbound,
                    target_object_kind: "order".to_string(),
                    fields: vec!["orderNumber".to_string()],
                    include_relation_metadata: None,
                    limit: 1000,
                    relations: Vec::new(),
                }],
            },
            budget: GraphQueryBudget {
                max_depth: 1,
                max_root_objects: 2,
                max_relation_shapes: 1,
                max_relation_batches: 1,
                max_rows: 100,
                allow_higher_risk_policy: None,
            },
        };

        let plan = plan_graph_load(&request);
        let codes = plan
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();

        assert!(!plan.can_execute);
        assert!(codes.contains(&"graph_load.validation.missing_scope"));
        assert!(codes.contains(&"graph_load.validation.over_budget"));
    }

    fn artifact_with_payload(kind: &ArtifactKind, payload: serde_json::Value) -> ArtifactEnvelope {
        let payload: JsonValue =
            norn_ir::json_tree_from_value(payload).expect("payload should decode to JsonValue");
        let payload =
            ArtifactPayload::from_kind_json(kind, payload).expect("test payload is an object");
        ArtifactEnvelope {
            ir_version: SUPPORTED_IR_VERSION.to_string(),
            artifact_id: "commerce.artifact.demo".to_string(),
            namespace: "commerce".to_string(),
            name: "Demo".to_string(),
            version: "0.1.0".to_string(),
            lifecycle: ArtifactLifecycle::Draft,
            scope: ScopeRef::default(),
            dependencies: Vec::new(),
            provenance: provenance(),
            payload,
        }
    }

    fn artifact_fixture(name: &str) -> ArtifactEnvelope {
        let fixture = match name {
            "retail-inventory.connector.json" => {
                include_str!("../../../fixtures/ir/retail-inventory.connector.json")
            }
            "retail-replenishment.widget.json" => {
                include_str!("../../../fixtures/ir/retail-replenishment.widget.json")
            }
            _ => panic!("unknown fixture {name}"),
        };

        serde_json::from_str(fixture).expect("shared IR fixture should decode")
    }

    // -----------------------------------------------------------------------
    // Entity / Relation payload validation (closing lib.rs:487 hole)
    // -----------------------------------------------------------------------

    #[test]
    fn validates_entity_with_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Entity,
            json!({
                "objectKind": "commerce.Product",
                "fields": { "sku": { "type": "text", "required": true } }
            }),
        );
        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_entity_without_fields() {
        let artifact = artifact_with_payload(&ArtifactKind::Entity, json!({}));
        let diagnostics = artifact
            .validate()
            .expect_err("entity without fields must fail");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "entity.validation.invalid_payload"),
            "expected entity.validation.invalid_payload, got: {:?}",
            diagnostics
        );
    }

    #[test]
    fn rejects_entity_with_non_object_fields() {
        let artifact =
            artifact_with_payload(&ArtifactKind::Entity, json!({ "fields": "not-an-object" }));
        let diagnostics = artifact
            .validate()
            .expect_err("entity with non-object fields must fail");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "entity.validation.invalid_payload")
        );
    }

    #[test]
    fn entity_and_relation_ir_reject_unimplemented_semantics_before_promotion() {
        let entity = artifact_with_payload(
            &ArtifactKind::Entity,
            json!({
                "objectKind": "commerce.User",
                "fields": {
                    "role": { "type": "string", "required": true, "enum": ["viewer"] }
                },
                "computedBy": "ambient_callback"
            }),
        );
        let entity_diagnostics = entity
            .validate()
            .expect_err("ignored EntityIR semantics must fail closed");
        let entity_paths = entity_diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.path.as_deref())
            .collect::<Vec<_>>();
        assert!(entity_paths.contains(&"payload.computedBy"));
        assert!(entity_paths.contains(&"payload.fields.role.enum"));

        let relation = artifact_with_payload(
            &ArtifactKind::Relation,
            json!({
                "from": "commerce.User",
                "to": "commerce.Team",
                "cardinality": "many_to_many",
                "through": "commerce.Membership"
            }),
        );
        let relation_diagnostics = relation
            .validate()
            .expect_err("unimplemented RelationIR through semantics must fail closed");
        assert!(
            relation_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path.as_deref() == Some("payload.through"))
        );
    }

    #[test]
    fn entity_ir_accepts_only_closed_string_enum_semantics() {
        let valid = artifact_with_payload(
            &ArtifactKind::Entity,
            json!({
                "objectKind": "commerce.Order",
                "fields": {
                    "status": {
                        "type": "enum",
                        "required": true,
                        "values": ["draft", "submitted"]
                    }
                }
            }),
        );
        valid
            .validate()
            .expect("closed string enum semantics should validate");

        for values in [json!([]), json!(["draft", "draft"]), json!(["draft", 3])] {
            let invalid = artifact_with_payload(
                &ArtifactKind::Entity,
                json!({
                    "objectKind": "commerce.Order",
                    "fields": { "status": { "type": "enum", "values": values } }
                }),
            );
            invalid
                .validate()
                .expect_err("empty, duplicate, and non-string enum values must reject");
        }
    }

    #[test]
    fn entity_ir_rejects_noncanonical_semantic_object_kinds() {
        for object_kind in [
            " inventory.StockPosition",
            "inventory.Stock Position",
            "inventory.\tStockPosition",
            "inventory.*",
            "inventory.\0StockPosition",
            "inventory.\u{a0}StockPosition",
        ] {
            let invalid = artifact_with_payload(
                &ArtifactKind::Entity,
                json!({
                    "objectKind": object_kind,
                    "fields": {}
                }),
            );
            let diagnostics = invalid
                .validate()
                .expect_err("noncanonical semantic identities must reject");
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| { diagnostic.path.as_deref() == Some("payload.objectKind") })
            );
        }
    }

    #[test]
    fn entity_ir_accepts_only_closed_mutation_authority_descriptors() {
        for authority in [
            json!({ "kind": "generic_action" }),
            json!({ "kind": "domain_engine", "domain": "inventory" }),
            json!({ "kind": "domain_engine", "domain": "accounting" }),
        ] {
            artifact_with_payload(
                &ArtifactKind::Entity,
                json!({
                    "objectKind": "inventory.StockPosition",
                    "fields": {},
                    "mutationAuthority": authority
                }),
            )
            .validate()
            .expect("closed mutation authority should validate");
        }

        for authority in [
            json!({ "kind": "domain_engine" }),
            json!({ "kind": "domain_engine", "domain": "unknown" }),
            json!({ "kind": "generic_action", "domain": "inventory" }),
            json!({ "kind": "domain_engine", "domain": "inventory", "callback": "ambient" }),
        ] {
            artifact_with_payload(
                &ArtifactKind::Entity,
                json!({
                    "objectKind": "inventory.StockPosition",
                    "fields": {},
                    "mutationAuthority": authority
                }),
            )
            .validate()
            .expect_err("open, incomplete, or unknown mutation authority must reject");
        }
    }

    #[test]
    fn validates_relation_with_required_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Relation,
            json!({
                "from": "commerce.Customer",
                "to": "commerce.Invoice",
                "cardinality": "one_to_many"
            }),
        );
        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_relation_with_invalid_cardinality() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Relation,
            json!({
                "from": "commerce.Customer",
                "to": "commerce.Invoice",
                "cardinality": "sometimes"
            }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("relation with invalid cardinality must fail");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "relation.validation.invalid_payload")
        );
    }

    #[test]
    fn rejects_relation_without_from() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Relation,
            json!({
                "to": "commerce.Invoice"
            }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("relation without from must fail");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "relation.validation.invalid_payload")
        );
    }

    // -----------------------------------------------------------------------
    // Derived / ViewIR v0 tests
    // -----------------------------------------------------------------------

    fn valid_derived_payload() -> serde_json::Value {
        json!({
            "viewVersion": "0.1",
            "viewId": "commerce.view.open_invoices",
            "inputs": [
                {
                    "artifactId": "commerce.entity.invoice",
                    "kind": "entity",
                    "version": "0.1.0",
                    "alias": "invoice"
                }
            ],
            "calc": { "kind": "field_projection", "fields": ["id"] },
            "outputSchema": { "type": "array" },
            "maintenance": "recompute_on_read"
        })
    }

    #[test]
    fn validates_minimal_derived_view() {
        let artifact = artifact_with_payload(&ArtifactKind::Derived, valid_derived_payload());
        assert!(
            artifact.validate().is_ok(),
            "minimal derived view should be valid: {:?}",
            artifact.validate()
        );
    }

    #[test]
    fn rejects_derived_with_effectful_ts_calc() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Derived,
            json!({
                "viewVersion": "0.1",
                "viewId": "commerce.view.bad",
                "inputs": [{
                    "artifactId": "commerce.entity.invoice",
                    "kind": "entity",
                    "version": "0.1.0",
                    "alias": "invoice"
                }],
                "calc": { "kind": "effectful_ts", "sourceRef": "some/module.ts" },
                "outputSchema": {},
                "maintenance": "recompute_on_read"
            }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("effectful_ts in derived must fail");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "derived.validation.io_in_view"),
            "expected derived.validation.io_in_view, got: {:?}",
            diagnostics
        );
    }

    #[test]
    fn rejects_derived_with_raw_promise_calc() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Derived,
            json!({
                "viewVersion": "0.1",
                "viewId": "commerce.view.bad",
                "inputs": [{
                    "artifactId": "commerce.entity.invoice",
                    "kind": "entity",
                    "version": "0.1.0",
                    "alias": "invoice"
                }],
                "calc": { "kind": "raw_promise", "sourceRef": "some/module.ts" },
                "outputSchema": {},
                "maintenance": "recompute_on_read"
            }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("raw_promise calc must always fail");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "derived.validation.io_in_view")
        );
    }

    #[test]
    fn rejects_derived_with_incremental_maintenance() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Derived,
            json!({
                "viewVersion": "0.1",
                "viewId": "commerce.view.bad",
                "inputs": [{
                    "artifactId": "commerce.entity.invoice",
                    "kind": "entity",
                    "version": "0.1.0",
                    "alias": "invoice"
                }],
                "calc": { "kind": "field_projection", "fields": ["id"] },
                "outputSchema": {},
                "maintenance": "incremental"
            }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("incremental maintenance must be rejected");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "derived.validation.incremental_not_supported"),
            "expected derived.validation.incremental_not_supported, got: {:?}",
            diagnostics
        );
    }

    #[test]
    fn rejects_derived_with_empty_inputs() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Derived,
            json!({
                "viewVersion": "0.1",
                "viewId": "commerce.view.bad",
                "inputs": [],
                "calc": { "kind": "field_projection", "fields": ["id"] },
                "outputSchema": {},
                "maintenance": "recompute_on_read"
            }),
        );
        let diagnostics = artifact.validate().expect_err("empty inputs must fail");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "derived.validation.invalid_payload")
        );
    }

    #[test]
    fn rejects_derived_input_with_action_kind() {
        // Only entity / relation / derived are valid input kinds.
        let artifact = artifact_with_payload(
            &ArtifactKind::Derived,
            json!({
                "viewVersion": "0.1",
                "viewId": "commerce.view.bad",
                "inputs": [{
                    "artifactId": "commerce.action.something",
                    "kind": "action",
                    "version": "0.1.0",
                    "alias": "something"
                }],
                "calc": { "kind": "field_projection", "fields": ["id"] },
                "outputSchema": {},
                "maintenance": "recompute_on_read"
            }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("action input kind must fail");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "derived.validation.invalid_payload")
        );
    }

    #[test]
    fn rejects_derived_input_without_version() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Derived,
            json!({
                "viewVersion": "0.1",
                "viewId": "commerce.view.bad",
                "inputs": [{
                    "artifactId": "commerce.entity.invoice",
                    "kind": "entity",
                    "alias": "invoice"
                }],
                "calc": { "kind": "field_projection", "fields": ["id"] },
                "outputSchema": {},
                "maintenance": "recompute_on_read"
            }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("versionless input must fail");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "derived.validation.invalid_payload")
        );
    }

    #[test]
    fn validates_derived_with_as_of_param_temporal_scope_and_as_of_input() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Derived,
            json!({
                "viewVersion": "0.1",
                "viewId": "compliance.view.invoice_at_submission",
                "inputs": [
                    {
                        "artifactId": "einvoicing.entity.invoice",
                        "kind": "entity",
                        "version": "0.1.0",
                        "alias": "invoice"
                    },
                    {
                        "artifactId": "einvoicing.entity.submission_event",
                        "kind": "entity",
                        "version": "0.1.0",
                        "alias": "as_of"
                    }
                ],
                "calc": { "kind": "field_projection", "fields": ["id"] },
                "outputSchema": { "type": "array" },
                "maintenance": "recompute_on_read",
                "temporalScope": { "mode": "as_of_param" }
            }),
        );
        assert!(
            artifact.validate().is_ok(),
            "as_of_param with as_of input should be valid: {:?}",
            artifact.validate()
        );
    }

    #[test]
    fn rejects_derived_as_of_param_without_as_of_input() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Derived,
            json!({
                "viewVersion": "0.1",
                "viewId": "compliance.view.bad",
                "inputs": [{
                    "artifactId": "einvoicing.entity.invoice",
                    "kind": "entity",
                    "version": "0.1.0",
                    "alias": "invoice"
                }],
                "calc": { "kind": "field_projection", "fields": ["id"] },
                "outputSchema": {},
                "maintenance": "recompute_on_read",
                "temporalScope": { "mode": "as_of_param" }
            }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("as_of_param without as_of input must fail");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "derived.validation.invalid_payload"),
            "expected derived.validation.invalid_payload, got: {:?}",
            diagnostics
        );
    }

    #[test]
    fn validates_derived_with_current_temporal_scope() {
        let mut payload = valid_derived_payload();
        payload["temporalScope"] = json!({ "mode": "current" });
        let artifact = artifact_with_payload(&ArtifactKind::Derived, payload);
        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_derived_with_field_projection_calc_and_empty_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Derived,
            json!({
                "viewVersion": "0.1",
                "viewId": "commerce.view.bad",
                "inputs": [{
                    "artifactId": "commerce.entity.invoice",
                    "kind": "entity",
                    "version": "0.1.0",
                    "alias": "invoice"
                }],
                "calc": { "kind": "field_projection", "fields": [] },
                "outputSchema": {},
                "maintenance": "recompute_on_read"
            }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("empty fields in field_projection must fail");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "derived.validation.invalid_payload")
        );
    }
}

fn validate_catalog_connector_extensions(
    extensions: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(JsonValue::Object(extensions)) = extensions else {
        return;
    };
    for key in ["norn.shopify.graphql/1", "norn.catalog.projection/1"] {
        let Some(value) = extensions.get(key) else {
            continue;
        };
        let encoded = serde_json::to_string(value).unwrap_or_default();
        let valid = if key == "norn.shopify.graphql/1" {
            serde_json::from_str::<norn_ir::connector::ShopifyGraphqlProfile>(&encoded)
                .is_ok_and(|profile| matches!(profile.api_version.as_str(), "2026-01" | "2026-04"))
        } else {
            serde_json::from_str::<norn_ir::connector::CatalogProjectionProfile>(&encoded)
                .is_ok_and(|profile| {
                    !profile.actor.trim().is_empty()
                        && !profile.mapping.schema_id.is_empty()
                        && !profile.mapping.version.is_empty()
                        && profile.mapping.sha256.len() == 64
                        && profile
                            .mapping
                            .sha256
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                })
        };
        if !valid {
            diagnostics.push(invalid_diagnostic(
                "connector.validation.invalid_payload",
                "invalid typed catalogue protocol extension",
                format!("payload.extensions.{key}"),
                artifact_id,
            ));
        }
    }
}

#[cfg(test)]
mod catalog_protocol_tests {
    use super::*;
    #[test]
    fn write_only_graphql_connector_requires_valid_typed_protocol_and_mapping_pin() {
        let source = include_str!(
            "../../../fixtures/dev/catalog-sync/definitions/shopify_demo.connector.json"
        );
        let value: serde_json::Value = serde_json::from_str(source).expect("fixture");
        let artifact = crate::decode_artifact_json(&value).expect("typed connector");
        assert!(
            artifact.validate().is_ok(),
            "write-only destination is declarative"
        );
        for extension in [
            serde_json::json!({"norn.shopify.graphql/1":{"apiVersion":"2025-01"}}),
            serde_json::json!({"norn.shopify.graphql/1":{"apiVersion":"2026-04","query":"arbitrary"}}),
            serde_json::json!({"norn.catalog.projection/1":{"actor":"catalog_projector","mapping":{"schemaId":"m","version":"1","sha256":"unpinned"}}}),
        ] {
            let mut invalid = value.clone();
            invalid["payload"]["extensions"] = extension;
            let artifact =
                crate::decode_artifact_json(&invalid).expect("extension reaches checker");
            assert!(
                artifact.validate().is_err(),
                "malformed protocol/profile must fail admission"
            );
        }
        let mut empty = value;
        empty["payload"]["commands"] = serde_json::json!([]);
        assert!(
            crate::decode_artifact_json(&empty)
                .expect("empty profile")
                .validate()
                .is_err()
        );
    }
}
