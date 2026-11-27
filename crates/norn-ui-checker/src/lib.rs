//! `norn-ui-checker`: Ring-1 UI and Widget UX admission diagnostics (#1613).
//!
//! Composes strictly above Ring-0 [`norn_checker`] semantic admission with
//! Ring-1 UI/widget validation: widget artifact envelopes ([`envelope`]),
//! typed Widget UX contracts ([`widget_ux`]), interaction rules
//! ([`ui_interaction`]), page statecharts ([`ui_machine`]), semantic views
//! ([`ui_view`]), catalog components ([`ui_catalog`]), page-to-lifecycle
//! composition ([`ui_lifecycle`]), UI SDK layers and the authority split
//! ([`ui_layer`]) over the pinned UI SDK catalog ([`ui_sdk`]).

// Deterministic core policy (#3299).
#![deny(clippy::disallowed_types)]

pub mod envelope;
pub mod ui_catalog;
pub mod ui_interaction;
pub mod ui_layer;
pub mod ui_lifecycle;
pub mod ui_machine;
pub mod ui_sdk;
pub mod ui_view;
pub mod widget_ux;

use norn_checker::{
    CheckReceipt, CheckedArtifactSet, Diagnostic, DiagnosticCode, DiagnosticRemediation,
    DiagnosticSeverity, DiagnosticsDigest, EntityArtifactSetContext, IDENTIFIER_ADMISSION_PROFILE,
    ReceiptError, ReceiptSources,
};
use norn_ir::explain::{ExplainKind, ExplainReport};
use norn_ir::{ArtifactEnvelope, ArtifactPayload, FlowSourceMap, ResourceLimitProfile};

pub use widget_ux::{
    GeneratedTransitionEnabledWhenError, check_widget_ux_set, generated_transition_enabled_when,
};

const fn severity_rank(severity: &DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Info => 0,
        DiagnosticSeverity::Warning => 1,
        DiagnosticSeverity::Error => 2,
    }
}

fn merge_widget_entries<T>(
    ring0: Vec<T>,
    extra: Vec<T>,
    is_marker: impl Fn(&T) -> bool,
    same_key: impl Fn(&T, &T) -> bool,
    severity: impl Fn(&T) -> &DiagnosticSeverity,
) -> impl Iterator<Item = T> {
    let mut out: Vec<(T, bool)> = Vec::with_capacity(ring0.len().saturating_add(extra.len()));
    let push_extra = |out: &mut Vec<(T, bool)>, extras: Vec<T>| {
        for finding in extras {
            if let Some(index) = out
                .iter()
                .position(|(existing, _)| same_key(existing, &finding))
            {
                let Some((existing, from_ring0)) = out.get(index) else {
                    continue;
                };
                let from_ring0 = *from_ring0;
                let new_rank = severity_rank(severity(&finding));
                let old_rank = severity_rank(severity(existing));
                if new_rank > old_rank {
                    if let Some(slot) = out.get_mut(index) {
                        *slot = (finding, false);
                    }
                } else if from_ring0 && new_rank == old_rank {
                    out.remove(index);
                    out.push((finding, false));
                }
            } else {
                out.push((finding, false));
            }
        }
    };
    let mut pending = Some(extra);
    for finding in ring0 {
        if is_marker(&finding) {
            if let Some(extras) = pending.take() {
                push_extra(&mut out, extras);
            }
        } else {
            out.push((finding, true));
        }
    }
    if let Some(extras) = pending {
        push_extra(&mut out, extras);
    }
    out.into_iter().map(|(diagnostic, _)| diagnostic)
}

fn merge_legacy_widget_diagnostics(
    ring0: Vec<norn_ir::Diagnostic>,
    extra: Vec<norn_ir::Diagnostic>,
) -> Vec<norn_ir::Diagnostic> {
    merge_widget_entries(
        ring0,
        extra,
        |finding| finding.code == norn_checker::codes::WIDGET_RING1_REQUIRED,
        |existing, finding| {
            existing.code == finding.code
                && existing.artifact_id == finding.artifact_id
                && existing.path == finding.path
        },
        |finding| &finding.severity,
    )
    .collect()
}

fn merge_widget_diagnostics(ring0: Vec<Diagnostic>, extra: Vec<Diagnostic>) -> Vec<Diagnostic> {
    norn_checker::entity_artifacts::dedupe_findings(merge_widget_entries(
        ring0,
        extra,
        |finding| finding.code.as_str() == norn_checker::codes::WIDGET_RING1_REQUIRED,
        |existing, finding| {
            existing.code == finding.code
                && existing.artifact_id == finding.artifact_id
                && existing
                    .span
                    .as_ref()
                    .map_or("", |span| span.pointer.as_str())
                    == finding
                        .span
                        .as_ref()
                        .map_or("", |span| span.pointer.as_str())
        },
        |finding| &finding.severity,
    ))
}

fn widget_extra_diagnostics(artifact: &ArtifactEnvelope) -> Vec<Diagnostic> {
    if !matches!(artifact.payload, ArtifactPayload::Widget(_)) {
        return Vec::new();
    }
    let payload = norn_checker::envelope::validation_json_value(&artifact.payload);
    let legacy = envelope::validate_widget_extra(artifact, &payload);
    match serde_json::to_value(artifact) {
        Ok(document) => legacy
            .into_iter()
            .map(|diagnostic| Diagnostic::from_legacy_in(diagnostic, &document))
            .collect(),
        Err(_unserializable) => legacy.into_iter().map(Diagnostic::from_legacy).collect(),
    }
}

/// Validate one artifact envelope with Ring-0 checks completed first, then
/// Ring-1 widget payload/dependency validation merged at the
/// [`norn_checker::codes::WIDGET_RING1_REQUIRED`] marker.
///
/// # Errors
///
/// Returns every legacy diagnostic found when the envelope is invalid.
pub fn validate_artifact(artifact: &ArtifactEnvelope) -> Result<(), Vec<norn_ir::Diagnostic>> {
    if !matches!(artifact.payload, ArtifactPayload::Widget(_)) {
        return norn_checker::validate_artifact(artifact);
    }
    let ring0 = match norn_checker::validate_artifact(artifact) {
        Ok(()) => Vec::new(),
        Err(findings) => findings,
    };
    let payload = norn_checker::envelope::validation_json_value(&artifact.payload);
    let diagnostics =
        merge_legacy_widget_diagnostics(ring0, envelope::validate_widget_extra(artifact, &payload));
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    {
        Err(diagnostics)
    } else {
        Ok(())
    }
}

/// Check one artifact envelope with Ring-0 checks completed first, then Ring-1
/// widget payload/dependency validation merged at the
/// [`norn_checker::codes::WIDGET_RING1_REQUIRED`] marker.
#[must_use]
pub fn check_artifact(artifact: &ArtifactEnvelope) -> Vec<Diagnostic> {
    let findings = norn_checker::check_artifact(artifact);
    if !matches!(artifact.payload, ArtifactPayload::Widget(_)) {
        return findings;
    }
    let had_ring1_marker = findings
        .iter()
        .any(|d| d.code.as_str() == norn_checker::codes::WIDGET_RING1_REQUIRED);
    if !had_ring1_marker {
        return findings;
    }
    merge_widget_diagnostics(findings, widget_extra_diagnostics(artifact))
}

/// Decode an artifact envelope from wire JSON and check it with Ring-0 checks
/// completed first, then Ring-1 widget payload/dependency validation merged at
/// the [`norn_checker::codes::WIDGET_RING1_REQUIRED`] marker.
#[must_use]
pub fn check_artifact_json(value: &serde_json::Value) -> Vec<Diagnostic> {
    let findings = norn_checker::check_artifact_json(value);
    let had_ring1_marker = findings
        .iter()
        .any(|d| d.code.as_str() == norn_checker::codes::WIDGET_RING1_REQUIRED);
    if !had_ring1_marker {
        return findings;
    }
    if let Ok(artifact) = norn_checker::decode_artifact_json(value)
        && matches!(artifact.payload, ArtifactPayload::Widget(_))
    {
        let payload = norn_checker::envelope::validation_json_value(&artifact.payload);
        let extra = envelope::validate_widget_extra(&artifact, &payload)
            .into_iter()
            .map(|diagnostic| Diagnostic::from_legacy_in(diagnostic, value))
            .collect();
        return merge_widget_diagnostics(findings, extra);
    }
    findings
}

/// Resolve `artifacts` against `context.base` and run every Ring-0 set check
/// to completion first, then merge each candidate widget's Ring-1 envelope
/// findings in its exact per-artifact slot and insert set-level Widget UX / UI
/// law checks between the pre-UX and post-UX Ring-0 phases with
/// higher-severity-wins deduplication.
#[must_use]
pub fn check_artifact_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    let phases = norn_checker::check_artifact_set_ring0_phases(artifacts, context);
    let normalized_context = EntityArtifactSetContext {
        base: &phases.normalized_base,
        ..*context
    };

    let merged_per_artifact = phases
        .normalized_artifacts
        .iter()
        .zip(phases.per_artifact)
        .flat_map(|(artifact, ring0_local)| {
            if matches!(artifact.payload, ArtifactPayload::Widget(_)) {
                merge_widget_diagnostics(ring0_local, widget_extra_diagnostics(artifact))
            } else {
                ring0_local
            }
        });

    let ux_findings = if phases.resolved_ok {
        check_widget_ux_set(&phases.normalized_artifacts, &normalized_context)
    } else {
        Vec::new()
    };

    norn_checker::entity_artifacts::dedupe_findings(
        merged_per_artifact
            .chain(phases.pre_ux_set)
            .chain(ux_findings)
            .chain(phases.post_ux_set),
    )
}

fn check_artifact_set_with_receipt_inner(
    sources: &ReceiptSources,
    context: &EntityArtifactSetContext<'_>,
    limits: &ResourceLimitProfile,
    flow_source_map: Option<&FlowSourceMap>,
) -> Result<CheckedArtifactSet, ReceiptError> {
    let context = sources.validated_context(context)?;
    let artifacts = sources.artifacts();
    let limit_profile = limits.reference()?;
    let mut diagnostics = check_artifact_set(artifacts, &context);
    for artifact in artifacts {
        if matches!(
            &artifact.payload,
            ArtifactPayload::Setting(s)
                if s.setting_version.as_value()
                    != Some(&norn_ir::JsonValue::String(
                        norn_checker::receipt::CURRENT_SETTING_VERSION.into()
                    ))
        ) {
            diagnostics.push(Diagnostic {
                witness: None,
                code: DiagnosticCode::new("setting.validation.invalid_payload"),
                severity: DiagnosticSeverity::Error,
                message: format!(
                    "new setting admissions require settingVersion {}; retained value profiles require their verified receipt",
                    norn_checker::receipt::CURRENT_SETTING_VERSION
                ),
                span: None,
                artifact_id: Some(artifact.artifact_id.clone()),
                remediation: Some(DiagnosticRemediation::FixDefinition),
            });
        }
    }
    if let Some(flow_source_map) = flow_source_map {
        diagnostics.extend(norn_checker::validate_flow_source_map(
            flow_source_map,
            artifacts,
            context.base,
        ));
    }
    let mut type_env = None;
    if !diagnostics.iter().any(Diagnostic::is_error) {
        match norn_checker::entity_scopes::receipt_type_env(artifacts, context.base, limits) {
            Ok(env_ref) => type_env = Some(env_ref),
            Err(Ok(errors)) => diagnostics.extend(errors.iter().map(|error| Diagnostic {
                witness: None,
                code: DiagnosticCode::new(norn_checker::receipt::codes::ENVIRONMENT_UNDERIVABLE),
                severity: DiagnosticSeverity::Error,
                message: format!(
                    "the ZEN Expression type environment of the artifact set cannot be derived: {error}"
                ),
                span: None,
                artifact_id: None,
                remediation: Some(DiagnosticRemediation::FixDefinition),
            })),
            Err(Err(error)) => return Err(error.into()),
        }
    }
    let has_errors = diagnostics.iter().any(Diagnostic::is_error);
    let receipt = CheckReceipt {
        checker: norn_checker::receipt::CHECKER_NAME.to_owned(),
        checker_version: norn_checker::receipt::CHECKER_VERSION.to_owned(),
        artifact_set: sources.identity().clone(),
        limit_profile,
        expr_profile: norn_checker::receipt::EXPR_PROFILE.to_owned(),
        identifier_profile: IDENTIFIER_ADMISSION_PROFILE.to_owned(),
        type_env,
        diagnostics: DiagnosticsDigest::of(&diagnostics)?,
        admitted: !has_errors,
        flow_source_map: flow_source_map.cloned(),
        law_enrollment: if has_errors {
            Vec::new()
        } else {
            norn_checker::law_enrollment::inherited_laws(context.base)?
        },
    };
    Ok(CheckedArtifactSet {
        diagnostics,
        receipt,
    })
}

/// Check `artifacts` against `context` (Ring-0 + Ring-1 UI checks) under
/// `limits` and issue the [`norn_checker::CheckReceipt`].
///
/// # Errors
///
/// As [`norn_checker::check_artifact_set_with_receipt`].
pub fn check_artifact_set_with_receipt(
    artifacts: &ReceiptSources,
    context: &EntityArtifactSetContext<'_>,
    limits: &ResourceLimitProfile,
) -> Result<CheckedArtifactSet, ReceiptError> {
    check_artifact_set_with_receipt_inner(artifacts, context, limits, None)
}

/// Check `artifacts` with an optional flow source map (Ring-0 + Ring-1 UI
/// checks) under `limits` and issue the [`norn_checker::CheckReceipt`].
///
/// # Errors
///
/// As [`norn_checker::check_artifact_set_with_flow_source_map`].
pub fn check_artifact_set_with_flow_source_map(
    artifacts: &ReceiptSources,
    context: &EntityArtifactSetContext<'_>,
    limits: &ResourceLimitProfile,
    flow_source_map: Option<&FlowSourceMap>,
) -> Result<CheckedArtifactSet, ReceiptError> {
    check_artifact_set_with_receipt_inner(artifacts, context, limits, flow_source_map)
}

/// EXPLAIN-CLOSURE-1 composed strictly above Ring-0 + Ring-1 UI/widget
/// admission checks.
///
/// # Errors
///
/// As [`norn_checker::explain::explain`].
pub fn explain(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
    kind: ExplainKind,
    id: &str,
) -> Result<ExplainReport, Vec<Diagnostic>> {
    if artifacts.len().saturating_add(context.base.len()) > 16_384 {
        return Err(vec![Diagnostic {
            witness: None,
            code: DiagnosticCode::new(norn_checker::verification::codes::CLOSURE_REFUSED),
            severity: DiagnosticSeverity::Error,
            message: "definition count exceeds the graph node budget".to_owned(),
            span: None,
            artifact_id: None,
            remediation: None,
        }]);
    }
    let diagnostics = check_artifact_set(artifacts, context);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Err(diagnostics);
    }
    norn_checker::explain::project_report(artifacts, context, kind, id)
}

#[cfg(test)]
mod ring1_composition_tests {
    use std::collections::BTreeSet;

    use norn_checker::{DiagnosticsDigest, EntityArtifactSetContext, ReceiptSources};
    use norn_ir::{ArtifactEnvelope, DiagnosticSeverity, ResourceLimitProfile, RuntimeValue};
    use serde_json::json;

    use crate::{
        check_artifact, check_artifact_json, check_artifact_set, check_artifact_set_with_receipt,
        merge_legacy_widget_diagnostics, validate_artifact,
    };

    fn widget_fixture_json() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        Ok(serde_json::from_str(include_str!(
            "../../../fixtures/ir/retail-replenishment.widget.json"
        ))?)
    }

    fn cyclic_concept_fixture() -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
        Ok(serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": "test.concept.cycle",
            "kind": "defined_concept",
            "namespace": "test",
            "name": "Cycle",
            "version": "1.0.0",
            "lifecycle": "draft",
            "scope": {},
            "dependencies": [{
                "artifactId": "test.concept.cycle",
                "version": "1.0.0",
                "kind": "defined_concept",
                "required": true
            }],
            "provenance": { "source": "system" },
            "payload": {
                "id": "test.concept.cycle",
                "version": "1.0.0",
                "base": {
                    "artifactId": "test.Item.entity",
                    "version": "1.0.0",
                    "member": { "kind": "type", "typeName": "test.Item" }
                },
                "predicate": "true",
                "bounds": ResourceLimitProfile::norn_limits_v1(),
                "dependencies": [{
                    "artifactId": "test.concept.cycle",
                    "version": "1.0.0",
                    "member": { "kind": "artifact" }
                }]
            }
        }))?)
    }

    fn unpinned_unit_entity_fixture() -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
        Ok(serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": "test.UnpinnedWeight.entity",
            "kind": "entity",
            "namespace": "test",
            "name": "UnpinnedWeight",
            "version": "1.0.0",
            "lifecycle": "draft",
            "scope": {},
            "dependencies": [],
            "provenance": { "source": "system" },
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "test.UnpinnedWeight",
                    "properties": [{
                        "id": "mass",
                        "type": {
                            "kind": "decimal",
                            "scale": 3,
                            "unit": { "system": "ucum", "code": "kg" }
                        }
                    }]
                }
            }
        }))?)
    }

    #[test]
    fn duplicate_widget_artifact_id_preserves_copy2_local_error_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let valid_copy: ArtifactEnvelope = serde_json::from_value(widget_fixture_json()?)?;
        let mut bad_copy_json = widget_fixture_json()?;
        if let Some(obj) = bad_copy_json.as_object_mut() {
            obj.insert("irVersion".to_owned(), json!("9.9"));
            obj.insert("dependencies".to_owned(), json!([]));
        } else {
            return Err("widget fixture must be an object".into());
        }
        let bad_copy: ArtifactEnvelope = serde_json::from_value(bad_copy_json)?;

        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        let findings = check_artifact_set(&[valid_copy, bad_copy], &context);
        let (Some(ir_version_idx), Some(missing_dep_idx)) = (
            findings.iter().position(|d| {
                d.is_error() && d.code.as_str() == "ir.validation.unsupported_version"
            }),
            findings.iter().position(|d| {
                d.is_error() && d.code.as_str() == "widget.validation.missing_dependency"
            }),
        ) else {
            return Err(format!(
                "Both copy-2 Ring-0 and Ring-1 errors must be reported: {findings:#?}"
            )
            .into());
        };
        assert!(
            ir_version_idx < missing_dep_idx,
            "Copy 2's ir.validation.unsupported_version must precede copy 2's widget.validation.missing_dependency: {findings:#?}"
        );
        Ok(())
    }

    #[test]
    fn projection_hints_null_reports_legacy_message_across_all_four_entrypoints()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut widget_json = widget_fixture_json()?;
        if let Some(payload) = widget_json
            .get_mut("payload")
            .and_then(serde_json::Value::as_object_mut)
        {
            payload.insert("projectionHints".to_owned(), serde_json::Value::Null);
        } else {
            return Err("widget fixture payload must be an object".into());
        }
        let widget_env: ArtifactEnvelope = serde_json::from_value(widget_json.clone())?;

        let legacy_err = match validate_artifact(&widget_env) {
            Ok(()) => return Err("validate_artifact must reject projectionHints: null".into()),
            Err(diags) => diags,
        };
        assert!(
            legacy_err.iter().any(|d| {
                d.code == "widget.validation.invalid_payload"
                    && d.path.as_deref() == Some("payload.projectionHints")
                    && d.message == "value must be an object"
            }),
            "validate_artifact must preserve legacy message: {legacy_err:#?}"
        );

        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        for (label, findings) in [
            ("check_artifact", check_artifact(&widget_env)),
            ("check_artifact_json", check_artifact_json(&widget_json)),
            (
                "check_artifact_set",
                check_artifact_set(std::slice::from_ref(&widget_env), &context),
            ),
        ] {
            assert!(
                findings.iter().any(|d| {
                    d.is_error()
                        && d.code.as_str() == "widget.validation.invalid_payload"
                        && d.span
                            .as_ref()
                            .is_some_and(|s| s.pointer == "/payload/projectionHints")
                        && d.message == "value must be an object"
                }),
                "{label} must preserve legacy message 'value must be an object': {findings:#?}"
            );
        }
        Ok(())
    }

    fn ux_widget_fixture() -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
        let mut widget_json = widget_fixture_json()?;
        if let Some(obj) = widget_json.as_object_mut() {
            obj.insert("dependencies".to_owned(), json!([]));
            obj.insert("irVersion".to_owned(), json!("9.9"));
        } else {
            return Err("widget fixture must be an object".into());
        }
        if let Some(payload) = widget_json
            .get_mut("payload")
            .and_then(serde_json::Value::as_object_mut)
        {
            payload.insert(
                "ux".to_owned(),
                json!({
                    "uxVersion": "0.1",
                    "pages": [],
                    "dataBindings": [],
                    "actionBindings": [{
                        "bindingId": "invoke",
                        "action": { "id": "missing.action", "version": "0.1.0" },
                        "inputMapping": {},
                        "inputs": {},
                        "requiredCapability": "test.invoke"
                    }],
                    "fields": [],
                    "forms": [],
                    "actionPlacements": [],
                    "navigationItems": [],
                    "overrides": []
                }),
            );
        } else {
            return Err("widget fixture payload must be an object".into());
        }
        Ok(serde_json::from_value(widget_json)?)
    }

    #[test]
    fn mixed_set_preserves_origin_main_phase_order_and_receipt_digest()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        let artifacts = [
            ux_widget_fixture()?,
            cyclic_concept_fixture()?,
            unpinned_unit_entity_fixture()?,
        ];
        let findings = check_artifact_set(&artifacts, &context);

        let (
            Some(ir_version_idx),
            Some(missing_dep_idx),
            Some(ux_set_idx),
            Some(unit_idx),
            Some(cycle_idx),
        ) = (
            findings.iter().position(|d| {
                d.is_error() && d.code.as_str() == "ir.validation.unsupported_version"
            }),
            findings.iter().position(|d| {
                d.is_error() && d.code.as_str() == "widget.validation.missing_dependency"
            }),
            findings.iter().position(|d| {
                d.is_error()
                    && d.code.as_str() == "widget.validation.invalid_payload"
                    && d.span
                        .as_ref()
                        .is_some_and(|s| s.pointer.starts_with("/payload/ux"))
            }),
            findings
                .iter()
                .position(|d| d.is_error() && d.code.as_str() == "core.values.unit_mismatch"),
            findings
                .iter()
                .position(|d| d.is_error() && d.code.as_str() == "core.graph.cycle"),
        )
        else {
            return Err(format!(
                "Expected local, ux-set, unit_admission, and defined_concepts errors: {findings:#?}"
            )
            .into());
        };

        assert!(
            ir_version_idx < missing_dep_idx
                && missing_dep_idx < ux_set_idx
                && ux_set_idx < unit_idx
                && unit_idx < cycle_idx,
            "Exact origin/main phase order must hold (local < widget_ux_set < unit_mismatch < cycle): {findings:#?}"
        );
        assert!(
            !findings
                .iter()
                .any(|d| d.code.as_str() == norn_checker::codes::WIDGET_RING1_REQUIRED),
            "Ring-1 must strip WIDGET_RING1_REQUIRED: {findings:#?}"
        );

        let raw_sources: Vec<RuntimeValue> = artifacts
            .iter()
            .map(|a| Ok(RuntimeValue::from_json_text(serde_json::to_string(a)?)?))
            .collect::<Result<_, Box<dyn std::error::Error>>>()?;
        let sources = ReceiptSources::new(&raw_sources, &[])?;
        let limits = ResourceLimitProfile::norn_limits_v1();

        let ring0_receipt =
            norn_checker::check_artifact_set_with_receipt(&sources, &context, &limits)?;
        assert!(!ring0_receipt.receipt.admitted);
        assert!(ring0_receipt.diagnostics.iter().any(|d| {
            d.is_error() && d.code.as_str() == norn_checker::codes::WIDGET_RING1_REQUIRED
        }));

        let ring1_receipt = check_artifact_set_with_receipt(&sources, &context, &limits)?;
        assert!(!ring1_receipt.receipt.admitted);
        assert_eq!(ring1_receipt.diagnostics, findings);
        assert_eq!(
            ring1_receipt.receipt.diagnostics,
            DiagnosticsDigest::of(&findings)?
        );
        Ok(())
    }

    #[test]
    fn merge_legacy_widget_diagnostics_preserves_ring0_error_over_ring1_warning_and_info() {
        let error = norn_ir::Diagnostic {
            code: "widget.validation.invalid_payload".to_owned(),
            severity: DiagnosticSeverity::Error,
            message: "ring-0 error".to_owned(),
            artifact_id: Some("w".to_owned()),
            path: Some("payload.projectionHints".to_owned()),
            remediation: None,
        };
        let marker = norn_ir::Diagnostic {
            code: norn_checker::codes::WIDGET_RING1_REQUIRED.to_owned(),
            severity: DiagnosticSeverity::Error,
            message: "ring1 required".to_owned(),
            artifact_id: Some("w".to_owned()),
            path: Some("payload".to_owned()),
            remediation: None,
        };
        let warning = norn_ir::Diagnostic {
            severity: DiagnosticSeverity::Warning,
            message: "ring-1 warning".to_owned(),
            ..error.clone()
        };
        let info = norn_ir::Diagnostic {
            severity: DiagnosticSeverity::Info,
            message: "ring-1 info".to_owned(),
            ..error.clone()
        };

        let merged = merge_legacy_widget_diagnostics(
            vec![error.clone(), marker.clone()],
            vec![warning.clone(), info],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged.first().map(|d| &d.severity),
            Some(&DiagnosticSeverity::Error)
        );

        let upgraded = merge_legacy_widget_diagnostics(vec![warning, marker], vec![error]);
        assert_eq!(upgraded.len(), 1);
        assert_eq!(
            upgraded.first().map(|d| &d.severity),
            Some(&DiagnosticSeverity::Error)
        );
    }

    #[test]
    fn view_states_empty_precedes_projection_hints_null() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut widget_json = widget_fixture_json()?;
        if let Some(payload) = widget_json
            .get_mut("payload")
            .and_then(serde_json::Value::as_object_mut)
        {
            payload.insert("viewStates".to_owned(), json!([]));
            payload.insert("projectionHints".to_owned(), serde_json::Value::Null);
        } else {
            return Err("widget fixture payload must be an object".into());
        }
        let widget_env: ArtifactEnvelope = serde_json::from_value(widget_json)?;

        let legacy_err = match validate_artifact(&widget_env) {
            Ok(()) => return Err("validate_artifact must reject invalid widget".into()),
            Err(diags) => diags,
        };
        let (Some(vs_legacy), Some(ph_legacy)) = (
            legacy_err
                .iter()
                .position(|d| d.path.as_deref() == Some("payload.viewStates")),
            legacy_err.iter().position(|d| {
                d.path.as_deref() == Some("payload.projectionHints")
                    && d.message == "value must be an object"
            }),
        ) else {
            return Err(format!("Expected viewStates and projectionHints: {legacy_err:#?}").into());
        };
        assert!(
            vs_legacy < ph_legacy,
            "validate_artifact must emit viewStates before projectionHints: {legacy_err:#?}"
        );

        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        for (label, findings) in [
            ("check_artifact", check_artifact(&widget_env)),
            (
                "check_artifact_set",
                check_artifact_set(std::slice::from_ref(&widget_env), &context),
            ),
        ] {
            let (Some(vs_idx), Some(ph_idx)) = (
                findings.iter().position(|d| {
                    d.span
                        .as_ref()
                        .is_some_and(|s| s.pointer == "/payload/viewStates")
                }),
                findings.iter().position(|d| {
                    d.span
                        .as_ref()
                        .is_some_and(|s| s.pointer == "/payload/projectionHints")
                        && d.message == "value must be an object"
                }),
            ) else {
                return Err(format!(
                    "{label}: expected viewStates and projectionHints: {findings:#?}"
                )
                .into());
            };
            assert!(
                vs_idx < ph_idx,
                "{label} must emit viewStates before projectionHints: {findings:#?}"
            );
        }
        Ok(())
    }
}
