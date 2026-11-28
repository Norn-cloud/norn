//! Core 08 (`WORLD-ISOLATE-1`, `FORK-PIN-1`, D53): fork-scoped question
//! evaluation through the shared read authority and disclosure boundary.

use super::{
    CheckedSnapshot, QuestionRuntime, admit_named_question, allowed, refuse_labeled_paths_on,
    verify_checked_snapshot, visible_recording,
};
use crate::{
    authz::read_denied,
    host::DevHost,
    privacy::{DisclosedForkAnswer, disclosure::project_fork_answer},
    refusal::Refusal,
    scope::ScopeQuery,
};
use norn_cell::{CellError, MAIN_WORLD, ObjectRecord, engine::SqlEngine};
use norn_checker::metric::compute::QuestionPlan;
use std::collections::BTreeSet;

/// Core 08 (`WORLD-ISOLATE-1`, `FORK-PIN-1`, D53): evaluate an admitted named
/// question inside a forked host under its durable fork pin, enforcing the same
/// read authorization, marking checks, and D53 disclosure redaction as
/// [`super::get_answer`] while returning a sealed simulated fork envelope.
/// # Errors
/// Denied scope/authority, `core.world.simulation_forbidden` on a non-fork host,
/// `core.world.fork_invalid_world` on a historical cutoff or unrelated world,
/// marking refusals, or typed compute refusals.
pub fn get_fork_answer<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    question: &str,
    runtime: &dyn QuestionRuntime<E>,
) -> Result<DisclosedForkAnswer, Refusal> {
    let (_epoch, actor, key, plan) = admit_named_question(host, scope, actor, question, runtime)?;
    fork_answer_plan(host, runtime, actor, question, &plan, key)
}

/// Core 08 (`WORLD-ISOLATE-1`, `FORK-PIN-1`): evaluate an admitted `plan` on a
/// forked host under the shared read PDP, marking checks (against inherited
/// `MAIN_WORLD` storage), and D53 fork disclosure redaction.
fn fork_answer_plan<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    actor: &str,
    question: &str,
    plan: &QuestionPlan,
    key: &[u8],
) -> Result<DisclosedForkAnswer, Refusal> {
    let cell = host.cell()?;
    if cell.store().fork_pin().is_none() {
        return Err(Refusal::from_compute(CellError::WorldSimulationForbidden));
    }
    let checked_version = crate::object_routes::version_of(&cell.store().read())?;
    let snapshot = runtime.snapshot(cell.store(), plan, actor)?;
    if snapshot.context.scope != host.scope().key() {
        return Err(read_denied());
    }
    if snapshot.cutoff.is_some() {
        return Err(Refusal::from_compute(CellError::ForkInvalidWorld));
    }
    let historical = plan.as_of();
    refuse_labeled_paths_on(host, &cell.store().read(), plan, MAIN_WORLD, historical)?;
    let permits = |row: &ObjectRecord, grant| allowed(host, runtime, actor, question, row, grant);
    let mut withheld = BTreeSet::new();
    let mut visible = |row: &ObjectRecord| visible_recording(row, &permits, &mut withheld);
    let outcome = runtime.evaluate_in_fork(cell.store(), plan, snapshot.context, &mut visible);
    let evaluated_frontier = outcome.as_ref().ok().map(|fork| fork.result.known_as_of);
    let disclosed = outcome.map(|fork| {
        project_fork_answer(
            (host, runtime, cell.store()),
            (actor, question, key, plan),
            &withheld,
            fork,
        )
    });
    verify_checked_snapshot(
        host,
        cell.store(),
        plan,
        &CheckedSnapshot {
            world: MAIN_WORLD,
            checked_version,
            evaluated_frontier,
            historical,
            live_read: true,
        },
    )?;
    disclosed.map_err(Refusal::from_compute)?
}
