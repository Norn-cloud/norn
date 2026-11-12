//! RUN-ROUTES-1: start, read and list runs of installed flows.
//!
//! A run is the object of its flow's generated Run type, keyed by its run id;
//! its state is that type's lifecycle (the flow's state machine). The cell's
//! run ledger (RUN-LEDGER-1) indexes runs for reads and is never authority.
//!
//! `POST /v1/runs` starts a run of an installed flow: the flow's `decide` rule
//! evaluates its start turn, and the cell commits the run object, the first
//! steps' commands and the ledger row in one commit. `GET /v1/runs/{id}`
//! reads a run. Every route admits
//! explicit scope first. Starting is the host operation
//! [`crate::authz::Operation::StartRun`], decided by the trusted [`crate::authz::Authorizer`];
//! a run holds no grants of its own. Every run read passes the ordinary
//! `values` read gate on the run's object type, and a run the caller may not
//! read is indistinguishable from an absent one.
//!
//! A run document carries its step timeline, rebuilt from the run object's
//! committed versions: each lifecycle state that waits on a step opens that
//! step, the next state closes it, and a terminal state ends the run. A step
//! the cell itself cannot move (a tool that needs an executor, a timer, a
//! person's decision) is `waiting`, with what it waits on; that is a state,
//! not an error.

use std::collections::BTreeMap;

use http::StatusCode;
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::{FlowRef, RunRow, RunStart, RunStarter};
use norn_cell::{CellStore, MAIN_WORLD};
use norn_ir::{RuleRoot, RuntimeValue};
use norn_kernel::{DecisionTurn, EnvelopeLimits, FactKey, LogicalTime, TypeCatalog, World};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::authz::{AccessRequest, Operation, ReadCapability, ReadRequest};
use crate::flow_definitions::{DECISION_FUNCTION, owned_ids};
use crate::host::DevHost;
use crate::refusal::{Refusal, codes};
use crate::scope::ScopeQuery;

/// The longest run id, in bytes.
pub const MAX_RUN_ID_BYTES: usize = 128;

/// The most run-object versions a timeline reads: the newest ones. A flow
/// has at most 64 durable steps, so a run that has not looped through
/// retries fits well inside it.
pub const MAX_TIMELINE_VERSIONS: u64 = 256;

/// Builds the kernel PDP input for reading one capability of an object type,
/// from the trusted transport's verified principal and grant mapping.
pub type ReadAccess<'a> = &'a dyn Fn(ReadCapability, &str) -> norn_kernel::authz::AccessRequest;

/// Stable `run.*` refusal codes the routes add to the cell's own
/// (`run.start_invalid`, `run.state_invalid`, `run.exists`).
pub mod run_codes {
    /// No run with this id is visible to the caller in this scope.
    pub const NOT_FOUND: &str = "run.not_found";
    /// The run id was already started for another flow, or by another turn.
    pub const EXISTS: &str = "run.exists";
    /// No active installed flow has this id.
    pub const FLOW_NOT_FOUND: &str = "run.flow_not_found";
    /// The cell changed while the run started; the same run id may retry.
    pub const START_CONFLICT: &str = "run.start_conflict";
}

/// `POST /v1/runs` body.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartRunRequest {
    /// The installed flow's definition id (`flow.id`); its rule is
    /// `<flow>.flow.rule`.
    pub flow: String,
    /// The caller-chosen run id: `[A-Za-z0-9._:-]`, at most 128 bytes. A retry
    /// with the same id and flow returns the existing run.
    pub run_id: String,
}

/// The flow a run executes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunFlow {
    /// The flow's definition id.
    pub id: String,
    /// The flow's `decide` rule artifact id (`<id>.flow.rule`).
    pub rule_id: String,
    /// The rule version the run is pinned to.
    pub version: String,
    /// The flow's display title, when it has one.
    pub title: Option<String>,
}

/// What a step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStepKind {
    /// It calls a tool.
    Perform,
    /// It asks a person (`norn.decision@1`).
    Decide,
    /// It sleeps until a timer.
    Wait,
    /// The run completed.
    Succeed,
    /// The run failed.
    Fail,
}

/// Where a step stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStepStatus {
    /// It is being worked on in the cell.
    Running,
    /// It waits on something outside the run (see `waitingOn`).
    Waiting,
    /// It finished.
    Done,
    /// It ended the run in a failure.
    Failed,
}

/// What a waiting step waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepWait {
    /// A person's decision.
    Decision,
    /// An executor for its tool, which this cell does not run itself.
    Executor,
    /// A timer.
    Timer,
}

/// What a step receipt records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepReceiptKind {
    /// A Function's receipt.
    Function,
    /// A person's decision.
    Decision,
    /// The commit that moved the run.
    Commit,
}

/// One receipt behind a step, as the caller may see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StepReceipt {
    /// What it records.
    pub kind: StepReceiptKind,
    /// The receipt hash, or its keyed handle (D53).
    pub receipt_hash: String,
    /// The commit.
    pub commit_seq: u64,
    /// When it was recorded (logical milliseconds).
    pub recorded_at_ms: u64,
    /// Who acted, in display words, never an id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
}

/// Why a step or run failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RunStepError {
    /// The failed lifecycle state.
    pub code: String,
    /// Display words.
    pub message: String,
}

/// One step of a run's timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStep {
    /// The flow's step id (`succeed` or `fail` for the run's end).
    pub id: String,
    /// The step's display label from the flow, when it has one.
    pub label: Option<String>,
    /// What the step is.
    pub kind: RunStepKind,
    /// Where it stands.
    pub status: RunStepStatus,
    /// What a waiting step waits on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<StepWait>,
    /// When the run reached it (logical milliseconds).
    pub started_at_ms: Option<u64>,
    /// When the run left it.
    pub ended_at_ms: Option<u64>,
    /// The step's receipts the caller may see; none are disclosed yet.
    pub receipts: Vec<StepReceipt>,
    /// Why it failed.
    pub error: Option<RunStepError>,
}

/// One run, as the routes return it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunDocument {
    /// The run id.
    pub id: String,
    /// Its generation.
    pub generation: u64,
    /// The flow it runs.
    pub flow: RunFlow,
    /// The run's object type.
    pub object_type: String,
    /// Its lifecycle state; `null` once the run object is deleted.
    pub state: Option<String>,
    /// The run object's version.
    pub version: u64,
    /// Start time (logical milliseconds).
    pub created_at_ms: u64,
    /// The starting commit.
    pub created_commit_seq: u64,
    /// Last write time (logical milliseconds).
    pub updated_at_ms: u64,
    /// The last commit that wrote the run object.
    pub updated_commit_seq: u64,
    /// The run has ended and will not change.
    pub terminal: bool,
    /// The steps the run reached, in order.
    pub steps: Vec<RunStep>,
    /// The open decision the run waits on, when the caller may decide it;
    /// `null` otherwise.
    pub decision: Option<RunDecision>,
}

/// The open decision a run waits on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunDecision {
    /// The decision id (`POST /v1/decisions/{id}:decide`).
    pub id: String,
    /// When it expires (logical milliseconds).
    pub expires_at_ms: u64,
}

fn run_exists() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        run_codes::EXISTS,
        "the run id is taken",
        "choose another runId",
    )
}

fn run_not_found() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        run_codes::NOT_FOUND,
        "no such run",
        "no run with this id is visible to the caller in this scope",
    )
}

fn flow_not_found() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        run_codes::FLOW_NOT_FOUND,
        "no such installed flow",
        "name the id of an active flow installed in the cell's definitions",
    )
}

fn start_denied() -> Refusal {
    Refusal::new(
        StatusCode::FORBIDDEN,
        codes::AUTHZ_DENIED,
        "the run request is not authorized",
        "starting a run of this flow is denied",
    )
}

fn valid_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_RUN_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn readable<E: SqlEngine>(host: &DevHost<E>, access: ReadAccess<'_>, row: &RunRow) -> bool {
    host.read_permitted(&ReadRequest {
        capability: ReadCapability::Values,
        access: &access(ReadCapability::Values, &row.object_type),
    })
}

/// What a run's document says about its flow: identity and display words.
/// A retired flow keeps its pinned identity and loses the rest.
pub(crate) struct FlowView {
    pub(crate) id: String,
    pub(crate) rule_id: String,
    /// The Run type's lifecycle property; empty when the flow is retired.
    pub(crate) state_property: String,
    pub(crate) title: Option<String>,
    pub(crate) labels: BTreeMap<String, String>,
    /// Each perform step's function id, from the rule's command records.
    pub(crate) functions: BTreeMap<String, String>,
}

/// An active installed flow: its rule and Run type, and its view.
pub(crate) struct InstalledFlow {
    pub(crate) rule: RuleRoot,
    pub(crate) version: String,
    pub(crate) object_type: String,
    pub(crate) environment_hash: String,
    pub(crate) view: FlowView,
}

fn text<'v>(value: &'v Value, pointer: &str) -> Option<&'v str> {
    value.pointer(pointer).and_then(Value::as_str)
}

/// The flow `flow` as the cell's active definitions install it. A retired
/// flow, or one that is not a flow, is not found.
pub(crate) fn installed_flow<E: SqlEngine>(
    store: &CellStore<E>,
    flow: &str,
) -> Result<InstalledFlow, Refusal> {
    let reader = store.read();
    let retained = reader
        .retained_definition(flow, None)
        .map_err(Refusal::host)?
        .map(|record| record.definition)
        .filter(|definition| {
            definition.kind == "flow"
                && definition.lifecycle == norn_cell::retained::RetainedLifecycle::Active
        })
        .ok_or_else(flow_not_found)?;
    let payload: Value = serde_json::from_str(&retained.payload).map_err(Refusal::host)?;
    let active = reader
        .installed_definitions(None)
        .map_err(Refusal::host)?
        .ok_or_else(flow_not_found)?;
    let artifacts: Vec<Value> =
        serde_json::from_str(&active.canonical_set).map_err(Refusal::host)?;
    let find = |id: &str, kind: &str| {
        artifacts.iter().find(|artifact| {
            text(artifact, "/artifactId") == Some(id) && text(artifact, "/kind") == Some(kind)
        })
    };
    let (entity_id, rule_id) = owned_ids(flow);
    let rule = find(&rule_id, "rule").ok_or_else(flow_not_found)?;
    let entity = find(&entity_id, "entity").ok_or_else(flow_not_found)?;
    let lifecycles = entity
        .pointer("/payload/objectType/lifecycles")
        .and_then(Value::as_array)
        .ok_or_else(flow_not_found)?;
    let (Some(object_type), [lifecycle], Some(version), Some("decide"), Some(rule_payload)) = (
        text(entity, "/payload/objectType/name"),
        lifecycles.as_slice(),
        text(rule, "/version"),
        text(rule, "/payload/contract"),
        rule.pointer("/payload"),
    ) else {
        return Err(flow_not_found());
    };
    let functions = norn_checker::flow_performs::perform_refs(rule_payload)
        .map_err(|_unverifiable| flow_not_found())?
        .into_iter()
        .filter_map(|perform| Some((perform.step?, perform.name?.0)))
        .collect();
    let labels = payload
        .pointer("/flow/steps")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|step| {
            Some((
                text(step, "/id")?.to_owned(),
                text(step, "/label")?.trim().to_owned(),
            ))
        })
        .collect();
    Ok(InstalledFlow {
        rule: serde_json::from_value(rule_payload.clone())
            .map_err(|_not_a_rule| flow_not_found())?,
        version: version.to_owned(),
        object_type: object_type.to_owned(),
        environment_hash: active.receipt_sha256,
        view: FlowView {
            id: flow.to_owned(),
            rule_id,
            state_property: text(lifecycle, "/property")
                .ok_or_else(flow_not_found)?
                .to_owned(),
            title: text(&payload, "/flow/title").map(|title| title.trim().to_owned()),
            labels,
            functions,
        },
    })
}

/// Whether the host lets `actor` start runs of `flow`: the one decision
/// `POST /v1/runs` makes ([`Operation::StartRun`]), which a flow listing also
/// reports as `canStart` so a client offers only the starts the cell allows.
#[must_use]
pub fn may_start<E: SqlEngine>(host: &DevHost<E>, actor: &str, flow: &str) -> bool {
    host.authorizer
        .authorize(&AccessRequest {
            scope: host.scope(),
            actor: Some(actor),
            operation: Operation::StartRun(flow.to_owned()),
        })
        .is_ok()
}

/// `POST /v1/runs`: start a run of an installed flow.
///
/// # Errors
/// Missing or foreign scope or a denied caller (one opaque 403), an invalid
/// body, an unknown or retired flow (`run.flow_not_found`), a run id already
/// started for another flow (`run.exists`), a refused start turn
/// (`run.start_refused`, `run.flow_inadmissible`, or the kernel's own code),
/// a concurrent change (`run.start_conflict`), or a host failure.
#[utoipa::path(post, path = "/v1/runs", params(ScopeQuery), request_body = StartRunRequest, responses((status = 201, description = "The run started.", body = RunDocument), (status = 200, description = "A retry with the same run id and flow: the existing run.", body = RunDocument), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "No such installed flow.", body = Refusal), (status = 409, description = "The run id is taken, or the cell changed while the run started.", body = Refusal), (status = 422, description = "The flow refused to start.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn start_run<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    body: &StartRunRequest,
    access: ReadAccess<'_>,
    now: LogicalTime,
    // The verified principal the host recorded for this request: the run's
    // action steps act under exactly it (ACTION-STEP-1).
    starter: Option<RunStarter>,
) -> Result<(StatusCode, RunDocument), Refusal> {
    let actor = actor
        .filter(|actor| !actor.trim().is_empty())
        .ok_or_else(start_denied)?;
    let epoch = host.definition_read();
    let authorized = may_start(host, actor, &body.flow);
    let scoped = host
        .scope()
        .admit(scope.org_id.as_deref(), scope.environment_id.as_deref());
    if !authorized || scoped.is_err() {
        return Err(start_denied());
    }
    let _epoch = epoch?;
    if !valid_run_id(&body.run_id) {
        return Err(Refusal::invalid(
            "runId must be 1-128 bytes of [A-Za-z0-9._:-]",
        ));
    }
    let mut cell = host.cell()?;
    let flow = installed_flow(cell.store(), &body.flow)?;
    if let Some(row) = cell
        .store()
        .read()
        .flow_run(MAIN_WORLD, &body.run_id)
        .map_err(Refusal::host)?
    {
        // An exact retry returns the run; another flow's id never leaks.
        return if row.flow.id == body.flow && readable(host, access, &row) {
            Ok((
                StatusCode::OK,
                document(host, actor, cell.store(), &flow.view, row)?,
            ))
        } else {
            Err(run_exists())
        };
    }
    let now = crate::actions::turn_clock(&mut cell, now)?;
    commit_start(&mut cell, &flow, &body.run_id, now, starter)?;
    let row = cell
        .store()
        .read()
        .flow_run(MAIN_WORLD, &body.run_id)
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("a committed run start left no ledger row"))?;
    Ok((
        StatusCode::CREATED,
        document(host, actor, cell.store(), &flow.view, row)?,
    ))
}

/// A refused turn names its stable code only: the rule's or the kernel's
/// detail can echo run values, which this route does not disclose.
fn turn_refused(code: &str, message: &str) -> Refusal {
    Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        code,
        message,
        "the code names why the flow's turn was refused",
    )
}

/// Evaluate the installed flow's start turn and commit it with its run.
fn commit_start<E: SqlEngine>(
    cell: &mut crate::host::ActionCell<E>,
    flow: &InstalledFlow,
    run_id: &str,
    now: LogicalTime,
    starter: Option<RunStarter>,
) -> Result<(), Refusal> {
    let output = norn_kernel::flow_turn::start(&flow.rule, &flow.environment_hash, run_id)
        .map_err(|error| turn_refused(error.code(), "the flow refused to start"))?;
    let model = cell.store().model().clone();
    let catalog = TypeCatalog::new(model.object_types())
        .with_property_types(model.property_types())
        .with_interfaces(model.interfaces())
        .with_governing_laws(model.law_artifacts());
    let run_key = FactKey::new(flow.object_type.clone(), run_id.to_owned());
    let slice = norn_kernel::load_slice(&*cell, [run_key]).map_err(Refusal::host)?;
    let turn = DecisionTurn {
        id: format!("norn.run/{run_id}/start"),
        now,
        world: World::Main,
        consumes: None,
    };
    let commit = norn_kernel::decision_cell_commit::<_, _, _, norn_cell::CellError>(
        &catalog,
        &slice,
        &turn,
        &output,
        &EnvelopeLimits::V1,
    )
    .map_err(|refusal| turn_refused(refusal.code(), "the flow's start turn was refused"))?;
    let start = RunStart {
        run_id: run_id.to_owned(),
        flow: FlowRef {
            id: flow.view.id.clone(),
            version: flow.version.clone(),
        },
        object_type: flow.object_type.clone(),
        state_property: flow.view.state_property.clone(),
        started_by: starter,
    };
    cell.commit_run_start(commit, &start).map_err(|error| {
        let norn_kernel::CommitError::Host(host_error) = error else {
            return Refusal::new(
                StatusCode::CONFLICT,
                run_codes::START_CONFLICT,
                "the run start was refused",
                "the cell changed while the run started; retry with the same runId",
            );
        };
        if let norn_cell::CellError::RunRefused(code) = host_error {
            return if code == run_codes::EXISTS {
                run_exists()
            } else {
                turn_refused(code, "the run start was refused")
            };
        }
        // A storage failure is the host's, never the caller's conflict.
        Refusal::host(host_error)
    })?;
    Ok(())
}

/// `GET /v1/runs/{id}`.
///
/// # Errors
/// Missing or foreign scope; `run.not_found` for an absent or unreadable run.
#[utoipa::path(get, path = "/v1/runs/{id}", params(ScopeQuery, ("id" = String, Path, description = "The run id.")), responses((status = 200, description = "The run.", body = RunDocument), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope denied.", body = Refusal), (status = 404, description = "No such run is visible to the caller.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn get_run<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: &str,
    id: &str,
    access: ReadAccess<'_>,
) -> Result<RunDocument, Refusal> {
    let _epoch = host.definition_read()?;
    host.scope()
        .admit(scope.org_id.as_deref(), scope.environment_id.as_deref())?;
    if !valid_run_id(id) {
        return Err(run_not_found());
    }
    let cell = host.cell()?;
    let row = cell
        .store()
        .read()
        .flow_run(MAIN_WORLD, id)
        .map_err(Refusal::host)?
        .filter(|row| readable(host, access, row))
        .ok_or_else(run_not_found)?;
    let flow = run_flow(cell.store(), &row)?;
    document(host, actor, cell.store(), &flow, row)
}

/// The view of the flow a started run runs: the installed flow's while it is
/// active, otherwise its pinned identity with no display words (its runs
/// stay readable after the flow is retired).
fn run_flow<E: SqlEngine>(store: &CellStore<E>, row: &RunRow) -> Result<FlowView, Refusal> {
    match installed_flow(store, &row.flow.id) {
        Ok(flow) => Ok(flow.view),
        Err(refusal) if refusal.code == run_codes::FLOW_NOT_FOUND => {
            let (_, rule_id) = owned_ids(&row.flow.id);
            Ok(FlowView {
                id: row.flow.id.clone(),
                rule_id,
                state_property: String::new(),
                title: None,
                labels: BTreeMap::new(),
                functions: BTreeMap::new(),
            })
        }
        Err(refusal) => Err(refusal),
    }
}

/// The run document for `row`, with its timeline.
fn document<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    store: &CellStore<E>,
    flow: &FlowView,
    row: RunRow,
) -> Result<RunDocument, Refusal> {
    let decision = if terminal_state_of(store, &row) {
        None
    } else {
        crate::decisions::run_decision(host, store, actor, &row.run_id)?
    };
    let terminal = terminal_state_of(store, &row);
    let steps = timeline(store, flow, &row, terminal)?;
    Ok(RunDocument {
        id: row.run_id,
        generation: row.generation,
        flow: RunFlow {
            id: flow.id.clone(),
            rule_id: flow.rule_id.clone(),
            version: row.flow.version,
            title: flow.title.clone(),
        },
        object_type: row.object_type,
        state: row.state,
        version: row.version,
        created_at_ms: row.created_at_ms,
        created_commit_seq: row.created_commit_seq,
        updated_at_ms: row.updated_at_ms,
        updated_commit_seq: row.updated_commit_seq,
        terminal,
        steps,
        decision,
    })
}

/// Whether the run has ended: its object is deleted, or its state is
/// terminal.
fn terminal_state_of<E: SqlEngine>(store: &CellStore<E>, row: &RunRow) -> bool {
    row.state
        .as_deref()
        .is_none_or(|state| terminal_state(store, &row.object_type, state))
}

/// Whether `state` is a terminal state of a lifecycle of `object_type`. A
/// Run type the model no longer declares is not terminal (fail closed: the
/// run may still change once it is declared again).
fn terminal_state<E: SqlEngine>(store: &CellStore<E>, object_type: &str, state: &str) -> bool {
    store
        .model()
        .object_types()
        .find(|declared| declared.name.as_str() == object_type)
        .is_some_and(|declared| {
            declared
                .lifecycles
                .iter()
                .any(|lifecycle| lifecycle.terminal.iter().any(|end| end == state))
        })
}

/// What a lowered Run state means for the timeline (`lowerFlow`'s states).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase<'s> {
    /// `started`: the run exists and has not reached a step.
    Started,
    /// Waiting on a perform step's result.
    Performing(&'s str),
    /// Waiting on the timer before a retry of a perform step.
    Retrying(&'s str),
    /// Sleeping in a sleep step.
    Sleeping(&'s str),
    /// `completed`.
    Completed,
    /// `failed_<tag>`.
    Failed(&'s str),
}

fn phase(state: &str) -> Option<Phase<'_>> {
    if state == "started" {
        return Some(Phase::Started);
    }
    if state == "completed" {
        return Some(Phase::Completed);
    }
    if let Some(step) = state.strip_prefix("awaiting_perform_") {
        return Some(Phase::Performing(step));
    }
    // A step with a retry policy waits on attempt `n`'s result in
    // `awaiting_retry_<step>_attempt_<n>`, and on the retry timer before
    // attempt `n` in `retrying_<step>_attempt_<n>`.
    if let Some(rest) = state.strip_prefix("awaiting_retry_") {
        return rest
            .rsplit_once("_attempt_")
            .map(|(step, _)| Phase::Performing(step));
    }
    if let Some(rest) = state.strip_prefix("retrying_") {
        return rest
            .rsplit_once("_attempt_")
            .map(|(step, _)| Phase::Retrying(step));
    }
    if let Some(step) = state.strip_prefix("sleeping_") {
        return Some(Phase::Sleeping(step));
    }
    state.strip_prefix("failed_").map(Phase::Failed)
}

/// The run's steps, from the states its committed versions passed through.
fn timeline<E: SqlEngine>(
    store: &CellStore<E>,
    flow: &FlowView,
    row: &RunRow,
    terminal: bool,
) -> Result<Vec<RunStep>, Refusal> {
    if flow.state_property.is_empty() {
        // A retired flow: its state names are not known to be `lowerFlow`'s.
        return Ok(Vec::new());
    }
    let reader = store.read();
    let first = row
        .version
        .saturating_sub(MAX_TIMELINE_VERSIONS.saturating_sub(1))
        .max(1);
    let mut states: Vec<(Option<String>, u64)> = Vec::new();
    for version in first..=row.version {
        let evidence = reader
            .fact_version_evidence(MAIN_WORLD, &row.object_type, &row.run_id, version)
            .map_err(Refusal::host)?;
        let state = evidence
            .snapshot()
            .values
            .as_ref()
            .and_then(|values| values.get(&flow.state_property))
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned);
        states.push((state, evidence.receipt().body.logical_time));
    }
    Ok(steps_from(flow, &states, terminal))
}

fn step_kind(flow: &FlowView, step: &str) -> RunStepKind {
    if flow.functions.get(step).map(String::as_str) == Some(DECISION_FUNCTION) {
        RunStepKind::Decide
    } else {
        RunStepKind::Perform
    }
}

fn open(flow: &FlowView, id: &str, kind: RunStepKind, at: u64) -> RunStep {
    RunStep {
        id: id.to_owned(),
        label: flow.labels.get(id).cloned(),
        kind,
        status: RunStepStatus::Running,
        waiting_on: None,
        started_at_ms: Some(at),
        ended_at_ms: None,
        receipts: Vec::new(),
        error: None,
    }
}

fn failure(tag: &str) -> RunStepError {
    RunStepError {
        code: format!("failed_{tag}"),
        message: format!("The run failed: {}", tag.replace('_', " ")),
    }
}

/// Close the step the run is in at `at`: done, or failed with `tag`.
fn close(
    steps: &mut [RunStep],
    current: &mut Option<(usize, bool)>,
    at: u64,
    failed: Option<&str>,
) {
    if let Some(step) = current.take().and_then(|(index, _)| steps.get_mut(index)) {
        step.ended_at_ms = Some(at);
        if let Some(tag) = failed {
            step.status = RunStepStatus::Failed;
            step.error = Some(failure(tag));
        } else {
            step.status = RunStepStatus::Done;
        }
    }
}

/// Fold the run's states (each with the logical time it was committed) into
/// its steps. Pure: the same states always give the same timeline.
fn steps_from(flow: &FlowView, states: &[(Option<String>, u64)], terminal: bool) -> Vec<RunStep> {
    let mut steps: Vec<RunStep> = Vec::new();
    // The index of the step the run is in, and whether it waits on a timer.
    let mut current: Option<(usize, bool)> = None;
    for (state, at) in states {
        let at = *at;
        let Some(state) = state.as_deref() else {
            // The run object was deleted: the run has ended.
            close(&mut steps, &mut current, at, None);
            continue;
        };
        let (id, timer, kind) = match phase(state) {
            Some(Phase::Performing(id)) => (id, false, step_kind(flow, id)),
            Some(Phase::Retrying(id)) => (id, true, step_kind(flow, id)),
            Some(Phase::Sleeping(id)) => (id, true, RunStepKind::Wait),
            Some(Phase::Completed) => {
                close(&mut steps, &mut current, at, None);
                let mut end = open(flow, "succeed", RunStepKind::Succeed, at);
                end.status = RunStepStatus::Done;
                end.ended_at_ms = Some(at);
                steps.push(end);
                continue;
            }
            Some(Phase::Failed(tag)) => {
                close(&mut steps, &mut current, at, Some(tag));
                let mut end = open(flow, "fail", RunStepKind::Fail, at);
                end.status = RunStepStatus::Done;
                end.ended_at_ms = Some(at);
                end.error = Some(failure(tag));
                steps.push(end);
                continue;
            }
            Some(Phase::Started) | None => continue,
        };
        // A retry or a timer inside the same step keeps it open.
        if current
            .and_then(|(index, _)| steps.get(index))
            .is_some_and(|step| step.id == id)
        {
            current = current.map(|(index, _)| (index, timer));
            continue;
        }
        close(&mut steps, &mut current, at, None);
        steps.push(open(flow, id, kind, at));
        current = Some((steps.len().saturating_sub(1), timer));
    }
    if let Some((index, timer)) = current
        && let Some(step) = steps.get_mut(index)
    {
        if terminal {
            step.status = RunStepStatus::Done;
        } else {
            // The cell runs no tool, timer or person itself: the step waits.
            step.status = RunStepStatus::Waiting;
            step.waiting_on = Some(if timer || step.kind == RunStepKind::Wait {
                StepWait::Timer
            } else if step.kind == RunStepKind::Decide {
                StepWait::Decision
            } else {
                StepWait::Executor
            });
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> FlowView {
        FlowView {
            id: "demo.approve".into(),
            rule_id: "demo.approve.flow.rule".into(),
            state_property: "status".into(),
            title: Some("Approve".into()),
            labels: BTreeMap::from([("ask".to_owned(), "Ask the lead".to_owned())]),
            functions: BTreeMap::from([
                ("ask".to_owned(), DECISION_FUNCTION.to_owned()),
                ("notify".to_owned(), "demo.notify".to_owned()),
            ]),
        }
    }

    fn states(list: &[(&str, u64)]) -> Vec<(Option<String>, u64)> {
        list.iter()
            .map(|(state, at)| (Some((*state).to_owned()), *at))
            .collect()
    }

    #[test]
    fn retries_stay_one_step_and_a_failure_ends_the_run() {
        let steps = steps_from(
            &flow(),
            &states(&[
                ("awaiting_retry_notify_attempt_1", 1),
                ("retrying_notify_attempt_2", 2),
                ("awaiting_retry_notify_attempt_2", 3),
                ("failed_timeout", 4),
            ]),
            true,
        );
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].id, "notify");
        assert_eq!(steps[0].status, RunStepStatus::Failed);
        assert_eq!(steps[1].kind, RunStepKind::Fail);
        assert_eq!(
            steps[1].error.as_ref().map(|error| error.code.as_str()),
            Some("failed_timeout")
        );
    }

    #[test]
    fn a_completed_run_ends_in_a_done_succeed_step() {
        let steps = steps_from(
            &flow(),
            &states(&[("awaiting_perform_ask", 1), ("completed", 9)]),
            true,
        );
        assert_eq!(steps[0].status, RunStepStatus::Done);
        assert_eq!(steps[1].kind, RunStepKind::Succeed);
        assert_eq!(steps[1].status, RunStepStatus::Done);
    }
}
