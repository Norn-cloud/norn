//! HOST-INSTALL-1: change a running cell's kernel definitions, plan first.
//!
//! `POST /v1/cell/definitions:plan` and `POST /v1/cell/definitions:apply`
//! take a pack's artifacts (none in a reserved namespace) and admit them over
//! the cell's active set the way an install does ([`kernel_set::admit_over`]):
//! the retained artifacts, any of the same id replaced by its new version,
//! re-admitted whole on the base the retained receipt records. Nothing is
//! removed through this path.
//!
//! - **Authority** is [`Operation::WriteDefinitions`] (the grant table's
//!   definition writers: owner and admin), decided with the explicit scope
//!   before the body is read. Anyone else gets the one opaque 403.
//! - **A refused set** (the checker's findings, a set the cell cannot serve,
//!   or live data the new laws break) answers the same refusal from plan and
//!   apply, and apply writes nothing.
//! - **Live data** (HOST-INSTALL-LIVE-1, #1360): every stored object of a
//!   type whose governing laws or declaration change is evaluated under the
//!   candidate laws. When only the laws change, the set installs and every
//!   object a law breaks is held back with an open finding pinned to the law
//!   and the values it compared, as an import holds a row back: it stays out
//!   of every answer, `/findings` and `:quarantined` serve it for review, and
//!   a write that satisfies the law releases it. The plan reports how many
//!   objects apply would hold back, with a sample. A violation in a type whose
//!   declaration also changes cannot be held back that way: the set is
//!   refused (409 `defs.live_data_refused`) and nothing is written.
//! - **Apply** is compare-and-set on the planned `expectedVersion` and
//!   installs atomically (`CellStore::install_definitions`: the version, its
//!   receipt and the ledger event in one transaction) under the model a
//!   restarted host rebuilds from the retained set (HOST-REOPEN-1). A set
//!   that is already the active one installs nothing (`unchanged`).

use std::collections::BTreeMap;

use http::StatusCode;
use norn_cell::engine::SqlEngine;
use norn_cell::{CellError, CellStore, InstalledDefinitionInput, InstalledDefinitionSet, Model};
use norn_ir::{ArtifactEnvelope, DataQualityFinding, DataQualityRuleRef, RuntimeValue};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use utoipa::ToSchema;

use crate::authz::Operation;
use crate::host::{DevHost, reopen_model, serves_actions};
use crate::kernel_set::{self, AdmittedSet, KernelRefusal};
use crate::refusal::{Refusal, RefusalDiagnostic, RefusalSpan};
use crate::scope::ScopeQuery;

/// The plan route.
pub const PLAN_PATH: &str = "/v1/cell/definitions:plan";
/// The apply route.
pub const APPLY_PATH: &str = "/v1/cell/definitions:apply";

/// The most stored objects one plan evaluates under the candidate laws; a
/// change that would need more is refused rather than checked in part.
pub const MAX_CHECKED_OBJECTS: usize = 50_000;
const PAGE: usize = 500;
/// Evaluation fuel per stored object, as a commit's law check spends it.
const LAW_FUEL: u64 = 1_000_000;

/// Stable refusal and finding codes.
pub mod codes {
    /// The submitted definitions are not admissible over the active set (422).
    pub const REFUSED: &str = "defs.refused";
    /// A stored object the candidate laws refuse (a finding of
    /// [`LIVE_DATA_REFUSED`]).
    pub const LIVE_DATA_VIOLATION: &str = "defs.live_data_violation";
    /// Stored objects the candidate laws break in a type whose declaration
    /// also changes: they cannot be held back, so the set is refused (409).
    pub const LIVE_DATA_REFUSED: &str = "defs.live_data_refused";
    /// A stored object a candidate law could not be evaluated on (a finding).
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
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
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
    /// Stored objects the candidate laws break, which apply holds back with
    /// findings (#1360).
    pub held_back_objects: u64,
    /// Up to [`HELD_BACK_SAMPLE`] of them: which object, and which law with
    /// the values it compared.
    pub held_back: Vec<HeldBackObject>,
    /// The checker's non-blocking findings and live-data warnings.
    pub diagnostics: Vec<RefusalDiagnostic>,
    /// The candidate set's admission receipt digest.
    pub receipt_sha256: String,
}

/// A stored object the candidate laws break.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeldBackObject {
    /// The object type.
    pub object_type: String,
    /// The object id.
    pub id: String,
    /// The law it breaks.
    pub law: String,
    /// The stored values the law compared, by path.
    #[schema(value_type = BTreeMap<String, serde_json::Value>)]
    pub values: BTreeMap<String, RuntimeValue>,
}

/// The most held-back objects a plan lists; the count names them all.
pub const HELD_BACK_SAMPLE: usize = 20;

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
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
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
fn candidate<E: SqlEngine>(
    host: &DevHost<E>,
    body: &str,
) -> Result<(Candidate, Option<u64>), Refusal> {
    let (request, tenant) = submission(host, body)?;
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
    let (model, new_object_types, next_version) = {
        let cell = host.cell()?;
        let store = cell.store();
        let serving = serves_actions(store.model());
        let model = reopen_model(&admitted.canonical_set, serving)
            .map_err(|refusal| unservable(refusal.explanation))?;
        let next_version = if unchanged {
            active.version
        } else {
            store
                .check_definitions(
                    &model,
                    &install_input(&admitted, &active, 0),
                    Some(active.version),
                )
                .map_err(preflight_refusal)?
                .version
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
        (model, new_object_types, next_version)
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
                checked_objects: 0,
                held_back_objects: 0,
                held_back: Vec::new(),
                diagnostics: Vec::new(),
                receipt_sha256: admitted.receipt_sha256.clone(),
            },
            admitted,
            model,
            active,
        },
        request.expected_version,
    ))
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

/// What the candidate laws make of the stored objects.
#[derive(Default)]
struct Live {
    /// Objects evaluated.
    checked: u64,
    /// Objects a law breaks in a type whose declaration also changes: they
    /// cannot be held back under the candidate model, so the set is refused.
    refused: Vec<RefusalDiagnostic>,
    /// Laws that could not be evaluated on stored values alone (warnings).
    unchecked: Vec<RefusalDiagnostic>,
    /// The open findings apply holds the breaking objects back with.
    findings: Vec<DataQualityFinding>,
    /// Each held-back object (sorted, once) with its first law.
    held: BTreeMap<(String, String), HeldBackObject>,
}

impl Live {
    fn plan(&self, plan: &mut DefinitionsPlan) {
        plan.checked_objects = self.checked;
        plan.held_back_objects = u64::try_from(self.held.len()).unwrap_or(u64::MAX);
        plan.held_back = self.held.values().take(HELD_BACK_SAMPLE).cloned().collect();
        plan.diagnostics.extend(self.unchecked.iter().cloned());
    }
}

/// HOST-INSTALL-LIVE-1: evaluate every stored object of a type whose
/// governing laws or declaration the candidate changes under the candidate's
/// laws. A law the object breaks is a finding to hold it back with, first
/// seen at `at_ms`; one in a type whose declaration changes is a refusal; a
/// law that cannot be evaluated on stored values alone (one that reads a
/// link) is a warning.
#[allow(
    clippy::too_many_lines,
    reason = "one paged pass decides each stored object: checked, held back, refused or unchecked"
)]
fn live_findings<E: SqlEngine>(
    store: &CellStore<E>,
    candidate: &Model,
    at_ms: u64,
) -> Result<Live, Refusal> {
    let current = store.model();
    let catalog = norn_kernel::TypeCatalog::new(candidate.object_types())
        .with_interfaces(candidate.interfaces())
        .with_property_types(candidate.property_types())
        .with_governing_laws(candidate.law_artifacts());
    let at = norn_ir::provenance::UtcTimestamp(crate::sources::rfc3339(at_ms));
    let reader = store.read();
    let mut live = Live::default();
    for object_type in current.object_types() {
        let name = object_type.name.as_str();
        let Some(next) = candidate.object_types().find(|t| t.name.as_str() == name) else {
            continue;
        };
        let laws_now = current.governing_laws(name).unwrap_or_default();
        let laws_next = candidate.governing_laws(name).unwrap_or_default();
        if laws_next.is_empty() || (laws_now == laws_next && next == object_type) {
            continue;
        }
        let reshaped = next != object_type;
        let mut after: Option<String> = None;
        loop {
            let page = reader
                .list_after(norn_cell::MAIN_WORLD, name, after.as_deref(), PAGE)
                .map_err(Refusal::host)?;
            for record in &page {
                // `list_after` includes subtypes: each is checked as itself.
                if record.object_type != name {
                    continue;
                }
                live.checked = live.checked.saturating_add(1);
                if usize::try_from(live.checked).unwrap_or(usize::MAX) > MAX_CHECKED_OBJECTS {
                    return Err(conflict(
                        codes::LIVE_DATA_UNBOUNDED,
                        "the change would need more stored objects checked than one plan evaluates",
                        format!(
                            "a change that alters the laws of {name} is checked against at most {MAX_CHECKED_OBJECTS} stored objects"
                        ),
                    ));
                }
                let key = norn_kernel::FactKey::new(name, record.id.clone());
                let pointer = format!("/objects/{name}/{}", record.id);
                let mut budget = LAW_FUEL;
                let failures = match norn_kernel::governing_laws::evaluate(
                    &catalog,
                    &key,
                    &record.values,
                    &mut budget,
                ) {
                    Ok(failures) => failures,
                    Err(reason) => {
                        live.unchecked.push(RefusalDiagnostic {
                            code: codes::LIVE_DATA_UNCHECKED.to_owned(),
                            severity: "warning".to_owned(),
                            message: format!(
                                "{name} {} was not evaluated under the candidate laws: {reason}",
                                record.id
                            ),
                            law: None,
                            span: Some(RefusalSpan { pointer }),
                        });
                        continue;
                    }
                };
                for failure in failures {
                    let law = match &failure.rule_ref {
                        DataQualityRuleRef::Law { law_id, .. } => law_id.clone(),
                        DataQualityRuleRef::Invariant { invariant_id, .. } => {
                            invariant_id.0.clone()
                        }
                        DataQualityRuleRef::Builtin { id, .. } => id.clone(),
                    };
                    if reshaped {
                        live.refused.push(RefusalDiagnostic {
                            code: codes::LIVE_DATA_VIOLATION.to_owned(),
                            severity: "error".to_owned(),
                            message: format!(
                                "{name} {} breaks {law}: {}",
                                record.id, failure.message
                            ),
                            law: Some(law),
                            span: Some(RefusalSpan {
                                pointer: pointer.clone(),
                            }),
                        });
                        continue;
                    }
                    live.findings
                        .push(held_finding(&key, &record.values, &failure, &at)?);
                    live.held
                        .entry((name.to_owned(), record.id.clone()))
                        .or_insert_with(|| HeldBackObject {
                            object_type: name.to_owned(),
                            id: record.id.clone(),
                            law,
                            values: failure.values.clone(),
                        });
                }
            }
            match page.last() {
                Some(last) if page.len() == PAGE => after = Some(last.id.clone()),
                _ => break,
            }
        }
    }
    Ok(live)
}

/// The open finding `failure` holds the stored object `key` back with: the
/// same shape a mapped import records (rule, subject, evidence digest of the
/// stored values, outcome, the values the law compared), with no source
/// provenance (it is the install's own evaluation), first seen at `at`.
fn held_finding(
    key: &norn_kernel::FactKey,
    values: &BTreeMap<String, RuntimeValue>,
    failure: &norn_kernel::governing_laws::LawFailure,
    at: &norn_ir::provenance::UtcTimestamp,
) -> Result<DataQualityFinding, Refusal> {
    let digest = norn_ir::canonical::canonical_json_of(values)
        .map(|bytes| {
            format!(
                "sha256:{}",
                norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes)
            )
        })
        .map_err(Refusal::host)?;
    DataQualityFinding::v2(
        failure.rule_ref.clone(),
        vec![norn_ir::DataQualitySubject::new(
            key.object_type.clone(),
            key.key.clone(),
        )],
        failure.severity,
        failure.message.clone(),
        failure.outcome.clone(),
        Vec::new(),
        at.clone(),
        at.clone(),
        norn_ir::DataQualityFindingStatus::Open,
    )
    .and_then(|finding| finding.with_evidence_digest(digest))
    .map(|finding| finding.with_values(failure.values.clone()))
    .map_err(Refusal::host)
}

/// The live-data refusal: objects a law breaks in a type whose declaration
/// also changes, which cannot be held back under the candidate model.
fn held_back(live: &Live) -> Option<Refusal> {
    if live.refused.is_empty() {
        return None;
    }
    let mut refusal = conflict(
        codes::LIVE_DATA_REFUSED,
        "stored objects break the candidate laws of a type whose declaration also changes",
        format!(
            "{} stored object finding(s) in a changed type; change the type and its laws in separate installs, or correct the objects. Nothing was changed",
            live.refused.len()
        ),
    );
    refusal.diagnostics.clone_from(&live.refused);
    Some(refusal)
}

/// `POST /v1/cell/definitions:plan`: what applying the submission over the
/// active definitions would change. Never writes.
///
/// # Errors
/// The opaque 403; 400 for a body that is not a change or names a reserved
/// or repeated artifact; 413 over the bound; 422 `defs.refused` with the
/// checker's findings; 409 `defs.live_data_refused` with the stored objects
/// the candidate laws break in a type whose declaration also changes, `defs.migration_refused`, or
/// `defs.not_installed`.
#[utoipa::path(post, path = "/v1/cell/definitions:plan", operation_id = "plan_cell_definitions", params(ScopeQuery), request_body = DefinitionsChangeRequest, responses((status = 200, description = "What applying the submission would change.", body = DefinitionsPlan), (status = 400, description = "The request was refused as invalid, or names an artifact in a reserved namespace.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 409, description = "The request was refused: stored objects break the candidate laws of a type whose declaration also changes (`defs.live_data_refused`; objects a law-only change breaks are held back with findings instead), the live data cannot be migrated, or the cell has no definitions.", body = Refusal), (status = 413, description = "The request was refused: the body exceeds the host bound.", body = Refusal), (status = 422, description = "The request was refused (`defs.refused`): the definitions are not admissible over the active set.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn plan_definitions<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    body: &str,
) -> Result<DefinitionsPlan, Refusal> {
    crate::definition_routes::authorize(host, scope, actor, Operation::WriteDefinitions)?;
    let (mut candidate, _expected) = candidate(host, body)?;
    if !candidate.plan.unchanged {
        let _epoch = host.definition_read()?;
        let cell = host.cell()?;
        let live = live_findings(
            cell.store(),
            &candidate.model,
            candidate.active.installed_at,
        )?;
        if let Some(refusal) = held_back(&live) {
            return Err(refusal);
        }
        live.plan(&mut candidate.plan);
    }
    Ok(candidate.plan)
}

/// `POST /v1/cell/definitions:apply`: install the submission over the active
/// definitions as the next version, compare-and-set on `expectedVersion`.
///
/// # Errors
/// As [`plan_definitions`]; 400 without `expectedVersion`; 409 `defs.stale`
/// when the active version is no longer the planned one.
#[utoipa::path(post, path = "/v1/cell/definitions:apply", operation_id = "apply_cell_definitions", params(ScopeQuery), request_body = DefinitionsChangeRequest, responses((status = 200, description = "The definitions were installed, or already are the active set.", body = DefinitionsApplied), (status = 400, description = "The request was refused as invalid: no `expectedVersion`, a reserved or repeated artifact.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 409, description = "The request was refused: the definitions changed since the plan (`defs.stale`), stored objects break the candidate laws of a type whose declaration also changes (`defs.live_data_refused`; objects a law-only change breaks are held back with findings instead), or the live data cannot be migrated.", body = Refusal), (status = 413, description = "The request was refused: the body exceeds the host bound.", body = Refusal), (status = 422, description = "The request was refused (`defs.refused`): the definitions are not admissible over the active set.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn apply_definitions<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    body: &str,
    now_ms: u64,
) -> Result<DefinitionsApplied, Refusal> {
    crate::definition_routes::authorize(host, scope, actor, Operation::WriteDefinitions)?;
    let (candidate, expected) = candidate(host, body)?;
    let expected = expected.ok_or_else(|| {
        Refusal::invalid("an apply names the expectedVersion its plan was made against")
    })?;
    let Candidate {
        admitted,
        model,
        active,
        mut plan,
    } = candidate;
    if expected != active.version {
        return Err(preflight_refusal(CellError::DefinitionVersionConflict {
            expected: Some(expected),
            actual: Some(active.version),
        }));
    }
    if plan.unchanged {
        return Ok(DefinitionsApplied {
            outcome: ApplyOutcome::Unchanged,
            definitions_version: active.version,
            previous_version: active.version,
            receipt_sha256: active.receipt_sha256.clone(),
            commit_seq: active.commit_seq,
            plan,
        });
    }
    let input = install_input(&admitted, &active, now_ms);
    let installed_at = input.installed_at;
    let mut live = Live::default();
    let installed = host.install_definitions_checked(model, input, Some(expected), |store| {
        live = live_findings(store, &install_model(store, &admitted)?, installed_at)?;
        held_back(&live).map_or_else(|| Ok(std::mem::take(&mut live.findings)), Err)
    })?;
    live.plan(&mut plan);
    Ok(DefinitionsApplied {
        outcome: ApplyOutcome::Installed,
        definitions_version: installed.version,
        previous_version: active.version,
        receipt_sha256: installed.receipt_sha256,
        commit_seq: installed.commit_seq,
        plan,
    })
}

/// The candidate's serving model, rebuilt under the write epoch (the moved
/// model goes to the install itself).
fn install_model<E: SqlEngine>(
    store: &CellStore<E>,
    admitted: &AdmittedSet,
) -> Result<Model, Refusal> {
    reopen_model(&admitted.canonical_set, serves_actions(store.model()))
        .map_err(|refusal| unservable(refusal.explanation))
}
