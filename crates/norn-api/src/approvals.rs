//! APPROVAL-1: a call of an approval-gated action parks as a decision.
//!
//! While an active `approval` definition gates an action, the shared action
//! dispatch runs the kernel's action pipeline for the call (the caller's
//! authorization, input admission, target visibility, preconditions) against
//! [`WouldCommit`], a host that serves every read but stops at the commit
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

use std::collections::BTreeMap;

use http::StatusCode;
use norn_cell::decision_ledger::{DecisionRow, Resolution};
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::RunStarter;
use norn_cell::{CellError, MAIN_WORLD};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::cell_host::{
    BoundedFact, CellCommit, CellHost, CommitError, CommitReceipt, DueTimer, FactState,
    LinkReadTarget, LinkState, Observed, Performed, PredicateObject, PredicateQuery,
};
use norn_kernel::{
    Command, CommandId, Effect, FactEvent, FactKey, FreshnessPolicy, Generation, IdempotencyKey,
    LinkKey, LogicalTime,
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

/// The command id of a parked call: one per action, input, idempotency key
/// and requester, so the same call parks once, and an approval approves
/// exactly the call it was asked about.
fn command_id(parked_call: &ParkedCall) -> Result<String, Refusal> {
    let digest = norn_ir::canonical::domain_hash_hex(
        b"norn.approval.call/1\0",
        &(
            &parked_call.action,
            &parked_call.idempotency_key,
            &parked_call.requester.subject,
            &parked_call.input,
        ),
    )
    .map_err(Refusal::host)?;
    Ok(format!(
        "norn.approval/{}",
        digest.get(..32).unwrap_or(&digest)
    ))
}

fn runtime(value: &Value) -> Result<RuntimeValue, Refusal> {
    RuntimeValue::from_json(value.clone()).map_err(Refusal::host)
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
    let id = command_id(parked_call)?;
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
    let approval: RuntimeObject = BTreeMap::from([
        ("definition".to_owned(), text(&gate.definition)),
        (
            "revision".to_owned(),
            norn_ir::runtime::encode(&gate.revision).map_err(Refusal::host)?,
        ),
        ("action".to_owned(), text(&parked_call.action)),
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
