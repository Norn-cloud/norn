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
//!
//! An item that is due but cannot be handled stays due, with its retry
//! eligibility kept in the cell per item and incarnation (TIMER-RETRY-1):
//! every wake, re-arm and cold open respects its backoff, failed tries count
//! toward a wake's budget, and items are tried earliest deadline first
//! across all three kinds, so a stuck item never starves the rest. An
//! expiry whose resolving commit failed is resumed from its recorded result.
//! A flow timer runs under its run's pinned rule and environment
//! (TIMER-PIN-1).
//!
//! APPROVAL-RECOVERY-1: an approved action call is due from the commit that
//! records its approval until it settles. A host that stopped before the
//! call committed leaves it due, and the next wake (or the cold open that
//! arms one) commits it exactly once as its requester
//! ([`crate::decisions`]), or settles its refusal. A host that cannot act as
//! the requester at the wake ([`run_due`](crate::timers::run_due) has no
//! [`Requester`](crate::timers::Requester)) keeps it due
//! on its backoff.

use norn_cell::MAIN_WORLD;
use norn_cell::due_retry::{approval_item, decision_item, timer_item};
use norn_cell::engine::SqlEngine;
use norn_cell::read::PendingTimer;
use norn_ir::RuntimeObject;
use norn_kernel::{
    CellCommit, CellHost, DecisionTurn, DueTimer, EnvelopeLimits, FactEvent, FactKey, LogicalTime,
    TypeCatalog, World,
};

use crate::decisions::{OpenDecision, Resumed, Settle};
use crate::flow_driver::AsStarter;
use crate::host::{ActionCell, DevHost};
use crate::question_routes::QuestionRuntime;
use crate::refusal::Refusal;

/// The most items one [`run_due`] call tries, handled or failed alike; the
/// rest are left for the next wake, which [`DueWork::next`] then asks for at
/// once.
pub const MAX_DUE_STEPS: usize = 64;

/// The first backoff of an item that was due but could not be handled (a run
/// of a retired flow, a refused turn); it doubles with each failure of the
/// same incarnation, up to [`MAX_RETRY_MS`]. The retry eligibility is kept
/// in the cell (TIMER-RETRY-1), so every wake, request, cold open and the
/// dev host's loop respect it, and one stuck item never wakes the cell in a
/// tight loop.
pub const STUCK_RETRY_MS: u64 = 60_000;

/// The longest backoff of an item that keeps failing.
pub const MAX_RETRY_MS: u64 = 3_600_000;

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
    /// Approved action calls committed (APPROVAL-RECOVERY-1).
    pub approved_calls: u32,
    /// Approved action calls refused for good, with why, for the operator's
    /// log; each refusal is settled on its decision and never retried.
    pub refused_calls: Vec<String>,
    /// Why each item that was due could not be handled, for the operator's
    /// log; such an item stays due and is retried after its backoff.
    pub failed: Vec<String>,
    /// When to wake next (logical milliseconds), or `None` when nothing is
    /// pending: the wake time itself when eligible work was left for the
    /// next wake, else the earliest time any pending item may be tried (its
    /// deadline, or its retry eligibility after a failure).
    pub next: Option<u64>,
}

impl DueWork {
    /// Whether anything was committed.
    #[must_use]
    pub const fn committed(&self) -> bool {
        self.expired_reservations > 0
            || self.timers > 0
            || self.decisions > 0
            || self.approved_calls > 0
            || !self.refused_calls.is_empty()
    }
}

/// How a wake acts as an approved call's requester (APPROVAL-RECOVERY-1):
/// the host's principal switch ([`AsStarter`]) and the cell's disclosure
/// runtime the dispatch answers with.
pub struct Requester<'a, E: SqlEngine> {
    /// Runs one call as a recorded principal.
    pub as_requester: AsStarter<'a>,
    /// The cell's disclosure runtime.
    pub disclosure: &'a dyn QuestionRuntime<E>,
}

/// The earliest time the cell has work to try (logical milliseconds): a
/// timer (quantity or flow), an open decision's expiry, a recorded expiry
/// still to resolve, or an approved call still to commit, each no earlier
/// than its retry eligibility when it
/// failed before (TIMER-RETRY-1). It may already have passed: the host then
/// wakes at once. `None` when nothing is pending.
///
/// # Errors
/// A host failure.
pub fn next_due<E: SqlEngine>(host: &DevHost<E>) -> Result<Option<u64>, Refusal> {
    let _epoch = host.definition_read()?;
    let cell = host.cell()?;
    next_wake(&cell)
}

/// One item due at a wake.
enum Item {
    Quantity(PendingTimer),
    Timer(PendingTimer),
    Decision(Box<OpenDecision>),
}

impl Item {
    /// When it fell due, then its kind and id: the order items are tried in.
    fn order(&self) -> (u64, u8, &str) {
        match self {
            Self::Quantity(pending) => (pending.timer.due, 0, &pending.timer.timer_id),
            Self::Timer(pending) => (pending.timer.due, 1, &pending.timer.timer_id),
            Self::Decision(decision) => (decision.expires_at_ms, 2, &decision.id),
        }
    }

    /// Its retry key and incarnation (TIMER-RETRY-1).
    fn retry_key(&self) -> (String, u64) {
        match self {
            Self::Quantity(pending) | Self::Timer(pending) => {
                (timer_item(&pending.timer.timer_id), pending.commit_seq)
            }
            Self::Decision(decision) => (decision_item(&decision.id), decision.generation.0),
        }
    }
}

/// Run what is due at `now`, trying at most [`MAX_DUE_STEPS`] items:
/// release expired quantity reservations, deliver flow timers to their runs,
/// and resolve decisions past their deadline (resuming an expiry whose
/// resolving commit never landed). Items are tried earliest deadline first
/// across all three kinds, each committing on its own through the cell's
/// ordinary path; one that cannot be handled is recorded with its retry
/// eligibility (TIMER-RETRY-1), counts toward the budget, and never stops
/// the items behind it. An item still within its backoff is not tried.
///
/// # Errors
/// A host failure reading the cell or recording a failure. An item that
/// fails is not an error of the call: it stays due.
pub fn run_due<E: SqlEngine>(host: &DevHost<E>, now: LogicalTime) -> Result<DueWork, Refusal> {
    run_due_with(host, now, None)
}

/// [`run_due`], first committing the approved calls still due as their
/// requesters through `requester` (APPROVAL-RECOVERY-1). They count toward
/// the same budget, oldest approval first; without a `requester` each is
/// a failure kept due on its backoff.
///
/// # Errors
/// As [`run_due`].
pub fn run_due_with<E: SqlEngine>(
    host: &DevHost<E>,
    now: LogicalTime,
    requester: Option<&Requester<'_, E>>,
) -> Result<DueWork, Refusal> {
    let mut work = DueWork::default();
    let (tried, calls_left) = resume_calls(host, now, requester, &mut work)?;
    let budget = MAX_DUE_STEPS.saturating_sub(tried);
    let _epoch = host.definition_read()?;
    let mut cell = host.cell()?;
    let now = crate::actions::turn_clock(&mut cell, now)?;
    let at = now.as_millis();
    cell.store().prune_timer_retries().map_err(Refusal::host)?;
    let mut items = eligible(&cell, at)?;
    // One more than the budget tells whether work is left for the next wake.
    let more = calls_left || items.len() > budget;
    items.truncate(budget);
    for item in items {
        let (key, incarnation) = item.retry_key();
        let handled = match &item {
            Item::Quantity(pending) => {
                cell.advance_to(now);
                cell.expire_quantity(pending)
                    .map(|()| &mut work.expired_reservations)
                    .map_err(|error| format!("quantity expiry {}: {error}", pending.timer.timer_id))
            }
            Item::Timer(pending) => norn_ir::runtime::decode(&pending.timer.timer)
                .map_err(Refusal::host)
                .and_then(|timer| {
                    let due = DueTimer {
                        timer,
                        incarnation: norn_kernel::LedgerSeq(pending.commit_seq),
                    };
                    fire_timer(&mut cell, &due, now)
                })
                .map(|()| &mut work.timers)
                .map_err(|refusal| format!("timer {}: {}", pending.timer.timer_id, why(&refusal))),
            Item::Decision(decision) => expire_decision(&mut cell, decision, now)
                .map(|()| &mut work.decisions)
                .map_err(|why| format!("decision {}: {why}", decision.id)),
        };
        match handled {
            Ok(count) => {
                *count = count.saturating_add(1);
                cell.store().clear_due_retry(&key).map_err(Refusal::host)?;
            }
            Err(why) => {
                cell.store()
                    .record_due_failure(&key, incarnation, at, (STUCK_RETRY_MS, MAX_RETRY_MS))
                    .map_err(Refusal::host)?;
                work.failed.push(why);
            }
        }
    }
    work.next = if more { Some(at) } else { next_wake(&cell)? };
    Ok(work)
}

/// APPROVAL-RECOVERY-1: try the approved calls still due at `now`, oldest
/// approval first, at most [`MAX_DUE_STEPS`]; each is tried with the cell
/// released, so it goes through the ordinary dispatch. How many were tried,
/// and whether more are left for the next wake.
fn resume_calls<E: SqlEngine>(
    host: &DevHost<E>,
    now: LogicalTime,
    requester: Option<&Requester<'_, E>>,
    work: &mut DueWork,
) -> Result<(usize, bool), Refusal> {
    let (now, mut calls) = {
        let _epoch = host.definition_read()?;
        let mut cell = host.cell()?;
        let now = crate::actions::turn_clock(&mut cell, now)?;
        let calls = cell
            .store()
            .read()
            .eligible_approved_calls(MAIN_WORLD, now.as_millis(), MAX_DUE_STEPS.saturating_add(1))
            .map_err(Refusal::host)?;
        (now, calls)
    };
    let left = calls.len() > MAX_DUE_STEPS;
    calls.truncate(MAX_DUE_STEPS);
    for call in &calls {
        let key = approval_item(&call.decision_id);
        let resumed = resume_call(host, &call.decision_id, now, requester, work);
        let _epoch = host.definition_read()?;
        let cell = host.cell()?;
        match resumed {
            Ok(()) => cell.store().clear_due_retry(&key).map_err(Refusal::host)?,
            Err(why) => {
                cell.store()
                    .record_due_failure(&key, 1, now.as_millis(), (STUCK_RETRY_MS, MAX_RETRY_MS))
                    .map_err(Refusal::host)?;
                work.failed
                    .push(format!("approved call {}: {why}", call.decision_id));
            }
        }
    }
    Ok((calls.len(), left))
}

/// Commit one approved call still due, as its requester, and move a run
/// whose step waits on it; why it could not be.
fn resume_call<E: SqlEngine>(
    host: &DevHost<E>,
    decision_id: &str,
    now: LogicalTime,
    requester: Option<&Requester<'_, E>>,
    work: &mut DueWork,
) -> Result<(), String> {
    let Some(requester) = requester else {
        return Err("no requester principal at this wake".to_owned());
    };
    let resumed = crate::decisions::resume_approved(
        host,
        decision_id,
        requester.disclosure,
        now,
        requester.as_requester,
    )
    .map_err(|refusal| why(&refusal).to_owned())?;
    match resumed {
        // Settled by someone else meanwhile.
        None | Some(Resumed::Committed | Resumed::Refused(_)) => Ok(()),
        Some(Resumed::Dispatched(call, Settle::Committed)) => {
            debug_assert!(call.committed);
            work.approved_calls = work.approved_calls.saturating_add(1);
            drive_waiting(host, decision_id, now, requester)
        }
        Some(Resumed::Dispatched(call, Settle::Refused)) => {
            work.refused_calls.push(format!(
                "approved call {decision_id}: {}",
                call.code.as_deref().unwrap_or_default()
            ));
            Ok(())
        }
        Some(Resumed::Dispatched(call, Settle::Retry)) => {
            Err(call.code.unwrap_or_else(|| "not committed".to_owned()))
        }
    }
}

/// Move the run whose action step waits on the approved call of
/// `decision_id`, if one does (ACTION-STEP-1): its drive replays the
/// committed call and delivers its result.
fn drive_waiting<E: SqlEngine>(
    host: &DevHost<E>,
    decision_id: &str,
    now: LogicalTime,
    requester: &Requester<'_, E>,
) -> Result<(), String> {
    let run = {
        let _epoch = host
            .definition_read()
            .map_err(|refusal| why(&refusal).to_owned())?;
        let cell = host.cell().map_err(|refusal| why(&refusal).to_owned())?;
        let reader = cell.store().read();
        let key = reader
            .approved_call(MAIN_WORLD, decision_id)
            .map_err(|error| error.to_string())?
            .and_then(|row| reader.outbox_command(&row.command_id).ok().flatten())
            .and_then(|command| {
                command
                    .command
                    .get("payload")
                    .and_then(|payload| payload.get("approval"))
                    .and_then(crate::approvals::held_call)
            })
            .map(|held| held.call.idempotency_key);
        match key {
            Some(key) => crate::flow_driver::run_waiting_on(cell.store(), &key)
                .map_err(|refusal| why(&refusal).to_owned())?,
            None => None,
        }
    };
    if let Some(run) = run {
        crate::flow_driver::drive_run(
            host,
            &run,
            requester.as_requester,
            requester.disclosure,
            now,
        )
        .map_err(|refusal| format!("its run {run} did not move: {}", why(&refusal)))?;
    }
    Ok(())
}

/// Every item due at `at` and eligible to be tried, earliest deadline first
/// across reservations, flow timers and decisions, at most one more than the
/// budget.
fn eligible<E: SqlEngine>(cell: &ActionCell<E>, at: u64) -> Result<Vec<Item>, Refusal> {
    let bound = MAX_DUE_STEPS.saturating_add(1);
    let reader = cell.store().read();
    let mut items: Vec<Item> = reader
        .eligible_due_timers(at, bound, true)
        .map_err(Refusal::host)?
        .into_iter()
        .map(Item::Quantity)
        .collect();
    items.extend(
        reader
            .eligible_due_timers(at, bound, false)
            .map_err(Refusal::host)?
            .into_iter()
            .map(Item::Timer),
    );
    let retries = reader
        .due_retries(&decision_item(""))
        .map_err(Refusal::host)?;
    items.extend(
        decisions(cell)?
            .into_iter()
            .filter(|decision| {
                decision.expires_at_ms <= at
                    && retries
                        .get(&decision_item(&decision.id))
                        .filter(|retry| retry.incarnation == decision.generation.0)
                        .is_none_or(|retry| retry.eligible_at <= at)
            })
            .map(|decision| Item::Decision(Box::new(decision))),
    );
    items.sort_by(|left, right| left.order().cmp(&right.order()));
    items.truncate(bound);
    Ok(items)
}

/// Every decision a wake may resolve: the open ones, and the expiries
/// recorded whose resolving commit never landed.
fn decisions<E: SqlEngine>(cell: &ActionCell<E>) -> Result<Vec<OpenDecision>, Refusal> {
    let mut all = crate::decisions::open_decisions(cell.store())?;
    all.extend(crate::decisions::stalled_expiries(cell.store())?);
    Ok(all)
}

/// Resolve one overdue decision as expired; why it could not be.
fn expire_decision<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    decision: &OpenDecision,
    now: LogicalTime,
) -> Result<(), String> {
    crate::decisions::expire(cell, decision, now).map_err(|refusal| why(&refusal).to_owned())?;
    let resolved = cell
        .store()
        .read()
        .decision_resolution(MAIN_WORLD, &decision.id)
        .map_err(|error| error.to_string())?
        .is_some();
    if resolved {
        Ok(())
    } else {
        // A retired flow's run keeps waiting on its decision.
        Err("its run's flow is retired".to_owned())
    }
}

/// When the cell next has work to try: the earliest timer or decision
/// deadline, or approved call still due, each no earlier than its retry
/// eligibility (TIMER-RETRY-1).
fn next_wake<E: SqlEngine>(cell: &ActionCell<E>) -> Result<Option<u64>, Refusal> {
    let reader = cell.store().read();
    let timer = reader.next_eligible_timer().map_err(Refusal::host)?;
    let retries = reader
        .due_retries(&decision_item(""))
        .map_err(Refusal::host)?;
    let decision = decisions(cell)?
        .into_iter()
        .map(|decision| {
            let eligible = retries
                .get(&decision_item(&decision.id))
                .filter(|retry| retry.incarnation == decision.generation.0)
                .map_or(0, |retry| retry.eligible_at);
            decision.expires_at_ms.max(eligible)
        })
        .min();
    let approved = reader
        .next_approved_call(MAIN_WORLD)
        .map_err(Refusal::host)?;
    Ok(min(min(timer, decision), approved))
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

/// Deliver one due timer: a flow run's timer goes to its run's timer turn
/// under the rule and environment the run was pinned to when it started
/// (TIMER-PIN-1), committed consuming the timer; a timer that names no live
/// run (the run ended, or nothing on this host scheduled it for a run) is
/// consumed by an empty commit, so its firing is on the ledger and it never
/// fires again. A run whose flow is retired, or whose pin is unavailable,
/// keeps waiting.
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
            // TIMER-PIN-1: the run's own rule and environment, as pinned when it
            // started; a pin the cell cannot produce keeps the timer due.
            let flow = crate::run_routes::pinned_flow(cell.store(), &row)?;
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
