//! `norn-ui-checker`: Ring-1 UI and Widget UX admission diagnostics (#1613).
//!
//! Composes Ring-0 [`norn_checker`] semantic admission with Ring-1 UI/widget
//! validation: widget artifact envelopes ([`envelope`]), typed Widget UX
//! contracts ([`widget_ux`]), interaction rules ([`ui_interaction`]), page
//! statecharts ([`ui_machine`]), semantic views ([`ui_view`]), catalog
//! components ([`ui_catalog`]), and page-to-lifecycle composition
//! ([`ui_lifecycle`]).

// Deterministic core policy (#3299).
#![deny(clippy::disallowed_types)]

pub mod envelope;
pub mod ui_catalog;
pub mod ui_interaction;
pub mod ui_lifecycle;
pub mod ui_machine;
pub mod ui_view;
pub mod widget_ux;

use norn_checker::{
    CheckedArtifactSet, Diagnostic, EntityArtifactSetContext, ReceiptError, ReceiptSources,
};
use norn_ir::{ArtifactEnvelope, FlowSourceMap, ResourceLimitProfile};

pub use widget_ux::{
    GeneratedTransitionEnabledWhenError, check_widget_ux_set, generated_transition_enabled_when,
};

/// Validate one artifact envelope with both Ring-0 checks and Ring-1 widget
/// payload/dependency validation.
///
/// # Errors
///
/// Returns every legacy diagnostic found when the envelope is invalid.
pub fn validate_artifact(artifact: &ArtifactEnvelope) -> Result<(), Vec<norn_ir::Diagnostic>> {
    norn_checker::validate_artifact_with(artifact, envelope::validate_widget_extra)
}

/// Check one artifact envelope with both Ring-0 checks and Ring-1 widget
/// payload/dependency validation.
#[must_use]
pub fn check_artifact(artifact: &ArtifactEnvelope) -> Vec<Diagnostic> {
    norn_checker::check_artifact_with(artifact, envelope::validate_widget_extra)
}

/// Decode an artifact envelope from wire JSON and check it with both Ring-0
/// checks and Ring-1 widget payload/dependency validation.
#[must_use]
pub fn check_artifact_json(value: &serde_json::Value) -> Vec<Diagnostic> {
    norn_checker::check_artifact_json_with(value, envelope::validate_widget_extra)
}

/// Resolve `artifacts` against `context.base` and run both Ring-0 set checks
/// and Ring-1 Widget UX / UI law checks with identical deduplication and
/// diagnostic parity.
#[must_use]
pub fn check_artifact_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    norn_checker::artifact_set::check_artifact_set_with(
        artifacts,
        context,
        &mut check_artifact,
        &mut check_widget_ux_set,
    )
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
    norn_checker::check_artifact_set_with_receipt_using(
        artifacts,
        context,
        limits,
        check_artifact_set,
    )
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
    norn_checker::check_artifact_set_with_flow_source_map_using(
        artifacts,
        context,
        limits,
        flow_source_map,
        check_artifact_set,
    )
}
