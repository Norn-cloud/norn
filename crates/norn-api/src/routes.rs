//! Authorized definition reads shared by transport adapters.

use http::StatusCode;
use norn_cell::{InstalledDefinitionSet, engine::SqlEngine};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{host::DevHost, refusal::Refusal, scope::CellScope};
use norn_ir::{ResourceLimitProfile, RuntimeValue};

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
    /// Not retained; null rather than a new check at read time.
    pub layout_diagnostics: Option<serde_json::Value>,
    /// Original checker receipt, without host reinterpretation.
    pub receipt: Box<RawValue>,
    /// Original receipt digest.
    pub receipt_sha256: String,
    /// Finding text is not retained; the original receipt carries its digest.
    pub diagnostics: Option<serde_json::Value>,
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
    let epoch = host.definition_read();
    let authorized = host.authorizer.authorize(&crate::authz::AccessRequest {
        scope: host.scope(),
        actor,
        operation: crate::authz::Operation::ReadDefinitions,
    });
    let scoped = host
        .scope()
        .admit(query.org_id.as_deref(), query.environment_id.as_deref());
    if authorized.is_err() || scoped.is_err() {
        return Err(Refusal::new(
            StatusCode::FORBIDDEN,
            crate::refusal::codes::AUTHZ_DENIED,
            "the read request is not authorized",
            "access to the requested read is denied",
        ));
    }
    let _epoch = epoch?;
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

fn defs_response(set: InstalledDefinitionSet, scope: &CellScope) -> Result<DefsResponse, Refusal> {
    verify_retained(&set, scope)?;
    let receipt = set
        .admission_receipt
        .ok_or_else(|| Refusal::host("missing receipt"))?;
    Ok(DefsResponse {
        admitted: true,
        version: set.version,
        layout_revision: set.layout_revision,
        layout: set
            .layout
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(Refusal::host)?
            .unwrap_or_default(),
        layout_diagnostics: None,
        receipt: RawValue::from_string(receipt).map_err(Refusal::host)?,
        receipt_sha256: set.receipt_sha256,
        diagnostics: None,
        registry: set.registry,
        base_artifact_ids: Vec::new(),
        artifacts: serde_json::from_str(&set.canonical_set).map_err(Refusal::host)?,
    })
}

fn verify_retained(installed: &InstalledDefinitionSet, scope: &CellScope) -> Result<(), Refusal> {
    let receipt = installed
        .admission_receipt
        .as_deref()
        .ok_or_else(|| Refusal::host("missing receipt"))?;
    let digest = norn_checker::admission_receipt_sha256(receipt).map_err(Refusal::host)?;
    if digest != installed.receipt_sha256 {
        return Err(Refusal::host("retained receipt digest mismatch"));
    }
    let retained: serde_json::Value = serde_json::from_str(receipt).map_err(Refusal::host)?;
    let limits = ResourceLimitProfile::norn_limits_v1();
    let profile =
        serde_json::to_value(limits.reference().map_err(Refusal::host)?).map_err(Refusal::host)?;
    // Release pins never grant type authority; verify self-contained sources only.
    if retained
        .pointer("/artifactSet/base")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|base| !base.is_empty())
        || retained.get("limitProfile") != Some(&profile)
    {
        return Err(context_unavailable());
    }
    let RuntimeValue::Array(sources) =
        RuntimeValue::from_json_text(&installed.canonical_set).map_err(Refusal::host)?
    else {
        return Err(Refusal::host(
            "retained definitions must be an artifact array",
        ));
    };
    for source in &sources {
        let artifact: norn_ir::ArtifactEnvelope =
            norn_ir::runtime::decode(source).map_err(Refusal::host)?;
        if norn_checker::scope_conflict(&scope.as_scope_ref(), &artifact.scope).is_some() {
            return Err(Refusal::host("retained artifact outside cell scope"));
        }
    }
    norn_checker::receipt::verify_admission_receipt_sources(
        receipt,
        &installed.receipt_sha256,
        &sources,
        &[],
        &limits,
    )
    .map(drop)
    .map_err(|error| {
        if matches!(
            &error,
            norn_checker::ReceiptError::UnsupportedReceiptVersion { .. }
        ) {
            context_unavailable()
        } else {
            Refusal::host(error)
        }
    })
}
fn context_unavailable() -> Refusal {
    Refusal::new(
        http::StatusCode::CONFLICT,
        "defs.receipt_context_unavailable",
        "the retained admission context is unavailable",
        "this host cannot resolve the receipt's external base, limit profile or checker version",
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::unwrap_used, reason = "authorized read correspondence")]
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

    const SOURCE: &str = include_str!("../../../fixtures/ir/retail-reorder-routing.rule.json");
    fn host() -> DevHost<NativeEngine> {
        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        let context = EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        };
        let sources = [RuntimeValue::from_json_text(SOURCE).unwrap()];
        let checked = check_artifact_set_with_receipt(
            &ReceiptSources::new(&sources, &[]).unwrap(),
            &context,
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .unwrap();
        assert!(checked.receipt.admitted);
        let input = InstalledDefinitionInput {
            canonical_set: format!("[{SOURCE}]"),
            registry: vec!["synthetic.library@1.0.0".into()],
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
    fn installed(host: &DevHost<NativeEngine>) -> InstalledDefinitionSet {
        host.cell()
            .unwrap()
            .store()
            .read()
            .installed_definitions(None)
            .unwrap()
            .unwrap()
    }
    fn query() -> DefinitionsQuery {
        serde_json::from_str(r#"{"orgId":"org_demo","environmentId":"dev"}"#).unwrap()
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
        let refused = get_defs(&host, &bad, actor).unwrap_err();
        assert_eq!(refused.code, "authz.denied");
        bad.org_id = None;
        let refused = get_defs(&host, &bad, actor).unwrap_err();
        assert_eq!(refused.code, "authz.denied");
    }
    #[test]
    fn denied_read_refusals_do_not_disclose_the_physical_scope() {
        for host in [host(), host().with_authorizer(Fixture)] {
            for actor in [None, Some("clerk@demo"), Some("reviewer@demo")] {
                let mut matching = query();
                matching.version = Some(999);
                let baseline = get_defs(&host, &matching, actor).unwrap_err();
                let expected = serde_json::to_vec(&baseline).unwrap();
                for org in [None, Some("foreign"), Some("org_demo")] {
                    for environment in [None, Some("foreign"), Some("dev")] {
                        let request = DefinitionsQuery {
                            org_id: org.map(str::to_owned),
                            environment_id: environment.map(str::to_owned),
                            version: Some(999),
                        };
                        let refusal = get_defs(&host, &request, actor).unwrap_err();
                        if baseline.status == 403 {
                            assert_eq!(refusal.status, baseline.status);
                            assert_eq!(serde_json::to_vec(&refusal).unwrap(), expected);
                        }
                        let wire = serde_json::to_string(&refusal).unwrap();
                        assert!(!wire.contains(&host.scope().org_id), "{wire}");
                        assert!(!wire.contains(&host.scope().environment_id), "{wire}");
                    }
                }
            }
        }
    }
    #[test]
    fn authorization_runs_for_matching_foreign_and_missing_scopes() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Counting(Arc<AtomicUsize>);
        impl Authorizer for Counting {
            fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
                self.0.fetch_add(1, Ordering::Relaxed);
                crate::authz::DenyAll.authorize(request)
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let host = host().with_authorizer(Counting(Arc::clone(&calls)));
        for org in [Some("org_demo"), Some("foreign"), None] {
            let mut request = query();
            request.org_id = org.map(str::to_owned);
            assert_eq!(
                get_defs(&host, &request, None).unwrap_err().code,
                "authz.denied"
            );
        }
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }
    #[test]
    fn admitted_document_and_missing_version_preserve_cell_state() {
        let actor = Some("reviewer@demo");
        let host = host().with_authorizer(Fixture);
        let before = host.cell().unwrap().store().verified_head().unwrap();
        let result = get_defs(&host, &query(), actor).unwrap();
        assert!(result.admitted && result.artifacts.len() == 1);
        assert_eq!(result.artifacts.first().unwrap().get(), SOURCE.trim());
        assert_eq!(result.registry, ["synthetic.library@1.0.0"]);
        assert!(result.diagnostics.is_none() && result.layout_diagnostics.is_none());
        let installed = installed(&host);
        assert_eq!(
            result.receipt.get(),
            installed.admission_receipt.as_deref().unwrap()
        );
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
    #[test]
    fn retained_receipt_checks_sources_and_context_without_readmission() {
        let host = host();
        let installed = installed(&host);
        let verify = |set: &InstalledDefinitionSet| verify_retained(set, host.scope());
        let rewrite = |set: &mut InstalledDefinitionSet, key: &str, value: serde_json::Value| {
            let mut receipt =
                norn_ir::RuntimeValue::from_json_text(set.admission_receipt.as_deref().unwrap())
                    .unwrap();
            let value = RuntimeValue::from_json_text(value.to_string()).unwrap();
            receipt.as_object_mut().unwrap().insert(key.into(), value);
            set.admission_receipt =
                Some(String::from_utf8(receipt.canonical_json().unwrap()).unwrap());
            set.receipt_sha256 =
                norn_checker::admission_receipt_sha256(set.admission_receipt.as_deref().unwrap())
                    .unwrap();
        };
        assert!(verify(&installed).is_ok());
        let mut bad = installed.clone();
        bad.receipt_sha256 = "0".repeat(64);
        assert_eq!(verify(&bad).unwrap_err().status, 500);
        let wrong_set = serde_json::json!({"artifacts":[],"base":[],"sha256":"0".repeat(64)});
        let wrong_env =
            serde_json::json!({"id":"norn.expr.env","version":1,"sha256":"0".repeat(64)});
        let external_base = serde_json::json!({"artifacts":[],"base":[{}],"sha256":"0".repeat(64)});
        let future_limits =
            serde_json::json!({"id":"future.limits","version":99,"sha256":"0".repeat(64)});
        let future_checker = serde_json::json!({"name":"norn-checker","version":"999.0.0"});
        let unavailable = "defs.receipt_context_unavailable";
        for (key, value, expected) in [
            ("admitted", serde_json::json!(false), "host.error"),
            ("artifactSet", wrong_set, "host.error"),
            ("typeEnv", wrong_env, "host.error"),
            ("artifactSet", external_base, unavailable),
            ("limitProfile", future_limits, unavailable),
            ("checker", future_checker, unavailable),
        ] {
            let mut bad = installed.clone();
            rewrite(&mut bad, key, value);
            assert_eq!(verify(&bad).unwrap_err().code, expected, "{key}");
        }
        let mut bad = installed;
        bad.canonical_set = "[]".into();
        assert_eq!(verify(&bad).unwrap_err().status, 500);
    }
}
