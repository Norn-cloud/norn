//! APPROVAL-1: a call of an approval-gated action parks as a decision.
//!
//! While an active `approval` definition gates an action, the shared action
//! dispatch prepares the call without committing it
//! ([`norn_kernel::prepare_action_intent`]). Only calls that reach commit park
//! as one `norn.decision@1` command. Approved calls execute under
//! the requester's own authority, pinned to the action digest
//! (APPROVAL-REVISION-1) and definitions version (APPROVAL-PIN-1), and recover
//! only on the requester's own retry (APPROVAL-RECOVERY-1).

use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use http::StatusCode;
use norn_cell::MAIN_WORLD;
use norn_cell::decision_ledger::{ApprovalRefused, DecisionRow, Resolution};
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::RunStarter;
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::cell_host::{CellCommit, CellHost, FactMutation, FactWrite};
use norn_kernel::{
    ACTION_CALL_TYPE, ActionCallRecord, Command, CommandId, Effect, FactEvent, FactKey,
    FreshnessPolicy, Generation, IdempotencyKey, LogicalTime,
};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::host::ActionCell;
use crate::refusal::Refusal;

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
    /// Optional provenance trace context carried by the call when it parked.
    pub(crate) trace: Option<norn_ir::TraceContext>,
    /// AUTH-INDEPENDENT-PERSON (#618): the prepared-intent digest of the
    /// commit the call would make when it parked: what its approval is
    /// granted for. `None` for a call parked before intents were bound,
    /// whose approval runs nothing ([`approved_intent`]).
    pub(crate) intent_digest: Option<String>,
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
    // A call parked before exact spellings were kept has none; one that has
    // any other `exactInput` than its encoding is no parked call (#618).
    let input = if let Some(exact) = approval.get("exactInput") {
        decode_exact(exact.as_str()?)?
    } else {
        let Some(RuntimeValue::Object(input)) = approval.get("input") else {
            return None;
        };
        input.clone()
    };
    Some(HeldCall {
        definition: text("definition")?,
        revision: norn_ir::runtime::decode(approval.get("revision")?).ok()?,
        call: ParkedCall {
            action: text("action")?,
            input,
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
            trace: match approval.get("trace") {
                None => None,
                Some(trace) => {
                    let decoded: norn_ir::TraceContext = norn_ir::runtime::decode(trace).ok()?;
                    decoded.validate().ok()?;
                    Some(decoded)
                }
            },
            intent_digest: text("intentDigest"),
        },
    })
}

/// AUTH-INDEPENDENT-PERSON (#618): the parked input as written. The outbox
/// keeps a command in Norn1 form, which normalizes text, so the exact spelling
/// the call's approval is granted for travels encoded beside it.
fn encode_exact(input: &RuntimeObject) -> Result<String, Refusal> {
    let exact = norn_ir::canonical::canonical_json_exact_of(input).map_err(Refusal::host)?;
    Ok(URL_SAFE_NO_PAD.encode(exact))
}

fn decode_exact(encoded: &str) -> Option<RuntimeObject> {
    let exact = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let RuntimeValue::Object(input) = RuntimeValue::from_json_text(exact).ok()? else {
        return None;
    };
    Some(input)
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
    for definition in crate::approval_definitions::active_approvals_in(&reader)? {
        let payload: Value = serde_json::from_str(&definition.payload).map_err(Refusal::host)?;
        if payload.get("action").and_then(Value::as_str) == Some(action_id) {
            return Ok(Some(Gate {
                definition: definition.id,
                revision: definition.revision,
                payload,
            }));
        }
    }
    Ok(None)
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
            trace: parked_call.trace.as_ref(),
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
    pub(crate) trace: Option<&'a norn_ir::TraceContext>,
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
    let digest = match (parts.definitions_version, parts.trace) {
        (None, None) => norn_ir::canonical::domain_hash_hex(b"norn.approval.call/2\0", &call),
        (Some(pin), None) => {
            norn_ir::canonical::domain_hash_hex(b"norn.approval.pinned-call/2\0", &(call, pin))
        }
        (None, Some(trace)) => {
            norn_ir::canonical::domain_hash_hex(b"norn.approval.traced-call/1\0", &(call, trace))
        }
        (Some(pin), Some(trace)) => norn_ir::canonical::domain_hash_hex(
            b"norn.approval.pinned-traced-call/1\0",
            &(call, pin, trace),
        ),
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

/// S6b: the invocation an approved call is bound to, read under the
/// caller's cell hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Binding {
    /// A direct call: only a direct call runs it.
    Direct,
    /// A flow step attempt's call: only that attempt's own dispatch runs it.
    Step(StepAttempt),
    /// A step's call parked before attempts were recorded, whose run no
    /// longer waits on it: nothing runs it.
    Ended,
}

/// S6b: what `row`'s approved call `held` is bound to. A call parked with
/// its step attempt is bound to that attempt. A call parked before attempts
/// were recorded is bound by its approval's recorded run: a direct call when
/// that is no flow run (a direct call's approval records its own parked
/// command), else that run's pending step attempt that makes exactly this
/// call now, if one does.
///
/// # Errors
/// A host failure.
pub(crate) fn binding<E: SqlEngine>(
    cell: &ActionCell<E>,
    row: &norn_cell::decision_ledger::ApprovedCall,
    held: &HeldCall,
) -> Result<Binding, Refusal> {
    if let Some(attempt) = &held.call.attempt {
        return Ok(Binding::Step(attempt.clone()));
    }
    if cell
        .store()
        .read()
        .flow_run(MAIN_WORLD, &row.run_id)
        .map_err(Refusal::host)?
        .is_none()
    {
        return Ok(Binding::Direct);
    }
    Ok(
        crate::flow_driver::attempt_of_run(cell.store(), &row.run_id, &held.call.idempotency_key)?
            .map_or(Binding::Ended, Binding::Step),
    )
}

/// S6b: whether the approved call of `decision_id` may still run: a direct
/// call always may; a flow step's call only while the exact step attempt it
/// is bound to ([`binding`]) waits on it, or once it committed (its
/// replay).
///
/// # Errors
/// A host failure.
pub(crate) fn attempt_live<E: SqlEngine>(
    cell: &ActionCell<E>,
    decision_id: &str,
) -> Result<bool, Refusal> {
    let Some(row) = cell
        .store()
        .read()
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
    match binding(cell, &row, &held)? {
        Binding::Direct => Ok(true),
        Binding::Step(attempt) => attempt.waiting(cell.store()),
        Binding::Ended => Ok(false),
    }
}

/// S2: whether `principal`, the authenticated principal of the request
/// running a call, is the parked call's requester: the same subject, actor
/// kind and role table, and never another accountable person than the one
/// recorded (#618). Its roles are the ones verified now.
pub(crate) fn is_requester(principal: Option<&RunStarter>, requester: &RunStarter) -> bool {
    principal.is_some_and(|principal| {
        principal.subject == requester.subject
            && principal.actor_kind == requester.actor_kind
            && principal.role_table == requester.role_table
            && norn_kernel::retained_person_matches(
                requester.accountable_person.as_deref(),
                principal.accountable_person.as_deref(),
            )
    })
}

/// AUTH-INDEPENDENT-PERSON (#618): the prepared-intent digest the approval
/// `decision_id` was granted for, which the call's commit must match. A call
/// parked before intents were bound names none: its approval covers no
/// commit, and that is recorded as final (call again for a fresh approval).
///
/// # Errors
/// `approval.definition_changed`; a host failure.
pub(crate) fn approved_intent<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    decision_id: &str,
) -> Result<String, Refusal> {
    let digest = approved_call(cell, decision_id)?.and_then(|(held, _)| held.call.intent_digest);
    match digest {
        Some(digest) => Ok(digest),
        None => Err(refuse(cell, decision_id, definition_changed())?),
    }
}

/// S2: the approval `decision_id` is consumed only by its parked
/// requester's own authenticated request; any other principal is denied,
/// and nothing is recorded. S5a: and only by the invocation it is bound to
/// ([`binding`], S6b): a flow step's call by the flow's own dispatch of that
/// exact step attempt (`attempt`, carried by the trusted invocation, never
/// derived from the idempotency key), a direct call by a direct call. Any
/// other invocation is denied the same way.
///
/// # Errors
/// The opaque action denial; a host failure.
pub(crate) fn require_requester<E: SqlEngine>(
    cell: &ActionCell<E>,
    decision_id: &str,
    principal: Option<&RunStarter>,
    attempt: Option<&StepAttempt>,
) -> Result<(), Refusal> {
    let row = cell
        .store()
        .read()
        .approved_call(MAIN_WORLD, decision_id)
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("an approved call lost its record"))?;
    let (held, _state) = approved_state(cell, &row)?;
    let bound = match (binding(cell, &row, &held)?, attempt) {
        (Binding::Direct, None) => true,
        (Binding::Step(bound), Some(attempt)) => bound == *attempt,
        _other => false,
    };
    if bound && is_requester(principal, &held.call.requester) {
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
            "exactInput".to_owned(),
            text(&encode_exact(&parked_call.input)?),
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
    if let Some(trace) = &parked_call.trace {
        approval.insert("trace".to_owned(), trace.to_runtime_value());
    }
    if let Some(digest) = &parked_call.intent_digest {
        approval.insert("intentDigest".to_owned(), text(digest));
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
    CellHost::commit(cell, commit).map_err(|error| Refusal::from_commit_host_error(&error))?;
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
    // Exactly the approved call: its action, its requester, its input, its
    // trace. The parked command keeps its input in Norn1 form, as the
    // command's own identity hashes it; the exact spelling the commit wrote
    // is bound by the approval's intent digest (#618).
    let exact = recorded.values.as_ref().map(|values| {
        ActionCallRecord::from_values(values).filter(|record| {
            record.action_id == held.call.action
                && record.actor == held.call.requester.subject
                && same_input(&record.input, &held.call.input)
                && record.trace == held.call.trace
        })
    });
    let conflict = || CallState::Refused("action.idempotency_conflict".to_owned());
    let state = match (row.consumed_seq, exact) {
        // S6c: the commit that consumed this approval wrote the record.
        (Some(_seq), Some(Some(record))) => CallState::Committed(Box::new(record)),
        (Some(_seq), _missing) => {
            return Err(Refusal::host(
                "an approved call's consuming commit lost its record",
            ));
        }
        // S6b: a step's call whose attempt no longer waits on it never runs.
        (None, None) => match &held.call.attempt {
            Some(attempt) if !attempt.waiting(cell.store())? => {
                CallState::Refused(approval_codes::ATTEMPT_ENDED.to_owned())
            }
            _waiting_or_direct => CallState::NotExecuted,
        },
        // S6c: exactly this call, committed by a commit that consumed no
        // approval (one made before consumption was recorded, or an ungated
        // one): its commit. Read from the ledger-derived rows alone, so an
        // opened file and its restored export answer alike; a commit that
        // consumed another approval is never this one's.
        (None, Some(Some(record))) => {
            let seq = crate::actions::record_commit_seq(
                cell.store(),
                &held.call.action,
                &held.call.idempotency_key,
            )?;
            let consumed = cell
                .store()
                .read()
                .approval_consumed_at(MAIN_WORLD, seq)
                .map_err(Refusal::host)?;
            match consumed {
                None => CallState::Committed(Box::new(record)),
                Some(_other) => conflict(),
            }
        }
        // Another call used the key: never this approval's commit, and it
        // can never run.
        (None, Some(None)) => conflict(),
    };
    Ok((held, state))
}

/// Whether two call inputs are one input under Norn1 equality, the rule the
/// parked command stores and identifies its call by.
fn same_input(recorded: &RuntimeObject, parked: &RuntimeObject) -> bool {
    let norn1 = norn_ir::canonical::canonical_json_of::<RuntimeObject>;
    matches!((norn1(recorded), norn1(parked)), (Ok(left), Ok(right)) if left == right)
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
    if refusal.is_unrecorded_approval_refusal() {
        return Ok(refusal);
    }
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
        Err(error) => Err(Refusal::from_commit_host_error(&error)),
    }
}

/// APPROVAL-WHEN-1: whether `gate` holds a call whose commit carries
/// `writes`. Without `when`, every call. Otherwise the call's target record
/// (the object the kernel's `norn.ActionCall` record names) is compared; a
/// target with no proposed values or without the property, or values the
/// comparison cannot order, hold the call (fail closed).
///
/// #1469 S9: how values compare comes from the target property's declared
/// type (`semantics` of the target's object type and the property): a
/// number compares as an exact decimal, text and other literals compare as
/// written (`"1"` and `"1.0"` differ), and a type with no comparison holds
/// the call.
pub(crate) fn applies(
    gate: &Gate,
    writes: &[FactWrite],
    semantics: impl Fn(&str, &str) -> Option<Compared>,
) -> bool {
    let Some(when) = gate.payload.get("when") else {
        return true;
    };
    let (Some(property), Some(op), Some(expected)) = (
        when.get("property").and_then(Value::as_str),
        when.get("op").and_then(Value::as_str),
        when.get("value"),
    ) else {
        return true;
    };
    // #1469 S9: the call's record is its target, by the exact key the
    // kernel's own call record names, never by order among the writes. A
    // call whose target has no proposed values is held (fail closed).
    let values_of = |key: &FactKey| {
        writes
            .iter()
            .filter(|write| write.key == *key)
            .find_map(|write| match &write.mutation {
                FactMutation::Put { values } => Some(values),
                FactMutation::Delete => None,
            })
    };
    let target = writes
        .iter()
        .filter(|write| write.key.object_type.as_str() == norn_kernel::ACTION_CALL_TYPE)
        .find_map(|write| match &write.mutation {
            FactMutation::Put { values } => match values.get("object") {
                Some(RuntimeValue::Object(object)) => match (
                    object.get("objectType").and_then(RuntimeValue::as_str),
                    object.get("key").and_then(RuntimeValue::as_str),
                ) {
                    (Some(object_type), Some(key)) => Some(FactKey::new(object_type, key)),
                    _ => None,
                },
                _ => None,
            },
            FactMutation::Delete => None,
        });
    let Some(target) = target else {
        return true;
    };
    let Some(actual) = values_of(&target).and_then(|values| values.get(property)) else {
        return true;
    };
    match semantics(target.object_type.as_str(), property) {
        Some(Compared::Decimal) => compare(actual, op, expected).unwrap_or(true),
        Some(Compared::Literal) => literal(actual, op, expected).unwrap_or(true),
        None => true,
    }
}

/// How a `when` condition compares a property, from its declared type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Compared {
    /// An integer or decimal: ordered, as exact decimals.
    Decimal,
    /// Text, an enum, a boolean, a date or a reference: equal as written.
    Literal,
}

/// The comparison `object_type.property` takes in `model`; `None` when its
/// type has none (or is unknown), which holds the call.
pub(crate) fn semantics_in(
    model: &norn_cell::Model,
    object_type: &str,
    property: &str,
) -> Option<Compared> {
    use norn_ir::PropertyType;
    let types = model.effective_property_types(object_type).ok()?;
    let (_, kind) = types.into_iter().find(|(id, _)| id == property)?;
    match kind? {
        PropertyType::Integer { .. }
        | PropertyType::Decimal { .. }
        | PropertyType::DecimalBinding { .. } => Some(Compared::Decimal),
        PropertyType::Text { .. }
        | PropertyType::Enum { .. }
        | PropertyType::Boolean {}
        | PropertyType::Date {}
        | PropertyType::Ref { .. } => Some(Compared::Literal),
        PropertyType::Timestamp { .. }
        | PropertyType::Duration { .. }
        | PropertyType::Record { .. }
        | PropertyType::JsonExt { .. } => None,
    }
}

/// `actual op expected` for a literal: only `eq` and `ne`, of the same JSON
/// shape, compared as written; `None` otherwise.
fn literal(actual: &RuntimeValue, op: &str, expected: &Value) -> Option<bool> {
    let equal = match (actual, expected) {
        (RuntimeValue::String(left), Value::String(right)) => left == right,
        (RuntimeValue::Bool(left), Value::Bool(right)) => left == right,
        _ => return None,
    };
    match op {
        "eq" => Some(equal),
        "ne" => Some(!equal),
        _ => None,
    }
}

/// `actual op expected` as exact decimals, or `None` when either is not one
/// or `op` is not a comparison.
fn compare(actual: &RuntimeValue, op: &str, expected: &Value) -> Option<bool> {
    use std::cmp::Ordering;
    let decimal = |value: &RuntimeValue| match value {
        RuntimeValue::Number(number) => Some(*number),
        RuntimeValue::String(text) => norn_ir::Decimal::parse(text).ok(),
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Array(_)
        | RuntimeValue::Object(_) => None,
    };
    let expected_decimal = match expected {
        // Only an integer is exact as a JSON number (#1469 S10); admission
        // refuses any other, and so does this comparison (fail closed).
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            norn_ir::Decimal::parse(&number.to_string()).ok()
        }
        Value::Number(_) => return None,
        Value::String(text) => norn_ir::Decimal::parse(text).ok(),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
    };
    let ordering: Option<Ordering> = match (decimal(actual), expected_decimal) {
        (Some(left), Some(right)) => left.partial_cmp(&right),
        _ => None,
    };
    Some(match (op, ordering?) {
        ("eq", ordering) => ordering == Ordering::Equal,
        ("ne", ordering) => ordering != Ordering::Equal,
        ("lt", ordering) => ordering == Ordering::Less,
        ("le", ordering) => ordering != Ordering::Greater,
        ("gt", ordering) => ordering == Ordering::Greater,
        ("ge", ordering) => ordering != Ordering::Less,
        _ => return None,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixture values")]
mod tests {
    //! APPROVAL-WHEN-1: which calls a narrowed approval holds.
    use super::*;
    use norn_kernel::cell_host::FactVersion;
    use serde_json::json;

    fn gate(when: Option<Value>) -> Gate {
        let mut payload = json!({ "action": "purchasing.request.submit" });
        if let (Some(when), Some(object)) = (when, payload.as_object_mut()) {
            object.insert("when".to_owned(), when);
        }
        Gate {
            definition: "purchasing.large_requests".to_owned(),
            revision: 1,
            payload,
        }
    }

    /// The purchase request's declared types: `amount` a decimal, `status`
    /// and `code` text.
    fn typed(object_type: &str, property: &str) -> Option<Compared> {
        match (object_type, property) {
            ("purchasing.PurchaseRequest", "amount") => Some(Compared::Decimal),
            ("purchasing.PurchaseRequest", "status" | "code") => Some(Compared::Literal),
            _ => None,
        }
    }

    fn put_at(object_type: &str, key: &str, values: Value) -> FactWrite {
        let values = if let RuntimeValue::Object(values) = RuntimeValue::from_json(values).unwrap()
        {
            values
        } else {
            RuntimeObject::new()
        };
        FactWrite {
            key: FactKey::new(object_type, key),
            expected: FactVersion(1),
            mutation: FactMutation::Put { values },
        }
    }

    fn put(object_type: &str, values: Value) -> FactWrite {
        put_at(object_type, "PR-1", values)
    }

    /// The kernel's call record, naming `key` of a purchase request as the
    /// call's target.
    fn call_of(key: &str) -> FactWrite {
        put_at(
            "norn.ActionCall",
            "purchasing.request.submit/k1",
            json!({ "amount": "1",
                "object": { "objectType": "purchasing.PurchaseRequest", "key": key } }),
        )
    }

    fn request(amount: &Value) -> Vec<FactWrite> {
        vec![
            call_of("PR-1"),
            put(
                "purchasing.PurchaseRequest",
                json!({ "amount": amount.clone(), "status": "submitted" }),
            ),
        ]
    }

    /// #618: a parked call keeps its input's exact spelling, and an
    /// `exactInput` that is anything but that encoding never falls back to
    /// the normalized `input`.
    #[test]
    fn a_parked_call_reads_back_its_exact_input_or_none() {
        let parked = |exact: Value| {
            RuntimeValue::from_json(json!({
                "definition": "ops.gate", "revision": 1, "action": "ops.act",
                "input": { "title": "caf\u{e9}" }, "idempotencyKey": "k1",
                "requester": { "subject": "user:a", "actorKind": "user", "roles": [],
                    "roleTable": "fixtures/1", "accountablePerson": "a" },
                "exactInput": exact
            }))
            .unwrap()
        };
        let decomposed = URL_SAFE_NO_PAD.encode(r#"{"title":"cafe\u0301"}"#);
        let read = held_call(&parked(json!(decomposed))).map(|held| held.call.input);
        let title = read.as_ref().and_then(|input| input.get("title"));
        assert_eq!(title, Some(&RuntimeValue::String("cafe\u{301}".to_owned())));
        assert_eq!(held_call(&parked(Value::Null)), None);
        assert_eq!(held_call(&parked(json!(1))), None);
    }

    /// #1469 S9: `when` reads the call's target, never whichever write sorts
    /// first; a target with no proposed values holds the call.
    #[test]
    fn when_reads_the_target_record_by_its_exact_key() {
        let large = gate(Some(
            json!({ "property": "amount", "op": "gt", "value": "1000" }),
        ));
        let writes = vec![
            call_of("PR-9"),
            put_at("purchasing.Line", "A-1", json!({ "amount": "5" })),
            put_at(
                "purchasing.PurchaseRequest",
                "PR-1",
                json!({ "amount": "3" }),
            ),
            put_at(
                "purchasing.PurchaseRequest",
                "PR-9",
                json!({ "amount": "2500.00" }),
            ),
        ];
        assert!(applies(&large, &writes, typed), "the target's 2500 waits");
        let untouched = vec![
            call_of("PR-9"),
            put_at("purchasing.Line", "A-1", json!({ "amount": "5" })),
        ];
        assert!(applies(&large, &untouched, typed), "no target values: held");
        let unnamed = vec![put_at(
            "purchasing.PurchaseRequest",
            "PR-9",
            json!({ "amount": "5" }),
        )];
        assert!(applies(&large, &unnamed, typed), "no call record: held");
    }

    /// #1469 S10: a decimal threshold compares exactly as written.
    #[test]
    fn a_decimal_threshold_compares_exactly() {
        let over = gate(Some(
            json!({ "property": "amount", "op": "gt", "value": "1000.10" }),
        ));
        assert!(!applies(&over, &request(&json!("1000.10")), typed));
        assert!(applies(&over, &request(&json!("1000.11")), typed));
        assert!(!applies(&over, &request(&json!("1000.100")), typed));
        let at_least = gate(Some(
            json!({ "property": "amount", "op": "ge", "value": "1000.10" }),
        ));
        assert!(applies(&at_least, &request(&json!("1000.10")), typed));
        assert!(!applies(&at_least, &request(&json!("1000.09")), typed));
        // A fractional JSON number is not exact: it holds the call.
        let inexact = gate(Some(
            json!({ "property": "amount", "op": "gt", "value": 2000.5 }),
        ));
        assert!(applies(&inexact, &request(&json!("5")), typed));
    }

    #[test]
    fn an_amount_threshold_holds_only_the_larger_requests() {
        let large = gate(Some(
            json!({ "property": "amount", "op": "gt", "value": "1000" }),
        ));
        assert!(applies(&large, &request(&json!("2500.00")), typed));
        assert!(applies(&large, &request(&json!(1000.01)), typed));
        assert!(!applies(&large, &request(&json!("1000.00")), typed));
        assert!(!applies(&large, &request(&json!("990")), typed));
        // The kernel's own records never stand in for the call's record.
        assert!(!applies(&large, &request(&json!("12")), typed));
    }

    #[test]
    fn text_compares_by_equality_and_anything_else_holds_the_call() {
        let status = gate(Some(
            json!({ "property": "status", "op": "eq", "value": "submitted" }),
        ));
        assert!(applies(&status, &request(&json!("5")), typed));
        let other = gate(Some(
            json!({ "property": "status", "op": "ne", "value": "submitted" }),
        ));
        assert!(!applies(&other, &request(&json!("5")), typed));
        // Fail closed: an unordered comparison, a missing property, no record.
        let ordered = gate(Some(
            json!({ "property": "status", "op": "lt", "value": "z" }),
        ));
        assert!(applies(&ordered, &request(&json!("5")), typed));
        let missing = gate(Some(
            json!({ "property": "priority", "op": "eq", "value": "high" }),
        ));
        assert!(applies(&missing, &request(&json!("5")), typed));
        assert!(applies(&missing, &[], typed));
        assert!(applies(&gate(None), &request(&json!("5")), typed));
    }

    /// #1469 S9: a text property compares as written, never as a decimal:
    /// `"1"` and `"1.0"` differ, so `ne "1"` holds `"1.0"`.
    #[test]
    fn text_compares_as_written() {
        let coded = |code: &str| {
            vec![
                call_of("PR-1"),
                put(
                    "purchasing.PurchaseRequest",
                    json!({ "amount": "5", "code": code }),
                ),
            ]
        };
        let not_one = gate(Some(
            json!({ "property": "code", "op": "ne", "value": "1" }),
        ));
        assert!(applies(&not_one, &coded("1.0"), typed));
        assert!(!applies(&not_one, &coded("1"), typed));
        let is_one = gate(Some(
            json!({ "property": "code", "op": "eq", "value": "1" }),
        ));
        assert!(!applies(&is_one, &coded("1.0"), typed));
        // Text is not ordered: an ordered comparison holds the call.
        let above = gate(Some(
            json!({ "property": "code", "op": "gt", "value": "1" }),
        ));
        assert!(applies(&above, &coded("0"), typed));
        // A property whose type has no comparison holds the call.
        let untyped = gate(Some(
            json!({ "property": "amount", "op": "gt", "value": "1" }),
        ));
        assert!(applies(&untyped, &coded("1"), |_, _| None));
    }
}
