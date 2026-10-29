//! Model-free Decider laws over a driven turn sequence (T2-KER-3).
//!
//! The [`DeciderMachine`](crate::DeciderMachine) checks a decider against a
//! reference model. The laws here need no model: they relate `step`,
//! `decide`, `evolve`, and fresh decider instances to each other, so they
//! apply to any decider (toy or real) and to any initial state, not only the
//! one `initial_state` returns.
//!
//! Every law returns `Result<(), TestCaseError>` so it composes with
//! [`proptest`] strategies through [`check_property`](crate::check_property).
//! [`drive`], [`step_law`], and [`rejection_is_transparent`] take the `step`
//! function as a [`StepFn`] parameter: a test passes [`norn_kernel::step`], a mutant self-test
//! passes a deliberately broken one to prove the law can fail.

use std::convert::Infallible;
use std::fmt;

use norn_kernel::{
    Decider, Decision, DecisionOf, Effect, EnvelopeKind, StepError, StepResult, fold,
};
use proptest::test_runner::TestCaseError;
use proptest::{prop_assert, prop_assert_eq};

use crate::model::{DecisionShape, Turn};

/// A decider whose data can be compared and printed, as every law needs.
pub trait Checkable:
    Decider<
        State: fmt::Debug + PartialEq,
        Input: Clone + fmt::Debug + PartialEq,
        Event: Clone + fmt::Debug + PartialEq,
        Command: fmt::Debug + PartialEq,
        Rejection: PartialEq,
    >
{
}

impl<D> Checkable for D where
    D: Decider<
            State: fmt::Debug + PartialEq,
            Input: Clone + fmt::Debug + PartialEq,
            Event: Clone + fmt::Debug + PartialEq,
            Command: fmt::Debug + PartialEq,
            Rejection: PartialEq,
        >
{
}

/// The signature of [`norn_kernel::step`]; laws take it as a parameter.
pub type StepFn<D> = fn(
    &D,
    &<D as Decider>::State,
    &<D as Decider>::Input,
    &norn_kernel::Observations,
    &norn_kernel::EnvelopeLimits,
) -> StepResult<D>;

/// The effects type of decider `D`.
pub type EffectOf<D> = Effect<<D as Decider>::Command, <D as Decider>::Input>;

pub(crate) fn exceeds(actual: usize, limit: u32) -> bool {
    u32::try_from(actual).map_or(true, |actual| actual > limit)
}

/// The kernel rules applied to an already-taken `decide` result: world check,
/// the rejection, envelope caps in order (events, commands, timers), the
/// perform-outside-main check, then an explicit `evolve` loop.
///
/// This is the independent oracle for [`norn_kernel::step`]; the model-based
/// harness and [`drive`] share it.
///
/// # Errors
///
/// The [`StepError`] `step` must return for this turn.
pub fn kernel_rules<D: Decider + ?Sized>(
    decider: &D,
    before: &D::State,
    turn: &Turn<D::Input>,
    outcome: Result<DecisionOf<D>, D::Rejection>,
) -> StepResult<D> {
    let world = &turn.observations.world;
    if !world.is_writable() {
        return Err(StepError::ReadOnlyWorld);
    }
    let decision = outcome.map_err(StepError::Rejected)?;
    let shape = DecisionShape::of(&decision);
    let limits = &turn.limits;
    for (kind, actual, limit) in [
        (EnvelopeKind::Events, shape.events, limits.max_events),
        (EnvelopeKind::Commands, shape.performs, limits.max_commands),
        (EnvelopeKind::Timers, shape.timers, limits.max_timers),
    ] {
        if exceeds(actual, limit) {
            return Err(StepError::EnvelopeExceeded {
                kind,
                limit,
                actual,
            });
        }
    }
    if shape.performs > 0 && !world.may_perform() {
        return Err(StepError::PerformOutsideMain {
            count: shape.performs,
        });
    }
    let Decision { events, effects } = decision;
    let mut state = before.clone();
    for event in &events {
        state = decider.evolve(&state, event);
    }
    Ok(norn_kernel::Step {
        state,
        events,
        effects,
    })
}

/// What one driven turn did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRecord {
    /// Whether `step` accepted the turn.
    pub accepted: bool,
    /// Ledger length after the turn.
    pub ledger_len: usize,
    /// Effect-queue length after the turn.
    pub effects_len: usize,
}

/// The committed result of driving a turn sequence through `step`, as a host
/// would record it: the ledger, the effect queue, and each turn's state.
pub struct Trace<D: Decider> {
    /// The state the sequence started from.
    pub initial: D::State,
    /// State after each turn (refused turns repeat the previous state).
    pub states: Vec<D::State>,
    /// Per-turn outcome.
    pub turns: Vec<TurnRecord>,
    /// Every accepted event, in commit order.
    pub ledger: Vec<D::Event>,
    /// Every accepted effect, in enqueue order.
    pub effects: Vec<EffectOf<D>>,
}

impl<D: Decider> Trace<D> {
    /// The state after the last turn.
    pub fn last_state(&self) -> &D::State {
        self.states.last().unwrap_or(&self.initial)
    }
}

impl<D: Checkable> fmt::Debug for Trace<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Trace")
            .field("initial", &self.initial)
            .field("states", &self.states)
            .field("turns", &self.turns)
            .field("ledger", &self.ledger)
            .field("effects", &self.effects)
            .finish()
    }
}

/// Drives `turns` through `step` from `initial` and records what a host
/// would commit. Checks nothing; see [`step_law`].
pub fn drive<D: Decider>(
    step: StepFn<D>,
    decider: &D,
    initial: D::State,
    turns: &[Turn<D::Input>],
) -> Trace<D> {
    let checked: Result<_, Infallible> =
        drive_checked(step, decider, initial, turns, |_, _, _| Ok(()));
    match checked {
        Ok(trace) => trace,
        Err(never) => match never {},
    }
}

/// **Step law**, checked on every turn while driving: the result of `step`
/// equals `decide` followed by [`kernel_rules`]. So a refusal (rejection,
/// read-only world, envelope cap, perform outside main) commits zero events
/// and zero effects and leaves state untouched, and an accepted turn commits
/// exactly `decide`'s events and effects, in order, with `fold(evolve)` as
/// its state.
///
/// # Errors
///
/// The first turn whose `step` result differs from the oracle.
pub fn step_law<D: Checkable>(
    step: StepFn<D>,
    decider: &D,
    initial: D::State,
    turns: &[Turn<D::Input>],
) -> Result<Trace<D>, TestCaseError> {
    drive_checked(step, decider, initial, turns, |before, turn, actual| {
        let outcome = decider.decide(before, &turn.input, &turn.observations);
        let expected = kernel_rules(decider, before, turn, outcome);
        prop_assert_eq!(
            actual,
            &expected,
            "law violated (step): step differs from decide → world check → caps → perform-outside-main → evolve\nstate: {:?}\nturn: {:?}",
            before,
            turn
        );
        Ok(())
    })
}

fn drive_checked<D: Decider, X>(
    step: StepFn<D>,
    decider: &D,
    initial: D::State,
    turns: &[Turn<D::Input>],
    mut check: impl FnMut(&D::State, &Turn<D::Input>, &StepResult<D>) -> Result<(), X>,
) -> Result<Trace<D>, X> {
    let mut trace: Trace<D> = Trace {
        initial,
        states: Vec::with_capacity(turns.len()),
        turns: Vec::with_capacity(turns.len()),
        ledger: Vec::new(),
        effects: Vec::new(),
    };
    for turn in turns {
        let before = trace.last_state().clone();
        let actual = step(
            decider,
            &before,
            &turn.input,
            &turn.observations,
            &turn.limits,
        );
        check(&before, turn, &actual)?;
        let accepted = actual.is_ok();
        let after = match actual {
            Ok(accepted) => {
                trace.ledger.extend(accepted.events);
                trace.effects.extend(accepted.effects);
                accepted.state
            }
            Err(_) => before,
        };
        trace.states.push(after);
        trace.turns.push(TurnRecord {
            accepted,
            ledger_len: trace.ledger.len(),
            effects_len: trace.effects.len(),
        });
    }
    Ok(trace)
}

/// **Replay law**: materialized state is `fold(evolve)` over the ledger.
///
/// After every turn, the state equals `fold(initial, ledger[..n])`; and for
/// every split `k` of the ledger, `fold(fold(initial, ledger[..k]),
/// ledger[k..]) == fold(initial, ledger)` (folding is a monoid action of the
/// event sequence on states).
///
/// # Errors
///
/// The first turn or split where the equation fails.
pub fn replay_is_fold<D: Checkable>(decider: &D, trace: &Trace<D>) -> Result<(), TestCaseError> {
    for (ix, (state, record)) in trace.states.iter().zip(&trace.turns).enumerate() {
        let prefix = trace.ledger.get(..record.ledger_len).unwrap_or_default();
        let replayed = fold(decider, trace.initial.clone(), prefix);
        prop_assert_eq!(
            &replayed,
            state,
            "law violated (replay): fold(evolve) over the ledger != materialized state after turn {}",
            ix
        );
    }
    let whole = fold(decider, trace.initial.clone(), &trace.ledger);
    prop_assert_eq!(
        &whole,
        trace.last_state(),
        "law violated (replay): replay(ledger) != materialized state"
    );
    for split in 0..=trace.ledger.len() {
        let (prefix, suffix) = trace.ledger.split_at(split);
        let resumed = fold(
            decider,
            fold(decider, trace.initial.clone(), prefix),
            suffix,
        );
        prop_assert_eq!(
            &resumed,
            &whole,
            "law violated (replay): fold(fold(s, ledger[..{}]), ledger[{}..]) != fold(s, ledger)",
            split,
            split
        );
    }
    Ok(())
}

/// **Purity law**: `decide` is a function of `(state, input, observations)`.
///
/// Two fresh decider instances, and a repeated call on one instance, return
/// equal results for every turn — historical worlds included, although
/// `step` refuses those before consulting `decide`.
///
/// # Errors
///
/// The first turn with differing results.
pub fn decide_is_pure<D: Checkable>(
    make: &dyn Fn() -> D,
    state: &D::State,
    turns: &[Turn<D::Input>],
) -> Result<(), TestCaseError> {
    let (first, second) = (make(), make());
    for turn in turns {
        let a = first.decide(state, &turn.input, &turn.observations);
        let again = first.decide(state, &turn.input, &turn.observations);
        let b = second.decide(state, &turn.input, &turn.observations);
        prop_assert_eq!(
            &a,
            &again,
            "law violated (purity): decide is not deterministic on one instance\nturn: {:?}",
            turn
        );
        prop_assert_eq!(
            &a,
            &b,
            "law violated (purity): decide differs between fresh instances\nturn: {:?}",
            turn
        );
    }
    Ok(())
}

/// **Rejection law**: a refused turn is invisible.
///
/// Driving `turns` and driving only the turns that were accepted — each on a
/// fresh decider — commit the same ledger and effects and reach the same
/// state, and every accepted turn is accepted again. A refusal therefore
/// leaves no trace in state, ledger, effect queue, or hidden decider state.
///
/// # Errors
///
/// Any difference between the two runs.
pub fn rejection_is_transparent<D: Checkable>(
    step: StepFn<D>,
    make: &dyn Fn() -> D,
    initial: &D::State,
    turns: &[Turn<D::Input>],
) -> Result<(), TestCaseError> {
    let full = drive(step, &make(), initial.clone(), turns);
    let kept: Vec<_> = turns
        .iter()
        .zip(&full.turns)
        .filter(|(_, record)| record.accepted)
        .map(|(turn, _)| turn.clone())
        .collect();
    let filtered = drive(step, &make(), initial.clone(), &kept);
    prop_assert!(
        filtered.turns.iter().all(|record| record.accepted),
        "law violated (rejection): an accepted turn is refused once the refused turns are removed\nfull: {:?}\nfiltered: {:?}",
        full,
        filtered
    );
    prop_assert_eq!(
        &full.ledger,
        &filtered.ledger,
        "law violated (rejection): refused turns changed the ledger"
    );
    prop_assert_eq!(
        &full.effects,
        &filtered.effects,
        "law violated (rejection): refused turns changed the effect queue"
    );
    prop_assert_eq!(
        full.last_state(),
        filtered.last_state(),
        "law violated (rejection): refused turns changed state"
    );
    Ok(())
}
