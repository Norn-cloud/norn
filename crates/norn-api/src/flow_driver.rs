//! FLOW-DRIVER-1: a perform step's result moves its run.
//!
//! A run's turns dispatch perform commands into the cell's outbox; a parked
//! one (`norn.decision@1`, PARKED-1) waits until the cell resolves it. Once a
//! command's result is recorded, [`result_turn`] evaluates the flow's result
//! turn over the run's current values and prepares the commit that consumes
//! that delivery, so the run advances exactly once per result. This is host
//! code: the decide route calls it after its own authorization, never a
//! client directly.

use http::StatusCode;
use norn_cell::MAIN_WORLD;
use norn_cell::engine::SqlEngine;
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{
    CellCommit, CommandId, Consumed, DecisionTurn, EnvelopeLimits, FactEvent, FactKey, Generation,
    LogicalTime, TypeCatalog, World,
};

use crate::host::ActionCell;
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
    pub(crate) outcome: norn_kernel::flow_turn::StepOutcome,
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

/// The run's state after a commit, from the run ledger.
///
/// # Errors
/// A host failure, or a run without a ledger row.
pub(crate) fn run_state<E: SqlEngine>(
    cell: &ActionCell<E>,
    run_id: &str,
) -> Result<Option<String>, Refusal> {
    cell.store()
        .read()
        .flow_run(MAIN_WORLD, run_id)
        .map_err(Refusal::host)?
        .map(|row| row.state)
        .ok_or_else(|| Refusal::host("a run lost its ledger row"))
}
