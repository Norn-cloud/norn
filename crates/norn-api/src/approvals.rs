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

use std::collections::BTreeMap;

use http::StatusCode;
use norn_cell::decision_ledger::{DecisionRow, Resolution};
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::RunStarter;
use norn_cell::{CellError, MAIN_WORLD};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::cell_host::{
    BoundedFact, CellCommit, CellHost, CommitError, CommitReceipt, DueTimer, FactMutation,
    FactState, FactWrite, LinkReadTarget, LinkState, Observed, Performed, PredicateObject,
    PredicateQuery,
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
/// nothing is written. It keeps the object writes the commit carried, so a
/// gate's `when` reads the record the call would write (APPROVAL-WHEN-1).
pub(crate) struct WouldCommit<'a, E: SqlEngine> {
    cell: &'a ActionCell<E>,
    writes: Vec<FactWrite>,
}

impl<'a, E: SqlEngine> WouldCommit<'a, E> {
    /// A dry run over `cell`.
    pub(crate) const fn new(cell: &'a ActionCell<E>) -> Self {
        Self {
            cell,
            writes: Vec::new(),
        }
    }

    /// The object writes of the commit the dry run reached, if any.
    pub(crate) fn into_writes(self) -> Vec<FactWrite> {
        self.writes
    }

    fn stop<T>(
        &mut self,
        commit: CellCommit<FactEvent, RuntimeObject, RuntimeObject>,
    ) -> Result<T, CommitError<CellError>> {
        self.writes = commit.writes;
        Err(CommitError::Host(CellError::DecisionRefused(WOULD_COMMIT)))
    }
}

/// APPROVAL-WHEN-1: whether `gate` holds a call whose commit carries
/// `writes`. Without `when`, every call. Otherwise the call's target record
/// (the object the kernel's `norn.ActionCall` record names) is compared; a
/// target with no proposed values or without the property, or values the
/// comparison cannot order, hold the call (fail closed).
pub(crate) fn applies(gate: &Gate, writes: &[FactWrite]) -> bool {
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
    let Some(actual) = target
        .as_ref()
        .and_then(values_of)
        .and_then(|values| values.get(property))
    else {
        return true;
    };
    compare(actual, op, expected).unwrap_or(true)
}

/// `actual op expected`, or `None` when the values do not support `op`.
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
    if let Some(ordering) = ordering {
        return Some(match op {
            "eq" => ordering == Ordering::Equal,
            "ne" => ordering != Ordering::Equal,
            "lt" => ordering == Ordering::Less,
            "le" => ordering != Ordering::Greater,
            "gt" => ordering == Ordering::Greater,
            "ge" => ordering != Ordering::Less,
            _ => return None,
        });
    }
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

impl<E: SqlEngine> CellHost for WouldCommit<'_, E> {
    type Event = FactEvent;
    type Command = RuntimeObject;
    type Input = RuntimeObject;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.cell.load(key)
    }
    fn supports_reference_reads(&self) -> bool {
        self.cell.supports_reference_reads()
    }
    fn load_reference(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.cell.load_reference(key, max_bytes)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.cell.load_bounded(key, max_bytes)
    }
    fn resolve_accepted_receipt(
        &self,
        reference: &norn_kernel::receipt_binding::ReceiptReference,
        manifest: &norn_ir::function::Digest,
    ) -> Result<
        Option<norn_kernel::receipt_binding::ReceiptBinding<norn_kernel::cell_host::JsonObject>>,
        norn_kernel::receipt_binding::ReceiptBindingError<CellError>,
    > {
        self.cell.resolve_accepted_receipt(reference, manifest)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.cell.load_link(key)
    }
    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, CellError> {
        self.cell
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }
    fn read_predicate(
        &self,
        predicate: &PredicateQuery,
        max_members: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<PredicateObject>>, CellError> {
        self.cell.read_predicate(predicate, max_members, max_bytes)
    }
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, RuntimeObject, RuntimeObject>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.stop(commit)
    }
    fn commit_action_quantity(
        &mut self,
        commit: CellCommit<FactEvent, RuntimeObject, RuntimeObject>,
        _requests: &[norn_ir::quantity_ledger::QuantityRequest],
        _idempotency_key: &str,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.stop(commit)
    }
    fn commit_action_references(
        &mut self,
        commit: CellCommit<FactEvent, RuntimeObject, RuntimeObject>,
        _proof: norn_kernel::action_commit::ActionReferenceReads,
        _quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.stop(commit)
    }
    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<FactEvent, RuntimeObject, RuntimeObject>,
        _proof: norn_kernel::cell_host::OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.stop(commit)
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
        self.cell.serialize_by(key)
    }
    fn publish(&mut self, _receipt: &CommitReceipt) -> Result<(), CellError> {
        Ok(())
    }
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
        assert!(applies(&large, &writes), "the target's 2500 waits");
        let untouched = vec![
            call_of("PR-9"),
            put_at("purchasing.Line", "A-1", json!({ "amount": "5" })),
        ];
        assert!(applies(&large, &untouched), "no target values: held");
        let unnamed = vec![put_at(
            "purchasing.PurchaseRequest",
            "PR-9",
            json!({ "amount": "5" }),
        )];
        assert!(applies(&large, &unnamed), "no call record: held");
    }

    /// #1469 S10: a decimal threshold compares exactly as written.
    #[test]
    fn a_decimal_threshold_compares_exactly() {
        let over = gate(Some(
            json!({ "property": "amount", "op": "gt", "value": "1000.10" }),
        ));
        assert!(!applies(&over, &request(&json!("1000.10"))));
        assert!(applies(&over, &request(&json!("1000.11"))));
        assert!(!applies(&over, &request(&json!("1000.100"))));
        let at_least = gate(Some(
            json!({ "property": "amount", "op": "ge", "value": "1000.10" }),
        ));
        assert!(applies(&at_least, &request(&json!("1000.10"))));
        assert!(!applies(&at_least, &request(&json!("1000.09"))));
        // A fractional JSON number is not exact: it holds the call.
        let inexact = gate(Some(
            json!({ "property": "amount", "op": "gt", "value": 2000.5 }),
        ));
        assert!(applies(&inexact, &request(&json!("5"))));
    }

    #[test]
    fn an_amount_threshold_holds_only_the_larger_requests() {
        let large = gate(Some(
            json!({ "property": "amount", "op": "gt", "value": "1000" }),
        ));
        assert!(applies(&large, &request(&json!("2500.00"))));
        assert!(applies(&large, &request(&json!(1000.01))));
        assert!(!applies(&large, &request(&json!("1000.00"))));
        assert!(!applies(&large, &request(&json!("990"))));
        // The kernel's own records never stand in for the call's record.
        assert!(!applies(&large, &request(&json!("12"))));
    }

    #[test]
    fn text_compares_by_equality_and_anything_else_holds_the_call() {
        let status = gate(Some(
            json!({ "property": "status", "op": "eq", "value": "submitted" }),
        ));
        assert!(applies(&status, &request(&json!("5"))));
        let other = gate(Some(
            json!({ "property": "status", "op": "ne", "value": "submitted" }),
        ));
        assert!(!applies(&other, &request(&json!("5"))));
        // Fail closed: an unordered comparison, a missing property, no record.
        let ordered = gate(Some(
            json!({ "property": "status", "op": "lt", "value": "z" }),
        ));
        assert!(applies(&ordered, &request(&json!("5"))));
        let missing = gate(Some(
            json!({ "property": "priority", "op": "eq", "value": "high" }),
        ));
        assert!(applies(&missing, &request(&json!("5"))));
        assert!(applies(&missing, &[]));
        assert!(applies(&gate(None), &request(&json!("5"))));
    }
}
