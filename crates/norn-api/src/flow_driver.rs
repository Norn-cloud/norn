//! FLOW-DRIVER-1, ACTION-STEP-1, and QUESTION-STEP-1: a perform step's result moves its run.
//!
//! A run's turns dispatch perform commands into the cell's outbox. A parked
//! one waits until the cell resolves it: a person decides a `norn.decision@1`
//! step (DECISIONS-1), the cell executes a `norn.action@1` step itself, and
//! the cell evaluates a `norn.question@1` step read-only ([`drive_run`]). Once a command's result is recorded, the flow's result
//! turn is evaluated over the run's current values and committed consuming
//! that delivery, so the run advances exactly once per result. This is host
//! code: the decide route and the host call it after their own
//! authorization, never a client directly.
//!
//! APPROVAL-RECOVERY-1: an action step whose approved call already has an
//! outcome (it committed, or its refusal is recorded) takes that outcome
//! without the action being called again, and a result
//! recorded but not yet consumed stays due until the run consumes it. The
//! host's wake delivers both; it never calls an action.

use http::StatusCode;
use norn_cell::MAIN_WORLD;
use norn_cell::engine::SqlEngine;
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{
    CellCommit, CommandId, Consumed, DecisionTurn, EnvelopeLimits, FactEvent, FactKey, Generation,
    LogicalTime, TurnEnvelopeLimits, TurnEnvelopeMeter, World,
};

use norn_cell::flow_run::RunStarter;
use norn_kernel::flow_turn::StepOutcome;

use crate::actions::ActionAnswer;
use crate::host::{ActionCell, DevHost};
use crate::question_routes::QuestionRuntime;
use crate::refusal::Refusal;
use crate::run_routes::InstalledFlow;

/// One perform step's delivery: the command, its run and step, and how the
/// step ended.
#[derive(Clone)]
pub(crate) struct Delivery<'a> {
    pub(crate) command: &'a CommandId,
    pub(crate) generation: Generation,
    pub(crate) run_id: &'a str,
    pub(crate) step: &'a str,
    pub(crate) attempt: RuntimeValue,
    pub(crate) outcome: StepOutcome,
}

fn refused(code: &str, message: &str) -> Refusal {
    Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        code,
        message,
        "the code names why the flow's turn was refused",
    )
}

/// The commit of `delivery`'s result turn: the flow's rule over the run's
/// current values and the step's output, consuming the delivery. Nothing is
/// committed here. The turn id is the run, step and attempt, so a command a
/// result turn dispatches has an id no other turn of the run produces.
///
/// # Errors
/// The run object is gone; the rule refused the turn (`run.turn_refused`,
/// `run.flow_inadmissible`); the kernel refused its output (`422` without
/// disclosing flow run operands); a host failure.
pub(crate) fn result_turn<E: SqlEngine>(
    cell: &ActionCell<E>,
    flow: &InstalledFlow,
    delivery: Delivery<'_>,
    now: LogicalTime,
) -> Result<CellCommit<FactEvent, RuntimeObject, RuntimeObject>, Refusal> {
    let key = FactKey::new(flow.object_type.clone(), delivery.run_id.to_owned());
    let slice = norn_kernel::load_slice(cell, [key.clone()]).map_err(Refusal::host)?;
    let run: RuntimeObject = slice
        .get(&key)
        .and_then(|state| state.values.clone())
        .ok_or_else(|| Refusal::host("a waiting run has no run object"))?;
    let attempt = match &delivery.attempt {
        RuntimeValue::Number(number) => number.to_string(),
        RuntimeValue::String(text) => text.clone(),
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Array(_)
        | RuntimeValue::Object(_) => return Err(Refusal::host("a perform command has no attempt")),
    };
    let output = norn_kernel::flow_turn::result(
        &flow.rule,
        &flow.environment_hash,
        run,
        norn_kernel::flow_turn::StepResult {
            run_id: delivery.run_id,
            step: delivery.step,
            attempt: delivery.attempt,
            outcome: delivery.outcome,
        },
    )
    .map_err(|error| refused(error.code(), "the flow refused the result"))?;
    let model = cell.store().model().clone();
    let catalog = model
        .type_catalog()
        .with_governing_laws(model.law_artifacts());
    let turn = DecisionTurn {
        id: format!(
            "norn.run/{}/result/{}/{attempt}",
            delivery.run_id, delivery.step
        ),
        now,
        world: World::Main,
        consumes: Some(Consumed::Result {
            command: delivery.command.clone(),
            generation: delivery.generation,
        }),
    };
    let mut meter = TurnEnvelopeMeter::with_limits(
        EnvelopeLimits::V1,
        TurnEnvelopeLimits {
            max_events: u64::from(EnvelopeLimits::V1.max_events),
            max_commands: u64::from(EnvelopeLimits::V1.max_commands),
            max_timers: u64::from(EnvelopeLimits::V1.max_timers),
        },
    );
    norn_kernel::decision_cell_commit_with_host_and_meter(
        cell, &catalog, &slice, &turn, &output, &mut meter,
    )
    .map_err(|refusal| refused(refusal.code(), "the flow's result turn was refused"))
}

/// The most action steps one [`drive_run`] executes; a run whose steps
/// keep dispatching more continues on its next drive.
pub const MAX_ACTION_STEPS: usize = 64;

/// How the host runs one flow step (`norn.action@1` or `norn.question@1`) or
/// approved action call under a recorded principal's authority: it makes
/// `principal` the verified principal its policy decides for, runs `call`, and
/// restores the request's own principal on every path. `None` when the host
/// cannot act as that principal, which denies the step.
pub type AsStarter<'a> =
    &'a dyn Fn(&RunStarter, &mut dyn FnMut() -> Result<(), Refusal>) -> Option<Result<(), Refusal>>;

/// The prefix of every step attempt's call key ([`step_call_key`]).
const STEP_CALL_PREFIX: &str = "step-";

/// How many hex digits of the digest a step attempt's call key keeps.
const STEP_CALL_DIGITS: usize = 48;

/// The idempotency key of a step attempt's action call: one per attempt,
/// bounded, so a re-drive replays the same call instead of calling twice.
fn step_call_key(step_key: &str, attempt: &RuntimeValue) -> Result<String, Refusal> {
    let key = norn_ir::canonical::domain_hash_hex(b"norn.action.step/1\0", &(step_key, attempt))
        .map_err(Refusal::host)?;
    Ok(format!(
        "{STEP_CALL_PREFIX}{}",
        key.get(..STEP_CALL_DIGITS).unwrap_or(&key)
    ))
}

/// S5a: whether `key` has the shape of a step attempt's call key: the
/// host's own identity for a flow step's call, which a direct call never
/// takes.
#[must_use]
pub(crate) fn is_step_call_key(key: &str) -> bool {
    key.strip_prefix(STEP_CALL_PREFIX).is_some_and(|digest| {
        digest.len() == STEP_CALL_DIGITS
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// The run whose pending action step makes the call keyed `idempotency_key`,
/// if a step does: an approval of that call moves that run (APPROVAL-1).
///
/// # Errors
/// A host failure.
pub(crate) fn run_waiting_on<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    idempotency_key: &str,
) -> Result<Option<String>, Refusal> {
    Ok(waiting_attempts(store, idempotency_key, None)?
        .into_iter()
        .next()
        .map(|attempt| attempt.run_id))
}

/// S6b: `run_id`'s pending action step attempt that makes the call keyed
/// `idempotency_key`, if one does. Read under the caller's cell hold.
///
/// # Errors
/// A host failure.
pub(crate) fn attempt_of_run<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    run_id: &str,
    idempotency_key: &str,
) -> Result<Option<crate::approvals::StepAttempt>, Refusal> {
    Ok(waiting_attempts(store, idempotency_key, Some(run_id))?
        .into_iter()
        .next())
}

/// Which cell-executed parked step a command represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellStepKind {
    /// `norn.action@1`
    Action,
    /// `norn.question@1`
    Question,
}

/// A parked `norn.action@1` or `norn.question@1` command payload's parsed coordinates.
struct ParkedCellCommand<'a> {
    kind: CellStepKind,
    payload: &'a RuntimeValue,
    run_id: &'a str,
    step: &'a str,
    attempt: &'a RuntimeValue,
    step_key: &'a str,
}

fn parse_cell_command(command: &RuntimeValue) -> Option<ParkedCellCommand<'_>> {
    let payload = command.get("payload")?;
    let text = |name: &str| payload.get(name).and_then(RuntimeValue::as_str);
    let kind = match text("function") {
        Some(norn_cell::flow_run::ACTION_FUNCTION) => CellStepKind::Action,
        Some(norn_cell::flow_run::QUESTION_FUNCTION) => CellStepKind::Question,
        Some(_) | None => return None,
    };
    let (Some(run_id), Some(step), Some(attempt)) =
        (text("runId"), text("step"), payload.get("attempt"))
    else {
        return None;
    };
    Some(ParkedCellCommand {
        kind,
        payload,
        run_id,
        step,
        attempt,
        step_key: text("idempotencyKey").unwrap_or(step),
    })
}

fn parse_action_command(command: &RuntimeValue) -> Option<ParkedCellCommand<'_>> {
    parse_cell_command(command).filter(|parsed| parsed.kind == CellStepKind::Action)
}

/// The pending action step attempts (of `run_id` only, when given) that
/// make the call keyed `idempotency_key`: each one's run, and its parked
/// command at that generation. Only a call parked before attempts were
/// recorded is placed this way; a call parks with the attempt its trusted
/// invocation names (S5a).
///
/// # Errors
/// A host failure.
fn waiting_attempts<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    idempotency_key: &str,
    run_id: Option<&str>,
) -> Result<Vec<crate::approvals::StepAttempt>, Refusal> {
    let mut found = Vec::new();
    if !is_step_call_key(idempotency_key) {
        return Ok(found);
    }
    for pending in store.read().parked_commands().map_err(Refusal::host)? {
        let Some(parsed) = parse_action_command(&pending.command.command) else {
            continue;
        };
        if pending.result.is_none()
            && run_id.is_none_or(|wanted| wanted == parsed.run_id)
            && step_call_key(parsed.step_key, parsed.attempt)? == idempotency_key
        {
            found.push(crate::approvals::StepAttempt {
                run_id: parsed.run_id.to_owned(),
                command: pending.command.command_id.clone(),
                generation: pending.command.generation,
            });
        }
    }
    Ok(found)
}

/// S5a: whether the idempotency record of `action_id`'s call keyed
/// `idempotency_key` was committed by `attempt`'s own dispatch, as the
/// ledger records it ([`norn_cell::decision_ledger::STEP_CALL_OWNED`]).
/// Only such a record is ever that step's outcome: one a direct call wrote
/// under the step's key, or one committed before ownership was recorded,
/// never is.
///
/// # Errors
/// A host failure: the record or its receipt is missing.
pub(crate) fn owns<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    action_id: &str,
    idempotency_key: &str,
    attempt: &crate::approvals::StepAttempt,
) -> Result<bool, Refusal> {
    let seq = crate::actions::record_commit_seq(store, action_id, idempotency_key)?;
    let owner = store
        .read()
        .step_call_owner_at(MAIN_WORLD, seq)
        .map_err(Refusal::host)?;
    Ok(owner.is_some_and(|owner| {
        owner.run_id == attempt.run_id
            && owner.command_id == attempt.command
            && owner.generation == attempt.generation
    }))
}

/// S5a: whether `attempt` still waits and makes exactly the call keyed
/// `idempotency_key`: its `norn.action@1` command is parked at its
/// generation with no result, for its run, and its call key is that key.
/// Read under the caller's cell hold.
///
/// # Errors
/// A host failure.
pub(crate) fn attempt_makes<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    attempt: &crate::approvals::StepAttempt,
    idempotency_key: &str,
) -> Result<bool, Refusal> {
    let Some(pending) = store
        .read()
        .pending_command(&attempt.command)
        .map_err(Refusal::host)?
    else {
        return Ok(false);
    };
    if pending.command.generation != attempt.generation || pending.result.is_some() {
        return Ok(false);
    }
    let Some(parsed) = parse_action_command(&pending.command.command) else {
        return Ok(false);
    };
    Ok(parsed.run_id == attempt.run_id
        && step_call_key(parsed.step_key, parsed.attempt)? == idempotency_key)
}

/// A parked `norn.action@1` or `norn.question@1` step: its kind, delivery
/// coordinates, step input, attempt call key, starter, and starting definitions version.
struct StepCall {
    kind: CellStepKind,
    run_id: String,
    command: CommandId,
    generation: Generation,
    step: String,
    attempt: RuntimeValue,
    call_key: String,
    input: RuntimeValue,
    starter: Option<RunStarter>,
    run_definitions_version: Option<u64>,
    run_connection_bindings: Option<crate::flow_question::ConnectionBindings>,
}

/// Every parked cell-executed step (`norn.action@1` or `norn.question@1`,
/// filtered by `run_id` and `only_actions`), with its recorded result if any.
fn cell_steps<E: SqlEngine>(
    cell: &ActionCell<E>,
    run_id: Option<&str>,
    only_actions: bool,
) -> Result<Vec<(StepCall, Option<RuntimeValue>)>, Refusal> {
    let reader = cell.store().read();
    let mut found = Vec::new();
    for pending in reader.parked_commands().map_err(Refusal::host)? {
        let Some(parsed) = parse_cell_command(&pending.command.command) else {
            continue;
        };
        if only_actions && parsed.kind != CellStepKind::Action {
            continue;
        }
        if run_id.is_some_and(|wanted| wanted != parsed.run_id) {
            continue;
        }
        let input = parsed
            .payload
            .get("input")
            .and_then(|input| input.get(parsed.step))
            .cloned()
            .unwrap_or(RuntimeValue::Null);
        let run_start = reader
            .run_start(MAIN_WORLD, parsed.run_id)
            .map_err(Refusal::host)?;
        let starter = run_start
            .as_ref()
            .and_then(|start| start.started_by.clone());
        let run_connection_bindings = run_start.and_then(|start| start.connection_bindings);
        let run_definitions_version = match reader
            .flow_run(MAIN_WORLD, parsed.run_id)
            .map_err(Refusal::host)?
        {
            Some(row) => reader
                .receipt_at(row.created_commit_seq)
                .map_err(Refusal::host)?
                .map(|receipt| receipt.body.definitions_version),
            None => None,
        };
        found.push((
            StepCall {
                kind: parsed.kind,
                run_id: parsed.run_id.to_owned(),
                command: CommandId(pending.command.command_id.clone()),
                generation: Generation(pending.command.generation),
                step: parsed.step.to_owned(),
                call_key: step_call_key(parsed.step_key, parsed.attempt)?,
                attempt: parsed.attempt.clone(),
                input,
                starter,
                run_definitions_version,
                run_connection_bindings,
            },
            pending.result,
        ));
    }
    Ok(found)
}

/// Every parked `norn.action@1` step (of `run_id` only, when given), with
/// its recorded result when one is recorded and not yet consumed.
fn action_steps<E: SqlEngine>(
    cell: &ActionCell<E>,
    run_id: Option<&str>,
) -> Result<Vec<(StepCall, Option<RuntimeValue>)>, Refusal> {
    cell_steps(cell, run_id, true)
}

/// The pending (no result recorded) cell-executed step (`norn.action@1` or
/// `norn.question@1`) of `run_id`, if any.
fn pending_step<E: SqlEngine>(
    cell: &ActionCell<E>,
    run_id: &str,
) -> Result<Option<StepCall>, Refusal> {
    Ok(cell_steps(cell, Some(run_id), false)?
        .into_iter()
        .find_map(|(step, result)| result.is_none().then_some(step)))
}

/// APPROVAL-RECOVERY-1: a pending action step whose approved call already
/// has its outcome, and that outcome as the step's result.
pub(crate) struct RecordedStep {
    /// The approving decision.
    pub(crate) decision_id: String,
    /// When its approval committed (logical milliseconds): due from then.
    pub(crate) approved_at_ms: u64,
    run_id: String,
    command: CommandId,
    generation: Generation,
    step: String,
    attempt: RuntimeValue,
    outcome: StepOutcome,
    /// The result is recorded already and only its consumption is due.
    recorded: bool,
}

/// Every pending action step whose approved call has an outcome: the call
/// committed (exactly the approved call), or its refusal is recorded, or the
/// step's result is recorded and not yet consumed. The approval is the one
/// bound to the step's exact call ([`step_approval`]). A refusal of a call
/// whose action or gate changed since it parked is not the step's outcome:
/// the step's next drive asks for a new approval (APPROVAL-REVISION-1).
///
/// # Errors
/// A host failure.
pub(crate) fn recorded_steps<E: SqlEngine>(
    cell: &ActionCell<E>,
) -> Result<Vec<RecordedStep>, Refusal> {
    let mut found = Vec::new();
    for (step, result) in action_steps(cell, None)? {
        let Some((decision_id, approved_at_ms, approval)) = step_approval(cell, &step)? else {
            continue;
        };
        let (outcome, recorded) = match result {
            Some(RuntimeValue::Object(result)) if result.contains_key("_tag") => {
                (StepOutcome::Failure(result), true)
            }
            Some(RuntimeValue::Object(result)) => (StepOutcome::Output(result), true),
            Some(_other) => return Err(Refusal::host("an action step's result is not an object")),
            None => match approval {
                StepApproval::Outcome(outcome) => (outcome, false),
                StepApproval::Waiting | StepApproval::Superseded => continue,
            },
        };
        found.push(RecordedStep {
            decision_id,
            approved_at_ms,
            run_id: step.run_id,
            command: step.command,
            generation: step.generation,
            step: step.step,
            attempt: step.attempt,
            outcome,
            recorded,
        });
    }
    Ok(found)
}

/// Where the approval bound to a step's call leaves the step.
enum StepApproval {
    /// The approved call has its outcome: the step's result.
    Outcome(StepOutcome),
    /// Approved, not yet executed: only the requester's own authenticated
    /// request runs it (APPROVAL-RECOVERY-1).
    Waiting,
    /// Its action or gate changed since it parked: the step asks anew.
    Superseded,
}

/// The approval bound to `step`'s exact call, if one approved it: its
/// decision, when it was approved, and where it leaves the step. An
/// approval binds a step only when its parked call is exactly the call the
/// step attempt makes: parked for that exact attempt (its run, its command
/// and generation), under the attempt's idempotency key, the action, the
/// input, matching definitions pin (the step's explicit pin, if any), and the
/// run's starter, its whole recorded identity, as requester (S5a); a call
/// parked before attempts were recorded binds by its approval's recorded
/// run (S6b, [`crate::approvals::binding`]). Another call under the same
/// key never delivers its result to the step, and a committed record is the
/// step's output only when its attempt's own dispatch committed it
/// ([`owns`]). Of several bound approvals the one the ledger committed last
/// supersedes the earlier ones (S5c).
fn step_approval<E: SqlEngine>(
    cell: &ActionCell<E>,
    step: &StepCall,
) -> Result<Option<(String, u64, StepApproval)>, Refusal> {
    use crate::approvals::CallState;
    let (Some(starter), Ok(action)) = (
        step.starter.as_ref(),
        norn_cell::flow_run::parse_action_step(&step.input),
    ) else {
        return Ok(None);
    };
    let rows = cell
        .store()
        .read()
        .approved_calls_of_run(MAIN_WORLD, &step.run_id)
        .map_err(Refusal::host)?;
    let attempt = crate::approvals::StepAttempt {
        run_id: step.run_id.clone(),
        command: step.command.0.clone(),
        generation: step.generation.0,
    };
    let mut bound: Option<(
        norn_cell::decision_ledger::ApprovedCall,
        crate::approvals::HeldCall,
        CallState,
    )> = None;
    let expected_defs_version = action.pin.as_ref().and_then(|pin| pin.definitions_version);
    // In the ledger order of the approving commits: the last exact one binds.
    for row in rows {
        let (held, state) = crate::approvals::approved_state(cell, &row)?;
        let exact = crate::approvals::binding(cell, &row, &held)?
            == crate::approvals::Binding::Step(attempt.clone())
            && held.call.idempotency_key == step.call_key
            && held.call.action == action.action
            && held.call.input == action.input
            && held.call.trace == action.trace
            && held.call.definitions_version == expected_defs_version
            && held.call.requester == *starter;
        if exact {
            bound = Some((row, held, state));
        }
    }
    let Some((row, held, state)) = bound else {
        return Ok(None);
    };
    let approval = match state {
        CallState::NotExecuted => StepApproval::Waiting,
        CallState::Refused(code)
            if code == crate::approvals::approval_codes::DEFINITION_CHANGED =>
        {
            StepApproval::Superseded
        }
        CallState::Refused(code) => StepApproval::Outcome(failure("Refused", Some(&code))),
        // S5a: a record the step attempt's own dispatch did not commit is
        // never its outcome, as the drive's replay of it is refused.
        CallState::Committed(_record)
            if !owns(
                cell.store(),
                &held.call.action,
                &held.call.idempotency_key,
                &attempt,
            )? =>
        {
            StepApproval::Outcome(failure(
                "Refused",
                Some(&crate::actions::foreign_step_record().code),
            ))
        }
        CallState::Committed(record) => {
            let (object_type, object_id, version) =
                crate::actions::recorded_target(cell.store(), &record, &held.call.idempotency_key)?;
            StepApproval::Outcome(output(object_type, object_id, version)?)
        }
    };
    Ok(Some((row.decision_id, row.resolved_at_ms, approval)))
}

fn output(object_type: String, object_id: String, version: u64) -> Result<StepOutcome, Refusal> {
    Ok(StepOutcome::Output(std::collections::BTreeMap::from([
        ("objectType".to_owned(), RuntimeValue::String(object_type)),
        ("objectId".to_owned(), RuntimeValue::String(object_id)),
        (
            "version".to_owned(),
            norn_ir::runtime::encode(&version).map_err(Refusal::host)?,
        ),
    ])))
}

/// Deliver one [`RecordedStep`] to its run, under `cell`: the outcome is
/// recorded as the step's result (unless it is already) and the run's
/// result turn commits consuming it. A result recorded meanwhile by another
/// delivery is left to it.
///
/// # Errors
/// The flow refused the result; the run object is gone; a host failure.
/// The step stays due.
pub(crate) fn deliver_recorded<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    step: &RecordedStep,
    now: LogicalTime,
) -> Result<(), Refusal> {
    deliver_in(
        cell,
        Delivery {
            command: &step.command,
            generation: step.generation,
            run_id: &step.run_id,
            step: &step.step,
            attempt: step.attempt.clone(),
            outcome: step.outcome.clone(),
        },
        step.recorded,
        &[],
        now,
    )
}

fn failure(tag: &str, code: Option<&str>) -> StepOutcome {
    let mut failure: RuntimeObject = std::collections::BTreeMap::from([(
        "_tag".to_owned(),
        RuntimeValue::String(tag.to_owned()),
    )]);
    if let Some(code) = code {
        failure.insert("code".to_owned(), RuntimeValue::String(code.to_owned()));
    }
    StepOutcome::Failure(failure)
}

/// ACTION-STEP-1: execute `run_id`'s pending `norn.action@1` steps, each
/// through the ordinary governed action path under the run starter's
/// authority ([`AsStarter`]), and move the run with each step's result. A run
/// with no recorded starter, or a starter the host cannot act as, gets the
/// typed failure `Denied`; any other refusal of the action is `Refused` with
/// its code. The run holds no authority of its own, and no one who decided an
/// earlier step lends it any. Called by the host after a commit that may have
/// dispatched steps (a start, a decision), never by a client.
///
/// APPROVAL-RECOVERY-1: a step result recorded but not yet consumed, or an
/// approved call's outcome, is delivered first, the same way the wake
/// delivers it (`deliver_recorded`). An approved call not yet executed
/// runs only under `as_requester`: the requester's own authenticated
/// request, with its current authority. When there is none, or its try is
/// a denial of authority (decided anew on every try), the step stays
/// pending and its approval stays executable. S7: what a try ends with is
/// reconciled with the step's approval and recorded under one cell hold
/// (`settle`): an approved call that has its outcome by then (another
/// request committed it) gives the step that outcome, never this try's.
///
/// # Errors
/// A host failure; the step stays pending and the next drive retries it, and
/// the action call's idempotency key makes that retry replay.
pub fn drive_run<E: SqlEngine>(
    host: &DevHost<E>,
    run_id: &str,
    as_starter: AsStarter<'_>,
    as_requester: AsStarter<'_>,
    disclosure: &dyn QuestionRuntime<E>,
    now: LogicalTime,
) -> Result<(), Refusal> {
    for _ in 0..MAX_ACTION_STEPS {
        let (step, waiting, already_committed) = {
            let _epoch = host.definition_read()?;
            let mut cell = host.cell()?;
            if let Some(recorded) = recorded_steps(&cell)?
                .into_iter()
                .find(|recorded| recorded.run_id == run_id)
            {
                deliver_recorded(&mut cell, &recorded, now)?;
                continue;
            }
            let Some(step) = pending_step(&cell, run_id)? else {
                return Ok(());
            };
            if step.kind == CellStepKind::Question {
                (step, false, None)
            } else {
                let waiting = matches!(
                    step_approval(&cell, &step)?,
                    Some((.., StepApproval::Waiting))
                );
                // STEP-PIN-1 retry recovery: if this attempt already committed its
                // action before a crash prior to settlement, recover that committed
                // outcome before checking pins for a new execution.
                let committed = committed_outcome_for_step(&cell, &step)?;
                (step, waiting, committed)
            }
        };
        if step.kind == CellStepKind::Question {
            let dispatch = crate::flow_question::dispatch_question_step(
                host,
                &crate::flow_question::QuestionStepContext {
                    input: &step.input,
                    starter: step.starter.as_ref(),
                    run_definitions_version: step.run_definitions_version,
                    run_connection_bindings: step.run_connection_bindings.as_ref(),
                },
                as_starter,
                disclosure,
            )?;
            settle_question(host, &step, dispatch, now)?;
            continue;
        }
        if let Some(outcome) = already_committed {
            if !settle(host, &step, outcome, false, now)? {
                return Ok(());
            }
            continue;
        }
        let Some((outcome, denied)) = dispatch_step(
            host,
            &step,
            waiting,
            as_starter,
            as_requester,
            disclosure,
            now,
        )?
        else {
            return Ok(());
        };
        if !settle(host, &step, outcome, denied, now)? {
            return Ok(());
        }
    }
    Ok(())
}

/// Dispatch one parked `norn.action@1` step through `call_action_bound`.
/// Returns `Ok(None)` when the step stays pending (waiting on approval or on
/// its requester's own request).
fn dispatch_step<E: SqlEngine>(
    host: &DevHost<E>,
    step: &StepCall,
    waiting: bool,
    as_starter: AsStarter<'_>,
    as_requester: AsStarter<'_>,
    disclosure: &dyn QuestionRuntime<E>,
    now: LogicalTime,
) -> Result<Option<(StepOutcome, bool)>, Refusal> {
    // S5a: the step attempt this dispatch runs the call for, carried to
    // the gate by this trusted invocation.
    let attempt = crate::approvals::StepAttempt {
        run_id: step.run_id.clone(),
        command: step.command.0.clone(),
        generation: step.generation.0,
    };
    let parsed = norn_cell::flow_run::parse_action_step(&step.input);
    match (step.starter.as_ref(), parsed) {
        (_, Err(refused)) => Ok(Some((failure("Refused", Some(refused.code())), false))),
        (None, Ok(_)) => Ok(Some((failure("Denied", None), true))),
        (Some(starter), Ok(action)) => {
            let step_pin = crate::step_pin::StepActionPin {
                run_definitions_version: step.run_definitions_version,
                explicit: action.pin.clone(),
            };
            let scope = crate::actions::ActionQuery {
                org_id: Some(host.scope().org_id.clone()),
                environment_id: Some(host.scope().environment_id.clone()),
                version: action.pin.as_ref().and_then(|pin| pin.definitions_version),
            };
            let request = crate::actions::ActionRequest {
                input: RuntimeValue::Object(action.input),
                idempotency_key: step.call_key.clone(),
                trace: action.trace.map(Into::into),
            };
            // S2: the request's own authenticated principal, read before
            // any recorded authority stands in for it. Only a try run as
            // the requester carries it; a try under the starter's
            // recorded authority never consumes an approval, even one
            // recorded after this drive read the step.
            let principal = waiting
                .then(|| host.authorizer.verified_principal(&starter.subject))
                .flatten();
            let mut answer: Option<ActionAnswer> = None;
            let mut call = || -> Result<(), Refusal> {
                answer = Some(crate::actions::call_action_bound(
                    host,
                    &crate::actions::ActionCall {
                        query: &scope,
                        actor: Some(&starter.subject),
                        principal: principal.as_ref(),
                        attempt: Some(&attempt),
                        step_pin: Some(&step_pin),
                        action_id: &action.action,
                        request: &request,
                        now,
                    },
                    disclosure,
                    None,
                )?);
                Ok(())
            };
            let acting = if waiting { as_requester } else { as_starter };
            match acting(starter, &mut call) {
                // The requester is not here to run its approved call.
                None if waiting => Ok(None),
                None => Ok(Some((failure("Denied", None), true))),
                Some(Ok(())) => match answer {
                    // An approval gates the action: the step waits on its
                    // decision, and the run moves once someone decides.
                    Some(ActionAnswer::Pending(_)) => Ok(None),
                    Some(ActionAnswer::Receipt(receipt)) => Ok(Some((
                        output(receipt.object_type, receipt.object_id, receipt.version)?,
                        false,
                    ))),
                    None => Err(Refusal::host("action dispatch returned no answer")),
                },
                Some(Err(refused)) => match refused.category {
                    crate::refusal::RefusalCategory::Internal => Err(refused),
                    crate::refusal::RefusalCategory::Permission
                    | crate::refusal::RefusalCategory::Visibility => {
                        Ok(Some((failure("Denied", None), true)))
                    }
                    crate::refusal::RefusalCategory::Precondition
                    | crate::refusal::RefusalCategory::Conflict
                    | crate::refusal::RefusalCategory::Invalid
                    | crate::refusal::RefusalCategory::Limit => {
                        Ok(Some((failure("Refused", Some(&refused.code)), false)))
                    }
                },
            }
        }
    }
}

fn committed_outcome_for_step<E: SqlEngine>(
    cell: &ActionCell<E>,
    step: &StepCall,
) -> Result<Option<StepOutcome>, Refusal> {
    let Ok(action) = norn_cell::flow_run::parse_action_step(&step.input) else {
        return Ok(None);
    };
    let attempt = crate::approvals::StepAttempt {
        run_id: step.run_id.clone(),
        command: step.command.0.clone(),
        generation: step.generation.0,
    };
    let Some((object_type, object_id, version)) =
        crate::step_pin::committed_step_outcome(cell, &action.action, &step.call_key, &attempt)?
    else {
        return Ok(None);
    };
    output(object_type, object_id, version).map(Some)
}

/// S7: record what a try of `step`'s call ended with, reconciled with the
/// step's approval under one cell hold: an approved call with an outcome by
/// now (committed, or its refusal recorded, by any request) gives the step
/// that outcome instead of the try's; a denial of authority (`denied`,
/// decided anew on every try) leaves the step pending while an approval of
/// its call stands approved and not executed (S2: credentials are judged
/// under the dispatch's hold, so a step whose call an approval now waits on
/// stays pending for its requester). `false` when the step stays pending. A result recorded
/// meanwhile by another drive is left to it.
fn settle<E: SqlEngine>(
    host: &DevHost<E>,
    step: &StepCall,
    tried: StepOutcome,
    denied: bool,
    now: LogicalTime,
) -> Result<bool, Refusal> {
    let _epoch = host.definition_read()?;
    let mut cell = host.cell()?;
    let outcome = match step_approval(&cell, step)? {
        Some((.., StepApproval::Outcome(recorded))) => recorded,
        Some((.., StepApproval::Waiting)) if denied => return Ok(false),
        Some((.., StepApproval::Waiting | StepApproval::Superseded)) | None => {
            if let Some(committed) = committed_outcome_for_step(&cell, step)? {
                committed
            } else {
                tried
            }
        }
    };
    deliver_in(
        &mut cell,
        Delivery {
            command: &step.command,
            generation: step.generation,
            run_id: &step.run_id,
            step: &step.step,
            attempt: step.attempt.clone(),
            outcome,
        },
        false,
        &[],
        now,
    )?;
    Ok(true)
}

/// Settle a read-only `norn.question@1` step's outcome directly into the run's
/// result turn under one cell hold, atomically recording its read source types
/// (`RunQuestionRead`) whenever an `Answered` or `StaleSource` outcome settles.
fn settle_question<E: SqlEngine>(
    host: &DevHost<E>,
    step: &StepCall,
    dispatch: crate::flow_question::QuestionDispatch,
    now: LogicalTime,
) -> Result<(), Refusal> {
    let _epoch = host.definition_read()?;
    let mut cell = host.cell()?;
    deliver_in(
        &mut cell,
        Delivery {
            command: &step.command,
            generation: step.generation,
            run_id: &step.run_id,
            step: &step.step,
            attempt: step.attempt.clone(),
            outcome: dispatch.outcome,
        },
        false,
        &dispatch.sources,
        now,
    )
}

/// Record a step's result and commit its run's result turn under `cell`;
/// `recorded` when the result is recorded already and only its consumption
/// is due.
fn deliver_in<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    delivery: Delivery<'_>,
    recorded: bool,
    question_sources: &[norn_cell::flow_run::RunQuestionSource],
    now: LogicalTime,
) -> Result<(), Refusal> {
    let row = cell
        .store()
        .read()
        .flow_run(MAIN_WORLD, delivery.run_id)
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("a run lost its ledger row"))?;
    let flow = crate::run_routes::installed_flow(cell.store(), &row.flow.id)?;
    let now = crate::actions::turn_clock(cell, now)?;
    let result = match &delivery.outcome {
        StepOutcome::Output(object) | StepOutcome::Failure(object) => object.clone(),
    };
    let commit = result_turn(cell, &flow, delivery, now)?;
    let settlement = if recorded {
        norn_cell::Settlement::Recorded
    } else {
        norn_cell::Settlement::Record(result)
    };
    let committed = if question_sources.is_empty() {
        cell.commit_settled(commit, settlement)
    } else {
        let events = crate::flow_question::question_read_events(
            &row.run_id,
            &flow.object_type,
            question_sources,
        )?;
        cell.attributed(events, |cell| cell.commit_settled(commit, settlement))
    };
    let Err(error) = committed else {
        return Ok(());
    };
    // Settled already, by another drive.
    if crate::decisions::settled_elsewhere(&error) {
        return Ok(());
    }
    let norn_kernel::CommitError::Host(host_error) = error else {
        return Err(Refusal::new(
            StatusCode::CONFLICT,
            "run.result_not_pending",
            "the run changed while its step result was delivered",
            "nothing was recorded; the next drive delivers the step's result again",
        ));
    };
    Err(Refusal::host(host_error))
}
