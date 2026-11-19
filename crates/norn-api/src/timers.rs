//! TIMER-ALARM-1: the cell's due work, run from the host's clock.
//!
//! A cell has deadlines nobody calls for: a quantity reservation's expiry, a
//! flow run's sleep or retry timer, and a decision's deadline
//! (DECISION-TERMINAL-1). The host learns the earliest of them
//! ([`next_due`](crate::timers::next_due)), wakes then (a Durable Object alarm, the dev host's loop),
//! and hands its clock to [`run_due`](crate::timers::run_due), which runs every item due by then
//! through the cell's ordinary checked commit paths.
//!
//! Every item is consumed by the commit that handles it: a quantity expiry
//! and a flow timer consume their timer incarnation, and an expired decision
//! records its resolution. A wake delivered twice (at-least-once alarms, a
//! restart in between) finds nothing due the second time and commits
//! nothing. Time comes only from the host's clock argument.

use norn_cell::MAIN_WORLD;
use norn_cell::engine::SqlEngine;
use norn_ir::RuntimeObject;
use norn_kernel::{
    CellCommit, CellHost, DecisionTurn, DueTimer, EnvelopeLimits, FactEvent, FactKey, LogicalTime,
    TypeCatalog, World,
};

use crate::host::{ActionCell, DevHost};
use crate::refusal::Refusal;

/// The most items one [`run_due`] call handles; the rest are left for the
/// next wake, which [`DueWork::next`] then asks for at once.
pub const MAX_DUE_STEPS: usize = 64;

/// How long a host waits before it retries an item that was due but could
/// not be handled (a run of a retired flow, a refused turn), so one stuck
/// item never wakes the cell in a tight loop.
pub const STUCK_RETRY_MS: u64 = 60_000;

/// What one [`run_due`] call did, and when the host should wake next.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DueWork {
    /// Quantity reservations released at their expiry.
    pub expired_reservations: u32,
    /// Flow timers delivered to their runs (or consumed, for a timer no run
    /// waits on any more).
    pub timers: u32,
    /// Decisions resolved as expired.
    pub decisions: u32,
    /// Why each item that was due could not be handled, for the operator's
    /// log; such an item stays due and is retried.
    pub failed: Vec<String>,
    /// When to wake next (logical milliseconds), or `None` when nothing is
    /// pending. At most the wake time itself when work was left for the next
    /// wake; never earlier than [`STUCK_RETRY_MS`] ahead for an item that was
    /// due and could not be handled.
    pub next: Option<u64>,
}

impl DueWork {
    /// Whether anything was committed.
    #[must_use]
    pub const fn committed(&self) -> bool {
        self.expired_reservations > 0 || self.timers > 0 || self.decisions > 0
    }
}

/// The earliest deadline the cell holds (logical milliseconds): a timer
/// (quantity or flow) or an open decision's expiry. It may already have
/// passed: the host then wakes at once. `None` when nothing is pending.
///
/// # Errors
/// A host failure.
pub fn next_due<E: SqlEngine>(host: &DevHost<E>) -> Result<Option<u64>, Refusal> {
    let _epoch = host.definition_read()?;
    let cell = host.cell()?;
    let timer = cell
        .store()
        .read()
        .next_timer_due()
        .map_err(Refusal::host)?;
    let decision = earliest_decision(&cell, None)?;
    Ok(min(timer, decision))
}

/// Run everything due at `now`, at most [`MAX_DUE_STEPS`] items: release
/// expired quantity reservations, deliver flow timers to their runs, and
/// resolve decisions past their deadline. Each item commits on its own
/// through the cell's ordinary path; a failed item is left due and retried
/// no sooner than [`STUCK_RETRY_MS`] later.
///
/// # Errors
/// A host failure reading the cell. An item that fails is not an error of
/// the call: it stays due.
pub fn run_due<E: SqlEngine>(host: &DevHost<E>, now: LogicalTime) -> Result<DueWork, Refusal> {
    let _epoch = host.definition_read()?;
    let mut cell = host.cell()?;
    let now = crate::actions::turn_clock(&mut cell, now)?;
    let mut work = DueWork::default();
    let mut steps = 0;
    // Quantity expiries first: each consumes its timer with the release.
    // One that fails is corrupt state: the pass stops there, fail closed.
    while steps < MAX_DUE_STEPS {
        match cell.expire_quantity_due() {
            Ok(true) => {
                work.expired_reservations = work.expired_reservations.saturating_add(1);
                steps = steps.saturating_add(1);
            }
            Ok(false) => break,
            Err(error) => {
                work.failed.push(format!("quantity expiry: {error}"));
                break;
            }
        }
    }
    // Flow timers, earliest first, each on its own: one that cannot be
    // delivered stays due without hiding the ones behind it.
    let due_timers = cell
        .store()
        .read()
        .due_timers(now.as_millis(), MAX_DUE_STEPS.saturating_sub(steps))
        .map_err(Refusal::host)?;
    for pending in due_timers {
        steps = steps.saturating_add(1);
        let delivered = norn_ir::runtime::decode(&pending.timer.timer)
            .map_err(Refusal::host)
            .and_then(|timer| {
                let due = DueTimer {
                    timer,
                    incarnation: norn_kernel::LedgerSeq(pending.commit_seq),
                };
                fire_timer(&mut cell, &due, now)
            });
        match delivered {
            Ok(()) => work.timers = work.timers.saturating_add(1),
            Err(refusal) => work.failed.push(format!(
                "timer {}: {}",
                pending.timer.timer_id,
                why(&refusal)
            )),
        }
    }
    // Decisions past their deadline.
    let overdue: Vec<_> = crate::decisions::open_decisions(cell.store())?
        .into_iter()
        .filter(|decision| decision.expires_at_ms <= now.as_millis())
        .collect();
    for decision in overdue {
        if steps >= MAX_DUE_STEPS {
            break;
        }
        steps = steps.saturating_add(1);
        if let Err(refusal) = crate::decisions::expire(&mut cell, &decision, now) {
            work.failed
                .push(format!("decision {}: {}", decision.id, why(&refusal)));
            continue;
        }
        let resolved = cell
            .store()
            .read()
            .decision_resolution(MAIN_WORLD, &decision.id)
            .map_err(Refusal::host)?
            .is_some();
        if resolved {
            work.decisions = work.decisions.saturating_add(1);
        } else {
            // A retired flow's run keeps waiting on its decision.
            work.failed.push(format!(
                "decision {}: its run's flow is retired",
                decision.id
            ));
        }
    }
    let at = now.as_millis();
    work.next = if steps >= MAX_DUE_STEPS {
        Some(at)
    } else {
        let reader = cell.store().read();
        let future = min(
            reader.next_timer_due_after(at).map_err(Refusal::host)?,
            earliest_decision(&cell, Some(at))?,
        );
        let overdue = min(
            reader.next_timer_due().map_err(Refusal::host)?,
            earliest_decision(&cell, None)?,
        )
        .is_some_and(|due| due <= at);
        let retry = (!work.failed.is_empty() || overdue).then(|| at.saturating_add(STUCK_RETRY_MS));
        min(future, retry)
    };
    Ok(work)
}

/// What an item's refusal says for the operator: its host cause, else its code.
fn why(refusal: &Refusal) -> &str {
    refusal
        .extra
        .as_ref()
        .and_then(|extra| extra.cause.as_deref())
        .unwrap_or(&refusal.code)
}

fn min(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, None) => left,
        (None, right) => right,
    }
}

/// The earliest open decision deadline, strictly after `after` when given.
fn earliest_decision<E: SqlEngine>(
    cell: &ActionCell<E>,
    after: Option<u64>,
) -> Result<Option<u64>, Refusal> {
    Ok(crate::decisions::open_decisions(cell.store())?
        .into_iter()
        .map(|decision| decision.expires_at_ms)
        .filter(|due| after.is_none_or(|after| *due > after))
        .min())
}

/// Deliver one due timer: a flow run's timer goes to its run's timer turn,
/// committed consuming the timer; a timer that names no live run (the run
/// ended, or nothing on this host scheduled it for a run) is consumed by an
/// empty commit, so its firing is on the ledger and it never fires again.
/// A run whose flow is retired keeps waiting, as on a decision.
fn fire_timer<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    due: &DueTimer<RuntimeObject>,
    now: LogicalTime,
) -> Result<(), Refusal> {
    let consumed = Some(due.consumed());
    let record = &due.timer.payload;
    let run_id = match (
        record.get("kind").and_then(norn_ir::RuntimeValue::as_str),
        record.get("runId").and_then(norn_ir::RuntimeValue::as_str),
    ) {
        (Some("timer"), Some(run_id)) => Some(run_id.to_owned()),
        _ => None,
    };
    let row = match &run_id {
        Some(run_id) => cell
            .store()
            .read()
            .flow_run(MAIN_WORLD, run_id)
            .map_err(Refusal::host)?,
        None => None,
    };
    let commit = match (run_id, row) {
        (Some(run_id), Some(row)) => {
            let flow = crate::run_routes::installed_flow(cell.store(), &row.flow.id)?;
            let key = FactKey::new(flow.object_type.clone(), run_id.clone());
            let slice = norn_kernel::load_slice(&*cell, [key.clone()]).map_err(Refusal::host)?;
            match slice.get(&key).and_then(|state| state.values.clone()) {
                Some(run) => {
                    let output = norn_kernel::flow_turn::timer(
                        &flow.rule,
                        &flow.environment_hash,
                        run,
                        record.clone(),
                    )
                    .map_err(|error| Refusal::host(error.to_string()))?;
                    let model = cell.store().model().clone();
                    let catalog = TypeCatalog::new(model.object_types())
                        .with_property_types(model.property_types())
                        .with_interfaces(model.interfaces())
                        .with_governing_laws(model.law_artifacts());
                    let turn = DecisionTurn {
                        id: format!(
                            "norn.run/{run_id}/timer/{}/{}",
                            due.timer.id.0, due.incarnation.0
                        ),
                        now,
                        world: World::Main,
                        consumes: consumed,
                    };
                    norn_kernel::decision_cell_commit::<_, _, _, norn_cell::CellError>(
                        &catalog,
                        &slice,
                        &turn,
                        &output,
                        &EnvelopeLimits::V1,
                    )
                    .map_err(|refusal| Refusal::host(refusal.code()))?
                }
                // The run object is gone: the run ended.
                None => empty(consumed),
            }
        }
        _ => empty(consumed),
    };
    CellHost::commit(cell, commit).map_err(|error| Refusal::host(format!("{error:?}")))?;
    Ok(())
}

fn empty(
    consumes: Option<norn_kernel::Consumed>,
) -> CellCommit<FactEvent, RuntimeObject, RuntimeObject> {
    CellCommit {
        consumes,
        ..CellCommit::empty()
    }
}
