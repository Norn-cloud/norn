//! DECISIONS-1: the decisions flow runs wait on, and deciding them.
//!
//! A flow lowers "ask a person" to a perform of `norn.decision@1`; the cell
//! parks that command (PARKED-1) and the run waits in `awaiting_perform_<step>`.
//! The parked command is the open decision: its request (question, options,
//! risk, decider roles, expiry) is the step's input. Its id is opaque and
//! bound to the command's generation.
//!
//! `GET /v1/decisions?state=pending` lists the open decisions the caller may
//! decide, soonest deadline first. `POST /v1/decisions/{id}:decide` records
//! the caller's choice: the cell checks the decider (the host operation
//! [`crate::authz::Operation::Decide`] over the request's declared roles and law), the
//! deadline, the choice and the note, then records the result and commits the
//! run's result turn with the decision's resolution in one commit
//! (DECISION-LEDGER-1). A decider adds a condition; never authority: the run's
//! later steps keep acting under the run starter's authority. A caller who may
//! not decide sees exactly what a caller asking for an absent decision sees.
//!
//! The same routes list and decide the approvals parked action calls wait on
//! (APPROVAL-1, [`crate::approvals`]): such a decision carries the call
//! (`call`), and its requester never decides it.

use std::collections::BTreeMap;

use http::StatusCode;
use norn_cell::decision_ledger::{
    DecisionRequest, DecisionResolved, DecisionRow, MAX_EXPIRES_IN_MS, OptionEffect, Resolution,
};
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::RunStarter;
use norn_cell::read::PendingCommand;
use norn_cell::{CellStore, MAIN_WORLD};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::flow_turn::StepOutcome;
use norn_kernel::{CommandId, Generation, LogicalTime};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::approvals::{CallState, HeldCall};
use crate::authz::{AccessRequest, Operation, ReadCapability, ReadRequest};
pub use crate::decision_refusal::DECISION_REFUSAL_ACTION;
use crate::decision_refusal::check_answer;
use crate::flow_definitions::DECISION_FUNCTION;
use crate::flow_driver::{AsStarter, Delivery, result_turn};
use crate::host::{ActionCell, DevHost};
use crate::question_routes::{QuestionRuntime, handle_key, world_values};
use crate::refusal::Refusal;
use crate::run_routes::{FlowView, InstalledFlow, ReadAccess, installed_flow};
use crate::scope::ScopeQuery;

/// The most open decisions a list request examines; past it, `total` is a
/// lower bound (`totalCapped`).
pub const MAX_DECISION_SCAN: usize = 500;

/// The largest page.
pub const MAX_DECISION_PAGE: u32 = 200;

/// The longest idempotency key, in bytes.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

/// Stable `decision.*` refusal codes.
pub mod decision_codes {
    /// No open decision with this id that the caller may decide.
    pub const NOT_FOUND: &str = "decision.not_found";
    /// Someone resolved it first.
    pub const RESOLVED: &str = "decision.resolved";
    /// Its deadline passed.
    pub const EXPIRED: &str = "decision.expired";
    /// The decider law is not satisfied for the caller.
    pub const LAW_NOT_SATISFIED: &str = "decision.law_not_satisfied";
    /// `choice` names no option.
    pub const UNKNOWN_CHOICE: &str = "decision.unknown_choice";
    /// The note is longer than 500 characters.
    pub const NOTE_TOO_LONG: &str = "decision.note_too_long";
    /// The idempotency key was used for a different choice or note.
    pub const IDEMPOTENCY_CONFLICT: &str = "decision.idempotency_conflict";
}

/// How much is at stake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionRisk {
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
}

/// What choosing an option does: exactly one of an action or a binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionOptionEffect {
    /// The action the option starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// The binding the option sets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
}

/// One option a decider may choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionOptionDocument {
    /// The option id `choice` names.
    pub id: String,
    /// What the decider reads.
    pub label: String,
    /// How far it reaches, in the decider's words.
    pub scope: String,
    /// What it does.
    pub effect: DecisionOptionEffect,
}

/// The run a decision belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRun {
    /// The run id.
    pub id: String,
    /// Its generation.
    pub generation: u64,
    /// Its lifecycle state.
    pub state: Option<String>,
}

/// The flow a decision belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionFlow {
    /// The flow's `decide` rule artifact id.
    pub rule_id: String,
    /// The rule version the run is pinned to.
    pub version: String,
    /// The flow's display title.
    pub title: Option<String>,
}

/// The step that asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionStep {
    /// The step id.
    pub id: String,
    /// Its display label.
    pub label: Option<String>,
}

/// What the decision is about. Empty in this version.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionContext {
    /// The objects the decision concerns.
    pub subjects: Vec<DecisionSubject>,
}

/// One object a decision concerns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionSubject {
    /// Its object type.
    pub object_type: String,
    /// Its id.
    pub id: String,
}

/// One open decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionDocument {
    /// The opaque decision id.
    pub id: String,
    /// The run waiting on it.
    pub run: DecisionRun,
    /// The run's flow.
    pub flow: DecisionFlow,
    /// The step that asks.
    pub step: DecisionStep,
    /// The question.
    pub question: String,
    /// How much is at stake.
    pub risk: DecisionRisk,
    /// The options.
    pub options: Vec<DecisionOptionDocument>,
    /// An agent's recommended option id; a recommendation, never a decision.
    pub agent_pick: Option<String>,
    /// When the run asked (logical milliseconds).
    pub requested_at_ms: u64,
    /// When the decision expires (logical milliseconds), at most seven days
    /// after it was asked.
    pub expires_at_ms: u64,
    /// What it is about.
    pub context: DecisionContext,
    /// Reserved; always `null` in this version.
    #[schema(value_type = Option<std::collections::BTreeMap<String, serde_json::Value>>)]
    pub input_schema: Option<serde_json::Value>,
    /// For the approval of a parked action call (APPROVAL-1), the call
    /// waiting on it; absent for a flow's ask.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<DecisionCall>,
}

/// The parked action call an approval decides (APPROVAL-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionCall {
    /// The action called.
    pub action: String,
    /// The approval definition that gates it.
    pub definition: String,
    /// The call's admitted input, as its requester sent it.
    #[schema(value_type = BTreeMap<String, serde_json::Value>)]
    pub input: RuntimeValue,
    /// Optional provenance trace context carried by the parked call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub trace: Option<crate::actions::TraceContext>,
}

/// One page of open decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionsPage {
    /// The decisions, soonest deadline first, ties to the oldest request.
    pub items: Vec<DecisionDocument>,
    /// How many open decisions the caller may decide.
    pub total: u64,
    /// `total` is a lower bound: more were not examined.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub total_capped: bool,
    /// Pass as `cursor` for the next page; `null` on the last page.
    pub next_cursor: Option<String>,
    /// The cell's clock when the page was read (logical milliseconds).
    pub server_now_ms: u64,
}

/// `GET /v1/decisions` query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct DecisionsQuery {
    /// Organization (required).
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Environment (required).
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
    /// `pending` (required): only open decisions are listed.
    pub state: Option<String>,
    /// Only decisions of runs of this flow (its definition id).
    pub flow: Option<String>,
    /// The previous page's `nextCursor`.
    pub cursor: Option<String>,
    /// Page size, 0 to 200 (default 50); 0 returns only the count.
    pub limit: Option<u32>,
}

/// `POST /v1/decisions/{id}:decide` body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecideRequest {
    /// One of the decision's option ids.
    pub choice: String,
    /// A note, at most 500 characters.
    #[serde(default)]
    pub note: Option<String>,
    /// The caller's retry key: the same key and body replays the decision.
    pub idempotency_key: String,
}

/// The decision as decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecidedDecision {
    /// The decision id.
    pub id: String,
    /// The chosen option id.
    pub choice: String,
    /// The note.
    pub note: Option<String>,
    /// Who decided, in display words, never an id.
    pub decided_by: String,
    /// When (logical milliseconds).
    pub decided_at_ms: u64,
}

/// The commit that recorded the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecidedReceipt {
    /// The receipt hash, or its keyed handle (D53).
    pub receipt_hash: String,
    /// The commit.
    pub commit_seq: u64,
    /// When it was recorded (logical milliseconds).
    pub recorded_at_ms: u64,
}

/// The run after the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecidedRun {
    /// The run id.
    pub id: String,
    /// Its lifecycle state.
    pub state: Option<String>,
}

/// Whether this call decided, or replayed an identical earlier call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecideOutcome {
    /// This call decided.
    Decided,
    /// The same key and body decided earlier.
    Replayed,
}

/// `POST /v1/decisions/{id}:decide` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Decided {
    /// Decided now, or replayed.
    pub outcome: DecideOutcome,
    /// The decision.
    pub decision: DecidedDecision,
    /// Its commit.
    pub receipt: DecidedReceipt,
    /// The run, or for an approval of a direct action call the parked call's
    /// id with no state.
    pub run: DecidedRun,
    /// For an approved action call (APPROVAL-1), where it stands after this
    /// call (APPROVAL-RECOVERY-1): on a replay its recorded outcome, or, while
    /// it is approved and not yet executed, what running it now answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<DecidedCall>,
}

/// Where an approved action call stands (APPROVAL-RECOVERY-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecidedCallState {
    /// Exactly the approved call committed.
    Committed,
    /// It was refused when it ran (`code`), for good: its approval never
    /// runs it again.
    Refused,
    /// Approved, not yet executed: no request of the requester's has run
    /// it yet, the host stopped before it committed, or its try could not
    /// commit it (`code` says why). Only the requester's own retry runs it,
    /// once, under the requester's current authority.
    NotExecuted,
}

/// What the approved call is now (APPROVAL-1, APPROVAL-RECOVERY-1). The
/// approval stands either way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecidedCall {
    /// Whether the call committed.
    pub committed: bool,
    /// The refusal code when it did not: its recorded refusal, or why this
    /// try could not run it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Where it stands.
    pub state: DecidedCallState,
}

impl DecidedCall {
    fn of(state: &CallState, tried: Option<String>) -> Self {
        match state {
            CallState::Committed(_) => Self {
                committed: true,
                code: None,
                state: DecidedCallState::Committed,
            },
            CallState::Refused(code) => Self {
                committed: false,
                code: Some(code.clone()),
                state: DecidedCallState::Refused,
            },
            CallState::NotExecuted => Self {
                committed: false,
                code: tried,
                state: DecidedCallState::NotExecuted,
            },
        }
    }
}

/// Who resolved a decision first (409 `decision.resolved`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionResolution {
    /// How it was resolved.
    pub resolution: String,
    /// Who, in display words, never an id.
    pub by: Option<String>,
    /// When (logical milliseconds).
    pub at_ms: u64,
    /// The chosen option, only for a reader who may read the run.
    pub choice: Option<String>,
    /// For an approval of a parked action call, where the call stands
    /// (APPROVAL-RECOVERY-1), read only: nothing runs. Present for the
    /// approver and for a reader of the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<DecidedCall>,
}

/// A refused decide call: the shared refusal, and on `decision.resolved` who
/// resolved the decision first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecideRefusal {
    /// The refusal.
    #[serde(flatten)]
    pub refusal: Refusal,
    /// Who resolved the decision first; on `decision.resolved` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<DecisionResolution>,
}

impl From<Refusal> for Box<DecideRefusal> {
    fn from(refusal: Refusal) -> Self {
        Box::new(DecideRefusal {
            refusal,
            resolution: None,
        })
    }
}

/// An open decision as the cell holds it.
pub(crate) struct OpenDecision {
    pub(crate) id: String,
    pub(crate) command: CommandId,
    pub(crate) generation: Generation,
    pub(crate) run_id: String,
    pub(crate) step: String,
    pub(crate) attempt: RuntimeValue,
    pub(crate) request: DecisionRequest,
    pub(crate) requested_at_ms: u64,
    pub(crate) expires_at_ms: u64,
    /// The parked call of an approval-gated action this decision approves
    /// (APPROVAL-1), when it is one.
    pub(crate) call: Option<HeldCall>,
}

/// The one refusal for an absent decision, and for a caller who may not
/// decide it.
#[must_use]
pub fn not_found() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        decision_codes::NOT_FOUND,
        "no such decision",
        "no open decision with this id is waiting on the caller",
    )
}

/// The decision id of a parked command at a generation: opaque, and bound to
/// the generation, so a re-issued command is a new decision.
///
/// # Errors
/// Canonical encoding failed.
pub(crate) fn decision_id(command_id: &str, generation: u64) -> Result<String, Refusal> {
    let digest =
        norn_ir::canonical::domain_hash_hex(b"norn.decision.id/1\0", &(command_id, generation))
            .map_err(Refusal::host)?;
    Ok(format!("dec_{}", digest.get(..32).unwrap_or(&digest)))
}

fn text<'v>(value: &'v RuntimeValue, member: &str) -> Option<&'v str> {
    value.get(member).and_then(RuntimeValue::as_str)
}

/// The open decision a parked command is, or `None` for a parked command
/// that is not a flow's well-formed ask or a well-formed parked call (it
/// never lists and never decides).
pub(crate) fn open_decision<E: SqlEngine>(
    store: &CellStore<E>,
    pending: &PendingCommand,
) -> Result<Option<OpenDecision>, Refusal> {
    if pending.result.is_some() {
        // Already answered; the consuming commit is pending.
        return Ok(None);
    }
    parked_decision(store, pending)
}

/// The decision a parked command is, whether or not a result is recorded
/// for it yet; `None` for a parked command that is not a decision.
fn parked_decision<E: SqlEngine>(
    store: &CellStore<E>,
    pending: &PendingCommand,
) -> Result<Option<OpenDecision>, Refusal> {
    let Some(payload) = pending
        .command
        .command
        .get("payload")
        .filter(|payload| text(payload, "function") == Some(DECISION_FUNCTION))
    else {
        return Ok(None);
    };
    let (Some(run_id), Some(step), Some(attempt)) = (
        text(payload, "runId"),
        text(payload, "step"),
        payload.get("attempt"),
    ) else {
        return Ok(None);
    };
    let Some(input) = payload.get("input").and_then(|input| input.get(step)) else {
        return Ok(None);
    };
    let Ok(bytes) = input.canonical_json() else {
        return Ok(None);
    };
    let Ok(request) = norn_cell::decision_ledger::parse_request(&bytes) else {
        return Ok(None);
    };
    let call = match payload.get("approval") {
        None => None,
        Some(approval) => match crate::approvals::held_call(approval) {
            Some(call) => Some(call),
            None => return Ok(None),
        },
    };
    let requested_at_ms = store
        .read()
        .receipt_at(pending.commit_seq)
        .map_err(Refusal::host)?
        .map(|receipt| receipt.body.logical_time)
        .ok_or_else(|| Refusal::host("a parked command's commit has no receipt"))?;
    let wait = request
        .expires_in_ms
        .unwrap_or(MAX_EXPIRES_IN_MS)
        .min(MAX_EXPIRES_IN_MS);
    Ok(Some(OpenDecision {
        id: decision_id(&pending.command.command_id, pending.command.generation)?,
        command: CommandId(pending.command.command_id.clone()),
        generation: Generation(pending.command.generation),
        run_id: run_id.to_owned(),
        step: step.to_owned(),
        attempt: attempt.clone(),
        request,
        requested_at_ms,
        expires_at_ms: requested_at_ms.saturating_add(wait),
        call,
    }))
}

/// Every open decision, oldest command first.
pub(crate) fn open_decisions<E: SqlEngine>(
    store: &CellStore<E>,
) -> Result<Vec<OpenDecision>, Refusal> {
    let mut open = Vec::new();
    for pending in store.read().parked_commands().map_err(Refusal::host)? {
        if let Some(decision) = open_decision(store, &pending)? {
            open.push(decision);
        }
    }
    Ok(open)
}

/// The typed failure an expired decision delivers to its step
/// (`norn.decision@1`'s declared `Expired` error).
fn expired_failure() -> RuntimeObject {
    BTreeMap::from([(
        "_tag".to_owned(),
        RuntimeValue::String("Expired".to_owned()),
    )])
}

/// DECISION-TERMINAL-1: every decision whose expiry was recorded as its
/// parked result but whose resolving commit never landed (it failed, or the
/// host stopped in between). Such a decision is no longer open, so nothing
/// lists or decides it; the host's wake resumes exactly the recorded result
/// ([`expire`]) until the resolution commits.
pub(crate) fn stalled_expiries<E: SqlEngine>(
    store: &CellStore<E>,
) -> Result<Vec<OpenDecision>, Refusal> {
    let expired = RuntimeValue::Object(expired_failure());
    let mut stalled = Vec::new();
    for pending in store.read().parked_commands().map_err(Refusal::host)? {
        if pending.result.as_ref() != Some(&expired) {
            continue;
        }
        if let Some(decision) = parked_decision(store, &pending)? {
            stalled.push(decision);
        }
    }
    Ok(stalled)
}

/// Whether the parked command of `decision` holds exactly the expiry as its
/// recorded result, awaiting the commit that consumes it.
fn expiry_recorded<E: SqlEngine>(
    store: &CellStore<E>,
    decision: &OpenDecision,
) -> Result<bool, Refusal> {
    Ok(store
        .read()
        .pending_result(&decision.command.0, decision.generation.0)
        .map_err(Refusal::host)?
        .and_then(|pending| pending.result)
        == Some(RuntimeValue::Object(expired_failure())))
}

/// Whether `actor` may decide: the host decides [`Operation::Decide`] over
/// the request's declared roles and law.
fn may_decide<E: SqlEngine>(host: &DevHost<E>, actor: &str, decision: &OpenDecision) -> bool {
    let request = &decision.request;
    if decision
        .call
        .as_ref()
        .is_some_and(|call| !crate::approval_identity::allows_decider(host, actor, call))
    {
        return false;
    }
    host.authorizer
        .authorize(&AccessRequest {
            scope: host.scope(),
            actor: Some(actor),
            operation: Operation::Decide {
                roles: request.decider.roles.clone(),
                law: request.decider.law.clone(),
            },
        })
        .is_ok()
}

fn risk(request: &DecisionRequest) -> DecisionRisk {
    match request.risk {
        norn_cell::decision_ledger::Risk::Low => DecisionRisk::Low,
        norn_cell::decision_ledger::Risk::Medium => DecisionRisk::Medium,
        norn_cell::decision_ledger::Risk::High => DecisionRisk::High,
    }
}

fn effect(effect: &OptionEffect) -> DecisionOptionEffect {
    DecisionOptionEffect {
        action: effect.action.clone(),
        binding: effect.binding.clone(),
    }
}

/// The step's label: the flow's label for the step, or for the canvas node
/// it was lowered from (`ask_<node>`).
fn step_label(flow: &FlowView, step: &str) -> Option<String> {
    flow.labels
        .get(step)
        .or_else(|| {
            step.strip_prefix("ask_")
                .and_then(|node| flow.labels.get(node))
        })
        .cloned()
}

fn options(request: &DecisionRequest) -> Vec<DecisionOptionDocument> {
    request
        .options
        .iter()
        .map(|option| DecisionOptionDocument {
            id: option.id.clone(),
            label: option.label.clone(),
            scope: option.scope.clone(),
            effect: effect(&option.effect),
        })
        .collect()
}

/// The run a parked call's step belongs to, when a flow step made the call:
/// the step attempt recorded when it parked (S6b), so an attempt that ended
/// before the decision still names its run; else the parked call's own id,
/// which is not a run.
fn call_run<E: SqlEngine>(
    store: &CellStore<E>,
    decision: &OpenDecision,
    call: &HeldCall,
) -> Result<DecisionRun, Refusal> {
    let run_id = match &call.call.attempt {
        Some(attempt) => Some(attempt.run_id.clone()),
        // Parked before attempts were recorded.
        None => crate::flow_driver::run_waiting_on(store, &call.call.idempotency_key)?,
    };
    let Some(run_id) = run_id else {
        return Ok(DecisionRun {
            id: decision.run_id.clone(),
            generation: decision.generation.0,
            state: None,
        });
    };
    let row = store
        .read()
        .flow_run(MAIN_WORLD, &run_id)
        .map_err(Refusal::host)?;
    Ok(DecisionRun {
        id: run_id,
        generation: row
            .as_ref()
            .map_or(decision.generation.0, |row| row.generation),
        state: row.and_then(|row| row.state),
    })
}

/// An approval's document: the parked call in place of a flow's ask.
fn call_document<E: SqlEngine>(
    store: &CellStore<E>,
    decision: &OpenDecision,
    call: &HeldCall,
) -> Result<DecisionDocument, Refusal> {
    Ok(DecisionDocument {
        id: decision.id.clone(),
        run: call_run(store, decision, call)?,
        flow: DecisionFlow {
            rule_id: call.definition.clone(),
            version: call.revision.to_string(),
            title: None,
        },
        step: DecisionStep {
            id: decision.step.clone(),
            label: None,
        },
        question: decision.request.question.clone(),
        risk: risk(&decision.request),
        options: options(&decision.request),
        agent_pick: decision.request.agent_pick.clone(),
        requested_at_ms: decision.requested_at_ms,
        expires_at_ms: decision.expires_at_ms,
        context: DecisionContext::default(),
        input_schema: None,
        call: Some(DecisionCall {
            action: call.call.action.clone(),
            definition: call.definition.clone(),
            input: RuntimeValue::Object(call.call.input.clone()),
            trace: call.call.trace.clone().map(Into::into),
        }),
    })
}

fn document<E: SqlEngine>(
    store: &CellStore<E>,
    decision: &OpenDecision,
    flows: &mut BTreeMap<String, Option<InstalledFlow>>,
) -> Result<Option<DecisionDocument>, Refusal> {
    if let Some(call) = &decision.call {
        return call_document(store, decision, call).map(Some);
    }
    let Some(run) = store
        .read()
        .flow_run(MAIN_WORLD, &decision.run_id)
        .map_err(Refusal::host)?
    else {
        return Ok(None);
    };
    let flow = match flows.entry(run.flow.id.clone()) {
        std::collections::btree_map::Entry::Occupied(known) => known.into_mut(),
        std::collections::btree_map::Entry::Vacant(slot) => {
            slot.insert(installed_flow(store, &run.flow.id).ok())
        }
    };
    // A decision of a retired flow's run waits, but is not listed.
    let Some(flow) = flow else {
        return Ok(None);
    };
    Ok(Some(DecisionDocument {
        id: decision.id.clone(),
        run: DecisionRun {
            id: run.run_id,
            generation: run.generation,
            state: run.state,
        },
        flow: DecisionFlow {
            rule_id: flow.view.rule_id.clone(),
            version: run.flow.version,
            title: flow.view.title.clone(),
        },
        step: DecisionStep {
            id: decision.step.clone(),
            label: step_label(&flow.view, &decision.step),
        },
        question: decision.request.question.clone(),
        risk: risk(&decision.request),
        options: options(&decision.request),
        agent_pick: decision.request.agent_pick.clone(),
        requested_at_ms: decision.requested_at_ms,
        expires_at_ms: decision.expires_at_ms,
        context: DecisionContext::default(),
        input_schema: None,
        call: None,
    }))
}

/// The cursor after `decision`: its sort key.
fn cursor_of(decision: &DecisionDocument) -> String {
    format!(
        "{}.{}.{}",
        decision.expires_at_ms, decision.requested_at_ms, decision.id
    )
}

fn sort_key(decision: &DecisionDocument) -> (u64, u64, &str) {
    (
        decision.expires_at_ms,
        decision.requested_at_ms,
        decision.id.as_str(),
    )
}

fn parse_cursor(cursor: &str) -> Result<(u64, u64, String), Refusal> {
    let invalid = || Refusal::invalid("cursor is not a cursor this route issued");
    let mut parts = cursor.splitn(3, '.');
    let (Some(expires), Some(requested), Some(id)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    Ok((
        expires.parse().map_err(|_bad| invalid())?,
        requested.parse().map_err(|_bad| invalid())?,
        id.to_owned(),
    ))
}

/// `GET /v1/decisions?state=pending`: the open decisions the caller may
/// decide, soonest deadline first, ties to the oldest request, then id.
///
/// # Errors
/// Missing or foreign scope; a missing caller (one opaque 403); an invalid
/// state, limit or cursor; a host failure.
#[utoipa::path(get, path = "/v1/decisions", params(DecisionsQuery), responses((status = 200, description = "One page of the open decisions the caller may decide.", body = DecisionsPage), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn list_decisions<E: SqlEngine>(
    host: &DevHost<E>,
    query: &DecisionsQuery,
    actor: Option<&str>,
    now: LogicalTime,
) -> Result<DecisionsPage, Refusal> {
    let _epoch = host.definition_read()?;
    host.scope()
        .admit(query.org_id.as_deref(), query.environment_id.as_deref())?;
    let actor = actor
        .filter(|actor| !actor.trim().is_empty())
        .ok_or_else(crate::authz::read_denied)?;
    if query.state.as_deref() != Some("pending") {
        return Err(Refusal::invalid("state must be pending"));
    }
    let limit = query.limit.unwrap_or(50);
    if limit > MAX_DECISION_PAGE {
        return Err(Refusal::invalid("limit must be 0-200"));
    }
    let after = query.cursor.as_deref().map(parse_cursor).transpose()?;
    let cell = host.cell()?;
    let now_ms = now.as_millis().max(cell.now().as_millis());
    let mut flows = BTreeMap::new();
    let mut visible = Vec::new();
    let mut capped = false;
    for decision in open_decisions(cell.store())? {
        if decision.expires_at_ms <= now_ms || !may_decide(host, actor, &decision) {
            continue;
        }
        if visible.len() == MAX_DECISION_SCAN {
            capped = true;
            break;
        }
        let Some(document) = document(cell.store(), &decision, &mut flows)? else {
            continue;
        };
        if query
            .flow
            .as_deref()
            .is_some_and(|flow| document.flow.rule_id != format!("{flow}.flow.rule"))
        {
            continue;
        }
        visible.push(document);
    }
    visible.sort_by(|left, right| sort_key(left).cmp(&sort_key(right)));
    let total = u64::try_from(visible.len()).unwrap_or(u64::MAX);
    let wanted = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut items: Vec<DecisionDocument> = visible
        .into_iter()
        .filter(|document| {
            after.as_ref().is_none_or(|(expires, requested, id)| {
                sort_key(document) > (*expires, *requested, id.as_str())
            })
        })
        .collect();
    let more = items.len() > wanted;
    items.truncate(wanted);
    let next_cursor = if more && wanted > 0 {
        items.last().map(cursor_of)
    } else {
        None
    };
    Ok(DecisionsPage {
        items,
        total,
        total_capped: capped,
        next_cursor,
        server_now_ms: now_ms,
    })
}

/// `GET /v1/decisions:requested` query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct RequestedQuery {
    /// Organization (required).
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Environment (required).
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
    /// Only this decision, in whatever state it stands (the id a parked
    /// call's `202` answer named). Without it, the calls that wait: those
    /// pending a decider and those approved and not yet run.
    pub id: Option<String>,
}

/// Where the caller's parked call stands with its approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestedStatus {
    /// It waits for a decider.
    Pending,
    /// A decider approved it; `call.state` says whether it ran.
    Approved,
    /// A decider rejected it; it never runs.
    Rejected,
    /// No one decided before its deadline; it never runs.
    Expired,
    /// It was withdrawn before anyone decided.
    Withdrawn,
}

/// The parked call itself, exactly as it waits: retrying it means calling
/// `action` with `encodedInput` as the input and `idempotencyKey` as the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestedCall {
    /// The action called.
    pub action: String,
    /// The approval definition that gates it.
    pub definition: String,
    /// The call's admitted input.
    #[schema(value_type = BTreeMap<String, serde_json::Value>)]
    pub input: RuntimeValue,
    /// The same input as canonical JSON text, numbers digit for digit, so a
    /// client that parses JSON into binary floats can still send it back
    /// exactly.
    pub encoded_input: String,
    /// The call's idempotency key.
    pub idempotency_key: String,
    /// Optional provenance trace context carried by the parked call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub trace: Option<crate::actions::TraceContext>,
    /// Once it is approved, where the call stands (APPROVAL-RECOVERY-1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<DecidedCall>,
}

/// One approval-gated call the caller made, as its requester sees it
/// (APPROVAL-1): never a decider's view, and never anyone else's call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestedDecision {
    /// The opaque decision id.
    pub id: String,
    /// The approval's question.
    pub question: String,
    /// How much is at stake.
    pub risk: DecisionRisk,
    /// When the call parked (logical milliseconds).
    pub requested_at_ms: u64,
    /// When the decision expires (logical milliseconds).
    pub expires_at_ms: u64,
    /// Where it stands.
    pub status: RequestedStatus,
    /// When it was resolved (logical milliseconds), once it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at_ms: Option<u64>,
    /// The call.
    pub call: RequestedCall,
    /// The cell offers the caller to run it now: it is approved, not yet
    /// executed, a direct call (a flow step's call runs by retrying its
    /// run), and the caller is its requester. An offer, not a grant: the
    /// retry is decided again under the caller's current authority.
    pub can_run: bool,
}

/// The caller's own approval-gated calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestedPage {
    /// Soonest deadline first, ties to the oldest request, then id.
    pub items: Vec<RequestedDecision>,
    /// More calls were not examined.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub total_capped: bool,
    /// The cell's clock when the page was read (logical milliseconds).
    pub server_now_ms: u64,
}

/// Whether `held` is a direct call `principal` made: exactly the requester
/// its dispatch admits (subject, actor kind and role table,
/// [`crate::approvals::is_requester`]); a flow step's call is its run's,
/// and the run's retry drives it.
fn requested_by<E: SqlEngine>(
    store: &CellStore<E>,
    held: &HeldCall,
    principal: &RunStarter,
) -> Result<bool, Refusal> {
    Ok(
        crate::approvals::is_requester(Some(principal), &held.call.requester)
            && held.call.attempt.is_none()
            && crate::flow_driver::run_waiting_on(store, &held.call.idempotency_key)?.is_none(),
    )
}

fn requested_document(
    decision: &OpenDecision,
    held: &HeldCall,
    status: RequestedStatus,
    resolved_at_ms: Option<u64>,
    state: Option<&CallState>,
) -> Result<RequestedDecision, Refusal> {
    let input = RuntimeValue::Object(held.call.input.clone());
    // The exact spelling the call's approval is granted for (#618).
    let encoded_input = String::from_utf8(
        norn_ir::canonical::canonical_json_exact_of(&input).map_err(Refusal::host)?,
    )
    .map_err(Refusal::host)?;
    Ok(RequestedDecision {
        id: decision.id.clone(),
        question: decision.request.question.clone(),
        risk: risk(&decision.request),
        requested_at_ms: decision.requested_at_ms,
        expires_at_ms: decision.expires_at_ms,
        status,
        resolved_at_ms,
        can_run: status == RequestedStatus::Approved
            && matches!(state, Some(CallState::NotExecuted)),
        call: RequestedCall {
            action: held.call.action.clone(),
            definition: held.definition.clone(),
            input,
            encoded_input,
            idempotency_key: held.call.idempotency_key.clone(),
            trace: held.call.trace.clone().map(Into::into),
            state: state.map(|state| DecidedCall::of(state, None)),
        },
    })
}

/// The parked call `command_id` (at `generation`, decision `decision_id`)
/// as its requester `principal` sees it; `None` when it is not a direct call
/// of theirs, or no longer the call that id names. Its payload is loaded
/// only here, for a row [`StoreReader::parked_calls_of`] already found to be
/// the principal's own.
///
/// [`StoreReader::parked_calls_of`]: norn_cell::read::StoreReader::parked_calls_of
fn requested_call<E: SqlEngine>(
    cell: &ActionCell<E>,
    (command_id, generation, decision_id): (&str, u64, &str),
    principal: &RunStarter,
    now_ms: u64,
) -> Result<Option<RequestedDecision>, Refusal> {
    let store = cell.store();
    let Some(pending) = store
        .read()
        .outbox_entry(command_id)
        .map_err(Refusal::host)?
        .filter(|pending| pending.command.generation == generation)
    else {
        return Ok(None);
    };
    let Some(decision) = parked_decision(store, &pending)? else {
        return Ok(None);
    };
    let Some(held) = decision.call.as_ref() else {
        return Ok(None);
    };
    if decision.id != decision_id || !requested_by(store, held, principal)? {
        return Ok(None);
    }
    if pending.result.is_none() {
        // Its deadline passed and the expiry is not recorded yet: it never
        // runs, whatever records it.
        let status = if decision.expires_at_ms <= now_ms {
            RequestedStatus::Expired
        } else {
            RequestedStatus::Pending
        };
        return requested_document(&decision, held, status, None, None).map(Some);
    }
    let Some(row) = store
        .read()
        .decision_resolution(MAIN_WORLD, decision_id)
        .map_err(Refusal::host)?
        .filter(|row| row.resolved.command_id == command_id)
    else {
        // A result is recorded and its resolving commit is still to come.
        return Ok(None);
    };
    let approved = row.resolved.resolution == Resolution::Decided
        && row.resolved.choice.as_deref() == Some(crate::approvals::APPROVE);
    let status = match row.resolved.resolution {
        Resolution::Decided if approved => RequestedStatus::Approved,
        Resolution::Decided => RequestedStatus::Rejected,
        Resolution::Expired => RequestedStatus::Expired,
        Resolution::Withdrawn => RequestedStatus::Withdrawn,
    };
    let state = if approved {
        let Some(approved_row) = store
            .read()
            .approved_call(MAIN_WORLD, decision_id)
            .map_err(Refusal::host)?
        else {
            return Ok(None);
        };
        let (_held, state) = crate::approvals::approved_state(cell, &approved_row)?;
        // A call approved before approved calls were recorded as direct is
        // offered only when its approval still binds it to a direct call.
        if crate::approvals::binding(cell, &approved_row, held)?
            != crate::approvals::Binding::Direct
        {
            return Ok(None);
        }
        Some(state)
    } else {
        None
    };
    requested_document(
        &decision,
        held,
        status,
        Some(row.resolved_at_ms),
        state.as_ref(),
    )
    .map(Some)
}

/// `GET /v1/decisions:requested`: the caller's own approval-gated calls, as
/// their requester sees them (APPROVAL-1, APPROVAL-RECOVERY-1). A requester
/// never decides their call and never lists it among the decisions waiting
/// on them; this is where they learn that it was approved and retry it
/// (`canRun`), with the exact input and key it parked with. Without `id`:
/// the calls pending a decider before their deadline and those approved and
/// not yet run. With `id`: that decision in any state, or no item when it is
/// not the caller's direct call (the cell does not say which).
///
/// The caller is the host's verified principal, compared as the dispatch
/// compares it (subject, actor kind and role table): the same subject under
/// another kind or role table is someone else, and reads nothing. Its calls
/// are found by that identity before any parked payload is loaded, so
/// another requester's id costs what an id that exists nowhere costs.
///
/// # Errors
/// Missing or foreign scope; a missing caller (one opaque 403); an invalid
/// id; a host failure.
#[utoipa::path(get, path = "/v1/decisions:requested", params(RequestedQuery), responses((status = 200, description = "The caller's own approval-gated calls.", body = RequestedPage), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn requested_decisions<E: SqlEngine>(
    host: &DevHost<E>,
    query: &RequestedQuery,
    actor: Option<&str>,
    now: LogicalTime,
) -> Result<RequestedPage, Refusal> {
    let _epoch = host.definition_read()?;
    host.scope()
        .admit(query.org_id.as_deref(), query.environment_id.as_deref())?;
    let actor = actor
        .filter(|actor| !actor.trim().is_empty())
        .ok_or_else(crate::authz::read_denied)?;
    let wanted = query.id.as_deref();
    if wanted.is_some_and(|id| id.is_empty() || id.len() > MAX_IDEMPOTENCY_KEY_BYTES) {
        return Err(Refusal::invalid("id must be a decision id"));
    }
    let cell = host.cell()?;
    let now_ms = now.as_millis().max(cell.now().as_millis());
    let mut items = Vec::new();
    let mut capped = false;
    // No verified principal: nobody's requester, so nothing is theirs.
    if let Some(principal) = host.authorizer.verified_principal(actor) {
        let own = cell
            .store()
            .read()
            .parked_calls_of(
                MAIN_WORLD,
                (
                    &principal.subject,
                    &principal.actor_kind,
                    &principal.role_table,
                ),
                wanted.is_none(),
            )
            .map_err(Refusal::host)?;
        for (command_id, generation) in own {
            let id = decision_id(&command_id, generation)?;
            if wanted.is_some_and(|wanted| wanted != id) {
                continue;
            }
            let Some(item) =
                requested_call(&cell, (&command_id, generation, &id), &principal, now_ms)?
            else {
                continue;
            };
            // Without an id only the calls that wait: pending a decider
            // before their deadline, or approved and not yet run.
            if wanted.is_none() && !(item.status == RequestedStatus::Pending || item.can_run) {
                continue;
            }
            // The limit counts only the caller's own calls that wait.
            if items.len() == MAX_DECISION_SCAN {
                capped = true;
                break;
            }
            items.push(item);
        }
    }
    items.sort_by(|left, right| {
        (left.expires_at_ms, left.requested_at_ms, left.id.as_str()).cmp(&(
            right.expires_at_ms,
            right.requested_at_ms,
            right.id.as_str(),
        ))
    });
    Ok(RequestedPage {
        items,
        total_capped: capped,
        server_now_ms: now_ms,
    })
}

fn request_hash(body: &DecideRequest) -> Result<String, Refusal> {
    norn_ir::canonical::domain_hash_hex(b"norn.decision.decide/1\0", &(&body.choice, &body.note))
        .map_err(Refusal::host)
}

/// Who resolved, in the caller's words.
fn by(row: &DecisionRow, actor: &str) -> Option<String> {
    row.resolved.decided_by.as_deref().map(|decider| {
        if decider == actor {
            "you".to_owned()
        } else {
            "another decider".to_owned()
        }
    })
}

/// `POST /v1/decisions/{id}:decide`.
///
/// For the approval of a parked action call (APPROVAL-1) the decider is
/// never the call's requester (#618), and no flow turn runs: the answer is
/// recorded, and an approved call then runs through the shared action
/// dispatch only under the requester's current authority ([`AsStarter`]:
/// the host runs it only when it can establish that authority, which a
/// decider's request never does). Otherwise the answer's `call` is
/// `not_executed` and the requester's own authenticated retry runs it
/// (APPROVAL-RECOVERY-1).
///
/// # Errors
/// In this order: 404 `decision.not_found` (also for a caller who may not
/// decide it), 409 `decision.resolved`, 410 `decision.expired`, 403
/// `decision.law_not_satisfied`, 422 `decision.unknown_choice` (`/choice`),
/// `decision.note_too_long` (`/note`) or `request.invalid`, 409
/// `decision.idempotency_conflict`; the flow's or the kernel's refusal of the
/// run's result turn; a host failure.
#[utoipa::path(post, path = "/v1/decisions/{id}:decide", params(ScopeQuery, ("id" = String, Path, description = "The decision id.")), request_body = DecideRequest, responses((status = 200, description = "Decided, or replayed an identical earlier call.", body = Decided), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope denied, or the decider law is not satisfied.", body = Refusal), (status = 404, description = "No such open decision waits on the caller.", body = Refusal), (status = 409, description = "Someone resolved it first, or the key was used for another answer.", body = DecideRefusal), (status = 410, description = "The decision expired.", body = Refusal), (status = 422, description = "The choice or note was refused.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn decide<E: SqlEngine>(
    host: &DevHost<E>,
    request: &DecideCall<'_>,
    access: ReadAccess<'_>,
    disclosure: &dyn QuestionRuntime<E>,
    as_requester: AsStarter<'_>,
) -> Result<Decided, Box<DecideRefusal>> {
    let (mut decided, approved) = decide_held(host, request, access, disclosure)?;
    // The cell and the definitions epoch are released: an approved call
    // still to run goes through the ordinary dispatch as its requester.
    if let Some(decision_id) = approved {
        decided.call = run_approved(host, &decision_id, disclosure, request.now, as_requester)
            .map_err(Box::from)?;
    }
    Ok(decided)
}

/// [`decide`] under the cell: the decision, and the decision whose approved
/// call to answer once the cell is released.
fn decide_held<E: SqlEngine>(
    host: &DevHost<E>,
    request: &DecideCall<'_>,
    access: ReadAccess<'_>,
    disclosure: &dyn QuestionRuntime<E>,
) -> Result<(Decided, Option<String>), Box<DecideRefusal>> {
    let DecideCall {
        scope,
        actor,
        id,
        body,
        now,
    } = *request;
    let _epoch = host.definition_read()?;
    let key = handle_key(disclosure)?;
    let reader = Reader {
        host,
        actor: actor.unwrap_or_default(),
        access,
        disclosure,
        key,
    };
    host.scope()
        .admit(scope.org_id.as_deref(), scope.environment_id.as_deref())?;
    let actor = actor
        .filter(|actor| !actor.trim().is_empty())
        .ok_or_else(not_found)?;
    let mut cell = host.cell()?;
    let open = open_decisions(cell.store())?
        .into_iter()
        .find(|decision| decision.id == id);
    let Some(decision) = open else {
        return resolved_answer(&reader, &cell, id, body);
    };
    if !may_decide(host, actor, &decision) {
        return Err(Box::from(not_found()));
    }
    let now = crate::actions::turn_clock(&mut cell, now)?;
    if now.as_millis() >= decision.expires_at_ms {
        expire(&mut cell, &decision, now)?;
        return Err(Box::from(Refusal::new(
            StatusCode::GONE,
            decision_codes::EXPIRED,
            "the decision expired",
            "its deadline passed before anyone decided",
        )));
    }
    check_answer(host, actor, &cell, &decision, body)?;
    let recorded = match &decision.call {
        None => record(&mut cell, &decision, body, actor, now)?,
        Some(held) => record_call(&mut cell, &decision, held, body, actor)?,
    };
    if !recorded {
        return resolved_answer(&reader, &cell, id, body);
    }
    let decided = answer(&reader, &cell, id, body, DecideOutcome::Decided)?;
    let approved = (decision.call.is_some() && body.choice == crate::approvals::APPROVE)
        .then(|| decision.id.clone());
    Ok((decided, approved))
}

/// APPROVAL-RECOVERY-1: the approved call of `decision_id` as it stands, and
/// while it is approved and not yet executed, run once now through the
/// shared action dispatch under the requester's current authority when the
/// host can establish it (`as_requester`; else nothing runs), and the
/// dispatch lets only exactly the approved call past
/// its gate (APPROVAL-REVISION-1, APPROVAL-PIN-1), recording a refusal that
/// decides it. A committed or refused call is answered as recorded and
/// nothing runs. `None` when the decision approved no call (an approval
/// recorded before approved calls were is backfilled when the cell opens).
///
/// # Errors
/// A host failure reading the call.
fn run_approved<E: SqlEngine>(
    host: &DevHost<E>,
    decision_id: &str,
    disclosure: &dyn QuestionRuntime<E>,
    now: LogicalTime,
    as_requester: AsStarter<'_>,
) -> Result<Option<DecidedCall>, Refusal> {
    let missing = || Refusal::host("an approved call lost its record");
    let (held, command) = {
        let _epoch = host.definition_read()?;
        let cell = host.cell()?;
        let Some((held, state)) = crate::approvals::approved_call(&cell, decision_id)? else {
            return Ok(None);
        };
        if !matches!(state, CallState::NotExecuted) {
            return Ok(Some(DecidedCall::of(&state, None)));
        }
        let command = cell
            .store()
            .read()
            .approved_call(MAIN_WORLD, decision_id)
            .map_err(Refusal::host)?
            .ok_or_else(missing)?
            .command_id;
        (held, command)
    };
    let request = crate::actions::ActionRequest {
        input: RuntimeValue::Object(held.call.input.clone()),
        idempotency_key: held.call.idempotency_key.clone(),
        trace: held.call.trace.clone().map(Into::into),
    };
    // APPROVAL-PIN-1: the requester's pin holds at approval too.
    let query = crate::actions::ActionQuery {
        org_id: Some(host.scope().org_id.clone()),
        environment_id: Some(host.scope().environment_id.clone()),
        version: held.call.definitions_version,
    };
    let mut commit = || {
        // S2: the request's own authenticated principal, as the host's
        // trusted seam verified it for this try; the dispatch compares it
        // with the parked requester under its own hold.
        let principal = host
            .authorizer
            .verified_principal(&held.call.requester.subject);
        crate::actions::call_action_bound(
            host,
            &crate::actions::ActionCall {
                query: &query,
                actor: Some(&held.call.requester.subject),
                principal: principal.as_ref(),
                // S5a: the step attempt the call was parked for, as the
                // trusted flow dispatch recorded it; none for a direct call.
                attempt: held.call.attempt.as_ref(),
                action_id: &held.call.action,
                request: &request,
                now,
            },
            disclosure,
            Some(&command),
        )
    };
    let tried = match as_requester(&held.call.requester, &mut commit) {
        Some(Ok(_answer)) => None,
        Some(Err(refusal)) => Some(refusal.code),
        // The requester's current authority cannot be established on this
        // request (it is not the requester's own): nothing runs, and the call
        // stays approved and not executed for the requester's retry.
        None => None,
    };
    let _epoch = host.definition_read()?;
    let cell = host.cell()?;
    let (_held, state) =
        crate::approvals::approved_call(&cell, decision_id)?.ok_or_else(missing)?;
    Ok(Some(DecidedCall::of(&state, tried)))
}

/// One decide call: scope, the verified caller, the decision and the answer.
#[derive(Debug, Clone, Copy)]
pub struct DecideCall<'a> {
    /// The request's explicit scope.
    pub scope: &'a ScopeQuery,
    /// The verified caller, if any.
    pub actor: Option<&'a str>,
    /// The decision id.
    pub id: &'a str,
    /// The answer.
    pub body: &'a DecideRequest,
    /// The host's clock.
    pub now: LogicalTime,
}

/// Record `actor`'s answer and move the run: the flow's result turn is
/// checked first, then the answer is recorded as the parked command's result
/// and the result turn commits with the decision's resolution. `false` when
/// another call recorded an answer first.
fn record<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    decision: &OpenDecision,
    body: &DecideRequest,
    actor: &str,
    now: LogicalTime,
) -> Result<bool, Refusal> {
    let row = cell
        .store()
        .read()
        .flow_run(MAIN_WORLD, &decision.run_id)
        .map_err(Refusal::host)?
        .ok_or_else(not_found)?;
    let flow = installed_flow(cell.store(), &row.flow.id).map_err(|_retired| not_found())?;
    let mut output: RuntimeObject = BTreeMap::from([(
        "choice".to_owned(),
        RuntimeValue::String(body.choice.clone()),
    )]);
    if let Some(note) = &body.note {
        output.insert("note".to_owned(), RuntimeValue::String(note.clone()));
    }
    let delivery = |outcome| Delivery {
        command: &decision.command,
        generation: decision.generation,
        run_id: &decision.run_id,
        step: &decision.step,
        attempt: decision.attempt.clone(),
        outcome,
    };
    // The flow must accept the answer before anything is recorded.
    result_turn(
        cell,
        &flow,
        delivery(StepOutcome::Output(output.clone())),
        now,
    )?;
    if cell
        .complete_parked(&decision.command, decision.generation, output.clone())
        .map_err(Refusal::host)?
        .is_none()
    {
        return Ok(false);
    }
    let commit = result_turn(cell, &flow, delivery(StepOutcome::Output(output)), now)?;
    let resolved = DecisionResolved {
        decision_id: decision.id.clone(),
        command_id: decision.command.0.clone(),
        generation: decision.generation.0,
        run_id: decision.run_id.clone(),
        step: decision.step.clone(),
        resolution: Resolution::Decided,
        choice: Some(body.choice.clone()),
        note: body.note.clone(),
        decided_by: Some(actor.to_owned()),
        idempotency_key: Some(body.idempotency_key.clone()),
        request_hash: Some(request_hash(body)?),
    };
    cell.commit_decision_resolution(commit, &resolved)
        .map_err(resolution_refused)?;
    Ok(true)
}

fn resolution_refused(error: norn_kernel::CommitError<norn_cell::CellError>) -> Refusal {
    let norn_kernel::CommitError::Host(host_error) = error else {
        // The run changed under the turn: the recorded answer stays
        // pending, and the same call resolves it again.
        return Refusal::new(
            StatusCode::CONFLICT,
            decision_codes::RESOLVED,
            "the run changed while the decision was recorded",
            "decide again with the same idempotencyKey",
        );
    };
    if let norn_cell::CellError::DecisionRefused(code) = host_error {
        return Refusal::new(
            StatusCode::CONFLICT,
            code,
            "the decision was resolved first",
            "another call resolved this decision",
        );
    }
    Refusal::host(host_error)
}

/// Record `actor`'s answer to an approval (APPROVAL-1): the answer is the
/// parked command's result, consumed with the resolution in one commit. No
/// flow turn runs; a step waiting on the call moves when its run is next
/// driven. `false` when another call recorded an answer first.
fn record_call<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    decision: &OpenDecision,
    held: &HeldCall,
    body: &DecideRequest,
    actor: &str,
) -> Result<bool, Refusal> {
    let mut output: RuntimeObject = BTreeMap::from([(
        "choice".to_owned(),
        RuntimeValue::String(body.choice.clone()),
    )]);
    if let Some(note) = &body.note {
        output.insert("note".to_owned(), RuntimeValue::String(note.clone()));
    }
    let resolved = DecisionResolved {
        decision_id: decision.id.clone(),
        command_id: decision.command.0.clone(),
        generation: decision.generation.0,
        run_id: call_run(cell.store(), decision, held)?.id,
        step: decision.step.clone(),
        resolution: Resolution::Decided,
        choice: Some(body.choice.clone()),
        note: body.note.clone(),
        decided_by: Some(actor.to_owned()),
        idempotency_key: Some(body.idempotency_key.clone()),
        request_hash: Some(request_hash(body)?),
    };
    resolve_call(cell, decision, output, &resolved)
}

/// Record `output` as an approval's parked result and commit `resolved`
/// consuming it. `false` when another call recorded a result first.
fn resolve_call<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    decision: &OpenDecision,
    output: RuntimeObject,
    resolved: &DecisionResolved,
) -> Result<bool, Refusal> {
    let resumed =
        resolved.resolution == Resolution::Expired && expiry_recorded(cell.store(), decision)?;
    if !resumed
        && cell
            .complete_parked(&decision.command, decision.generation, output)
            .map_err(Refusal::host)?
            .is_none()
    {
        return Ok(false);
    }
    let commit = norn_kernel::cell_host::CellCommit {
        consumes: Some(norn_kernel::cell_host::Consumed::Result {
            command: decision.command.clone(),
            generation: decision.generation,
        }),
        ..norn_kernel::cell_host::CellCommit::empty()
    };
    cell.commit_decision_resolution(commit, resolved)
        .map_err(resolution_refused)?;
    Ok(true)
}

/// DECISION-TERMINAL-1: resolve a decision past its deadline as expired. The
/// run receives the step's typed `Expired` failure (`norn.decision@1`'s
/// declared error) in the commit that records the resolution, so it moves to
/// the flow's handler or its `failed_expired` state instead of waiting
/// forever. A decision someone else resolved meanwhile is left as it is; a
/// retired flow's run keeps waiting.
pub(crate) fn expire<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    decision: &OpenDecision,
    now: LogicalTime,
) -> Result<(), Refusal> {
    if let Some(held) = &decision.call {
        let failure = expired_failure();
        let resolved = DecisionResolved {
            decision_id: decision.id.clone(),
            command_id: decision.command.0.clone(),
            generation: decision.generation.0,
            run_id: call_run(cell.store(), decision, held)?.id,
            step: decision.step.clone(),
            resolution: Resolution::Expired,
            choice: None,
            note: None,
            decided_by: None,
            idempotency_key: None,
            request_hash: None,
        };
        resolve_call(cell, decision, failure, &resolved)?;
        return Ok(());
    }
    let Some(row) = cell
        .store()
        .read()
        .flow_run(MAIN_WORLD, &decision.run_id)
        .map_err(Refusal::host)?
    else {
        return Ok(());
    };
    let Ok(flow) = installed_flow(cell.store(), &row.flow.id) else {
        return Ok(());
    };
    let failure = expired_failure();
    let delivery = |outcome| Delivery {
        command: &decision.command,
        generation: decision.generation,
        run_id: &decision.run_id,
        step: &decision.step,
        attempt: decision.attempt.clone(),
        outcome,
    };
    // The flow must accept the failure before anything is recorded.
    result_turn(
        cell,
        &flow,
        delivery(StepOutcome::Failure(failure.clone())),
        now,
    )?;
    // An expiry recorded earlier whose resolving commit never landed is
    // resumed with exactly that recorded result; anything else recorded
    // meanwhile is left to the call that recorded it.
    if !expiry_recorded(cell.store(), decision)?
        && cell
            .complete_parked(&decision.command, decision.generation, failure.clone())
            .map_err(Refusal::host)?
            .is_none()
    {
        return Ok(());
    }
    let commit = result_turn(cell, &flow, delivery(StepOutcome::Failure(failure)), now)?;
    let resolved = DecisionResolved {
        decision_id: decision.id.clone(),
        command_id: decision.command.0.clone(),
        generation: decision.generation.0,
        run_id: decision.run_id.clone(),
        step: decision.step.clone(),
        resolution: Resolution::Expired,
        choice: None,
        note: None,
        decided_by: None,
        idempotency_key: None,
        request_hash: None,
    };
    cell.commit_decision_resolution(commit, &resolved)
        .map_err(|error| Refusal::from_commit_host_error(&error))?;
    Ok(())
}

/// The caller and what it may see: its read mapping, and the cell's
/// receipt-disclosure authority (D53).
struct Reader<'a, E: SqlEngine> {
    host: &'a DevHost<E>,
    actor: &'a str,
    access: ReadAccess<'a>,
    disclosure: &'a dyn QuestionRuntime<E>,
    key: &'a [u8],
}

impl<E: SqlEngine> Reader<'_, E> {
    /// Whether the caller may read the run's values.
    fn reads_run(&self, cell: &ActionCell<E>, run_id: &str) -> Result<bool, Refusal> {
        let Some(row) = cell
            .store()
            .read()
            .flow_run(MAIN_WORLD, run_id)
            .map_err(Refusal::host)?
        else {
            return Ok(false);
        };
        Ok(self.host.read_permitted(&ReadRequest {
            capability: ReadCapability::Values,
            access: &(self.access)(ReadCapability::Values, &row.object_type),
        }))
    }

    /// The receipt hash the caller may see: raw only with whole-world Values,
    /// else its keyed handle (D53).
    fn receipt_hash(&self, store: &CellStore<E>, receipt: &norn_cell::Receipt) -> String {
        if world_values(self.host, self.disclosure, store, self.actor) {
            receipt.hash.clone()
        } else {
            norn_cell::compute::lineage::keyed_receipt_handle(self.key, &receipt.hash)
        }
    }
}

/// The answer for a decision the ledger holds: a replay of the caller's own
/// identical call, or who resolved it first. Only a reader of the run learns
/// that someone else resolved it; anyone else sees an absent decision.
fn resolved_answer<E: SqlEngine>(
    reader: &Reader<'_, E>,
    cell: &ActionCell<E>,
    id: &str,
    body: &DecideRequest,
) -> Result<(Decided, Option<String>), Box<DecideRefusal>> {
    let Some(row) = cell
        .store()
        .read()
        .decision_resolution(MAIN_WORLD, id)
        .map_err(Refusal::host)?
    else {
        return Err(Box::from(not_found()));
    };
    let own = row.resolved.decided_by.as_deref() == Some(reader.actor);
    if own && row.resolved.idempotency_key.as_deref() == Some(body.idempotency_key.as_str()) {
        return if row.resolved.request_hash.as_deref() == Some(request_hash(body)?.as_str()) {
            // APPROVAL-REVISION-1: a replayed approval of a parked call is
            // fenced again before anything it approved runs.
            let approved = (row.resolved.choice.as_deref() == Some(crate::approvals::APPROVE)
                && row
                    .resolved
                    .command_id
                    .starts_with(crate::approvals::COMMAND_PREFIX))
            .then(|| row.resolved.decision_id.clone());
            Ok((
                answer(reader, cell, id, body, DecideOutcome::Replayed)?,
                approved,
            ))
        } else {
            Err(Box::from(Refusal::new(
                StatusCode::CONFLICT,
                decision_codes::IDEMPOTENCY_CONFLICT,
                "the idempotency key was used for another answer",
                "use a new idempotencyKey for a different choice or note",
            )))
        };
    }
    let reads_run = reader.reads_run(cell, &row.resolved.run_id)?;
    if !own && !reads_run {
        return Err(Box::from(not_found()));
    }
    let refusal = Refusal::new(
        StatusCode::CONFLICT,
        decision_codes::RESOLVED,
        "the decision was already resolved",
        "someone resolved it first",
    );
    let resolution = DecisionResolution {
        resolution: row.resolved.resolution.as_str().to_owned(),
        by: by(&row, reader.actor),
        at_ms: row.resolved_at_ms,
        choice: if reads_run {
            row.resolved.choice.clone()
        } else {
            None
        },
        call: crate::approvals::approved_call(cell, id)?
            .map(|(_held, state)| DecidedCall::of(&state, None)),
    };
    Err(Box::new(DecideRefusal {
        refusal,
        resolution: Some(resolution),
    }))
}

fn answer<E: SqlEngine>(
    reader: &Reader<'_, E>,
    cell: &ActionCell<E>,
    id: &str,
    body: &DecideRequest,
    outcome: DecideOutcome,
) -> Result<Decided, Refusal> {
    let row = cell
        .store()
        .read()
        .decision_resolution(MAIN_WORLD, id)
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("a recorded decision has no ledger row"))?;
    let receipt = cell
        .store()
        .read()
        .receipt_at(row.commit_seq)
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("a decision's commit has no receipt"))?;
    Ok(Decided {
        outcome,
        decision: DecidedDecision {
            id: id.to_owned(),
            choice: body.choice.clone(),
            note: body.note.clone(),
            decided_by: "you".to_owned(),
            decided_at_ms: row.resolved_at_ms,
        },
        receipt: DecidedReceipt {
            receipt_hash: reader.receipt_hash(cell.store(), &receipt),
            commit_seq: row.commit_seq,
            recorded_at_ms: receipt.body.logical_time,
        },
        run: DecidedRun {
            id: row.resolved.run_id.clone(),
            // An approval of a direct call has no run.
            state: cell
                .store()
                .read()
                .flow_run(MAIN_WORLD, &row.resolved.run_id)
                .map_err(Refusal::host)?
                .and_then(|run| run.state),
        },
        call: None,
    })
}

/// The open decision `run_id` waits on, when `actor` may decide it.
///
/// # Errors
/// A host failure.
pub(crate) fn run_decision<E: SqlEngine>(
    host: &DevHost<E>,
    store: &CellStore<E>,
    actor: &str,
    run_id: &str,
) -> Result<Option<crate::run_routes::RunDecision>, Refusal> {
    Ok(open_decisions(store)?
        .into_iter()
        .find(|decision| decision.run_id == run_id && may_decide(host, actor, decision))
        .map(|decision| crate::run_routes::RunDecision {
            id: decision.id,
            expires_at_ms: decision.expires_at_ms,
        }))
}
