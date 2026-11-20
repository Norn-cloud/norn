//! APPROVAL-1: a call of an approval-gated action parks as a decision.
//!
//! While an active `approval` definition gates an action, the shared action
//! dispatch runs the kernel's action pipeline for the call (the caller's
//! authorization, input admission, target visibility, preconditions) against
//! `WouldCommit`, a host that serves every read but stops at the commit
//! point. Only a call that reaches it parks: one `norn.decision@1` command
//! carrying the admitted call and the requester's verified principal,
//! committed alone. A caller who could not commit the call gets exactly the
//! refusal an ungated call would, and creates nothing.
//!
//! A decider who holds one of the approval's roles and is not the requester
//! approves or rejects it (DECISIONS-1). The resolution is the record: once
//! approved, exactly that call (the same action, input, idempotency key and
//! requester) commits through the same dispatch, under the requester's own
//! authority, with everything decided again at that commit, including the
//! laws the cell checks only when it commits. Nothing bypasses the dispatch:
//! the approver's decision executes the call by calling it as the requester.
//!
//! APPROVAL-REVISION-1: an approval is given for the action as it was
//! defined when the call parked. The parked call carries the action
//! definition's digest, and its command id hashes it, so an approval never
//! lets the call commit under a changed action: approving a call whose action
//! changed since it parked refuses it (`approval.definition_changed`), and
//! the requester's call under the new definition parks anew for a fresh
//! approval.
//!
//! APPROVAL-PIN-1: a call pinned to a definitions version (`?version=`)
//! parks with that pin, and its approval commits it pinned the same way: a
//! catalog that changed between parking and approval refuses the commit
//! (`defs.stale`) and commits nothing. The pin is part of the call's
//! identity: the requester's retry under another catalog parks anew and
//! waits for a fresh decision.
//!
//! APPROVAL-RECOVERY-1: the approval of a parked call is recorded first and
//! the call commits after it, so a host can stop in between. Nothing ever
//! runs the call without a request: an approved call that did not commit
//! stays visibly approved and not executed (`not_executed`), and only the
//! requester's own retry runs it, once, under the requester's current
//! authority (no other request can establish it), only as exactly the
//! approved call (its command id: requester, input, idempotency key, action
//! digest, gate definition and revision, definitions pin), and only for the
//! authenticated principal of the request that runs it: the parked
//! requester's subject, actor kind and role table, compared under the
//! dispatch's own hold (S2). The commit that runs it names the approval it
//! consumed, so whether this approval's call committed is that record (S6c);
//! a refusal decided while it ran is recorded on the ledger and is final,
//! whatever commits under the same key later. A call a flow step parked
//! carries that exact step attempt (its run, its parked command and
//! generation) from the moment it parks, and runs only while that attempt
//! waits on it (S6b). A denial of authority is not recorded: authority is
//! decided anew on every try.
//!
//! APPROVAL-LIVE-1: the requester's current authority is established by
//! the requester's own verified credential on its retry, or, for the
//! approver's decide (fresh or replayed) and the drive of a step waiting on
//! it, looked up by the host from the organization's authority at that
//! moment (Studio's membership on a hosted cell, the local principal table
//! on `norn dev`), never taken from the roles recorded when the call parked.
//! While the host looks requesters up, every attempt that can run the call
//! (the requester's own retry included) runs under that lookup. A requester
//! who no longer holds the authority gets the call refused for good,
//! `approval.requester_unauthorized`, recorded like any other final refusal
//! while the approval stands. A lookup that cannot answer runs nothing and
//! records nothing (`approval.authority_unavailable`, retryable).

use std::collections::BTreeMap;

use http::StatusCode;
use norn_cell::decision_ledger::{ApprovalRefused, DecisionRow, Resolution};
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::RunStarter;
use norn_cell::{CellError, MAIN_WORLD};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::cell_host::{
    BoundedFact, CellCommit, CellHost, CommitError, CommitReceipt, DueTimer, FactState,
    LinkReadTarget, LinkState, Observed, Performed, PredicateObject, PredicateQuery,
};
use norn_kernel::{
    ACTION_CALL_TYPE, ActionCallRecord, Command, CommandId, Effect, FactEvent, FactKey,
    FreshnessPolicy, Generation, IdempotencyKey, LinkKey, LogicalTime,
};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::host::ActionCell;
use crate::refusal::Refusal;

/// The sentinel [`WouldCommit`] answers every commit with.
pub(crate) const WOULD_COMMIT: &str = "approval.would_commit";

/// The step id a parked call's decision carries.
pub const APPROVAL_STEP: &str = "approval";

/// The command id prefix of every parked call ([`command_id`]).
pub(crate) const COMMAND_PREFIX: &str = "norn.approval/";

/// The option that approves a parked call.
pub const APPROVE: &str = "approve";

/// The option that rejects a parked call.
pub const REJECT: &str = "reject";

/// Stable `approval.*` refusal codes.
pub mod approval_codes {
    /// A decider rejected this call.
    pub const REJECTED: &str = "approval.rejected";
    /// The decision this call waited on expired.
    pub const EXPIRED: &str = "approval.expired";
    /// APPROVAL-REVISION-1: the action changed after the call parked, so
    /// its approval does not cover it; call again to request a new one.
    pub const DEFINITION_CHANGED: &str = "approval.definition_changed";
    /// APPROVAL-RECOVERY-1: the flow step attempt this call was approved for
    /// no longer waits on it, so its approval never runs it.
    pub const ATTEMPT_ENDED: &str = "approval.attempt_ended";
    /// APPROVAL-LIVE-1: the requester no longer holds the authority the
    /// approved call needs (no longer a member of the organization, or its
    /// current roles do not grant the call). The approval stands; its call
    /// is refused for good.
    pub const REQUESTER_UNAUTHORIZED: &str = "approval.requester_unauthorized";
    /// APPROVAL-LIVE-1: the requester's current authority could not be
    /// looked up, so the approved call did not run; it stays approved and
    /// not executed, and a retry runs it.
    pub const AUTHORITY_UNAVAILABLE: &str = "approval.authority_unavailable";
}

/// The active approval gating one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gate {
    pub(crate) definition: String,
    pub(crate) revision: u64,
    pub(crate) payload: Value,
}

/// A parked call: what the requester asked, admitted and authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParkedCall {
    pub(crate) action: String,
    pub(crate) input: RuntimeObject,
    pub(crate) idempotency_key: String,
    pub(crate) requester: RunStarter,
    /// APPROVAL-REVISION-1: the digest of the action's definition the call
    /// was admitted under; empty for a call parked before digests were bound,
    /// which matches no definition.
    pub(crate) action_digest: String,
    /// APPROVAL-PIN-1: the definitions version the requester pinned the call
    /// to (`?version=`), if any. Its approval commits it only while that
    /// version is still active (`defs.stale` otherwise).
    pub(crate) definitions_version: Option<u64>,
    /// S6b: the flow step attempt waiting on this call when it parked, if
    /// one was. Its approval runs it only while that attempt waits.
    pub(crate) attempt: Option<StepAttempt>,
}

/// S6b: one flow step attempt making an action call: its run and its parked
/// `norn.action@1` command at that generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepAttempt {
    pub(crate) run_id: String,
    pub(crate) command: String,
    pub(crate) generation: u64,
}

impl StepAttempt {
    fn decode(value: &RuntimeValue) -> Option<Self> {
        let text = |name: &str| {
            value
                .get(name)
                .and_then(RuntimeValue::as_str)
                .map(str::to_owned)
        };
        Some(Self {
            run_id: text("runId")?,
            command: text("command")?,
            generation: norn_ir::runtime::decode(value.get("generation")?).ok()?,
        })
    }

    fn encode(&self) -> Result<RuntimeValue, Refusal> {
        Ok(RuntimeValue::Object(BTreeMap::from([
            (
                "runId".to_owned(),
                RuntimeValue::String(self.run_id.clone()),
            ),
            (
                "command".to_owned(),
                RuntimeValue::String(self.command.clone()),
            ),
            (
                "generation".to_owned(),
                norn_ir::runtime::encode(&self.generation).map_err(Refusal::host)?,
            ),
        ])))
    }

    /// Whether this attempt still waits: its command is parked at its
    /// generation with no result recorded.
    pub(crate) fn waiting<E: SqlEngine>(
        &self,
        store: &norn_cell::CellStore<E>,
    ) -> Result<bool, Refusal> {
        Ok(store
            .read()
            .pending_command(&self.command)
            .map_err(Refusal::host)?
            .is_some_and(|row| row.command.generation == self.generation && row.result.is_none()))
    }
}

/// A parked call as its decision carries it: the approval that held it and
/// the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeldCall {
    /// The approval definition that gated it.
    pub(crate) definition: String,
    /// That definition's revision.
    pub(crate) revision: u64,
    /// The call.
    pub(crate) call: ParkedCall,
}

/// The parked call a decision's `approval` member carries, as [`park`]
/// wrote it; `None` for anything else.
pub(crate) fn held_call(approval: &RuntimeValue) -> Option<HeldCall> {
    let text = |name: &str| {
        approval
            .get(name)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
    };
    let Some(RuntimeValue::Object(input)) = approval.get("input") else {
        return None;
    };
    Some(HeldCall {
        definition: text("definition")?,
        revision: norn_ir::runtime::decode(approval.get("revision")?).ok()?,
        call: ParkedCall {
            action: text("action")?,
            input: input.clone(),
            idempotency_key: text("idempotencyKey")?,
            requester: norn_ir::runtime::decode(approval.get("requester")?).ok()?,
            action_digest: text("actionDigest").unwrap_or_default(),
            definitions_version: match approval.get("definitionsVersion") {
                None => None,
                Some(pin) => Some(norn_ir::runtime::decode(pin).ok()?),
            },
            attempt: match approval.get("attempt") {
                None => None,
                Some(attempt) => Some(StepAttempt::decode(attempt)?),
            },
        },
    })
}

/// The decision a parked call waits on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingDecision {
    /// The decision id (`POST /v1/decisions/{id}:decide`).
    pub id: String,
    /// When it expires (logical milliseconds).
    pub expires_at_ms: u64,
}

/// `202`: the call waits for a person's approval; nothing was committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingAction {
    /// Always `pending`.
    pub outcome: String,
    /// The decision it waits on.
    pub decision: PendingDecision,
}

/// What the gate holds a call to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Held {
    /// It waits on this decision; nothing was committed.
    Pending(PendingAction),
    /// A decider approved exactly this call: it commits now.
    Approved,
}

/// The active approval gating `action_id`, if one does.
///
/// # Errors
/// A host failure.
pub(crate) fn gate<E: SqlEngine>(
    cell: &ActionCell<E>,
    action_id: &str,
) -> Result<Option<Gate>, Refusal> {
    let reader = cell.store().read();
    let mut after: Option<String> = None;
    loop {
        let page = reader
            .retained_definitions("approval", false, after.as_deref(), 200)
            .map_err(Refusal::host)?;
        for record in &page {
            let payload: Value =
                serde_json::from_str(&record.definition.payload).map_err(Refusal::host)?;
            if payload.get("action").and_then(Value::as_str) == Some(action_id) {
                return Ok(Some(Gate {
                    definition: record.definition.id.clone(),
                    revision: record.definition.revision,
                    payload,
                }));
            }
        }
        if page.len() < 200 {
            return Ok(None);
        }
        after = page.last().map(|record| record.definition.id.clone());
    }
}

/// Whether a kernel refusal is [`WouldCommit`]'s sentinel: the call passed
/// every check and reached the commit point.
pub(crate) fn would_have_committed(refusal: &norn_kernel::ActionRefusal<CellError>) -> bool {
    matches!(
        refusal,
        norn_kernel::ActionRefusal::Decision(norn_kernel::DecisionRefusal::Host(
            CellError::DecisionRefused(WOULD_COMMIT)
        ))
    )
}

/// The decision request a parked call carries (`norn.decision@1` input).
fn request(gate: &Gate) -> Value {
    let action = gate.payload.get("action").cloned().unwrap_or(Value::Null);
    let mut request = serde_json::json!({
        "question": gate.payload.get("question").cloned().unwrap_or(Value::Null),
        "risk": gate.payload.get("risk").cloned().unwrap_or_else(|| Value::String("medium".into())),
        "options": [
            { "id": APPROVE, "label": "Approve", "scope": "This call", "effect": { "action": action } },
            { "id": REJECT, "label": "Reject", "scope": "This call",
              "effect": { "binding": gate.definition } }
        ],
        "decider": gate.payload.get("decider").cloned().unwrap_or(Value::Null),
    });
    if let (Some(object), Some(expires)) =
        (request.as_object_mut(), gate.payload.get("expiresInMs"))
    {
        object.insert("expiresInMs".to_owned(), expires.clone());
    }
    request
}

/// The command id of a parked call: one per action, its effective
/// definition's digest, the gate revision holding it, input, idempotency
/// key, requester and definitions pin, so the same call parks once, and an
/// approval approves exactly the call it was asked about under the action
/// it was asked about (APPROVAL-REVISION-1).
///
/// APPROVAL-PIN-1: a pinned call's id also hashes its pin, under its own
/// domain tag, so the requester's retry under another catalog (unpinned, or
/// pinned to another version) is another call that parks for a fresh
/// decision, never the earlier approval.
fn command_id(gate: &Gate, parked_call: &ParkedCall) -> Result<String, Refusal> {
    call_command_id(
        gate,
        &CallParts {
            action: &parked_call.action,
            action_digest: &parked_call.action_digest,
            idempotency_key: &parked_call.idempotency_key,
            requester: &parked_call.requester.subject,
            input: &parked_call.input,
            definitions_version: parked_call.definitions_version,
        },
    )
}

/// What identifies a parked call besides its gate ([`command_id`]).
pub(crate) struct CallParts<'a> {
    pub(crate) action: &'a str,
    pub(crate) action_digest: &'a str,
    pub(crate) idempotency_key: &'a str,
    pub(crate) requester: &'a str,
    pub(crate) input: &'a RuntimeObject,
    pub(crate) definitions_version: Option<u64>,
}

/// [`command_id`] from the call's parts: what an approval replay compares.
pub(crate) fn call_command_id(gate: &Gate, parts: &CallParts<'_>) -> Result<String, Refusal> {
    let call = (
        parts.action,
        parts.action_digest,
        // The gate revision the decider roles come from: a changed gate
        // asks anew.
        &gate.definition,
        gate.revision,
        parts.idempotency_key,
        parts.requester,
        parts.input,
    );
    let digest = match parts.definitions_version {
        None => norn_ir::canonical::domain_hash_hex(b"norn.approval.call/2\0", &call),
        Some(pin) => {
            norn_ir::canonical::domain_hash_hex(b"norn.approval.pinned-call/2\0", &(call, pin))
        }
    }
    .map_err(Refusal::host)?;
    Ok(format!(
        "{COMMAND_PREFIX}{}",
        digest.get(..32).unwrap_or(&digest)
    ))
}

fn runtime(value: &Value) -> Result<RuntimeValue, Refusal> {
    RuntimeValue::from_json(value.clone()).map_err(Refusal::host)
}

/// APPROVAL-REVISION-1: the action or its gate changed after the call
/// parked; its approval does not cover the call as it would run now.
pub(crate) fn definition_changed() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        approval_codes::DEFINITION_CHANGED,
        "the action changed since this call was approved",
        "the approval covers the action and gate as they were when the call parked; call again to request a new approval",
    )
}

/// `approval.attempt_ended`: the step attempt the approved call was for no
/// longer waits on it.
pub(crate) fn attempt_ended() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        approval_codes::ATTEMPT_ENDED,
        "the run step this call was approved for no longer waits on it",
        "an approval runs a step's call only while that exact step attempt is pending",
    )
}

/// `approval.requester_unauthorized` (APPROVAL-LIVE-1): the requester's
/// current authority does not cover the approved call.
#[must_use]
pub fn requester_unauthorized() -> Refusal {
    Refusal::new(
        StatusCode::FORBIDDEN,
        approval_codes::REQUESTER_UNAUTHORIZED,
        "the requester no longer holds the authority this call needs",
        "the approval stands, but its call is refused for good; the requester calls again with a new idempotency key once they hold the authority",
    )
}

/// `approval.authority_unavailable` (APPROVAL-LIVE-1): the requester's
/// current authority could not be looked up; nothing ran.
#[must_use]
pub fn authority_unavailable() -> Refusal {
    Refusal::new(
        StatusCode::SERVICE_UNAVAILABLE,
        approval_codes::AUTHORITY_UNAVAILABLE,
        "the requester's current authority could not be looked up",
        "the approved call did not run and stays approved; retry the decision, or the requester retries the call",
    )
}

/// APPROVAL-LIVE-1: record `approval.requester_unauthorized` as final for
/// the approved call of `decision_id`, in its own commit under the cell:
/// the requester's current authority does not cover it. The call's state is
/// read again under the hold first, so a refusal recorded first stands and
/// an exact commit is never masked.
///
/// # Errors
/// A host failure; nothing is recorded and the call stays approved and not
/// executed.
pub(crate) fn refuse_unauthorized<E: SqlEngine>(
    host: &crate::host::DevHost<E>,
    decision_id: &str,
) -> Result<(), Refusal> {
    let _epoch = host.definition_read()?;
    let mut cell = host.cell()?;
    record_final(&mut cell, decision_id, requester_unauthorized()).map(|_refusal| ())
}

/// S6b: whether the approved call of `decision_id` may still run: a direct
/// call always may; a flow step's call only while the exact step attempt it
/// parked for (recorded when it parked) waits on it, or once it committed
/// (its replay).
///
/// # Errors
/// A host failure.
pub(crate) fn attempt_live<E: SqlEngine>(
    cell: &ActionCell<E>,
    decision_id: &str,
) -> Result<bool, Refusal> {
    let reader = cell.store().read();
    let Some(row) = reader
        .approved_call(MAIN_WORLD, decision_id)
        .map_err(Refusal::host)?
    else {
        return Ok(true);
    };
    let (held, state) = approved_state(cell, &row)?;
    // A committed call replays, live or not.
    if matches!(state, CallState::Committed(_)) {
        return Ok(true);
    }
    if let Some(attempt) = &held.call.attempt {
        return attempt.waiting(cell.store());
    }
    // Parked before attempts were recorded: the run its approval names.
    if reader
        .flow_run(MAIN_WORLD, &row.run_id)
        .map_err(Refusal::host)?
        .is_none()
    {
        return Ok(true);
    }
    Ok(
        crate::flow_driver::run_waiting_on(cell.store(), &held.call.idempotency_key)?.as_deref()
            == Some(row.run_id.as_str()),
    )
}

/// S2: whether `principal`, the authenticated principal of the request
/// running a call, is the parked call's requester: the same subject, actor
/// kind and role table. Its roles are the ones verified now.
pub(crate) fn is_requester(principal: Option<&RunStarter>, requester: &RunStarter) -> bool {
    principal.is_some_and(|principal| {
        principal.subject == requester.subject
            && principal.actor_kind == requester.actor_kind
            && principal.role_table == requester.role_table
    })
}

/// S2: the approval `decision_id` is consumed only by its parked
/// requester's own authenticated request; any other principal is denied,
/// and nothing is recorded.
///
/// # Errors
/// The opaque action denial; a host failure.
pub(crate) fn require_requester<E: SqlEngine>(
    cell: &ActionCell<E>,
    decision_id: &str,
    principal: Option<&RunStarter>,
) -> Result<(), Refusal> {
    let (held, _state) = approved_call(cell, decision_id)?
        .ok_or_else(|| Refusal::host("an approved call lost its record"))?;
    if is_requester(principal, &held.call.requester) {
        Ok(())
    } else {
        Err(crate::actions::action_denied())
    }
}

fn rejected() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        approval_codes::REJECTED,
        "a decider rejected this call",
        "the approval this call waited on was rejected; nothing was committed",
    )
}

fn expired() -> Refusal {
    Refusal::new(
        StatusCode::GONE,
        approval_codes::EXPIRED,
        "the approval this call waited on expired",
        "no one decided before its deadline; nothing was committed",
    )
}

/// What a resolved approval holds its call to.
fn resolved(row: &DecisionRow) -> Result<Held, Refusal> {
    match row.resolved.resolution {
        Resolution::Decided if row.resolved.choice.as_deref() == Some(APPROVE) => {
            Ok(Held::Approved)
        }
        Resolution::Expired => Err(expired()),
        Resolution::Decided | Resolution::Withdrawn => Err(rejected()),
    }
}

/// Hold `parked_call` behind `gate`: an approved call commits; a rejected or
/// expired one is refused; otherwise it parks as one `norn.decision@1`
/// command carrying the call, and answers the decision it waits on. The same
/// call parked again answers the same decision.
///
/// # Errors
/// `approval.rejected`, `approval.expired`; a host failure.
pub(crate) fn hold<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    gate: &Gate,
    parked_call: &ParkedCall,
    now: LogicalTime,
) -> Result<Held, Refusal> {
    let id = command_id(gate, parked_call)?;
    let decision_id = crate::decisions::decision_id(&id, 1)?;
    if let Some(row) = cell
        .store()
        .read()
        .decision_resolution(MAIN_WORLD, &decision_id)
        .map_err(Refusal::host)?
    {
        return resolved(&row);
    }
    let unparked = cell
        .store()
        .read()
        .pending_command(&id)
        .map_err(Refusal::host)?
        .is_none();
    if unparked {
        park(cell, gate, parked_call, &id)?;
    }
    let parked = cell
        .store()
        .read()
        .pending_command(&id)
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("a parked call is missing"))?;
    let Some(decision) = crate::decisions::open_decision(cell.store(), &parked)? else {
        // Answered, and its resolution is committing.
        return Err(Refusal::new(
            StatusCode::CONFLICT,
            crate::decisions::decision_codes::RESOLVED,
            "the approval is being resolved",
            "call again with the same idempotency key",
        ));
    };
    if now.as_millis() >= decision.expires_at_ms {
        crate::decisions::expire(cell, &decision, now)?;
        return Err(expired());
    }
    Ok(Held::Pending(PendingAction {
        outcome: "pending".to_owned(),
        decision: PendingDecision {
            id: decision.id,
            expires_at_ms: decision.expires_at_ms,
        },
    }))
}

/// Commit the parked command for `parked_call`, alone.
fn park<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    gate: &Gate,
    parked_call: &ParkedCall,
    id: &str,
) -> Result<(), Refusal> {
    let starter = norn_ir::runtime::encode(&parked_call.requester).map_err(Refusal::host)?;
    let text = |value: &str| RuntimeValue::String(value.to_owned());
    let mut approval: RuntimeObject = BTreeMap::from([
        ("definition".to_owned(), text(&gate.definition)),
        (
            "revision".to_owned(),
            norn_ir::runtime::encode(&gate.revision).map_err(Refusal::host)?,
        ),
        ("action".to_owned(), text(&parked_call.action)),
        ("actionDigest".to_owned(), text(&parked_call.action_digest)),
        (
            "input".to_owned(),
            RuntimeValue::Object(parked_call.input.clone()),
        ),
        (
            "idempotencyKey".to_owned(),
            text(&parked_call.idempotency_key),
        ),
        ("requester".to_owned(), starter),
    ]);
    if let Some(pin) = parked_call.definitions_version {
        approval.insert(
            "definitionsVersion".to_owned(),
            norn_ir::runtime::encode(&pin).map_err(Refusal::host)?,
        );
    }
    if let Some(attempt) = &parked_call.attempt {
        approval.insert("attempt".to_owned(), attempt.encode()?);
    }
    let payload: RuntimeObject = BTreeMap::from([
        ("kind".to_owned(), text("perform")),
        (
            "function".to_owned(),
            text(crate::flow_definitions::DECISION_FUNCTION),
        ),
        ("functionVersion".to_owned(), text("1")),
        ("runId".to_owned(), text(id)),
        ("step".to_owned(), text(APPROVAL_STEP)),
        ("attempt".to_owned(), runtime(&Value::from(1))?),
        ("idempotencyKey".to_owned(), text(id)),
        (
            "input".to_owned(),
            RuntimeValue::Object(BTreeMap::from([(
                APPROVAL_STEP.to_owned(),
                runtime(&request(gate))?,
            )])),
        ),
        ("approval".to_owned(), RuntimeValue::Object(approval)),
    ]);
    let commit = CellCommit {
        effects: vec![Effect::Perform(Command {
            id: CommandId(id.to_owned()),
            generation: Generation(1),
            idempotency_key: IdempotencyKey(id.to_owned()),
            freshness: FreshnessPolicy::AppendOnly,
            deadline: None,
            payload,
        })],
        ..CellCommit::<FactEvent, RuntimeObject, RuntimeObject>::empty()
    };
    // Under the cell lock nothing else parks this id: any refusal is the
    // host's.
    CellHost::commit(cell, commit).map_err(|error| Refusal::host(format!("{error:?}")))?;
    Ok(())
}

/// APPROVAL-RECOVERY-1: where an approved call stands.
#[derive(Debug, Clone)]
pub(crate) enum CallState {
    /// Approved, not yet executed: the requester's retry or the approver's
    /// replay runs it.
    NotExecuted,
    /// Exactly this call committed; its idempotency record.
    Committed(Box<ActionCallRecord>),
    /// It was refused while it ran, for good: the refusal's code.
    Refused(String),
}

/// The approved call of `decision_id` and where it stands; `None` when the
/// decision approved no call.
///
/// # Errors
/// A host failure.
pub(crate) fn approved_call<E: SqlEngine>(
    cell: &ActionCell<E>,
    decision_id: &str,
) -> Result<Option<(HeldCall, CallState)>, Refusal> {
    let Some(row) = cell
        .store()
        .read()
        .approved_call(MAIN_WORLD, decision_id)
        .map_err(Refusal::host)?
    else {
        return Ok(None);
    };
    approved_state(cell, &row).map(Some)
}

/// The parked call `row` approved and where it stands.
///
/// # Errors
/// A host failure: the parked command is gone or unreadable.
pub(crate) fn approved_state<E: SqlEngine>(
    cell: &ActionCell<E>,
    row: &norn_cell::decision_ledger::ApprovedCall,
) -> Result<(HeldCall, CallState), Refusal> {
    let held = cell
        .store()
        .read()
        .outbox_command(&row.command_id)
        .map_err(Refusal::host)?
        .and_then(|command| {
            command
                .command
                .get("payload")
                .and_then(|payload| payload.get("approval"))
                .and_then(held_call)
        })
        .ok_or_else(|| Refusal::host("an approved call's parked command is missing"))?;
    // S6c: a recorded refusal is final: no later commit under the same key
    // (another gate revision's approval of the same call) makes it committed.
    if let Some(refused) = &row.refused {
        return Ok((held, CallState::Refused(refused.code.clone())));
    }
    let key = FactKey::new(
        ACTION_CALL_TYPE,
        format!("{}/{}", held.call.action, held.call.idempotency_key),
    );
    let recorded = CellHost::load(cell, &key).map_err(Refusal::host)?;
    // Exactly the approved call: its action, its requester, its input.
    let exact = recorded.values.as_ref().map(|values| {
        ActionCallRecord::from_values(values).filter(|record| {
            record.action_id == held.call.action
                && record.actor == held.call.requester.subject
                && record.input == held.call.input
        })
    });
    let state = match (row.consumed_seq, exact) {
        // S6c: the commit that consumed this approval wrote the record.
        (Some(_seq), Some(Some(record))) => CallState::Committed(Box::new(record)),
        (Some(_seq), _missing) => {
            return Err(Refusal::host(
                "an approved call's consuming commit lost its record",
            ));
        }
        (None, None) => CallState::NotExecuted,
        // Backfilled: its commits name no approval, so the exact record is
        // its commit.
        (None, Some(Some(record))) if row.legacy => CallState::Committed(Box::new(record)),
        // Another commit used the key (another call, or the same call under
        // another approval): never this approval's commit, and it can never
        // run.
        (None, Some(_other)) => CallState::Refused("action.idempotency_conflict".to_owned()),
    };
    Ok((held, state))
}

/// The decision that approved the parked call `command`, if one did.
///
/// # Errors
/// A host failure.
pub(crate) fn approving_decision<E: SqlEngine>(
    cell: &ActionCell<E>,
    command: &str,
) -> Result<Option<String>, Refusal> {
    let decision_id = crate::decisions::decision_id(command, 1)?;
    let row = cell
        .store()
        .read()
        .decision_resolution(MAIN_WORLD, &decision_id)
        .map_err(Refusal::host)?;
    Ok(row
        .filter(|row| {
            row.resolved.resolution == Resolution::Decided
                && row.resolved.choice.as_deref() == Some(APPROVE)
        })
        .map(|_| decision_id))
}

/// The refusal recorded for the approved call of `decision_id`, as its
/// callers get it, if one is recorded.
///
/// # Errors
/// A host failure.
pub(crate) fn recorded_refusal<E: SqlEngine>(
    cell: &ActionCell<E>,
    decision_id: &str,
) -> Result<Option<Refusal>, Refusal> {
    Ok(cell
        .store()
        .read()
        .approved_call(MAIN_WORLD, decision_id)
        .map_err(Refusal::host)?
        .and_then(|row| row.refused)
        .map(|refused| {
            let status = StatusCode::from_u16(refused.status).unwrap_or(StatusCode::CONFLICT);
            Refusal::new(
                status,
                &refused.code,
                "this approved call was refused when it ran",
                "its approval never runs it again; call with a new idempotency key to ask for a new approval",
            )
            // S11: the category it was served with, never one rebuilt from
            // the status.
            .with_category(
                crate::refusal::RefusalCategory::from_wire_name(&refused.category)
                    .unwrap_or_else(|| crate::refusal::RefusalCategory::of_status(status)),
            )
        }))
}

/// [`refuse`] under the approving decision `approval`, when the refused try
/// ran under one: the refusal the caller gets (a host failure recording it
/// in its place).
pub(crate) fn refused_under<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    approval: Option<&str>,
    refusal: Refusal,
) -> Refusal {
    match approval {
        Some(decision_id) => refuse(cell, decision_id, refusal).unwrap_or_else(|host| host),
        None => refusal,
    }
}

/// Record `refusal` as final for the approved call of `decision_id` when it
/// decides the call (anything but a host failure or a denial of authority,
/// which is decided anew on every try), in its own commit under the cell
/// the refused try holds; `refusal` back, or the refusal recorded first.
///
/// # Errors
/// A host failure recording it; nothing is recorded and the call stays
/// approved and not executed.
pub(crate) fn refuse<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    decision_id: &str,
    refusal: Refusal,
) -> Result<Refusal, Refusal> {
    use crate::refusal::RefusalCategory;
    if matches!(
        refusal.category,
        RefusalCategory::Internal | RefusalCategory::Permission | RefusalCategory::Visibility
    ) {
        return Ok(refusal);
    }
    record_final(cell, decision_id, refusal)
}

/// Record `refusal` as final for the approved call of `decision_id`,
/// whatever its category, under `cell`: `refusal` back, or the refusal
/// recorded first.
fn record_final<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    decision_id: &str,
    refusal: Refusal,
) -> Result<Refusal, Refusal> {
    // Under the hold the refused try holds, the call's state is read again
    // before anything is recorded: a refusal recorded first stands, and an
    // exact commit is never masked by a later refusal (it is the call's
    // answer, and the try's refusal is not recorded).
    if let Some(recorded) = recorded_refusal(cell, decision_id)? {
        return Ok(recorded);
    }
    if let Some((_held, CallState::Committed(_))) = approved_call(cell, decision_id)? {
        return Ok(refusal);
    }
    let refused = ApprovalRefused {
        decision_id: decision_id.to_owned(),
        status: refusal.status,
        code: refusal.code.clone(),
        category: refusal.category.wire_name().to_owned(),
    };
    match cell.commit_approval_refusal(&refused) {
        Ok(_receipt) => Ok(refusal),
        Err(error) => Err(Refusal::host(format!("{error:?}"))),
    }
}

/// A host for one dry run of the kernel's action pipeline over `cell`: every
/// read is the cell's, and every commit stops with [`WOULD_COMMIT`], so
/// nothing is written.
pub(crate) struct WouldCommit<'a, E: SqlEngine>(pub(crate) &'a ActionCell<E>);

fn stop<T>() -> Result<T, CommitError<CellError>> {
    Err(CommitError::Host(CellError::DecisionRefused(WOULD_COMMIT)))
}

impl<E: SqlEngine> CellHost for WouldCommit<'_, E> {
    type Event = FactEvent;
    type Command = RuntimeObject;
    type Input = RuntimeObject;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.0.load(key)
    }
    fn supports_reference_reads(&self) -> bool {
        self.0.supports_reference_reads()
    }
    fn load_reference(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.0.load_reference(key, max_bytes)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.0.load_bounded(key, max_bytes)
    }
    fn resolve_accepted_receipt(
        &self,
        reference: &norn_kernel::receipt_binding::ReceiptReference,
        manifest: &norn_ir::function::Digest,
    ) -> Result<
        Option<norn_kernel::receipt_binding::ReceiptBinding<norn_kernel::cell_host::JsonObject>>,
        norn_kernel::receipt_binding::ReceiptBindingError<CellError>,
    > {
        self.0.resolve_accepted_receipt(reference, manifest)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.0.load_link(key)
    }
    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, CellError> {
        self.0
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }
    fn read_predicate(
        &self,
        predicate: &PredicateQuery,
        max_members: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<PredicateObject>>, CellError> {
        self.0.read_predicate(predicate, max_members, max_bytes)
    }
    fn commit(
        &mut self,
        _commit: CellCommit<FactEvent, RuntimeObject, RuntimeObject>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        stop()
    }
    fn commit_action_quantity(
        &mut self,
        _commit: CellCommit<FactEvent, RuntimeObject, RuntimeObject>,
        _requests: &[norn_ir::quantity_ledger::QuantityRequest],
        _idempotency_key: &str,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        stop()
    }
    fn commit_action_references(
        &mut self,
        _commit: CellCommit<FactEvent, RuntimeObject, RuntimeObject>,
        _proof: norn_kernel::action_commit::ActionReferenceReads,
        _quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        stop()
    }
    fn commit_owned_rows(
        &mut self,
        _commit: CellCommit<FactEvent, RuntimeObject, RuntimeObject>,
        _proof: norn_kernel::cell_host::OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        stop()
    }
    fn perform(&mut self) -> Result<Option<Performed<RuntimeObject>>, CellError> {
        Ok(None)
    }
    fn observe(&mut self) -> Result<Option<Observed<RuntimeObject>>, CellError> {
        Ok(None)
    }
    fn clock(&mut self) -> Result<Option<DueTimer<RuntimeObject>>, CellError> {
        Ok(None)
    }
    fn serialize_by(&self, key: &FactKey) -> norn_kernel::cell_host::SerializationLane {
        self.0.serialize_by(key)
    }
    fn publish(&mut self, _receipt: &CommitReceipt) -> Result<(), CellError> {
        Ok(())
    }
}
