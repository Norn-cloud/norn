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

fn present_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Optional provenance trace context attached to an action call and retained
/// on its committed receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TraceContext {
    /// Opaque correlation identifier tying multi-step causes and effects
    /// together (1 to 256 UTF-8 bytes, no control characters or outer whitespace).
    #[schema(min_length = 1, max_length = 256)]
    pub trace_id: String,
    /// Optional reference to the goal, prompt, commitment, or decision that
    /// initiated the action (1 to 512 UTF-8 bytes, no control characters or
    /// outer whitespace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, min_length = 1, max_length = 512)]
    pub intent_ref: Option<String>,
    /// Optional content-addressed SHA-256 digest (`sha256:<64 lower-case hex>`)
    /// of an off-ledger agent or user session transcript. Raw transcript text
    /// is never stored in the ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, pattern = "^sha256:[0-9a-f]{64}$")]
    pub session_digest: Option<String>,
}

impl<'de> Deserialize<'de> for TraceContext {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        norn_ir::TraceContext::deserialize(deserializer).map(Self::from)
    }
}

impl TraceContext {
    /// Validate and convert this wire trace context to the canonical kernel
    /// [`norn_ir::TraceContext`].
    ///
    /// # Errors
    /// `request.invalid` (400) pointing to the malformed trace member.
    pub fn to_ir(&self) -> Result<norn_ir::TraceContext, Refusal> {
        let ir = norn_ir::TraceContext {
            trace_id: self.trace_id.clone(),
            intent_ref: self.intent_ref.clone(),
            session_digest: self
                .session_digest
                .as_ref()
                .map(|digest| norn_ir::function::Digest(digest.clone())),
        };
        ir.validate()
            .map_err(|error| invalid_at(error.pointer(), &error.to_string()))?;
        Ok(ir)
    }
}

impl From<norn_ir::TraceContext> for TraceContext {
    fn from(value: norn_ir::TraceContext) -> Self {
        Self {
            trace_id: value.trace_id,
            intent_ref: value.intent_ref,
            session_digest: value.session_digest.map(|digest| digest.0),
        }
    }
}

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
    /// Optional provenance trace context persisted on the committed receipt.
    #[serde(default, deserialize_with = "present_non_null")]
    #[schema(nullable = false)]
    pub trace: Option<TraceContext>,
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
    /// Optional provenance trace context retained from the original call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub trace: Option<TraceContext>,
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
    if let Some(trace) = &request.trace {
        trace.to_ir()?;
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
#[utoipa::path(post, path = "/v1/actions/{actionId}", params(ActionQuery, ("actionId" = String, Path, description = "The admitted action identity.")), request_body = ActionRequest, responses((status = 200, description = "The call committed, or replayed an identical earlier call.", body = ActionReceipt), (status = 202, description = "An approval gates the action: the call waits for a person's decision, and nothing was committed.", body = crate::approvals::PendingAction), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 409, description = "The request was refused: a stale pin, a conflicting retry, or a rejected approval.", body = Refusal), (status = 410, description = "The approval the call waited on expired.", body = Refusal), (status = 413, description = "The request was refused: the body exceeds the host bound.", body = Refusal), (status = 422, description = "The request was refused: a law or precondition does not hold.", body = Refusal), (status = 429, description = "The request was refused: a declared limit was exceeded.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal), (status = 503, description = "The request was refused (`action.receipts_unconfigured`, retryable): the host keys no receipt handles, so no call can commit. Told only to a verified member calling an action the scope's policy governs; anyone else gets the 403.", body = Refusal)))]
pub fn call_action<E: SqlEngine>(
    host: &DevHost<E>,
    query: &ActionQuery,
    actor: Option<&str>,
    action_id: &str,
    request: &ActionRequest,
    disclosure: &dyn QuestionRuntime<E>,
    now: LogicalTime,
) -> Result<ActionAnswer, Refusal> {
    // S5a: a flow step's call key is the host's own identity for that step
    // attempt's call. A direct call never takes it, so it can neither park,
    // consume an approval nor commit a record a step would take as its
    // outcome.
    if crate::flow_driver::is_step_call_key(&request.idempotency_key) {
        return Err(step_key_reserved());
    }
    // AUTOMATION-KEY-1 (S5a-equivalent): automation-owned idempotency keys are
    // reserved for the automation runtime and never accepted on the public route.
    if crate::automation_definitions::is_automation_call_key(&request.idempotency_key) {
        return Err(automation_key_reserved());
    }
    // The transport's own verified principal for this request: the only one
    // that may consume an approval of its call (S2).
    let principal = actor.and_then(|actor| host.authorizer.verified_principal(actor));
    call_action_bound(
        host,
        &ActionCall {
            query,
            actor,
            principal: principal.as_ref(),
            attempt: None,
            action_id,
            request,
            now,
        },
        disclosure,
        None,
    )
}

/// S5a: a step's call key is reserved for the flow step attempt's own
/// dispatch of it.
fn step_key_reserved() -> Refusal {
    invalid_at(
        "/idempotencyKey",
        "idempotencyKey is reserved for a flow step's own call",
    )
}

/// AUTOMATION-KEY-1: an automation-owned call key is reserved for the
/// automation runtime's own dispatch.
fn automation_key_reserved() -> Refusal {
    invalid_at(
        "/idempotencyKey",
        "idempotencyKey is reserved for the automation runtime",
    )
}

/// S5a: the record under a step's call key was not committed by that step
/// attempt's own dispatch, so it is never the step's outcome.
pub(crate) fn foreign_step_record() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        "action.idempotency_conflict",
        "another call holds this step's call key",
        "the record under the step's call key was not committed by the step's own call",
    )
}

/// S5a: a step attempt's call answers only with the record its own dispatch
/// committed, now or on an earlier try, never another's under its key.
fn require_owned<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    action_id: &str,
    idempotency_key: &str,
    attempt: Option<&crate::approvals::StepAttempt>,
) -> Result<(), Refusal> {
    match attempt {
        Some(attempt) if !crate::flow_driver::owns(store, action_id, idempotency_key, attempt)? => {
            Err(foreign_step_record())
        }
        _owned_or_direct => Ok(()),
    }
}

/// S5a: `attempt`, when the trusted invocation names one, still waits on
/// exactly the call keyed `idempotency_key`.
fn require_making<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    attempt: Option<&crate::approvals::StepAttempt>,
    idempotency_key: &str,
) -> Result<(), Refusal> {
    match attempt {
        Some(attempt) if !crate::flow_driver::attempt_makes(store, attempt, idempotency_key)? => {
            Err(crate::approvals::attempt_ended())
        }
        _making_or_direct => Ok(()),
    }
}

/// S5a: the step-key namespace is fenced at bound dispatch, under the
/// caller's hold: a call under a step's call key runs only for the trusted
/// invocation of the step attempt that makes it now. A direct call held
/// under such a key before the fence (its approval's dispatch) never runs,
/// and that refusal is recorded for its approval.
fn fence_step_key<E: SqlEngine>(
    cell: &mut crate::host::ActionCell<E>,
    idempotency_key: &str,
    attempt: Option<&crate::approvals::StepAttempt>,
    approval: Option<&str>,
) -> Result<(), Refusal> {
    if crate::automation_definitions::is_automation_call_key(idempotency_key) {
        return Err(crate::approvals::refused_under(
            cell,
            approval,
            automation_key_reserved(),
        ));
    }
    if !crate::flow_driver::is_step_call_key(idempotency_key) {
        return Ok(());
    }
    let refused = match attempt {
        None => step_key_reserved(),
        Some(_making) => match require_making(cell.store(), attempt, idempotency_key) {
            Ok(()) => return Ok(()),
            Err(refusal) => refusal,
        },
    };
    Err(crate::approvals::refused_under(cell, approval, refused))
}

/// One action call, as [`call_action`] takes it.
pub(crate) struct ActionCall<'a> {
    pub(crate) query: &'a ActionQuery,
    /// The subject the call runs as.
    pub(crate) actor: Option<&'a str>,
    /// S2: the authenticated principal of the request making the call, kept
    /// apart from `actor`: an approval of the call is consumed only when it
    /// is the parked requester. `None` for a call run under recorded
    /// authority (a run's starter), which never consumes an approval.
    pub(crate) principal: Option<&'a norn_cell::flow_run::RunStarter>,
    /// S5a: the flow step attempt making the call, carried only by the
    /// trusted invocations that run a step's call (the flow's dispatch, and
    /// an approval's dispatch of the call it holds), never derived from the
    /// idempotency key. `None` for a direct call.
    pub(crate) attempt: Option<&'a crate::approvals::StepAttempt>,
    pub(crate) action_id: &'a str,
    pub(crate) request: &'a ActionRequest,
    pub(crate) now: LogicalTime,
}

/// [`call_action`], for an approved call when `approved` (the approved
/// parked command's id) is given: inside the same definitions epoch and cell
/// hold as the dispatch, before anything writes, the call must park as
/// exactly that command under the gate and the action as defined now, else
/// it is refused `approval.definition_changed` and nothing is written
/// (APPROVAL-REVISION-1). A refusal that decides an approved call is
/// recorded as final (APPROVAL-RECOVERY-1). S5a: a call under a step's
/// call key runs only for the trusted dispatch of the step attempt that
/// makes it, and answers only with a record that attempt's own dispatch
/// committed.
pub(crate) fn call_action_bound<E: SqlEngine>(
    host: &DevHost<E>,
    invocation: &ActionCall<'_>,
    disclosure: &dyn QuestionRuntime<E>,
    approved: Option<&str>,
) -> Result<ActionAnswer, Refusal> {
    let ActionCall {
        query,
        actor,
        principal,
        attempt,
        action_id,
        request,
        now,
    } = *invocation;
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
    let trace = request
        .trace
        .as_ref()
        .map(TraceContext::to_ir)
        .transpose()?;
    let mut cell = host.cell()?;
    // ADMIT-CACHE-1 (#1533): verified once per definitions version.
    let (installed_version, admitted) = host
        .retained(&cell.store().read())?
        .ok_or_else(action_denied)?;
    let (artifact, action) = admitted
        .actions
        .iter()
        .find(|(_, action)| action.action_id == action_id)
        .ok_or_else(action_denied)?;
    if query.version.is_some_and(|pin| pin != installed_version) {
        return Err(stale_pin(&mut cell, approved, principal, attempt)?);
    }
    let model = cell.store().model().clone();
    // ADMIT-CACHE-1: the model's definition work is derived once per model.
    let catalog = model
        .type_catalog()
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
        trace,
    };
    // APPROVAL-REVISION-1: an approval binds to the action as defined now,
    // with what it writes.
    let action_digest =
        crate::action_admission::effective_digest(&admitted, action, &model, &catalog)?;
    let outcome = match through_gate(
        host,
        &mut cell,
        &catalog,
        &kernel_call,
        &Admitted {
            action_digest,
            approved,
            principal,
            attempt,
            definitions_version: query.version,
        },
    )? {
        Gated::Pending(pending) => return Ok(ActionAnswer::Pending(pending)),
        Gated::Replayed(outcome) => *outcome,
        Gated::Commit(approval) => {
            let approval = approval.as_deref();
            commit_call(host, &mut cell, &catalog, &kernel_call, approval, attempt)?
        }
    };
    require_owned(cell.store(), action_id, &request.idempotency_key, attempt)?;
    let store = cell.store();
    let raw_hash = world_values(host, disclosure, store, actor);
    caller_receipt(store, action_id, request, outcome, raw_hash, key).map(ActionAnswer::Receipt)
}

/// Commit a call past its gate, as the caller gets it: under an approval,
/// the commit names the approval it consumes in its own transaction (S6c),
/// and a refusal that decides the approved call is recorded. A step
/// attempt's call names that attempt as its owner the same way (S5a).
fn commit_call<E: SqlEngine>(
    host: &DevHost<E>,
    cell: &mut crate::host::ActionCell<E>,
    catalog: &TypeCatalog<'_>,
    kernel_call: &norn_kernel::ActionCall<'_>,
    approval: Option<&str>,
    attempt: Option<&crate::approvals::StepAttempt>,
) -> Result<ActionOutcome, Refusal> {
    let kernel_authorizer = KernelAuthorizer(host.authorizer.as_ref());
    let commit = |cell: &mut crate::host::ActionCell<E>| {
        norn_kernel::commit_action(cell, catalog, kernel_call, &kernel_authorizer).map_err(
            |error| {
                kernel_refusal(
                    host,
                    &kernel_call.actor,
                    &kernel_call.action.action_id,
                    &error,
                )
            },
        )
    };
    let mut events = Vec::new();
    if let Some(decision) = approval {
        events.push(
            norn_cell::decision_ledger::ApprovalConsumed {
                decision_id: decision.to_owned(),
            }
            .event()
            .map_err(Refusal::host)?,
        );
    }
    if let Some(attempt) = attempt {
        events.push(
            norn_cell::decision_ledger::StepCallOwner {
                run_id: attempt.run_id.clone(),
                command_id: attempt.command.clone(),
                generation: attempt.generation,
            }
            .event()
            .map_err(Refusal::host)?,
        );
    }
    let committed = if events.is_empty() {
        commit(cell)
    } else {
        cell.attributed(events, commit)
    };
    committed.map_err(|refusal| crate::approvals::refused_under(cell, approval, refusal))
}

/// `defs.stale`: the call pins a definitions version that is no longer
/// active. APPROVAL-PIN-1: an approved call pinned to another catalog never
/// runs under its approval, and that refusal is recorded for it.
fn stale_pin<E: SqlEngine>(
    cell: &mut crate::host::ActionCell<E>,
    approved: Option<&str>,
    principal: Option<&norn_cell::flow_run::RunStarter>,
    attempt: Option<&crate::approvals::StepAttempt>,
) -> Result<Refusal, Refusal> {
    let stale = Refusal::new(
        StatusCode::CONFLICT,
        "defs.stale",
        "the definitions changed; refresh the action catalog",
        "the call pins a definitions version that is no longer active",
    );
    let approval = approved
        .map(|command| crate::decisions::decision_id(command, 1))
        .transpose()?;
    // S2: only the requester's own request decides its approved call.
    if let Some(decision) = &approval {
        crate::approvals::require_requester(cell, decision, principal, attempt)?;
    }
    Ok(crate::approvals::refused_under(
        cell,
        approval.as_deref(),
        stale,
    ))
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
    if matches!(
        error,
        norn_kernel::ActionRefusal::Decision(norn_kernel::DecisionRefusal::Host(
            norn_cell::CellError::AutomationFanoutExceeded { .. }
        ))
    ) {
        return Refusal::new(
            StatusCode::TOO_MANY_REQUESTS,
            "automation.fanout_exceeded",
            "the commit matched too many automation triggers",
            "this commit exceeded the per-commit automation trigger fan-out limit; nothing was committed",
        );
    }
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

/// What the gate weighs besides the kernel call: the action's digest, the
/// approved command an approver's dispatch runs, the request's
/// authenticated principal, the trusted step attempt making the call (S5a)
/// and the call's definitions pin.
struct Admitted<'a> {
    action_digest: String,
    approved: Option<&'a str>,
    principal: Option<&'a norn_cell::flow_run::RunStarter>,
    attempt: Option<&'a crate::approvals::StepAttempt>,
    definitions_version: Option<u64>,
}

/// What an approval gate (APPROVAL-1) does with a call.
enum Gated {
    /// No gate, or a decider approved exactly this call: commit it, under
    /// the approving decision when one approved it.
    Commit(Option<String>),
    /// An identical earlier call committed: its replay.
    Replayed(Box<ActionOutcome>),
    /// It parked, or waits on the decision it parked as.
    Pending(crate::approvals::PendingAction),
}

/// APPROVAL-1: while an approval gates the action, the kernel's whole
/// pipeline decides the call for its caller against a host that stops at the
/// commit point. A call that reaches it parks, unless a decider already
/// approved exactly this call; a refusal is the ungated one, and parks
/// nothing. APPROVAL-RECOVERY-1: an approved call whose refusal is recorded
/// gets that refusal and is never decided again; a refusal that decides it
/// now is recorded, under this same cell hold. S2: an approval is consumed
/// only by its parked requester's authenticated principal, compared under
/// this hold before anything is recorded or committed.
fn through_gate<E: SqlEngine>(
    host: &DevHost<E>,
    cell: &mut crate::host::ActionCell<E>,
    catalog: &TypeCatalog<'_>,
    kernel_call: &norn_kernel::ActionCall<'_>,
    admitted: &Admitted<'_>,
) -> Result<Gated, Refusal> {
    let Admitted {
        ref action_digest,
        approved,
        principal,
        attempt,
        definitions_version,
    } = *admitted;
    let action_id = kernel_call.action.action_id.as_str();
    let gate = crate::approvals::gate(cell, action_id)?;
    // The parked command this exact call is under the gate and the action as
    // defined now.
    let command = gate
        .as_ref()
        .map(|gate| {
            crate::approvals::call_command_id(
                gate,
                &crate::approvals::CallParts {
                    action: action_id,
                    action_digest,
                    idempotency_key: &kernel_call.idempotency_key,
                    requester: &kernel_call.actor,
                    input: &kernel_call.input,
                    definitions_version,
                    trace: kernel_call.trace.as_ref(),
                },
            )
        })
        .transpose()?;
    let approval = approval_of(cell, approved, command.as_deref(), principal, attempt)?;
    if let Some(decision) = &approval
        && let Some(refused) = crate::approvals::recorded_refusal(cell, decision)?
    {
        return Err(refused);
    }
    if let Some(decision) = &approval
        && !crate::approvals::attempt_live(cell, decision)?
    {
        return Err(crate::approvals::refuse(
            cell,
            decision,
            crate::approvals::attempt_ended(),
        )?);
    }
    let key = &kernel_call.idempotency_key;
    fence_step_key(cell, key, attempt, approval.as_deref())?;
    let Some(gate) = gate else {
        return Ok(Gated::Commit(None));
    };
    let mut dry_run = crate::approvals::WouldCommit::new(&*cell);
    let dry = norn_kernel::commit_action(
        &mut dry_run,
        catalog,
        kernel_call,
        &KernelAuthorizer(host.authorizer.as_ref()),
    );
    let writes = dry_run.into_writes();
    match dry {
        Ok(replayed) => Ok(Gated::Replayed(Box::new(replayed))),
        // APPROVAL-WHEN-1: a call whose record the gate's `when` excludes
        // commits as an ungated call does; an approved one still names the
        // approval it consumes (S6c), so its call reads committed.
        Err(error)
            if crate::approvals::would_have_committed(&error)
                && !crate::approvals::applies(&gate, &writes, |object_type, property| {
                    crate::approvals::semantics_in(cell.store().model(), object_type, property)
                }) =>
        {
            Ok(Gated::Commit(approval))
        }
        Err(error) if crate::approvals::would_have_committed(&error) => {
            if approval.is_some() {
                return Ok(Gated::Commit(approval));
            }
            let requester = host
                .authorizer
                .verified_principal(&kernel_call.actor)
                .ok_or_else(action_denied)?;
            // S5a: a call parks for a step attempt only while that attempt
            // still waits on exactly this call, judged under this hold.
            require_making(cell.store(), attempt, key)?;
            let parked = crate::approvals::ParkedCall {
                action: action_id.to_owned(),
                input: kernel_call.input.clone(),
                idempotency_key: kernel_call.idempotency_key.clone(),
                requester,
                action_digest: action_digest.clone(),
                definitions_version,
                // S6b: the step attempt making this call, recorded now.
                attempt: attempt.cloned(),
                trace: kernel_call.trace.clone(),
            };
            Ok(
                match crate::approvals::hold(cell, &gate, &parked, kernel_call.turn.now)? {
                    crate::approvals::Held::Pending(pending) => Gated::Pending(pending),
                    crate::approvals::Held::Approved => Gated::Commit(None),
                },
            )
        }
        Err(error) => {
            let refusal = kernel_refusal(host, &kernel_call.actor, action_id, &error);
            Err(crate::approvals::refused_under(
                cell,
                approval.as_deref(),
                refusal,
            ))
        }
    }
}

/// The approval a call runs under, if one does: the approver's dispatch's
/// own (`approved`), or the approval of exactly this call's parked command.
/// S2: consumed only by its parked requester's authenticated principal;
/// S5a: and only by the invocation it was parked for (its step attempt, or
/// none for a direct call).
/// APPROVAL-REVISION-1: an approver's try is fenced here, under the epoch
/// and the cell the dispatch holds, before anything parks or writes.
fn approval_of<E: SqlEngine>(
    cell: &mut crate::host::ActionCell<E>,
    approved: Option<&str>,
    command: Option<&str>,
    principal: Option<&norn_cell::flow_run::RunStarter>,
    attempt: Option<&crate::approvals::StepAttempt>,
) -> Result<Option<String>, Refusal> {
    let approval = match (approved, command) {
        (Some(approved), _command) => Some(crate::decisions::decision_id(approved, 1)?),
        (None, Some(command)) => crate::approvals::approving_decision(cell, command)?,
        (None, None) => None,
    };
    if let Some(decision) = &approval {
        crate::approvals::require_requester(cell, decision, principal, attempt)?;
    }
    if let (Some(approved), Some(decision)) = (approved, &approval)
        && command != Some(approved)
    {
        return Err(crate::approvals::refuse(
            cell,
            decision,
            crate::approvals::definition_changed(),
        )?);
    }
    Ok(approval)
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
        trace: record.trace.map(TraceContext::from),
    })
}

/// S6c: the commit that wrote the idempotency record of `action_id`'s call
/// keyed `idempotency_key`.
///
/// # Errors
/// A host failure: the record or its receipt is missing.
pub(crate) fn record_commit_seq<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    action_id: &str,
    idempotency_key: &str,
) -> Result<u64, Refusal> {
    Ok(original_receipt(store, action_id, idempotency_key)?
        .body
        .commit_seq)
}

/// The target `record`'s call wrote and its version as that commit wrote
/// it: what a run's action step takes as its output when the call committed
/// (APPROVAL-RECOVERY-1).
///
/// # Errors
/// A host failure.
pub(crate) fn recorded_target<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    record: &ActionCallRecord,
    idempotency_key: &str,
) -> Result<(String, String, u64), Refusal> {
    let receipt = original_receipt(store, &record.action_id, idempotency_key)?;
    let version = target_version(store, &receipt, record)?;
    Ok((
        record.object.object_type.as_str().to_owned(),
        record.object.key.clone(),
        version,
    ))
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
/// operations) is still a success: it reports the target's version as of
/// that commit, never one a later write made (S5b).
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
            .version_as_of(
                MAIN_WORLD,
                object_type,
                &record.object.key,
                receipt.body.commit_seq,
            )
            .map_err(Refusal::host),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "actions_tests.rs"]
mod tests;
