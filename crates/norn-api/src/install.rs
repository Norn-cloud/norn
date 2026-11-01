//! Exact admitted-definition response projection; no request authorization.

use crate::{defs::Admitted, refusal::Refusal};
use norn_ir::LayoutSidecar;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

/// The admitted definitions and their check receipt.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefsResponse {
    /// Always `true`: a cell serves only an admitted set.
    pub admitted: bool,
    /// Monotonic installed set version.
    pub version: u64,
    /// Layout revision; advances only when the retained sidecar changes,
    /// including when a semantic install prunes orphaned editor entries.
    pub layout_revision: u64,
    /// Editor-only layout snapshot. Geometry is excluded from `receipt`.
    #[schema(schema_with = crate::contracts::layout_sidecar_schema)]
    pub layout: LayoutSidecar,
    /// Layout merge and orphan-pruning warnings, separate from semantic
    /// checker diagnostics and receipt contents.
    #[schema(schema_with = crate::contracts::checker_diagnostics_schema)]
    pub layout_diagnostics: Value,
    /// The checker's Norn1 canonical `CheckReceipt`.
    #[schema(schema_with = crate::contracts::check_receipt_schema)]
    pub receipt: Value,
    /// The receipt's content hash.
    pub receipt_sha256: String,
    /// Non-error findings, `norn-checker/diagnostics@1`.
    #[schema(schema_with = crate::contracts::checker_diagnostics_schema)]
    pub diagnostics: Value,
    /// Exact registry releases forming the read-only base.
    pub registry: Vec<String>,
    /// Artifact ids that belong to the read-only registry base. Filter these
    /// ids from `artifacts` before submitting the candidate layer to PUT.
    pub base_artifact_ids: Vec<String>,
    /// The admitted canonical JSON IR artifacts, in load order.
    #[schema(schema_with = crate::contracts::artifact_envelopes_schema)]
    pub artifacts: Vec<Box<serde_json::value::RawValue>>,
    /// Optional non-executable authoring projection bound to this receipt.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(schema_with = crate::contracts::flow_source_map_schema)]
    pub flow_source_map: Option<norn_ir::FlowSourceMap>,
}

/// Project the host's admitted set without re-encoding its semantic literals.
///
/// # Errors
/// Refuses inconsistent receipt metadata or malformed retained source text.
pub fn defs_response(
    admitted: &Admitted,
    version: u64,
    layout_revision: u64,
) -> Result<DefsResponse, Refusal> {
    let flow_source_map = admitted
        .receipt
        .get("flowSourceMap")
        .cloned()
        .map(|value| {
            serde_json::from_value(value).map_err(|error| {
                Refusal::host(format!(
                    "the admission receipt has an invalid flow source map: {error}"
                ))
            })
        })
        .transpose()?;
    let base_artifact_ids = admitted
        .receipt
        .pointer("/artifactSet/base")
        .and_then(Value::as_array)
        .ok_or_else(|| Refusal::host("the admission receipt has no artifactSet.base array"))?
        .iter()
        .map(|artifact| {
            artifact
                .get("artifactId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| Refusal::host("a base artifact receipt has no artifactId"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DefsResponse {
        admitted: true,
        version,
        layout_revision,
        layout: admitted.layout.clone(),
        layout_diagnostics: norn_checker::diagnostics_to_json(&admitted.layout_diagnostics),
        receipt: admitted.receipt.clone(),
        receipt_sha256: admitted.receipt_sha256.clone(),
        diagnostics: admitted.diagnostics.clone(),
        registry: admitted.registry.clone(),
        base_artifact_ids,
        artifacts: serde_json::from_str(&admitted.canonical_set).map_err(Refusal::host)?,
        flow_source_map,
    })
}
