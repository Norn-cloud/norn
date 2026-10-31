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

// TODO(rust-safety): these downgrades were moved with the code from norn-ir
// (docs/platform/rust-safety-baseline.md) and keep the same staged burn-down.
pub mod action_set;
pub mod artifact_set;
pub mod codes;
pub mod connector_path;
pub mod consistency;
pub mod definitions_diff;
pub mod deontic;
mod diagnostic;
pub mod entity_artifacts;
pub mod entity_instance;
pub mod entity_lowering;
pub mod entity_model;
#[allow(
    missing_docs,
    clippy::pedantic,
    reason = "TODO(rust-safety): moved verbatim from norn-ir with its staged lint downgrades"
)]
#[deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]
pub mod envelope;
pub mod flow_source_map;
pub mod function;
mod graph;
pub mod interfaces;
pub mod jdm_profile;
pub mod layout;
pub mod lifecycle_writes;
pub mod lifecycles;
pub mod lint;
pub mod mapping;
mod markings;
pub mod metric;
pub mod metric_interchange;
#[allow(
    missing_docs,
    clippy::pedantic,
    reason = "TODO(rust-safety): moved verbatim from norn-ir with its staged lint downgrades"
)]
#[deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]
pub mod posting;
pub mod table_analysis;
pub use metric::check_metrics_and_questions;
pub mod naming_series;
pub mod object_types;
pub mod obligations;
#[allow(
    missing_docs,
    clippy::pedantic,
    reason = "TODO(rust-safety): moved verbatim from norn-ir with its staged lint downgrades"
)]
#[deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]
pub mod ontology;
pub mod property_codec;
pub mod property_types;
#[allow(
    missing_docs,
    clippy::pedantic,
    reason = "TODO(rust-safety): moved verbatim from norn-ir with its staged lint downgrades"
)]
#[deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]
pub mod quantity_ledger;
pub mod receipt;
pub mod rule_bindings;
#[allow(
    missing_docs,
    clippy::pedantic,
    reason = "TODO(rust-safety): moved verbatim from norn-ir with its staged lint downgrades"
)]
#[deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]
pub mod sealing;
mod source_commands;
pub mod source_schema;
pub mod typed_roots;
pub mod ui_interaction;
pub mod verification;
pub mod widget_ux;

pub use action_set::{check_action_firing, check_action_set};
pub use artifact_set::{
    ResolvedArtifactSet, check_artifact_set, may_reference, resolve_artifact_set, scope_conflict,
};
pub use codes::{CodeEntry, Tier, conformance_manifest};
pub use definitions_diff::{DefinitionDiff, DefinitionId, Narrowing, diff_definition_sets};
pub use diagnostic::{
    DIAGNOSTICS_FORMAT, Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity,
    SourceSpan, Span, diagnostics_to_json,
};
pub use entity_artifacts::{
    EntityArtifactSetContext, TenantLayer, check_artifact_set_entity_model, check_entity_artifacts,
    check_property_type_catalog_local,
};
pub use entity_instance::{
    EffectiveCatalog, EffectiveKey, EffectiveKind, EffectiveLifecycle, EffectiveLink,
    EffectiveObjectType, EffectiveProperty, InstanceError, KeyComponent, Unsupported,
    normalize_text,
};
pub use entity_lowering::{
    EntityForm, EntityObjectType, LoweredRelation, entity_object_type, entity_v1_deprecation,
    lower_cardinality_v1, lower_entity_v1, lower_relation_v1, relation_link,
};
pub use entity_model::{
    EntityContext, EntityModel, check_entity_model, check_interface_local,
    check_object_type_extension_local, check_object_type_local, effective_object_types,
};
pub use envelope::{ArtifactValidation, plan_graph_load, validate_artifact};
pub use flow_source_map::validate_flow_source_map;
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
    IDENTIFIER_ADMISSION_PROFILE, ReceiptError, ReceiptVersion, admission_receipt_sha256,
    check_artifact_set_for_install, check_artifact_set_for_install_with_flow_source_map,
    check_artifact_set_with_flow_source_map, check_artifact_set_with_receipt,
    check_artifact_set_with_verified_receipt, derive_type_env, recover_legacy_admission_receipt,
    verify_admission_receipt,
};
pub use widget_ux::{GeneratedTransitionEnabledWhenError, generated_transition_enabled_when};

pub use sealing::{
    seal, validate_customization_overlay, validate_runtime_capability_backstop,
    validate_seal_upgrade,
};
pub use typed_roots::decode_artifact_json;

/// Check one artifact envelope and return every finding in the checker
/// [`Diagnostic`] model (empty when the artifact is valid).
///
/// Same checks, codes, and messages as [`validate_artifact`]; only the
/// location is re-expressed as a JSON-pointer [`Span`], resolved against the
/// envelope's wire JSON (see [`Span::from_legacy_path_in`]).
#[must_use]
pub fn check_artifact(artifact: &norn_ir::ArtifactEnvelope) -> Vec<Diagnostic> {
    let (normalized, findings) = match crate::layout::normalize_artifact(artifact) {
        Ok((normalized, _layout)) => (normalized, Vec::new()),
        Err(findings) => (artifact.clone(), findings),
    };
    let legacy = crate::envelope::validate_artifact_unchecked(&normalized)
        .err()
        .unwrap_or_default();
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
#[must_use]
pub fn check_artifact_json(value: &serde_json::Value) -> Vec<Diagnostic> {
    match decode_artifact_json(value) {
        Ok(artifact) => check_artifact(&artifact),
        Err(legacy) => legacy
            .into_iter()
            .map(|diagnostic| Diagnostic::from_legacy_in(diagnostic, value))
            .collect(),
    }
}
