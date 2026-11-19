//! FLOW-DRIVER-1 and ACTION-STEP-1: a perform step's result moves its run.
//!
//! A run's turns dispatch perform commands into the cell's outbox. A parked
//! one waits until the cell resolves it: a person decides a `norn.decision@1`
//! step (DECISIONS-1), and the cell executes a `norn.action@1` step itself
//! ([`drive_run`]). Once a command's result is recorded, the flow's result
//! turn is evaluated over the run's current values and committed consuming
//! that delivery, so the run advances exactly once per result. This is host
//! code: the decide route and the host call it after their own
//! authorization, never a client directly.

use http::StatusCode;
use norn_cell::MAIN_WORLD;
use norn_cell::engine::SqlEngine;
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{
    CellCommit, CommandId, Consumed, DecisionTurn, EnvelopeLimits, FactEvent, FactKey, Generation,
    LogicalTime, TypeCatalog, World,
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
/// `run.flow_inadmissible`); the kernel refused its output; a host failure.
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
    let catalog = TypeCatalog::new(model.object_types())
        .with_property_types(model.property_types())
        .with_interfaces(model.interfaces())
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
    norn_kernel::decision_cell_commit::<_, _, _, norn_cell::CellError>(
        &catalog,
        &slice,
        &turn,
        &output,
        &EnvelopeLimits::V1,
    )
    .map_err(|refusal| refused(refusal.code(), "the flow's result turn was refused"))
}

/// The most action steps one [`drive_run`] executes; a run whose steps
/// keep dispatching more continues on its next drive.
pub const MAX_ACTION_STEPS: usize = 64;

/// How the host runs one action call under a recorded principal's authority
/// (a run's starter for its action steps, a parked call's requester once it
/// is approved): it makes `principal` the verified principal its policy
/// decides for, runs `call`, and restores the request's own principal on
/// every path. `None` when the host cannot act as that principal, which
/// denies the call.
pub type AsStarter<'a> = &'a dyn Fn(
    &RunStarter,
    &mut dyn FnMut() -> Result<ActionAnswer, Refusal>,
) -> Option<Result<ActionAnswer, Refusal>>;

/// The idempotency key of a step attempt's action call: one per attempt,
/// bounded, so a re-drive replays the same call instead of calling twice.
fn step_call_key(step_key: &str, attempt: &RuntimeValue) -> Result<String, Refusal> {
    let key = norn_ir::canonical::domain_hash_hex(b"norn.action.step/1\0", &(step_key, attempt))
        .map_err(Refusal::host)?;
    Ok(format!("step-{}", key.get(..48).unwrap_or(&key)))
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
    if !idempotency_key.starts_with("step-") {
        return Ok(None);
    }
    for pending in store.read().parked_commands().map_err(Refusal::host)? {
        let Some(payload) = pending.command.command.get("payload") else {
            continue;
        };
        let text = |name: &str| payload.get(name).and_then(RuntimeValue::as_str);
        let (Some(norn_cell::flow_run::ACTION_FUNCTION), Some(run), Some(step), Some(attempt)) = (
            text("function"),
            text("runId"),
            text("step"),
            payload.get("attempt"),
        ) else {
            continue;
        };
        if pending.result.is_none()
            && step_call_key(text("idempotencyKey").unwrap_or(step), attempt)? == idempotency_key
        {
            return Ok(Some(run.to_owned()));
        }
    }
    Ok(None)
}

/// A pending `norn.action@1` step of `run_id`: its delivery coordinates and
/// its step input.
struct PendingStep {
    command: CommandId,
    generation: Generation,
    step: String,
    attempt: RuntimeValue,
    idempotency_key: String,
    input: RuntimeValue,
}

fn pending_step<E: SqlEngine>(
    cell: &ActionCell<E>,
    run_id: &str,
) -> Result<Option<PendingStep>, Refusal> {
    for pending in cell
        .store()
        .read()
        .parked_commands()
        .map_err(Refusal::host)?
    {
        if pending.result.is_some() {
            continue;
        }
        let Some(payload) = pending.command.command.get("payload") else {
            continue;
        };
        let text = |name: &str| payload.get(name).and_then(RuntimeValue::as_str);
        if text("function") != Some(norn_cell::flow_run::ACTION_FUNCTION)
            || text("runId") != Some(run_id)
        {
            continue;
        }
        let (Some(step), Some(attempt)) = (text("step"), payload.get("attempt")) else {
            continue;
        };
        let input = payload
            .get("input")
            .and_then(|input| input.get(step))
            .cloned()
            .unwrap_or(RuntimeValue::Null);
        return Ok(Some(PendingStep {
            command: CommandId(pending.command.command_id.clone()),
            generation: Generation(pending.command.generation),
            step: step.to_owned(),
            attempt: attempt.clone(),
            idempotency_key: text("idempotencyKey").unwrap_or(step).to_owned(),
            input,
        }));
    }
    Ok(None)
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
/// # Errors
/// A host failure; the step stays pending and the next drive retries it, and
/// the action call's idempotency key makes that retry replay.
pub fn drive_run<E: SqlEngine>(
    host: &DevHost<E>,
    run_id: &str,
    as_starter: AsStarter<'_>,
    disclosure: &dyn QuestionRuntime<E>,
    now: LogicalTime,
) -> Result<(), Refusal> {
    for _ in 0..MAX_ACTION_STEPS {
        let (step, starter) = {
            let _epoch = host.definition_read()?;
            let cell = host.cell()?;
            let Some(step) = pending_step(&cell, run_id)? else {
                return Ok(());
            };
            let starter = cell
                .store()
                .read()
                .run_start(MAIN_WORLD, run_id)
                .map_err(Refusal::host)?
                .and_then(|start| start.started_by);
            (step, starter)
        };
        let outcome = match (starter, norn_cell::flow_run::parse_action_step(&step.input)) {
            (_, Err(refused)) => failure("Refused", Some(refused.code())),
            (None, Ok(_)) => failure("Denied", None),
            (Some(starter), Ok(action)) => {
                let scope = crate::actions::ActionQuery {
                    org_id: Some(host.scope().org_id.clone()),
                    environment_id: Some(host.scope().environment_id.clone()),
                    version: None,
                };
                let request = crate::actions::ActionRequest {
                    input: RuntimeValue::Object(action.input),
                    idempotency_key: step_call_key(&step.idempotency_key, &step.attempt)?,
                };
                let mut call = || {
                    crate::actions::call_action(
                        host,
                        &scope,
                        Some(&starter.subject),
                        &action.action,
                        &request,
                        disclosure,
                        now,
                    )
                };
                match as_starter(&starter, &mut call) {
                    None => failure("Denied", None),
                    // An approval gates the action: the step waits on its
                    // decision, and the run moves once someone decides.
                    Some(Ok(ActionAnswer::Pending(_))) => return Ok(()),
                    Some(Ok(ActionAnswer::Receipt(receipt))) => {
                        StepOutcome::Output(std::collections::BTreeMap::from([
                            (
                                "objectType".to_owned(),
                                RuntimeValue::String(receipt.object_type),
                            ),
                            (
                                "objectId".to_owned(),
                                RuntimeValue::String(receipt.object_id),
                            ),
                            (
                                "version".to_owned(),
                                norn_ir::runtime::encode(&receipt.version)
                                    .map_err(Refusal::host)?,
                            ),
                        ]))
                    }
                    Some(Err(refused)) => match refused.category {
                        crate::refusal::RefusalCategory::Internal => return Err(refused),
                        crate::refusal::RefusalCategory::Permission
                        | crate::refusal::RefusalCategory::Visibility => failure("Denied", None),
                        crate::refusal::RefusalCategory::Precondition
                        | crate::refusal::RefusalCategory::Conflict
                        | crate::refusal::RefusalCategory::Invalid
                        | crate::refusal::RefusalCategory::Limit => {
                            failure("Refused", Some(&refused.code))
                        }
                    },
                }
            }
        };
        deliver(host, run_id, &step, outcome, now)?;
    }
    Ok(())
}

/// Record `outcome` as the step's result and commit the run's result turn
/// consuming it. A result recorded meanwhile by another drive is left to it.
fn deliver<E: SqlEngine>(
    host: &DevHost<E>,
    run_id: &str,
    step: &PendingStep,
    outcome: StepOutcome,
    now: LogicalTime,
) -> Result<(), Refusal> {
    let _epoch = host.definition_read()?;
    let mut cell = host.cell()?;
    let row = cell
        .store()
        .read()
        .flow_run(MAIN_WORLD, run_id)
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("a run lost its ledger row"))?;
    let flow = crate::run_routes::installed_flow(cell.store(), &row.flow.id)?;
    let now = crate::actions::turn_clock(&mut cell, now)?;
    let recorded = match &outcome {
        StepOutcome::Output(object) | StepOutcome::Failure(object) => object.clone(),
    };
    let delivery = |outcome| Delivery {
        command: &step.command,
        generation: step.generation,
        run_id,
        step: &step.step,
        attempt: step.attempt.clone(),
        outcome,
    };
    // The flow must accept the result before anything is recorded.
    result_turn(&cell, &flow, delivery(outcome.clone()), now)?;
    if cell
        .complete_parked(&step.command, step.generation, recorded)
        .map_err(Refusal::host)?
        .is_none()
    {
        return Ok(());
    }
    let commit = result_turn(&cell, &flow, delivery(outcome), now)?;
    norn_kernel::CellHost::commit(&mut *cell, commit).map_err(|error| {
        let norn_kernel::CommitError::Host(host_error) = error else {
            return Refusal::new(
                StatusCode::CONFLICT,
                "run.result_not_pending",
                "the run changed while its step result was delivered",
                "the step's result stays recorded and the next drive delivers it",
            );
        };
        Refusal::host(host_error)
    })?;
    Ok(())
}
