//! HOST-PROVISION-1: the host-neutral tenant provisioning contract,
//! `POST /v1/cell:provision`.
//!
//! One service for every network host. A host supplies its own storage
//! engine, its compiled-in [`TrustedBase`](crate::provision::TrustedBase) and its server policy; this module
//! decides nothing about who the caller is.
//!
//! - **Authority** is the host's [`Authorizer`](crate::authz::Authorizer) deciding
//!   [`Operation::ProvisionCell`](crate::authz::Operation::ProvisionCell) for the verified actor on the physical
//!   scope ([`authorize`](crate::provision::authorize)). A cell that does not exist yet has no definitions,
//!   so this one decision cannot come from the cell's own policy.
//! - **The submission is untrusted** ([`admit_submission`](crate::provision::admit_submission)). It carries no
//!   artifact in a reserved namespace ([`kernel_set::is_reserved`](crate::kernel_set::is_reserved)): the host
//!   prepends its own base and admits the tenant's artifacts as candidates
//!   against it, so base membership never depends on a name or on bytes the
//!   client sent. The submission names the base it was authored against by
//!   digest; another digest refuses and states the host's.
//!   [`kernel_set::admit_kernel_set`](crate::kernel_set::admit_kernel_set) trusts reserved names and is for
//!   compiled-in seeds and the local dev host only; this path never calls it.
//! - **The write is atomic** ([`provision_cell`](crate::provision::provision_cell)): HOST-PROVISION-ATOMIC,
//!   `CellStore::provision_definitions`. A failure leaves a fresh engine.
//! - **A retry** of a provisioned cell is answered from its first installed
//!   revision ([`replay`](crate::provision::replay)): the same submission replays without writing, a
//!   different one refuses.

use std::collections::BTreeSet;

use http::StatusCode;
use norn_cell::{CellError, CellStore, InstalledDefinitionInput, engine::SqlEngine};
use norn_ir::{ArtifactEnvelope, RuntimeValue};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use utoipa::ToSchema;

use crate::{
    authz::{AccessRequest, Authorizer, Operation, read_denied},
    host::DevHost,
    kernel_set::{self, AdmittedSet, KernelRefusal},
    refusal::{Refusal, RefusalDiagnostic, RefusalSpan},
    scope::{CellScope, ScopeQuery},
};

/// The provisioning route.
pub const PROVISION_PATH: &str = "/v1/cell:provision";

/// The largest provisioning body a host accepts, in bytes.
pub const MAX_PROVISION_BYTES: usize = 1 << 20;

/// Stable refusal codes of the provisioning contract.
pub mod codes {
    /// The scope has no cell yet; told only to a verified member of its
    /// organization.
    pub const NOT_PROVISIONED: &str = "cell.not_provisioned";
    /// The cell was provisioned from a different submission.
    pub const PROVISION_CONFLICT: &str = "cell.provision_conflict";
    /// The submitted definitions are not admissible.
    pub const PROVISION_REFUSED: &str = "cell.provision_refused";
    /// The submission carries an artifact in a reserved namespace.
    pub const RESERVED_ARTIFACT: &str = "cell.reserved_artifact";
    /// The submission was authored against another trusted base.
    pub const BASE_MISMATCH: &str = "cell.base_mismatch";
    /// A submitted artifact is scoped outside the cell being provisioned
    /// (a diagnostic of `cell.provision_refused`).
    pub const SCOPE_CONFLICT: &str = "cell.scope_conflict";
}

/// The trusted base a host compiles in: the only artifacts a provisioned
/// cell holds in a reserved namespace.
#[derive(Debug, Clone)]
pub struct TrustedBase {
    artifacts: Vec<ArtifactEnvelope>,
    ids: BTreeSet<String>,
    sha256: String,
}

impl TrustedBase {
    /// The base made of `sources`, each one artifact envelope as JSON text.
    ///
    /// # Errors
    /// A source that is not an artifact envelope, an id outside the reserved
    /// namespaces, or a repeated id.
    pub fn from_sources(sources: &[&str]) -> Result<Self, String> {
        let mut entries = Vec::with_capacity(sources.len());
        for source in sources {
            let value = RuntimeValue::from_json_text(source).map_err(|error| error.to_string())?;
            let artifact: ArtifactEnvelope = norn_ir::runtime::decode(&value)?;
            if !kernel_set::is_reserved(&artifact.artifact_id) {
                return Err(format!(
                    "{} is not in a reserved namespace",
                    artifact.artifact_id
                ));
            }
            entries.push((artifact, value));
        }
        entries.sort_by(|(left, _), (right, _)| left.artifact_id.cmp(&right.artifact_id));
        let ids: BTreeSet<String> = entries
            .iter()
            .map(|(artifact, _)| artifact.artifact_id.clone())
            .collect();
        if ids.len() != entries.len() {
            return Err("the trusted base repeats an artifact id".to_owned());
        }
        let (artifacts, values): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        let canonical = RuntimeValue::Array(values)
            .canonical_json()
            .map_err(|error| error.to_string())?;
        let canonical = String::from_utf8(canonical).map_err(|error| error.to_string())?;
        Ok(Self {
            artifacts,
            ids,
            sha256: norn_cell::retained::payload_sha256(&canonical),
        })
    }

    /// Lower-case hex SHA-256 of the canonical JSON array of the base
    /// artifacts, ordered by artifact id.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The base artifact ids.
    #[must_use]
    pub const fn ids(&self) -> &BTreeSet<String> {
        &self.ids
    }
}

/// The submitted artifacts: artifact envelopes, as the IR defines them.
fn submitted_artifacts_schema() -> utoipa::openapi::schema::Array {
    utoipa::openapi::schema::ArrayBuilder::new()
        .items(utoipa::openapi::Ref::from_schema_name(
            "NornArtifactEnvelope",
        ))
        .description(Some(
            "The tenant's artifact envelopes; none in a reserved namespace (core., std.).",
        ))
        .build()
}

/// `POST /v1/cell:provision`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvisionRequest {
    /// Lower-case hex SHA-256 of the trusted base the artifacts were
    /// authored against; it must be the host's.
    pub base_sha256: String,
    /// The tenant's artifact envelopes. None may be in a reserved namespace
    /// (`core.`, `std.`): the host supplies the trusted base itself.
    #[schema(schema_with = submitted_artifacts_schema)]
    pub artifacts: Vec<Box<RawValue>>,
}

/// The one member read before an artifact is decoded.
#[derive(Deserialize)]
struct Named {
    #[serde(rename = "artifactId")]
    artifact_id: Option<String>,
}

/// Whether the call provisioned the cell or found it provisioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ProvisionOutcome {
    /// This call created the cell.
    Provisioned,
    /// The cell already holds exactly this submission; nothing was written.
    Replayed,
}

/// The provisioned cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionReceipt {
    /// Whether this call created the cell.
    pub outcome: ProvisionOutcome,
    /// The cell's logical identity, bound when it was created.
    pub cell_id: String,
    /// The definitions version the submission was installed as.
    pub definitions_version: u64,
    /// The checker's admission receipt digest for the installed set.
    pub receipt_sha256: String,
    /// The trusted base the set was admitted against.
    pub base_sha256: String,
}

/// Decide [`Operation::ProvisionCell`] for `actor` on the physical scope
/// `cell` and admit the explicit request scope, before a body is read.
///
/// # Errors
/// The one opaque 403 for an anonymous, denied or out-of-scope request.
pub fn authorize(
    authorizer: &dyn Authorizer,
    cell: &CellScope,
    scope: &ScopeQuery,
    actor: Option<&str>,
) -> Result<(), Refusal> {
    let decided = authorizer.authorize(&AccessRequest {
        scope: cell,
        actor,
        operation: Operation::ProvisionCell,
    });
    let scoped = cell.admit(scope.org_id.as_deref(), scope.environment_id.as_deref());
    if decided.is_err() || scoped.is_err() || actor.is_none() {
        return Err(read_denied());
    }
    Ok(())
}

/// The refusal a verified member of the organization gets from a scope with
/// no cell.
#[must_use]
pub fn not_provisioned() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        codes::NOT_PROVISIONED,
        "no cell is provisioned for this scope",
        "provision the cell with POST /v1/cell:provision before using it",
    )
}

fn conflict() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        codes::PROVISION_CONFLICT,
        "the cell is already provisioned from different definitions",
        "a cell is provisioned once; change its definitions through the definition routes",
    )
}

fn refused(diagnostics: Vec<RefusalDiagnostic>) -> Refusal {
    let mut refusal = Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        codes::PROVISION_REFUSED,
        "the submitted definitions are not admissible",
        "each diagnostic names the rule and where in the submission it applies",
    );
    refusal.diagnostics = diagnostics;
    refusal
}

/// A refusal that the host cannot serve the admitted model.
#[must_use]
pub fn unservable(detail: impl Into<String>) -> Refusal {
    refused(vec![RefusalDiagnostic {
        code: codes::PROVISION_REFUSED.to_owned(),
        severity: "error".to_owned(),
        message: detail.into(),
        law: None,
        span: None,
    }])
}

/// The submission's own artifacts, decoded from their exact text: none in a
/// reserved namespace, each an artifact envelope scoped inside `cell`.
fn tenant_artifacts(
    cell: &CellScope,
    sources: &[Box<RawValue>],
) -> Result<Vec<ArtifactEnvelope>, Refusal> {
    let mut reserved = Vec::new();
    let mut malformed = Vec::new();
    let mut tenant = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        // Read from the exact text: numbers keep their tokens.
        let id = serde_json::from_str::<Named>(source.get())
            .ok()
            .and_then(|named| named.artifact_id);
        match id.as_deref() {
            Some(id) if kernel_set::is_reserved(id) => reserved.push(RefusalDiagnostic::error(
                codes::RESERVED_ARTIFACT,
                format!("{id} is in a reserved namespace"),
                format!("/artifacts/{index}/artifactId"),
            )),
            Some(_) => {
                let decoded = RuntimeValue::from_json_text(source.get())
                    .map_err(|error| error.to_string())
                    .and_then(|value| norn_ir::runtime::decode::<ArtifactEnvelope>(&value));
                match decoded {
                    Ok(artifact) => tenant.push(artifact),
                    Err(error) => malformed.push(RefusalDiagnostic::error(
                        crate::refusal::codes::INVALID_REQUEST,
                        format!("not an artifact envelope: {error}"),
                        format!("/artifacts/{index}"),
                    )),
                }
            }
            None => malformed.push(RefusalDiagnostic::error(
                crate::refusal::codes::INVALID_REQUEST,
                "an artifact envelope names its artifactId",
                format!("/artifacts/{index}/artifactId"),
            )),
        }
    }
    if !reserved.is_empty() {
        let mut refusal = Refusal::new(
            StatusCode::BAD_REQUEST,
            codes::RESERVED_ARTIFACT,
            "the submission carries an artifact in a reserved namespace",
            "core. and std. name the trusted base, which the host supplies; submit only your own artifacts",
        );
        refusal.diagnostics = reserved;
        return Err(refusal);
    }
    if !malformed.is_empty() {
        let mut refusal = Refusal::invalid("the submission's artifacts are not artifact envelopes");
        refusal.diagnostics = malformed;
        return Err(refusal);
    }
    // An artifact scoped outside the cell would be refused by every later
    // read of the retained set: refuse it now, before anything is written,
    // as a flow install does.
    let outside: Vec<RefusalDiagnostic> = tenant
        .iter()
        .enumerate()
        .filter(|(_, artifact)| {
            norn_checker::scope_conflict(&cell.as_scope_ref(), &artifact.scope).is_some()
        })
        .map(|(index, artifact)| {
            RefusalDiagnostic::error(
                codes::SCOPE_CONFLICT,
                format!(
                    "{} is scoped outside the cell being provisioned",
                    artifact.artifact_id
                ),
                format!("/artifacts/{index}/scope"),
            )
        })
        .collect();
    if !outside.is_empty() {
        return Err(refused(outside));
    }
    // The decoded ids are checked again: the base is exactly the host's.
    if tenant
        .iter()
        .any(|artifact| kernel_set::is_reserved(&artifact.artifact_id))
    {
        return Err(Refusal::host(
            "a reserved artifact passed the submission check",
        ));
    }
    Ok(tenant)
}

/// Bound, decode and admit one provisioning body for the cell `cell` against
/// `base`. Call only after [`authorize`]: the findings describe the caller's
/// own submission.
///
/// # Errors
/// `request.too_large` (413) above [`MAX_PROVISION_BYTES`]; `request.invalid`
/// (400) for a body that is not a submission; `cell.base_mismatch` (409) for
/// another base digest, stating the host's; `cell.reserved_artifact` (400)
/// for any artifact in a reserved namespace, before admission;
/// `cell.provision_refused` (422) with `cell.scope_conflict` findings for
/// artifacts scoped outside `cell`, or with the checker's findings.
pub fn admit_submission(
    base: &TrustedBase,
    cell: &CellScope,
    body: &str,
) -> Result<AdmittedSet, Refusal> {
    if body.len() > MAX_PROVISION_BYTES {
        return Err(Refusal::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request.too_large",
            "the request body exceeds the host bound",
            "a provisioning body is at most 1 MiB",
        ));
    }
    let request: ProvisionRequest = serde_json::from_str(body).map_err(|error| {
        Refusal::invalid(format!(
            "the request body is not a provisioning submission: {error}"
        ))
    })?;
    if request.base_sha256 != base.sha256 {
        return Err(Refusal::new(
            StatusCode::CONFLICT,
            codes::BASE_MISMATCH,
            "the submission was authored against another trusted base",
            format!(
                "this host's trusted base is sha256 {}; rebuild the submission against it",
                base.sha256
            ),
        ));
    }
    let tenant = tenant_artifacts(cell, &request.artifacts)?;
    let position = |id: &str| {
        tenant
            .iter()
            .position(|artifact| artifact.artifact_id == id)
    };
    let mut artifacts = base.artifacts.clone();
    artifacts.extend(tenant.iter().cloned());
    kernel_set::admit_kernel_set_with_base(&artifacts, &base.ids).map_err(|refusal| match refusal {
        KernelRefusal::Refused(findings) => refused(
            findings
                .iter()
                .filter_map(|finding| {
                    let mut diagnostic =
                        RefusalDiagnostic::from_checker(std::slice::from_ref(finding), "").pop()?;
                    // A finding is placed in the submission when it names one
                    // of the tenant's artifacts; a base finding has no place.
                    diagnostic.span =
                        finding
                            .artifact_id
                            .as_deref()
                            .and_then(position)
                            .map(|index| RefusalSpan {
                                pointer: format!(
                                    "/artifacts/{index}{}",
                                    finding
                                        .span
                                        .as_ref()
                                        .map_or("", |span| span.pointer.as_str())
                                ),
                            });
                    Some(diagnostic)
                })
                .collect(),
        ),
        KernelRefusal::Invalid(detail) => unservable(detail),
    })
}

/// The install input of `admitted` at the logical time `installed_at`.
fn install_input(admitted: &AdmittedSet, installed_at: u64) -> InstalledDefinitionInput {
    InstalledDefinitionInput {
        canonical_set: admitted.canonical_set.clone(),
        registry: vec![],
        receipt_sha256: admitted.receipt_sha256.clone(),
        admission_receipt: Some(admitted.receipt.clone()),
        readmission: None,
        diff: serde_json::json!({}),
        installed_at,
        layout: None,
    }
}

fn receipt<E: SqlEngine>(
    store: &CellStore<E>,
    base: &TrustedBase,
    outcome: ProvisionOutcome,
) -> Result<ProvisionReceipt, Refusal> {
    let first = store
        .read()
        .installed_definitions(Some(1))
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("the provisioned cell has no first revision"))?;
    let cell_id = store
        .cell_id()
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("the provisioned cell has no identity"))?;
    Ok(ProvisionReceipt {
        outcome,
        cell_id: String::from(cell_id),
        definitions_version: first.version,
        receipt_sha256: first.receipt_sha256,
        base_sha256: base.sha256.clone(),
    })
}

/// Provision the scope's cell from the caller's definitions (owner only).
///
/// One engine transaction (HOST-PROVISION-ATOMIC) writes the store schema, the
/// scope binding, a fresh cell identity, the definitions, their receipt and
/// the ledger commit together, or nothing. The host calls this on a fresh
/// engine after [`authorize`] and [`admit_submission`]; `now_ms` is the host
/// clock (Unix milliseconds), recorded as the install time.
///
/// # Errors
/// `cell.provision_conflict` (409) when the engine already holds a cell
/// provisioned from something else; `host.error` (500) for a storage failure,
/// which leaves the engine as it was.
#[utoipa::path(post, path = "/v1/cell:provision", params(ScopeQuery), request_body = ProvisionRequest, responses((status = 200, description = "The cell was provisioned, or already holds exactly this submission.", body = ProvisionReceipt), (status = 400, description = "The request was refused as invalid, or carries an artifact in a reserved namespace.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 405, description = "The request was refused: the route takes POST.", body = Refusal), (status = 409, description = "The request was refused: the cell holds other definitions, or the submission names another trusted base.", body = Refusal), (status = 413, description = "The request was refused: the body exceeds the host bound.", body = Refusal), (status = 422, description = "The request was refused: the definitions are not admissible.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn provision_cell<E: SqlEngine>(
    engine: E,
    scope: &CellScope,
    base: &TrustedBase,
    admitted: AdmittedSet,
    now_ms: u64,
) -> Result<(CellStore<E>, ProvisionReceipt), Refusal> {
    let input = install_input(&admitted, now_ms);
    let store = CellStore::provision_definitions(engine, admitted.model, &input, &scope.key())
        .map_err(|error| {
            if matches!(error, CellError::IdempotencyConflict(_)) {
                conflict()
            } else {
                Refusal::host(error)
            }
        })?;
    let receipt = receipt(&store, base, ProvisionOutcome::Provisioned)?;
    Ok((store, receipt))
}

/// Answer a provisioning request for a cell that already exists, from its
/// first installed revision: the same submission replays, whatever was
/// installed since; anything else refuses. Never writes.
///
/// The caller has already authorized [`Operation::ProvisionCell`] on `host`.
///
/// # Errors
/// `cell.provision_conflict` (409) for a different submission.
pub fn replay<E: SqlEngine>(
    host: &DevHost<E>,
    base: &TrustedBase,
    admitted: &AdmittedSet,
) -> Result<ProvisionReceipt, Refusal> {
    let _epoch = host.definition_read()?;
    let cell = host.cell()?;
    let store = cell.store();
    let first = store
        .read()
        .installed_definitions(Some(1))
        .map_err(Refusal::host)?
        .ok_or_else(conflict)?;
    if first.canonical_set != admitted.canonical_set
        || first.receipt_sha256 != admitted.receipt_sha256
    {
        return Err(conflict());
    }
    receipt(store, base, ProvisionOutcome::Replayed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "contract fixtures")]
mod tests {
    use super::*;

    const UNITS: &str = include_str!("../../../registry/core/artifacts/core.units.json");

    fn cell() -> CellScope {
        CellScope {
            org_id: "org_a".into(),
            environment_id: "prod".into(),
        }
    }

    #[test]
    fn a_base_holds_only_reserved_ids_once_and_its_digest_ignores_order() {
        let std_types =
            include_str!("../../../registry/core/artifacts/std.value_types.entity.json");
        let one = TrustedBase::from_sources(&[UNITS, std_types]).unwrap();
        let other = TrustedBase::from_sources(&[std_types, UNITS]).unwrap();
        assert_eq!(one.sha256(), other.sha256());
        assert_eq!(one.sha256().len(), 64);
        assert!(TrustedBase::from_sources(&[UNITS, UNITS]).is_err());
        let foreign = include_str!("../../../registry/core/artifacts/party.Brand.entity.json");
        assert!(TrustedBase::from_sources(&[foreign]).is_err());
    }

    /// The network path never reaches by-name trust: any reserved id in a
    /// submission refuses before admission, whatever the artifact says.
    #[test]
    fn a_reserved_id_in_a_submission_refuses_before_admission() {
        let base = TrustedBase::from_sources(&[UNITS]).unwrap();
        for id in ["core.units", "core.forged", "std.forged"] {
            let body = serde_json::json!({
                "baseSha256": base.sha256(),
                // Not an artifact envelope at all: admission would refuse it
                // differently, so this refusal came first.
                "artifacts": [{ "artifactId": "tenant.ok" }, { "artifactId": id, "forged": true }],
            })
            .to_string();
            let refusal = admit_submission(&base, &cell(), &body).expect_err("reserved");
            assert_eq!(refusal.code, codes::RESERVED_ARTIFACT, "{id}");
            assert_eq!(refusal.status, 400);
            assert_eq!(
                refusal.diagnostics[0].span.as_ref().unwrap().pointer,
                "/artifacts/1/artifactId"
            );
        }
    }

    #[test]
    fn another_base_digest_refuses_and_states_the_hosts() {
        let base = TrustedBase::from_sources(&[UNITS]).unwrap();
        let body = serde_json::json!({ "baseSha256": "0".repeat(64), "artifacts": [] }).to_string();
        let refusal = admit_submission(&base, &cell(), &body).expect_err("mismatch");
        assert_eq!(refusal.code, codes::BASE_MISMATCH);
        assert_eq!(refusal.status, 409);
        assert!(refusal.explanation.contains(base.sha256()));
    }

    #[test]
    fn a_body_over_the_bound_or_not_a_submission_refuses() {
        let base = TrustedBase::from_sources(&[UNITS]).unwrap();
        let large = " ".repeat(MAX_PROVISION_BYTES + 1);
        assert_eq!(
            admit_submission(&base, &cell(), &large)
                .expect_err("large")
                .status,
            413
        );
        for body in [
            "",
            "[]",
            r#"{"artifacts":[]}"#,
            r#"{"baseSha256":"x","artifacts":[],"more":1}"#,
        ] {
            assert_eq!(
                admit_submission(&base, &cell(), body)
                    .expect_err("shape")
                    .status,
                400,
                "{body}"
            );
        }
        let nameless =
            serde_json::json!({ "baseSha256": base.sha256(), "artifacts": [{}] }).to_string();
        assert_eq!(
            admit_submission(&base, &cell(), &nameless)
                .expect_err("nameless")
                .status,
            400
        );
    }
}
