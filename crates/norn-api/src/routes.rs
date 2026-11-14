//! Authorized definition reads shared by transport adapters.

use http::StatusCode;
use norn_cell::{InstalledDefinitionSet, engine::SqlEngine};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use utoipa::{IntoParams, ToSchema};

use crate::{host::DevHost, refusal::Refusal, scope::CellScope};
use norn_ir::{ResourceLimitProfile, RuntimeValue};

/// Explicit scope and optional retained definition version.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct DefinitionsQuery {
    /// Required organization.
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Required environment.
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
    /// A retained revision; absent selects the active one.
    #[param(nullable = false)]
    pub version: Option<u64>,
}

/// An admitted document; source and receipt travel as exact JSON text.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = NornDefinitionsCatalog)]
pub struct DefsResponse {
    /// Always `true`.
    #[schema(schema_with = always_true_schema)]
    pub admitted: bool,
    /// Retained definition revision.
    pub version: u64,
    /// Editor sidecar revision.
    pub layout_revision: u64,
    /// Editor sidecar.
    #[schema(schema_with = layout_schema)]
    pub layout: norn_ir::LayoutSidecar,
    /// Always null.
    #[schema(required = true, schema_with = layout_diagnostics_schema)]
    pub layout_diagnostics: Option<serde_json::Value>,
    /// Original checker receipt, without host reinterpretation.
    #[schema(schema_with = receipt_schema)]
    pub receipt: Box<RawValue>,
    /// Original receipt digest.
    pub receipt_sha256: String,
    /// Always null.
    #[schema(required = true, schema_with = diagnostics_schema)]
    pub diagnostics: Option<serde_json::Value>,
    /// Selected registry releases.
    pub registry: Vec<String>,
    /// Read-only base artifact identities.
    pub base_artifact_ids: Vec<String>,
    /// Admitted semantic documents, preserving exact numeric values.
    #[schema(schema_with = artifacts_schema)]
    pub artifacts: Vec<Box<RawValue>>,
}

fn layout_schema() -> utoipa::openapi::Ref {
    utoipa::openapi::Ref::from_schema_name("LayoutSidecar")
}

fn always_true_schema() -> utoipa::openapi::schema::Object {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::SchemaType::Type(
            utoipa::openapi::Type::Boolean,
        ))
        .enum_values(Some([true]))
        .description(Some(
            "Always `true`: this route serves only an admitted revision. The member is kept \
             because readers of this route already rely on its shape.",
        ))
        .build()
}

fn always_null(description: &str) -> utoipa::openapi::schema::Object {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::SchemaType::Type(
            utoipa::openapi::Type::Null,
        ))
        .description(Some(description))
        .build()
}

fn layout_diagnostics_schema() -> utoipa::openapi::schema::Object {
    always_null(
        "Always null: layout diagnostics are not retained with a revision, and this read does \
         not re-run the checker. Kept for the shape readers already rely on; `null` is frozen, \
         so real diagnostics would arrive as a new member.",
    )
}

fn diagnostics_schema() -> utoipa::openapi::schema::Object {
    always_null(
        "Always null: finding text is not retained with a revision; `receipt` carries the \
         admission's digest. Kept for the shape readers already rely on; `null` is frozen, so \
         real diagnostics would arrive as a new member.",
    )
}

fn receipt_schema() -> utoipa::openapi::schema::Object {
    crate::objects::free_form(
        "The original checker receipt, without host reinterpretation.",
        false,
    )
}

fn artifacts_schema() -> utoipa::openapi::schema::Array {
    utoipa::openapi::schema::ArrayBuilder::new()
        .items(crate::objects::free_form(
            "One admitted artifact, preserving exact numeric values.",
            false,
        ))
        .description(Some(
            "Every admitted artifact of the revision, in one unpaged array: references between \
             artifacts resolve only against the whole set, so the set is not split. Its size is \
             the installed set's; this read applies no count bound of its own.",
        ))
        .build()
}

/// Read under one definition epoch; scope and authorization precede store access.
///
/// # Errors
/// Missing/mismatched scope, denied actor, absent version or host failure.
#[utoipa::path(get, path = "/v1/definitions", operation_id = "get_definitions_catalog", params(DefinitionsQuery), responses((status = 200, description = "The admitted definitions at the active or requested revision.", body = DefsResponse), (status = 400, description = "The request was refused as invalid (`request.invalid`): an unknown or repeated query parameter, a query over the host bound, or a version that does not parse.", body = Refusal), (status = 403, description = "The request was refused: a missing or foreign scope, or denied authority, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`defs.version_not_found`): the requested version was never installed, or no version was asked for and nothing is installed yet.", body = Refusal), (status = 409, description = "The request was refused (`defs.receipt_context_unavailable`): this host cannot resolve the retained admission context (the receipt's external base, limit profile or checker version).", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
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
    let base_artifact_ids = verify_retained(&set, scope)?;
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
        base_artifact_ids,
        artifacts: serde_json::from_str(&set.canonical_set).map_err(Refusal::host)?,
    })
}

/// Verify the retained set against its receipt. A receipt's base must be
/// retained in the same set: it is verified there, never resolved from a
/// release pin. Returns the base artifact ids.
pub(crate) fn verify_retained(
    installed: &InstalledDefinitionSet,
    scope: &CellScope,
) -> Result<Vec<String>, Refusal> {
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
    let base_refs = retained
        .pointer("/artifactSet/base")
        .and_then(serde_json::Value::as_array)
        .map_or_else(Vec::new, |base| {
            base.iter()
                .map(|entry| {
                    let text = |key| entry.get(key).and_then(serde_json::Value::as_str);
                    (text("artifactId"), text("version"))
                })
                .collect()
        });
    if retained.get("limitProfile") != Some(&profile) {
        return Err(context_unavailable());
    }
    let RuntimeValue::Array(sources) =
        RuntimeValue::from_json_text(&installed.canonical_set).map_err(Refusal::host)?
    else {
        return Err(Refusal::host(
            "retained definitions must be an artifact array",
        ));
    };
    let (mut candidates, mut base, mut base_ids) = (Vec::new(), Vec::new(), Vec::new());
    for source in sources {
        let artifact: norn_ir::ArtifactEnvelope =
            norn_ir::runtime::decode(&source).map_err(Refusal::host)?;
        if norn_checker::scope_conflict(&scope.as_scope_ref(), &artifact.scope).is_some() {
            return Err(Refusal::host("retained artifact outside cell scope"));
        }
        let identity = (
            Some(artifact.artifact_id.as_str()),
            Some(artifact.version.as_str()),
        );
        if base_refs.contains(&identity) {
            base_ids.push(artifact.artifact_id);
            base.push(source);
        } else {
            candidates.push(source);
        }
    }
    // An external base (absent from the retained set) cannot be verified here.
    if base.len() != base_refs.len() {
        return Err(context_unavailable());
    }
    norn_checker::receipt::verify_admission_receipt_sources(
        receipt,
        &installed.receipt_sha256,
        &candidates,
        &base,
        &limits,
    )
    .map(|_| base_ids)
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
                let expected = serde_json::to_string(&baseline).unwrap();
                for org in [None, Some("foreign"), Some("org_demo")] {
                    for environment in [None, Some("foreign"), Some("dev")] {
                        let request = DefinitionsQuery {
                            org_id: org.map(str::to_owned),
                            environment_id: environment.map(str::to_owned),
                            version: Some(999),
                        };
                        let refusal = get_defs(&host, &request, actor).unwrap_err();
                        let wire = serde_json::to_string(&refusal).unwrap();
                        if baseline.status == 403 {
                            assert_eq!(refusal.status, baseline.status);
                            assert_eq!(wire.as_bytes(), expected.as_bytes());
                        }
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
    /// The three-systems set admitted over its `core`/`std` base: the wire
    /// artifacts, the base ids and one retained set builder per artifact list.
    struct OverBase {
        artifacts: Vec<serde_json::Value>,
        base_ids: Vec<String>,
        receipt: String,
        digest: String,
    }
    impl OverBase {
        fn admit() -> Self {
            let artifacts: Vec<norn_ir::ArtifactEnvelope> = serde_json::from_str(include_str!(
                "../../../fixtures/demo/explorer/three-systems.definitions.json"
            ))
            .unwrap();
            let value = |a: &norn_ir::ArtifactEnvelope| {
                RuntimeValue::from_json(serde_json::to_value(a).unwrap()).unwrap()
            };
            let (base, candidates): (Vec<_>, Vec<_>) = artifacts.iter().cloned().partition(|a| {
                a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std.")
            });
            let checked = check_artifact_set_with_receipt(
                &ReceiptSources::new(
                    &candidates.iter().map(value).collect::<Vec<_>>(),
                    &base.iter().map(value).collect::<Vec<_>>(),
                )
                .unwrap(),
                &EntityArtifactSetContext {
                    registry: &BTreeSet::new(),
                    base: &base,
                    tenant: None,
                },
                &ResourceLimitProfile::norn_limits_v1(),
            )
            .unwrap();
            assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
            Self {
                artifacts: artifacts
                    .iter()
                    .map(|a| serde_json::to_value(a).unwrap())
                    .collect(),
                base_ids: base.iter().map(|a| a.artifact_id.clone()).collect(),
                receipt: String::from_utf8(checked.receipt.canonical_json().unwrap()).unwrap(),
                digest: checked.receipt.content_sha256().unwrap(),
            }
        }
        /// The same receipt and digest over `artifacts` as the retained set.
        fn retained(&self, artifacts: Vec<serde_json::Value>) -> InstalledDefinitionSet {
            let set = RuntimeValue::from_json(serde_json::Value::Array(artifacts)).unwrap();
            InstalledDefinitionSet {
                canonical_set: String::from_utf8(set.canonical_json().unwrap()).unwrap(),
                admission_receipt: Some(self.receipt.clone()),
                receipt_sha256: self.digest.clone(),
                ..installed(&host())
            }
        }
        fn position(&self, id: &str) -> usize {
            self.artifacts
                .iter()
                .position(|a| a["artifactId"] == id)
                .unwrap()
        }
        fn verify(&self, artifacts: Vec<serde_json::Value>) -> Result<Vec<String>, Refusal> {
            verify_retained(&self.retained(artifacts), host().scope())
        }
    }

    /// A receipt whose base is retained in the same set verifies here and
    /// names that base, in any retained order; the base is never resolved
    /// from outside the cell.
    #[test]
    fn retained_base_definitions_verify_and_are_listed() {
        let admitted = OverBase::admit();
        let mut expected = admitted.base_ids.clone();
        expected.sort();
        let mut listed = admitted.verify(admitted.artifacts.clone()).unwrap();
        listed.sort();
        assert_eq!(listed, expected);
        assert_ne!(listed.len(), 0);
        let mut reordered = admitted.artifacts.clone();
        reordered.reverse();
        let mut listed = admitted.verify(reordered).unwrap();
        listed.sort();
        assert_eq!(listed, expected);
    }

    /// Every substitution of a retained base artifact refuses against the
    /// original receipt and digest; an incomplete base is context-unavailable.
    #[test]
    fn substituted_replayed_or_missing_base_artifacts_refuse() {
        let admitted = OverBase::admit();
        let (a, b) = (
            admitted.position(&admitted.base_ids[0]),
            admitted.position(&admitted.base_ids[1]),
        );
        let unavailable = "defs.receipt_context_unavailable";
        // (a) Same id and version, other content: the content hash differs.
        let mut substituted = admitted.artifacts.clone();
        substituted[a]["lifecycle"] = "deprecated".into();
        assert_eq!(admitted.verify(substituted).unwrap_err().code, "host.error");
        // (b) Another version of a base artifact is not the receipt's base.
        let mut versioned = admitted.artifacts.clone();
        versioned[a]["version"] = "0.2.0".into();
        assert_eq!(admitted.verify(versioned).unwrap_err().code, unavailable);
        // (c) An older base artifact replayed against the newer receipt.
        let mut replayed = admitted.artifacts.clone();
        replayed[a]["version"] = "0.0.1".into();
        replayed[a]["lifecycle"] = "superseded".into();
        assert_eq!(admitted.verify(replayed).unwrap_err().code, unavailable);
        // (d) Base B missing, replaced by a duplicate of A: the base count
        // still matches, but the base set is not the receipt's.
        let mut duplicated = admitted.artifacts.clone();
        duplicated[b] = duplicated[a].clone();
        assert_eq!(admitted.verify(duplicated).unwrap_err().code, "host.error");
        // A partly missing base is the generic context-unavailable refusal.
        let mut partial = admitted.artifacts.clone();
        partial.remove(b);
        let refused = admitted.verify(partial).unwrap_err();
        assert_eq!((refused.status, refused.code.as_str()), (409, unavailable));
        assert_eq!(
            serde_json::to_string(&refused).unwrap(),
            serde_json::to_string(&context_unavailable()).unwrap()
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
