//! HOST-ACTION-1 (#805): one governed action service shared by every transport.
//!
//! A transport hands over the raw request body, the explicit query scope, the
//! action identity from its path and the actor its trusted principal seam
//! verified; nothing else. The service bounds and decodes the body exactly,
//! authorizes the call through the host's server policy, admits the scope
//! against the physical cell, resolves the action from the verified retained
//! definitions and dispatches to the kernel's `commit_action`, which decides
//! current- and proposed-state authorization, laws, preconditions and
//! idempotency. The receipt it returns follows D53: the raw receipt hash only
//! for a reader with whole-world Values, otherwise its keyed handle.
use crate::{
    authz::{AccessRequest, KernelAuthorizer, Operation, RefusalDetailRequest},
    host::DevHost,
    question_routes::{QuestionRuntime, handle_key, world_values},
    refusal::{Refusal, RefusalDiagnostic, RefusalSpan, codes},
};
use http::StatusCode;
use norn_cell::{MAIN_WORLD, Receipt, compute::lineage::keyed_receipt_handle, engine::SqlEngine};
use norn_ir::RuntimeValue;
use norn_kernel::{
    ACTION_CALL_TYPE, ActionCallRecord, ActionOutcome, DecisionTurn, LogicalTime, Refusable,
    TypeCatalog,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::{IntoParams, ToSchema};

/// Longest accepted request body, in bytes: the kernel's 256 KiB caller-input
/// bound plus room for the envelope. A longer body refuses before it is parsed.
pub const MAX_ACTION_BODY_BYTES: usize = 260 * 1024;
/// Longest idempotency key, in characters.
pub const MAX_IDEMPOTENCY_KEY_CHARS: usize = 128;
/// Longest idempotency key, in UTF-8 bytes.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;

/// `POST /v1/actions/{actionId}`: one call. The actor never travels in the body.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionRequest {
    /// Exact business arguments. A field declared `nornType: decimal` travels
    /// as its exact decimal text (`"12.500"`, VAL-EXACT-1); a number token
    /// there refuses `action.invalid_input`. Other number tokens are decoded
    /// digit for digit.
    #[schema(schema_with = action_input_schema)]
    pub input: RuntimeValue,
    /// The caller's retry identity: 1 to 128 characters, at most 256 UTF-8 bytes.
    pub idempotency_key: String,
}

/// Explicit scope and an optional definitions pin, from the query string.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct ActionQuery {
    /// Organization, matched against physical scope.
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Environment, matched against physical scope.
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
    /// The definitions version the caller read the action from. A different
    /// active version refuses `defs.stale` (409); absent calls the active one.
    #[param(nullable = false)]
    pub version: Option<u64>,
}

/// Whether this call committed now or replayed an earlier identical call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionCallOutcome {
    /// The call committed now.
    Committed,
    /// An identical earlier call committed; nothing new was written.
    Replayed,
}

/// The commit that carried the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionCommit {
    /// The receipt hash for a reader with Values over the whole cell, else
    /// its keyed handle (D53): the raw hash chains every earlier event.
    pub receipt_hash: String,
    /// The commit's ledger position.
    pub commit_seq: u64,
    /// The commit's logical time (Unix milliseconds).
    pub recorded_at_ms: u64,
}

/// A committed or replayed call: the target and the commit that wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionReceipt {
    /// Committed now, or replayed.
    pub outcome: ActionCallOutcome,
    /// The target's concrete object type.
    pub object_type: String,
    /// The target's id.
    pub object_id: String,
    /// The target's version as the original commit wrote it.
    pub version: u64,
    /// The original commit, the same on every replay.
    pub receipt: ActionCommit,
}

fn action_input_schema() -> utoipa::openapi::Ref {
    utoipa::openapi::Ref::from_schema_name("NornActionInput")
}

/// The one opaque action denial: the same document for a missing or
/// unverified actor, a denied policy, a foreign or missing scope and an
/// unknown action, so a refusal never tells an ungranted action from an
/// absent one.
#[must_use]
pub fn action_denied() -> Refusal {
    Refusal::new(
        StatusCode::FORBIDDEN,
        codes::AUTHZ_DENIED,
        "the action request is not authorized",
        "access to the requested action is denied",
    )
}

fn invalid_at(pointer: &str, message: &str) -> Refusal {
    let mut refusal = Refusal::invalid(message.to_owned());
    refusal.diagnostics = vec![RefusalDiagnostic {
        code: codes::INVALID_REQUEST.to_owned(),
        severity: "error".to_owned(),
        message: message.to_owned(),
        span: Some(RefusalSpan {
            pointer: pointer.to_owned(),
        }),
        law: None,
    }];
    refusal
}

/// Bound and decode one request body exactly. Duplicate or unknown members,
/// a non-object input and a malformed key refuse; numbers keep their tokens.
/// # Errors
/// `request.too_large` (413) above [`MAX_ACTION_BODY_BYTES`], else
/// `request.invalid` (400) with a pointer into the body.
pub fn decode_action_request(body: &str) -> Result<ActionRequest, Refusal> {
    if body.len() > MAX_ACTION_BODY_BYTES {
        return Err(Refusal::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request.too_large",
            "the request body exceeds the host bound",
            "an action call body is at most 260 KiB",
        ));
    }
    let request: ActionRequest = serde_json::from_str(body).map_err(|error| {
        Refusal::invalid(format!("the request body is not an action call: {error}"))
    })?;
    if !matches!(request.input, RuntimeValue::Object(_)) {
        return Err(invalid_at("/input", "input must be a JSON object"));
    }
    let key = request.idempotency_key.as_str();
    if key.trim().is_empty()
        || key.chars().count() > MAX_IDEMPOTENCY_KEY_CHARS
        || key.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || key.chars().any(char::is_control)
    {
        return Err(invalid_at(
            "/idempotencyKey",
            "idempotencyKey must be 1 to 128 printable characters and at most 256 bytes",
        ));
    }
    Ok(request)
}

/// What an action call answers: its receipt, or, for a call of an
/// approval-gated action (APPROVAL-1), the decision it waits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionAnswer {
    /// Committed now, or replayed (200).
    Receipt(ActionReceipt),
    /// Parked behind an approval; nothing was committed (202).
    Pending(crate::approvals::PendingAction),
}

/// Call one admitted action as the verified caller: it commits, or an
/// identical earlier call replays with its original receipt. While an
/// approval gates the action (APPROVAL-1), a call that would commit parks
/// instead, as one open decision, and answers 202 with it; a call that would
/// be refused is refused exactly as it would be ungated, and parks nothing.
/// # Errors
/// One opaque 403, decided before any storage read, for a missing or
/// unverified caller, a denied policy, a foreign scope or an unknown action;
/// the same opaque 403 for every visibility or permission refusal the kernel
/// decides (an invisible or absent target, a denied policy decision);
/// 409 `defs.stale` for a stale definitions pin; otherwise the kernel's typed
/// refusal, its detail disclosed only under an explicit diagnostic grant;
/// for a gated call whose decision was resolved, 409 `approval.rejected` or
/// 410 `approval.expired`. A refusal commits nothing.
#[utoipa::path(post, path = "/v1/actions/{actionId}", params(ActionQuery, ("actionId" = String, Path, description = "The admitted action identity.")), request_body = ActionRequest, responses((status = 200, description = "The call committed, or replayed an identical earlier call.", body = ActionReceipt), (status = 202, description = "An approval gates the action: the call waits for a person's decision, and nothing was committed.", body = crate::approvals::PendingAction), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 409, description = "The request was refused: a stale pin, a conflicting retry, or a rejected approval.", body = Refusal), (status = 410, description = "The approval the call waited on expired.", body = Refusal), (status = 413, description = "The request was refused: the body exceeds the host bound.", body = Refusal), (status = 422, description = "The request was refused: a law or precondition does not hold.", body = Refusal), (status = 429, description = "The request was refused: a declared limit was exceeded.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn call_action<E: SqlEngine>(
    host: &DevHost<E>,
    query: &ActionQuery,
    actor: Option<&str>,
    action_id: &str,
    request: &ActionRequest,
    disclosure: &dyn QuestionRuntime<E>,
    now: LogicalTime,
) -> Result<ActionAnswer, Refusal> {
    // `disclosure` is the cell's read runtime: its handle key and collection
    // grants decide which receipt coordinate the caller sees (D53).
    let actor = actor
        .filter(|actor| !actor.trim().is_empty())
        .ok_or_else(action_denied)?;
    let epoch = host.definition_read();
    let authorized = host.authorizer.authorize(&AccessRequest {
        scope: host.scope(),
        actor: Some(actor),
        operation: Operation::CallAction(action_id.to_owned()),
    });
    let scoped = host
        .scope()
        .admit(query.org_id.as_deref(), query.environment_id.as_deref());
    if authorized.is_err() || scoped.is_err() {
        return Err(action_denied());
    }
    let _epoch = epoch?;
    let key = handle_key(disclosure)?;
    let RuntimeValue::Object(input) = &request.input else {
        return Err(invalid_at("/input", "input must be a JSON object"));
    };
    let mut cell = host.cell()?;
    let installed = cell
        .store()
        .read()
        .installed_definitions(None)
        .map_err(Refusal::host)?
        .ok_or_else(action_denied)?;
    let admitted = crate::action_admission::RetainedActions::load(&installed, host.scope())?;
    let (artifact, action) = admitted
        .actions
        .iter()
        .find(|(_, action)| action.action_id == action_id)
        .ok_or_else(action_denied)?;
    if query.version.is_some_and(|pin| pin != installed.version) {
        return Err(Refusal::new(
            StatusCode::CONFLICT,
            "defs.stale",
            "the definitions changed; refresh the action catalog",
            "the call pins a definitions version that is no longer active",
        ));
    }
    let model = cell.store().model().clone();
    let catalog = TypeCatalog::new(model.object_types())
        .with_property_types(model.property_types())
        .with_interfaces(model.interfaces())
        .with_governing_laws(model.law_artifacts())
        .with_action_rules(&admitted.rules);
    let now = turn_clock(&mut cell, now)?;
    let kernel_call = norn_kernel::ActionCall {
        action,
        action_artifact_id: &artifact.artifact_id,
        action_version: &artifact.version,
        scope: BTreeMap::from([
            ("orgId".into(), host.scope().org_id.clone()),
            ("environmentId".into(), host.scope().environment_id.clone()),
        ]),
        actor: actor.to_owned(),
        input: input.clone(),
        idempotency_key: request.idempotency_key.clone(),
        object_type: None,
        turn: DecisionTurn {
            id: format!("{action_id}/{}", request.idempotency_key),
            now,
            // L5 (Fable S2): the turn's world is the store's pin, never assumed.
            world: cell.store().world(),
            consumes: None,
        },
    };
    let outcome = match through_gate(host, &mut cell, &catalog, &kernel_call)? {
        Gated::Pending(pending) => return Ok(ActionAnswer::Pending(pending)),
        Gated::Replayed(outcome) => *outcome,
        Gated::Commit => norn_kernel::commit_action(
            &mut *cell,
            &catalog,
            &kernel_call,
            &KernelAuthorizer(host.authorizer.as_ref()),
        )
        .map_err(|error| kernel_refusal(host, actor, action_id, &error))?,
    };
    let store = cell.store();
    let raw_hash = world_values(host, disclosure, store, actor);
    caller_receipt(store, action_id, request, outcome, raw_hash, key).map(ActionAnswer::Receipt)
}

/// The caller's view of a kernel refusal. Visibility and permission refusals
/// are the one opaque action document: the kernel decides them per target, so
/// a typed 404 or 403 would tell a known action from an unknown one. Laws,
/// preconditions, conflicts, invalid input and limits stay typed.
fn kernel_refusal<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    action_id: &str,
    error: &norn_kernel::ActionRefusal<norn_cell::CellError>,
) -> Refusal {
    let refusal = error.refusal();
    if matches!(
        refusal.category,
        norn_kernel::RefusalCategory::Visibility | norn_kernel::RefusalCategory::Permission
    ) {
        return action_denied();
    }
    let internal = refusal.category == norn_kernel::RefusalCategory::Internal;
    let projected = crate::refusal_disclosure::project_refusal(
        host.authorizer.as_ref(),
        &RefusalDetailRequest {
            scope: host.scope(),
            actor,
            action_id,
        },
        refusal,
    );
    // INCIDENT-1: the kernel keeps only its fixed internal message; the
    // operator's log gets the host error's whole source chain.
    if internal {
        projected.with_cause(crate::refusal::error_chain(error))
    } else {
        projected
    }
}

/// What an approval gate (APPROVAL-1) does with a call.
enum Gated {
    /// No gate, or a decider approved exactly this call: commit it.
    Commit,
    /// An identical earlier call committed: its replay.
    Replayed(Box<ActionOutcome>),
    /// It parked, or waits on the decision it parked as.
    Pending(crate::approvals::PendingAction),
}

/// APPROVAL-1: while an approval gates the action, the kernel's whole
/// pipeline decides the call for its caller against a host that stops at the
/// commit point. A call that reaches it parks, unless a decider already
/// approved exactly this call; a refusal is the ungated one, and parks
/// nothing.
fn through_gate<E: SqlEngine>(
    host: &DevHost<E>,
    cell: &mut crate::host::ActionCell<E>,
    catalog: &TypeCatalog<'_>,
    kernel_call: &norn_kernel::ActionCall<'_>,
) -> Result<Gated, Refusal> {
    let action_id = kernel_call.action.action_id.as_str();
    let Some(gate) = crate::approvals::gate(cell, action_id)? else {
        return Ok(Gated::Commit);
    };
    let dry = norn_kernel::commit_action(
        &mut crate::approvals::WouldCommit(&*cell),
        catalog,
        kernel_call,
        &KernelAuthorizer(host.authorizer.as_ref()),
    );
    match dry {
        Ok(replayed) => Ok(Gated::Replayed(Box::new(replayed))),
        Err(error) if crate::approvals::would_have_committed(&error) => {
            let requester = host
                .authorizer
                .verified_principal(&kernel_call.actor)
                .ok_or_else(action_denied)?;
            let parked = crate::approvals::ParkedCall {
                action: action_id.to_owned(),
                input: kernel_call.input.clone(),
                idempotency_key: kernel_call.idempotency_key.clone(),
                requester,
            };
            Ok(
                match crate::approvals::hold(cell, &gate, &parked, kernel_call.turn.now)? {
                    crate::approvals::Held::Pending(pending) => Gated::Pending(pending),
                    crate::approvals::Held::Approved => Gated::Commit,
                },
            )
        }
        Err(error) => Err(kernel_refusal(host, &kernel_call.actor, action_id, &error)),
    }
}

/// The kernel turn's time: the cell clock after advancing it to `now` and the
/// stored floor. It is the clock the receipt records, so a naming series and
/// the receipt agree even after a refused call moved the cell clock past `now`
/// without moving the floor.
pub(crate) fn turn_clock<E: SqlEngine>(
    cell: &mut crate::host::ActionCell<E>,
    now: LogicalTime,
) -> Result<LogicalTime, Refusal> {
    let floor = cell.store().read().clock_floor().map_err(Refusal::host)?;
    cell.advance_to(now.max(LogicalTime::from_millis(floor)));
    Ok(cell.now())
}

/// The caller's view of `outcome`: the receipt it committed or replayed,
/// its hash raw only when `raw_hash` (whole-world Values), else keyed.
fn caller_receipt<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    action_id: &str,
    request: &ActionRequest,
    outcome: ActionOutcome,
    raw_hash: bool,
    key: &[u8],
) -> Result<ActionReceipt, Refusal> {
    let (outcome, record, receipt) = match outcome {
        ActionOutcome::Committed { receipt, record } => {
            let receipt = store
                .read()
                .receipt_at(receipt.seq.0)
                .map_err(Refusal::host)?
                .ok_or_else(|| Refusal::host("the committed receipt is missing"))?;
            (ActionCallOutcome::Committed, record, receipt)
        }
        ActionOutcome::Replayed { record } => {
            let receipt = original_receipt(store, action_id, &request.idempotency_key)?;
            (ActionCallOutcome::Replayed, record, receipt)
        }
    };
    let version = target_version(store, &receipt, &record)?;
    let receipt_hash = if raw_hash {
        receipt.hash.clone()
    } else {
        keyed_receipt_handle(key, &receipt.hash)
    };
    Ok(ActionReceipt {
        outcome,
        object_type: record.object.object_type.as_str().to_owned(),
        object_id: record.object.key,
        version,
        receipt: ActionCommit {
            receipt_hash,
            commit_seq: receipt.body.commit_seq,
            recorded_at_ms: receipt.body.logical_time,
        },
    })
}

/// The authenticated receipt of the commit that wrote this call's
/// idempotency record: a replay reports the original commit, never a new one.
fn original_receipt<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    action_id: &str,
    idempotency_key: &str,
) -> Result<Receipt, Refusal> {
    let record_key = format!("{action_id}/{idempotency_key}");
    let read = store.read();
    let version = read
        .version(MAIN_WORLD, ACTION_CALL_TYPE, &record_key)
        .map_err(Refusal::host)?;
    read.receipt_for_fact_version(MAIN_WORLD, ACTION_CALL_TYPE, &record_key, version)
        .map_err(Refusal::host)
}

/// The target's version as `receipt` wrote it. A call that commits without
/// rewriting its target (a row mutation with an empty parent patch and no row
/// operations) is still a success: it reports the target's current version,
/// which a fresh commit reads under the same cell lock.
fn target_version<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    receipt: &Receipt,
    record: &ActionCallRecord,
) -> Result<u64, Refusal> {
    let object_type = record.object.object_type.as_str();
    let written = receipt
        .body
        .written
        .iter()
        .find(|row| row.object_type == object_type && row.key == record.object.key);
    match written {
        Some(row) => Ok(row.version),
        None => store
            .read()
            .version(MAIN_WORLD, object_type, &record.object.key)
            .map_err(Refusal::host),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "actions_tests.rs"]
mod tests;
