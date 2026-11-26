//! HOST-INSTALL-1: change a running cell's kernel definitions, plan first.
//!
//! `POST /v1/cell/definitions:plan` and `POST /v1/cell/definitions:apply`
//! take a pack's artifacts (none in a reserved namespace) and admit them over
//! the cell's active set the way an install does ([`kernel_set::admit_over`]):
//! the retained artifacts, any of the same id replaced by its new version,
//! re-admitted whole on the base the retained receipt records. Nothing is
//! removed through this path.
//!
//! - **Authority** is [`Operation::WriteDefinitions`](crate::authz::Operation::WriteDefinitions) (the grant table's
//!   definition writers: owner and admin), decided with the explicit scope
//!   before the body is read. Anyone else gets the one opaque 403.
//! - **A refused set** (the checker's findings, a set the cell cannot serve,
//!   or live data the new laws break) answers the same refusal from plan and
//!   apply, and apply writes nothing.
//! - **Live data** (HOST-INSTALL-LIVE-1): the store checks it on every
//!   install, whatever path installs (this route, the dev host's reload,
//!   source and upload packs, a seed upgrade). Every stored object whose
//!   effective law dependencies the candidate changes (its declaration or
//!   laws, a type it extends, a type a link its laws read reaches) is
//!   evaluated under the candidate laws on the migration candidate's view
//!   (materialized defaults, the stored links). A violation, or an object no
//!   law could be evaluated on, is a finding naming the object, and the set
//!   is held back: it is never installed over data it would refuse.
//! - **Apply** is compare-and-set on the planned `expectedVersion` and
//!   installs atomically (`CellStore::install_definitions`: the version, its
//!   receipt and the ledger event in one transaction) under the model a
//!   restarted host rebuilds from the retained set (HOST-REOPEN-1). A set
//!   that is already the active one installs nothing (`unchanged`).

use std::collections::BTreeMap;

use http::StatusCode;
use norn_cell::engine::SqlEngine;
use norn_cell::{CellError, InstalledDefinitionInput, InstalledDefinitionSet, Model};
use norn_ir::{ArtifactEnvelope, RuntimeValue};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use utoipa::ToSchema;

use crate::authz::Operation;
use crate::definition_routes::DefinitionKinds;
use crate::host::{DevHost, reopen_model, serves_actions};
use crate::kernel_set::{self, AdmittedSet, KernelRefusal};
use crate::refusal::{Refusal, RefusalDiagnostic, RefusalSpan};
use crate::scope::ScopeQuery;

/// The plan route.
pub const PLAN_PATH: &str = "/v1/cell/definitions:plan";
/// The apply route.
pub const APPLY_PATH: &str = "/v1/cell/definitions:apply";

/// The most stored objects one plan evaluates under the candidate laws; a
/// change that would need more is refused rather than checked in part (the
/// store's `MAX_LAW_CHECKED_OBJECTS`, which enforces it).
pub const MAX_CHECKED_OBJECTS: usize = 50_000;

/// Stable refusal and finding codes.
pub mod codes {
    /// The submitted definitions are not admissible over the active set (422).
    pub const REFUSED: &str = "defs.refused";
    /// A stored object the candidate laws refuse (a finding of
    /// [`LIVE_DATA_REFUSED`]).
    pub const LIVE_DATA_VIOLATION: &str = "defs.live_data_violation";
    /// Stored objects the candidate laws refuse: the set is held back (409).
    pub const LIVE_DATA_REFUSED: &str = "defs.live_data_refused";
    /// A stored object a candidate law could not be evaluated on (a finding
    /// of [`LIVE_DATA_REFUSED`]): unchecked is refused, never admitted.
    pub const LIVE_DATA_UNCHECKED: &str = "defs.live_data_unchecked";
    /// More stored objects than one plan evaluates (409).
    pub const LIVE_DATA_UNBOUNDED: &str = "defs.live_data_unbounded";
    /// The cell has no active definitions to change (409).
    pub const NOT_INSTALLED: &str = "defs.not_installed";
}

/// The submitted artifacts: artifact envelopes, as the IR defines them.
fn submitted_artifacts_schema() -> utoipa::openapi::schema::Array {
    utoipa::openapi::schema::ArrayBuilder::new()
        .items(utoipa::openapi::Ref::from_schema_name(
            "NornArtifactEnvelope",
        ))
        .description(Some(
            "The pack's artifact envelopes; none in a reserved namespace (core., std.). Each one is added, or replaces the active artifact of its id.",
        ))
        .build()
}

/// `POST /v1/cell/definitions:plan` and `:apply`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DefinitionsChangeRequest {
    /// The artifacts to add or replace.
    #[schema(schema_with = submitted_artifacts_schema)]
    pub artifacts: Vec<Box<RawValue>>,
    /// The active definitions version the change was planned against.
    /// Required by apply; a plan reports the version it planned against.
    #[serde(default)]
    pub expected_version: Option<u64>,
    /// The pack's approvals (#1469): admitted against the submitted set's
    /// actions, and installed with it in one transaction, so a new action
    /// is never live without its gate. A plan returns each one's
    /// restatement; an apply carries each one's `signoff` of it and the
    /// `expectedVersion` (revision) the plan reported. Any refusal installs
    /// nothing.
    #[serde(default)]
    pub approvals: Vec<crate::definition_routes::InstallDefinitionRequest>,
}

/// An approval the change installs with the set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlannedApproval {
    /// The approval id.
    pub id: String,
    /// The id's current revision, if any: the apply's `expectedVersion`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_revision: Option<u64>,
    /// Whether the active revision is already this approval.
    pub unchanged: bool,
    /// The server's words for the approval: what the signer signs.
    pub restatement: String,
    /// Lower-case hex SHA-256 of the restatement: the apply's
    /// `signoff.restatementSha256`.
    pub restatement_sha256: String,
}

/// One artifact the change adds or replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlannedArtifact {
    /// The artifact id.
    pub artifact_id: String,
    /// The artifact kind (`entity`, `law`, `action`, …).
    pub kind: String,
    /// The active artifact's version, when the change replaces one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_version: Option<String>,
    /// The submitted artifact's version.
    pub to_version: String,
}

/// What applying the submission would change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionsPlan {
    /// The active definitions version the plan was made against: apply's
    /// `expectedVersion`.
    pub base_version: u64,
    /// The definitions version an apply would install, or the active one when
    /// nothing changes.
    pub next_version: u64,
    /// Whether the submission is already the active set (apply writes nothing).
    pub unchanged: bool,
    /// Artifacts the change adds.
    pub added: Vec<PlannedArtifact>,
    /// Artifacts the change replaces.
    pub changed: Vec<PlannedArtifact>,
    /// Submitted artifacts identical to the active ones.
    pub same: Vec<String>,
    /// Object types the installed set would serve that the active one does not.
    pub new_object_types: Vec<String>,
    /// Stored objects evaluated under the candidate laws.
    pub checked_objects: u64,
    /// The checker's non-blocking findings and live-data warnings.
    pub diagnostics: Vec<RefusalDiagnostic>,
    /// The candidate set's admission receipt digest.
    pub receipt_sha256: String,
    /// The approvals the change installs with the set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approvals: Vec<PlannedApproval>,
}

/// Whether an apply installed a new version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ApplyOutcome {
    /// A new definitions version was installed.
    Installed,
    /// The submission is already the active set; nothing was written.
    Unchanged,
}

/// The installed definitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionsApplied {
    /// Whether this call installed a version.
    pub outcome: ApplyOutcome,
    /// The active definitions version now.
    pub definitions_version: u64,
    /// The version the change was applied over.
    pub previous_version: u64,
    /// The installed set's admission receipt digest.
    pub receipt_sha256: String,
    /// The ledger commit that installed it.
    pub commit_seq: u64,
    /// What the change added, replaced and found identical.
    pub plan: DefinitionsPlan,
}

fn refused(diagnostics: Vec<RefusalDiagnostic>) -> Refusal {
    let mut refusal = Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        codes::REFUSED,
        "the submitted definitions are not admissible over the cell's active definitions",
        "each diagnostic names the rule and where in the submission it applies; nothing was changed",
    );
    refusal.diagnostics = diagnostics;
    refusal
}

fn unservable(detail: impl Into<String>) -> Refusal {
    refused(vec![RefusalDiagnostic {
        code: codes::REFUSED.to_owned(),
        severity: "error".to_owned(),
        message: detail.into(),
        law: None,
        span: None,
    }])
}

fn conflict(code: &str, message: &str, explanation: String) -> Refusal {
    Refusal::new(StatusCode::CONFLICT, code, message, explanation)
}

/// The refusal of a store preflight, as a definitions install words it.
#[allow(
    clippy::needless_pass_by_value,
    reason = "`map_err` hands the error over by value"
)]
fn preflight_refusal(error: CellError) -> Refusal {
    if let Some(refusal) = live_law_refusal(&error) {
        return refusal;
    }
    if let CellError::DefinitionVersionConflict { expected, actual } = &error {
        return conflict(
            "defs.stale",
            "the definitions changed since this revision was planned",
            format!(
                "planned against {expected:?}, the cell is at {actual:?}; re-plan against the active revision"
            ),
        );
    }
    if let CellError::DefinitionMigrationRefused(reason) = &error {
        return conflict(
            "defs.migration_refused",
            "the candidate definitions cannot carry the cell's live data",
            reason.clone(),
        );
    }
    unservable(error.to_string())
}

/// A checked submission: what it admits to, and its plan.
struct Candidate {
    admitted: AdmittedSet,
    model: Model,
    active: InstalledDefinitionSet,
    plan: DefinitionsPlan,
}

fn canonical(artifact: &ArtifactEnvelope) -> Result<Vec<u8>, Refusal> {
    let wire = serde_json::to_value(artifact).map_err(Refusal::host)?;
    RuntimeValue::from_json(wire)
        .map_err(Refusal::host)?
        .canonical_json()
        .map_err(Refusal::host)
}

fn planned(artifact: &ArtifactEnvelope, from: Option<&ArtifactEnvelope>) -> PlannedArtifact {
    PlannedArtifact {
        artifact_id: artifact.artifact_id.clone(),
        kind: serde_json::to_value(artifact.kind())
            .ok()
            .and_then(|kind| kind.as_str().map(str::to_owned))
            .unwrap_or_default(),
        from_version: from.map(|active| active.version.clone()),
        to_version: artifact.version.clone(),
    }
}

/// The submission's request and its own artifacts: bounded, decoded, none
/// reserved, none repeated, each scoped inside the cell.
fn submission<E: SqlEngine>(
    host: &DevHost<E>,
    body: &str,
) -> Result<(DefinitionsChangeRequest, Vec<ArtifactEnvelope>), Refusal> {
    if body.len() > crate::provision::MAX_PROVISION_BYTES {
        return Err(Refusal::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request.too_large",
            "the request body exceeds the host bound",
            "a definitions change is at most 1 MiB",
        ));
    }
    let request: DefinitionsChangeRequest = serde_json::from_str(body).map_err(|error| {
        Refusal::invalid(format!(
            "the request body is not a definitions change: {error}"
        ))
    })?;
    if request.artifacts.is_empty() {
        return Err(Refusal::invalid(
            "a definitions change names at least one artifact",
        ));
    }
    let tenant = crate::provision::tenant_artifacts(host.scope(), &request.artifacts, refused)?;
    let mut ids = std::collections::BTreeSet::new();
    let repeated: Vec<RefusalDiagnostic> = tenant
        .iter()
        .enumerate()
        .filter(|(_, artifact)| !ids.insert(artifact.artifact_id.as_str()))
        .map(|(index, artifact)| {
            RefusalDiagnostic::error(
                crate::refusal::codes::INVALID_REQUEST,
                format!("{} is submitted more than once", artifact.artifact_id),
                format!("/artifacts/{index}/artifactId"),
            )
        })
        .collect();
    if !repeated.is_empty() {
        let mut refusal = Refusal::invalid("a definitions change names each artifact once");
        refusal.diagnostics = repeated;
        return Err(refusal);
    }
    Ok((request, tenant))
}

/// The submitted artifacts against the active ones: added, replaced, identical.
#[allow(clippy::type_complexity, reason = "the three lists of a plan")]
fn diff(
    tenant: &[ArtifactEnvelope],
    active: &InstalledDefinitionSet,
) -> Result<(Vec<PlannedArtifact>, Vec<PlannedArtifact>, Vec<String>), Refusal> {
    let retained = kernel_set::retained_artifacts(&active.canonical_set).map_err(Refusal::host)?;
    let by_id: BTreeMap<&str, &ArtifactEnvelope> = retained
        .iter()
        .map(|artifact| (artifact.artifact_id.as_str(), artifact))
        .collect();
    let (mut added, mut changed, mut same) = (Vec::new(), Vec::new(), Vec::new());
    for artifact in tenant {
        match by_id.get(artifact.artifact_id.as_str()) {
            None => added.push(planned(artifact, None)),
            Some(active) if canonical(active)? == canonical(artifact)? => {
                same.push(artifact.artifact_id.clone());
            }
            Some(active) => changed.push(planned(artifact, Some(active))),
        }
    }
    Ok((added, changed, same))
}

/// Parse, bound and admit one body over the active set, then preflight the
/// install on the store without writing. Call only after authorization.
type Approvals = Vec<crate::definition_routes::InstallDefinitionRequest>;

fn candidate<E: SqlEngine>(
    host: &DevHost<E>,
    body: &str,
) -> Result<(Candidate, Option<u64>, Approvals), Refusal> {
    let (mut request, tenant) = submission(host, body)?;
    let approvals = std::mem::take(&mut request.approvals);
    let active = host.active_definitions()?.ok_or_else(|| {
        conflict(
            codes::NOT_INSTALLED,
            "the cell has no definitions to change",
            "provision the cell with POST /v1/cell:provision first".to_owned(),
        )
    })?;
    let admitted = kernel_set::admit_over(
        &active.canonical_set,
        active.admission_receipt.as_deref(),
        &tenant,
    )
    .map_err(|refusal| match refusal {
        KernelRefusal::Refused(findings) => refused(crate::provision::placed(&findings, &tenant)),
        KernelRefusal::Invalid(detail) => unservable(detail),
    })?;
    let (added, changed, same) = diff(&tenant, &active)?;
    let unchanged = admitted.canonical_set == active.canonical_set;
    let (model, new_object_types, next_version, checked_objects) = {
        let cell = host.cell()?;
        let store = cell.store();
        let serving = serves_actions(store.model());
        let model = reopen_model(&admitted.canonical_set, serving)
            .map_err(|refusal| unservable(refusal.explanation))?;
        let (next_version, checked) = if unchanged {
            (active.version, 0)
        } else {
            // HOST-INSTALL-LIVE-1: the store's preflight runs the live-law
            // check every install runs, on the migration candidate's view.
            let version = store
                .check_definitions(
                    &model,
                    &install_input(&admitted, &active, 0),
                    Some(active.version),
                )
                .map_err(preflight_refusal)?
                .version;
            (version, store.live_law_checked())
        };
        let served: std::collections::BTreeSet<&str> = store
            .model()
            .object_types()
            .map(|t| t.name.as_str())
            .collect();
        let new_object_types = model
            .object_types()
            .map(|t| t.name.as_str())
            .filter(|name| !served.contains(name))
            .map(str::to_owned)
            .collect();
        (model, new_object_types, next_version, checked)
    };
    Ok((
        Candidate {
            plan: DefinitionsPlan {
                base_version: active.version,
                next_version,
                unchanged,
                added,
                changed,
                same,
                new_object_types,
                checked_objects,
                diagnostics: Vec::new(),
                receipt_sha256: admitted.receipt_sha256.clone(),
                approvals: Vec::new(),
            },
            admitted,
            model,
            active,
        },
        request.expected_version,
        approvals,
    ))
}

/// The action specs (`action_id -> reads_clock`) of a canonical artifact set.
pub(crate) fn set_action_specs(
    canonical_set: &str,
) -> Result<std::collections::BTreeMap<String, bool>, Refusal> {
    let artifacts: Vec<serde_json::Value> =
        serde_json::from_str(canonical_set).map_err(Refusal::host)?;
    let mut specs = std::collections::BTreeMap::new();
    for artifact in &artifacts {
        if artifact.get("kind").and_then(serde_json::Value::as_str) != Some("action") {
            continue;
        }
        let Some(action_id) = artifact
            .pointer("/payload/actionId")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let reads_clock = artifact
            .get("payload")
            .and_then(|payload| serde_json::from_value::<norn_ir::ActionRoot>(payload.clone()).ok())
            .is_some_and(|action| norn_kernel::action_reads_clock(&action));
        specs.insert(action_id.to_owned(), reads_clock);
    }
    Ok(specs)
}

/// Admit every approval of the change against the candidate set's actions
/// (#1469); with `signed_at`, as an apply. One approval per action, and one
/// refusal refuses the change. Also refuses introducing `clock` derivation on
/// any action that remains gated by an active approval.
fn riding<E: SqlEngine>(
    host: &DevHost<E>,
    actor: Option<&str>,
    kinds: &DefinitionKinds,
    candidate: &Candidate,
    approvals: &Approvals,
    signed_at: Option<u64>,
) -> Result<Vec<crate::definition_routes::RidingApproval>, Refusal> {
    let actions = set_action_specs(&candidate.admitted.canonical_set)?;
    let mut ids = std::collections::BTreeSet::new();
    let mut gated = std::collections::BTreeSet::new();
    for (index, request) in approvals.iter().enumerate() {
        let action = serde_json::from_str::<serde_json::Value>(request.payload.get())
            .ok()
            .and_then(|payload| payload.get("action")?.as_str().map(str::to_owned));
        if !ids.insert(request.id.clone()) || action.is_some_and(|action| !gated.insert(action)) {
            return Err(refused(vec![RefusalDiagnostic::error(
                crate::approval_definitions::codes::DUPLICATE,
                "an approval id or its action appears more than once in the change",
                format!("/approvals/{index}"),
            )]));
        }
    }
    let retained_gated =
        crate::approval_definitions::gated_except(host, &|other| ids.contains(other))?;
    for action_id in &retained_gated {
        if actions.get(action_id).copied().unwrap_or(false) {
            return Err(refused(vec![RefusalDiagnostic::error(
                crate::approval_definitions::codes::INVALID,
                format!(
                    "action {action_id:?} derives from `clock` and is gated by an active approval (advancing the clock between approval and release invalidates the prepared intent digest)"
                ),
                "/artifacts",
            )]));
        }
    }
    if approvals.is_empty() {
        return Ok(Vec::new());
    }
    let actor = actor.ok_or_else(crate::authz::read_denied)?;
    // #1469 S2: one gate per action in the FINAL set. The change's own
    // approvals were checked against each other above; an active approval
    // the change revises gates whatever its new payload says, so it is not
    // counted at its old action (a gate moved off an action frees it for a
    // replacement in the same change). Each revision is still compare-and-set
    // at commit.
    approvals
        .iter()
        .enumerate()
        .map(|(index, request)| {
            crate::definition_routes::riding_approval(
                host,
                actor,
                kinds,
                request,
                &actions,
                signed_at,
                &format!("/approvals/{index}"),
                &ids,
            )
        })
        .collect()
}

fn planned_approvals(
    approvals: &[crate::definition_routes::RidingApproval],
) -> Vec<PlannedApproval> {
    approvals
        .iter()
        .map(|approval| PlannedApproval {
            id: approval.id.clone(),
            from_revision: approval.from_revision,
            unchanged: approval.unchanged,
            restatement: approval.restatement.clone(),
            restatement_sha256: approval.restatement_sha256.clone(),
        })
        .collect()
}

fn install_input(
    admitted: &AdmittedSet,
    active: &InstalledDefinitionSet,
    installed_at: u64,
) -> InstalledDefinitionInput {
    InstalledDefinitionInput {
        canonical_set: admitted.canonical_set.clone(),
        registry: vec![],
        receipt_sha256: admitted.receipt_sha256.clone(),
        admission_receipt: Some(admitted.receipt.clone()),
        readmission: None,
        diff: serde_json::json!({}),
        // The logical install time never runs backwards.
        installed_at: installed_at.max(active.installed_at.saturating_add(1)),
        layout: None,
    }
}

/// HOST-INSTALL-LIVE-1: the definitions refusal of a store's live-law
/// check, which every install runs at the shared install boundary
/// (`norn_cell`'s migration preflight, over the migration candidate's view):
/// a violation is an error finding naming the object and the law; an object
/// no candidate law could be evaluated on is a `defs.live_data_unchecked`
/// finding (published as a warning) that holds the set back by its code; too
/// many objects to check is `defs.live_data_unbounded`. `None` for any other
/// error.
#[must_use]
pub fn live_law_refusal(error: &CellError) -> Option<Refusal> {
    if let CellError::DefinitionLawUnbounded { object_type, limit } = error {
        return Some(conflict(
            codes::LIVE_DATA_UNBOUNDED,
            "the change would need more stored objects checked than one plan evaluates",
            format!(
                "a change that alters the laws of {object_type} is checked against at most {limit} stored objects"
            ),
        ));
    }
    let CellError::DefinitionLawRefused { findings, .. } = error else {
        return None;
    };
    let diagnostics: Vec<RefusalDiagnostic> = findings
        .iter()
        .map(|finding| RefusalDiagnostic {
            code: if finding.unchecked {
                codes::LIVE_DATA_UNCHECKED
            } else {
                codes::LIVE_DATA_VIOLATION
            }
            .to_owned(),
            severity: if finding.unchecked {
                "warning"
            } else {
                "error"
            }
            .to_owned(),
            message: finding.message.clone(),
            law: finding.law.clone(),
            span: Some(RefusalSpan {
                pointer: format!("/objects/{}/{}", finding.object_type, finding.id),
            }),
        })
        .collect();
    // Nothing is admitted over a finding, whatever its severity.
    Some(
        held_back(&diagnostics)
            .unwrap_or_else(|| Refusal::host("a refused live-law check carried no finding")),
    )
}

/// The live-data refusal of `findings`, if any is an error: a violation or
/// an object no candidate law could be evaluated on.
fn held_back(findings: &[RefusalDiagnostic]) -> Option<Refusal> {
    let errors: Vec<RefusalDiagnostic> = findings
        .iter()
        // LIVE-FAIL-CLOSED-1: unchecked is refused whatever its severity says.
        .filter(|finding| finding.severity == "error" || finding.code == codes::LIVE_DATA_UNCHECKED)
        .cloned()
        .collect();
    if errors.is_empty() {
        return None;
    }
    let mut refusal = conflict(
        codes::LIVE_DATA_REFUSED,
        "stored objects break the candidate laws; the definitions are held back",
        format!(
            "{} stored object finding(s); correct the objects or the laws, then plan again. Nothing was changed",
            errors.len()
        ),
    );
    refusal.diagnostics = errors;
    Some(refusal)
}

/// `POST /v1/cell/definitions:plan`: what applying the submission over the
/// active definitions would change. Never writes.
///
/// # Errors
/// The opaque 403; 400 for a body that is not a change or names a reserved
/// or repeated artifact; 413 over the bound; 422 `defs.refused` with the
/// checker's findings; 409 `defs.live_data_refused` with the stored objects
/// the candidate laws refuse, `defs.migration_refused`, or
/// `defs.not_installed`.
#[utoipa::path(post, path = "/v1/cell/definitions:plan", operation_id = "plan_cell_definitions", params(ScopeQuery), request_body = DefinitionsChangeRequest, responses((status = 200, description = "What applying the submission would change.", body = DefinitionsPlan), (status = 400, description = "The request was refused as invalid, or names an artifact in a reserved namespace.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 409, description = "The request was refused: stored objects break the candidate laws (`defs.live_data_refused`), the live data cannot be migrated, or the cell has no definitions.", body = Refusal), (status = 413, description = "The request was refused: the body exceeds the host bound.", body = Refusal), (status = 422, description = "The request was refused (`defs.refused`): the definitions are not admissible over the active set.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn plan_definitions<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    kinds: &DefinitionKinds,
    body: &str,
) -> Result<DefinitionsPlan, Refusal> {
    crate::definition_routes::authorize(host, scope, actor, Operation::WriteDefinitions)?;
    let (candidate, _expected, approvals) = candidate(host, body)?;
    let approvals = riding(host, actor, kinds, &candidate, &approvals, None)?;
    let mut plan = candidate.plan;
    with_approvals(&mut plan, &approvals);
    Ok(plan)
}

/// Record the change's approvals in `plan`. #1469 S3: each one is changed
/// or unchanged by its own payload, signed or not, and a changed gate
/// installs the next version even over an unchanged set.
fn with_approvals(
    plan: &mut DefinitionsPlan,
    approvals: &[crate::definition_routes::RidingApproval],
) {
    plan.approvals = planned_approvals(approvals);
    if plan.unchanged && plan.approvals.iter().any(|approval| !approval.unchanged) {
        plan.unchanged = false;
        plan.next_version = plan.base_version.saturating_add(1);
    }
}

/// One source approval revision the dev host commits with its set.
#[derive(Debug)]
pub struct SourceApproval {
    /// The revision to append: the next active revision of a declared
    /// approval, or the retirement of one the sources no longer declare.
    pub change: norn_cell::retained::RetainedDefinitionChange,
    /// The server's words the actor signed, for an active revision.
    pub restatement: Option<String>,
}

/// The dev host's source approvals (DEV-DEFINITIONS-1, #1469 S2): each of
/// `declared` (`(id, payload)`) admitted, restated and signed by `actor` at
/// `signed_at` the way `:apply` admits an approval riding a set, against
/// `canonical_set`'s actions; and the retirement of every active approval
/// `declared` does not name. An approval that takes over the action of a
/// retiring one (a rename) admits, because both commit together. Unchanged
/// approvals append nothing. The revisions ride one install of the set
/// ([`DevHost::install_definitions_riding`]), so none of them, and no
/// action of the set, is ever live without the others.
///
/// # Errors
/// The id of the first refused approval and its refusal; a refusal returns
/// nothing to commit.
pub fn source_approvals<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    kinds: &DefinitionKinds,
    canonical_set: &str,
    declared: &[(String, Box<RawValue>)],
    signed_at: u64,
) -> Result<Vec<SourceApproval>, Box<(String, Refusal)>> {
    use crate::definition_routes::{InstallDefinitionRequest, SignoffRequest};
    let whole = |refusal: Refusal| Box::new(("approval".to_owned(), refusal));
    let actions = set_action_specs(canonical_set).map_err(whole)?;
    let active = crate::definition_routes::active_approvals(host).map_err(whole)?;
    let retiring: std::collections::BTreeSet<String> = active
        .iter()
        .filter(|record| declared.iter().all(|(id, _)| *id != record.id))
        .map(|record| record.id.clone())
        .collect();
    // #1469 S2: uniqueness is of the FINAL set. Every declared approval is
    // checked against the other declared ones below, so no active approval
    // this reload revises or retires counts at its old action: a gate moved
    // to another action frees its old one for a replacement in this reload.
    let superseded: std::collections::BTreeSet<String> = retiring
        .iter()
        .cloned()
        .chain(declared.iter().map(|(id, _)| id.clone()))
        .collect();
    let mut out = Vec::new();
    let mut gated = std::collections::BTreeSet::new();
    for (id, payload) in declared {
        let at = |refusal: Refusal| Box::new((id.clone(), refusal));
        let action = serde_json::from_str::<serde_json::Value>(payload.get())
            .ok()
            .and_then(|payload| payload.get("action")?.as_str().map(str::to_owned));
        if action.is_some_and(|action| !gated.insert(action)) {
            return Err(at(refused(vec![RefusalDiagnostic::error(
                crate::approval_definitions::codes::DUPLICATE,
                "another declared approval already gates this action",
                "/action",
            )])));
        }
        let mut request = InstallDefinitionRequest {
            kind: "approval".to_owned(),
            id: id.clone(),
            payload: payload.clone(),
            expected_version: None,
            signoff: None,
        };
        let admit = |request: &InstallDefinitionRequest, signed_at| {
            crate::definition_routes::riding_approval(
                host,
                actor,
                kinds,
                request,
                &actions,
                signed_at,
                "",
                &superseded,
            )
        };
        let planned = admit(&request, None).map_err(at)?;
        if planned.unchanged {
            continue;
        }
        // The actor signs the server's own words, through the same check.
        request.expected_version = planned.from_revision;
        request.signoff = Some(SignoffRequest {
            restatement_sha256: planned.restatement_sha256,
        });
        let signed = admit(&request, Some(signed_at)).map_err(at)?;
        let change = signed
            .change
            .ok_or_else(|| at(Refusal::host("a changed approval produced no revision")))?;
        out.push(SourceApproval {
            change,
            restatement: Some(signed.restatement),
        });
    }
    for mut definition in active
        .into_iter()
        .filter(|record| retiring.contains(&record.id))
    {
        let expected = definition.revision;
        definition.revision = expected.saturating_add(1);
        definition.lifecycle = norn_cell::retained::RetainedLifecycle::Retired;
        definition.signoff = None;
        out.push(SourceApproval {
            change: norn_cell::retained::RetainedDefinitionChange {
                definition,
                expected_revision: Some(expected),
            },
            restatement: None,
        });
    }
    Ok(out)
}

/// `POST /v1/cell/definitions:apply`: install the submission over the active
/// definitions as the next version, compare-and-set on `expectedVersion`.
///
/// # Errors
/// As [`plan_definitions`]; 400 without `expectedVersion`; 409 `defs.stale`
/// when the active version is no longer the planned one.
#[utoipa::path(post, path = "/v1/cell/definitions:apply", operation_id = "apply_cell_definitions", params(ScopeQuery), request_body = DefinitionsChangeRequest, responses((status = 200, description = "The definitions were installed, or already are the active set.", body = DefinitionsApplied), (status = 400, description = "The request was refused as invalid: no `expectedVersion`, a reserved or repeated artifact.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 409, description = "The request was refused: the definitions changed since the plan (`defs.stale`), stored objects break the candidate laws (`defs.live_data_refused`), or the live data cannot be migrated.", body = Refusal), (status = 413, description = "The request was refused: the body exceeds the host bound.", body = Refusal), (status = 422, description = "The request was refused (`defs.refused`): the definitions are not admissible over the active set.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn apply_definitions<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    kinds: &DefinitionKinds,
    body: &str,
    now_ms: u64,
) -> Result<DefinitionsApplied, Refusal> {
    crate::definition_routes::authorize(host, scope, actor, Operation::WriteDefinitions)?;
    let (candidate, expected, approvals) = candidate(host, body)?;
    let input = install_input(&candidate.admitted, &candidate.active, now_ms);
    let approvals = riding(
        host,
        actor,
        kinds,
        &candidate,
        &approvals,
        Some(input.installed_at),
    )?;
    let changes: Vec<norn_cell::retained::RetainedDefinitionChange> = approvals
        .iter()
        .filter_map(|approval| approval.change.clone())
        .collect();
    let expected = expected.ok_or_else(|| {
        Refusal::invalid("an apply names the expectedVersion its plan was made against")
    })?;
    let Candidate {
        model,
        active,
        mut plan,
        ..
    } = candidate;
    with_approvals(&mut plan, &approvals);
    if expected != active.version {
        return Err(preflight_refusal(CellError::DefinitionVersionConflict {
            expected: Some(expected),
            actual: Some(active.version),
        }));
    }
    if plan.unchanged && changes.is_empty() {
        return Ok(DefinitionsApplied {
            outcome: ApplyOutcome::Unchanged,
            definitions_version: active.version,
            previous_version: active.version,
            receipt_sha256: active.receipt_sha256.clone(),
            commit_seq: active.commit_seq,
            plan,
        });
    }
    // HOST-INSTALL-LIVE-1: the install itself runs the live-law check, under
    // the write epoch, on the migration candidate's view.
    // #1469: the set and its approvals in one store transaction; any
    // refusal, of the set or of an approval, publishes nothing.
    let installed = host.install_definitions_riding(model, input, Some(expected), &changes)?;
    plan.checked_objects = host.cell()?.store().live_law_checked();
    Ok(DefinitionsApplied {
        outcome: ApplyOutcome::Installed,
        definitions_version: installed.version,
        previous_version: active.version,
        receipt_sha256: installed.receipt_sha256,
        commit_seq: installed.commit_seq,
        plan,
    })
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test assertions"
)]
mod tests {
    use super::{RefusalDiagnostic, RefusalSpan, codes, held_back};

    fn finding(code: &str, severity: &str) -> RefusalDiagnostic {
        RefusalDiagnostic {
            code: code.to_owned(),
            severity: severity.to_owned(),
            message: "m".to_owned(),
            law: None,
            span: Some(RefusalSpan {
                pointer: "/objects/t/1".to_owned(),
            }),
        }
    }

    /// LIVE-FAIL-CLOSED-1: an object no candidate law could be evaluated on
    /// holds the definitions back, as a violation does; it is never admitted.
    #[test]
    fn an_unchecked_object_holds_the_definitions_back() {
        for severity in ["error", "warning"] {
            let refusal = held_back(&[finding(codes::LIVE_DATA_UNCHECKED, severity)])
                .expect("unchecked is refused");
            assert_eq!(refusal.code, codes::LIVE_DATA_REFUSED);
            assert_eq!(refusal.diagnostics.len(), 1);
            assert_eq!(refusal.diagnostics[0].code, codes::LIVE_DATA_UNCHECKED);
        }
        assert!(held_back(&[]).is_none());
    }
}
