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
        .store()
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

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(
    clippy::unwrap_used,
    missing_docs,
    reason = "authorized read correspondence"
)]
mod tests {
    use super::*;
    use crate::{
        authz::{AccessRequest, Authorizer},
        scope::CellScope,
    };
    use norn_cell::{CellStore, InstalledDefinitionInput, Model, engine::NativeEngine};
    use norn_checker::{EntityArtifactSetContext, ReceiptSources, check_artifact_set_with_receipt};
    use norn_ir::ResourceLimitProfile;
    use std::collections::BTreeSet;

    fn host() -> DevHost<NativeEngine> {
        let scope = CellScope {
            org_id: "demo".into(),
            environment_id: "dev".into(),
        };
        let registry = BTreeSet::new();
        let sources = ReceiptSources::new(&[], &[]).unwrap();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        let checked = check_artifact_set_with_receipt(
            &sources,
            &context,
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .unwrap();
        let input = InstalledDefinitionInput {
            canonical_set: "[]".into(),
            registry: Vec::new(),
            receipt_sha256: checked.receipt.content_sha256().unwrap(),
            admission_receipt: Some(
                String::from_utf8(checked.receipt.canonical_json().unwrap()).unwrap(),
            ),
            readmission: None,
            diff: serde_json::json!({}),
            installed_at: 1,
            layout: None,
        };
        let store = CellStore::provision_definitions(
            NativeEngine::open_in_memory().unwrap(),
            Model::from_definitions(&[], &[]).unwrap(),
            &input,
            &scope.key(),
        )
        .unwrap();
        DevHost::from_store(store, scope, |command| command.payload.clone()).unwrap()
    }
    fn query() -> DefinitionsQuery {
        serde_json::from_str(r#"{"orgId":"demo","environmentId":"dev"}"#).unwrap()
    }
    struct Fixture;
    impl Authorizer for Fixture {
        fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
            if request.actor == Some("reviewer@demo") {
                Ok(())
            } else {
                crate::authz::DenyAll.authorize(request)
            }
        }
    }
    #[test]
    fn default_deny_and_explicit_scope_precede_disclosure() {
        let actor = Some("reviewer@demo");
        let host = host();
        assert_eq!(get_defs(&host, &query(), actor).unwrap_err().status, 403);
        let host = host.with_authorizer(Fixture);
        assert_eq!(get_defs(&host, &query(), None).unwrap_err().status, 403);
        let mut bad = query();
        bad.org_id = Some("foreign".into());
        assert_eq!(
            get_defs(&host, &bad, actor).unwrap_err().code,
            "scope.denied"
        );
        bad.org_id = None;
        assert_eq!(
            get_defs(&host, &bad, actor).unwrap_err().code,
            "scope.missing"
        );
    }
    #[test]
    fn admitted_document_and_missing_version_preserve_cell_state() {
        let actor = Some("reviewer@demo");
        let host = host().with_authorizer(Fixture);
        let before = host.cell().unwrap().store().verified_head().unwrap();
        let result = get_defs(&host, &query(), actor).unwrap();
        assert!(result.admitted && result.artifacts.is_empty());
        assert_eq!(
            norn_checker::admission_receipt_sha256(result.receipt.get()).unwrap(),
            result.receipt_sha256
        );
        let mut absent = query();
        absent.version = Some(999);
        assert_eq!(get_defs(&host, &absent, actor).unwrap_err().status, 404);
        assert_eq!(
            before,
            host.cell().unwrap().store().verified_head().unwrap()
        );
    }
}
