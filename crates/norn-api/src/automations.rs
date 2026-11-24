//! AUTOMATION-1: server-driven When / If / Then automation routes (`:preview`,
//! `/{id}:run`, and `/{id}/runs`).
//!
//! Execution, proposal parking, and ledger run history live in
//! `automation_exec`; every route enforces scope, role, and row-level
//! authorization before scanning or firing.

use std::collections::BTreeMap;

use http::StatusCode;
use norn_cell::engine::SqlEngine;
use norn_cell::retained::RetainedLifecycle;
use norn_cell::{MAIN_WORLD, ObjectRecord, StoreReader};
use norn_ir::{ActionEffectProgram, RuntimeObject, RuntimeValue};
use norn_kernel::LogicalTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::openapi::Type;
use utoipa::openapi::schema::{AdditionalProperties, Object, ObjectBuilder, SchemaType};
use utoipa::{IntoParams, ToSchema};

use crate::authz::{Operation, ReadCapability, ReadRequest};
use crate::automation_definitions::{AutomationPayload, AutomationThen, AutomationWhen};
pub(crate) use crate::automation_exec::{
    FireTarget, approved_proposal_call, fire_for_record, next_stale_wake, on_action_committed,
    read_runs_from_ledger, run_approved_proposal, run_due_stale,
};
use crate::decisions::{
    DecisionCall, DecisionContext, DecisionDocument, DecisionFlow, DecisionOptionDocument,
    DecisionOptionEffect, DecisionRisk, DecisionRun, DecisionStep, DecisionSubject, OpenDecision,
};
use crate::definition_routes::authorize;
use crate::host::DevHost;
use crate::object_routes::{ObjectAccess, collection, mapped, row_readable, scan_listed};
use crate::question_routes::QuestionRuntime;
use crate::refusal::{Refusal, RefusalDiagnostic};
use crate::scope::ScopeQuery;

/// An automation proposal carried inside a parked `norn.decision@1` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeldProposal {
    pub(crate) definition: String,
    pub(crate) revision: u64,
    pub(crate) title: String,
    pub(crate) object_type: String,
    pub(crate) object_id: String,
    pub(crate) object_version: u64,
    pub(crate) action: String,
    pub(crate) input: RuntimeObject,
    pub(crate) idempotency_key: String,
}

/// Decode the `automation` member of a parked `norn.decision@1` command.
#[must_use]
pub(crate) fn held_proposal(value: &RuntimeValue) -> Option<HeldProposal> {
    let text = |name: &str| {
        value
            .get(name)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
    };
    let Some(RuntimeValue::Object(input)) = value.get("input") else {
        return None;
    };
    Some(HeldProposal {
        definition: text("definition")?,
        revision: norn_ir::runtime::decode(value.get("revision")?).ok()?,
        title: text("title")?,
        object_type: text("objectType")?,
        object_id: text("objectId")?,
        object_version: norn_ir::runtime::decode(value.get("objectVersion")?).ok()?,
        action: text("action")?,
        input: input.clone(),
        idempotency_key: text("idempotencyKey")?,
    })
}

/// Project an open automation proposal decision to the wire [`DecisionDocument`].
pub(crate) fn proposal_document(
    decision: &OpenDecision,
    proposal: &HeldProposal,
) -> DecisionDocument {
    let risk = match decision.request.risk {
        norn_cell::decision_ledger::Risk::Low => DecisionRisk::Low,
        norn_cell::decision_ledger::Risk::Medium => DecisionRisk::Medium,
        norn_cell::decision_ledger::Risk::High => DecisionRisk::High,
    };
    let options = decision
        .request
        .options
        .iter()
        .map(|option| DecisionOptionDocument {
            id: option.id.clone(),
            label: option.label.clone(),
            scope: option.scope.clone(),
            effect: DecisionOptionEffect {
                action: option.effect.action.clone(),
                binding: option.effect.binding.clone(),
            },
        })
        .collect();
    DecisionDocument {
        id: decision.id.clone(),
        run: DecisionRun {
            id: decision.run_id.clone(),
            generation: decision.generation.0,
            state: None,
        },
        flow: DecisionFlow {
            rule_id: proposal.definition.clone(),
            version: proposal.revision.to_string(),
            title: Some(proposal.title.clone()),
        },
        step: DecisionStep {
            id: decision.step.clone(),
            label: Some("Proposed action".to_owned()),
        },
        question: decision.request.question.clone(),
        risk,
        options,
        agent_pick: decision.request.agent_pick.clone(),
        requested_at_ms: decision.requested_at_ms,
        expires_at_ms: decision.expires_at_ms,
        context: DecisionContext {
            subjects: vec![DecisionSubject {
                object_type: proposal.object_type.clone(),
                id: proposal.object_id.clone(),
            }],
        },
        input_schema: None,
        call: Some(DecisionCall {
            action: proposal.action.clone(),
            definition: proposal.definition.clone(),
            input: RuntimeValue::Object(proposal.input.clone()),
            trace: None,
        }),
    }
}

pub(crate) fn action_meta<E: SqlEngine>(
    host: &DevHost<E>,
    reader: &StoreReader<'_, E>,
    object_type: &str,
    action_id: &str,
) -> Result<(String, String), Refusal> {
    let actions = crate::automation_definitions::actions_map(host, reader, object_type)?;
    let action = actions.get(action_id).ok_or_else(|| {
        Refusal::invalid(format!(
            "action {action_id:?} is not admitted for {object_type}"
        ))
    })?;
    let field = match &action.effect_program {
        Some(
            ActionEffectProgram::UpsertInputObject {
                object_id_field, ..
            }
            | ActionEffectProgram::FireTransition {
                object_id_field, ..
            }
            | ActionEffectProgram::MutateParentWithRows {
                object_id_field, ..
            },
        ) => object_id_field.clone(),
        None => {
            return Err(Refusal::invalid(format!(
                "action {action_id:?} has no runnable effect program"
            )));
        }
    };
    let title = action
        .title
        .clone()
        .unwrap_or_else(|| action.action_id.clone());
    Ok((field, title))
}

pub(crate) fn build_action_input(
    then: &AutomationThen,
    object_id_field: &str,
    object_id: &str,
) -> Result<RuntimeObject, Refusal> {
    let mut map = RuntimeObject::new();
    for (key, value) in then.input() {
        map.insert(key.clone(), json_to_runtime(value)?);
    }
    map.insert(
        object_id_field.to_owned(),
        RuntimeValue::String(object_id.to_owned()),
    );
    Ok(map)
}

pub(crate) fn json_to_runtime(value: &Value) -> Result<RuntimeValue, Refusal> {
    let text = serde_json::to_string(value).map_err(Refusal::host)?;
    RuntimeValue::from_json_text(&text).map_err(Refusal::host)
}

pub(crate) fn dedupe_hash(
    automation_id: &str,
    revision: u64,
    object_type: &str,
    object_id: &str,
    object_version: u64,
) -> Result<String, Refusal> {
    let digest = norn_ir::canonical::domain_hash_hex(
        b"norn.automation.dedupe/1\0",
        &(
            automation_id,
            revision,
            object_type,
            object_id,
            object_version,
        ),
    )
    .map_err(Refusal::host)?;
    Ok(digest.get(..32).unwrap_or(&digest).to_owned())
}

pub(crate) fn run_entry_id(dedupe_key: &str, suffix: &str) -> Result<String, Refusal> {
    let digest =
        norn_ir::canonical::domain_hash_hex(b"norn.automation.run_id/1\0", &(dedupe_key, suffix))
            .map_err(Refusal::host)?;
    Ok(format!("arun_{}", digest.get(..24).unwrap_or(&digest)))
}

/// Command id prefix of every automation-proposed Inbox decision.
pub const COMMAND_PREFIX: &str = "norn.automation/";
/// Step id carried by an automation proposal decision.
pub const AUTOMATION_STEP: &str = "propose";
/// Ledger event type recording one automation execution outcome.
pub const AUTOMATION_RUN_EVENT: &str = "norn.automation.run";
/// Maximum visible rows scanned during a preview or run evaluation.
pub const MAX_AUTOMATION_SCAN: usize = 1_000;
/// Maximum matching rows returned in a preview response.
pub const MAX_PREVIEW_MATCHES: usize = 100;
/// Maximum runs returned by `GET /v1/automations/{id}/runs`.
pub const MAX_RUN_HISTORY: usize = 100;

/// Request body for `POST /v1/automations:preview`: either an installed
/// automation `id` or a draft `payload`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationPreviewRequest {
    /// Installed automation definition id to preview.
    #[serde(default)]
    pub id: Option<String>,
    /// Draft automation payload to check and preview without installing.
    #[serde(default)]
    #[schema(schema_with = free_object_schema)]
    pub payload: Option<Value>,
}

/// One action the automation would propose or call for a matched record.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationProposedAction {
    /// Admitted action id.
    pub action_id: String,
    /// Complete input object (static arguments merged with target id field).
    #[schema(schema_with = free_object_schema)]
    pub input: BTreeMap<String, RuntimeValue>,
}

/// One record currently matching an automation during preview.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationPreviewMatch {
    /// Concrete object type.
    pub object_type: String,
    /// Record id.
    pub id: String,
    /// Record version.
    pub version: u64,
    /// Projected property values the caller may read.
    #[schema(schema_with = free_object_schema)]
    pub values: BTreeMap<String, RuntimeValue>,
    /// Human-readable explanation of why this record matched.
    pub reason: String,
    /// Elapsed milliseconds since last commit, for staleness triggers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_for_ms: Option<u64>,
    /// The action that would be proposed or run.
    pub proposed_action: AutomationProposedAction,
}

/// Response of `POST /v1/automations:preview`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationPreviewResponse {
    /// Target object type.
    pub object_type: String,
    /// Number of readable records scanned.
    pub scanned_count: u64,
    /// Number of matching records found.
    pub matched_count: u64,
    /// True when the scan bound or match display cap was reached.
    pub approximate: bool,
    /// Plain-words restatement of the automation.
    pub restatement: String,
    /// Matching records (bounded by [`MAX_PREVIEW_MATCHES`]).
    pub matches: Vec<AutomationPreviewMatch>,
}

/// Outcome of one automation run entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunOutcome {
    /// A proposal was parked in the Inbox for a human decision.
    Proposed,
    /// The action committed under the cell's laws and authority.
    Committed,
    /// The cell refused the action (law, precondition, or authority).
    Refused,
    /// Skipped (disabled or already evaluated for this record version).
    Skipped,
}

/// One durable automation run entry recorded on the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunRecord {
    /// Unique run entry id.
    pub id: String,
    /// Automation definition id.
    pub automation_id: String,
    /// Automation definition revision.
    pub automation_revision: u64,
    /// Deduplication key `(automation, revision, objectType, objectId, version)`.
    pub dedupe_key: String,
    /// What triggered this evaluation.
    pub trigger: String,
    /// Subject object type.
    pub object_type: String,
    /// Subject record id.
    pub object_id: String,
    /// Subject record version.
    pub object_version: u64,
    /// Action targeted by this run.
    pub action_id: String,
    /// Outcome.
    pub outcome: AutomationRunOutcome,
    /// Parked Inbox decision id, when `outcome == Proposed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    /// Commit sequence, when an action committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_seq: Option<u64>,
    /// Refusal code, when `outcome == Refused`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_code: Option<String>,
    /// Refusal message, when `outcome == Refused`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_message: Option<String>,
    /// Logical timestamp in milliseconds.
    pub recorded_at_ms: u64,
}

/// Response of `POST /v1/automations/{id}:run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunResponse {
    /// Automation definition id.
    pub id: String,
    /// Automation definition revision.
    pub version: u64,
    /// Whether the automation was enabled when evaluated.
    pub enabled: bool,
    /// Records scanned.
    pub scanned_count: u64,
    /// Records matching `when` and `if`.
    pub matched_count: u64,
    /// Proposals parked into the Inbox.
    pub proposed: u64,
    /// Actions committed directly under laws and authority.
    pub committed: u64,
    /// Actions refused by laws, preconditions, or authority.
    pub refused: u64,
    /// Already-evaluated records skipped by deduplication.
    pub skipped: u64,
    /// Run entries produced during this evaluation.
    pub runs: Vec<AutomationRunRecord>,
}

/// Query parameters for `GET /v1/automations/{id}/runs`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct AutomationRunsQuery {
    /// Organization (required).
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Environment (required).
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
    /// Maximum runs to return (1..100, default 50).
    pub limit: Option<u32>,
}

/// Response of `GET /v1/automations/{id}/runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunsPage {
    /// Automation definition id.
    pub id: String,
    /// Recent runs, newest first.
    pub items: Vec<AutomationRunRecord>,
}

fn free_object_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(SchemaType::Type(Type::Object))
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .description(Some("An automation definition payload object."))
        .build()
}

/// `POST /v1/automations:preview`: test an automation against current readable
/// records without firing any action or creating any Inbox proposal.
///
/// # Errors
/// 403 when unauthorized; 404 if `id` is unknown; 422 if the draft payload is
/// invalid; 500 on host failure.
#[utoipa::path(post, path = "/v1/automations:preview", params(ScopeQuery), request_body = AutomationPreviewRequest, responses((status = 200, description = "Records matching the automation right now; nothing was fired.", body = AutomationPreviewResponse), (status = 400, description = "Invalid request.", body = Refusal), (status = 403, description = "Scope or authority denied.", body = Refusal), (status = 404, description = "Automation not found.", body = Refusal), (status = 422, description = "Automation payload refused.", body = Refusal), (status = 500, description = "Host failure.", body = Refusal)))]
pub fn preview_automation<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    roles: &[&str],
    request: &AutomationPreviewRequest,
    access: &dyn ObjectAccess,
    now: LogicalTime,
) -> Result<AutomationPreviewResponse, Refusal> {
    authorize(host, scope, actor, Operation::ReadDefinitions)?;
    let (canonical, payload) = resolve_preview_payload(host, roles, request)?;
    let (_epoch, actor) = collection(host, scope, actor, &payload.object_type, access)?;
    let scan = scan_listed(
        host,
        access,
        &actor,
        &payload.object_type,
        (None, MAX_AUTOMATION_SCAN, MAX_AUTOMATION_SCAN),
    )?;
    let (action_id_field, now_ms, commit_times) = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        let (field, _) = action_meta(
            host,
            &reader,
            &payload.object_type,
            payload.then.action_id(),
        )?;
        let now_ms = now.as_millis().max(cell.now().as_millis());
        let times = if matches!(payload.when, AutomationWhen::Stale { .. }) {
            let mut map = BTreeMap::new();
            for listed in scan.listed.iter().filter(|l| l.readable) {
                if let Some(ts) = record_last_commit_ms(&reader, &listed.row) {
                    map.insert(listed.row.id.clone(), ts);
                }
            }
            map
        } else {
            BTreeMap::new()
        };
        (field, now_ms, times)
    };
    let mut scanned = 0_u64;
    let mut hit_count = 0_u64;
    let mut matches = Vec::new();
    for listed in &scan.listed {
        if !listed.readable {
            continue;
        }
        scanned = scanned.saturating_add(1);
        let Some((reason, stale_for_ms)) = eval_record_match(
            &payload,
            &listed.row,
            commit_times.get(&listed.row.id).copied(),
            now_ms,
        ) else {
            continue;
        };
        hit_count = hit_count.saturating_add(1);
        if matches.len() < MAX_PREVIEW_MATCHES {
            let input = build_action_input(&payload.then, &action_id_field, &listed.row.id)?;
            matches.push(AutomationPreviewMatch {
                object_type: listed.row.object_type.clone(),
                id: listed.row.id.clone(),
                version: listed.row.version,
                values: listed.row.values.clone(),
                reason,
                stale_for_ms,
                proposed_action: AutomationProposedAction {
                    action_id: payload.then.action_id().to_owned(),
                    input,
                },
            });
        }
    }
    let restatement = crate::automation_definitions::restate(&canonical)
        .unwrap_or_else(|_| payload.title.clone());
    Ok(AutomationPreviewResponse {
        object_type: payload.object_type,
        scanned_count: scanned,
        matched_count: hit_count,
        approximate: !scan.exhausted
            || usize::try_from(hit_count).unwrap_or(usize::MAX) > MAX_PREVIEW_MATCHES,
        restatement,
        matches,
    })
}

/// `POST /v1/automations/{id}:run`: evaluate an installed automation against
/// current readable records and fire proposals or governed action calls.
///
/// # Errors
/// 403 when unauthorized; 404 when unknown or retired; 500 on host failure.
#[utoipa::path(post, path = "/v1/automations/{id}:run", params(("id" = String, Path, description = "Automation definition id."), ScopeQuery), responses((status = 200, description = "Evaluation results and recorded run entries.", body = AutomationRunResponse), (status = 400, description = "Invalid request.", body = Refusal), (status = 403, description = "Scope or authority denied.", body = Refusal), (status = 404, description = "Automation not found.", body = Refusal), (status = 500, description = "Host failure.", body = Refusal)))]
pub fn run_automation<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    id: &str,
    access: &dyn ObjectAccess,
    disclosure: &dyn QuestionRuntime<E>,
    now: LogicalTime,
) -> Result<AutomationRunResponse, Refusal> {
    authorize(host, scope, actor, Operation::WriteDefinitions)?;
    let (revision, payload) = load_active_automation(host, id)?;
    if !payload.enabled {
        return Ok(AutomationRunResponse {
            id: id.to_owned(),
            version: revision,
            enabled: false,
            scanned_count: 0,
            matched_count: 0,
            proposed: 0,
            committed: 0,
            refused: 0,
            skipped: 0,
            runs: Vec::new(),
        });
    }
    let (epoch, verified_actor) = collection(host, scope, actor, &payload.object_type, access)?;
    let scan = scan_listed(
        host,
        access,
        &verified_actor,
        &payload.object_type,
        (None, MAX_AUTOMATION_SCAN, MAX_AUTOMATION_SCAN),
    )?;
    let (action_id_field, action_title, now_ms, commit_times) = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        let (field, title) = action_meta(
            host,
            &reader,
            &payload.object_type,
            payload.then.action_id(),
        )?;
        let now_ms = now.as_millis().max(cell.now().as_millis());
        let times = if matches!(payload.when, AutomationWhen::Stale { .. }) {
            let mut map = BTreeMap::new();
            for listed in scan.listed.iter().filter(|l| l.readable) {
                if let Some(ts) = record_last_commit_ms(&reader, &listed.row) {
                    map.insert(listed.row.id.clone(), ts);
                }
            }
            map
        } else {
            BTreeMap::new()
        };
        (field, title, now_ms, times)
    };
    // Release the definitions read guard before firing actions: `call_action_bound`
    // acquires its own `definition_read()` epoch and recursive read locking on the
    // same thread can deadlock if a definition write queues in between.
    drop(epoch);
    let mut scanned = 0_u64;
    let mut matched = 0_u64;
    let mut runs = Vec::new();
    for listed in &scan.listed {
        if !listed.readable {
            continue;
        }
        scanned = scanned.saturating_add(1);
        if eval_record_match(
            &payload,
            &listed.row,
            commit_times.get(&listed.row.id).copied(),
            now_ms,
        )
        .is_none()
        {
            continue;
        }
        matched = matched.saturating_add(1);
        let run = fire_for_record(
            host,
            &FireTarget {
                automation_id: id,
                revision,
                payload: &payload,
                trigger: "manual",
                action_id_field: &action_id_field,
                action_title: &action_title,
                record: &listed.row,
            },
            Some(&verified_actor),
            Some(disclosure),
            LogicalTime::from_millis(now_ms),
        )?;
        runs.push(run);
    }
    let (proposed, committed, refused, skipped) = summarize_runs(&runs);
    Ok(AutomationRunResponse {
        id: id.to_owned(),
        version: revision,
        enabled: true,
        scanned_count: scanned,
        matched_count: matched,
        proposed,
        committed,
        refused,
        skipped,
        runs,
    })
}

fn summarize_runs(runs: &[AutomationRunRecord]) -> (u64, u64, u64, u64) {
    let (mut proposed, mut committed, mut refused, mut skipped) = (0_u64, 0_u64, 0_u64, 0_u64);
    for run in runs {
        match run.outcome {
            AutomationRunOutcome::Proposed => proposed = proposed.saturating_add(1),
            AutomationRunOutcome::Committed => committed = committed.saturating_add(1),
            AutomationRunOutcome::Refused => refused = refused.saturating_add(1),
            AutomationRunOutcome::Skipped => skipped = skipped.saturating_add(1),
        }
    }
    (proposed, committed, refused, skipped)
}

/// `GET /v1/automations/{id}/runs`: durable run history for one automation,
/// newest first, including proposals, commits, and refusals.
///
/// # Errors
/// 403 when unauthorized; 404 when unknown; 400 on invalid limit; 500 on host failure.
#[utoipa::path(get, path = "/v1/automations/{id}/runs", params(("id" = String, Path, description = "Automation definition id."), AutomationRunsQuery), responses((status = 200, description = "Recent automation run outcomes.", body = AutomationRunsPage), (status = 400, description = "Invalid request.", body = Refusal), (status = 403, description = "Scope or authority denied.", body = Refusal), (status = 404, description = "Automation not found.", body = Refusal), (status = 500, description = "Host failure.", body = Refusal)))]
pub fn list_automation_runs<E: SqlEngine>(
    host: &DevHost<E>,
    query: &AutomationRunsQuery,
    actor: Option<&str>,
    id: &str,
    access: &dyn ObjectAccess,
) -> Result<AutomationRunsPage, Refusal> {
    let scope = ScopeQuery {
        org_id: query.org_id.clone(),
        environment_id: query.environment_id.clone(),
    };
    authorize(host, &scope, actor, Operation::ReadDefinitions)?;
    let limit = usize::try_from(query.limit.unwrap_or(50)).unwrap_or(MAX_RUN_HISTORY);
    if limit == 0 || limit > MAX_RUN_HISTORY {
        return Err(Refusal::invalid(format!(
            "limit must be 1 to {MAX_RUN_HISTORY}"
        )));
    }
    let object_type = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        let record = reader
            .retained_definition(id, None)
            .map_err(Refusal::host)?
            .filter(|rec| rec.definition.kind == "automation")
            .ok_or_else(not_found)?;
        let payload: AutomationPayload =
            serde_json::from_str(&record.definition.payload).map_err(Refusal::host)?;
        payload.object_type
    };
    let (_epoch, verified_actor) = collection(host, &scope, actor, &object_type, access)?;
    let raw_items = {
        let cell = host.cell()?;
        let head_seq = cell.store().verified_head().map_err(Refusal::host)?.seq;
        let mut found = read_runs_from_ledger(&cell.store().read(), id, head_seq)?;
        found.reverse();
        found
    };
    // Enforce collection and row-level authorization for every recorded run
    // subject (including prior revisions that targeted another object type).
    let mut items = Vec::with_capacity(limit.min(raw_items.len()));
    let mut subjects: BTreeMap<(String, String), Option<ObjectRecord>> = BTreeMap::new();
    {
        let cell = host.cell()?;
        let reader = cell.store().read();
        for run in &raw_items {
            let key = (run.object_type.clone(), run.object_id.clone());
            if let std::collections::btree_map::Entry::Vacant(slot) = subjects.entry(key) {
                let row = reader
                    .object(MAIN_WORLD, &run.object_type, &run.object_id)
                    .map_err(Refusal::host)?;
                slot.insert(row);
            }
        }
    }
    let scope_key = host.scope().key();
    for run in raw_items {
        if items.len() >= limit {
            break;
        }
        let col_ok = mapped(
            host.scope(),
            access,
            &verified_actor,
            ReadCapability::Population,
            &run.object_type,
            None,
        )
        .is_ok_and(|req| {
            host.read_permitted(&ReadRequest {
                capability: ReadCapability::Population,
                access: &req,
            })
        });
        if !col_ok {
            continue;
        }
        let subject_row = subjects
            .get(&(run.object_type.clone(), run.object_id.clone()))
            .and_then(Option::as_ref);
        let allowed = if let Some(row) = subject_row {
            row_readable(host, access, &verified_actor, row)
        } else {
            let fallback = ObjectRecord {
                object_type: run.object_type.clone(),
                id: run.object_id.clone(),
                scope: scope_key.clone(),
                version: run.object_version,
                values: BTreeMap::new(),
            };
            row_readable(host, access, &verified_actor, &fallback)
        };
        if allowed {
            items.push(run);
        }
    }
    Ok(AutomationRunsPage {
        id: id.to_owned(),
        items,
    })
}

fn resolve_preview_payload<E: SqlEngine>(
    host: &DevHost<E>,
    roles: &[&str],
    request: &AutomationPreviewRequest,
) -> Result<(String, AutomationPayload), Refusal> {
    match (&request.id, &request.payload) {
        (Some(id), None) => {
            let (_rev, payload) = load_active_automation(host, id)?;
            let canonical = serde_json::to_string(&payload).map_err(Refusal::host)?;
            Ok((canonical, payload))
        }
        (None, Some(value)) => {
            let mut findings: Vec<RefusalDiagnostic> = Vec::new();
            crate::automation_definitions::check(host, value, roles, &mut findings)?;
            if findings.iter().any(|f| f.severity == "error") {
                let mut refusal = Refusal::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    crate::definition_routes::codes::REFUSED,
                    "the automation definition was refused",
                    "correct the reported findings and preview again",
                );
                refusal.diagnostics = findings;
                return Err(refusal);
            }
            let canonical = serde_json::to_string(value).map_err(Refusal::host)?;
            let payload: AutomationPayload =
                serde_json::from_value(value.clone()).map_err(Refusal::host)?;
            Ok((canonical, payload))
        }
        _ => Err(Refusal::invalid(
            "provide exactly one of id or payload to preview an automation",
        )),
    }
}

fn load_active_automation<E: SqlEngine>(
    host: &DevHost<E>,
    id: &str,
) -> Result<(u64, AutomationPayload), Refusal> {
    let cell = host.cell()?;
    let reader = cell.store().read();
    let record = reader
        .retained_definition(id, None)
        .map_err(Refusal::host)?
        .filter(|rec| {
            rec.definition.kind == "automation"
                && rec.definition.lifecycle == RetainedLifecycle::Active
        })
        .ok_or_else(not_found)?;
    let payload: AutomationPayload =
        serde_json::from_str(&record.definition.payload).map_err(Refusal::host)?;
    Ok((record.definition.revision, payload))
}

pub(crate) fn eval_record_match(
    payload: &AutomationPayload,
    record: &ObjectRecord,
    last_commit_ms: Option<u64>,
    now_ms: u64,
) -> Option<(String, Option<u64>)> {
    let (when_reason, stale_for_ms) = match &payload.when {
        AutomationWhen::Event { event, action_id } => {
            let text = match action_id {
                Some(action) => format!("On action {action}"),
                None => format!("On {event}"),
            };
            (text, None)
        }
        AutomationWhen::LifecycleEntered { property, state } => {
            if record.values.get(property).and_then(RuntimeValue::as_str) != Some(state.as_str()) {
                return None;
            }
            (format!("{property} is {state}"), None)
        }
        AutomationWhen::Stale {
            after_ms,
            property,
            state,
        } => {
            if let (Some(prop), Some(wanted)) = (property, state)
                && record.values.get(prop).and_then(RuntimeValue::as_str) != Some(wanted.as_str())
            {
                return None;
            }
            let last = last_commit_ms?;
            let elapsed = now_ms.saturating_sub(last);
            if elapsed < *after_ms {
                return None;
            }
            let prefix = match (property, state) {
                (Some(p), Some(s)) => format!("{p} is {s} and unchanged"),
                _ => "unchanged".to_owned(),
            };
            (format!("{prefix} for {elapsed} ms"), Some(elapsed))
        }
    };
    if !guard_passes(payload, record) {
        return None;
    }
    let reason = match &payload.guard {
        Some(guard) => format!("{when_reason}; guard `{}` is true", guard.expression.trim()),
        None => when_reason,
    };
    Some((reason, stale_for_ms))
}

pub(crate) fn guard_passes(payload: &AutomationPayload, record: &ObjectRecord) -> bool {
    let Some(guard) = &payload.guard else {
        return true;
    };
    norn_checker::guard_expr::evaluate_record_guard(guard.expression.trim(), &record.values)
        .unwrap_or(false)
}

pub(crate) fn record_last_commit_ms<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    record: &ObjectRecord,
) -> Option<u64> {
    reader
        .receipt_for_fact_version(MAIN_WORLD, &record.object_type, &record.id, record.version)
        .ok()
        .map(|receipt| receipt.body.logical_time)
}

fn not_found() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        crate::definition_routes::codes::NOT_FOUND,
        "no such automation",
        "no active automation with this id exists in this cell",
    )
}
