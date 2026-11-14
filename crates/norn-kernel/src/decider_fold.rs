//! The decider's pure decision core: the ledger fold, the order of a turn's
//! checks, and the envelope cap decisions.
//!
//! This file depends on `core` only, so Charon extracts it standalone
//! (`--start-from`) and `verification/kernel-proof-v1/Decider` proves the
//! extracted functions against their written semantics for every decider and
//! every input. `decider.rs` and `envelope.rs` are the glue: they compute counts,
//! clone states and build the public errors, and call into this file for each
//! decision.

/// A state transition: the part of a decider a ledger fold needs.
pub(crate) trait Evolve {
    /// Materialized state.
    type State;
    /// A ledger event.
    type Event;

    /// The state before any event.
    fn initial_state(&self) -> Self::State;

    /// Applies one event.
    fn evolve(&self, state: &Self::State, event: &Self::Event) -> Self::State;
}

/// Folds `events` into `state` with [`Evolve::evolve`], first event first.
pub(crate) fn fold_events<D: Evolve>(
    decider: &D,
    state: D::State,
    events: &[D::Event],
) -> D::State {
    let mut state = state;
    let mut index: usize = 0;
    while let Some(event) = events.get(index) {
        state = decider.evolve(&state, event);
        let Some(next) = index.checked_add(1) else {
            break;
        };
        index = next;
    }
    state
}

/// Rebuilds state from a ledger: [`fold_events`] from the initial state.
pub(crate) fn replay_events<D: Evolve>(decider: &D, events: &[D::Event]) -> D::State {
    fold_events(decider, decider.initial_state(), events)
}

/// Which envelope cap a count exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvelopeCap {
    /// The event cap.
    Events,
    /// The command (`perform`) cap.
    Commands,
    /// The timer cap.
    Timers,
}

/// One count, or one cap, per envelope kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Events.
    pub(crate) events: u64,
    /// Commands.
    pub(crate) commands: u64,
    /// Timers.
    pub(crate) timers: u64,
}

/// The count of one kind.
pub(crate) const fn count_of(counts: Counts, cap: EnvelopeCap) -> u64 {
    match cap {
        EnvelopeCap::Events => counts.events,
        EnvelopeCap::Commands => counts.commands,
        EnvelopeCap::Timers => counts.timers,
    }
}

/// The first kind, in the order events, commands, timers, whose count is above
/// its cap; `None` when every count fits.
pub(crate) const fn exceeded_cap(caps: Counts, counts: Counts) -> Option<EnvelopeCap> {
    if counts.events > caps.events {
        return Some(EnvelopeCap::Events);
    }
    if counts.commands > caps.commands {
        return Some(EnvelopeCap::Commands);
    }
    if counts.timers > caps.timers {
        return Some(EnvelopeCap::Timers);
    }
    None
}

/// An aggregate turn cap a commit would exceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TurnExceeded {
    /// Which cap.
    pub(crate) cap: EnvelopeCap,
    /// The total the commit would reach, or `u64::MAX` when it overflows.
    pub(crate) actual: u64,
}

/// The total of `used + added` when it exceeds `cap`, saturated at `u64::MAX`
/// on overflow; `None` when it fits.
const fn turn_overflow(used: u64, cap: u64, added: u64) -> Option<u64> {
    match used.checked_add(added) {
        None => Some(u64::MAX),
        Some(total) => {
            if total > cap {
                Some(total)
            } else {
                None
            }
        }
    }
}

/// The first kind, in the order events, commands, timers, whose total after
/// adding `added` to `used` is above its cap (or overflows); `None` when every
/// total fits.
pub(crate) const fn exceeded_turn_cap(
    used: Counts,
    caps: Counts,
    added: Counts,
) -> Option<TurnExceeded> {
    if let Some(actual) = turn_overflow(used.events, caps.events, added.events) {
        return Some(TurnExceeded {
            cap: EnvelopeCap::Events,
            actual,
        });
    }
    if let Some(actual) = turn_overflow(used.commands, caps.commands, added.commands) {
        return Some(TurnExceeded {
            cap: EnvelopeCap::Commands,
            actual,
        });
    }
    if let Some(actual) = turn_overflow(used.timers, caps.timers, added.timers) {
        return Some(TurnExceeded {
            cap: EnvelopeCap::Timers,
            actual,
        });
    }
    None
}

/// One turn's inputs as the step driver sees them: the world, the decider's
/// decision, the decision's counts and events, and the per-decision caps.
pub(crate) trait StepTurn: Evolve {
    /// What the decider decides.
    type Decision;
    /// The decider's rejection.
    type Rejection;

    /// Whether the turn's world accepts new events.
    fn writable(&self) -> bool;
    /// Whether the turn's world may perform commands.
    fn may_perform(&self) -> bool;
    /// The decider's decision for this turn against `state`.
    ///
    /// # Errors
    ///
    /// The decider's rejection.
    fn decide(&self, state: &Self::State) -> Result<Self::Decision, Self::Rejection>;
    /// The decision's event, command and timer counts.
    fn counts(&self, decision: &Self::Decision) -> Counts;
    /// The per-decision caps.
    fn caps(&self) -> Counts;
    /// The decision's new ledger events, in order.
    fn events<'d>(&self, decision: &'d Self::Decision) -> &'d [Self::Event];
    /// A copy of `state` to evolve.
    fn clone_state(&self, state: &Self::State) -> Self::State;
}

/// Why the step driver refused a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepRefusal<R> {
    /// The world is read-only.
    ReadOnlyWorld,
    /// The decider rejected the input.
    Rejected(R),
    /// The decision is above a per-decision cap.
    EnvelopeExceeded {
        /// The first cap exceeded.
        cap: EnvelopeCap,
        /// The decision's count of that kind.
        actual: u64,
    },
    /// A world that may not perform requested commands.
    PerformOutsideMain {
        /// The decision's command count.
        commands: u64,
    },
}

/// What [`run_step`] returns: the evolved state and the decision, or the first
/// failed check.
pub(crate) type StepOutcome<T> = Result<
    (<T as Evolve>::State, <T as StepTurn>::Decision),
    StepRefusal<<T as StepTurn>::Rejection>,
>;

/// One turn, checked in order: the world is writable; the decider accepts;
/// the decision fits the caps; a world that may not perform requests no
/// command. Only then are the decision's events folded into a copy of `state`.
///
/// # Errors
///
/// The first failed check.
pub(crate) fn run_step<T: StepTurn>(turn: &T, state: &T::State) -> StepOutcome<T> {
    if !turn.writable() {
        return Err(StepRefusal::ReadOnlyWorld);
    }
    let decision = match turn.decide(state) {
        Ok(decision) => decision,
        Err(rejection) => return Err(StepRefusal::Rejected(rejection)),
    };
    let counts = turn.counts(&decision);
    if let Some(cap) = exceeded_cap(turn.caps(), counts) {
        return Err(StepRefusal::EnvelopeExceeded {
            cap,
            actual: count_of(counts, cap),
        });
    }
    if counts.commands > 0 && !turn.may_perform() {
        return Err(StepRefusal::PerformOutsideMain {
            commands: counts.commands,
        });
    }
    let next = fold_events(turn, turn.clone_state(state), turn.events(&decision));
    Ok((next, decision))
}
