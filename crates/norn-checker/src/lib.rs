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
    ResolvedArtifactSet, check_artifact_set, check_artifact_set_with_ui, may_reference,
    resolve_artifact_set, scope_conflict,
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
pub use envelope::{
    ArtifactValidation, plan_graph_load, validate_artifact, validate_artifact_with_ui,
};
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

/// Strictly additive Ring-1 UI validation hooks composed over Ring-0 checks.
///
/// Ring-0 always executes its own envelope, typed-root, entity-model, action,
/// rule, lifecycle, mapping, law, and receipt checks unconditionally. Hooks may
/// inspect inputs and return additional diagnostics to append; they can never
/// mutate, clear, or suppress Ring-0 findings.
pub trait UiValidator {
    /// Validate an `ArtifactPayload::Widget` envelope payload before typed-root
    /// agreement. Returned diagnostics are appended to Ring-0's envelope
    /// findings.
    fn validate_widget_payload(
        &self,
        artifact: &norn_ir::ArtifactEnvelope,
        payload: &norn_ir::JsonValue,
    ) -> Vec<norn_ir::Diagnostic>;

    /// Run Ring-1 cross-artifact UI/widget checks over normalized artifacts
    /// after Ring-0 resolution succeeds. Returned diagnostics are appended to
    /// Ring-0's set findings.
    fn check_ui_set(
        &self,
        artifacts: &[norn_ir::ArtifactEnvelope],
        context: &EntityArtifactSetContext<'_>,
    ) -> Vec<Diagnostic>;
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

/// Check one artifact envelope with strictly additive Ring-1 UI validation
/// composed over Ring-0 envelope and typed-root checks.
#[must_use]
pub fn check_artifact_with_ui(
    artifact: &norn_ir::ArtifactEnvelope,
    ui: &dyn UiValidator,
) -> Vec<Diagnostic> {
    check_artifact_inner(artifact, Some(ui))
}

#[must_use]
pub(crate) fn check_artifact_inner(
    artifact: &norn_ir::ArtifactEnvelope,
    ui: Option<&dyn UiValidator>,
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
/// `ArtifactPayload::Widget` fails closed here; use [`check_artifact_json_with_ui`]
/// via Ring-1 `norn-ui-checker` to validate widget envelopes.
#[must_use]
pub fn check_artifact_json(value: &serde_json::Value) -> Vec<Diagnostic> {
    check_artifact_json_inner(value, None)
}

/// Decode an artifact envelope from its wire JSON and check it with strictly
/// additive Ring-1 UI validation via [`check_artifact_with_ui`].
#[must_use]
pub fn check_artifact_json_with_ui(
    value: &serde_json::Value,
    ui: &dyn UiValidator,
) -> Vec<Diagnostic> {
    check_artifact_json_inner(value, Some(ui))
}

fn check_artifact_json_inner(
    value: &serde_json::Value,
    ui: Option<&dyn UiValidator>,
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

/// Check `artifacts` with Ring-0 checks plus strictly additive Ring-1 UI
/// validation and issue its [`CheckReceipt`]. Ring-0 always runs
/// unconditionally; `ui` can only append diagnostics.
///
/// # Errors
/// As [`check_artifact_set_with_receipt`].
pub fn check_artifact_set_with_receipt_ui(
    artifacts: &ReceiptSources,
    context: &EntityArtifactSetContext<'_>,
    limits: &norn_ir::ResourceLimitProfile,
    ui: &dyn UiValidator,
) -> Result<CheckedArtifactSet, ReceiptError> {
    let context = artifacts.context(context)?;
    receipt::check_artifact_set_with_profile(receipt::ReceiptCheckRequest {
        artifacts: &artifacts.artifacts,
        context: &context,
        limits,
        artifact_set: artifacts.identity.clone(),
        identifier_profile: IDENTIFIER_ADMISSION_PROFILE,
        flow_source_map: None,
        retained: false,
        ui: Some(ui),
    })
}

/// Check `artifacts` with an optional flow source map, running Ring-0 checks
/// plus strictly additive Ring-1 UI validation, and issue its [`CheckReceipt`].
///
/// # Errors
/// As [`check_artifact_set_with_flow_source_map`].
pub fn check_artifact_set_with_flow_source_map_ui(
    artifacts: &ReceiptSources,
    context: &EntityArtifactSetContext<'_>,
    limits: &norn_ir::ResourceLimitProfile,
    flow_source_map: Option<&norn_ir::FlowSourceMap>,
    ui: &dyn UiValidator,
) -> Result<CheckedArtifactSet, ReceiptError> {
    let context = artifacts.context(context)?;
    receipt::check_artifact_set_with_profile(receipt::ReceiptCheckRequest {
        artifacts: &artifacts.artifacts,
        context: &context,
        limits,
        artifact_set: artifacts.identity.clone(),
        identifier_profile: IDENTIFIER_ADMISSION_PROFILE,
        flow_source_map,
        retained: false,
        ui: Some(ui),
    })
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
