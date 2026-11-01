//! Admitted definition data retained by the API host.

use norn_ir::{ArtifactEnvelope, LayoutSidecar};
use serde_json::Value;

/// The definition projection of an admitted host set.
///
/// Host admission supplies these fields. This data container and its response
/// projection do not authorize requests or install definitions.
#[derive(Debug, Clone)]
pub struct Admitted {
    /// Decoded canonical envelopes in load order.
    pub artifacts: Vec<ArtifactEnvelope>,
    /// Exact canonical source array, including decimal literals.
    pub canonical_set: String,
    /// Exact registry releases forming the read-only base.
    pub registry: Vec<String>,
    /// The checker's canonical receipt, unchanged.
    pub receipt: Value,
    /// The checker's receipt hash, unchanged.
    pub receipt_sha256: String,
    /// Non-error checker findings.
    pub diagnostics: Value,
    /// Editor-only layout, excluded from the semantic receipt.
    pub layout: LayoutSidecar,
    /// Layout warnings, separate from semantic findings.
    pub layout_diagnostics: Vec<norn_checker::Diagnostic>,
}
