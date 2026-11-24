//! `norn-checker`: shared contract/lifecycle diagnostics.
//!
//! Ring 0 — pure, portable semantic core; see `AGENTS.md` for the allowed and
//! forbidden dependencies. `norn-ir` holds the typed IR; every check over it
//! lives here (SKELETON.md: "`validate_*` functions move to `norn-checker`").
//!
//! T1-CHK-1 (#2524) moved the existing validators out of `norn-ir` with zero
//! behaviour change: artifact-envelope and graph-load checks ([`envelope`]),
//! ontology packs ([`ontology`]), sealing ([`sealing`]), the quantity-ledger
//! checker and posting boundary ([`quantity_ledger`], [`posting`]). They still
//! emit the legacy [`norn_ir::Diagnostic`] wire type; [`Diagnostic`] is the
//! checker model with stable codes and JSON-pointer [`Span`]s that they map
//! onto. Every code is registered in [`codes`], and [`diagnostics_to_json`]
//! is the stable JSON output (T1-CHK-2).
//!
//! [`check_artifact_set_with_receipt`] issues the [`CheckReceipt`] of an
//! artifact-set check: the set's identity, the resource-limit profile, the
//! ZEN Expression type environment and checker profile, the checker version,
//! and a digest of the findings ([`receipt`], #2851).

// Deterministic core policy (#3299); host adapters retain their own arithmetic/collections.
#![deny(clippy::disallowed_types)]

pub mod action_set;
pub mod agent;
pub mod artifact_set;
pub mod classification;
pub mod codes;
mod concept_roles;
pub mod connector_path;
pub mod consistency;
/// Conservative action/policy projection for finite control analysis.
pub mod controls_adapter;
mod defined_concepts;
pub mod definition_catalog;
pub mod definitions_diff;
pub mod deontic;
mod diagnostic;
pub mod duties;
pub mod engine_release;
pub mod entity_artifacts;
pub mod entity_instance;
pub mod entity_lowering;
pub mod entity_model;
pub mod envelope;
pub mod exact_rational;
mod finite;
/// The `perform` records of a lowered flow's decide rule.
pub mod flow_performs;
pub mod flow_source_map;
pub mod function;
pub mod graph;
/// Closure-keyed structural admission evidence reuse.
pub mod incremental;
mod inheritance;
pub mod interfaces;
pub mod jdm_profile;
pub mod labels;
pub mod layout;
pub mod lifecycle_soundness;
pub mod lifecycle_writes;
pub mod lifecycles;
pub mod lint;
pub mod mapping;
mod markings;
pub mod metric;
pub mod metric_interchange;
mod model;
mod model_privacy;
pub mod posting;
pub mod soundness;
pub mod source_agreement;
pub mod table_analysis;
pub mod tool_effect;
mod unit_admission;
mod unit_codes;
pub use metric::check_metrics_and_questions;
pub mod explain;
pub mod law_enrollment;
pub mod law_examples;
pub mod law_exceptions;
pub mod naming_series;
pub mod norm_precedence;
pub mod object_types;
pub mod obligations;
pub mod ontology;
pub mod parameter;
mod parameter_window;
mod predicate_property;
pub mod property_codec;
pub mod property_types;
pub mod quantity_ledger;
pub mod receipt;
pub mod rule_bindings;
pub mod sealing;
mod source_commands;
pub mod source_schema;
pub mod tool_ports;
pub mod transform;
pub mod typed_roots;
pub mod verification;

pub use action_set::{check_action_firing, check_action_set};
pub use agent::{AgentContext, check_agent};
pub use artifact_set::{
    ResolvedArtifactSet, check_artifact_set, may_reference, resolve_artifact_set, scope_conflict,
};
pub use codes::{CodeEntry, Tier, conformance_manifest};
pub use definitions_diff::{
    AdditivityChange, DefinitionDiff, DefinitionId, Narrowing, diff_definition_sets,
};
pub use diagnostic::{
    DIAGNOSTICS_FORMAT, Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity,
    DiagnosticWitness, SourceSpan, Span, StructuralEvidence, WitnessMove, WitnessSide, WitnessStep,
    diagnostics_to_json,
};
pub use engine_release::{ENGINE_DOWNGRADE, ENGINE_UNKNOWN_VERSION, ENGINE_UPGRADE_UNSUPPORTED};
pub use entity_artifacts::{
    EntityArtifactSetContext, TenantLayer, check_artifact_set_entity_model, check_entity_artifacts,
    check_property_type_catalog_local,
};
pub use entity_instance::{
    EffectiveCatalog, EffectiveKey, EffectiveKind, EffectiveLifecycle, EffectiveLink,
    EffectiveObjectType, EffectiveProperty, IDENTITY_COLLISION, IDENTITY_REUSE,
    IDENTITY_REUSE_MESSAGE, InstanceError, KeyComponent, Unsupported, normalize_text,
};
pub use entity_lowering::{
    EntityForm, EntityObjectType, LoweredRelation, entity_object_type, entity_v1_deprecation,
    lower_cardinality_v1, lower_entity_v1, lower_relation_v1, relation_link,
};
pub use entity_model::{
    EffectiveViewRefused, EntityContext, EntityModel, check_entity_model, check_interface_local,
    check_object_type_extension_local, check_object_type_local, effective_object_types,
};
pub use envelope::{ArtifactValidation, plan_graph_load, validate_artifact};
pub use flow_source_map::validate_flow_source_map;
pub use jdm_profile::{RuleClosure, RuleClosureError, rule_closure};
pub use layout::{
    PreparedArtifact, extract_inline_layout, hydrate_inline_layout,
    layout_merge_conflict_diagnostic, prepare_artifact_source, prune_layout,
    widget_list_column_key,
};
pub use lifecycle_writes::check_lifecycle_write_set;
pub use lifecycles::{
    DeclaredLifecycle, LifecycleWrite, check_lifecycle_writes, check_transition_firing,
    declared_lifecycles,
};
pub use markings::marking_catalog_from_artifacts;
pub use naming_series::{NamingPatternError, NamingPatternToken, parse_naming_series_pattern};
pub use object_types::check_object_type_defs;
pub use ontology::{
    LinkCoverage, ontology_term_is_a, validate_link_registry, validate_ontology_instance,
    validate_ontology_instance_in_registry, validate_ontology_pack, validate_ontology_registry,
};
pub use property_types::{check_property_type_defs, resolved_property_type};
pub use receipt::{
    ArtifactRef, ArtifactSetRef, CheckReceipt, CheckedArtifactSet, DiagnosticsDigest,
    IDENTIFIER_ADMISSION_PROFILE, ReceiptError, ReceiptSources, ReceiptVersion,
    admission_receipt_sha256, check_artifact_set_for_install,
    check_artifact_set_for_install_with_flow_source_map, check_artifact_set_with_flow_source_map,
    check_artifact_set_with_receipt, check_artifact_set_with_verified_receipt, derive_type_env,
    recover_legacy_admission_receipt, verify_admission_receipt,
};

pub use sealing::{
    seal, validate_customization_overlay, validate_runtime_capability_backstop,
    validate_seal_upgrade,
};
pub use typed_roots::decode_artifact_json;

/// Crate-private strictly additive Ring-1 UI validation hook composed over
/// Ring-0 checks. Ring-0 always executes its own envelope, typed-root,
/// entity-model, action, rule, lifecycle, mapping, law, and receipt checks
/// unconditionally first; hooks only return additional diagnostics to append.
pub(crate) trait Ring1UiHook {
    fn validate_widget_payload(
        &self,
        artifact: &norn_ir::ArtifactEnvelope,
        payload: &norn_ir::JsonValue,
    ) -> Vec<norn_ir::Diagnostic>;

    fn check_ui_set(
        &self,
        artifacts: &[norn_ir::ArtifactEnvelope],
        context: &EntityArtifactSetContext<'_>,
    ) -> Vec<Diagnostic>;
}

/// Private Ring-1 composition seam restricted exclusively to `norn-ui-checker`
/// via the non-default `ring1-ui-internal` feature and enforced by
/// `norn-skeleton-guard::ring0_boundary`.
#[doc(hidden)]
#[cfg(feature = "ring1-ui-internal")]
pub mod __ring1_ui_seam {
    use crate::{
        CheckedArtifactSet, Diagnostic, EntityArtifactSetContext, IDENTIFIER_ADMISSION_PROFILE,
        ReceiptError, ReceiptSources, Ring1UiHook,
    };

    /// Internal seal token restricting Ring-1 UI composition to `norn-ui-checker`.
    #[derive(Clone, Copy, Debug)]
    pub struct Ring1UiSeal {
        _private: (),
    }

    impl Ring1UiSeal {
        /// Construct the internal seal token. `norn-skeleton-guard` rejects
        /// references outside `crates/norn-checker/` and `crates/norn-ui-checker/`.
        #[must_use]
        pub const fn new_internal() -> Self {
            Self { _private: () }
        }
    }

    /// Strictly additive Ring-1 UI validator trait (internal to `norn-ui-checker`).
    pub trait UiValidator {
        /// Validate an `ArtifactPayload::Widget` envelope payload after Ring-0
        /// envelope and typed-root checks complete.
        fn validate_widget_payload(
            &self,
            artifact: &norn_ir::ArtifactEnvelope,
            payload: &norn_ir::JsonValue,
        ) -> Vec<norn_ir::Diagnostic>;

        /// Run Ring-1 cross-artifact UI checks after all Ring-0 set checks complete.
        fn check_ui_set(
            &self,
            artifacts: &[norn_ir::ArtifactEnvelope],
            context: &EntityArtifactSetContext<'_>,
        ) -> Vec<Diagnostic>;
    }

    struct Adapter<'a>(&'a dyn UiValidator);

    impl Ring1UiHook for Adapter<'_> {
        fn validate_widget_payload(
            &self,
            artifact: &norn_ir::ArtifactEnvelope,
            payload: &norn_ir::JsonValue,
        ) -> Vec<norn_ir::Diagnostic> {
            self.0.validate_widget_payload(artifact, payload)
        }

        fn check_ui_set(
            &self,
            artifacts: &[norn_ir::ArtifactEnvelope],
            context: &EntityArtifactSetContext<'_>,
        ) -> Vec<Diagnostic> {
            self.0.check_ui_set(artifacts, context)
        }
    }

    /// Validate one artifact envelope with Ring-0 checks plus Ring-1 widget checks.
    ///
    /// # Errors
    /// As [`crate::validate_artifact`].
    pub fn validate_artifact(
        _seal: Ring1UiSeal,
        artifact: &norn_ir::ArtifactEnvelope,
        ui: &dyn UiValidator,
    ) -> Result<(), Vec<norn_ir::Diagnostic>> {
        let adapter = Adapter(ui);
        crate::envelope::validate_artifact_inner(artifact, Some(&adapter))
    }

    /// Check one artifact envelope with Ring-0 checks plus Ring-1 widget checks.
    #[must_use]
    pub fn check_artifact(
        _seal: Ring1UiSeal,
        artifact: &norn_ir::ArtifactEnvelope,
        ui: &dyn UiValidator,
    ) -> Vec<Diagnostic> {
        let adapter = Adapter(ui);
        crate::check_artifact_inner(artifact, Some(&adapter))
    }

    /// Decode and check one artifact JSON with Ring-0 checks plus Ring-1 widget checks.
    #[must_use]
    pub fn check_artifact_json(
        _seal: Ring1UiSeal,
        value: &serde_json::Value,
        ui: &dyn UiValidator,
    ) -> Vec<Diagnostic> {
        let adapter = Adapter(ui);
        crate::check_artifact_json_inner(value, Some(&adapter))
    }

    /// Run Ring-0 set checks to completion, then append Ring-1 UI set checks.
    #[must_use]
    pub fn check_artifact_set(
        _seal: Ring1UiSeal,
        artifacts: &[norn_ir::ArtifactEnvelope],
        context: &EntityArtifactSetContext<'_>,
        ui: &dyn UiValidator,
    ) -> Vec<Diagnostic> {
        let adapter = Adapter(ui);
        crate::artifact_set::check_artifact_set_inner(artifacts, context, Some(&adapter))
    }

    /// Issue an admission receipt over Ring-0 plus Ring-1 UI checks.
    ///
    /// # Errors
    /// As [`crate::check_artifact_set_with_receipt`].
    pub fn check_artifact_set_with_receipt(
        _seal: Ring1UiSeal,
        artifacts: &ReceiptSources,
        context: &EntityArtifactSetContext<'_>,
        limits: &norn_ir::ResourceLimitProfile,
        ui: &dyn UiValidator,
    ) -> Result<CheckedArtifactSet, ReceiptError> {
        let adapter = Adapter(ui);
        let context = artifacts.context(context)?;
        crate::receipt::check_artifact_set_with_profile(crate::receipt::ReceiptCheckRequest {
            artifacts: &artifacts.artifacts,
            context: &context,
            limits,
            artifact_set: artifacts.identity.clone(),
            identifier_profile: IDENTIFIER_ADMISSION_PROFILE,
            flow_source_map: None,
            retained: false,
            ui: Some(&adapter),
        })
    }

    /// Issue an admission receipt with optional flow source map over Ring-0 plus Ring-1 UI checks.
    ///
    /// # Errors
    /// As [`crate::check_artifact_set_with_flow_source_map`].
    pub fn check_artifact_set_with_flow_source_map(
        _seal: Ring1UiSeal,
        artifacts: &ReceiptSources,
        context: &EntityArtifactSetContext<'_>,
        limits: &norn_ir::ResourceLimitProfile,
        flow_source_map: Option<&norn_ir::FlowSourceMap>,
        ui: &dyn UiValidator,
    ) -> Result<CheckedArtifactSet, ReceiptError> {
        let adapter = Adapter(ui);
        let context = artifacts.context(context)?;
        crate::receipt::check_artifact_set_with_profile(crate::receipt::ReceiptCheckRequest {
            artifacts: &artifacts.artifacts,
            context: &context,
            limits,
            artifact_set: artifacts.identity.clone(),
            identifier_profile: IDENTIFIER_ADMISSION_PROFILE,
            flow_source_map,
            retained: false,
            ui: Some(&adapter),
        })
    }

    /// Explain a declaration after Ring-0 plus Ring-1 UI admission checks.
    ///
    /// # Errors
    /// As [`crate::explain::explain`].
    pub fn explain(
        _seal: Ring1UiSeal,
        artifacts: &[norn_ir::ArtifactEnvelope],
        context: &EntityArtifactSetContext<'_>,
        kind: norn_ir::explain::ExplainKind,
        id: &str,
        ui: &dyn UiValidator,
    ) -> Result<norn_ir::explain::ExplainReport, Vec<Diagnostic>> {
        let adapter = Adapter(ui);
        crate::explain::explain_inner(artifacts, context, kind, id, Some(&adapter))
    }
}

/// Check one artifact envelope and return every finding in the checker
/// [`Diagnostic`] model. The artifact is valid when no returned finding
/// satisfies [`Diagnostic::is_error`]; informational notes may remain.
///
/// Same checks, codes, and messages as [`validate_artifact`]; only the
/// location is re-expressed as a JSON-pointer [`Span`], resolved against the
/// envelope's wire JSON (see [`Span::from_legacy_path_in`]).
/// `ArtifactPayload::Widget` fails closed here because widget validation lives
/// in Ring-1 `norn-ui-checker`.
#[must_use]
pub fn check_artifact(artifact: &norn_ir::ArtifactEnvelope) -> Vec<Diagnostic> {
    check_artifact_inner(artifact, None)
}

#[must_use]
pub(crate) fn check_artifact_inner(
    artifact: &norn_ir::ArtifactEnvelope,
    ui: Option<&dyn Ring1UiHook>,
) -> Vec<Diagnostic> {
    if let Some(finding) = definition_catalog::catalog_write(artifact) {
        return vec![finding];
    }
    let (normalized, findings) = match crate::layout::normalize_artifact(artifact) {
        Ok((normalized, _layout)) => (normalized, Vec::new()),
        Err(findings) => (artifact.clone(), findings),
    };
    let legacy = crate::envelope::collect_artifact_diagnostics_inner(&normalized, ui);
    // Resolve dotted paths against the envelope's wire JSON so keys that
    // contain `.` stay one pointer segment.
    let mut checked = match serde_json::to_value(artifact) {
        Ok(document) => legacy
            .into_iter()
            .map(|diagnostic| Diagnostic::from_legacy_in(diagnostic, &document))
            .collect::<Vec<_>>(),
        Err(_unserializable) => legacy.into_iter().map(Diagnostic::from_legacy).collect(),
    };
    for finding in findings {
        if !checked.iter().any(|existing| {
            existing.code == finding.code
                && existing.artifact_id == finding.artifact_id
                && existing.span.as_ref().map(|span| &span.pointer)
                    == finding.span.as_ref().map(|span| &span.pointer)
        }) {
            checked.push(finding);
        }
    }
    checked
}

/// Decode an artifact envelope from its wire JSON and check it: the JSON/UI
/// admission path. Decode failures ([`decode_artifact_json`]) and envelope
/// findings ([`check_artifact`]) are both returned in the checker
/// [`Diagnostic`] model, with spans resolved against `value`.
/// `ArtifactPayload::Widget` fails closed here; use Ring-1 `norn-ui-checker`
/// to validate widget envelopes.
#[must_use]
pub fn check_artifact_json(value: &serde_json::Value) -> Vec<Diagnostic> {
    check_artifact_json_inner(value, None)
}

fn check_artifact_json_inner(
    value: &serde_json::Value,
    ui: Option<&dyn Ring1UiHook>,
) -> Vec<Diagnostic> {
    if let Some(finding) = definition_catalog::catalog_write_json(value) {
        return vec![finding];
    }
    match decode_artifact_json(value) {
        Ok(artifact) => check_artifact_inner(&artifact, ui),
        Err(legacy) => legacy
            .into_iter()
            .map(|diagnostic| Diagnostic::from_legacy_in(diagnostic, value))
            .collect(),
    }
}

/// Single-cell execution diagnostic shared with runtime admission.
pub mod execution_profile;

/// Whether `text` is a timestamp the engine admits: the RFC 3339 subset that
/// the mapping's `timestamp(...)` coercion and stored timestamps accept.
/// Callers outside the admission path (upload column typing) classify with
/// this instead of a looser parser of their own.
#[must_use]
pub fn is_canonical_timestamp(text: &str) -> bool {
    norn_expr::temporal::canonical_timestamp(text).is_ok()
}

#[cfg(test)]
mod ring1_ui_hook_tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use crate::{
        Diagnostic, DiagnosticCode, DiagnosticSeverity, EntityArtifactSetContext,
        IDENTIFIER_ADMISSION_PROFILE, ReceiptSources, Ring1UiHook, Span,
    };

    struct NoopHook;

    impl Ring1UiHook for NoopHook {
        fn validate_widget_payload(
            &self,
            _artifact: &norn_ir::ArtifactEnvelope,
            _payload: &norn_ir::JsonValue,
        ) -> Vec<norn_ir::Diagnostic> {
            Vec::new()
        }

        fn check_ui_set(
            &self,
            _artifacts: &[norn_ir::ArtifactEnvelope],
            _context: &EntityArtifactSetContext<'_>,
        ) -> Vec<Diagnostic> {
            Vec::new()
        }
    }

    struct WarningHook {
        code: &'static str,
        pointer: &'static str,
        artifact_id: &'static str,
    }

    impl Ring1UiHook for WarningHook {
        fn validate_widget_payload(
            &self,
            artifact: &norn_ir::ArtifactEnvelope,
            _payload: &norn_ir::JsonValue,
        ) -> Vec<norn_ir::Diagnostic> {
            vec![norn_ir::Diagnostic {
                code: self.code.to_owned(),
                severity: DiagnosticSeverity::Warning,
                message: "ring-1 advisory warning".to_owned(),
                artifact_id: Some(artifact.artifact_id.clone()),
                path: Some("payload".to_owned()),
                remediation: None,
            }]
        }

        fn check_ui_set(
            &self,
            _artifacts: &[norn_ir::ArtifactEnvelope],
            _context: &EntityArtifactSetContext<'_>,
        ) -> Vec<Diagnostic> {
            vec![Diagnostic {
                witness: None,
                code: DiagnosticCode::new(self.code),
                severity: DiagnosticSeverity::Warning,
                message: "ring-1 set warning colliding with ring-0 error".to_owned(),
                span: Some(Span {
                    pointer: self.pointer.to_owned(),
                    source: None,
                }),
                artifact_id: Some(self.artifact_id.to_owned()),
                remediation: None,
            }]
        }
    }

    struct CollidingWarningHook(String);

    impl Ring1UiHook for CollidingWarningHook {
        fn validate_widget_payload(
            &self,
            artifact: &norn_ir::ArtifactEnvelope,
            _payload: &norn_ir::JsonValue,
        ) -> Vec<norn_ir::Diagnostic> {
            vec![norn_ir::Diagnostic {
                code: "widget.validation.invalid_payload".to_owned(),
                severity: DiagnosticSeverity::Warning,
                message: "colliding warning".to_owned(),
                artifact_id: Some(artifact.artifact_id.clone()),
                path: Some("payload.projectionHints".to_owned()),
                remediation: None,
            }]
        }

        fn check_ui_set(
            &self,
            _artifacts: &[norn_ir::ArtifactEnvelope],
            _context: &EntityArtifactSetContext<'_>,
        ) -> Vec<Diagnostic> {
            vec![Diagnostic {
                witness: None,
                code: DiagnosticCode::new("widget.validation.invalid_payload"),
                severity: DiagnosticSeverity::Warning,
                message: "colliding set warning".to_owned(),
                span: Some(Span {
                    pointer: "/payload/projectionHints".to_owned(),
                    source: None,
                }),
                artifact_id: Some(self.0.clone()),
                remediation: None,
            }]
        }
    }

    #[test]
    fn noop_ring1_hook_cannot_suppress_ring0_envelope_set_receipt_or_explain_errors() {
        let mut bad_widget: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/ir/retail-replenishment.widget.json"
        ))
        .expect("widget fixture");
        bad_widget["irVersion"] = json!("9.9");
        let env: norn_ir::ArtifactEnvelope =
            serde_json::from_value(bad_widget.clone()).expect("widget envelope");
        let raw = norn_ir::RuntimeValue::from_json_text(
            serde_json::to_string(&bad_widget).expect("serialize"),
        )
        .expect("raw");
        let sources = ReceiptSources::new(&[raw], &[]).expect("sources");
        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        let limits = norn_ir::ResourceLimitProfile::norn_limits_v1();

        let err = crate::envelope::validate_artifact_inner(&env, Some(&NoopHook))
            .expect_err("Ring-0 envelope error must survive NoopHook");
        assert!(
            err.iter()
                .any(|d| d.code == "ir.validation.unsupported_version")
        );

        let set = crate::artifact_set::check_artifact_set_inner(
            std::slice::from_ref(&env),
            &context,
            Some(&NoopHook),
        );
        assert!(
            set.iter()
                .any(|d| d.is_error() && d.code.as_str() == "ir.validation.unsupported_version")
        );

        let receipt_ctx = sources.context(&context).expect("context");
        let checked =
            crate::receipt::check_artifact_set_with_profile(crate::receipt::ReceiptCheckRequest {
                artifacts: &sources.artifacts,
                context: &receipt_ctx,
                limits: &limits,
                artifact_set: sources.identity.clone(),
                identifier_profile: IDENTIFIER_ADMISSION_PROFILE,
                flow_source_map: None,
                retained: false,
                ui: Some(&NoopHook),
            })
            .expect("receipt");
        assert!(!checked.receipt.admitted);

        let explain_err = crate::explain::explain_inner(
            std::slice::from_ref(&env),
            &context,
            norn_ir::explain::ExplainKind::Concept,
            "anything",
            Some(&NoopHook),
        )
        .expect_err("Ring-0 explain error must survive NoopHook");
        assert!(
            explain_err
                .iter()
                .any(|d| d.is_error() && d.code.as_str() == "ir.validation.unsupported_version")
        );
    }

    #[test]
    fn ring1_warning_cannot_skip_ring0_typed_root_validation() {
        let mut malformed_widget: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/ir/retail-replenishment.widget.json"
        ))
        .expect("widget fixture");
        // Null `projectionHints` violates `WidgetRoot` typed-root decoding.
        malformed_widget["payload"]["projectionHints"] = serde_json::Value::Null;
        let env: norn_ir::ArtifactEnvelope =
            serde_json::from_value(malformed_widget).expect("widget envelope");

        let hook = WarningHook {
            code: "layout.validation.merge_conflict",
            pointer: "/payload",
            artifact_id: "retail.replenishment.widget",
        };
        let findings = crate::check_artifact_inner(&env, Some(&hook));
        assert!(
            findings.iter().any(|d| {
                d.is_error()
                    && d.code.as_str() == "widget.validation.invalid_payload"
                    && d.span
                        .as_ref()
                        .is_some_and(|s| s.pointer == "/payload/projectionHints")
            }),
            "Ring-0 typed-root error must still fire even when Ring-1 emits a warning: {findings:#?}"
        );
    }

    #[test]
    fn dedupe_findings_retains_higher_severity_on_key_collision() {
        let warning = Diagnostic {
            witness: None,
            code: DiagnosticCode::new("ir.validation.unsupported_version"),
            severity: DiagnosticSeverity::Warning,
            message: "advisory warning".to_owned(),
            span: Some(Span {
                pointer: "/irVersion".to_owned(),
                source: None,
            }),
            artifact_id: Some("a.widget".to_owned()),
            remediation: None,
        };
        let error = Diagnostic {
            severity: DiagnosticSeverity::Error,
            message: "unsupported IR version".to_owned(),
            ..warning.clone()
        };
        let deduped = crate::entity_artifacts::dedupe_findings([warning, error]);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].severity, DiagnosticSeverity::Error);
    }

    #[test]
    fn ring1_warning_cannot_displace_ring0_error_in_set_check() {
        let mut bad_widget: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/ir/retail-replenishment.widget.json"
        ))
        .expect("widget fixture");
        bad_widget["dependencies"] = json!([]);
        bad_widget["payload"]["projectionHints"] = serde_json::Value::Null;
        let artifact_id = bad_widget["artifactId"]
            .as_str()
            .expect("artifactId")
            .to_owned();
        let raw = norn_ir::RuntimeValue::from_json_text(
            serde_json::to_string(&bad_widget).expect("serialize"),
        )
        .expect("raw");
        let sources = ReceiptSources::new(&[raw], &[]).expect("sources");
        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        let limits = norn_ir::ResourceLimitProfile::norn_limits_v1();

        let hook = CollidingWarningHook(artifact_id);
        let receipt_ctx = sources.context(&context).expect("context");
        let checked =
            crate::receipt::check_artifact_set_with_profile(crate::receipt::ReceiptCheckRequest {
                artifacts: &sources.artifacts,
                context: &receipt_ctx,
                limits: &limits,
                artifact_set: sources.identity.clone(),
                identifier_profile: IDENTIFIER_ADMISSION_PROFILE,
                flow_source_map: None,
                retained: false,
                ui: Some(&hook),
            })
            .expect("receipt");
        assert!(
            !checked.receipt.admitted,
            "Ring-1 warning must never displace Ring-0 error to mint admitted=true"
        );
        assert!(
            checked.diagnostics.iter().any(|d| {
                d.is_error()
                    && d.code.as_str() == "widget.validation.invalid_payload"
                    && d.span
                        .as_ref()
                        .is_some_and(|s| s.pointer == "/payload/projectionHints")
            }),
            "Ring-0 Error must remain present: {:#?}",
            checked.diagnostics
        );
    }
}
