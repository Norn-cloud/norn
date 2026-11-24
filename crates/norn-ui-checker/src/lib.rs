//! `norn-ui-checker`: Ring-1 UI and Widget UX admission diagnostics (#1613).
//!
//! Composes strictly above Ring-0 [`norn_checker`] semantic admission with
//! Ring-1 UI/widget validation: widget artifact envelopes ([`envelope`]),
//! typed Widget UX contracts ([`widget_ux`]), interaction rules
//! ([`ui_interaction`]), page statecharts ([`ui_machine`]), semantic views
//! ([`ui_view`]), catalog components ([`ui_catalog`]), and page-to-lifecycle
//! composition ([`ui_lifecycle`]).

// Deterministic core policy (#3299).
#![deny(clippy::disallowed_types)]

pub mod envelope;
pub mod ui_catalog;
pub mod ui_interaction;
pub mod ui_lifecycle;
pub mod ui_machine;
pub mod ui_view;
pub mod widget_ux;

use std::collections::BTreeMap;

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

const fn legacy_severity_rank(severity: &DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Info => 0,
        DiagnosticSeverity::Warning => 1,
        DiagnosticSeverity::Error => 2,
    }
}

fn dedupe_legacy_findings(
    diagnostics: impl IntoIterator<Item = norn_ir::Diagnostic>,
) -> Vec<norn_ir::Diagnostic> {
    let mut out: Vec<norn_ir::Diagnostic> = Vec::new();
    for finding in diagnostics {
        if let Some(existing) = out.iter_mut().find(|existing| {
            existing.code == finding.code
                && existing.artifact_id == finding.artifact_id
                && existing.path == finding.path
        }) {
            if legacy_severity_rank(&finding.severity) > legacy_severity_rank(&existing.severity) {
                *existing = finding;
            }
        } else {
            out.push(finding);
        }
    }
    out
}

fn splice_legacy_widget_findings(
    ring0: Vec<norn_ir::Diagnostic>,
    extra: Vec<norn_ir::Diagnostic>,
) -> Vec<norn_ir::Diagnostic> {
    let mut merged = Vec::with_capacity(ring0.len().saturating_add(extra.len()));
    let mut pending = Some(extra);
    for finding in ring0 {
        if finding.code == norn_checker::codes::WIDGET_RING1_REQUIRED {
            if let Some(extras) = pending.take() {
                merged.extend(extras);
            }
        } else {
            merged.push(finding);
        }
    }
    if let Some(extras) = pending {
        merged.extend(extras);
    }
    dedupe_legacy_findings(merged)
}

fn splice_single_widget_findings(
    ring0: Vec<Diagnostic>,
    extra: Vec<Diagnostic>,
) -> Vec<Diagnostic> {
    let mut merged = Vec::with_capacity(ring0.len().saturating_add(extra.len()));
    let mut pending = Some(extra);
    for finding in ring0 {
        if finding.code.as_str() == norn_checker::codes::WIDGET_RING1_REQUIRED {
            if let Some(extras) = pending.take() {
                merged.extend(extras);
            }
        } else {
            merged.push(finding);
        }
    }
    if let Some(extras) = pending {
        merged.extend(extras);
    }
    norn_checker::entity_artifacts::dedupe_findings(merged)
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

fn normalize_artifacts(artifacts: &[ArtifactEnvelope]) -> Vec<ArtifactEnvelope> {
    artifacts
        .iter()
        .map(
            |artifact| match norn_checker::layout::normalize_artifact(artifact) {
                Ok((normalized, _layout)) => normalized,
                Err(_layout_findings) => artifact.clone(),
            },
        )
        .collect()
}

/// Validate one artifact envelope with Ring-0 checks completed first, then
/// Ring-1 widget payload/dependency validation spliced in-place at the
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
        splice_legacy_widget_findings(ring0, envelope::validate_widget_extra(artifact, &payload));
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
/// widget payload/dependency validation spliced in-place at the
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
    splice_single_widget_findings(findings, widget_extra_diagnostics(artifact))
}

/// Decode an artifact envelope from wire JSON and check it with Ring-0 checks
/// completed first, then Ring-1 widget payload/dependency validation spliced
/// in-place at the [`norn_checker::codes::WIDGET_RING1_REQUIRED`] marker.
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
        return splice_single_widget_findings(findings, extra);
    }
    findings
        .into_iter()
        .filter(|d| d.code.as_str() != norn_checker::codes::WIDGET_RING1_REQUIRED)
        .collect()
}

/// Resolve `artifacts` against `context.base` and run every Ring-0 set check
/// to completion first, then splice each widget's Ring-1 envelope findings
/// in-place at its [`norn_checker::codes::WIDGET_RING1_REQUIRED`] marker and
/// append Ring-1 Widget UX / UI law checks with higher-severity-wins
/// deduplication.
#[must_use]
pub fn check_artifact_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    let ring0_findings = norn_checker::check_artifact_set(artifacts, context);

    let normalized_artifacts = normalize_artifacts(artifacts);
    let normalized_base = normalize_artifacts(context.base);
    let normalized_context = EntityArtifactSetContext {
        base: &normalized_base,
        ..*context
    };

    let mut widget_extras: BTreeMap<String, Vec<Diagnostic>> = BTreeMap::new();
    for artifact in &normalized_artifacts {
        if matches!(artifact.payload, ArtifactPayload::Widget(_)) {
            widget_extras
                .entry(artifact.artifact_id.clone())
                .or_default()
                .extend(widget_extra_diagnostics(artifact));
        }
    }

    let mut merged = Vec::with_capacity(ring0_findings.len());
    for finding in ring0_findings {
        if finding.code.as_str() == norn_checker::codes::WIDGET_RING1_REQUIRED {
            if let Some(id) = finding.artifact_id.as_deref()
                && let Some(extras) = widget_extras.remove(id)
            {
                merged.extend(extras);
            }
        } else {
            merged.push(finding);
        }
    }
    for (_id, extras) in widget_extras {
        merged.extend(extras);
    }

    if norn_checker::resolve_artifact_set(&normalized_artifacts, &normalized_base).is_ok() {
        merged.extend(check_widget_ux_set(
            &normalized_artifacts,
            &normalized_context,
        ));
    }
    norn_checker::entity_artifacts::dedupe_findings(merged)
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
        match norn_checker::derive_type_env(artifacts, context.base, limits) {
            Ok(env) => type_env = Some(env.reference()?),
            Err(errors) => diagnostics.extend(errors.iter().map(|error| Diagnostic {
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

    use norn_checker::{EntityArtifactSetContext, ReceiptSources};
    use norn_ir::{ArtifactEnvelope, DiagnosticSeverity, ResourceLimitProfile, RuntimeValue};
    use serde_json::json;

    use crate::{
        check_artifact_set, check_artifact_set_with_receipt, splice_legacy_widget_findings,
    };

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
                            "precision": 12,
                            "scale": 3,
                            "unit": { "system": "ucum", "code": "kg" }
                        }
                    }]
                }
            }
        }))?)
    }

    #[test]
    fn ring0_set_unit_and_defined_concept_checks_complete_in_original_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut bad_widget: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/ir/retail-replenishment.widget.json"
        ))?;
        // Strip dependencies so Ring-1 emits `widget.validation.missing_dependency`,
        // AND break Ring-0 envelope (`irVersion`) so Ring-0 emits its own error.
        if let Some(obj) = bad_widget.as_object_mut() {
            obj.insert("dependencies".to_owned(), json!([]));
            obj.insert("irVersion".to_owned(), json!("9.9"));
        } else {
            return Err("widget fixture must be an object".into());
        }
        let widget_env: ArtifactEnvelope = serde_json::from_value(bad_widget.clone())?;

        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        let artifacts = [
            widget_env,
            cyclic_concept_fixture()?,
            unpinned_unit_entity_fixture()?,
        ];

        let findings = check_artifact_set(&artifacts, &context);
        let ir_version_idx = findings
            .iter()
            .position(|d| d.is_error() && d.code.as_str() == "ir.validation.unsupported_version");
        let missing_dep_idx = findings.iter().position(|d| {
            d.is_error() && d.code.as_str() == "widget.validation.missing_dependency"
        });
        let unit_idx = findings
            .iter()
            .position(|d| d.is_error() && d.code.as_str() == "core.values.unit_mismatch");
        let cycle_idx = findings
            .iter()
            .position(|d| d.is_error() && d.code.as_str() == "core.graph.cycle");

        assert!(
            ir_version_idx.is_some(),
            "Ring-0 per-artifact error must survive Ring-1 composition: {findings:#?}"
        );
        assert!(
            missing_dep_idx.is_some(),
            "Ring-1 widget error must also be reported: {findings:#?}"
        );
        assert!(
            unit_idx.is_some(),
            "Ring-0 unit_admission::check must complete independently: {findings:#?}"
        );
        assert!(
            cycle_idx.is_some(),
            "Ring-0 defined_concepts::check_cycles must complete independently: {findings:#?}"
        );
        assert!(
            ir_version_idx < missing_dep_idx && missing_dep_idx < unit_idx && unit_idx < cycle_idx,
            "Spliced widget envelope findings must precede unit_admission and defined_concepts: {findings:#?}"
        );
        assert!(
            !findings
                .iter()
                .any(|d| d.code.as_str() == norn_checker::codes::WIDGET_RING1_REQUIRED),
            "Ring-1 must strip only WIDGET_RING1_REQUIRED: {findings:#?}"
        );

        let raw_widget = RuntimeValue::from_json_text(serde_json::to_string(&bad_widget)?)?;
        let sources = ReceiptSources::new(&[raw_widget], &[])?;
        let limits = ResourceLimitProfile::norn_limits_v1();

        let ring0_receipt =
            norn_checker::check_artifact_set_with_receipt(&sources, &context, &limits)?;
        assert!(
            !ring0_receipt.receipt.admitted,
            "Standalone Ring-0 receipt must fail closed on widgets"
        );
        assert!(ring0_receipt.diagnostics.iter().any(|d| {
            d.is_error() && d.code.as_str() == norn_checker::codes::WIDGET_RING1_REQUIRED
        }));

        let ring1_receipt = check_artifact_set_with_receipt(&sources, &context, &limits)?;
        assert!(
            !ring1_receipt.receipt.admitted,
            "Ring-1 receipt must refuse widget with Ring-0 and Ring-1 errors"
        );
        Ok(())
    }

    #[test]
    fn splice_legacy_widget_findings_preserves_ring0_error_over_ring1_warning_and_info() {
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

        let merged = splice_legacy_widget_findings(
            vec![error.clone(), marker.clone()],
            vec![warning.clone(), info],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged.first().map(|d| &d.severity),
            Some(&DiagnosticSeverity::Error)
        );

        let upgraded = splice_legacy_widget_findings(vec![warning, marker], vec![error]);
        assert_eq!(upgraded.len(), 1);
        assert_eq!(
            upgraded.first().map(|d| &d.severity),
            Some(&DiagnosticSeverity::Error)
        );
    }
}
