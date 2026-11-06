//! FLOW-DRIVER-1: the cell-side loop that moves flow runs.
//!
//! A run's turns dispatch perform commands into the cell's outbox. An
//! ordinary perform is executed in FIFO order by the cell's performer; a parked
//! one (`norn.decision@1`, PARKED-1) waits until the cell resolves it. Either
//! way its recorded result is a pending delivery. [`deliver_result`] turns that
//! delivery into the flow's result turn and commits it with the delivery
//! consumed, so the run advances exactly once per result. This is host code: it
//! is called by the cell's performer loop and by the decide route after their
//! own authorization, never by a client directly.

use http::StatusCode;
use norn_cell::engine::SqlEngine;
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{
    CommandId, Consumed, DecisionTurn, EnvelopeLimits, FactKey, Generation, TypeCatalog, World,
};

use crate::host::DevHost;
use crate::refusal::Refusal;
use crate::run_routes::{RunDocument, installed_flow};

/// The most ordinary performs one [`drive_performs`] call executes.
pub const MAX_PERFORMS_PER_DRIVE: usize = 64;

fn not_pending() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        "run.result_not_pending",
        "no pending result",
        "the command has no recorded, unconsumed result",
    )
}

fn malformed(what: &str) -> Refusal {
    Refusal::host(format!("flow perform command lacks {what}"))
}

/// The run, step, attempt and result object a flow perform's pending
/// delivery carries.
fn step_result(
    pending: norn_cell::read::PendingCommand,
) -> Result<(String, String, RuntimeValue, RuntimeObject), Refusal> {
    let payload = pending
        .command
        .command
        .get("payload")
        .ok_or_else(|| malformed("a payload"))?;
    let text = |name: &str| {
        payload
            .get(name)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
            .ok_or_else(|| malformed(name))
    };
    let (run_id, step) = (text("runId")?, text("step")?);
    let attempt = payload
        .get("attempt")
        .cloned()
        .ok_or_else(|| malformed("an attempt"))?;
    let Some(RuntimeValue::Object(result)) = pending.result else {
        return Err(malformed("an object result"));
    };
    Ok((run_id, step, attempt, result))
}

/// Deliver the pending result of `command` at `generation` to its run's flow
/// rule, and commit the result turn with the delivery consumed.
///
/// # Errors
/// `run.result_not_pending` when there is no recorded, unconsumed result; the
/// kernel's refusal when the turn is refused (the delivery stays pending); a
/// host failure.
pub fn deliver_result<E: SqlEngine>(
    host: &DevHost<E>,
    command: &CommandId,
    generation: Generation,
) -> Result<RunDocument, Refusal> {
    let _epoch = host.definition_read()?;
    let mut cell = host.cell()?;
    let pending = cell
        .store()
        .read()
        .pending_result(&command.0, generation.0)
        .map_err(Refusal::host)?
        .ok_or_else(not_pending)?;
    let (run_id, step, attempt, result) = step_result(pending)?;
    let row = cell
        .store()
        .read()
        .flow_run(norn_cell::MAIN_WORLD, &run_id)
        .map_err(Refusal::host)?
        .ok_or_else(|| malformed("a started run"))?;
    let flow = installed_flow(cell.store(), &row.flow.id)?;
    let key = FactKey::new(flow.object_type.clone(), run_id.clone());
    let slice = norn_kernel::load_slice(&*cell, [key.clone()]).map_err(Refusal::host)?;
    let run: RuntimeObject = slice
        .get(&key)
        .and_then(|state| state.values.clone())
        .ok_or_else(|| malformed("a live run object"))?;
    let output = norn_kernel::flow_start::flow_result(
        &flow.rule,
        &flow.environment_hash,
        run,
        norn_kernel::flow_start::StepResult {
            run_id: &run_id,
            step: &step,
            attempt,
            result,
        },
    )
    .map_err(|error| {
        Refusal::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            error.code(),
            "the flow refused the result",
            error.to_string(),
        )
    })?;
    let model = cell.store().model();
    let catalog = TypeCatalog::new(model.object_types())
        .with_interfaces(model.interfaces())
        .with_governing_laws(model.law_artifacts())
        .with_property_types(model.property_types());
    let consumed = Consumed::Result {
        command: command.clone(),
        generation,
    };
    let turn = DecisionTurn {
        id: format!("norn.run.result/{}/{}", command.0, generation.0),
        now: cell.now(),
        world: World::Main,
        consumes: Some(consumed),
    };
    let commit = norn_kernel::decision_cell_commit::<_, _, _, norn_cell::CellError>(
        &catalog,
        &slice,
        &turn,
        &output,
        &EnvelopeLimits::V1,
    )
    .map_err(|refusal| {
        Refusal::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            refusal.code(),
            "the flow's result turn was refused",
            refusal.to_string(),
        )
    })?;
    norn_kernel::CellHost::commit(&mut *cell, commit).map_err(|error| {
        if let norn_kernel::CommitError::Host(host_error) = error {
            Refusal::host(host_error)
        } else {
            // Another turn consumed the result or changed the run first.
            not_pending()
        }
    })?;
    cell.store()
        .read()
        .flow_run(norn_cell::MAIN_WORLD, &run_id)
        .map_err(Refusal::host)?
        .map(RunDocument::from)
        .ok_or_else(|| Refusal::host("a run lost its ledger row"))
}

/// Perform pending ordinary commands in FIFO order with the cell's performer
/// and deliver each result to its run. Parked commands are left waiting.
/// Returns the runs it advanced, in order.
///
/// # Errors
/// The first refusal; earlier deliveries stay committed.
pub fn drive_performs<E: SqlEngine>(host: &DevHost<E>) -> Result<Vec<RunDocument>, Refusal> {
    let mut advanced = Vec::new();
    for _ in 0..MAX_PERFORMS_PER_DRIVE {
        let performed = {
            let mut cell = host.cell()?;
            // Only a flow's perform belongs to this loop; any other command at
            // the head waits for its own performer.
            let head = cell.store().read().head_command().map_err(Refusal::host)?;
            let flow_perform = head.is_some_and(|head| {
                head.command.command.get("payload").is_some_and(|payload| {
                    payload.get("kind").and_then(RuntimeValue::as_str) == Some("perform")
                        && payload.get("runId").is_some()
                })
            });
            if !flow_perform {
                break;
            }
            norn_kernel::CellHost::perform(&mut *cell).map_err(Refusal::host)?
        };
        let Some(performed) = performed else { break };
        advanced.push(deliver_result(
            host,
            &performed.command,
            performed.generation,
        )?);
    }
    Ok(advanced)
}
