//! Authorized definition reads shared by transport adapters.

use http::StatusCode;
use norn_cell::{InstalledDefinitionSet, engine::SqlEngine};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{host::DevHost, refusal::Refusal};

/// Explicit scope and optional retained definition version.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionsQuery {
    /// Required organization.
    pub org_id: Option<String>,
    /// Required environment.
    pub environment_id: Option<String>,
    /// Absent selects the active revision.
    pub version: Option<u64>,
}

/// An admitted document; source and receipt travel as exact JSON text.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefsResponse {
    /// Always true.
    pub admitted: bool,
    /// Retained definition revision.
    pub version: u64,
    /// Editor sidecar revision.
    pub layout_revision: u64,
    /// Editor sidecar.
    pub layout: norn_ir::LayoutSidecar,
    /// Sidecar findings, separate from semantic admission.
    pub layout_diagnostics: serde_json::Value,
    /// Original checker receipt, without host reinterpretation.
    pub receipt: Box<RawValue>,
    /// Original receipt digest.
    pub receipt_sha256: String,
    /// Non-error checker findings.
    pub diagnostics: serde_json::Value,
    /// Selected registry releases.
    pub registry: Vec<String>,
    /// Read-only base artifact identities.
    pub base_artifact_ids: Vec<String>,
    /// Admitted semantic documents, preserving exact numeric values.
    pub artifacts: Vec<Box<RawValue>>,
}

/// Read under one definition epoch; scope and authorization precede store access.
///
/// # Errors
/// Missing/mismatched scope, denied actor, absent version or host failure.
pub fn get_defs<E: SqlEngine>(
    host: &DevHost<E>,
    query: &DefinitionsQuery,
    actor: Option<&str>,
) -> Result<DefsResponse, Refusal> {
    let _epoch = host.definition_read()?;
    host.scope()
        .admit(query.org_id.as_deref(), query.environment_id.as_deref())?;
    host.authorizer.authorize(&crate::authz::AccessRequest {
        scope: host.scope(),
        actor,
        operation: crate::authz::Operation::ReadDefinitions,
    })?;
    let installed = host
        .cell()?
        .read()
        .installed_definitions(query.version)
        .map_err(Refusal::host)?
        .ok_or_else(|| {
            Refusal::new(
                StatusCode::NOT_FOUND,
                "defs.version_not_found",
                "no such definitions version",
                "the requested version has never been installed",
            )
        })?;
    defs_response(installed, host.scope())
}

fn defs_response(
    installed: InstalledDefinitionSet,
    scope: &crate::scope::CellScope,
) -> Result<DefsResponse, Refusal> {
    let receipt = installed
        .admission_receipt
        .ok_or_else(|| Refusal::host("missing receipt"))?;
    let admitted = crate::defs::Admitted::from_source(&installed.canonical_set, scope)
        .map_err(|error| Refusal::host(error.code))?;
    let digest = norn_checker::admission_receipt_sha256(&receipt).map_err(Refusal::host)?;
    if digest != installed.receipt_sha256 || digest != admitted.receipt_sha256 {
        return Err(Refusal::host(
            "retained receipt does not bind these definitions",
        ));
    }
    Ok(DefsResponse {
        admitted: true,
        version: installed.version,
        layout_revision: installed.layout_revision,
        layout: installed
            .layout
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(Refusal::host)?
            .unwrap_or_default(),
        layout_diagnostics: norn_checker::diagnostics_to_json(&[]),
        receipt: RawValue::from_string(receipt).map_err(Refusal::host)?,
        receipt_sha256: installed.receipt_sha256,
        diagnostics: admitted.diagnostics,
        registry: installed.registry,
        base_artifact_ids: Vec::new(),
        artifacts: serde_json::from_str(&installed.canonical_set).map_err(Refusal::host)?,
    })
}

#[cfg(feature = "http-host")]
/// HTTP adapter over the same authorized read used by other transports.
pub fn router<E: SqlEngine + Send + 'static>(host: std::sync::Arc<DevHost<E>>) -> axum::Router {
    use axum::{
        extract::{Query, State},
        http::HeaderMap,
        response::IntoResponse,
    };
    async fn read<E: SqlEngine + Send + 'static>(
        State(host): State<std::sync::Arc<DevHost<E>>>,
        query: Result<Query<DefinitionsQuery>, axum::extract::rejection::QueryRejection>,
        headers: HeaderMap,
    ) -> axum::response::Response {
        let actor = headers
            .get("x-norn-actor")
            .and_then(|value| value.to_str().ok());
        let result = query
            .map_err(|error| Refusal::invalid(error.to_string()))
            .and_then(|Query(query)| get_defs(&host, &query, actor));
        match result {
            Ok(document) => axum::Json(document).into_response(),
            Err(refusal) => (
                StatusCode::from_u16(refusal.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                axum::Json(refusal),
            )
                .into_response(),
        }
    }
    axum::Router::new()
        .route("/v1/defs", axum::routing::get(read::<E>))
        .with_state(host)
}
